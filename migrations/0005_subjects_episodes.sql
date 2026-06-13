-- Subject/episode substrate (storage_schema_version = 4).

CREATE TABLE IF NOT EXISTS subjects (
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

CREATE UNIQUE INDEX IF NOT EXISTS idx_subjects_scope_key
    ON subjects(profile_id, workspace_id, subject_key);
CREATE INDEX IF NOT EXISTS idx_subjects_scope_kind
    ON subjects(profile_id, workspace_id, kind);

CREATE TABLE IF NOT EXISTS episodes (
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

CREATE INDEX IF NOT EXISTS idx_episodes_scope_subject
    ON episodes(profile_id, workspace_id, subject_id, created_at);
CREATE INDEX IF NOT EXISTS idx_episodes_source
    ON episodes(source_kind, source_ref);
