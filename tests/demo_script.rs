use std::process::Command;

#[test]
fn demo_script_uses_configured_profile_and_workspace_in_mcp_canary() {
    let output = Command::new("bash")
        .arg("scripts/demo-substrate.sh")
        .env("DEMO_PROFILE", "team")
        .env("DEMO_WORKSPACE", "fixture-team")
        .env("CODEX_MEMORYD_DEMO_KEEP", "1")
        .output()
        .expect("run demo script with custom profile/workspace");

    assert!(
        output.status.success(),
        "demo script failed with non-default profile/workspace: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("PASS"),
        "demo script did not reach the success marker: {}",
        stdout
    );
}

#[test]
fn demo_script_dry_run_lists_fixture_only_release_path() {
    let output = Command::new("bash")
        .arg("scripts/demo-substrate.sh")
        .arg("--dry-run")
        .output()
        .expect("run demo script dry run");

    assert!(
        output.status.success(),
        "demo dry run failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "temp fixture DB",
        "sync-local fixture import",
        "subject and episode",
        "recall with policy metadata",
        "card show",
        "adapter export",
        "git-import fixture",
        "procedure preview/apply/recall",
        "eval substrate",
        "read-only MCP canary",
    ] {
        assert!(
            stdout.contains(expected),
            "missing dry-run step: {expected}"
        );
    }

    assert!(
        !stdout.contains(".dogfood/memory.db") && !stdout.contains("~/.codex/memories"),
        "demo dry run must stay fixture-only and avoid real dogfood/Codex memories"
    );
}
