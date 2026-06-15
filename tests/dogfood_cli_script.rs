use std::process::Command;

#[test]
fn memd_dry_run_shows_local_db_front_door() {
    let output = Command::new("bash")
        .arg("scripts/memd")
        .arg("--dry-run")
        .arg("dream")
        .arg("--apply")
        .env("CODEX_MEMORYD_BIN", "/tmp/codex-memoryd-test-bin")
        .env("CODEX_MEMORYD_DB", "/tmp/codex-memoryd-test.db")
        .env("CODEX_MEMORYD_PROFILE", "personal")
        .env("CODEX_MEMORYD_WORKSPACE", "josh-personal")
        .output()
        .expect("run memd dry run");

    assert!(
        output.status.success(),
        "memd dry run failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "CODEX_MEMORYD_DB=/tmp/codex-memoryd-test.db",
        "CODEX_MEMORYD_PROFILE=personal",
        "CODEX_MEMORYD_WORKSPACE=josh-personal",
        "/tmp/codex-memoryd-test-bin",
        "dream",
        "--apply",
    ] {
        assert!(
            stdout.contains(expected),
            "missing dry-run piece: {expected}"
        );
    }
}
