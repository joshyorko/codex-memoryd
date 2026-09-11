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
        supersedes: vec![],
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
fn inferred_claims_require_two_distinct_evidence_roots() {
    let mut copied = evidence();
    copied[1].root_id = copied[0].root_id.clone();
    let validation = SemanticValidation {
        supported: true,
        validator: "fixture-validator".into(),
        reason: "supported".into(),
    };

    let decision = evaluate_candidate(
        &policy(),
        "personal",
        &candidate(true, "values concise communication"),
        &copied,
        &[],
        &[],
        Some(&validation),
    );

    assert_eq!(decision.operation, ConsolidationOperation::Defer);
    assert_eq!(decision.reason, "insufficient_distinct_evidence");
}

#[test]
fn user_statements_require_user_authored_evidence() {
    let mut assistant_evidence = evidence();
    assistant_evidence[0].actor = "assistant".into();

    let decision = evaluate_candidate(
        &policy(),
        "personal",
        &candidate(false, "prefers concise updates"),
        &assistant_evidence,
        &[],
        &[],
        None,
    );

    assert_eq!(decision.operation, ConsolidationOperation::Defer);
    assert_eq!(decision.reason, "source_actor_not_allowed");
}

#[test]
fn disallowed_adoption_operation_is_deferred_by_policy() {
    let mut restricted = policy();
    restricted.operations = vec![ConsolidationOperation::Defer];

    let decision = evaluate_candidate(
        &restricted,
        "personal",
        &candidate(false, "prefers concise updates"),
        &evidence(),
        &[],
        &[],
        None,
    );

    assert_eq!(decision.operation, ConsolidationOperation::Defer);
    assert_eq!(decision.reason, "operation_not_allowed");
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

#[test]
fn repeated_derivation_with_unchanged_roots_stays_no_change() {
    for _ in 0..20 {
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
        assert_eq!(decision.distinct_evidence_roots.len(), 2);
    }
}

#[test]
fn quoted_hypothetical_and_task_scoped_statements_do_not_become_preferences() {
    for claim in [
        "Hypothetically, I prefer blue.",
        "As an example, I prefer blue.",
        "I prefer blue only for this task.",
        "I do not remember preferring blue.",
    ] {
        let decision = evaluate_candidate(
            &policy(),
            "personal",
            &candidate(false, claim),
            &evidence(),
            &[],
            &[],
            None,
        );
        assert_eq!(decision.operation, ConsolidationOperation::Defer, "{claim}");
        assert_eq!(decision.reason, "statement_scope_or_negation_uncertain");
    }
}

#[test]
fn secret_shaped_candidate_content_is_not_adopted() {
    let decision = evaluate_candidate(
        &policy(),
        "personal",
        &candidate(false, "OPENAI_API_KEY=sk-abcdefghijklmnop1234"),
        &evidence(),
        &[],
        &[],
        None,
    );
    assert_eq!(decision.operation, ConsolidationOperation::Defer);
    assert_eq!(decision.reason, "candidate_content_rejected");
}
