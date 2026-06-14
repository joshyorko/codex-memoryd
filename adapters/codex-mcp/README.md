# Codex MCP adapter package

This package connects Codex to `codex-memoryd` through local MCP stdio. It is
read-only by default and exposes no write tools.

## Install

1. Build or install `codex-memoryd`.
2. Copy `.env.example` to a private env file and set absolute paths.
3. Render or adapt [`templates/config.toml`](./templates/config.toml) into
   `~/.codex/config.toml`.

The template intentionally uses `--read-only` and `enabled_tools` limited to
`memory_status`, `memory_recall`, and `memory_search`.

## Verify

```sh
codex mcp get codex_memoryd
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  | "$CODEX_MEMORYD_BIN" --db "$CODEX_MEMORYD_DB" mcp stdio --read-only
```

Expected result: the tool list contains only `memory_status`,
`memory_recall`, and `memory_search`.

## Uninstall

Remove the `[mcp_servers.codex_memoryd]` block from `~/.codex/config.toml`.
No memory data is deleted.
