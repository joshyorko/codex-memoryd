-- Lightweight semantic relation substrate (storage_schema_version = 8).
--
-- Aliases and relations are additive, local SQLite tables. Application code
-- enforces profile/workspace endpoint scope and evidence requirements.

CREATE TABLE IF NOT EXISTS subject_aliases (
    id              TEXT PRIMARY KEY,
    profile_id      TEXT NOT NULL,
    workspace_id    TEXT NOT NULL,
    subject_id      TEXT NOT NULL,
    alias_key       TEXT NOT NULL,
    source_evidence TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    metadata        TEXT NOT NULL DEFAULT '{}',
    UNIQUE(profile_id, workspace_id, alias_key)
);

CREATE INDEX IF NOT EXISTS idx_subject_aliases_subject
    ON subject_aliases(profile_id, workspace_id, subject_id);

CREATE TABLE IF NOT EXISTS relations (
    id                 TEXT PRIMARY KEY,
    profile_id         TEXT NOT NULL,
    workspace_id       TEXT NOT NULL,
    from_subject_id    TEXT NOT NULL,
    relation_type      TEXT NOT NULL,
    to_subject_id      TEXT NOT NULL,
    confidence         REAL NOT NULL DEFAULT 0.5,
    state              TEXT NOT NULL,
    source_episode_ids TEXT NOT NULL DEFAULT '[]',
    source_evidence    TEXT,
    created_at         TEXT NOT NULL,
    retired_at         TEXT,
    metadata           TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_relations_from
    ON relations(profile_id, workspace_id, from_subject_id, state);
CREATE INDEX IF NOT EXISTS idx_relations_to
    ON relations(profile_id, workspace_id, to_subject_id, state);
CREATE UNIQUE INDEX IF NOT EXISTS idx_relations_scope_dedupe
    ON relations(profile_id, workspace_id, from_subject_id, relation_type, to_subject_id)
    WHERE retired_at IS NULL AND state != 'retired';
