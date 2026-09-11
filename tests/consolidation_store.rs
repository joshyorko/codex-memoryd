use codex_memoryd::consolidation::*;
use codex_memoryd::domain::{Portability, RecordType, Scope, Sensitivity};
use codex_memoryd::store::NewRecord;
use codex_memoryd::store::Store;
use serde_json::json;

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
            supersedes: vec![],
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
        supersedes: vec![],
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
        supersedes: vec![],
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
fn automatic_apply_receipt_deduplicates_shared_record_ids() {
    let store = Store::open(":memory:").unwrap();
    let mut proposal = batch("output-a");
    proposal.policy_digest = automatic_policy().digest();
    proposal.candidates.push(ConsolidationCandidate {
        candidate_id: "candidate-2".into(),
        output_digest: "output-b".into(),
        claim: "prefers concise updates".into(),
        claim_class: "preference".into(),
        subject: "synthetic-user".into(),
        inferred: false,
        source_ids: vec!["source-2".into()],
        supporting_spans: vec!["I prefer concise updates".into()],
        supersedes: vec![],
    });
    let decisions = vec![
        ConsolidationDecision {
            candidate_id: "candidate-1".into(),
            output_digest: "output-a".into(),
            operation: ConsolidationOperation::AdoptStatement,
            reason: "supported".into(),
            distinct_evidence_roots: vec!["root-1".into()],
            supersedes: vec![],
            validator: None,
        },
        ConsolidationDecision {
            candidate_id: "candidate-2".into(),
            output_digest: "output-b".into(),
            operation: ConsolidationOperation::AdoptStatement,
            reason: "supported".into(),
            distinct_evidence_roots: vec!["root-2".into()],
            supersedes: vec![],
            validator: None,
        },
    ];
    store
        .persist_consolidation_batch(&proposal, Some(&decisions), "validated")
        .unwrap();
    let applied = store
        .apply_consolidation_proposal("batch-recovery-1", &automatic_policy(), &decisions)
        .unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(store.count_records().unwrap(), 1);
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
        supersedes: vec![],
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
fn guarded_undo_preserves_a_later_edit() {
    let store = Store::open(":memory:").unwrap();
    let mut proposal = batch("output-a");
    proposal.policy_digest = automatic_policy().digest();
    let decision = ConsolidationDecision {
        candidate_id: "candidate-1".into(),
        output_digest: "output-a".into(),
        operation: ConsolidationOperation::AdoptStatement,
        reason: "supported".into(),
        distinct_evidence_roots: vec!["root-1".into()],
        supersedes: vec![],
        validator: None,
    };
    store
        .persist_consolidation_batch(&proposal, Some(&[decision.clone()]), "validated")
        .unwrap();
    let ids = store
        .apply_consolidation_proposal("batch-recovery-1", &automatic_policy(), &[decision])
        .unwrap();
    let record = store.get_record(&ids[0]).unwrap().unwrap();
    store
        .upsert_record(&NewRecord {
            profile_id: record.profile_id.clone(),
            workspace_id: record.workspace_id.clone(),
            repo_id: None,
            subject_id: None,
            episode_id: None,
            scope: Scope::Workspace,
            record_type: RecordType::Preference,
            content: record.content.clone(),
            related_files: vec![],
            tags: vec![],
            sensitivity: Sensitivity::Personal,
            portability: Portability::ProfileOnly,
            confidence: record.confidence,
            source_ids: vec!["later-edit".into()],
            content_hash: record.content_hash.clone(),
            supersedes: vec![],
            metadata: json!({"operator_correction":true}),
        })
        .unwrap();
    assert!(store
        .undo_consolidation_proposal("batch-recovery-1")
        .unwrap()
        .is_empty());
    assert!(!store.get_record(&ids[0]).unwrap().unwrap().archived);
}

#[test]
fn guarded_undo_restores_a_superseded_record_and_archives_replacement() {
    let store = Store::open(":memory:").unwrap();
    store
        .ensure_workspace("personal", "fixture-workspace")
        .unwrap();
    let old = NewRecord {
        profile_id: "personal".into(),
        workspace_id: "fixture-workspace".into(),
        repo_id: None,
        subject_id: None,
        episode_id: None,
        scope: Scope::Workspace,
        record_type: RecordType::Preference,
        content: "prefers concise updates".into(),
        related_files: vec![],
        tags: vec![],
        sensitivity: Sensitivity::Personal,
        portability: Portability::ProfileOnly,
        confidence: 0.8,
        source_ids: vec!["old-source".into()],
        content_hash: codex_memoryd::ids::content_hash(
            "personal",
            "fixture-workspace",
            None,
            "preference",
            "workspace",
            "prefers concise updates",
        ),
        supersedes: vec![],
        metadata: json!({"origin":"fixture"}),
    };
    let old_id = store.upsert_record(&old).unwrap().id().to_string();
    let mut proposal = batch("output-new");
    proposal.policy_digest = automatic_policy().digest();
    proposal.candidates[0].claim = "prefers stable interfaces".into();
    proposal.candidates[0].output_digest = "output-new".into();
    proposal.candidates[0].supersedes = vec![old_id.clone()];
    let decision = ConsolidationDecision {
        candidate_id: "candidate-1".into(),
        output_digest: "output-new".into(),
        operation: ConsolidationOperation::AdoptStatement,
        reason: "supported correction".into(),
        distinct_evidence_roots: vec!["new-source".into()],
        supersedes: vec![old_id.clone()],
        validator: None,
    };
    store
        .persist_consolidation_batch(&proposal, Some(&[decision.clone()]), "validated")
        .unwrap();
    let applied = store
        .apply_consolidation_proposal("batch-recovery-1", &automatic_policy(), &[decision])
        .unwrap();
    assert_eq!(applied.len(), 1);

    assert_eq!(
        store
            .undo_consolidation_proposal("batch-recovery-1")
            .unwrap(),
        applied
    );
    assert!(!store.get_record(&old_id).unwrap().unwrap().archived);
    assert!(store.get_record(&applied[0]).unwrap().unwrap().archived);
}

#[test]
fn deferred_candidate_does_not_freeze_independent_adoption() {
    let store = Store::open(":memory:").unwrap();
    let mut proposal = batch("output-a");
    proposal.policy_digest = automatic_policy().digest();
    proposal.candidates.push(ConsolidationCandidate {
        candidate_id: "candidate-2".into(),
        output_digest: "output-b".into(),
        claim: "prefers stable interfaces".into(),
        claim_class: "preference".into(),
        subject: "synthetic-user".into(),
        inferred: false,
        source_ids: vec!["source-2".into()],
        supporting_spans: vec!["I prefer stable interfaces".into()],
        supersedes: vec![],
    });
    let decisions = vec![
        ConsolidationDecision {
            candidate_id: "candidate-1".into(),
            output_digest: "output-a".into(),
            operation: ConsolidationOperation::Defer,
            reason: "validator unavailable".into(),
            distinct_evidence_roots: vec![],
            supersedes: vec![],
            validator: None,
        },
        ConsolidationDecision {
            candidate_id: "candidate-2".into(),
            output_digest: "output-b".into(),
            operation: ConsolidationOperation::AdoptStatement,
            reason: "supported".into(),
            distinct_evidence_roots: vec!["root-2".into()],
            supersedes: vec![],
            validator: None,
        },
    ];
    store
        .persist_consolidation_batch(&proposal, Some(&decisions), "validated")
        .unwrap();
    let applied = store
        .apply_consolidation_proposal("batch-recovery-1", &automatic_policy(), &decisions)
        .unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(store.count_records().unwrap(), 1);
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
        supersedes: vec![],
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
