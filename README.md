# codex-memoryd

`codex-memoryd` is an agent-agnostic, local-first memory substrate for coding
agents. The daemon, CLI, Docker Compose, and MCP stdio surfaces are delivery
modes; the durable model is the point. Memory is recall, not authority:
retrieved context may inform a turn, but it never overrides current
instructions, repository state, or policy.

Current landed shape:

- loopback native daemon on `127.0.0.1:8787`
- Docker Compose dogfood with `.dogfood/memory.db` as the real daemon DB
- MCP dogfood with a separate sandbox DB at `.dogfood/mcp-sandbox-memory.db`
- read-only Codex MCP tools: `memory_status`, `memory_recall`, `memory_search`
- local Codex memory import via `sync-local`
- git trailer import via `git-import`
- cards, packs, adapters, recall policy, and evidence ledger
- subject / episode substrate for stable anchors
- Dreamer preview/apply for evidence-backed consolidation

Docs worth skimming first:

- [`docs/dogfood-local.md`](./docs/dogfood-local.md)
- [`docs/dogfood-mcp.md`](./docs/dogfood-mcp.md)
- [`docs/codex-integration.md`](./docs/codex-integration.md)
- [`docs/evidence-ledger.md`](./docs/evidence-ledger.md)
- [`docs/agent-agnostic-memory-substrate.md`](./docs/agent-agnostic-memory-substrate.md)
- [`docs/dreamer-loop-research.md`](./docs/dreamer-loop-research.md)
- [`docs/dreamer-loop-design.md`](./docs/dreamer-loop-design.md)

## What It Owns

- durable records, sources, checkpoints, conclusions, turns
- stable subjects and episodes
- generated cards, packed recall, and adapter exports
- local Codex memory import and git trailer import
- recall policy and safety classification
- export
- evidence ledger entries
- Dreamer preview/apply

## What It Does Not Own

- agent execution, approvals, or prompt assembly
- hidden reasoning storage
- secrets, auth tokens, or `.env` dumps
- generic workflow orchestration
- a dashboard or vector DB requirement
- automatic work/personal memory merging

## Safety Invariants

- recall is `recall_not_authority`
- local-first and loopback-only are the default dogfood posture
- preview happens before apply
- no auto-apply, no automatic prompt injection
- secrets, tokens, and `.env` dumps are rejected before durable write
- profile and workspace boundaries are enforced
- provider failures fail open instead of blocking Codex turns
- the read-only Codex MCP path exposes no write tools

## Landed Surfaces

### Native Dogfood Daemon

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

The daemon is intended for local-only use. Direct loopback keeps `/v1/*`
protected routes in the same-host trust boundary.

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
against `/host-codex-memories`, refreshes `.dogfood/mcp-sandbox-memory.db`, and
does a raw `--read-only` MCP stdio tool canary.

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

## Quickstart

1. Build and start the native daemon.
1. Confirm `healthz` and `v1/status`.
1. Point Codex at the provider backend.
1. Import local memory with `sync-local --preview` before `--apply`.
1. Use `recall` as context, not authority.

## Memory Shapes

- `Card`: a compact summary surface for a subject or workspace.
- `Pack`: the recall envelope, including ranking, policy, citations, and truncation.
- `Adapter`: a downstream export view such as `agents-md`.
- `Recall policy`: ranking and admission metadata that keeps recall contextual and safe.
- `Evidence ledger`: the append-only provenance trail for writes, imports, and synthesis.

### First-run path (source build)

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

Minimal codex-side memory config:

```toml
[memories]
backend = "provider"              # local | provider | hybrid
provider = "codex_memoryd"        # honcho | codex_memoryd
provider_url = "http://127.0.0.1:8787"
profile = "personal"
workspace = "josh-personal"
local_import_policy = "prompt"    # prompt | manual | startup_preview | startup_apply
write_policy = "visible_turns"    # off | visible_turns
sync_policy = "manual"            # manual | startup
cross_profile_policy = "default_deny"
```

`local` preserves upstream Codex behavior. `provider` and `hybrid` select the
portable provider path; if `codex-memoryd` is unavailable, Codex must fail open.

## Core Model

- `Subject`: the stable identity of what the memory is about.
- `Episode`: an append-only evidence event attached to a subject.
- `Evidence ledger`: the append-only audit trail explaining why a subject or
  projection exists, changed, or was rejected.
- `Projection`: the compact durable memory record produced from evidence.

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

The broader patch / rollback lifecycle lives in the substrate roadmap docs. In
this repo, Dreamer preview/apply and the evidence ledger are the landed pieces;
general patch lifecycle is still a roadmap lane.

See:

- [`docs/evidence-ledger.md`](./docs/evidence-ledger.md)
- [`docs/dreamer-loop-research.md`](./docs/dreamer-loop-research.md)
- [`docs/dreamer-loop-design.md`](./docs/dreamer-loop-design.md)

## CLI

The CLI opens the store directly, so it works without a running daemon.

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

# Dreamer preview/apply
codex-memoryd dream --profile personal --workspace josh-personal --preview
codex-memoryd dream --profile personal --workspace josh-personal --apply

# Local memory import
codex-memoryd sync-local --preview ~/.codex/memories
codex-memoryd sync-local --apply ~/.codex/memories

# Local Git evidence import
codex-memoryd git-import --preview /path/to/repo
codex-memoryd git-import --apply /path/to/repo

# Export and forget
codex-memoryd export --profile personal --workspace josh-personal > backup.jsonl
codex-memoryd adapter export --target agents-md \
  --profile personal --workspace josh-personal > AGENTS.memory.md
codex-memoryd forget <record-id>
codex-memoryd forget <record-id> --delete
```

`git-import` scans recent local commits for explicit `Memory-*` trailers such as
`Memory-Decision`, `Memory-Verify`, and `Memory-Gotcha`. Apply mode writes safe
subject episodes and evidence ledger rows only; it does not promote Git evidence
to active memory records.

## Config and Compatibility

`codex-memoryd` is the provider side of the portable memory contract. The
Codex fork is the client side. The compatibility target is
`joshyorko/codex@tap-release`; that branch is the adapter target, not the
source of truth for this repo.

Canonical Codex-side memory shape:

| `backend` | `provider` | Durable store | Provider target | Local memory role |
| --- | --- | --- | --- | --- |
| `local` | — | upstream local only | — | source of truth |
| `provider` | `honcho` | Honcho | Honcho base URL | import source only |
| `provider` | `codex_memoryd` | codex-memoryd SQLite | `provider_url` → `/v1` | import source only |
| `hybrid` | `honcho` | Honcho + local cache | Honcho base URL | cache / debug / rebuild |
| `hybrid` | `codex_memoryd` | codex-memoryd SQLite + local cache | `provider_url` → `/v1` | cache / debug / rebuild |

See [`docs/codex-integration.md`](./docs/codex-integration.md) for the full
endpoint map, turn-input recall, writeback shape, local-memory import, and
fail-open contract.

## Roadmap by Issue Band

These are coarse groupings from the current issue stack. The exact tracker
mapping lives in the board; the README keeps the bands readable instead of
pretending the docs spell out every one-to-one issue.

| Band | Theme | Notes |
| --- | --- | --- |
| `#43-#45` | substrate vocabulary | subject / episode boundaries and evidence ledger MVP |
| `#50-#57` | substrate hardening | compiled cards, adapter views, capability gates, recall policy, context packs, git import, eval harness |
| `#66-#70` | frontier lanes | procedural memory, operational valence, quarantine / trust scoring, multimodal evidence, adapter conformance |
| `#80-#86` | docs and operator polish | runbooks, release-adjacent cleanup, README overhaul (`#86`) |

## Build and Test

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

## License

MIT.

## Memory Bakeoff

For a narrow comparison of local-only Codex memory, Honcho provider mode, and
codex-memoryd provider mode, see
[`demos/memory-bakeoff/README.md`](./demos/memory-bakeoff/README.md).
