//! CLI smoke tests: invoke the compiled `codex-memoryd` binary against a temp
//! database and assert real behavior (record creation, secret rejection,
//! idempotent local import, forget, doctor).

use std::path::PathBuf;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("codex-memoryd").expect("binary built")
}

fn db_path(dir: &TempDir) -> PathBuf {
    dir.path().join("memory.db")
}

fn count_table(db: &PathBuf, table: &str) -> i64 {
    let conn = Connection::open(db).unwrap();
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn count_archived(db: &PathBuf) -> i64 {
    let conn = Connection::open(db).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM memory_records WHERE archived = 1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn cli_doctor_reports_ok() {
    let dir = TempDir::new().unwrap();
    bin()
        .arg("--db")
        .arg(db_path(&dir))
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"storage_writable\": true"));
}

#[test]
fn cli_status_is_json() {
    let dir = TempDir::new().unwrap();
    bin()
        .arg("--db")
        .arg(db_path(&dir))
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"provider_name\": \"codex-memoryd\"",
        ))
        .stdout(predicate::str::contains("\"api_version\": \"v1\""));
}

#[test]
fn cli_status_storage_path_failure_is_actionable() {
    let dir = TempDir::new().unwrap();
    let not_a_dir = dir.path().join("not-a-dir");
    std::fs::write(&not_a_dir, "file blocks directory creation").unwrap();

    bin()
        .arg("--db")
        .arg(not_a_dir.join("memory.db"))
        .arg("status")
        .assert()
        .failure()
        .stderr(predicate::str::contains("check --db/CODEX_MEMORYD_DB"));
}

#[test]
fn cli_serve_bind_conflict_is_actionable() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    bin()
        .arg("--db")
        .arg(&db)
        .args(["serve", "--bind", &addr])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to listen"))
        .stderr(predicate::str::contains("--bind 127.0.0.1:<port>"));
}

#[test]
fn cli_conclude_then_search_roundtrip() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "conclude",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--content",
            "Decision: prefer rusqlite bundled for storage",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("record_ids"));

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "search",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--query",
            "rusqlite",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("rusqlite"));
}

#[test]
fn cli_subject_episode_roundtrip() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let subject_output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "subject",
            "create",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--key",
            "workflow:dogfood-import",
            "--kind",
            "workflow",
            "--display-name",
            "Dogfood import",
        ])
        .output()
        .unwrap();
    assert!(subject_output.status.success());
    let subject: Value = serde_json::from_slice(&subject_output.stdout).unwrap();
    assert_eq!(subject["created"], true);
    let subject_id = subject["subject"]["id"].as_str().unwrap().to_string();

    let duplicate_output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "subject",
            "create",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--key",
            "workflow:dogfood-import",
            "--kind",
            "workflow",
            "--display-name",
            "Ignored duplicate",
        ])
        .output()
        .unwrap();
    assert!(duplicate_output.status.success());
    let duplicate: Value = serde_json::from_slice(&duplicate_output.stdout).unwrap();
    assert_eq!(duplicate["created"], false);
    assert_eq!(duplicate["subject"]["id"], subject["subject"]["id"]);

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "subject",
            "list",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--kind",
            "workflow",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("workflow:dogfood-import"));

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "subject",
            "get",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            &subject_id,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dogfood import"));

    let episode_output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "episode",
            "create",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--subject-id",
            &subject_id,
            "--source-kind",
            "fizzy_card",
            "--source-ref",
            "491",
            "--summary",
            "Container/import/MCP gate verified",
            "--status",
            "completed",
        ])
        .output()
        .unwrap();
    assert!(episode_output.status.success());
    let episode: Value = serde_json::from_slice(&episode_output.stdout).unwrap();
    assert_eq!(episode["created"], true);
    assert_eq!(episode["episode"]["subject_id"], subject_id);
    let episode_id = episode["episode"]["id"].as_str().unwrap().to_string();

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "episode",
            "list",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--subject-id",
            &subject_id,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Container/import/MCP gate verified",
        ));

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "episode",
            "get",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            &episode_id,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("fizzy_card"));
}

#[test]
fn cli_patch_preview_renders_markdown() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "conclude",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--content",
            "Decision: use rusqlite with bundled SQLite",
        ])
        .assert()
        .success();

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "patch",
            "preview",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--format",
            "markdown",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Memory patch preview"))
        .stdout(predicate::str::contains("+ decision"));
}

#[test]
fn cli_conclude_rejects_secret() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "conclude",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--content",
            "aws_secret_access_key=wJalrXUtnFEMIabcdefghijkl1234567890",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("secret_detected"));
}

#[test]
fn cli_sync_local_preview_then_apply_idempotent() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    // Build a fake ~/.codex/memories layout.
    let mem_root = dir.path().join("codex_memories");
    std::fs::create_dir_all(mem_root.join("rollout_summaries")).unwrap();
    std::fs::write(
        mem_root.join("memory_summary.md"),
        "# Preferences\n- prefer repo-native workflows\n- use cargo test\n",
    )
    .unwrap();
    std::fs::write(
        mem_root.join("rollout_summaries/2026-06-05.md"),
        "# Checkpoint\n- implemented sync endpoint\n",
    )
    .unwrap();

    // Preview writes nothing.
    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "sync-local",
            "--preview",
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&mem_root)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"mode\": \"preview\""))
        .stdout(predicate::str::contains("\"created\": 0"));

    // Apply writes records.
    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "sync-local",
            "--apply",
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&mem_root)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"mode\": \"apply\""));

    // Re-apply creates nothing new (idempotent).
    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "sync-local",
            "--apply",
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&mem_root)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"created\": 0"));
}

#[test]
fn cli_sync_local_skips_external_symlinked_files_and_dirs() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let source_root = dir.path().join("source_root");
    let in_root = source_root.join("keep");
    let symlinked_file_target = dir.path().join("external").join("outside.md");
    let symlinked_dir_target = dir.path().join("external_dir");

    std::fs::create_dir_all(&in_root).unwrap();
    std::fs::create_dir_all(symlinked_file_target.parent().unwrap()).unwrap();
    std::fs::create_dir_all(symlinked_dir_target.join("notes")).unwrap();
    std::fs::write(
        in_root.join("memory.md"),
        "# Root\n- this file should be synced\n",
    )
    .unwrap();
    std::fs::write(
        &symlinked_file_target,
        "# Secret\n- external secret: aws_secret_access_key=EXTERNAL_TEST_SECRET_TOKEN\n",
    )
    .unwrap();
    std::fs::write(
        symlinked_dir_target.join("notes").join("nested.md"),
        "# External nested\n- do-not-sync\n",
    )
    .unwrap();

    let symlinked_file = source_root.join("outside.md");
    let symlinked_dir = source_root.join("outside-dir");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&symlinked_file_target, &symlinked_file).unwrap();
        std::os::unix::fs::symlink(&symlinked_dir_target, &symlinked_dir).unwrap();
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(&symlinked_file_target, &symlinked_file).unwrap();
        std::os::windows::fs::symlink_dir(&symlinked_dir_target, &symlinked_dir).unwrap();
    }

    let preview = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "sync-local",
            "--preview",
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&source_root)
        .output()
        .unwrap();
    assert!(preview.status.success());
    let preview_stdout = String::from_utf8_lossy(&preview.stdout);
    assert!(preview_stdout.contains("\"mode\": \"preview\""));
    assert!(preview_stdout.contains("\"files_scanned\": 1"));
    assert!(preview_stdout.contains("\"created\": 0"));
    assert!(!preview_stdout.contains("outside.md"));
    assert!(!preview_stdout.contains("EXTERNAL_TEST_SECRET_TOKEN"));

    let apply = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "sync-local",
            "--apply",
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&source_root)
        .output()
        .unwrap();
    assert!(apply.status.success());
    let apply_stdout = String::from_utf8_lossy(&apply.stdout);
    assert!(apply_stdout.contains("\"mode\": \"apply\""));
    assert!(apply_stdout.contains("\"files_scanned\": 1"));
    assert!(apply_stdout.contains("\"created\": 1"));
    assert!(!apply_stdout.contains("outside.md"));
    assert!(!apply_stdout.contains("EXTERNAL_TEST_SECRET_TOKEN"));

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "search",
            "--profile",
            "personal",
            "--workspace",
            "ws",
            "--query",
            "this file should be synced",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("this file should be synced"));
}

#[test]
fn cli_card_workspace_summary_json_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "conclude",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--content",
            "Decision: pin cargo to v1.0.0",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("record_ids"));

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "conclude",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--content",
            "Decision: enable deterministic cards for issue #50",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("record_ids"));

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "workspace_summary",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(first.status.success());
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();

    let second = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "workspace_summary",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();

    assert_eq!(first["card_type"], "workspace_summary");
    assert_eq!(first["scope"], "workspace");
    assert_eq!(first["profile"], "personal");
    assert_eq!(first["workspace"], "josh-personal");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["build_spec_version"], "card-summary-v1");
    assert!(first["content_hash"].as_str().is_some());
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
}

#[test]
fn cli_card_workspace_summary_markdown_renders() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "conclude",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--content",
            "Decision: keep markdown output for card views",
        ])
        .assert()
        .success();

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "workspace_summary",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--format",
            "markdown",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("# Card summary: workspace_summary"));
    assert!(stdout.contains("Profile: personal"));
    assert!(stdout.contains("Authority: recall_not_authority"));
    assert!(stdout.contains("Content hash: "));
}

#[test]
fn cli_card_show_rejects_invalid_format() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "workspace_summary",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--format",
            "html",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid --format 'html'"));
}

#[test]
fn cli_card_subject_summary_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let subject_output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "subject",
            "create",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--key",
            "issue50-subject-summary",
            "--kind",
            "workflow",
            "--display-name",
            "Issue #50 card smoke",
        ])
        .output()
        .unwrap();
    assert!(subject_output.status.success());
    let subject: Value = serde_json::from_slice(&subject_output.stdout).unwrap();
    let subject_id = subject["subject"]["id"].as_str().unwrap().to_string();

    // Optional record-binding path if supported: episode records can be attached
    // to a subject and may appear in subject snapshots when present.
    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "episode",
            "create",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--subject-id",
            &subject_id,
            "--source-kind",
            "fizzy_card",
            "--source-ref",
            "50",
            "--summary",
            "Issue #50 subject card path",
            "--status",
            "completed",
        ])
        .assert()
        .success();

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "subject_summary",
            "--subject-id",
            &subject_id,
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(first.status.success());
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();

    let second = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "subject_summary",
            "--subject-id",
            &subject_id,
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();

    assert_eq!(first["card_type"], "subject_summary");
    assert_eq!(first["scope"], "subject");
    assert_eq!(first["subject_id"], subject_id);
    assert_eq!(first["authority"], "recall_not_authority");
    assert!(first["content_hash"].as_str().is_some());
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);

    let markdown = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "subject_summary",
            "--subject-id",
            &subject_id,
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--format",
            "markdown",
        ])
        .output()
        .unwrap();
    assert!(markdown.status.success());
    let markdown = String::from_utf8_lossy(&markdown.stdout);
    assert!(markdown.contains(&format!("Subject: {subject_id}")));
    assert!(markdown.contains("# Card summary: subject_summary"));
}

#[test]
fn cli_dream_preview_empty_workspace_is_json() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "dream",
            "--preview",
            "--profile",
            "personal",
            "--workspace",
            "test",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"mode\": \"preview\""))
        .stdout(predicate::str::contains("\"candidates\": []"))
        .stdout(predicate::str::contains(
            "\"authority\": \"recall_not_authority\"",
        ));
}

#[test]
fn cli_dream_preview_seeded_evidence_is_stable_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "conclude",
            "--profile",
            "personal",
            "--workspace",
            "test",
            "--content",
            "Decision: use cargo test for validation",
        ])
        .assert()
        .success();

    let before = [
        count_table(&db, "memory_records"),
        count_table(&db, "conclusions"),
        count_table(&db, "checkpoints"),
        count_table(&db, "visible_turns"),
        count_archived(&db),
    ];

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "dream",
            "--preview",
            "--profile",
            "personal",
            "--workspace",
            "test",
        ])
        .output()
        .unwrap();
    assert!(first.status.success());
    let second = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "dream",
            "--preview",
            "--profile",
            "personal",
            "--workspace",
            "test",
        ])
        .output()
        .unwrap();
    assert!(second.status.success());
    assert_eq!(first.stdout, second.stdout, "preview JSON must be stable");

    let after = [
        count_table(&db, "memory_records"),
        count_table(&db, "conclusions"),
        count_table(&db, "checkpoints"),
        count_table(&db, "visible_turns"),
        count_archived(&db),
    ];
    assert_eq!(after, before, "dream preview must not write durable rows");
}

#[test]
fn cli_help_lists_all_commands() {
    bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("recall"))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("dream"))
        .stdout(predicate::str::contains("sync-local"))
        .stdout(predicate::str::contains("export"))
        .stdout(predicate::str::contains("forget"))
        .stdout(predicate::str::contains("doctor"));
}

#[test]
fn readme_keeps_first_run_path_documented() {
    let readme = include_str!("../README.md");
    for required in [
        "First-run path (source build)",
        "CODEX_MEMORYD_DB",
        "codex-memoryd doctor",
        "curl -fsS http://127.0.0.1:8787/v1/status",
        "codex-memoryd sync-local --preview",
        "codex-memoryd sync-local --apply",
        "codex-memoryd conclude --profile personal",
        "codex-memoryd recall --profile personal",
        "Fail-open note",
    ] {
        assert!(readme.contains(required), "README missing {required:?}");
    }
}
