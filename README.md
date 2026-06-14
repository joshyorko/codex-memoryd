# codex-memoryd

`codex-memoryd` is a local-first, agent-agnostic memory substrate for coding
agents. The durable store is the product; the daemon, CLI, Docker Compose
heartbeat, adapter exports, and MCP stdio are delivery modes. Memory is recall,
not authority: retrieved context can inform a turn, but it never overrides the
current user, repo, or policy state.

## Current Surface

This is the landed MVP surface today:

| Surface | Status | Notes |
| --- | --- | --- |
| Local loopback server | landed | Native daemon binds `127.0.0.1:8787` with persistent SQLite storage. |
| Docker Compose dogfood | landed | Uses `.dogfood/memory.db` as the real daemon DB and keeps host publish loopback-only. |
| Compose heartbeat | landed | `scripts/dogfood-compose-heartbeat.sh` rebuilds, restarts, and smoke-checks the stack. |
| Codex memory import | landed | `sync-local --preview` / `--apply` import local Codex memories. |
| Native memory migration plan | documented | Phases native Codex memory from import/fallback toward optional `memoryd-canonical` mode. |
| Subject / episode substrate | landed | Stable subjects, append-only episodes, and the evidence ledger are the core memory shape. |
| Recall policy metadata | landed | `recall_not_authority`, ranking, admission, and provenance metadata travel with recall. |
| Current-state cards | landed MVP | `workspace_summary`, `subject_summary`, `active_preferences`, `open_questions`, `recent_scars`, `procedures_index`. |
| Context packs | landed MVP | `default`, `debugging`, `onboarding`, `planning`, `active_task`, `review`, and `personal_context` pack modes are supported. |
| Adapter exports | landed | `agents-md`, `claude-code`, `copilot`, `github-instructions`, `mcp-json`, `mcp-pack`, `markdown`, and `markdown-wiki`. |
| Adapter conformance | landed | `conformance adapters` emits a deterministic report for adapter authority, provenance, and budget behavior. |
| Adapter packages | landed | Installable templates for Codex MCP, Claude-style local MCP, Copilot instructions, and generic MCP/markdown clients. |
| Git import | landed | Imports commit trailers plus refs fixtures for JSON and JSONL exports. |
| MCP dogfood | landed read-only | Exposes `memory_status`, `memory_recall`, and `memory_search` only. |
| Dreamer patch lifecycle | landed MVP | Preview/apply exists for reviewable consolidation; broader procedural automation is still roadmap work. |

## Safety Model

- Loopback-only is the default dogfood posture.
- `recall_not_authority` is mandatory. Recall can inform, not command.
- Preview happens before apply.
- Apply is idempotent and policy-gated.
- No automatic prompt injection.
- Secrets, tokens, `.env` dumps, hidden reasoning, private keys, auth files, and raw confidential logs are rejected before durable write.
- Profile and workspace boundaries are enforced.
- Recall admits only the requested profile/workspace by default. Metadata-marked
  `quarantined`, `high`/`unsafe` risk, `unsafe`/`rejected`/`blocked`, and
  `superseded` records are withheld unless a future explicit review path changes
  the state.
- Stale records may still be returned, but they are marked stale and
  deprioritized. Archived/superseded records remain explainable through withheld
  counts rather than raw recalled content.
- Provider failure fails open instead of blocking Codex turns.
- The read-only MCP path exposes no write tools.

## First Run

### First-run path (source build)

This is the safest local run.

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
curl -fsS http://127.0.0.1:8787/healthz
curl -fsS http://127.0.0.1:8787/v1/status | jq
target/release/codex-memoryd doctor
target/release/codex-memoryd sync-local --preview ~/.codex/memories \
  --profile personal --workspace josh-personal
target/release/codex-memoryd sync-local --apply ~/.codex/memories \
  --profile personal --workspace josh-personal
target/release/codex-memoryd conclude --profile personal --workspace josh-personal \
  --content "Decision: keep codex-memoryd local-first."
target/release/codex-memoryd recall --profile personal --workspace josh-personal \
  --pack-mode onboarding --query "What is the safe dogfood mode?"
target/release/codex-memoryd card show --profile personal --workspace josh-personal \
  --type workspace_summary
```

Fail-open note: if `codex-memoryd` is unavailable, Codex provider or hybrid
mode should skip provider recall and continue the turn instead of blocking work.

### Canonical local runtime helper

For day-to-day native dogfood, use the checked-in runtime helper instead of
copying process-management snippets by hand:

```bash
scripts/codex-memoryd-local-runtime.sh start
scripts/codex-memoryd-local-runtime.sh status
scripts/codex-memoryd-local-runtime.sh smoke
scripts/codex-memoryd-local-runtime.sh restart-survival
scripts/codex-memoryd-local-runtime.sh stop
```

The helper defaults to `CODEX_MEMORYD_BIND=127.0.0.1:8787`,
`CODEX_MEMORYD_DB=$PWD/.dogfood/memory.db`, `CODEX_MEMORYD_PROFILE=personal`,
and `CODEX_MEMORYD_WORKSPACE=josh-personal`. It refuses non-loopback binds
unless `CODEX_MEMORYD_ALLOW_NON_LOOPBACK=1` is set for an explicitly
self-hosted deployment behind HTTPS authentication.

### Compose smoke and heartbeat

Compose is useful for reproducible smoke checks, but it is not the primary
dogfood surface.

```bash
docker compose up -d --build
curl -fsS http://127.0.0.1:8787/healthz
curl -fsS http://127.0.0.1:8787/v1/status | jq
scripts/dogfood-compose-heartbeat.sh
docker compose down
```

## Operator Flows

### Local Codex memory import

`sync-local` is the supported path for importing existing Codex memories. Use
preview first, then apply.

```bash
target/release/codex-memoryd sync-local --preview ~/.codex/memories \
  --profile personal --workspace josh-personal
target/release/codex-memoryd sync-local --apply ~/.codex/memories \
  --profile personal --workspace josh-personal
```

For the native Codex memory migration phases, parity canaries, duplicate-loop
risks, and canonical-mode checklist, see
[`docs/native-codex-memory-migration.md`](./docs/native-codex-memory-migration.md).

### Recall and search

Recall is contextual evidence, not authority. Pack modes currently accept
`default`, `debugging`, `onboarding`, `planning`, `active_task`, `review`, and
`personal_context`. Hyphenated CLI input such as `active-task` is normalized to
the wire value `active_task`.

| Mode | Bias |
| --- | --- |
| `default` | balanced recall |
| `debugging` | gotchas, failures, rollback/recovery, commands |
| `onboarding` | conventions, architecture, setup, current state |
| `planning` | checkpoints, decisions, blockers, open questions, next steps |
| `active_task` | current handoff, blockers, next steps, commands |
| `review` | PR/review risk, verification, regressions, rollback notes |
| `personal_context` | user preferences and operating workflow defaults |

```bash
target/release/codex-memoryd recall --profile personal --workspace josh-personal \
  --pack-mode default --query "How do we use codex-memoryd?"
target/release/codex-memoryd search --profile personal --workspace josh-personal \
  --query "safe dogfood"
```

### Cards and exports

Current-state cards are deterministic views from the same store. Adapter exports
compile the same substrate into downstream file formats.

```bash
target/release/codex-memoryd card show --profile personal --workspace josh-personal \
  --type subject_summary
target/release/codex-memoryd adapter export --target agents-md \
  --profile personal --workspace josh-personal > AGENTS.memory.md
target/release/codex-memoryd adapter export --target mcp-pack \
  --profile personal --workspace josh-personal > mcp-pack.json
target/release/codex-memoryd adapter export --target mcp-json \
  --profile personal --workspace josh-personal > mcp-json.json
target/release/codex-memoryd conformance adapters --format json
```

Cards are generated on demand, so there is no persisted card cache to invalidate.
Each record carries explicit freshness metadata (`freshness.stale` and
`freshness.age_days`), and the card-level `freshness` value is
`contains_stale_records` whenever any included record is past the stale display
window. CLI markdown and adapter views render the same fresh/stale label. The
card smoke suite includes a fixture-backed markdown snapshot for this contract.

### Adapter packages

Installable adapter package templates live under [`adapters/`](./adapters/).
They are thin host-specific wrappers around `codex-memoryd` and default to
read-only operation:

- `adapters/codex-mcp`: Codex `~/.codex/config.toml` MCP snippet.
- `adapters/claude-local`: Claude-style local MCP server JSON snippet.
- `adapters/copilot-instructions`: Copilot instructions export wrapper.
- `adapters/generic-mcp-markdown`: generic MCP plus `AGENTS.md` and
  `GEMINI.md` markdown export wrappers.

Each package includes `.env.example`, install steps, verify commands, and
uninstall notes. The packages do not duplicate memory logic; they call
`codex-memoryd mcp stdio --read-only` or `codex-memoryd adapter export`.

### Git evidence import

`git-import` reads recent commit trailers such as `Memory-Decision`,
`Memory-Verify`, and `Memory-Gotcha`, plus refs fixtures for JSON and JSONL
exports.

```bash
target/release/codex-memoryd git-import --preview /path/to/repo
target/release/codex-memoryd git-import --apply /path/to/repo
target/release/codex-memoryd git-import --preview --refs-fixture /path/to/refs.jsonl /path/to/repo
target/release/codex-memoryd git-import --apply --refs-fixture /path/to/refs.json /path/to/repo
```

Preview/apply/idempotency are the same path in both modes. Apply writes safe
subject episodes and evidence ledger rows only; it does not promote Git evidence
to active memory records on its own.

### MCP read-only dogfood

The Codex-facing MCP runbook is intentionally read-only. `mcp stdio` defaults
to the read-only tool tier; `--read-only` is still accepted for explicit
adapter configs.

```bash
target/release/codex-memoryd --db .dogfood/mcp-sandbox-memory.db mcp stdio --read-only
```

The server exposes only `memory_status`, `memory_recall`, and `memory_search`.
See [`docs/dogfood-mcp.md`](./docs/dogfood-mcp.md) for the exact
`~/.codex/config.toml` snippet and smoke checks.

The write-capable tier is intentionally opt-in:

```bash
target/release/codex-memoryd --db ~/.codex-memoryd/memory.db mcp stdio --write-tools
```

That tier adds `memory_create`, `memory_conclude`, `memory_checkpoint`,
`memory_import_preview`, and `memory_import_apply`. These tools still pass
through the same write policy, secret detection, import preview/apply, and
provenance paths as the HTTP and CLI surfaces. Do not expose `--write-tools` to
a Codex sandbox or remote client unless an adapter capability review has
explicitly allowed that client to write.

## Memory Model

- `Subject`: stable identity for what the memory is about.
- `Episode`: immutable event attached to a subject.
- `Evidence ledger`: append-only provenance for writes, imports, and synthesis.
- `Card`: deterministic current-state view for cheap recall.
- `Pack`: budgeted recall bundle for a specific adapter or mode.
- `Adapter export`: downstream rendering such as `AGENTS.md`, `CLAUDE.md`, `mcp-json`, or `markdown-wiki`.
- `Recall policy metadata`: admission, ranking, provenance, and `recall_not_authority`.

This substrate is intentionally not a graph engine, CRM, scheduler, or agent
harness. Subjects and episodes are internal anchors, not a standalone exported
surface.

## Roadmap

The board still has open work. The lanes below are status snapshots, not claims
that the product is done:

| Issue | Lane | Status |
| --- | --- | --- |
| `#50` | current-state cards | landed deterministic cards with stale metadata and fixture-backed rendering coverage |
| `#53` | eval suite | open; still validating capability gates, recall, and context-pack behavior |
| `#55` | recall policy | admission gates now withhold quarantined/high-risk/unsafe/superseded records by default; remaining work depends on broader eval/review lanes |
| `#56` | adapter context packs | closeable by this implementation: deterministic recall packs, adapter-specific pack names, budget/truncation reporting, and regression coverage are present |
| `#57` | git import | landed for local trailers and refs fixtures covering commits, PRs, issues, and review comments |
| `#70` | adapter conformance | `conformance adapters` report now certifies adapter authority, provenance, and budget behavior |

## Related Docs

- [`docs/agent-agnostic-memory-substrate.md`](./docs/agent-agnostic-memory-substrate.md)
- [`docs/codex-integration.md`](./docs/codex-integration.md)
- [`docs/dogfood-local.md`](./docs/dogfood-local.md)
- [`docs/dogfood-mcp.md`](./docs/dogfood-mcp.md)
- [`docs/native-codex-memory-migration.md`](./docs/native-codex-memory-migration.md)
- [`docs/evidence-ledger.md`](./docs/evidence-ledger.md)
- [`docs/dreamer-loop-design.md`](./docs/dreamer-loop-design.md)
- [`docs/dreamer-loop-research.md`](./docs/dreamer-loop-research.md)

## License

MIT.
