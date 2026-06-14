# Claude local MCP adapter package

This package provides a Claude-style local MCP server config. It is a thin
wrapper around the `codex-memoryd mcp stdio --read-only` entrypoint.

## Install

1. Build or install `codex-memoryd`.
2. Copy `.env.example` to your local adapter env file.
3. Add [`templates/mcp-server.json`](./templates/mcp-server.json) to the host
   client's local MCP server configuration.

## Verify

Run the same command from the template:

```sh
"$CODEX_MEMORYD_BIN" --db "$CODEX_MEMORYD_DB" mcp stdio --read-only
```

Then issue a `tools/list` request from the host client. Expected tools:
`memory_status`, `memory_recall`, and `memory_search`.

## Uninstall

Remove the `codex-memoryd` server entry from the host client's MCP config.
No memory data is deleted.
