CREATE TABLE IF NOT EXISTS consolidation_proposals (
    batch_id        TEXT PRIMARY KEY,
    scope           TEXT NOT NULL,
    proposal_digest TEXT NOT NULL,
    batch_json      TEXT NOT NULL,
    decisions_json  TEXT,
    applied_record_ids_json TEXT,
    status          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (scope, proposal_digest)
);
CREATE INDEX IF NOT EXISTS idx_consolidation_proposals_scope_status
    ON consolidation_proposals(scope, status, updated_at);


-- Freeze both sides of supersession; missing snapshots fail closed on replay.
CREATE TABLE IF NOT EXISTS consolidation_target_revisions (
    batch_id TEXT NOT NULL REFERENCES consolidation_proposals(batch_id),
    record_id TEXT NOT NULL,
    proposed_revision TEXT NOT NULL,
    applied_revision TEXT,
    PRIMARY KEY (batch_id, record_id)
);
