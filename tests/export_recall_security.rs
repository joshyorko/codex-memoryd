use codex_memoryd::domain::Portability;
use codex_memoryd::domain::Profile;
use codex_memoryd::domain::RecordType;
use codex_memoryd::domain::Scope;
use codex_memoryd::domain::Sensitivity;
use codex_memoryd::export;
use codex_memoryd::export::ExportFormat;
use codex_memoryd::export::ExportParams;
use codex_memoryd::ids;
use codex_memoryd::recall;
use codex_memoryd::recall::RecallParams;
use codex_memoryd::recall::SearchParams;
use codex_memoryd::store::NewRecord;
use codex_memoryd::store::Store;

fn store() -> Store {
    let store = Store::open(":memory:").expect("open store");
    store.ensure_workspace("personal", "ws").expect("workspace");
    store
}

fn insert_record(store: &Store, content: &str, sensitivity: Sensitivity, archived: bool) -> String {
    insert_scoped_record(
        store,
        "personal",
        "ws",
        None,
        content,
        sensitivity,
        archived,
    )
}

fn insert_scoped_record(
    store: &Store,
    profile_id: &str,
    workspace_id: &str,
    repo_id: Option<&str>,
    content: &str,
    sensitivity: Sensitivity,
    archived: bool,
) -> String {
    let rec = NewRecord {
        profile_id: profile_id.to_string(),
        workspace_id: workspace_id.to_string(),
        repo_id: repo_id.map(|repo| repo.to_string()),
        subject_id: None,
        episode_id: None,
        scope: Scope::Workspace,
        record_type: RecordType::Other,
        content: content.to_string(),
        related_files: vec![],
        tags: vec![],
        sensitivity,
        portability: Portability::Portable,
        confidence: 0.8,
        source_ids: vec![],
        content_hash: ids::content_hash(
            profile_id,
            workspace_id,
            repo_id,
            RecordType::Other.as_str(),
            "workspace",
            content,
        ),
        supersedes: vec![],
        metadata: serde_json::Value::Null,
    };
    let outcome = store.upsert_record(&rec).expect("insert record");
    let id = outcome.id().to_string();
    if archived {
        let (archived_ids, not_found) = store
            .archive_records("personal", Some("ws"), std::slice::from_ref(&id))
            .expect("archive record");
        assert_eq!(archived_ids, vec![id.clone()]);
        assert!(not_found.is_empty());
    }
    id
}

fn insert_record_with_metadata(
    store: &Store,
    content: &str,
    metadata: serde_json::Value,
) -> String {
    let rec = NewRecord {
        profile_id: "personal".to_string(),
        workspace_id: "ws".to_string(),
        repo_id: None,
        subject_id: None,
        episode_id: None,
        scope: Scope::Workspace,
        record_type: RecordType::Other,
        content: content.to_string(),
        related_files: vec![],
        tags: vec![],
        sensitivity: Sensitivity::Personal,
        portability: Portability::Portable,
        confidence: 0.8,
        source_ids: vec![],
        content_hash: ids::content_hash(
            "personal",
            "ws",
            None,
            RecordType::Other.as_str(),
            "workspace",
            content,
        ),
        supersedes: vec![],
        metadata,
    };
    store
        .upsert_record(&rec)
        .expect("insert record")
        .id()
        .to_string()
}

#[test]
fn recall_ignores_secret_blocked_records() {
    let store = store();
    insert_record(&store, "visible recall note", Sensitivity::Personal, false);
    insert_record(
        &store,
        "secret recall note",
        Sensitivity::SecretBlocked,
        false,
    );

    let params = RecallParams {
        profile: Profile::Personal,
        workspace: "ws",
        repo: None,
        query: "recall note",
        files: &[],
        max_tokens: 1000,
        pack_mode: "default",
        include_types: &[],
        exclude_types: &[],
        recency_days: None,
    };

    let resp = recall::recall(&store, &params).expect("recall");
    assert_eq!(resp.facts.len(), 1);
    assert_eq!(resp.citations.len(), 1);
    assert!(resp
        .facts
        .iter()
        .all(|fact| !fact.content.contains("secret recall note")));
    assert!(resp
        .summary
        .as_deref()
        .unwrap_or("")
        .contains("1 relevant memory record"));
}

#[test]
fn recall_withholds_quarantined_high_risk_unsafe_and_superseded_metadata_by_default() {
    let store = store();
    let visible_id = insert_record_with_metadata(
        &store,
        "visible safe recall note",
        serde_json::json!({"origin": "conclusion"}),
    );
    insert_record_with_metadata(
        &store,
        "poison quarantined recall note",
        serde_json::json!({"candidate_state": "quarantined"}),
    );
    insert_record_with_metadata(
        &store,
        "poison high risk recall note",
        serde_json::json!({"source_risk": "high"}),
    );
    insert_record_with_metadata(
        &store,
        "poison unsafe recall note",
        serde_json::json!({"admission": "unsafe"}),
    );
    insert_record_with_metadata(
        &store,
        "poison superseded recall note",
        serde_json::json!({"state": "superseded"}),
    );

    let params = RecallParams {
        profile: Profile::Personal,
        workspace: "ws",
        repo: None,
        query: "recall note",
        files: &[],
        max_tokens: 1000,
        pack_mode: "default",
        include_types: &[],
        exclude_types: &[],
        recency_days: None,
    };

    let resp = recall::recall(&store, &params).expect("recall");
    assert_eq!(resp.authority, "recall_not_authority");
    assert_eq!(resp.facts.len(), 1);
    assert_eq!(resp.facts[0].id, visible_id);
    assert!(resp
        .facts
        .iter()
        .all(|fact| !fact.content.contains("poison")));
    assert!(resp
        .withheld
        .iter()
        .any(|withheld| { withheld.reason == "policy_quarantined" && withheld.count == 1 }));
    assert!(resp
        .withheld
        .iter()
        .any(|withheld| { withheld.reason == "policy_high_risk" && withheld.count == 1 }));
    assert!(resp
        .withheld
        .iter()
        .any(|withheld| { withheld.reason == "policy_unsafe" && withheld.count == 1 }));
    assert!(resp
        .withheld
        .iter()
        .any(|withheld| { withheld.reason == "policy_superseded" && withheld.count == 1 }));
}

#[test]
fn recall_cross_profile_bleed_remains_default_deny() {
    let store = store();
    store
        .ensure_workspace("work", "ws")
        .expect("work workspace");
    insert_scoped_record(
        &store,
        "work",
        "ws",
        None,
        "same query work-only recall note",
        Sensitivity::WorkConfidential,
        false,
    );
    let personal_id = insert_record(
        &store,
        "same query personal recall note",
        Sensitivity::Personal,
        false,
    );

    let params = RecallParams {
        profile: Profile::Personal,
        workspace: "ws",
        repo: None,
        query: "same query recall note",
        files: &[],
        max_tokens: 1000,
        pack_mode: "default",
        include_types: &[],
        exclude_types: &[],
        recency_days: None,
    };

    let resp = recall::recall(&store, &params).expect("recall");
    assert_eq!(resp.facts.len(), 1);
    assert_eq!(resp.facts[0].id, personal_id);
    assert!(!resp.facts[0].content.contains("work-only"));
}

#[test]
fn recall_allows_legacy_metadata_without_admission_markers() {
    let store = store();
    let id = insert_record_with_metadata(
        &store,
        "legacy metadata recall note",
        serde_json::json!({"origin": "sync_local", "local_path": "memory/MEMORY.md"}),
    );

    let params = RecallParams {
        profile: Profile::Personal,
        workspace: "ws",
        repo: None,
        query: "legacy metadata",
        files: &[],
        max_tokens: 1000,
        pack_mode: "default",
        include_types: &[],
        exclude_types: &[],
        recency_days: None,
    };

    let resp = recall::recall(&store, &params).expect("recall");
    assert_eq!(resp.authority, "recall_not_authority");
    assert_eq!(resp.policy.authority, "recall_not_authority");
    assert_eq!(resp.facts.len(), 1);
    assert_eq!(resp.facts[0].id, id);
}

#[test]
fn search_ignores_secret_blocked_records_even_when_archived() {
    let store = store();
    insert_record(
        &store,
        "secret search note",
        Sensitivity::SecretBlocked,
        true,
    );

    let params = SearchParams {
        profile: Profile::Personal,
        workspace: Some("ws"),
        repo_id: None,
        query: "secret search note",
        scope: None,
        record_type: None,
        include_archived: true,
        limit: 10,
        offset: 0,
    };

    let resp = recall::search(&store, &params).expect("search");
    assert!(resp.matches.is_empty());
}

#[test]
fn export_counts_and_omits_secret_blocked_records() {
    let store = store();
    insert_record(&store, "visible export note", Sensitivity::Personal, false);
    insert_record(
        &store,
        "secret export note",
        Sensitivity::SecretBlocked,
        true,
    );

    let params = ExportParams {
        profile: Profile::Personal,
        workspace: Some("ws"),
        repo_id: None,
        include_archived: true,
        format: ExportFormat::Jsonl,
        target_profile: None,
    };

    let result = export::export(&store, &params).expect("export");
    assert_eq!(result.record_count, 1);
    assert_eq!(result.omitted_secret, 1);
    assert_eq!(result.omitted_boundary, 0);
    assert!(result.body.contains("visible export note"));
    assert!(!result.body.contains("secret export note"));
}

#[test]
fn export_secret_count_respects_scope_filters() {
    let store = store();
    store
        .ensure_workspace("personal", "other-ws")
        .expect("workspace");

    insert_scoped_record(
        &store,
        "personal",
        "ws",
        Some("repo-a"),
        "visible scoped export note",
        Sensitivity::Personal,
        false,
    );
    insert_scoped_record(
        &store,
        "personal",
        "ws",
        Some("repo-a"),
        "matching secret export note",
        Sensitivity::SecretBlocked,
        false,
    );
    insert_scoped_record(
        &store,
        "personal",
        "ws",
        Some("repo-b"),
        "other repo secret export note",
        Sensitivity::SecretBlocked,
        false,
    );
    insert_scoped_record(
        &store,
        "personal",
        "other-ws",
        Some("repo-a"),
        "other workspace secret export note",
        Sensitivity::SecretBlocked,
        false,
    );
    insert_scoped_record(
        &store,
        "personal",
        "ws",
        Some("repo-a"),
        "archived secret export note",
        Sensitivity::SecretBlocked,
        true,
    );

    let params = ExportParams {
        profile: Profile::Personal,
        workspace: Some("ws"),
        repo_id: Some("repo-a"),
        include_archived: false,
        format: ExportFormat::Jsonl,
        target_profile: None,
    };

    let result = export::export(&store, &params).expect("export");
    assert_eq!(result.record_count, 1);
    assert_eq!(result.omitted_secret, 1);
    assert_eq!(result.omitted_boundary, 0);
    assert!(result.body.contains("visible scoped export note"));
    assert!(!result.body.contains("matching secret export note"));
    assert!(!result.body.contains("other repo secret export note"));
    assert!(!result.body.contains("other workspace secret export note"));
    assert!(!result.body.contains("archived secret export note"));
}
