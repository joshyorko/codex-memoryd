# Hosted App Adapter Design

> **Design slice for issue #82.**
> This document defines how a hosted assistant app can talk to a user's
> `codex-memoryd` server as an external memory source. It does not define a
> hosted `codex-memoryd` service, dashboard, or deployment platform.

## Outcome

Hosted assistant clients can use `codex-memoryd` as a read-only memory adapter
for status, recall, and search while keeping the daemon as the source of truth.
The hosted app supplies connection settings, calls the daemon over the existing
HTTP API, and treats returned memory as advisory context.

## Boundary

The MVP adapter is a client contract:

- `codex-memoryd` remains the memory server and source of truth.
- The hosted app stores adapter settings, not memory records.
- The hosted app does not apply patches, write memories, import local files, or
  export records in the MVP.
- No deployment, tunnel, reverse proxy, bearer-token issuer, dashboard, or
  platform-specific secret store is implied by this document.

If a hosted app cannot reach the user's `codex-memoryd` base URL, the assistant
must continue without memory context. Recall is useful context, not authority.

## Adapter Settings

Every hosted-app adapter instance needs these settings:

| Setting | Required | Description |
| --- | --- | --- |
| `base_url` | yes | HTTPS or loopback URL for the user's `codex-memoryd` server. Default local examples use `http://127.0.0.1:8787`; hosted apps normally need a user-managed authenticated HTTPS front door. |
| `profile` | yes | Memory profile, such as `personal`, `work`, `oss`, or `homelab`. |
| `workspace` | yes | Workspace id within the selected profile. |
| `mode` | yes | `read_only` for the MVP. Later modes may include `review_required` and `write_enabled` behind capability gates. |
| `credential_ref` | no | Reference to an app-managed credential or connector secret. The adapter must not require raw secrets in prompts, docs, fixtures, or tool payloads. |
| `pack_mode` | no | Optional recall pack mode, such as `default`, `planning`, `debugging`, or `review`. |
| `max_tokens` | no | Optional recall budget. The daemon remains responsible for deterministic truncation and reporting. |

The adapter should display `base_url`, `profile`, `workspace`, and `mode` during
setup and review so users can see which memory boundary is active.

## MVP Tool Surface

The first hosted-app pass exposes only read tools:

| Tool | Daemon endpoint | Capability | Purpose |
| --- | --- | --- | --- |
| `memory_status` | `GET /v1/status` | none for local status; future remote front doors may require transport auth | Check daemon health, configured profiles/workspaces, degraded reasons, and capability mode. |
| `memory_recall` | `POST /v1/recall` | `recall.read` | Fetch budgeted pre-turn context for the active profile/workspace. |
| `memory_search` | `POST /v1/search` | `search.read` | Search stored records explicitly for user-visible memory lookup. |

The hosted app must not expose write/apply/import/export/forget tools until the
daemon reports the required capabilities and the user has an interactive review
path. Tool declarations for the MVP should be "tool only": no UI widget is
required to use status, recall, or search.

## Request Shape

`memory_recall` maps hosted-app inputs to `POST /v1/recall`:

```json
{
  "profile": "personal",
  "workspace": "josh-personal",
  "query": "Continue the hosted app adapter design.",
  "files": ["docs/hosted-app-adapter.md"],
  "max_tokens": 1200,
  "pack_mode": "default",
  "metadata": {
    "adapter": "hosted-app",
    "mode": "read_only"
  }
}
```

`memory_search` maps hosted-app inputs to `POST /v1/search`:

```json
{
  "profile": "personal",
  "workspace": "josh-personal",
  "query": "hosted app adapter",
  "limit": 10,
  "include_archived": false
}
```

The hosted app may include a `repo` object when the client knows repository
identity. It should omit unknown repo fields instead of inventing them.

## Response Handling

The hosted app must respect the existing envelope:

- `ok = true` means `data` is usable.
- `ok = false` means the adapter should show or log the bounded error code and
  continue without memory.
- `warnings` are user-visible setup or degraded-state signals.
- `/v1/recall` output keeps `authority = "recall_not_authority"`.

The assistant should cite or summarize returned memory as context. It must not
claim the daemon's recall is current fact without verification when the memory
itself is stale, low-confidence, cross-boundary, or otherwise qualified.

Returned memory identifiers are opaque handles such as `mr_*` and `msrc_*`.
Hosted clients may treat those values as inert display references and may
validate their fixed grammar, but must not infer storage location, authority,
tenant, object key, or path semantics from them.

## Review And Approval Later

Memory review and patch approval are an interactive layer, not part of the MVP.
A later hosted app may add:

- a review queue for proposed observations, conclusions, and patches;
- a diff viewer for `patches.preview`;
- explicit approve/reject actions for `observations.apply`,
  `patches.apply`, `forget.archive`, and `forget.delete`;
- audit events that record decision metadata without hidden reasoning.

Those flows require capability gates before any write or apply action. A hosted
app must not infer write permission from successful status, recall, or search.

## Safety Rules

- Fail open: unavailable memory never blocks an assistant turn.
- Default to `read_only`.
- Require explicit capability gates before any mutation, export, apply, forget,
  or cross-profile flow.
- Treat all returned memory handles as non-bearer tokens. Possession of an
  `mr_*` string alone never grants dereference authority.
- Do not store `.env` dumps, private keys, auth files, raw confidential logs,
  encrypted reasoning, or giant tool output.
- Do not silently bridge work and personal profiles.
- Do not make private hosted-platform assumptions in core docs or fixtures.

## Local Acceptance Fixture

The fixture
[`../tests/fixtures/hosted_app_adapter.tool_only.json`](../tests/fixtures/hosted_app_adapter.tool_only.json)
captures the MVP tool-only contract. The contract test verifies that it only
declares status, recall, and search; that recall/search requests deserialize into
the daemon protocol types; and that the live service accepts those requests.
