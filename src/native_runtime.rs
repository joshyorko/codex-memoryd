//! Productized local runtime management for the native binary mode.

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
use crate::store::Store;

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
        let bind = bind_candidate.unwrap_or_else(|| format!("{host}:{port}"));
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
    RuntimeStatusReport {
        runtime: runtime_name(opts.runtime).to_string(),
        process: if running_pid(&opts.pid_file).is_some() {
            "running".to_string()
        } else {
            "stopped".to_string()
        },
        pid: running_pid(&opts.pid_file),
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
    fs::create_dir_all(opts.log_file.parent().unwrap_or_else(|| Path::new(".")))
        .map_err(Error::from)?;
    fs::create_dir_all(opts.db.parent().unwrap_or_else(|| Path::new("."))).map_err(Error::from)?;

    if running_pid(&opts.pid_file).is_none() {
        ensure_bind_available(opts)?;
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&opts.log_file)
            .map_err(Error::from)?;
        let log_err = log.try_clone().map_err(Error::from)?;
        let mut command = Command::new(&opts.binary);
        command
            .arg("serve")
            .env("CODEX_MEMORYD_DB", &opts.db)
            .env("CODEX_MEMORYD_BIND", &opts.bind)
            .env("CODEX_MEMORYD_PROFILE", &opts.profile)
            .env("CODEX_MEMORYD_WORKSPACE", &opts.workspace)
            .env("CODEX_MEMORYD_LOG", &opts.log_level)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .spawn()
            .map_err(|e| Error::storage(format!("start {}: {e}", opts.binary.display())))?;
        fs::write(&opts.pid_file, child.id().to_string()).map_err(Error::from)?;
    }

    wait_for_ready(opts)?;
    Ok(status(opts))
}

fn up_container(opts: &RuntimeOptions) -> Result<RuntimeStatusReport> {
    let runtime = container_runtime(opts)?;
    fs::create_dir_all(&opts.home).map_err(Error::from)?;
    if !opts.codex_memories_dir.exists() {
        fs::create_dir_all(&opts.codex_memories_dir).map_err(Error::from)?;
    }

    if container_exists(&runtime, &opts.container_name)? {
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
            format!("{}:/data", opts.home.display()),
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
            "CODEX_MEMORYD_DB=/data/memory.db".to_string(),
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

    wait_for_ready(opts)?;
    Ok(status_container(opts))
}

fn down_native(opts: &RuntimeOptions) -> Result<RuntimeStatusReport> {
    let Some(pid) = running_pid(&opts.pid_file) else {
        let _ = fs::remove_file(&opts.pid_file);
        return Ok(status(opts));
    };
    let ok = Command::new("kill")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status()
        .map_err(Error::from)?
        .success();
    if !ok {
        return Err(Error::storage(format!("failed to stop pid {pid}")));
    }
    for _ in 0..40 {
        if running_pid(&opts.pid_file).is_none() {
            let _ = fs::remove_file(&opts.pid_file);
            return Ok(status(opts));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(Error::storage(format!(
        "codex-memoryd did not stop cleanly pid={pid}"
    )))
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

fn wait_for_ready(opts: &RuntimeOptions) -> Result<()> {
    wait_for_health(opts)?;
    let status_url = format!("{}/v1/status", opts.url.trim_end_matches('/'));
    for _ in 0..30 {
        if let Ok(body) = http_get(&status_url) {
            let parsed: serde_json::Value = serde_json::from_str(&body)?;
            if parsed
                .pointer("/data/status")
                .and_then(|v| v.as_str())
                .is_some_and(|status| status == "local_only")
            {
                return Ok(());
            }
            if let Some(status) = parsed.pointer("/data/status").and_then(|v| v.as_str()) {
                return Err(Error::storage(format!(
                    "daemon status is '{status}', expected local_only"
                )));
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(Error::storage(format!(
        "timed out waiting for {status_url}"
    )))
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
        db: format!("{}:/data/memory.db", opts.home.display()),
        log_file: format!(
            "{} logs {}",
            container_runtime_name(opts),
            opts.container_name
        ),
        health: health.to_string(),
    }
}

fn running_pid(pid_file: &Path) -> Option<u32> {
    let raw = fs::read_to_string(pid_file).ok()?;
    let pid = raw.trim().parse::<u32>().ok()?;
    let ok = Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success();
    ok.then_some(pid)
}

fn container_runtime(opts: &RuntimeOptions) -> Result<String> {
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
