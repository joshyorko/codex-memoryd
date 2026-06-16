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
use codex_memoryd::eval;
use codex_memoryd::git_import;
use codex_memoryd::git_import::GitImportMode;
use codex_memoryd::git_import::GitImportParams;
use codex_memoryd::ingest::ArtifactKind;
use codex_memoryd::mcp;
use codex_memoryd::native_runtime;
use codex_memoryd::native_runtime::InitMode;
use codex_memoryd::native_runtime::RuntimeKind;
use codex_memoryd::native_runtime::RuntimeOptions;
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

    /// Force direct in-process SQLite mode for admin/recovery/offline work.
    #[arg(long, global = true)]
    pub local: bool,

    /// Daemon endpoint for client mode.
    #[arg(long, global = true, value_name = "URL", env = "CODEX_MEMORYD_URL")]
    pub url: Option<String>,

    /// Managed runtime kind for lifecycle commands.
    #[arg(long, global = true, value_enum, env = "CODEX_MEMORYD_RUNTIME")]
    pub runtime: Option<RuntimeKind>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize product runtime config and directories.
    Init {
        /// Initialize repo-local .dogfood layout for development.
        #[arg(long)]
        dogfood: bool,
        /// Host for generated runtime URL/bind.
        #[arg(long)]
        host: Option<String>,
        /// Port for generated runtime URL/bind.
        #[arg(long)]
        port: Option<u16>,
        /// Full bind address for generated runtime config, e.g. 127.0.0.1:8989.
        #[arg(long)]
        bind: Option<String>,
    },
    /// Start the managed daemon runtime.
    Up,
    /// Stop the managed daemon runtime.
    Down,
    /// Print managed daemon logs.
    Logs {
        #[arg(long, default_value_t = 80)]
        lines: usize,
    },
    /// Restart the managed daemon runtime.
    Restart,
    /// Upgrade the managed runtime.
    Upgrade,
    /// Build local runtime images.
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },
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
    /// Preview, apply, recall, or manage the lifecycle of procedures.
    Procedure {
        #[command(subcommand)]
        command: ProcedureCommand,
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
    /// Inspect resolved runtime/config values.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
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
    /// Quarantine or promote a memory record by id.
    Quarantine {
        #[command(subcommand)]
        command: QuarantineCommand,
    },
    /// Run self-checks across storage, schema, backup, policy, MCP, quarantine,
    /// procedures, and adapters.
    Doctor {
        #[arg(long, default_value = "summary")]
        format: String,
    },
    /// Back up, verify, or restore the durable store.
    Backup {
        #[command(subcommand)]
        command: BackupCommand,
    },
    /// Report fixture-scale performance / cost budgets (records, bytes, tokens).
    Perf {
        #[arg(long, default_value = "summary")]
        format: String,
    },
    /// Run deterministic eval suites.
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
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
        #[arg(value_enum)]
        action: Option<DreamAction>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DreamAction {
    Enable,
    Disable,
    Status,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Show resolved configuration values.
    Show {
        #[arg(long)]
        resolved: bool,
    },
    /// Print shell env for the resolved runtime.
    Env,
    /// Validate the resolved config.
    Doctor,
}

#[derive(Subcommand, Debug)]
pub enum ImageCommand {
    /// Build a local container image from this repo.
    Build {
        #[arg(long, default_value = "codex-memoryd:local")]
        tag: String,
        #[arg(long, default_value = ".")]
        context: PathBuf,
        #[arg(long)]
        container_runtime: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum McpCommand {
    /// Run the MCP server over stdio.
    Stdio {
        /// Expose only the read tools and reject write tool calls.
        #[arg(long)]
        read_only: bool,
        /// Opt in to write-capable MCP tools. The default stdio surface is read-only.
        #[arg(long, conflicts_with = "read_only")]
        write_tools: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProcedureCommand {
    /// Preview procedure candidates derived from successful episodes.
    Preview {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        subject_id: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Recall active procedures, applying activation matching with abstention.
    Recall {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        subject_id: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        /// Include retired/superseded/quarantined procedures.
        #[arg(long)]
        include_retired: bool,
    },
    /// Retire a procedure (drops out of default recall, stays inspectable).
    Retire {
        id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Supersede one procedure with another (lifecycle transition).
    Supersede {
        /// The procedure being replaced.
        #[arg(long)]
        old_id: String,
        /// The replacement procedure.
        #[arg(long)]
        new_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Record counter-evidence (failed reuse) against a procedure.
    CounterEvidence {
        id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        /// Counter-evidence count at which the procedure is quarantined.
        #[arg(long, default_value_t = 2)]
        quarantine_threshold: i64,
    },
}

#[derive(Subcommand, Debug)]
pub enum BackupCommand {
    /// Create a verified backup of the durable store plus a manifest.
    Create {
        /// Destination database path (a `.manifest.json` is written beside it).
        #[arg(long, value_name = "FILE")]
        dest: PathBuf,
    },
    /// Verify a backup database against its manifest (digest, integrity, schema).
    Verify {
        /// Backup database path (its manifest must sit beside it).
        #[arg(long, value_name = "FILE")]
        path: PathBuf,
    },
    /// Preview what restoring a backup over the target store would change.
    RestorePreview {
        /// Backup database path to restore from.
        #[arg(long, value_name = "FILE")]
        from: PathBuf,
        /// Restore target (defaults to the configured storage path).
        #[arg(long, value_name = "FILE")]
        target: Option<PathBuf>,
    },
    /// Apply a restore after re-verifying the backup (takes a safety copy).
    RestoreApply {
        /// Backup database path to restore from.
        #[arg(long, value_name = "FILE")]
        from: PathBuf,
        /// Restore target (defaults to the configured storage path).
        #[arg(long, value_name = "FILE")]
        target: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum EvalCommand {
    /// Run the agent-agnostic substrate eval suite.
    Substrate {
        #[arg(long, default_value = "summary")]
        format: String,
        /// Also run deterministic local baselines and emit a comparison.
        #[arg(long)]
        compare: bool,
    },
    /// Run the procedure-focused eval suite (activation, lifecycle, evidence).
    Procedures {
        #[arg(long, default_value = "summary")]
        format: String,
    },
    /// Run long-history retrieval quality evals and ranking ablations.
    Retrieval {
        #[arg(long, default_value = "summary")]
        format: String,
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
pub enum QuarantineCommand {
    /// Withhold a record from default recall, cards, exports, and adapters.
    Add {
        id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        reason: String,
    },
    /// Promote a quarantined record back into default recall/export surfaces.
    Promote {
        id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
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

    fn runtime_options(&self) -> RuntimeOptions {
        self.runtime_options_with_endpoint(None, None, None)
    }

    fn runtime_options_with_endpoint(
        &self,
        host: Option<String>,
        port: Option<u16>,
        bind: Option<String>,
    ) -> RuntimeOptions {
        RuntimeOptions::resolve(
            self.runtime,
            self.url.clone(),
            self.db.clone(),
            host,
            port,
            bind,
        )
    }

    fn use_client_mode(&self) -> bool {
        !self.local && self.db.is_none()
    }

    fn client_endpoint(&self, path: &str) -> String {
        format!(
            "{}{}",
            self.runtime_options().url.trim_end_matches('/'),
            path
        )
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
    if cli.url.is_some()
        && cli.db.is_some()
        && !cli.local
        && !matches!(
            cli.command,
            Command::Serve { .. }
                | Command::Init { .. }
                | Command::Up
                | Command::Down
                | Command::Logs { .. }
                | Command::Restart
                | Command::Upgrade
        )
    {
        return Err(error::Error::invalid_request(
            "--url and --db conflict unless --local makes direct SQLite mode explicit",
        ));
    }

    match &cli.command {
        Command::Init {
            dogfood,
            host,
            port,
            bind,
        } => {
            if cli.url.is_some() && (host.is_some() || port.is_some() || bind.is_some()) {
                return Err(error::Error::invalid_request(
                    "--url conflicts with init --host/--port/--bind; choose one endpoint form",
                ));
            }
            let mode = if *dogfood {
                InitMode::Dogfood
            } else {
                InitMode::Product
            };
            let report = native_runtime::init(
                &cli.runtime_options_with_endpoint(host.clone(), *port, bind.clone()),
                mode,
            )?;
            print_json(&report)?;
            Ok(())
        }
        Command::Up => {
            let report = native_runtime::up(&cli.runtime_options())?;
            print_json(&report)?;
            Ok(())
        }
        Command::Down => {
            let report = native_runtime::down(&cli.runtime_options())?;
            print_json(&report)?;
            Ok(())
        }
        Command::Logs { lines } => {
            print!("{}", native_runtime::logs(&cli.runtime_options(), *lines)?);
            Ok(())
        }
        Command::Restart => {
            let report = native_runtime::restart(&cli.runtime_options())?;
            print_json(&report)?;
            Ok(())
        }
        Command::Upgrade => {
            let report = native_runtime::upgrade(&cli.runtime_options())?;
            print_json(&report)?;
            Ok(())
        }
        Command::Image { command } => {
            match command {
                ImageCommand::Build {
                    tag,
                    context,
                    container_runtime,
                } => {
                    let mut opts = cli.runtime_options();
                    if let Some(runtime) = container_runtime {
                        opts.container_runtime = Some(runtime.clone());
                    }
                    let report = native_runtime::build_image(&opts, tag, context)?;
                    print_json(&report)?;
                }
            }
            Ok(())
        }
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
            if cli.local || cli.db.is_some() {
                let service = cli.open_service(None)?;
                let status = service.status()?;
                print_json(&status)?;
            } else if let Ok(body) = native_runtime::http_get(&format!(
                "{}/v1/status",
                cli.runtime_options().url.trim_end_matches('/')
            )) {
                print!("{body}");
            } else {
                let report = native_runtime::status(&cli.runtime_options());
                print_json(&report)?;
            }
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
            action,
            profile,
            workspace,
            repo,
            preview: _,
            apply,
            scheduled,
            now,
            since,
        } => {
            if let Some(action) = action {
                match action {
                    DreamAction::Enable => {
                        let path = cli
                            .config
                            .clone()
                            .unwrap_or_else(codex_memoryd::config::default_config_path);
                        set_dream_scheduler_enabled(&path, true)?;
                        print_json(&json!({
                            "ok": true,
                            "config_file": path,
                            "dream_scheduler_enabled": true,
                            "restart_required": true,
                        }))?;
                    }
                    DreamAction::Disable => {
                        let path = cli
                            .config
                            .clone()
                            .unwrap_or_else(codex_memoryd::config::default_config_path);
                        set_dream_scheduler_enabled(&path, false)?;
                        print_json(&json!({
                            "ok": true,
                            "config_file": path,
                            "dream_scheduler_enabled": false,
                            "restart_required": true,
                        }))?;
                    }
                    DreamAction::Status => {
                        let config = cli.load_config(None)?;
                        print_json(&json!({
                            "enabled": config.dream_scheduler.enabled,
                            "interval_seconds": config.dream_scheduler.interval_seconds,
                            "idle_window_seconds": config.dream_scheduler.idle_window_seconds,
                            "min_session_age_seconds": config.dream_scheduler.min_session_age_seconds,
                            "min_turn_count": config.dream_scheduler.min_turn_count,
                            "max_batch_size": config.dream_scheduler.max_batch_size,
                            "max_candidates": config.dream_scheduler.max_candidates,
                            "max_runtime_seconds": config.dream_scheduler.max_runtime_seconds,
                        }))?;
                    }
                }
                return Ok(());
            }
            if *scheduled {
                let service = cli.open_service(None)?;
                let resp = service.scheduled_dream(now.clone())?;
                print_json(&resp)?;
                return Ok(());
            }
            let mode = if *apply { "apply" } else { "preview" };
            let req = DreamRequest {
                profile: profile.clone(),
                workspace: workspace.clone(),
                repo: repo.clone().map(|repo_id| domain::RepoIdentity {
                    repo_id,
                    ..Default::default()
                }),
                mode: Some(mode.to_string()),
                now: now.clone(),
                since: since.clone(),
            };
            if cli.use_client_mode() {
                let body = serde_json::to_string(&req)?;
                print!(
                    "{}",
                    native_runtime::http_post_json(&cli.client_endpoint("/v1/dream"), &body)?
                );
            } else {
                let service = cli.open_service(None)?;
                let resp = service.dream(req)?;
                print_json(&resp)?;
            }
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
            if cli.use_client_mode() {
                let body = serde_json::to_string(&req)?;
                print!(
                    "{}",
                    native_runtime::http_post_json(&cli.client_endpoint("/v1/recall"), &body)?
                );
            } else {
                let service = cli.open_service(None)?;
                let resp = service.recall(req)?;
                print_json(&resp)?;
            }
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
            if cli.use_client_mode() {
                let body = serde_json::to_string(&req)?;
                print!(
                    "{}",
                    native_runtime::http_post_json(&cli.client_endpoint("/v1/search"), &body)?
                );
            } else {
                let service = cli.open_service(None)?;
                let resp = service.search(req)?;
                print_json(&resp)?;
            }
            Ok(())
        }
        Command::Conclude {
            profile,
            workspace,
            content,
        } => {
            let req = ConclusionsRequest {
                profile: profile.clone(),
                workspace: workspace.clone(),
                repo: None,
                target: Some("user".to_string()),
                conclusions: Some(vec![content.clone()]),
                metadata: None,
                record_type: None,
            };
            if cli.use_client_mode() {
                let body = serde_json::to_string(&req)?;
                print!(
                    "{}",
                    native_runtime::http_post_json(&cli.client_endpoint("/v1/conclusions"), &body)?
                );
            } else {
                let service = cli.open_service(None)?;
                let resp = service.conclusions(req)?;
                print_json(&resp)?;
            }
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
        Command::Procedure { command } => {
            let service = cli.open_service(None)?;
            match command {
                ProcedureCommand::Preview {
                    profile,
                    workspace,
                    subject_id,
                    limit,
                } => {
                    let resp = service.procedures_preview(ProceduresPreviewRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        subject_id: subject_id.clone(),
                        limit: *limit,
                    })?;
                    print_json(&resp)?;
                }
                ProcedureCommand::Recall {
                    profile,
                    workspace,
                    query,
                    subject_id,
                    limit,
                    include_retired,
                } => {
                    let resp = service.procedures_recall(ProceduresRecallRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        query: query.clone(),
                        subject_id: subject_id.clone(),
                        limit: *limit,
                        include_retired: *include_retired,
                    })?;
                    print_json(&resp)?;
                }
                ProcedureCommand::Retire {
                    id,
                    profile,
                    workspace,
                } => {
                    let resp =
                        service.procedure_retire(profile.as_deref(), workspace.as_deref(), id)?;
                    print_json(&resp)?;
                }
                ProcedureCommand::Supersede {
                    old_id,
                    new_id,
                    profile,
                    workspace,
                } => {
                    let resp = service.procedure_supersede(
                        profile.as_deref(),
                        workspace.as_deref(),
                        old_id,
                        new_id,
                    )?;
                    print_json(&resp)?;
                }
                ProcedureCommand::CounterEvidence {
                    id,
                    profile,
                    workspace,
                    quarantine_threshold,
                } => {
                    let resp = service.procedure_counter_evidence(
                        profile.as_deref(),
                        workspace.as_deref(),
                        id,
                        *quarantine_threshold,
                    )?;
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
            if cli.use_client_mode() {
                let body = serde_json::to_string(&req)?;
                print!(
                    "{}",
                    native_runtime::http_post_json(
                        &cli.client_endpoint("/v1/sync/local-codex-memory"),
                        &body
                    )?
                );
            } else {
                let service = cli.open_service(None)?;
                let resp = service.sync_local(req)?;
                print_json(&resp)?;
            }
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
                "exported {} record(s); omitted {} secret, {} quarantined, {} boundary",
                result.record_count,
                result.omitted_secret,
                result.omitted_quarantined,
                result.omitted_boundary
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
            match command {
                AdapterCommand::Export {
                    profile,
                    workspace,
                    target,
                    subject_id,
                    max_bytes,
                    format,
                } => {
                    let req = AdapterExportRequest {
                        profile: profile.clone(),
                        workspace: workspace.clone(),
                        target: target.clone(),
                        subject_id: subject_id.clone(),
                        max_bytes: *max_bytes,
                    };
                    let resp = if cli.use_client_mode() {
                        let body = serde_json::to_string(&req)?;
                        let envelope: serde_json::Value =
                            serde_json::from_str(&native_runtime::http_post_json(
                                &cli.client_endpoint("/v1/adapter/export"),
                                &body,
                            )?)?;
                        serde_json::from_value(
                            envelope
                                .get("data")
                                .cloned()
                                .ok_or_else(|| error::Error::internal("missing adapter data"))?,
                        )?
                    } else {
                        let service = cli.open_service(None)?;
                        service.adapter_export(req)?
                    };
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
        Command::Config { command } => match command {
            ConfigCommand::Show { resolved } => {
                let config = cli.load_config(None)?;
                let runtime = cli.runtime_options();
                print_json(&json!({
                            "resolved": *resolved,
                            "config_file": cli.config.clone().unwrap_or_else(codex_memoryd::config::default_config_path),
                            "registry": config_registry(&cli, &config, &runtime),
                            "client": {
                                "url": runtime.url,
                                "local": cli.local,
                            },
                            "runtime": {
                                "kind": format!("{:?}", runtime.runtime).to_ascii_lowercase(),
                "home": runtime.home,
                "db": runtime.db,
                "bind": runtime.bind,
                "url": runtime.url,
                "host": runtime.host,
                "port": runtime.port,
                "profile": runtime.profile,
                "workspace": runtime.workspace,
                "decision": runtime_decision(runtime.runtime),
                "pid_file": runtime.pid_file,
                                "log_file": runtime.log_file,
                                "image": runtime.image,
                                "container_name": runtime.container_name,
                                "container_runtime": runtime.container_runtime,
                                "codex_memories_dir": runtime.codex_memories_dir,
                                "uid": runtime.uid,
                                "gid": runtime.gid,
                            },
                            "daemon": {
                                "bind": config.bind,
                                "storage_kind": config.storage_kind,
                                "storage_path": config.storage_path,
                                "default_profile": config.default_profile,
                                "default_workspace": config.default_workspace,
                                "log_level": config.log_level,
                                "declare_loopback_publish": config.declare_loopback_publish,
                            },
                            "dream": {
                                "scheduler_enabled": config.dream_scheduler.enabled,
                                "scheduler_interval_seconds": config.dream_scheduler.interval_seconds,
                                "idle_window_seconds": config.dream_scheduler.idle_window_seconds,
                                "min_session_age_seconds": config.dream_scheduler.min_session_age_seconds,
                                "min_turn_count": config.dream_scheduler.min_turn_count,
                                "max_batch_size": config.dream_scheduler.max_batch_size,
                                "max_candidates": config.dream_scheduler.max_candidates,
                                "max_runtime_seconds": config.dream_scheduler.max_runtime_seconds,
                            }
                        }))?;
                Ok(())
            }
            ConfigCommand::Env => {
                let runtime = cli.runtime_options();
                println!("CODEX_MEMORYD_RUNTIME={:?}", runtime.runtime);
                println!("CODEX_MEMORYD_HOME={}", runtime.home.display());
                println!("CODEX_MEMORYD_RUNTIME_DIR={}", runtime.home.display());
                println!("CODEX_MEMORYD_URL={}", runtime.url);
                println!("CODEX_MEMORYD_HOST={}", runtime.host);
                println!("CODEX_MEMORYD_PORT={}", runtime.port);
                println!("CODEX_MEMORYD_BIND={}", runtime.bind);
                println!("CODEX_MEMORYD_DB={}", runtime.db.display());
                println!("CODEX_MEMORYD_PID_FILE={}", runtime.pid_file.display());
                println!("CODEX_MEMORYD_LOG_FILE={}", runtime.log_file.display());
                println!("CODEX_MEMORYD_PROFILE={}", runtime.profile);
                println!("CODEX_MEMORYD_WORKSPACE={}", runtime.workspace);
                println!("CODEX_MEMORYD_LOG={}", runtime.log_level);
                println!("CODEX_MEMORYD_IMAGE={}", runtime.image);
                println!("CODEX_MEMORYD_CONTAINER_NAME={}", runtime.container_name);
                println!(
                    "CODEX_MEMORYD_CONTAINER_RUNTIME={}",
                    runtime
                        .container_runtime
                        .clone()
                        .unwrap_or_else(|| "auto".to_string())
                );
                if let Some(uid) = &runtime.uid {
                    println!("CODEX_MEMORYD_UID={uid}");
                }
                if let Some(gid) = &runtime.gid {
                    println!("CODEX_MEMORYD_GID={gid}");
                }
                println!(
                    "CODEX_MEMORYD_CODEX_MEMORIES_DIR={}",
                    runtime.codex_memories_dir.display()
                );
                Ok(())
            }
            ConfigCommand::Doctor => {
                let config = cli.load_config(None)?;
                print_json(&json!({
                    "ok": true,
                    "config_file": cli.config.clone().unwrap_or_else(codex_memoryd::config::default_config_path),
                    "storage_path": config.storage_path,
                    "bind": config.bind,
                    "loopback_only": config.bind_is_loopback(),
                }))?;
                Ok(())
            }
        },
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
        Command::Quarantine { command } => {
            let service = cli.open_service(None)?;
            match command {
                QuarantineCommand::Add {
                    id,
                    profile,
                    workspace,
                    reason,
                } => {
                    let profile = service.resolve_profile(profile)?;
                    let workspace = workspace.as_deref();
                    let (quarantined, not_found) = service.store.quarantine_records(
                        profile.as_str(),
                        workspace,
                        std::slice::from_ref(id),
                        reason,
                    )?;
                    print_json(&json!({
                        "quarantined": quarantined,
                        "not_found": not_found,
                    }))?;
                }
                QuarantineCommand::Promote {
                    id,
                    profile,
                    workspace,
                } => {
                    let profile = service.resolve_profile(profile)?;
                    let workspace = workspace.as_deref();
                    let (promoted, not_found) = service.store.promote_quarantined_records(
                        profile.as_str(),
                        workspace,
                        std::slice::from_ref(id),
                    )?;
                    print_json(&json!({
                        "promoted": promoted,
                        "not_found": not_found,
                    }))?;
                }
            }
            Ok(())
        }
        Command::Doctor { format } => {
            let service = cli.open_service(None)?;
            let report = codex_memoryd::doctor::run(&service)?;
            let writable = report.storage.writable;
            match format.as_str().to_ascii_lowercase().as_str() {
                "json" => print_json(&report)?,
                "summary" | "human" | "markdown" => {
                    print_markdown(&codex_memoryd::doctor::render_summary(&report))
                }
                other => {
                    return Err(error::Error::invalid_request(format!(
                        "invalid --format '{other}'; expected 'json' or 'summary'"
                    )))
                }
            }
            if !writable {
                return Err(error::Error::storage("storage is not writable"));
            }
            Ok(())
        }
        Command::Backup { command } => {
            let config = cli.load_config(None)?;
            match command {
                BackupCommand::Create { dest } => {
                    let store = Store::open(&config.storage_path)?;
                    let now = codex_memoryd::ids::now_rfc3339();
                    let result = codex_memoryd::backup::create_backup(&store, dest, &now)?;
                    print_json(&json!({
                        "ok": true,
                        "database_path": result.database_path,
                        "manifest_path": result.manifest_path,
                        "manifest": result.manifest,
                    }))?;
                    Ok(())
                }
                BackupCommand::Verify { path } => {
                    let result = codex_memoryd::backup::verify_backup(path)?;
                    let ok = result.ok;
                    print_json(&result)?;
                    if ok {
                        Ok(())
                    } else {
                        Err(error::Error::invalid_request(
                            "backup verification failed".to_string(),
                        ))
                    }
                }
                BackupCommand::RestorePreview { from, target } => {
                    let target = target
                        .clone()
                        .unwrap_or_else(|| config.storage_path.clone());
                    let preview = codex_memoryd::backup::restore_preview(from, &target)?;
                    print_json(&preview)?;
                    Ok(())
                }
                BackupCommand::RestoreApply { from, target } => {
                    let target = target
                        .clone()
                        .unwrap_or_else(|| config.storage_path.clone());
                    let now = codex_memoryd::ids::now_rfc3339();
                    let result = codex_memoryd::backup::restore_apply(from, &target, &now)?;
                    print_json(&result)?;
                    Ok(())
                }
            }
        }
        Command::Perf { format } => {
            // Real monotonic clock for the CLI; timing is informational only.
            let clock = || {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            };
            let report = codex_memoryd::perf::run_perf_report(clock)?;
            match format.as_str().to_ascii_lowercase().as_str() {
                "json" => print_json(&report)?,
                "summary" | "human" | "markdown" => {
                    print_markdown(&codex_memoryd::perf::render_summary(&report))
                }
                other => {
                    return Err(error::Error::invalid_request(format!(
                        "invalid --format '{other}'; expected 'json' or 'summary'"
                    )))
                }
            }
            Ok(())
        }
        Command::Eval { command } => match command {
            EvalCommand::Substrate { format, compare } => {
                let report = eval::run_substrate_eval()?;
                let fmt = format.as_str().to_ascii_lowercase();
                match fmt.as_str() {
                    "json" => print_json(&report)?,
                    "summary" | "human" | "markdown" => {
                        print_markdown(&eval::render_substrate_summary(&report))
                    }
                    other => {
                        return Err(error::Error::invalid_request(format!(
                            "invalid --format '{other}'; expected 'json' or 'summary'"
                        )))
                    }
                }
                if *compare {
                    let comparison = eval::run_comparative_eval()?;
                    match fmt.as_str() {
                        "json" => print_json(&comparison)?,
                        _ => print_markdown(&eval::render_comparative_summary(&comparison)),
                    }
                }
                Ok(())
            }
            EvalCommand::Procedures { format } => {
                let report = codex_memoryd::proc_eval::run_procedure_eval()?;
                match format.as_str().to_ascii_lowercase().as_str() {
                    "json" => print_json(&report)?,
                    "summary" | "human" | "markdown" => {
                        print_markdown(&codex_memoryd::proc_eval::render_summary(&report))
                    }
                    other => {
                        return Err(error::Error::invalid_request(format!(
                            "invalid --format '{other}'; expected 'json' or 'summary'"
                        )))
                    }
                }
                Ok(())
            }
            EvalCommand::Retrieval { format } => {
                let report = codex_memoryd::retrieval_eval::run_retrieval_eval()?;
                match format.as_str().to_ascii_lowercase().as_str() {
                    "json" => print_json(&report)?,
                    "summary" | "human" | "markdown" => print_markdown(
                        &codex_memoryd::retrieval_eval::render_retrieval_summary(&report),
                    ),
                    other => {
                        return Err(error::Error::invalid_request(format!(
                            "invalid --format '{other}'; expected 'json' or 'summary'"
                        )))
                    }
                }
                Ok(())
            }
        },
        Command::Mcp { command } => match command {
            McpCommand::Stdio {
                read_only: _,
                write_tools,
            } => {
                let service = cli.open_service(None)?;
                mcp::run_stdio(service, *write_tools)?;
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

fn set_dream_scheduler_enabled(path: &std::path::Path, enabled: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(error::Error::from)?;
    }
    let value = if enabled { "true" } else { "false" };
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let mut out = Vec::new();
    let mut in_dream = false;
    let mut saw_dream = false;
    let mut wrote_key = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_dream && !wrote_key {
                out.push(format!("scheduler_enabled = {value}"));
                wrote_key = true;
            }
            in_dream = trimmed == "[dream]";
            saw_dream |= in_dream;
        }
        if in_dream && trimmed.starts_with("scheduler_enabled") {
            out.push(format!("scheduler_enabled = {value}"));
            wrote_key = true;
        } else {
            out.push(line.to_string());
        }
    }

    if saw_dream {
        if in_dream && !wrote_key {
            out.push(format!("scheduler_enabled = {value}"));
        }
    } else {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push("[dream]".to_string());
        out.push(format!("scheduler_enabled = {value}"));
    }

    std::fs::write(path, format!("{}\n", out.join("\n"))).map_err(error::Error::from)
}

fn runtime_decision(runtime: RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Native | RuntimeKind::Auto => "native",
        RuntimeKind::Container => "container",
        RuntimeKind::ComposeDev => "compose-dev",
    }
}

fn config_registry(cli: &Cli, config: &Config, runtime: &RuntimeOptions) -> serde_json::Value {
    let entry = |key: &str,
                 owner: &str,
                 source: &str,
                 value: serde_json::Value,
                 persisted_by_init: bool,
                 restart_required: bool,
                 passed_to_container: bool,
                 safe_to_commit: bool,
                 may_contain_secrets: bool| {
        json!({
            "key": key,
            "owner": owner,
            "source": source,
            "value": value,
            "persisted_by_init": persisted_by_init,
            "restart_required": restart_required,
            "passed_to_container": passed_to_container,
            "safe_to_commit": safe_to_commit,
            "may_contain_secrets": may_contain_secrets,
        })
    };
    json!([
        entry(
            "CODEX_MEMORYD_URL",
            "client",
            source_for(cli.url.is_some(), "CODEX_MEMORYD_URL"),
            json!(runtime.url),
            true,
            false,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_RUNTIME",
            "native-runtime/container-runtime",
            source_for(cli.runtime.is_some(), "CODEX_MEMORYD_RUNTIME"),
            json!(format!("{:?}", runtime.runtime).to_ascii_lowercase()),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_HOME",
            "native-runtime/container-runtime",
            env_source("CODEX_MEMORYD_HOME", "default"),
            json!(runtime.home),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_RUNTIME_DIR",
            "native-runtime",
            env_source("CODEX_MEMORYD_RUNTIME_DIR", "compat-default"),
            json!(runtime.home),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_HOST",
            "container-runtime",
            env_source("CODEX_MEMORYD_HOST", "default"),
            json!(runtime.host),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_PORT",
            "container-runtime",
            env_source("CODEX_MEMORYD_PORT", "default"),
            json!(runtime.port),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_BIND",
            "daemon",
            env_source("CODEX_MEMORYD_BIND", "derived-default"),
            json!(config.bind),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DB",
            "daemon/admin-recovery",
            source_for(cli.db.is_some(), "CODEX_MEMORYD_DB"),
            json!(config.storage_path),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_PROFILE",
            "daemon",
            env_source("CODEX_MEMORYD_PROFILE", "default"),
            json!(config.default_profile),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_WORKSPACE",
            "daemon",
            env_source("CODEX_MEMORYD_WORKSPACE", "default"),
            json!(config.default_workspace),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_LOG",
            "daemon",
            source_for(cli.log.is_some(), "CODEX_MEMORYD_LOG"),
            json!(config.log_level),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DECLARE_LOOPBACK_PUBLISH",
            "daemon/container-runtime",
            env_source("CODEX_MEMORYD_DECLARE_LOOPBACK_PUBLISH", "default"),
            json!(config.declare_loopback_publish),
            false,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_BIN",
            "native-runtime",
            env_source("CODEX_MEMORYD_BIN", "current_exe"),
            json!(runtime.binary),
            false,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_PID_FILE",
            "native-runtime",
            env_source("CODEX_MEMORYD_PID_FILE", "default"),
            json!(runtime.pid_file),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_LOG_FILE",
            "native-runtime",
            env_source("CODEX_MEMORYD_LOG_FILE", "default"),
            json!(runtime.log_file),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_ALLOW_NON_LOOPBACK",
            "native-runtime",
            env_source("CODEX_MEMORYD_ALLOW_NON_LOOPBACK", "default"),
            json!(runtime.allow_non_loopback),
            false,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_IMAGE",
            "container-runtime",
            env_source("CODEX_MEMORYD_IMAGE", "default"),
            json!(runtime.image),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_CONTAINER_NAME",
            "container-runtime",
            env_source("CODEX_MEMORYD_CONTAINER_NAME", "default"),
            json!(runtime.container_name),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_CONTAINER_RUNTIME",
            "container-runtime",
            env_source("CODEX_MEMORYD_CONTAINER_RUNTIME", "auto"),
            json!(runtime.container_runtime),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_UID",
            "container-runtime",
            env_source("CODEX_MEMORYD_UID", "current-user"),
            json!(runtime.uid),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_GID",
            "container-runtime",
            env_source("CODEX_MEMORYD_GID", "current-user"),
            json!(runtime.gid),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_CODEX_MEMORIES_DIR",
            "container-runtime",
            env_source("CODEX_MEMORYD_CODEX_MEMORIES_DIR", "default"),
            json!(runtime.codex_memories_dir),
            true,
            true,
            false,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED",
            "daemon",
            env_source("CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED", "config/default"),
            json!(config.dream_scheduler.enabled),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DREAM_SCHEDULER_INTERVAL_SECONDS",
            "daemon",
            env_source(
                "CODEX_MEMORYD_DREAM_SCHEDULER_INTERVAL_SECONDS",
                "config/default"
            ),
            json!(config.dream_scheduler.interval_seconds),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DREAM_IDLE_WINDOW_SECONDS",
            "daemon",
            env_source("CODEX_MEMORYD_DREAM_IDLE_WINDOW_SECONDS", "config/default"),
            json!(config.dream_scheduler.idle_window_seconds),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DREAM_MIN_SESSION_AGE_SECONDS",
            "daemon",
            env_source(
                "CODEX_MEMORYD_DREAM_MIN_SESSION_AGE_SECONDS",
                "config/default"
            ),
            json!(config.dream_scheduler.min_session_age_seconds),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DREAM_MIN_TURN_COUNT",
            "daemon",
            env_source("CODEX_MEMORYD_DREAM_MIN_TURN_COUNT", "config/default"),
            json!(config.dream_scheduler.min_turn_count),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DREAM_MAX_BATCH_SIZE",
            "daemon",
            env_source("CODEX_MEMORYD_DREAM_MAX_BATCH_SIZE", "config/default"),
            json!(config.dream_scheduler.max_batch_size),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DREAM_MAX_CANDIDATES",
            "daemon",
            env_source("CODEX_MEMORYD_DREAM_MAX_CANDIDATES", "config/default"),
            json!(config.dream_scheduler.max_candidates),
            true,
            true,
            true,
            true,
            false
        ),
        entry(
            "CODEX_MEMORYD_DREAM_MAX_RUNTIME_SECONDS",
            "daemon",
            env_source("CODEX_MEMORYD_DREAM_MAX_RUNTIME_SECONDS", "config/default"),
            json!(config.dream_scheduler.max_runtime_seconds),
            true,
            true,
            true,
            true,
            false
        ),
    ])
}

fn source_for(cli_present: bool, env_key: &str) -> &'static str {
    if cli_present {
        "cli"
    } else {
        env_source(env_key, "config/default")
    }
}

fn env_source(env_key: &str, fallback: &'static str) -> &'static str {
    if std::env::var(env_key)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
    {
        "env"
    } else {
        fallback
    }
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
        lines.push(format!(
            "  - freshness: {}",
            if record.freshness.stale {
                "stale"
            } else {
                "fresh"
            }
        ));
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
