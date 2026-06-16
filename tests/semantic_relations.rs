use codex_memoryd::domain::{Relation, Subject, SubjectAlias, SubjectKind};
use codex_memoryd::error::ErrorCode;
use codex_memoryd::ids;
use codex_memoryd::store::Store;
use serde_json::json;

fn subject(id: &str, profile: &str, workspace: &str, key: &str, kind: SubjectKind) -> Subject {
    Subject {
        id: id.to_string(),
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        subject_key: key.to_string(),
        kind,
        display_name: key.to_string(),
        created_at: ids::now_rfc3339(),
        updated_at: ids::now_rfc3339(),
        metadata: json!({}),
    }
}

fn alias(id: &str, profile: &str, workspace: &str, subject_id: &str, key: &str) -> SubjectAlias {
    SubjectAlias {
        id: id.to_string(),
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        subject_id: subject_id.to_string(),
        alias_key: key.to_string(),
        source_evidence: "ep_alias".to_string(),
        created_at: ids::now_rfc3339(),
        metadata: json!({}),
    }
}

fn relation(
    id: &str,
    profile: &str,
    workspace: &str,
    from_subject_id: &str,
    relation_type: &str,
    to_subject_id: &str,
    source_episode_ids: Vec<&str>,
) -> Relation {
    Relation {
        id: id.to_string(),
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        from_subject_id: from_subject_id.to_string(),
        relation_type: relation_type.to_string(),
        to_subject_id: to_subject_id.to_string(),
        confidence: 0.92,
        state: "active".to_string(),
        source_episode_ids: source_episode_ids.into_iter().map(str::to_string).collect(),
        source_evidence: Some("ev_relation".to_string()),
        created_at: ids::now_rfc3339(),
        retired_at: None,
        metadata: json!({}),
    }
}

#[test]
fn subject_aliases_resolve_only_inside_profile_workspace_scope() {
    let store = Store::open(":memory:").unwrap();
    store.ensure_workspace("personal", "semantic-ws").unwrap();
    store.ensure_workspace("work", "semantic-ws").unwrap();
    store
        .insert_or_get_subject(&subject(
            "subj_alice",
            "personal",
            "semantic-ws",
            "person:alice",
            SubjectKind::Person,
        ))
        .unwrap();
    store
        .insert_or_get_subject(&subject(
            "subj_work_alice",
            "work",
            "semantic-ws",
            "person:alice",
            SubjectKind::Person,
        ))
        .unwrap();

    let (stored, created) = store
        .insert_or_get_subject_alias(&alias(
            "alias_al",
            "personal",
            "semantic-ws",
            "subj_alice",
            "person:al",
        ))
        .unwrap();
    assert!(created);
    assert_eq!(stored.source_evidence, "ep_alias");

    let resolved = store
        .resolve_subject_alias("personal", "semantic-ws", "person:al")
        .unwrap()
        .expect("personal alias resolves");
    assert_eq!(resolved.id, "subj_alice");
    assert!(store
        .resolve_subject_alias("work", "semantic-ws", "person:al")
        .unwrap()
        .is_none());

    let err = store
        .insert_or_get_subject_alias(&alias(
            "alias_cross",
            "personal",
            "semantic-ws",
            "subj_work_alice",
            "person:work-alice",
        ))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ProfileBoundaryDenied);
}

#[test]
fn relations_are_evidence_backed_and_never_expand_cross_profile() {
    let store = Store::open(":memory:").unwrap();
    store.ensure_workspace("personal", "semantic-ws").unwrap();
    store.ensure_workspace("work", "semantic-ws").unwrap();
    for item in [
        subject(
            "subj_lighthouse",
            "personal",
            "semantic-ws",
            "project:lighthouse",
            SubjectKind::Project,
        ),
        subject(
            "subj_blue_harbor",
            "personal",
            "semantic-ws",
            "project:blue-harbor",
            SubjectKind::Project,
        ),
        subject(
            "subj_work_secret",
            "work",
            "semantic-ws",
            "service:internal-vault",
            SubjectKind::Concept,
        ),
    ] {
        store.insert_or_get_subject(&item).unwrap();
    }

    let (stored, created) = store
        .insert_or_get_relation(&relation(
            "rel_lighthouse_codename",
            "personal",
            "semantic-ws",
            "subj_lighthouse",
            "uses",
            "subj_blue_harbor",
            vec!["ep_lighthouse"],
        ))
        .unwrap();
    assert!(created);
    assert_eq!(stored.source_episode_ids, vec!["ep_lighthouse"]);

    let expanded = store
        .relation_expanded_subjects(
            "personal",
            "semantic-ws",
            &["subj_lighthouse".to_string()],
            2,
        )
        .unwrap();
    let ids = expanded
        .iter()
        .map(|subject| subject.subject_id.as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"subj_blue_harbor"));
    assert!(!ids.contains(&"subj_work_secret"));
    assert!(expanded
        .iter()
        .any(|subject| subject.evidence_refs.contains(&"ep_lighthouse".to_string())));

    let err = store
        .insert_or_get_relation(&relation(
            "rel_cross",
            "personal",
            "semantic-ws",
            "subj_lighthouse",
            "depends_on",
            "subj_work_secret",
            vec!["ep_cross"],
        ))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ProfileBoundaryDenied);

    let missing_episode_evidence = relation(
        "rel_missing_episode_evidence",
        "personal",
        "semantic-ws",
        "subj_lighthouse",
        "depends_on",
        "subj_blue_harbor",
        vec![],
    );
    let err = store
        .insert_or_get_relation(&missing_episode_evidence)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidRequest);

    let mut missing_source_evidence = relation(
        "rel_missing_source_evidence",
        "personal",
        "semantic-ws",
        "subj_lighthouse",
        "depends_on",
        "subj_blue_harbor",
        vec!["ep_missing_source"],
    );
    missing_source_evidence.source_evidence = None;
    let err = store
        .insert_or_get_relation(&missing_source_evidence)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidRequest);
}
