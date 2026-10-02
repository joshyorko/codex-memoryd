use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use codex_memoryd::{
    config::Config, protocol::ConclusionsRequest, server, service::Service, store::Store,
};
use serde_json::{json, Value};
use tempfile::TempDir;

struct Adapter {
    child: Child,
    output: BufReader<std::process::ChildStdout>,
}
impl Adapter {
    fn start(endpoint: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
            .args([
                "--url",
                endpoint,
                "mcp",
                "stdio",
                "--daemon",
                "--profile",
                "personal",
                "--workspace",
                "bounded",
            ])
            .env("HOME", "/proc/memoryd-impossible-home")
            .env_remove("CODEX_MEMORYD_DB")
            .env_remove("CODEX_MEMORYD_URL")
            .env("HTTP_PROXY", "http://proxy-user:proxy-SECRET@127.0.0.1:9")
            .env("ALL_PROXY", "http://proxy-user:proxy-SECRET@127.0.0.1:9")
            .env("NO_PROXY", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut adapter = Self { child, output };
        let response = adapter.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"daemon-test","version":"1"}}}));
        assert!(response["result"].is_object(), "{response}");
        writeln!(
            adapter.child.stdin.as_mut().unwrap(),
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        adapter
    }
    fn request(&mut self, message: Value) -> Value {
        writeln!(self.child.stdin.as_mut().unwrap(), "{message}").unwrap();
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "adapter exited before responding"
        );
        serde_json::from_str(&line).unwrap()
    }
    fn call(&mut self, name: &str, args: Value) -> Value {
        self.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":args}}))
    }
    fn stop(mut self) {
        self.child.stdin.take();
        assert!(self.child.wait().unwrap().success());
    }
}
impl Drop for Adapter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Daemon {
    endpoint: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<thread::JoinHandle<()>>,
    _dir: TempDir,
}
impl Daemon {
    fn start() -> Self {
        Self::start_with_config(Config::default())
    }
    fn start_with_config(config: Config) -> Self {
        let dir = TempDir::new().unwrap();
        let service = Service::new(Store::open(&dir.path().join("daemon.db")).unwrap(), config);
        for workspace in ["bounded", "other"] {
            service
                .conclusions(ConclusionsRequest {
                    profile: Some("personal".into()),
                    workspace: Some(workspace.into()),
                    repo: None,
                    target: Some("user".into()),
                    conclusions: Some(vec![format!("synthetic nebula {workspace} fact")]),
                    metadata: None,
                    record_type: None,
                })
                .unwrap();
        }
        for (profile, workspace) in [
            ("personal", "bounded"),
            ("personal", "other"),
            ("work", "bounded"),
        ] {
            let request =
                serde_json::from_value::<codex_memoryd::protocol::CheckpointRequest>(json!({
                    "profile": profile, "workspace": workspace,
                    "summary": format!("checkpoint nebula {profile} {workspace}"),
                    "next_steps": [format!("next {profile} {workspace}")]
                }))
                .unwrap();
            service.checkpoint(request).unwrap();
        }
        service
            .store
            .upsert_record(&codex_memoryd::store::NewRecord {
                profile_id: "personal".into(),
                workspace_id: "bounded".into(),
                repo_id: None,
                subject_id: None,
                episode_id: None,
                scope: codex_memoryd::domain::Scope::Workspace,
                record_type: codex_memoryd::domain::RecordType::Other,
                content: "nebula UPSTREAM-BLOCKED-PRIVATE".into(),
                related_files: vec![],
                tags: vec![],
                sensitivity: codex_memoryd::domain::Sensitivity::SecretBlocked,
                portability: codex_memoryd::domain::Portability::NeverExport,
                confidence: 1.0,
                source_ids: vec![],
                content_hash: "blocked-synthetic-hash".into(),
                supersedes: vec![],
                metadata: json!({}),
            })
            .unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let join = thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                ready_tx
                    .send(format!("http://{}", listener.local_addr().unwrap()))
                    .unwrap();
                axum::serve(listener, server::router(service))
                    .with_graceful_shutdown(async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            });
        });
        Self {
            endpoint: ready_rx.recv().unwrap(),
            stop: Some(stop),
            join: Some(join),
            _dir: dir,
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        self.join.take().unwrap().join().unwrap();
    }
}

#[test]
fn existing_daemon_reads_scope_without_local_store_and_survives_adapter_restart() {
    let daemon = Daemon::start();
    for _ in 0..2 {
        let mut adapter = Adapter::start(&daemon.endpoint);
        let normal_endpoint = daemon.endpoint.clone();
        let normal_client = thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap();
            for _ in 0..20 {
                let response: Value = client
                    .post(format!("{normal_endpoint}/v1/search"))
                    .json(&json!({"profile":"personal","workspace":"bounded","query":"nebula"}))
                    .send()
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json()
                    .unwrap();
                assert!(response["data"]["matches"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["content"].as_str().unwrap().contains("nebula bounded")));
                assert!(!response.to_string().contains("nebula other"));
                thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        let listed =
            adapter.request(json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}));
        let names: Vec<_> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["memory_status", "memory_recall", "memory_search"]);
        let status = adapter.call("memory_status", json!({}));
        assert_eq!(
            status["result"]["structuredContent"]["provider_name"],
            "codex-memoryd"
        );
        assert!(!status.to_string().contains("daemon.db"));
        for tool in ["memory_recall", "memory_search"] {
            let result = adapter.call(tool, json!({"query":"nebula"}));
            assert_ne!(result["result"]["isError"], true, "{result}");
            assert!(result.to_string().contains("nebula bounded"), "{result}");
            assert!(!result.to_string().contains("nebula other"), "{result}");
            assert!(
                !result.to_string().contains("UPSTREAM-BLOCKED-PRIVATE"),
                "{result}"
            );
            if tool == "memory_recall" {
                let checkpoints = result["result"]["structuredContent"]["checkpoints"]
                    .as_array()
                    .unwrap();
                assert_eq!(checkpoints.len(), 1, "{result}");
                assert_eq!(
                    checkpoints[0]["summary"],
                    "checkpoint nebula personal bounded"
                );
                assert!(!result.to_string().contains("checkpoint nebula work"));
            }
            let denied = adapter.call(tool, json!({"query":"nebula", "workspace":"other"}));
            assert_eq!(denied["result"]["isError"], true, "{denied}");
        }
        for tool in ["memory_recall", "memory_search"] {
            let denied = adapter.call(tool, json!({"query":"nebula", "profile":"work"}));
            assert_eq!(
                denied["result"]["structuredContent"]["error"]["code"],
                "profile_boundary_denied"
            );
            let empty = adapter.call(tool, json!({"query":"nebula", "workspace":""}));
            assert_eq!(
                empty["result"]["structuredContent"]["error"]["code"],
                "profile_boundary_denied"
            );
            let repeated = adapter.call(
                tool,
                json!({"query":"nebula", "profile":"personal", "workspace":"bounded"}),
            );
            assert_ne!(repeated["result"]["isError"], true);
        }
        let denied = adapter.call("memory_conclude", json!({"content":"not written"}));
        assert!(denied["error"].is_object(), "{denied}");
        adapter.stop();
        normal_client.join().unwrap();
        assert!(
            reqwest::blocking::get(format!("{}/v1/status", daemon.endpoint))
                .unwrap()
                .status()
                .is_success()
        );
    }
}

#[test]
fn daemon_down_never_falls_back_and_later_call_reconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut adapter = Adapter::start(&endpoint);
    let down = adapter.call("memory_search", json!({"query":"nebula"}));
    assert_eq!(down["result"]["isError"], true);
    let listener = TcpListener::bind(addr).unwrap();
    let responder = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_http_request(&stream);
        let body = json!({"ok":true,"data":{"matches":[],"next_cursor":null},"warnings":[],"request_id":"req_test","provider":{"name":"codex-memoryd","version":"test"}}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let recovered = adapter.call("memory_search", json!({"query":"nebula"}));
    assert_ne!(recovered["result"]["isError"], true, "{recovered}");
    adapter.stop();
    responder.join().unwrap();
}

// Drain the POST before emitting a response. Otherwise an early HTTP reply
// can cancel reqwest's request writer and test a broken transport, not a
// slow response body or reconnect. Keep fixtures faithful to the real server.
fn read_http_request(stream: &std::net::TcpStream) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    let mut length = 0;
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    std::io::Read::read_exact(&mut reader, &mut body).unwrap();
}

fn raw_response(status: &str, body: &str, extra_headers: &str) -> String {
    format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}", body.len())
}
fn raw_call(response: String) -> Value {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let responder = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_http_request(&stream);
        let _ = stream.write_all(response.as_bytes());
    });
    let mut adapter = Adapter::start(&endpoint);
    let result = adapter.call("memory_search", json!({"query":"nebula"}));
    adapter.stop();
    responder.join().unwrap();
    result
}

#[test]
fn bounded_upstream_errors_are_truthful_and_do_not_disclose_messages() {
    let failure = json!({"ok":false,"error":{"code":"policy_denied","message":"private path /secret/corpus token=SECRET"},"warnings":["SECRET"],"request_id":"req_test","provider":{"name":"codex-memoryd","version":"test"}}).to_string();
    let cases = [
        (
            raw_response("401 Unauthorized", "SECRET", ""),
            "auth_missing",
        ),
        (raw_response("403 Forbidden", "SECRET", ""), "policy_denied"),
        (
            raw_response("422 Unprocessable Entity", &failure, ""),
            "policy_denied",
        ),
        (raw_response("200 OK", "SECRET", ""), "internal_error"),
        (
            raw_response("200 OK", r#"{"ok":true,"data":{"matches":[]}}"#, ""),
            "internal_error",
        ),
        (
            raw_response("200 OK", &"x".repeat(2 * 1024 * 1024 + 1), ""),
            "internal_error",
        ),
    ];
    for (response, code) in cases {
        let result = raw_call(response);
        assert_eq!(result["result"]["isError"], true, "{result}");
        assert_eq!(
            result["result"]["structuredContent"]["error"]["code"], code,
            "{result}"
        );
        assert!(!result.to_string().contains("SECRET"));
        assert!(!result.to_string().contains("/secret/corpus"));
    }
}

#[test]
fn redirects_do_not_reach_the_target() {
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let location = format!(
        "Location: http://{}/private?token=SECRET\r\n",
        target.local_addr().unwrap()
    );
    let result = raw_call(raw_response("302 Found", "SECRET", &location));
    assert_eq!(
        result["result"]["structuredContent"]["error"]["code"],
        "policy_denied"
    );
    assert!(!result.to_string().contains("SECRET"));
    assert_eq!(
        target.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn untrusted_upstream_matches_cannot_escape_workspace() {
    let body = json!({"ok":true,"data":{"matches":[{"id":"m1","type":"fact","scope":"workspace","content":"PRIVATE","confidence":1.0,"workspace_id":"other","repo_id":null,"tags":[],"archived":false,"updated_at":"2026-10-02"}],"next_cursor":null},"warnings":[],"request_id":"req_test","provider":{"name":"codex-memoryd","version":"test"}}).to_string();
    let result = raw_call(raw_response("200 OK", &body, ""));
    assert_eq!(
        result["result"]["structuredContent"]["error"]["code"],
        "profile_boundary_denied"
    );
    assert!(!result.to_string().contains("PRIVATE"));
}

#[test]
fn slow_body_is_bounded_by_total_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let responder = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_http_request(&stream);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 10000\r\nConnection: close\r\n\r\n").unwrap();
        for _ in 0..12 {
            if stream.write_all(b" ").is_err() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(250));
        }
    });
    let mut adapter = Adapter::start(&endpoint);
    let started = std::time::Instant::now();
    let result = adapter.call("memory_search", json!({"query":"nebula"}));
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(
        result["result"]["structuredContent"]["error"]["code"],
        "storage_unavailable"
    );
    assert!(result.to_string().contains("timed out"), "{result}");
    adapter.stop();
    responder.join().unwrap();
}

#[test]
fn startup_rejects_nonlocal_endpoints_ambient_selection_and_write_modes() {
    let dir = TempDir::new().unwrap();
    let forbidden_db = dir.path().join("never-created").join("memory.db");
    let common = [
        "mcp",
        "stdio",
        "--daemon",
        "--profile",
        "personal",
        "--workspace",
        "bounded",
    ];
    for endpoint in [
        "http://example.com:8989",
        "http://127.0.0.1",
        "http://2130706433:8989",
        "https://127.0.0.1:8989",
        "http://localhost:8989",
        "http://user:SECRET@127.0.0.1:8989",
        "http://127.0.0.1:8989/path",
        "http://127.0.0.1:8989/?token=SECRET",
        "http://127.0.0.1:8989/#fragment",
        "http://127.0.0.1:0",
        "bad-SECRET-url",
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
            .args(["--url", endpoint])
            .args(common)
            .env_remove("CODEX_MEMORYD_DB")
            .output()
            .unwrap();
        assert!(!result.status.success(), "accepted {endpoint}");
        assert!(!String::from_utf8_lossy(&result.stderr).contains("SECRET"));
    }
    for extras in [
        vec!["--local"],
        vec!["--db", forbidden_db.to_str().unwrap()],
        vec!["--config", "/unreadable/config"],
        vec!["--runtime", "native"],
        vec!["--write-tools"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
            .args(["--url", "http://127.0.0.1:9"])
            .args(common)
            .args(extras)
            .env_remove("CODEX_MEMORYD_DB")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
    }
    for omitted in [
        vec!["mcp", "stdio", "--daemon"],
        vec!["mcp", "stdio", "--daemon", "--profile", "personal"],
        vec!["mcp", "stdio", "--daemon", "--workspace", "bounded"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
            .args(["--url", "http://127.0.0.1:9"])
            .args(omitted)
            .env_remove("CODEX_MEMORYD_DB")
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
    let missing_url = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
        .args(common)
        .env("CODEX_MEMORYD_URL", "http://127.0.0.1:9")
        .env_remove("CODEX_MEMORYD_DB")
        .output()
        .unwrap();
    assert!(!missing_url.status.success());
    let env_db = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
        .args(["--url", "http://127.0.0.1:9"])
        .args(common)
        .env("CODEX_MEMORYD_DB", &forbidden_db)
        .output()
        .unwrap();
    assert!(!env_db.status.success());
    assert!(!forbidden_db.parent().unwrap().exists());
}

#[test]
fn daemon_attachment_cannot_bypass_actual_http_transport_gate() {
    let config = Config {
        bind: "0.0.0.0:8989".into(),
        ..Config::default()
    };
    let daemon = Daemon::start_with_config(config);
    let mut adapter = Adapter::start(&daemon.endpoint);
    let status = adapter.call("memory_status", json!({}));
    assert_eq!(
        status["result"]["structuredContent"]["status"],
        "auth_missing"
    );
    for tool in ["memory_recall", "memory_search"] {
        let denied = adapter.call(tool, json!({"query":"nebula"}));
        assert_eq!(
            denied["result"]["structuredContent"]["error"]["code"],
            "auth_missing"
        );
        assert!(!denied.to_string().contains("nebula personal bounded"));
    }
    adapter.stop();
}

#[test]
fn explicit_daemon_attachment_ignores_private_ambient_runtime_selection() {
    let daemon = Daemon::start();
    let sentinel = "private-runtime-selection-synthetic-sentinel";
    let input = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"ambient-runtime-test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_status","arguments":{}}}),
    ].into_iter().map(|message| message.to_string()).collect::<Vec<_>>().join("\n") + "\n";
    let mut child = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
        .args([
            "--url",
            &daemon.endpoint,
            "mcp",
            "stdio",
            "--daemon",
            "--profile",
            "personal",
            "--workspace",
            "bounded",
        ])
        .env("CODEX_MEMORYD_RUNTIME", sentinel)
        .env("HOME", "/proc/memoryd-impossible-home")
        .env_remove("CODEX_MEMORYD_DB")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input_pipe = child.stdin.take().unwrap();
    input_pipe.write_all(input.as_bytes()).unwrap();
    drop(input_pipe);
    let output = child.wait_with_output().unwrap();
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains(sentinel),
        "ambient managed-runtime selection must not be logged by daemon attachment"
    );
    assert!(
        output.status.success(),
        "explicit attachment must not depend on ambient managed runtime"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(sentinel));
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(
        responses[1]["result"]["structuredContent"]["provider_name"],
        "codex-memoryd"
    );
}

#[test]
fn direct_local_commands_still_validate_ambient_runtime_selection() {
    let dir = TempDir::new().unwrap();
    for command in [vec!["status"], vec!["mcp", "stdio"]] {
        let db = dir.path().join("never-created").join("memory.db");
        let output = Command::new(env!("CARGO_BIN_EXE_codex-memoryd"))
            .arg("--db")
            .arg(&db)
            .arg("--local")
            .args(command)
            .env("CODEX_MEMORYD_RUNTIME", "invalid-synthetic-runtime")
            .env_remove("CODEX_MEMORYD_URL")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid CODEX_MEMORYD_RUNTIME"));
        assert!(output.stdout.is_empty());
        assert!(!db.parent().unwrap().exists());
    }
}
