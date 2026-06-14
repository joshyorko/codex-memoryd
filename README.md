# codex-memoryd

`codex-memoryd` is a local-first, agent-agnostic memory daemon for coding
agents. The durable store is the product; the daemon, CLI, Docker Compose
heartbeat, adapter exports, and MCP stdio are delivery modes. Memory is
recall, not authority: retrieved context may inform a turn, but it never
overrides current instructions, repository state, or policy.

## Current Surface

- loopback native daemon on `127.0.0.1:8787`
- Docker Compose dogfood with `.dogfood/memory.db` as the real daemon DB
- Compose heartbeat script for repeatable local verification
- MCP dogfood with a separate sandbox DB at `.dogfood/mcp-sandbox-memory.db`
- read-only Codex MCP tools: `memory_status`, `memory_recall`, `memory_search`
- local Codex memory import via `sync-local`
- git trailer import via `git-import`
- refs fixture import for JSON and JSONL exports
- subject / episode substrate
- current-state cards: `workspace_summary`, `subject_summary`,
  `active_preferences`, `open_questions`, `recent_scars`,
  `procedures_index`
- adapter exports for `agents-md`, `claude-code`, `copilot`,
  `github-instructions`, `markdown`, and `mcp-pack`
- adapter context packs for `agents-md`, `claude-code`, `copilot`, and
  `mcp-pack`
- recall policy metadata with `recall_not_authority`
- Dreamer preview/apply for evidence-backed consolidation

## Safety Model

- recall is `recall_not_authority`
- local-first and loopback-only are the default dogfood posture
- the Compose heartbeat keeps the published host port loopback-only
- preview happens before apply
- no auto-apply, no automatic prompt injection
- secrets, tokens, `.env` dumps, hidden reasoning, and raw confidential logs are rejected before durable write
- profile and workspace boundaries are enforced
- provider failures fail open instead of blocking Codex turns
- the read-only Codex MCP path exposes no write tools

## Dogfood

### Native Daemon

The safest local run is the loopback daemon backed by a persistent SQLite DB:

```bash
cargo build --release
mkdir -p .dogfood/logs .dogfood/exports

CODEX_MEMORYD_DB="$PWD/.dogfood/memory.db" \
CODEX_MEMORYD_BIND="127.0.0.1:8787" \
CODEX_MEMORYD_PROFILE="personal" \
CODEX_MEMORYD_WORKSPACE="josh-personal" \
target/release/codex-memoryd serve

curl -fsS http://127.0.0.1:8787/healthz
curl -fsS http://127.0.0.1:8787/v1/status | jq
```

Direct loopback keeps protected routes in the same-host trust boundary.

### Docker Compose Dogfood

Compose is useful for local smoke tests, but it is not the primary dogfood
surface. It binds `0.0.0.0` inside the container, publishes only
`127.0.0.1:8787` on the host, and keeps the real DB at `.dogfood/memory.db`.
That means `/v1/status` is available for diagnostics while protected `/v1/*`
routes report `auth_missing` in this mode.

```bash
mkdir -p .dogfood
docker compose up -d --build
curl -fsS http://127.0.0.1:8787/healthz
curl -fsS http://127.0.0.1:8787/v1/status | jq
docker compose down
```

#### Compose runtime heartbeat

Repo-local reproducible heartbeat:

```bash
scripts/dogfood-compose-heartbeat.sh
```

This rebuilds and restarts Compose, verifies container health and localhost-only
publish on `127.0.0.1:8787`, runs import preview/apply/idempotent second apply
against `/host-codex-memories`, refreshes `.dogfood/mcp-sandbox-memory.db`,
and does a raw `--read-only` MCP stdio tool canary.

### MCP Dogfood

The Codex-facing MCP runbook is intentionally read-only. It uses the sandbox DB
at `.dogfood/mcp-sandbox-memory.db` and exposes only:

- `memory_status`
- `memory_recall`
- `memory_search`

The real dogfood daemon remains on `.dogfood/memory.db` at `127.0.0.1:8787`.
No write tools are exposed to Codex in that config.

See [`docs/dogfood-mcp.md`](./docs/dogfood-mcp.md) for the exact `~/.codex/config.toml`
snippet and smoke checks.

## First Run

1. Build and start the native daemon.
1. Confirm `healthz` and `v1/status`.
1. Point Codex at the provider backend.
1. Import local memory with `sync-local --preview` before `--apply`.
1. Use `recall` as context, not authority.

## First-run path (source build)

```bash
cargo build --release
mkdir -p .dogfood/logs .dogfood/exports

CODEX_MEMORYD_DB="$PWD/.dogfood/memory.db" \
CODEX_MEMORYD_BIND="127.0.0.1:8787" \
CODEX_MEMORYD_PROFILE="personal" \
CODEX_MEMORYD_WORKSPACE="josh-personal" \
target/release/codex-memoryd serve
```

In another shell:

```bash
CODEX_MEMORYD_DB="$PWD/.dogfood/memory.db" target/release/codex-memoryd doctor
curl -fsS http://127.0.0.1:8787/v1/status | jq
target/release/codex-memoryd sync-local --preview ~/.codex/memories \
  --profile personal --workspace josh-personal
target/release/codex-memoryd sync-local --apply ~/.codex/memories \
  --profile personal --workspace josh-personal
target/release/codex-memoryd conclude --profile personal --workspace josh-personal \
  --content "Decision: keep codex-memoryd local-first."
target/release/codex-memoryd recall --profile personal --workspace josh-personal \
  --query "local-first memory"
```

Fail-open note: if `codex-memoryd` is unavailable, Codex provider/hybrid mode
should skip provider recall and continue the turn instead of blocking work.

Current Codex-side integration details live in
[`docs/codex-integration.md`](./docs/codex-integration.md) and
[`docs/dogfood-mcp.md`](./docs/dogfood-mcp.md).

## Core Commands

```bash
# Status and self-check
codex-memoryd status
codex-memoryd doctor

# Recall and search
codex-memoryd recall --profile personal --workspace josh-personal --query "how do we serve the provider"
codex-memoryd search --profile personal --workspace josh-personal --query "axum"

# Durable write
codex-memoryd conclude --profile personal --workspace josh-personal \
  --content "Decision: use rusqlite bundled for storage"

# Subjects and episodes
codex-memoryd subject create --profile personal --workspace josh-personal \
  --key workflow:dogfood-import --kind workflow --display-name "Dogfood import gate"
codex-memoryd episode create --profile personal --workspace josh-personal \
  --subject-id <subject-id> --source-kind fizzy_card --source-ref 491 \
  --summary "Container/import/MCP gate verified green."

# Cards and adapters
codex-memoryd card show --profile personal --workspace josh-personal --type workspace_summary
codex-memoryd adapter export --target agents-md \
  --profile personal --workspace josh-personal > AGENTS.memory.md

# Dreamer and patches
codex-memoryd dream --profile personal --workspace josh-personal --preview
codex-memoryd dream --profile personal --workspace josh-personal --apply
codex-memoryd patch preview --profile personal --workspace josh-personal
codex-memoryd patch apply --profile personal --workspace josh-personal --run-id <run-id>

# Local memory import
codex-memoryd sync-local --preview ~/.codex/memories
codex-memoryd sync-local --apply ~/.codex/memories

# Local Git evidence import
codex-memoryd git-import --preview /path/to/repo
codex-memoryd git-import --apply /path/to/repo
codex-memoryd git-import --preview --refs-fixture /path/to/refs.jsonl /path/to/repo
codex-memoryd git-import --apply --refs-fixture /path/to/refs.json /path/to/repo

# Export and forget
codex-memoryd export --profile personal --workspace josh-personal > backup.jsonl
codex-memoryd forget <record-id>
codex-memoryd forget <record-id> --delete

# MCP stdio
codex-memoryd mcp stdio --read-only
```

`git-import` scans recent local commits for explicit `Memory-*` trailers such
as `Memory-Decision`, `Memory-Verify`, and `Memory-Gotcha`. Apply mode writes
safe subject episodes and evidence ledger rows only; it does not promote Git
evidence to active memory records.

`--refs-fixture` accepts a JSON or JSONL export of PR, issue, or review-comment
evidence and runs the same preview/apply/idempotency flow without calling the
GitHub API.

## Memory Shapes

- `Card`: a compact summary surface for `workspace_summary`,
  `subject_summary`, `active_preferences`, `open_questions`, `recent_scars`,
  or `procedures_index`
- `Pack`: the recall envelope, including ranking, policy, citations, and truncation
- `Adapter`: a downstream export view such as `agents-md` or `mcp-pack`
- `Recall policy`: ranking and admission metadata that keeps recall contextual and safe
- `Evidence ledger`: the append-only provenance trail for writes, imports, and synthesis

Each current-state card is deterministic and renders as JSON or markdown from
the same store.

## Adapters

Current adapter targets on master:

- `agents-md` for `AGENTS.md`-style memory views, with `agents-md-v1`
  context packs
- `claude-code` for Claude memory views, with `claude-code-v1` context packs
- `copilot` for Copilot instructions views, with `copilot-v1` context packs
- `github-instructions` for GitHub instructions views
- `markdown` for plain markdown memory views
- `mcp-pack` for deterministic JSON context packs, with `mcp-json-v1`

`agents-md`, `claude-code`, `copilot`, and `mcp-pack` currently carry context
packs. The other adapters stay markdown-forward.

## Core Model

- `Subject`: the stable identity of what the memory is about
- `Episode`: an append-only evidence event attached to a subject
- `Evidence ledger`: the append-only audit trail explaining why a subject or
  projection exists, changed, or was rejected
- `Projection`: the compact durable memory record produced from evidence

This substrate is intentionally not a graph engine, CRM, scheduler, or agent
harness. Export remains record-centric for now; subjects and episodes are
internal anchors, not a standalone exported surface.

See [`docs/agent-agnostic-memory-substrate.md`](./docs/agent-agnostic-memory-substrate.md)
for the fuller substrate plan.

## Dreamer and Evidence

Dreamer is the background synthesis lane. It consolidates safe evidence from
visible turns, conclusions, checkpoints, imported local memories, and active
records into durable candidate memories.

Current rules:

- preview first, apply second
- evidence is asymmetric: user turns and conclusions are strong; assistant
  turns are weak unless adopted; imported memories are corroborating only
- drift-prone facts can be demoted or rewritten
- apply is idempotent and policy-gated
- ledger writes are append-only and do not store raw rejected secrets or hidden
  reasoning

Patch preview/apply/rollback is landed as a reviewable Dreamer patch lifecycle.
Broader procedural patch automation remains a roadmap lane.

See:

- [`docs/evidence-ledger.md`](./docs/evidence-ledger.md)
- [`docs/dreamer-loop-research.md`](./docs/dreamer-loop-research.md)
- [`docs/dreamer-loop-design.md`](./docs/dreamer-loop-design.md)

## Git Import

`git-import` is the local evidence bridge for commit trailers and exported refs
fixtures.

- commit trailers: scans recent local commits for `Memory-Decision`,
  `Memory-Verify`, and `Memory-Gotcha`
- refs fixtures: accepts JSON or JSONL exports of PR, issue, and review-comment
  evidence
- safety: preview/apply/idempotency are the same path in both modes
- output: safe subject episodes plus evidence ledger rows only

## Roadmap by Issue Lane

Current landed surfaces are MVP/additive, not the end state. Open work still
lives on the board:

| Lane | Theme | Status |
| --- | --- | --- |
| `#50` | current-state cards | landed MVP surface; follow-on polish remains |
| `#53` | eval suite | open on the board; still validating capability gates, recall, and context-pack behavior |
| `#55` | recall policy | landed metadata plus `recall_not_authority`; hardening remains |
| `#56` | adapter context packs | landed for `agents-md`, `claude-code`, `copilot`, and `mcp-pack`; broader cleanup remains |
| `#57` | git import | landed for local trailers and refs fixtures; more cases remain |
| `#70` | adapter conformance | open validation lane |

## Build and Test

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

## Related Docs

- [`docs/dogfood-local.md`](./docs/dogfood-local.md)
- [`docs/dogfood-mcp.md`](./docs/dogfood-mcp.md)
- [`docs/codex-integration.md`](./docs/codex-integration.md)
- [`docs/evidence-ledger.md`](./docs/evidence-ledger.md)
- [`docs/agent-agnostic-memory-substrate.md`](./docs/agent-agnostic-memory-substrate.md)
- [`demos/memory-bakeoff/README.md`](./demos/memory-bakeoff/README.md)

## License

MIT.
