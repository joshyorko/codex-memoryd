use codex_memoryd::domain::{Subject, SubjectKind};
use codex_memoryd::ids;
use codex_memoryd::semantic_import::{
    apply_semantic_import, preview_semantic_import, SemanticAliasInput, SemanticImportReport,
    SemanticImportRequest, SemanticRelationInput,
};
use codex_memoryd::store::Store;
use serde_json::json;

fn subject(id: &str, profile: &str, workspace: &str, key: &str) -> Subject {
    Subject {
        id: id.to_string(),
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        subject_key: key.to_string(),
        kind: SubjectKind::Project,
        display_name: key.to_string(),
        created_at: ids::now_rfc3339(),
        updated_at: ids::now_rfc3339(),
        metadata: json!({}),
    }
}

fn seeded_store() -> Store {
    let store = Store::open(":memory:").unwrap();
    store
        .insert_or_get_subject(&subject("subj_alice", "personal", "semantic-ws", "alice"))
        .unwrap();
    store
        .insert_or_get_subject(&subject(
            "subj_billing",
            "personal",
            "semantic-ws",
            "billing",
        ))
        .unwrap();
    store
        .insert_or_get_subject(&subject("subj_work", "work", "semantic-ws", "work-secret"))
        .unwrap();
    store
}

fn valid_request() -> SemanticImportRequest {
    SemanticImportRequest {
        profile_id: "personal".to_string(),
        workspace_id: "semantic-ws".to_string(),
        aliases: vec![SemanticAliasInput {
            subject_id: "subj_alice".to_string(),
            alias_key: "al".to_string(),
            source_evidence: "episode:ep_alias".to_string(),
        }],
        relations: vec![SemanticRelationInput {
            from_subject_id: "subj_alice".to_string(),
            relation_type: "owns".to_string(),
            to_subject_id: "subj_billing".to_string(),
            confidence: 0.92,
            source_episode_ids: vec!["episode:ep_owns".to_string()],
            source_evidence: Some("episode:ep_owns".to_string()),
        }],
    }
}

fn rejected_reasons(report: &SemanticImportReport) -> Vec<String> {
    report
        .rejected
        .iter()
        .filter_map(|entry| entry.reason.clone())
        .collect()
}

#[test]
fn preview_valid_semantic_import_does_not_write() {
    let store = seeded_store();
    let req = valid_request();

    let report = preview_semantic_import(&store, &req).unwrap();

    assert_eq!(report.counts.would_apply, 2);
    assert_eq!(report.counts.rejected, 0);
    assert!(store
        .resolve_subject_alias("personal", "semantic-ws", "al")
        .unwrap()
        .is_none());
    assert!(store
        .relation_expanded_subjects("personal", "semantic-ws", &["subj_alice".to_string()], 1)
        .unwrap()
        .is_empty());
}

#[test]
fn apply_valid_semantic_import_writes_and_is_idempotent() {
    let store = seeded_store();
    let req = valid_request();

    let first = apply_semantic_import(&store, &req).unwrap();

    assert_eq!(first.counts.applied_aliases, 1);
    assert_eq!(first.counts.applied_relations, 1);
    assert_eq!(first.counts.rejected, 0);

    let alias = store
        .resolve_subject_alias("personal", "semantic-ws", "al")
        .unwrap()
        .expect("alias resolves after apply");
    assert_eq!(alias.id, "subj_alice");

    let expanded = store
        .relation_expanded_subjects("personal", "semantic-ws", &["subj_alice".to_string()], 1)
        .unwrap();
    assert_eq!(expanded.len(), 1);
    assert_eq!(expanded[0].subject_id, "subj_billing");

    let second = apply_semantic_import(&store, &req).unwrap();
    assert_eq!(second.counts.applied_aliases, 0);
    assert_eq!(second.counts.applied_relations, 0);
    assert_eq!(second.counts.already_present, 2);
}

#[test]
fn rejects_missing_evidence_and_unknown_relation_type() {
    let store = seeded_store();
    let mut req = valid_request();
    req.aliases[0].source_evidence = "".to_string();
    req.relations[0].relation_type = "invented".to_string();
    req.relations[0].source_episode_ids.clear();
    req.relations[0].source_evidence = None;

    let report = preview_semantic_import(&store, &req).unwrap();
    let reasons = rejected_reasons(&report);

    assert!(reasons
        .iter()
        .any(|reason| reason == "missing_alias_evidence"));
    assert!(reasons
        .iter()
        .any(|reason| reason == "unknown_relation_type"));
    assert!(reasons
        .iter()
        .any(|reason| reason == "missing_relation_episode_evidence"));
    assert!(reasons
        .iter()
        .any(|reason| reason == "missing_relation_source_evidence"));
}

#[test]
fn rejects_cross_profile_relation_endpoint() {
    let store = seeded_store();
    let mut req = valid_request();
    req.aliases.clear();
    req.relations[0].to_subject_id = "subj_work".to_string();

    let report = preview_semantic_import(&store, &req).unwrap();
    let reasons = rejected_reasons(&report);

    assert!(reasons
        .iter()
        .any(|reason| reason == "relation_endpoint_out_of_scope"));
    assert_eq!(report.counts.rejected, 1);
}

#[test]
fn rejects_alias_conflict() {
    let store = seeded_store();
    apply_semantic_import(&store, &valid_request()).unwrap();

    let mut conflict = valid_request();
    conflict.aliases[0].subject_id = "subj_billing".to_string();
    conflict.relations.clear();

    let report = preview_semantic_import(&store, &conflict).unwrap();
    let reasons = rejected_reasons(&report);

    assert!(reasons.iter().any(|reason| reason == "alias_conflict"));
    assert_eq!(report.counts.rejected, 1);
}

#[test]
fn alias_idempotency_uses_trimmed_subject_id() {
    let store = seeded_store();
    let req = SemanticImportRequest {
        profile_id: "personal".to_string(),
        workspace_id: "semantic-ws".to_string(),
        aliases: vec![SemanticAliasInput {
            subject_id: " subj_alice ".to_string(),
            alias_key: "al".to_string(),
            source_evidence: "episode:ep_alias".to_string(),
        }],
        relations: vec![],
    };

    let first = apply_semantic_import(&store, &req).unwrap();
    assert_eq!(first.counts.applied_aliases, 1);

    let second = apply_semantic_import(&store, &req).unwrap();
    assert_eq!(second.counts.already_present, 1);
    assert_eq!(second.counts.rejected, 0);
}
