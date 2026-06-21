# Dreamer worker planning (#192)

Status: design-only first wave. This plan keeps current preview/apply behavior unchanged, makes deterministic operation the default, and forbids hidden model calls or auto-apply.

## Current implementation

Dreamer already supports manual `dream --preview` and `dream --apply`, patch preview/apply with run-id binding, an HTTP `/v1/dream` path, persisted dream-run audit/watermark rows, and an optional in-process scheduler. Status exposes last Dreamer run and scheduler state.

The current Dreamer engine is deterministic and policy/store-backed; it does not call an LLM. Scheduled runs are disabled by default and gated by interval, idle window, minimum session age, minimum turn count, max batch size, max candidates, and max runtime.

## Worker modes

| Mode | Default | Cost | Allowed in CI | Contract |
| --- | --- | --- | --- | --- |
| `deterministic` | Yes | Free/local | Yes | Existing heuristic Dreamer. No model calls. Output feeds preview/apply. |
| `local-model` | No | Local resources | No by default | Optional later path. Must not require a model download; status shows model path/readiness. |
| `provider` | No | Potential paid calls | No | Explicit provider/model/API-key readiness only; no hidden calls and no CI calls. |

## Lifecycle proposal

- `dream worker status` reports enabled state, mode, paid-provider readiness, last run, last error, watermark, next eligible run, limits, and whether apply is automatic (`false`).
- `dream worker enable --mode deterministic` is the only first implementation candidate.
- `dream worker run --preview` can force one bounded preview cycle.
- `dream worker run --apply` should remain explicit and should use the same policy gates and run-id/patch semantics as current apply paths.
- Multiple daemons sharing one DB require a lease or idempotency story before background apply is considered.

## Safety requirements

- No hidden provider calls.
- No hidden local model downloads.
- No automatic apply.
- Generated worker output is advisory and must preserve source refs.
- Recall remains background context, not authority.
- Status must make paid-provider configuration visible.
- Non-loopback/no-auth exposure continues to degrade status.

## Open issues before implementation

- Whether preview runs should persist dream-run audit rows; current code does, older design prose said preview writes nothing.
- Whether scheduled runs should ever apply directly or always produce preview artifacts.
- How to represent worker leases if native runtime, container runtime, and dev Compose all point at one DB.
- Whether MCP should expose worker status/run commands or remain read-only by default.

## Test plan for future code

- Scheduler disabled/enabled status.
- Deterministic mode makes no provider calls.
- Idle/short-session skips.
- Runtime/candidate/batch limits.
- Preview/apply run-id mismatch protection.
- Repeated apply idempotency.
- Multi-instance duplicate-run simulation.
- Provider configured vs provider active status without making a network call.
