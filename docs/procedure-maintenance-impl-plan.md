# Procedure maintenance — implementation plan (#158)

> **Status: PLAN ONLY. Do not start coding yet.** This plan is **gated on #154**:
> begin implementation only after the #154 relation slice has landed or its
> schema is confirmed stable, so the two efforts agree on one combined
> `STORAGE_SCHEMA_VERSION` bump and one migration generation rather than racing
> two. Until then this is the spec; the design rationale lives in
> `docs/procedure-maintenance.md` and the behavior fixtures in
> `tests/fixtures/procedure_maintenance/`.

This is the build order for whoever implements #158 (Codex or root). It is
grounded in the **current** code (verified on master at schema v7): the
procedure substrate already has `insert_or_get_procedure`, `query_procedures`,
`get_procedure`, `retire_procedure`, `supersede_procedure`,
`record_procedure_counter_evidence`, `validate_procedure`,
`count_procedures_by_state` in `src/store.rs`, and the lifecycle columns from
migrations 0007/0008 (`state`, `version`, `first_seen`, `last_validated`,
`superseded_by`, `counter_evidence_count`, `negative_examples`).

## Hard constraints (carry through every step)

- **Preview-only.** No automatic apply. Maintenance produces a report and
  candidate actions; apply is a separate, explicit step (mirror `procedures
  apply` / `patch apply`).
- **No false resurrection.** A `retired`/`superseded` procedure must never be
  moved back into default recall by maintenance. (Regression hook + fixture.)
- **Merge unions negative examples.** A merge candidate's result must contain
  the union of both parents' `negative_examples` and `source_episode_ids`.
- **Promote re-runs the unsafe guard.** Promotion to `active` must re-run the
  same `unsafe_content` validation that `procedures apply` uses; a procedure
  that would fail apply cannot be promoted.
- Do **not** touch retrieval relation code (#154), Async Dreamer (#157), graph,
  or embeddings.
- All new contract surfaces are **additive** under `docs/compatibility-policy.md`.

## Build order

### Step 0 — gate check (do this first)
Confirm #154's relation slice has landed or its migration generation is fixed.
Agree the combined schema version: if #154 bumps 7→8, maintenance is 8→9 (or a
single coordinated bump if both land together). Reference
`codex_memoryd::store::STORAGE_SCHEMA_VERSION` in every test — never a literal
(this broke 4 tests in PR #160; don't repeat it).

### Step 1 — schema slice (smallest reviewable change)
- Add `migrations/00NN_procedure_maintenance.sql` as a **no-op marker**
  (comment only), exactly like `0008_procedure_lifecycle.sql`.
- Add `ensure_procedure_maintenance_columns(conn)` in `src/store.rs`, called
  from `migrate()` right after `ensure_procedure_lifecycle_columns`. It
  `ensure_column`s:
  - `success_count INTEGER NOT NULL DEFAULT 0`
  - `reuse_count INTEGER NOT NULL DEFAULT 0`
  - `last_used_at TEXT` (NULL)
- Bump `STORAGE_SCHEMA_VERSION`; update `tests/fixtures/status.response.json`;
  extend `tests/schema_upgrade.rs` with a new-generation case proving a
  pre-maintenance procedures row upgrades losslessly (counts default to 0,
  `last_used_at` NULL).
- Add the three columns to `PROCEDURE_COLS` and `row_to_procedure`, and to the
  `Procedure` domain struct + `insert_or_get_procedure` (mirror exactly how the
  0008 lifecycle columns were threaded — same pattern, same places).
- **Defer** the `procedure_score_events` table. Only add it if Step 3's
  `confidence_trend` fixture proves per-event history is needed. Counters first.

### Step 2 — scoring module (`src/proc_maint.rs`, pure + deterministic)
- A `procedure_score(procedure, now, weights) -> ProcedureScore` function:
  components = reuse, success_rate (`success_count / (success_count +
  failure_count)`), evidence (`source_episode_ids.len()`), confidence, minus
  staleness penalty (`now - last_validated`/`last_used_at`) and failure
  (`counter_evidence_count`). Weights from config with documented defaults.
- `ProcedureScore` carries the **per-component breakdown** so a low score is
  explainable (the report shows it).
- Unit tests in-module: ordering, boundary (no successes/failures → defined
  behavior), staleness monotonicity. No DB, no model.

### Step 3 — maintenance report + candidate actions (preview only)
- `Service::procedure_maintenance(profile, workspace) -> MaintenanceReport`:
  scores all in-scope procedures, then derives **candidate** actions:
  - `retire` — stale + low reuse
  - `quarantine` — `counter_evidence_count` ≥ threshold
  - `promote` — `quarantined`/`candidate` with strong success_rate + evidence
    (marks `requires_guard: unsafe_content_recheck`)
  - `merge` — two same-subject procedures with token-similar
    `activation_query`+`steps` (reuse the `activation` module tokenizer;
    threshold in config); names the keep/supersede ids and the **unioned**
    negative_examples + source_episode_ids
  - `split` — one procedure whose `source_episode_ids` activation intents are
    disjoint (activation matcher fires on two query families)
- Report is **content-free** beyond procedure names (already non-secret); no raw
  episode content. JSON + summary, `version: 1`.
- **No apply in this step.** Returning candidates is the whole deliverable.

### Step 4 — apply path (separate, explicit)
- `Service::procedure_maintenance_apply(action)` dispatches one reviewed action:
  - retire → existing `retire_procedure`
  - quarantine → existing `record_procedure_counter_evidence` semantics
  - promote → new candidate→active transition that **re-runs the unsafe guard**
    (`validate_procedure_candidate`/`unsafe_procedure_reasons`); reject if unsafe
  - merge → keep higher-scored, `supersede_procedure(loser → winner)`, write the
    unioned `negative_examples`/`source_episode_ids` onto the winner
  - split → propose two new candidates, retire the original on apply
- Every apply is one explicit call; never batched-auto. Mirror how `procedures
  apply` validates scope and rejects with reasons.

### Step 5 — CLI + recall counters
- `codex-memoryd procedure maintenance [--format json|summary]` (report) and a
  guarded apply subcommand. Mirror the existing `ProcedureCommand` arms.
- **The one retrieval touch, flagged for explicit approval:** bump `reuse_count`
  and set `last_used_at` when `procedures_recall` activates a procedure. This is
  a single increment in the recall path, behind the maintenance feature. Do it
  here, in the impl PR, with its own test — not silently earlier.

### Step 6 — eval metrics + safety hooks
- Extend `proc_eval` (#150) with maintenance metrics: `merge_precision`,
  `false_resurrection_rate` (must be 0), `unsafe_promotion_rate` (must be 0).
- Wire the 5 merged fixtures (`tests/fixtures/procedure_maintenance/`) into a
  new `tests/procedure_maintenance.rs`:
  - `score_ranking` → healthy scores above stale/failed
  - `merge_candidate` → one merge candidate, unioned negatives asserted
  - `retire_stale` → retire candidate, **not** auto-applied
  - `promote_recovered` → promote candidate re-runs the unsafe guard
  - `no_false_resurrection` → high-scoring retired procedure never resurrected
- Add a contract snapshot for the `MaintenanceReport` shape in
  `tests/contract_snapshots.rs`.

## Validation (every PR)
`cargo fmt --all --check`, `git diff --check`, `cargo clippy --all-targets`,
full `cargo test`. Keep fixtures synthetic; no secrets; no unsafe content in
report output.

## Suggested PR slicing (small, reviewable)
1. **Schema slice** (Step 1) — columns + migration + upgrade test. Tiny.
2. **Score + report** (Steps 2–3) — preview-only, with score/merge/retire
   fixtures wired. No apply.
3. **Apply + CLI + counters + eval hooks** (Steps 4–6) — the mutating surface,
   with all safety-hook fixtures green.

## Open questions for root (answer before Step 1)
1. Combined vs. separate schema bump with #154 — depends on landing order.
2. `confidence_trend`: counters-only vs. `procedure_score_events` table.
   Recommendation: counters; add the table only if a fixture forces it.
3. Merge/split similarity threshold home (config key + default).
4. Is the recall-path `reuse_count` increment (Step 5) approved? It is the only
   retrieval touch in the whole feature and is otherwise out of scope.
