//! Provider configuration (SPEC §11.2). Resolution order is: built-in defaults
//! → config file (`~/.codex-memoryd/config.toml` by default) → environment
//! variables → explicit CLI flags. Later sources win.

use serde::Deserialize;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;

use crate::error::Error;
use crate::error::Result;
use crate::hybrid_retrieval::{
    DEFAULT_HYBRID_BACKEND, DEFAULT_HYBRID_DIMS, DEFAULT_HYBRID_FUSION_K,
};

pub const DEFAULT_BIND: &str = "127.0.0.1:8787";
pub const DEFAULT_MAX_RECALL_TOKENS: usize = 1200;
pub const DEFAULT_MAX_RECORD_CHARS: usize = 8_000;

/// Raw config as parsed from a TOML file. All fields optional so partial files
/// merge cleanly onto defaults.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileConfig {
    #[serde(default)]
    pub server: ServerSection,
    #[serde(default)]
    pub runtime: RuntimeSection,
    #[serde(default)]
    pub storage: StorageSection,
    #[serde(default)]
    pub policy: PolicySection,
    #[serde(default)]
    pub recall: RecallSection,
    #[serde(default)]
    pub dream: DreamSection,
    #[serde(default)]
    pub log: LogSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServerSection {
    pub bind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuntimeSection {
    #[serde(default)]
    pub adjacent: AdjacentRuntimeSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AdjacentRuntimeSection {
    pub enabled: Option<bool>,
    pub name: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StorageSection {
    pub kind: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PolicySection {
    pub default_profile: Option<String>,
    pub default_workspace: Option<String>,
    pub cross_profile_policy: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecallSection {
    pub max_tokens: Option<usize>,
    pub max_record_chars: Option<usize>,
    #[serde(default)]
    pub hybrid: HybridRecallSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HybridRecallSection {
    pub enabled: Option<bool>,
    pub backend: Option<String>,
    pub dims: Option<usize>,
    pub fusion_k: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DreamSection {
    pub scheduler_enabled: Option<bool>,
    pub scheduler_interval_seconds: Option<u64>,
    pub idle_window_seconds: Option<i64>,
    pub min_session_age_seconds: Option<i64>,
    pub min_turn_count: Option<usize>,
    pub max_batch_size: Option<usize>,
    pub max_candidates: Option<usize>,
    pub max_runtime_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub struct DreamSchedulerConfig {
    pub enabled: bool,
    pub interval_seconds: u64,
    pub idle_window_seconds: i64,
    pub min_session_age_seconds: i64,
    pub min_turn_count: usize,
    pub max_batch_size: usize,
    pub max_candidates: usize,
    pub max_runtime_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct HybridRecallConfig {
    pub enabled: bool,
    pub backend: String,
    pub dims: usize,
    pub fusion_k: usize,
}

impl Default for HybridRecallConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: DEFAULT_HYBRID_BACKEND.to_string(),
            dims: DEFAULT_HYBRID_DIMS,
            fusion_k: DEFAULT_HYBRID_FUSION_K,
        }
    }
}

impl HybridRecallConfig {
    fn validate(&self) -> Result<()> {
        if self.dims == 0 {
            return Err(Error::invalid_request("recall.hybrid.dims must be > 0"));
        }
        if self.fusion_k == 0 {
            return Err(Error::invalid_request("recall.hybrid.fusion_k must be > 0"));
        }
        if self.backend != DEFAULT_HYBRID_BACKEND {
            return Err(Error::invalid_request(format!(
                "unsupported recall.hybrid.backend '{}' (only '{}' supported)",
                self.backend, DEFAULT_HYBRID_BACKEND
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogSection {
    pub level: Option<String>,
    pub format: Option<String>,
}

/// Fully-resolved, validated config used at runtime.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub adjacent_runtime: AdjacentRuntimeConfig,
    pub storage_kind: String,
    pub storage_path: PathBuf,
    pub default_profile: String,
    pub default_workspace: String,
    pub cross_profile_policy: String,
    pub max_recall_tokens: usize,
    pub max_record_chars: usize,
    pub hybrid_recall: HybridRecallConfig,
    pub dream_scheduler: DreamSchedulerConfig,
    pub log_level: String,
    pub log_format: String,
    /// Operator declaration that a non-loopback *process* bind is nonetheless
    /// only reachable over loopback because it sits behind a loopback-only
    /// publish/front door (e.g. Docker `127.0.0.1:8787->8787`). This does NOT
    /// change what the daemon binds; it only lets status report `local_only`
    /// instead of `auth_missing` when the operator has asserted the network
    /// boundary out-of-band. Opt-in, default false.
    pub declare_loopback_publish: bool,
}

#[derive(Debug, Clone)]
pub struct AdjacentRuntimeConfig {
    pub enabled: bool,
    pub name: String,
    pub url: Option<String>,
}

impl Default for AdjacentRuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            name: "adjacent-app".to_string(),
            url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalEndpointHost {
    Localhost,
    Ip(IpAddr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalHttpEndpoint {
    pub host: LocalEndpointHost,
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: DEFAULT_BIND.to_string(),
            adjacent_runtime: AdjacentRuntimeConfig::default(),
            storage_kind: "sqlite".to_string(),
            storage_path: default_storage_path(),
            default_profile: "personal".to_string(),
            default_workspace: "default".to_string(),
            cross_profile_policy: "default_deny".to_string(),
            max_recall_tokens: DEFAULT_MAX_RECALL_TOKENS,
            max_record_chars: DEFAULT_MAX_RECORD_CHARS,
            hybrid_recall: HybridRecallConfig::default(),
            dream_scheduler: DreamSchedulerConfig {
                enabled: false,
                interval_seconds: 3600,
                idle_window_seconds: 900,
                min_session_age_seconds: 300,
                min_turn_count: 2,
                max_batch_size: 500,
                max_candidates: 50,
                max_runtime_seconds: 30,
            },
            log_level: "info".to_string(),
            log_format: "text".to_string(),
            declare_loopback_publish: false,
        }
    }
}

/// Optional CLI overrides applied last (highest precedence).
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub bind: Option<String>,
    pub storage_path: Option<PathBuf>,
    pub default_profile: Option<String>,
    pub default_workspace: Option<String>,
    pub log_level: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigLoadSource<'a> {
    Default,
    ExplicitRequired(&'a Path),
    ExplicitOptional(&'a Path),
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.codex-memoryd/config.toml`
pub fn default_config_path() -> PathBuf {
    default_home_dir().join("config.toml")
}

/// `~/.codex-memoryd/memory.db`
pub fn default_storage_path() -> PathBuf {
    default_home_dir().join("memory.db")
}

/// Runtime home for product CLI mode. Defaults to `~/.codex-memoryd`, with
/// `CODEX_MEMORYD_HOME` as the operator-level override.
pub fn default_home_dir() -> PathBuf {
    std::env::var("CODEX_MEMORYD_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| expand_path(&v))
        .unwrap_or_else(|| home_dir().join(".codex-memoryd"))
}

/// Expand a leading `~` and environment references in a path string.
pub fn expand_path(raw: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(raw).into_owned())
}

impl Config {
    /// Load config with full precedence resolution.
    ///
    /// * `config_path` — explicit path to a TOML file, or None to use the
    ///   default location (which is optional: a missing default file is fine).
    /// * env vars: `CODEX_MEMORYD_BIND`, `CODEX_MEMORYD_DB`,
    ///   `CODEX_MEMORYD_PROFILE`, `CODEX_MEMORYD_WORKSPACE`,
    ///   `CODEX_MEMORYD_LOG`, and `CODEX_MEMORYD_DREAM_*` scheduler controls.
    /// * `overrides` — explicit CLI flags.
    pub fn load(config_path: Option<&Path>, overrides: &CliOverrides) -> Result<Config> {
        let source = match config_path {
            Some(path) => ConfigLoadSource::ExplicitRequired(path),
            None => ConfigLoadSource::Default,
        };
        Self::load_from_source(source, overrides)
    }

    /// Load config using an explicit source contract.
    ///
    /// `Default` discovers the optional default config, `ExplicitRequired`
    /// errors when its selected file is absent, and `ExplicitOptional` only
    /// inspects its selected file when present without falling back to the
    /// default config path.
    pub fn load_from_source(
        source: ConfigLoadSource<'_>,
        overrides: &CliOverrides,
    ) -> Result<Config> {
        let mut config = Config::default();

        // 1. Config file.
        let (path, required) = match source {
            ConfigLoadSource::Default => (default_config_path(), false),
            ConfigLoadSource::ExplicitRequired(path) => (path.to_path_buf(), true),
            ConfigLoadSource::ExplicitOptional(path) => (path.to_path_buf(), false),
        };
        if path.exists() {
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                Error::invalid_request(format!("read config {}: {e}", path.display()))
            })?;
            let file: FileConfig = toml::from_str(&raw).map_err(|e| {
                Error::invalid_request(format!("parse config {}: {e}", path.display()))
            })?;
            config.merge_file(file);
        } else if required {
            return Err(Error::invalid_request(format!(
                "config file not found: {}",
                path.display()
            )));
        }

        // 2. Environment variables.
        if let Ok(bind) = std::env::var("CODEX_MEMORYD_BIND") {
            if !bind.trim().is_empty() {
                config.bind = bind;
            }
        }
        if let Ok(db) = std::env::var("CODEX_MEMORYD_DB") {
            if !db.trim().is_empty() {
                config.storage_path = expand_path(&db);
            }
        }
        if let Ok(profile) = std::env::var("CODEX_MEMORYD_PROFILE") {
            if !profile.trim().is_empty() {
                config.default_profile = profile;
            }
        }
        if let Ok(ws) = std::env::var("CODEX_MEMORYD_WORKSPACE") {
            if !ws.trim().is_empty() {
                config.default_workspace = ws;
            }
        }
        if let Ok(level) = std::env::var("CODEX_MEMORYD_LOG") {
            if !level.trim().is_empty() {
                config.log_level = level;
            }
        }
        apply_dream_env(&mut config)?;
        apply_hybrid_env(&mut config)?;
        // Opt-in operator declaration that a non-loopback process bind is fronted
        // by a loopback-only publish (e.g. Docker `127.0.0.1:8787->8787`). Accepts
        // 1/true/yes. Default off — only honored when explicitly set.
        if let Ok(v) = std::env::var("CODEX_MEMORYD_DECLARE_LOOPBACK_PUBLISH") {
            config.declare_loopback_publish =
                matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }

        // 3. CLI overrides (highest precedence).
        if let Some(bind) = &overrides.bind {
            config.bind = bind.clone();
        }
        if let Some(path) = &overrides.storage_path {
            config.storage_path = path.clone();
        }
        if let Some(profile) = &overrides.default_profile {
            config.default_profile = profile.clone();
        }
        if let Some(ws) = &overrides.default_workspace {
            config.default_workspace = ws.clone();
        }
        if let Some(level) = &overrides.log_level {
            config.log_level = level.clone();
        }

        config.validate()?;
        Ok(config)
    }

    fn merge_file(&mut self, file: FileConfig) {
        if let Some(bind) = file.server.bind {
            self.bind = bind;
        }
        if let Some(enabled) = file.runtime.adjacent.enabled {
            self.adjacent_runtime.enabled = enabled;
        }
        if let Some(name) = file.runtime.adjacent.name {
            self.adjacent_runtime.name = name;
        }
        if let Some(url) = file.runtime.adjacent.url {
            self.adjacent_runtime.url = clean_env_value(url);
        }
        if let Some(kind) = file.storage.kind {
            self.storage_kind = kind;
        }
        if let Some(path) = file.storage.path {
            self.storage_path = expand_path(&path);
        }
        if let Some(p) = file.policy.default_profile {
            self.default_profile = p;
        }
        if let Some(w) = file.policy.default_workspace {
            self.default_workspace = w;
        }
        if let Some(c) = file.policy.cross_profile_policy {
            self.cross_profile_policy = c;
        }
        if let Some(t) = file.recall.max_tokens {
            self.max_recall_tokens = t;
        }
        if let Some(c) = file.recall.max_record_chars {
            self.max_record_chars = c;
        }
        if let Some(enabled) = file.recall.hybrid.enabled {
            self.hybrid_recall.enabled = enabled;
        }
        if let Some(backend) = file.recall.hybrid.backend {
            self.hybrid_recall.backend = backend;
        }
        if let Some(dims) = file.recall.hybrid.dims {
            self.hybrid_recall.dims = dims;
        }
        if let Some(fusion_k) = file.recall.hybrid.fusion_k {
            self.hybrid_recall.fusion_k = fusion_k;
        }
        if let Some(enabled) = file.dream.scheduler_enabled {
            self.dream_scheduler.enabled = enabled;
        }
        if let Some(seconds) = file.dream.scheduler_interval_seconds {
            self.dream_scheduler.interval_seconds = seconds;
        }
        if let Some(seconds) = file.dream.idle_window_seconds {
            self.dream_scheduler.idle_window_seconds = seconds;
        }
        if let Some(seconds) = file.dream.min_session_age_seconds {
            self.dream_scheduler.min_session_age_seconds = seconds;
        }
        if let Some(count) = file.dream.min_turn_count {
            self.dream_scheduler.min_turn_count = count;
        }
        if let Some(size) = file.dream.max_batch_size {
            self.dream_scheduler.max_batch_size = size;
        }
        if let Some(count) = file.dream.max_candidates {
            self.dream_scheduler.max_candidates = count;
        }
        if let Some(seconds) = file.dream.max_runtime_seconds {
            self.dream_scheduler.max_runtime_seconds = seconds;
        }
        if let Some(l) = file.log.level {
            self.log_level = l;
        }
        if let Some(f) = file.log.format {
            self.log_format = f;
        }
    }

    fn validate(&self) -> Result<()> {
        if self.storage_kind != "sqlite" {
            return Err(Error::invalid_request(format!(
                "unsupported storage kind '{}' (only 'sqlite' is supported)",
                self.storage_kind
            )));
        }
        if self.adjacent_runtime.enabled && self.adjacent_runtime.url.is_none() {
            return Err(Error::invalid_request(
                "runtime.adjacent.url must be set when runtime.adjacent.enabled = true",
            ));
        }
        if let Some(url) = self.adjacent_runtime.url.as_deref() {
            if parse_local_http_endpoint(url).is_none() {
                return Err(Error::invalid_request(
                    "runtime.adjacent.url must be a local http(s)://HOST:PORT endpoint",
                ));
            }
        }
        if crate::domain::Profile::parse(&self.default_profile).is_none() {
            return Err(Error::invalid_request(format!(
                "invalid default_profile '{}'",
                self.default_profile
            )));
        }
        if self.max_recall_tokens == 0 {
            return Err(Error::invalid_request("recall.max_tokens must be > 0"));
        }
        if self.dream_scheduler.interval_seconds == 0 {
            return Err(Error::invalid_request(
                "dream.scheduler_interval_seconds must be > 0",
            ));
        }
        if self.dream_scheduler.max_batch_size == 0 {
            return Err(Error::invalid_request("dream.max_batch_size must be > 0"));
        }
        self.hybrid_recall.validate()?;
        Ok(())
    }

    /// Whether the configured daemon bind is loopback-only. This intentionally
    /// treats unresolved hostnames as non-loopback, except for `localhost`.
    pub fn bind_is_loopback(&self) -> bool {
        bind_host(&self.bind).is_some_and(|host| {
            if host.eq_ignore_ascii_case("localhost") {
                return true;
            }
            host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        })
    }

    /// Whether the daemon's `/v1` surface is effectively reachable only over
    /// loopback: either the process binds loopback directly, or the operator
    /// has explicitly declared a loopback-only publish/front door
    /// (`declare_loopback_publish`, the Docker `127.0.0.1:8787->8787` case).
    /// Used by status to decide `local_only` vs `auth_missing`.
    pub fn effective_loopback_only(&self) -> bool {
        self.bind_is_loopback() || self.declare_loopback_publish
    }
}

fn clean_env_value(value: String) -> Option<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(clean_env_value)
}

fn parse_bool_value(key: &str, value: Option<String>) -> Result<Option<bool>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(Error::invalid_request(format!(
            "{key} must be one of 1/0/true/false/yes/no/on/off"
        ))),
    }
}

fn parse_u64_value(key: &str, value: Option<String>) -> Result<Option<u64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    value.parse::<u64>().map(Some).map_err(|_| {
        Error::invalid_request(format!("{key} must be an unsigned integer, got '{value}'"))
    })
}

fn parse_i64_value(key: &str, value: Option<String>) -> Result<Option<i64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    value
        .parse::<i64>()
        .map(Some)
        .map_err(|_| Error::invalid_request(format!("{key} must be an integer, got '{value}'")))
}

fn parse_usize_value(key: &str, value: Option<String>) -> Result<Option<usize>> {
    let Some(value) = value else {
        return Ok(None);
    };
    value.parse::<usize>().map(Some).map_err(|_| {
        Error::invalid_request(format!("{key} must be an unsigned integer, got '{value}'"))
    })
}

fn apply_dream_env(config: &mut Config) -> Result<()> {
    apply_dream_env_from(config, env_value)
}

fn apply_dream_env_from<F>(config: &mut Config, get: F) -> Result<()>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(enabled) = parse_bool_value(
        "CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED",
        get("CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED").and_then(clean_env_value),
    )? {
        config.dream_scheduler.enabled = enabled;
    }
    if let Some(seconds) = parse_u64_value(
        "CODEX_MEMORYD_DREAM_SCHEDULER_INTERVAL_SECONDS",
        get("CODEX_MEMORYD_DREAM_SCHEDULER_INTERVAL_SECONDS").and_then(clean_env_value),
    )? {
        config.dream_scheduler.interval_seconds = seconds;
    }
    if let Some(seconds) = parse_i64_value(
        "CODEX_MEMORYD_DREAM_IDLE_WINDOW_SECONDS",
        get("CODEX_MEMORYD_DREAM_IDLE_WINDOW_SECONDS").and_then(clean_env_value),
    )? {
        config.dream_scheduler.idle_window_seconds = seconds;
    }
    if let Some(seconds) = parse_i64_value(
        "CODEX_MEMORYD_DREAM_MIN_SESSION_AGE_SECONDS",
        get("CODEX_MEMORYD_DREAM_MIN_SESSION_AGE_SECONDS").and_then(clean_env_value),
    )? {
        config.dream_scheduler.min_session_age_seconds = seconds;
    }
    if let Some(count) = parse_usize_value(
        "CODEX_MEMORYD_DREAM_MIN_TURN_COUNT",
        get("CODEX_MEMORYD_DREAM_MIN_TURN_COUNT").and_then(clean_env_value),
    )? {
        config.dream_scheduler.min_turn_count = count;
    }
    if let Some(size) = parse_usize_value(
        "CODEX_MEMORYD_DREAM_MAX_BATCH_SIZE",
        get("CODEX_MEMORYD_DREAM_MAX_BATCH_SIZE").and_then(clean_env_value),
    )? {
        config.dream_scheduler.max_batch_size = size;
    }
    if let Some(count) = parse_usize_value(
        "CODEX_MEMORYD_DREAM_MAX_CANDIDATES",
        get("CODEX_MEMORYD_DREAM_MAX_CANDIDATES").and_then(clean_env_value),
    )? {
        config.dream_scheduler.max_candidates = count;
    }
    if let Some(seconds) = parse_u64_value(
        "CODEX_MEMORYD_DREAM_MAX_RUNTIME_SECONDS",
        get("CODEX_MEMORYD_DREAM_MAX_RUNTIME_SECONDS").and_then(clean_env_value),
    )? {
        config.dream_scheduler.max_runtime_seconds = seconds;
    }
    Ok(())
}

fn apply_hybrid_env(config: &mut Config) -> Result<()> {
    apply_hybrid_env_from(config, env_value)
}

fn apply_hybrid_env_from<F>(config: &mut Config, get: F) -> Result<()>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(enabled) = parse_bool_value(
        "CODEX_MEMORYD_RECALL_HYBRID_ENABLED",
        get("CODEX_MEMORYD_RECALL_HYBRID_ENABLED").and_then(clean_env_value),
    )? {
        config.hybrid_recall.enabled = enabled;
    }
    if let Some(backend) = get("CODEX_MEMORYD_RECALL_HYBRID_BACKEND").and_then(clean_env_value) {
        config.hybrid_recall.backend = backend;
    }
    if let Some(dims) = parse_usize_value(
        "CODEX_MEMORYD_RECALL_HYBRID_DIMS",
        get("CODEX_MEMORYD_RECALL_HYBRID_DIMS").and_then(clean_env_value),
    )? {
        config.hybrid_recall.dims = dims;
    }
    if let Some(fusion_k) = parse_usize_value(
        "CODEX_MEMORYD_RECALL_HYBRID_FUSION_K",
        get("CODEX_MEMORYD_RECALL_HYBRID_FUSION_K").and_then(clean_env_value),
    )? {
        config.hybrid_recall.fusion_k = fusion_k;
    }
    Ok(())
}

fn bind_host(bind: &str) -> Option<String> {
    if let Ok(addr) = bind.parse::<SocketAddr>() {
        return Some(match addr {
            SocketAddr::V4(addr) => addr.ip().to_string(),
            SocketAddr::V6(addr) => addr.ip().to_string(),
        });
    }
    if bind.starts_with('[') {
        return bind
            .find(']')
            .map(|end| bind[1..end].to_string())
            .filter(|host| !host.is_empty());
    }
    bind.rsplit_once(':')
        .map(|(host, _)| host.trim().to_string())
}

pub(crate) fn parse_local_http_endpoint(url: &str) -> Option<LocalHttpEndpoint> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    if let Some(port) = authority
        .strip_prefix("localhost:")
        .and_then(|port| port.parse::<u16>().ok())
    {
        return Some(LocalHttpEndpoint {
            host: LocalEndpointHost::Localhost,
            port,
        });
    }
    let socket = authority.parse::<std::net::SocketAddr>().ok()?;
    let host = match socket.ip() {
        IpAddr::V4(ip) if ip.is_loopback() => LocalEndpointHost::Ip(IpAddr::V4(ip)),
        IpAddr::V6(ip) if ip.is_loopback() => LocalEndpointHost::Ip(IpAddr::V6(ip)),
        _ => return None,
    };
    Some(LocalHttpEndpoint {
        host,
        port: socket.port(),
    })
}

pub fn adjacent_runtime_conflicts(memoryd_bind: &str, adjacent_url: &str) -> bool {
    let Some(adjacent) = parse_local_http_endpoint(adjacent_url) else {
        return false;
    };
    let Some((memoryd_host, memoryd_port)) =
        bind_host(memoryd_bind).and_then(|host| bind_port(memoryd_bind).map(|port| (host, port)))
    else {
        return false;
    };
    if memoryd_port != adjacent.port {
        return false;
    }
    if memoryd_host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let Ok(memoryd_ip) = memoryd_host.parse::<IpAddr>() else {
        return false;
    };
    if memoryd_ip.is_unspecified() {
        return true;
    }
    match (&adjacent.host, memoryd_ip) {
        (LocalEndpointHost::Localhost, ip) => ip.is_loopback(),
        (LocalEndpointHost::Ip(adjacent_ip), ip) => adjacent_ip == &ip,
    }
}

fn bind_port(bind: &str) -> Option<u16> {
    if bind.starts_with('[') {
        let (_, port) = bind.rsplit_once("]:")?;
        return port.parse().ok();
    }
    bind.rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let cfg = Config::default();
        cfg.validate().expect("defaults must validate");
        assert_eq!(cfg.bind, DEFAULT_BIND);
        assert_eq!(cfg.cross_profile_policy, "default_deny");
        assert!(!cfg.hybrid_recall.enabled);
        assert_eq!(cfg.hybrid_recall.backend, DEFAULT_HYBRID_BACKEND);
        assert_eq!(cfg.hybrid_recall.dims, DEFAULT_HYBRID_DIMS);
        assert_eq!(cfg.hybrid_recall.fusion_k, DEFAULT_HYBRID_FUSION_K);
    }

    #[test]
    fn cli_overrides_win() {
        let overrides = CliOverrides {
            bind: Some("0.0.0.0:9999".to_string()),
            default_profile: Some("work".to_string()),
            ..Default::default()
        };
        let cfg = Config::load(None, &overrides).expect("load");
        assert_eq!(cfg.bind, "0.0.0.0:9999");
        assert_eq!(cfg.default_profile, "work");
    }

    #[test]
    fn bind_loopback_detection_covers_supported_local_modes() {
        let mut cfg = Config::default();
        assert!(cfg.bind_is_loopback());

        cfg.bind = "localhost:8787".to_string();
        assert!(cfg.bind_is_loopback());

        cfg.bind = "[::1]:8787".to_string();
        assert!(cfg.bind_is_loopback());

        cfg.bind = "0.0.0.0:8787".to_string();
        assert!(!cfg.bind_is_loopback());

        cfg.bind = "[::1:8787".to_string();
        assert!(!cfg.bind_is_loopback());

        cfg.bind = "not-a-bind".to_string();
        assert!(!cfg.bind_is_loopback());
    }

    #[test]
    fn declare_loopback_publish_only_affects_effective_loopback() {
        let mut cfg = Config::default();
        // Loopback bind: effectively loopback regardless of the declaration.
        assert!(cfg.bind_is_loopback());
        assert!(cfg.effective_loopback_only());

        // Non-loopback process bind (the Docker 0.0.0.0 case), no declaration:
        // still reported as not-loopback (auth_missing).
        cfg.bind = "0.0.0.0:8787".to_string();
        assert!(!cfg.bind_is_loopback());
        assert!(!cfg.effective_loopback_only());

        // Operator declares a loopback-only publish/front door: effective
        // loopback flips true, but the raw process bind detection is unchanged
        // (so the HTTP /v1 transport gate, which uses bind_is_loopback, is NOT
        // loosened).
        cfg.declare_loopback_publish = true;
        assert!(!cfg.bind_is_loopback(), "raw bind detection unchanged");
        assert!(
            cfg.effective_loopback_only(),
            "declaration makes it effective"
        );
    }

    #[test]
    fn env_can_enable_dream_scheduler_for_compose() {
        let mut cfg = Config::default();
        apply_dream_env_from(&mut cfg, |key| {
            Some(
                match key {
                    "CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED" => "1",
                    "CODEX_MEMORYD_DREAM_SCHEDULER_INTERVAL_SECONDS" => "120",
                    "CODEX_MEMORYD_DREAM_IDLE_WINDOW_SECONDS" => "30",
                    "CODEX_MEMORYD_DREAM_MIN_SESSION_AGE_SECONDS" => "15",
                    "CODEX_MEMORYD_DREAM_MIN_TURN_COUNT" => "3",
                    "CODEX_MEMORYD_DREAM_MAX_BATCH_SIZE" => "25",
                    "CODEX_MEMORYD_DREAM_MAX_CANDIDATES" => "7",
                    "CODEX_MEMORYD_DREAM_MAX_RUNTIME_SECONDS" => "9",
                    _ => return None,
                }
                .to_string(),
            )
        })
        .expect("apply dream env");

        assert!(cfg.dream_scheduler.enabled);
        assert_eq!(cfg.dream_scheduler.interval_seconds, 120);
        assert_eq!(cfg.dream_scheduler.idle_window_seconds, 30);
        assert_eq!(cfg.dream_scheduler.min_session_age_seconds, 15);
        assert_eq!(cfg.dream_scheduler.min_turn_count, 3);
        assert_eq!(cfg.dream_scheduler.max_batch_size, 25);
        assert_eq!(cfg.dream_scheduler.max_candidates, 7);
        assert_eq!(cfg.dream_scheduler.max_runtime_seconds, 9);
    }

    #[test]
    fn invalid_dream_env_value_is_rejected() {
        let mut cfg = Config::default();
        let err = apply_dream_env_from(&mut cfg, |key| {
            (key == "CODEX_MEMORYD_DREAM_MAX_CANDIDATES").then(|| "many".to_string())
        })
        .expect_err("invalid env");
        assert!(err
            .to_string()
            .contains("CODEX_MEMORYD_DREAM_MAX_CANDIDATES"));
    }

    #[test]
    fn recall_hybrid_file_config_can_enable() {
        let file: FileConfig = toml::from_str(
            r#"
[recall.hybrid]
enabled = true
backend = "local_sparse_hash"
dims = 128
fusion_k = 42
"#,
        )
        .expect("parse file config");
        let mut cfg = Config::default();
        cfg.merge_file(file);
        assert!(cfg.hybrid_recall.enabled);
        assert_eq!(cfg.hybrid_recall.backend, DEFAULT_HYBRID_BACKEND);
        assert_eq!(cfg.hybrid_recall.dims, 128);
        assert_eq!(cfg.hybrid_recall.fusion_k, 42);
    }

    #[test]
    fn recall_hybrid_env_can_enable() {
        let mut cfg = Config::default();
        apply_hybrid_env_from(&mut cfg, |key| match key {
            "CODEX_MEMORYD_RECALL_HYBRID_ENABLED" => Some("1".to_string()),
            "CODEX_MEMORYD_RECALL_HYBRID_BACKEND" => Some(DEFAULT_HYBRID_BACKEND.to_string()),
            "CODEX_MEMORYD_RECALL_HYBRID_DIMS" => Some("96".to_string()),
            "CODEX_MEMORYD_RECALL_HYBRID_FUSION_K" => Some("77".to_string()),
            _ => None,
        })
        .expect("apply env");
        assert!(cfg.hybrid_recall.enabled);
        assert_eq!(cfg.hybrid_recall.backend, DEFAULT_HYBRID_BACKEND);
        assert_eq!(cfg.hybrid_recall.dims, 96);
        assert_eq!(cfg.hybrid_recall.fusion_k, 77);
    }

    #[test]
    fn adjacent_runtime_defaults_to_disabled() {
        let cfg = Config::default();
        assert!(!cfg.adjacent_runtime.enabled);
        assert_eq!(cfg.adjacent_runtime.name, "adjacent-app");
        assert_eq!(cfg.adjacent_runtime.url, None);
    }

    #[test]
    fn adjacent_runtime_file_config_is_explicit_only() {
        let file: FileConfig = toml::from_str(
            r#"
[runtime.adjacent]
enabled = true
name = "dogfood-router"
url = "http://127.0.0.1:4318"
"#,
        )
        .expect("parse file config");
        let mut cfg = Config::default();
        cfg.merge_file(file);
        cfg.validate().expect("valid adjacent runtime");
        assert!(cfg.adjacent_runtime.enabled);
        assert_eq!(cfg.adjacent_runtime.name, "dogfood-router");
        assert_eq!(
            cfg.adjacent_runtime.url.as_deref(),
            Some("http://127.0.0.1:4318")
        );
    }

    #[test]
    fn adjacent_runtime_enabled_requires_url() {
        let file: FileConfig = toml::from_str(
            r#"
[runtime.adjacent]
enabled = true
"#,
        )
        .expect("parse file config");
        let mut cfg = Config::default();
        cfg.merge_file(file);
        let err = cfg.validate().expect_err("missing adjacent url");
        assert!(err.to_string().contains("runtime.adjacent.url"));
    }

    #[test]
    fn adjacent_runtime_rejects_non_local_url() {
        let file: FileConfig = toml::from_str(
            r#"
[runtime.adjacent]
enabled = true
url = "https://example.com:443"
"#,
        )
        .expect("parse file config");
        let mut cfg = Config::default();
        cfg.merge_file(file);
        let err = cfg.validate().expect_err("non-local url must fail");
        assert!(err.to_string().contains("local http(s)://HOST:PORT"));
    }

    #[test]
    fn adjacent_runtime_conflict_treats_wildcard_bind_as_loopback_conflict() {
        assert!(adjacent_runtime_conflicts(
            "0.0.0.0:8787",
            "http://127.0.0.1:8787"
        ));
        assert!(adjacent_runtime_conflicts(
            "[::]:8787",
            "https://[::1]:8787"
        ));
        assert!(!adjacent_runtime_conflicts(
            "127.0.0.1:8787",
            "http://127.0.0.1:4318"
        ));
    }
}
