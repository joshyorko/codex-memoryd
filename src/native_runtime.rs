//! Productized local runtime management for the native binary mode.

mod native_process;

use std::fs;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use serde::Serialize;

use crate::config;
use crate::error::{Error, Result};
use crate::store::{Store, STORAGE_SCHEMA_VERSION};

pub const DEFAULT_URL: &str = "http://127.0.0.1:8787";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RuntimeKind {
    Native,
    Container,
    Auto,
    ComposeDev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSelectionSource {
    /// An explicit runtime argument supplied by the caller.
    Cli,
    /// A process environment runtime selection.
    Environment,
    /// A runtime selection discovered from the selected runtime home.
    RuntimeEnv,
    /// No runtime selection; use the native/default endpoint behavior.
    Default,
}

impl RuntimeSelectionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Environment => "env",
            Self::RuntimeEnv => "runtime.env",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeResolution {
    pub options: RuntimeOptions,
    pub runtime_source: RuntimeSelectionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitMode {
    Product,
    Dogfood,
}

#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    pub runtime: RuntimeKind,
    pub home: PathBuf,
    pub url: String,
    pub host: String,
    pub port: u16,
    pub bind: String,
    pub db: PathBuf,
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
    pub profile: String,
    pub workspace: String,
    pub log_level: String,
    pub binary: PathBuf,
    pub allow_non_loopback: bool,
    pub image: String,
    pub container_name: String,
    pub container_runtime: Option<String>,
    pub codex_memories_dir: PathBuf,
    pub uid: Option<String>,
    pub gid: Option<String>,
    /// Whether the resolved endpoint was explicitly selected or configured.
    /// A derived default endpoint must not be used as managed-runtime health
    /// evidence because it may belong to an unrelated daemon.
    endpoint_configured: bool,
}

#[derive(Debug, Serialize)]
pub struct RuntimeInitReport {
    pub runtime: String,
    pub home: String,
    pub db: String,
    pub bind: String,
    pub url: String,
    pub host: String,
    pub port: u16,
    pub image: String,
    pub created: Vec<String>,
    pub reused: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
    pub message: String,
    pub next_command: String,
    pub container_note: Option<String>,
    pub local_image_command: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RuntimeStatusReport {
    pub runtime: String,
    pub process: String,
    pub pid: Option<u32>,
    pub url: String,
    pub bind: String,
    pub db: String,
    pub log_file: String,
    pub health: String,
}

impl RuntimeOptions {
    pub fn resolve(
        runtime: Option<RuntimeKind>,
        url: Option<String>,
        db_override: Option<PathBuf>,
        host_override: Option<String>,
        port_override: Option<u16>,
        bind_override: Option<String>,
    ) -> RuntimeOptions {
        Self::resolve_with_source(
            runtime,
            url,
            db_override,
            host_override,
            port_override,
            bind_override,
        )
        .options
    }

    pub fn resolve_with_source(
        runtime: Option<RuntimeKind>,
        url: Option<String>,
        db_override: Option<PathBuf>,
        host_override: Option<String>,
        port_override: Option<u16>,
        bind_override: Option<String>,
    ) -> RuntimeResolution {
        let home = env_path("CODEX_MEMORYD_HOME")
            .or_else(|| env_path("CODEX_MEMORYD_RUNTIME_DIR"))
            .unwrap_or_else(config::default_home_dir);
        let runtime_env = read_runtime_env(&home.join("runtime.env"));
        let endpoint_overridden =
            host_override.is_some() || port_override.is_some() || bind_override.is_some();
        let endpoint_address_overridden = host_override.is_some() || port_override.is_some();
        let (runtime, runtime_source) = resolve_runtime(runtime, &runtime_env);
        let process_bind = env_value("CODEX_MEMORYD_BIND");
        let runtime_env_bind = runtime_env_value(&runtime_env, "CODEX_MEMORYD_BIND");
        let process_host = env_value("CODEX_MEMORYD_HOST");
        let runtime_env_host = runtime_env_value(&runtime_env, "CODEX_MEMORYD_HOST");
        let process_port = env_value("CODEX_MEMORYD_PORT");
        let runtime_env_port = runtime_env_value(&runtime_env, "CODEX_MEMORYD_PORT");
        let process_url =
            env_value("CODEX_MEMORYD_URL").or_else(|| env_value("CODEX_MEMORYD_BASE_URL"));
        let runtime_env_url = runtime_env_value(&runtime_env, "CODEX_MEMORYD_URL")
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_BASE_URL"));

        // Keep an inherited bind available while resolving a partial host or
        // port override. It supplies the unspecified component; the final
        // bind is rebuilt below when either address component was overridden.
        let bind_candidate = bind_override
            .clone()
            .or_else(|| process_bind.clone())
            .or_else(|| runtime_env_bind.clone());
        let parsed_bind = bind_candidate.as_deref().and_then(parse_bind_host_port);

        let host = host_override
            .or_else(|| process_host.clone())
            .or_else(|| runtime_env_host.clone())
            .or_else(|| parsed_bind.as_ref().map(|(host, _)| host.clone()))
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let port = port_override
            .or_else(|| {
                process_port
                    .as_deref()
                    .and_then(|v| v.trim().parse::<u16>().ok())
            })
            .or_else(|| {
                runtime_env_port
                    .as_deref()
                    .and_then(|v| v.trim().parse::<u16>().ok())
            })
            .or_else(|| parsed_bind.as_ref().map(|(_, port)| *port))
            .unwrap_or(8787);
        let bind = if endpoint_address_overridden && bind_override.is_none() {
            format!("{host}:{port}")
        } else {
            bind_candidate.unwrap_or_else(|| format!("{host}:{port}"))
        };
        let db = db_override
            .or_else(|| env_path("CODEX_MEMORYD_DB"))
            .or_else(|| runtime_env_path(&runtime_env, "CODEX_MEMORYD_DB"))
            .unwrap_or_else(|| home.join("memory.db"));
        let pid_file = env_path("CODEX_MEMORYD_PID_FILE")
            .or_else(|| runtime_env_path(&runtime_env, "CODEX_MEMORYD_PID_FILE"))
            .unwrap_or_else(|| home.join("codex-memoryd.pid"));
        let log_file = env_path("CODEX_MEMORYD_LOG_FILE")
            .or_else(|| runtime_env_path(&runtime_env, "CODEX_MEMORYD_LOG_FILE"))
            .unwrap_or_else(|| home.join("logs").join("codex-memoryd.log"));
        let profile = std::env::var("CODEX_MEMORYD_PROFILE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_PROFILE"))
            .unwrap_or_else(|| "personal".to_string());
        let workspace = std::env::var("CODEX_MEMORYD_WORKSPACE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_WORKSPACE"))
            .unwrap_or_else(|| "default".to_string());
        let log_level = std::env::var("CODEX_MEMORYD_LOG")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_LOG"))
            .unwrap_or_else(|| "info".to_string());
        let binary = env_path("CODEX_MEMORYD_BIN")
            .or_else(|| runtime_env_path(&runtime_env, "CODEX_MEMORYD_BIN"))
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| PathBuf::from("codex-memoryd"));
        let allow_non_loopback = std::env::var("CODEX_MEMORYD_ALLOW_NON_LOOPBACK")
            .ok()
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_ALLOW_NON_LOOPBACK"))
            .is_some_and(|v| {
                matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
            });
        let image = std::env::var("CODEX_MEMORYD_IMAGE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_IMAGE"))
            .unwrap_or_else(|| "ghcr.io/joshyorko/codex-memoryd:latest".to_string());
        let container_name = std::env::var("CODEX_MEMORYD_CONTAINER_NAME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_CONTAINER_NAME"))
            .unwrap_or_else(|| "codex-memoryd".to_string());
        let container_runtime = std::env::var("CODEX_MEMORYD_CONTAINER_RUNTIME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_CONTAINER_RUNTIME"));
        let codex_memories_dir = env_path("CODEX_MEMORYD_CODEX_MEMORIES_DIR")
            .or_else(|| runtime_env_path(&runtime_env, "CODEX_MEMORYD_CODEX_MEMORIES_DIR"))
            .unwrap_or_else(|| config::expand_path("~/.codex/memories"));
        let uid = std::env::var("CODEX_MEMORYD_UID")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_UID"))
            .or_else(|| command_output("id", &["-u"]));
        let gid = std::env::var("CODEX_MEMORYD_GID")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| runtime_env_value(&runtime_env, "CODEX_MEMORYD_GID"))
            .or_else(|| command_output("id", &["-g"]));
        // An explicit URL is the client endpoint override. Without one, an
        // explicit runtime wins over process URL variables so a lower-
        // precedence client setting cannot redirect managed-runtime status;
        // the selected runtime.env URL remains available for that runtime.
        let bind_configured = endpoint_overridden
            || process_bind.is_some()
            || runtime_env_bind.is_some()
            || process_host.is_some()
            || runtime_env_host.is_some()
            || process_port.is_some()
            || runtime_env_port.is_some();
        let (url, endpoint_configured) = if let Some(url) = url.filter(|v| !v.trim().is_empty()) {
            (url, true)
        } else if endpoint_overridden {
            (format!("http://{host}:{port}"), true)
        } else if runtime_source == RuntimeSelectionSource::Cli {
            match runtime_env_url {
                Some(url) => (url, true),
                None => (format!("http://{host}:{port}"), bind_configured),
            }
        } else {
            match process_url.or(runtime_env_url) {
                Some(url) => (url, true),
                None => (format!("http://{host}:{port}"), bind_configured),
            }
        };

        RuntimeResolution {
            options: RuntimeOptions {
                runtime,
                home,
                url,
                host,
                port,
                bind,
                db,
                pid_file,
                log_file,
                profile,
                workspace,
                log_level,
                binary,
                allow_non_loopback,
                image,
                container_name,
                container_runtime,
                codex_memories_dir,
                uid,
                gid,
                endpoint_configured,
            },
            runtime_source,
        }
    }
}

pub fn init(opts: &RuntimeOptions, mode: InitMode) -> Result<RuntimeInitReport> {
    let mut opts = opts.clone();
    if mode == InitMode::Dogfood {
        opts.home = PathBuf::from(".dogfood");
        opts.db = opts.home.join("memory.db");
        opts.pid_file = opts.home.join("codex-memoryd.pid");
        opts.log_file = opts.home.join("logs").join("codex-memoryd.log");
        opts.workspace = "josh-personal".to_string();
    }

    let mut created = Vec::new();
    let mut reused = Vec::new();
    let mut updated = Vec::new();
    let skipped = Vec::new();

    ensure_dir(&opts.home, &mut created, &mut reused)?;
    for dir in [
        opts.home.join("backups"),
        opts.home.join("exports"),
        opts.home.join("logs"),
    ] {
        ensure_dir(&dir, &mut created, &mut reused)?;
    }

    // Persist one stable database identity so a later `up` from another
    // working directory cannot silently select a different relative path.
    fs::create_dir_all(
        opts.db
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )
    .map_err(Error::from)?;
    opts.db = resolve_database_path(&opts.db)?;

    let config_path = opts.home.join("config.toml");
    write_or_update(
        &config_path,
        &render_config(&opts),
        &mut created,
        &mut reused,
        &mut updated,
    )?;

    let env_path = opts.home.join("runtime.env");
    write_or_update(
        &env_path,
        &render_runtime_env(&opts),
        &mut created,
        &mut reused,
        &mut updated,
    )?;

    if opts.db.exists() {
        reused.push(opts.db.display().to_string());
    } else {
        Store::open(&opts.db)?;
        created.push(opts.db.display().to_string());
    }

    Ok(RuntimeInitReport {
        runtime: runtime_name(opts.runtime).to_string(),
        home: opts.home.display().to_string(),
        db: opts.db.display().to_string(),
        bind: opts.bind.clone(),
        url: opts.url.clone(),
        host: opts.host.clone(),
        port: opts.port,
        image: opts.image.clone(),
        created,
        reused,
        updated,
        skipped,
        message: "init seeds config/runtime only; next command: codex-memoryd up"
            .to_string(),
        next_command: "codex-memoryd up".to_string(),
        container_note: (opts.runtime == RuntimeKind::Container).then(|| {
            format!(
                "container mode pulls/runs {}; override CODEX_MEMORYD_IMAGE or build a local image first",
                opts.image
            )
        }),
        local_image_command: (opts.runtime == RuntimeKind::Container)
            .then(|| "codex-memoryd image build --tag codex-memoryd:local".to_string()),
    })
}

pub fn up(opts: &RuntimeOptions) -> Result<RuntimeStatusReport> {
    match opts.runtime {
        RuntimeKind::Native | RuntimeKind::Auto => up_native(opts),
        RuntimeKind::Container => up_container(opts),
        RuntimeKind::ComposeDev => Err(Error::invalid_request(
            "compose-dev is a development path; use docker compose or scripts directly",
        )),
    }
}

pub fn down(opts: &RuntimeOptions) -> Result<RuntimeStatusReport> {
    match opts.runtime {
        RuntimeKind::Native | RuntimeKind::Auto => down_native(opts),
        RuntimeKind::Container => down_container(opts),
        RuntimeKind::ComposeDev => Err(Error::invalid_request(
            "compose-dev is a development path; use docker compose or scripts directly",
        )),
    }
}

pub fn restart(opts: &RuntimeOptions) -> Result<RuntimeStatusReport> {
    let _ = down(opts)?;
    up(opts)
}

pub fn upgrade(opts: &RuntimeOptions) -> Result<serde_json::Value> {
    match opts.runtime {
        RuntimeKind::Native | RuntimeKind::Auto => Ok(serde_json::json!({
            "runtime": runtime_name(opts.runtime),
            "status": "noop",
            "message": "native runtime upgrades by replacing the installed codex-memoryd binary",
        })),
        RuntimeKind::Container => {
            let runtime = container_runtime(opts)?;
            run_status(&runtime, &["pull", &opts.image])?;
            let state = restart(opts)?;
            Ok(serde_json::json!({
                "runtime": "container",
                "image": opts.image,
                "status": "upgraded",
                "state": state,
            }))
        }
        RuntimeKind::ComposeDev => Err(Error::invalid_request(
            "compose-dev upgrades are handled by docker compose pull/build",
        )),
    }
}

pub fn status(opts: &RuntimeOptions) -> RuntimeStatusReport {
    if opts.runtime == RuntimeKind::Container {
        return status_container(opts);
    }
    let (process, pid) = native_process_state(opts);
    RuntimeStatusReport {
        runtime: runtime_name(opts.runtime).to_string(),
        process: process.to_string(),
        pid,
        url: opts.url.clone(),
        bind: opts.bind.clone(),
        db: opts.db.display().to_string(),
        log_file: opts.log_file.display().to_string(),
        health: if http_get(&format!("{}/healthz", opts.url.trim_end_matches('/'))).is_ok() {
            "ok".to_string()
        } else {
            "unreachable".to_string()
        },
    }
}

pub fn native_process_unverified(opts: &RuntimeOptions) -> bool {
    native_process_state(opts).0 == "unverified"
}

/// Clear the launch identity before the managed server starts subprocesses.
pub fn acknowledge_native_startup() -> Result<()> {
    native_process::acknowledge_startup().map_err(Error::from)
}

fn native_process_state(opts: &RuntimeOptions) -> (&'static str, Option<u32>) {
    match native_process::Paths::new(&opts.pid_file) {
        Ok(paths) => match native_process::inspect(&paths) {
            native_process::Inspection::Running(process) => ("running", Some(process.pid())),
            native_process::Inspection::Stopped => ("stopped", None),
            native_process::Inspection::Unverified(_) => ("unverified", None),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ("stopped", None),
        Err(_) => ("unverified", None),
    }
}

pub fn logs(opts: &RuntimeOptions, lines: usize) -> Result<String> {
    if opts.runtime == RuntimeKind::Container {
        let runtime = container_runtime(opts)?;
        let output = Command::new(&runtime)
            .args(["logs", "--tail", &lines.to_string(), &opts.container_name])
            .output()
            .map_err(Error::from)?;
        if !output.status.success() {
            return Err(Error::storage(format!(
                "{runtime} logs {} failed",
                opts.container_name
            )));
        }
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    if !opts.log_file.exists() {
        return Ok(format!("log file not found: {}\n", opts.log_file.display()));
    }
    let text = fs::read_to_string(&opts.log_file).map_err(Error::from)?;
    let mut out = text.lines().rev().take(lines).collect::<Vec<_>>();
    out.reverse();
    Ok(format!("{}\n", out.join("\n")))
}

pub fn build_image(opts: &RuntimeOptions, tag: &str, context: &Path) -> Result<serde_json::Value> {
    let runtime = container_runtime(opts)?;
    let status = Command::new(&runtime)
        .args(["build", "-t", tag])
        .arg(context)
        .status()
        .map_err(Error::from)?;
    if !status.success() {
        return Err(Error::storage(format!(
            "{runtime} build -t {tag} {} failed with status {status}; verify Docker/Podman can build this repo or set CODEX_MEMORYD_IMAGE to a pullable image",
            context.display()
        )));
    }
    Ok(serde_json::json!({
        "runtime": runtime,
        "tag": tag,
        "context": context,
        "next_command": format!("CODEX_MEMORYD_IMAGE={tag} codex-memoryd --runtime container up"),
    }))
}

pub fn http_get(url: &str) -> Result<String> {
    http_request("GET", url, None)
}

pub fn http_post_json(url: &str, body: &str) -> Result<String> {
    http_request("POST", url, Some(body))
}

fn http_request(method: &str, url: &str, body: Option<&str>) -> Result<String> {
    let parsed = parse_loopback_url(url)?;
    let mut stream = TcpStream::connect_timeout(&parsed.addr, Duration::from_millis(500))
        .map_err(|e| Error::storage(format!("connect {url}: {e}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(Error::from)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(Error::from)?;
    let req = if let Some(body) = body {
        format!(
            "{method} {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            parsed.path,
            parsed.host_header,
            body.len(),
            body
        )
    } else {
        format!(
            "{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            parsed.path, parsed.host_header
        )
    };
    stream.write_all(req.as_bytes()).map_err(Error::from)?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).map_err(Error::from)?;
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| Error::storage(format!("invalid HTTP response from {url}")))?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(Error::storage(format!(
            "HTTP request failed for {url}: {}",
            head.lines().next().unwrap_or("unknown status")
        )));
    }
    Ok(body.to_string())
}

fn up_native(opts: &RuntimeOptions) -> Result<RuntimeStatusReport> {
    require_safe_bind(opts)?;
    let mut opts = opts.clone();
    fs::create_dir_all(opts.log_file.parent().unwrap_or_else(|| Path::new(".")))
        .map_err(Error::from)?;
    fs::create_dir_all(opts.db.parent().unwrap_or_else(|| Path::new("."))).map_err(Error::from)?;
    opts.db = resolve_database_path(&opts.db)?;
    fs::create_dir_all(
        opts.pid_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )
    .map_err(Error::from)?;
    let paths = native_process::Paths::new(&opts.pid_file).map_err(Error::from)?;
    let _lock = native_process::lock(&paths).map_err(Error::from)?;
    let already_running = match native_process::inspect(&paths) {
        native_process::Inspection::Running(_) => true,
        native_process::Inspection::Stopped => false,
        native_process::Inspection::Unverified(reason) => {
            return Err(unverified_pid_error(&opts.pid_file, &reason))
        }
    };

    if !already_running {
        native_process::preflight().map_err(|err| {
            Error::storage(format!("safe native lifecycle is unavailable: {err}"))
        })?;
        ensure_bind_available(&opts)?;
        native_process::clear(&paths).map_err(Error::from)?;
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&opts.log_file)
            .map_err(Error::from)?;
        let log_err = log.try_clone().map_err(Error::from)?;
        let marker = uuid::Uuid::new_v4().to_string();
        let (startup_ack, child_stdin) = native_process::startup_channel().map_err(Error::from)?;
        let mut command = Command::new(&opts.binary);
        command
            .arg("serve")
            .env("CODEX_MEMORYD_DB", &opts.db)
            .env("CODEX_MEMORYD_BIND", &opts.bind)
            .env("CODEX_MEMORYD_PROFILE", &opts.profile)
            .env("CODEX_MEMORYD_WORKSPACE", &opts.workspace)
            .env("CODEX_MEMORYD_LOG", &opts.log_level)
            .env(native_process::MARKER_ENV, &marker)
            .env(native_process::STATE_PATH_ENV, paths.pid_path())
            .env(native_process::ACK_ENV, "1")
            .stdin(child_stdin)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .spawn()
            .map_err(|e| Error::storage(format!("start {}: {e}", opts.binary.display())))?;
        let (process, identity) =
            native_process::capture(&paths, child.id(), &marker).map_err(|err| {
                Error::storage(format!(
                    "started child could not be verified; inspect it before retrying: {err}"
                ))
            })?;
        if let Err(err) = native_process::wait_for_startup_ack(startup_ack, &marker) {
            let cleanup = process.terminate_and_wait();
            return Err(Error::storage(format!("daemon did not establish safe process ownership: {err}; child cleanup: {cleanup:?}")));
        }
        if let Err(err) = native_process::write(&paths, child.id(), &identity) {
            let cleanup = process.terminate_and_wait();
            return Err(Error::storage(format!(
                "could not persist process identity: {err}; child cleanup: {cleanup:?}"
            )));
        }
    }

    wait_for_ready(&opts, &opts.db.to_string_lossy())?;
    Ok(status(&opts))
}

fn up_container(opts: &RuntimeOptions) -> Result<RuntimeStatusReport> {
    let runtime = container_runtime(opts)?;
    fs::create_dir_all(&opts.home).map_err(Error::from)?;
    if !opts.codex_memories_dir.exists() {
        fs::create_dir_all(&opts.codex_memories_dir).map_err(Error::from)?;
    }
    let database_parent = opts
        .db
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(database_parent).map_err(Error::from)?;
    let (database_parent, container_db_path) = container_database_mount(opts)?;

    if container_exists(&runtime, &opts.container_name)? {
        ensure_container_database_mount(
            &runtime,
            &opts.container_name,
            &database_parent,
            &container_db_path,
        )?;
        if !container_running(&runtime, &opts.container_name)? {
            ensure_bind_available(opts)?;
        }
        run_status(&runtime, &["start", &opts.container_name])?;
    } else {
        ensure_bind_available(opts)?;
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            opts.container_name.clone(),
            "--label".to_string(),
            "io.github.joshyorko.codex-memoryd.managed=true".to_string(),
        ];
        if let Some((uid, gid)) = opts.uid.as_ref().zip(opts.gid.as_ref()) {
            args.push("--user".to_string());
            args.push(format!("{uid}:{gid}"));
        }
        args.extend([
            "--publish".to_string(),
            format!("{}:{}:8787", opts.host, opts.port),
            "--volume".to_string(),
            format!("{}:/data", database_parent.display()),
            "--volume".to_string(),
            format!(
                "{}:/host-codex-memories:ro",
                opts.codex_memories_dir.display()
            ),
            "--env".to_string(),
            "CODEX_MEMORYD_BIND=0.0.0.0:8787".to_string(),
            "--env".to_string(),
            "CODEX_MEMORYD_DECLARE_LOOPBACK_PUBLISH=1".to_string(),
            "--env".to_string(),
            format!("CODEX_MEMORYD_DB={container_db_path}"),
            "--env".to_string(),
            format!("CODEX_MEMORYD_PROFILE={}", opts.profile),
            "--env".to_string(),
            format!("CODEX_MEMORYD_WORKSPACE={}", opts.workspace),
            "--env".to_string(),
            format!("CODEX_MEMORYD_LOG={}", opts.log_level),
        ]);
        for key in [
            "CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED",
            "CODEX_MEMORYD_DREAM_SCHEDULER_INTERVAL_SECONDS",
            "CODEX_MEMORYD_DREAM_IDLE_WINDOW_SECONDS",
            "CODEX_MEMORYD_DREAM_MIN_SESSION_AGE_SECONDS",
            "CODEX_MEMORYD_DREAM_MIN_TURN_COUNT",
            "CODEX_MEMORYD_DREAM_MAX_BATCH_SIZE",
            "CODEX_MEMORYD_DREAM_MAX_CANDIDATES",
            "CODEX_MEMORYD_DREAM_MAX_RUNTIME_SECONDS",
        ] {
            if let Ok(value) = std::env::var(key) {
                args.push("--env".to_string());
                args.push(format!("{key}={value}"));
            }
        }
        args.push(opts.image.clone());
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        run_status_with_hint(
            &runtime,
            &arg_refs,
            &format!(
                "container runtime could not run image {}; build a local image with `codex-memoryd image build --tag codex-memoryd:local` or set CODEX_MEMORYD_IMAGE",
                opts.image
            ),
        )?;
    }

    wait_for_ready(opts, &container_db_path)?;
    Ok(status_container(opts))
}

fn container_database_mount(opts: &RuntimeOptions) -> Result<(PathBuf, String)> {
    let database = if opts.db.is_absolute() {
        opts.db.clone()
    } else {
        std::env::current_dir()
            .map_err(|err| {
                Error::invalid_request(format!(
                    "cannot resolve relative container database path {}: {err}",
                    opts.db.display()
                ))
            })?
            .join(&opts.db)
    };
    let filename = database
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| {
            Error::invalid_request(format!(
                "container database path must name a UTF-8 file: {}",
                database.display()
            ))
        })?;
    if filename
        .chars()
        .any(|character| character.is_control() || character == ':')
    {
        return Err(Error::invalid_request(
            "container database filename contains characters that cannot be represented safely in a Docker/Podman mount",
        ));
    }
    let configured_parent = database.parent().ok_or_else(|| {
        Error::invalid_request(format!(
            "container database path has no usable parent directory: {}",
            database.display()
        ))
    })?;
    let configured_metadata = fs::metadata(configured_parent).map_err(|err| {
        Error::invalid_request(format!(
            "container database parent directory is not usable: {} ({err})",
            configured_parent.display()
        ))
    })?;
    if !configured_metadata.is_dir() {
        return Err(Error::invalid_request(format!(
            "container database parent is not a directory: {}",
            configured_parent.display()
        )));
    }
    let configured_parent = fs::canonicalize(configured_parent).map_err(|err| {
        Error::invalid_request(format!(
            "container database parent cannot be resolved safely: {} ({err})",
            database
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .display()
        ))
    })?;
    let resolved_database = match fs::symlink_metadata(&database) {
        Ok(_) => {
            let resolved = fs::canonicalize(&database).map_err(|err| {
                Error::invalid_request(format!(
                    "container database cannot be resolved safely: {} ({err})",
                    database.display()
                ))
            })?;
            if !fs::metadata(&resolved).map_err(Error::from)?.is_file() {
                return Err(Error::invalid_request(format!(
                    "container database path is not a file: {}",
                    database.display()
                )));
            }
            resolved
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => configured_parent.join(filename),
        Err(err) => {
            return Err(Error::invalid_request(format!(
                "container database path is not usable: {} ({err})",
                database.display()
            )))
        }
    };
    let database_parent = resolved_database.parent().ok_or_else(|| {
        Error::invalid_request(format!(
            "container database path has no usable parent directory: {}",
            resolved_database.display()
        ))
    })?;
    let database_parent = fs::canonicalize(database_parent).map_err(|err| {
        Error::invalid_request(format!(
            "container database parent cannot be resolved safely: {} ({err})",
            database_parent.display()
        ))
    })?;
    if database_parent == Path::new("/") {
        return Err(Error::invalid_request(
            "container runtime refuses to mount the host root as the database parent; set CODEX_MEMORYD_DB inside a dedicated data directory",
        ));
    }
    let database_parent_text = database_parent.to_str().ok_or_else(|| {
        Error::invalid_request(
            "container database parent is not valid UTF-8 and cannot be represented safely",
        )
    })?;
    if database_parent_text.contains(':') || database_parent_text.chars().any(char::is_control) {
        return Err(Error::invalid_request(format!(
            "container database parent cannot be represented safely in a Docker/Podman volume mount: {database_parent_text}"
        )));
    }
    let filename = resolved_database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Error::invalid_request(format!(
                "container database filename is not valid UTF-8: {}",
                resolved_database.display()
            ))
        })?;
    if filename
        .chars()
        .any(|character| character.is_control() || character == ':')
    {
        return Err(Error::invalid_request(
            "resolved container database filename contains characters that cannot be represented safely in a Docker/Podman mount",
        ));
    }
    Ok((database_parent, format!("/data/{filename}")))
}

fn ensure_container_database_mount(
    runtime: &str,
    name: &str,
    expected_parent: &Path,
    expected_database_path: &str,
) -> Result<()> {
    let template = r#"{{range .Mounts}}{{if eq .Destination "/data"}}{{.Source}}{{end}}{{end}}"#;
    let output = Command::new(runtime)
        .args(["container", "inspect", "--format", template, name])
        .output()
        .map_err(Error::from)?;
    if !output.status.success() {
        return Err(Error::storage(format!(
            "could not verify the /data database mount for managed container {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let actual_source = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if actual_source.is_empty() {
        return Err(Error::storage(format!(
            "managed container {name} has no verifiable /data bind mount; refusing to start it"
        )));
    }
    let actual_parent = fs::canonicalize(&actual_source).map_err(|err| {
        Error::storage(format!(
            "managed container {name} /data mount source cannot be resolved safely: {} ({err})",
            actual_source
        ))
    })?;
    if actual_parent != expected_parent {
        return Err(Error::storage(format!(
            "managed container {name} is mounted from {} at /data, but the configured database requires {}; refusing to start it because it would use the wrong database",
            actual_parent.display(),
            expected_parent.display()
        )));
    }

    let template = r#"{{range .Config.Env}}{{println .}}{{end}}"#;
    let output = Command::new(runtime)
        .args(["container", "inspect", "--format", template, name])
        .output()
        .map_err(Error::from)?;
    if !output.status.success() {
        return Err(Error::storage(format!(
            "could not verify CODEX_MEMORYD_DB for managed container {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let inspected_environment = String::from_utf8_lossy(&output.stdout);
    let configured_databases = inspected_environment
        .lines()
        .filter_map(|line| line.strip_prefix("CODEX_MEMORYD_DB="))
        .collect::<Vec<_>>();
    if configured_databases.as_slice() != [expected_database_path] {
        let actual = match configured_databases.as_slice() {
            [] => "missing".to_string(),
            [only] => (*only).to_string(),
            _ => "multiple values".to_string(),
        };
        return Err(Error::storage(format!(
            "managed container {name} has CODEX_MEMORYD_DB={actual}, but the configured database requires {expected_database_path}; refusing to start it because it would use the wrong database"
        )));
    }
    Ok(())
}

fn resolve_database_path(database: &Path) -> Result<PathBuf> {
    let cwd = std::env::current_dir().map_err(Error::from)?;
    resolve_database_path_from(database, &cwd)
}

fn resolve_database_path_from(database: &Path, cwd: &Path) -> Result<PathBuf> {
    if database == Path::new(":memory:") {
        return Ok(database.to_path_buf());
    }
    let database = if database.is_absolute() {
        database.to_path_buf()
    } else {
        cwd.join(database)
    };
    let filename = database.file_name().ok_or_else(|| {
        Error::invalid_request(format!(
            "native database path has no filename: {}",
            database.display()
        ))
    })?;
    let parent = database
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let canonical_parent = fs::canonicalize(parent).map_err(|err| {
        Error::invalid_request(format!(
            "native database parent cannot be resolved safely: {} ({err})",
            parent.display()
        ))
    })?;
    match fs::canonicalize(&database) {
        Ok(path) => Ok(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(canonical_parent.join(filename))
        }
        Err(err) => Err(Error::invalid_request(format!(
            "native database path is not usable: {} ({err})",
            database.display()
        ))),
    }
}

fn down_native(opts: &RuntimeOptions) -> Result<RuntimeStatusReport> {
    let paths = match native_process::Paths::new(&opts.pid_file) {
        Ok(paths) => paths,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(status(opts)),
        Err(err) => return Err(Error::from(err)),
    };
    let _lock = native_process::lock(&paths).map_err(Error::from)?;
    match native_process::inspect(&paths) {
        native_process::Inspection::Stopped => {}
        native_process::Inspection::Unverified(reason) => {
            return Err(unverified_pid_error(&opts.pid_file, &reason))
        }
        native_process::Inspection::Running(process) => {
            let pid = process.pid();
            process.terminate_and_wait().map_err(|err| {
                Error::storage(format!("failed to stop verified pid {pid}: {err}"))
            })?;
        }
    }
    native_process::clear(&paths).map_err(Error::from)?;
    Ok(status(opts))
}

fn unverified_pid_error(path: &Path, reason: &str) -> Error {
    Error::storage(format!("cannot verify native process in {} ({reason}); no signal was sent. Inspect the runtime and follow the safe recovery procedure in docs/getting-started.md", path.display()))
}

fn down_container(opts: &RuntimeOptions) -> Result<RuntimeStatusReport> {
    let runtime = container_runtime(opts)?;
    if container_exists(&runtime, &opts.container_name)? {
        let _ = run_status(&runtime, &["stop", &opts.container_name]);
    }
    Ok(status_container(opts))
}

fn wait_for_health(opts: &RuntimeOptions) -> Result<()> {
    let url = format!("{}/healthz", opts.url.trim_end_matches('/'));
    for _ in 0..30 {
        if http_get(&url).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(Error::storage(format!("timed out waiting for {url}")))
}

fn wait_for_ready(opts: &RuntimeOptions, db_path: &str) -> Result<()> {
    wait_for_health(opts)?;
    let status_url = format!("{}/v1/status", opts.url.trim_end_matches('/'));
    for _ in 0..30 {
        if let Ok(body) = http_get(&status_url) {
            let parsed: serde_json::Value = serde_json::from_str(&body)?;
            if managed_status_ready(&parsed, &opts.profile, &opts.workspace, db_path) {
                return Ok(());
            }
            if let Some(status) = parsed.pointer("/data/status").and_then(|v| v.as_str()) {
                return Err(Error::storage(format!(
                    "daemon status is '{status}', core readiness requirements are not met"
                )));
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(Error::storage(format!(
        "timed out waiting for {status_url}"
    )))
}

fn managed_status_ready(
    body: &serde_json::Value,
    profile: &str,
    workspace: &str,
    db_path: &str,
) -> bool {
    let Some(data) = body.get("data") else {
        return false;
    };
    let status = data.get("status").and_then(|value| value.as_str());
    let Some(storage) = data.get("storage") else {
        return false;
    };
    let Some(features) = data.get("features") else {
        return false;
    };
    if storage.get("kind").and_then(|value| value.as_str()) != Some("sqlite")
        || storage.get("path").and_then(|value| value.as_str()) != Some(db_path)
        || storage.get("writable").and_then(|value| value.as_bool()) != Some(true)
        || data
            .get("storage_schema_version")
            .and_then(|value| value.as_i64())
            != Some(STORAGE_SCHEMA_VERSION)
        || features.get("exposure").and_then(|value| value.as_str()) != Some("local_only")
        || features.get("auth").and_then(|value| value.as_str()) != Some("none")
        || !managed_scope_available(data, status, profile, workspace)
    {
        return false;
    }

    let Some(reasons) = data
        .get("degraded_reasons")
        .and_then(|value| value.as_array())
    else {
        return false;
    };
    match status {
        Some("local_only") => reasons.is_empty(),
        Some("degraded") => degraded_only_by_dreamer(data, reasons),
        _ => false,
    }
}

fn managed_scope_available(
    data: &serde_json::Value,
    status: Option<&str>,
    profile: &str,
    workspace: &str,
) -> bool {
    let Some(profiles) = data
        .get("active_profiles")
        .and_then(|value| value.as_array())
    else {
        return false;
    };
    let Some(workspaces) = data
        .get("active_workspaces")
        .and_then(|value| value.as_array())
    else {
        return false;
    };
    let Some(scope) = data.pointer("/features/configured_scope") else {
        return false;
    };
    if scope.get("profile").and_then(|value| value.as_str()) != Some(profile)
        || scope.get("workspace").and_then(|value| value.as_str()) != Some(workspace)
    {
        return false;
    }
    if profiles.is_empty() && workspaces.is_empty() {
        return status == Some("local_only")
            && scope.get("available").and_then(|value| value.as_bool()) == Some(false);
    }
    profiles.iter().any(|value| value.as_str() == Some(profile))
        && workspaces
            .iter()
            .any(|value| value.as_str() == Some(workspace))
        && scope.get("available").and_then(|value| value.as_bool()) == Some(true)
}

fn degraded_only_by_dreamer(data: &serde_json::Value, reasons: &[serde_json::Value]) -> bool {
    let Some(last_dream) = data.get("last_dream") else {
        return false;
    };
    if last_dream.get("status").and_then(|value| value.as_str()) != Some("error")
        || last_dream.get("mode").and_then(|value| value.as_str()) != Some("preview")
        || reasons.len() != 1
    {
        return false;
    }
    let error = last_dream
        .get("error_summary")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());
    let expected = error.map_or_else(
        || "last Dreamer run failed".to_string(),
        |error| format!("last Dreamer run failed: {error}"),
    );
    reasons[0].as_str() == Some(expected.as_str())
}

fn status_container(opts: &RuntimeOptions) -> RuntimeStatusReport {
    let process = container_runtime(opts)
        .ok()
        .and_then(|runtime| container_running(&runtime, &opts.container_name).ok())
        .map(|running| if running { "running" } else { "stopped" })
        .unwrap_or("unavailable")
        .to_string();
    let health = if process == "running"
        && opts.endpoint_configured
        && http_get(&format!("{}/healthz", opts.url.trim_end_matches('/'))).is_ok()
    {
        "ok"
    } else {
        "unreachable"
    };
    RuntimeStatusReport {
        runtime: "container".to_string(),
        process,
        pid: None,
        url: opts.url.clone(),
        bind: format!("{}:{} -> container:8787", opts.host, opts.port),
        db: container_database_mount(opts)
            .map(|(parent, path)| format!("{}:{path}", parent.display()))
            .unwrap_or_else(|_| opts.db.display().to_string()),
        log_file: format!(
            "{} logs {}",
            container_runtime_name(opts),
            opts.container_name
        ),
        health: health.to_string(),
    }
}

pub fn container_runtime(opts: &RuntimeOptions) -> Result<String> {
    if let Some(runtime) = &opts.container_runtime {
        if runtime.trim().eq_ignore_ascii_case("auto") {
            // Fall through to discovery below.
        } else {
            if command_exists(runtime) {
                return Ok(runtime.clone());
            }
            return Err(Error::invalid_request(format!(
                "configured container runtime not found: {runtime}"
            )));
        }
    }
    for runtime in ["docker", "podman"] {
        if command_exists(runtime) {
            return Ok(runtime.to_string());
        }
    }
    Err(Error::invalid_request(
        "managed container runtime requires docker or podman",
    ))
}

fn container_runtime_name(opts: &RuntimeOptions) -> String {
    if let Some(runtime) = &opts.container_runtime {
        if runtime.trim().eq_ignore_ascii_case("auto") {
            return "docker|podman".to_string();
        }
        return runtime.clone();
    }
    "docker|podman".to_string()
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn command_output(name: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(name).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn container_exists(runtime: &str, name: &str) -> Result<bool> {
    Ok(Command::new(runtime)
        .args(["container", "inspect", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(Error::from)?
        .success())
}

fn container_running(runtime: &str, name: &str) -> Result<bool> {
    let output = Command::new(runtime)
        .args([
            "container",
            "inspect",
            "--format",
            "{{.State.Running}}",
            name,
        ])
        .output()
        .map_err(Error::from)?;
    Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
}

fn run_status(runtime: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(runtime)
        .args(args)
        .status()
        .map_err(Error::from)?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::storage(format!(
            "{runtime} {} failed with status {status}",
            args.join(" ")
        )))
    }
}

fn run_status_with_hint(runtime: &str, args: &[&str], hint: &str) -> Result<()> {
    let status = Command::new(runtime)
        .args(args)
        .status()
        .map_err(Error::from)?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::storage(format!(
            "{runtime} {} failed with status {status}; {hint}",
            args.join(" ")
        )))
    }
}

fn ensure_dir(path: &Path, created: &mut Vec<String>, reused: &mut Vec<String>) -> Result<()> {
    if path.exists() {
        reused.push(path.display().to_string());
    } else {
        fs::create_dir_all(path).map_err(Error::from)?;
        created.push(path.display().to_string());
    }
    Ok(())
}

fn write_or_update(
    path: &Path,
    body: &str,
    created: &mut Vec<String>,
    reused: &mut Vec<String>,
    updated: &mut Vec<String>,
) -> Result<()> {
    if path.exists() {
        let existing = fs::read_to_string(path).map_err(Error::from)?;
        if existing == body {
            reused.push(path.display().to_string());
        } else {
            fs::write(path, body).map_err(Error::from)?;
            updated.push(path.display().to_string());
        }
    } else {
        fs::write(path, body).map_err(Error::from)?;
        created.push(path.display().to_string());
    }
    Ok(())
}

fn ensure_bind_available(opts: &RuntimeOptions) -> Result<()> {
    match TcpListener::bind(&opts.bind) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(e) => Err(Error::storage(format!(
            "cannot bind {}; port {} is busy or unavailable ({e}). Stop the process using it or set another port with CODEX_MEMORYD_PORT=<port> / `codex-memoryd init --port <port>`",
            opts.bind, opts.port
        ))),
    }
}

fn render_config(opts: &RuntimeOptions) -> String {
    format!(
        r#"[server]
bind = "{bind}"

[storage]
kind = "sqlite"
path = "{db}"

[policy]
default_profile = "{profile}"
default_workspace = "{workspace}"

[log]
level = "{log_level}"
"#,
        bind = opts.bind,
        db = opts.db.display(),
        profile = opts.profile,
        workspace = opts.workspace,
        log_level = opts.log_level,
    )
}

fn render_runtime_env(opts: &RuntimeOptions) -> String {
    format!(
        r#"CODEX_MEMORYD_RUNTIME={runtime}
CODEX_MEMORYD_HOME={home}
CODEX_MEMORYD_RUNTIME_DIR={home}
CODEX_MEMORYD_URL={url}
CODEX_MEMORYD_HOST={host}
CODEX_MEMORYD_PORT={port}
CODEX_MEMORYD_BIND={bind}
CODEX_MEMORYD_DB={db}
CODEX_MEMORYD_PID_FILE={pid_file}
CODEX_MEMORYD_LOG_FILE={log_file}
CODEX_MEMORYD_PROFILE={profile}
CODEX_MEMORYD_WORKSPACE={workspace}
CODEX_MEMORYD_LOG={log_level}
CODEX_MEMORYD_IMAGE={image}
CODEX_MEMORYD_CONTAINER_NAME={container_name}
CODEX_MEMORYD_CONTAINER_RUNTIME={container_runtime}
CODEX_MEMORYD_UID={uid}
CODEX_MEMORYD_GID={gid}
CODEX_MEMORYD_CODEX_MEMORIES_DIR={codex_memories_dir}
"#,
        runtime = runtime_name(opts.runtime),
        home = opts.home.display(),
        url = opts.url,
        host = opts.host,
        port = opts.port,
        bind = opts.bind,
        db = opts.db.display(),
        pid_file = opts.pid_file.display(),
        log_file = opts.log_file.display(),
        profile = opts.profile,
        workspace = opts.workspace,
        log_level = opts.log_level,
        image = opts.image,
        container_name = opts.container_name,
        container_runtime = opts
            .container_runtime
            .clone()
            .unwrap_or_else(|| "auto".to_string()),
        uid = opts.uid.clone().unwrap_or_default(),
        gid = opts.gid.clone().unwrap_or_default(),
        codex_memories_dir = opts.codex_memories_dir.display(),
    )
}

fn require_safe_bind(opts: &RuntimeOptions) -> Result<()> {
    if bind_is_loopback(&opts.bind) || opts.allow_non_loopback {
        Ok(())
    } else {
        Err(Error::invalid_request(format!(
            "refusing non-loopback bind '{}'; set CODEX_MEMORYD_ALLOW_NON_LOOPBACK=1 only behind HTTPS auth",
            opts.bind
        )))
    }
}

fn bind_is_loopback(bind: &str) -> bool {
    bind.starts_with("127.")
        || bind.starts_with("localhost:")
        || bind.starts_with("[::1]:")
        || bind.starts_with("::1:")
}

fn env_path(key: &str) -> Option<PathBuf> {
    env_value(key).map(|v| config::expand_path(&v))
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn runtime_env_path(entries: &[(String, String)], key: &str) -> Option<PathBuf> {
    runtime_env_value(entries, key).map(|v| config::expand_path(&v))
}

fn runtime_env_value(entries: &[(String, String)], key: &str) -> Option<String> {
    entries
        .iter()
        .find_map(|(entry_key, value)| (entry_key == key).then(|| value.clone()))
        .filter(|v| !v.trim().is_empty())
}

fn runtime_env_runtime(entries: &[(String, String)], key: &str) -> Option<RuntimeKind> {
    match runtime_env_value(entries, key)?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "native" => Some(RuntimeKind::Native),
        "container" => Some(RuntimeKind::Container),
        "auto" => Some(RuntimeKind::Auto),
        "compose-dev" | "compose_dev" | "compose" => Some(RuntimeKind::ComposeDev),
        _ => None,
    }
}

fn read_runtime_env(path: &Path) -> Vec<(String, String)> {
    let Ok(body) = fs::read_to_string(path) else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            ))
        })
        .collect()
}

fn parse_bind_host_port(bind: &str) -> Option<(String, u16)> {
    if let Ok(addr) = bind.parse::<SocketAddr>() {
        return Some((addr.ip().to_string(), addr.port()));
    }
    if bind.starts_with('[') {
        let end = bind.find(']')?;
        let host = bind[1..end].to_string();
        let port = bind[end + 1..].strip_prefix(':')?.parse::<u16>().ok()?;
        return Some((host, port));
    }
    let (host, port) = bind.rsplit_once(':')?;
    Some((host.to_string(), port.parse::<u16>().ok()?))
}

fn resolve_runtime(
    explicit: Option<RuntimeKind>,
    runtime_env: &[(String, String)],
) -> (RuntimeKind, RuntimeSelectionSource) {
    if let Some(runtime) = explicit {
        return (runtime, RuntimeSelectionSource::Cli);
    }
    if let Some(runtime) = env_runtime("CODEX_MEMORYD_RUNTIME") {
        return (runtime, RuntimeSelectionSource::Environment);
    }
    if let Some(runtime) = runtime_env_runtime(runtime_env, "CODEX_MEMORYD_RUNTIME") {
        return (runtime, RuntimeSelectionSource::RuntimeEnv);
    }
    (RuntimeKind::Native, RuntimeSelectionSource::Default)
}

fn env_runtime(key: &str) -> Option<RuntimeKind> {
    match std::env::var(key)
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "native" => Some(RuntimeKind::Native),
        "container" => Some(RuntimeKind::Container),
        "auto" => Some(RuntimeKind::Auto),
        "compose-dev" | "compose_dev" | "compose" => Some(RuntimeKind::ComposeDev),
        _ => None,
    }
}

fn runtime_name(runtime: RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Native => "native",
        RuntimeKind::Container => "container",
        RuntimeKind::Auto => "auto",
        RuntimeKind::ComposeDev => "compose-dev",
    }
}

struct ParsedUrl {
    addr: SocketAddr,
    host_header: String,
    path: String,
}

fn parse_loopback_url(url: &str) -> Result<ParsedUrl> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| Error::invalid_request("only http:// loopback URLs are supported"))?;
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = host_port
        .rsplit_once(':')
        .ok_or_else(|| Error::invalid_request(format!("URL must include port: {url}")))?;
    if host != "127.0.0.1" && host != "localhost" {
        return Err(Error::invalid_request(format!(
            "refusing non-loopback URL host '{host}'"
        )));
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| Error::invalid_request(format!("invalid URL port in {url}")))?;
    Ok(ParsedUrl {
        addr: SocketAddr::from(([127, 0, 0, 1], port)),
        host_header: host_port.to_string(),
        path: format!("/{}", path),
    })
}

#[cfg(test)]
mod managed_readiness_tests {
    use super::{managed_status_ready, resolve_database_path_from};
    use crate::store::STORAGE_SCHEMA_VERSION;
    use serde_json::json;
    use std::fs;
    use std::path::Path;

    fn local_status() -> serde_json::Value {
        json!({"data": {
            "status": "local_only",
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "storage": {"kind": "sqlite", "path": "/tmp/synthetic.db", "writable": true},
            "active_profiles": ["personal"],
            "active_workspaces": ["synthetic-scope"],
            "last_dream": null,
            "degraded_reasons": [],
            "features": {"exposure": "local_only", "auth": "none",
                "configured_scope": {"profile": "personal", "workspace": "synthetic-scope", "available": true}}
        }})
    }

    fn dreamer_degraded_status() -> serde_json::Value {
        json!({"data": {
            "status": "degraded",
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "storage": {"kind": "sqlite", "path": "/tmp/synthetic.db", "writable": true},
            "active_profiles": ["personal"],
            "active_workspaces": ["synthetic-scope"],
            "last_dream": {
                "mode": "preview",
                "status": "error",
                "error_summary": "dream provider input byte budget exhausted"
            },
            "degraded_reasons": ["last Dreamer run failed: dream provider input byte budget exhausted"],
            "features": {"exposure": "local_only", "auth": "none",
                "configured_scope": {"profile": "personal", "workspace": "synthetic-scope", "available": true}}
        }})
    }

    fn ready(body: &serde_json::Value, profile: &str, workspace: &str) -> bool {
        managed_status_ready(body, profile, workspace, "/tmp/synthetic.db")
    }

    #[test]
    fn accepts_core_ready_and_dreamer_only_degraded_statuses() {
        assert!(ready(&local_status(), "personal", "synthetic-scope"));
        assert!(ready(
            &dreamer_degraded_status(),
            "personal",
            "synthetic-scope"
        ));

        let empty_store = json!({"data": {
            "status": "local_only",
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "storage": {"kind": "sqlite", "path": "/tmp/empty.db", "writable": true},
            "active_profiles": [],
            "active_workspaces": [],
            "last_dream": null,
            "degraded_reasons": [],
            "features": {"exposure": "local_only", "auth": "none",
                "configured_scope": {"profile": "personal", "workspace": "default", "available": false}}
        }});
        assert!(managed_status_ready(
            &empty_store,
            "personal",
            "default",
            "/tmp/empty.db"
        ));

        let mut missing_degraded_scope = dreamer_degraded_status();
        missing_degraded_scope["data"]["active_profiles"] = json!([]);
        missing_degraded_scope["data"]["active_workspaces"] = json!([]);
        assert!(!ready(
            &missing_degraded_scope,
            "personal",
            "synthetic-scope"
        ));
    }

    #[test]
    fn rejects_profile_workspace_cross_product_from_real_status() {
        let store = crate::store::Store::open(":memory:").unwrap();
        store
            .ensure_workspace("personal", "other-workspace")
            .unwrap();
        store.ensure_workspace("work", "synthetic-scope").unwrap();
        let mut config = crate::config::Config::default();
        config.default_profile = "personal".into();
        config.default_workspace = "synthetic-scope".into();
        let service = crate::service::Service::new(store.clone(), config);
        let status = serde_json::to_value(service.status().unwrap()).unwrap();
        let path = status["storage"]["path"].as_str().unwrap().to_string();
        assert!(
            !managed_status_ready(
                &json!({"data": status}),
                "personal",
                "synthetic-scope",
                &path
            ),
            "unrelated profile and workspace must not establish a ready scope"
        );

        store
            .ensure_workspace("personal", "synthetic-scope")
            .unwrap();
        let status = serde_json::to_value(service.status().unwrap()).unwrap();
        assert!(managed_status_ready(
            &json!({"data": status}),
            "personal",
            "synthetic-scope",
            &path
        ));
    }

    #[test]
    fn rejects_inconsistent_empty_scope_claims() {
        let mut status = local_status();
        status["data"]["active_profiles"] = json!([]);
        status["data"]["active_workspaces"] = json!([]);
        assert!(!ready(&status, "personal", "synthetic-scope"));
        status["data"]["features"]["configured_scope"]["available"] = json!(false);
        assert!(ready(&status, "personal", "synthetic-scope"));
        assert!(!ready(&status, "work", "synthetic-scope"));
        assert!(!ready(&status, "personal", "other-workspace"));
        status["data"]["features"]
            .as_object_mut()
            .unwrap()
            .remove("configured_scope");
        assert!(!ready(&status, "personal", "synthetic-scope"));
    }

    #[test]
    fn rejects_non_dreamer_and_unverifiable_degradation() {
        let mut mixed_reason = dreamer_degraded_status();
        mixed_reason["data"]["degraded_reasons"]
            .as_array_mut()
            .unwrap()
            .push(json!("storage is not writable"));
        assert!(!ready(&mixed_reason, "personal", "synthetic-scope"));

        let mut no_receipt = dreamer_degraded_status();
        no_receipt["data"]["last_dream"] = json!(null);
        assert!(!ready(&no_receipt, "personal", "synthetic-scope"));

        let mut failed_apply = dreamer_degraded_status();
        failed_apply["data"]["last_dream"]["mode"] = json!("apply");
        assert!(!ready(&failed_apply, "personal", "synthetic-scope"));

        let mut no_reason = dreamer_degraded_status();
        no_reason["data"]["degraded_reasons"] = json!([]);
        assert!(!ready(&no_reason, "personal", "synthetic-scope"));

        let caller_boolean = json!({"ready": true, "data": {"status": "degraded"}});
        assert!(!ready(&caller_boolean, "personal", "synthetic-scope"));
    }

    #[test]
    fn rejects_storage_schema_authorization_exposure_and_scope_failures() {
        let mut unreadable = local_status();
        unreadable["data"]["storage"]["writable"] = json!(false);
        assert!(!ready(&unreadable, "personal", "synthetic-scope"));

        let mut malformed_storage = local_status();
        malformed_storage["data"]["storage"] = json!(null);
        assert!(!ready(&malformed_storage, "personal", "synthetic-scope"));

        let mut wrong_schema = local_status();
        wrong_schema["data"]["storage_schema_version"] = json!(STORAGE_SCHEMA_VERSION - 1);
        assert!(!ready(&wrong_schema, "personal", "synthetic-scope"));

        let mut missing_schema = local_status();
        missing_schema["data"]
            .as_object_mut()
            .unwrap()
            .remove("storage_schema_version");
        assert!(!ready(&missing_schema, "personal", "synthetic-scope"));

        let mut remote_exposure = local_status();
        remote_exposure["data"]["features"]["exposure"] = json!("auth_missing");
        assert!(!ready(&remote_exposure, "personal", "synthetic-scope"));

        let mut missing_exposure = local_status();
        missing_exposure["data"]["features"]
            .as_object_mut()
            .unwrap()
            .remove("exposure");
        assert!(!ready(&missing_exposure, "personal", "synthetic-scope"));

        let mut other_database = local_status();
        other_database["data"]["storage"]["path"] = json!("/tmp/other.db");
        assert!(!ready(&other_database, "personal", "synthetic-scope"));

        let mut authorization_failure = local_status();
        authorization_failure["data"]["features"]["auth"] = json!("required");
        assert!(!ready(
            &authorization_failure,
            "personal",
            "synthetic-scope"
        ));

        let mut inconsistent_status = local_status();
        inconsistent_status["data"]["degraded_reasons"] = json!(["storage is not writable"]);
        assert!(!ready(&inconsistent_status, "personal", "synthetic-scope"));

        assert!(!ready(&local_status(), "other-profile", "synthetic-scope"));
        assert!(!ready(&local_status(), "personal", "other-workspace"));
    }

    #[test]
    fn relative_database_identity_is_bound_to_the_starting_working_directory() {
        let temp = tempfile::tempdir().expect("temp directory");
        let first_cwd = temp.path().join("first");
        let second_cwd = temp.path().join("second");
        fs::create_dir_all(first_cwd.join("data")).expect("first database directory");
        fs::create_dir_all(second_cwd.join("data")).expect("second database directory");

        let configured = Path::new("data/memory.db");
        let first_path = resolve_database_path_from(configured, &first_cwd)
            .expect("resolve first database identity");
        let second_path = resolve_database_path_from(configured, &second_cwd)
            .expect("resolve second database identity");
        assert_ne!(first_path, second_path);

        let mut status = local_status();
        status["data"]["storage"]["path"] = json!(first_path.display().to_string());
        assert!(managed_status_ready(
            &status,
            "personal",
            "synthetic-scope",
            &first_path.display().to_string()
        ));
        assert!(!managed_status_ready(
            &status,
            "personal",
            "synthetic-scope",
            &second_path.display().to_string()
        ));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod container_readiness_regressions {
    use super::{up, RuntimeKind, RuntimeOptions};
    use crate::store::STORAGE_SCHEMA_VERSION;
    use serde_json::{json, Value};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    fn container_status(path: &str) -> Value {
        json!({"data": {
            "status": "local_only",
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "storage": {"kind": "sqlite", "path": path, "writable": true},
            "active_profiles": ["personal"],
            "active_workspaces": ["synthetic-scope"],
            "last_dream": null,
            "degraded_reasons": [],
            "features": {"exposure": "local_only", "auth": "none",
                "configured_scope": {"profile": "personal", "workspace": "synthetic-scope", "available": true}}
        }})
    }

    fn status_server(status: Value) -> (String, Arc<AtomicBool>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_server = Arc::clone(&stop);
        let status_body = status.to_string();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !stop_server.load(Ordering::Relaxed) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 2048];
                        let read = stream.read(&mut request).unwrap_or(0);
                        let request = String::from_utf8_lossy(&request[..read]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let body = if path == "/healthz" {
                            "{}"
                        } else {
                            status_body.as_str()
                        };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (format!("http://{address}"), stop, server)
    }

    fn fake_container_runtime(
        path: &std::path::Path,
        invocation_log: &std::path::Path,
        state_file: &std::path::Path,
        mounted_data_source: &std::path::Path,
        container_database_path: &str,
        existing_database_path: Option<&str>,
        initial_existing_container: bool,
    ) {
        let mount_source_file = path.with_extension("mount-source");
        let database_env_file = path.with_extension("database-env");
        fs::write(
            path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nprintf '%s\\n' \"$*\" >> '{}';\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ]; then if [ -f '{}' ]; then if [ \"$3\" = \"--format\" ]; then case \"$4\" in *State.Running*) printf true ;; *Mounts*) cat '{}' ;; *Config.Env*) cat '{}' ;; esac; fi; exit 0; fi; exit 1; fi\nif [ \"$1\" = \"run\" ]; then printf '%s' '{}' > '{}'; printf 'CODEX_MEMORYD_DB=%s\\n' '{}' > '{}'; touch '{}'; exit 0; fi\nif [ \"$1\" = \"start\" ]; then exit 0; fi\nexit 1\n",
                invocation_log.display(),
                state_file.display(),
                mount_source_file.display(),
                database_env_file.display(),
                mounted_data_source.display(),
                mount_source_file.display(),
                container_database_path,
                database_env_file.display(),
                state_file.display()
            ),
        )
        .unwrap();
        if initial_existing_container {
            fs::write(
                &mount_source_file,
                mounted_data_source.as_os_str().to_string_lossy().as_bytes(),
            )
            .unwrap();
            fs::write(
                &database_env_file,
                format!(
                    "CODEX_MEMORYD_DB={}\n",
                    existing_database_path.unwrap_or(container_database_path)
                ),
            )
            .unwrap();
            fs::write(state_file, "existing").unwrap();
        }
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn run_managed_container_up(
        status_path: &str,
        db_relative_path: &str,
        existing_mount_relative_path: Option<&str>,
        existing_database_path: Option<&str>,
    ) -> (
        crate::error::Result<super::RuntimeStatusReport>,
        String,
        String,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("fake-container-runtime");
        let invocation_log = temp.path().join("container-runtime.log");
        let state_file = temp.path().join("container-created");
        let db = temp.path().join(db_relative_path);
        let database_parent = db.parent().unwrap().to_string_lossy().into_owned();
        let container_database_path =
            format!("/data/{}", db.file_name().unwrap().to_string_lossy());
        let mounted_source = existing_mount_relative_path
            .map(|path| temp.path().join(path))
            .unwrap_or_else(|| PathBuf::from(&database_parent));
        if existing_mount_relative_path.is_some() {
            fs::create_dir_all(&mounted_source).unwrap();
        }
        fake_container_runtime(
            &runtime,
            &invocation_log,
            &state_file,
            &mounted_source,
            &container_database_path,
            existing_database_path,
            existing_mount_relative_path.is_some(),
        );
        let (url, stop, server) = status_server(container_status(status_path));
        let port = 0;
        let options = RuntimeOptions {
            runtime: RuntimeKind::Container,
            home: temp.path().join("runtime-home"),
            url,
            host: "127.0.0.1".into(),
            port,
            bind: format!("127.0.0.1:{port}"),
            db,
            pid_file: temp.path().join("unused.pid"),
            log_file: temp.path().join("unused.log"),
            profile: "personal".into(),
            workspace: "synthetic-scope".into(),
            log_level: "info".into(),
            binary: PathBuf::from("unused-binary"),
            allow_non_loopback: false,
            image: "synthetic-image".into(),
            container_name: "synthetic-memoryd".into(),
            container_runtime: Some(runtime.to_string_lossy().into_owned()),
            codex_memories_dir: temp.path().join("synthetic-memories"),
            uid: None,
            gid: None,
            endpoint_configured: true,
        };

        let result = up(&options);
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        let invocations = fs::read_to_string(invocation_log).unwrap_or_default();
        (result, invocations, database_parent)
    }

    #[test]
    fn up_accepts_healthy_container_status_at_container_database_path() {
        let (result, invocations, database_parent) =
            run_managed_container_up("/data/memory.db", "runtime-home/memory.db", None, None);
        assert!(
            result.is_ok(),
            "managed container startup failed: {result:?}"
        );
        assert_eq!(result.unwrap().health, "ok");
        assert!(invocations.contains(&format!("--volume {database_parent}:/data")));
        assert!(invocations.contains("CODEX_MEMORYD_DB=/data/memory.db"));
    }

    #[test]
    fn up_rejects_container_status_for_wrong_database_path() {
        let (result, _, _) =
            run_managed_container_up("/data/wrong.db", "runtime-home/memory.db", None, None);
        let error = result.expect_err("wrong managed-container storage path must fail readiness");
        assert!(error
            .message
            .contains("core readiness requirements are not met"));
    }

    #[test]
    fn up_mounts_and_verifies_the_configured_custom_database() {
        let (result, invocations, database_parent) = run_managed_container_up(
            "/data/custom.sqlite",
            "custom-store/custom.sqlite",
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "managed container startup failed for configured database: {result:?}"
        );
        assert!(invocations.contains(&format!("--volume {database_parent}:/data")));
        assert!(invocations.contains("CODEX_MEMORYD_DB=/data/custom.sqlite"));
    }

    #[test]
    fn up_rejects_an_existing_container_with_a_stale_database_mount() {
        let (result, invocations, database_parent) = run_managed_container_up(
            "/data/memory.db",
            "new-store/memory.db",
            Some("old-store"),
            None,
        );
        let error = result.expect_err("old container must not start with a different DB mount");
        assert!(error.message.contains("mounted from"), "{error:?}");
        assert!(error.message.contains(&database_parent), "{error:?}");
        assert!(!invocations.contains("start synthetic-memoryd"));
        assert!(!invocations.contains("run -d"));
    }

    #[test]
    fn up_reuses_an_existing_container_with_the_configured_database_mount() {
        let (result, invocations, _database_parent) = run_managed_container_up(
            "/data/memory.db",
            "runtime-home/memory.db",
            Some("runtime-home"),
            None,
        );
        assert!(
            result.is_ok(),
            "matching managed container should be reusable: {result:?}"
        );
        assert!(invocations.contains("container inspect --format"));
        assert!(invocations.contains("start synthetic-memoryd"));
        assert!(!invocations.contains("run -d"));
    }

    #[test]
    fn up_rejects_an_existing_container_with_a_stale_database_filename() {
        let (result, invocations, _) = run_managed_container_up(
            "/data/old.db",
            "runtime-home/new.db",
            Some("runtime-home"),
            Some("/data/old.db"),
        );
        let error = result.expect_err("existing container must not start with a stale DB filename");
        assert!(error.message.contains("CODEX_MEMORYD_DB"), "{error:?}");
        assert!(error.message.contains("/data/new.db"), "{error:?}");
        assert!(error.message.contains("/data/old.db"), "{error:?}");
        assert!(!invocations.contains("start synthetic-memoryd"));
        assert!(!invocations.contains("run -d"));
    }
}
