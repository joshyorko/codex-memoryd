use codex_memoryd::consolidation::*;
use codex_memoryd::domain::{Portability, RecordType, Scope, Sensitivity, TemporalState};
use codex_memoryd::error::ErrorCode;
use codex_memoryd::ids;
use codex_memoryd::store::{NewRecord, Store};
use serde_json::json;

fn policy_with_budget(
    max_candidates: usize,
    max_source_records: usize,
    max_input_bytes: usize,
    max_output_bytes: usize,
) -> ConsolidationPolicy {
    ConsolidationPolicy {
        contract_version: CONSOLIDATION_CONTRACT_VERSION.into(),
        mode: ConsolidationMode::Automatic,
        scopes: vec!["personal".into()],
        claim_classes: vec!["preference".into()],
        source_classes: vec!["user_statement".into()],
        operations: vec![ConsolidationOperation::AdoptStatement],
        budget: ConsolidationBudget {
            max_candidates,
            max_source_records,
            max_provider_calls: 1,
            max_input_bytes,
            max_output_bytes,
        },
        retention_days: 30,
        semantic_validation: false,
        legacy_metadata: None,
    }
}

fn policy() -> ConsolidationPolicy {
    policy_with_budget(10, 10, 4096, 4096)
}

fn candidate(id: &str, claim: &str, source_ids: &[&str]) -> ConsolidationCandidate {
    ConsolidationCandidate {
        candidate_id: id.into(),
        output_digest: format!("output-{id}"),
        claim: claim.into(),
        claim_class: "preference".into(),
        subject: "user".into(),
        inferred: false,
        source_ids: source_ids.iter().map(|id| (*id).into()).collect(),
        supporting_spans: vec![claim.into()],
        supersedes: vec![],
        temporal_state: None,
        valid_until: None,
        historical_reason: None,
    }
}

fn batch(
    id: &str,
    policy: &ConsolidationPolicy,
    candidates: Vec<ConsolidationCandidate>,
) -> ConsolidationBatch {
    ConsolidationBatch {
        contract_version: CONSOLIDATION_CONTRACT_VERSION.into(),
        batch_id: id.into(),
        policy_digest: policy.digest(),
        profile: "personal".into(),
        workspace: "review".into(),
        repo_id: None,
        scope: "personal".into(),
        source_cursor: ConsolidationSourceCursor {
            since: None,
            until: Some("2030-01-01T00:00:00Z".into()),
            explicit_since: false,
        },
        snapshot_digest: format!("snapshot-{id}"),
        candidates,
    }
}

fn decisions(batch: &ConsolidationBatch) -> Vec<ConsolidationDecision> {
    batch
        .candidates
        .iter()
        .map(|candidate| ConsolidationDecision {
            candidate_id: candidate.candidate_id.clone(),
            output_digest: candidate.output_digest.clone(),
            operation: ConsolidationOperation::AdoptStatement,
            reason: "supported".into(),
            distinct_evidence_roots: candidate.source_ids.clone(),
            supersedes: candidate.supersedes.clone(),
            validator: None,
        })
        .collect()
}

fn apply_batch(
    store: &Store,
    id: &str,
    policy: &ConsolidationPolicy,
    candidates: Vec<ConsolidationCandidate>,
) -> Vec<String> {
    let batch = batch(id, policy, candidates);
    let decisions = decisions(&batch);
    store
        .persist_consolidation_batch(&batch, Some(&decisions), "validated")
        .expect("persist batch");
    store
        .apply_consolidation_proposal(&batch.batch_id, policy, &decisions)
        .expect("apply batch")
}

fn normal_record(content: &str, source_ids: &[&str]) -> NewRecord {
    NewRecord {
        profile_id: "personal".into(),
        workspace_id: "review".into(),
        repo_id: None,
        subject_id: None,
        episode_id: None,
        scope: Scope::User,
        record_type: RecordType::Preference,
        content: content.into(),
        related_files: vec![],
        tags: vec![],
        sensitivity: Sensitivity::Personal,
        portability: Portability::ProfileOnly,
        confidence: 0.8,
        source_ids: source_ids.iter().map(|id| (*id).into()).collect(),
        content_hash: ids::content_hash(
            "personal",
            "review",
            None,
            RecordType::Preference.as_str(),
            Scope::User.as_str(),
            content,
        ),
        supersedes: vec![],
        metadata: json!({"origin": "fixture"}),
    }
}

fn record_with_hash(content: &str, content_hash: String, source_ids: &[&str]) -> NewRecord {
    let mut record = normal_record(content, source_ids);
    record.content_hash = content_hash;
    record
}

#[test]
fn reused_record_merges_sources_and_preserves_temporal_fields() {
    let store = Store::open(":memory:").expect("store");
    let content = "prefers durable snapshots";
    let existing_id = store
        .upsert_record(&normal_record(content, &["source-old"]))
        .expect("existing record")
        .id()
        .to_string();
    let mut candidate = candidate("reuse", content, &["source-new", "source-old"]);
    candidate.temporal_state = Some("historical".into());
    candidate.valid_until = Some("2029-12-31T00:00:00Z".into());
    candidate.historical_reason = Some("superseded by a later decision".into());

    let applied = apply_batch(&store, "reuse-batch", &policy(), vec![candidate]);
    assert_eq!(applied, vec![existing_id.clone()]);

    let record = store
        .get_record(&existing_id)
        .expect("read record")
        .unwrap();
    assert_eq!(record.source_ids, vec!["source-old", "source-new"]);
    assert_eq!(record.temporal_state, TemporalState::Historical);
    assert_eq!(record.valid_until.as_deref(), Some("2029-12-31T00:00:00Z"));
    assert_eq!(
        record.historical_reason.as_deref(),
        Some("superseded by a later decision")
    );
}

#[test]
fn inactive_exact_hash_is_rejected_before_supersession() {
    let store = Store::open(":memory:").expect("store");
    let replacement_content = "prefers the amber deployment";
    let inactive_hash = ids::exact_content_hash(
        "personal",
        "review",
        None,
        RecordType::Preference.as_str(),
        Scope::User.as_str(),
        replacement_content,
    );
    let inactive_id = store
        .upsert_record(&record_with_hash(
            replacement_content,
            inactive_hash,
            &["inactive-source"],
        ))
        .expect("inactive replacement")
        .id()
        .to_string();
    store
        .archive_records(
            "personal",
            Some("review"),
            std::slice::from_ref(&inactive_id),
        )
        .expect("archive inactive replacement");
    let target_id = store
        .upsert_record(&normal_record(
            "prefers the legacy deployment",
            &["target-source"],
        ))
        .expect("supersession target")
        .id()
        .to_string();

    let mut replacement = candidate("replacement", replacement_content, &["new-source"]);
    replacement.supersedes = vec![target_id.clone()];
    let proposal = batch("inactive-replacement-batch", &policy(), vec![replacement]);
    let proposal_decisions = decisions(&proposal);
    store
        .persist_consolidation_batch(&proposal, Some(&proposal_decisions), "validated")
        .expect("persist batch");
    let error = store
        .apply_consolidation_proposal(&proposal.batch_id, &policy(), &proposal_decisions)
        .expect_err("inactive replacement must fail closed");

    assert_eq!(error.code, ErrorCode::BundlePlanStale);
    assert!(error.message.contains("replacement is inactive"));
    assert!(
        store
            .get_record(&inactive_id)
            .expect("read inactive")
            .unwrap()
            .archived
    );
    assert!(
        !store
            .get_record(&target_id)
            .expect("read target")
            .unwrap()
            .archived
    );
    assert_eq!(store.count_records().expect("record count"), 2);
}

#[test]
fn normal_upsert_reuses_consolidated_record_and_merges_provenance() {
    let store = Store::open(":memory:").expect("store");
    let content = "Use git branch -d feature";
    let candidate = candidate("normal-hash", content, &["consolidated-source"]);
    let applied = apply_batch(&store, "normal-hash-batch", &policy(), vec![candidate]);
    let adopted_id = applied.first().expect("adopted record").clone();
    let normal_hash = ids::content_hash(
        "personal",
        "review",
        None,
        RecordType::Preference.as_str(),
        Scope::User.as_str(),
        content,
    );
    let exact_hash = ids::exact_content_hash(
        "personal",
        "review",
        None,
        RecordType::Preference.as_str(),
        Scope::User.as_str(),
        content,
    );

    let outcome = store
        .upsert_record(&NewRecord {
            source_ids: vec!["normal-source".into()],
            content_hash: normal_hash.clone(),
            ..normal_record(content, &[])
        })
        .expect("normal ingestion");

    assert_eq!(outcome.id(), adopted_id);
    assert_eq!(store.count_records().expect("record count"), 1);
    let record = store
        .get_record(&adopted_id)
        .expect("read adopted")
        .unwrap();
    assert_ne!(record.content_hash, normal_hash);
    assert_eq!(record.content_hash, exact_hash);
    assert_eq!(
        record.source_ids,
        vec!["consolidated-source", "normal-source"]
    );
}

#[test]
fn candidate_budget_rejection_happens_before_reuse_or_insert() {
    let store = Store::open(":memory:").expect("store");
    let existing_content = "prefers the first option";
    let existing_id = store
        .upsert_record(&normal_record(existing_content, &["original-source"]))
        .expect("existing record")
        .id()
        .to_string();
    let constrained = policy_with_budget(1, 10, 4096, 4096);
    let first = candidate("over-limit-first", existing_content, &["new-source"]);
    let second = candidate(
        "over-limit-second",
        "prefers the second option",
        &["other-source"],
    );
    let proposal = batch("candidate-budget-batch", &constrained, vec![first, second]);
    let proposal_decisions = decisions(&proposal);
    store
        .persist_consolidation_batch(&proposal, Some(&proposal_decisions), "validated")
        .expect("persist batch");

    let error = store
        .apply_consolidation_proposal(&proposal.batch_id, &constrained, &proposal_decisions)
        .expect_err("candidate budget must reject the batch");
    assert_eq!(error.code, ErrorCode::BundleLimitExceeded);
    assert!(error.message.contains("max_candidates"));
    assert_eq!(store.count_records().expect("record count"), 1);
    let existing = store
        .get_record(&existing_id)
        .expect("read existing")
        .unwrap();
    assert_eq!(existing.source_ids, vec!["original-source"]);
    assert!(existing
        .metadata
        .get("governed_consolidation_applied")
        .is_none());
}

fn assert_budget_rejected(
    id: &str,
    policy: ConsolidationPolicy,
    candidate: ConsolidationCandidate,
) {
    let store = Store::open(":memory:").expect("store");
    let proposal = batch(id, &policy, vec![candidate]);
    let proposal_decisions = decisions(&proposal);
    store
        .persist_consolidation_batch(&proposal, Some(&proposal_decisions), "validated")
        .expect("persist batch");
    let error = store
        .apply_consolidation_proposal(&proposal.batch_id, &policy, &proposal_decisions)
        .expect_err("budget must reject the batch");
    assert_eq!(error.code, ErrorCode::BundleLimitExceeded);
    assert_eq!(store.count_records().expect("record count"), 0);
}

#[test]
fn source_and_byte_budgets_are_enforced_before_writes() {
    assert_budget_rejected(
        "source-budget-batch",
        policy_with_budget(10, 1, 4096, 4096),
        candidate(
            "source-budget",
            "source bounded claim",
            &["source-a", "source-b"],
        ),
    );
    assert_budget_rejected(
        "input-budget-batch",
        policy_with_budget(10, 10, 1, 4096),
        candidate("input-budget", "input bounded claim", &["source-a"]),
    );
    assert_budget_rejected(
        "output-budget-batch",
        policy_with_budget(10, 10, 4096, 1),
        candidate("output-budget", "output exceeds one byte", &["source-a"]),
    );
}
