use codex_memoryd::config::Config;
use codex_memoryd::domain::RepoIdentity;
use codex_memoryd::protocol::*;
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

fn temp_service() -> (Service, TempDir, String) {
    let tempdir = TempDir::new().expect("tempdir");
    let db_path = tempdir.path().join("codex-memoryd.sqlite");
    let store = Store::open(&db_path).expect("open store");
    let config = Config {
        default_workspace: "ws".to_string(),
        ..Default::default()
    };
    (
        Service::new(store, config),
        tempdir,
        db_path.to_string_lossy().into_owned(),
    )
}

#[derive(Debug)]
struct LedgerRow {
    profile_id: String,
    workspace_id: String,
    repo_id: Option<String>,
    subject_key: Option<String>,
    source_kind: String,
    source_id: Option<String>,
    source_path: Option<String>,
    source_hash: String,
    safe_summary: String,
    policy_state: String,
    metadata: Value,
}

fn ledger_rows(db_path: &str) -> Vec<LedgerRow> {
    let conn = Connection::open(db_path).expect("open sqlite");
    let mut stmt = conn
        .prepare(
            "SELECT profile_id, workspace_id, repo_id, subject_key, source_kind,
                    source_id, source_path, source_hash, safe_summary, policy_state,
                    metadata
             FROM evidence_ledger
             ORDER BY created_at, id",
        )
        .expect("prepare ledger query");
    stmt.query_map([], |row| {
        Ok(LedgerRow {
            profile_id: row.get(0)?,
            workspace_id: row.get(1)?,
            repo_id: row.get(2)?,
            subject_key: row.get(3)?,
            source_kind: row.get(4)?,
            source_id: row.get(5)?,
            source_path: row.get(6)?,
            source_hash: row.get(7)?,
            safe_summary: row.get(8)?,
            policy_state: row.get(9)?,
            metadata: serde_json::from_str(&row.get::<_, String>(10)?).unwrap_or(Value::Null),
        })
    })
    .expect("query ledger")
    .collect::<std::result::Result<Vec<_>, _>>()
    .expect("collect ledger rows")
}

fn ledger_count(db_path: &str) -> usize {
    ledger_rows(db_path).len()
}

#[test]
fn status_reports_bumped_storage_schema_version() {
    let (svc, _tmp, _db_path) = temp_service();
    let status = svc.status().expect("status");
    assert_eq!(status.storage_schema_version, 3);
}

#[test]
fn accepted_writes_append_scoped_evidence_rows() {
    let (svc, _tmp, db_path) = temp_service();

    svc.turns(TurnsRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        session: Some(TurnSession {
            id: Some("sess-ledger".to_string()),
            thread_id: Some("thread-ledger".to_string()),
            source: Some("test".to_string()),
            metadata: None,
        }),
        messages: Some(vec![TurnMessage {
            actor: "user".to_string(),
            content: "I prefer cargo test for this repo".to_string(),
            created_at: Some("2026-06-09T00:00:00Z".to_string()),
            metadata: None,
        }]),
        write_policy: None,
    })
    .expect("turn accepted");

    svc.conclusions(ConclusionsRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: Some(RepoIdentity {
            repo_id: "git:repo-ledger".to_string(),
            is_git: true,
            ..Default::default()
        }),
        target: Some("user".to_string()),
        conclusions: Some(vec![
            "Decision: use rusqlite with bundled SQLite".to_string()
        ]),
        metadata: None,
        record_type: None,
    })
    .expect("conclusion accepted");

    svc.checkpoint(CheckpointRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        session: None,
        summary: Some("Implemented the evidence ledger MVP".to_string()),
        changed_files: vec!["src/store.rs".to_string()],
        decisions: vec!["append-only evidence rows".to_string()],
        blockers: vec![],
        next_steps: vec!["run focused tests".to_string()],
        tests_run: vec!["cargo test --test evidence_ledger".to_string()],
        tests_not_run: vec![],
        branch: None,
        commit: None,
    })
    .expect("checkpoint accepted");

    let rows = ledger_rows(&db_path);
    assert_eq!(rows.len(), 3);

    let turn = rows
        .iter()
        .find(|row| row.source_kind == "visible_turn")
        .unwrap();
    assert_eq!(turn.profile_id, "personal");
    assert_eq!(turn.workspace_id, "ws");
    assert_eq!(turn.policy_state, "accepted");
    assert!(turn.source_id.is_some());
    assert!(turn
        .source_path
        .as_deref()
        .unwrap_or("")
        .starts_with("turn:"));
    assert!(turn.source_hash.starts_with("sha256:"));
    assert!(turn.safe_summary.contains("cargo test"));

    let concl = rows
        .iter()
        .find(|row| row.source_kind == "conclusion")
        .unwrap();
    assert_eq!(concl.repo_id.as_deref(), Some("git:repo-ledger"));
    assert_eq!(concl.policy_state, "accepted");
    assert!(concl.source_id.is_some());
    assert!(concl.safe_summary.contains("rusqlite"));

    let ckpt = rows
        .iter()
        .find(|row| row.source_kind == "checkpoint")
        .unwrap();
    assert_eq!(ckpt.policy_state, "accepted");
    assert_eq!(ckpt.subject_key, None);
    assert!(ckpt.safe_summary.contains("evidence ledger"));
    assert_eq!(ckpt.metadata["checkpoint_id"].as_str().is_some(), true);
}

#[test]
fn rejected_secret_turn_records_redacted_ledger_row() {
    let (svc, _tmp, db_path) = temp_service();

    let err = svc
        .turns(TurnsRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            session: Some(TurnSession {
                id: Some("sess-secret".to_string()),
                thread_id: None,
                source: Some("test".to_string()),
                metadata: None,
            }),
            messages: Some(vec![TurnMessage {
                actor: "user".to_string(),
                content: "Here is my key: ghp_abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
                created_at: None,
                metadata: None,
            }]),
            write_policy: None,
        })
        .expect("turns should reject but still respond");

    assert_eq!(err.rejected, 1);

    let rows = ledger_rows(&db_path);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.source_kind, "visible_turn");
    assert_eq!(row.policy_state, "secret_detected");
    assert!(row.source_hash.starts_with("sha256:"));
    assert!(row.safe_summary.to_ascii_lowercase().contains("secret"));
    assert!(!row
        .safe_summary
        .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
}

#[test]
fn distinct_rejected_conclusions_keep_distinct_ledger_rows() {
    let (svc, _tmp, db_path) = temp_service();

    let resp = svc
        .conclusions(ConclusionsRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            target: Some("user".to_string()),
            conclusions: Some(vec![
                "Use ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaa for one".to_string(),
                "Use ghp_bbbbbbbbbbbbbbbbbbbbbbbbbbbb for two".to_string(),
            ]),
            metadata: None,
            record_type: None,
        })
        .expect("conclusions should reject but still respond");

    assert_eq!(resp.rejected.len(), 2);
    let rows = ledger_rows(&db_path);
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|row| row.source_kind == "conclusion" && row.policy_state == "secret_detected"));
    assert!(rows.iter().all(|row| !row.safe_summary.contains("ghp_")));
}

#[test]
fn sync_apply_is_idempotent_for_ledger_rows() {
    let (svc, _tmp, db_path) = temp_service();
    let file = SyncFile {
        path: "memory_summary.md".to_string(),
        kind: Some("memory_summary".to_string()),
        content: "# Memory\n\n- I prefer cargo test\n".to_string(),
        hash: None,
        modified_at: Some("2026-06-09T00:00:00Z".to_string()),
        idempotency_key: Some("sync-ledger".to_string()),
        metadata: None,
    };

    let req = SyncRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        source_root: Some("/tmp/memory".to_string()),
        mode: Some("apply".to_string()),
        files: Some(vec![file.clone()]),
        metadata: None,
    };

    svc.sync_local(req.clone()).expect("first sync");
    let after_first = ledger_count(&db_path);
    svc.sync_local(req).expect("second sync");
    let after_second = ledger_count(&db_path);

    assert_eq!(after_first, 1);
    assert_eq!(after_second, 1);
    let row = &ledger_rows(&db_path)[0];
    assert_eq!(row.source_kind, "sync_local");
    assert_eq!(row.policy_state, "accepted");
    assert_eq!(row.source_path.as_deref(), Some("memory_summary.md"));
    assert!(row.source_hash.starts_with("sha256:"));
    assert!(row.safe_summary.contains("memory_summary"));
}

#[test]
fn sync_preview_rejections_do_not_append_ledger_rows() {
    let (svc, _tmp, db_path) = temp_service();
    let req = SyncRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        source_root: Some("/tmp/memory".to_string()),
        mode: Some("preview".to_string()),
        files: Some(vec![SyncFile {
            path: "secrets.md".to_string(),
            kind: Some("memory_summary".to_string()),
            content: "Here is my key: ghp_abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
            hash: None,
            modified_at: None,
            idempotency_key: None,
            metadata: None,
        }]),
        metadata: None,
    };

    let resp = svc.sync_local(req).expect("preview sync responds");

    assert_eq!(resp.rejected, 1);
    assert_eq!(ledger_count(&db_path), 0);
}

#[test]
fn dream_apply_is_idempotent_for_ledger_rows() {
    let (svc, _tmp, db_path) = temp_service();

    svc.conclusions(ConclusionsRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        target: Some("user".to_string()),
        conclusions: Some(vec![
            "Decision: use cargo test for the repository".to_string()
        ]),
        metadata: None,
        record_type: None,
    })
    .expect("seed conclusion");

    let before = ledger_count(&db_path);
    let req = DreamRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        mode: Some("apply".to_string()),
        now: Some("2026-06-10T00:00:00Z".to_string()),
        since: Some("2026-06-01T00:00:00Z".to_string()),
    };

    svc.dream(req.clone()).expect("first dream apply");
    let after_first = ledger_count(&db_path);
    svc.dream(req).expect("second dream apply");
    let after_second = ledger_count(&db_path);

    assert_eq!(after_first, before + 1);
    assert_eq!(after_second, after_first);
    let dream = ledger_rows(&db_path)
        .into_iter()
        .find(|row| row.source_kind == "dream_apply")
        .expect("dream ledger row");
    assert_eq!(dream.policy_state, "accepted");
    assert!(dream.subject_key.is_some());
    assert!(dream.source_hash.starts_with("sha256:"));
    assert!(dream.metadata["dream_run_id"].as_str().is_some());
}
