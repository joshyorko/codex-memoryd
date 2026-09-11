use codex_memoryd::consolidation::*;
use codex_memoryd::store::Store;

fn batch(output: &str) -> ConsolidationBatch {
    ConsolidationBatch {
        contract_version: CONSOLIDATION_CONTRACT_VERSION.into(),
        policy_digest: "policy-1".into(),
        batch_id: "batch-recovery-1".into(),
        scope: "fixture/personal".into(),
        profile: "personal".into(),
        workspace: "fixture-workspace".into(),
        source_cursor: ConsolidationSourceCursor {
            since: None,
            until: Some("2026-09-11T00:00:00Z".into()),
            explicit_since: false,
        },
        snapshot_digest: output.into(),
        candidates: vec![ConsolidationCandidate {
            candidate_id: "candidate-1".into(),
            output_digest: output.into(),
            claim: "prefers concise updates".into(),
            claim_class: "preference".into(),
            subject: "synthetic-user".into(),
            inferred: false,
            source_ids: vec!["source-1".into()],
            supporting_spans: vec!["I prefer concise updates".into()],
        }],
    }
}

#[test]
fn proposal_survives_reopen_and_exact_replay_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memoryd.sqlite");
    let first = Store::open(path.to_str().unwrap()).unwrap();
    assert!(first
        .persist_consolidation_batch(&batch("output-a"), None, "proposed")
        .unwrap());
    assert!(!first
        .persist_consolidation_batch(&batch("output-a"), None, "proposed")
        .unwrap());
    drop(first);
    let reopened = Store::open(path.to_str().unwrap()).unwrap();
    assert_eq!(
        reopened
            .read_consolidation_batch("batch-recovery-1")
            .unwrap()
            .unwrap(),
        batch("output-a")
    );
}

#[test]
fn changed_payload_cannot_reuse_batch_identity() {
    let store = Store::open(":memory:").unwrap();
    store
        .persist_consolidation_batch(&batch("output-a"), None, "proposed")
        .unwrap();
    let error = store
        .persist_consolidation_batch(&batch("output-b"), None, "proposed")
        .unwrap_err();
    assert!(error.message.contains("different payload"));
}

#[test]
fn decision_snapshot_is_read_back_with_exact_batch() {
    let store = Store::open(":memory:").unwrap();
    let proposal = batch("output-a");
    let decision = ConsolidationDecision {
        candidate_id: "candidate-1".into(),
        output_digest: "output-a".into(),
        operation: ConsolidationOperation::AdoptStatement,
        reason: "supported".into(),
        distinct_evidence_roots: vec!["root-1".into()],
        validator: None,
    };
    store
        .persist_consolidation_batch(&proposal, Some(&[decision.clone()]), "validated")
        .unwrap();
    let read = store
        .read_consolidation_proposal("batch-recovery-1")
        .unwrap()
        .unwrap();
    assert_eq!(read.batch, proposal);
    assert_eq!(read.decisions, Some(vec![decision]));
    assert_eq!(read.status, "validated");
}

fn automatic_policy() -> ConsolidationPolicy {
    ConsolidationPolicy {
        contract_version: CONSOLIDATION_CONTRACT_VERSION.into(),
        mode: ConsolidationMode::Automatic,
        scopes: vec!["fixture/personal".into()],
        claim_classes: vec!["preference".into()],
        source_classes: vec!["user_statement".into()],
        operations: vec![ConsolidationOperation::AdoptStatement],
        budget: ConsolidationBudget {
            max_candidates: 5,
            max_source_records: 5,
            max_provider_calls: 1,
            max_input_bytes: 1000,
            max_output_bytes: 1000,
        },
        retention_days: 30,
        semantic_validation: true,
        legacy_metadata: None,
    }
}

#[test]
fn automatic_apply_is_idempotent_and_preview_cannot_apply() {
    let store = Store::open(":memory:").unwrap();
    let mut proposal = batch("output-a");
    proposal.policy_digest = automatic_policy().digest();
    let decision = ConsolidationDecision {
        candidate_id: "candidate-1".into(),
        output_digest: "output-a".into(),
        operation: ConsolidationOperation::AdoptStatement,
        reason: "supported".into(),
        distinct_evidence_roots: vec!["root-1".into()],
        validator: None,
    };
    store
        .persist_consolidation_batch(&proposal, Some(&[decision.clone()]), "validated")
        .unwrap();
    let first = store
        .apply_consolidation_proposal("batch-recovery-1", &automatic_policy(), &[decision.clone()])
        .unwrap();
    let second = store
        .apply_consolidation_proposal("batch-recovery-1", &automatic_policy(), &[decision])
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(store.count_records().unwrap(), 1);
    let mut preview = automatic_policy();
    preview.mode = ConsolidationMode::Preview;
    assert!(store
        .apply_consolidation_proposal("batch-recovery-1", &preview, &[])
        .is_err());
}

#[test]
fn guarded_undo_archives_only_untouched_batch_records() {
    let store = Store::open(":memory:").unwrap();
    let mut proposal = batch("output-a");
    proposal.policy_digest = automatic_policy().digest();
    let decision = ConsolidationDecision {
        candidate_id: "candidate-1".into(),
        output_digest: "output-a".into(),
        operation: ConsolidationOperation::AdoptStatement,
        reason: "supported".into(),
        distinct_evidence_roots: vec!["root-1".into()],
        validator: None,
    };
    store
        .persist_consolidation_batch(&proposal, Some(&[decision.clone()]), "validated")
        .unwrap();
    let ids = store
        .apply_consolidation_proposal("batch-recovery-1", &automatic_policy(), &[decision])
        .unwrap();
    assert_eq!(
        store
            .undo_consolidation_proposal("batch-recovery-1")
            .unwrap(),
        ids
    );
    assert!(store.get_record(&ids[0]).unwrap().unwrap().archived);
    assert!(store
        .undo_consolidation_proposal("batch-recovery-1")
        .is_err());
}

#[test]
fn changed_policy_cannot_apply_a_persisted_batch() {
    let store = Store::open(":memory:").unwrap();
    let proposal = batch("output-a");
    let decision = ConsolidationDecision {
        candidate_id: "candidate-1".into(),
        output_digest: "output-a".into(),
        operation: ConsolidationOperation::AdoptStatement,
        reason: "supported".into(),
        distinct_evidence_roots: vec!["root-1".into()],
        validator: None,
    };
    store
        .persist_consolidation_batch(&proposal, Some(&[decision.clone()]), "validated")
        .unwrap();
    let error = store
        .apply_consolidation_proposal("batch-recovery-1", &automatic_policy(), &[decision])
        .unwrap_err();
    assert!(error.message.contains("policy changed"));
}
