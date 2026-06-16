# Local Dogfood Runbook

This runbook brings `codex-memoryd` up as a safe local dogfood memory service for Josh. It keeps memory local-first, loopback-only, manually operated, and explicitly non-authoritative.

For connecting current Codex to this service through MCP stdio, see [`dogfood-mcp.md`](./dogfood-mcp.md).
For write-capable dogfood tests, use the sandbox-only lane in
[`dogfood-write-sandbox.md`](./dogfood-write-sandbox.md).
For the native Codex memory migration phases, canaries, duplicate-loop risks,
and `memoryd-canonical` prerequisites, see
[`native-codex-memory-migration.md`](./native-codex-memory-migration.md).

## Safety Posture

- Bind only `127.0.0.1:8989` on the host for Josh's active dogfood path because
  Headroom owns `127.0.0.1:8787`.
- Use a persistent SQLite database.
- Import existing Codex memories with `sync-local --preview` before `--apply`.
- Use manual `conclude`, `checkpoint`, `recall`, and `export` only.
- Use `scripts/dogfood-write-sandbox.sh` for write-capable MCP/CLI tests; do
  not point write tools at the real dogfood DB.
- Keep Dreamer scheduler disabled and never run Dreamer auto-apply.
- Do not configure automatic prompt injection.
- Do not store secrets, auth tokens, `.env` dumps, hidden reasoning, raw confidential logs, or instructions that override current user, repo, system, or developer policy.
- Treat all recall output as `recall_not_authority`; current repo files, tests, and user instructions win.

## Native Loopback Service

The product dogfood path is the installed binary managing a native loopback
daemon. Docker Compose remains a repo development/debug path, not product
bootstrap.

The canonical operator path is:

```bash
codex-memoryd init --port 8989
codex-memoryd up
codex-memoryd status
codex-memoryd sync-local --preview ~/.codex/memories
codex-memoryd sync-local --apply ~/.codex/memories
codex-memoryd recall --query "safe dogfood mode"
```

`init` writes product runtime state under `~/.codex-memoryd/`, including
`runtime.env` with `CODEX_MEMORYD_URL=http://127.0.0.1:8989`,
`CODEX_MEMORYD_HOST=127.0.0.1`, and `CODEX_MEMORYD_PORT=8989`. `up`, `status`,
`sync-local`, and `recall` use that resolved URL/port without manual env
spelunking.

The legacy helper remains useful for repo-local debugging:

```bash
scripts/codex-memoryd-local-runtime.sh start
scripts/codex-memoryd-local-runtime.sh status
scripts/codex-memoryd-local-runtime.sh smoke
scripts/codex-memoryd-local-runtime.sh restart-survival
scripts/codex-memoryd-local-runtime.sh stop
```

The helper writes runtime state under `.dogfood/`, uses `.dogfood/memory.db`, and
keeps its own host bind default unless overridden.
For systemd user-service dogfood, render the unit first and inspect it before
installing:

```bash
scripts/codex-memoryd-local-runtime.sh systemd-unit > .dogfood/codex-memoryd.service
systemctl --user link "$PWD/.dogfood/codex-memoryd.service"
systemctl --user enable --now codex-memoryd.service
systemctl --user status codex-memoryd.service
curl -fsS http://127.0.0.1:8989/v1/status | jq
```

For self-hosting, keep the daemon behind a normal authenticated HTTPS front
door. Non-loopback binds are rejected by the helper unless
`CODEX_MEMORYD_ALLOW_NON_LOOPBACK=1` is set intentionally; the local default
must remain `http://127.0.0.1:8989`.

Adapter examples should use the same deployment vocabulary everywhere:

```toml
[memory_provider.codex_memoryd]
base_url = "http://127.0.0.1:8989"
profile = "personal"
workspace = "josh-personal"
credential_env = "CODEX_MEMORYD_TOKEN"
```

For local loopback dogfood, omit `credential_env` unless an adapter requires a
placeholder. For self-hosted HTTPS, store the credential in the client runtime's
secret manager or environment; do not write it into repo docs or checked-in
config.

The manual equivalent is still useful when debugging the exact process command:

```bash
cargo build --bin codex-memoryd
mkdir -p .dogfood/logs .dogfood/exports

setsid -f env \
  CODEX_MEMORYD_DB="$PWD/.dogfood/memory.db" \
  CODEX_MEMORYD_BIND="127.0.0.1:8989" \
  CODEX_MEMORYD_PROFILE="personal" \
  CODEX_MEMORYD_WORKSPACE="josh-personal" \
  CODEX_MEMORYD_LOG="info" \
  target/debug/codex-memoryd serve >> .dogfood/logs/codex-memoryd.log 2>&1

pgrep -n -f 'target/debug/codex-memoryd serve' > .dogfood/codex-memoryd.pid
pgrep -af 'target/debug/codex-memoryd serve'
curl -fsS http://127.0.0.1:8989/healthz
curl -fsS http://127.0.0.1:8989/v1/status | jq
```

Stop it with:

```bash
kill "$(cat .dogfood/codex-memoryd.pid)"
```

## Compose Runtime Heartbeat

For repeatable Compose verification, run:

```bash
scripts/dogfood-compose-heartbeat.sh
```

This heartbeat rebuilds and relaunches Compose from current checkout, checks
health/status/doctor, runs `sync-local` preview/apply/apply, refreshes
`.dogfood/mcp-sandbox-memory.db` from `.dogfood/memory.db`, verifies localhost-only
publish on `127.0.0.1:8989`, and runs a raw MCP stdio canary in `--read-only` mode.

Enable the scheduled Dreamer in Compose with one environment flag:

```bash
CODEX_MEMORYD_DREAM_SCHEDULER_ENABLED=1 docker compose up -d --build
curl -fsS http://127.0.0.1:8989/v1/status | jq '.data.features.dream_scheduler'
```

For normal operator CLI work against the same `.dogfood/memory.db`, prefer the
local front door over `docker compose exec`:

```bash
scripts/memd status | jq
scripts/memd sync-local --preview ~/.codex/memories
scripts/memd sync-local --apply ~/.codex/memories
scripts/memd dream --preview
scripts/memd dream --apply
```

Write-capable dogfood uses a separate sandbox lane:

```bash
scripts/dogfood-write-sandbox.sh run
```

That lane refreshes `.dogfood/write-sandbox-memory.db` with
`codex-memoryd backup create`, runs CLI and MCP writes only against the
sandbox, and emits content-free diff and manual-promotion artifacts.

## Manual Dogfood Flow

Set the database path for all CLI commands:

```bash
export CODEX_MEMORYD_DB="$PWD/.dogfood/memory.db"
export DOGFOOD_PROFILE=personal
export DOGFOOD_WORKSPACE=josh-personal
```

Verify health:

```bash
target/debug/codex-memoryd doctor | jq
```

Export before import:

```bash
target/debug/codex-memoryd export \
  --profile "$DOGFOOD_PROFILE" \
  --workspace "$DOGFOOD_WORKSPACE" \
  > .dogfood/exports/pre-import-personal-josh-personal.jsonl
```

Preview and apply local Codex memory import:

```bash
target/debug/codex-memoryd sync-local --preview \
  --profile "$DOGFOOD_PROFILE" \
  --workspace "$DOGFOOD_WORKSPACE" \
  ~/.codex/memories > .dogfood/sync-preview.json

target/debug/codex-memoryd sync-local --apply \
  --profile "$DOGFOOD_PROFILE" \
  --workspace "$DOGFOOD_WORKSPACE" \
  ~/.codex/memories > .dogfood/sync-apply.json

target/debug/codex-memoryd sync-local --apply \
  --profile "$DOGFOOD_PROFILE" \
  --workspace "$DOGFOOD_WORKSPACE" \
  ~/.codex/memories > .dogfood/sync-apply-second.json
```

Before treating this daemon as the preferred memory surface, compare the
preview/apply/apply counts and run the parity canaries in
[`native-codex-memory-migration.md`](./native-codex-memory-migration.md).

Write manual dogfood memory:

```bash
target/debug/codex-memoryd conclude \
  --profile "$DOGFOOD_PROFILE" \
  --workspace "$DOGFOOD_WORKSPACE" \
  --content "Decision: safe dogfood mode for codex-memoryd means local-first loopback daemon, persistent SQLite storage, manual sync/conclude/checkpoint/recall/export only, recall-not-authority, preview before apply, no automatic prompt injection, no scheduler apply, and no Dreamer auto-apply."
```

The CLI does not expose `checkpoint` yet. Use HTTP:

```bash
cat > .dogfood/checkpoint.json <<'JSON'
{
  "profile": "personal",
  "workspace": "josh-personal",
  "repo": {
    "repo_id": "github:joshyorko/codex-memoryd",
    "is_git": true
  },
  "summary": "Dogfood checkpoint: codex-memoryd is running locally in safe manual mode.",
  "changed_files": ["docs/dogfood-local.md"],
  "decisions": [
    "Keep dogfood service local-first and loopback-only.",
    "Treat recall as contextual evidence, not authority."
  ],
  "next_steps": [
    "Keep automatic prompt injection disabled.",
    "Export before and after imports."
  ],
  "tests_run": [
    "curl -fsS http://127.0.0.1:8989/healthz",
    "target/debug/codex-memoryd doctor"
  ],
  "tests_not_run": [],
  "branch": "patchraptor/codex-memoryd-dogfood-local",
  "commit": null
}
JSON

curl -fsS -X POST http://127.0.0.1:8989/v1/checkpoints \
  -H 'content-type: application/json' \
  --data @.dogfood/checkpoint.json | jq
```

Run recall canaries:

```bash
for query in \
  "What is codex-memoryd's north star?" \
  "What must never become durable memory?" \
  "What is the safe dogfood mode?" \
  "What is the current substrate PR stack?"
do
  target/debug/codex-memoryd recall \
    --profile "$DOGFOOD_PROFILE" \
    --workspace "$DOGFOOD_WORKSPACE" \
    --query "$query" \
    --max-tokens 1200 | jq '{summary, authority, facts: .facts[0:3], checkpoints: .checkpoints[0:1]}'
done
```

Export after import:

```bash
target/debug/codex-memoryd export \
  --profile "$DOGFOOD_PROFILE" \
  --workspace "$DOGFOOD_WORKSPACE" \
  > .dogfood/exports/post-import-personal-josh-personal.jsonl
```

Restart survival check:

```bash
kill "$(cat .dogfood/codex-memoryd.pid)"
setsid -f env CODEX_MEMORYD_DB="$PWD/.dogfood/memory.db" CODEX_MEMORYD_BIND="127.0.0.1:8989" target/debug/codex-memoryd serve >> .dogfood/logs/codex-memoryd.log 2>&1
pgrep -n -f 'target/debug/codex-memoryd serve' > .dogfood/codex-memoryd.pid
curl -fsS http://127.0.0.1:8989/v1/status | jq
target/debug/codex-memoryd recall --profile personal --workspace josh-personal --query "What is the safe dogfood mode?" --max-tokens 800 | jq
```

## Bootstrap Report, 2026-06-13

- Branch: `patchraptor/codex-memoryd-dogfood-local`
- Code commit tested: `07934e9`
- Service: native `target/debug/codex-memoryd serve`
- Bind: `127.0.0.1:8989`
- Storage: `.dogfood/memory.db`
- Health: `/healthz` returned `{"ok":true}`
- Status: `local_only`, schema `3`, writable SQLite, no warnings
- Doctor: `local_only`, FTS5 enabled, record count `346` after import/manual writes
- Dreamer scheduler: disabled
- Auto prompt injection: not configured
- Sync preview: 10 files scanned, 343 proposed, 0 rejected, 0 warnings
- Sync apply: 343 created, 0 rejected, 0 warnings
- Sync apply second run: 343 skipped, proving idempotence
- Manual conclusions: safe dogfood mode plus north-star/safety canary anchor
- Manual checkpoint: current PR stack #58-#64 and #65 paused
- Post-import export: 346 records
- Restart survival: recall still returned safe dogfood mode after daemon restart

Backups and artifacts are runtime-local under `.dogfood/`, which is ignored by git.

GitHub stack at the time of this run:

- <https://github.com/joshyorko/codex-memoryd/pull/58>
- <https://github.com/joshyorko/codex-memoryd/pull/59>
- <https://github.com/joshyorko/codex-memoryd/pull/60>
- <https://github.com/joshyorko/codex-memoryd/pull/61>
- <https://github.com/joshyorko/codex-memoryd/pull/62>
- <https://github.com/joshyorko/codex-memoryd/pull/63>
- <https://github.com/joshyorko/codex-memoryd/pull/64>

Fizzy tracker: <https://fizzy.joshyorko.com/1/cards/491>

Oracle was available locally (`oracle --help` worked), but was not needed for this bootstrap run because the task was operational verification rather than unresolved architecture review.
