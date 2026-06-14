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
use codex_memoryd::conformance;
use codex_memoryd::domain;
use codex_memoryd::error;
use codex_memoryd::error::Result;
use codex_memoryd::git_import;
use codex_memoryd::git_import::GitImportMode;
use codex_memoryd::git_import::GitImportParams;
use codex_memoryd::ingest::ArtifactKind;
use codex_memoryd::mcp;
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
        #[arg(long)]
        pack_mode: Option<String>,
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
    /// Create, list, or inspect durable subjects.
    Subject {
        #[command(subcommand)]
        command: SubjectCommand,
    },
    /// Create, list, or inspect subject episodes.
    Episode {
        #[command(subcommand)]
        command: EpisodeCommand,
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
    /// Import local Git commit trailers as evidence episodes.
    GitImport {
        /// Preview only (no durable writes).
        #[arg(long, conflicts_with = "apply")]
        preview: bool,
        /// Apply (write subject episodes, idempotent).
        #[arg(long)]
        apply: bool,
        /// Import a JSON or JSONL exported refs fixture instead of commit trailers.
        #[arg(long, value_name = "FILE")]
        refs_fixture: Option<PathBuf>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        /// Maximum commits to scan from HEAD.
        #[arg(long, default_value_t = 100)]
        max_count: usize,
        /// Local Git repository path.
        #[arg(value_name = "REPO")]
        repo: PathBuf,
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
    /// Show a generated card snapshot.
    Card {
        #[command(subcommand)]
        command: CardCommand,
    },
    /// Render generated adapter views.
    Adapter {
        #[command(subcommand)]
        command: AdapterCommand,
    },
    /// Run local conformance reports.
    Conformance {
        #[command(subcommand)]
        command: ConformanceCommand,
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
    /// Run MCP transport entrypoints.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Review, apply, explain, or rollback Dreamer-generated memory patches.
    Patch {
        #[command(subcommand)]
        command: PatchCommand,
    },
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
        #[arg(long)]
        since: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum McpCommand {
    /// Run the MCP server over stdio.
    Stdio {
        /// Expose only the read tools and reject write tool calls.
        #[arg(long)]
        read_only: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum PatchCommand {
    /// Preview the patch as JSON or Markdown.
    Preview {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        now: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Apply a patch after verifying the preview run id.
    Apply {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        now: Option<String>,
        #[arg(long)]
        since: Option<String>,
    },
    /// Explain a patch or memory record.
    Explain {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        memory_id: Option<String>,
    },
    /// Roll back a patch in preview or apply mode.
    Rollback {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        run_id: String,
        #[arg(long, conflicts_with = "apply")]
        preview: bool,
        #[arg(long)]
        apply: bool,
        #[arg(long, default_value = "json")]
        format: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum CardCommand {
    /// Show a deterministic summary card.
    Show {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        r#type: String,
        #[arg(long)]
        subject_id: Option<String>,
        #[arg(long, default_value = "json")]
        format: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum AdapterCommand {
    /// Export a generated adapter view to stdout.
    Export {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        target: String,
        #[arg(long)]
        subject_id: Option<String>,
        #[arg(long)]
        max_bytes: Option<usize>,
        #[arg(long, default_value = "markdown")]
        format: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConformanceCommand {
    /// Run the adapter conformance report.
    Adapters {
        #[arg(long, default_value = "json")]
        format: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SubjectCommand {
    /// Create a subject, idempotent by profile/workspace/key.
    Create {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        key: String,
        #[arg(long, default_value = "other")]
        kind: String,
        #[arg(long)]
        display_name: String,
    },
    /// List subjects in a workspace.
    List {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        kind: Option<String>,
    },
    /// Get a subject by id.
    Get {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum EpisodeCommand {
    /// Create an episode for a subject.
    Create {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        subject_id: String,
        #[arg(long)]
        source_kind: String,
        #[arg(long)]
        source_ref: String,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        started_at: Option<String>,
        #[arg(long)]
        ended_at: Option<String>,
        #[arg(long)]
        trust_level: Option<String>,
    },
    /// List episodes in a workspace, optionally filtered by subject.
    List {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        subject_id: Option<String>,
    },
    /// Get an episode by id.
    Get {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        id: String,
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

    pub(crate) fn open_service(&self, bind: Option<String>) -> Result<Service> {
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
        Command::Patch { command } => {
            let service = cli.open_service(None)?;
            match command {
                PatchCommand::Preview {
                    profile,
                    workspace,
                    repo,
                    now,
                    since,
                    format,
                } => {
                    let resp = service.patch_preview(DreamRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        repo: repo.clone().map(|repo_id| domain::RepoIdentity {
                            repo_id,
                            ..Default::default()
                        }),
                        mode: Some("preview".to_string()),
                        now: now.clone(),
                        since: since.clone(),
                    })?;
                    if format.eq_ignore_ascii_case("markdown") {
                        print_markdown(&resp.markdown);
                    } else {
                        print_json(&resp)?;
                    }
                }
                PatchCommand::Apply {
                    profile,
                    workspace,
                    repo,
                    run_id,
                    now,
                    since,
                } => {
                    let resp = service.patch_apply(MemoryPatchApplyRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        repo: repo.clone().map(|repo_id| domain::RepoIdentity {
                            repo_id,
                            ..Default::default()
                        }),
                        run_id: run_id.clone(),
                        now: now.clone(),
                        since: since.clone(),
                    })?;
                    print_json(&resp)?;
                }
                PatchCommand::Explain {
                    profile,
                    workspace,
                    repo,
                    run_id,
                    memory_id,
                } => {
                    let resp = service.patch_explain(MemoryPatchExplainRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        repo: repo.clone().map(|repo_id| domain::RepoIdentity {
                            repo_id,
                            ..Default::default()
                        }),
                        run_id: run_id.clone(),
                        memory_id: memory_id.clone(),
                    })?;
                    print_json(&resp)?;
                }
                PatchCommand::Rollback {
                    profile,
                    workspace,
                    repo,
                    run_id,
                    preview,
                    apply,
                    format,
                } => {
                    let resp = service.patch_rollback(MemoryPatchRollbackRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        repo: repo.clone().map(|repo_id| domain::RepoIdentity {
                            repo_id,
                            ..Default::default()
                        }),
                        run_id: run_id.clone(),
                        preview: *preview || !*apply,
                        now: None,
                    })?;
                    if format.eq_ignore_ascii_case("markdown") {
                        print_markdown(&resp.markdown);
                    } else {
                        print_json(&resp)?;
                    }
                }
            }
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
            since,
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
                since: since.clone(),
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
            pack_mode,
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
                pack_mode: pack_mode.clone(),
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
        Command::Subject { command } => {
            let service = cli.open_service(None)?;
            match command {
                SubjectCommand::Create {
                    profile,
                    workspace,
                    key,
                    kind,
                    display_name,
                } => {
                    let resp = service.create_subject(SubjectCreateRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        subject_key: Some(key.clone()),
                        kind: Some(kind.clone()),
                        display_name: Some(display_name.clone()),
                        metadata: None,
                    })?;
                    print_json(&resp)?;
                }
                SubjectCommand::List {
                    profile,
                    workspace,
                    kind,
                } => {
                    let resp = service.list_subjects(SubjectListRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        kind: kind.clone(),
                    })?;
                    print_json(&resp)?;
                }
                SubjectCommand::Get {
                    profile,
                    workspace,
                    id,
                } => {
                    let resp = service.get_subject(SubjectGetRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        id: Some(id.clone()),
                    })?;
                    print_json(&resp)?;
                }
            }
            Ok(())
        }
        Command::Episode { command } => {
            let service = cli.open_service(None)?;
            match command {
                EpisodeCommand::Create {
                    profile,
                    workspace,
                    subject_id,
                    source_kind,
                    source_ref,
                    summary,
                    status,
                    started_at,
                    ended_at,
                    trust_level,
                } => {
                    let resp = service.create_episode(EpisodeCreateRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        subject_id: Some(subject_id.clone()),
                        source_kind: Some(source_kind.clone()),
                        source_ref: Some(source_ref.clone()),
                        started_at: started_at.clone(),
                        ended_at: ended_at.clone(),
                        status: status.clone(),
                        summary: Some(summary.clone()),
                        trust_level: trust_level.clone(),
                        source_metadata: None,
                        metadata: None,
                    })?;
                    print_json(&resp)?;
                }
                EpisodeCommand::List {
                    profile,
                    workspace,
                    subject_id,
                } => {
                    let resp = service.list_episodes(EpisodeListRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        subject_id: subject_id.clone(),
                    })?;
                    print_json(&resp)?;
                }
                EpisodeCommand::Get {
                    profile,
                    workspace,
                    id,
                } => {
                    let resp = service.get_episode(EpisodeGetRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        id: Some(id.clone()),
                    })?;
                    print_json(&resp)?;
                }
            }
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
        Command::GitImport {
            preview,
            apply,
            profile,
            workspace,
            max_count,
            refs_fixture,
            repo,
        } => {
            let service = cli.open_service(None)?;
            let mode = if *apply {
                GitImportMode::Apply
            } else {
                GitImportMode::Preview
            };
            let _ = preview; // preview is the default; flag is for clarity
            let resp = git_import::run(
                &service,
                GitImportParams {
                    repo_path: repo,
                    refs_fixture: refs_fixture.as_deref(),
                    profile: profile.clone(),
                    workspace: workspace.clone(),
                    mode,
                    max_count: *max_count,
                },
            )?;
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
        Command::Card { command } => {
            let service = cli.open_service(None)?;
            match command {
                CardCommand::Show {
                    profile,
                    workspace,
                    r#type,
                    subject_id,
                    format,
                } => {
                    let resp = service.card_show(CardShowRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        r#type: r#type.clone(),
                        subject_id: subject_id.clone(),
                    })?;
                    let format = format.as_str().to_ascii_lowercase();
                    match format.as_str() {
                        "markdown" => print_markdown(&render_card_markdown(&resp)),
                        "json" => print_json(&resp)?,
                        _ => {
                            return Err(error::Error::invalid_request(format!(
                                "invalid --format '{format}'; expected 'json' or 'markdown'",
                                format = format
                            )))
                        }
                    }
                }
            }
            Ok(())
        }
        Command::Adapter { command } => {
            let service = cli.open_service(None)?;
            match command {
                AdapterCommand::Export {
                    profile,
                    workspace,
                    target,
                    subject_id,
                    max_bytes,
                    format,
                } => {
                    let resp = service.adapter_export(AdapterExportRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        target: target.clone(),
                        subject_id: subject_id.clone(),
                        max_bytes: *max_bytes,
                    })?;
                    let format = format.as_str().to_ascii_lowercase();
                    match format.as_str() {
                        "markdown" => print_markdown(&resp.markdown),
                        "json" => print_json(&resp)?,
                        _ => {
                            return Err(error::Error::invalid_request(format!(
                                "invalid --format '{format}'; expected 'json' or 'markdown'",
                                format = format
                            )))
                        }
                    }
                }
            }
            Ok(())
        }
        Command::Conformance { command } => match command {
            ConformanceCommand::Adapters { format } => {
                let report = conformance::run_adapter_conformance()?;
                let format = format.as_str().to_ascii_lowercase();
                match format.as_str() {
                    "markdown" => {
                        print_markdown(&conformance::render_adapter_conformance_markdown(&report))
                    }
                    "json" => print_json(&report)?,
                    _ => {
                        return Err(error::Error::invalid_request(format!(
                            "invalid --format '{format}'; expected 'json' or 'markdown'",
                            format = format
                        )))
                    }
                }
                Ok(())
            }
        },
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
                "last_dream": status.last_dream,
                "record_count": service.store.count_records().unwrap_or(-1),
            });
            print_json(&report)?;
            if !status.storage.writable {
                return Err(error::Error::storage("storage is not writable"));
            }
            Ok(())
        }
        Command::Mcp { command } => match command {
            McpCommand::Stdio { read_only } => {
                let service = cli.open_service(None)?;
                mcp::run_stdio(service, *read_only)?;
                Ok(())
            }
        },
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
            let link_meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if link_meta.file_type().is_symlink() {
                continue;
            }
            if link_meta.is_dir() {
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

fn print_markdown(text: &str) {
    println!("{text}");
}

fn render_card_markdown(card: &CardShowResponse) -> String {
    let mut lines = vec![
        format!("# Card summary: {}", card.card_type),
        format!("Profile: {}", card.profile),
        format!("Workspace: {}", card.workspace),
        format!("Scope: {}", card.scope),
        format!("Generated at: {}", card.generated_at),
        format!("Freshness: {}", card.freshness),
        format!("Authority: {}", card.authority),
        format!("Build spec: {}", card.build_spec_version),
        format!("Content hash: {}", card.content_hash),
    ];

    if let Some(subject_id) = &card.subject_id {
        lines.push(format!("Subject: {}", subject_id));
    }

    lines.push(String::new());
    lines.push(format!("## Records ({})", card.records.len()));
    for record in &card.records {
        lines.push(format!(
            "- {} [{}] {} ({})",
            record.id, record.record_type, record.scope, record.confidence
        ));
        lines.push(format!("  - updated_at: {}", record.updated_at));
        if !record.tags.is_empty() {
            lines.push(format!("  - tags: {}", record.tags.join(", ")));
        }
        if !record.related_files.is_empty() {
            lines.push(format!(
                "  - related_files: {}",
                record.related_files.join(", ")
            ));
        }
        if !record.source_ids.is_empty() {
            lines.push(format!("  - source_ids: {}", record.source_ids.join(", ")));
        }
        lines.push(format!("  - content: {}", record.content));
        lines.push(String::new());
    }

    lines.join("\n")
}
