# Async Dreamer v2

Async Dreamer jobs are an explicit execution seam for bounded Dreamer preview
runs. They do not start a background worker, apply patches, promote candidates,
or write durable memory. The existing deterministic Dreamer path remains the
default and does not require a model or network access.

## Job modes

`kind` is currently restricted to `dream_preview`. `mode` selects one reviewed
adapter:

- `deterministic` (default): runs the existing deterministic preview engine.
  Any legacy `provider.command.argv` value is retained as inert data and is
  never executed.
- `local-model`: calls an explicitly supplied or configured loopback HTTP(S)
  endpoint. The daemon never downloads a model.
- `provider`: calls an explicitly supplied or enabled remote HTTP(S) provider.
  This mode is opt-in and may incur cost.

Model-backed jobs use the typed `dream-preview-v1` JSON envelope. The HTTP
adapter accepts the envelope directly or inside an OpenAI-compatible chat
response. Shell commands, shell interpolation, arbitrary subprocesses, and
provider calls during startup or status checks are not part of this boundary.

## Runtime setup

Provider credentials are runtime-only configuration. They are read from
`CODEX_MEMORYD_PROVIDER_API_KEY` and are never accepted in a job provider
object, persisted job/run JSON, audit metadata, candidate provenance, logs, or
responses. A minimal local setup is:

```text
CODEX_MEMORYD_PROVIDER_ENABLED=true
CODEX_MEMORYD_PROVIDER_ADAPTER=local-model
CODEX_MEMORYD_PROVIDER_ENDPOINT=http://127.0.0.1:8080/v1
CODEX_MEMORYD_PROVIDER_MODEL=my-local-runtime
```

Remote setup uses `CODEX_MEMORYD_PROVIDER_ADAPTER=provider`, an HTTPS endpoint
and an explicit model. Inline URL credentials are rejected. A request may
provide an endpoint/model/provider override, but a remote request still needs
an explicit provider mode and a positive `max_cost_micros` budget. Local
endpoints must resolve to loopback addresses.

The provider configuration also supports:

- `CODEX_MEMORYD_PROVIDER_TIMEOUT_SECONDS`
- `CODEX_MEMORYD_PROVIDER_MAX_RESPONSE_BYTES`
- `CODEX_MEMORYD_PROVIDER_NAME`
- `CODEX_MEMORYD_PROVIDER_COST_PER_1K_INPUT_MICROS`
- `CODEX_MEMORYD_PROVIDER_COST_PER_1K_OUTPUT_MICROS`
- `CODEX_MEMORYD_PROVIDER_DAILY_COST_CEILING_MICROS`

No configuration above causes a call by itself. Deterministic jobs continue to
work when all provider configuration is absent.

## Budgets and cost

Every job carries a typed budget:

- `max_runtime_seconds`
- `max_input_records`
- `max_candidates`
- `max_input_tokens` and `max_output_tokens`
- `max_input_bytes` and `max_output_bytes`
- `max_provider_calls` and `max_retries`
- `max_cost_micros`
- optional `daily_cost_ceiling_micros`

Zero leaves token/byte limits unconstrained; zero provider calls uses one
compatibility call, and zero retries disables retries. A remote provider job
must set a positive per-run cost ceiling. The
adapter enforces timeout, response-size, input/output, call, retry, and cost
limits before returning candidates. A configured daily ceiling is a rolling
24-hour ceiling across audited provider runs; a replay of the same bounded run
does not double-count its existing audit row. Exhaustion produces a terminal
error audit and no durable-memory write.

## Trust boundary and candidate review

Provider output is untrusted input. The daemon:

1. Requires the `dream-preview-v1` response schema and matching
   profile/workspace/repository scope.
2. Requires each proposed candidate to use a supported record type, a screened
   content/subject/action/state value, and evidence references from the job's
   evidence window.
3. Applies deterministic policy classification and clamps provider confidence.
4. Marks model-backed candidates `recall_not_authority`,
   `provider_generated`, `provider_preview`, and `apply_eligible=false`.
5. Adds provider/model/adapter version, request hash, and input hash
   provenance to the preview.

Model-backed runs call only the deterministic Dreamer preview engine and the
typed adapter. They do not call Dreamer apply, scheduled apply, patch apply,
promotion, supersession, archival, or direct memory-write paths. Candidate
`supersedes`/`retires` values are accepted only when they are evidence
references, so they remain review metadata rather than mutation commands.

Review candidates through the existing Dreamer preview and memory-patch
workflow. Applying a patch is a separate, explicit operator action; provider
output is never authority for that action.

## Auditing and status

`dream_jobs` records the requested mode, scope, typed budget, safe provider
identity, status, last run, and sanitized failure. `dream_runs` records the
source window, candidate/policy outcomes, provider/model/adapter provenance,
request/input hashes, and budget usage (records, tokens, bytes, calls,
retries, candidates, and cost). No credential is included in either row.

`/v1/status` reports the effective mode, scheduler enabled/active state,
preview-only capability, local/provider configured state, and local/provider
readiness. Readiness requires the endpoint and model; local readiness also
requires a loopback endpoint. Status performs no provider call and exposes only
safe budget/cost summaries.

There is intentionally no Async Dreamer background loop or automatic retry
lifecycle. A caller explicitly invokes the job execution seam, and then
reviews the resulting bounded preview and audit record.

## Failure behavior

Malformed JSON or schema, scope mismatch, content-policy rejection, timeout,
HTTP failure, response-cap exhaustion, cancellation/runtime exhaustion, and
token/call/retry/cost budget exhaustion terminate safely. Errors are bounded
and sanitized before they are returned or persisted. Failed runs still have a
`dream_runs` error audit and a terminal `dream_jobs` status; durable memory
remains unchanged.
