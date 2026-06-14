# Generic MCP and markdown adapter package

This package is for Hermes/OpenClaw-style clients and markdown instruction
trees that can consume either MCP stdio config or generated markdown files.

## Install MCP

Add [`templates/mcp.json`](./templates/mcp.json) to the client MCP registry and
substitute the environment placeholders. Keep `--read-only` unless you are
building an explicitly reviewed write path.

## Install markdown exports

```sh
"$CODEX_MEMORYD_BIN" --db "$CODEX_MEMORYD_DB" adapter export \
  --target agents-md \
  --profile "$CODEX_MEMORYD_PROFILE" \
  --workspace "$CODEX_MEMORYD_WORKSPACE" \
  --format markdown > AGENTS.memory.md

"$CODEX_MEMORYD_BIN" --db "$CODEX_MEMORYD_DB" adapter export \
  --target markdown \
  --profile "$CODEX_MEMORYD_PROFILE" \
  --workspace "$CODEX_MEMORYD_WORKSPACE" \
  --format markdown > GEMINI.memory.md
```

[`templates/AGENTS.memory.md`](./templates/AGENTS.memory.md) and
[`templates/GEMINI.memory.md`](./templates/GEMINI.memory.md) provide static
wrappers for clients that need checked-in instruction tree entries.

## Verify

```sh
rg 'recall_not_authority|read-only' AGENTS.memory.md GEMINI.memory.md
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  | "$CODEX_MEMORYD_BIN" --db "$CODEX_MEMORYD_DB" mcp stdio --read-only
```

Expected MCP tools: `memory_status`, `memory_recall`, and `memory_search`.

## Uninstall

Remove the MCP registry entry and generated `*.memory.md` files. No memory data
is deleted.
