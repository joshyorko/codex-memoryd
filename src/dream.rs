//! Deterministic Dreamer heuristics for staleness, state transitions, and
//! supersession. This module is intentionally policy/store-backed and does not
//! call an LLM.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::Duration;
use time::OffsetDateTime;

use crate::domain::MemoryRecord;
use crate::domain::Profile;
use crate::error::Result;
use crate::ids;
use crate::policy;
use crate::policy::PolicyDecision;
use crate::protocol::DreamCandidate;
use crate::protocol::DreamRejection;
use crate::protocol::DreamResponse;
use crate::protocol::DreamStaleRecord;
use crate::store::NewRecord;
use crate::store::RecordQuery;
use crate::store::Store;
use crate::store::UpsertOutcome;

pub const DREAM_IMPLEMENTATION_VERSION: &str = "heuristic-v1";
pub const DREAM_RULESET_VERSION: &str = "dreamer-heuristics-v1";
pub const DREAM_FIXTURE_SCHEMA_VERSION: Option<&str> = None;

static RELATIVE_TIME: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(today|tomorrow|tonight|this week|next week|this weekend|currently|right now|soon|as of now|going to|planning to|will\s+(add|build|complete|deploy|fix|implement|merge|migrate|patch|release|remove|replace|resolve|run|ship|switch|test|update|write))\b",
    )
    .expect("relative-time regex")
});

const STOPWORDS: &[&str] = &[
    "about",
    "after",
    "again",
    "backend",
    "being",
    "blocked",
    "completed",
    "currently",
    "decision",
    "deployed",
    "done",
    "evaluating",
    "fixed",
    "going",
    "implemented",
    "into",
    "later",
    "longer",
    "merged",
    "options",
    "planned",
    "planning",
    "please",
    "proposal",
    "right",
    "run",
    "still",
    "summary",
    "that",
    "this",
    "use",
    "uses",
    "will",
    "with",
    "yes",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceClass {
    UserVisibleTurn,
    AdoptedAssistantProposal,
    AssistantVisibleTurn,
    ExplicitConclusion,
    Checkpoint,
    ImportedMemory,
    ActiveMemory,
}

impl EvidenceClass {
    fn as_str(self) -> &'static str {
        match self {
            EvidenceClass::UserVisibleTurn => "user_visible_turn",
            EvidenceClass::AdoptedAssistantProposal => "adopted_assistant_proposal",
            EvidenceClass::AssistantVisibleTurn => "assistant_visible_turn",
            EvidenceClass::ExplicitConclusion => "explicit_conclusion",
            EvidenceClass::Checkpoint => "checkpoint",
            EvidenceClass::ImportedMemory => "imported_memory",
            EvidenceClass::ActiveMemory => "active_memory",
        }
    }

    fn weight(self) -> f64 {
        match self {
            EvidenceClass::UserVisibleTurn => 1.0,
            EvidenceClass::AdoptedAssistantProposal => 1.25,
            EvidenceClass::AssistantVisibleTurn => 0.25,
            EvidenceClass::ExplicitConclusion => 2.0,
            EvidenceClass::Checkpoint => 1.5,
            EvidenceClass::ImportedMemory => 0.5,
            EvidenceClass::ActiveMemory => 0.0,
        }
    }
}

#[derive(Debug, Clone)]
struct EvidenceScore {
    classes: Vec<EvidenceClass>,
    weight: f64,
    reason: String,
    candidate_state: String,
    apply_eligible: bool,
}

pub struct DreamParams<'a> {
    pub profile: Profile,
    pub workspace: &'a str,
    pub repo_id: Option<&'a str>,
    pub mode: &'a str,
    pub now: &'a str,
    pub recency_cutoff: Option<&'a str>,
    pub include_archived_sources: bool,
    pub max_records: usize,
    pub max_candidates: Option<usize>,
}

pub fn run(store: &Store, params: &DreamParams) -> Result<DreamResponse> {
    let records = store.query_records(&RecordQuery {
        profile_id: Some(params.profile.as_str().to_string()),
        workspace_id: Some(params.workspace.to_string()),
        repo_id: params.repo_id.map(str::to_string),
        record_type: None,
        scope: None,
        include_archived: params.include_archived_sources,
        recency_cutoff: params.recency_cutoff.map(|s| s.to_string()),
        limit: params.max_records,
        offset: 0,
    })?;
    let mut candidates = Vec::new();
    let mut stale = Vec::new();
    let mut rejected = Vec::new();

    for record in &records {
        let state = state_for_record(record);
        let drift_prone = state != "historical" && is_drift_prone(&record.content);
        let valid_until = valid_until_for(record);
        let expired = valid_until
            .as_deref()
            .map(|until| is_after(params.now, until))
            .unwrap_or(false);
        if drift_prone {
            let historical_reason = expired.then(|| "expired relative-time content".to_string());
            stale.push(DreamStaleRecord {
                memory_id: record.id.clone(),
                drift_prone,
                state: state.clone(),
                expires_at: valid_until.clone(),
                valid_until: valid_until.clone(),
                suggested_action: if expired {
                    "rewrite_historical".to_string()
                } else {
                    "set_valid_until".to_string()
                },
                historical_reason: historical_reason.clone(),
            });
            if expired {
                let content = format!(
                    "As of {}, {}",
                    date_part(&record.created_at),
                    record.content
                );
                push_candidate(
                    &mut candidates,
                    &mut rejected,
                    record,
                    "rewrite_historical",
                    &content,
                    "historical",
                    false,
                    None,
                    historical_reason,
                    vec![record.id.clone()],
                );
            }
        }
    }

    for newer in &records {
        let newer_state = state_for_record(newer);
        for older in &records {
            if newer.id == older.id
                || !same_boundary(newer, older)
                || newer.created_at <= older.created_at
            {
                continue;
            }
            if supersedes(newer, older, &newer_state) {
                let reason = format!(
                    "newer {} evidence supersedes older {} state",
                    newer_state,
                    state_for(&older.content)
                );
                push_candidate(
                    &mut candidates,
                    &mut rejected,
                    newer,
                    "supersede",
                    &newer.content,
                    &newer_state,
                    is_drift_prone(&newer.content),
                    valid_until_for(newer),
                    Some(reason),
                    vec![older.id.clone()],
                );
            }
        }
    }

    if params.recency_cutoff.is_none() || params.include_archived_sources {
        push_threshold_candidates(&records, &mut candidates, &mut rejected);
    }

    dedupe_candidates(&mut candidates);
    if let Some(max) = params.max_candidates {
        candidates.truncate(max);
    }

    let run_id = stable_run_id(params, &records);
    let mut archived = Vec::new();
    let mut created = Vec::new();
    if params.mode == "apply" {
        for candidate in &candidates {
            if !candidate.apply_eligible {
                continue;
            }
            let content = match policy::screen_content(&candidate.content, policy::MAX_RECORD_CHARS)
            {
                PolicyDecision::Accept(clean) => clean,
                PolicyDecision::Reject { code, reason } => {
                    let _ = store.record_policy_event(
                        Some(params.profile.as_str()),
                        Some(params.workspace),
                        "rejected_dream_candidate",
                        &code,
                        &reason,
                        "dream",
                    );
                    rejected.push(DreamRejection {
                        reason,
                        supersedes: candidate.supersedes.clone(),
                    });
                    continue;
                }
            };
            let class = policy::classify(&content, params.profile, params.repo_id.is_some());
            let content_hash = ids::content_hash(
                params.profile.as_str(),
                params.workspace,
                params.repo_id,
                class.record_type.as_str(),
                class.scope.as_str(),
                &content,
            );
            let metadata = json!({
                "origin": "dreamer",
                "dream_run_id": run_id.clone(),
                "run_id": run_id.clone(),
                "subject_key": candidate.subject_key,
                "candidate_state": candidate.candidate_state,
                "threshold_reason": candidate.threshold_reason,
                "evidence_weight": candidate.evidence_weight,
                "evidence_classes": candidate.evidence_classes,
                "evidence_ids": candidate.evidence_ids,
                "evidence_count": candidate.evidence_count,
                "user_evidence_count": candidate.user_evidence_count,
                "assistant_evidence_count": candidate.assistant_evidence_count,
                "first_seen_at": candidate.first_seen_at,
                "last_seen_at": candidate.last_seen_at,
                "state": candidate.state,
                "drift_prone": candidate.drift_prone,
                "expires_at": candidate.expires_at,
                "valid_until": candidate.valid_until,
                "historical_reason": candidate.historical_reason,
                "supersedes": candidate.supersedes,
                "promotion_reason": candidate.promotion_reason,
                "evidence_window": {
                    "start": candidate.first_seen_at,
                    "end": candidate.last_seen_at,
                },
            });
            let outcome = store.upsert_record(&NewRecord {
                profile_id: params.profile.as_str().to_string(),
                workspace_id: params.workspace.to_string(),
                repo_id: params.repo_id.map(|s| s.to_string()),
                scope: class.scope,
                record_type: class.record_type,
                content,
                related_files: class.related_files,
                tags: class.tags,
                sensitivity: class.sensitivity,
                portability: class.portability,
                confidence: candidate.confidence,
                source_ids: candidate.evidence_ids.clone(),
                content_hash,
                supersedes: candidate.supersedes.clone(),
                metadata,
            })?;
            if let UpsertOutcome::Created(id) = outcome {
                created.push(id);
            }
            if !candidate.supersedes.is_empty() {
                let reason = candidate
                    .historical_reason
                    .as_deref()
                    .unwrap_or("superseded by newer Dreamer evidence");
                let (mut newly_archived, _) = store.archive_records_with_metadata(
                    params.profile.as_str(),
                    Some(params.workspace),
                    &candidate.supersedes,
                    "superseded",
                    reason,
                )?;
                archived.append(&mut newly_archived);
            }
        }
        archived.sort();
        archived.dedup();
        created.sort();
        created.dedup();
    }

    Ok(DreamResponse {
        run_id,
        mode: params.mode.to_string(),
        profile: params.profile.as_str().to_string(),
        workspace: params.workspace.to_string(),
        repo_id: params.repo_id.map(|s| s.to_string()),
        now: params.now.to_string(),
        candidates,
        stale,
        rejected,
        archived,
        created,
        authority: "recall_not_authority".to_string(),
    })
}

fn stable_run_id(params: &DreamParams, records: &[MemoryRecord]) -> String {
    let mut seed = format!(
        "{}\x1f{}\x1f{}\x1f{}\x1f{}",
        params.profile.as_str(),
        params.workspace,
        params.repo_id.unwrap_or(""),
        params.mode,
        params.now
    );
    if let Some(source_window_start) = params.recency_cutoff {
        seed.push('\x1f');
        seed.push_str(source_window_start);
    }
    for record in records {
        seed.push('\x1f');
        seed.push_str(&record.id);
        seed.push('\x1f');
        seed.push_str(&record.updated_at);
        seed.push('\x1f');
        seed.push_str(&record.content);
    }
    let hash = ids::sha256_hex(seed.as_bytes());
    format!("dream_{}", &hash["sha256:".len()..39])
}

pub fn config_hash() -> String {
    ids::sha256_hex(format!("{DREAM_IMPLEMENTATION_VERSION}:{DREAM_RULESET_VERSION}").as_bytes())
}

pub fn source_counts(records: &[MemoryRecord]) -> serde_json::Value {
    let mut counts = BTreeMap::<String, usize>::new();
    for record in records {
        *counts
            .entry(record.record_type.as_str().to_string())
            .or_default() += 1;
    }
    json!(counts)
}

pub fn candidate_counts(response: &DreamResponse) -> serde_json::Value {
    let mut by_action = BTreeMap::<String, usize>::new();
    let mut by_policy = BTreeMap::<String, usize>::new();
    for candidate in &response.candidates {
        *by_action.entry(candidate.action.clone()).or_default() += 1;
        *by_policy.entry(candidate.policy.clone()).or_default() += 1;
    }
    json!({
        "by_action": by_action,
        "by_policy": by_policy,
    })
}

fn push_threshold_candidates(
    records: &[MemoryRecord],
    candidates: &mut Vec<DreamCandidate>,
    rejected: &mut Vec<DreamRejection>,
) {
    let mut groups: BTreeMap<String, Vec<&MemoryRecord>> = BTreeMap::new();
    for record in records {
        groups
            .entry(subject_key(&record.content))
            .or_default()
            .push(record);
    }
    for (subject, mut evidence) in groups {
        evidence.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let score = score_evidence(&evidence);
        if score.candidate_state == "rejected" {
            continue;
        }
        let Some(content) = threshold_content(&subject, &evidence, &score) else {
            continue;
        };
        let state = if evidence
            .iter()
            .any(|record| state_for_record(record) == "completed")
        {
            "completed"
        } else {
            "active"
        };
        let Some(last) = evidence.last().copied() else {
            continue;
        };
        push_candidate_with_score(
            candidates,
            rejected,
            last,
            "promote",
            &content,
            state,
            is_drift_prone(&content),
            None,
            None,
            vec![],
            score,
            &evidence,
        );
    }
}

fn score_evidence(evidence: &[&MemoryRecord]) -> EvidenceScore {
    let mut classes = evidence
        .iter()
        .map(|r| evidence_class(r))
        .collect::<Vec<_>>();
    if has_assistant_adoption(evidence) {
        for class in &mut classes {
            if *class == EvidenceClass::AssistantVisibleTurn {
                *class = EvidenceClass::AdoptedAssistantProposal;
            }
        }
    }
    let mut unique_classes = Vec::new();
    for class in &classes {
        if !unique_classes.contains(class) {
            unique_classes.push(*class);
        }
    }
    let weight = classes.iter().map(|class| class.weight()).sum::<f64>();
    let user_turns = classes
        .iter()
        .filter(|class| **class == EvidenceClass::UserVisibleTurn)
        .count();
    let conclusions = classes
        .iter()
        .filter(|class| **class == EvidenceClass::ExplicitConclusion)
        .count();
    let checkpoints = classes
        .iter()
        .filter(|class| **class == EvidenceClass::Checkpoint)
        .count();
    let adopted = classes
        .iter()
        .filter(|class| **class == EvidenceClass::AdoptedAssistantProposal)
        .count();
    let assistant_only = !classes.is_empty()
        && classes
            .iter()
            .all(|class| *class == EvidenceClass::AssistantVisibleTurn);
    let active_only = !classes.is_empty()
        && classes
            .iter()
            .all(|class| *class == EvidenceClass::ActiveMemory);
    let self_reinforcing = !classes.is_empty()
        && classes.iter().all(|class| {
            matches!(
                class,
                EvidenceClass::ImportedMemory | EvidenceClass::ActiveMemory
            )
        });
    let single_unconfirmed_preference =
        evidence.len() == 1 && evidence[0].record_type == crate::domain::RecordType::Preference;

    let (candidate_state, reason, apply_eligible) = if assistant_only {
        ("quarantined", "assistant_only_proposal_quarantined", false)
    } else if active_only {
        ("rejected", "active_memory_only", false)
    } else if self_reinforcing {
        (
            "quarantined",
            "imported_or_active_memory_without_fresh_primary_evidence",
            false,
        )
    } else if single_unconfirmed_preference {
        ("quarantined", "single_unconfirmed_preference", false)
    } else if conclusions > 0 {
        ("accepted", "explicit_conclusion", true)
    } else if checkpoints > 0 {
        ("accepted", "checkpoint_backed_task_state", true)
    } else if adopted > 0 {
        ("accepted", "user_adopted_assistant_proposal", true)
    } else if user_turns >= 2 || distinct_days(evidence) >= 2 {
        ("accepted", "repeated_user_steering", true)
    } else if weight >= 2.0 {
        ("accepted", "weighted_primary_evidence_threshold", true)
    } else {
        ("quarantined", "insufficient_primary_evidence", false)
    };

    EvidenceScore {
        classes: unique_classes,
        weight,
        reason: reason.to_string(),
        candidate_state: candidate_state.to_string(),
        apply_eligible,
    }
}

fn evidence_class(record: &MemoryRecord) -> EvidenceClass {
    let origin = record.metadata.get("origin").and_then(|v| v.as_str());
    let actor = record
        .metadata
        .get("actor")
        .or_else(|| record.metadata.get("target"))
        .and_then(|v| v.as_str());
    let artifact_kind = record
        .metadata
        .get("artifact_kind")
        .and_then(|v| v.as_str());
    if origin == Some("visible_turn") && actor == Some("user") {
        EvidenceClass::UserVisibleTurn
    } else if origin == Some("visible_turn") && actor == Some("assistant") {
        EvidenceClass::AssistantVisibleTurn
    } else if origin == Some("conclusion") {
        EvidenceClass::ExplicitConclusion
    } else if origin == Some("checkpoint") {
        EvidenceClass::Checkpoint
    } else if origin == Some("codex-local-memory") || artifact_kind == Some("memory_summary") {
        EvidenceClass::ImportedMemory
    } else {
        EvidenceClass::ActiveMemory
    }
}

fn has_assistant_adoption(evidence: &[&MemoryRecord]) -> bool {
    let mut saw_assistant = false;
    for record in evidence {
        match evidence_class(record) {
            EvidenceClass::AssistantVisibleTurn => saw_assistant = true,
            EvidenceClass::UserVisibleTurn if saw_assistant => {
                if contains_any(
                    &record.content.to_ascii_lowercase(),
                    &["yes", "do that", "use that", "adopt", "ship it", "go with"],
                ) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn threshold_content(
    subject: &str,
    evidence: &[&MemoryRecord],
    score: &EvidenceScore,
) -> Option<String> {
    if score.candidate_state == "accepted" {
        return evidence.last().map(|record| record.content.clone());
    }
    let latest = evidence.last()?;
    Some(format!(
        "Quarantined Dreamer candidate for subject `{subject}`: {}",
        summarize_for_threshold(&latest.content)
    ))
}

fn summarize_for_threshold(content: &str) -> String {
    content
        .split_whitespace()
        .take(18)
        .collect::<Vec<_>>()
        .join(" ")
}

fn distinct_days(evidence: &[&MemoryRecord]) -> usize {
    evidence
        .iter()
        .filter_map(|record| record.created_at.split('T').next())
        .collect::<BTreeSet<_>>()
        .len()
}

fn push_candidate(
    candidates: &mut Vec<DreamCandidate>,
    rejected: &mut Vec<DreamRejection>,
    evidence: &MemoryRecord,
    action: &str,
    content: &str,
    state: &str,
    drift_prone: bool,
    valid_until: Option<String>,
    historical_reason: Option<String>,
    supersedes: Vec<String>,
) {
    let evidence_refs = [evidence];
    let score = score_evidence(&evidence_refs);
    push_candidate_with_score(
        candidates,
        rejected,
        evidence,
        action,
        content,
        state,
        drift_prone,
        valid_until,
        historical_reason,
        supersedes,
        score,
        &evidence_refs,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_candidate_with_score(
    candidates: &mut Vec<DreamCandidate>,
    rejected: &mut Vec<DreamRejection>,
    evidence: &MemoryRecord,
    action: &str,
    content: &str,
    state: &str,
    drift_prone: bool,
    valid_until: Option<String>,
    historical_reason: Option<String>,
    supersedes: Vec<String>,
    score: EvidenceScore,
    evidence_records: &[&MemoryRecord],
) {
    match policy::screen_content(content, policy::MAX_RECORD_CHARS) {
        PolicyDecision::Accept(clean) => {
            let evidence_ids = evidence_records
                .iter()
                .flat_map(|record| evidence_ids(record))
                .collect::<Vec<_>>();
            let evidence_count = evidence_ids.len();
            let user_evidence_count = evidence_records
                .iter()
                .filter(|record| {
                    evidence_class(record) == EvidenceClass::UserVisibleTurn
                        || record.metadata.get("target").and_then(|v| v.as_str()) == Some("user")
                })
                .count();
            let assistant_evidence_count = evidence_records
                .iter()
                .filter(|record| {
                    evidence_class(record) == EvidenceClass::AssistantVisibleTurn
                        || record.metadata.get("target").and_then(|v| v.as_str())
                            == Some("assistant")
                })
                .count();
            let promotion_reason =
                promotion_reason(action, &historical_reason, Some(score.reason.as_str()));
            let evidence_classes = score
                .classes
                .iter()
                .map(|class| class.as_str().to_string())
                .collect::<Vec<_>>();
            let first_seen_at = evidence_records
                .first()
                .map(|record| record.created_at.clone())
                .unwrap_or_else(|| evidence.created_at.clone());
            let last_seen_at = evidence_records
                .last()
                .map(|record| record.updated_at.clone())
                .unwrap_or_else(|| evidence.updated_at.clone());
            candidates.push(DreamCandidate {
                action: action.to_string(),
                proposed_type: evidence.record_type.as_str().to_string(),
                content: clean,
                confidence: (evidence.confidence + 0.05).min(0.95),
                state: state.to_string(),
                drift_prone,
                expires_at: valid_until.clone(),
                valid_until,
                historical_reason,
                supersedes,
                policy: "accept".to_string(),
                candidate_state: score.candidate_state,
                subject_key: subject_key(&evidence.content),
                threshold_reason: score.reason,
                evidence_weight: score.weight,
                evidence_classes,
                evidence_ids,
                evidence_count,
                user_evidence_count,
                assistant_evidence_count,
                first_seen_at,
                last_seen_at,
                promotion_reason,
                apply_eligible: score.apply_eligible
                    && apply_eligible(evidence, evidence_count, assistant_evidence_count),
            })
        }
        PolicyDecision::Reject { reason, .. } => {
            rejected.push(DreamRejection { reason, supersedes })
        }
    }
}

fn evidence_ids(evidence: &MemoryRecord) -> Vec<String> {
    if evidence.source_ids.is_empty() {
        vec![evidence.id.clone()]
    } else {
        evidence.source_ids.clone()
    }
}

fn apply_eligible(
    evidence: &MemoryRecord,
    evidence_count: usize,
    assistant_evidence_count: usize,
) -> bool {
    if evidence_count > 0 && assistant_evidence_count == evidence_count {
        return false;
    }
    let origin = evidence.metadata.get("origin").and_then(|v| v.as_str());
    let artifact_kind = evidence
        .metadata
        .get("artifact_kind")
        .and_then(|v| v.as_str());
    !(origin == Some("codex-local-memory") && artifact_kind == Some("memory_summary"))
}

fn promotion_reason(
    action: &str,
    historical_reason: &Option<String>,
    threshold_reason: Option<&str>,
) -> String {
    historical_reason.clone().unwrap_or_else(|| {
        match action {
            "rewrite_historical" => "expired drift-prone memory rewritten as historical fact",
            "supersede" => "newer evidence supersedes older active memory",
            "promote" => threshold_reason.unwrap_or("deterministic promotion threshold met"),
            _ => threshold_reason.unwrap_or("dreamer candidate accepted by deterministic policy"),
        }
        .to_string()
    })
}

fn dedupe_candidates(candidates: &mut Vec<DreamCandidate>) {
    let mut seen = BTreeSet::new();
    candidates.retain(|c| {
        let key = format!("{}:{}:{:?}", c.action, normalize(&c.content), c.supersedes);
        seen.insert(key)
    });
}

pub fn is_drift_prone(content: &str) -> bool {
    RELATIVE_TIME.is_match(content)
}

fn state_for(content: &str) -> String {
    let lower = content.to_ascii_lowercase();
    if contains_any(
        &lower,
        &[
            "completed",
            "done",
            "fixed",
            "implemented",
            "merged",
            "deployed",
            "shipped",
            "resolved",
            "no longer",
        ],
    ) {
        "completed".to_string()
    } else if contains_any(&lower, &["blocked", "stuck", "waiting on", "failing"]) {
        "blocked".to_string()
    } else if contains_any(
        &lower,
        &[
            "planned",
            "planning to",
            "going to",
            "will ",
            "todo",
            "next step",
        ],
    ) {
        "planned".to_string()
    } else if contains_any(
        &lower,
        &["currently", "right now", "in progress", "working on"],
    ) {
        "active".to_string()
    } else {
        "active".to_string()
    }
}

fn state_for_record(record: &MemoryRecord) -> String {
    record
        .metadata
        .get("state")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| state_for(&record.content))
}

fn supersedes(newer: &MemoryRecord, older: &MemoryRecord, newer_state: &str) -> bool {
    if subject_key(&newer.content) != subject_key(&older.content) {
        return false;
    }
    if lexical_overlap(&newer.content, &older.content) < 1 {
        return false;
    }
    let old_state = state_for_record(older);
    let old_lower = older.content.to_ascii_lowercase();
    let new_lower = newer.content.to_ascii_lowercase();
    let old_unsettled = matches!(old_state.as_str(), "planned" | "blocked" | "active")
        || contains_any(
            &old_lower,
            &["tbd", "evaluating", "not decided", "will ", "planned"],
        );
    let new_settled = newer_state == "completed"
        || contains_any(
            &new_lower,
            &[
                "uses ",
                "decided",
                "no longer",
                "implemented",
                "fixed",
                "merged",
                "deployed",
            ],
        );
    old_unsettled && new_settled
}

fn same_boundary(a: &MemoryRecord, b: &MemoryRecord) -> bool {
    a.profile_id == b.profile_id && a.workspace_id == b.workspace_id && a.repo_id == b.repo_id
}

fn valid_until_for(record: &MemoryRecord) -> Option<String> {
    record
        .metadata
        .get("valid_until")
        .or_else(|| record.metadata.get("expires_at"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| infer_valid_until(&record.content, &record.created_at))
}

fn infer_valid_until(content: &str, created_at: &str) -> Option<String> {
    if !is_drift_prone(content) {
        return None;
    }
    let lower = content.to_ascii_lowercase();
    let base = parse_time(created_at)?;
    let days = if lower.contains("tomorrow") || lower.contains("tonight") {
        2
    } else if lower.contains("this week") || lower.contains("this weekend") {
        8
    } else if lower.contains("next week") {
        15
    } else {
        7
    };
    Some(format_time(base + Duration::days(days)))
}

fn subject_key(content: &str) -> String {
    tokens(content)
        .into_iter()
        .find(|t| !STOPWORDS.contains(&t.as_str()))
        .unwrap_or_else(|| normalize(content).chars().take(32).collect())
}

fn lexical_overlap(a: &str, b: &str) -> usize {
    let a = tokens(a).into_iter().collect::<BTreeSet<_>>();
    let b = tokens(b).into_iter().collect::<BTreeSet<_>>();
    a.intersection(&b)
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .count()
}

fn tokens(content: &str) -> Vec<String> {
    normalize(content)
        .split_whitespace()
        .filter(|t| t.len() > 2)
        .map(|t| t.to_string())
        .collect()
}

fn normalize(content: &str) -> String {
    content
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn is_after(a: &str, b: &str) -> bool {
    match (parse_time(a), parse_time(b)) {
        (Some(a), Some(b)) => a > b,
        _ => a > b,
    }
}

fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn format_time(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn date_part(value: &str) -> &str {
    value.split('T').next().unwrap_or(value)
}
