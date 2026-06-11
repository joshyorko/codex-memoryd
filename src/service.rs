//! Provider service layer: the request-handling logic shared by the HTTP server
//! and the CLI. Each method takes a typed protocol request and returns a typed
//! protocol response (or a stable [`Error`]).
//!
//! This is where validation, policy screening, classification, and store calls
//! are orchestrated. Keeping it transport-agnostic lets the CLI exercise the
//! exact same code paths as HTTP.

use std::sync::Arc;

use serde_json::json;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::config::Config;
use crate::domain::Checkpoint;
use crate::domain::Conclusion;
use crate::domain::Portability;
use crate::domain::Profile;
use crate::domain::RecordType;
use crate::domain::RepoIdentity;
use crate::domain::Scope;
use crate::domain::Sensitivity;
use crate::domain::VisibleTurn;
use crate::dream;
use crate::error::Error;
use crate::error::ErrorCode;
use crate::error::Result;
use crate::export;
use crate::export::ExportFormat;
use crate::export::ExportParams;
use crate::export::ExportResult;
use crate::ids;
use crate::ingest;
use crate::ingest::SyncMode;
use crate::ingest::SyncParams;
use crate::metrics::Metrics;
use crate::policy;
use crate::policy::PolicyDecision;
use crate::protocol::*;
use crate::recall;
use crate::recall::RecallParams;
use crate::recall::SearchParams;
use crate::status;
use crate::store::DreamRunAudit;
use crate::store::NewRecord;
use crate::store::RecordQuery;
use crate::store::Store;

/// The provider service. Cheaply cloneable (Arc inside).
#[derive(Clone)]
pub struct Service {
    pub store: Store,
    pub config: Arc<Config>,
    pub metrics: Arc<Metrics>,
}

impl Service {
    pub fn new(store: Store, config: Config) -> Service {
        Service {
            store,
            config: Arc::new(config),
            metrics: Arc::new(Metrics::new()),
        }
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Resolve a profile string, applying the configured default when absent.
    fn resolve_profile(&self, raw: &Option<String>) -> Result<Profile> {
        let value = raw
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.config.default_profile.clone());
        Profile::parse(&value).ok_or_else(|| {
            Error::new(
                ErrorCode::UnknownProfile,
                format!("unknown profile '{value}'"),
            )
        })
    }

    /// Resolve a required workspace, applying the configured default when absent.
    fn resolve_workspace(&self, raw: &Option<String>) -> String {
        raw.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(sanitize_workspace)
            .unwrap_or_else(|| self.config.default_workspace.clone())
    }

    /// Register repo identity and return its repo_id (if any).
    fn register_repo(&self, repo: &Option<RepoIdentity>) -> Result<Option<String>> {
        match repo {
            Some(r) if !r.repo_id.trim().is_empty() => {
                self.store.ensure_repo(
                    &r.repo_id,
                    r.root.as_deref(),
                    r.remote.as_deref(),
                    r.branch.as_deref(),
                    r.commit.as_deref(),
                    r.is_git,
                )?;
                Ok(Some(r.repo_id.clone()))
            }
            _ => Ok(None),
        }
    }

    // ------------------------------------------------------------------
    // Status
    // ------------------------------------------------------------------

    pub fn status(&self) -> Result<StatusResponse> {
        status::build_status(&self.store, &self.config, &self.metrics)
    }

    // ------------------------------------------------------------------
    // Recall
    // ------------------------------------------------------------------

    pub fn recall(&self, req: RecallRequest) -> Result<RecallResponse> {
        Metrics::incr(&self.metrics.recall_requests);
        let profile = self.resolve_profile(&req.profile)?;
        let workspace = self.resolve_workspace(&req.workspace);
        let query = req.query.clone().unwrap_or_default();

        let include_types = parse_types(&req.include_types);
        let exclude_types = parse_types(&req.exclude_types);
        let max_tokens = req
            .max_tokens
            .unwrap_or(self.config.max_recall_tokens)
            .max(1);

        let params = RecallParams {
            profile,
            workspace: &workspace,
            repo: req.repo.as_ref(),
            query: &query,
            files: &req.files,
            max_tokens,
            include_types: &include_types,
            exclude_types: &exclude_types,
            recency_days: req.recency_days,
        };
        recall::recall(&self.store, &params)
    }

    // ------------------------------------------------------------------
    // Search
    // ------------------------------------------------------------------

    pub fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        Metrics::incr(&self.metrics.search_requests);
        let profile = self.resolve_profile(&req.profile)?;
        let workspace = req
            .workspace
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(sanitize_workspace);
        let record_type = match &req.record_type {
            Some(t) => Some(
                RecordType::parse(t)
                    .ok_or_else(|| Error::invalid_request(format!("unknown type '{t}'")))?,
            ),
            None => None,
        };
        let scope = match &req.scope {
            Some(s) => Some(
                Scope::parse(s)
                    .ok_or_else(|| Error::invalid_request(format!("unknown scope '{s}'")))?,
            ),
            None => None,
        };
        let limit = req.limit.unwrap_or(20).clamp(1, 200);
        let offset = req
            .cursor
            .as_deref()
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(0);

        let params = SearchParams {
            profile,
            workspace: workspace.as_deref(),
            repo_id: req.repo.as_ref().map(|r| r.repo_id.as_str()),
            query: req.query.as_deref().unwrap_or(""),
            scope,
            record_type,
            include_archived: req.include_archived,
            limit,
            offset,
        };
        recall::search(&self.store, &params)
    }

    // ------------------------------------------------------------------
    // Turns
    // ------------------------------------------------------------------

    pub fn turns(&self, req: TurnsRequest) -> Result<TurnsResponse> {
        let profile = self.resolve_profile(&req.profile)?;
        let workspace = self.resolve_workspace(&req.workspace);
        let repo_id = self.register_repo(&req.repo)?;
        self.store.ensure_workspace(profile.as_str(), &workspace)?;

        let session = req
            .session
            .ok_or_else(|| Error::invalid_request("session is required for /v1/turns"))?;
        let messages = req
            .messages
            .ok_or_else(|| Error::invalid_request("messages is required for /v1/turns"))?;

        let session_id = session
            .id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| ids::new_id("session"));
        let source = session
            .source
            .clone()
            .unwrap_or_else(|| "codex".to_string());
        self.store.ensure_session(
            &session_id,
            profile.as_str(),
            &workspace,
            repo_id.as_deref(),
            session.thread_id.as_deref(),
            &source,
        )?;

        let mut accepted = 0usize;
        let mut rejections: Vec<Rejection> = Vec::new();
        let mut source_ids: Vec<String> = Vec::new();
        let mut derived_record_ids: Vec<String> = Vec::new();

        for (idx, msg) in messages.into_iter().enumerate() {
            let actor = msg.actor.trim().to_ascii_lowercase();
            if actor != "user" && actor != "assistant" {
                rejections.push(Rejection {
                    index: Some(idx),
                    reason: format!("invalid actor '{}': must be user or assistant", msg.actor),
                    code: "invalid_request".to_string(),
                });
                Metrics::incr(&self.metrics.writeback_rejected);
                continue;
            }

            let decision = policy::screen_content(&msg.content, self.config.max_record_chars);
            let content = match decision {
                PolicyDecision::Accept(c) => c,
                PolicyDecision::Reject { code, reason } => {
                    rejections.push(Rejection {
                        index: Some(idx),
                        reason: reason.clone(),
                        code: code.clone(),
                    });
                    Metrics::incr(&self.metrics.writeback_rejected);
                    let _ = self.store.record_policy_event(
                        Some(profile.as_str()),
                        Some(&workspace),
                        "rejected_turn",
                        &code,
                        &reason,
                        "turns",
                    );
                    continue;
                }
            };

            // Store the visible turn (provenance).
            let turn = VisibleTurn {
                id: ids::new_id("turn"),
                session_id: session_id.clone(),
                actor: actor.clone(),
                content: content.clone(),
                created_at: msg.created_at.clone().unwrap_or_else(ids::now_rfc3339),
                metadata: msg.metadata.clone().unwrap_or(Value::Null),
            };
            self.store.insert_visible_turn(&turn)?;

            // Record a source for the turn.
            let source_hash = ids::source_hash(profile.as_str(), &workspace, &session_id, &content);
            let (src, _created) = self.store.upsert_source(
                profile.as_str(),
                &workspace,
                "visible_turn",
                Some(&format!("turn:{}", turn.id)),
                &source_hash,
                &json!({ "actor": actor, "session_id": session_id }),
            )?;
            source_ids.push(src.id.clone());
            accepted += 1;
            Metrics::incr(&self.metrics.writeback_accepted);

            // Derive a simple memory record from user preference/decision-like
            // statements (SPEC §6.4 "derive candidate memory records").
            if let Some(record_id) = self.maybe_derive_record(
                profile,
                &workspace,
                repo_id.as_deref(),
                &content,
                &src.id,
            )? {
                derived_record_ids.push(record_id);
            }
        }

        Ok(TurnsResponse {
            accepted,
            rejected: rejections.len(),
            rejections,
            source_ids,
            derived_record_ids,
        })
    }

    /// Heuristically derive a durable record from a visible turn when it looks
    /// like a durable fact (preference/decision/command/gotcha). Returns the
    /// new record id if one was created.
    fn maybe_derive_record(
        &self,
        profile: Profile,
        workspace: &str,
        repo_id: Option<&str>,
        content: &str,
        source_id: &str,
    ) -> Result<Option<String>> {
        let class = policy::classify(content, profile, repo_id.is_some());
        // Only derive for high-signal types; skip generic chatter.
        let worth_storing = matches!(
            class.record_type,
            RecordType::Preference
                | RecordType::Decision
                | RecordType::Command
                | RecordType::Gotcha
                | RecordType::RepoConvention
        );
        if !worth_storing {
            return Ok(None);
        }
        let content_hash = ids::content_hash(
            profile.as_str(),
            workspace,
            repo_id,
            class.record_type.as_str(),
            class.scope.as_str(),
            content,
        );
        let new = NewRecord {
            profile_id: profile.as_str().to_string(),
            workspace_id: workspace.to_string(),
            repo_id: repo_id.map(|s| s.to_string()),
            scope: class.scope,
            record_type: class.record_type,
            content: content.to_string(),
            related_files: class.related_files,
            tags: class.tags,
            sensitivity: class.sensitivity,
            portability: class.portability,
            confidence: class.confidence,
            source_ids: vec![source_id.to_string()],
            content_hash,
            supersedes: vec![],
            metadata: json!({ "origin": "visible_turn", "source_id": source_id }),
        };
        match self.store.upsert_record(&new)? {
            crate::store::UpsertOutcome::Created(id) => Ok(Some(id)),
            crate::store::UpsertOutcome::Skipped(_) => Ok(None),
        }
    }

    // ------------------------------------------------------------------
    // Conclusions
    // ------------------------------------------------------------------

    pub fn conclusions(&self, req: ConclusionsRequest) -> Result<ConclusionsResponse> {
        let profile = self.resolve_profile(&req.profile)?;
        let workspace = self.resolve_workspace(&req.workspace);
        let repo_id = self.register_repo(&req.repo)?;
        self.store.ensure_workspace(profile.as_str(), &workspace)?;

        let target = req.target.clone().unwrap_or_else(|| "user".to_string());
        let conclusions = req
            .conclusions
            .ok_or_else(|| Error::invalid_request("conclusions is required"))?;
        let forced_type = match &req.record_type {
            Some(t) => Some(
                RecordType::parse(t)
                    .ok_or_else(|| Error::invalid_request(format!("unknown type '{t}'")))?,
            ),
            None => None,
        };

        let mut created = Vec::new();
        let mut record_ids = Vec::new();
        let mut rejected = Vec::new();

        for raw in conclusions {
            let decision = policy::screen_content(&raw, self.config.max_record_chars);
            let content = match decision {
                PolicyDecision::Accept(c) => c,
                PolicyDecision::Reject { code, reason } => {
                    rejected.push(ConclusionRejection {
                        content: redact_for_echo(&raw),
                        reason: reason.clone(),
                        code: code.clone(),
                    });
                    Metrics::incr(&self.metrics.writeback_rejected);
                    let _ = self.store.record_policy_event(
                        Some(profile.as_str()),
                        Some(&workspace),
                        "rejected_conclusion",
                        &code,
                        &reason,
                        "conclusions",
                    );
                    continue;
                }
            };

            // Persist the conclusion entity.
            let conclusion = Conclusion {
                id: ids::new_id("concl"),
                profile_id: profile.as_str().to_string(),
                workspace_id: workspace.clone(),
                repo_id: repo_id.clone(),
                target: target.clone(),
                content: content.clone(),
                source_id: None,
                created_at: ids::now_rfc3339(),
                metadata: req.metadata.clone().unwrap_or(Value::Null),
            };
            self.store.insert_conclusion(&conclusion)?;
            created.push(conclusion.id.clone());

            // Conclusions become memory records (SPEC §6.5).
            let mut class = policy::classify(&content, profile, repo_id.is_some());
            if let Some(t) = forced_type {
                class.record_type = t;
            }
            let content_hash = ids::content_hash(
                profile.as_str(),
                &workspace,
                repo_id.as_deref(),
                class.record_type.as_str(),
                class.scope.as_str(),
                &content,
            );
            let new = NewRecord {
                profile_id: profile.as_str().to_string(),
                workspace_id: workspace.clone(),
                repo_id: repo_id.clone(),
                scope: class.scope,
                record_type: class.record_type,
                content,
                related_files: class.related_files,
                tags: class.tags,
                sensitivity: class.sensitivity,
                portability: class.portability,
                confidence: class.confidence,
                source_ids: vec![],
                content_hash,
                supersedes: vec![],
                metadata: json!({ "origin": "conclusion", "conclusion_id": conclusion.id, "target": target }),
            };
            if let crate::store::UpsertOutcome::Created(id) = self.store.upsert_record(&new)? {
                record_ids.push(id);
            }
            Metrics::incr(&self.metrics.writeback_accepted);
        }

        Ok(ConclusionsResponse {
            created,
            record_ids,
            rejected,
        })
    }

    // ------------------------------------------------------------------
    // Checkpoints
    // ------------------------------------------------------------------

    pub fn checkpoint(&self, req: CheckpointRequest) -> Result<CheckpointResponse> {
        let profile = self.resolve_profile(&req.profile)?;
        let workspace = self.resolve_workspace(&req.workspace);
        let repo_id = self.register_repo(&req.repo)?;
        self.store.ensure_workspace(profile.as_str(), &workspace)?;

        let summary = req
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::invalid_request("checkpoint summary is required"))?
            .to_string();

        // Screen the summary for secrets (checkpoints are durable memory).
        let summary = match policy::screen_content(&summary, self.config.max_record_chars) {
            PolicyDecision::Accept(c) => c,
            PolicyDecision::Reject { code, reason } => {
                let _ = self.store.record_policy_event(
                    Some(profile.as_str()),
                    Some(&workspace),
                    "rejected_checkpoint",
                    &code,
                    &reason,
                    "checkpoints",
                );
                return Err(Error::new(map_code(&code), reason));
            }
        };

        let session_id = req
            .session
            .as_ref()
            .and_then(|s| s.id.clone())
            .filter(|s| !s.trim().is_empty());
        if let Some(sid) = &session_id {
            self.store.ensure_session(
                sid,
                profile.as_str(),
                &workspace,
                repo_id.as_deref(),
                req.session.as_ref().and_then(|s| s.thread_id.as_deref()),
                "checkpoint",
            )?;
        }

        let checkpoint = Checkpoint {
            id: ids::new_id("ckpt"),
            session_id,
            profile_id: profile.as_str().to_string(),
            workspace_id: workspace.clone(),
            repo_id: repo_id.clone(),
            summary: summary.clone(),
            changed_files: req.changed_files,
            decisions: req.decisions,
            blockers: req.blockers,
            next_steps: req.next_steps,
            tests_run: req.tests_run,
            tests_not_run: req.tests_not_run,
            branch: req.branch,
            commit: req.commit,
            created_at: ids::now_rfc3339(),
        };
        self.store.insert_checkpoint(&checkpoint)?;

        // Also store a task_checkpoint memory record so recall can surface it as
        // a fact when checkpoints aren't separately requested.
        let content_hash = ids::content_hash(
            profile.as_str(),
            &workspace,
            repo_id.as_deref(),
            RecordType::TaskCheckpoint.as_str(),
            Scope::Session.as_str(),
            &summary,
        );
        let _ = self.store.upsert_record(&NewRecord {
            profile_id: profile.as_str().to_string(),
            workspace_id: workspace.clone(),
            repo_id: repo_id.clone(),
            scope: if repo_id.is_some() {
                Scope::Repo
            } else {
                Scope::Session
            },
            record_type: RecordType::TaskCheckpoint,
            content: summary,
            related_files: checkpoint.changed_files.clone(),
            tags: vec!["task_checkpoint".to_string()],
            sensitivity: default_sensitivity(profile),
            portability: Portability::ProfileOnly,
            confidence: 0.7,
            source_ids: vec![],
            content_hash,
            supersedes: vec![],
            metadata: json!({ "origin": "checkpoint", "checkpoint_id": checkpoint.id }),
        });

        Ok(CheckpointResponse {
            id: checkpoint.id,
            created_at: checkpoint.created_at,
        })
    }

    // ------------------------------------------------------------------
    // Dreamer
    // ------------------------------------------------------------------

    pub fn dream(&self, req: DreamRequest) -> Result<DreamResponse> {
        let profile = self.resolve_profile(&req.profile)?;
        let workspace = self.resolve_workspace(&req.workspace);
        let repo_id = self.register_repo(&req.repo)?;
        let mode = req.mode.unwrap_or_else(|| "preview".to_string());
        let started_at = ids::now_rfc3339();
        if mode != "preview" && mode != "apply" {
            let _ = self.store.insert_dream_run(&dream_error_audit(
                profile.as_str(),
                &workspace,
                repo_id.as_deref(),
                &mode,
                &started_at,
                None,
                None,
                "dream mode must be preview or apply",
            ));
            return Err(Error::invalid_request(
                "dream mode must be preview or apply",
            ));
        }
        let now = req.now.unwrap_or_else(|| {
            let current = ids::now_rfc3339();
            let day = current.split('T').next().unwrap_or("1970-01-01");
            format!("{day}T00:00:00Z")
        });
        if OffsetDateTime::parse(&now, &Rfc3339).is_err() {
            let _ = self.store.insert_dream_run(&dream_error_audit(
                profile.as_str(),
                &workspace,
                repo_id.as_deref(),
                &mode,
                &started_at,
                None,
                Some(&now),
                "dream now must be an RFC3339 timestamp",
            ));
            return Err(Error::invalid_request(
                "dream now must be an RFC3339 timestamp",
            ));
        }
        if let Some(since) = req.since.as_deref() {
            if OffsetDateTime::parse(since, &Rfc3339).is_err() {
                let _ = self.store.insert_dream_run(&dream_error_audit(
                    profile.as_str(),
                    &workspace,
                    repo_id.as_deref(),
                    &mode,
                    &started_at,
                    Some(since),
                    Some(&now),
                    "dream since must be an RFC3339 timestamp",
                ));
                return Err(Error::invalid_request(
                    "dream since must be an RFC3339 timestamp",
                ));
            }
        }
        let source_window_start = match req.since {
            Some(since) => Some(since),
            None => self
                .store
                .dream_watermark(profile.as_str(), &workspace, repo_id.as_deref())?,
        };
        let source_records = self.store.query_records(&RecordQuery {
            profile_id: Some(profile.as_str().to_string()),
            workspace_id: Some(workspace.clone()),
            repo_id: repo_id.clone(),
            record_type: None,
            scope: None,
            include_archived: false,
            recency_cutoff: source_window_start.clone(),
            limit: 500,
            offset: 0,
        })?;
        let result = dream::run(
            &self.store,
            &dream::DreamParams {
                profile,
                workspace: &workspace,
                repo_id: repo_id.as_deref(),
                mode: &mode,
                now: &now,
                source_window_start: source_window_start.as_deref(),
            },
        );
        match result {
            Ok(resp) => {
                let completed_at = ids::now_rfc3339();
                self.store.insert_dream_run(&DreamRunAudit {
                    id: resp.run_id.clone(),
                    profile_id: resp.profile.clone(),
                    workspace_id: resp.workspace.clone(),
                    repo_id: resp.repo_id.clone(),
                    mode: resp.mode.clone(),
                    status: "ok".to_string(),
                    started_at,
                    completed_at: Some(completed_at),
                    implementation_version: dream::DREAM_IMPLEMENTATION_VERSION.to_string(),
                    config_hash: dream::config_hash(),
                    ruleset_version: dream::DREAM_RULESET_VERSION.to_string(),
                    fixture_schema_version: dream::DREAM_FIXTURE_SCHEMA_VERSION.map(str::to_string),
                    source_window_start,
                    source_window_end: Some(now),
                    source_counts: dream::source_counts(&source_records),
                    candidate_counts: dream::candidate_counts(&resp),
                    created_count: resp.created.len() as i64,
                    archived_count: resp.archived.len() as i64,
                    rejected_count: resp.rejected.len() as i64,
                    error_summary: None,
                })?;
                Ok(resp)
            }
            Err(err) => {
                let summary = sanitize_error_summary(&err.message);
                let _ = self.store.insert_dream_run(&dream_error_audit(
                    profile.as_str(),
                    &workspace,
                    repo_id.as_deref(),
                    &mode,
                    &started_at,
                    source_window_start.as_deref(),
                    Some(&now),
                    &summary,
                ));
                Err(err)
            }
        }
    }

    // ------------------------------------------------------------------
    // Sync local Codex memory
    // ------------------------------------------------------------------

    pub fn sync_local(&self, req: SyncRequest) -> Result<SyncResponse> {
        let profile = self.resolve_profile(&req.profile)?;
        let workspace = self.resolve_workspace(&req.workspace);
        let repo_id = self.register_repo(&req.repo)?;
        let source_root = req
            .source_root
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::new(ErrorCode::SyncSourceInvalid, "source_root is required"))?
            .to_string();
        let mode = SyncMode::parse(&req.mode.unwrap_or_else(|| "preview".to_string()))?;
        let files = req
            .files
            .ok_or_else(|| Error::invalid_request("files is required"))?;

        Metrics::add(&self.metrics.sync_scanned, files.len() as u64);

        let params = SyncParams {
            profile,
            workspace: &workspace,
            repo_id: repo_id.as_deref(),
            source_root: &source_root,
            mode,
            files: &files,
            max_record_chars: self.config.max_record_chars,
        };
        let resp = ingest::run_sync(&self.store, &params)?;
        Metrics::add(&self.metrics.sync_created, resp.created as u64);
        Metrics::add(&self.metrics.sync_skipped, resp.skipped as u64);
        Metrics::add(&self.metrics.sync_rejected, resp.rejected as u64);
        Ok(resp)
    }

    // ------------------------------------------------------------------
    // Forget
    // ------------------------------------------------------------------

    pub fn forget(&self, req: ForgetRequest) -> Result<ForgetResponse> {
        // Forget is profile-scoped: callers can only archive/delete records in
        // their own profile (and workspace, when supplied). Out-of-scope ids are
        // reported as not_found rather than touched (SPEC §4.1.2, §10.3).
        let profile = self.resolve_profile(&req.profile)?;
        let workspace = req
            .workspace
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(sanitize_workspace);
        let ids_list = req
            .ids
            .ok_or_else(|| Error::invalid_request("ids is required"))?;
        if ids_list.is_empty() {
            return Err(Error::invalid_request("ids must not be empty"));
        }
        let mode = req.mode.unwrap_or_else(|| "archive".to_string());
        match mode.trim().to_ascii_lowercase().as_str() {
            "delete" => {
                let (deleted, not_found) =
                    self.store
                        .delete_records(profile.as_str(), workspace.as_deref(), &ids_list)?;
                Ok(ForgetResponse {
                    archived: vec![],
                    deleted,
                    not_found,
                    errors: vec![],
                })
            }
            "archive" => {
                let (archived, not_found) = self.store.archive_records(
                    profile.as_str(),
                    workspace.as_deref(),
                    &ids_list,
                )?;
                Ok(ForgetResponse {
                    archived,
                    deleted: vec![],
                    not_found,
                    errors: vec![],
                })
            }
            other => Err(Error::invalid_request(format!(
                "invalid forget mode '{other}' (archive|delete)"
            ))),
        }
    }

    // ------------------------------------------------------------------
    // Export
    // ------------------------------------------------------------------

    pub fn export(&self, query: ExportQuery) -> Result<ExportResult> {
        let profile = self.resolve_profile(&query.profile)?;
        let target_profile = match &query.target_profile {
            Some(t) if !t.trim().is_empty() => Some(Profile::parse(t).ok_or_else(|| {
                Error::new(
                    ErrorCode::UnknownProfile,
                    format!("unknown target profile '{t}'"),
                )
            })?),
            _ => None,
        };
        let params = ExportParams {
            profile,
            workspace: query.workspace.as_deref(),
            repo_id: query.repo_id.as_deref(),
            include_archived: query.include_archived.unwrap_or(false),
            format: ExportFormat::parse(query.format.as_deref()),
            target_profile,
        };
        export::export(&self.store, &params)
    }
}

fn dream_error_audit(
    profile_id: &str,
    workspace_id: &str,
    repo_id: Option<&str>,
    mode: &str,
    started_at: &str,
    source_window_start: Option<&str>,
    source_window_end: Option<&str>,
    error_summary: &str,
) -> DreamRunAudit {
    let completed_at = ids::now_rfc3339();
    DreamRunAudit {
        id: ids::new_id("dream"),
        profile_id: profile_id.to_string(),
        workspace_id: workspace_id.to_string(),
        repo_id: repo_id.map(str::to_string),
        mode: mode.to_string(),
        status: "error".to_string(),
        started_at: started_at.to_string(),
        completed_at: Some(completed_at),
        implementation_version: dream::DREAM_IMPLEMENTATION_VERSION.to_string(),
        config_hash: dream::config_hash(),
        ruleset_version: dream::DREAM_RULESET_VERSION.to_string(),
        fixture_schema_version: dream::DREAM_FIXTURE_SCHEMA_VERSION.map(str::to_string),
        source_window_start: source_window_start.map(str::to_string),
        source_window_end: source_window_end.map(str::to_string),
        source_counts: json!({}),
        candidate_counts: json!({}),
        created_count: 0,
        archived_count: 0,
        rejected_count: 0,
        error_summary: Some(sanitize_error_summary(error_summary)),
    }
}

fn sanitize_error_summary(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(160)
        .collect()
}

fn parse_types(raw: &[String]) -> Vec<RecordType> {
    raw.iter().filter_map(|t| RecordType::parse(t)).collect()
}

fn sanitize_workspace(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '-') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed
    }
}

fn default_sensitivity(profile: Profile) -> Sensitivity {
    match profile {
        Profile::Work => Sensitivity::WorkConfidential,
        Profile::Personal => Sensitivity::Personal,
        Profile::Oss | Profile::Homelab => Sensitivity::Public,
    }
}

fn redact_for_echo(content: &str) -> String {
    // Never echo back possibly-secret content verbatim in a rejection.
    let preview: String = content.chars().take(48).collect();
    if content.chars().count() > 48 {
        format!("{preview}…")
    } else {
        preview
    }
}

fn map_code(code: &str) -> ErrorCode {
    match code {
        "secret_detected" => ErrorCode::SecretDetected,
        "policy_denied" => ErrorCode::PolicyDenied,
        "profile_boundary_denied" => ErrorCode::ProfileBoundaryDenied,
        "invalid_request" => ErrorCode::InvalidRequest,
        _ => ErrorCode::PolicyDenied,
    }
}
