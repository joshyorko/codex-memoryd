CREATE TABLE IF NOT EXISTS evidence_ledger (
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

CREATE INDEX IF NOT EXISTS idx_evidence_ledger_scope_created
    ON evidence_ledger(profile_id, workspace_id, repo_id, created_at DESC);
