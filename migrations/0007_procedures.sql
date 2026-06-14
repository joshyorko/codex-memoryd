-- Procedural memory substrate (storage_schema_version = 5).

CREATE TABLE IF NOT EXISTS procedures (
    id                    TEXT PRIMARY KEY,
    profile_id            TEXT NOT NULL,
    workspace_id          TEXT NOT NULL,
    subject_id            TEXT,
    repo_id               TEXT,
    name                  TEXT NOT NULL,
    activation_query      TEXT NOT NULL,
    steps                 TEXT NOT NULL,
    guardrails            TEXT NOT NULL,
    termination_condition TEXT NOT NULL,
    source_episode_ids    TEXT NOT NULL DEFAULT '[]',
    confidence            REAL NOT NULL DEFAULT 0.5,
    state                 TEXT NOT NULL,
    created_at            TEXT NOT NULL,
    retired_at            TEXT,
    metadata              TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_procedures_scope_state
    ON procedures(profile_id, workspace_id, state, created_at);
CREATE INDEX IF NOT EXISTS idx_procedures_subject
    ON procedures(profile_id, workspace_id, subject_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_procedures_candidate_dedupe
    ON procedures(profile_id, workspace_id, subject_id, activation_query, steps)
    WHERE retired_at IS NULL;
