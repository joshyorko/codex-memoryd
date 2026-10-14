//! Acceptance coverage for two independent native MemoryD homes.
//!
//! All records and native Codex memory files in this test are synthetic and
//! live under a disposable temp directory. No user home or external service is
//! read or changed.

use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use codex_memoryd::store::Store;
use serde_json::{json, Value};
use tempfile::TempDir;

const PROFILE: &str = "personal";
const WORKSPACE: &str = "dakota";
const FRIDAY_MARKER: &str = "amber-lantern";
const CODEX_MARKER: &str = "violet-otter";

struct Instance {
    name: &'static str,
    home: PathBuf,
    port: u16,
    url: String,
    profile: &'static str,
    workspace: &'static str,
    #[cfg(target_os = "linux")]
    reapers: Mutex<Vec<JoinHandle<()>>>,
}

impl Instance {
    fn new(name: &'static str, home: PathBuf, port: u16) -> Self {
        Self {
            name,
            home,
            port,
            url: format!("http://127.0.0.1:{port}"),
            profile: PROFILE,
            workspace: WORKSPACE,
            #[cfg(target_os = "linux")]
            reapers: Mutex::new(Vec::new()),
        }
    }

    fn command(&self) -> Command {
        command_for_home(&self.home)
    }

    fn initialize(&self) -> Value {
        let output = self
            .command()
            .env("CODEX_MEMORYD_PROFILE", self.profile)
            .env("CODEX_MEMORYD_WORKSPACE", self.workspace)
            .args(["init", "--host", "127.0.0.1", "--port"])
            .arg(self.port.to_string())
            .output()
            .expect("run init");
        let report = success_json(output, &format!("{} init", self.name));
        assert_eq!(report["home"], self.home.display().to_string());
        assert_eq!(report["db"], self.database().display().to_string());
        assert!(self.home.join("config.toml").is_file());
        assert!(self.home.join("runtime.env").is_file());
        assert!(self.database().is_file());
        report
    }

    fn database(&self) -> PathBuf {
        self.home.join("memory.db")
    }

    fn pid_file(&self) -> PathBuf {
        self.home.join("codex-memoryd.pid")
    }

    fn log_file(&self) -> PathBuf {
        self.home.join("logs/codex-memoryd.log")
    }

    fn inspect_paths(&self) -> Value {
        let output = self
            .command()
            .args(["paths", "--format", "json"])
            .output()
            .expect("run paths");
        let paths = success_json(output, &format!("{} paths", self.name));
        assert_eq!(
            paths["entries"]["runtime_home"]["path"],
            self.home.display().to_string()
        );
        assert_eq!(
            paths["entries"]["database"]["path"],
            self.database().display().to_string()
        );
        assert_eq!(
            paths["entries"]["config_file"]["path"],
            self.home.join("config.toml").display().to_string()
        );
        assert_eq!(
            paths["entries"]["pid_file"]["path"],
            self.pid_file().display().to_string()
        );
        assert_eq!(
            paths["entries"]["log_file"]["path"],
            self.log_file().display().to_string()
        );
        assert_eq!(paths["bind"]["value"], format!("127.0.0.1:{}", self.port));
        assert_eq!(paths["url"]["value"], self.url);
        paths
    }

    fn inspect_config(&self) -> Value {
        let output = self
            .command()
            .args(["config", "show", "--resolved"])
            .output()
            .expect("run config show");
        let config = success_json(output, &format!("{} config show", self.name));
        assert_eq!(
            config["config_file"],
            self.home.join("config.toml").display().to_string()
        );
        assert_eq!(config["runtime"]["home"], self.home.display().to_string());
        assert_eq!(
            config["runtime"]["db"],
            self.database().display().to_string()
        );
        assert_eq!(
            config["daemon"]["storage_path"],
            self.database().display().to_string()
        );
        config
    }

    fn start(&self) -> Value {
        let output = self.command().arg("up").output().expect("run up");
        if let Ok(pid) = fs::read_to_string(self.pid_file()) {
            if let Ok(pid) = pid.trim().parse::<u32>() {
                self.track_child(pid);
            }
        }
        let report = success_json(output, &format!("{} up", self.name));
        assert_eq!(report["process"], "running");
        assert_eq!(report["health"], "ok");
        assert_eq!(report["url"], self.url);
        assert_eq!(report["db"], self.database().display().to_string());
        assert_eq!(report["bind"], format!("127.0.0.1:{}", self.port));
        assert!(report["pid"].as_u64().is_some());
        assert!(self.pid_file().is_file());
        assert!(self.log_file().is_file());
        report
    }

    fn restart(&self) -> Value {
        let output = self.command().arg("restart").output().expect("run restart");
        if let Ok(pid) = fs::read_to_string(self.pid_file()) {
            if let Ok(pid) = pid.trim().parse::<u32>() {
                self.track_child(pid);
            }
        }
        let report = success_json(output, &format!("{} restart", self.name));
        assert_eq!(report["process"], "running");
        assert_eq!(report["health"], "ok");
        assert_eq!(report["db"], self.database().display().to_string());
        report
    }

    fn status(&self) -> Value {
        let output = self.command().arg("status").output().expect("run status");
        let status = success_json(output, &format!("{} status", self.name));
        status
    }

    fn conclude(&self, content: &str) -> Value {
        let output = self
            .command()
            .arg("--url")
            .arg(&self.url)
            .args([
                "conclude",
                "--profile",
                self.profile,
                "--workspace",
                self.workspace,
                "--content",
            ])
            .arg(content)
            .output()
            .expect("run conclude");
        let body = success_json(output, &format!("{} conclude", self.name));
        let response = data(&body);
        assert!(!response["created"]
            .as_array()
            .expect("created records")
            .is_empty());
        response.clone()
    }

    fn sync_local(&self, mode: &str, source: &Path) -> Value {
        let mut command = self.command();
        command
            .args([
                "sync-local",
                mode,
                "--profile",
                self.profile,
                "--workspace",
                self.workspace,
            ])
            .arg(source);
        let output = command.output().expect("run sync-local");
        let body = success_json(output, &format!("{} sync-local {mode}", self.name));
        let response = data(&body);
        response.clone()
    }

    fn recall(&self, query: &str) -> Value {
        let output = self
            .command()
            .args([
                "recall",
                "--profile",
                self.profile,
                "--workspace",
                self.workspace,
                "--query",
                query,
            ])
            .output()
            .expect("run recall");
        let body = success_json(output, &format!("{} recall", self.name));
        data(&body).clone()
    }

    fn mcp_read(&self, query: &str) -> Vec<Value> {
        let requests = [
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "clientInfo": {"name": "native-instance-acceptance", "version": "0.1.0"},
                    "capabilities": {}
                }
            }),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "memory_status", "arguments": {}}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "memory_recall",
                    "arguments": {
                        "profile": self.profile,
                        "workspace": self.workspace,
                        "query": query,
                        "maxTokens": 800
                    }
                }
            }),
        ];
        let stdin = requests
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let mut child = self
            .command()
            .args(["mcp", "stdio", "--read-only"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn MCP stdio");
        child
            .stdin
            .as_mut()
            .expect("MCP stdin")
            .write_all(format!("{stdin}\n").as_bytes())
            .expect("write MCP requests");
        drop(child.stdin.take());
        let output = child.wait_with_output().expect("wait for MCP stdio");
        let stdout = String::from_utf8(output.stdout).expect("MCP stdout is UTF-8");
        assert!(
            output.status.success(),
            "{} MCP stdio failed: stdout={stdout}\nstderr={}",
            self.name,
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
            .lines()
            .map(|line| serde_json::from_str(line).expect("MCP response is JSON"))
            .collect()
    }

    fn backup(&self, path: &Path) -> Value {
        let output = self
            .command()
            .args(["--local", "backup", "create", "--dest"])
            .arg(path)
            .output()
            .expect("run backup create");
        let backup = success_json(output, &format!("{} backup create", self.name));
        assert_eq!(backup["ok"], true);
        assert!(path.is_file());
        backup
    }

    fn verify_backup(&self, path: &Path) -> Value {
        let output = self
            .command()
            .args(["--local", "backup", "verify", "--path"])
            .arg(path)
            .output()
            .expect("run backup verify");
        let report = success_json(output, &format!("{} backup verify", self.name));
        assert_eq!(report["ok"], true);
        report
    }

    fn stop(&self) -> Value {
        let output = self.command().arg("down").output().expect("run down");
        let report = success_json(output, &format!("{} down", self.name));
        assert_eq!(report["process"], "stopped");
        assert!(!self.pid_file().exists());
        self.join_reapers();
        report
    }

    fn track_child(&self, pid: u32) {
        #[cfg(target_os = "linux")]
        self.reapers
            .lock()
            .expect("reaper lock")
            .push(reap_adopted_process(pid));
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
    }

    fn join_reapers(&self) {
        #[cfg(target_os = "linux")]
        for reaper in self.reapers.lock().expect("reaper lock").drain(..) {
            reaper.join().expect("reap detached daemon");
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.pid_file().exists() {
            let _ = self.command().arg("down").output();
        }
        self.join_reapers();
    }
}

#[cfg(target_os = "linux")]
struct Subreaper;

#[cfg(target_os = "linux")]
impl Subreaper {
    fn install() -> Self {
        let result = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
        assert_eq!(result, 0, "install Linux child subreaper");
        Self
    }
}

#[cfg(target_os = "linux")]
impl Drop for Subreaper {
    fn drop(&mut self) {
        let _ = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0) };
    }
}

#[cfg(target_os = "linux")]
fn reap_adopted_process(pid: u32) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
            if result == pid as libc::pid_t {
                return;
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ECHILD)
                || started.elapsed() > Duration::from_secs(10)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })
}

fn command_for_home(home: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin("codex-memoryd"));
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("CODEX_MEMORYD_")) {
        command.env_remove(key);
    }
    command.env("CODEX_MEMORYD_HOME", home);
    command
}

fn success_json(output: Output, label: &str) -> Value {
    assert!(
        output.status.success(),
        "{label} failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{label} did not return JSON: {error}"))
}

fn data(value: &Value) -> &Value {
    value.get("data").unwrap_or(value)
}

fn reserve_loopback_ports(count: usize) -> (Vec<u16>, Vec<TcpListener>) {
    let listeners = (0..count)
        .map(|_| TcpListener::bind(("127.0.0.1", 0)).expect("allocate loopback port"))
        .collect::<Vec<_>>();
    let ports = listeners
        .iter()
        .map(|listener| listener.local_addr().expect("listener address").port())
        .collect();
    (ports, listeners)
}

fn returned_by_id<'a>(responses: &'a [Value], id: u64) -> &'a Value {
    responses
        .iter()
        .find(|response| response["id"] == id)
        .unwrap_or_else(|| panic!("missing MCP response id {id}"))
}

#[test]
fn native_instances_have_independent_paths_lifecycle_import_and_mcp() {
    #[cfg(target_os = "linux")]
    let _subreaper = Subreaper::install();

    let sandbox = TempDir::new().expect("temporary acceptance sandbox");
    let (ports, port_reservations) = reserve_loopback_ports(2);
    let friday = Instance::new("synthetic-Friday", sandbox.path().join("Friday"), ports[0]);
    let codex = Instance::new("synthetic-Codex", sandbox.path().join("Codex"), ports[1]);

    friday.initialize();
    codex.initialize();
    friday.inspect_paths();
    friday.inspect_config();
    codex.inspect_paths();
    codex.inspect_config();
    assert_ne!(friday.database(), codex.database());
    assert_ne!(friday.pid_file(), codex.pid_file());
    assert_ne!(friday.log_file(), codex.log_file());
    assert_ne!(friday.port, codex.port);

    // Start both native daemons concurrently on distinct loopback ports.
    drop(port_reservations);
    std::thread::scope(|scope| {
        let friday_start = scope.spawn(|| friday.start());
        let codex_start = scope.spawn(|| codex.start());
        friday_start.join().expect("Friday startup thread");
        codex_start.join().expect("Codex startup thread");
    });

    friday.conclude(&format!(
        "The synthetic Friday store uses marker {FRIDAY_MARKER} for its isolated acceptance fixture."
    ));

    let synthetic_memories = sandbox.path().join("Codex/.codex/memories");
    fs::create_dir_all(&synthetic_memories).expect("create synthetic native memories");
    fs::write(
        synthetic_memories.join("memory_summary.md"),
        format!(
            "# Synthetic preference\n- The synthetic Codex store uses marker {CODEX_MARKER} for its isolated migration fixture.\n"
        ),
    )
    .expect("write synthetic Codex memory");

    let preview = codex.sync_local("--preview", &synthetic_memories);
    assert_eq!(preview["mode"], "preview");
    assert_eq!(preview["created"], 0);
    assert_eq!(preview["rejected"], 0);
    let applied = codex.sync_local("--apply", &synthetic_memories);
    assert_eq!(applied["mode"], "apply");
    assert!(applied["created"].as_u64().unwrap_or_default() > 0);
    let applied_again = codex.sync_local("--apply", &synthetic_memories);
    assert_eq!(applied_again["created"], 0);
    assert_eq!(applied_again["updated"], 0);

    let friday_recall = friday.recall(FRIDAY_MARKER);
    let codex_recall = codex.recall(CODEX_MARKER);
    assert!(friday_recall.to_string().contains(FRIDAY_MARKER));
    assert!(!friday_recall.to_string().contains(CODEX_MARKER));
    assert!(codex_recall.to_string().contains(CODEX_MARKER));
    assert!(!codex_recall.to_string().contains(FRIDAY_MARKER));

    for (instance, status) in [(&friday, friday.status()), (&codex, codex.status())] {
        assert_eq!(
            status["data"]["storage"]["path"],
            instance.database().display().to_string()
        );
        assert_eq!(status["data"]["status"], "local_only");
    }

    for (instance, marker) in [(&friday, FRIDAY_MARKER), (&codex, CODEX_MARKER)] {
        let responses = instance.mcp_read(marker);
        let listed = returned_by_id(&responses, 2);
        let tools = listed["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("MCP tool list response: {responses:?}"));
        let tool_names = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("MCP tool name"))
            .collect::<Vec<_>>();
        assert_eq!(
            tool_names,
            ["memory_status", "memory_recall", "memory_search"]
        );
        let status = &returned_by_id(&responses, 3)["result"]["structuredContent"];
        assert_eq!(
            status["storage"]["path"],
            instance.database().display().to_string()
        );
        let recall = &returned_by_id(&responses, 4)["result"]["structuredContent"];
        assert_eq!(recall["authority"], "recall_not_authority");
        assert!(recall.to_string().contains(marker));
        assert!(!recall.to_string().contains(if marker == FRIDAY_MARKER {
            CODEX_MARKER
        } else {
            FRIDAY_MARKER
        }));
        if marker == CODEX_MARKER {
            let imported = recall["facts"]
                .as_array()
                .expect("MCP recall facts")
                .iter()
                .find(|fact| {
                    fact["content"]
                        .as_str()
                        .is_some_and(|content| content.contains(CODEX_MARKER))
                })
                .expect("imported Codex fact");
            assert_eq!(
                imported["policy"]["provenance"]["origin"],
                "codex-local-memory"
            );
            assert!(!imported["policy"]["provenance"]["evidence_refs"]
                .as_array()
                .expect("evidence refs")
                .is_empty());
        }
    }

    // Restart Friday while Codex remains live; the independent daemon and DB
    // must continue answering through their own runtime home.
    friday.restart();
    assert!(friday
        .recall(FRIDAY_MARKER)
        .to_string()
        .contains(FRIDAY_MARKER));
    let codex_after_friday_restart = codex.status();
    assert_eq!(
        codex_after_friday_restart["data"]["storage"]["path"],
        codex.database().display().to_string()
    );
    assert!(codex
        .recall(CODEX_MARKER)
        .to_string()
        .contains(CODEX_MARKER));

    // A backup is a recovery point before reconfiguration or retry.
    codex.stop();
    let ledger = Store::open(&codex.database())
        .expect("open synthetic Codex database after stop")
        .list_evidence_ledger(PROFILE, WORKSPACE)
        .expect("read import provenance ledger");
    let imported_ledger_row = ledger
        .iter()
        .find(|row| row.source_path.as_deref() == Some("memory_summary.md"))
        .expect("imported memory ledger row");
    assert_eq!(imported_ledger_row.source_kind, "sync_local");
    assert_eq!(imported_ledger_row.policy_state, "accepted");
    assert!(imported_ledger_row.source_hash.starts_with("sha256:"));

    let backup_path = codex.home.join("backups/pre-recovery-memory.db");
    codex.backup(&backup_path);
    codex.verify_backup(&backup_path);
    let codex_db_before_failed_start =
        fs::read(codex.database()).expect("read stopped Codex store");

    // Stopping only Codex leaves Friday's status and recall intact.
    let codex_stopped = codex.status();
    assert_eq!(codex_stopped["process"], "stopped");
    assert_eq!(codex_stopped["db"], codex.database().display().to_string());
    assert_eq!(
        friday.status()["data"]["storage"]["path"],
        friday.database().display().to_string()
    );

    // A failed Codex bind must neither adopt Friday's live database nor write
    // a Codex PID file. Process overrides are used only to reproduce a port
    // collision; the saved Codex runtime.env remains pointed at its own port.
    let failed_start = command_for_home(&codex.home)
        .env("CODEX_MEMORYD_BIND", format!("127.0.0.1:{}", friday.port))
        .env("CODEX_MEMORYD_HOST", "127.0.0.1")
        .env("CODEX_MEMORYD_PORT", friday.port.to_string())
        .env("CODEX_MEMORYD_URL", &friday.url)
        .arg("up")
        .output()
        .expect("attempt conflicting Codex startup");
    assert!(
        !failed_start.status.success(),
        "port collision should fail startup"
    );
    assert!(String::from_utf8_lossy(&failed_start.stderr).contains("already in use"));
    assert!(!codex.pid_file().exists());
    assert_eq!(
        fs::read(codex.database()).expect("Codex DB after failed startup"),
        codex_db_before_failed_start
    );
    let codex_after_failure = codex.status();
    assert_eq!(codex_after_failure["process"], "stopped");
    assert_eq!(
        codex_after_failure["db"],
        codex.database().display().to_string()
    );
    assert_eq!(
        friday.status()["data"]["storage"]["path"],
        friday.database().display().to_string()
    );
    assert!(friday
        .recall(FRIDAY_MARKER)
        .to_string()
        .contains(FRIDAY_MARKER));

    // Reinitializing the same selected home with a free port reuses its DB;
    // startup and MCP recall then recover the imported fact after restart.
    let reinitialized = codex.initialize();
    let database_path = codex.database().to_string_lossy().into_owned();
    assert!(reinitialized["reused"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some(&database_path)));
    codex.start();
    assert!(codex
        .recall(CODEX_MARKER)
        .to_string()
        .contains(CODEX_MARKER));
    let responses = codex.mcp_read(CODEX_MARKER);
    let status = &returned_by_id(&responses, 3)["result"]["structuredContent"];
    assert_eq!(
        status["storage"]["path"],
        codex.database().display().to_string()
    );
    let recall = &returned_by_id(&responses, 4)["result"]["structuredContent"];
    assert!(recall.to_string().contains(CODEX_MARKER));
    assert_eq!(
        friday.status()["data"]["storage"]["path"],
        friday.database().display().to_string()
    );
    codex.stop();
    assert!(friday
        .recall(FRIDAY_MARKER)
        .to_string()
        .contains(FRIDAY_MARKER));
    friday.stop();
}
