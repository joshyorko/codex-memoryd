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

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogSection {
    pub level: Option<String>,
    pub format: Option<String>,
}

/// Fully-resolved, validated config used at runtime.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub storage_kind: String,
    pub storage_path: PathBuf,
    pub default_profile: String,
    pub default_workspace: String,
    pub cross_profile_policy: String,
    pub max_recall_tokens: usize,
    pub max_record_chars: usize,
    pub dream_scheduler: DreamSchedulerConfig,
    pub log_level: String,
    pub log_format: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: DEFAULT_BIND.to_string(),
            storage_kind: "sqlite".to_string(),
            storage_path: default_storage_path(),
            default_profile: "personal".to_string(),
            default_workspace: "default".to_string(),
            cross_profile_policy: "default_deny".to_string(),
            max_recall_tokens: DEFAULT_MAX_RECALL_TOKENS,
            max_record_chars: DEFAULT_MAX_RECORD_CHARS,
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

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.codex-memoryd/config.toml`
pub fn default_config_path() -> PathBuf {
    home_dir().join(".codex-memoryd").join("config.toml")
}

/// `~/.codex-memoryd/memory.db`
pub fn default_storage_path() -> PathBuf {
    home_dir().join(".codex-memoryd").join("memory.db")
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
    ///   `CODEX_MEMORYD_LOG`.
    /// * `overrides` — explicit CLI flags.
    pub fn load(config_path: Option<&Path>, overrides: &CliOverrides) -> Result<Config> {
        let mut config = Config::default();

        // 1. Config file.
        let (path, required) = match config_path {
            Some(p) => (p.to_path_buf(), true),
            None => (default_config_path(), false),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let cfg = Config::default();
        cfg.validate().expect("defaults must validate");
        assert_eq!(cfg.bind, DEFAULT_BIND);
        assert_eq!(cfg.cross_profile_policy, "default_deny");
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
}
