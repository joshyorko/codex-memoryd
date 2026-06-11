CREATE TABLE IF NOT EXISTS dream_runs (
    id                     TEXT PRIMARY KEY,
    profile_id             TEXT NOT NULL,
    workspace_id           TEXT NOT NULL,
    repo_id                TEXT,
    mode                   TEXT NOT NULL,
    status                 TEXT NOT NULL,
    started_at             TEXT NOT NULL,
    completed_at           TEXT,
    implementation_version TEXT NOT NULL,
    config_hash            TEXT NOT NULL,
    ruleset_version        TEXT NOT NULL,
    fixture_schema_version TEXT,
    source_window_start    TEXT,
    source_window_end      TEXT,
    source_counts          TEXT NOT NULL DEFAULT '{}',
    candidate_counts       TEXT NOT NULL DEFAULT '{}',
    created_count          INTEGER NOT NULL DEFAULT 0,
    archived_count         INTEGER NOT NULL DEFAULT 0,
    rejected_count         INTEGER NOT NULL DEFAULT 0,
    error_summary          TEXT
);

CREATE INDEX IF NOT EXISTS idx_dream_runs_scope_completed
    ON dream_runs(profile_id, workspace_id, repo_id, status, completed_at);
