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
            temporal_state: None,
            valid_until: None,
            historical_reason: None,
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
fn rejected_inference_with_no_new_evidence_is_suppressed() {
    let store = Store::open(":memory:").unwrap();
    let mut proposal = batch("rejected-inference");
    proposal.batch_id = "rejected-inference-batch".into();
    proposal.candidates[0].candidate_id = "rejected-candidate".into();
    proposal.candidates[0].output_digest = "rejected-inference".into();
    proposal.candidates[0].claim = "values concise communication".into();
    proposal.candidates[0].claim_class = "pattern".into();
    proposal.candidates[0].inferred = true;
    proposal.candidates[0].source_ids = vec!["source-a".into(), "source-b".into()];
    let rejection = ConsolidationDecision {
        candidate_id: "rejected-candidate".into(),
        output_digest: "rejected-inference".into(),
        operation: ConsolidationOperation::Reject,
        reason: "user rejected inferred insight".into(),
        distinct_evidence_roots: vec!["root-a".into(), "root-b".into()],
        supersedes: vec![],
        validator: Some("fixture-validator".into()),
    };
    store
        .persist_consolidation_batch(&proposal, Some(&[rejection]), "validated")
        .unwrap();

    let mut paraphrase = proposal.candidates[0].clone();
    paraphrase.candidate_id = "new-candidate-id".into();
    paraphrase.output_digest = "new-output".into();
    paraphrase.claim = "prefers concise communication".into();
    let current_time = codex_memoryd::ids::now_rfc3339();
    assert!(store
        .is_consolidation_candidate_suppressed(&proposal.scope, &paraphrase, 30, &current_time)
        .unwrap());
    assert!(!store
        .is_consolidation_candidate_suppressed(
            &proposal.scope,
            &paraphrase,
            30,
            "2099-01-01T00:00:00Z"
        )
        .unwrap());

    paraphrase.source_ids.push("source-new".into());
    assert!(!store
        .is_consolidation_candidate_suppressed(&proposal.scope, &paraphrase, 30, &current_time)
        .unwrap());
}

#[test]
fn source_withdrawal_suppresses_queued_proposals_and_archives_derivatives() {
    let store = Store::open(":memory:").unwrap();
    let (withdrawn_source, _) = store
        .upsert_source(
            "personal",
            "fixture-workspace",
            "fixture",
            Some("fixture:withdrawn"),
            "withdrawn-source-hash",
            &json!({"fixture": true}),
        )
        .unwrap();
    let withdrawn_source_id = withdrawn_source.id;

    let mut applied_batch = batch("withdrawal-output");
    applied_batch.batch_id = "withdrawal-applied".into();
    applied_batch.policy_digest = automatic_policy().digest();
    applied_batch.candidates[0].source_ids = vec![withdrawn_source_id.clone()];
    let applied_decision = ConsolidationDecision {
        candidate_id: "candidate-1".into(),
        output_digest: "withdrawal-output".into(),
        operation: ConsolidationOperation::AdoptStatement,
        reason: "supported".into(),
        distinct_evidence_roots: vec![withdrawn_source_id.clone()],
        supersedes: vec![],
        validator: None,
    };
    store
        .persist_consolidation_batch(
            &applied_batch,
            Some(std::slice::from_ref(&applied_decision)),
            "validated",
        )
        .unwrap();
    let applied_ids = store
        .apply_consolidation_proposal(
            "withdrawal-applied",
            &automatic_policy(),
            std::slice::from_ref(&applied_decision),
        )
        .unwrap();

    let mut queued_batch = batch("withdrawal-queued-output");
    queued_batch.batch_id = "withdrawal-queued".into();
    queued_batch.candidates[0].source_ids = vec![withdrawn_source_id.clone()];
    store
        .persist_consolidation_batch(&queued_batch, None, "proposed")
        .unwrap();

    let withdrawn = store
        .withdraw_consolidation_source(
            "personal",
            "fixture-workspace",
            &withdrawn_source_id,
            "fixture user withdrawal",
        )
        .unwrap();

    assert_eq!(withdrawn, applied_ids);
    assert!(store.get_record(&applied_ids[0]).unwrap().unwrap().archived);
    assert!(store
        .get_source(&withdrawn_source_id)
        .unwrap()
        .unwrap()
        .metadata["withdrawn"]
        .as_bool()
        .unwrap());
    assert_eq!(
        store
            .read_consolidation_proposal("withdrawal-queued")
            .unwrap()
            .unwrap()
            .status,
        "rejected"
    );
    let error = store
        .apply_consolidation_proposal("withdrawal-queued", &automatic_policy(), &[])
        .unwrap_err();
    assert!(error.message.contains("withdrawn"));
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
fn automatic_apply_preserves_candidate_temporal_fields() {
    let store = Store::open(":memory:").unwrap();
    let mut proposal = batch("temporal-output");
    proposal.policy_digest = automatic_policy().digest();
    proposal.candidates[0].temporal_state = Some("historical".into());
    proposal.candidates[0].valid_until = Some("2026-09-01T00:00:00Z".into());
    proposal.candidates[0].historical_reason = Some("expired source claim".into());
    let decision = ConsolidationDecision {
        candidate_id: "candidate-1".into(),
        output_digest: "temporal-output".into(),
        operation: ConsolidationOperation::AdoptStatement,
        reason: "preserve temporal state".into(),
        distinct_evidence_roots: vec!["root-1".into()],
        supersedes: vec![],
        validator: None,
    };

    store
        .persist_consolidation_batch(&proposal, Some(&[decision.clone()]), "validated")
        .unwrap();
    let applied = store
        .apply_consolidation_proposal(&proposal.batch_id, &automatic_policy(), &[decision])
        .unwrap();
    let record = store.get_record(&applied[0]).unwrap().unwrap();

    assert_eq!(
        record.temporal_state,
        codex_memoryd::domain::TemporalState::Historical
    );
    assert_eq!(record.valid_until.as_deref(), Some("2026-09-01T00:00:00Z"));
    assert_eq!(
        record.historical_reason.as_deref(),
        Some("expired source claim")
    );
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
        temporal_state: None,
        valid_until: None,
        historical_reason: None,
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
fn automatic_apply_preserves_case_sensitive_claims() {
    let store = Store::open(":memory:").unwrap();
    let mut proposal = batch("output-case-a");
    proposal.policy_digest = automatic_policy().digest();
    proposal.candidates[0].claim = "Use git branch -d feature".into();
    proposal.candidates[0].output_digest = "output-case-a".into();
    proposal.candidates.push(ConsolidationCandidate {
        candidate_id: "candidate-case-b".into(),
        output_digest: "output-case-b".into(),
        claim: "Use git branch -D feature".into(),
        claim_class: "preference".into(),
        subject: "synthetic-user".into(),
        inferred: false,
        source_ids: vec!["source-2".into()],
        supporting_spans: vec!["Use git branch -D feature".into()],
        supersedes: vec![],
        temporal_state: None,
        valid_until: None,
        historical_reason: None,
    });
    let decisions = vec![
        ConsolidationDecision {
            candidate_id: "candidate-1".into(),
            output_digest: "output-case-a".into(),
            operation: ConsolidationOperation::AdoptStatement,
            reason: "supported".into(),
            distinct_evidence_roots: vec!["root-1".into()],
            supersedes: vec![],
            validator: None,
        },
        ConsolidationDecision {
            candidate_id: "candidate-case-b".into(),
            output_digest: "output-case-b".into(),
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

    assert_eq!(applied.len(), 2);
    assert_eq!(store.count_records().unwrap(), 2);
}

#[test]
fn automatic_apply_keeps_meaning_preservation_perturbations_distinct() {
    let store = Store::open(":memory:").unwrap();
    let claims = [
        "Use git branch -d feature",
        "Use git branch -D feature",
        "Deploy from /srv/Alpha",
        "Deploy from /srv/alpha",
        "Reserve 1 GiB for the cache",
        "Reserve 2 GiB for the cache",
        "Do not deploy on Friday",
        "Deploy on Friday",
        "Use concise updates",
        "Usa actualizaciones concisas",
        "  Preserve exact indentation",
        "Preserve exact indentation",
    ];
    let mut proposal = batch("meaning-preservation");
    proposal.batch_id = "meaning-preservation-batch".into();
    proposal.policy_digest = automatic_policy().digest();
    proposal.candidates = claims
        .iter()
        .enumerate()
        .map(|(index, claim)| ConsolidationCandidate {
            candidate_id: format!("meaning-candidate-{index}"),
            output_digest: format!("meaning-output-{index}"),
            claim: (*claim).into(),
            claim_class: "preference".into(),
            subject: "synthetic-user".into(),
            inferred: false,
            source_ids: vec![format!("meaning-source-{index}")],
            supporting_spans: vec![(*claim).into()],
            supersedes: vec![],
            temporal_state: None,
            valid_until: None,
            historical_reason: None,
        })
        .collect();
    let decisions = proposal
        .candidates
        .iter()
        .map(|candidate| ConsolidationDecision {
            candidate_id: candidate.candidate_id.clone(),
            output_digest: candidate.output_digest.clone(),
            operation: ConsolidationOperation::AdoptStatement,
            reason: "supported".into(),
            distinct_evidence_roots: candidate.source_ids.clone(),
            supersedes: vec![],
            validator: None,
        })
        .collect::<Vec<_>>();

    store
        .persist_consolidation_batch(&proposal, Some(&decisions), "validated")
        .unwrap();
    let applied = store
        .apply_consolidation_proposal(
            "meaning-preservation-batch",
            &automatic_policy(),
            &decisions,
        )
        .unwrap();
    assert_eq!(applied.len(), claims.len());

    let records = store.query_records(&Default::default()).unwrap();
    assert_eq!(records.len(), claims.len());
    for claim in claims {
        assert!(records.iter().any(|record| record.content == claim));
    }
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
        temporal_state: None,
        valid_until: None,
        historical_reason: None,
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

fn review_supersession_fixture() -> (
    Store,
    NewRecord,
    String,
    ConsolidationBatch,
    ConsolidationDecision,
) {
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
    (store, old, old_id, proposal, decision)
}

#[test]
fn review_apply_rejects_updated_supersession_target() {
    let (store, mut old, old_id, proposal, decision) = review_supersession_fixture();
    store
        .persist_consolidation_batch(&proposal, Some(&[decision.clone()]), "validated")
        .unwrap();
    old.source_ids.push("later-source".into());
    store.upsert_record(&old).unwrap();
    assert!(store
        .apply_consolidation_proposal(&proposal.batch_id, &automatic_policy(), &[decision])
        .is_err());
    assert!(!store.get_record(&old_id).unwrap().unwrap().archived);
}

#[test]
fn review_undo_preserves_later_superseded_target_edit() {
    let (store, mut old, old_id, proposal, decision) = review_supersession_fixture();
    store
        .persist_consolidation_batch(&proposal, Some(&[decision.clone()]), "validated")
        .unwrap();
    let applied = store
        .apply_consolidation_proposal(&proposal.batch_id, &automatic_policy(), &[decision])
        .unwrap();
    old.source_ids.push("later-source".into());
    store.upsert_record(&old).unwrap();
    let before = store.get_record(&old_id).unwrap().unwrap();
    assert!(store
        .undo_consolidation_proposal(&proposal.batch_id)
        .unwrap()
        .is_empty());
    let after = store.get_record(&old_id).unwrap().unwrap();
    assert_eq!(before.metadata, after.metadata);
    assert_eq!(before.source_ids, after.source_ids);
    assert!(after.archived);
    assert!(!store.get_record(&applied[0]).unwrap().unwrap().archived);
}

#[test]
fn review_digest_replay_after_crash_preserves_original_proposal() {
    let store = Store::open(":memory:").unwrap();
    let first = batch("same-digest");
    store
        .persist_consolidation_batch(&first, None, "proposed")
        .unwrap();
    let mut retry = first.clone();
    retry.batch_id = "later-tick".into();
    retry.source_cursor.until = Some("2026-09-12T00:00:00Z".into());
    assert!(!store
        .persist_consolidation_batch(&retry, None, "proposed")
        .unwrap());
    assert_eq!(
        store
            .read_consolidation_batch(&first.batch_id)
            .unwrap()
            .unwrap(),
        first
    );
}
