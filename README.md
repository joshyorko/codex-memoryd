# codex-memoryd

`codex-memoryd` is a local-first portable memory provider for coding agents. It
treats memory as recall, not authority. Records stay scoped by profile,
workspace, and repo. Preview shows what would change before any policy-gated
apply.

Dreamer proposes candidate memories from safe visible turns, checkpoints,
conclusions, and imported local memories, then shows a preview before any
policy-gated apply. Records carry provenance, supersession, and fail-open
behavior when the provider is unavailable. Hidden reasoning, secrets, raw
confidential logs, and credentials never become durable memory.

It is written in Rust and purpose-built for [Codex](../codex)'s provider
contract in [`SPEC.md`](./SPEC.md): durable memory storage, recall, ingestion,
dedupe, safety classification, and export across machines, devcontainers, and
agent surfaces. It is local-first: an SQLite database, a loopback HTTP daemon,
and a CLI. No embeddings, vector database, dashboard, or cloud hosting are
required for the MVP.

> **Memory is recall, not authority.** Retrieved memory informs the agent but
> never overrides current user instructions, repository files, `AGENTS.md`,
> explicit policy, or verified current state. Recall responses are explicitly
> tagged `authority = "recall_not_authority"`.

## What it is (and isn't)

Codex owns agent execution, turn lifecycle, sandboxing, approvals, and prompt
assembly. `codex-memoryd` owns:

- durable storage of memory records, sources, checkpoints, conclusions, turns;
- **recall** — compact, ranked, repo-aware context before a turn;
- **ingestion** — importing existing local Codex memory artifacts;
- **dedupe** — idempotent writes and imports via content/source hashing;
- **safety** — secret + prompt-injection blocking, profile-boundary enforcement;
- **export** — boundary-aware, secret-omitting record export.

It does **not** execute coding tasks, act as a general workflow engine, store
secrets / hidden reasoning, or auto-merge work and personal memory.

## Try it in 5 minutes

This native path is Linux-first and works from a clean clone on
Ubuntu/Bluefin-class systems with Rust installed. It keeps demo state under the
repository's `.demo/` directory so it is easy to inspect and remove.

```bash
# Build and install a local native binary.
# If you have not cloned yet:
#   git clone https://github.com/joshyorko/codex-memoryd.git
#   cd codex-memoryd
cargo build --release
install -Dm755 target/release/codex-memoryd "$HOME/.local/bin/codex-memoryd"
export PATH="$HOME/.local/bin:$PATH"

# Start the daemon on loopback with an isolated demo database.
mkdir -p .demo
export CODEX_MEMORYD_DB="$PWD/.demo/memory.db"
codex-memoryd serve > .demo/codex-memoryd.log 2>&1 &
echo $! > .demo/codex-memoryd.pid

# Verify the daemon and local storage.
curl -fsS http://127.0.0.1:8787/v1/status | jq
codex-memoryd doctor | jq

# Configure Codex to use this provider, replacing any existing [memories] block.
python3 - <<'PY'
from pathlib import Path
p = Path.home() / ".codex" / "config.toml"
p.parent.mkdir(parents=True, exist_ok=True)
text = p.read_text() if p.exists() else ""
out = []
skip = False
for line in text.splitlines():
    stripped = line.strip()
    if stripped.startswith("[") and stripped.endswith("]"):
        skip = stripped == "[memories]"
        if not skip:
            out.append(line)
    elif not skip:
        out.append(line)
memories = """[memories]
backend = "provider"
provider = "codex_memoryd"
provider_url = "http://127.0.0.1:8787"
profile = "personal"
workspace = "codex-memoryd-demo"
local_import_policy = "prompt"
write_policy = "visible_turns"
sync_policy = "manual"
cross_profile_policy = "default_deny"
"""
p.write_text("\n".join(out).rstrip() + "\n\n" + memories)
PY

# Create a compact local Codex-memory fixture, then preview and apply import.
mkdir -p .demo/codex-memories/rollout_summaries
cat > .demo/codex-memories/memory_summary.md <<'MD'
# Preferences
- Prefer local-first memory providers for coding agents.
MD
codex-memoryd sync-local --preview --profile personal \
  --workspace codex-memoryd-demo .demo/codex-memories | jq
codex-memoryd sync-local --apply --profile personal \
  --workspace codex-memoryd-demo .demo/codex-memories | jq

# Write one conclusion, recall it, and export safe records.
codex-memoryd conclude --profile personal --workspace codex-memoryd-demo \
  --content "Decision: codex-memoryd first-run demo verifies status, recall, and export." | jq
codex-memoryd recall --profile personal --workspace codex-memoryd-demo \
  --query "first-run demo status recall export" | jq
codex-memoryd export --profile personal --workspace codex-memoryd-demo \
  > .demo/codex-memoryd-export.jsonl
cat .demo/codex-memoryd-export.jsonl

# Stop and restart the daemon.
kill "$(cat .demo/codex-memoryd.pid)"
codex-memoryd serve > .demo/codex-memoryd.log 2>&1 &
echo $! > .demo/codex-memoryd.pid
curl -fsS http://127.0.0.1:8787/v1/status | jq '.data.status'
kill "$(cat .demo/codex-memoryd.pid)"
```

Expected status JSON shape:

```json
{
  "ok": true,
  "data": {
    "provider_name": "codex-memoryd",
    "api_version": "v1",
    "status": "local_only",
    "storage": {
      "kind": "sqlite",
      "path": "/absolute/path/to/.demo/memory.db",
      "writable": true
    },
    "features": {
      "fts5": true,
      "exposure": "local_only"
    },
    "degraded_reasons": []
  },
  "warnings": [],
  "request_id": "req_...",
  "provider": {
    "name": "codex-memoryd",
    "version": "0.1.0"
  }
}
```

Expected recall JSON shape (CLI output):

```json
{
  "summary": "Decision: codex-memoryd first-run demo verifies status, recall, and export.",
  "facts": [
    {
      "id": "mem_...",
      "type": "decision",
      "scope": "user",
      "content": "Decision: codex-memoryd first-run demo verifies status, recall, and export.",
      "confidence": 0.9,
      "repo_id": null,
      "related_files": [],
      "updated_at": "2026-...",
      "stale": false
    }
  ],
  "checkpoints": [],
  "citations": [
    {
      "memory_id": "mem_...",
      "source_id": "src_...",
      "source_path": null
    }
  ],
  "truncated": false,
  "authority": "recall_not_authority"
}
```

Docker Compose path:

```bash
docker compose up -d --build
curl -fsS http://127.0.0.1:8787/v1/status | jq
docker compose logs -f codex-memoryd
docker compose down
```

Native release binary path, once a GitHub Release asset is published:

```bash
install -Dm755 ./codex-memoryd-linux-x86_64 "$HOME/.local/bin/codex-memoryd"
codex-memoryd --version
```

Troubleshooting first run:

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `bind ... failed to listen` | Port `8787` is already in use or the bind address is invalid. | Stop the other process or run `codex-memoryd serve --bind 127.0.0.1:8788` and update Codex `provider_url`. |
| `storage_unavailable` with `create storage dir` or `open ...` | The `--db`/`CODEX_MEMORYD_DB` path is blocked by a file or lacks permissions. | Choose a writable path such as `CODEX_MEMORYD_DB=$PWD/.demo/memory.db` or fix directory ownership. |
| `/v1/status` has `status: "auth_missing"` | The daemon is configured for non-loopback exposure without built-in auth. | Keep host exposure on `127.0.0.1` (Docker default) until a separate authenticated proxy/auth story exists. |
| `curl: (7) Failed to connect` | The daemon is not running or was started with a different port. | Check `.demo/codex-memoryd.log`, restart `codex-memoryd serve`, and verify the configured `provider_url`. |
| `jq: command not found` | `jq` is not installed. | Install it (`sudo apt install jq`) or omit `| jq`; responses are still JSON. |

If the daemon is down, Codex-side provider/hybrid mode is designed to fail open:
recall returns empty and writes are best-effort rather than blocking the coding
path. See issues #14 and #16 for live smoke/bakeoff follow-ups.

## Architecture

A single Rust crate with strict module boundaries (SPEC §3.2):

| Module | Responsibility |
| --- | --- |
| `protocol` | Wire request/response types + the common response envelope |
| `domain` | Durable entities (records, sources, checkpoints, …) |
| `config` | Config resolution (file → env → flags) |
| `store` | SQLite persistence, migrations, FTS5 probe + LIKE fallback |
| `policy` | Secret/injection detection, profile boundaries, classification |
| `ingest` | Local Codex memory import: chunk → classify → dedupe |
| `recall` | Ranking, token-budget packing, citations, checkpoints |
| `service` | Transport-agnostic request handling (shared by HTTP + CLI) |
| `server` | axum HTTP transport |
| `cli` | clap command-line interface |
| `export` / `status` / `metrics` | Export, status assembly, counters |

Stack: **axum** (HTTP), **clap** (CLI), **rusqlite** with bundled SQLite +
**r2d2** pool (storage), **serde** (types), **tracing** (logs), **regex**
(secret detection), **sha2** (hashing), **uuid** (ids).

## Build

```bash
cd codex-memoryd
cargo build --release
# binary at target/release/codex-memoryd
```

SQLite is compiled from source via the `bundled` feature, so the build needs a C
compiler but no system libsqlite3. FTS5 is therefore always available; if a
future build disables it, the store automatically falls back to `LIKE` search
and reports `degraded` status.

## Run the local daemon

```bash
# Defaults: bind 127.0.0.1:8787, storage ~/.codex-memoryd/memory.db
codex-memoryd serve

# Override bind / storage:
codex-memoryd serve --bind 127.0.0.1:8787 --db ~/.codex-memoryd/memory.db

# Check it:
curl -s http://127.0.0.1:8787/v1/status | jq
```

The daemon binds loopback by default and shuts down gracefully on SIGINT/SIGTERM.
This is the supported direct-run mode: `/healthz` is unauthenticated liveness,
and `/v1/*` is intended only for same-host Codex clients. There is no production
remote bearer-token/auth implementation in this daemon yet.

Supported bind/exposure modes:

- `127.0.0.1:8787`, `[::1]:8787`, or `localhost:8787`: supported local-only
  operation. `/v1/status` reports `status = "local_only"` when storage is
  otherwise healthy.
- Docker Compose default: the daemon binds `0.0.0.0:8787` inside the container,
  but the host publishes `127.0.0.1:8787:8787`. This is still a local-only host
  exposure.
- Non-loopback host publishing (for example `8787:8787`, `0.0.0.0:8787`, or a
  LAN address) is unsupported for production remote use until auth/isolation
  lands. `/v1/status` reports `status = "auth_missing"` and an actionable warning
  for this configuration; do not expose `/v1/*` to untrusted networks.

## CLI

The CLI operates on the same code paths as the HTTP server (it opens the store
directly), so it works without a running daemon. All read commands print JSON to
stdout; logs go to stderr.

```bash
# Status / self-check
codex-memoryd status
codex-memoryd doctor

# Recall and search
codex-memoryd recall --profile personal --workspace josh-personal --query "how do we serve the provider"
codex-memoryd search --profile personal --workspace josh-personal --query "axum"

# Write a durable conclusion (becomes a memory record after policy screening)
codex-memoryd conclude --profile personal --workspace josh-personal \
  --content "Decision: use rusqlite bundled for storage"

# Preview deterministic Dreamer candidates (no durable writes)
codex-memoryd dream --profile personal --workspace josh-personal --preview

# Import local Codex memory (provider local-ingest mode reads the filesystem)
codex-memoryd sync-local --preview ~/.codex/memories
codex-memoryd sync-local --apply   ~/.codex/memories

# Export (JSONL by default) and forget
codex-memoryd export --profile personal --workspace josh-personal > backup.jsonl
codex-memoryd forget <record-id>            # archives by default
codex-memoryd forget <record-id> --delete   # hard delete (secrets/PII)
```

## Configure storage

Resolution order (later wins): **defaults → config file → env vars → CLI flags**.

- Config file: `~/.codex-memoryd/config.toml` (see [`config.example.toml`](./config.example.toml))
- Storage DB: `~/.codex-memoryd/memory.db` (override with `--db` or `CODEX_MEMORYD_DB`)
- Env vars: `CODEX_MEMORYD_BIND`, `CODEX_MEMORYD_DB`, `CODEX_MEMORYD_PROFILE`,
  `CODEX_MEMORYD_WORKSPACE`, `CODEX_MEMORYD_LOG`

```toml
[server]
bind = "127.0.0.1:8787"

[storage]
kind = "sqlite"
path = "~/.codex-memoryd/memory.db"

[policy]
default_profile = "personal"
cross_profile_policy = "default_deny"

[recall]
max_tokens = 1200
```

## Run with Docker

```bash
# Build + start with a persistent named volume (codex_memoryd_data):
docker compose up -d --build

# Verify:
curl -s http://127.0.0.1:8787/v1/status | jq

# Logs / stop:
docker compose logs -f codex-memoryd
docker compose down            # keeps the volume
```

The image runs as a non-root user, stores data under the `/data` volume, binds
`0.0.0.0:8787` inside the container (published to `127.0.0.1:8787` on the host),
and ships a `HEALTHCHECK` hitting `/healthz`. Keep the host-side publish on
`127.0.0.1`; changing it to all interfaces is unsupported without an external
authenticating proxy. No secrets are baked into the image.

## How the Codex fork calls it

The Codex fork has provider-agnostic portable memory (config,
`PortableMemoryRuntime`, the `MemoryProvider` trait, selected
local/provider/hybrid behavior, turn-input recall, turn-item writeback).

### Final config contract

This is the canonical `[memories]` shape that codex-memoryd targets:

```toml
# Codex-side ~/.codex/config.toml
[memories]
backend = "provider"              # local | provider | hybrid
provider = "codex_memoryd"        # honcho | codex_memoryd  (when backend != local)
provider_url = "http://127.0.0.1:8787"
profile = "personal"
workspace = "josh-personal"
local_import_policy = "prompt"    # prompt | manual | startup_preview | startup_apply
write_policy = "visible_turns"    # off | visible_turns
sync_policy = "manual"            # manual | startup
cross_profile_policy = "default_deny"
```

`backend` stays a small stable enum; `provider` selects the implementation, so
adding providers never grows the `backend` enum. `provider_url` points the
runtime's HTTP client at this daemon's `/v1` API.

### Compatibility matrix

| `backend` | `provider` | Durable store | `provider_url` target | Local memory role |
| --- | --- | --- | --- | --- |
| `local` | — (ignored) | none (upstream local only) | — | source of truth |
| `provider` | `honcho` | Honcho (cloud/self-host) | Honcho base URL | import source only |
| `provider` | `codex_memoryd` | codex-memoryd SQLite | `http://127.0.0.1:8787` | import source only |
| `hybrid` | `honcho` | Honcho + local cache | Honcho base URL | cache / debug / rebuild |
| `hybrid` | `codex_memoryd` | codex-memoryd + local cache | `http://127.0.0.1:8787` | cache / debug / rebuild |

In `local` mode, `provider`/`provider_url` are ignored and codex-memoryd is not
contacted. In `provider`/`hybrid` mode the runtime fails open: if the daemon is
unreachable, recall returns empty and writes are best-effort (in `hybrid`, local
memory continues to serve). Provider errors and daemon-down conditions must not
block the Codex-side user path.

### Status vs. Codex tap-release

The shape above is implemented by `joshyorko/codex@tap-release`, including the
`provider = "codex_memoryd"` adapter, `provider_url`, manual local import, and
`visible_turns` writeback over this daemon's `/v1` API. Older Codex PR #55
snapshots exposed only the Honcho-shaped subset; the historical delta is kept in
[`docs/codex-integration.md`](./docs/codex-integration.md#historical-codex-side-delta-from-pr-55).
This repo does not modify `codex/`.

Typical first-run switchover (once both sides ship the final shape):

```bash
export CODEX_MEMORYD_URL=http://127.0.0.1:8787
codex memory status
codex memory import-local --preview   # safe: writes nothing
codex memory import-local --apply     # idempotent
codex
```

Live tap-release proof smoke:

```bash
# After building or otherwise obtaining a joshyorko/codex@tap-release binary:
CODEX_BIN=/tmp/codex-tap-release/codex-rs/target/debug/codex \
  scripts/codex-tap-release-smoke.sh
```

The smoke starts `codex-memoryd` on loopback, runs Codex in `provider` and
`hybrid` modes, captures `/v1/status`, recall authority, turn writeback counts,
local import preview/apply idempotency, and verifies daemon-down fail-open
behavior.

See [`docs/codex-integration.md`](./docs/codex-integration.md) for the full
endpoint map, the local-import wire format, and the fail-open contract.

## Roadmap: Dreamer loop (research)

A background/offline memory-synthesis pass — the **Dreamer loop** — consolidates
repeated safe evidence into durable, provenance-backed records, marks
drift-prone facts with validity metadata, and supersedes outdated records. It is
**evidence-backed consolidation, not autonomous authority**: candidates are
grouped by deterministic `subject_key`, previewed before writes, and accepted,
quarantined, or rejected by policy and threshold gates.

Dreamer treats evidence asymmetrically. User turns, explicit conclusions, and
checkpoints are strong signals (with checkpoints strongest for task/repo state);
assistant turns are weak unless adopted; imported memories are secondary
corroboration only; active records are only conflict/supersession inputs, never
primary evidence for creating a new active memory. Candidate apply records carry
provenance (`origin`, run/window, evidence ids/counts, `subject_key`,
`promotion_reason`), memory state (`planned`, `active`, `blocked`, `completed`,
`historical`, or `superseded`), and drift/expiry fields (`drift_prone`,
`expires_at`, `valid_until`) where applicable.

Public writing about ChatGPT memory is inspiration only. This repository makes no
claim about private OpenAI memory internals, compatibility, or architecture. See
[`docs/dreamer-loop-research.md`](./docs/dreamer-loop-research.md) (motivation,
threat model, non-claims) and
[`docs/dreamer-loop-design.md`](./docs/dreamer-loop-design.md) (CLI/API,
storage, staleness/supersession, eval fixtures).

Implementation order is tracked by issues: #10 deterministic preview, #11
fixture sidecars and recall-before/after evals, #19 evidence weighting and
thresholds, #13 state/staleness/supersession, #12 policy-gated apply, #20
dream-run audit/watermarks, #14 live Codex smoke, #15 reliability/auth/fail-open
contracts, #17 first-run demo, #16 local-first bakeoff, and #18 local MCP tools.

## Safety & profile boundaries

**Secret blocking** (rejected or redacted before any durable write): private
keys (PEM/OpenSSH/PGP), API keys (OpenAI/Anthropic/Honcho/GitHub/Slack/Google/
Stripe), AWS keys, generic `key=`/`password=`/`token=` assignments, JWTs,
connection strings with inline credentials, `.env` dumps, encrypted/hidden
reasoning markers, and oversized unstructured blobs likely to hide secrets.

**Prompt-injection blocking**: durable memories that look like attempts to
override system/developer/user policy ("ignore previous instructions", "you are
now system", "reveal the system prompt", …) are rejected.

**Profile boundaries** (export defaults, SPEC §10.3):

| From → To | Behavior |
| --- | --- |
| `work` → `personal` | **deny** |
| `work` → any other | **deny** |
| `personal` → `work` | allow **only** generic user operating preferences (preference/identity, public/personal) |
| `work` → `work`, `personal` → `personal` | allow |
| `oss`/`homelab` → `personal` | allow (implementation-defined; these are non-confidential surfaces) |

Export always omits `secret_blocked` records and never crosses a `never_export`
record between profiles. Every policy decision (rejection or boundary denial) is
recorded in the `policy_events` table for audit and surfaced in metrics.

**Authority**: provider recall is contextual, not authoritative. Codex treats it
below current user instructions, system/developer instructions, `AGENTS.md`,
repository files, and verified current state.

## Testing

```bash
cargo test          # unit + conformance + HTTP smoke + CLI smoke
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

The conformance suite ([`tests/conformance.rs`](./tests/conformance.rs)) covers
the MVP surface from SPEC §15.3: status, profile/workspace isolation, record
create/search, recall filtering, secret + injection rejection, conclusion →
record, checkpoint store/recall, local import preview/apply idempotency,
work→personal export denial, forget archiving, and secret omission on export.

## License

MIT.
