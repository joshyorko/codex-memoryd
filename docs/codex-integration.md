# Codex ⇄ codex-memoryd integration

This document maps the `codex-memoryd` HTTP API to how the Codex fork's
provider-agnostic portable memory runtime uses it. It complements
[`../SPEC.md`](../SPEC.md) (the normative contract) with concrete wire payloads.

`codex-memoryd` does **not** modify the Codex fork. Codex talks to it over HTTP
JSON on loopback. The Codex side already has the `MemoryProvider` trait, the
`PortableMemoryRuntime`, and selected `local | provider | hybrid` backend
behavior; codex-memoryd is selected when `backend = "provider"` (or `"hybrid"`)
and `provider = "codex_memoryd"`.

> **Contract status.** The config shape and wire payloads in this document are
> the contract implemented by `joshyorko/codex@tap-release`. Older Codex PR #55
> snapshots only exposed the Honcho-shaped subset; see
> [Historical Codex-side delta from PR #55](#historical-codex-side-delta-from-pr-55)
> for the migration notes. This repo does not modify `codex/`.

## Local-only transport and auth contract

Supported direct daemon use is local-only:

- default bind `127.0.0.1:8787` (or `[::1]`/`localhost`) for same-host Codex;
- Docker Compose publishing `127.0.0.1:8787:8787` on the host while the daemon
  binds `0.0.0.0:8787` inside the container;
- `/healthz` stays a simple unauthenticated liveness endpoint.

There is currently no bearer-token, tenant isolation, or production remote auth
implementation for `codex-memoryd`. If `/v1/*` is reachable on a non-loopback
interface, that deployment is unsupported unless an external authenticated tunnel
or reverse proxy supplies the missing protection. In that case `/v1/status`
reports `status = "auth_missing"` with a warning instead of implying remote
production safety.

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
| `POST /v1/dream` | Run Dreamer preview/apply | local scheduling and service callers |
| `POST /v1/sync/local-codex-memory` | Import local Codex memory | `codex memory import-local` |
| `POST /v1/forget` | Archive / delete | memory management |
| `GET /v1/export` | Safe record export | backup / migration |

Dreamer integration is implemented for local use:
- `codex-memoryd dream --preview` and `codex-memoryd dream --apply` are active.
- Service entrypoints `Service::dream` and `Service::scheduled_dream` are wired from
  `src/service.rs`.
- `POST /v1/dream` is implemented in `src/server.rs`.
- `src/dream.rs` contains the core pipeline.
- Store durability now includes `dream_runs` audit rows and watermark tracking.
- `/v1/status` includes the last Dreamer run result and scheduler state.
- Dream reports now expose a first-class `evidence_window` with per-stream counts
  and safe source refs for visible turns, conclusions, checkpoints, imported
  memory sources, and active memory records. The audit row reuses that bundle via
  the existing `source_counts` JSON column.

Dreamer output is recall input, not authority: it proposes evidence-backed
candidate memories from safe visible turns, conclusions, checkpoints, and imported
local memories, then requires preview and policy-gated apply before durable records
change. MCP Dreamer tooling remains unfinished, and the loop is not yet fully
productized.

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
  "max_tokens": 1200,
  "pack_mode": "default"
}
```

Response `data`:

```json
{
  "summary": "3 relevant memory record(s), 1 decision(s), 1 gotcha(s). Treat as contextual recall, not authority.",
  "authority": "recall_not_authority",
  "policy": {
    "authority": "recall_not_authority",
    "admission_gates": ["profile_workspace"],
    "ranking_signals": ["profile_workspace", "repo_match", "file_match", "query_match", "pack_mode:default"]
  },
  "pack": {
    "mode": "default",
    "template": "default",
    "template_budget_tokens": 1200,
    "max_tokens": 1200,
    "used_tokens": 830,
    "candidate_count": 5,
    "admitted_count": 3,
    "withheld_count": 2,
    "truncated": false
  },
  "facts": [
    {
      "id": "mem_…",
      "type": "decision",
      "scope": "repo",
      "content": "…",
      "confidence": 0.85,
      "repo_id": "git:…",
      "related_files": ["…"],
      "updated_at": "…",
      "stale": false,
      "policy": {
        "rank": 1,
        "freshness": { "stale": false, "age_days": 2 },
        "provenance": {
          "profile_id": "personal",
          "workspace_id": "josh-personal",
          "repo_id": "git:…",
          "evidence_refs": ["src_…"],
          "subject_id": null,
          "episode_id": null,
          "source_risk": "medium",
          "trust_level": "high"
        },
        "admission": {
          "decision": "admitted",
          "reason": "admitted_ranked",
          "gates": ["profile_workspace"]
        },
        "ranking_signals": ["repo_match", "query_match"]
      }
    }
  ],
  "checkpoints": [
    { "id": "ckpt_…", "summary": "…", "branch": "main", "commit": null, "next_steps": ["…"], "created_at": "…" }
  ],
  "citations": [ { "memory_id": "mem_…", "source_id": "src_…", "source_path": "memory_summary.md" } ],
  "withheld": [
    { "reason": "secret_blocked", "count": 1, "gates": ["secret_blocked"] },
    { "reason": "policy_quarantined", "count": 1, "gates": ["admission_policy", "quarantine"] },
    { "reason": "policy_high_risk", "count": 1, "gates": ["admission_policy", "source_risk"] },
    { "reason": "policy_unsafe", "count": 1, "gates": ["admission_policy", "unsafe"] },
    { "reason": "policy_superseded", "count": 1, "gates": ["admission_policy", "supersession"] },
    { "reason": "pack_truncated", "count": 2, "gates": ["max_tokens", "result_limit"] }
  ],
  "truncated": false
}
```

For compatibility, existing fields (`summary`, `facts`, `checkpoints`, `citations`, `truncated`, and legacy `authority`) remain valid and unchanged in meaning. New top-level `policy`, `pack`, and per-fact `policy` objects are additive metadata used for diagnostics, pack selection, and ranking auditability.
`facts[].policy.provenance.source_risk` and `facts[].policy.provenance.trust_level` are additive diagnostics derived from existing record sensitivity and source metadata; they do not affect ranking, storage, or admission.
`facts[].policy.admission` explains why an emitted item was admitted. `withheld`
is optional and reports deterministic counts by gate only; it never includes raw
withheld content.
Default recall withholds records whose metadata marks them quarantined,
high-risk/unsafe, rejected/blocked, or superseded. Stale active records remain
admissible but carry stale/deprioritized admission metadata. Cross-profile recall
is default-deny: a personal recall request does not return work records, and vice
versa, without an explicit future policy exception.

Ranking (SPEC §8.3): same profile/workspace → same repo → exact related-file
match → high-confidence decisions/gotchas/commands → recent checkpoints → stable
preferences → broad/old memory. Results are packed to the lower of request
`max_tokens` and the selected template budget. Optional `pack_mode` accepts
`default`, `debugging`, `onboarding`, `planning`, `active_task`, `review`, and
`personal_context`; hyphenated aliases such as `active-task` are normalized.
Unknown modes return `invalid_request`. Archived and `secret_blocked` records
are never returned.

Pack reports include deterministic budget/truncation counters:
`template_budget_tokens`, effective `max_tokens`, estimated `used_tokens`,
`candidate_count`, `admitted_count`, `withheld_count`, and `truncated`.

**Fail-open contract**: if the provider is down or returns an error, Codex must
proceed with the turn as if recall returned empty. Recall is best-effort and must
not block the user path.

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
and injection; rejected messages appear in `rejections` with a code and bounded
reason, never with the raw rejected content. Accepted messages are stored as
`visible_turns` + `memory_sources`, and high-signal content
(preferences/decisions/commands/gotchas/conventions) derives a memory record.
Writeback errors must not fail the user's turn.

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
  "api_version": "v1", "storage_schema_version": 2,
  "status": "local_only",
  "storage": { "kind": "sqlite", "path": "/data/memory.db", "writable": true },
  "active_profiles": ["personal"], "active_workspaces": ["josh-personal"],
  "last_sync": null, "pending_writes": 0,
  "local_import": { "status": "unknown", "last_preview_at": null, "last_apply_at": null, "unsynced_count": 0 },
  "features": { "fts5": true, "search_mode": "fts5", "exposure": "local_only", "auth": "none", "metrics": { "...": 0 } },
  "degraded_reasons": []
}
```

`status` values are:

- `local_only`: healthy storage and loopback-only exposure (the normal local
  default);
- `auth_missing`: non-loopback bind/exposure without built-in auth; unsupported
  for production remote use;
- `degraded`: usable storage with a fallback such as LIKE search instead of
  FTS5;
- `unavailable`: storage is not writable or the provider cannot serve memory;
- `ok` / `auth_required`: reserved for a future authenticated remote mode.

The runtime uses `local_import.status` to decide whether to show the "local
memories unsynced" banner.

## Config contract & compatibility matrix

Final `[memories]` shape (canonical target):

```toml
[memories]
backend = "provider"              # local | provider | hybrid
provider = "codex_memoryd"        # honcho | codex_memoryd  (when backend != local)
provider_url = "http://127.0.0.1:8787"
profile = "personal"
workspace = "josh-personal"
local_import_policy = "prompt"    # prompt | manual | startup_preview | startup_apply
write_policy = "visible_turns"    # off | visible_turns
sync_policy = "manual"            # manual | startup
cross_profile_policy = "default_deny"
```

| `backend` | `provider` | Durable store | Provider HTTP target | Local memory role |
| --- | --- | --- | --- | --- |
| `local` | — (ignored) | none (upstream local only) | — | source of truth |
| `provider` | `honcho` | Honcho | Honcho v3 base URL | import source only |
| `provider` | `codex_memoryd` | codex-memoryd SQLite | `provider_url` → `/v1` | import source only |
| `hybrid` | `honcho` | Honcho + local cache | Honcho v3 base URL | cache / debug / rebuild |
| `hybrid` | `codex_memoryd` | codex-memoryd SQLite + local cache | `provider_url` → `/v1` | cache / debug / rebuild |

Field defaults and meanings are normative in [`../SPEC.md` §11.1](../SPEC.md).

## Live tap-release smoke

The live smoke script is proof tooling only; it is not part of normal
`cargo test`, and it does not make this repository clone or build the Codex fork
unless you choose to do that as part of preparing `CODEX_BIN`.

```bash
# One-time setup outside this repository, or use any existing tap-release binary.
git clone --branch tap-release https://github.com/joshyorko/codex /tmp/codex-tap-release
cargo build --manifest-path /tmp/codex-tap-release/codex-rs/Cargo.toml \
  -p codex-cli --bin codex

# From this repository:
CODEX_BIN=/tmp/codex-tap-release/codex-rs/target/debug/codex \
  scripts/codex-tap-release-smoke.sh
```

The script starts `codex-memoryd` on `http://127.0.0.1:8787`, isolates
`CODEX_HOME` under a temporary directory, configures Codex with:

```toml
[memories]
backend = "provider"              # then repeated with "hybrid"
provider = "codex_memoryd"
provider_url = "http://127.0.0.1:8787"
write_policy = "visible_turns"
local_import_policy = "manual"
```

It captures pasteable output for:

- `/v1/status`;
- `/v1/conclusions`;
- `/v1/recall`, including `authority = "recall_not_authority"`;
- `/v1/turns` accepted/rejected writeback counts;
- `/v1/sync/local-codex-memory` preview, apply, and second apply
  idempotency;
- Codex `memory status`, `debug prompt-input`, and `memory import-local` in
  `provider` mode;
- Codex `memory status` and `debug prompt-input` in `hybrid` mode;
- daemon-down fail-open behavior, where the hybrid prompt build must still exit
  successfully.

The final line prints the temporary `smoke-output.txt` path so it can be pasted
into an OpenAI-facing demo or issue comment.

## Historical Codex-side delta from PR #55

PR #55 is the foundation (config struct, `PortableMemoryRuntime`,
`MemoryProvider` trait, `local | honcho | hybrid` selection, turn-input recall,
turn-item writeback, the `/v1/sync/local-codex-memory` endpoint constant). To
honor the final shape above, the following codex-side changes are required.
**These are codex-side work items, tracked here for coordination; this repo does
not implement them** (no edits to `codex/`).

1. **`MemoryBackendKind`** (`config/src/types.rs`): change variants from
   `Local | Honcho | Hybrid` to `Local | Provider | Hybrid`. Accept legacy
   `honcho` as an alias that normalizes to `backend = "provider"`,
   `provider = "honcho"` (SPEC §11.1.1 compatibility note) so existing configs
   keep loading.
2. **New `MemoryProvider` enum + `provider` field** on `MemoriesToml` /
   `MemoriesConfig`: `honcho | codex_memoryd`, used to pick the concrete client
   when `backend != local`.
3. **`provider_url` field**: a general provider endpoint. Map the existing
   `honcho_base_url` onto it when `provider = "honcho"` for back-compat.
4. **`local_import_policy` field**: `prompt | manual | startup_preview |
   startup_apply` (SPEC §7.4), driving first-run import behavior.
5. **A `codex_memoryd` HTTP client** implementing the `MemoryProvider` trait
   against this daemon's `/v1` API (envelope-aware), selected in `selected.rs`'s
   `portable_provider_for_settings` when `provider = "codex_memoryd"`. Today both
   `Honcho` and `Hybrid` route to the Honcho v3 client only.

These items have landed on `joshyorko/codex@tap-release`; they remain here as
historical migration notes for anyone comparing against old PR #55 checkouts.
The fixtures in [`../tests/fixtures`](../tests/fixtures) let the Codex side
build and test that client against this exact contract.

### Request/response shapes the codex-side client must produce

These match the daemon's protocol types (all request fields are optional /
defaulted server-side; a minimal session is accepted):

- **status**: `GET /v1/status` → envelope wrapping the status object above.
- **recall**: `POST /v1/recall` with `{ profile, workspace, repo?, query,
  files?, max_tokens?, pack_mode? }` → envelope wrapping `{ summary, facts[],
  checkpoints[], citations[], truncated, authority, policy, pack }`.
- **turns**: `POST /v1/turns` with `{ profile, workspace, session{ id }, messages[
  { actor, content } ] }` → envelope wrapping `{ accepted, rejected,
  rejections[], source_ids[], derived_record_ids[] }`. The Codex
  `VisibleMemoryMessage { actor, content, metadata }` maps directly onto a
  message entry.
- **sync**: `POST /v1/sync/local-codex-memory` with `{ profile, workspace, repo?,
  source_root, mode, files[ { path, kind?, content, hash?, modified_at?,
  idempotency_key?, metadata? } ] }` → envelope wrapping the sync result. The
  Codex `PortableMemoryFile { path, content, metadata, idempotency_key }` maps
  onto a file entry (the daemon infers `kind` from `path` when omitted and
  computes its own `source_hash`, so `hash`/`kind` are optional). For
  multimodal kinds (`screenshot_image`, `ocr_text_extract`, `log_excerpt`,
  `document_excerpt`, `git_diff`, `terminal_output_excerpt`), `content` is the
  extracted text excerpt, not a raw blob. Raw artifacts are referenced by
  `path`, `hash`, and allowlisted metadata such as `artifact_ref`/`media_type`;
  secret-like excerpt text is redacted before durable storage.
