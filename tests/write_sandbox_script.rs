use std::fs;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

#[test]
fn write_sandbox_dry_run_lists_safety_contract() {
    let output = Command::new("bash")
        .arg("scripts/dogfood-write-sandbox.sh")
        .arg("--dry-run")
        .output()
        .expect("run write sandbox dry run");

    assert!(
        output.status.success(),
        "write sandbox dry run failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "schema preflight",
        "backup create",
        "write-capable sandbox DB",
        "content-free diff report",
        "manual promotion preview",
        "real DB unchanged",
    ] {
        assert!(
            stdout.contains(expected),
            "missing dry-run safety step: {expected}"
        );
    }
}

#[test]
fn write_sandbox_fixture_run_keeps_real_db_content_free() {
    let dir = TempDir::new().unwrap();
    let real_db = dir.path().join("real.db");
    let sandbox_db = dir.path().join("sandbox.db");
    let artifact_dir = dir.path().join("artifacts");
    let bin = cargo_bin("codex-memoryd");
    let real_content = "Decision: real source stays stable for sandbox tests.";
    let sandbox_content = "write sandbox canary";

    let seed = Command::new(&bin)
        .arg("--db")
        .arg(&real_db)
        .args([
            "conclude",
            "--profile",
            "personal",
            "--workspace",
            "write-sandbox-test",
            "--content",
            real_content,
        ])
        .output()
        .expect("seed real fixture db");
    assert!(
        seed.status.success(),
        "seed failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&seed.stdout),
        String::from_utf8_lossy(&seed.stderr)
    );

    let output = Command::new("bash")
        .arg("scripts/dogfood-write-sandbox.sh")
        .arg("run")
        .arg("--real-db")
        .arg(&real_db)
        .arg("--sandbox-db")
        .arg(&sandbox_db)
        .arg("--artifact-dir")
        .arg(&artifact_dir)
        .arg("--profile")
        .arg("personal")
        .arg("--workspace")
        .arg("write-sandbox-test")
        .arg("--query")
        .arg(sandbox_content)
        .env("CODEX_MEMORYD_SANDBOX_BIN", &bin)
        .output()
        .expect("run write sandbox fixture");

    assert!(
        output.status.success(),
        "write sandbox fixture failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("real_unchanged=true"));
    assert!(stdout.contains("manual promotion preview"));
    assert!(
        !stdout.contains(real_content) && !stdout.contains(sandbox_content),
        "stdout must not serialize real or sandbox content"
    );

    let report = fs::read_to_string(artifact_dir.join("sandbox-diff-report.json"))
        .expect("diff report exists");
    assert!(report.contains("\"real_unchanged\": true"));
    assert!(report.contains("\"content_hash\""));
    assert!(
        !report.contains(real_content) && !report.contains(sandbox_content),
        "diff report must stay content-free"
    );

    let real_after = Command::new(&bin)
        .arg("--db")
        .arg(&real_db)
        .args([
            "recall",
            "--profile",
            "personal",
            "--workspace",
            "write-sandbox-test",
            "--query",
            sandbox_content,
        ])
        .output()
        .expect("recall real fixture db");
    assert!(
        real_after.status.success(),
        "real recall failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&real_after.stdout),
        String::from_utf8_lossy(&real_after.stderr)
    );
    let real_recall = String::from_utf8_lossy(&real_after.stdout);
    assert!(
        !real_recall.contains(sandbox_content),
        "sandbox canary must not be written to real DB"
    );
}
