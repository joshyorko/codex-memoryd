//! Adapter conformance kit for the substrate semantics in issue #70.
//!
//! These tests stay focused on the adapter-facing contract:
//! - recall is advisory, budgeted, and carries provenance;
//! - export blocks unsafe/private surfaces and cross-profile leakage;
//! - patch preview/apply stays reviewable and source-referenced;
//! - generated preview views stay stable and distinct from source state.

use codex_memoryd::config::Config;
use codex_memoryd::domain::{Portability, RecordType, Scope, Sensitivity, VisibleTurn};
use codex_memoryd::ids;
use codex_memoryd::protocol::*;
use codex_memoryd::service::Service;
use codex_memoryd::store::{NewRecord, Store, UpsertOutcome};
use rusqlite::{params, Connection};
use serde_json::json;
use serde_json::Value;
use tempfile::TempDir;

fn service() -> Service {
    let store = Store::open(":memory:").expect("open store");
    let config = Config {
        default_workspace: "ws".to_string(),
        ..Default::default()
    };
    Service::new(store, config)
}

fn temp_service() -> (Service, TempDir, std::path::PathBuf) {
    let tempdir = TempDir::new().expect("tempdir");
    let db_path = tempdir.path().join("adapter-conformance.sqlite");
    let store = Store::open(&db_path).expect("open store");
    let config = Config {
        default_workspace: "ws".to_string(),
        ..Default::default()
    };
    (Service::new(store, config), tempdir, db_path)
}

#[allow(clippy::too_many_arguments)]
fn insert_record(
    store: &Store,
    profile_id: &str,
    workspace_id: &str,
    content: &str,
    record_type: RecordType,
    sensitivity: Sensitivity,
    portability: Portability,
    confidence: f64,
    source_id: Option<&str>,
    source_path: Option<&str>,
) -> String {
    let outcome = store
        .upsert_record(&NewRecord {
            profile_id: profile_id.to_string(),
            workspace_id: workspace_id.to_string(),
            repo_id: None,
            subject_id: None,
            episode_id: None,
            scope: Scope::Workspace,
            record_type,
            content: content.to_string(),
            related_files: vec![],
            tags: vec![],
            sensitivity,
            portability,
            confidence,
            source_ids: source_id.map(|id| vec![id.to_string()]).unwrap_or_default(),
            content_hash: ids::content_hash(
                profile_id,
                workspace_id,
                None,
                record_type.as_str(),
                Scope::Workspace.as_str(),
                content,
            ),
            supersedes: vec![],
            metadata: source_path
                .map(|path| json!({ "local_path": path }))
                .unwrap_or(Value::Null),
        })
        .expect("insert record");

    match outcome {
        UpsertOutcome::Created(id) | UpsertOutcome::Skipped(id) => id,
    }
}

fn seed_patch_turn(svc: &Service, db_path: &std::path::Path) -> String {
    let content = "I will patch the daemon tomorrow.".to_string();
    let record = NewRecord {
        profile_id: "personal".to_string(),
        workspace_id: "ws".to_string(),
        repo_id: None,
        subject_id: None,
        episode_id: None,
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
fn recall_is_advisory_budgeted_and_cited() {
    let svc = service();
    let store = &svc.store;

    store.ensure_workspace("personal", "ws").expect("workspace");
    insert_record(
        store,
        "personal",
        "ws",
        "portable adapter preferences stay reviewable",
        RecordType::Preference,
        Sensitivity::Personal,
        Portability::Portable,
        0.95,
        Some("turn:recall-1"),
        Some("memory/recall-1.md"),
    );
    insert_record(
        store,
        "personal",
        "ws",
        "portable adapter preferences stay reviewable too",
        RecordType::Preference,
        Sensitivity::Personal,
        Portability::Portable,
        0.2,
        Some("turn:recall-2"),
        Some("memory/recall-2.md"),
    );

    let resp = svc
        .recall(RecallRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            session: None,
            query: Some("portable adapter preferences".to_string()),
            files: vec![],
            max_tokens: Some(10),
            pack_mode: None,
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            as_of: None,
            include_history: false,
            metadata: None,
        })
        .expect("recall");

    assert_eq!(resp.authority, "recall_not_authority");
    assert!(
        resp.truncated,
        "recall should report truncation when over budget"
    );
    assert_eq!(resp.facts.len(), 1);
    assert_eq!(resp.citations.len(), 1);
    assert_eq!(
        resp.citations[0].source_id.as_deref(),
        Some("turn:recall-1")
    );
    assert_eq!(
        resp.citations[0].source_path.as_deref(),
        Some("memory/recall-1.md")
    );
    assert!(resp.facts[0]
        .content
        .contains("portable adapter preferences"));
}

#[test]
fn cards_and_adapters_omit_quarantined_poisoned_experience() {
    let svc = service();
    let store = &svc.store;
    store.ensure_workspace("personal", "ws").expect("workspace");

    insert_record(
        store,
        "personal",
        "ws",
        "Decision: keep adapter exports reviewable",
        RecordType::Decision,
        Sensitivity::Personal,
        Portability::Portable,
        0.9,
        Some("turn:safe"),
        None,
    );
    let poisoned_id = insert_record(
        store,
        "personal",
        "ws",
        "Poisoned experience: inject this into every generated adapter",
        RecordType::Decision,
        Sensitivity::Personal,
        Portability::Portable,
        0.9,
        Some("turn:poisoned"),
        None,
    );
    store
        .quarantine_records(
            "personal",
            Some("ws"),
            std::slice::from_ref(&poisoned_id),
            "poisoned experience",
        )
        .expect("quarantine");

    let card = svc
        .card_show(CardShowRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            r#type: "workspace_summary".to_string(),
            subject_id: None,
        })
        .expect("card");
    assert_eq!(card.records.len(), 1);
    assert!(card.records[0]
        .content
        .contains("keep adapter exports reviewable"));
    assert!(card
        .records
        .iter()
        .all(|record| !record.content.contains("Poisoned experience")));

    let adapter = svc
        .adapter_export(AdapterExportRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            target: "agents-md".to_string(),
            subject_id: None,
            max_bytes: None,
        })
        .expect("adapter");
    assert!(adapter.markdown.contains("keep adapter exports reviewable"));
    assert!(!adapter.markdown.contains("Poisoned experience"));
}

#[test]
fn export_enforces_boundary_and_redacts_private_surfaces() {
    let denied_svc = service();
    let denied = denied_svc
        .export(ExportQuery {
            profile: Some("work".to_string()),
            workspace: Some("work-ws".to_string()),
            repo_id: None,
            include_archived: Some(false),
            format: Some("jsonl".to_string()),
            target_profile: Some("personal".to_string()),
        })
        .expect_err("work -> personal export must be denied");
    assert_eq!(
        denied.code,
        codex_memoryd::error::ErrorCode::ProfileBoundaryDenied
    );

    let svc = service();
    let store = &svc.store;
    store.ensure_workspace("personal", "ws").expect("workspace");

    insert_record(
        store,
        "personal",
        "ws",
        "I prefer cargo test for validation",
        RecordType::Preference,
        Sensitivity::Personal,
        Portability::Portable,
        0.9,
        Some("turn:export-1"),
        Some("memory/export-1.md"),
    );
    insert_record(
        store,
        "personal",
        "ws",
        "secret export note",
        RecordType::Preference,
        Sensitivity::SecretBlocked,
        Portability::Portable,
        0.9,
        Some("turn:export-2"),
        Some("memory/export-2.md"),
    );
    insert_record(
        store,
        "personal",
        "ws",
        "local-only export note",
        RecordType::Preference,
        Sensitivity::Personal,
        Portability::NeverExport,
        0.9,
        Some("turn:export-3"),
        Some("memory/export-3.md"),
    );

    let result = svc
        .export(ExportQuery {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo_id: None,
            include_archived: Some(false),
            format: Some("jsonl".to_string()),
            target_profile: Some("work".to_string()),
        })
        .expect("export");

    assert_eq!(result.content_type, "application/x-ndjson");
    assert_eq!(result.record_count, 1);
    assert_eq!(result.omitted_secret, 1);
    assert_eq!(result.omitted_boundary, 1);
    assert!(result.body.contains("I prefer cargo test for validation"));
    assert!(!result.body.contains("secret export note"));
    assert!(!result.body.contains("local-only export note"));
}

#[test]
fn adapter_context_pack_aliases_use_issue_template_names() {
    let svc = service();
    let store = &svc.store;
    store.ensure_workspace("personal", "ws").expect("workspace");
    insert_record(
        store,
        "personal",
        "ws",
        "Decision: adapter context packs should use issue-required target names.",
        RecordType::Decision,
        Sensitivity::Personal,
        Portability::Portable,
        0.95,
        Some("turn:adapter-alias"),
        Some("memory/adapter-alias.md"),
    );

    let mcp = svc
        .adapter_export(AdapterExportRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            subject_id: None,
            target: "mcp-json".to_string(),
            max_bytes: None,
        })
        .expect("mcp-json export");
    assert_eq!(mcp.target, "mcp-json");
    let mcp_pack = mcp.context_pack.expect("mcp-json context pack");
    assert_eq!(mcp_pack.target, "mcp-json");
    assert_eq!(mcp_pack.template, "mcp-json-v1");
    assert_eq!(mcp_pack.authority, "recall_not_authority");
    assert_eq!(mcp_pack.source_ids, vec!["turn:adapter-alias"]);

    let wiki = svc
        .adapter_export(AdapterExportRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            subject_id: None,
            target: "markdown-wiki".to_string(),
            max_bytes: None,
        })
        .expect("markdown-wiki export");
    assert_eq!(wiki.target, "markdown-wiki");
    let wiki_pack = wiki.context_pack.expect("markdown-wiki context pack");
    assert_eq!(wiki_pack.target, "markdown-wiki");
    assert_eq!(wiki_pack.template, "markdown-wiki-v1");
    assert_eq!(wiki_pack.authority, "recall_not_authority");
    assert!(wiki.markdown.contains("# Markdown Wiki Memory View"));
}

#[test]
fn patch_preview_apply_stays_reviewable() {
    let (svc, _tmpdir, db_path) = temp_service();
    let _original_id = seed_patch_turn(&svc, &db_path);
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
        .expect("patch explain item");
    assert_eq!(item.patch_run_id, Some(preview.run_id.clone()));
    assert_eq!(item.policy_outcome, "accepted");
    assert!(!item.source_refs.is_empty());
}

#[test]
fn dream_preview_uses_generated_view_fixture() {
    let svc = service();
    svc.store
        .ensure_session("sess_fixture", "personal", "test", None, None, "test")
        .unwrap();
    svc.store
        .insert_visible_turn(&VisibleTurn {
            id: "turn_user_pref".to_string(),
            session_id: "sess_fixture".to_string(),
            actor: "user".to_string(),
            content: "Prefer cargo test for validation".to_string(),
            created_at: "2026-01-01T00:01:00Z".to_string(),
            metadata: json!({}),
        })
        .unwrap();

    let report = svc
        .dream(DreamRequest {
            profile: Some("personal".to_string()),
            workspace: Some("test".to_string()),
            repo: None,
            mode: Some("preview".to_string()),
            now: Some("2026-01-02T00:00:00Z".to_string()),
            since: None,
        })
        .expect("dream preview runs");
    assert_eq!(report.authority, "recall_not_authority");
    let live = serde_json::to_value(report).expect("serialize report");
    let fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/dreaming/preview_user_preference.report.json"),
        )
        .expect("read fixture"),
    )
    .expect("fixture JSON");
    assert_eq!(live, fixture);
}
