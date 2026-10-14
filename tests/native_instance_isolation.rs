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
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use codex_memoryd::store::Store;
use serde_json::{json, Value};
use tempfile::TempDir;

const PROFILE: &str = "personal";
const WORKSPACE: &str = "dakota";
const FRIDAY_MARKER: &str = "amber-lantern";
const CODEX_MARKER: &str = "violet-otter";
#[cfg(target_os = "linux")]
static SUBREAPER_TEST_LOCK: Mutex<()> = Mutex::new(());

struct Instance {
    name: &'static str,
    home: PathBuf,
    port: u16,
    url: String,
    profile: &'static str,
    workspace: &'static str,
    #[cfg(target_os = "linux")]
    reapers: Mutex<Vec<ReaperHandle>>,
}

#[cfg(target_os = "linux")]
struct ReaperHandle {
    pid: u32,
    stopping: Arc<AtomicBool>,
    join: JoinHandle<Result<(), String>>,
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
            .args(["--bind", &format!("127.0.0.1:{}", self.port)])
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
        let old_pid = self.read_pid();
        if let Some(pid) = old_pid {
            self.request_reap(pid);
        }
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
        #[cfg(target_os = "linux")]
        if let Some(pid) = old_pid {
            self.finish_reaper(pid);
        }
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
        let pid = self.read_pid();
        if let Some(pid) = pid {
            self.request_reap(pid);
        }
        let output = self.command().arg("down").output().expect("run down");
        let report = success_json(output, &format!("{} down", self.name));
        assert_eq!(report["process"], "stopped");
        assert!(!self.pid_file().exists());
        #[cfg(target_os = "linux")]
        if let Some(pid) = pid {
            self.finish_reaper(pid);
        }
        report
    }

    fn read_pid(&self) -> Option<u32> {
        fs::read_to_string(self.pid_file())
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()
    }

    fn track_child(&self, pid: u32) {
        #[cfg(target_os = "linux")]
        {
            let mut reapers = self.reapers.lock().expect("reaper lock");
            if reapers.iter().any(|reaper| reaper.pid == pid) {
                return;
            }
            let stopping = Arc::new(AtomicBool::new(false));
            reapers.push(ReaperHandle {
                pid,
                join: reap_adopted_process(pid, stopping.clone()),
                stopping,
            });
        }
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
    }

    fn request_reap(&self, pid: u32) {
        #[cfg(target_os = "linux")]
        if let Some(reaper) = self
            .reapers
            .lock()
            .expect("reaper lock")
            .iter()
            .find(|reaper| reaper.pid == pid)
        {
            reaper.stopping.store(true, Ordering::Release);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
    }

    #[cfg(target_os = "linux")]
    fn finish_reaper(&self, pid: u32) {
        let reaper = {
            let mut reapers = self.reapers.lock().expect("reaper lock");
            reapers
                .iter()
                .position(|reaper| reaper.pid == pid)
                .map(|index| reapers.remove(index))
        };
        if let Some(reaper) = reaper {
            reaper
                .join
                .join()
                .expect("reaper thread panicked")
                .unwrap_or_else(|error| panic!("{error}"));
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.pid_file().exists() {
            if let Some(pid) = self.read_pid() {
                self.request_reap(pid);
            }
            let _ = self.command().arg("down").output();
        }
        #[cfg(target_os = "linux")]
        for reaper in self.reapers.get_mut().expect("reaper lock").drain(..) {
            reaper.stopping.store(true, Ordering::Release);
            let _ = reaper.join.join();
        }
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
fn reap_adopted_process(pid: u32, stopping: Arc<AtomicBool>) -> JoinHandle<Result<(), String>> {
    std::thread::spawn(move || {
        let mut stopping_since = None;
        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
            if result == pid as libc::pid_t {
                return Ok(());
            }
            let is_stopping = stopping.load(Ordering::Acquire);
            if is_stopping && stopping_since.is_none() {
                stopping_since = Some(Instant::now());
            }
            let timeout = || {
                stopping_since.is_some_and(|started| started.elapsed() > Duration::from_secs(10))
            };
            if result == 0 {
                if timeout() {
                    return Err(format!("timed out reaping detached daemon pid {pid}"));
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                let process_exists = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
                    || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
                if !process_exists {
                    return Ok(());
                }
                if timeout() {
                    return Err(format!(
                        "timed out waiting for detached daemon pid {pid} to become reapable"
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(format!("waitpid failed for daemon pid {pid}: {error}"));
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

fn database_snapshot(database: &Path, selected_tables: Option<&[&str]>) -> Value {
    use rusqlite::types::ValueRef;

    let connection = rusqlite::Connection::open(database).expect("open SQLite snapshot");
    let mut table_statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("list SQLite tables");
    let table_names = table_statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query SQLite tables")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect SQLite table names");
    let mut tables = serde_json::Map::new();
    for table in table_names {
        if selected_tables.is_some_and(|selected| !selected.contains(&table.as_str())) {
            continue;
        }
        let escaped_name = table.replace('"', "\"\"");
        let sql = format!("SELECT * FROM \"{escaped_name}\"");
        let mut statement = connection.prepare(&sql).expect("prepare table snapshot");
        let column_count = statement.column_count();
        let mut rows = statement.query([]).expect("query table snapshot rows");
        let mut snapshot_rows = Vec::new();
        while let Some(row) = rows.next().expect("read table snapshot row") {
            let mut values = Vec::with_capacity(column_count);
            for index in 0..column_count {
                let value = match row.get_ref(index).expect("read snapshot value") {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => json!(value),
                    ValueRef::Text(value) => {
                        Value::String(String::from_utf8_lossy(value).into_owned())
                    }
                    ValueRef::Blob(value) => json!(value),
                };
                values.push(value);
            }
            snapshot_rows.push(Value::Array(values));
        }
        snapshot_rows.sort_by_key(Value::to_string);
        tables.insert(table, Value::Array(snapshot_rows));
    }
    Value::Object(tables)
}

fn documented_md_wrapper() -> &'static str {
    const RUNBOOK: &str = include_str!("../docs/dakota-native-codex-instance.md");
    let start = RUNBOOK.find("md() {\n").expect("runbook md wrapper start");
    let end = RUNBOOK[start..]
        .find("\n}\n```")
        .map(|offset| start + offset + 2)
        .expect("runbook md wrapper end");
    &RUNBOOK[start..end]
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

fn documented_port_check_commands() -> Vec<&'static str> {
    const RUNBOOK: &str = include_str!("../docs/dakota-native-codex-instance.md");
    RUNBOOK
        .lines()
        .filter(|line| line.starts_with("python3 -c ") && line.contains("sys.argv[1]"))
        .collect()
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
    let _subreaper_test_lock = SUBREAPER_TEST_LOCK.lock().expect("subreaper test lock");
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

    let store_before_preview = database_snapshot(&codex.database(), None);
    let preview = codex.sync_local("--preview", &synthetic_memories);
    assert_eq!(preview["mode"], "preview");
    assert_eq!(preview["created"], 0);
    assert_eq!(preview["rejected"], 0);
    assert_eq!(
        database_snapshot(&codex.database(), None),
        store_before_preview,
        "preview must leave records and provenance unchanged"
    );
    let applied = codex.sync_local("--apply", &synthetic_memories);
    assert_eq!(applied["mode"], "apply");
    assert!(applied["created"].as_u64().unwrap_or_default() > 0);
    let store_after_apply = database_snapshot(
        &codex.database(),
        Some(&["memory_records", "memory_sources", "evidence_ledger"]),
    );
    let applied_again = codex.sync_local("--apply", &synthetic_memories);
    assert_eq!(applied_again["created"], 0);
    assert_eq!(applied_again["updated"], 0);
    assert_eq!(
        database_snapshot(
            &codex.database(),
            Some(&["memory_records", "memory_sources", "evidence_ledger"]),
        ),
        store_after_apply,
        "repeating apply must not change records or duplicate provenance"
    );

    let friday_recall = friday.recall(FRIDAY_MARKER);
    let codex_recall = codex.recall(CODEX_MARKER);
    assert!(friday_recall.to_string().contains(FRIDAY_MARKER));
    assert!(!friday_recall.to_string().contains(CODEX_MARKER));
    assert!(codex_recall.to_string().contains(CODEX_MARKER));
    assert!(!codex_recall.to_string().contains(FRIDAY_MARKER));
    let friday_cross_recall = friday.recall(CODEX_MARKER);
    let codex_cross_recall = codex.recall(FRIDAY_MARKER);
    assert!(!friday_cross_recall.to_string().contains(CODEX_MARKER));
    assert!(!codex_cross_recall.to_string().contains(FRIDAY_MARKER));

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
    for (instance, foreign_marker) in [(&friday, CODEX_MARKER), (&codex, FRIDAY_MARKER)] {
        let responses = instance.mcp_read(foreign_marker);
        let recall = &returned_by_id(&responses, 4)["result"]["structuredContent"];
        assert!(!recall.to_string().contains(foreign_marker));
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

    // Reinitializing the same selected home with a new port reuses its DB;
    // the previous port remains occupied while the new endpoint starts.
    let original_port_reservation =
        TcpListener::bind(("127.0.0.1", codex.port)).expect("reserve stopped Codex port");
    let (new_ports, new_port_reservations) = reserve_loopback_ports(1);
    let new_port = new_ports[0];
    // Exercise CLI endpoint override precedence against the stale bind in the
    // existing runtime.env. The runbook smoke test separately verifies its
    // explicit matching --bind command.
    let reinitialize_without_bind = command_for_home(&codex.home)
        .args(["init", "--host", "127.0.0.1", "--port"])
        .arg(new_port.to_string())
        .output()
        .expect("reinitialize selected home on a new port");
    let reinitialized = success_json(reinitialize_without_bind, "Codex changed-port init");
    let recovered_codex = Instance::new("synthetic-Codex-recovered", codex.home.clone(), new_port);
    let paths = recovered_codex.inspect_paths();
    assert_ne!(recovered_codex.port, codex.port);
    assert!(TcpListener::bind(("127.0.0.1", codex.port)).is_err());
    assert_eq!(paths["bind"]["value"], format!("127.0.0.1:{new_port}"));
    assert_eq!(paths["url"]["value"], recovered_codex.url);
    let database_path = codex.database().to_string_lossy().into_owned();
    assert!(reinitialized["reused"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some(&database_path)));
    drop(new_port_reservations);
    recovered_codex.start();
    assert!(recovered_codex
        .recall(CODEX_MARKER)
        .to_string()
        .contains(CODEX_MARKER));
    let responses = recovered_codex.mcp_read(CODEX_MARKER);
    let status = &returned_by_id(&responses, 3)["result"]["structuredContent"];
    assert_eq!(
        status["storage"]["path"],
        recovered_codex.database().display().to_string()
    );
    let recall = &returned_by_id(&responses, 4)["result"]["structuredContent"];
    assert!(recall.to_string().contains(CODEX_MARKER));
    assert_eq!(
        friday.status()["data"]["storage"]["path"],
        friday.database().display().to_string()
    );
    recovered_codex.stop();
    assert!(friday
        .recall(FRIDAY_MARKER)
        .to_string()
        .contains(FRIDAY_MARKER));
    friday.stop();
    drop(original_port_reservation);
}

#[test]
fn documented_dakota_commands_isolate_hostile_inherited_environment() {
    #[cfg(target_os = "linux")]
    let _subreaper_test_lock = SUBREAPER_TEST_LOCK.lock().expect("subreaper test lock");
    #[cfg(target_os = "linux")]
    let _subreaper = Subreaper::install();

    let sandbox = TempDir::new().expect("temporary runbook sandbox");
    let operator_home = sandbox.path().join("operator-home");
    let memory_home = operator_home.join(".codex-memoryd-codex");
    let memories = operator_home.join(".codex/memories");
    fs::create_dir_all(&memories).expect("create synthetic runbook memories");
    fs::write(
        memories.join("memory_summary.md"),
        format!("# Synthetic Dakota memory\n- Runbook fixture marker: {CODEX_MARKER}.\n"),
    )
    .expect("write synthetic runbook memory");

    let (ports, reservations) = reserve_loopback_ports(1);
    let port = ports[0];
    drop(reservations);

    let binary = assert_cmd::cargo::cargo_bin("codex-memoryd");
    let binary_dir = binary.parent().expect("binary parent");
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = format!(
        "{}:{}",
        binary_dir.display(),
        inherited_path.to_string_lossy()
    );
    let script = format!(
        r#"set -euo pipefail
MEMORYD_CODEX_HOME="$HOME/.codex-memoryd-codex"
{}
trap 'md down >/dev/null 2>&1 || true' EXIT
MEMORYD_CODEX_PORT={port}
md init --host 127.0.0.1 --port "$MEMORYD_CODEX_PORT" \
  --bind "127.0.0.1:$MEMORYD_CODEX_PORT"
md paths --format json > "$MEMORYD_CODEX_HOME/paths.json"
md config show --resolved > "$MEMORYD_CODEX_HOME/config-resolved.json"
md up > "$MEMORYD_CODEX_HOME/up.json"
md status | jq -e --arg db "$MEMORYD_CODEX_HOME/memory.db" \
  '{{status: .data.status, storage_path: .data.storage.path}} | select(.status == "local_only" and .storage_path == $db)' \
  > "$MEMORYD_CODEX_HOME/status.json"
md sync-local --preview --profile personal --workspace dakota "$HOME/.codex/memories" \
  | tee "$MEMORYD_CODEX_HOME/sync-local-preview.json"
md sync-local --apply --profile personal --workspace dakota "$HOME/.codex/memories" \
  | tee "$MEMORYD_CODEX_HOME/sync-local-apply.json"
md sync-local --apply --profile personal --workspace dakota "$HOME/.codex/memories" \
  | tee "$MEMORYD_CODEX_HOME/sync-local-apply-second.json"
md recall --profile personal --workspace dakota \
  --query "What do my native Codex memories say about this project? {CODEX_MARKER}" \
  --max-tokens 1200 \
  | jq '.data // . | {{authority, facts: .facts[0:3], citations}}' \
  > "$MEMORYD_CODEX_HOME/recall.json"
printf '%s\n' \
  '{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-11-25","clientInfo":{{"name":"dakota-check","version":"1"}},"capabilities":{{}}}}}}' \
  '{{"jsonrpc":"2.0","method":"notifications/initialized"}}' \
  '{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}' \
  '{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"memory_status","arguments":{{}}}}}}' \
  '{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"memory_recall","arguments":{{"profile":"personal","workspace":"dakota","query":"{CODEX_MARKER}","maxTokens":1200}}}}}}' \
  | md mcp stdio --read-only \
  | jq -c 'select(.id == 2 or .id == 3 or .id == 4)' \
  > "$MEMORYD_CODEX_HOME/mcp.jsonl"
md restart > "$MEMORYD_CODEX_HOME/restart.json"
md status > "$MEMORYD_CODEX_HOME/status-after-restart.json"
md down > "$MEMORYD_CODEX_HOME/down.json"
"#,
        documented_md_wrapper()
    );

    let mut child = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("HOME", &operator_home)
        .env("PATH", path)
        .env("TMPDIR", sandbox.path())
        .env(
            "CODEX_MEMORYD_HOME",
            sandbox.path().join("Friday-hostile-env"),
        )
        .env(
            "CODEX_MEMORYD_DB",
            sandbox.path().join("Friday-hostile-env/friday.db"),
        )
        .env("CODEX_MEMORYD_URL", "http://127.0.0.1:1")
        .env("CODEX_MEMORYD_BIND", "127.0.0.1:1")
        .env("CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED", "true")
        .env("CODEX_MEMORYD_DREAM_SCHEDULED_PROVIDER_ENABLED", "true")
        .env("CODEX_MEMORYD_PROVIDER_ENABLED", "true")
        .env("CODEX_MEMORYD_PROVIDER_ADAPTER", "not-a-real-adapter")
        .env(
            "CODEX_MEMORYD_PROVIDER_ENDPOINT",
            "https://provider.invalid/v1",
        )
        .env("CODEX_MEMORYD_PROVIDER_API_KEY", "synthetic-not-a-secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run literal Dakota commands with hostile inherited environment");
    #[cfg(target_os = "linux")]
    let pid_file = memory_home.join("codex-memoryd.pid");
    let deadline = Instant::now() + Duration::from_secs(30);
    #[cfg(target_os = "linux")]
    let mut reapers = Vec::new();
    #[cfg(target_os = "linux")]
    let mut tracked_pids = Vec::new();
    loop {
        #[cfg(target_os = "linux")]
        if pid_file.exists() {
            let pid = fs::read_to_string(&pid_file)
                .expect("read runbook daemon pid")
                .trim()
                .parse::<u32>()
                .expect("parse runbook daemon pid");
            if !tracked_pids.contains(&pid) {
                tracked_pids.push(pid);
                let stopping = Arc::new(AtomicBool::new(false));
                let join = reap_adopted_process(pid, stopping.clone());
                reapers.push((stopping, join));
            }
        }
        if child
            .try_wait()
            .expect("check runbook command status")
            .is_some()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "runbook command did not start daemon"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child
        .wait_with_output()
        .expect("wait for literal runbook commands");
    #[cfg(target_os = "linux")]
    for (stopping, reaper) in reapers {
        stopping.store(true, Ordering::Release);
        reaper
            .join()
            .expect("runbook reaper thread panicked")
            .expect("reap runbook daemon");
    }
    assert!(
        output.status.success(),
        "documented wrapper commands failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let config: Value = serde_json::from_slice(
        &fs::read(memory_home.join("config-resolved.json")).expect("resolved runbook config"),
    )
    .expect("resolved runbook config JSON");
    assert_eq!(config["runtime"]["home"], memory_home.display().to_string());
    assert_eq!(config["runtime"]["workspace"], "dakota");
    assert_eq!(
        config["daemon"]["storage_path"],
        memory_home.join("memory.db").display().to_string()
    );
    assert_eq!(config["dream"]["scheduler_enabled"], false);
    let scheduler = config["registry"]
        .as_array()
        .expect("config registry")
        .iter()
        .find(|entry| entry["key"] == "CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED")
        .expect("scheduler registry entry");
    assert_eq!(scheduler["value"], false);

    let preview: Value = serde_json::from_slice(
        &fs::read(memory_home.join("sync-local-preview.json")).expect("preview output"),
    )
    .expect("preview JSON");
    let applied: Value = serde_json::from_slice(
        &fs::read(memory_home.join("sync-local-apply.json")).expect("apply output"),
    )
    .expect("apply JSON");
    let applied_again: Value = serde_json::from_slice(
        &fs::read(memory_home.join("sync-local-apply-second.json")).expect("repeat apply output"),
    )
    .expect("repeat apply JSON");
    assert_eq!(data(&preview)["created"], 0);
    assert!(data(&applied)["created"].as_u64().unwrap_or_default() > 0);
    assert_eq!(data(&applied_again)["created"], 0);
    assert_eq!(data(&applied_again)["updated"], 0);

    let recall: Value = serde_json::from_slice(
        &fs::read(memory_home.join("recall.json")).expect("runbook recall output"),
    )
    .expect("runbook jq recall projection");
    assert_eq!(recall["authority"], "recall_not_authority");
    assert!(recall.to_string().contains(CODEX_MARKER));
    let mcp = fs::read_to_string(memory_home.join("mcp.jsonl")).expect("runbook MCP output");
    assert!(mcp.contains("memory_status"));
    assert!(mcp.contains("memory_recall"));
    assert!(mcp.contains(&memory_home.join("memory.db").display().to_string()));
    let imported_ledger = Store::open(&memory_home.join("memory.db"))
        .expect("inspect runbook store")
        .list_evidence_ledger(PROFILE, WORKSPACE)
        .expect("inspect runbook provenance");
    assert!(imported_ledger.iter().any(|row| {
        row.source_path.as_deref() == Some("memory_summary.md")
            && row.source_kind == "sync_local"
            && row.policy_state == "accepted"
    }));
}

#[test]
fn documented_port_checks_use_selected_port_for_initialization_and_recovery() {
    let commands = documented_port_check_commands();
    assert_eq!(
        commands.len(),
        2,
        "expected initial and recovery port probes"
    );
    assert!(commands
        .iter()
        .all(|command| command.contains("$MEMORYD_CODEX_PORT")));

    for command in commands {
        let (ports, reservations) = reserve_loopback_ports(1);
        let port = ports[0];
        let script = format!("MEMORYD_CODEX_PORT={port}\n{command}");
        let occupied = Command::new("bash")
            .args(["-c", &script])
            .output()
            .expect("run port probe while port is occupied");
        assert!(
            !occupied.status.success(),
            "port probe should reject an occupied selected port {port}"
        );

        drop(reservations);
        let available = Command::new("bash")
            .args(["-c", &script])
            .output()
            .expect("run port probe after port is released");
        assert!(
            available.status.success(),
            "port probe should accept available selected port {port}: {}",
            String::from_utf8_lossy(&available.stderr)
        );
        assert!(String::from_utf8_lossy(&available.stdout).contains(&port.to_string()));
    }
}

#[test]
fn init_partial_endpoint_overrides_preserve_inherited_bind_component() {
    let sandbox = TempDir::new().expect("temporary endpoint resolution sandbox");
    let port_home = sandbox.path().join("port-override");
    let port_override = command_for_home(&port_home)
        .env("CODEX_MEMORYD_BIND", "127.0.0.2:9123")
        .args(["init", "--port", "9124"])
        .output()
        .expect("initialize with inherited bind and port override");
    success_json(port_override, "init with inherited bind and port override");
    let port_paths = success_json(
        command_for_home(&port_home)
            .args(["paths", "--format", "json"])
            .output()
            .expect("inspect port-override paths"),
        "paths after port override",
    );
    assert_eq!(port_paths["bind"]["value"], "127.0.0.2:9124");
    assert_eq!(port_paths["url"]["value"], "http://127.0.0.2:9124");

    let host_home = sandbox.path().join("host-override");
    let host_override = command_for_home(&host_home)
        .env("CODEX_MEMORYD_BIND", "127.0.0.2:9123")
        .args(["init", "--host", "127.0.0.3"])
        .output()
        .expect("initialize with inherited bind and host override");
    success_json(host_override, "init with inherited bind and host override");
    let host_paths = success_json(
        command_for_home(&host_home)
            .args(["paths", "--format", "json"])
            .output()
            .expect("inspect host-override paths"),
        "paths after host override",
    );
    assert_eq!(host_paths["bind"]["value"], "127.0.0.3:9123");
    assert_eq!(host_paths["url"]["value"], "http://127.0.0.3:9123");
}
