//! Regression tests use only children spawned by this test and disposable homes.
#![cfg(target_os = "linux")]

use std::fs;
use std::net::TcpListener;
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
        fs::write(home.path().join("codex-memoryd.pid"), child.id().to_string())
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
fn legacy_unrelated_live_pid_is_never_signalled_by_down() {
    let mut fixture = LegacyPidFixture::new();
    let _ = fixture.run("down");
    assert!(
        fixture.child.try_wait().expect("inspect child").is_none(),
        "down signalled a child that was not managed by MemoryD"
    );
}
