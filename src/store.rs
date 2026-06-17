//! Durable storage layer over SQLite (SPEC §3.1, §4). Owns the connection pool,
//! migrations, the FTS5 capability probe with LIKE fallback, and all CRUD /
//! search / dedupe operations.
//!
//! The store is intentionally free of policy logic: callers (ingest, server)
//! screen content via [`crate::policy`] before persisting.

use std::collections::BTreeSet;
use std::collections::VecDeque;
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
use crate::domain::Episode;
use crate::domain::MemoryRecord;
use crate::domain::MemorySource;
use crate::domain::Portability;
use crate::domain::Procedure;
use crate::domain::RecordType;
use crate::domain::Relation;
use crate::domain::RelationExpansion;
use crate::domain::Scope;
use crate::domain::Sensitivity;
use crate::domain::Subject;
use crate::domain::SubjectAlias;
use crate::domain::SubjectKind;
use crate::domain::TemporalState;
use crate::domain::VisibleTurn;
use crate::error::Error;
use crate::error::ErrorCode;
use crate::error::Result;
use crate::ids;

pub const STORAGE_SCHEMA_VERSION: i64 = 9;

const MIGRATION_INIT: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_FTS: &str = include_str!("../migrations/0002_fts.sql");
const MIGRATION_DREAM_RUNS: &str = include_str!("../migrations/0003_dream_runs.sql");
const MIGRATION_EVIDENCE_LEDGER: &str = include_str!("../migrations/0004_evidence_ledger.sql");
const MIGRATION_SUBJECTS_EPISODES: &str = include_str!("../migrations/0005_subjects_episodes.sql");
const MIGRATION_TRUST_QUARANTINE: &str = include_str!("../migrations/0006_trust_quarantine.sql");
const MIGRATION_PROCEDURES: &str = include_str!("../migrations/0007_procedures.sql");
const MIGRATION_PROCEDURE_LIFECYCLE: &str =
    include_str!("../migrations/0008_procedure_lifecycle.sql");
const MIGRATION_SEMANTIC_RELATIONS: &str =
    include_str!("../migrations/0009_semantic_relations.sql");
const MIGRATION_TEMPORAL_RECORDS: &str = include_str!("../migrations/0010_temporal_records.sql");

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

/// Counts of records omitted before recall ranking for deterministic storage reasons.
#[derive(Debug, Clone, Default)]
pub struct RecallOmissionCounts {
    pub archived: usize,
    pub secret_blocked: usize,
    pub quarantined: usize,
}

/// A new memory record to upsert. The store computes nothing here except
/// applying defaults; ids/hashes come from the caller.
#[derive(Debug, Clone)]
pub struct NewRecord {
    pub profile_id: String,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub subject_id: Option<String>,
    pub episode_id: Option<String>,
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
        ensure_memory_record_ref_columns(&conn)?;

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
        conn.execute_batch(MIGRATION_SUBJECTS_EPISODES)?;
        conn.execute_batch(MIGRATION_TRUST_QUARANTINE)?;
        ensure_trust_columns(&conn)?;
        conn.execute_batch(MIGRATION_PROCEDURES)?;
        conn.execute_batch(MIGRATION_PROCEDURE_LIFECYCLE)?;
        ensure_procedure_lifecycle_columns(&conn)?;
        conn.execute_batch(MIGRATION_SEMANTIC_RELATIONS)?;
        conn.execute_batch(MIGRATION_TEMPORAL_RECORDS)?;
        ensure_temporal_columns(&conn)?;

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

    /// Run SQLite's `PRAGMA integrity_check` and report whether the database is
    /// structurally sound. Used by the backup/restore workflow and `doctor`.
    pub fn integrity_ok(&self) -> Result<bool> {
        let conn = self.conn()?;
        let result: String = conn.query_row("PRAGMA integrity_check(1)", [], |r| r.get(0))?;
        Ok(result.eq_ignore_ascii_case("ok"))
    }

    /// Produce a self-contained backup of this database at `dest` using SQLite's
    /// online backup API. The backup checkpoints WAL content into the
    /// destination file, so the result is consistent without copying `-wal`
    /// sidecars. The source database is not mutated.
    pub fn online_backup_to(&self, dest: impl AsRef<Path>) -> Result<()> {
        let dest = dest.as_ref();
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Error::storage(format!("create backup dir {}: {e}", parent.display()))
                })?;
            }
        }
        let conn = self.conn()?;
        conn.backup(rusqlite::DatabaseName::Main, dest, None)
            .map_err(|e| Error::storage(format!("online backup to {}: {e}", dest.display())))?;
        Ok(())
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
    // Subjects & episodes
    // ------------------------------------------------------------------

    pub fn insert_or_get_subject(&self, subject: &Subject) -> Result<(Subject, bool)> {
        let conn = self.conn()?;
        let inserted = conn.execute(
            "INSERT INTO subjects(
                id, profile_id, workspace_id, subject_key, kind, display_name,
                created_at, updated_at, metadata
             )
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(profile_id, workspace_id, subject_key) DO NOTHING",
            params![
                subject.id,
                subject.profile_id,
                subject.workspace_id,
                subject.subject_key,
                subject.kind.as_str(),
                subject.display_name,
                subject.created_at,
                subject.updated_at,
                subject.metadata.to_string(),
            ],
        )?;
        if inserted == 1 {
            return Ok((subject.clone(), true));
        }

        let existing = conn
            .query_row(
                &format!(
                    "SELECT {SUBJECT_COLS} FROM subjects
                     WHERE profile_id = ?1 AND workspace_id = ?2 AND subject_key = ?3"
                ),
                params![
                    subject.profile_id,
                    subject.workspace_id,
                    subject.subject_key
                ],
                row_to_subject,
            )
            .optional()?
            .ok_or_else(|| {
                Error::storage(format!(
                    "subject conflict for {}/{}/{} but no existing row was visible",
                    subject.profile_id, subject.workspace_id, subject.subject_key
                ))
            })?;
        Ok((existing, false))
    }

    pub fn find_subject_by_key(
        &self,
        profile_id: &str,
        workspace_id: &str,
        subject_key: &str,
    ) -> Result<Option<Subject>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                &format!(
                    "SELECT {SUBJECT_COLS} FROM subjects
                     WHERE profile_id = ?1 AND workspace_id = ?2 AND subject_key = ?3"
                ),
                params![profile_id, workspace_id, subject_key],
                row_to_subject,
            )
            .optional()?;
        Ok(result)
    }

    pub fn get_subject(
        &self,
        profile_id: &str,
        workspace_id: &str,
        id: &str,
    ) -> Result<Option<Subject>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                &format!(
                    "SELECT {SUBJECT_COLS} FROM subjects
                     WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3"
                ),
                params![profile_id, workspace_id, id],
                row_to_subject,
            )
            .optional()?;
        Ok(result)
    }

    pub fn list_subjects(
        &self,
        profile_id: &str,
        workspace_id: &str,
        kind: Option<SubjectKind>,
    ) -> Result<Vec<Subject>> {
        let conn = self.conn()?;
        let mut sql = format!(
            "SELECT {SUBJECT_COLS} FROM subjects WHERE profile_id = ?1 AND workspace_id = ?2"
        );
        let rows = if let Some(kind) = kind {
            sql.push_str(" AND kind = ?3 ORDER BY updated_at DESC, subject_key ASC");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(
                    params![profile_id, workspace_id, kind.as_str()],
                    row_to_subject,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        } else {
            sql.push_str(" ORDER BY updated_at DESC, subject_key ASC");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![profile_id, workspace_id], row_to_subject)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        Ok(rows)
    }

    pub fn subject_exists_in_scope(
        &self,
        profile_id: &str,
        workspace_id: &str,
        id: &str,
    ) -> Result<bool> {
        let conn = self.conn()?;
        let exists = conn
            .prepare(
                "SELECT 1 FROM subjects WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
            )?
            .exists(params![profile_id, workspace_id, id])?;
        Ok(exists)
    }

    pub fn insert_episode(&self, episode: &Episode) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO episodes(
                id, profile_id, workspace_id, subject_id, source_kind, source_ref,
                started_at, ended_at, status, summary, trust_level, source_metadata,
                created_at, updated_at, metadata
             )
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                episode.id,
                episode.profile_id,
                episode.workspace_id,
                episode.subject_id,
                episode.source_kind,
                episode.source_ref,
                episode.started_at,
                episode.ended_at,
                episode.status,
                episode.summary,
                episode.trust_level,
                episode.source_metadata.to_string(),
                episode.created_at,
                episode.updated_at,
                episode.metadata.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn list_successful_episodes(
        &self,
        profile_id: &str,
        workspace_id: &str,
        subject_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Episode>> {
        let conn = self.conn()?;
        let limit = limit.clamp(1, 200) as i64;
        let mut stmt = conn.prepare(
            "SELECT id, profile_id, workspace_id, subject_id, source_kind, source_ref,
                    started_at, ended_at, status, summary, trust_level, source_metadata,
                    created_at, updated_at, metadata
             FROM episodes
             WHERE profile_id = ?1 AND workspace_id = ?2
               AND (?3 IS NULL OR subject_id = ?3)
               AND lower(coalesce(status, '')) IN ('success', 'succeeded', 'completed', 'passed')
             ORDER BY coalesce(ended_at, created_at) DESC, id DESC LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(
                params![profile_id, workspace_id, subject_id, limit],
                row_to_episode,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------------
    // Subject aliases & relations
    // ------------------------------------------------------------------

    /// Explicitly apply a subject alias. This is the write path for aliases:
    /// callers must review/decide before calling it; no recall path creates
    /// aliases silently.
    pub fn insert_or_get_subject_alias(
        &self,
        alias: &SubjectAlias,
    ) -> Result<(SubjectAlias, bool)> {
        if alias.alias_key.trim().is_empty() {
            return Err(Error::invalid_request("subject alias_key is required"));
        }
        if alias.source_evidence.trim().is_empty() {
            return Err(Error::invalid_request(
                "subject alias source_evidence is required",
            ));
        }
        if !self.subject_exists_in_scope(
            &alias.profile_id,
            &alias.workspace_id,
            &alias.subject_id,
        )? {
            return Err(Error::profile_boundary(
                "subject alias endpoint is outside the profile/workspace scope",
            ));
        }

        let conn = self.conn()?;
        let inserted = conn.execute(
            "INSERT INTO subject_aliases(
                id, profile_id, workspace_id, subject_id, alias_key,
                source_evidence, created_at, metadata
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(profile_id, workspace_id, alias_key) DO NOTHING",
            params![
                alias.id,
                alias.profile_id,
                alias.workspace_id,
                alias.subject_id,
                alias.alias_key,
                alias.source_evidence,
                alias.created_at,
                alias.metadata.to_string()
            ],
        )?;
        if inserted == 1 {
            return Ok((alias.clone(), true));
        }

        let existing = conn
            .query_row(
                &format!(
                    "SELECT {SUBJECT_ALIAS_COLS} FROM subject_aliases
                     WHERE profile_id = ?1 AND workspace_id = ?2 AND alias_key = ?3"
                ),
                params![alias.profile_id, alias.workspace_id, alias.alias_key],
                row_to_subject_alias,
            )
            .optional()?
            .ok_or_else(|| Error::storage("subject alias conflict without visible row"))?;
        if existing.subject_id != alias.subject_id {
            return Err(Error::profile_boundary(
                "subject alias already resolves to a different scoped subject",
            ));
        }
        Ok((existing, false))
    }

    pub fn resolve_subject_alias(
        &self,
        profile_id: &str,
        workspace_id: &str,
        alias_key: &str,
    ) -> Result<Option<Subject>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                &format!(
                    "SELECT {SUBJECT_COLS_S}
                     FROM subject_aliases a
                     JOIN subjects s
                       ON s.id = a.subject_id
                      AND s.profile_id = a.profile_id
                      AND s.workspace_id = a.workspace_id
                     WHERE a.profile_id = ?1 AND a.workspace_id = ?2
                       AND a.alias_key = ?3"
                ),
                params![profile_id, workspace_id, alias_key],
                row_to_subject,
            )
            .optional()?;
        Ok(result)
    }

    pub fn get_subject_alias(
        &self,
        profile_id: &str,
        workspace_id: &str,
        alias_key: &str,
    ) -> Result<Option<SubjectAlias>> {
        let conn = self.conn()?;
        let alias = conn
            .query_row(
                &format!(
                    "SELECT {SUBJECT_ALIAS_COLS}
                     FROM subject_aliases
                     WHERE profile_id = ?1 AND workspace_id = ?2 AND alias_key = ?3"
                ),
                params![profile_id, workspace_id, alias_key],
                row_to_subject_alias,
            )
            .optional()?;
        Ok(alias)
    }

    /// Explicitly apply an evidence-backed relation. Scope and evidence are
    /// checked before persistence; relation-aware recall only reads active rows.
    pub fn insert_or_get_relation(&self, relation: &Relation) -> Result<(Relation, bool)> {
        validate_relation(relation)?;
        if !self.subject_exists_in_scope(
            &relation.profile_id,
            &relation.workspace_id,
            &relation.from_subject_id,
        )? || !self.subject_exists_in_scope(
            &relation.profile_id,
            &relation.workspace_id,
            &relation.to_subject_id,
        )? {
            return Err(Error::profile_boundary(
                "relation endpoints must both be inside the same profile/workspace scope",
            ));
        }

        let conn = self.conn()?;
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO relations(
                id, profile_id, workspace_id, from_subject_id, relation_type,
                to_subject_id, confidence, state, source_episode_ids,
                source_evidence, created_at, retired_at, metadata
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                relation.id,
                relation.profile_id,
                relation.workspace_id,
                relation.from_subject_id,
                relation.relation_type,
                relation.to_subject_id,
                relation.confidence,
                relation.state,
                serde_json::to_string(&relation.source_episode_ids)?,
                relation.source_evidence,
                relation.created_at,
                relation.retired_at,
                relation.metadata.to_string()
            ],
        )?;
        if inserted == 1 {
            return Ok((relation.clone(), true));
        }

        let existing = conn
            .query_row(
                &format!(
                    "SELECT {RELATION_COLS} FROM relations
                     WHERE profile_id = ?1 AND workspace_id = ?2
                       AND from_subject_id = ?3 AND relation_type = ?4
                       AND to_subject_id = ?5
                       AND retired_at IS NULL AND state != 'retired'"
                ),
                params![
                    relation.profile_id,
                    relation.workspace_id,
                    relation.from_subject_id,
                    relation.relation_type,
                    relation.to_subject_id
                ],
                row_to_relation,
            )
            .optional()?
            .ok_or_else(|| Error::storage("relation conflict without visible row"))?;
        Ok((existing, false))
    }

    pub fn get_active_relation(
        &self,
        profile_id: &str,
        workspace_id: &str,
        from_subject_id: &str,
        relation_type: &str,
        to_subject_id: &str,
    ) -> Result<Option<Relation>> {
        let conn = self.conn()?;
        let relation = conn
            .query_row(
                &format!(
                    "SELECT {RELATION_COLS} FROM relations
                     WHERE profile_id = ?1 AND workspace_id = ?2
                       AND from_subject_id = ?3 AND relation_type = ?4
                       AND to_subject_id = ?5
                       AND retired_at IS NULL AND state != 'retired'"
                ),
                params![
                    profile_id,
                    workspace_id,
                    from_subject_id,
                    relation_type,
                    to_subject_id
                ],
                row_to_relation,
            )
            .optional()?;
        Ok(relation)
    }

    /// Traverse active outgoing relations within one profile/workspace. The
    /// target subject join is scope-filtered, so a malformed cross-scope row
    /// still cannot expand across the boundary.
    pub fn relation_expanded_subjects(
        &self,
        profile_id: &str,
        workspace_id: &str,
        seed_subject_ids: &[String],
        max_depth: usize,
    ) -> Result<Vec<RelationExpansion>> {
        let max_depth = max_depth.clamp(1, 3);
        let conn = self.conn()?;
        let mut seen = seed_subject_ids.iter().cloned().collect::<BTreeSet<_>>();
        let mut queue = VecDeque::new();
        for subject_id in seed_subject_ids {
            queue.push_back((
                subject_id.clone(),
                0usize,
                Vec::<String>::new(),
                Vec::<String>::new(),
            ));
        }

        let mut expansions = Vec::new();
        while let Some((subject_id, depth, via, evidence)) = queue.pop_front() {
            if depth >= max_depth || expansions.len() >= 64 {
                continue;
            }
            let mut stmt = conn.prepare(&format!(
                "SELECT {RELATION_COLS_R}
                     FROM relations r
                     JOIN subjects target
                       ON target.id = r.to_subject_id
                      AND target.profile_id = r.profile_id
                      AND target.workspace_id = r.workspace_id
                     WHERE r.profile_id = ?1 AND r.workspace_id = ?2
                       AND r.from_subject_id = ?3
                       AND r.state = 'active' AND r.retired_at IS NULL
                     ORDER BY r.confidence DESC, r.created_at DESC, r.id ASC"
            ))?;
            let rows = stmt
                .query_map(
                    params![profile_id, workspace_id, subject_id],
                    row_to_relation,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for relation in rows {
                if !seen.insert(relation.to_subject_id.clone()) {
                    continue;
                }
                let mut via_relation_ids = via.clone();
                via_relation_ids.push(relation.id.clone());
                let mut evidence_refs = evidence.clone();
                evidence_refs.extend(relation.source_episode_ids.clone());
                if let Some(source_evidence) = &relation.source_evidence {
                    evidence_refs.push(source_evidence.clone());
                }
                evidence_refs = dedupe_strings(evidence_refs);
                expansions.push(RelationExpansion {
                    subject_id: relation.to_subject_id.clone(),
                    depth: depth + 1,
                    via_relation_ids: via_relation_ids.clone(),
                    evidence_refs: evidence_refs.clone(),
                });
                queue.push_back((
                    relation.to_subject_id,
                    depth + 1,
                    via_relation_ids,
                    evidence_refs,
                ));
            }
        }
        Ok(expansions)
    }

    pub fn records_for_subjects(
        &self,
        profile_id: &str,
        workspace_id: &str,
        subject_ids: &[String],
        limit: usize,
    ) -> Result<Vec<MemoryRecord>> {
        if subject_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn()?;
        let mut seen = BTreeSet::new();
        let mut rows = Vec::new();
        for subject_id in subject_ids {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RECORD_COLS} FROM memory_records
                 WHERE profile_id = ?1 AND workspace_id = ?2
                   AND subject_id = ?3
                   AND archived = 0
                   AND sensitivity != 'secret_blocked'
                   AND trust_state != 'quarantined'
                 ORDER BY updated_at DESC, confidence DESC, id ASC"
            ))?;
            for row in
                stmt.query_map(params![profile_id, workspace_id, subject_id], row_to_record)?
            {
                let record = row?;
                if seen.insert(record.id.clone()) {
                    rows.push(record);
                }
            }
        }
        rows.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| {
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.id.cmp(&b.id))
        });
        rows.truncate(limit);
        Ok(rows)
    }

    pub fn insert_or_get_procedure(&self, procedure: &Procedure) -> Result<(Procedure, bool)> {
        let conn = self.conn()?;
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO procedures(
                id, profile_id, workspace_id, subject_id, repo_id, name, activation_query,
                steps, guardrails, termination_condition, source_episode_ids, confidence,
                state, created_at, retired_at, version, first_seen, last_validated,
                superseded_by, counter_evidence_count, negative_examples, metadata
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                procedure.id,
                procedure.profile_id,
                procedure.workspace_id,
                procedure.subject_id,
                procedure.repo_id,
                procedure.name,
                procedure.activation_query,
                procedure.steps,
                procedure.guardrails,
                procedure.termination_condition,
                serde_json::to_string(&procedure.source_episode_ids)
                    .unwrap_or_else(|_| "[]".to_string()),
                procedure.confidence,
                procedure.state,
                procedure.created_at,
                procedure.retired_at,
                procedure.version,
                procedure.first_seen,
                procedure.last_validated,
                procedure.superseded_by,
                procedure.counter_evidence_count,
                serde_json::to_string(&procedure.negative_examples)
                    .unwrap_or_else(|_| "[]".to_string()),
                serde_json::to_string(&procedure.metadata).unwrap_or_else(|_| "{}".to_string())
            ],
        )?;
        if inserted == 1 {
            Ok((procedure.clone(), true))
        } else {
            let existing = conn
                .query_row(
                    &format!(
                        "SELECT {PROCEDURE_COLS}
                     FROM procedures
                     WHERE profile_id = ?1 AND workspace_id = ?2
                       AND ((subject_id IS NULL AND ?3 IS NULL) OR subject_id = ?3)
                       AND activation_query = ?4 AND steps = ?5 AND retired_at IS NULL"
                    ),
                    params![
                        procedure.profile_id,
                        procedure.workspace_id,
                        procedure.subject_id,
                        procedure.activation_query,
                        procedure.steps
                    ],
                    row_to_procedure,
                )
                .optional()?
                .ok_or_else(|| Error::storage("procedure dedupe lookup missed existing row"))?;
            Ok((existing, false))
        }
    }

    pub fn query_procedures(
        &self,
        profile_id: &str,
        workspace_id: &str,
        subject_id: Option<&str>,
        query: Option<&str>,
        include_retired: bool,
        limit: usize,
    ) -> Result<Vec<Procedure>> {
        let conn = self.conn()?;
        let limit = limit.clamp(1, 100) as i64;
        let pattern = query
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| format!("%{q}%"));
        let include_retired = if include_retired { 1 } else { 0 };
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROCEDURE_COLS}
             FROM procedures
             WHERE profile_id = ?1 AND workspace_id = ?2
               AND (?3 IS NULL OR subject_id = ?3)
               AND (?4 = 1 OR (state NOT IN ('retired','superseded','quarantined')
                               AND retired_at IS NULL))
               AND (?5 IS NULL OR lower(name) LIKE lower(?5)
                    OR lower(activation_query) LIKE lower(?5)
                    OR lower(steps) LIKE lower(?5))
             ORDER BY (state = 'active') DESC, confidence DESC,
                      last_validated DESC, created_at DESC, id DESC LIMIT ?6"
        ))?;
        let rows = stmt
            .query_map(
                params![
                    profile_id,
                    workspace_id,
                    subject_id,
                    include_retired,
                    pattern,
                    limit
                ],
                row_to_procedure,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Fetch a single procedure by id within scope.
    pub fn get_procedure(
        &self,
        profile_id: &str,
        workspace_id: &str,
        id: &str,
    ) -> Result<Option<Procedure>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                &format!(
                    "SELECT {PROCEDURE_COLS} FROM procedures
                     WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3"
                ),
                params![profile_id, workspace_id, id],
                row_to_procedure,
            )
            .optional()?;
        Ok(result)
    }

    /// Transition a procedure to the `retired` state (issue #146). Historical
    /// procedures remain inspectable but drop out of default recall. Returns the
    /// updated procedure, or `None` if it is not found in scope.
    pub fn retire_procedure(
        &self,
        profile_id: &str,
        workspace_id: &str,
        id: &str,
        now: &str,
    ) -> Result<Option<Procedure>> {
        {
            let conn = self.conn()?;
            conn.execute(
                "UPDATE procedures
                    SET state = 'retired', retired_at = ?4
                  WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3
                    AND state != 'retired'",
                params![profile_id, workspace_id, id, now],
            )?;
        }
        // Connection is released above before the follow-up read (the in-memory
        // pool has a single connection).
        self.get_procedure(profile_id, workspace_id, id)
    }

    /// Supersede `old_id` with `new_id` (issue #146). The old procedure is
    /// marked `superseded`, links to the successor, and the successor's version
    /// is bumped past the old one. Both must already exist in scope.
    pub fn supersede_procedure(
        &self,
        profile_id: &str,
        workspace_id: &str,
        old_id: &str,
        new_id: &str,
        now: &str,
    ) -> Result<Option<Procedure>> {
        {
            let mut conn = self.conn()?;
            let tx = conn.transaction()?;
            let old_version: Option<i64> = tx
                .query_row(
                    "SELECT version FROM procedures
                     WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
                    params![profile_id, workspace_id, old_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(old_version) = old_version else {
                return Ok(None);
            };
            tx.execute(
                "UPDATE procedures
                    SET state = 'superseded', retired_at = ?4, superseded_by = ?5
                  WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
                params![profile_id, workspace_id, old_id, now, new_id],
            )?;
            tx.execute(
                "UPDATE procedures
                    SET version = ?4, last_validated = ?5
                  WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
                params![profile_id, workspace_id, new_id, old_version + 1, now],
            )?;
            tx.commit()?;
        }
        self.get_procedure(profile_id, workspace_id, new_id)
    }

    /// Record counter-evidence (failed reuse / contradiction) against a
    /// procedure (issue #146). Increments the counter; once it reaches
    /// `quarantine_threshold`, the procedure is quarantined out of recall.
    /// Returns the updated procedure.
    pub fn record_procedure_counter_evidence(
        &self,
        profile_id: &str,
        workspace_id: &str,
        id: &str,
        quarantine_threshold: i64,
        now: &str,
    ) -> Result<Option<Procedure>> {
        {
            let conn = self.conn()?;
            let changed = conn.execute(
                "UPDATE procedures
                    SET counter_evidence_count = counter_evidence_count + 1
                  WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
                params![profile_id, workspace_id, id],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            conn.execute(
                "UPDATE procedures
                    SET state = 'quarantined', retired_at = ?4
                  WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3
                    AND counter_evidence_count >= ?5
                    AND state NOT IN ('retired','superseded')",
                params![profile_id, workspace_id, id, now, quarantine_threshold],
            )?;
        }
        self.get_procedure(profile_id, workspace_id, id)
    }

    /// Mark a procedure validated now (successful reuse / eval pass).
    pub fn validate_procedure(
        &self,
        profile_id: &str,
        workspace_id: &str,
        id: &str,
        now: &str,
    ) -> Result<Option<Procedure>> {
        {
            let conn = self.conn()?;
            conn.execute(
                "UPDATE procedures SET last_validated = ?4
                  WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
                params![profile_id, workspace_id, id, now],
            )?;
        }
        self.get_procedure(profile_id, workspace_id, id)
    }

    pub fn find_episode_by_source(
        &self,
        profile_id: &str,
        workspace_id: &str,
        source_kind: &str,
        source_ref: &str,
    ) -> Result<Option<Episode>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                &format!(
                    "SELECT {EPISODE_COLS} FROM episodes
                     WHERE profile_id = ?1 AND workspace_id = ?2
                       AND source_kind = ?3 AND source_ref = ?4"
                ),
                params![profile_id, workspace_id, source_kind, source_ref],
                row_to_episode,
            )
            .optional()?;
        Ok(result)
    }

    pub fn get_episode(
        &self,
        profile_id: &str,
        workspace_id: &str,
        id: &str,
    ) -> Result<Option<Episode>> {
        let conn = self.conn()?;
        let result = conn
            .query_row(
                &format!(
                    "SELECT {EPISODE_COLS} FROM episodes
                     WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3"
                ),
                params![profile_id, workspace_id, id],
                row_to_episode,
            )
            .optional()?;
        Ok(result)
    }

    pub fn list_episodes(
        &self,
        profile_id: &str,
        workspace_id: &str,
        subject_id: Option<&str>,
    ) -> Result<Vec<Episode>> {
        let conn = self.conn()?;
        let mut sql = format!(
            "SELECT {EPISODE_COLS} FROM episodes WHERE profile_id = ?1 AND workspace_id = ?2"
        );
        let rows = if let Some(subject_id) = subject_id {
            sql.push_str(" AND subject_id = ?3 ORDER BY created_at DESC, id ASC");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(
                    params![profile_id, workspace_id, subject_id],
                    row_to_episode,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        } else {
            sql.push_str(" ORDER BY created_at DESC, id ASC");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![profile_id, workspace_id], row_to_episode)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        Ok(rows)
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

        let id = ids::new_id("mem");
        let now = ids::now_rfc3339();
        let (trust_state, trust_score, quarantine_reason, quarantined_at) =
            trust_defaults_for_new_record(new, &now);
        let (
            valid_from,
            valid_until,
            observed_at,
            invalidated_at,
            superseded_by,
            historical_reason,
            temporal_state,
        ) = temporal_defaults_for_new_record(new, &now);
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO memory_records(
                id, profile_id, workspace_id, repo_id, subject_id, episode_id,
                scope, type, content, related_files, tags, sensitivity,
                portability, confidence, source_ids, content_hash, supersedes,
                created_at, updated_at, last_used_at, archived, trust_state, trust_score,
                quarantine_reason, quarantined_at, promoted_at, valid_from, valid_until,
                observed_at, invalidated_at, superseded_by, historical_reason, temporal_state,
                metadata)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?18,NULL,0,?19,?20,?21,?22,NULL,?23,?24,?25,?26,?27,?28,?29,?30)",
            params![
                id,
                new.profile_id,
                new.workspace_id,
                new.repo_id,
                new.subject_id,
                new.episode_id,
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
                trust_state,
                trust_score,
                quarantine_reason,
                quarantined_at,
                valid_from,
                valid_until,
                observed_at,
                invalidated_at,
                superseded_by,
                historical_reason,
                temporal_state,
                new.metadata.to_string(),
            ],
        )?;
        Ok(UpsertOutcome::Created(id))
    }

    /// Mark records as quarantined. Quarantined records stay durable and
    /// inspectable by id, but default query/search/export paths withhold them.
    pub fn quarantine_records(
        &self,
        profile_id: &str,
        workspace_id: Option<&str>,
        ids_to_quarantine: &[String],
        reason: &str,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let now = ids::now_rfc3339();
        let mut quarantined = Vec::new();
        let mut not_found = Vec::new();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE memory_records
                 SET trust_state = 'quarantined', trust_score = 0.0,
                     quarantine_reason = ?1, quarantined_at = ?2,
                     promoted_at = NULL, updated_at = ?2
                 WHERE id = ?3 AND trust_state != 'quarantined'",
            )?;
            for id in ids_to_quarantine {
                if !self.scoped_record_exists(&tx, id, profile_id, workspace_id)? {
                    not_found.push(id.clone());
                    continue;
                }
                if stmt.execute(params![reason, now, id])? > 0 {
                    quarantined.push(id.clone());
                }
            }
        }
        tx.commit()?;
        Ok((quarantined, not_found))
    }

    /// Explicitly promote quarantined records back into default recall/export.
    pub fn promote_quarantined_records(
        &self,
        profile_id: &str,
        workspace_id: Option<&str>,
        ids_to_promote: &[String],
    ) -> Result<(Vec<String>, Vec<String>)> {
        let now = ids::now_rfc3339();
        let mut promoted = Vec::new();
        let mut not_found = Vec::new();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE memory_records
                 SET trust_state = 'trusted', trust_score = 1.0,
                     quarantine_reason = NULL, promoted_at = ?1, updated_at = ?1
                 WHERE id = ?2 AND trust_state = 'quarantined'",
            )?;
            for id in ids_to_promote {
                if !self.scoped_record_exists(&tx, id, profile_id, workspace_id)? {
                    not_found.push(id.clone());
                    continue;
                }
                if stmt.execute(params![now, id])? > 0 {
                    promoted.push(id.clone());
                } else {
                    not_found.push(id.clone());
                }
            }
        }
        tx.commit()?;
        Ok((promoted, not_found))
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
        archived_by_patch_run_id: Option<&str>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let now = ids::now_rfc3339();
        self.archive_records_with_metadata_at(
            profile_id,
            workspace_id,
            ids_to_archive,
            state,
            historical_reason,
            archived_by_patch_run_id,
            &now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn archive_records_with_metadata_at(
        &self,
        profile_id: &str,
        workspace_id: Option<&str>,
        ids_to_archive: &[String],
        state: &str,
        historical_reason: &str,
        archived_by_patch_run_id: Option<&str>,
        now: &str,
    ) -> Result<(Vec<String>, Vec<String>)> {
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
                        "policy_outcome".to_string(),
                        Value::String(state.to_string()),
                    );
                    obj.insert(
                        "historical_reason".to_string(),
                        Value::String(historical_reason.to_string()),
                    );
                    obj.insert("archived_at".to_string(), Value::String(now.to_string()));
                    if state == "counter_evidence" {
                        if let Some(marker) = obj.get_mut("marker").and_then(Value::as_object_mut) {
                            marker.insert("retired_at".to_string(), Value::String(now.to_string()));
                            marker.insert(
                                "retirement_reason".to_string(),
                                Value::String("counter_evidence".to_string()),
                            );
                            marker.insert(
                                "retirement_historical_reason".to_string(),
                                Value::String(historical_reason.to_string()),
                            );
                        }
                    }
                    if let Some(run_id) = archived_by_patch_run_id {
                        obj.insert(
                            "archived_by_patch_run_id".to_string(),
                            Value::String(run_id.to_string()),
                        );
                    }
                }
                if update.execute(params![now, metadata.to_string(), id])? > 0 {
                    archived.push(id.clone());
                }
            }
        }
        tx.commit()?;
        Ok((archived, not_found))
    }

    pub fn restore_records_with_metadata(
        &self,
        profile_id: &str,
        workspace_id: Option<&str>,
        ids_to_restore: &[String],
        patch_run_id: &str,
        reason: &str,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let now = ids::now_rfc3339();
        let mut restored = Vec::new();
        let mut not_found = Vec::new();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut select =
                tx.prepare("SELECT metadata, archived FROM memory_records WHERE id = ?1")?;
            let mut update = tx.prepare(
                "UPDATE memory_records SET archived = 0, updated_at = ?1, metadata = ?2 WHERE id = ?3 AND archived = 1",
            )?;
            for id in ids_to_restore {
                if !self.scoped_record_exists(&tx, id, profile_id, workspace_id)? {
                    not_found.push(id.clone());
                    continue;
                }
                let (raw, archived_flag): (String, i64) = select
                    .query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(|e| Error::storage(format!("load metadata for restore {id}: {e}")))?;
                if archived_flag == 0 {
                    not_found.push(id.clone());
                    continue;
                }
                let mut metadata = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
                if !metadata.is_object() {
                    metadata = serde_json::json!({});
                }
                let allow_restore = metadata
                    .get("archived_by_patch_run_id")
                    .and_then(|v| v.as_str())
                    .map(|value| value == patch_run_id)
                    .unwrap_or(false);
                if !allow_restore {
                    not_found.push(id.clone());
                    continue;
                }
                if let Some(obj) = metadata.as_object_mut() {
                    obj.insert("state".to_string(), Value::String("restored".to_string()));
                    obj.insert(
                        "policy_outcome".to_string(),
                        Value::String("restored".to_string()),
                    );
                    obj.insert("restored_at".to_string(), Value::String(now.clone()));
                    obj.insert(
                        "restored_by_patch_run_id".to_string(),
                        Value::String(patch_run_id.to_string()),
                    );
                    obj.insert(
                        "restored_reason".to_string(),
                        Value::String(reason.to_string()),
                    );
                }
                if update.execute(params![now, metadata.to_string(), id])? > 0 {
                    restored.push(id.clone());
                }
            }
        }
        tx.commit()?;
        Ok((restored, not_found))
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

    pub fn supersede_record(
        &self,
        profile_id: &str,
        workspace_id: &str,
        old_id: &str,
        new_id: &str,
        reason: &str,
        now: &str,
    ) -> Result<Option<MemoryRecord>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        if !self.scoped_record_exists(&tx, old_id, profile_id, Some(workspace_id))?
            || !self.scoped_record_exists(&tx, new_id, profile_id, Some(workspace_id))?
        {
            tx.commit()?;
            return Ok(None);
        }

        tx.execute(
            "UPDATE memory_records
             SET temporal_state = 'superseded',
                 superseded_by = ?4,
                 valid_until = COALESCE(valid_until, ?5),
                 historical_reason = ?6,
                 updated_at = ?5
             WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
            params![profile_id, workspace_id, old_id, new_id, now, reason],
        )?;

        let raw: String = tx.query_row(
            "SELECT supersedes FROM memory_records
             WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
            params![profile_id, workspace_id, new_id],
            |row| row.get(0),
        )?;
        let mut supersedes = json_str_list(&raw);
        if !supersedes.iter().any(|id| id == old_id) {
            supersedes.push(old_id.to_string());
            tx.execute(
                "UPDATE memory_records
                 SET supersedes = ?4, updated_at = ?5
                 WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
                params![
                    profile_id,
                    workspace_id,
                    new_id,
                    serde_json::to_string(&supersedes)?,
                    now,
                ],
            )?;
        }

        tx.commit()?;
        drop(conn);
        self.get_record(old_id)
    }

    pub fn invalidate_record(
        &self,
        profile_id: &str,
        workspace_id: &str,
        id: &str,
        reason: &str,
        now: &str,
    ) -> Result<Option<MemoryRecord>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        if !self.scoped_record_exists(&tx, id, profile_id, Some(workspace_id))? {
            tx.commit()?;
            return Ok(None);
        }
        tx.execute(
            "UPDATE memory_records
             SET temporal_state = 'invalidated',
                 invalidated_at = ?4,
                 valid_until = COALESCE(valid_until, ?4),
                 historical_reason = ?5,
                 updated_at = ?4
             WHERE profile_id = ?1 AND workspace_id = ?2 AND id = ?3",
            params![profile_id, workspace_id, id, now, reason],
        )?;
        tx.commit()?;
        drop(conn);
        self.get_record(id)
    }

    pub fn records_by_patch_run_id(
        &self,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
        patch_run_id: &str,
        include_archived: bool,
    ) -> Result<Vec<MemoryRecord>> {
        let conn = self.conn()?;
        let mut sql = format!(
            "SELECT {RECORD_COLS} FROM memory_records WHERE profile_id = ?1 AND workspace_id = ?2"
        );
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(profile_id.to_string()),
            Box::new(workspace_id.to_string()),
        ];
        if let Some(repo_id) = repo_id {
            sql.push_str(" AND repo_id = ?");
            args.push(Box::new(repo_id.to_string()));
        }
        if !include_archived {
            sql.push_str(" AND archived = 0");
        }
        sql.push_str(" AND json_extract(metadata, '$.patch_run_id') = ?");
        args.push(Box::new(patch_run_id.to_string()));
        sql.push_str(" ORDER BY updated_at DESC, id ASC");
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_record)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn archived_records_by_patch_run_id(
        &self,
        profile_id: &str,
        workspace_id: &str,
        repo_id: Option<&str>,
        patch_run_id: &str,
    ) -> Result<Vec<MemoryRecord>> {
        let conn = self.conn()?;
        let mut sql = format!(
            "SELECT {RECORD_COLS} FROM memory_records WHERE profile_id = ?1 AND workspace_id = ?2 AND archived = 1"
        );
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(profile_id.to_string()),
            Box::new(workspace_id.to_string()),
        ];
        if let Some(repo_id) = repo_id {
            sql.push_str(" AND repo_id = ?");
            args.push(Box::new(repo_id.to_string()));
        }
        sql.push_str(" AND json_extract(metadata, '$.archived_by_patch_run_id') = ?");
        args.push(Box::new(patch_run_id.to_string()));
        sql.push_str(" ORDER BY updated_at DESC, id ASC");
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_record)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
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
        sql.push_str(" AND trust_state != 'quarantined'");
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

    pub fn recall_omission_counts(
        &self,
        query: &RecordQuery,
        query_text: &str,
    ) -> Result<RecallOmissionCounts> {
        let conn = self.conn()?;
        let archived = count_recall_omission(&conn, query, query_text, "archived")?;
        let secret_blocked = count_recall_omission(&conn, query, query_text, "secret_blocked")?;
        let quarantined = count_recall_omission(&conn, query, query_text, "quarantined")?;
        Ok(RecallOmissionCounts {
            archived,
            secret_blocked,
            quarantined,
        })
    }

    /// Export rows and the matching `secret_blocked` count from one read
    /// transaction so the omitted count matches the exported snapshot.
    pub fn export_records(&self, query: &RecordQuery) -> Result<(Vec<MemoryRecord>, usize, usize)> {
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

        let omitted_quarantined = {
            let mut sql = "SELECT COUNT(*) FROM memory_records WHERE 1=1".to_string();
            let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            append_export_scope_filters(&mut sql, &mut args, query);
            sql.push_str(" AND sensitivity != 'secret_blocked' AND trust_state = 'quarantined'");

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
            sql.push_str(" AND trust_state != 'quarantined'");
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
        Ok((rows, omitted_secret, omitted_quarantined))
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
                policy_state, created_at, trust_state, trust_score, metadata
             )
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'trusted',1.0,?14)",
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

    /// Count records currently in the quarantine state across all profiles.
    /// Content-free aggregate for diagnostics.
    pub fn count_quarantined_records(&self) -> Result<i64> {
        let conn = self.conn()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_records WHERE trust_state = 'quarantined'",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Count procedures grouped by lifecycle state (active, retired, …).
    /// Returns pairs sorted by state for stable diagnostics output.
    pub fn count_procedures_by_state(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT state, COUNT(*) FROM procedures GROUP BY state ORDER BY state")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count rows in a table; returns 0 if the table does not exist (e.g. an
    /// older schema generation before that table was introduced). Used by the
    /// schema report and the upgrade-safety matrix.
    pub fn count_table_rows(&self, table: &str) -> Result<i64> {
        if !is_safe_identifier(table) {
            return Err(Error::invalid_request(format!(
                "invalid table identifier '{table}'"
            )));
        }
        let conn = self.conn()?;
        let exists: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type IN ('table','view') AND name = ?1")?
            .exists(params![table])?;
        if !exists {
            return Ok(0);
        }
        let n: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
        Ok(n)
    }

    /// Machine-readable schema/storage report: the recorded schema version, the
    /// compiled `STORAGE_SCHEMA_VERSION`, whether they match, and row counts for
    /// the durable tables. Diagnostics never include stored content — only
    /// counts and structural facts — so this is safe to print and snapshot.
    pub fn schema_report(&self) -> Result<SchemaReport> {
        let conn = self.conn()?;
        let recorded: Option<String> = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let recorded_version = recorded.as_deref().and_then(|v| v.parse::<i64>().ok());
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

        // Count inline on this connection. Do NOT call count_table_rows() here:
        // it would check out a second connection and deadlock the single-
        // connection in-memory pool.
        let mut tables = Vec::new();
        for table in DURABLE_TABLES {
            let exists: bool = conn
                .prepare(
                    "SELECT 1 FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
                )?
                .exists(params![table])?;
            let rows = if exists {
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?
            } else {
                0
            };
            tables.push(TableCount { table, rows });
        }

        Ok(SchemaReport {
            recorded_version,
            user_version,
            expected_version: STORAGE_SCHEMA_VERSION,
            up_to_date: recorded_version == Some(STORAGE_SCHEMA_VERSION),
            fts_enabled: self.fts_enabled,
            tables,
        })
    }
}

/// The durable tables tracked by the schema report and upgrade-safety matrix.
/// Order is stable so the report is snapshot-friendly.
pub const DURABLE_TABLES: &[&str] = &[
    "memory_records",
    "conclusions",
    "checkpoints",
    "subjects",
    "episodes",
    "subject_aliases",
    "relations",
    "evidence_ledger",
    "procedures",
    "memory_sources",
    "dream_runs",
    "policy_events",
];

/// A table name and its row count.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TableCount {
    pub table: &'static str,
    pub rows: i64,
}

/// Structured schema/storage diagnostics. Content-free: counts and structural
/// facts only, safe to print and snapshot.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SchemaReport {
    /// Version recorded in `schema_meta` (None on a pre-meta-table DB).
    pub recorded_version: Option<i64>,
    /// SQLite `PRAGMA user_version`.
    pub user_version: i64,
    /// The version this binary writes (`STORAGE_SCHEMA_VERSION`).
    pub expected_version: i64,
    /// True when the recorded version matches the expected version.
    pub up_to_date: bool,
    /// Whether FTS5 search is active (vs. the LIKE fallback).
    pub fts_enabled: bool,
    /// Row counts for the durable tables.
    pub tables: Vec<TableCount>,
}

/// Conservative SQLite identifier guard for the few places we interpolate a
/// table name into SQL (no bound-parameter form exists for identifiers).
fn is_safe_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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

fn ensure_memory_record_ref_columns(conn: &rusqlite::Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(memory_records)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !columns.iter().any(|name| name == "subject_id") {
        conn.execute("ALTER TABLE memory_records ADD COLUMN subject_id TEXT", [])?;
    }
    if !columns.iter().any(|name| name == "episode_id") {
        conn.execute("ALTER TABLE memory_records ADD COLUMN episode_id TEXT", [])?;
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memory_records_subject
         ON memory_records(subject_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memory_records_episode
         ON memory_records(episode_id)",
        [],
    )?;
    Ok(())
}

fn ensure_trust_columns(conn: &rusqlite::Connection) -> Result<()> {
    ensure_column(
        conn,
        "memory_records",
        "trust_state",
        "TEXT NOT NULL DEFAULT 'trusted'",
    )?;
    ensure_column(
        conn,
        "memory_records",
        "trust_score",
        "REAL NOT NULL DEFAULT 1.0",
    )?;
    ensure_column(conn, "memory_records", "quarantine_reason", "TEXT")?;
    ensure_column(conn, "memory_records", "quarantined_at", "TEXT")?;
    ensure_column(conn, "memory_records", "promoted_at", "TEXT")?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memory_records_trust_state
         ON memory_records(trust_state)",
        [],
    )?;

    ensure_column(
        conn,
        "evidence_ledger",
        "trust_state",
        "TEXT NOT NULL DEFAULT 'trusted'",
    )?;
    ensure_column(
        conn,
        "evidence_ledger",
        "trust_score",
        "REAL NOT NULL DEFAULT 1.0",
    )?;
    ensure_column(conn, "evidence_ledger", "quarantine_reason", "TEXT")?;
    ensure_column(conn, "evidence_ledger", "quarantined_at", "TEXT")?;
    ensure_column(conn, "evidence_ledger", "promoted_at", "TEXT")?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_evidence_ledger_trust_state
         ON evidence_ledger(trust_state)",
        [],
    )?;
    Ok(())
}

/// Back-fill procedure lifecycle columns (issue #146): versioning, validation
/// timestamps, supersession links, counter-evidence counters, and negative
/// activation examples. Idempotent; runs on every open.
fn ensure_procedure_lifecycle_columns(conn: &rusqlite::Connection) -> Result<()> {
    ensure_column(conn, "procedures", "version", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_column(conn, "procedures", "first_seen", "TEXT")?;
    ensure_column(conn, "procedures", "last_validated", "TEXT")?;
    ensure_column(conn, "procedures", "superseded_by", "TEXT")?;
    ensure_column(
        conn,
        "procedures",
        "counter_evidence_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "procedures",
        "negative_examples",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_procedures_superseded_by
         ON procedures(superseded_by)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_procedures_last_validated
         ON procedures(profile_id, workspace_id, last_validated)",
        [],
    )?;
    Ok(())
}

fn ensure_temporal_columns(conn: &rusqlite::Connection) -> Result<()> {
    ensure_column(conn, "memory_records", "valid_from", "TEXT")?;
    ensure_column(conn, "memory_records", "valid_until", "TEXT")?;
    ensure_column(conn, "memory_records", "observed_at", "TEXT")?;
    ensure_column(conn, "memory_records", "invalidated_at", "TEXT")?;
    ensure_column(conn, "memory_records", "superseded_by", "TEXT")?;
    ensure_column(conn, "memory_records", "historical_reason", "TEXT")?;
    ensure_column(
        conn,
        "memory_records",
        "temporal_state",
        "TEXT NOT NULL DEFAULT 'current'",
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memory_records_temporal_state
         ON memory_records(profile_id, workspace_id, temporal_state)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memory_records_valid_time
         ON memory_records(profile_id, workspace_id, valid_from, valid_until)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memory_records_superseded_by
         ON memory_records(superseded_by)",
        [],
    )?;
    Ok(())
}

fn trust_defaults_for_new_record(
    new: &NewRecord,
    now: &str,
) -> (String, f64, Option<String>, Option<String>) {
    let explicit_state = metadata_text(&new.metadata, "trust_state")
        .or_else(|| metadata_text(&new.metadata, "promotion_status"))
        .or_else(|| metadata_text(&new.metadata, "state"))
        .or_else(|| metadata_text(&new.metadata, "candidate_state"))
        .map(|value| normalized_trust_value(&value));
    let source_risk = metadata_text(&new.metadata, "source_risk")
        .or_else(|| metadata_text(&new.metadata, "risk"))
        .map(|value| normalized_trust_value(&value));

    let should_quarantine =
        matches!(
            explicit_state.as_deref(),
            Some("quarantined" | "quarantine")
        ) || matches!(source_risk.as_deref(), Some("high" | "unsafe" | "blocked"));

    if should_quarantine {
        let reason = metadata_text(&new.metadata, "quarantine_reason")
            .or_else(|| {
                source_risk
                    .as_ref()
                    .map(|risk| format!("source_risk:{risk}"))
            })
            .or_else(|| Some("explicit_quarantine".to_string()));
        return (
            "quarantined".to_string(),
            0.0,
            reason,
            Some(now.to_string()),
        );
    }

    (
        "trusted".to_string(),
        metadata_number(&new.metadata, "trust_score")
            .map(|score| score.clamp(0.0, 1.0))
            .unwrap_or(1.0),
        None,
        None,
    )
}

fn temporal_defaults_for_new_record(
    new: &NewRecord,
    now: &str,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
) {
    let temporal_state = metadata_text(&new.metadata, "temporal_state")
        .or_else(|| metadata_text(&new.metadata, "state"))
        .and_then(|value| TemporalState::parse(&value))
        .unwrap_or(TemporalState::Current)
        .as_str()
        .to_string();

    (
        metadata_text(&new.metadata, "valid_from"),
        metadata_text(&new.metadata, "valid_until"),
        metadata_text(&new.metadata, "observed_at").or_else(|| Some(now.to_string())),
        metadata_text(&new.metadata, "invalidated_at"),
        metadata_text(&new.metadata, "superseded_by"),
        metadata_text(&new.metadata, "historical_reason"),
        temporal_state,
    )
}

fn metadata_text(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn metadata_number(metadata: &Value, key: &str) -> Option<f64> {
    metadata.get(key).and_then(|value| value.as_f64())
}

fn normalized_trust_value(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn ensure_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !columns.iter().any(|name| name == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
            [],
        )?;
    }
    Ok(())
}

// ----------------------------------------------------------------------
// Row mappers and SQL helpers
// ----------------------------------------------------------------------

const RECORD_COLS: &str = "id, profile_id, workspace_id, repo_id, subject_id, episode_id, scope, type, content, related_files, tags, sensitivity, portability, confidence, source_ids, content_hash, supersedes, created_at, updated_at, last_used_at, archived, trust_state, trust_score, quarantine_reason, quarantined_at, promoted_at, valid_from, valid_until, observed_at, invalidated_at, superseded_by, historical_reason, temporal_state, metadata";
const SUBJECT_COLS: &str =
    "id, profile_id, workspace_id, subject_key, kind, display_name, created_at, updated_at, metadata";
const SUBJECT_COLS_S: &str =
    "s.id, s.profile_id, s.workspace_id, s.subject_key, s.kind, s.display_name, s.created_at, s.updated_at, s.metadata";
const EPISODE_COLS: &str =
    "id, profile_id, workspace_id, subject_id, source_kind, source_ref, started_at, ended_at, status, summary, trust_level, source_metadata, created_at, updated_at, metadata";
const SUBJECT_ALIAS_COLS: &str =
    "id, profile_id, workspace_id, subject_id, alias_key, source_evidence, created_at, metadata";
const RELATION_COLS: &str = "id, profile_id, workspace_id, from_subject_id, relation_type, to_subject_id, confidence, state, source_episode_ids, source_evidence, created_at, retired_at, metadata";
const RELATION_COLS_R: &str = "r.id, r.profile_id, r.workspace_id, r.from_subject_id, r.relation_type, r.to_subject_id, r.confidence, r.state, r.source_episode_ids, r.source_evidence, r.created_at, r.retired_at, r.metadata";
const PROCEDURE_COLS: &str = "id, profile_id, workspace_id, subject_id, repo_id, name, activation_query, steps, guardrails, termination_condition, source_episode_ids, confidence, state, created_at, retired_at, version, first_seen, last_validated, superseded_by, counter_evidence_count, negative_examples, metadata";

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
    sql.push_str(&format!(" AND {} != 'quarantined'", col("trust_state")));
    if let Some(cutoff) = &filters.recency_cutoff {
        sql.push_str(&format!(" AND {} >= ?", col("updated_at")));
        args.push(Box::new(cutoff.clone()));
    }
}

fn count_recall_omission(
    conn: &rusqlite::Connection,
    filters: &RecordQuery,
    query_text: &str,
    reason: &str,
) -> Result<usize> {
    let mut sql = "SELECT COUNT(*) FROM memory_records WHERE 1=1".to_string();
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(p) = &filters.profile_id {
        sql.push_str(" AND profile_id = ?");
        args.push(Box::new(p.clone()));
    }
    if let Some(w) = &filters.workspace_id {
        sql.push_str(" AND workspace_id = ?");
        args.push(Box::new(w.clone()));
    }
    if let Some(r) = &filters.repo_id {
        sql.push_str(" AND repo_id = ?");
        args.push(Box::new(r.clone()));
    }
    if let Some(t) = &filters.record_type {
        sql.push_str(" AND type = ?");
        args.push(Box::new(t.as_str().to_string()));
    }
    if let Some(s) = &filters.scope {
        sql.push_str(" AND scope = ?");
        args.push(Box::new(s.as_str().to_string()));
    }
    if let Some(cutoff) = &filters.recency_cutoff {
        sql.push_str(" AND updated_at >= ?");
        args.push(Box::new(cutoff.clone()));
    }

    match reason {
        "archived" => sql.push_str(
            " AND archived = 1 AND sensitivity != 'secret_blocked' AND trust_state != 'quarantined'",
        ),
        "secret_blocked" => sql.push_str(" AND sensitivity = 'secret_blocked'"),
        "quarantined" => {
            sql.push_str(" AND sensitivity != 'secret_blocked' AND trust_state = 'quarantined'")
        }
        _ => return Ok(0),
    }

    let trimmed = query_text.trim();
    if !trimmed.is_empty() {
        sql.push_str(" AND (content LIKE ? OR tags LIKE ? OR related_files LIKE ?)");
        let like = format!("%{}%", escape_like(trimmed));
        args.push(Box::new(like.clone()));
        args.push(Box::new(like.clone()));
        args.push(Box::new(like));
    }

    let params_ref: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let count: i64 = conn.query_row(&sql, params_ref.as_slice(), |row| row.get(0))?;
    Ok(count as usize)
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

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn validate_relation(relation: &Relation) -> Result<()> {
    if relation.from_subject_id.trim().is_empty() || relation.to_subject_id.trim().is_empty() {
        return Err(Error::invalid_request("relation endpoints are required"));
    }
    if relation.relation_type.trim().is_empty() {
        return Err(Error::invalid_request("relation_type is required"));
    }
    if !matches!(
        relation.relation_type.as_str(),
        "uses" | "owns" | "prefers" | "works_on" | "depends_on" | "supersedes" | "blocked_by"
    ) {
        return Err(Error::invalid_request(format!(
            "unknown relation_type '{}'",
            relation.relation_type
        )));
    }
    if !matches!(relation.state.as_str(), "candidate" | "active" | "retired") {
        return Err(Error::invalid_request(format!(
            "unknown relation state '{}'",
            relation.state
        )));
    }
    if relation.source_episode_ids.is_empty()
        || relation
            .source_episode_ids
            .iter()
            .all(|value| value.trim().is_empty())
        || relation
            .source_evidence
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(Error::invalid_request(
            "relation evidence is required via source_episode_ids and source_evidence",
        ));
    }
    Ok(())
}

fn row_to_record(row: &Row) -> rusqlite::Result<MemoryRecord> {
    Ok(MemoryRecord {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        repo_id: row.get(3)?,
        subject_id: row.get(4)?,
        episode_id: row.get(5)?,
        scope: Scope::parse(&row.get::<_, String>(6)?).unwrap_or(Scope::Workspace),
        record_type: RecordType::parse(&row.get::<_, String>(7)?).unwrap_or(RecordType::Other),
        content: row.get(8)?,
        related_files: json_str_list(&row.get::<_, String>(9)?),
        tags: json_str_list(&row.get::<_, String>(10)?),
        sensitivity: Sensitivity::parse(&row.get::<_, String>(11)?)
            .unwrap_or(Sensitivity::Personal),
        portability: Portability::parse(&row.get::<_, String>(12)?)
            .unwrap_or(Portability::ProfileOnly),
        confidence: row.get(13)?,
        source_ids: json_str_list(&row.get::<_, String>(14)?),
        content_hash: row.get(15)?,
        supersedes: json_str_list(&row.get::<_, String>(16)?),
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
        last_used_at: row.get(19)?,
        archived: row.get::<_, i64>(20)? != 0,
        trust_state: row.get(21)?,
        trust_score: row.get(22)?,
        quarantine_reason: row.get(23)?,
        quarantined_at: row.get(24)?,
        promoted_at: row.get(25)?,
        valid_from: row.get(26)?,
        valid_until: row.get(27)?,
        observed_at: row.get(28)?,
        invalidated_at: row.get(29)?,
        superseded_by: row.get(30)?,
        historical_reason: row.get(31)?,
        temporal_state: TemporalState::parse(&row.get::<_, String>(32)?)
            .unwrap_or(TemporalState::Current),
        metadata: json_value(&row.get::<_, String>(33)?),
    })
}

fn row_to_subject(row: &Row) -> rusqlite::Result<Subject> {
    Ok(Subject {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        subject_key: row.get(3)?,
        kind: SubjectKind::parse(&row.get::<_, String>(4)?).unwrap_or(SubjectKind::Other),
        display_name: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        metadata: json_value(&row.get::<_, String>(8)?),
    })
}

fn row_to_episode(row: &Row) -> rusqlite::Result<Episode> {
    Ok(Episode {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        subject_id: row.get(3)?,
        source_kind: row.get(4)?,
        source_ref: row.get(5)?,
        started_at: row.get(6)?,
        ended_at: row.get(7)?,
        status: row.get(8)?,
        summary: row.get(9)?,
        trust_level: row.get(10)?,
        source_metadata: json_value(&row.get::<_, String>(11)?),
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        metadata: json_value(&row.get::<_, String>(14)?),
    })
}

fn row_to_subject_alias(row: &Row) -> rusqlite::Result<SubjectAlias> {
    Ok(SubjectAlias {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        subject_id: row.get(3)?,
        alias_key: row.get(4)?,
        source_evidence: row.get(5)?,
        created_at: row.get(6)?,
        metadata: json_value(&row.get::<_, String>(7)?),
    })
}

fn row_to_relation(row: &Row) -> rusqlite::Result<Relation> {
    Ok(Relation {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        from_subject_id: row.get(3)?,
        relation_type: row.get(4)?,
        to_subject_id: row.get(5)?,
        confidence: row.get(6)?,
        state: row.get(7)?,
        source_episode_ids: json_str_list(&row.get::<_, String>(8)?),
        source_evidence: row.get(9)?,
        created_at: row.get(10)?,
        retired_at: row.get(11)?,
        metadata: json_value(&row.get::<_, String>(12)?),
    })
}

fn row_to_procedure(row: &Row) -> rusqlite::Result<Procedure> {
    Ok(Procedure {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        workspace_id: row.get(2)?,
        subject_id: row.get(3)?,
        repo_id: row.get(4)?,
        name: row.get(5)?,
        activation_query: row.get(6)?,
        steps: row.get(7)?,
        guardrails: row.get(8)?,
        termination_condition: row.get(9)?,
        source_episode_ids: json_str_list(&row.get::<_, String>(10)?),
        confidence: row.get(11)?,
        state: row.get(12)?,
        created_at: row.get(13)?,
        retired_at: row.get(14)?,
        version: row.get(15)?,
        first_seen: row.get(16)?,
        last_validated: row.get(17)?,
        superseded_by: row.get(18)?,
        counter_evidence_count: row.get(19)?,
        negative_examples: json_str_list(&row.get::<_, String>(20)?),
        metadata: json_value(&row.get::<_, String>(21)?),
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
            subject_id: None,
            episode_id: None,
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
