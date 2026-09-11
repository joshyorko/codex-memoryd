use codex_memoryd::consolidation::*;
use serde_json::json;
use std::path::PathBuf;

fn budget() -> ConsolidationBudget {
    ConsolidationBudget {
        max_candidates: 10,
        max_source_records: 100,
        max_provider_calls: 2,
        max_input_bytes: 4096,
        max_output_bytes: 4096,
    }
}

#[test]
fn policy_is_versioned_bounded_and_closed() {
    let policy = ConsolidationPolicy {
        contract_version: CONSOLIDATION_CONTRACT_VERSION.into(),
        mode: ConsolidationMode::Automatic,
        scopes: vec!["personal".into()],
        claim_classes: vec!["preference".into()],
        source_classes: vec!["user_statement".into()],
        operations: vec![ConsolidationOperation::AdoptStatement],
        budget: budget(),
        retention_days: 30,
        semantic_validation: true,
        legacy_metadata: None,
    };
    assert!(policy.validate().is_ok());
    assert!(serde_json::from_value::<ConsolidationPolicy>(json!({"contract_version":"consolidation.v1","mode":"automatic","scopes":["x"],"claim_classes":[],"source_classes":[],"operations":[],"budget":{},"retention_days":1,"semantic_validation":true,"unexpected":1})).is_err());
}

#[test]
fn explicit_empty_cutoff_is_distinct_from_omitted_cutoff() {
    let omitted: ConsolidationSourceCursor =
        serde_json::from_value(json!({"explicit_since":false})).unwrap();
    let empty: ConsolidationSourceCursor =
        serde_json::from_value(json!({"since":"","explicit_since":true})).unwrap();
    assert!(!omitted.explicit_since && omitted.since.is_none());
    assert!(empty.explicit_since && empty.since.as_deref() == Some(""));
}

#[test]
fn exact_output_identity_is_part_of_a_candidate() {
    let a = ConsolidationCandidate {
        candidate_id: "same-source".into(),
        output_digest: "output-a".into(),
        claim: "one".into(),
        claim_class: "preference".into(),
        subject: "user".into(),
        inferred: false,
        source_ids: vec!["source-1".into()],
        supporting_spans: vec!["one".into()],
    };
    let b = ConsolidationCandidate {
        output_digest: "output-b".into(),
        ..a.clone()
    };
    assert_ne!(a, b);
}

#[test]
fn optional_legacy_metadata_is_accepted_without_widening_contract() {
    let raw = json!({"contract_version":"consolidation.v1","mode":"preview","scopes":["x"],"claim_classes":[],"source_classes":[],"operations":[],"budget":{"max_candidates":1,"max_source_records":1,"max_provider_calls":1,"max_input_bytes":1,"max_output_bytes":1},"retention_days":1,"semantic_validation":false,"legacy_metadata":{"source":"old"}});
    assert!(serde_json::from_value::<ConsolidationPolicy>(raw).is_ok());
}

#[test]
fn fixture_manifest_freezes_all_required_cases() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/consolidation/manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(manifest["contract_version"], CONSOLIDATION_CONTRACT_VERSION);
    assert_eq!(manifest["fixture_count"], 26);
    assert_eq!(manifest["fixtures"].as_array().unwrap().len(), 26);
}
