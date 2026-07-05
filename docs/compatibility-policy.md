# Compatibility policy

`codex-memoryd` exposes several contracts that agents and operators depend on:
the HTTP `/v1` responses, the CLI command surface and its JSON output, the MCP
tool registry, the eval report shapes, adapter export JSON/Markdown, the
backup/restore manifest, and the `doctor` report. This document defines what
counts as an **additive** change (allowed at any time) versus a **breaking**
change (requires a version note and deprecation), and points at the snapshot
tests that enforce it.

This is a v0.1 policy: it documents intent and the guardrails that exist today.
It does not promise long-term API stability beyond what is written here.

## Compatibility levels

| Surface | Contract | Enforced by |
| --- | --- | --- |
| HTTP `/v1` responses | Response envelope + documented body keys | `tests/contract_snapshots.rs`, `tests/contract_fixtures.rs`, `tests/http_smoke.rs` |
| CLI flags / output | Subcommand names, flags, and JSON shapes | `tests/cli_smoke.rs`, `tests/contract_snapshots.rs` |
| MCP tool registry | Read-only and write tool name sets + schemas | `tests/contract_snapshots.rs` (`mcp::READ_ONLY_TOOL_NAMES` / `WRITE_TOOL_NAMES`), `tests/mcp_stdio.rs` |
| Eval reports | `eval substrate`, `eval substrate --compare`, `eval procedures` JSON | `tests/contract_snapshots.rs` |
| Adapter export | JSON/Markdown metadata + context-pack budget | `tests/contract_snapshots.rs`, `tests/adapter_conformance.rs`, `tests/conformance.rs` |
| Backup/restore | Manifest JSON shape + manifest version | `tests/contract_snapshots.rs`, `tests/backup_restore.rs` |
| Doctor report | Section keys for diagnostics | `tests/contract_snapshots.rs`, `tests/cli_smoke.rs` |
| Storage schema | `STORAGE_SCHEMA_VERSION` + lossless upgrade | `tests/schema_upgrade.rs` |

## Additive changes (allowed)

These do not break existing consumers and may land at any time:

- **Adding a new key** to a response object, eval report, manifest, or doctor
  section. Consumers must ignore unknown keys.
- **Adding a new CLI subcommand or a new optional flag** with a default.
- **Adding a new MCP tool.** (Update the registry contract test's count and the
  table above so the addition is intentional, not accidental.)
- **Adding a new adapter target**, eval fixture family, or pack mode.
- **Adding a new enum/string value** in an open-ended field (e.g. a new
  procedure `state`, a new policy reason code), provided existing values keep
  their meaning. Consumers must tolerate unknown values.
- **Adding a new storage table or column.** The store back-fills columns
  idempotently on open; old databases continue to upgrade losslessly.

## Breaking changes (require a version note + deprecation)

These break existing consumers and must not land silently:

- **Removing or renaming** a documented response key, CLI flag/subcommand, MCP
  tool, manifest field, or doctor section.
- **Changing the type** of an existing field (e.g. string → object, scalar →
  array) or the meaning of an existing enum value.
- **Tightening acceptance** so previously-valid input is now rejected, or
  **changing a default** that alters output shape.
- **Removing a storage column** older binaries read, or making a schema change
  that an old database cannot upgrade through.
- **Changing `recall_not_authority`** semantics, or any safety invariant
  (profile boundary deny-by-default, quarantine withholding, secret rejection).
  These are load-bearing and are not negotiable additive surface.
- **Making public memory handles carry location or authority.** Public `mr_*`,
  `msrc_*`, `msub_*`, `mep_*`, and `mcp_*` values are opaque presentation
  handles only. They must not embed filesystem paths, URLs, tenants, object
  keys, query fragments, class names, or secret selectors.

A breaking change requires: a note in the changelog/release gate, a bump of the
relevant version field (`manifest_version`, eval report `version`,
`STORAGE_SCHEMA_VERSION`, or the package version as appropriate), and — where
practical — a deprecation period in which the old shape still works.

## Deprecation guidance

When a field or command must change incompatibly:

1. Add the new shape alongside the old (additive) and document both.
2. Mark the old shape deprecated in its doc/help text and the release notes.
3. Keep the old shape working for at least one minor release.
4. Remove it only in a release whose notes call out the removal, and update the
   matching snapshot test in the same change so the break is explicit.

## How the snapshot tests work

`tests/contract_snapshots.rs` builds each contract surface through the real
service/struct APIs and asserts the **required key set** is present — it does
not assert exact values (timestamps, ids, and counts vary legitimately). That
means:

- Adding a key keeps the tests green (additive).
- Removing or renaming a key fails the matching test with a message pointing at
  this policy — turning an accidental break into a visible, intentional choice.

The MCP registry test additionally pins the read-only tool **count**, so adding
a read-only tool is a deliberate one-line update rather than a silent change.

## Memory Handle ADR

`codex-memoryd` now treats outbound memory pointers as a security boundary:

- Public memory-facing IDs use a fixed opaque grammar with allowlisted
  prefixes: `mr_`, `msrc_`, `msub_`, `mep_`, and `mcp_`.
- The suffix is a deterministic one-way digest over a server-side identifier.
  Clients may validate the grammar, but they cannot derive storage location or
  authority from the handle itself.
- Read surfaces may return attacker-controlled inert data, but they must not
  return storage paths or raw source identifiers as dereference hints.
- Full dereference grants and alias mutation authorization remain deferred. This
  slice only hardens the invariant that pointer IDs themselves are inert and
  non-authoritative.
