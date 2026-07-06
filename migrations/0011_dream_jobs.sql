CREATE TABLE IF NOT EXISTS dream_jobs (
    id             TEXT PRIMARY KEY,
    profile_id     TEXT NOT NULL,
    workspace_id   TEXT NOT NULL,
    repo_id        TEXT,
    kind           TEXT NOT NULL,
    mode           TEXT NOT NULL,
    status         TEXT NOT NULL,
    budget_json    TEXT NOT NULL DEFAULT '{}',
    provider_json  TEXT NOT NULL DEFAULT '{}',
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    last_run_id    TEXT,
    last_run_at    TEXT,
    last_error     TEXT
);

CREATE INDEX IF NOT EXISTS idx_dream_jobs_scope_updated
    ON dream_jobs(profile_id, workspace_id, repo_id, updated_at DESC);
