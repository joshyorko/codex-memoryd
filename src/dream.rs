//! Deterministic Dreamer preview. Phase 1 is intentionally read-only: it
//! gathers bounded evidence from existing tables and emits the exact candidate
//! objects that a future apply mode would have to policy-gate before writing.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::domain::MemoryRecord;
use crate::domain::Profile;
use crate::domain::RecordType;
use crate::error::Result;
use crate::ids;
use crate::policy;
use crate::policy::PolicyDecision;
use crate::store::RecordQuery;
use crate::store::Store;

const EVIDENCE_LIMIT: usize = 100;
const ACTIVE_RECORD_LIMIT: usize = 200;

#[derive(Debug, Clone)]
pub struct DreamParams {
    pub profile: Profile,
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DreamReport {
    pub mode: String,
    pub run_id: String,
    pub profile: String,
    pub workspace: String,
    pub evidence_window: EvidenceWindow,
    pub evidence_scanned: EvidenceCounts,
    pub evidence_counts: EvidenceCounts,
    pub candidates: Vec<DreamCandidate>,
    pub rejected: Vec<DreamCandidate>,
    pub quarantined: Vec<DreamCandidate>,
    pub stale: Vec<StaleCandidate>,
    pub impact: ImpactEstimate,
    pub created: usize,
    pub archived: usize,
    pub authority: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceWindow {
    pub start: Option<String>,
    pub end: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceCounts {
    pub visible_turns: usize,
    pub conclusions: usize,
    pub checkpoints: usize,
    pub imported_memories: usize,
    pub active_records: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DreamCandidate {
    pub subject_key: String,
    pub action: String,
    pub proposed_type: String,
    pub proposed_scope: String,
    pub content: String,
    pub confidence: f64,
    pub evidence: Vec<DreamEvidence>,
    pub evidence_counts: BTreeMap<String, usize>,
    pub promotion_reason: String,
    pub drift_prone: bool,
    pub policy: String,
    pub supersedes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DreamEvidence {
    pub kind: String,
    pub id: String,
    pub strength: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StaleCandidate {
    pub memory_id: String,
    pub subject_key: String,
    pub drift_prone: bool,
    pub suggested_action: String,
    pub evidence: Vec<DreamEvidence>,
    pub evidence_counts: BTreeMap<String, usize>,
    pub policy: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactEstimate {
    pub records_added: usize,
    pub records_archived: usize,
    pub estimated_tokens: usize,
}

#[derive(Debug, Clone)]
struct CandidateBuilder {
    candidate: DreamCandidate,
}

/// Build a deterministic preview report. This function must not call any store
/// write method; preview is the trust boundary.
pub fn preview(store: &Store, params: DreamParams, max_record_chars: usize) -> Result<DreamReport> {
    let profile = params.profile.as_str();
    let workspace = params.workspace;

    let visible_turns = store.dream_visible_turns(profile, &workspace, EVIDENCE_LIMIT)?;
    let conclusions = store.dream_conclusions(profile, &workspace, EVIDENCE_LIMIT)?;
    let checkpoints = store.dream_checkpoints(profile, &workspace, EVIDENCE_LIMIT)?;
    let imported_sources = store.dream_memory_sources(profile, &workspace, EVIDENCE_LIMIT)?;
    let mut active_records = store.query_records(&RecordQuery {
        profile_id: Some(profile.to_string()),
        workspace_id: Some(workspace.clone()),
        limit: ACTIVE_RECORD_LIMIT,
        ..RecordQuery::default()
    })?;
    active_records.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    let counts = EvidenceCounts {
        visible_turns: visible_turns.len(),
        conclusions: conclusions.len(),
        checkpoints: checkpoints.len(),
        imported_memories: imported_sources
            .iter()
            .filter(|s| s.kind != "visible_turn")
            .count(),
        active_records: active_records.len(),
    };

    let mut builders: BTreeMap<String, CandidateBuilder> = BTreeMap::new();
    let mut rejected = Vec::new();
    let mut quarantined = Vec::new();

    for turn in visible_turns.iter().filter(|t| t.actor == "user") {
        add_proposal(
            &mut builders,
            &mut rejected,
            ProposalInput {
                kind: "visible_turn",
                id: &turn.id,
                strength: "strong",
                content: &turn.content,
                profile: params.profile,
                repo_present: false,
                reason: "user visible turn is primary evidence",
                max_record_chars,
            },
        );
    }

    for conclusion in &conclusions {
        add_proposal(
            &mut builders,
            &mut rejected,
            ProposalInput {
                kind: "conclusion",
                id: &conclusion.id,
                strength: "strong",
                content: &conclusion.content,
                profile: params.profile,
                repo_present: conclusion.repo_id.is_some(),
                reason: "conclusion is explicit durable evidence",
                max_record_chars,
            },
        );
    }

    for checkpoint in &checkpoints {
        add_proposal(
            &mut builders,
            &mut rejected,
            ProposalInput {
                kind: "checkpoint",
                id: &checkpoint.id,
                strength: "strong",
                content: &checkpoint.summary,
                profile: params.profile,
                repo_present: checkpoint.repo_id.is_some(),
                reason: "checkpoint is strong task-state evidence",
                max_record_chars,
            },
        );
    }

    for turn in visible_turns.iter().filter(|t| t.actor == "assistant") {
        add_assistant_proposal(
            &mut builders,
            &mut quarantined,
            &turn.id,
            &turn.content,
            params.profile,
            max_record_chars,
        );
    }

    let active_by_subject = active_records
        .iter()
        .map(|record| (record_subject_key(record), record))
        .collect::<BTreeMap<_, _>>();

    let mut candidates = Vec::new();
    for (subject_key, mut builder) in builders {
        if let Some(active) = active_by_subject.get(&subject_key) {
            builder
                .candidate
                .evidence
                .push(evidence("active_record", &active.id, "conflict"));
            increment(&mut builder.candidate.evidence_counts, "active_records");
            builder.candidate.action = "reject".to_string();
            builder.candidate.policy = "duplicate_active_record".to_string();
            builder.candidate.promotion_reason =
                "already represented by an active memory record".to_string();
            rejected.push(builder.candidate);
        } else {
            candidates.push(finalize_candidate(builder.candidate));
        }
    }

    for source in imported_sources.iter().filter(|s| s.kind != "visible_turn") {
        // Imported local memory is secondary/corroborating evidence only. With no
        // adopted strong proposal to attach to, it is intentionally not promoted.
        let mut counts = BTreeMap::new();
        counts.insert("imported_memories".to_string(), 1);
        let content = source
            .source_path
            .clone()
            .unwrap_or_else(|| source.source_hash.clone());
        quarantined.push(DreamCandidate {
            subject_key: subject_key("imported_memory", "workspace", &content),
            action: "quarantine".to_string(),
            proposed_type: "other".to_string(),
            proposed_scope: "workspace".to_string(),
            content,
            confidence: 0.2,
            evidence: vec![evidence("imported_memory", &source.id, "secondary")],
            evidence_counts: counts,
            promotion_reason: "imported local memory requires primary evidence before promotion"
                .to_string(),
            drift_prone: false,
            policy: "secondary_only".to_string(),
            supersedes: vec![],
        });
    }

    let stale = active_records
        .iter()
        .filter(|record| is_drift_prone(&record.content))
        .map(stale_candidate)
        .collect::<Vec<_>>();

    rejected.sort_by(|a, b| a.subject_key.cmp(&b.subject_key));
    quarantined.sort_by(|a, b| a.subject_key.cmp(&b.subject_key));
    candidates.sort_by(|a, b| a.subject_key.cmp(&b.subject_key));

    let estimated_tokens = candidates
        .iter()
        .map(|c| c.content.chars().count().div_ceil(4))
        .sum();
    let impact = ImpactEstimate {
        records_added: candidates.len(),
        records_archived: stale.len(),
        estimated_tokens,
    };

    let evidence_window = EvidenceWindow {
        start: None,
        end: max_timestamp(&visible_turns, &conclusions, &checkpoints, &active_records),
    };
    let run_id = stable_run_id(profile, &workspace, &counts, &evidence_window);

    Ok(DreamReport {
        mode: "preview".to_string(),
        run_id,
        profile: profile.to_string(),
        workspace,
        evidence_window,
        evidence_scanned: counts.clone(),
        evidence_counts: counts,
        candidates,
        rejected,
        quarantined,
        stale,
        impact,
        created: 0,
        archived: 0,
        authority: "recall_not_authority".to_string(),
    })
}

struct ProposalInput<'a> {
    kind: &'static str,
    id: &'a str,
    strength: &'static str,
    content: &'a str,
    profile: Profile,
    repo_present: bool,
    reason: &'static str,
    max_record_chars: usize,
}

fn add_proposal(
    builders: &mut BTreeMap<String, CandidateBuilder>,
    rejected: &mut Vec<DreamCandidate>,
    input: ProposalInput<'_>,
) {
    let content = match policy::screen_content(input.content, input.max_record_chars) {
        PolicyDecision::Accept(content) => content,
        PolicyDecision::Reject { code, reason } => {
            rejected.push(rejected_candidate(input, &code, &reason));
            return;
        }
    };
    let class = policy::classify(&content, input.profile, input.repo_present);
    if class.record_type == RecordType::Other {
        return;
    }
    let key = subject_key(class.record_type.as_str(), class.scope.as_str(), &content);
    let entry = builders.entry(key.clone()).or_insert_with(|| {
        let mut counts = BTreeMap::new();
        counts.insert(kind_count_key(input.kind).to_string(), 0);
        CandidateBuilder {
            candidate: DreamCandidate {
                subject_key: key,
                action: "create".to_string(),
                proposed_type: class.record_type.as_str().to_string(),
                proposed_scope: class.scope.as_str().to_string(),
                content: content.clone(),
                confidence: class.confidence,
                evidence: vec![],
                evidence_counts: counts,
                promotion_reason: input.reason.to_string(),
                drift_prone: is_drift_prone(&content),
                policy: "accept".to_string(),
                supersedes: vec![],
            },
        }
    });
    entry
        .candidate
        .evidence
        .push(evidence(input.kind, input.id, input.strength));
    increment(
        &mut entry.candidate.evidence_counts,
        kind_count_key(input.kind),
    );
    if input.kind == "conclusion" || input.kind == "checkpoint" {
        entry.candidate.confidence += 0.05;
    }
}

fn add_assistant_proposal(
    builders: &mut BTreeMap<String, CandidateBuilder>,
    quarantined: &mut Vec<DreamCandidate>,
    id: &str,
    content: &str,
    profile: Profile,
    max_record_chars: usize,
) {
    let content = match policy::screen_content(content, max_record_chars) {
        PolicyDecision::Accept(content) => content,
        PolicyDecision::Reject { .. } => return,
    };
    let class = policy::classify(&content, profile, false);
    if class.record_type == RecordType::Other {
        return;
    }
    let key = subject_key(class.record_type.as_str(), class.scope.as_str(), &content);
    if let Some(builder) = builders.get_mut(&key) {
        builder
            .candidate
            .evidence
            .push(evidence("visible_turn", id, "weak"));
        increment(&mut builder.candidate.evidence_counts, "visible_turns");
        builder.candidate.confidence += 0.02;
        return;
    }

    let mut counts = BTreeMap::new();
    counts.insert("visible_turns".to_string(), 1);
    quarantined.push(DreamCandidate {
        subject_key: key,
        action: "quarantine".to_string(),
        proposed_type: class.record_type.as_str().to_string(),
        proposed_scope: class.scope.as_str().to_string(),
        content,
        confidence: round_confidence(class.confidence * 0.5),
        evidence: vec![evidence("visible_turn", id, "weak")],
        evidence_counts: counts,
        promotion_reason: "assistant-only proposal requires user adoption".to_string(),
        drift_prone: false,
        policy: "assistant_only".to_string(),
        supersedes: vec![],
    });
}

fn rejected_candidate(input: ProposalInput<'_>, code: &str, reason: &str) -> DreamCandidate {
    let mut counts = BTreeMap::new();
    counts.insert(kind_count_key(input.kind).to_string(), 1);
    DreamCandidate {
        subject_key: subject_key("rejected", "workspace", input.content),
        action: "reject".to_string(),
        proposed_type: "other".to_string(),
        proposed_scope: "workspace".to_string(),
        content: "[redacted rejected evidence]".to_string(),
        confidence: 0.0,
        evidence: vec![evidence(input.kind, input.id, input.strength)],
        evidence_counts: counts,
        promotion_reason: reason.to_string(),
        drift_prone: false,
        policy: code.to_string(),
        supersedes: vec![],
    }
}

fn stale_candidate(record: &MemoryRecord) -> StaleCandidate {
    let mut counts = BTreeMap::new();
    counts.insert("active_records".to_string(), 1);
    StaleCandidate {
        memory_id: record.id.clone(),
        subject_key: record_subject_key(record),
        drift_prone: true,
        suggested_action: "rewrite_historical".to_string(),
        evidence: vec![evidence("active_record", &record.id, "conflict")],
        evidence_counts: counts,
        policy: "stale_review".to_string(),
    }
}

fn finalize_candidate(mut candidate: DreamCandidate) -> DreamCandidate {
    let strong = candidate
        .evidence
        .iter()
        .filter(|e| e.strength == "strong")
        .count();
    if strong > 1 {
        candidate.confidence += 0.05;
    }
    candidate.confidence = round_confidence(candidate.confidence);
    candidate
}

fn evidence(kind: &str, id: &str, strength: &str) -> DreamEvidence {
    DreamEvidence {
        kind: kind.to_string(),
        id: id.to_string(),
        strength: strength.to_string(),
    }
}

fn increment(counts: &mut BTreeMap<String, usize>, key: &str) {
    *counts.entry(key.to_string()).or_insert(0) += 1;
}

fn kind_count_key(kind: &str) -> &str {
    match kind {
        "conclusion" => "conclusions",
        "checkpoint" => "checkpoints",
        "imported_memory" => "imported_memories",
        "active_record" => "active_records",
        _ => "visible_turns",
    }
}

fn record_subject_key(record: &MemoryRecord) -> String {
    subject_key(
        record.record_type.as_str(),
        record.scope.as_str(),
        &record.content,
    )
}

fn subject_key(record_type: &str, scope: &str, content: &str) -> String {
    let hash = ids::content_hash("dream", "preview", None, record_type, scope, content);
    format!("{record_type}:{scope}:{}", &hash["sha256:".len()..23])
}

fn is_drift_prone(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    [
        "currently",
        "for now",
        "temporary",
        "temporarily",
        "today",
        "this week",
        "deprecated",
        "legacy",
        "old ",
        "tbd",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn round_confidence(value: f64) -> f64 {
    (value.clamp(0.0, 0.99) * 100.0).round() / 100.0
}

fn max_timestamp(
    turns: &[crate::domain::VisibleTurn],
    conclusions: &[crate::domain::Conclusion],
    checkpoints: &[crate::domain::Checkpoint],
    records: &[MemoryRecord],
) -> Option<String> {
    let mut values = Vec::new();
    values.extend(turns.iter().map(|t| t.created_at.clone()));
    values.extend(conclusions.iter().map(|c| c.created_at.clone()));
    values.extend(checkpoints.iter().map(|c| c.created_at.clone()));
    values.extend(records.iter().map(|r| r.updated_at.clone()));
    values.into_iter().max()
}

fn stable_run_id(
    profile: &str,
    workspace: &str,
    counts: &EvidenceCounts,
    window: &EvidenceWindow,
) -> String {
    let seed = format!(
        "{profile}\x1f{workspace}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{:?}",
        counts.visible_turns,
        counts.conclusions,
        counts.checkpoints,
        counts.imported_memories,
        counts.active_records,
        window.end
    );
    let hash = ids::sha256_hex(seed.as_bytes());
    format!("dream_{}", &hash["sha256:".len()..23])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::Portability;
    use crate::domain::Scope;
    use crate::domain::Sensitivity;
    use crate::domain::VisibleTurn;
    use crate::store::NewRecord;

    fn mem_store() -> Store {
        Store::open(":memory:").expect("open in-memory store")
    }

    fn params() -> DreamParams {
        DreamParams {
            profile: Profile::Personal,
            workspace: "ws".to_string(),
        }
    }

    #[test]
    fn empty_workspace_preview_is_empty() {
        let store = mem_store();
        let report = preview(&store, params(), policy::MAX_RECORD_CHARS).unwrap();
        assert!(report.candidates.is_empty());
        assert!(report.rejected.is_empty());
        assert!(report.quarantined.is_empty());
        assert_eq!(report.created, 0);
        assert_eq!(report.archived, 0);
    }

    #[test]
    fn active_records_alone_do_not_create_candidates() {
        let store = mem_store();
        store.ensure_workspace("personal", "ws").unwrap();
        let new = NewRecord {
            profile_id: "personal".to_string(),
            workspace_id: "ws".to_string(),
            repo_id: None,
            scope: Scope::User,
            record_type: RecordType::Preference,
            content: "Prefer repo-native commands".to_string(),
            related_files: vec![],
            tags: vec![],
            sensitivity: Sensitivity::Personal,
            portability: Portability::Portable,
            confidence: 0.75,
            source_ids: vec![],
            content_hash: ids::content_hash(
                "personal",
                "ws",
                None,
                "preference",
                "user",
                "Prefer repo-native commands",
            ),
            metadata: json!({}),
        };
        store.upsert_record(&new).unwrap();

        let report = preview(&store, params(), policy::MAX_RECORD_CHARS).unwrap();
        assert!(report.candidates.is_empty());
        assert!(report.quarantined.is_empty());
        assert!(report.rejected.is_empty());
    }

    #[test]
    fn assistant_only_proposal_is_quarantined() {
        let store = mem_store();
        store
            .ensure_session("sess1", "personal", "ws", None, None, "test")
            .unwrap();
        store
            .insert_visible_turn(&VisibleTurn {
                id: "turn_assistant".to_string(),
                session_id: "sess1".to_string(),
                actor: "assistant".to_string(),
                content: "Prefer cargo test for validation".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                metadata: json!({}),
            })
            .unwrap();

        let report = preview(&store, params(), policy::MAX_RECORD_CHARS).unwrap();
        assert!(report.candidates.is_empty());
        assert_eq!(report.quarantined.len(), 1);
        assert_eq!(report.quarantined[0].action, "quarantine");
        assert_eq!(report.quarantined[0].policy, "assistant_only");
    }
}
