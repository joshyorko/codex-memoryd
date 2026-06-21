# Runtime UX plan: paths/status inventory and Codex MCP setup (#184, #181, #175, #183)

Status: first-wave plan. The recommended first implementation is read-only path inventory; Codex config mutation and adjacent app lanes should follow in separate PRs.

## Current surface

The CLI already has lifecycle commands (`init`, `up`, `down`, `logs`, `restart`, `upgrade`, `image build`, `serve`, `status`), direct memory commands, `config show --resolved`, `config env`, `config doctor`, MCP stdio, and Dreamer/patch commands.

Runtime path resolution is centralized through runtime options: home, URL/host/port/bind, database path, PID file, log file, runtime env, image/container metadata, and Codex memories directory. `init` creates the home, backups, exports, logs, config, runtime env, and DB.

## First slice: #184 paths/status inventory

Add a read-only inventory command before any config mutation:

```bash
codex-memoryd paths --format json
codex-memoryd paths --format summary
```

The JSON should list:

- `config_file` — durable user config.
- `runtime_home` — durable runtime root.
- `database` — durable SQLite store.
- `runtime_env` — durable/generated runtime env.
- `pid_file` — ephemeral process state.
- `log_file` — ephemeral/restart-surviving diagnostic file depending on runtime home retention.
- `backups_dir` — durable backups.
- `exports_dir` — durable exports.
- `codex_memories_dir` — external source directory used by `sync-local`.
- `url`, `bind`, and runtime kind.

Each path entry should include at least `path`, `kind` (`file`, `dir`, `endpoint`, or `external`), `durability` (`durable`, `ephemeral`, `generated`, or `external`), `exists`, and `owner` (`codex-memoryd` or `external`).

Keep `/v1/status` unchanged in the first implementation to avoid public contract churn.

## Second slice: #181 Codex MCP setup wizard

Add a target-specific wizard only after #184 lands:

```bash
codex-memoryd mcp codex preview
codex-memoryd mcp codex apply
codex-memoryd mcp codex status
codex-memoryd mcp codex remove
```

Rules:

- Preview writes nothing.
- Apply backs up the target config before mutation.
- Apply manages only an owned `codex-memoryd` MCP block.
- Remove deletes only that owned block.
- Unrelated Codex config is preserved byte-for-byte where practical.
- Generated MCP command defaults to `mcp stdio --read-only` and exposes only `memory_status`, `memory_recall`, and `memory_search`.
- Output uses resolved runtime values and points back to `codex-memoryd paths` for diagnostics.

## Later slices

- #175 broader CLI-first runtime UX should close after paths and MCP wizard prove the user can inspect and wire the native binary path without Compose.
- #183 adjacent app lane should be disabled by default, explicit in config, reported separately from core daemon lifecycle, and tested for endpoint conflicts/no-op default behavior.

## File and test boundaries

- Avoid storage internals (`src/store.rs`, migrations, backup behavior).
- Avoid `/v1/status` protocol changes until a dedicated contract update PR.
- Implement CLI-heavy changes one command family at a time to avoid conflicts in `src/cli.rs`.
- Tests should use temp homes/config files and cover path resolution, preview writes-nothing, apply idempotency, removal, and preservation of unrelated config.
