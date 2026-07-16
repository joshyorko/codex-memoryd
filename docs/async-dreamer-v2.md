# Async Dreamer v2 MVP

Status: first-PR design for issue #157. This slice adds an explicit local job
primitive and keeps execution deterministic, preview-only, and bounded.

## Goals

- Define a durable job record for local Dreamer reasoning runs.
- Reuse existing Dreamer preview logic and existing `dream_runs` audit rows.
- Make budgets explicit in request/job data before any cloud or local-model work.
- Keep all outputs previewable candidates and patches. No silent apply.

## Non-goals in this PR

- No background worker loop.
- No automatic apply.
- No provider command execution.
- No MCP or broad CLI wiring.
- No dependency on eval work (#189) or runtime worker work (#183).

## Job model

The long-term job taxonomy for Async Dreamer v2 is:

- `summarize_evidence_window`
- `detect_conflicts`
- `propose_relations`
- `propose_temporal_transitions`
- `propose_procedures`
- `compact_cards`
- `compact_packs`

The MVP executable job shape is intentionally narrow:

- `kind`: only `dream_preview`
- `mode`: only `deterministic`
- `budget.max_runtime_seconds`
- `budget.max_input_records`
- `budget.max_candidates` (preview output-size cap in candidate units; each
  proposal or policy rejection consumes one unit, while stale evidence notices
  do not)
- `provider.command.argv` as stored config/data only

Jobs persist in `dream_jobs`. The row stores the explicit budget/provider shape,
scope, last run id, last run time, and last error. This creates an audit seam
for later schedulers or explicit CLI commands without introducing background
writes now.

## Run model

Runs reuse the existing `dream_runs` table instead of inventing a second run
ledger. A job run:

1. Validates scope, timestamps, and deterministic-only constraints.
2. Persists/updates the `dream_jobs` row as `running`.
3. Calls the existing Dreamer preview engine with bounded `max_records` and
   `max_candidates`.
4. Stores the resulting Dreamer audit row in `dream_runs`.
5. Updates the job row with `last_run_id`, `last_run_at`, final status, and any
   safe error summary.

Failure outcomes also append an `error` run in `dream_runs` (same scope/window)
so operators can audit failed attempts alongside successful preview passes.

This keeps run evidence aligned with current preview/apply reporting and avoids
new hidden storage behavior.

## Safety constraints

- Deterministic only in this MVP.
- Provider command data is persisted but never executed.
- Preview only: no memory writes from job execution.
- Existing evidence refs, candidate previews, and patch/apply workflow remain
  unchanged.
- Input and output remain bounded by explicit job budgets.

## Failure states

- Invalid `kind`
- Invalid `mode`
- Invalid RFC3339 `now`
- Invalid RFC3339 `since`
- Zero `max_input_records`
- Zero `max_candidates`
- Dreamer preview failure from existing store/policy/runtime paths

Failures update the job row with `status=error` and `last_error`. They do not
apply memory mutations.

## Follow-up path

- Add config-backed default job templates if the operator wants named jobs.
- Add an explicit CLI surface only after the job/status contract is reviewed.
- Add optional local-model or provider execution behind separate design review,
  disabled by default, still preview-only, and still bounded by explicit
  budgets.
