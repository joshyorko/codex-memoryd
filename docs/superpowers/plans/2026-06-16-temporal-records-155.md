# Temporal Records #155 Implementation Plan

**For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or subagent-driven-development task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land one focused #155 slice: temporal fields on memory records, default current recall filtering, and opt-in as-of/history recall.

**Architecture:** Store temporal facts as first-class nullable columns on `memory_records` with `temporal_state` defaulting to `current`. Recall keeps existing storage safety gates, then applies deterministic temporal admission: default mode admits only current records at the evaluation time; `as_of` evaluates valid-time intervals; `include_history` relaxes temporal withholding for inspection. CLI and service expose the opt-in modes without hidden writes.

**Tech Stack:** Rust 2021, rusqlite, clap, serde, existing recall/service/store layers.

---

### Task 1: Temporal Fixture Tests

**Files:**
- Create: `tests/temporal_recall.rs`
- Read: `tests/fixtures/temporal/*.json`

- [ ] Write a failing fixture harness that seeds synthetic records with exact fixture ids.
- [ ] Assert default recall hides superseded, invalidated, completed, historical, and future planned records.
- [ ] Assert as-of recall can see historical records valid at the requested instant.
- [ ] Assert back-compat records with no temporal timestamps remain current.
- [ ] Run: `rtk test cargo test --test temporal_recall`
- [ ] Expected red: missing temporal API/schema/fields.

### Task 2: CLI Smoke Tests

**Files:**
- Modify: `tests/cli_smoke.rs`
- Modify: `src/cli.rs`
- Modify: `src/protocol.rs`

- [ ] Add CLI smoke coverage for `codex-memoryd recall --as-of <timestamp>`.
- [ ] Add CLI smoke coverage for `codex-memoryd recall --include-history`.
- [ ] Run targeted smoke test and confirm red before implementation.

### Task 3: Storage Schema

**Files:**
- Add: `migrations/0010_temporal_records.sql`
- Modify: `src/domain.rs`
- Modify: `src/store.rs`
- Modify: `tests/schema_upgrade.rs`

- [ ] Add `TemporalState` and temporal fields to `MemoryRecord`.
- [ ] Add idempotent `ensure_temporal_columns`.
- [ ] Add temporal columns to `RECORD_COLS`, `row_to_record`, and insert defaults.
- [ ] Bump `STORAGE_SCHEMA_VERSION` from 8 to 9.
- [ ] Add store helpers for supersede/invalidate transitions.
- [ ] Keep existing `NewRecord` callers source-compatible by defaulting temporal columns from metadata or current values.

### Task 4: Recall Modes

**Files:**
- Modify: `src/recall.rs`
- Modify: `src/service.rs`
- Modify: `src/protocol.rs`

- [ ] Add recall request fields `as_of` and `include_history`.
- [ ] Add internal deterministic `now` support for tests.
- [ ] Add current/as-of/include-history predicates.
- [ ] Add withheld reasons `temporal_historical` and `temporal_planned`.
- [ ] Preserve `recall_not_authority` and existing trust/quarantine/secret gates.

### Task 5: Docs and Validation

**Files:**
- Modify: `docs/temporal-records.md`
- Modify: `README.md`
- Maybe modify: `docs/semantic-layer.md`

- [ ] Update docs from design-only to implemented slice.
- [ ] Document field meanings, default recall, as-of/history recall, migration behavior, and non-goals.
- [ ] Run `rtk cargo fmt --all --check`.
- [ ] Run targeted tests.
- [ ] Run `rtk git diff --check`.
- [ ] Run full `rtk test cargo test`.
- [ ] Request reviewer agent diff review before PR.
