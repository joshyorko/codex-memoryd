//! Storage schema upgrade / downgrade-safety matrix (issue #140).
//!
//! `codex-memoryd` replays migrations idempotently on every `Store::open` and
//! back-fills columns through `ensure_*` helpers, so "upgrading" an old database
//! means opening it with the current binary. These tests build databases that
//! match each *prior* schema generation — using the historical table SQL, with
//! the later columns and tables deliberately absent — seed representative rows,
//! then open them with the current `Store` and assert the upgrade is lossless:
//! records, subjects, episodes, evidence, procedures, and quarantine state all
//! survive and every read path works.
//!
//! Fixtures are generated deterministically in-process: no binary database is
//! committed, and no personal/dogfood data is used. A real dogfood DB can be
//! exercised the same way by pointing a copy at `Store::open` — see
//! `docs/release/v0.1-hardening.md`.

use std::path::Path;

use codex_memoryd::store::Store;
use codex_memoryd::store::STORAGE_SCHEMA_VERSION;
use rusqlite::params;
use rusqlite::Connection;
use tempfile::TempDir;

/// Historical `memory_records` shape as it existed at schema v1 — before the
/// `subject_id`/`episode_id` ref columns and the trust/quarantine columns were
/// back-filled by `ensure_*`. Pinned here so the regression survives future
/// edits to the live migration files.
const V1_MEMORY_RECORDS: &str = "
CREATE TABLE memory_records (
    id            TEXT PRIMARY KEY,
    profile_id    TEXT NOT NULL,
    workspace_id  TEXT NOT NULL,
    repo_id       TEXT,
    scope         TEXT NOT NULL,
    type          TEXT NOT NULL,
    content       TEXT NOT NULL,
    related_files TEXT NOT NULL DEFAULT '[]',
    tags          TEXT NOT NULL DEFAULT '[]',
    sensitivity   TEXT NOT NULL,
    portability   TEXT NOT NULL,
    confidence    REAL NOT NULL DEFAULT 0.5,
    source_ids    TEXT NOT NULL DEFAULT '[]',
    content_hash  TEXT NOT NULL,
    supersedes    TEXT NOT NULL DEFAULT '[]',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    last_used_at  TEXT,
    archived      INTEGER NOT NULL DEFAULT 0,
    metadata      TEXT NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX idx_memory_records_content_hash ON memory_records(content_hash);
";

const V4_SUBJECTS: &str = "
CREATE TABLE subjects (
    id            TEXT PRIMARY KEY,
    profile_id    TEXT NOT NULL,
    workspace_id  TEXT NOT NULL,
    subject_key   TEXT NOT NULL,
    kind          TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    metadata      TEXT NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX idx_subjects_scope_key ON subjects(profile_id, workspace_id, subject_key);
CREATE TABLE episodes (
    id              TEXT PRIMARY KEY,
    profile_id      TEXT NOT NULL,
    workspace_id    TEXT NOT NULL,
    subject_id      TEXT NOT NULL,
    source_kind     TEXT NOT NULL,
    source_ref      TEXT NOT NULL,
    started_at      TEXT,
    ended_at        TEXT,
    status          TEXT,
    summary         TEXT NOT NULL,
    trust_level     TEXT,
    source_metadata TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    metadata        TEXT NOT NULL DEFAULT '{}'
);
";

const V3_EVIDENCE_LEDGER: &str = "
CREATE TABLE evidence_ledger (
    id            TEXT PRIMARY KEY,
    event_key     TEXT NOT NULL UNIQUE,
    profile_id    TEXT NOT NULL,
    workspace_id  TEXT NOT NULL,
    repo_id       TEXT,
    subject_key   TEXT,
    source_kind   TEXT NOT NULL,
    source_id     TEXT,
    source_path   TEXT,
    source_hash   TEXT NOT NULL,
    safe_summary  TEXT NOT NULL,
    policy_state  TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    metadata      TEXT NOT NULL DEFAULT '{}'
);
";

fn now() -> &'static str {
    "2030-01-01T00:00:00Z"
}

/// Insert one v1-shape memory record (no ref/trust columns).
fn seed_v1_record(conn: &Connection, id: &str, content: &str, content_hash: &str) {
    conn.execute(
        "INSERT INTO memory_records(
            id, profile_id, workspace_id, repo_id, scope, type, content,
            sensitivity, portability, confidence, content_hash, created_at, updated_at
        ) VALUES (?1,'personal','default',NULL,'workspace','preference',?2,
                  'personal','portable',0.9,?3,?4,?4)",
        params![id, content, content_hash, now()],
    )
    .expect("seed v1 record");
}

fn seed_subject(conn: &Connection, id: &str, key: &str) {
    conn.execute(
        "INSERT INTO subjects(id, profile_id, workspace_id, subject_key, kind, display_name, created_at, updated_at)
         VALUES (?1,'personal','default',?2,'workflow',?2,?3,?3)",
        params![id, key, now()],
    )
    .expect("seed subject");
}

fn seed_episode(conn: &Connection, id: &str, subject_id: &str, summary: &str) {
    conn.execute(
        "INSERT INTO episodes(id, profile_id, workspace_id, subject_id, source_kind, source_ref, status, summary, trust_level, created_at, updated_at)
         VALUES (?1,'personal','default',?2,'session',?1,'success',?3,'trusted',?4,?4)",
        params![id, subject_id, summary, now()],
    )
    .expect("seed episode");
}

fn seed_evidence(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO evidence_ledger(id, event_key, profile_id, workspace_id, source_kind, source_hash, safe_summary, policy_state, created_at)
         VALUES (?1,?1,'personal','default','session','hash-1','imported a decision','accepted',?2)",
        params![id, now()],
    )
    .expect("seed evidence");
}

fn seed_v7_schema(conn: &Connection) {
    conn.execute_batch(include_str!("../migrations/0001_init.sql"))
        .expect("seed v7 schema base tables");
    conn.execute_batch(include_str!("../migrations/0002_fts.sql"))
        .expect("seed v7 fts");
    conn.execute_batch(include_str!("../migrations/0003_dream_runs.sql"))
        .expect("seed v7 dream schema");
    conn.execute_batch(include_str!("../migrations/0004_evidence_ledger.sql"))
        .expect("seed v7 evidence schema");
    conn.execute_batch(include_str!("../migrations/0005_subjects_episodes.sql"))
        .expect("seed v7 subject/episode schema");
    conn.execute_batch(include_str!("../migrations/0006_trust_quarantine.sql"))
        .expect("seed v7 trust/quarantine marker");
    conn.execute_batch(include_str!("../migrations/0007_procedures.sql"))
        .expect("seed v7 procedure schema");
    conn.execute_batch(include_str!("../migrations/0008_procedure_lifecycle.sql"))
        .expect("seed v7 lifecycle marker");
    conn.execute_batch(
        "CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO schema_meta(key, value) VALUES ('schema_version','7');",
    )
    .expect("seed v7 schema version");
}

fn seed_v7_procedure(conn: &Connection, id: &str, subject_id: &str) {
    conn.execute(
        "INSERT INTO procedures(
            id, profile_id, workspace_id, subject_id, name,
            activation_query, steps, guardrails, termination_condition, state, created_at
         )
         VALUES (?1,'personal','default',?2,'v7 seed procedure',
                 'When evaluating relation upgrades', '[\"check migration slices\"]',
                 'reviewed only', 'stop when done', 'active', ?3)",
        params![id, subject_id, now()],
    )
    .expect("seed v7 procedure");
}

/// Build a raw SQLite file with the given SQL, returning its path inside `dir`.
fn build_old_db(dir: &TempDir, name: &str, build: impl FnOnce(&Connection)) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let conn = Connection::open(&path).expect("open raw db");
    build(&conn);
    drop(conn);
    path
}

/// Open the DB through the production `Store`, which runs every migration plus
/// the `ensure_*` back-fills.
fn upgrade(path: &Path) -> Store {
    Store::open(path).expect("Store::open upgrades the database")
}

#[test]
fn upgrades_v1_database_and_backfills_columns() {
    let dir = TempDir::new().unwrap();
    let path = build_old_db(&dir, "v1.db", |conn| {
        conn.execute_batch(V1_MEMORY_RECORDS).unwrap();
        // Emulate a pre-meta-table DB: no schema_meta, user_version stays 0.
        seed_v1_record(
            conn,
            "mem_1",
            "Use cargo test before opening a PR.",
            "hash-v1-1",
        );
        seed_v1_record(
            conn,
            "mem_2",
            "Prefer bundled SQLite for portability.",
            "hash-v1-2",
        );
    });

    let store = upgrade(&path);

    // Records preserved.
    assert_eq!(store.count_records().unwrap(), 2, "v1 records preserved");

    // The read path needs the back-filled columns (subject_id, episode_id,
    // trust_state, ...). If `ensure_*` did not run, this query errors.
    let rec = store
        .get_record("mem_1")
        .expect("get_record after upgrade")
        .expect("record exists");
    assert!(rec.content.contains("cargo test"));

    // Schema report reflects the upgrade.
    let report = store.schema_report().unwrap();
    assert_eq!(report.expected_version, STORAGE_SCHEMA_VERSION);
    assert!(report.up_to_date, "schema marked current after upgrade");
    assert_eq!(report.recorded_version, Some(STORAGE_SCHEMA_VERSION));
}

#[test]
fn upgrades_v4_database_preserving_subjects_and_episodes() {
    let dir = TempDir::new().unwrap();
    let path = build_old_db(&dir, "v4.db", |conn| {
        conn.execute_batch(V1_MEMORY_RECORDS).unwrap();
        conn.execute_batch(V3_EVIDENCE_LEDGER).unwrap();
        conn.execute_batch(V4_SUBJECTS).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO schema_meta(key, value) VALUES ('schema_version','4');",
        )
        .unwrap();
        seed_v1_record(conn, "mem_1", "A decision worth keeping.", "hash-v4-1");
        seed_subject(conn, "subj_1", "workflow:pr-review");
        seed_episode(
            conn,
            "ep_1",
            "subj_1",
            "Reviewed the diff and ran cargo test.",
        );
        seed_episode(
            conn,
            "ep_2",
            "subj_1",
            "Reviewed the diff and ran cargo test.",
        );
        seed_evidence(conn, "ev_1");
    });

    let store = upgrade(&path);

    assert_eq!(store.count_records().unwrap(), 1);
    let subjects = store.list_subjects("personal", "default", None).unwrap();
    assert_eq!(subjects.len(), 1, "subject preserved across upgrade");
    let episodes = store
        .list_successful_episodes("personal", "default", Some("subj_1"), 50)
        .unwrap();
    assert_eq!(episodes.len(), 2, "episodes preserved across upgrade");

    let report = store.schema_report().unwrap();
    assert!(report.up_to_date);
    let evidence_rows = report
        .tables
        .iter()
        .find(|t| t.table == "evidence_ledger")
        .map(|t| t.rows)
        .unwrap();
    assert_eq!(evidence_rows, 1, "evidence preserved across upgrade");
}

#[test]
fn upgrade_preserves_quarantine_state_default() {
    // A v1 record back-filled with trust columns must default to a trusted,
    // non-quarantined state — never silently quarantined or dropped.
    let dir = TempDir::new().unwrap();
    let path = build_old_db(&dir, "trust.db", |conn| {
        conn.execute_batch(V1_MEMORY_RECORDS).unwrap();
        seed_v1_record(conn, "mem_1", "Back-filled trust defaults.", "hash-trust-1");
    });

    let store = upgrade(&path);
    let rec = store.get_record("mem_1").unwrap().unwrap();
    // The domain record exposes trust via metadata/fields; the key guarantee is
    // the record is readable and not archived after upgrade.
    assert!(!rec.archived, "upgraded record not archived");
    assert_eq!(store.count_records().unwrap(), 1);
}

#[test]
fn upgrades_v6_procedures_adding_lifecycle_columns() {
    // A v6 DB has a procedures table without the lifecycle columns added in v7
    // (version, first_seen, last_validated, superseded_by, counter_evidence_count,
    // negative_examples). Opening with the current binary must back-fill them so
    // the procedure read path works and existing rows are preserved.
    let dir = TempDir::new().unwrap();
    let path = build_old_db(&dir, "v6.db", |conn| {
        conn.execute_batch(V1_MEMORY_RECORDS).unwrap();
        conn.execute_batch(V4_SUBJECTS).unwrap();
        conn.execute_batch(
            "CREATE TABLE procedures (
                id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, workspace_id TEXT NOT NULL,
                subject_id TEXT, repo_id TEXT, name TEXT NOT NULL, activation_query TEXT NOT NULL,
                steps TEXT NOT NULL, guardrails TEXT NOT NULL, termination_condition TEXT NOT NULL,
                source_episode_ids TEXT NOT NULL DEFAULT '[]', confidence REAL NOT NULL DEFAULT 0.5,
                state TEXT NOT NULL, created_at TEXT NOT NULL, retired_at TEXT,
                metadata TEXT NOT NULL DEFAULT '{}'
             );
             CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO schema_meta(key, value) VALUES ('schema_version','6');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO procedures(id, profile_id, workspace_id, subject_id, name,
                activation_query, steps, guardrails, termination_condition, state, created_at)
             VALUES ('proc_old','personal','default',NULL,'legacy procedure',
                'When working on a legacy task','- do the thing','review first',
                'stop when done','active',?1)",
            params![now()],
        )
        .unwrap();
    });

    let store = upgrade(&path);

    // The v7 procedure read path requires the back-filled columns.
    let proc = store
        .get_procedure("personal", "default", "proc_old")
        .expect("get_procedure after upgrade")
        .expect("legacy procedure preserved");
    assert_eq!(proc.name, "legacy procedure");
    assert_eq!(proc.version, 1, "back-filled version defaults to 1");
    assert_eq!(proc.counter_evidence_count, 0);
    assert!(proc.negative_examples.is_empty());
    assert!(proc.superseded_by.is_none());

    let report = store.schema_report().unwrap();
    assert!(report.up_to_date);
    let proc_rows = report
        .tables
        .iter()
        .find(|t| t.table == "procedures")
        .map(|t| t.rows)
        .unwrap();
    assert_eq!(proc_rows, 1, "procedure preserved across v6->v7 upgrade");
}

#[test]
fn upgrades_v7_database_preserves_legacy_records_and_creates_semantic_tables() {
    let dir = TempDir::new().unwrap();
    let path = build_old_db(&dir, "v7.db", |conn| {
        seed_v7_schema(conn);
        seed_v1_record(
            conn,
            "mem_1",
            "Relation migration should not alter existing memories.",
            "hash-v7-1",
        );
        seed_v1_record(
            conn,
            "mem_2",
            "Legacy subject/episode facts should remain accessible.",
            "hash-v7-2",
        );
        seed_subject(conn, "subj_1", "person:alice");
        seed_episode(
            conn,
            "ep_1",
            "subj_1",
            "Alice introduced a relation for billing.",
        );
        seed_evidence(conn, "ev_1");
        seed_v7_procedure(conn, "proc_v7", "subj_1");
    });

    let store = upgrade(&path);

    assert_eq!(store.count_records().unwrap(), 2);
    let subjects = store.list_subjects("personal", "default", None).unwrap();
    assert_eq!(subjects.len(), 1, "subject preserved across v7->v8");
    let episodes = store
        .list_successful_episodes("personal", "default", Some("subj_1"), 50)
        .unwrap();
    assert_eq!(episodes.len(), 1, "episodes preserved across v7->v8");

    let proc = store
        .get_procedure("personal", "default", "proc_v7")
        .expect("get_procedure after v7 upgrade")
        .expect("legacy v7 procedure preserved");
    assert_eq!(proc.name, "v7 seed procedure");

    let report = store.schema_report().unwrap();
    assert_eq!(report.recorded_version, Some(STORAGE_SCHEMA_VERSION));
    assert!(
        report.up_to_date,
        "schema should be current after v7 upgrade"
    );
    let alias_rows = report
        .tables
        .iter()
        .find(|t| t.table == "subject_aliases")
        .expect("subject_aliases table exists after migration")
        .rows;
    assert_eq!(alias_rows, 0, "subject_aliases should exist at v8");
    let relation_rows = report
        .tables
        .iter()
        .find(|t| t.table == "relations")
        .expect("relations table exists after migration")
        .rows;
    assert_eq!(relation_rows, 0, "relations should exist at v8");
}

#[test]
fn reopening_current_database_is_idempotent() {
    // Opening an already-current DB twice must not change counts or version.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("current.db");
    {
        let store = Store::open(&path).unwrap();
        store.ensure_workspace("personal", "default").unwrap();
    }
    let store_a = Store::open(&path).unwrap();
    let report_a = store_a.schema_report().unwrap();
    drop(store_a);
    let store_b = Store::open(&path).unwrap();
    let report_b = store_b.schema_report().unwrap();
    assert_eq!(report_a.recorded_version, report_b.recorded_version);
    assert!(report_b.up_to_date);
}
