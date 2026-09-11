use codex_memoryd::consolidation::policy::*;
use codex_memoryd::consolidation::*;

fn policy() -> ConsolidationPolicy {
    ConsolidationPolicy {
        contract_version: CONSOLIDATION_CONTRACT_VERSION.into(),
        mode: ConsolidationMode::Automatic,
        scopes: vec!["personal".into()],
        claim_classes: vec!["preference".into(), "pattern".into()],
        source_classes: vec!["user_statement".into(), "turn".into()],
        operations: vec![
            ConsolidationOperation::AdoptStatement,
            ConsolidationOperation::AdoptInference,
            ConsolidationOperation::NoChange,
            ConsolidationOperation::Defer,
        ],
        budget: ConsolidationBudget {
            max_candidates: 10,
            max_source_records: 10,
            max_provider_calls: 1,
            max_input_bytes: 1000,
            max_output_bytes: 1000,
        },
        retention_days: 30,
        semantic_validation: true,
        legacy_metadata: None,
    }
}

fn candidate(inferred: bool, claim: &str) -> ConsolidationCandidate {
    ConsolidationCandidate {
        candidate_id: "candidate".into(),
        output_digest: "digest".into(),
        claim: claim.into(),
        claim_class: if inferred { "pattern" } else { "preference" }.into(),
        subject: "user".into(),
        inferred,
        source_ids: vec!["source-a".into(), "source-b".into()],
        supporting_spans: vec!["support".into()],
    }
}

fn evidence() -> Vec<EvidenceDescriptor> {
    vec![
        EvidenceDescriptor {
            id: "source-a".into(),
            root_id: "root-a".into(),
            source_class: "turn".into(),
            actor: "user".into(),
            subject: "user".into(),
            content: "likes concise updates".into(),
            supporting_span: Some("likes concise updates".into()),
        },
        EvidenceDescriptor {
            id: "source-b".into(),
            root_id: "root-b".into(),
            source_class: "turn".into(),
            actor: "user".into(),
            subject: "user".into(),
            content: "prefers concise updates".into(),
            supporting_span: Some("prefers concise updates".into()),
        },
    ]
}

#[test]
fn positive_statement_and_inference_adopt_with_distinct_roots() {
    let statement = evaluate_candidate(
        &policy(),
        "personal",
        &candidate(false, "prefers concise updates"),
        &evidence(),
        &[],
        &[],
        None,
    );
    assert_eq!(statement.operation, ConsolidationOperation::AdoptStatement);
    let validation = SemanticValidation {
        supported: true,
        validator: "fixture-validator".into(),
        reason: "supported".into(),
    };
    let inference = evaluate_candidate(
        &policy(),
        "personal",
        &candidate(true, "values concise communication"),
        &evidence(),
        &[],
        &[],
        Some(&validation),
    );
    assert_eq!(inference.operation, ConsolidationOperation::AdoptInference);
    assert_eq!(inference.distinct_evidence_roots.len(), 2);
}

#[test]
fn valid_ids_without_semantic_support_do_not_adopt() {
    let validation = SemanticValidation {
        supported: false,
        validator: "fixture-validator".into(),
        reason: "not entailed".into(),
    };
    let decision = evaluate_candidate(
        &policy(),
        "personal",
        &candidate(true, "unrelated claim"),
        &evidence(),
        &[],
        &[],
        Some(&validation),
    );
    assert_eq!(decision.operation, ConsolidationOperation::Defer);
    assert_eq!(decision.reason, "semantic_support_not_established");
}

#[test]
fn duplicate_roots_and_scope_mismatch_are_not_independent_support() {
    let mut copied = evidence();
    copied[1].root_id = "root-a".into();
    let decision = evaluate_candidate(
        &policy(),
        "personal",
        &candidate(false, "new claim"),
        &copied,
        &[],
        &[],
        None,
    );
    assert_eq!(decision.distinct_evidence_roots, vec!["root-a"]);
    let denied = evaluate_candidate(
        &policy(),
        "other",
        &candidate(false, "new claim"),
        &evidence(),
        &[],
        &[],
        None,
    );
    assert_eq!(denied.reason, "scope_not_allowed");
}

#[test]
fn represented_claim_with_no_new_root_is_no_change() {
    let decision = evaluate_candidate(
        &policy(),
        "personal",
        &candidate(false, "existing"),
        &evidence(),
        &["existing".into()],
        &["root-a".into(), "root-b".into()],
        None,
    );
    assert_eq!(decision.operation, ConsolidationOperation::NoChange);
}
