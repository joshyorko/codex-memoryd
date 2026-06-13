# Local Dogfood Runbook

This runbook brings `codex-memoryd` up as a safe local dogfood memory service for Josh. It keeps memory local-first, loopback-only, manually operated, and explicitly non-authoritative.

## Safety Posture

- Bind only `127.0.0.1:8787` on the host.
- Use a persistent SQLite database.
- Import existing Codex memories with `sync-local --preview` before `--apply`.
- Use manual `conclude`, `checkpoint`, `recall`, and `export` only.
- Keep Dreamer scheduler disabled and never run Dreamer auto-apply.
- Do not configure automatic prompt injection.
- Do not store secrets, auth tokens, `.env` dumps, hidden reasoning, raw confidential logs, or instructions that override current user, repo, system, or developer policy.
- Treat all recall output as `recall_not_authority`; current repo files, tests, and user instructions win.

## Native Loopback Service

The Docker Compose path is useful for ordinary local smoke tests, but it binds `0.0.0.0` inside the container and therefore reports `auth_missing`. For this stricter dogfood run, use the native daemon so status reports `local_only`.

```bash
cargo build --bin codex-memoryd
mkdir -p .dogfood/logs .dogfood/exports

setsid -f env \
  CODEX_MEMORYD_DB="$PWD/.dogfood/memory.db" \
  CODEX_MEMORYD_BIND="127.0.0.1:8787" \
  CODEX_MEMORYD_PROFILE="personal" \
  CODEX_MEMORYD_WORKSPACE="josh-personal" \
  CODEX_MEMORYD_LOG="info" \
  target/debug/codex-memoryd serve >> .dogfood/logs/codex-memoryd.log 2>&1

pgrep -n -f 'target/debug/codex-memoryd serve' > .dogfood/codex-memoryd.pid
pgrep -af 'target/debug/codex-memoryd serve'
curl -fsS http://127.0.0.1:8787/healthz
curl -fsS http://127.0.0.1:8787/v1/status | jq
```

Stop it with:

```bash
kill "$(cat .dogfood/codex-memoryd.pid)"
```

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
    "curl -fsS http://127.0.0.1:8787/healthz",
    "target/debug/codex-memoryd doctor"
  ],
  "tests_not_run": [],
  "branch": "patchraptor/codex-memoryd-dogfood-local",
  "commit": null
}
JSON

curl -fsS -X POST http://127.0.0.1:8787/v1/checkpoints \
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
setsid -f env CODEX_MEMORYD_DB="$PWD/.dogfood/memory.db" CODEX_MEMORYD_BIND="127.0.0.1:8787" target/debug/codex-memoryd serve >> .dogfood/logs/codex-memoryd.log 2>&1
pgrep -n -f 'target/debug/codex-memoryd serve' > .dogfood/codex-memoryd.pid
curl -fsS http://127.0.0.1:8787/v1/status | jq
target/debug/codex-memoryd recall --profile personal --workspace josh-personal --query "What is the safe dogfood mode?" --max-tokens 800 | jq
```

## Bootstrap Report, 2026-06-13

- Branch: `patchraptor/codex-memoryd-dogfood-local`
- Code commit tested: `07934e9`
- Service: native `target/debug/codex-memoryd serve`
- Bind: `127.0.0.1:8787`
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
