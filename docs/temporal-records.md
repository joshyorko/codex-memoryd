# Temporal records as-of recall (#155) — implemented slice

This document describes the landed #155 implementation slice.

## Scope that shipped

- Added temporal columns on `memory_records`:
  - `valid_from`
  - `valid_until`
  - `observed_at`
  - `invalidated_at`
  - `superseded_by`
  - `historical_reason`
  - `temporal_state`
- Added recall read-path controls:
  - `recall --as-of <RFC3339>`
  - `recall --include-history`
- Added temporal fixtures under `tests/fixtures/temporal/`.
- Bumped storage schema to `STORAGE_SCHEMA_VERSION = 9`.
- Added migration marker `migrations/0010_temporal_records.sql` and `ensure_temporal_columns` setup in `src/store.rs`.

`0010_temporal_records.sql` is currently an idempotent migration marker; column creation/backfill happens via `ensure_temporal_columns` so existing DBs upgrade safely and losslessly.

## Storage semantics

Existing `memory_records` are treated as current facts by default to preserve prior behavior:

- `temporal_state`: default `'current'` for new rows and existing rows on migration.
- Temporal timestamps default to `NULL` for existing rows.
- `valid_from` and `observed_at` can be NULL; recall comparisons treat NULL as not blocking current-time recall.

## Default recall (no `--as-of`, no `--include-history`)

Default recall now applies temporal admission in addition to existing trust/quarantine/metadata gates:

1. Exclude `temporal_state = 'planned'` and any record with `valid_from > now`.
2. Exclude non-`current` temporal state (`superseded`, `completed`, `invalidated`, `historical`-style values).
3. Exclude records with `valid_until <= now`.
4. Exclude records with `invalidated_at <= now`.

Records withheld for this reason are surfaced with temporal deny reasons:
- `temporal_planned`
- `temporal_historical`

## `--as-of` recall

`recall --as-of <RFC3339>` evaluates what was true at that instant:

1. `valid_from` must be absent or `<= as_of`.
2. `observed_at` must be absent or `<= as_of` (prevents future knowledge leak).
3. `valid_until` must be absent or `> as_of`.
4. `invalidated_at` must be absent or `> as_of`.

This is intentionally read-only, bitemporal-style behavior: a fact superseded later may still be returned for an earlier instant.

## `--include-history`

`recall --include-history` relaxes temporal withholding so historical rows are recall-visible for inspection/debugging workflows.

Notes:

- It is still a recall request, not a state mutation.
- Current safety gates remain in force (e.g., quarantined/high-risk logic), but temporal currentness filtering is bypassed.

## Migration, back-compat, and rollout assumptions

- Migration marker strategy follows existing store patterns: additive columns plus safe defaults.
- Existing non-temporal data remains visible in current recall by default.
- Backfill fixture `backfill_default_current.json` continues to assert this compatibility behavior.

## Non-goals for this slice

- No Dreamer auto-rewrite / contradiction automation.
- No embeddings/graph-DB temporal model replacement.
- No procedure maintenance workflow work.
- No cross-slice behavior changes beyond implemented recall + schema controls.

## Example CLI usage

```bash
codex-memoryd recall --query "How should I configure this task?" \
  --as-of "2026-03-01T00:00:00Z"

codex-memoryd recall --query "What has changed over time?" --include-history
```

```bash
codex-memoryd recall --query "safe default behavior" \
  --as-of "2026-01-10T00:00:00Z" \
  --include-history
```

## Fixture set

Temporal fixtures are in `tests/fixtures/temporal/`:
- `changed_preference.json`
- `repo_state_change.json`
- `completed_work.json`
- `contradicted_claim.json`
- `relative_time_record.json`
- `backfill_default_current.json`

Each fixture defines scenario-specific query/expectation inputs for default recall, `--as-of`, and history behavior.

## Related docs

- `docs/temporal-records.md` is this doc for #155.
- `docs/compatibility-policy.md` for schema bump compatibility expectations.
