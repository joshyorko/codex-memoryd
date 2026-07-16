//! Deterministic Dreamer heuristics for staleness, state transitions, and
//! supersession. This module is intentionally policy/store-backed and does not
//! call an LLM.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::time::Instant;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::json;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::Duration;
use time::OffsetDateTime;

use crate::domain::Checkpoint;
use crate::domain::Conclusion;
use crate::domain::MemoryRecord;
use crate::domain::MemorySource;
use crate::domain::Profile;
use crate::domain::TemporalState;
use crate::domain::VisibleTurn;
use crate::error::Result;
use crate::ids;
use crate::policy;
use crate::policy::PolicyDecision;
use crate::protocol::DreamCandidate;
use crate::protocol::DreamEvidenceSource;
use crate::protocol::DreamEvidenceStream;
use crate::protocol::DreamEvidenceWindow;
use crate::protocol::DreamObservation;
use crate::protocol::DreamRejection;
use crate::protocol::DreamResponse;
use crate::protocol::DreamStaleRecord;
use crate::store::ledger_safe_summary;
use crate::store::EvidenceLedgerEntry;
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

const SUBJECT_NOISE_WORDS: &[&str] = &[
    "about",
    "after",
    "again",
    "and",
    "being",
    "blocked",
    "completed",
    "currently",
    "decision",
    "deploy",
    "deployed",
    "earlier",
    "done",
    "evaluating",
    "fix",
    "fixed",
    "going",
    "implement",
    "implemented",
    "into",
    "is",
    "later",
    "longer",
    "merge",
    "merged",
    "new",
    "no",
    "not",
    "now",
    "old",
    "options",
    "planned",
    "planning",
    "please",
    "proposal",
    "resolved",
    "resolve",
    "right",
    "run",
    "soon",
    "still",
    "state",
    "summary",
    "superseding",
    "that",
    "this",
    "the",
    "then",
    "there",
    "these",
    "those",
    "though",
    "tbd",
    "today",
    "tomorrow",
    "tonight",
    "week",
    "use",
    "uses",
    "using",
    "will",
    "with",
    "yes",
    "next",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    BattleScar,
    ComfortPath,
    Surprise,
    RecoveryPattern,
    ConfidenceDelta,
}

impl MarkerKind {
    fn as_str(self) -> &'static str {
        match self {
            MarkerKind::BattleScar => "battle_scar",
            MarkerKind::ComfortPath => "comfort_path",
            MarkerKind::Surprise => "surprise",
            MarkerKind::RecoveryPattern => "recovery_pattern",
            MarkerKind::ConfidenceDelta => "confidence_delta",
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

#[derive(Debug, Clone)]
struct MarkerValence {
    marker_type: &'static str,
    operational_valence: &'static str,
    intensity: f64,
    confidence_delta: f64,
    decay_half_life_days: f64,
}

impl MarkerValence {
    fn for_kind(kind: MarkerKind) -> Self {
        match kind {
            MarkerKind::BattleScar => Self {
                marker_type: "operational_valence",
                operational_valence: "negative",
                intensity: 0.9,
                confidence_delta: -0.2,
                decay_half_life_days: 30.0,
            },
            MarkerKind::ComfortPath => Self {
                marker_type: "operational_valence",
                operational_valence: "positive",
                intensity: 0.7,
                confidence_delta: 0.15,
                decay_half_life_days: 45.0,
            },
            MarkerKind::Surprise => Self {
                marker_type: "operational_valence",
                operational_valence: "mixed",
                intensity: 0.5,
                confidence_delta: 0.0,
                decay_half_life_days: 30.0,
            },
            MarkerKind::RecoveryPattern => Self {
                marker_type: "operational_valence",
                operational_valence: "positive",
                intensity: 0.6,
                confidence_delta: 0.1,
                decay_half_life_days: 45.0,
            },
            MarkerKind::ConfidenceDelta => Self {
                marker_type: "operational_valence",
                operational_valence: "mixed",
                intensity: 0.4,
                confidence_delta: 0.05,
                decay_half_life_days: 30.0,
            },
        }
    }
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
    pub patch_run_id: Option<&'a str>,
    pub deadline: Option<Instant>,
}

pub fn run(store: &Store, params: &DreamParams) -> Result<(DreamResponse, bool)> {
    check_deadline(params)?;
    let mut records = store.query_records(&RecordQuery {
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
    check_deadline(params)?;
    let imported_limit = params.max_records.saturating_sub(records.len());
    if imported_limit > 0 {
        records.extend(imported_chatgpt_candidate_records(
            store,
            params,
            imported_limit,
        )?);
    }
    check_deadline(params)?;
    records.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    let evidence_window =
        build_evidence_window(store, params, params.recency_cutoff, params.now, &records)?;
    let mut candidates = Vec::new();
    let mut stale = Vec::new();
    let mut rejected = Vec::new();

    check_deadline(params)?;

    for record in &records {
        check_deadline(params)?;
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

    check_deadline(params)?;

    for newer in &records {
        check_deadline(params)?;
        let newer_state = state_for_record(newer);
        for older in &records {
            check_deadline(params)?;
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
        push_threshold_candidates(params, &records, &mut candidates, &mut rejected)?;
    }

    check_deadline(params)?;
    attach_counter_evidence_retires(&records, &mut candidates);
    dedupe_candidates(&mut candidates);
    let mut max_candidates_hit = false;
    if let Some(max) = params.max_candidates {
        if let Some(limit_for_processing) = max.checked_add(1) {
            candidates.truncate(limit_for_processing);
            max_candidates_hit = candidates.len() > max;
            if max_candidates_hit {
                candidates.truncate(max);
            }
        } else {
            candidates.truncate(max);
        }
    }

    check_deadline(params)?;
    let run_id = stable_run_id(params, &evidence_window, &records);
    let mut archived = Vec::new();
    let mut created = Vec::new();
    if params.mode == "apply" {
        for candidate in &candidates {
            if !candidate.apply_eligible {
                continue;
            }
            let marker = marker_from_candidate(candidate, params.now);
            let content = match policy::screen_content(&candidate.content, policy::MAX_RECORD_CHARS)
            {
                PolicyDecision::Accept(clean) => clean,
                PolicyDecision::Reject { code, reason } => {
                    let source_hash = ids::sha256_hex(
                        format!(
                            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                            params.profile.as_str(),
                            params.workspace,
                            candidate.subject_key,
                            code,
                            ids::sha256_hex(candidate.content.as_bytes())
                        )
                        .as_bytes(),
                    );
                    let _ = store.record_evidence_ledger(&EvidenceLedgerEntry {
                        profile_id: params.profile.as_str().to_string(),
                        workspace_id: params.workspace.to_string(),
                        repo_id: params.repo_id.map(|s| s.to_string()),
                        subject_key: Some(candidate.subject_key.clone()),
                        source_kind: "dream_apply".to_string(),
                        source_id: candidate.evidence_ids.first().cloned(),
                        source_path: Some(format!("dream:{}", candidate.subject_key)),
                        source_hash,
                        safe_summary: ledger_safe_summary(&format!(
                            "rejected dream candidate {} for {}: {}",
                            candidate.action, candidate.subject_key, reason
                        )),
                        policy_state: code.clone(),
                        metadata: json!({
                            "dream_run_id": run_id.clone(),
                            "patch_run_id": params.patch_run_id,
                            "action": candidate.action,
                            "subject_key": candidate.subject_key,
                            "reason": reason,
                        }),
                    });
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
            let observation = observation_from_candidate(candidate);
            let observation_id = observation.id.clone();
            let observation_refs = observation.evidence_refs.clone();
            let observation_metadata = observation_metadata_json(&observation, marker.as_ref());
            let safe_summary = ledger_safe_summary(&candidate.content);
            let source_id = candidate.evidence_ids.first().cloned();
            let mut metadata = json!({
                "origin": "dreamer",
                "dream_run_id": run_id.clone(),
                "run_id": run_id.clone(),
                "patch_run_id": params.patch_run_id,
                "policy_outcome": candidate.candidate_state,
                "subject_key": candidate.subject_key,
                "candidate_state": candidate.candidate_state,
                "threshold_reason": candidate.threshold_reason,
                "evidence_weight": candidate.evidence_weight,
                "evidence_classes": candidate.evidence_classes,
                "evidence_ids": candidate.evidence_ids,
                "evidence_refs": candidate.evidence_refs,
                "retires": candidate.retires,
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
                "observation_id": observation_id,
                "observation_refs": observation_refs,
                "observation": observation_metadata,
            });
            if let Some(marker) = &marker {
                metadata["marker"] = json!(marker);
            }
            let outcome = store.upsert_record(&NewRecord {
                profile_id: params.profile.as_str().to_string(),
                workspace_id: params.workspace.to_string(),
                repo_id: params.repo_id.map(|s| s.to_string()),
                subject_id: None,
                episode_id: None,
                scope: class.scope,
                record_type: class.record_type,
                content,
                related_files: class.related_files,
                tags: class.tags,
                sensitivity: class.sensitivity,
                portability: class.portability,
                confidence: candidate.confidence,
                source_ids: candidate.evidence_ids.clone(),
                content_hash: content_hash.clone(),
                supersedes: candidate.supersedes.clone(),
                metadata,
            })?;
            store.record_evidence_ledger(&EvidenceLedgerEntry {
                profile_id: params.profile.as_str().to_string(),
                workspace_id: params.workspace.to_string(),
                repo_id: params.repo_id.map(|s| s.to_string()),
                subject_key: Some(candidate.subject_key.clone()),
                source_kind: "dream_apply".to_string(),
                source_id,
                source_path: Some(format!("dream:{}", candidate.subject_key)),
                source_hash: content_hash,
                safe_summary,
                policy_state: "accepted".to_string(),
                metadata: json!({
                    "dream_run_id": run_id.clone(),
                    "patch_run_id": params.patch_run_id,
                    "action": candidate.action,
                    "subject_key": candidate.subject_key,
                    "promotion_reason": candidate.promotion_reason,
                    "candidate_state": candidate.candidate_state,
                    "evidence_count": candidate.evidence_count,
                    "marker": marker,
                }),
            })?;
            if let UpsertOutcome::Created(id) = outcome {
                created.push(id);
            }
            if !candidate.supersedes.is_empty() {
                let reason = candidate
                    .historical_reason
                    .as_deref()
                    .unwrap_or("superseded by newer Dreamer evidence");
                let archive_state = if marker
                    .as_ref()
                    .is_some_and(|marker| !marker.counter_evidence_refs.is_empty())
                {
                    "counter_evidence"
                } else {
                    "superseded"
                };
                let (mut newly_archived, _) = store.archive_records_with_metadata_at(
                    params.profile.as_str(),
                    Some(params.workspace),
                    &candidate.supersedes,
                    archive_state,
                    reason,
                    params.patch_run_id,
                    params.now,
                )?;
                archived.append(&mut newly_archived);
            }
        }
        archived.sort();
        archived.dedup();
        created.sort();
        created.dedup();
    }
    let observations = observations_from_candidates(&candidates);
    let markers = markers_from_candidates(&candidates, params.now);

    Ok((
        DreamResponse {
            run_id,
            mode: params.mode.to_string(),
            profile: params.profile.as_str().to_string(),
            workspace: params.workspace.to_string(),
            repo_id: params.repo_id.map(|s| s.to_string()),
            now: params.now.to_string(),
            evidence_window,
            candidates,
            observations,
            markers,
            stale,
            rejected,
            archived,
            created,
            authority: "recall_not_authority".to_string(),
        },
        max_candidates_hit,
    ))
}

fn imported_chatgpt_candidate_records(
    store: &Store,
    params: &DreamParams,
    limit: usize,
) -> Result<Vec<MemoryRecord>> {
    let turns = store.dream_visible_turns(
        params.profile.as_str(),
        params.workspace,
        params.repo_id,
        params.recency_cutoff,
        params.max_records,
    )?;
    let mut records = Vec::new();
    for turn in turns {
        if records.len() >= limit {
            break;
        }
        if turn.metadata.get("origin").and_then(|value| value.as_str()) != Some("chatgpt-export") {
            continue;
        }
        let class = policy::classify(&turn.content, params.profile, params.repo_id.is_some());
        if !matches!(
            class.record_type,
            crate::domain::RecordType::Preference
                | crate::domain::RecordType::Decision
                | crate::domain::RecordType::Command
                | crate::domain::RecordType::Gotcha
                | crate::domain::RecordType::RepoConvention
        ) {
            continue;
        }
        let source_path = turn
            .metadata
            .get("message_id")
            .and_then(|value| value.as_str())
            .map(|message_id| {
                let conversation_id = turn
                    .metadata
                    .get("conversation_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                format!("chatgpt:{conversation_id}:{message_id}")
            });
        records.push(MemoryRecord {
            id: format!("dreamsrc_{}", ids::sha256_hex(turn.id.as_bytes())),
            profile_id: params.profile.as_str().to_string(),
            workspace_id: params.workspace.to_string(),
            repo_id: params.repo_id.map(str::to_string),
            subject_id: None,
            episode_id: None,
            scope: class.scope,
            record_type: class.record_type,
            content: turn.content.clone(),
            related_files: class.related_files,
            tags: class.tags,
            sensitivity: class.sensitivity,
            portability: class.portability,
            confidence: class.confidence,
            source_ids: vec![turn.id.clone()],
            content_hash: ids::content_hash(
                params.profile.as_str(),
                params.workspace,
                params.repo_id,
                class.record_type.as_str(),
                class.scope.as_str(),
                &turn.content,
            ),
            supersedes: vec![],
            created_at: turn.created_at.clone(),
            updated_at: turn.created_at.clone(),
            last_used_at: None,
            archived: false,
            trust_state: "trusted".to_string(),
            trust_score: 1.0,
            quarantine_reason: None,
            quarantined_at: None,
            promoted_at: None,
            valid_from: None,
            valid_until: None,
            observed_at: Some(turn.created_at.clone()),
            invalidated_at: None,
            superseded_by: None,
            historical_reason: None,
            temporal_state: TemporalState::Current,
            metadata: json!({
                "origin": "visible_turn",
                "source": "chatgpt-export",
                "source_id": turn.id,
                "source_path": source_path,
                "actor": turn.actor,
            }),
        });
    }
    Ok(records)
}

fn build_evidence_window(
    store: &Store,
    params: &DreamParams,
    start: Option<&str>,
    end: &str,
    active_records: &[MemoryRecord],
) -> Result<DreamEvidenceWindow> {
    let visible_turns = store.dream_visible_turns(
        params.profile.as_str(),
        params.workspace,
        params.repo_id,
        start,
        params.max_records,
    )?;
    let conclusions = store.dream_conclusions(
        params.profile.as_str(),
        params.workspace,
        params.repo_id,
        start,
        params.max_records,
    )?;
    let checkpoints = store.dream_checkpoints(
        params.profile.as_str(),
        params.workspace,
        params.repo_id,
        start,
        params.max_records,
    )?;
    let imported_memories = store.dream_memory_sources(
        params.profile.as_str(),
        params.workspace,
        start,
        params.max_records,
    )?;

    Ok(DreamEvidenceWindow {
        start: start.map(str::to_string),
        end: end.to_string(),
        visible_turns: stream_from_visible_turns(&visible_turns),
        conclusions: stream_from_conclusions(&conclusions),
        checkpoints: stream_from_checkpoints(&checkpoints),
        imported_memories: stream_from_sources(&imported_memories),
        active_memory_records: stream_from_memory_records(active_records),
    })
}

fn stream_from_visible_turns(records: &[VisibleTurn]) -> DreamEvidenceStream {
    DreamEvidenceStream {
        count: records.len(),
        sources: records
            .iter()
            .map(|record| DreamEvidenceSource {
                id: record.id.clone(),
                kind: "visible_turn".to_string(),
                created_at: record.created_at.clone(),
                updated_at: None,
                actor: Some(record.actor.clone()),
                record_type: None,
                state: None,
                source_path: Some(format!("turn:{}", record.session_id)),
                summary: Some("visible_turn".to_string()),
            })
            .collect(),
    }
}

fn stream_from_conclusions(records: &[Conclusion]) -> DreamEvidenceStream {
    DreamEvidenceStream {
        count: records.len(),
        sources: records
            .iter()
            .map(|record| DreamEvidenceSource {
                id: record.id.clone(),
                kind: "conclusion".to_string(),
                created_at: record.created_at.clone(),
                updated_at: None,
                actor: Some(record.target.clone()),
                record_type: None,
                state: None,
                source_path: record.source_id.clone(),
                summary: Some("conclusion".to_string()),
            })
            .collect(),
    }
}

fn stream_from_checkpoints(records: &[Checkpoint]) -> DreamEvidenceStream {
    DreamEvidenceStream {
        count: records.len(),
        sources: records
            .iter()
            .map(|record| DreamEvidenceSource {
                id: record.id.clone(),
                kind: "checkpoint".to_string(),
                created_at: record.created_at.clone(),
                updated_at: None,
                actor: None,
                record_type: Some("task_checkpoint".to_string()),
                state: None,
                source_path: record
                    .session_id
                    .as_ref()
                    .map(|session_id| format!("session:{session_id}")),
                summary: Some("checkpoint".to_string()),
            })
            .collect(),
    }
}

fn stream_from_sources(records: &[MemorySource]) -> DreamEvidenceStream {
    DreamEvidenceStream {
        count: records.len(),
        sources: records
            .iter()
            .map(|record| DreamEvidenceSource {
                id: record.id.clone(),
                kind: record.kind.clone(),
                created_at: record.created_at.clone(),
                updated_at: Some(record.ingested_at.clone()),
                actor: None,
                record_type: None,
                state: None,
                source_path: record.source_path.clone(),
                summary: Some(record.kind.clone()),
            })
            .collect(),
    }
}

fn stream_from_memory_records(records: &[MemoryRecord]) -> DreamEvidenceStream {
    DreamEvidenceStream {
        count: records.len(),
        sources: records
            .iter()
            .map(|record| DreamEvidenceSource {
                id: record.id.clone(),
                kind: "memory_record".to_string(),
                created_at: record.created_at.clone(),
                updated_at: Some(record.updated_at.clone()),
                actor: None,
                record_type: Some(record.record_type.as_str().to_string()),
                state: Some(state_for_record(record)),
                source_path: None,
                summary: Some(format!(
                    "{}:{}",
                    record.record_type.as_str(),
                    state_for_record(record)
                )),
            })
            .collect(),
    }
}

fn stable_run_id(
    params: &DreamParams,
    evidence_window: &DreamEvidenceWindow,
    records: &[MemoryRecord],
) -> String {
    let mut seed = String::new();
    push_seed_part(&mut seed, params.profile.as_str());
    push_seed_part(&mut seed, params.workspace);
    push_seed_part(&mut seed, params.repo_id.unwrap_or(""));
    push_seed_part(&mut seed, params.mode);
    push_seed_part(&mut seed, evidence_window.start.as_deref().unwrap_or(""));
    push_seed_part(&mut seed, &evidence_window.end);
    push_evidence_stream_seed(
        &mut seed,
        "visible_turns",
        &evidence_window.visible_turns,
        false,
    );
    push_evidence_stream_seed(
        &mut seed,
        "conclusions",
        &evidence_window.conclusions,
        false,
    );
    push_evidence_stream_seed(
        &mut seed,
        "checkpoints",
        &evidence_window.checkpoints,
        false,
    );
    push_evidence_stream_seed(
        &mut seed,
        "imported_memories",
        &evidence_window.imported_memories,
        true,
    );
    let mut ordered_records = records.iter().collect::<Vec<_>>();
    ordered_records.sort_by(|a, b| {
        a.updated_at
            .cmp(&b.updated_at)
            .then(a.id.cmp(&b.id))
            .then(a.content_hash.cmp(&b.content_hash))
    });
    for record in ordered_records {
        push_seed_part(&mut seed, &record.id);
        push_seed_part(&mut seed, &record.updated_at);
        push_seed_part(&mut seed, &record.content_hash);
    }
    let hash = ids::sha256_hex(seed.as_bytes());
    format!("dream_{}", &hash["sha256:".len()..39])
}

fn push_seed_part(seed: &mut String, value: &str) {
    seed.push('\x1f');
    seed.push_str(value);
}

fn push_evidence_stream_seed(
    seed: &mut String,
    label: &str,
    stream: &DreamEvidenceStream,
    include_updated_at: bool,
) {
    push_seed_part(seed, label);
    push_seed_part(seed, &stream.count.to_string());
    let mut sources = stream.sources.iter().collect::<Vec<_>>();
    sources.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then(a.created_at.cmp(&b.created_at))
            .then(a.updated_at.cmp(&b.updated_at))
    });
    for source in sources {
        push_seed_part(seed, &source.id);
        push_seed_part(seed, &source.created_at);
        if include_updated_at {
            push_seed_part(seed, source.updated_at.as_deref().unwrap_or(""));
        }
    }
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
    params: &DreamParams,
    records: &[MemoryRecord],
    candidates: &mut Vec<DreamCandidate>,
    rejected: &mut Vec<DreamRejection>,
) -> Result<()> {
    check_deadline(params)?;
    let mut boundary_groups: BTreeMap<(String, String, Option<String>), Vec<&MemoryRecord>> =
        BTreeMap::new();
    for record in records {
        check_deadline(params)?;
        boundary_groups
            .entry((
                record.profile_id.clone(),
                record.workspace_id.clone(),
                record.repo_id.clone(),
            ))
            .or_default()
            .push(record);
    }

    for mut boundary_records in boundary_groups.into_values() {
        check_deadline(params)?;
        boundary_records.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let mut groups = subject_groups(params, &boundary_records)?;
        groups.sort_by(|a, b| {
            a.first()
                .unwrap()
                .created_at
                .cmp(&b.first().unwrap().created_at)
                .then(a.first().unwrap().id.cmp(&b.first().unwrap().id))
        });

        for evidence in groups {
            check_deadline(params)?;
            let subject = subject_key_for_record(evidence.last().unwrap());
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
    Ok(())
}

fn subject_groups<'a>(
    params: &DreamParams,
    records: &'a [&'a MemoryRecord],
) -> Result<Vec<Vec<&'a MemoryRecord>>> {
    let mut groups: Vec<Vec<&'a MemoryRecord>> = Vec::new();
    for record in records {
        check_deadline(params)?;
        if let Some(group) = groups
            .iter_mut()
            .find(|group| same_subject(group[0], record))
        {
            group.push(*record);
        } else {
            groups.push(vec![*record]);
        }
    }
    Ok(groups)
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
            EvidenceClass::UserVisibleTurn
                if saw_assistant
                    && contains_any(
                        &record.content.to_ascii_lowercase(),
                        &["yes", "do that", "use that", "adopt", "ship it", "go with"],
                    ) =>
            {
                return true;
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

#[allow(clippy::too_many_arguments)]
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
            let evidence_refs = evidence_records
                .iter()
                .map(|record| evidence_ref(record))
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
            let candidate_supersedes = supersedes.clone();
            let retires = supersedes;
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
                supersedes: candidate_supersedes,
                policy: "accept".to_string(),
                candidate_state: score.candidate_state,
                subject_key: subject_key_for_record(evidence),
                threshold_reason: score.reason,
                evidence_weight: score.weight,
                evidence_classes,
                evidence_ids,
                evidence_refs,
                retires,
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

fn evidence_ref(record: &MemoryRecord) -> DreamEvidenceSource {
    let kind = record
        .metadata
        .get("origin")
        .and_then(|value| value.as_str())
        .map(|origin| match origin {
            "visible_turn" => "visible_turn",
            "conclusion" => "conclusion",
            "checkpoint" => "checkpoint",
            "codex-local-memory" => "imported_memory",
            _ => "memory_record",
        })
        .unwrap_or("memory_record")
        .to_string();
    DreamEvidenceSource {
        id: record.id.clone(),
        kind,
        created_at: record.created_at.clone(),
        updated_at: Some(record.updated_at.clone()),
        actor: record
            .metadata
            .get("actor")
            .or_else(|| record.metadata.get("target"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        record_type: Some(record.record_type.as_str().to_string()),
        state: Some(state_for_record(record)),
        source_path: record
            .metadata
            .get("source_path")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        summary: Some(format!(
            "{}:{}",
            record.record_type.as_str(),
            state_for_record(record)
        )),
    }
}

fn observations_from_candidates(candidates: &[DreamCandidate]) -> Vec<DreamObservation> {
    candidates.iter().map(observation_from_candidate).collect()
}

fn markers_from_candidates(candidates: &[DreamCandidate], as_of: &str) -> Vec<DreamObservation> {
    candidates
        .iter()
        .filter_map(|candidate| marker_from_candidate(candidate, as_of))
        .collect()
}

fn observation_from_candidate(candidate: &DreamCandidate) -> DreamObservation {
    let id = stable_observation_id(candidate);
    DreamObservation {
        id: id.clone(),
        key: id,
        kind: "dream_observation".to_string(),
        marker_kind: None,
        marker_type: None,
        operational_valence: None,
        intensity: None,
        decayed_intensity: None,
        confidence_delta: None,
        decay_half_life_days: None,
        category: candidate.candidate_state.clone(),
        subject_key: candidate.subject_key.clone(),
        summary: ledger_safe_summary(&candidate.content),
        content: candidate.content.clone(),
        confidence: candidate.confidence,
        state: candidate.state.clone(),
        trigger: None,
        trigger_json: None,
        outcome: None,
        outcome_json: None,
        recovery: None,
        recovery_json: None,
        future_guidance: None,
        evidence_refs: candidate.evidence_refs.clone(),
        retires: candidate.supersedes.clone(),
        counter_evidence_refs: Vec::new(),
        retired_at: None,
        first_seen_at: candidate.first_seen_at.clone(),
        last_seen_at: candidate.last_seen_at.clone(),
        authority: "recall_not_authority".to_string(),
        policy: candidate.policy.clone(),
        apply_eligible: candidate.apply_eligible,
    }
}

fn marker_from_candidate(candidate: &DreamCandidate, as_of: &str) -> Option<DreamObservation> {
    let marker_kind = marker_kind_for_candidate(candidate)?;
    let id = stable_marker_id(candidate, marker_kind);
    let (trigger, outcome, recovery, future_guidance) = marker_details(marker_kind, candidate);
    let valence = MarkerValence::for_kind(marker_kind);
    let decayed_intensity = decayed_intensity(
        valence.intensity,
        valence.decay_half_life_days,
        &candidate.first_seen_at,
        as_of,
    );
    let counter_evidence_refs = if valence.operational_valence == "positive" {
        candidate.retires.clone()
    } else {
        Vec::new()
    };
    Some(DreamObservation {
        id: id.clone(),
        key: id,
        kind: "dream_observation".to_string(),
        marker_kind: Some(marker_kind.as_str().to_string()),
        marker_type: Some(valence.marker_type.to_string()),
        operational_valence: Some(valence.operational_valence.to_string()),
        intensity: Some(valence.intensity),
        decayed_intensity: Some(decayed_intensity),
        confidence_delta: Some(valence.confidence_delta),
        decay_half_life_days: Some(valence.decay_half_life_days),
        category: candidate.candidate_state.clone(),
        subject_key: candidate.subject_key.clone(),
        summary: ledger_safe_summary(&trigger),
        content: candidate.content.clone(),
        confidence: candidate.confidence,
        state: candidate.state.clone(),
        trigger: Some(trigger),
        trigger_json: Some(json!({
            "summary": ledger_safe_summary(&candidate.content),
            "subject_key": candidate.subject_key,
            "evidence_count": candidate.evidence_count,
        })),
        outcome: Some(outcome),
        outcome_json: Some(json!({
            "state": candidate.state,
            "candidate_state": candidate.candidate_state,
            "confidence": candidate.confidence,
        })),
        recovery: Some(recovery),
        recovery_json: Some(json!({
            "historical_reason": candidate.historical_reason,
            "promotion_reason": candidate.promotion_reason,
        })),
        future_guidance: Some(future_guidance),
        evidence_refs: candidate.evidence_refs.clone(),
        retires: candidate.retires.clone(),
        counter_evidence_refs,
        retired_at: None,
        first_seen_at: candidate.first_seen_at.clone(),
        last_seen_at: candidate.last_seen_at.clone(),
        authority: "recall_not_authority".to_string(),
        policy: candidate.policy.clone(),
        apply_eligible: candidate.apply_eligible,
    })
}

fn decayed_intensity(
    intensity: f64,
    half_life_days: f64,
    first_seen_at: &str,
    last_seen_at: &str,
) -> f64 {
    let elapsed_days = match (
        OffsetDateTime::parse(first_seen_at, &Rfc3339),
        OffsetDateTime::parse(last_seen_at, &Rfc3339),
    ) {
        (Ok(first), Ok(last)) => (last - first).whole_days().max(0) as f64,
        _ => 0.0,
    };
    let decayed = if half_life_days > 0.0 {
        intensity * 0.5_f64.powf(elapsed_days / half_life_days)
    } else {
        intensity
    };
    (decayed * 1000.0).round() / 1000.0
}

fn stable_observation_id(candidate: &DreamCandidate) -> String {
    ids::sha256_hex(
        format!(
            "dream_observation:{}:{}:{}:{}:{}:{}",
            candidate.subject_key,
            candidate.action,
            candidate.proposed_type,
            candidate.first_seen_at,
            candidate.last_seen_at,
            candidate.evidence_ids.join(",")
        )
        .as_bytes(),
    )
}

fn stable_marker_id(candidate: &DreamCandidate, kind: MarkerKind) -> String {
    ids::sha256_hex(
        format!(
            "dream_marker:{}:{}:{}:{}:{}:{}:{}",
            kind.as_str(),
            candidate.subject_key,
            candidate.action,
            candidate.proposed_type,
            candidate.first_seen_at,
            candidate.last_seen_at,
            candidate.evidence_ids.join(","),
        )
        .as_bytes(),
    )
}

fn marker_kind_for_candidate(candidate: &DreamCandidate) -> Option<MarkerKind> {
    let content = candidate.content.to_ascii_lowercase();
    if has_explicit_surprise_language(&content) || has_correction_with_surprise_language(&content) {
        Some(MarkerKind::Surprise)
    } else if contains_any(
        &content,
        &[
            "battle scar",
            "battle_scar",
            "failed",
            "failure",
            "broken",
            "broke",
        ],
    ) && contains_any(
        &content,
        &[
            "recover",
            "recovered",
            "recovery",
            "retry",
            "fallback",
            "resume",
            "backoff",
            "unblock",
            "switching",
        ],
    ) {
        Some(MarkerKind::BattleScar)
    } else if contains_any(
        &content,
        &[
            "confidence delta",
            "confidence_delta",
            "more confident",
            "less confident",
            "confidence increased",
            "confidence dropped",
        ],
    ) {
        Some(MarkerKind::ConfidenceDelta)
    } else if contains_any(
        &content,
        &[
            "comfort path",
            "comfort_path",
            "known good",
            "known-good",
            "default path",
            "preferred path",
            "go-to",
            "repeatable",
        ],
    ) || matches!(
        candidate.promotion_reason.as_str(),
        "repeated_user_steering" | "user_adopted_assistant_proposal"
    ) || repeated_smooth_success(candidate, &content)
    {
        Some(MarkerKind::ComfortPath)
    } else if contains_any(
        &content,
        &[
            "recovery pattern",
            "recovery_pattern",
            "recover",
            "recovered",
            "retry",
            "fallback",
            "resume",
            "backoff",
            "unblock",
        ],
    ) {
        Some(MarkerKind::RecoveryPattern)
    } else {
        None
    }
}

fn repeated_smooth_success(candidate: &DreamCandidate, content: &str) -> bool {
    candidate.evidence_count >= 2
        && contains_any(
            content,
            &[
                "passed cleanly",
                "worked smoothly",
                "stable path",
                "repeatable success",
                "smooth success",
                "green twice",
            ],
        )
}

fn has_explicit_surprise_language(content: &str) -> bool {
    contains_any(
        content,
        &[
            "surprise",
            "surprising",
            "unexpected",
            "counterintuitive",
            "turns out",
            "didn't expect",
            "unexpectedly",
        ],
    )
}

fn has_correction_with_surprise_language(content: &str) -> bool {
    contains_any(content, &["actually", "correction", "corrected", "instead"])
        && contains_any(
            content,
            &[
                "surprise",
                "surprising",
                "unexpected",
                "counterintuitive",
                "turns out",
                "didn't expect",
                "unexpectedly",
            ],
        )
}

fn marker_details(
    kind: MarkerKind,
    candidate: &DreamCandidate,
) -> (String, String, String, String) {
    let trigger = ledger_safe_summary(&candidate.content);
    match kind {
        MarkerKind::BattleScar => (
            format!("Battle scar trigger: {trigger}"),
            "This records the failure path that left a scar.".to_string(),
            candidate.historical_reason.clone().unwrap_or_else(|| {
                "Recovered by the fallback path recorded in the ledger.".to_string()
            }),
            "Keep the fallback path handy and document the failure mode.".to_string(),
        ),
        MarkerKind::ComfortPath => (
            format!("Comfort path: {trigger}"),
            "This is the known-good path that keeps working.".to_string(),
            "No special recovery required.".to_string(),
            "Use this path by default unless fresher evidence says otherwise.".to_string(),
        ),
        MarkerKind::Surprise => (
            format!("Surprise: {trigger}"),
            "The unexpected result was useful enough to keep.".to_string(),
            "Recheck if the surprise repeats under new conditions.".to_string(),
            "Treat this as a caveat and validate before depending on it.".to_string(),
        ),
        MarkerKind::RecoveryPattern => (
            format!("Recovery pattern: {trigger}"),
            "The recovery sequence restored progress.".to_string(),
            candidate
                .historical_reason
                .clone()
                .unwrap_or_else(|| "Retry or fallback got the work unstuck.".to_string()),
            "Run the recovery sequence first when the same failure returns.".to_string(),
        ),
        MarkerKind::ConfidenceDelta => (
            format!("Confidence delta: {trigger}"),
            format!("Confidence moved to {:.2}.", candidate.confidence),
            format!(
                "The evidence mix now includes {} refs.",
                candidate.evidence_refs.len()
            ),
            "Look for the same evidence mix before changing confidence again.".to_string(),
        ),
    }
}

fn observation_metadata_json(
    observation: &DreamObservation,
    marker: Option<&DreamObservation>,
) -> Value {
    let mut value = json!({
        "id": observation.id,
        "key": observation.key,
        "kind": observation.kind,
        "category": observation.category,
        "subject_key": observation.subject_key,
        "summary": observation.summary,
        "content": observation.content,
        "confidence": observation.confidence,
        "state": observation.state,
        "authority": observation.authority,
        "policy": observation.policy,
        "apply_eligible": observation.apply_eligible,
    });
    if let Some(marker) = marker {
        value["marker_kind"] = json!(marker.marker_kind);
        value["marker_type"] = json!(marker.marker_type);
        value["operational_valence"] = json!(marker.operational_valence);
        value["intensity"] = json!(marker.intensity);
        value["decayed_intensity"] = json!(marker.decayed_intensity);
        value["confidence_delta"] = json!(marker.confidence_delta);
        value["decay_half_life_days"] = json!(marker.decay_half_life_days);
        value["trigger"] = json!(marker.trigger);
        value["trigger_json"] = json!(marker.trigger_json);
        value["outcome"] = json!(marker.outcome);
        value["outcome_json"] = json!(marker.outcome_json);
        value["recovery"] = json!(marker.recovery);
        value["recovery_json"] = json!(marker.recovery_json);
        value["future_guidance"] = json!(marker.future_guidance);
        value["counter_evidence_refs"] = json!(marker.counter_evidence_refs);
        value["retired_at"] = json!(marker.retired_at);
    }
    value
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

fn attach_counter_evidence_retires(records: &[MemoryRecord], candidates: &mut [DreamCandidate]) {
    for candidate in candidates {
        let Some(kind) = marker_kind_for_candidate(candidate) else {
            continue;
        };
        let valence = MarkerValence::for_kind(kind);
        if valence.operational_valence != "positive" {
            continue;
        }
        for record in records {
            if candidate.supersedes.contains(&record.id) {
                continue;
            }
            if marker_operational_valence(record) != Some("negative") {
                continue;
            }
            if marker_retired(record) {
                continue;
            }
            if same_counter_evidence_subject(candidate, record) {
                candidate.supersedes.push(record.id.clone());
                candidate.retires.push(record.id.clone());
            }
        }
        candidate.supersedes.sort();
        candidate.supersedes.dedup();
        candidate.retires.sort();
        candidate.retires.dedup();
    }
}

fn marker_operational_valence(record: &MemoryRecord) -> Option<&str> {
    record
        .metadata
        .get("marker")
        .and_then(|marker| marker.get("operational_valence"))
        .and_then(Value::as_str)
}

fn marker_retired(record: &MemoryRecord) -> bool {
    record
        .metadata
        .get("marker")
        .and_then(|marker| marker.get("retired_at"))
        .is_some_and(|value| !value.is_null())
}

fn same_counter_evidence_subject(candidate: &DreamCandidate, record: &MemoryRecord) -> bool {
    let record_subject = record
        .metadata
        .get("subject_key")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| subject_key_for_record(record));
    candidate.subject_key == record_subject
        || token_overlap(&candidate.subject_key, &record_subject) >= 2
        || token_overlap(&candidate.content, &record.content) >= 2
}

fn token_overlap(left: &str, right: &str) -> usize {
    let left = normalized_subject_tokens(left);
    let right = normalized_subject_tokens(right);
    left.intersection(&right).count()
}

fn normalized_subject_tokens(value: &str) -> BTreeSet<String> {
    normalize(value)
        .split_whitespace()
        .filter(|token| token.len() > 2 && !SUBJECT_NOISE_WORDS.contains(token))
        .map(str::to_string)
        .collect()
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
    if !same_subject(newer, older) {
        return false;
    }
    let old_state = state_for_record(older);
    let old_lower = older.content.to_ascii_lowercase();
    let new_lower = newer.content.to_ascii_lowercase();
    let old_unsettled = matches!(old_state.as_str(), "planned" | "blocked" | "active")
        || (old_state != "completed"
            && contains_any(
                &old_lower,
                &["tbd", "evaluating", "not decided", "will ", "planned"],
            ));
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
    let terms = subject_terms(content);
    if terms.is_empty() {
        normalize(content).chars().take(32).collect()
    } else {
        terms.into_iter().take(3).collect::<Vec<_>>().join("-")
    }
}

fn subject_key_for_record(record: &MemoryRecord) -> String {
    record
        .metadata
        .get("subject_key")
        .and_then(|value| value.as_str())
        .filter(|subject_key| !subject_key.is_empty())
        .map(|subject_key| subject_key.to_string())
        .unwrap_or_else(|| subject_key(&record.content))
}

fn subject_terms(content: &str) -> Vec<String> {
    let mut terms = tokens(content)
        .into_iter()
        .filter(|t| !SUBJECT_NOISE_WORDS.contains(&t.as_str()))
        .collect::<Vec<_>>();
    terms.sort_unstable();
    terms.dedup();
    terms
}

fn same_subject(a: &MemoryRecord, b: &MemoryRecord) -> bool {
    let a_subject_key = subject_key_for_record(a);
    let b_subject_key = subject_key_for_record(b);
    if !a_subject_key.is_empty() && !b_subject_key.is_empty() && a_subject_key == b_subject_key {
        return true;
    }

    let a_terms = subject_terms(&a.content);
    let b_terms = subject_terms(&b.content);
    if a_terms.is_empty() || b_terms.is_empty() {
        return false;
    }
    if subject_key(&a.content) == subject_key(&b.content) {
        return true;
    }

    let a_terms = a_terms.into_iter().collect::<BTreeSet<_>>();
    let b_terms = b_terms.into_iter().collect::<BTreeSet<_>>();
    let shared = a_terms.intersection(&b_terms).collect::<Vec<_>>();
    let meaningful_shared = shared
        .iter()
        .filter(|term| !GENERIC_SHARED_TERMS.contains(&term.as_str()))
        .count();
    if meaningful_shared >= 2 {
        return true;
    }
    if shared.len() == 1 && shared[0].as_str() == "cargo" && command_phrase_bridge(a, b) {
        return true;
    }
    if shared.iter().any(|term| term.as_str() == "storage") {
        return storage_bridge(&a_terms, &b_terms);
    }
    false
}

fn command_phrase_bridge(a: &MemoryRecord, b: &MemoryRecord) -> bool {
    let a = normalize(&a.content);
    let b = normalize(&b.content);
    COMMAND_PHRASE_HINTS
        .iter()
        .any(|phrase| a.contains(phrase) && b.contains(phrase))
}

fn storage_bridge(a_terms: &BTreeSet<String>, b_terms: &BTreeSet<String>) -> bool {
    let a_backend = a_terms.contains("backend");
    let b_backend = b_terms.contains("backend");
    let a_tech = contains_any_term(a_terms, STORAGE_TECH_HINTS);
    let b_tech = contains_any_term(b_terms, STORAGE_TECH_HINTS);
    (a_backend && b_tech) || (b_backend && a_tech)
}

fn contains_any_term(terms: &BTreeSet<String>, needles: &[&str]) -> bool {
    needles.iter().any(|needle| terms.contains(*needle))
}

const STORAGE_TECH_HINTS: &[&str] = &[
    "api", "cargo", "command", "commands", "key", "repo", "rusqlite", "script", "sqlite", "test",
    "tests", "tool", "tools", "fts5", "sqlite3", "bundle", "bundled",
];

const GENERIC_SHARED_TERMS: &[&str] = &["storage", "sync"];

const COMMAND_PHRASE_HINTS: &[&str] = &["cargo test"];

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

fn check_deadline(params: &DreamParams) -> Result<()> {
    if params
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(crate::error::Error::internal(
            "dream job exceeded max_runtime_seconds",
        ));
    }
    Ok(())
}

fn date_part(value: &str) -> &str {
    value.split('T').next().unwrap_or(value)
}
