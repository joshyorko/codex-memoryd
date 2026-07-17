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

#[test]
fn adjacent_runtime_defaults_to_disabled_status() {
    let dir = TempDir::new().expect("tempdir");
    let db = dir.path().join("memory.db");
    let output = Command::new(cargo_bin("codex-memoryd"))
        .arg("--db")
        .arg(&db)
        .arg("status")
        .output()
        .expect("run status");

    assert!(
        output.status.success(),
        "status failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("status is json");
    assert_eq!(json["adjacent_runtime"]["status"], "disabled");
    assert_eq!(json["adjacent_runtime"]["configured"], false);
    assert_eq!(
        json["adjacent_runtime"]["ownership"]["owner"],
        "adjacent-app"
    );
}

#[test]
fn adjacent_runtime_config_reports_reachable_endpoint() {
    let dir = TempDir::new().expect("tempdir");
    let db = dir.path().join("memory.db");
    let config = dir.path().join("config.toml");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    std::fs::write(
        &config,
        format!(
            "[runtime.adjacent]\nenabled = true\nname = \"dogfood-router\"\nurl = \"http://{addr}\"\n"
        ),
    )
    .expect("write config");

    let output = Command::new(cargo_bin("codex-memoryd"))
        .arg("--config")
        .arg(&config)
        .arg("--db")
        .arg(&db)
        .arg("status")
        .output()
        .expect("run status");

    assert!(
        output.status.success(),
        "status failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("status is json");
    assert_eq!(json["adjacent_runtime"]["status"], "reachable");
    assert_eq!(json["adjacent_runtime"]["configured"], true);
    assert_eq!(json["adjacent_runtime"]["name"], "dogfood-router");
    assert_eq!(
        json["adjacent_runtime"]["ownership"]["conflict_with_memoryd"],
        false
    );
}

#[test]
fn adjacent_runtime_config_reports_endpoint_conflict() {
    let dir = TempDir::new().expect("tempdir");
    let db = dir.path().join("memory.db");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        "[runtime.adjacent]\nenabled = true\nurl = \"http://127.0.0.1:8787\"\n",
    )
    .expect("write config");

    let output = Command::new(cargo_bin("codex-memoryd"))
        .arg("--config")
        .arg(&config)
        .arg("--db")
        .arg(&db)
        .arg("status")
        .output()
        .expect("run status");

    assert!(
        output.status.success(),
        "status failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("status is json");
    assert_eq!(json["adjacent_runtime"]["status"], "conflict");
    assert_eq!(
        json["adjacent_runtime"]["ownership"]["conflict_with_memoryd"],
        true
    );
    assert_eq!(
        json["adjacent_runtime"]["ownership"]["memoryd_endpoint"],
        "http://127.0.0.1:8787"
    );
}
