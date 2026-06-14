# MCP v2 Surface

`codex-memoryd` exposes MCP as an adapter surface over the existing local-first
daemon semantics. MCP does not become a separate source of truth and does not
get adapter-specific write policy.

## Local stdio

Local stdio is the default safe path:

```bash
codex-memoryd --db ~/.codex-memoryd/memory.db mcp stdio
```

`mcp stdio` defaults to the read-only tier. `--read-only` is accepted as an
explicit no-write adapter marker:

```bash
codex-memoryd --db ~/.codex-memoryd/memory.db mcp stdio --read-only
```

Read-only tools:

- `memory_status`
- `memory_recall`
- `memory_search`

This tier is the only tier intended for current Codex sandbox dogfood.

## Write tier

Write tools are exposed only with an explicit opt-in:

```bash
codex-memoryd --db ~/.codex-memoryd/memory.db mcp stdio --write-tools
```

Write-capable tools:

- `memory_create`
- `memory_conclude`
- `memory_checkpoint`
- `memory_import_preview`
- `memory_import_apply`

`memory_import_preview` never writes durable records. `memory_import_apply`
uses the same sync/import service path as the CLI and HTTP API, including
idempotency, policy screening, profile/workspace scoping, and provenance.

`memory_create` and `memory_conclude` both write through the existing
conclusions service. Policy-denied content is returned in structured
`rejected` entries rather than being stored.

## Tool schemas

Tool list responses include MCP annotations:

- read tools set `readOnlyHint = true`
- write tools set `readOnlyHint = false`
- import preview sets `destructiveHint = false`
- import apply sets `destructiveHint = true`

The write-tier schema snapshot is checked in at
[`tests/fixtures/mcp_tools.write.json`](../tests/fixtures/mcp_tools.write.json)
and is verified by `cargo test --test mcp_stdio`.

## Recall authority

MCP recall and search responses preserve normal `codex-memoryd` semantics:
memory is `recall_not_authority`. User instructions, repository files, current
tool output, and test results override recalled memory. Responses carry the
same structured content as the service APIs, including evidence and provenance
metadata where those APIs provide it.

## Self-hosted proxy path

The supported remote shape is a small self-hosted MCP proxy that runs next to a
`codex-memoryd` daemon and forwards to local loopback or stdio. The proxy must
own:

- authentication
- network TLS
- client identity
- capability policy
- rate limits and audit logs
- read-only versus write-tier selection

Do not publish the raw local daemon or a write-tier MCP process directly to the
internet. A remote deployment that only exposes the read-only tier can be
reviewed independently from one that exposes `--write-tools`.

## Capability gate expectation

Issue #52 is the long-term capability gate. Until that lands as a first-class
policy model, the MCP adapter uses the narrowest enforceable gate:

- default: read-only
- write tier: explicit `--write-tools`
- Codex sandbox configs: read-only tools only

When #52 lands, `--write-tools` should become necessary but not sufficient:
the adapter should also require the configured client capability policy to
allow each write tool.
