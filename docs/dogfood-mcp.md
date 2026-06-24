# Local Dogfood MCP Runbook

This runbook connects current Codex to `codex-memoryd` through MCP stdio in safe dogfood mode. It uses manual tool access only; it does not enable prompt injection, Dreamer apply, scheduler apply, remote MCP, or the tap-release memory provider path.

For the larger native Codex memory migration plan, including when MCP recall is
safe enough to support `memoryd-canonical` mode, see
[`native-codex-memory-migration.md`](./native-codex-memory-migration.md).
For write-capable MCP testing, use the separate sandbox lane in
[`dogfood-write-sandbox.md`](./dogfood-write-sandbox.md).

## Safety Posture

- Keep the real dogfood daemon and database untouched.
- Real dogfood database: `.dogfood/memory.db`.
- MCP test database: `.dogfood/mcp-sandbox-memory.db`.
- Transport: local stdio only.
- Codex-facing tools: `memory_status`, `memory_recall`, and `memory_search` only.
- Write tools are not exposed to Codex in this config, and stdio defaults to
  read-only server-side. The `--read-only` flag is kept in the config as an
  explicit adapter safety marker.
- Write-capable MCP belongs only to `.dogfood/write-sandbox-memory.db` through
  `scripts/dogfood-write-sandbox.sh`; never add `--write-tools` to the real
  dogfood MCP config.
- Recall is `recall_not_authority`; user instructions, repo files, and tests override memory.
- No automatic memory writes, hidden reasoning storage, secret storage, prompt injection, or Dreamer auto-apply.

## Create Sandbox DB

Use SQLite backup so the running daemon's WAL is handled safely without touching the real database:

```bash
sqlite3 .dogfood/memory.db ".backup '.dogfood/mcp-sandbox-memory.db'"
sqlite3 .dogfood/mcp-sandbox-memory.db \
  "select count(*) from memory_records; select count(*) from checkpoints; select count(*) from conclusions;"
```

This backup refresh is also what the compose heartbeat script runs automatically.

Bootstrap result on 2026-06-13:

- `memory_records`: `346`
- `checkpoints`: `1`
- `conclusions`: `2`

## Codex MCP Config

Preferred flow:

```bash
codex-memoryd mcp codex preview
codex-memoryd mcp codex apply
codex-memoryd mcp codex status
```

The wizard writes only the owned `[mcp_servers.codex_memoryd]` block, uses the
resolved binary/database paths from the current runtime, and backs up an
existing `~/.codex/config.toml` before mutation.

Generated block shape:

```toml
[mcp_servers.codex_memoryd_dogfood]
command = "/var/home/kdlocpanda/second_brain/Resources/codex-memory-lab/codex-memoryd/target/debug/codex-memoryd"
args = ["--db", "/var/home/kdlocpanda/second_brain/Resources/codex-memory-lab/codex-memoryd/.dogfood/mcp-sandbox-memory.db", "mcp", "stdio", "--read-only"]
enabled_tools = ["memory_status", "memory_recall", "memory_search"]
default_tools_approval_mode = "approve"
startup_timeout_sec = 30
tool_timeout_sec = 30
```

Verify Codex sees the safe server shape:

```bash
codex mcp get codex_memoryd_dogfood
```

Expected key lines:

```text
enabled: true
enabled_tools: memory_status, memory_recall, memory_search
transport: stdio
default_tools_approval_mode: approve
```

## Direct MCP Smoke

Use direct JSON-RPC over stdio to verify the server before asking Codex to call it:

```bash
target/debug/codex-memoryd --db .dogfood/mcp-sandbox-memory.db mcp stdio --read-only < .dogfood/mcp-smoke.requests.jsonl
```

The compose heartbeat script runs an equivalent raw `mcp stdio --read-only` request
canary and enforces:

- tools/list returns exactly `memory_status`, `memory_recall`, `memory_search`
- `memory_conclude` is rejected

Observed direct results in read-only mode:

- `initialize`: ok
- `tools/list`: `memory_status`, `memory_recall`, `memory_search`
- `memory_status`: `local_only`, storage `.dogfood/mcp-sandbox-memory.db`, Dreamer scheduler disabled
- `memory_recall`, north star canary: returned useful facts with `authority = "recall_not_authority"`
- `memory_recall`, safe dogfood canary: returned useful facts with `authority = "recall_not_authority"`
- `memory_search`, `safe dogfood`: returned 5 matches
- `memory_search`, `dogfood`: returned 2 matches
- `memory_search`, `Dreamer`: returned 5 matches

These MCP canaries are necessary but not sufficient for native memory migration.
Run the native-memory parity canaries from
[`native-codex-memory-migration.md`](./native-codex-memory-migration.md) before
changing any real dogfood write posture.

## Write-Capable Sandbox

Use the dedicated lane when a test needs MCP `--write-tools`:

```bash
scripts/dogfood-write-sandbox.sh run
```

It refreshes the sandbox with `codex-memoryd backup create`, runs write canaries
only against `.dogfood/write-sandbox-memory.db`, writes a content-free diff
report, and produces a manual promotion preview. The script does not promote
anything to `.dogfood/memory.db`.

## Current Codex Verification

This was verified with a fresh Codex exec process because the already-running desktop thread did not hot-reload the newly added MCP server.

```bash
codex exec --ephemeral --json --sandbox read-only -m gpt-5.4-mini \
  -c model_reasoning_effort='"low"' \
  -C /var/home/kdlocpanda/second_brain/Resources/codex-memory-lab/codex-memoryd \
  'Use only the MCP server codex_memoryd_dogfood. Do not use shell. Call memory_status, memory_recall with query "What is the safe dogfood mode?", and memory_search with query "codex-memoryd" limit 3. Report compact JSON with: status, storage_path, tools_used, recall_authority, recall_first_fact, search_count.'
```

Observed result:

```json
{
  "status": "local_only",
  "storage_path": "/var/home/kdlocpanda/second_brain/Resources/codex-memory-lab/codex-memoryd/.dogfood/mcp-sandbox-memory.db",
  "tools_used": ["memory_status", "memory_recall", "memory_search"],
  "recall_authority": "recall_not_authority",
  "recall_first_fact": "Dogfood checkpoint: current codex-memoryd PR stack is open and clean from #58 through #64.",
  "search_count": 0
}
```

`memory_search` with `codex-memoryd` returned zero matches, so a second search canary used `safe dogfood`:

```bash
codex exec --ephemeral --json --sandbox read-only -m gpt-5.4-mini \
  -c model_reasoning_effort='"low"' \
  -C /var/home/kdlocpanda/second_brain/Resources/codex-memory-lab/codex-memoryd \
  'Use only MCP server codex_memoryd_dogfood. Do not use shell. Call memory_search with profile personal, workspace josh-personal, query "safe dogfood", limit 2. Return compact JSON with search_count and first_match.'
```

Observed result:

```json
{
  "search_count": 2,
  "first_match": {
    "type": "task_checkpoint",
    "content": "Decision: safe dogfood mode for codex-memoryd means local-first loopback daemon, persistent SQLite storage, manual sync/conclude/checkpoint/recall/export only, recall-not-authority, preview before apply, no automatic prompt injection, no scheduler apply, and no Dreamer auto-apply."
  }
}
```

## Compatibility Note

Current Codex sends MCP `tools/call.params._meta`. `codex-memoryd` initially rejected that field with `invalid tools/call params`. The compatibility fix is to accept `_meta` on `tools/call` while still rejecting unknown fields inside tool arguments.

Regression:

```bash
cargo test --test mcp_stdio
```

Observed: `8 passed`.

## Known Limits

- Keep `--read-only` enabled for stdio dogfood configs as a visible safety
  marker, even though read-only is now the default. Never add `--write-tools`
  to the Codex sandbox dogfood server.
- Search quality depends on terms. `safe dogfood`, `dogfood`, and `Dreamer` returned matches; `codex-memoryd` alone did not.
- The real dogfood daemon continues to run separately against `.dogfood/memory.db` on `127.0.0.1:8989`.
