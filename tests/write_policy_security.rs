use codex_memoryd::config::Config;
use codex_memoryd::error::ErrorCode;
use codex_memoryd::protocol::*;
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

fn temp_service() -> (Service, TempDir, String) {
    let tempdir = TempDir::new().expect("tempdir");
    let db_path = tempdir.path().join("codex-memoryd.sqlite");
    let db_path_str = db_path.to_string_lossy().into_owned();
    let store = Store::open(&db_path).expect("open store");
    let config = Config {
        default_workspace: "ws".to_string(),
        ..Default::default()
    };
    (Service::new(store, config), tempdir, db_path_str)
}

fn count_rows(db_path: &str, table: &str) -> i64 {
    let conn = Connection::open(db_path).expect("open sqlite");
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .expect("count rows")
}

#[test]
fn checkpoint_rejects_secret_in_detail_field_and_does_not_persist() {
    let (svc, _tmp, db_path) = temp_service();

    let err = svc
        .checkpoint(CheckpointRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            session: None,
            summary: Some("Implemented the OAuth callback handler".to_string()),
            changed_files: vec!["src/oauth.rs".to_string()],
            decisions: vec!["Use token ghp_abcdefghijklmnopqrstuvwxyz0123456789".to_string()],
            blockers: vec![],
            next_steps: vec!["wire the web flow".to_string()],
            tests_run: vec!["cargo test --lib".to_string()],
            tests_not_run: vec![],
            branch: Some("feature/oauth".to_string()),
            commit: None,
        })
        .expect_err("checkpoint should reject secret-bearing detail fields");

    assert_eq!(err.code, ErrorCode::SecretDetected);
    assert_eq!(count_rows(&db_path, "checkpoints"), 0);
    assert_eq!(count_rows(&db_path, "memory_records"), 0);

    let recall = svc
        .recall(RecallRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            session: None,
            query: Some("OAuth callback".to_string()),
            files: vec![],
            max_tokens: Some(1000),
            pack_mode: None,
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            metadata: None,
        })
        .expect("recall after rejection");
    assert!(recall.checkpoints.is_empty());
    assert!(recall.facts.is_empty());
}

#[test]
fn conclusions_reject_nested_secret_metadata_and_do_not_persist() {
    let (svc, _tmp, db_path) = temp_service();

    let err = svc
        .conclusions(ConclusionsRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            target: Some("user".to_string()),
            conclusions: Some(vec!["Decision: use sqlite for local durability".to_string()]),
            metadata: Some(json!({
                "origin": "integration-test",
                "nested": {
                    "api_key": "sk-test-1234567890abcdefghijklmnop"
                }
            })),
            record_type: None,
        })
        .expect_err("conclusions metadata should be screened");

    assert_eq!(err.code, ErrorCode::SecretDetected);
    assert_eq!(count_rows(&db_path, "conclusions"), 0);
    assert_eq!(count_rows(&db_path, "memory_records"), 0);
}

#[test]
fn repo_remote_with_credentials_is_rejected_and_not_persisted() {
    let (svc, _tmp, db_path) = temp_service();

    let err = svc
        .conclusions(ConclusionsRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: Some(codex_memoryd::domain::RepoIdentity {
                repo_id: "git:https://github.com/example/private-repo".to_string(),
                root: Some("/worktrees/private-repo".to_string()),
                remote: Some(
                    "https://oauth2:plain-password@github.com/example/private-repo.git".to_string(),
                ),
                branch: Some("main".to_string()),
                commit: Some("abc123".to_string()),
                is_git: true,
            }),
            target: Some("user".to_string()),
            conclusions: Some(vec!["Decision: keep the provider local-first".to_string()]),
            metadata: None,
            record_type: None,
        })
        .expect_err("credential-bearing remotes must be rejected");

    assert_eq!(err.code, ErrorCode::SecretDetected);
    assert_eq!(count_rows(&db_path, "repos"), 0);
    assert_eq!(count_rows(&db_path, "conclusions"), 0);
    assert_eq!(count_rows(&db_path, "memory_records"), 0);
}

#[test]
fn checkpoint_memory_record_upsert_errors_are_not_swallowed() {
    let (svc, _tmp, db_path) = temp_service();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute("DROP TABLE memory_records", [])
        .expect("drop memory_records");

    let err = svc
        .checkpoint(CheckpointRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            session: None,
            summary: Some("Implemented the checkpoint writer".to_string()),
            changed_files: vec!["src/service.rs".to_string()],
            decisions: vec!["keep the write path narrow".to_string()],
            blockers: vec![],
            next_steps: vec!["add auth middleware".to_string()],
            tests_run: vec!["cargo test --test write_policy_security".to_string()],
            tests_not_run: vec![],
            branch: Some("patch/checkpoint-hardening".to_string()),
            commit: None,
        })
        .expect_err("upsert failures must surface to the caller");

    assert_eq!(err.code, ErrorCode::StorageUnavailable);
    assert_eq!(count_rows(&db_path, "checkpoints"), 1);
}
