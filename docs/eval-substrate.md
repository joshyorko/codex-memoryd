# Substrate Eval Suite

`codex-memoryd eval substrate` is the deterministic, model-free eval gate for
the agent-agnostic memory substrate. It is intended for CI, PR review, and local
regression checks. The suite seeds a private in-memory fixture store and runs
real service-layer paths rather than a hosted benchmark or model-as-judge flow.

## Run

```bash
codex-memoryd eval retrieval --format summary
codex-memoryd eval retrieval --format json
cargo run -- eval substrate --format summary
cargo run -- eval substrate --format json
```

`--format summary` prints a human-readable report for operators. `--format json`
prints stable machine-readable output for CI and PR artifacts.

## Retrieval Eval Loop (Issue #153)

`codex-memoryd eval retrieval` is the dedicated command for retrieval quality checks.

```bash
codex-memoryd eval retrieval --format summary
codex-memoryd eval retrieval --format json
```

The long-history fixture is checked in at
`tests/fixtures/retrieval/long_history.json`. It covers single-hop, temporal,
contradiction, preference drift, multi-hop-ish, and open-domain questions.

Each retrieval run should include:

- raw chronological, keyword search, full-list, current memoryd recall,
  context-pack, and verbatim-evidence baselines
- explicit ablations for recency, type weight, evidence coverage,
  subject/episode match, procedure/valence, and freshness
- failed query ids that can become regression fixtures

`eval retrieval` remains deterministic. It reports checked-in fixture scores and
next ranking recommendations, not hosted-benchmark claims.

## Benchmark Runner Foundation (Issue #189)

`codex-memoryd eval benchmark synthetic` is the first neutral benchmark-runner
surface. It stays local-only and provider-free.

```bash
codex-memoryd eval benchmark synthetic --subset temporal --format summary
codex-memoryd eval benchmark synthetic --full --format json
codex-memoryd eval benchmark synthetic --input ./datasets/local-benchmark.json --full --format json
```

The default corpus lives at
`tests/fixtures/benchmark/synthetic_memory_v1.json`. It uses a neutral JSON
shape with:

- dataset metadata (`id`, `version`, `adapter`, `case_count`)
- cases with `history`, `question`, `expected`, and `metadata`

Full public datasets remain optional and out of CI. Operators can convert a
small local subset into this shape and pass it via `--input` without changing
production recall behavior or enabling provider calls.

## Current MVP Coverage

The first vertical slice covers the issue #53 safety and substrate gates that
already have real implementation paths:

- fact recall through the normal recall/ranking/packing path
- evidence coverage through recall citations
- cross-profile bleed via denied work-to-personal export
- poison rejection via the normal write-policy secret gate
- patch preview/apply/rollback through the Dreamer patch lifecycle
- procedural memory preview/apply/recall through the subject and episode APIs
- adapter/context-pack status through the `mcp-pack` adapter export
- pack cost in bytes and rough tokens
- valence utility signal by checking debugging pack-mode ranking metadata

The report includes fixture family names for the broader suite shape:
`fact_recall`, `temporal_updates`, `contradiction_supersession`,
`battle_scar_recovery`, `procedure_induction`, `patch_preview_apply_rollback`,
`cross_profile_bleed`, `poison_intake`, and
`adapter_exports_context_packs`.

## JSON Shape

The JSON output has stable top-level fields:

- `suite`, `version`, and `status`
- `fixture_families`
- `metrics`
- `checks`
- `triage`

`triage` is empty on pass. On failure it contains stable check names and
reviewable messages suitable for a PR artifact.

## Boundaries

The suite does not read or write the operator's configured database. It creates
an in-memory fixture store on every run, so it is deterministic and safe to run
in CI. It does not claim benchmark quality beyond the checked-in fixture output
and does not depend on external services.
