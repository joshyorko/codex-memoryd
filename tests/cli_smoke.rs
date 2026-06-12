//! CLI smoke tests: invoke the compiled `codex-memoryd` binary against a temp
//! database and assert real behavior (record creation, secret rejection,
//! idempotent local import, forget, doctor).

use std::path::PathBuf;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use rusqlite::Connection;
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
