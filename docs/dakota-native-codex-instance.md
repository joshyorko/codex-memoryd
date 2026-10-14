# Dakota: independent native Codex MemoryD

This runbook creates a second native MemoryD home for Codex on Dakota. It is
separate from Friday's existing instance, uses a loopback port, and imports
Codex memory files into local SQLite only. Native Codex memory remains
available as fallback; this does not enable provider writeback or canonical-only
mode.

The commands below match the process arguments and checks in
[`native_instance_isolation.rs`](../tests/native_instance_isolation.rs). The
automated test uses synthetic records, two temporary homes, and dynamically
allocated loopback ports. It does not access Dakota, Friday, or real Codex
memory.

## Select the Codex home

Use a clean command wrapper so inherited provider, scheduler, database, or URL
settings cannot override the selected home or enable provider access. The
wrapper keeps only the basic shell environment and the selected MemoryD home;
the home resolves its own database, runtime environment, PID file, log, and
endpoint.

```bash
MEMORYD_CODEX_HOME="$HOME/.codex-memoryd-codex"

md() {
  env -i \
    HOME="$HOME" \
    PATH="$PATH" \
    TMPDIR="${TMPDIR:-/tmp}" \
    CODEX_MEMORYD_HOME="$MEMORYD_CODEX_HOME" \
    codex-memoryd "$@"
}
```

Check the tentative port before initialization. If 8990 is occupied, choose a
free loopback port and update `MEMORYD_CODEX_PORT`; do not change Friday's
listener.

```bash
MEMORYD_CODEX_PORT=8990
python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 8990)); s.close(); print("127.0.0.1:8990 is available at check time")'
```

## Initialize and inspect resolved paths

```bash
md init --host 127.0.0.1 --port "$MEMORYD_CODEX_PORT" \
  --bind "127.0.0.1:$MEMORYD_CODEX_PORT"

md paths --format json | jq '{home: .entries.runtime_home.path, database: .entries.database.path, config: .entries.config_file.path, pid: .entries.pid_file.path, log: .entries.log_file.path, bind: .bind.value, url: .url.value}'

md config show --resolved | jq '{config_file, home: .runtime.home, database: .runtime.db, configured_database: .daemon.storage_path, bind: .runtime.bind, port: .runtime.port}'
```

Confirm the home is `$HOME/.codex-memoryd-codex`, the database is
`$HOME/.codex-memoryd-codex/memory.db`, and the bind is `127.0.0.1` on the
selected port. Stop if either database path or endpoint resolves elsewhere.

## Start and check the daemon

```bash
md up
md status | jq -e --arg db "$MEMORYD_CODEX_HOME/memory.db" \
  '{status: .data.status, storage_path: .data.storage.path} | select(.status == "local_only" and .storage_path == $db)'
md logs --lines 40
```

While running, `status.data.storage.path` should be the Codex database path. The
daemon status is `local_only`, and the bind remains loopback-only.

## Preview and apply native Codex memory

Run this on Dakota, where `~/.codex/memories` is local. Preview first and review
the counts and warnings before applying. The source files are read locally and
the accepted records are written to this selected local database.

```bash
md sync-local --preview --profile personal --workspace dakota "$HOME/.codex/memories" \
  | tee "$MEMORYD_CODEX_HOME/sync-local-preview.json"
jq '.data // . | {mode, files_scanned, proposed, created, updated, skipped, rejected, warnings}' \
  "$MEMORYD_CODEX_HOME/sync-local-preview.json"

md sync-local --apply --profile personal --workspace dakota "$HOME/.codex/memories" \
  | tee "$MEMORYD_CODEX_HOME/sync-local-apply.json"

md sync-local --apply --profile personal --workspace dakota "$HOME/.codex/memories" \
  | tee "$MEMORYD_CODEX_HOME/sync-local-apply-second.json"
jq '.data // . | {mode, files_scanned, created, updated, skipped, rejected}' \
  "$MEMORYD_CODEX_HOME/sync-local-apply-second.json"
```

The second apply should not create duplicates. Imports retain source provenance
and remain recall context with `recall_not_authority` semantics. No import,
backup, or MCP command in this runbook sends memory to a cloud service.

## Verify recall and read-only MCP

Use a topic present in the imported files for the recall query.

```bash
md recall --profile personal --workspace dakota \
  --query "What do my native Codex memories say about this project?" \
  --max-tokens 1200 \
  | jq '.data // . | {authority, facts: .facts[0:3], citations}'
```

This protocol check verifies MCP discovery, the selected database path,
`memory_status`, and recall through the read-only stdio server. It does not edit
`~/.codex/config.toml` or disable Codex's native memory behavior.

```bash
set -o pipefail
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"dakota-check","version":"1"},"capabilities":{}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_status","arguments":{}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"memory_recall","arguments":{"profile":"personal","workspace":"dakota","query":"What do my native Codex memories say about this project?","maxTokens":1200}}}' \
  | md mcp stdio --read-only \
  | jq -c 'select(.id == 2 or .id == 3 or .id == 4)'
```

`tools/list` should contain only `memory_status`, `memory_recall`, and
`memory_search`. The status tool's storage path should match the Codex database.

## Restart and recover safely

Restarting this home should preserve its database and imported recall:

```bash
md restart
md status | jq '{storage_path: (.data.storage.path // null), process: (.process // null), database: (.db // null)}'
md recall --profile personal --workspace dakota --query "What do my native Codex memories say about this project?"
```

If startup reports that the port is already in use, inspect this home, stop only
the daemon selected by this wrapper, and make a verified backup before changing
the endpoint. Do not delete the database or PID file by hand.

```bash
md status | jq --arg db "$MEMORYD_CODEX_HOME/memory.db" \
  '{selected_database: $db, endpoint_database: (.data.storage.path // null), matches_selected: (.data.storage.path == $db)}'
md logs --lines 80
md down
md --local backup create --dest "$MEMORYD_CODEX_HOME/backups/pre-recovery.db"
md --local backup verify --path "$MEMORYD_CODEX_HOME/backups/pre-recovery.db"
```

If the endpoint database does not match the selected database, do not run
`sync-local`; a different process owns that port. Choose another free port and
reinitialize this home first.

After checking a new port, update the selected home's config/runtime files with
`init`, then start and verify it. `init` reuses an existing database; confirm
the resolved path before proceeding.

```bash
MEMORYD_CODEX_PORT=8991
python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 8991)); s.close(); print("127.0.0.1:8991 is available at check time")'
md init --host 127.0.0.1 --port "$MEMORYD_CODEX_PORT" \
  --bind "127.0.0.1:$MEMORYD_CODEX_PORT"
md paths --format json | jq '{home: .entries.runtime_home.path, database: .entries.database.path, bind: .bind.value}'
md up
md status | jq --arg db "$MEMORYD_CODEX_HOME/memory.db" -e '.data.status == "local_only" and .data.storage.path == $db'
```

To stop only the Codex instance, run `md down`. The database remains at the
resolved path for the next startup. Friday's daemon and files do not appear in
this selected home's paths or lifecycle commands.
