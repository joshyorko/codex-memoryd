# Eval benchmark operations plan (#190, groundwork for #189 and #159)

Status: first-wave design. This plan builds on PR #180's disabled-by-default hybrid retrieval eval prototype without promoting it to production recall.

## Current eval surface

`codex-memoryd eval retrieval` is deterministic, fixture-backed, and currently accepts only `--format`. It loads the checked-in long-history fixture, runs local baselines, includes the `hybrid_sparse_fusion` experiment from PR #180, and emits JSON/summary reports with retrieval scores, context bytes, estimated tokens, ablations, improvements, recommendations, and notes.

The hybrid prototype is local sparse-hash + reciprocal-rank fusion, disabled by default, and must remain eval-only unless a later tranche explicitly changes production recall behavior.

## #190 goals

- Add subset/limit controls before any public benchmark runner.
- Add dry-run cost estimation before any model/provider path exists.
- Separate retrieval-only scoring from answer-generation scoring.
- Support no-LLM retrieval checks in CI.
- Add cache/artifact vocabulary keyed by dataset version, config hash, and commit SHA.
- Require explicit flags for full datasets/runs.
- Write partial reports for failed/interrupted runs.

## CLI proposal

Start with additive flags on `eval retrieval`:

```bash
codex-memoryd eval retrieval --format json --limit 5
codex-memoryd eval retrieval --format json --subset temporal_updates
codex-memoryd eval retrieval --format json --dry-run-cost
codex-memoryd eval retrieval --format json --report-out target/memoryd-evals/retrieval.json
```

Public benchmark runners should later use a separate namespace, for example:

```bash
codex-memoryd eval benchmark locomo --dataset ./datasets/locomo --limit 10 --retrieval-only
codex-memoryd eval benchmark locomo --dataset ./datasets/locomo --full --allow-provider-calls
```

## Report schema additions

Keep existing fields and add optional top-level objects:

- `selection`: fixture/dataset id, subset names, limit, selected question ids, skipped counts, full-run flag.
- `cost_budget`: estimated input/output tokens, provider/model if any, estimated cost, hard limits, dry-run flag.
- `cache`: cache key, cache root, hit/miss counts, artifact paths, dataset version, config hash, commit SHA.
- `artifacts`: JSON/markdown paths and partial-report status.
- `execution`: wall time, interrupted flag, errors summarized without private content.

## Cache rules

- Do not persist normal recall cards; README already treats cards as on-demand.
- Eval artifact caches are local, opt-in, and safe to delete.
- Cache keys include dataset version, eval config, code commit, and mode.
- Retrieval caches and answer-generation caches are separate.

## Public benchmark and bakeoff unlocks

- #189 can start once the neutral input/report shape supports tiny local subsets and retrieval-only mode.
- #159 can start after #189 defines benchmark inputs and provider adapter contracts.
- External systems and providers must be skipped unless explicitly configured by the operator.
- CI remains synthetic-fixture-only and external-service-free.

## Tests for future implementation

- CLI smoke for `--limit`, `--subset`, `--dry-run-cost`, and report-out.
- Deterministic JSON double-run.
- No-LLM retrieval-only scoring.
- Full-run requires explicit flag.
- Partial report is written on controlled failure.
- PR #180 hybrid fields remain present and unchanged unless intentionally versioned.
