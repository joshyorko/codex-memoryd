# Storage backend plan: SQLite now, optional libSQL later (#191)

Status: design-only first wave. This plan does not change default storage behavior, does not remove `rusqlite`, and does not change recall or Dreamer behavior.

## Goals

- Keep the current bundled SQLite path as the default and compatibility baseline.
- Define a seam that can eventually host SQLite, libSQL local, libSQL sync/self-hosted, and remote libSQL/Turso-style modes.
- Preserve local, external-service-free CI.
- Make cloud/sync/remote paths opt-in and visible in status.

## Current architecture inventory

The current store is a concrete SQLite implementation. `Store` wraps an `r2d2_sqlite` pool and carries FTS/degraded capability state. `Store::open` owns directory creation, WAL and PRAGMA setup, connection pool sizing, migration replay, imperative backfills, and FTS probing. Migrations include core records/sessions/visible turns, optional FTS5 triggers, Dreamer audit rows, evidence ledger, subjects/episodes, procedures, semantic relations, and marker migrations backed by Rust `ensure_*` helpers.

SQLite-specific behavior currently includes:

- WAL mode and per-connection PRAGMAs.
- `rusqlite` transactions and row mappers.
- SQLite online backup and integrity checks.
- FTS5 virtual tables, triggers, `MATCH`, `bm25`, and LIKE fallback.
- `json_extract` filtering and JSON-as-text metadata.
- Imperative `ALTER TABLE ADD COLUMN` backfills.
- Global `content_hash` uniqueness for record idempotency.

## Backend options

| Mode | Default? | Network? | Strengths | Risks / implications |
| --- | --- | --- | --- | --- |
| Current SQLite via `rusqlite` | Yes | No | Proven local path, bundled SQLite, existing migrations/tests/backups/FTS | Concrete monolith; SQLite APIs leak into store surface |
| libSQL local file | No | No | Potential compatibility path while staying local-first | Driver differences, FTS/backup/migration behavior must be proven |
| libSQL sync or self-hosted `sqld` | No | Optional/operator-owned | Future sync-oriented deployment, local app/runtime portability | Auth, conflict behavior, status visibility, offline semantics, CI skips |
| Remote managed libSQL/Turso-style | No | Yes | Operator-chosen remote sync/backup story | Cloud dependency, paid/network risk, credential handling, must be opt-in and never CI default |

## Proposed seam

Start by naming backend capabilities before introducing a large trait:

1. `StorageRuntime` / `StorageBackendInfo`
   - kind, path/URL display, local-vs-remote, writable, degraded reasons, capability flags.
2. `SchemaStore`
   - migrate, schema report, integrity check, table inventory.
3. `MemoryRecordStore`
   - upsert/query/export/search/lifecycle operations and idempotency behavior.
4. `EvidenceStore`
   - visible turns, source ledger, conclusions, checkpoints, Dreamer windows, policy events.
5. `SemanticProcedureStore`
   - subjects, episodes, aliases, relations, procedures.
6. `BackupStore`
   - backend-specific backup, verify, restore-preview/apply, manifest generation.
7. `SearchBackend`
   - FTS/LIKE/candidate retrieval capability and ranking semantics.

Do not immediately trait-ify every `Store` method. First split method groups and tests so `SqliteStore` remains the only implementation behind the existing `Store` facade.

## Operations that block abstraction

- Migration execution and Rust-side backfills.
- FTS5 triggers/ranking and LIKE fallback differences.
- Online backup and verification reopening behavior.
- `schema_report`/manifest table inventory not matching every actual table.
- JSON path queries in SQL.
- Transaction semantics across multi-table Dreamer/import writes.
- `content_hash` global uniqueness and source-ID merging.
- Search ranking compatibility and degraded capability reporting.
- Future vector/hybrid artifacts and sync conflict resolution.

## POC constraints

A compile-only POC is acceptable only if:

- It is behind an opt-in Cargo feature.
- It does not change the default `Store::open` path.
- It does not require cloud credentials or network.
- It is documented as non-production and excluded from default CI unless dependencies are already local/offline-friendly.

## Test plan for later implementation

- Schema upgrade and migration idempotency.
- FTS-enabled and FTS-degraded search behavior.
- Backup/verify/restore preview/apply.
- Recall/export security filtering.
- Record lifecycle, quarantine, supersession, and idempotent upsert.
- Evidence ledger and visible-turn writes.
- Dreamer preview/apply audit rows and watermarks.
- Semantic relation and procedure lifecycle tests.
