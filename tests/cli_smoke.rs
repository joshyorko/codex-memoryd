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
