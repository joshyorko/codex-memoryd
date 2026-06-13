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
