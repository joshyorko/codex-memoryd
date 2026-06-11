//! Command-line interface (clap). The CLI operates on the same
//! [`crate::service::Service`] as the HTTP server, so `recall`, `search`,
//! `sync-local`, `export`, and `forget` exercise identical code paths. `serve`
//! launches the daemon; `doctor` runs self-checks.

use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;
use serde_json::json;

use codex_memoryd::config::CliOverrides;
use codex_memoryd::config::Config;
use codex_memoryd::domain;
use codex_memoryd::error;
use codex_memoryd::error::Result;
use codex_memoryd::ingest::ArtifactKind;
use codex_memoryd::protocol::*;
use codex_memoryd::server;
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;

#[derive(Parser, Debug)]
#[command(
    name = "codex-memoryd",
    version,
    about = "Codex-native portable memory provider daemon + CLI"
)]
pub struct Cli {
    /// Path to config file (defaults to ~/.codex-memoryd/config.toml if present).
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Override storage database path.
    #[arg(long, global = true, value_name = "FILE", env = "CODEX_MEMORYD_DB")]
    pub db: Option<PathBuf>,

    /// Override log level (error|warn|info|debug|trace).
    #[arg(long, global = true, env = "CODEX_MEMORYD_LOG")]
    pub log: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the HTTP daemon.
    Serve {
        /// Bind address, e.g. 127.0.0.1:8787.
        #[arg(long, env = "CODEX_MEMORYD_BIND")]
        bind: Option<String>,
    },
    /// Print provider status as JSON.
    Status,
    /// Recall task-relevant memory.
    Recall {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        query: String,
        #[arg(long)]
        max_tokens: Option<usize>,
    },
    /// Search stored memory.
    Search {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        query: String,
        #[arg(long, value_name = "TYPE")]
        r#type: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        include_archived: bool,
    },
    /// Write a durable conclusion.
    Conclude {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        content: String,
    },
    /// Import local Codex memory from a directory (provider local-ingest mode).
    SyncLocal {
        /// Preview only (no durable writes).
        #[arg(long, conflicts_with = "apply")]
        preview: bool,
        /// Apply (write durable records, idempotent).
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        /// Source root, e.g. ~/.codex/memories.
        #[arg(value_name = "SOURCE_ROOT")]
        source_root: PathBuf,
    },
    /// Export safe records as JSONL (default) or JSON.
    Export {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        repo_id: Option<String>,
        #[arg(long)]
        include_archived: bool,
        #[arg(long, default_value = "jsonl")]
        format: String,
        #[arg(long)]
        target_profile: Option<String>,
    },
    /// Archive (default) or delete a memory record by id.
    Forget {
        /// Record id.
        id: String,
        #[arg(long)]
        profile: Option<String>,
        /// Hard delete instead of archive.
        #[arg(long)]
        delete: bool,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Run self-checks (storage writable, FTS5, schema).
    Doctor,
    /// Run the Dreamer loop in preview or apply mode.
    Dream {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, conflicts_with = "apply")]
        preview: bool,
        #[arg(long)]
        apply: bool,
        #[arg(long, conflicts_with_all = ["preview", "apply"])]
        scheduled: bool,
        #[arg(long)]
        now: Option<String>,
    },
}

impl Cli {
    fn overrides(&self, bind: Option<String>) -> CliOverrides {
        CliOverrides {
            bind,
            storage_path: self.db.clone(),
            default_profile: None,
            default_workspace: None,
            log_level: self.log.clone(),
        }
    }

    fn load_config(&self, bind: Option<String>) -> Result<Config> {
        Config::load(self.config.as_deref(), &self.overrides(bind))
    }

    fn open_service(&self, bind: Option<String>) -> Result<Service> {
        let config = self.load_config(bind)?;
        let store = Store::open(&config.storage_path)?;
        Ok(Service::new(store, config))
    }
}

/// Run the CLI. Returns process exit code.
pub fn run(cli: Cli) -> i32 {
    match dispatch(cli) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("error: {err}");
            // Map error categories to distinct exit codes for scripting.
            match err.code {
                error::ErrorCode::NotFound => 4,
                error::ErrorCode::ProfileBoundaryDenied
                | error::ErrorCode::SecretDetected
                | error::ErrorCode::PolicyDenied => 5,
                _ => 1,
            }
        }
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    match &cli.command {
        Command::Serve { bind } => {
            let service = cli.open_service(bind.clone())?;
            let bind_addr = service.config.bind.clone();
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| error::Error::internal(format!("tokio runtime: {e}")))?;
            runtime
                .block_on(server::serve(service, &bind_addr))
                .map_err(|e| error::Error::internal(e.to_string()))?;
            Ok(())
        }
        Command::Status => {
            let service = cli.open_service(None)?;
            let status = service.status()?;
            print_json(&status)?;
            Ok(())
        }
        Command::Dream {
            profile,
            workspace,
            repo,
            preview,
            apply,
            scheduled,
            now,
        } => {
            let service = cli.open_service(None)?;
            if *scheduled {
                let resp = service.scheduled_dream(now.clone())?;
                print_json(&resp)?;
                return Ok(());
            }
            let mode = if *apply {
                "apply"
            } else if *preview {
                "preview"
            } else {
                "preview"
            };
            let resp = service.dream(DreamRequest {
                profile: profile.clone(),
                workspace: workspace.clone(),
                repo: repo.clone().map(|repo_id| domain::RepoIdentity {
                    repo_id,
                    ..Default::default()
                }),
                mode: Some(mode.to_string()),
                now: now.clone(),
            })?;
            print_json(&resp)?;
            Ok(())
        }
        Command::Recall {
            profile,
            workspace,
            repo,
            query,
            max_tokens,
        } => {
            let service = cli.open_service(None)?;
            let req = RecallRequest {
                profile: profile.clone(),
                workspace: workspace.clone(),
                repo: repo.clone().map(|repo_id| domain::RepoIdentity {
                    repo_id,
                    ..Default::default()
                }),
                session: None,
                query: Some(query.clone()),
                files: vec![],
                max_tokens: *max_tokens,
                include_types: vec![],
                exclude_types: vec![],
                recency_days: None,
                metadata: None,
            };
            let resp = service.recall(req)?;
            print_json(&resp)?;
            Ok(())
        }
        Command::Search {
            profile,
            workspace,
            query,
            r#type,
            limit,
            include_archived,
        } => {
            let service = cli.open_service(None)?;
            let req = SearchRequest {
                profile: profile.clone(),
                workspace: workspace.clone(),
                repo: None,
                query: Some(query.clone()),
                scope: None,
                record_type: r#type.clone(),
                limit: *limit,
                include_archived: *include_archived,
                cursor: None,
            };
            let resp = service.search(req)?;
            print_json(&resp)?;
            Ok(())
        }
        Command::Conclude {
            profile,
            workspace,
            content,
        } => {
            let service = cli.open_service(None)?;
            let req = ConclusionsRequest {
                profile: profile.clone(),
                workspace: workspace.clone(),
                repo: None,
                target: Some("user".to_string()),
                conclusions: Some(vec![content.clone()]),
                metadata: None,
                record_type: None,
            };
            let resp = service.conclusions(req)?;
            print_json(&resp)?;
            Ok(())
        }
        Command::SyncLocal {
            preview,
            apply,
            profile,
            workspace,
            source_root,
        } => {
            let service = cli.open_service(None)?;
            let mode = if *apply { "apply" } else { "preview" };
            let _ = preview; // preview is the default; flag is for clarity
            let files = read_local_memory_files(source_root)?;
            let req = SyncRequest {
                profile: profile.clone(),
                workspace: workspace.clone(),
                repo: None,
                source_root: Some(source_root.display().to_string()),
                mode: Some(mode.to_string()),
                files: Some(files),
                metadata: None,
            };
            let resp = service.sync_local(req)?;
            print_json(&resp)?;
            Ok(())
        }
        Command::Export {
            profile,
            workspace,
            repo_id,
            include_archived,
            format,
            target_profile,
        } => {
            let service = cli.open_service(None)?;
            let query = ExportQuery {
                profile: profile.clone(),
                workspace: workspace.clone(),
                repo_id: repo_id.clone(),
                include_archived: Some(*include_archived),
                format: Some(format.clone()),
                target_profile: target_profile.clone(),
            };
            let result = service.export(query)?;
            // Print the raw export body to stdout (pipe to a file).
            print!("{}", result.body);
            eprintln!(
                "exported {} record(s); omitted {} secret, {} boundary",
                result.record_count, result.omitted_secret, result.omitted_boundary
            );
            Ok(())
        }
        Command::Forget {
            id,
            profile,
            delete,
            reason,
        } => {
            let service = cli.open_service(None)?;
            let req = ForgetRequest {
                profile: profile.clone(),
                workspace: None,
                ids: Some(vec![id.clone()]),
                mode: Some(if *delete { "delete" } else { "archive" }.to_string()),
                reason: reason.clone(),
            };
            let resp = service.forget(req)?;
            print_json(&resp)?;
            Ok(())
        }
        Command::Doctor => {
            let service = cli.open_service(None)?;
            let status = service.status()?;
            let report = json!({
                "storage_writable": status.storage.writable,
                "storage_path": status.storage.path,
                "fts5": status.features.get("fts5"),
                "dream_scheduler": status.features.get("dream_scheduler"),
                "schema_version": status.storage_schema_version,
                "status": status.status,
                "degraded_reasons": status.degraded_reasons,
                "record_count": service.store.count_records().unwrap_or(-1),
            });
            print_json(&report)?;
            if !status.storage.writable {
                return Err(error::Error::storage("storage is not writable"));
            }
            Ok(())
        }
    }
}

/// Read markdown files from a local Codex memories directory and build sync
/// file payloads. Mirrors the layout in SPEC §7.1. CLI may read the filesystem
/// directly (provider local-ingest mode, SPEC §6.6).
fn read_local_memory_files(root: &PathBuf) -> Result<Vec<SyncFile>> {
    if !root.exists() {
        return Err(error::Error::new(
            error::ErrorCode::SyncSourceInvalid,
            format!("source root does not exist: {}", root.display()),
        ));
    }
    let mut files = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| {
            error::Error::new(
                error::ErrorCode::SyncSourceInvalid,
                format!("read dir {}: {e}", dir.display()),
            )
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let kind = ArtifactKind::infer_from_path(&rel).as_str().to_string();
            files.push(SyncFile {
                path: rel,
                kind: Some(kind),
                content,
                hash: None,
                modified_at: None,
                idempotency_key: None,
                metadata: None,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    println!("{text}");
    Ok(())
}
