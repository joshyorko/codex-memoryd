use codex_memoryd::config::Config;
use codex_memoryd::domain::{Portability, RecordType, Scope, Sensitivity};
use codex_memoryd::ids;
use codex_memoryd::protocol::{
    DreamRequest, MemoryPatchApplyRequest, MemoryPatchExplainRequest, MemoryPatchRollbackRequest,
};
use codex_memoryd::service::Service;
use codex_memoryd::store::{NewRecord, Store, UpsertOutcome};
use rusqlite::{params, Connection};
use tempfile::TempDir;

fn service() -> (Service, TempDir, std::path::PathBuf) {
    let tempdir = TempDir::new().expect("tempdir");
    let db_path = tempdir.path().join("patches.sqlite");
    let store = Store::open(&db_path).expect("open store");
    let config = Config {
        default_workspace: "ws".to_string(),
        ..Default::default()
    };
    (Service::new(store, config), tempdir, db_path)
}

fn seed_turns(svc: &Service, db_path: &std::path::Path) -> String {
    let content = "I will patch the daemon tomorrow.".to_string();
    let record = NewRecord {
        profile_id: "personal".to_string(),
        workspace_id: "ws".to_string(),
        repo_id: None,
        scope: Scope::Workspace,
        record_type: RecordType::Decision,
        content: content.clone(),
        related_files: vec![],
        tags: vec!["seed".to_string()],
        sensitivity: Sensitivity::Personal,
        portability: Portability::ProfileOnly,
        confidence: 0.9,
        source_ids: vec![],
        content_hash: ids::content_hash("personal", "ws", None, "decision", "workspace", &content),
        supersedes: vec![],
        metadata: serde_json::json!({
            "origin": "conclusion",
            "target": "user",
        }),
    };
    let id = match svc.store.upsert_record(&record).expect("seed record") {
        UpsertOutcome::Created(id) | UpsertOutcome::Skipped(id) => id,
    };
    let conn = Connection::open(db_path).expect("open sqlite");
    conn.execute(
        "UPDATE memory_records SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
        params!["2029-12-20T00:00:00Z", id],
    )
    .expect("age record");
    id
}

#[test]
fn patch_lifecycle_binds_preview_applies_and_rolls_back_safely() {
    let (svc, _tmpdir, db_path) = service();
    let original_id = seed_turns(&svc, &db_path);
    let now = Some("2030-01-01T00:00:00Z".to_string());

    let preview = svc
        .patch_preview(DreamRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            mode: Some("preview".to_string()),
            now: now.clone(),
            since: None,
        })
        .expect("patch preview");

    assert!(preview.markdown.contains("# Memory patch preview"));
    assert!(preview.actions.iter().any(|action| action.op == "create"));

    let mismatch = svc.patch_apply(MemoryPatchApplyRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        run_id: format!("{}-mismatch", preview.run_id),
        now: now.clone(),
        since: None,
    });
    assert!(mismatch.is_err());

    let applied = svc
        .patch_apply(MemoryPatchApplyRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            run_id: preview.run_id.clone(),
            now: now.clone(),
            since: None,
        })
        .expect("patch apply");
    assert_ne!(applied.preview_run_id, applied.applied.run_id);
    assert!(!applied.applied.created.is_empty());

    let memory_id = applied.applied.created[0].clone();
    let explain = svc
        .patch_explain(MemoryPatchExplainRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            run_id: None,
            memory_id: Some(memory_id.clone()),
        })
        .expect("patch explain");
    let item = explain
        .items
        .iter()
        .find(|item| item.memory_id == memory_id)
        .unwrap();
    assert_eq!(item.patch_run_id, Some(preview.run_id.clone()));
    assert_eq!(item.policy_outcome, "accepted");
    assert!(!item.source_refs.is_empty());

    let rollback_preview = svc
        .patch_rollback(MemoryPatchRollbackRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            run_id: preview.run_id.clone(),
            preview: true,
            now: None,
        })
        .expect("rollback preview");
    assert!(rollback_preview.markdown.contains("Rollback preview"));
    assert!(rollback_preview.archived.contains(&memory_id));
    assert!(rollback_preview.restored.contains(&original_id));

    let rollback_apply = svc
        .patch_rollback(MemoryPatchRollbackRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            run_id: preview.run_id.clone(),
            preview: false,
            now: None,
        })
        .expect("rollback apply");
    assert!(rollback_apply.archived.contains(&memory_id));

    let record = svc
        .store
        .get_record(&memory_id)
        .expect("fetch record")
        .unwrap();
    assert!(record.archived);

    let original_record = svc
        .store
        .get_record(&original_id)
        .expect("fetch original record")
        .unwrap();
    assert!(!original_record.archived);
}
