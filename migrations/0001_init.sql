-- codex-memoryd initial schema (storage_schema_version = 1)
-- All timestamps are RFC3339 UTC strings. JSON columns store serde_json values.

CREATE TABLE IF NOT EXISTS profiles (
    id            TEXT PRIMARY KEY,
    display_name  TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    default_portability_policy TEXT NOT NULL DEFAULT 'profile_only'
);

CREATE TABLE IF NOT EXISTS workspaces (
    id            TEXT NOT NULL,
    profile_id    TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    PRIMARY KEY (profile_id, id)
);

CREATE TABLE IF NOT EXISTS repos (
    repo_id   TEXT PRIMARY KEY,
    root      TEXT,
    remote    TEXT,
    branch    TEXT,
    commit_sha TEXT,
    is_git    INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    repo_id      TEXT,
    thread_id    TEXT,
    source       TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    ended_at     TEXT,
    metadata     TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS visible_turns (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    actor       TEXT NOT NULL,
    content     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    metadata    TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_visible_turns_session ON visible_turns(session_id);

CREATE TABLE IF NOT EXISTS memory_sources (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    kind         TEXT NOT NULL,
    source_path  TEXT,
    source_hash  TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    ingested_at  TEXT NOT NULL,
    metadata     TEXT NOT NULL DEFAULT '{}'
);
-- Source-hash dedupe is scoped to profile/workspace/path (SPEC §7.9).
CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_sources_dedupe
    ON memory_sources(profile_id, workspace_id, source_path, source_hash);

CREATE TABLE IF NOT EXISTS memory_records (
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
    valid_from    TEXT,
    valid_until   TEXT,
    observed_at   TEXT,
    invalidated_at TEXT,
    superseded_by TEXT,
    historical_reason TEXT,
    temporal_state TEXT NOT NULL DEFAULT 'current',
    metadata      TEXT NOT NULL DEFAULT '{}'
);
-- Content-hash dedupe is the idempotency guarantee for writes/imports.
CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_records_content_hash
    ON memory_records(content_hash);
CREATE INDEX IF NOT EXISTS idx_memory_records_scope_filter
    ON memory_records(profile_id, workspace_id, archived);
CREATE INDEX IF NOT EXISTS idx_memory_records_repo
    ON memory_records(repo_id);
CREATE INDEX IF NOT EXISTS idx_memory_records_type
    ON memory_records(type);

CREATE TABLE IF NOT EXISTS conclusions (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    repo_id      TEXT,
    target       TEXT NOT NULL,
    content      TEXT NOT NULL,
    source_id    TEXT,
    created_at   TEXT NOT NULL,
    metadata     TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS checkpoints (
    id             TEXT PRIMARY KEY,
    session_id     TEXT,
    profile_id     TEXT NOT NULL,
    workspace_id   TEXT NOT NULL,
    repo_id        TEXT,
    summary        TEXT NOT NULL,
    changed_files  TEXT NOT NULL DEFAULT '[]',
    decisions      TEXT NOT NULL DEFAULT '[]',
    blockers       TEXT NOT NULL DEFAULT '[]',
    next_steps     TEXT NOT NULL DEFAULT '[]',
    tests_run      TEXT NOT NULL DEFAULT '[]',
    tests_not_run  TEXT NOT NULL DEFAULT '[]',
    branch         TEXT,
    commit_sha     TEXT,
    created_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_checkpoints_scope
    ON checkpoints(profile_id, workspace_id, repo_id);

CREATE TABLE IF NOT EXISTS sync_cursors (
    id              TEXT PRIMARY KEY,
    profile_id      TEXT NOT NULL,
    workspace_id    TEXT NOT NULL,
    source_root     TEXT NOT NULL,
    last_started_at   TEXT,
    last_completed_at TEXT,
    last_error      TEXT,
    metadata        TEXT NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sync_cursors_scope
    ON sync_cursors(profile_id, workspace_id, source_root);

CREATE TABLE IF NOT EXISTS policy_events (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT,
    workspace_id TEXT,
    kind        TEXT NOT NULL,    -- secret_detected | injection | boundary_denied | oversized | accepted
    code        TEXT NOT NULL,
    reason      TEXT NOT NULL,
    context     TEXT NOT NULL DEFAULT 'unknown',  -- turns | conclusions | sync | export | forget
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_policy_events_created ON policy_events(created_at);
