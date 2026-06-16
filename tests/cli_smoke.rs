//! CLI smoke tests: invoke the compiled `codex-memoryd` binary against a temp
//! database and assert real behavior (record creation, secret rejection,
//! idempotent local import, forget, doctor).

use std::path::PathBuf;
use std::process::Command;

use assert_cmd::prelude::*;
use codex_memoryd::domain::Portability;
use codex_memoryd::domain::RecordType;
use codex_memoryd::domain::Scope;
use codex_memoryd::domain::Sensitivity;
use codex_memoryd::ids;
use codex_memoryd::store::NewRecord;
use codex_memoryd::store::Store;
use codex_memoryd::store::UpsertOutcome;
use predicates::prelude::*;
use rusqlite::params;
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

fn bin() -> Command {
    Command::cargo_bin("codex-memoryd").expect("binary built")
}

fn unused_loopback_addr() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

fn wait_for_health(url: &str) {
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap();
    for _ in 0..40 {
        if http
            .get(format!("{url}/healthz"))
            .send()
            .is_ok_and(|resp| resp.status().is_success())
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("timed out waiting for {url}/healthz");
}

fn db_path(dir: &TempDir) -> PathBuf {
    dir.path().join("memory.db")
}

fn normalize_card_markdown_snapshot(markdown: &str) -> String {
    markdown
        .trim_end()
        .lines()
        .map(|line| {
            if line.starts_with("Content hash: ") {
                "Content hash: <sha256>".to_string()
            } else if line.starts_with("Generated at: ") {
                "Generated at: <fresh-timestamp>".to_string()
            } else if line.starts_with("- mem_") {
                "- <record_id> [decision] workspace (0.9)".to_string()
            } else if line.starts_with("  - updated_at: ") && !line.contains("2025-01-01") {
                "  - updated_at: <fresh-timestamp>".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc().format(&Rfc3339).unwrap()
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

fn seed_preference_record(
    db: &PathBuf,
    profile: &str,
    workspace: &str,
    content: &str,
    updated_at: &str,
    archived: bool,
) -> String {
    let store = Store::open(db).unwrap();
    let record = NewRecord {
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        repo_id: None,
        subject_id: None,
        episode_id: None,
        scope: Scope::Workspace,
        record_type: RecordType::Preference,
        content: content.to_string(),
        related_files: vec![],
        tags: vec!["preference".to_string()],
        sensitivity: Sensitivity::Personal,
        portability: Portability::ProfileOnly,
        confidence: 0.9,
        source_ids: vec!["src_test".to_string()],
        content_hash: ids::content_hash(
            profile,
            workspace,
            None,
            RecordType::Preference.as_str(),
            Scope::Workspace.as_str(),
            content,
        ),
        supersedes: vec![],
        metadata: serde_json::json!({"origin": "cli_smoke"}),
    };
    let id = match store.upsert_record(&record).unwrap() {
        UpsertOutcome::Created(id) | UpsertOutcome::Skipped(id) => id,
    };
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "UPDATE memory_records SET created_at = ?1, updated_at = ?1, archived = ?3 WHERE id = ?2",
        params![updated_at, id, archived as i64],
    )
    .unwrap();
    id
}

#[allow(clippy::too_many_arguments)]
fn seed_record(
    db: &PathBuf,
    profile: &str,
    workspace: &str,
    record_type: RecordType,
    content: &str,
    updated_at: &str,
    archived: bool,
    source_ids: Vec<String>,
) -> String {
    let store = Store::open(db).unwrap();
    let record = NewRecord {
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        repo_id: None,
        subject_id: None,
        episode_id: None,
        scope: Scope::Workspace,
        record_type,
        content: content.to_string(),
        related_files: vec![],
        tags: vec![record_type.as_str().to_string()],
        sensitivity: Sensitivity::Personal,
        portability: Portability::ProfileOnly,
        confidence: 0.9,
        source_ids,
        content_hash: ids::content_hash(
            profile,
            workspace,
            None,
            record_type.as_str(),
            Scope::Workspace.as_str(),
            content,
        ),
        supersedes: vec![],
        metadata: serde_json::json!({"origin": "cli_smoke"}),
    };
    let id = match store.upsert_record(&record).unwrap() {
        UpsertOutcome::Created(id) | UpsertOutcome::Skipped(id) => id,
    };
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "UPDATE memory_records SET created_at = ?1, updated_at = ?1, archived = ?3 WHERE id = ?2",
        params![updated_at, id, archived as i64],
    )
    .unwrap();
    id
}

#[allow(clippy::too_many_arguments)]
fn seed_record_with_details(
    db: &PathBuf,
    profile: &str,
    workspace: &str,
    record_type: RecordType,
    content: &str,
    updated_at: &str,
    archived: bool,
    source_ids: Vec<String>,
    tags: Vec<String>,
    metadata: Value,
) -> String {
    let store = Store::open(db).unwrap();
    let record = NewRecord {
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        repo_id: None,
        subject_id: None,
        episode_id: None,
        scope: Scope::Workspace,
        record_type,
        content: content.to_string(),
        related_files: vec![],
        tags,
        sensitivity: Sensitivity::Personal,
        portability: Portability::ProfileOnly,
        confidence: 0.9,
        source_ids,
        content_hash: ids::content_hash(
            profile,
            workspace,
            None,
            record_type.as_str(),
            Scope::Workspace.as_str(),
            content,
        ),
        supersedes: vec![],
        metadata,
    };
    let id = match store.upsert_record(&record).unwrap() {
        UpsertOutcome::Created(id) | UpsertOutcome::Skipped(id) => id,
    };
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "UPDATE memory_records SET created_at = ?1, updated_at = ?1, archived = ?3 WHERE id = ?2",
        params![updated_at, id, archived as i64],
    )
    .unwrap();
    id
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("run git {args:?}: {e}"));
    assert!(status.success(), "git {args:?} failed");
}

fn init_fixture_repo(dir: &TempDir, commit_args: &[&str]) -> PathBuf {
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    std::fs::write(repo.join("README.md"), "fixture repo\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, commit_args);
    repo
}

fn write_refs_fixture_jsonl(dir: &TempDir, name: &str, content: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn cli_doctor_reports_ok() {
    let dir = TempDir::new().unwrap();
    // Default (summary) output names the substrate and reports writable storage.
    bin()
        .arg("--db")
        .arg(db_path(&dir))
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("codex-memoryd doctor"))
        .stdout(predicate::str::contains("writable=true"));
}

#[test]
fn cli_doctor_json_has_all_sections() {
    let dir = TempDir::new().unwrap();
    let assert = bin()
        .arg("--db")
        .arg(db_path(&dir))
        .arg("doctor")
        .arg("--format")
        .arg("json")
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    let v: Value = serde_json::from_str(&out).expect("doctor json");
    for section in [
        "storage",
        "schema",
        "backup",
        "policy_corpus",
        "mcp",
        "quarantine",
        "procedures",
        "adapters",
    ] {
        assert!(v.get(section).is_some(), "doctor json missing '{section}'");
    }
    assert_eq!(v["storage"]["writable"], serde_json::json!(true));
    assert_eq!(v["mcp"]["read_only_tools"].as_array().unwrap().len(), 3);
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
fn cli_eval_substrate_emits_deterministic_json_and_summary() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let first = bin()
        .arg("--db")
        .arg(&db)
        .arg("eval")
        .arg("substrate")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let second = bin()
        .arg("--db")
        .arg(&db)
        .arg("eval")
        .arg("substrate")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(first, second, "eval JSON output should be deterministic");
    let report: Value = serde_json::from_slice(&first).expect("eval report JSON");
    assert_eq!(report["suite"], "substrate");
    assert_eq!(report["status"], "pass");
    assert_eq!(report["metrics"]["cross_profile_bleed_rate"], 0.0);
    assert_eq!(report["metrics"]["poison_acceptance_rate"], 0.0);
    assert_eq!(report["checks"]["patch_rollback"]["status"], "pass");
    assert_eq!(report["checks"]["procedure_memory"]["status"], "pass");
    assert_eq!(report["checks"]["adapter_context_pack"]["status"], "pass");
    assert!(report["metrics"]["pack_cost"]["bytes"].as_u64().unwrap() > 0);

    bin()
        .arg("--db")
        .arg(db)
        .arg("eval")
        .arg("substrate")
        .arg("--format")
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "codex-memoryd substrate eval: pass",
        ))
        .stdout(predicate::str::contains("cross-profile bleed: 0/"))
        .stdout(predicate::str::contains("poison acceptance: 0/"))
        .stdout(predicate::str::contains("patch rollback: pass"))
        .stdout(predicate::str::contains("procedure memory: pass"))
        .stdout(predicate::str::contains("adapter/context pack: pass"));
}

#[test]
fn cli_eval_retrieval_emits_long_history_scores_and_summary() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let first = bin()
        .arg("--db")
        .arg(&db)
        .arg("eval")
        .arg("retrieval")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let second = bin()
        .arg("--db")
        .arg(&db)
        .arg("eval")
        .arg("retrieval")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(
        first, second,
        "retrieval eval JSON output should be deterministic"
    );
    let report: Value = serde_json::from_slice(&first).expect("retrieval eval report JSON");
    assert_eq!(report["suite"], "retrieval_quality");
    assert_eq!(report["status"], "pass");
    assert_eq!(report["question_count"], 6);
    assert_eq!(
        report["fixture_families"],
        serde_json::json!([
            "single_hop",
            "temporal",
            "contradiction",
            "preference_drift",
            "multi_hop",
            "open_domain"
        ])
    );
    assert!(report["baselines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["name"] == "memoryd_recall"));
    let memoryd = report["baselines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "memoryd_recall")
        .expect("memoryd baseline");
    assert!(memoryd["failed_queries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|q| q == "q_multihop_evidence"));
    let relation_aware = report["baselines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "relation_aware_recall")
        .expect("relation-aware baseline");
    assert!(!relation_aware["failed_queries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|q| q == "q_multihop_evidence"));
    assert_eq!(relation_aware["cross_profile_leak"], false);
    assert!(
        relation_aware["recall_at_k"].as_f64().unwrap() > memoryd["recall_at_k"].as_f64().unwrap()
    );
    assert!(report["retrieval_improvements"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["query_id"] == "q_multihop_evidence"
            && item["before"] == "memoryd_recall"
            && item["after"] == "relation_aware_recall"));
    assert!(report["baselines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["name"] == "verbatim_evidence"));
    assert!(report["ranking_ablations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["name"] == "all_signals"));
    assert!(report["regression_fixtures"].as_array().unwrap().len() >= 1);
    assert!(report["next_recommended_ranking_changes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r.as_str().unwrap().contains("subject")));

    bin()
        .arg("--db")
        .arg(db)
        .arg("eval")
        .arg("retrieval")
        .arg("--format")
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "codex-memoryd retrieval quality eval: pass",
        ))
        .stdout(predicate::str::contains("long-history questions: 6"))
        .stdout(predicate::str::contains("memoryd_recall"))
        .stdout(predicate::str::contains("relation_aware_recall"))
        .stdout(predicate::str::contains("verbatim_evidence"))
        .stdout(predicate::str::contains("retrieval improvements"))
        .stdout(predicate::str::contains("next ranking changes"));
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
fn cli_recall_accepts_pack_mode_and_rejects_unknown_pack() {
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
            "Gotcha: sqlite debugging failed until rollback path was checked",
        ])
        .assert()
        .success();

    let default_output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "recall",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--query",
            "sqlite",
            "--pack-mode",
            "default",
        ])
        .output()
        .unwrap();
    assert!(default_output.status.success());
    let default_recall: Value = serde_json::from_slice(&default_output.stdout).unwrap();
    assert_eq!(default_recall["pack"]["mode"], "default");
    assert_eq!(default_recall["pack"]["template"], "default");
    assert_eq!(default_recall["pack"]["template_budget_tokens"], 1200);
    assert_eq!(default_recall["pack"]["max_tokens"], 1200);
    assert!(default_recall["policy"]["ranking_signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|signal| signal == "pack_template:default"));
    assert!(default_recall["policy"]["ranking_signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|signal| signal == "pack_budget:1200"));

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "recall",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--query",
            "sqlite",
            "--pack-mode",
            "debugging",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let recall: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(recall["pack"]["mode"], "debugging");
    assert_eq!(recall["pack"]["template"], "debugging");
    assert_eq!(recall["pack"]["template_budget_tokens"], 1000);
    assert_eq!(recall["pack"]["max_tokens"], 1000);
    assert!(recall["policy"]["ranking_signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|signal| signal == "pack_mode:debugging"));
    assert!(recall["policy"]["ranking_signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|signal| signal == "pack_template:debugging"));
    assert!(recall["policy"]["ranking_signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|signal| signal == "pack_budget:1000"));

    for (mode, normalized, budget) in [
        ("active-task", "active_task", 900),
        ("review", "review", 1_100),
        ("personal-context", "personal_context", 900),
    ] {
        let output = bin()
            .arg("--db")
            .arg(&db)
            .args([
                "recall",
                "--profile",
                "personal",
                "--workspace",
                "josh-personal",
                "--query",
                "sqlite",
                "--pack-mode",
                mode,
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let recall: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(recall["pack"]["mode"], normalized);
        assert_eq!(recall["pack"]["template"], normalized);
        assert_eq!(recall["pack"]["template_budget_tokens"], budget);
        assert!(recall["policy"]["ranking_signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|signal| signal == &format!("pack_mode:{normalized}")));
    }

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "recall",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--query",
            "sqlite",
            "--pack-mode",
            "everything",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown pack_mode"));
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
fn cli_git_import_preview_apply_and_second_apply_are_idempotent() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let repo = init_fixture_repo(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            "Document memory trailers",
            "-m",
            "Memory-Decision: import commit trailers as evidence episodes",
            "-m",
            "Memory-Rejected: reject unsupported draft paths",
            "-m",
            "Memory-Procedure: run cargo test --test cli_smoke cli_git_import",
            "-m",
            "Memory-Scar: avoid stale fixture assumptions",
            "-m",
            "Memory-Verify: cargo test --test cli_smoke cli_git_import",
        ],
    );

    let preview = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "git-import",
            "--preview",
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(preview.status.success());
    let preview_json: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview_json["mode"], "preview");
    assert_eq!(preview_json["proposed"], 5);
    assert!(preview_json["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|episode| episode["summary"]
            .as_str()
            .unwrap()
            .starts_with("Procedure: ")));
    assert!(preview_json["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|episode| episode["summary"].as_str().unwrap().starts_with("Scar: ")));
    assert!(preview_json["episodes"][0]["source_ref"]
        .as_str()
        .unwrap()
        .starts_with("git:"));
    assert_eq!(preview_json["created"], 0);
    assert_eq!(count_table(&db, "episodes"), 0);
    assert_eq!(count_table(&db, "evidence_ledger"), 0);

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "git-import",
            "--apply",
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(first.status.success());
    let first_json: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_json["mode"], "apply");
    assert_eq!(first_json["proposed"], 5);
    assert_eq!(first_json["created"], 5);
    assert_eq!(count_table(&db, "subjects"), 1);
    assert_eq!(count_table(&db, "episodes"), 5);
    assert_eq!(count_table(&db, "evidence_ledger"), 5);
    assert_eq!(count_table(&db, "memory_records"), 0);

    let second = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "git-import",
            "--apply",
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(second.status.success());
    let second_json: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_json["created"], 0);
    assert_eq!(second_json["skipped"], 5);
    assert_eq!(count_table(&db, "episodes"), 5);
    assert_eq!(count_table(&db, "evidence_ledger"), 5);
}

#[test]
fn cli_git_import_rejects_secret_trailers_without_leaking_content() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let repo = init_fixture_repo(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            "Secret trailer",
            "-m",
            "Memory-Procedure: token=ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        ],
    );

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "git-import",
            "--apply",
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["proposed"], 0);
    assert_eq!(parsed["rejected"], 1);
    assert!(!stdout.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert_eq!(count_table(&db, "episodes"), 0);
    assert_eq!(count_table(&db, "evidence_ledger"), 1);

    let conn = Connection::open(&db).unwrap();
    let (policy_state, safe_summary): (String, String) = conn
        .query_row(
            "SELECT policy_state, safe_summary FROM evidence_ledger",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(policy_state, "secret_detected");
    assert!(!safe_summary.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
}

#[test]
fn cli_git_import_refs_fixture_preview_apply_and_second_apply_are_idempotent() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let repo = init_fixture_repo(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            "Fixture repo",
            "-m",
            "Memory-Decision: keep refs fixtures file-based",
        ],
    );
    let fixture = write_refs_fixture_jsonl(
        &dir,
        "refs-fixture.jsonl",
        r#"{"kind":"commit","repo":"joshyorko/codex","id":"abc123def","authored_at":"2026-06-12T09:00:00Z","author":"josh","body":"Memory-Procedure: import commit refs fixtures"}
{"kind":"pr","repo":"joshyorko/codex","number":57,"authored_at":"2026-06-12T10:00:00Z","author":"josh","body":"Memory-Decision: keep refs imports file-based\nMemory-Verify: cargo test --test cli_smoke cli_git_import_refs_fixture"}
{"kind":"issue","repo":"joshyorko/codex","id":"404","author":"josh","body":"Memory-Gotcha: issue refs stay evidence-only"}
{"kind":"review_comment","repo":"joshyorko/codex","url":"https://github.com/joshyorko/codex/pull/57#discussion_r2","author":"josh","text":"Memory-Verify: review comments import through refs fixtures"}"#,
    );

    let preview = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "git-import",
            "--preview",
            "--refs-fixture",
            fixture.to_str().unwrap(),
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(preview.status.success());
    let preview_json: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview_json["mode"], "preview");
    assert_eq!(preview_json["proposed"], 5);
    assert!(preview_json["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|episode| episode["source_ref"]
            .as_str()
            .unwrap()
            .contains("review_comment")));
    assert!(preview_json["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|episode| episode["source"]["kind"] == "commit"));
    assert!(preview_json["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|episode| episode.get("commit").is_none()));
    assert!(preview_json["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|episode| episode["source"]["origin"] == "git-import-refs-fixture"));
    assert_eq!(preview_json["created"], 0);
    assert_eq!(count_table(&db, "subjects"), 0);
    assert_eq!(count_table(&db, "episodes"), 0);
    assert_eq!(count_table(&db, "evidence_ledger"), 0);
    assert_eq!(count_table(&db, "memory_records"), 0);

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "git-import",
            "--apply",
            "--refs-fixture",
            fixture.to_str().unwrap(),
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(first.status.success());
    let first_json: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_json["mode"], "apply");
    assert_eq!(first_json["proposed"], 5);
    assert_eq!(first_json["created"], 5);
    assert_eq!(count_table(&db, "subjects"), 1);
    assert_eq!(count_table(&db, "episodes"), 5);
    assert_eq!(count_table(&db, "evidence_ledger"), 5);
    assert_eq!(count_table(&db, "memory_records"), 0);

    let second = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "git-import",
            "--apply",
            "--refs-fixture",
            fixture.to_str().unwrap(),
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(second.status.success());
    let second_json: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_json["created"], 0);
    assert_eq!(second_json["skipped"], 5);
    assert_eq!(count_table(&db, "episodes"), 5);
    assert_eq!(count_table(&db, "evidence_ledger"), 5);
}

#[test]
fn cli_git_import_refs_fixture_rejects_secret_items_without_leaking_content() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let repo = init_fixture_repo(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            "Fixture repo",
            "-m",
            "Memory-Decision: keep refs imports file-based",
        ],
    );
    let fixture = write_refs_fixture_jsonl(
        &dir,
        "refs-fixture.json",
        r#"[{"kind":"review_comment","repo":"joshyorko/codex","url":"https://github.com/joshyorko/codex/pull/57#discussion_r1","author":"josh","text":"Memory-Gotcha: token=ghp_abcdefghijklmnopqrstuvwxyz0123456789"}]"#,
    );

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "git-import",
            "--apply",
            "--refs-fixture",
            fixture.to_str().unwrap(),
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["proposed"], 0);
    assert_eq!(parsed["rejected"], 1);
    assert!(!stdout.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert_eq!(count_table(&db, "episodes"), 0);
    assert_eq!(count_table(&db, "evidence_ledger"), 1);
    assert_eq!(count_table(&db, "memory_records"), 0);
}

#[test]
fn cli_git_import_refs_fixture_rejects_unknown_kind_without_echoing_it() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let repo = init_fixture_repo(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            "Fixture repo",
            "-m",
            "Memory-Decision: keep refs imports file-based",
        ],
    );
    let fixture = write_refs_fixture_jsonl(
        &dir,
        "refs-fixture.json",
        r#"[{"kind":"token=ghp_abcdefghijklmnopqrstuvwxyz0123456789","repo":"joshyorko/codex","number":57,"body":"Memory-Decision: this should not parse"}]"#,
    );

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "git-import",
            "--preview",
            "--refs-fixture",
            fixture.to_str().unwrap(),
            "--profile",
            "personal",
            "--workspace",
            "ws",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unsupported refs fixture kind"));
    assert!(!stderr.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
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
fn cli_card_workspace_summary_json_marks_stale_records() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let fresh_updated_at = now_rfc3339();

    seed_record(
        &db,
        "personal",
        "josh-personal",
        RecordType::Decision,
        "Decision: stale issue #50 card facts must be labeled.",
        "2025-01-01T00:00:00Z",
        false,
        vec!["src_stale_card".to_string()],
    );
    seed_record(
        &db,
        "personal",
        "josh-personal",
        RecordType::Decision,
        "Decision: fresh issue #50 card facts stay active.",
        &fresh_updated_at,
        false,
        vec!["src_fresh_card".to_string()],
    );

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
            "json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let card: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(card["freshness"], "contains_stale_records");
    let records = card["records"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0]["content"],
        "Decision: fresh issue #50 card facts stay active."
    );
    assert_eq!(records[0]["freshness"]["stale"], false);
    assert!(records[0]["freshness"]["age_days"].as_i64().unwrap() < 120);
    assert_eq!(
        records[1]["content"],
        "Decision: stale issue #50 card facts must be labeled."
    );
    assert_eq!(records[1]["freshness"]["stale"], true);
    assert!(records[1]["freshness"]["age_days"].as_i64().unwrap() > 120);
}

#[test]
fn cli_card_workspace_summary_markdown_matches_fixture() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let fresh_updated_at = now_rfc3339();

    seed_record(
        &db,
        "personal",
        "josh-personal",
        RecordType::Decision,
        "Decision: stale issue #50 card facts must be labeled.",
        "2025-01-01T00:00:00Z",
        false,
        vec!["src_stale_card".to_string()],
    );
    seed_record(
        &db,
        "personal",
        "josh-personal",
        RecordType::Decision,
        "Decision: fresh issue #50 card facts stay active.",
        &fresh_updated_at,
        false,
        vec!["src_fresh_card".to_string()],
    );

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

    let actual = normalize_card_markdown_snapshot(&String::from_utf8_lossy(&output.stdout));
    let expected = include_str!("fixtures/card_workspace_summary.expected.md").trim_end();
    assert_eq!(actual, expected);
}

#[test]
fn cli_card_open_questions_json_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    seed_record(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Question: should we keep the workspace scope explicit?",
        "2026-06-01T09:00:00Z",
        false,
        vec!["src_q1".to_string()],
    );
    seed_record(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Open question: what is the final sync boundary?",
        "2026-06-01T10:00:00Z",
        false,
        vec!["src_q2".to_string()],
    );
    seed_record(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Note: this contains a question mark but is not an explicit question?",
        "2026-06-01T11:00:00Z",
        false,
        vec!["src_ignore".to_string()],
    );
    seed_record(
        &db,
        "work",
        "team-workspace",
        RecordType::TaskCheckpoint,
        "Open question: task checkpoint wording should not opt in",
        "2026-06-01T11:30:00Z",
        false,
        vec!["src_checkpoint".to_string()],
    );
    seed_record(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Question: archived questions stay out of the card",
        "2026-06-01T12:00:00Z",
        true,
        vec!["src_archived".to_string()],
    );

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "open_questions",
            "--profile",
            "work",
            "--workspace",
            "team-workspace",
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
            "open_questions",
            "--profile",
            "work",
            "--workspace",
            "team-workspace",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();

    assert_eq!(first["card_type"], "open_questions");
    assert_eq!(first["scope"], "workspace");
    assert_eq!(first["profile"], "work");
    assert_eq!(first["workspace"], "team-workspace");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["build_spec_version"], "card-summary-v1");
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
    assert_eq!(first["records"].as_array().unwrap().len(), 2);
    assert_eq!(first["records"][0]["type"], "other");
    assert_eq!(
        first["records"][0]["source_ids"],
        serde_json::json!(["src_q2"])
    );
    assert_eq!(first["records"][1]["type"], "other");
    assert_eq!(
        first["records"][1]["source_ids"],
        serde_json::json!(["src_q1"])
    );
    assert!(first["records"].as_array().unwrap().iter().all(|record| {
        record["content"]
            .as_str()
            .map(|content| {
                content.starts_with("Question:") || content.starts_with("Open question:")
            })
            .unwrap_or(false)
    }));
    assert!(!first["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |record| record["source_ids"] == serde_json::json!(["src_ignore"])
                || record["source_ids"] == serde_json::json!(["src_checkpoint"])
        ));
}

#[test]
fn cli_card_open_questions_markdown_renders() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    seed_record(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Question: which workspace boundary should we use?",
        "2026-06-01T09:00:00Z",
        false,
        vec!["src_q1".to_string()],
    );
    seed_record(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Open question: how should we treat hidden writes?",
        "2026-06-01T10:00:00Z",
        false,
        vec!["src_q2".to_string()],
    );

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "open_questions",
            "--profile",
            "work",
            "--workspace",
            "team-workspace",
            "--format",
            "markdown",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("# Card summary: open_questions"));
    assert!(stdout.contains("Authority: recall_not_authority"));
    assert!(stdout.contains("Content hash: "));
    assert!(stdout.contains("Question: which workspace boundary should we use?"));
    assert!(stdout.contains("Open question: how should we treat hidden writes?"));
    assert!(stdout.contains("source_ids: src_q1"));
    assert!(stdout.contains("source_ids: src_q2"));
}

#[test]
fn cli_card_active_preferences_json_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    seed_preference_record(
        &db,
        "personal",
        "josh-personal",
        "Preference: use repo-native commands",
        "2026-06-13T12:00:00Z",
        false,
    );
    seed_preference_record(
        &db,
        "personal",
        "josh-personal",
        "Preference: prefer markdown card output",
        "2026-06-13T13:00:00Z",
        false,
    );
    seed_preference_record(
        &db,
        "personal",
        "josh-personal",
        "Preference: archived preference should not show",
        "2026-06-13T14:00:00Z",
        true,
    );
    seed_preference_record(
        &db,
        "personal",
        "other-workspace",
        "Preference: other workspace should not show",
        "2026-06-13T15:00:00Z",
        false,
    );

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "active_preferences",
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
            "active_preferences",
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

    assert_eq!(first["card_type"], "active_preferences");
    assert_eq!(first["scope"], "workspace");
    assert_eq!(first["profile"], "personal");
    assert_eq!(first["workspace"], "josh-personal");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["records"].as_array().unwrap().len(), 2);
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
    let contents = first["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["content"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        contents,
        vec![
            "Preference: prefer markdown card output",
            "Preference: use repo-native commands",
        ]
    );
    assert!(!first
        .to_string()
        .contains("Preference: archived preference should not show"));
    assert!(!first
        .to_string()
        .contains("Preference: other workspace should not show"));
}

#[test]
fn cli_card_active_preferences_markdown_renders() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    seed_preference_record(
        &db,
        "personal",
        "josh-personal",
        "Preference: render active preferences as markdown",
        "2026-06-13T12:00:00Z",
        false,
    );
    seed_preference_record(
        &db,
        "personal",
        "josh-personal",
        "Preference: archived markdown preference should not show",
        "2026-06-13T13:00:00Z",
        true,
    );

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "active_preferences",
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
    assert!(stdout.contains("# Card summary: active_preferences"));
    assert!(stdout.contains("Profile: personal"));
    assert!(stdout.contains("Workspace: josh-personal"));
    assert!(stdout.contains("Preference: render active preferences as markdown"));
    assert!(!stdout.contains("Preference: archived markdown preference should not show"));
}

#[test]
fn cli_card_recent_scars_json_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    seed_record_with_details(
        &db,
        "personal",
        "josh-personal",
        RecordType::Other,
        "Battle scar: the cache writes failed before, but we recovered by switching to the fallback path.",
        "2026-06-13T13:00:00Z",
        false,
        vec!["src_scar_1".to_string()],
        vec!["battle_scar".to_string()],
        serde_json::json!({
            "origin": "dreamer",
            "marker": { "marker_kind": "battle_scar" }
        }),
    );
    seed_record_with_details(
        &db,
        "personal",
        "josh-personal",
        RecordType::Other,
        "Experience marker: retrying the fallback path was the clean path.",
        "2026-06-13T14:00:00Z",
        false,
        vec!["src_experience_marker".to_string()],
        vec!["experience_marker".to_string()],
        serde_json::json!({
            "origin": "dreamer",
            "marker_kind": "experience_marker"
        }),
    );
    seed_record_with_details(
        &db,
        "personal",
        "other-workspace",
        RecordType::Other,
        "Battle scar: other workspace must stay out of the card.",
        "2026-06-13T15:00:00Z",
        false,
        vec!["src_other_workspace".to_string()],
        vec!["battle_scar".to_string()],
        serde_json::json!({
            "origin": "dreamer",
            "marker": { "marker_kind": "battle_scar" }
        }),
    );
    seed_record_with_details(
        &db,
        "personal",
        "josh-personal",
        RecordType::Other,
        "Battle scar: archived scar records should not show.",
        "2026-06-13T16:00:00Z",
        true,
        vec!["src_archived".to_string()],
        vec!["battle_scar".to_string()],
        serde_json::json!({
            "origin": "dreamer",
            "marker": { "marker_kind": "battle_scar" }
        }),
    );
    seed_record_with_details(
        &db,
        "personal",
        "josh-personal",
        RecordType::Other,
        "The cache failed and recovered after a fallback.",
        "2026-06-13T17:00:00Z",
        false,
        vec!["src_failure_text".to_string()],
        vec![],
        serde_json::json!({
            "origin": "cli_smoke"
        }),
    );

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "recent_scars",
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
            "recent_scars",
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

    assert_eq!(first["card_type"], "recent_scars");
    assert_eq!(first["scope"], "workspace");
    assert_eq!(first["profile"], "personal");
    assert_eq!(first["workspace"], "josh-personal");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["build_spec_version"], "card-summary-v1");
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
    assert_eq!(first["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        first["records"][0]["source_ids"],
        serde_json::json!(["src_scar_1"])
    );
    assert!(!first.to_string().contains("src_other_workspace"));
    assert!(!first.to_string().contains("src_archived"));
    assert!(!first.to_string().contains("src_experience_marker"));
    assert!(!first
        .to_string()
        .contains("Experience marker: retrying the fallback path was the clean path."));
    assert!(!first.to_string().contains("src_failure_text"));
    assert!(!first
        .to_string()
        .contains("The cache failed and recovered after a fallback."));
}

#[test]
fn cli_card_recent_scars_markdown_renders() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    seed_record_with_details(
        &db,
        "personal",
        "josh-personal",
        RecordType::Other,
        "Battle scar: keep the fallback path handy.",
        "2026-06-13T12:00:00Z",
        false,
        vec!["src_recent_scar".to_string()],
        vec!["battle_scar".to_string()],
        serde_json::json!({
            "origin": "dreamer",
            "marker": { "marker_kind": "battle_scar" }
        }),
    );

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "recent_scars",
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
    assert!(stdout.contains("# Card summary: recent_scars"));
    assert!(stdout.contains("Authority: recall_not_authority"));
    assert!(stdout.contains("Battle scar: keep the fallback path handy."));
    assert!(stdout.contains("source_ids: src_recent_scar"));
}

#[test]
fn cli_card_procedures_index_json_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::WorkflowPattern,
        "Workflow pattern: keep the workspace index in sync before review.",
        "2026-06-13T11:00:00Z",
        false,
        vec!["src_workflow_pattern".to_string()],
        vec!["workflow_pattern".to_string()],
        serde_json::json!({
            "origin": "cli_smoke",
            "marker_kind": "workflow_pattern"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::Command,
        "Command: run cargo test cli_card_procedures_index_json_is_deterministic",
        "2026-06-13T12:00:00Z",
        false,
        vec!["src_command".to_string()],
        vec!["procedure".to_string()],
        serde_json::json!({
            "origin": "cli_smoke"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::Command,
        "Command: cargo test is useful but not a procedure without a marker.",
        "2026-06-13T12:30:00Z",
        false,
        vec!["src_unmarked_command".to_string()],
        vec![],
        serde_json::json!({
            "origin": "cli_smoke"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Reusable procedure: refresh the card cache before a release.",
        "2026-06-13T13:00:00Z",
        false,
        vec!["src_tagged_procedure".to_string()],
        vec!["procedure".to_string()],
        serde_json::json!({
            "origin": "cli_smoke"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Workflow pattern: restart the daemon after config changes.",
        "2026-06-13T14:00:00Z",
        false,
        vec!["src_metadata_procedure".to_string()],
        vec![],
        serde_json::json!({
            "origin": "cli_smoke",
            "marker_kind": "procedure"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Procedure: text alone should not opt in.",
        "2026-06-13T15:00:00Z",
        false,
        vec!["src_text_only".to_string()],
        vec![],
        serde_json::json!({
            "origin": "cli_smoke"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::Command,
        "Command: archived procedures should not show.",
        "2026-06-13T16:00:00Z",
        true,
        vec!["src_archived".to_string()],
        vec!["procedure".to_string()],
        serde_json::json!({
            "origin": "cli_smoke"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "other-workspace",
        RecordType::WorkflowPattern,
        "Workflow pattern: other workspace should stay out.",
        "2026-06-13T17:00:00Z",
        false,
        vec!["src_other_workspace".to_string()],
        vec!["workflow_pattern".to_string()],
        serde_json::json!({
            "origin": "cli_smoke",
            "marker_kind": "workflow_pattern"
        }),
    );

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "procedures_index",
            "--profile",
            "work",
            "--workspace",
            "team-workspace",
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
            "procedures_index",
            "--profile",
            "work",
            "--workspace",
            "team-workspace",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();

    assert_eq!(first["card_type"], "procedures_index");
    assert_eq!(first["scope"], "workspace");
    assert_eq!(first["profile"], "work");
    assert_eq!(first["workspace"], "team-workspace");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["build_spec_version"], "card-summary-v1");
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
    assert_eq!(first["records"].as_array().unwrap().len(), 4);
    assert_eq!(
        first["records"][0]["source_ids"],
        serde_json::json!(["src_metadata_procedure"])
    );
    assert_eq!(
        first["records"][1]["source_ids"],
        serde_json::json!(["src_tagged_procedure"])
    );
    assert_eq!(
        first["records"][2]["source_ids"],
        serde_json::json!(["src_command"])
    );
    assert_eq!(
        first["records"][3]["source_ids"],
        serde_json::json!(["src_workflow_pattern"])
    );
    assert!(first
        .to_string()
        .contains("Workflow pattern: restart the daemon after config changes."));
    assert!(first
        .to_string()
        .contains("Reusable procedure: refresh the card cache before a release."));
    assert!(first
        .to_string()
        .contains("Command: run cargo test cli_card_procedures_index_json_is_deterministic"));
    assert!(!first
        .to_string()
        .contains("Command: cargo test is useful but not a procedure without a marker."));
    assert!(first
        .to_string()
        .contains("Workflow pattern: keep the workspace index in sync before review."));
    assert!(!first
        .to_string()
        .contains("Procedure: text alone should not opt in."));
    assert!(!first
        .to_string()
        .contains("Command: archived procedures should not show."));
    assert!(!first
        .to_string()
        .contains("Workflow pattern: other workspace should stay out."));
}

#[test]
fn cli_card_procedures_index_markdown_renders() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::Command,
        "Command: restore the procedure index from the latest records.",
        "2026-06-13T12:00:00Z",
        false,
        vec!["src_command".to_string()],
        vec!["procedure".to_string()],
        serde_json::json!({
            "origin": "cli_smoke"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Workflow pattern: this text-only procedure should be ignored.",
        "2026-06-13T13:00:00Z",
        false,
        vec!["src_text_only".to_string()],
        vec![],
        serde_json::json!({
            "origin": "cli_smoke"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::WorkflowPattern,
        "Workflow pattern: index reusable procedures by workspace.",
        "2026-06-13T14:00:00Z",
        false,
        vec!["src_workflow_pattern".to_string()],
        vec!["workflow_pattern".to_string()],
        serde_json::json!({
            "origin": "cli_smoke",
            "marker_kind": "workflow_pattern"
        }),
    );
    seed_record_with_details(
        &db,
        "work",
        "team-workspace",
        RecordType::Other,
        "Reusable procedure: keep this archived procedure out.",
        "2026-06-13T15:00:00Z",
        true,
        vec!["src_archived".to_string()],
        vec!["procedure".to_string()],
        serde_json::json!({
            "origin": "cli_smoke",
            "marker_kind": "procedure"
        }),
    );

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "card",
            "show",
            "--type",
            "procedures_index",
            "--profile",
            "work",
            "--workspace",
            "team-workspace",
            "--format",
            "markdown",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("# Card summary: procedures_index"));
    assert!(stdout.contains("Authority: recall_not_authority"));
    assert!(stdout.contains("Command: restore the procedure index from the latest records."));
    assert!(stdout.contains("Workflow pattern: index reusable procedures by workspace."));
    assert!(!stdout.contains("Workflow pattern: this text-only procedure should be ignored."));
    assert!(!stdout.contains("Reusable procedure: keep this archived procedure out."));
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
fn cli_adapter_agents_md_export_is_deterministic() {
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
            "Decision: agents-md adapter views are generated from memory cards",
        ])
        .assert()
        .success();

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "agents-md",
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
            "adapter",
            "export",
            "--target",
            "agents-md",
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

    assert_eq!(first["target"], "agents-md");
    assert_eq!(first["adapter_version"], "adapter-view-v1");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["source_card_type"], "workspace_summary");
    assert_eq!(first["budget"]["truncated"], false);
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
    let context_pack = &first["context_pack"];
    assert_eq!(context_pack["target"], "agents-md");
    assert_eq!(context_pack["template"], "agents-md-v1");
    assert_eq!(context_pack["adapter_version"], "adapter-view-v1");
    assert_eq!(context_pack["authority"], "recall_not_authority");
    assert_eq!(context_pack["profile"], "personal");
    assert_eq!(context_pack["workspace"], "josh-personal");
    assert_eq!(context_pack["card_type"], "workspace_summary");
    assert_eq!(context_pack["budget"]["truncated"], false);
    assert!(context_pack["records"].as_array().is_some());
    assert!(context_pack["source_ids"].as_array().is_some());
    let markdown = first["markdown"].as_str().unwrap();
    let response_source_ids = first
        .get("source_ids")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let legacy_digest = serde_json::json!({
        "target": first["target"],
        "adapter_version": first["adapter_version"],
        "profile": first["profile"],
        "workspace": first["workspace"],
        "subject_id": Value::Null,
        "source_card_type": first["source_card_type"],
        "source_ids": response_source_ids,
        "markdown": markdown,
    });
    let legacy_digest = ids::sha256_hex(&serde_json::to_vec(&legacy_digest).unwrap());
    assert_eq!(first["content_hash"], legacy_digest);
    assert!(markdown.contains("# AGENTS.md Memory View"));
    assert!(markdown.contains("Source of truth remains the local SQLite store"));
    assert!(markdown.contains("agents-md adapter views"));
}

#[test]
fn cli_adapter_claude_code_export_is_deterministic() {
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
            "Decision: claude-code adapter views reuse the memory card export path",
        ])
        .assert()
        .success();

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "claude-code",
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
            "adapter",
            "export",
            "--target",
            "claude-code",
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

    assert_eq!(first["target"], "claude-code");
    assert_eq!(first["adapter_version"], "adapter-view-v1");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["source_card_type"], "workspace_summary");
    assert_eq!(first["budget"]["truncated"], false);
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
    let context_pack = &first["context_pack"];
    assert_eq!(context_pack["target"], "claude-code");
    assert_eq!(context_pack["template"], "claude-code-v1");
    assert_eq!(context_pack["adapter_version"], "adapter-view-v1");
    assert_eq!(context_pack["authority"], "recall_not_authority");
    assert_eq!(context_pack["profile"], "personal");
    assert_eq!(context_pack["workspace"], "josh-personal");
    assert_eq!(context_pack["card_type"], "workspace_summary");
    assert_eq!(context_pack["budget"]["truncated"], false);
    assert_eq!(
        context_pack["source_ids"],
        first
            .get("source_ids")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]))
    );
    assert_eq!(context_pack["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        context_pack["records"][0]["content"],
        "Decision: claude-code adapter views reuse the memory card export path"
    );
    let markdown = first["markdown"].as_str().unwrap();
    assert!(markdown.contains("# CLAUDE.md Memory View"));
    assert!(markdown.contains("- Adapter target: `claude-code`"));
    assert!(markdown.contains("Source of truth remains the local SQLite store"));
    let response_source_ids = first
        .get("source_ids")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let legacy_digest = serde_json::json!({
        "target": first["target"],
        "adapter_version": first["adapter_version"],
        "profile": first["profile"],
        "workspace": first["workspace"],
        "subject_id": Value::Null,
        "source_card_type": first["source_card_type"],
        "source_ids": response_source_ids,
        "markdown": markdown,
    });
    let legacy_digest = ids::sha256_hex(&serde_json::to_vec(&legacy_digest).unwrap());
    assert_eq!(first["content_hash"], legacy_digest);

    let budgeted = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "claude-code",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--max-bytes",
            "160",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(budgeted.status.success());
    let budgeted: Value = serde_json::from_slice(&budgeted.stdout).unwrap();
    let markdown = budgeted["markdown"].as_str().unwrap();
    assert_eq!(budgeted["budget"]["max_bytes"], 160);
    assert_eq!(budgeted["budget"]["truncated"], true);
    assert!(budgeted["budget"]["rendered_bytes"].as_u64().unwrap() <= 160);
    assert_eq!(budgeted["context_pack"]["target"], "claude-code");
    assert_eq!(budgeted["context_pack"]["template"], "claude-code-v1");
    assert_eq!(budgeted["context_pack"]["budget"]["max_bytes"], 160);
    assert_eq!(budgeted["context_pack"]["budget"]["truncated"], true);
    assert_eq!(
        budgeted["context_pack"]["budget"]["rendered_bytes"],
        budgeted["budget"]["rendered_bytes"]
    );
    assert!(budgeted["context_pack"]["source_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(budgeted["context_pack"]["records"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        markdown.len() as u64,
        budgeted["budget"]["rendered_bytes"].as_u64().unwrap()
    );
}

#[test]
fn cli_adapter_copilot_export_is_deterministic() {
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
            "Decision: copilot adapter views use the shared memory card export path",
        ])
        .assert()
        .success();

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "copilot",
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
            "adapter",
            "export",
            "--target",
            "copilot",
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

    assert_eq!(first["target"], "copilot");
    assert_eq!(first["adapter_version"], "adapter-view-v1");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["source_card_type"], "workspace_summary");
    assert_eq!(first["budget"]["truncated"], false);
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
    let context_pack = &first["context_pack"];
    assert_eq!(context_pack["target"], "copilot");
    assert_eq!(context_pack["template"], "copilot-v1");
    assert_eq!(context_pack["adapter_version"], "adapter-view-v1");
    assert_eq!(context_pack["authority"], "recall_not_authority");
    assert_eq!(context_pack["profile"], "personal");
    assert_eq!(context_pack["workspace"], "josh-personal");
    assert_eq!(context_pack["card_type"], "workspace_summary");
    assert_eq!(context_pack["budget"]["truncated"], false);
    assert_eq!(
        context_pack["source_ids"],
        first
            .get("source_ids")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]))
    );
    assert_eq!(context_pack["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        context_pack["records"][0]["content"],
        "Decision: copilot adapter views use the shared memory card export path"
    );
    let markdown = first["markdown"].as_str().unwrap();
    assert!(markdown.contains("# Copilot Instructions Memory View"));
    assert!(markdown.contains("- Adapter target: `copilot`"));
    assert!(markdown.contains("Source of truth remains the local SQLite store"));
    let response_source_ids = first
        .get("source_ids")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let legacy_digest = serde_json::json!({
        "target": first["target"],
        "adapter_version": first["adapter_version"],
        "profile": first["profile"],
        "workspace": first["workspace"],
        "subject_id": Value::Null,
        "source_card_type": first["source_card_type"],
        "source_ids": response_source_ids,
        "markdown": markdown,
    });
    let legacy_digest = ids::sha256_hex(&serde_json::to_vec(&legacy_digest).unwrap());
    assert_eq!(first["content_hash"], legacy_digest);

    let budgeted = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "copilot",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--max-bytes",
            "160",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(budgeted.status.success());
    let budgeted: Value = serde_json::from_slice(&budgeted.stdout).unwrap();
    let markdown = budgeted["markdown"].as_str().unwrap();
    assert_eq!(budgeted["budget"]["max_bytes"], 160);
    assert_eq!(budgeted["budget"]["truncated"], true);
    assert!(budgeted["budget"]["rendered_bytes"].as_u64().unwrap() <= 160);
    assert_eq!(budgeted["context_pack"]["target"], "copilot");
    assert_eq!(budgeted["context_pack"]["template"], "copilot-v1");
    assert_eq!(budgeted["context_pack"]["budget"]["max_bytes"], 160);
    assert_eq!(budgeted["context_pack"]["budget"]["truncated"], true);
    assert_eq!(
        budgeted["context_pack"]["budget"]["rendered_bytes"],
        budgeted["budget"]["rendered_bytes"]
    );
    assert!(budgeted["context_pack"]["source_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(budgeted["context_pack"]["records"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        markdown.len() as u64,
        budgeted["budget"]["rendered_bytes"].as_u64().unwrap()
    );
}

#[test]
fn cli_adapter_github_instructions_export_is_deterministic() {
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
            "Decision: github-instructions adapter views mirror the custom instructions layout",
        ])
        .assert()
        .success();

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "github-instructions",
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
            "adapter",
            "export",
            "--target",
            "github-instructions",
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

    assert_eq!(first["target"], "github-instructions");
    assert_eq!(first["adapter_version"], "adapter-view-v1");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["source_card_type"], "workspace_summary");
    assert_eq!(first["budget"]["truncated"], false);
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
    let markdown = first["markdown"].as_str().unwrap();
    assert!(markdown.contains("# GitHub Instructions Memory View"));
    assert!(markdown.contains("- Adapter target: `github-instructions`"));
    assert!(markdown.contains("Source of truth remains the local SQLite store"));
}

#[test]
fn cli_adapter_markdown_export_is_deterministic() {
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
            "Decision: markdown adapter views use the shared memory card export path",
        ])
        .assert()
        .success();

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "markdown",
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
            "adapter",
            "export",
            "--target",
            "markdown",
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

    assert_eq!(first["target"], "markdown");
    assert_eq!(first["adapter_version"], "adapter-view-v1");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["source_card_type"], "workspace_summary");
    assert_eq!(first["budget"]["truncated"], false);
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);
    let markdown = first["markdown"].as_str().unwrap();
    assert!(markdown.contains("# Markdown Memory View"));
    assert!(markdown.contains("- Adapter target: `markdown`"));
    assert!(markdown.contains("Source of truth remains the local SQLite store"));
}

#[test]
fn cli_adapter_mcp_pack_export_is_deterministic() {
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
            "Decision: mcp-pack adapter exports deterministic JSON context",
        ])
        .assert()
        .success();

    let first = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "mcp-pack",
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
            "adapter",
            "export",
            "--target",
            "mcp-pack",
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

    assert_eq!(first["target"], "mcp-pack");
    assert_eq!(first["adapter_version"], "adapter-view-v1");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["source_card_type"], "workspace_summary");
    assert_eq!(first["budget"]["truncated"], false);
    assert_eq!(first["content_hash"], second["content_hash"]);
    assert_eq!(first, second);

    let context_pack = &first["context_pack"];
    assert_eq!(context_pack["target"], "mcp-pack");
    assert_eq!(context_pack["template"], "mcp-json-v1");
    assert_eq!(context_pack["adapter_version"], "adapter-view-v1");
    assert_eq!(context_pack["authority"], "recall_not_authority");
    assert_eq!(context_pack["profile"], "personal");
    assert_eq!(context_pack["workspace"], "josh-personal");
    assert_eq!(context_pack["card_type"], "workspace_summary");
    assert_eq!(context_pack["budget"]["truncated"], false);
    assert_eq!(context_pack["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        context_pack["records"][0]["content"],
        "Decision: mcp-pack adapter exports deterministic JSON context"
    );

    let markdown = first["markdown"].as_str().unwrap();
    assert!(markdown.contains("# MCP JSON Context Pack"));
    assert!(markdown.contains("```json"));
    assert!(markdown.contains("\"target\": \"mcp-pack\""));
    assert!(markdown.contains("\"template\": \"mcp-json-v1\""));
    assert!(markdown.contains("\"authority\": \"recall_not_authority\""));

    let budgeted = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "mcp-pack",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--max-bytes",
            "160",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(budgeted.status.success());
    let budgeted: Value = serde_json::from_slice(&budgeted.stdout).unwrap();
    let markdown = budgeted["markdown"].as_str().unwrap();
    assert_eq!(budgeted["budget"]["max_bytes"], 160);
    assert_eq!(budgeted["budget"]["truncated"], true);
    assert!(budgeted["budget"]["rendered_bytes"].as_u64().unwrap() <= 160);
    assert_eq!(budgeted["context_pack"]["budget"]["max_bytes"], 160);
    assert_eq!(budgeted["context_pack"]["budget"]["truncated"], true);
    assert_eq!(budgeted["context_pack"]["template"], "mcp-json-v1");
    assert_eq!(
        budgeted["context_pack"]["budget"]["rendered_bytes"],
        budgeted["budget"]["rendered_bytes"]
    );
    assert!(budgeted["context_pack"]["source_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(budgeted["context_pack"]["records"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        markdown.len() as u64,
        budgeted["budget"]["rendered_bytes"].as_u64().unwrap()
    );
}

#[test]
fn cli_conformance_adapters_report_is_deterministic() {
    let output = bin()
        .args(["conformance", "adapters", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let first: Value = serde_json::from_slice(&output.stdout).unwrap();

    let second = bin()
        .args(["conformance", "adapters", "--format", "json"])
        .output()
        .unwrap();
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();

    assert_eq!(first, second);
    assert_eq!(first["report"], "adapter_conformance_v1");
    assert_eq!(first["status"], "passed");
    assert_eq!(first["authority"], "recall_not_authority");
    assert_eq!(first["targets"].as_array().unwrap().len(), 6);
    assert!(first["targets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|target| target["target"] == "mcp-pack" && target["context_pack"] == true));
    assert!(first["checks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|check| check["status"] == "passed"));

    bin()
        .args(["conformance", "adapters", "--format", "markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Adapter conformance report"))
        .stdout(predicate::str::contains("`mcp-pack`: passed"));
}

#[test]
fn cli_adapter_agents_md_budget_and_target_validation() {
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
            "Decision: this long adapter export record should be truncated by a tiny byte budget",
        ])
        .assert()
        .success();

    let output = bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "agents-md",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
            "--max-bytes",
            "160",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let export: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(export["budget"]["max_bytes"], 160);
    assert_eq!(export["budget"]["truncated"], true);
    assert!(export["budget"]["rendered_bytes"].as_u64().unwrap() <= 160);
    assert_eq!(export["context_pack"]["target"], "agents-md");
    assert_eq!(export["context_pack"]["template"], "agents-md-v1");
    assert_eq!(export["context_pack"]["budget"]["max_bytes"], 160);
    assert_eq!(export["context_pack"]["budget"]["truncated"], true);
    assert_eq!(
        export["context_pack"]["budget"]["rendered_bytes"],
        export["budget"]["rendered_bytes"]
    );
    assert!(export["context_pack"]["source_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(export["context_pack"]["records"]
        .as_array()
        .unwrap()
        .is_empty());

    bin()
        .arg("--db")
        .arg(&db)
        .args([
            "adapter",
            "export",
            "--target",
            "not-a-target",
            "--profile",
            "personal",
            "--workspace",
            "josh-personal",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown adapter target"));
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
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("up"))
        .stdout(predicate::str::contains("down"))
        .stdout(predicate::str::contains("logs"))
        .stdout(predicate::str::contains("upgrade"))
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("recall"))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("dream"))
        .stdout(predicate::str::contains("sync-local"))
        .stdout(predicate::str::contains("git-import"))
        .stdout(predicate::str::contains("export"))
        .stdout(predicate::str::contains("forget"))
        .stdout(predicate::str::contains("doctor"));
}

#[test]
fn cli_container_status_reports_managed_runtime_shape_without_compose() {
    let dir = TempDir::new().unwrap();

    bin()
        .env("CODEX_MEMORYD_HOME", dir.path().join("memoryd-home"))
        .args(["--runtime", "container", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"runtime\": \"container\""))
        .stdout(predicate::str::contains("127.0.0.1:8787 -> container:8787"));
}

#[test]
fn cli_url_client_mode_routes_daily_commands_to_daemon() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let bind = unused_loopback_addr();
    let url = format!("http://{bind}");
    let mut child = bin()
        .arg("--db")
        .arg(&db)
        .args(["serve", "--bind", &bind])
        .spawn()
        .expect("spawn daemon");
    wait_for_health(&url);

    let result = std::panic::catch_unwind(|| {
        bin()
            .arg("--url")
            .arg(&url)
            .args([
                "conclude",
                "--profile",
                "personal",
                "--workspace",
                "josh-personal",
                "--content",
                "Decision: URL client mode routes CLI commands to the daemon",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("\"ok\":true"));

        bin()
            .arg("--url")
            .arg(&url)
            .args([
                "recall",
                "--profile",
                "personal",
                "--workspace",
                "josh-personal",
                "--query",
                "URL client mode daemon",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("\"ok\":true"))
            .stdout(predicate::str::contains("URL client mode"));

        bin()
            .arg("--url")
            .arg(&url)
            .args([
                "adapter",
                "export",
                "--profile",
                "personal",
                "--workspace",
                "josh-personal",
                "--target",
                "agents-md",
                "--max-bytes",
                "4000",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("recall_not_authority"));
    });

    let _ = child.kill();
    let _ = child.wait();
    if let Err(err) = result {
        std::panic::resume_unwind(err);
    }
}

#[test]
fn cli_init_is_idempotent_for_product_runtime_home() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("memoryd-home");

    bin()
        .env("CODEX_MEMORYD_HOME", &home)
        .args(["init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"created\""))
        .stdout(predicate::str::contains("config.toml"))
        .stdout(predicate::str::contains("runtime.env"));

    bin()
        .env("CODEX_MEMORYD_HOME", &home)
        .args(["init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"reused\""))
        .stdout(predicate::str::contains("config.toml"))
        .stdout(predicate::str::contains("runtime.env"));

    assert!(home.join("config.toml").exists());
    assert!(home.join("runtime.env").exists());
    assert!(home.join("memory.db").exists());
    assert!(home.join("logs").is_dir());
    assert!(home.join("backups").is_dir());
    assert!(home.join("exports").is_dir());
}

#[test]
fn cli_init_port_persists_runtime_env_for_later_commands() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("memoryd-home");

    bin()
        .env("CODEX_MEMORYD_HOME", &home)
        .args(["init", "--port", "8989"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"next_command\": \"codex-memoryd up\"",
        ))
        .stdout(predicate::str::contains("seeds config/runtime only"))
        .stdout(predicate::str::contains("next command: codex-memoryd up"))
        .stdout(predicate::str::contains("127.0.0.1:8989"));

    let runtime_env = std::fs::read_to_string(home.join("runtime.env")).unwrap();
    assert!(runtime_env.contains("CODEX_MEMORYD_URL=http://127.0.0.1:8989"));
    assert!(runtime_env.contains("CODEX_MEMORYD_HOST=127.0.0.1"));
    assert!(runtime_env.contains("CODEX_MEMORYD_PORT=8989"));
    assert!(runtime_env.contains("CODEX_MEMORYD_BIND=127.0.0.1:8989"));

    let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(config.contains("bind = \"127.0.0.1:8989\""));

    bin()
        .env("CODEX_MEMORYD_HOME", &home)
        .args(["config", "show", "--resolved"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"url\": \"http://127.0.0.1:8989\"",
        ))
        .stdout(predicate::str::contains("\"host\": \"127.0.0.1\""))
        .stdout(predicate::str::contains("\"port\": 8989"));
}

#[test]
fn cli_init_dogfood_creates_repo_local_layout() {
    let dir = TempDir::new().unwrap();

    bin()
        .current_dir(dir.path())
        .args(["init", "--dogfood"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".dogfood"))
        .stdout(predicate::str::contains("memory.db"));

    assert!(dir.path().join(".dogfood/config.toml").exists());
    assert!(dir.path().join(".dogfood/runtime.env").exists());
    assert!(dir.path().join(".dogfood/memory.db").exists());
}

#[test]
fn cli_image_build_help_documents_local_tag_path() {
    bin()
        .args(["image", "build", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--tag"))
        .stdout(predicate::str::contains("codex-memoryd:local"));
}

#[test]
fn cli_config_show_resolved_reports_runtime_and_daemon_values() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("memoryd-home");

    bin()
        .env("CODEX_MEMORYD_HOME", &home)
        .args(["config", "show", "--resolved"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"registry\""))
        .stdout(predicate::str::contains("\"owner\""))
        .stdout(predicate::str::contains("\"source\""))
        .stdout(predicate::str::contains("\"restart_required\""))
        .stdout(predicate::str::contains("\"client\""))
        .stdout(predicate::str::contains("\"runtime\""))
        .stdout(predicate::str::contains("\"daemon\""))
        .stdout(predicate::str::contains("\"decision\": \"native\""))
        .stdout(predicate::str::contains("127.0.0.1:8787"));
}

#[test]
fn cli_dream_enable_disable_status_mutates_config_file() {
    let dir = TempDir::new().unwrap();
    let config = dir.path().join("config.toml");

    bin()
        .arg("--config")
        .arg(&config)
        .args(["dream", "enable"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"dream_scheduler_enabled\": true",
        ));

    bin()
        .arg("--config")
        .arg(&config)
        .args(["dream", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"enabled\": true"));

    bin()
        .arg("--config")
        .arg(&config)
        .args(["dream", "disable"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"dream_scheduler_enabled\": false",
        ));

    let raw = std::fs::read_to_string(config).unwrap();
    assert!(raw.contains("scheduler_enabled = false"));
}

#[test]
fn readme_keeps_first_run_path_documented() {
    let readme = include_str!("../README.md");
    for required in [
        "First-run path (source build)",
        "Canonical local runtime helper",
        "scripts/codex-memoryd-local-runtime.sh smoke",
        "CODEX_MEMORYD_DB",
        "CODEX_MEMORYD_BIND=127.0.0.1:8787",
        "CODEX_MEMORYD_ALLOW_NON_LOOPBACK=1",
        "codex-memoryd doctor",
        "curl -fsS http://127.0.0.1:8787/v1/status",
        "codex-memoryd sync-local --preview",
        "codex-memoryd sync-local --apply",
        "codex-memoryd conclude --profile personal",
        "codex-memoryd recall --profile personal",
        "docs/native-codex-memory-migration.md",
        "Fail-open note",
    ] {
        assert!(readme.contains(required), "README missing {required:?}");
    }
}

#[test]
fn local_runtime_helper_documents_safe_runtime_contract() {
    let helper = include_str!("../scripts/codex-memoryd-local-runtime.sh");
    let runbook = include_str!("../docs/dogfood-local.md");
    for required in [
        "CODEX_MEMORYD_BIND",
        "127.0.0.1:8787",
        "CODEX_MEMORYD_DB",
        "CODEX_MEMORYD_PROFILE",
        "CODEX_MEMORYD_WORKSPACE",
        "CODEX_MEMORYD_ALLOW_NON_LOOPBACK",
        "systemd-unit",
        "restart-survival",
        "smoke=pass",
        "refusing non-loopback bind",
        "sync-local --preview",
        "recall",
        "export",
    ] {
        assert!(
            helper.contains(required),
            "runtime helper missing {required:?}"
        );
    }

    for required in ["base_url", "profile", "workspace", "credential_env"] {
        assert!(
            runbook.contains(required),
            "local runbook missing adapter config field {required:?}"
        );
    }
}
