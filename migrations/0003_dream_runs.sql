CREATE TABLE IF NOT EXISTS dream_runs (
    id                     TEXT,
    run_id                 TEXT,
    profile_id             TEXT NOT NULL,
    workspace_id           TEXT NOT NULL,
    repo_id                TEXT,
    mode                   TEXT NOT NULL,
    kind                   TEXT NOT NULL DEFAULT 'manual',
    status                 TEXT NOT NULL,
    started_at             TEXT NOT NULL,
    completed_at           TEXT,
    implementation_version TEXT NOT NULL DEFAULT '',
    config_hash            TEXT NOT NULL DEFAULT '',
    ruleset_version        TEXT NOT NULL DEFAULT '',
    fixture_schema_version TEXT,
    source_window_start    TEXT,
    source_window_end      TEXT,
    source_counts          TEXT NOT NULL DEFAULT '{}',
    candidate_counts       TEXT NOT NULL DEFAULT '{}',
    created_count          INTEGER NOT NULL DEFAULT 0,
    archived_count         INTEGER NOT NULL DEFAULT 0,
    rejected_count         INTEGER NOT NULL DEFAULT 0,
    error_summary          TEXT,
    watermark_before       TEXT,
    watermark_after        TEXT,
    error                  TEXT,
    candidates             INTEGER NOT NULL DEFAULT 0,
    created                INTEGER NOT NULL DEFAULT 0,
    archived               INTEGER NOT NULL DEFAULT 0,
    limits_hit             TEXT NOT NULL DEFAULT '[]'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_dream_runs_id
    ON dream_runs(id)
    WHERE id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_dream_runs_run_id
    ON dream_runs(run_id)
    WHERE run_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_dream_runs_scope_completed
    ON dream_runs(profile_id, workspace_id, repo_id, status, completed_at);

CREATE INDEX IF NOT EXISTS idx_dream_runs_scheduled_scope
    ON dream_runs(kind, profile_id, workspace_id, repo_id, completed_at);
