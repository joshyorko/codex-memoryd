use std::process::Command;

#[test]
fn release_gate_dry_run_lists_required_v0_1_checks() {
    let output = Command::new("bash")
        .arg("scripts/v0.1-release-gate.sh")
        .arg("--dry-run")
        .output()
        .expect("run release gate dry run");

    assert!(
        output.status.success(),
        "release gate dry run failed: stdout=\n{}\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "cargo fmt --all --check",
        "git diff --check",
        "cargo test",
        "doctor --format json",
        "eval substrate --compare --format json",
        "eval procedures --format json",
        "perf --format json",
        "scripts/demo-substrate.sh --dry-run",
        "scripts/dogfood-write-sandbox.sh --dry-run",
        "scripts/dogfood-compose-heartbeat.sh",
        "scripts/codex-memoryd-local-runtime.sh",
    ] {
        assert!(stdout.contains(expected), "missing gate check: {expected}");
    }

    for issue in [
        "#140", "#141", "#142", "#143", "#144", "#145", "#146", "#147", "#148", "#149", "#150",
        "#151", "#152",
    ] {
        assert!(stdout.contains(issue), "missing landed issue link: {issue}");
    }
}
