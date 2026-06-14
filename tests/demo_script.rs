use std::process::Command;

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
