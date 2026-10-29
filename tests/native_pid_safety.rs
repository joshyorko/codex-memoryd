//! Regression tests use only children spawned by this test and disposable homes.
#![cfg(target_os = "linux")]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Output};

use tempfile::TempDir;

struct LegacyPidFixture {
    home: TempDir,
    child: Child,
    port: u16,
}

impl LegacyPidFixture {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("temporary runtime");
        let child = Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .expect("disposable child");
        fs::write(
            home.path().join("codex-memoryd.pid"),
            child.id().to_string(),
        )
        .expect("legacy PID file");
        let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral loopback port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        Self { home, child, port }
    }

    fn run(&self, action: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
            .env_clear()
            .env("HOME", self.home.path())
            .env("CODEX_MEMORYD_HOME", self.home.path())
            .env("CODEX_MEMORYD_PORT", self.port.to_string())
            .env("CODEX_MEMORYD_BIND", format!("127.0.0.1:{}", self.port))
            .args(["--runtime", "native", action])
            .output()
            .expect("run isolated native command")
    }

    fn run_helper(&self, action: &str) -> Output {
        Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/codex-memoryd-local-runtime.sh"
            ))
            .arg(action)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.home.path())
            .env("CODEX_MEMORYD_RUNTIME_DIR", self.home.path())
            .env("CODEX_MEMORYD_BIN", env!("CARGO_BIN_EXE_codex-memoryd"))
            .env("CODEX_MEMORYD_DB", self.home.path().join("memory.db"))
            .env("CODEX_MEMORYD_BIND", format!("127.0.0.1:{}", self.port))
            .env(
                "CODEX_MEMORYD_BASE_URL",
                format!("http://127.0.0.1:{}", self.port),
            )
            .output()
            .unwrap()
    }
}

impl Drop for LegacyPidFixture {
    fn drop(&mut self) {
        // The child is ours and has not been reaped while the CLI is running,
        // so its PID cannot be reused by an unrelated process.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn legacy_unrelated_live_pid_is_not_reported_as_managed() {
    let fixture = LegacyPidFixture::new();
    let output = fixture.run("status");
    assert!(output.status.success(), "{:?}", output);
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_ne!(status["process"], "running");
    assert!(status["pid"].is_null());
}

#[test]
fn responsive_endpoint_does_not_hide_unverified_pid() {
    let fixture = LegacyPidFixture::new();
    let listener = TcpListener::bind(("127.0.0.1", fixture.port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    );
                    break;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                Err(_) => break,
            }
        }
    });
    let output = fixture.run("status");
    server.join().unwrap();
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["process"], "unverified");
    assert!(status["pid"].is_null());
}

#[test]
fn legacy_unrelated_live_pid_is_never_signalled_by_down() {
    let mut fixture = LegacyPidFixture::new();
    let output = fixture.run("down");
    assert!(!output.status.success());
    assert!(
        fixture.child.try_wait().expect("inspect child").is_none(),
        "down signalled a child that was not managed by MemoryD"
    );
}

#[test]
fn live_legacy_state_requires_explicit_recovery_before_starting() {
    let mut fixture = LegacyPidFixture::new();
    let output = fixture.run("up");
    assert!(!output.status.success());
    assert!(fixture.home.path().join("codex-memoryd.pid").exists());
    assert!(fixture.child.try_wait().unwrap().is_none());
}

#[test]
fn malformed_identity_and_pid_mismatch_fail_closed() {
    let mut fixture = LegacyPidFixture::new();
    let path = fixture.home.path().join("codex-memoryd.pid.identity");
    for raw in ["not json", "{}", "{\"version\":1,\"pid\":0}"] {
        fs::write(&path, raw).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let status = fixture.run("status");
        let report: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(report["process"], "unverified");
        assert!(!fixture.run("down").status.success());
        assert!(fixture.child.try_wait().unwrap().is_none());
    }
}

#[test]
fn legacy_unrelated_live_pid_is_never_signalled_by_restart_or_helper() {
    let mut fixture = LegacyPidFixture::new();
    let restart = fixture.run("restart");
    assert!(!restart.status.success());
    let helper = fixture.run_helper("stop");
    assert!(!helper.status.success());
    assert!(fixture.child.try_wait().unwrap().is_none());
}

#[test]
fn helper_uses_managed_lifecycle_for_its_own_disposable_runtime() {
    let fixture = LegacyPidFixture::new();
    fs::remove_file(fixture.home.path().join("codex-memoryd.pid")).unwrap();
    struct Cleanup<'a>(&'a LegacyPidFixture);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.run_helper("stop");
        }
    }
    let _cleanup = Cleanup(&fixture);
    for action in ["start", "status", "stop"] {
        let output = fixture.run_helper(action);
        assert!(
            output.status.success(),
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn older_binary_without_identity_handshake_cannot_publish_state() {
    let fixture = LegacyPidFixture::new();
    fs::remove_file(fixture.home.path().join("codex-memoryd.pid")).unwrap();
    let old_binary = fixture.home.path().join("old-memoryd");
    fs::write(&old_binary, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
    fs::set_permissions(&old_binary, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
        .env_clear()
        .env("HOME", fixture.home.path())
        .env("CODEX_MEMORYD_HOME", fixture.home.path())
        .env("CODEX_MEMORYD_PORT", fixture.port.to_string())
        .env("CODEX_MEMORYD_BIND", format!("127.0.0.1:{}", fixture.port))
        .env("CODEX_MEMORYD_BIN", old_binary)
        .args(["--runtime", "native", "up"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("child cleanup: Ok"));
    assert!(!fixture.home.path().join("codex-memoryd.pid").exists());
    assert!(!fixture
        .home
        .path()
        .join("codex-memoryd.pid.identity")
        .exists());
}

#[test]
fn malformed_state_fails_closed() {
    let mut fixture = LegacyPidFixture::new();
    for raw in ["0", "-1", "4294967296", "not-a-pid", ""] {
        fs::write(fixture.home.path().join("codex-memoryd.pid"), raw).unwrap();
        let status = fixture.run("status");
        let report: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_ne!(report["process"], "running", "{raw:?}");
        assert!(report["pid"].is_null());
        assert!(!fixture.run("down").status.success(), "{raw:?}");
        assert!(fixture.child.try_wait().unwrap().is_none());
    }
}

#[test]
fn exited_legacy_process_does_not_block_normal_startup() {
    let mut fixture = LegacyPidFixture::new();
    fixture.child.kill().unwrap();
    // Keep the dead child unreaped: its PID cannot be reassigned during this
    // test, and pidfd polling recognizes the exited process even as a zombie.
    let start = fixture.run("up");
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&start.stdout).unwrap();
    assert_eq!(report["process"], "running");
    let stop = fixture.run("down");
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(!fixture.home.path().join("codex-memoryd.pid").exists());
}
