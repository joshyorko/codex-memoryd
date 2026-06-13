//! Durable storage layer over SQLite (SPEC §3.1, §4). Owns the connection pool,
//! migrations, the FTS5 capability probe with LIKE fallback, and all CRUD /
//! search / dedupe operations.
//!
//! The store is intentionally free of policy logic: callers (ingest, server)
//! screen content via [`crate::policy`] before persisting.

use std::path::Path;
use std::path::PathBuf;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use rusqlite::OptionalExtension;
use rusqlite::Row;
use serde_json::Value;

use crate::domain::Checkpoint;
use crate::domain::Conclusion;
use crate::domain::MemoryRecord;
use crate::domain::MemorySource;
use crate::domain::Portability;
use crate::domain::RecordType;
use crate::domain::Scope;
use crate::domain::Sensitivity;
use crate::domain::VisibleTurn;
use crate::error::Error;
use crate::error::ErrorCode;
use crate::error::Result;
use crate::ids;

pub const STORAGE_SCHEMA_VERSION: i64 = 3;

const MIGRATION_INIT: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_FTS: &str = include_str!("../migrations/0002_fts.sql");
const MIGRATION_DREAM_RUNS: &str = include_str!("../migrations/0003_dream_runs.sql");
const MIGRATION_EVIDENCE_LEDGER: &str = include_str!("../migrations/0004_evidence_ledger.sql");

type SqlitePool = Pool<SqliteConnectionManager>;

/// Sync cursor timestamps: (last_started_at, last_completed_at, last_error).
pub type SyncCursorTimes = (Option<String>, Option<String>, Option<String>);

/// Append-only evidence ledger row.
#[derive(Debug, Clone)]
pub struct EvidenceLedgerEntry {
    pub profile_id: String,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub subject_key: Option<String>,
    pub source_kind: String,
    pub source_id: Option<String>,
    pub source_path: Option<String>,
    pub source_hash: String,
    pub safe_summary: String,
    pub policy_state: String,
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct DreamRunAudit {
    pub id: String,
    pub profile_id: String,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub mode: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub implementation_version: String,
    pub config_hash: String,
    pub ruleset_version: String,
    pub fixture_schema_version: Option<String>,
    pub source_window_start: Option<String>,
    pub source_window_end: Option<String>,
    pub source_counts: Value,
    pub candidate_counts: Value,
    pub created_count: i64,
    pub archived_count: i64,
    pub rejected_count: i64,
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DreamRunRecord {
    pub run_id: String,
    pub profile_id: String,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub mode: String,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub watermark_before: Option<String>,
    pub watermark_after: Option<String>,
    pub error: Option<String>,
    pub candidates: usize,
    pub created: usize,
    pub archived: usize,
    pub limits_hit: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DreamRunSummary {
    pub id: String,
    pub profile_id: String,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub mode: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub source_window_start: Option<String>,
    pub source_window_end: Option<String>,
    pub created_count: i64,
    pub archived_count: i64,
    pub rejected_count: i64,
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ScheduledDreamRunSummary {
    pub run_id: String,
    pub status: String,
    pub completed_at: Option<String>,
    pub watermark_after: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionActivity {
    pub started_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub turn_count: usize,
}

/// Normalize and cap a ledger summary so callers can safely pass content
/// excerpts, policy reasons, or operation summaries without raw blobs.
pub fn ledger_safe_summary(raw: &str) -> String {
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
        .take(240)
        .collect()
}

/// Filters applied to a record query.
#[derive(Debug, Clone, Default)]
pub struct RecordQuery {
    pub profile_id: Option<String>,
    pub workspace_id: Option<String>,
    pub repo_id: Option<String>,
    pub record_type: Option<RecordType>,
    pub scope: Option<Scope>,
    pub include_archived: bool,
    pub recency_cutoff: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// A new memory record to upsert. The store computes nothing here except
/// applying defaults; ids/hashes come from the caller.
#[derive(Debug, Clone)]
pub struct NewRecord {
    pub profile_id: String,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub scope: Scope,
    pub record_type: RecordType,
    pub content: String,
    pub related_files: Vec<String>,
    pub tags: Vec<String>,
    pub sensitivity: Sensitivity,
    pub portability: Portability,
    pub confidence: f64,
    pub source_ids: Vec<String>,
    pub content_hash: String,
    pub supersedes: Vec<String>,
    pub metadata: Value,
}

/// The result of an idempotent upsert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertOutcome {
    Created(String),
    Skipped(String),
}

impl UpsertOutcome {
    pub fn id(&self) -> &str {
        match self {
            UpsertOutcome::Created(id) | UpsertOutcome::Skipped(id) => id,
        }
    }
    pub fn created(&self) -> bool {
        matches!(self, UpsertOutcome::Created(_))
    }
}

/// The durable store handle. Cloneable (shares the pool).
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    fts_enabled: bool,
    path: PathBuf,
    degraded_reasons: Vec<String>,
}

impl Store {
    /// Open (or create) the SQLite store at `path`, run migrations, and probe
    /// FTS5. An in-memory store is created when `path` is `:memory:`.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref().to_path_buf();
        let is_memory = path.as_os_str() == ":memory:";
        let manager = if is_memory {
            SqliteConnectionManager::memory()
        } else {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        Error::storage(format!(
                            "create storage dir {}: {e}; check --db/CODEX_MEMORYD_DB path and directory permissions",
                            parent.display()
                        ))
                    })?;
                }
            }
            // Enable WAL once, up front, on a single connection. WAL is a
            // persistent property of the database file, so pooled connections
            // inherit it without each having to switch journal mode — which
            // would otherwise race to "database is locked" on a fresh file.
            let bootstrap = rusqlite::Connection::open(&path)
                .map_err(|e| Error::storage(format!("open {}: {e}", path.display())))?;
            bootstrap
                .execute_batch("PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;")
                .map_err(|e| {
                    Error::storage(format!(
                        "enable WAL for {}: {e}; check database write permissions",
                        path.display()
                    ))
                })?;
            SqliteConnectionManager::file(&path)
        };
        // Per-connection pragmas. journal_mode is intentionally NOT set here: it
        // is already persisted on the file (or irrelevant for in-memory).
        let manager = manager.with_init(|conn| {
            conn.execute_batch(
                "PRAGMA busy_timeout = 5000;
                 PRAGMA foreign_keys = ON;
                 PRAGMA synchronous = NORMAL;",
            )?;
            Ok(())
        });

        // In-memory pools must use a single connection or each checkout gets a
        // fresh empty database. Use max_size = 1 for memory.
        let max_size = if is_memory { 1 } else { 8 };
        let pool = Pool::builder()
            .max_size(max_size)
            .build(manager)
            .map_err(|e| Error::storage(format!("build pool: {e}")))?;

        let mut store = Store {
            pool,
            fts_enabled: false,
            path,
            degraded_reasons: Vec::new(),
        };
        store.migrate()?;
        Ok(store)
    }

    fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool.get().map_err(Error::from)
    }

    /// Run migrations and probe FTS5. Idempotent.
    fn migrate(&mut self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(MIGRATION_INIT)?;

        // Record schema version in a tiny meta table.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        conn.execute(
            "INSERT INTO schema_meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![STORAGE_SCHEMA_VERSION.to_string()],
        )?;
        conn.execute_batch(MIGRATION_DREAM_RUNS)?;
        conn.execute_batch(MIGRATION_EVIDENCE_LEDGER)?;

        // Probe FTS5 by attempting the virtual-table migration. If the SQLite
        // build lacks FTS5, this errors; we then fall back to LIKE search.
        match conn.execute_batch(MIGRATION_FTS) {
            Ok(()) => {
                self.fts_enabled = true;
            }
            Err(err) => {
                self.fts_enabled = false;
                self.degraded_reasons.push(format!(
                    "FTS5 unavailable, using LIKE search fallback: {err}"
                ));
            }
        }
        Ok(())
    }

    pub fn path_display(&self) -> String {
        self.path.display().to_string()
    }

    pub fn fts_enabled(&self) -> bool {
        self.fts_enabled
    }

    pub fn degraded_reasons(&self) -> &[String] {
        &self.degraded_reasons
    }

    /// Check the storage is writable by running a trivial transaction.
    pub fn writable(&self) -> bool {
        let Ok(conn) = self.conn() else {
            return false;
        };
        conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").is_ok()
    }

    // ------------------------------------------------------------------
    // Profiles & workspaces
    // ------------------------------------------------------------------

    /// Ensure a profile row exists (idempotent). Display name defaults to the id.
    pub fn ensure_profile(&self, profile_id: &str) -> Result<()> {
        let now = ids::now_rfc3339();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO profiles(id, display_name, created_at, updated_at, default_portability_policy)
             VALUES (?1, ?1, ?2, ?2, 'profile_only')
             ON CONFLICT(id) DO UPDATE SET updated_at = excluded.updated_at",
            params![profile_id, now],
        )?;
        Ok(())
    }

    /// Ensure a workspace row exists within a profile (idempotent).
    pub fn ensure_workspace(&self, profile_id: &str, workspace_id: &str) -> Result<()> {
        self.ensure_profile(profile_id)?;
        let now = ids::now_rfc3339();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO workspaces(id, profile_id, display_name, created_at, updated_at)
             VALUES (?1, ?2, ?1, ?3, ?3)
             ON CONFLICT(profile_id, id) DO UPDATE SET updated_at = excluded.updated_at",
            params![workspace_id, profile_id, now],
        )?;
        Ok(())
    }

    pub fn active_profiles(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT id FROM profiles ORDER BY id")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn active_workspaces(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT DISTINCT id FROM workspaces ORDER BY id")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn dream_session_activity(
        &self,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
    ) -> Result<SessionActivity> {
        let conn = self.conn()?;
        let mut sql = String::from(
            "SELECT MIN(s.started_at), MAX(COALESCE(t.created_at, s.started_at)), COUNT(t.id)
             FROM sessions s
             LEFT JOIN visible_turns t ON t.session_id = s.id
             WHERE s.profile_id = ?1 AND s.workspace_id = ?2",
        );
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(profile_id.to_string()),
            Box::new(workspace_id.to_string()),
        ];
        if let Some(repo) = repo_id {
            sql.push_str(" AND s.repo_id = ?");
            args.push(Box::new(repo.to_string()));
        } else {
            sql.push_str(" AND s.repo_id IS NULL");
        }
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let row = stmt.query_row(params_ref.as_slice(), |row| {
            Ok(SessionActivity {
                started_at: row.get(0)?,
                last_activity_at: row.get(1)?,
                turn_count: row.get::<_, i64>(2)? as usize,
            })
        })?;
        Ok(row)
    }

    // ------------------------------------------------------------------
    // Repos
    // ------------------------------------------------------------------

    pub fn ensure_repo(
        &self,
        repo_id: &str,
        root: Option<&str>,
        remote: Option<&str>,
        branch: Option<&str>,
        commit: Option<&str>,
        is_git: bool,
    ) -> Result<()> {
        let now = ids::now_rfc3339();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO repos(repo_id, root, remote, branch, commit_sha, is_git, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(repo_id) DO UPDATE SET
                root = excluded.root, remote = excluded.remote,
                branch = excluded.branch, commit_sha = excluded.commit_sha,
                is_git = excluded.is_git, updated_at = excluded.updated_at",
            params![repo_id, root, remote, branch, commit, is_git as i64, now],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Memory sources
    // ------------------------------------------------------------------

    /// Insert a memory source if its (profile,workspace,path,hash) is new.
    /// Returns (source, created?). On dedupe hit, returns the existing source.
    pub fn upsert_source(
        &self,
        profile_id: &str,
        workspace_id: &str,
        kind: &str,
        source_path: Option<&str>,
        source_hash: &str,
        metadata: &Value,
    ) -> Result<(MemorySource, bool)> {
        if let Some(existing) =
            self.find_source(profile_id, workspace_id, source_path, source_hash)?
        {
            return Ok((existing, false));
        }
        let now = ids::now_rfc3339();
        let id = ids::new_id("src");
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO memory_sources(id, profile_id, workspace_id, kind, source_path, source_hash, created_at, ingested_at, metadata)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?7,?8)",
            params![
                id,
                profile_id,
                workspace_id,
                kind,
                source_path,
                source_hash,
                now,
                metadata.to_string()
            ],
        )?;
        let source = MemorySource {
            id,
            profile_id: profile_id.to_string(),
            workspace_id: workspace_id.to_string(),
            kind: kind.to_string(),
            source_path: source_path.map(|s| s.to_string()),
            source_hash: source_hash.to_string(),
            created_at: now.clone(),
            ingested_at: now,
            metadata: metadata.clone(),
        };
        Ok((source, true))
    }

    pub fn find_source(
        &self,
        profile_id: &str,
        workspace_id: &str,
        source_path: Option<&str>,
        source_hash: &str,
    ) -> Result<Option<MemorySource>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                "SELECT id, profile_id, workspace_id, kind, source_path, source_hash, created_at, ingested_at, metadata
                 FROM memory_sources
                 WHERE profile_id = ?1 AND workspace_id = ?2 AND source_hash = ?3
                   AND (source_path IS ?4 OR source_path = ?4)",
                params![profile_id, workspace_id, source_hash, source_path],
                row_to_source,
            )
            .optional()?;
        Ok(result)
    }

    // ------------------------------------------------------------------
    // Memory records
    // ------------------------------------------------------------------

    /// Idempotent insert keyed on `content_hash`. If a record with the same
    /// content hash exists, returns `Skipped` and merges any new source ids.
    pub fn upsert_record(&self, new: &NewRecord) -> Result<UpsertOutcome> {
        if let Some(existing) = self.find_by_content_hash(&new.content_hash)? {
            // Merge new source ids and refresh updated_at; do not duplicate.
            if !new.source_ids.is_empty() {
                let mut merged = existing.source_ids.clone();
                for sid in &new.source_ids {
                    if !merged.contains(sid) {
                        merged.push(sid.clone());
                    }
                }
                let now = ids::now_rfc3339();
                let conn = self.conn()?;
                conn.execute(
                    "UPDATE memory_records SET source_ids = ?1, updated_at = ?2 WHERE id = ?3",
                    params![serde_json::to_string(&merged)?, now, existing.id],
                )?;
            }
            return Ok(UpsertOutcome::Skipped(existing.id));
        }

        let now = ids::now_rfc3339();
        let id = ids::new_id("mem");
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO memory_records(
                id, profile_id, workspace_id, repo_id, scope, type, content,
                related_files, tags, sensitivity, portability, confidence,
                source_ids, content_hash, supersedes, created_at, updated_at,
                last_used_at, archived, metadata)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?16,NULL,0,?17)",
            params![
                id,
                new.profile_id,
                new.workspace_id,
                new.repo_id,
                new.scope.as_str(),
                new.record_type.as_str(),
                new.content,
                serde_json::to_string(&new.related_files)?,
                serde_json::to_string(&new.tags)?,
                new.sensitivity.as_str(),
                new.portability.as_str(),
                new.confidence,
                serde_json::to_string(&new.source_ids)?,
                new.content_hash,
                serde_json::to_string(&new.supersedes)?,
                now,
                new.metadata.to_string(),
            ],
        )?;
        Ok(UpsertOutcome::Created(id))
    }

    /// Archive records and annotate their metadata with historical/supersession
    /// context. Records remain recoverable via include_archived queries.
    pub fn archive_records_with_metadata(
        &self,
        profile_id: &str,
        workspace_id: Option<&str>,
        ids_to_archive: &[String],
        state: &str,
        historical_reason: &str,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let now = ids::now_rfc3339();
        let mut archived = Vec::new();
        let mut not_found = Vec::new();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut select = tx.prepare("SELECT metadata FROM memory_records WHERE id = ?1")?;
            let mut update = tx.prepare(
                "UPDATE memory_records SET archived = 1, updated_at = ?1, metadata = ?2 WHERE id = ?3 AND archived = 0",
            )?;
            for id in ids_to_archive {
                if !self.scoped_record_exists(&tx, id, profile_id, workspace_id)? {
                    not_found.push(id.clone());
                    continue;
                }
                let raw: String = select.query_row(params![id], |r| r.get(0)).map_err(|e| {
                    Error::storage(format!("load metadata for archived record {id}: {e}"))
                })?;
                let mut metadata = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
                if !metadata.is_object() {
                    metadata = serde_json::json!({});
                }
                if let Some(obj) = metadata.as_object_mut() {
                    obj.insert("state".to_string(), Value::String(state.to_string()));
                    obj.insert(
                        "historical_reason".to_string(),
                        Value::String(historical_reason.to_string()),
                    );
                    obj.insert("archived_at".to_string(), Value::String(now.clone()));
                }
                if update.execute(params![now, metadata.to_string(), id])? > 0 {
                    archived.push(id.clone());
                }
            }
        }
        tx.commit()?;
        Ok((archived, not_found))
    }

    pub fn find_by_content_hash(&self, content_hash: &str) -> Result<Option<MemoryRecord>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                &format!("SELECT {RECORD_COLS} FROM memory_records WHERE content_hash = ?1"),
                params![content_hash],
                row_to_record,
            )
            .optional()?;
        Ok(result)
    }

    pub fn get_record(&self, id: &str) -> Result<Option<MemoryRecord>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                &format!("SELECT {RECORD_COLS} FROM memory_records WHERE id = ?1"),
                params![id],
                row_to_record,
            )
            .optional()?;
        Ok(result)
    }

    pub fn count_records(&self) -> Result<i64> {
        let conn = self.conn()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM memory_records", [], |r| r.get(0))?;
        Ok(n)
    }

    /// Update `last_used_at` for a batch of records (recall touch).
    pub fn touch_records(&self, ids_to_touch: &[String]) -> Result<()> {
        if ids_to_touch.is_empty() {
            return Ok(());
        }
        let now = ids::now_rfc3339();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt =
                tx.prepare("UPDATE memory_records SET last_used_at = ?1 WHERE id = ?2")?;
            for id in ids_to_touch {
                stmt.execute(params![now, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Archive records by id (soft delete), scoped to a profile (and optionally
    /// a workspace) so callers cannot touch records outside their boundary
    /// (SPEC §4.1.2, §10.3). Records that exist but fall outside the scope are
    /// reported as `not_found` to avoid leaking cross-profile existence.
    /// Returns (archived, not_found).
    pub fn archive_records(
        &self,
        profile_id: &str,
        workspace_id: Option<&str>,
        ids_to_archive: &[String],
    ) -> Result<(Vec<String>, Vec<String>)> {
        let now = ids::now_rfc3339();
        let mut archived = Vec::new();
        let mut not_found = Vec::new();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE memory_records SET archived = 1, updated_at = ?1 WHERE id = ?2 AND archived = 0",
            )?;
            for id in ids_to_archive {
                if !self.scoped_record_exists(&tx, id, profile_id, workspace_id)? {
                    not_found.push(id.clone());
                    continue;
                }
                if stmt.execute(params![now, id])? > 0 {
                    archived.push(id.clone());
                }
            }
        }
        tx.commit()?;
        Ok((archived, not_found))
    }

    /// Hard delete records by id, scoped to a profile (and optionally a
    /// workspace). Returns (deleted, not_found).
    pub fn delete_records(
        &self,
        profile_id: &str,
        workspace_id: Option<&str>,
        ids_to_delete: &[String],
    ) -> Result<(Vec<String>, Vec<String>)> {
        let mut deleted = Vec::new();
        let mut not_found = Vec::new();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut del_stmt = tx.prepare("DELETE FROM memory_records WHERE id = ?1")?;
            for id in ids_to_delete {
                if !self.scoped_record_exists(&tx, id, profile_id, workspace_id)? {
                    not_found.push(id.clone());
                    continue;
                }
                del_stmt.execute(params![id])?;
                deleted.push(id.clone());
            }
        }
        tx.commit()?;
        Ok((deleted, not_found))
    }

    /// Does a record with `id` exist within the given profile/workspace scope?
    fn scoped_record_exists(
        &self,
        tx: &rusqlite::Transaction,
        id: &str,
        profile_id: &str,
        workspace_id: Option<&str>,
    ) -> Result<bool> {
        let exists = match workspace_id {
            Some(ws) => tx.prepare(
                "SELECT 1 FROM memory_records WHERE id = ?1 AND profile_id = ?2 AND workspace_id = ?3",
            )?
            .exists(params![id, profile_id, ws])?,
            None => tx
                .prepare("SELECT 1 FROM memory_records WHERE id = ?1 AND profile_id = ?2")?
                .exists(params![id, profile_id])?,
        };
        Ok(exists)
    }

    /// Count active (non-archived) records derived from a given local import
    /// path within a profile/workspace.
    pub fn count_active_records_for_path(
        &self,
        profile_id: &str,
        workspace_id: &str,
        local_path: &str,
    ) -> Result<i64> {
        let conn = self.conn()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_records
             WHERE profile_id = ?1 AND workspace_id = ?2 AND archived = 0
               AND json_extract(metadata, '$.local_path') = ?3",
            params![profile_id, workspace_id, local_path],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Archive active records derived from `local_path` whose content_hash is
    /// NOT in `keep_hashes`. Used when a local file's content changed on
    /// re-import: prior chunks that no longer exist in the file are superseded
    /// (archived) rather than left to contradict the fresh import (SPEC §4.1.7
    /// "prefer updating or superseding old memories over duplicating").
    /// Returns the number of records archived.
    pub fn archive_stale_path_records(
        &self,
        profile_id: &str,
        workspace_id: &str,
        local_path: &str,
        keep_hashes: &[String],
    ) -> Result<usize> {
        let now = ids::now_rfc3339();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut archived = 0usize;
        {
            // Gather candidate ids first (can't easily bind a NOT IN list).
            let mut select = tx.prepare(
                "SELECT id, content_hash FROM memory_records
                 WHERE profile_id = ?1 AND workspace_id = ?2 AND archived = 0
                   AND json_extract(metadata, '$.local_path') = ?3",
            )?;
            let rows = select
                .query_map(params![profile_id, workspace_id, local_path], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut update = tx
                .prepare("UPDATE memory_records SET archived = 1, updated_at = ?1 WHERE id = ?2")?;
            for (id, hash) in rows {
                if !keep_hashes.iter().any(|h| h == &hash) {
                    update.execute(params![now, id])?;
                    archived += 1;
                }
            }
        }
        tx.commit()?;
        Ok(archived)
    }

    /// Filtered listing without text search (used by export and recall
    /// candidate gathering).
    pub fn query_records(&self, query: &RecordQuery) -> Result<Vec<MemoryRecord>> {
        let conn = self.conn()?;
        let mut sql = format!("SELECT {RECORD_COLS} FROM memory_records WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(p) = &query.profile_id {
            sql.push_str(" AND profile_id = ?");
            args.push(Box::new(p.clone()));
        }
        if let Some(w) = &query.workspace_id {
            sql.push_str(" AND workspace_id = ?");
            args.push(Box::new(w.clone()));
        }
        if let Some(r) = &query.repo_id {
            sql.push_str(" AND repo_id = ?");
            args.push(Box::new(r.clone()));
        }
        if let Some(t) = &query.record_type {
            sql.push_str(" AND type = ?");
            args.push(Box::new(t.as_str().to_string()));
        }
        if let Some(s) = &query.scope {
            sql.push_str(" AND scope = ?");
            args.push(Box::new(s.as_str().to_string()));
        }
        if !query.include_archived {
            sql.push_str(" AND archived = 0");
        }
        // Never surface secret-blocked records in any query (SPEC §6.2/§6.3).
        sql.push_str(" AND sensitivity != 'secret_blocked'");
        if let Some(cutoff) = &query.recency_cutoff {
            sql.push_str(" AND updated_at >= ?");
            args.push(Box::new(cutoff.clone()));
        }
        sql.push_str(" ORDER BY updated_at DESC");
        if query.limit > 0 {
            sql.push_str(&format!(" LIMIT {} OFFSET {}", query.limit, query.offset));
        }

        let mut stmt = conn.prepare(&sql)?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_record)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Export rows and the matching `secret_blocked` count from one read
    /// transaction so the omitted count matches the exported snapshot.
    pub fn export_records(&self, query: &RecordQuery) -> Result<(Vec<MemoryRecord>, usize)> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;

        let omitted_secret = {
            let mut sql = "SELECT COUNT(*) FROM memory_records WHERE 1=1".to_string();
            let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            append_export_scope_filters(&mut sql, &mut args, query);
            sql.push_str(" AND sensitivity = 'secret_blocked'");

            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                args.iter().map(|b| b.as_ref()).collect();
            let count: i64 = tx.query_row(&sql, params_ref.as_slice(), |row| row.get(0))?;
            count as usize
        };

        let rows = {
            let mut sql = format!("SELECT {RECORD_COLS} FROM memory_records WHERE 1=1");
            let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            append_export_scope_filters(&mut sql, &mut args, query);
            sql.push_str(" AND sensitivity != 'secret_blocked'");
            sql.push_str(" ORDER BY updated_at DESC");
            if query.limit > 0 {
                sql.push_str(&format!(" LIMIT {} OFFSET {}", query.limit, query.offset));
            }

            let mut stmt = tx.prepare(&sql)?;
            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                args.iter().map(|b| b.as_ref()).collect();
            let rows = stmt
                .query_map(params_ref.as_slice(), row_to_record)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };

        tx.commit()?;
        Ok((rows, omitted_secret))
    }

    /// Full-text-ish search. Uses FTS5 when available, otherwise LIKE.
    /// Returns records ranked by relevance (FTS) or recency (LIKE).
    pub fn search_records(
        &self,
        query_text: &str,
        filters: &RecordQuery,
    ) -> Result<Vec<MemoryRecord>> {
        let trimmed = query_text.trim();
        if trimmed.is_empty() {
            return self.query_records(filters);
        }
        if self.fts_enabled {
            match self.search_fts(trimmed, filters) {
                Ok(rows) => return Ok(rows),
                Err(_) => {
                    // FTS query syntax can choke on punctuation; fall back.
                }
            }
        }
        self.search_like(trimmed, filters)
    }

    fn search_fts(&self, query_text: &str, filters: &RecordQuery) -> Result<Vec<MemoryRecord>> {
        let conn = self.conn()?;
        let mut sql = format!(
            "SELECT {} FROM memory_records r
             JOIN memory_records_fts f ON f.id = r.id
             WHERE memory_records_fts MATCH ?1",
            record_cols_prefixed("r")
        );
        let match_query = fts_match_query(query_text);
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(match_query)];
        append_record_filters(&mut sql, &mut args, filters, "r");
        sql.push_str(" ORDER BY bm25(memory_records_fts)");
        if filters.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", filters.limit));
        }

        let mut stmt = conn.prepare(&sql)?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_record)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn search_like(&self, query_text: &str, filters: &RecordQuery) -> Result<Vec<MemoryRecord>> {
        let conn = self.conn()?;
        let mut sql = format!(
            "SELECT {RECORD_COLS} FROM memory_records WHERE (content LIKE ?1 OR tags LIKE ?1 OR related_files LIKE ?1)"
        );
        let like = format!("%{}%", escape_like(query_text));
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(like)];
        append_record_filters(&mut sql, &mut args, filters, "");
        sql.push_str(" ORDER BY updated_at DESC");
        if filters.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", filters.limit));
        }

        let mut stmt = conn.prepare(&sql)?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_record)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------------
    // Checkpoints
    // ------------------------------------------------------------------

    pub fn insert_checkpoint(&self, cp: &Checkpoint) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO checkpoints(
                id, session_id, profile_id, workspace_id, repo_id, summary,
                changed_files, decisions, blockers, next_steps, tests_run,
                tests_not_run, branch, commit_sha, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                cp.id,
                cp.session_id,
                cp.profile_id,
                cp.workspace_id,
                cp.repo_id,
                cp.summary,
                serde_json::to_string(&cp.changed_files)?,
                serde_json::to_string(&cp.decisions)?,
                serde_json::to_string(&cp.blockers)?,
                serde_json::to_string(&cp.next_steps)?,
                serde_json::to_string(&cp.tests_run)?,
                serde_json::to_string(&cp.tests_not_run)?,
                cp.branch,
                cp.commit,
                cp.created_at,
            ],
        )?;
        Ok(())
    }

    /// Recent checkpoints for a profile/workspace, optionally repo-scoped first.
    pub fn recent_checkpoints(
        &self,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Checkpoint>> {
        let conn = self.conn()?;
        // repo-matching checkpoints sort first, then by recency.
        let mut stmt = conn.prepare(
            "SELECT id, session_id, profile_id, workspace_id, repo_id, summary,
                    changed_files, decisions, blockers, next_steps, tests_run,
                    tests_not_run, branch, commit_sha, created_at
             FROM checkpoints
             WHERE profile_id = ?1 AND workspace_id = ?2
             ORDER BY (CASE WHEN repo_id IS ?3 OR repo_id = ?3 THEN 0 ELSE 1 END), created_at DESC
             LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(
                params![profile_id, workspace_id, repo_id, limit as i64],
                row_to_checkpoint,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Deterministic checkpoint listing for Dream preview evidence gathering.
    pub fn dream_checkpoints(
        &self,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Checkpoint>> {
        let conn = self.conn()?;
        let mut sql = "SELECT id, session_id, profile_id, workspace_id, repo_id, summary,
                changed_files, decisions, blockers, next_steps, tests_run,
                tests_not_run, branch, commit_sha, created_at
             FROM checkpoints
             WHERE profile_id = ? AND workspace_id = ?"
            .to_string();
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(profile_id.to_string()),
            Box::new(workspace_id.to_string()),
        ];
        if let Some(repo_id) = repo_id {
            sql.push_str(" AND repo_id = ?");
            args.push(Box::new(repo_id.to_string()));
        }
        if let Some(since) = since {
            sql.push_str(" AND created_at >= ?");
            args.push(Box::new(since.to_string()));
        }
        sql.push_str(" ORDER BY created_at DESC, id ASC");
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_checkpoint)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------------
    // Conclusions
    // ------------------------------------------------------------------

    pub fn insert_conclusion(&self, c: &Conclusion) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO conclusions(id, profile_id, workspace_id, repo_id, target, content, source_id, created_at, metadata)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                c.id,
                c.profile_id,
                c.workspace_id,
                c.repo_id,
                c.target,
                c.content,
                c.source_id,
                c.created_at,
                c.metadata.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Deterministic conclusion listing for Dream preview evidence gathering.
    pub fn dream_conclusions(
        &self,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Conclusion>> {
        let conn = self.conn()?;
        let mut sql =
            "SELECT id, profile_id, workspace_id, repo_id, target, content, source_id, created_at, metadata
             FROM conclusions
             WHERE profile_id = ? AND workspace_id = ?"
                .to_string();
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(profile_id.to_string()),
            Box::new(workspace_id.to_string()),
        ];
        if let Some(repo_id) = repo_id {
            sql.push_str(" AND repo_id = ?");
            args.push(Box::new(repo_id.to_string()));
        }
        if let Some(since) = since {
            sql.push_str(" AND created_at >= ?");
            args.push(Box::new(since.to_string()));
        }
        sql.push_str(" ORDER BY created_at DESC, id ASC");
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_conclusion)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------------
    // Visible turns
    // ------------------------------------------------------------------

    pub fn insert_visible_turn(&self, t: &VisibleTurn) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO visible_turns(id, session_id, actor, content, created_at, metadata)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                t.id,
                t.session_id,
                t.actor,
                t.content,
                t.created_at,
                t.metadata.to_string()
            ],
        )?;
        Ok(())
    }

    /// Deterministic visible-turn listing for Dream preview evidence gathering.
    pub fn dream_visible_turns(
        &self,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<VisibleTurn>> {
        let conn = self.conn()?;
        let mut sql = "SELECT t.id, t.session_id, t.actor, t.content, t.created_at, t.metadata
             FROM visible_turns t
             JOIN sessions s ON s.id = t.session_id
             WHERE s.profile_id = ? AND s.workspace_id = ?"
            .to_string();
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(profile_id.to_string()),
            Box::new(workspace_id.to_string()),
        ];
        if let Some(repo_id) = repo_id {
            sql.push_str(" AND s.repo_id = ?");
            args.push(Box::new(repo_id.to_string()));
        }
        if let Some(since) = since {
            sql.push_str(" AND t.created_at >= ?");
            args.push(Box::new(since.to_string()));
        }
        sql.push_str(" ORDER BY t.created_at DESC, t.id ASC");
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_visible_turn)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Deterministic imported-source listing for Dream preview evidence gathering.
    pub fn dream_memory_sources(
        &self,
        profile_id: &str,
        workspace_id: &str,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemorySource>> {
        let conn = self.conn()?;
        let mut sql =
            "SELECT id, profile_id, workspace_id, kind, source_path, source_hash, created_at, ingested_at, metadata
             FROM memory_sources
             WHERE profile_id = ? AND workspace_id = ?"
                .to_string();
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(profile_id.to_string()),
            Box::new(workspace_id.to_string()),
        ];
        if let Some(since) = since {
            sql.push_str(" AND ingested_at >= ?");
            args.push(Box::new(since.to_string()));
        }
        sql.push_str(" ORDER BY ingested_at DESC, id ASC");
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_source)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn ensure_session(
        &self,
        session_id: &str,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
        thread_id: Option<&str>,
        source: &str,
    ) -> Result<()> {
        let now = ids::now_rfc3339();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sessions(id, profile_id, workspace_id, repo_id, thread_id, source, started_at, ended_at, metadata)
             VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,'{}')
             ON CONFLICT(id) DO NOTHING",
            params![session_id, profile_id, workspace_id, repo_id, thread_id, source, now],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Dream run audit/watermarks
    // ------------------------------------------------------------------

    pub fn record_dream_run(&self, run: &DreamRunRecord) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO dream_runs(
                run_id, profile_id, workspace_id, repo_id, mode, kind, status,
                started_at, completed_at, watermark_before, watermark_after, error,
                candidates, created, archived, limits_hit
             )
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                run.run_id,
                run.profile_id,
                run.workspace_id,
                run.repo_id,
                run.mode,
                run.kind,
                run.status,
                run.started_at,
                run.completed_at,
                run.watermark_before,
                run.watermark_after,
                run.error,
                run.candidates as i64,
                run.created as i64,
                run.archived as i64,
                serde_json::to_string(&run.limits_hit).unwrap_or_else(|_| "[]".to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn last_scheduled_dream_run(&self) -> Result<Option<ScheduledDreamRunSummary>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                "SELECT run_id, status, completed_at, watermark_after, error
                 FROM dream_runs
                 WHERE kind = 'scheduled'
                 ORDER BY started_at DESC, run_id DESC
                 LIMIT 1",
                [],
                |row| {
                    Ok(ScheduledDreamRunSummary {
                        run_id: row.get(0)?,
                        status: row.get(1)?,
                        completed_at: row.get(2)?,
                        watermark_after: row.get(3)?,
                        error: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    pub fn scheduled_dream_watermark(
        &self,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
    ) -> Result<Option<String>> {
        let conn = self.conn()?;
        let result = if let Some(repo_id) = repo_id {
            conn.query_row(
                "SELECT watermark_after
                 FROM dream_runs
                 WHERE kind = 'scheduled' AND status IN ('ok', 'ok_with_limits')
                   AND watermark_after IS NOT NULL
                   AND profile_id = ?1 AND workspace_id = ?2 AND repo_id = ?3
                 ORDER BY completed_at DESC, run_id DESC
                 LIMIT 1",
                params![profile_id, workspace_id, repo_id],
                |row| row.get(0),
            )
            .optional()?
        } else {
            conn.query_row(
                "SELECT watermark_after
                 FROM dream_runs
                 WHERE kind = 'scheduled' AND status IN ('ok', 'ok_with_limits')
                   AND watermark_after IS NOT NULL
                   AND profile_id = ?1 AND workspace_id = ?2 AND repo_id IS NULL
                 ORDER BY completed_at DESC, run_id DESC
                 LIMIT 1",
                params![profile_id, workspace_id],
                |row| row.get(0),
            )
            .optional()?
        };
        Ok(result)
    }

    // ------------------------------------------------------------------
    // Sync cursors
    // ------------------------------------------------------------------

    pub fn start_sync_cursor(
        &self,
        profile_id: &str,
        workspace_id: &str,
        source_root: &str,
    ) -> Result<()> {
        let now = ids::now_rfc3339();
        let id = ids::new_id("sync");
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sync_cursors(id, profile_id, workspace_id, source_root, last_started_at, last_completed_at, last_error, metadata)
             VALUES (?1,?2,?3,?4,?5,NULL,NULL,'{}')
             ON CONFLICT(profile_id, workspace_id, source_root)
             DO UPDATE SET last_started_at = excluded.last_started_at, last_error = NULL",
            params![id, profile_id, workspace_id, source_root, now],
        )?;
        Ok(())
    }

    pub fn complete_sync_cursor(
        &self,
        profile_id: &str,
        workspace_id: &str,
        source_root: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let now = ids::now_rfc3339();
        let conn = self.conn()?;
        conn.execute(
            "UPDATE sync_cursors SET last_completed_at = ?1, last_error = ?2
             WHERE profile_id = ?3 AND workspace_id = ?4 AND source_root = ?5",
            params![now, error, profile_id, workspace_id, source_root],
        )?;
        Ok(())
    }

    pub fn get_sync_cursor(
        &self,
        profile_id: &str,
        workspace_id: &str,
        source_root: &str,
    ) -> Result<Option<SyncCursorTimes>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                "SELECT last_started_at, last_completed_at, last_error FROM sync_cursors
                 WHERE profile_id = ?1 AND workspace_id = ?2 AND source_root = ?3",
                params![profile_id, workspace_id, source_root],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(result)
    }

    pub fn last_sync_completed(&self) -> Result<Option<String>> {
        let conn = self.conn()?;
        let result: Option<String> = conn
            .query_row(
                "SELECT MAX(last_completed_at) FROM sync_cursors",
                [],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(result)
    }

    // ------------------------------------------------------------------
    // Dreamer audit runs
    // ------------------------------------------------------------------

    pub fn insert_dream_run(&self, run: &DreamRunAudit) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO dream_runs(
                id, profile_id, workspace_id, repo_id, mode, status,
                started_at, completed_at, implementation_version, config_hash,
                ruleset_version, fixture_schema_version, source_window_start,
                source_window_end, source_counts, candidate_counts,
                created_count, archived_count, rejected_count, error_summary)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            params![
                run.id,
                run.profile_id,
                run.workspace_id,
                run.repo_id,
                run.mode,
                run.status,
                run.started_at,
                run.completed_at,
                run.implementation_version,
                run.config_hash,
                run.ruleset_version,
                run.fixture_schema_version,
                run.source_window_start,
                run.source_window_end,
                run.source_counts.to_string(),
                run.candidate_counts.to_string(),
                run.created_count,
                run.archived_count,
                run.rejected_count,
                run.error_summary,
            ],
        )?;
        Ok(())
    }

    pub fn dream_watermark(
        &self,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
    ) -> Result<Option<String>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                "SELECT source_window_end FROM dream_runs
                 WHERE profile_id = ?1
                   AND workspace_id = ?2
                   AND ((repo_id IS NULL AND ?3 IS NULL) OR repo_id = ?3)
                   AND status = 'ok'
                   AND mode = 'apply'
                   AND completed_at IS NOT NULL
                   AND source_window_end IS NOT NULL
                 ORDER BY source_window_end DESC, completed_at DESC
                 LIMIT 1",
                params![profile_id, workspace_id, repo_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(result)
    }

    pub fn last_dream_run(&self) -> Result<Option<DreamRunSummary>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                "SELECT id, profile_id, workspace_id, repo_id, mode, status,
                        started_at, completed_at, source_window_start,
                        source_window_end, created_count, archived_count,
                        rejected_count, error_summary
                 FROM dream_runs
                 WHERE id IS NOT NULL
                 ORDER BY started_at DESC
                 LIMIT 1",
                [],
                row_to_dream_summary,
            )
            .optional()?;
        Ok(result)
    }

    // ------------------------------------------------------------------
    // Policy events
    // ------------------------------------------------------------------

    pub fn record_policy_event(
        &self,
        profile_id: Option<&str>,
        workspace_id: Option<&str>,
        kind: &str,
        code: &str,
        reason: &str,
        context: &str,
    ) -> Result<()> {
        let now = ids::now_rfc3339();
        let id = ids::new_id("pol");
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO policy_events(id, profile_id, workspace_id, kind, code, reason, context, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id, profile_id, workspace_id, kind, code, reason, context, now],
        )?;
        Ok(())
    }

    pub fn record_evidence_ledger(&self, entry: &EvidenceLedgerEntry) -> Result<()> {
        let now = ids::now_rfc3339();
        let id = ids::new_id("led");
        let event_key = evidence_ledger_event_key(entry);
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO evidence_ledger(
                id, event_key, profile_id, workspace_id, repo_id, subject_key,
                source_kind, source_id, source_path, source_hash, safe_summary,
                policy_state, created_at, metadata
             )
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                id,
                event_key,
                &entry.profile_id,
                &entry.workspace_id,
                &entry.repo_id,
                &entry.subject_key,
                &entry.source_kind,
                &entry.source_id,
                &entry.source_path,
                &entry.source_hash,
                &entry.safe_summary,
                &entry.policy_state,
                now,
                entry.metadata.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn count_policy_denials(&self) -> Result<i64> {
        let conn = self.conn()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM policy_events WHERE kind != 'accepted'",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }
}

fn evidence_ledger_event_key(entry: &EvidenceLedgerEntry) -> String {
    ids::sha256_hex(
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            entry.profile_id,
            entry.workspace_id,
            entry.repo_id.as_deref().unwrap_or(""),
            entry.subject_key.as_deref().unwrap_or(""),
            entry.source_kind,
            entry.source_id.as_deref().unwrap_or(""),
            entry.source_path.as_deref().unwrap_or(""),
            entry.source_hash,
            entry.policy_state,
        )
        .as_bytes(),
    )
}

// ----------------------------------------------------------------------
// Row mappers and SQL helpers
// ----------------------------------------------------------------------

const RECORD_COLS: &str = "id, profile_id, workspace_id, repo_id, scope, type, content, related_files, tags, sensitivity, portability, confidence, source_ids, content_hash, supersedes, created_at, updated_at, last_used_at, archived, metadata";

fn record_cols_prefixed(prefix: &str) -> String {
    RECORD_COLS
        .split(", ")
        .map(|c| format!("{prefix}.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn append_record_filters(
    sql: &mut String,
    args: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    filters: &RecordQuery,
    prefix: &str,
) {
    let col = |name: &str| {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        }
    };
    if let Some(p) = &filters.profile_id {
        sql.push_str(&format!(" AND {} = ?", col("profile_id")));
        args.push(Box::new(p.clone()));
    }
    if let Some(w) = &filters.workspace_id {
        sql.push_str(&format!(" AND {} = ?", col("workspace_id")));
        args.push(Box::new(w.clone()));
    }
    if let Some(r) = &filters.repo_id {
        sql.push_str(&format!(" AND {} = ?", col("repo_id")));
        args.push(Box::new(r.clone()));
    }
    if let Some(t) = &filters.record_type {
        sql.push_str(&format!(" AND {} = ?", col("type")));
        args.push(Box::new(t.as_str().to_string()));
    }
    if let Some(s) = &filters.scope {
        sql.push_str(&format!(" AND {} = ?", col("scope")));
        args.push(Box::new(s.as_str().to_string()));
    }
    if !filters.include_archived {
        sql.push_str(&format!(" AND {} = 0", col("archived")));
    }
    sql.push_str(&format!(" AND {} != 'secret_blocked'", col("sensitivity")));
    if let Some(cutoff) = &filters.recency_cutoff {
        sql.push_str(&format!(" AND {} >= ?", col("updated_at")));
        args.push(Box::new(cutoff.clone()));
    }
}

fn append_export_scope_filters(
    sql: &mut String,
    args: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    query: &RecordQuery,
) {
    if let Some(p) = &query.profile_id {
        sql.push_str(" AND profile_id = ?");
        args.push(Box::new(p.clone()));
    }
    if let Some(w) = &query.workspace_id {
        sql.push_str(" AND workspace_id = ?");
        args.push(Box::new(w.clone()));
    }
    if let Some(r) = &query.repo_id {
        sql.push_str(" AND repo_id = ?");
        args.push(Box::new(r.clone()));
    }
    if !query.include_archived {
        sql.push_str(" AND archived = 0");
    }
}

/// Build a safe FTS5 MATCH query: split on whitespace, quote each token, OR them.
fn fts_match_query(raw: &str) -> String {
    let tokens: Vec<String> = raw
        .split_whitespace()
        .filter_map(|tok| {
            let cleaned: String = tok
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                Some(format!("\"{cleaned}\""))
            }
        })
        .collect();
    if tokens.is_empty() {
        "\"\"".to_string()
    } else {
        tokens.join(" OR ")
    }
}

fn escape_like(raw: &str) -> String {
    raw.replace(['%', '_'], "")
}

fn json_str_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn json_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or(Value::Null)
}

fn row_to_record(row: &Row) -> rusqlite::Result<MemoryRecord> {
    Ok(MemoryRecord {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        repo_id: row.get(3)?,
        scope: Scope::parse(&row.get::<_, String>(4)?).unwrap_or(Scope::Workspace),
        record_type: RecordType::parse(&row.get::<_, String>(5)?).unwrap_or(RecordType::Other),
        content: row.get(6)?,
        related_files: json_str_list(&row.get::<_, String>(7)?),
        tags: json_str_list(&row.get::<_, String>(8)?),
        sensitivity: Sensitivity::parse(&row.get::<_, String>(9)?).unwrap_or(Sensitivity::Personal),
        portability: Portability::parse(&row.get::<_, String>(10)?)
            .unwrap_or(Portability::ProfileOnly),
        confidence: row.get(11)?,
        source_ids: json_str_list(&row.get::<_, String>(12)?),
        content_hash: row.get(13)?,
        supersedes: json_str_list(&row.get::<_, String>(14)?),
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        last_used_at: row.get(17)?,
        archived: row.get::<_, i64>(18)? != 0,
        metadata: json_value(&row.get::<_, String>(19)?),
    })
}

fn row_to_source(row: &Row) -> rusqlite::Result<MemorySource> {
    Ok(MemorySource {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        kind: row.get(3)?,
        source_path: row.get(4)?,
        source_hash: row.get(5)?,
        created_at: row.get(6)?,
        ingested_at: row.get(7)?,
        metadata: json_value(&row.get::<_, String>(8)?),
    })
}

fn row_to_dream_summary(row: &Row) -> rusqlite::Result<DreamRunSummary> {
    Ok(DreamRunSummary {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        repo_id: row.get(3)?,
        mode: row.get(4)?,
        status: row.get(5)?,
        started_at: row.get(6)?,
        completed_at: row.get(7)?,
        source_window_start: row.get(8)?,
        source_window_end: row.get(9)?,
        created_count: row.get(10)?,
        archived_count: row.get(11)?,
        rejected_count: row.get(12)?,
        error_summary: row.get(13)?,
    })
}

fn row_to_conclusion(row: &Row) -> rusqlite::Result<Conclusion> {
    Ok(Conclusion {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        repo_id: row.get(3)?,
        target: row.get(4)?,
        content: row.get(5)?,
        source_id: row.get(6)?,
        created_at: row.get(7)?,
        metadata: json_value(&row.get::<_, String>(8)?),
    })
}

fn row_to_checkpoint(row: &Row) -> rusqlite::Result<Checkpoint> {
    Ok(Checkpoint {
        id: row.get(0)?,
        session_id: row.get(1)?,
        profile_id: row.get(2)?,
        workspace_id: row.get(3)?,
        repo_id: row.get(4)?,
        summary: row.get(5)?,
        changed_files: json_str_list(&row.get::<_, String>(6)?),
        decisions: json_str_list(&row.get::<_, String>(7)?),
        blockers: json_str_list(&row.get::<_, String>(8)?),
        next_steps: json_str_list(&row.get::<_, String>(9)?),
        tests_run: json_str_list(&row.get::<_, String>(10)?),
        tests_not_run: json_str_list(&row.get::<_, String>(11)?),
        branch: row.get(12)?,
        commit: row.get(13)?,
        created_at: row.get(14)?,
    })
}

fn row_to_visible_turn(row: &Row) -> rusqlite::Result<VisibleTurn> {
    Ok(VisibleTurn {
        id: row.get(0)?,
        session_id: row.get(1)?,
        actor: row.get(2)?,
        content: row.get(3)?,
        created_at: row.get(4)?,
        metadata: json_value(&row.get::<_, String>(5)?),
    })
}

/// Map a "not found" sqlite outcome to our NotFound error code.
pub fn not_found(what: &str) -> Error {
    Error::new(ErrorCode::NotFound, format!("{what} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids;

    fn mem_store() -> Store {
        Store::open(":memory:").expect("open in-memory store")
    }

    fn sample_record(content: &str) -> NewRecord {
        NewRecord {
            profile_id: "personal".to_string(),
            workspace_id: "ws".to_string(),
            repo_id: None,
            scope: Scope::Workspace,
            record_type: RecordType::Decision,
            content: content.to_string(),
            related_files: vec![],
            tags: vec!["decision".to_string()],
            sensitivity: Sensitivity::Personal,
            portability: Portability::ProfileOnly,
            confidence: 0.9,
            source_ids: vec![],
            content_hash: ids::content_hash(
                "personal",
                "ws",
                None,
                "decision",
                "workspace",
                content,
            ),
            supersedes: vec![],
            metadata: Value::Null,
        }
    }

    #[test]
    fn migrations_run_and_schema_version_set() {
        let store = mem_store();
        assert!(store.writable());
        assert_eq!(store.count_records().unwrap(), 0);
    }

    #[test]
    fn upsert_is_idempotent_on_content_hash() {
        let store = mem_store();
        store.ensure_workspace("personal", "ws").unwrap();
        let rec = sample_record("Use axum for HTTP");
        let first = store.upsert_record(&rec).unwrap();
        assert!(first.created());
        let second = store.upsert_record(&rec).unwrap();
        assert!(!second.created());
        assert_eq!(store.count_records().unwrap(), 1);
    }

    #[test]
    fn search_finds_by_content() {
        let store = mem_store();
        store.ensure_workspace("personal", "ws").unwrap();
        store
            .upsert_record(&sample_record("Use axum for the HTTP server"))
            .unwrap();
        store
            .upsert_record(&sample_record("Prefer rusqlite for storage"))
            .unwrap();
        let filters = RecordQuery {
            profile_id: Some("personal".to_string()),
            workspace_id: Some("ws".to_string()),
            limit: 10,
            ..Default::default()
        };
        let hits = store.search_records("axum", &filters).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("axum"));
    }

    #[test]
    fn archive_hides_from_default_query() {
        let store = mem_store();
        store.ensure_workspace("personal", "ws").unwrap();
        let outcome = store
            .upsert_record(&sample_record("ephemeral note"))
            .unwrap();
        let id = outcome.id().to_string();
        let (archived, not_found) = store
            .archive_records("personal", Some("ws"), std::slice::from_ref(&id))
            .unwrap();
        assert_eq!(archived, vec![id]);
        assert!(not_found.is_empty());
        let filters = RecordQuery {
            profile_id: Some("personal".to_string()),
            workspace_id: Some("ws".to_string()),
            limit: 10,
            ..Default::default()
        };
        let visible = store.query_records(&filters).unwrap();
        assert!(
            visible.is_empty(),
            "archived record must not appear by default"
        );
    }

    #[test]
    fn source_dedupe_returns_existing() {
        let store = mem_store();
        store.ensure_workspace("personal", "ws").unwrap();
        let hash = ids::source_hash("personal", "ws", "a.md", "content");
        let (s1, created1) = store
            .upsert_source(
                "personal",
                "ws",
                "ad_hoc_note",
                Some("a.md"),
                &hash,
                &Value::Null,
            )
            .unwrap();
        assert!(created1);
        let (s2, created2) = store
            .upsert_source(
                "personal",
                "ws",
                "ad_hoc_note",
                Some("a.md"),
                &hash,
                &Value::Null,
            )
            .unwrap();
        assert!(!created2);
        assert_eq!(s1.id, s2.id);
    }
}
