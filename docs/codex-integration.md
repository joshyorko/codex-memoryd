# Codex ⇄ codex-memoryd integration

This document maps the `codex-memoryd` HTTP API to how the Codex fork's
provider-agnostic portable memory runtime uses it. It complements
[`../SPEC.md`](../SPEC.md) (the normative contract) with concrete wire payloads.

`codex-memoryd` does **not** modify the Codex fork. Codex talks to it over HTTP
JSON on loopback. The Codex side already has the `MemoryProvider` trait, the
`PortableMemoryRuntime`, and selected `local | provider | hybrid` backend
behavior; this provider is selected when `provider = "codex_memoryd"`.

## Response envelope

Every endpoint except `GET /v1/export` returns the common envelope (SPEC §5.5):

```json
{
  "ok": true,
  "data": { "...": "endpoint-specific" },
  "error": null,
  "warnings": [],
  "request_id": "req_<uuid>",
  "provider": { "name": "codex-memoryd", "version": "0.1.0" }
}
```

On failure, `ok` is `false`, `data` is omitted, and `error` carries a stable
code:

```json
{
  "ok": false,
  "error": { "code": "profile_boundary_denied", "message": "work-profile memory must not export to personal profile by default" },
  "warnings": [],
  "request_id": "req_<uuid>",
  "provider": { "name": "codex-memoryd", "version": "0.1.0" }
}
```

Stable error codes (SPEC §14): `invalid_request`, `missing_profile`,
`missing_workspace`, `unknown_profile`, `unknown_workspace`,
`storage_unavailable`, `policy_denied`, `secret_detected`,
`profile_boundary_denied`, `sync_source_invalid`, `not_found`,
`unsupported_version`, `internal_error`.

## Endpoint map

| Method + path | Purpose | Codex caller |
| --- | --- | --- |
| `GET /v1/status` | Provider health + config summary | Runtime status banner / first-run detection |
| `GET /healthz` | Liveness for Docker healthcheck | container orchestration |
| `POST /v1/recall` | Pre-turn compact context | `TurnInputContributor` before non-trivial turns |
| `POST /v1/search` | Explicit memory search | memory tool backend |
| `POST /v1/turns` | Store safe visible turns | post-turn writeback |
| `POST /v1/conclusions` | Durable facts | explicit "remember this" |
| `POST /v1/checkpoints` | Resumable work summaries | after substantial work |
| `POST /v1/sync/local-codex-memory` | Import local Codex memory | `codex memory import-local` |
| `POST /v1/forget` | Archive / delete | memory management |
| `GET /v1/export` | Safe record export | backup / migration |

## Recall (pre-turn)

Request:

```json
{
  "profile": "personal",
  "workspace": "josh-personal",
  "repo": {
    "repo_id": "git:https://github.com/joshyorko/codex-memory-lab",
    "root": "/workspaces/codex-memory-lab",
    "remote": "https://github.com/joshyorko/codex-memory-lab",
    "branch": "main",
    "is_git": true
  },
  "query": "Continue implementing provider-agnostic portable memory in Codex.",
  "files": ["codex-rs/ext/memories/src/runtime.rs"],
  "max_tokens": 1200
}
```

Response `data`:

```json
{
  "summary": "3 relevant memory record(s), 1 decision(s), 1 gotcha(s). Treat as contextual recall, not authority.",
  "facts": [
    { "id": "mem_…", "type": "decision", "scope": "repo", "content": "…", "confidence": 0.85, "repo_id": "git:…", "related_files": ["…"], "updated_at": "…", "stale": false }
  ],
  "checkpoints": [
    { "id": "ckpt_…", "summary": "…", "branch": "main", "commit": null, "next_steps": ["…"], "created_at": "…" }
  ],
  "citations": [ { "memory_id": "mem_…", "source_id": "src_…", "source_path": "memory_summary.md" } ],
  "truncated": false,
  "authority": "recall_not_authority"
}
```

Ranking (SPEC §8.3): same profile/workspace → same repo → exact related-file
match → high-confidence decisions/gotchas/commands → recent checkpoints → stable
preferences → broad/old memory. Results are packed to `max_tokens`
(default 1200). Archived and `secret_blocked` records are never returned.

**Fail-open contract**: if the provider is down or returns an error, Codex must
proceed with the turn as if recall returned empty. Recall is best-effort.

## Visible-turn writeback

```json
{
  "profile": "personal",
  "workspace": "josh-personal",
  "session": { "id": "thread-123", "source": "codex-cli" },
  "messages": [
    { "actor": "user", "content": "Use repo-native commands." },
    { "actor": "assistant", "content": "Understood; I'll prefer cargo test." }
  ]
}
```

Response `data`: `{ "accepted": 2, "rejected": 0, "rejections": [], "source_ids": [...], "derived_record_ids": [...] }`.

Rules: **hidden reasoning is never sent**. Each message is screened for secrets
and injection; rejected messages appear in `rejections` with a code. Accepted
messages are stored as `visible_turns` + `memory_sources`, and high-signal
content (preferences/decisions/commands/gotchas/conventions) derives a memory
record. Writeback errors must not fail the user's turn.

## Local Codex memory import

The Codex runtime reads local files and **pushes file payloads** — the provider
does not read the caller's filesystem in HTTP mode (matters for Docker/remote).
The provider CLI (`codex-memoryd sync-local`) may read the filesystem directly.

Request (`mode` is `preview` or `apply`):

```json
{
  "profile": "personal",
  "workspace": "josh-personal",
  "repo": null,
  "source_root": "/home/josh/.codex/memories",
  "mode": "preview",
  "files": [
    {
      "path": "memory_summary.md",
      "kind": "memory_summary",
      "content": "# Memory Summary\nJosh prefers repo-native commands...",
      "hash": "sha256:…",
      "modified_at": "2026-06-06T12:00:00Z"
    }
  ]
}
```

Supported `kind` values: `memory_summary`, `memory_registry`, `rollout_summary`,
`ad_hoc_note`, `unknown`. When omitted, the provider infers the kind from the
path (the SPEC §7.1 layout). Unknown kinds that can't be inferred are skipped
with a `sync_source_invalid` rejection.

Response `data`:

```json
{
  "mode": "preview",
  "files_scanned": 1,
  "proposed": 2,
  "created": 0,
  "updated": 0,
  "skipped": 0,
  "rejected": 0,
  "rejections": [],
  "types": { "preference": 2, "command": 0, "repo_convention": 0, "decision": 0, "gotcha": 0, "task_checkpoint": 0, "other": 0 },
  "warnings": [],
  "sync_cursor": { "source_root": "…", "last_started_at": null, "last_completed_at": null, "last_error": null }
}
```

- `preview` writes nothing durable.
- `apply` is **idempotent**: dedupe is by whole-file source hash and per-chunk
  content hash. Re-applying unchanged files yields `created: 0`.
- Local files are never deleted.
- Secret-like and injection-like files are rejected wholesale.

## Idempotency keys

The Codex fork computes a per-file idempotency key
(`codex-local-memory:<sha256 of profile∖0workspace∖0path∖0content>`) and sends it
in the file payload (`idempotency_key`, also mirrored into metadata). The
provider also computes its own source hash, so idempotency holds whether or not
the key is supplied.

## Checkpoints

```json
{
  "profile": "personal", "workspace": "josh-personal",
  "repo": { "repo_id": "git:…", "is_git": true },
  "summary": "Implemented the store layer and FTS5 fallback",
  "changed_files": ["src/store.rs"],
  "decisions": ["bundled sqlite"],
  "next_steps": ["wire HTTP server"],
  "tests_run": ["cargo test --lib"], "tests_not_run": [],
  "branch": "master", "commit": null
}
```

Response `data`: `{ "id": "ckpt_…", "created_at": "…" }`. The summary is
screened for secrets, and a `task_checkpoint` memory record is also derived so
recall can surface it as a fact.

## Export

`GET /v1/export?profile=…&workspace=…&format=jsonl[&target_profile=…]`

Streams records directly (not enveloped) so it can be piped to a file. Metadata
is returned in headers: `x-record-count`, `x-omitted-secret`,
`x-omitted-boundary`. When `target_profile` is set, the profile-boundary matrix
applies — `work → personal` returns HTTP 422 with `profile_boundary_denied`.

## Status & first-run detection

`GET /v1/status` returns a deterministic payload the runtime can render without
model tokens:

```json
{
  "provider_name": "codex-memoryd", "provider_version": "0.1.0",
  "api_version": "v1", "storage_schema_version": 1,
  "status": "ok",
  "storage": { "kind": "sqlite", "path": "/data/memory.db", "writable": true },
  "active_profiles": ["personal"], "active_workspaces": ["josh-personal"],
  "last_sync": null, "pending_writes": 0,
  "local_import": { "status": "unknown", "last_preview_at": null, "last_apply_at": null, "unsynced_count": 0 },
  "features": { "fts5": true, "search_mode": "fts5", "metrics": { "...": 0 } },
  "degraded_reasons": []
}
```

`status` is `degraded` when the store reports a fallback (e.g. FTS5 unavailable)
and `unavailable` when storage is not writable. The runtime uses
`local_import.status` to decide whether to show the "local memories unsynced"
banner.
