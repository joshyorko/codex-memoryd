# codex-memoryd adapter packages

These packages install thin host-specific adapters around the same
`codex-memoryd` substrate. They do not contain memory logic, hidden writeback,
or a host-specific database.

## Packages

| Package | Host target | Installed surface |
| --- | --- | --- |
| [`codex-mcp`](./codex-mcp/README.md) | Codex MCP config | `~/.codex/config.toml` server snippet |
| [`claude-local`](./claude-local/README.md) | Claude-style local MCP config | local MCP server JSON snippet |
| [`copilot-instructions`](./copilot-instructions/README.md) | GitHub Copilot | generated instructions markdown |
| [`generic-mcp-markdown`](./generic-mcp-markdown/README.md) | Hermes/OpenClaw-style clients and markdown trees | MCP JSON plus `AGENTS.md` / `GEMINI.md` exports |

## Shared contract

- Default mode is read-only.
- MCP adapters expose only `memory_status`, `memory_recall`, and
  `memory_search`.
- Write-capable MCP tools require an explicit `mcp stdio --write-tools`
  process plus a reviewed adapter capability policy. Do not expose write tools
  from generic Codex sandbox configs.
- Recall remains `recall_not_authority`; user instructions, repository files,
  and current tool output override memory.
- Packages point at an existing `codex-memoryd` binary and database; they do not
  copy or fork memory semantics.
- Export packages use `codex-memoryd adapter export` so safety gates, profile
  boundaries, and redaction stay centralized in the daemon.

## Shared environment

Every package includes a `.env.example` with these common settings:

```sh
CODEX_MEMORYD_BIN=/absolute/path/to/codex-memoryd
CODEX_MEMORYD_DB=/absolute/path/to/memory.db
CODEX_MEMORYD_PROFILE=personal
CODEX_MEMORYD_WORKSPACE=default
CODEX_MEMORYD_BASE_URL=http://127.0.0.1:8787
CODEX_MEMORYD_READ_ONLY=true
```

Use profile and workspace values that match the memory boundary you intend to
expose. Do not point a personal adapter at a work database unless that boundary
has been reviewed explicitly.
