-- Portable bundle identity and durable import receipts.
--
-- The instance row is populated by Store::migrate so a fresh database gets a
-- public identity without making SQLite responsible for UUID generation.

CREATE TABLE IF NOT EXISTS instance_metadata (
    singleton_key TEXT PRIMARY KEY,
    instance_id   TEXT NOT NULL UNIQUE,
    created_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS portable_object_origins (
    origin_instance_id    TEXT NOT NULL,
    object_kind           TEXT NOT NULL,
    origin_object_id      TEXT NOT NULL,
    local_object_id       TEXT NOT NULL,
    imported_object_digest TEXT NOT NULL,
    canonical             INTEGER NOT NULL DEFAULT 1,
    first_bundle_id       TEXT NOT NULL,
    imported_at           TEXT NOT NULL,
    PRIMARY KEY (origin_instance_id, object_kind, origin_object_id)
);
CREATE INDEX IF NOT EXISTS idx_portable_origins_local
    ON portable_object_origins(object_kind, local_object_id);

CREATE TABLE IF NOT EXISTS bundle_import_receipts (
    receipt_id              TEXT PRIMARY KEY,
    bundle_id               TEXT NOT NULL,
    manifest_digest         TEXT NOT NULL,
    source_instance_id      TEXT NOT NULL,
    destination_instance_id TEXT NOT NULL,
    mapping_digest          TEXT NOT NULL,
    plan_id                 TEXT NOT NULL,
    status                  TEXT NOT NULL,
    created_count           INTEGER NOT NULL DEFAULT 0,
    reused_identity_count   INTEGER NOT NULL DEFAULT 0,
    reused_content_count    INTEGER NOT NULL DEFAULT 0,
    external_reference_count INTEGER NOT NULL DEFAULT 0,
    applied_at              TEXT NOT NULL,
    UNIQUE (bundle_id, destination_instance_id, mapping_digest)
);
