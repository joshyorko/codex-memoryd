//! Non-Linux native lifecycle must not signal even a live disposable PID.
#![cfg(all(unix, not(target_os = "linux")))]

use std::fs;
use std::net::TcpListener;
use std::process::{Child, Command};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn native_lifecycle_fails_closed_without_safe_process_handles() {
    let home = tempfile::tempdir().unwrap();
    let mut child = ChildGuard(Command::new("/bin/sleep").arg("60").spawn().unwrap());
    fs::write(
        home.path().join("codex-memoryd.pid"),
        child.0.id().to_string(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    for action in ["status", "down", "restart", "up"] {
        let output = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
            .env_clear()
            .env("HOME", home.path())
            .env("CODEX_MEMORYD_HOME", home.path())
            .env("CODEX_MEMORYD_PORT", port.to_string())
            .env("CODEX_MEMORYD_BIND", format!("127.0.0.1:{port}"))
            .args(["--runtime", "native", action])
            .output()
            .unwrap();
        if action == "status" {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["process"], "unverified");
            assert!(report["pid"].is_null());
        } else {
            assert!(!output.status.success(), "{action} unexpectedly succeeded");
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "{action} signalled the fixture child"
        );
    }
}
