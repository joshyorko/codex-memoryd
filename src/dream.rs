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
    "right",
    "still",
    "summary",
    "that",
    "this",
    "uses",
    "will",
    "with",
];

pub struct DreamParams<'a> {
    pub profile: Profile,
    pub workspace: &'a str,
    pub repo_id: Option<&'a str>,
    pub mode: &'a str,
    pub now: &'a str,
    pub recency_cutoff: Option<&'a str>,
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
        include_archived: false,
        recency_cutoff: params.recency_cutoff.map(|s| s.to_string()),
        limit: params.max_records,
        offset: 0,
    })?;
    let mut candidates = Vec::new();
    let mut stale = Vec::new();
    let mut rejected = Vec::new();

    for record in &records {
        let drift_prone = is_drift_prone(&record.content);
        let state = state_for(&record.content);
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
        let newer_state = state_for(&newer.content);
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

    dedupe_candidates(&mut candidates);
    if let Some(max) = params.max_candidates {
        candidates.truncate(max);
    }

    let run_id = stable_run_id(params, &records);
    let mut archived = Vec::new();
    let mut created = Vec::new();
    if params.mode == "apply" {
        for candidate in &candidates {
            let class =
                policy::classify(&candidate.content, params.profile, params.repo_id.is_some());
            let content_hash = ids::content_hash(
                params.profile.as_str(),
                params.workspace,
                params.repo_id,
                class.record_type.as_str(),
                class.scope.as_str(),
                &candidate.content,
            );
            let metadata = json!({
                "origin": "dreamer",
                "run_id": run_id.clone(),
                "state": candidate.state,
                "drift_prone": candidate.drift_prone,
                "expires_at": candidate.expires_at,
                "valid_until": candidate.valid_until,
                "historical_reason": candidate.historical_reason,
                "supersedes": candidate.supersedes,
                "subject_key": subject_key(&candidate.content),
            });
            let outcome = store.upsert_record(&NewRecord {
                profile_id: params.profile.as_str().to_string(),
                workspace_id: params.workspace.to_string(),
                repo_id: params.repo_id.map(|s| s.to_string()),
                scope: class.scope,
                record_type: class.record_type,
                content: candidate.content.clone(),
                related_files: class.related_files,
                tags: class.tags,
                sensitivity: class.sensitivity,
                portability: class.portability,
                confidence: candidate.confidence,
                source_ids: vec![],
                content_hash,
                supersedes: candidate.supersedes.clone(),
                metadata,
            })?;
            created.push(outcome.id().to_string());
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
    match policy::screen_content(content, policy::MAX_RECORD_CHARS) {
        PolicyDecision::Accept(clean) => candidates.push(DreamCandidate {
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
        }),
        PolicyDecision::Reject { reason, .. } => {
            rejected.push(DreamRejection { reason, supersedes })
        }
    }
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

fn supersedes(newer: &MemoryRecord, older: &MemoryRecord, newer_state: &str) -> bool {
    if subject_key(&newer.content) != subject_key(&older.content) {
        return false;
    }
    if lexical_overlap(&newer.content, &older.content) < 1 {
        return false;
    }
    let old_state = state_for(&older.content);
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
