use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

#[test]
fn dream_scheduler_env_is_visible_in_status() {
    let dir = TempDir::new().expect("tempdir");
    let db = dir.path().join("memory.db");
    let output = Command::new(cargo_bin("codex-memoryd"))
        .arg("--db")
        .arg(&db)
        .arg("status")
        .env("CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED", "1")
        .output()
        .expect("run status");

    assert!(
        output.status.success(),
        "status failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("status is json");
    assert_eq!(
        json["features"]["dream_scheduler"]["enabled"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        json["dream_worker"]["enabled"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        json["dream_worker"]["mode"],
        serde_json::Value::String("deterministic".to_string())
    );
}
