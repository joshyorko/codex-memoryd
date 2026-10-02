# MCP adapter v2

“MCP v2” is the name of MemoryD's internal adapter/tool-tier generation from
issue #81. It is **not** an official MCP wire-protocol revision. The wire
revisions and transport implemented by this adapter are listed separately
below.

## Wire revisions and lifecycle

The current MCP specification is [`2026-07-28`](https://modelcontextprotocol.io/specification/2026-07-28).
MemoryD supports it and the handshake-based revisions
[`2025-11-25`](https://modelcontextprotocol.io/specification/2025-11-25) and
[`2025-06-18`](https://modelcontextprotocol.io/specification/2025-06-18).
The 2026 revision is stateless: each request carries protocol version, client
identity, and client capabilities in `params._meta`, and clients may use
`server/discover` to obtain supported versions and capabilities. `initialize`
and `notifications/initialized` belong to the 2025-and-earlier lifecycle, not
the 2026 revision.

The server advertises `2026-07-28`, `2025-11-25`, and `2025-06-18`. A modern stdio process
uses per-request metadata and `server/discover`; a legacy process negotiates
the requested supported legacy revision, then must receive
`notifications/initialized` before serving tools. The `2025-11-25` initialize
response is the fallback for other legacy versions; that client must support
the returned revision or disconnect. This includes the legacy `2025-06-18`
mode currently selected by public Codex MCP client source. A 2026 client must use `server/discover`,
not `initialize`. A stdio process selects one lifecycle era and does not mix
modern and legacy requests.

The current SDK-level interop test uses the official Rust MCP SDK client
`rmcp 3.5.0` with its `2026-07-28` discovery lifecycle, then lists tools,
calls `memory_status`, and shuts down over stdio. It is not a hosted
Codex/ChatGPT/Tunnel canary. Public tunnel-client source version `0.0.15`
depends on Go MCP SDK `v1.7.0` and uses its 2026 discovery path and 2025-11-25
legacy fallback; this is source-level evidence, not a test of an installed
tunnel binary. Public Codex
source currently defaults to the 2025-06-18 legacy lifecycle, with
2026-07-28 behind an experimental opt-in. Neither public-source version
identifies the binaries installed in a particular hosted or local environment.
The tunnel client's `2026-08-25` control-plane wire header is a separate
version from MCP's `2026-07-28` protocol version.
References: [Codex protocol-mode source](https://github.com/openai/codex/blob/ca466061d64f0b44f416135c7fd06aa7af850bbc/codex-rs/rmcp-client/src/protocol_mode.rs),
[tunnel-client version source](https://github.com/openai/tunnel-client/blob/c8aeedec334db55bbd69bb16db6b71276993d708/pkg/version/VERSION),
and the [Go MCP SDK compatibility notes](https://github.com/modelcontextprotocol/go-sdk/blob/v1.7.0/README.md#version-compatibility).

## Stdio transport

The only MCP transport implemented here is local stdio:

```bash
codex-memoryd --db ~/.codex-memoryd/memory.db mcp stdio
```

Messages are UTF-8 JSON-RPC objects, one newline-delimited message per line.
Each incoming line is limited to 1 MiB. `stdout` is reserved for MCP
JSON-RPC; diagnostics go to `stderr`. EOF on stdin exits cleanly. An output
failure (including a disconnected client) is returned as an error rather than
reported as a successful session.

MemoryD does **not** implement MCP over Streamable HTTP or the deprecated
HTTP+SSE transport. The existing MemoryD HTTP API is not an MCP endpoint.
Secure MCP Tunnel is an external stdio consumer/adapter, not a transport
implemented by MemoryD.

## Tool tiers and safety

`mcp stdio` defaults to the read-only tier. `--read-only` is accepted as an
explicit no-write-tool marker:

```bash
codex-memoryd --db ~/.codex-memoryd/memory.db mcp stdio --read-only
```

Read-only tools:

- `memory_status`
- `memory_recall`
- `memory_search`

Write-capable tools require explicit `--write-tools`:

```bash
codex-memoryd --db ~/.codex-memoryd/memory.db mcp stdio --write-tools
```

- `memory_create`
- `memory_conclude`
- `memory_checkpoint`
- `memory_import_preview`
- `memory_import_apply`

`memory_import_preview` never writes durable records. `memory_import_apply`
uses the existing sync/import service path, including idempotency, policy
screening, profile/workspace scoping, and provenance. `memory_create` and
`memory_conclude` use the existing conclusions service; denied content is
returned as a rejected entry rather than stored.

Tool schemas reject unknown arguments. Read tools advertise `readOnlyHint`;
write tools advertise `readOnlyHint = false`, and import tools advertise their
respective `destructiveHint`. These MCP annotations are advisory metadata, not
authorization. The server-side `--write-tools` gate and existing service
policy remain authoritative.

Successful tool calls return text and `structuredContent`. Tool execution
failures use an MCP tool result with `isError = true`; malformed requests and
unknown methods use JSON-RPC errors with the originating request ID when it
can be read. Notifications, including unsupported notifications, never
receive a JSON-RPC response.

The write-tier schema snapshot remains at
[`tests/fixtures/mcp_tools.write.json`](../tests/fixtures/mcp_tools.write.json)
and is checked by `cargo test --test mcp_stdio`.

## Recall authority and storage boundary

MCP recall and search preserve the normal MemoryD contract:
`recall_not_authority`. Current user instructions, repository files, tool
output, and test results override recalled memory. Results retain the
structured content, evidence, and provenance provided by the service.

The stdio CLI currently opens its configured local store; it does **not**
attach to an already-running daemon's store owner. Protocol conformance does
not implement or claim daemon-backed attachment. That is separate work tracked
by [issue #247](https://github.com/joshyorko/codex-memoryd/issues/247).

## Compatibility and live evidence

| Check | MemoryD version | Client version | Result |
| --- | --- | --- | --- |
| Official Rust SDK stdio discovery, tool listing/call, and shutdown | `0.1.0` (`Cargo.toml`) | `rmcp 3.5.0` | **PASS**; automated `mcp_stdio` test |
| Hosted discovery and synthetic tool call through Secure MCP Tunnel | `0.1.0` | Runtime Codex/hosted-client and `tunnel-client` versions unavailable in this environment (public tunnel-client source is `0.0.15`) | **NOT RUN**; no hosted credentials or tunnel client |

A disconnected local stdout consumer was manually reproduced and causes the
stdio process to exit non-zero. The prior tunnel-specific broken-pipe failure
has not been reproduced because `tunnel-client` and the hosted client are
unavailable here; its cause remains **unresolved**. A successful SDK stdio test
does not establish why that earlier tunnel attempt closed its pipe.

For remote integrations, use a separately reviewed proxy that owns
authentication, TLS, client identity, rate limits, audit logging, and tool
policy. Do not publish the raw local daemon or a write-enabled stdio process
directly to the internet.
