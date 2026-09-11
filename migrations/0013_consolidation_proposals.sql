CREATE TABLE IF NOT EXISTS consolidation_proposals (
    batch_id        TEXT PRIMARY KEY,
    scope           TEXT NOT NULL,
    proposal_digest TEXT NOT NULL,
    batch_json      TEXT NOT NULL,
    decisions_json  TEXT,
    status          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (scope, proposal_digest)
);
CREATE INDEX IF NOT EXISTS idx_consolidation_proposals_scope_status
    ON consolidation_proposals(scope, status, updated_at);
