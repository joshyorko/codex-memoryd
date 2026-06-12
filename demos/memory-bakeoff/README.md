# Memory bakeoff proof (Issue #16)

This folder contains a narrow proof for PR #32 work: what codex-memoryd can show today
without requiring live Honcho or external Codex binary behavior.

- Run script: `bash demos/memory-bakeoff/run-bakeoff.sh`
- Sample output: `demos/memory-bakeoff/sample-output.txt`
- Fixtures: `demos/memory-bakeoff/fixtures/memories/`

## 3-way comparison matrix (scope-limited)

| Dimension | local-only Codex memory | Honcho provider mode | codex-memoryd provider mode |
|---|---|---|---|
| Setup / dependency surface | Win: no daemon binary required; uses existing local memory files and Codex runtime config only. | Lose: requires network-access path and service credentials/configuration outside local defaults. | Win: add a local loopback service and one daemon binary; no cloud account required. |
| Source of truth | Local memory artifacts in Codex workspace; this is the only durable input/output path. | External provider is authoritative when configured. | Local durable store (`~/.codex-memoryd/memory.db`) becomes the explicit source for provider-backed recall. |
| Recall surface | Recall path is limited to local Codex memory artifacts and current Codex rules. | Recall is through Honcho-backed API semantics and network latency/protocol boundaries. | Recall is via `/v1/recall` on local loopback and is bounded by profile/workspace/query policy. |
| Import + supersession | Existing local imports are supported via provider-local logic; supersession behavior is provider-specific and not a general cross-system contract. | Depends on Honcho integration details at call time. | Import preview/apply + supersession are explicitly demonstrated in this harness (`/v1/sync/local-codex-memory`). |
| Profile boundary | Not a portability guarantee by itself; depends on local agent policy state in the workspace. | Depends on Honcho policy implementation and tenant config. | Explicitly asserted for export denial/workspace partition in this proof path. |
| Fail-open behavior | Fallback behavior depends on Codex runtime and provider setup for local-memory mode. | No hard guarantee documented in this local repo docs. | Explicit fail-open behavior is exercised: daemon-down is observed and local CLI remains usable. |
| Safety filtering | Safety is present where this repo’s provider policies apply; it is not guaranteed that every local source path is equivalent. | Depends on upstream Honcho policy and this repo can only pass through provider results. | Explicitly asserted in this proof: API-key pattern and hidden-reasoning markers are rejected by turns endpoint. |
| Portability | High for human-readable local artifacts; recall portability across machines depends on manual transfer/import. | Portable via Honcho remote store when configured. | Portable by design across machines with DB-backed durable storage and import/export tooling. |
| Out-of-scope limits | This proof does not re-verify full external Honcho API behavior. | This proof does not run live Honcho API calls or any Honcho credentials workflow. | This proof does not validate remote multi-host replication, multi-tenant hardening, or enterprise RBAC. |

## Scope of this harness

`run-bakeoff.sh` intentionally exercises codex-memoryd local assertions and fail-open behavior only:

- local loopback boot, first-recall readiness, status shape, and deterministic recall checks
- import preview/apply idempotency from fixtures
- safety rejections for token-like secrets and hidden-reasoning markers
- profile/workspace isolation boundaries in recall and export
- supersession and provenance fields in exported records
- daemon kill fail-open checks and CLI availability without daemon

Where this proof says “local-only,” “Honcho,” and “codex-memoryd,” it is only a documented comparison
matrix for this branch context. It is not a benchmark and does not rank every memory system.

## Deterministic artifacts

- `sample-output.txt` contains stable excerpts that correspond to script assertions,
  so readers can inspect intended outcomes without running an integration-heavy script.
- The live script output path is `OUT` from `run-bakeoff.sh`.

