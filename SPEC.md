# Codex Memory Provider Specification

Status: Draft v1 (language-agnostic)

Purpose: Define a Codex-native portable memory provider that can be used by Codex runtimes to recall, write, sync, inspect, and export durable memory across machines, devcontainers, and agent surfaces.

## Normative Language

The key words `MUST`, `MUST NOT`, `REQUIRED`, `SHOULD`, `SHOULD NOT`, `RECOMMENDED`, `MAY`, and `OPTIONAL` in this document are to be interpreted as described in RFC 2119.

`Implementation-defined` means the behavior is part of the implementation contract, but this specification does not prescribe one universal policy. Implementations MUST document the selected behavior.

`Provider` means a service, library, daemon, or in-memory implementation that satisfies this specification.

`Codex runtime` means the Codex process or extension layer that calls the provider before, during, or after agent turns.

`Memory is recall, not authority` means retrieved memory MAY inform the agent but MUST NOT override current user instructions, repository files, `AGENTS.md`, explicit policy, or verified current state.

## 1. Problem Statement

Codex currently has useful local memory behavior, but local memory alone does not solve cross-machine, cross-container, cross-session continuity. A user can have durable context in one Codex installation and lose it in another devcontainer, workstation, desktop instance, or remote environment.

Codex needs a portable memory provider contract that:

- preserves existing local memory behavior;
- allows provider-backed recall before planning;
- supports safe visible-turn writeback;
- imports existing Codex memory artifacts;
- separates personal, work, OSS, and homelab memory;
- avoids storing secrets or confidential data across unsafe boundaries;
- gives operators a clear status surface;
- keeps memory inspectable, exportable, and correctable.

This specification defines `codex-memoryd`, a Codex-native portable memory provider. The provider is purpose-built for Codex and understands Codex memory artifacts, repo-aware engineering memory, visible turns, task checkpoints, local memory imports, and profile/workspace boundaries.

Important boundary:

- Codex owns agent execution, turn lifecycle, sandboxing, approvals, and prompt assembly.
- `codex-memoryd` owns durable memory storage, recall, ingestion, dedupe, safety classification, and export.
- `codex-memoryd` MUST NOT execute coding tasks.
- `codex-memoryd` MUST NOT become a general workflow engine.
- `codex-memoryd` MAY run in-process for tests, as a local daemon, in Docker, or as a remote hosted service.

## 2. Goals and Non-Goals

### 2.1 Goals

- Provide a stable provider protocol for Codex portable memory.
- Support profile and workspace isolation.
- Support repo-aware recall for coding tasks.
- Support low-latency recall before a Codex turn.
- Support safe visible-turn writeback after a Codex turn.
- Support durable conclusions, decisions, commands, gotchas, conventions, landmarks, and task checkpoints.
- Import existing Codex local memory artifacts without deleting them.
- Deduplicate imported and written memories.
- Track provenance for every stored memory.
- Expose enough status to debug active provider configuration and sync state.
- Fail open from the Codex runtime perspective: provider failures MUST NOT break normal Codex execution.
- Support local-first deployment.
- Keep the MVP useful without requiring embeddings, vector databases, dashboards, or cloud hosting.

### 2.2 Non-Goals

- Rich web dashboard.
- Enterprise RBAC or multi-tenant admin console.
- Billing, quotas, or account management.
- General-purpose agent framework.
- General-purpose vector database API.
- ChatGPT export ingestion in the MVP.
- Automatic work-to-personal memory merge.
- Replacing `AGENTS.md`, repository docs, or checked-in policy.
- Replacing Codex’s local memory implementation.
- Mandating a single storage engine.
- Mandating a single deployment topology.
- Storing secrets, credentials, private keys, `.env` dumps, encrypted reasoning, or raw confidential logs.

## 3. System Overview

### 3.1 Main Components

1. `Provider API`
   - HTTP or in-process interface called by Codex.
   - Exposes recall, search, turn writeback, conclusions, local-memory sync, forget/archive, export, and status.

2. `Memory Runtime Adapter`
   - Codex-side adapter that calls the provider.
   - Lives inside Codex’s portable memory runtime.
   - Converts Codex turn, repo, and config data into provider requests.
   - Converts provider recall responses into Codex contextual memory fragments.

3. `Storage Layer`
   - Stores normalized memory records.
   - MVP SHOULD use SQLite or another local durable store.
   - Implementations MAY add Postgres, object storage, or embedded search engines later.

4. `Ingestion Layer`
   - Imports Codex local memory artifacts.
   - Parses `memory_summary.md`, `MEMORY.md`, rollout summaries, and ad-hoc notes.
   - Chunks, classifies, hashes, dedupes, and stores candidate memory records.

5. `Recall Layer`
   - Selects task-relevant memory for a Codex turn.
   - Uses profile, workspace, repo identity, files, prompt, recency, type, and confidence.
   - Returns compact context within a caller-provided budget.

6. `Writeback Layer`
   - Accepts safe visible turns and durable conclusions.
   - Filters unsafe or secret-like content.
   - Stores observations and derived memory records according to policy.

7. `Policy Layer`
   - Applies profile boundary rules.
   - Blocks unsafe storage and unsafe export.
   - Classifies sensitivity and portability.
   - Marks memory as recall, not authority.

8. `Sync State Layer`
   - Tracks imported local files by hash.
   - Prevents repeated duplicate imports.
   - Records last sync status and errors.

9. `Status Surface`
   - Reports provider health, profile, workspace, storage path, sync state, schema version, and queue status.
   - MAY be exposed through CLI, HTTP, logs, or Codex memory status integration.

10. `Observability`
    - Emits structured logs and counters for recall, writeback, sync, errors, and policy drops.

### 3.2 Abstraction Levels

`codex-memoryd` is easiest to port and maintain when kept in these layers:

1. `Protocol Layer`
   - Request and response schemas.
   - Versioning and error codes.

2. `Policy Layer`
   - Safety, sensitivity, portability, and profile boundary logic.

3. `Domain Layer`
   - Memory records, sessions, turns, sources, repos, conclusions, checkpoints.

4. `Storage Layer`
   - Durable persistence and indexing.

5. `Recall Layer`
   - Ranking, filtering, summarization, and context packing.

6. `Ingestion Layer`
   - Codex local memory import and dedupe.

7. `Transport Layer`
   - HTTP server, in-process test provider, Docker packaging, or remote hosting.

8. `Observability Layer`
   - Logs, status, health, metrics, and operator-facing errors.

### 3.3 External Dependencies

Implementations MAY depend on:

- local filesystem for Codex memory imports;
- SQLite or another durable store;
- optional embedding model or vector store;
- optional LLM summarizer/classifier;
- optional HTTP server runtime;
- optional Docker packaging;
- optional authentication proxy for remote deployments.

Implementations MUST document any external dependency required for the selected deployment profile.

## 4. Core Domain Model

### 4.1 Entities

#### 4.1.1 Profile

A high-level boundary for memory portability.

Required profiles:

- `personal`
- `work`
- `oss`
- `homelab`

Fields:

- `id` (string)
- `display_name` (string)
- `created_at` (timestamp)
- `updated_at` (timestamp)
- `default_portability_policy` (string)

Rules:

- A profile MUST be present on every memory record.
- The provider MUST default to `personal` only when the caller does not supply a profile and no stronger configured default exists.
- Work-profile records MUST NOT export to personal profile by default.

#### 4.1.2 Workspace

A logical isolation boundary within a profile.

Examples:

- `josh-personal`
- `gainwell-advisor-dev`
- `codex-memory-lab`
- `homelab`

Fields:

- `id` (string)
- `profile_id` (string)
- `display_name` (string)
- `created_at` (timestamp)
- `updated_at` (timestamp)

Rules:

- A workspace MUST belong to exactly one profile.
- A memory record MUST belong to exactly one workspace.
- Workspace IDs MUST be stable and safe for logs.

#### 4.1.3 Repo Identity

A normalized repository or working-directory identity.

Fields:

- `repo_id` (string)
  - Prefer `git:<normalized-remote-url>` when available.
  - Fall back to `path:<absolute-path-hash>` when no Git remote exists.
- `root` (string or null)
- `remote` (string or null)
- `branch` (string or null)
- `commit` (string or null)
- `is_git` (boolean)

Rules:

- Repo identity MUST NOT include credentials embedded in remote URLs.
- Repo identity SHOULD be included for repo-scoped recall and writeback.
- Repo identity MAY be null for user/global memory.

#### 4.1.4 Session

A Codex interaction thread or logical task session.

Fields:

- `id` (string)
- `profile_id` (string)
- `workspace_id` (string)
- `repo_id` (string or null)
- `thread_id` (string or null)
- `source` (string)
  - Examples: `codex-cli`, `codex-desktop`, `codex-devcontainer`, `import`
- `started_at` (timestamp)
- `ended_at` (timestamp or null)
- `metadata` (object)

Rules:

- Session IDs MUST be stable for a Codex thread when available.
- Providers MAY create synthetic session IDs for imports.

#### 4.1.5 Visible Turn

A user or assistant message visible outside hidden reasoning.

Fields:

- `id` (string)
- `session_id` (string)
- `actor` (`user` or `assistant`)
- `content` (string)
- `created_at` (timestamp)
- `metadata` (object)

Rules:

- Hidden reasoning MUST NOT be stored.
- Tool outputs MUST NOT be stored as visible turns unless explicitly sanitized and marked as such.
- Secret-like visible turns MUST be rejected or redacted according to policy.

#### 4.1.6 Memory Source

An artifact or event from which one or more memory records were derived.

Fields:

- `id` (string)
- `profile_id` (string)
- `workspace_id` (string)
- `kind` (string)
  - Examples: `local_memory_summary`, `local_memory_registry`, `rollout_summary`, `ad_hoc_note`, `visible_turn`, `manual_conclusion`
- `source_path` (string or null)
- `source_hash` (string)
- `created_at` (timestamp)
- `ingested_at` (timestamp)
- `metadata` (object)

Rules:

- Imported files MUST have source hashes.
- The provider SHOULD use source hashes to avoid duplicate imports.
- Source records SHOULD be retained for audit and export.

#### 4.1.7 Memory Record

The primary durable memory unit.

Fields:

- `id` (string)
- `profile_id` (string)
- `workspace_id` (string)
- `repo_id` (string or null)
- `scope` (string)
  - `user`, `profile`, `workspace`, `repo`, `file`, `session`
- `type` (string)
  - `preference`, `repo_convention`, `command`, `decision`, `gotcha`, `landmark`, `task_checkpoint`, `identity`, `workflow_pattern`, `other`
- `content` (string)
- `related_files` (list of strings)
- `tags` (list of strings)
- `sensitivity` (string)
  - `public`, `personal`, `work_confidential`, `secret_blocked`
- `portability` (string)
  - `portable`, `profile_only`, `workspace_only`, `never_export`
- `confidence` (number, 0.0 to 1.0)
- `source_ids` (list of strings)
- `content_hash` (string)
- `supersedes` (list of memory record IDs)
- `created_at` (timestamp)
- `updated_at` (timestamp)
- `last_used_at` (timestamp or null)
- `archived` (boolean)
- `metadata` (object)

Rules:

- `content` MUST be concise enough for retrieval and display.
- Providers SHOULD reject oversized memory records.
- Providers MUST retain provenance.
- Providers SHOULD prefer updating or superseding old memories over duplicating similar facts.
- Archived records MUST NOT be returned by default recall.

#### 4.1.8 Conclusion

A durable fact explicitly written by a user, agent, or ingestion pipeline.

Fields:

- `id` (string)
- `profile_id` (string)
- `workspace_id` (string)
- `repo_id` (string or null)
- `target` (string)
  - Examples: `user`, `assistant`, `repo`, `workspace`
- `content` (string)
- `source_id` (string or null)
- `created_at` (timestamp)
- `metadata` (object)

Rules:

- Conclusions SHOULD normally become memory records.
- Conclusions MUST pass policy checks before durable storage.

#### 4.1.9 Task Checkpoint

A resumable summary of project work.

Fields:

- `id` (string)
- `session_id` (string or null)
- `profile_id` (string)
- `workspace_id` (string)
- `repo_id` (string or null)
- `summary` (string)
- `changed_files` (list of strings)
- `decisions` (list of strings)
- `blockers` (list of strings)
- `next_steps` (list of strings)
- `tests_run` (list of strings)
- `tests_not_run` (list of strings)
- `branch` (string or null)
- `commit` (string or null)
- `created_at` (timestamp)

Rules:

- Checkpoints SHOULD be returned for resume-like prompts and recent project context.
- Checkpoints SHOULD be repo-scoped when repo identity is available.

#### 4.1.10 Sync Cursor

State for idempotent imports.

Fields:

- `id` (string)
- `profile_id` (string)
- `workspace_id` (string)
- `source_root` (string)
- `last_started_at` (timestamp or null)
- `last_completed_at` (timestamp or null)
- `last_error` (string or null)
- `seen_hashes` (implementation-defined)
- `metadata` (object)

Rules:

- Sync MUST be idempotent.
- Re-importing unchanged local memory files MUST NOT create duplicate memory records.

### 4.2 Stable Identifiers and Normalization Rules

- `Profile ID`
  - Lowercase stable string.
  - Required on all durable records.

- `Workspace ID`
  - Caller-provided stable string.
  - SHOULD contain only `[A-Za-z0-9._:-]`.
  - Unsafe characters MAY be normalized.

- `Repo ID`
  - Use sanitized Git remote when possible.
  - Strip credentials from URLs.
  - Remove `.git` suffix where appropriate.
  - Fall back to path hash when no remote exists.

- `Memory Record ID`
  - Provider-generated stable unique ID.
  - SHOULD be UUID, ULID, or content-addressed ID.

- `Content Hash`
  - Hash normalized content plus profile/workspace/repo/type/scope.
  - Used for dedupe.

- `Source Hash`
  - Hash raw imported source content plus source path and profile/workspace.

## 5. Provider Protocol

### 5.1 Transport

MVP transport SHOULD be HTTP JSON over localhost.

Required deployment-compatible shapes:

1. In-process provider for tests.
2. Local daemon on `127.0.0.1`.
3. Docker image later.
4. Remote hosted service later.

The protocol MUST be representable without HTTP so Codex tests can call an in-memory provider implementation.

### 5.2 Versioning

All HTTP APIs MUST be namespaced under `/v1`.

Providers MUST expose:

- provider schema version;
- supported API version;
- storage schema version;
- feature flags.

Breaking API changes require a new major path such as `/v2`.

### 5.3 Authentication

Local daemon MVP MAY rely on loopback-only binding and filesystem permissions.

Remote providers MUST require authentication.

API keys MUST NOT be logged.

Codex-side config SHOULD use environment-variable indirection for credentials.

### 5.4 Common Request Fields

Most write/search/recall requests SHOULD include:

- `profile`
- `workspace`
- `repo`
- `session`
- `source`
- `metadata`

If a required field is missing, the provider MUST either apply a documented default or return a typed validation error.

### 5.5 Common Response Fields

Responses SHOULD include:

- `ok` (boolean)
- `data` (object, when successful)
- `error` (object, when failed)
- `warnings` (list)
- `request_id` (string)
- `provider` (object with name/version, where useful)

## 6. HTTP API Specification

### 6.1 `GET /v1/status`

Returns provider health and configuration summary.

Response data fields:

- `provider_name`
- `provider_version`
- `api_version`
- `storage_schema_version`
- `status`
  - `ok`, `degraded`, `unavailable`
- `storage`
  - kind, path/endpoint, writable
- `active_profiles`
- `active_workspaces`
- `last_sync`
- `pending_writes`
- `local_import`
  - `status` (`unknown`, `not_found`, `unsynced`, `synced`, `error`)
  - `last_preview_at` (timestamp or null)
  - `last_apply_at` (timestamp or null)
  - `unsynced_count` (integer)
- `features`

Status MUST NOT expose secrets.

The `local_import` block lets the Codex runtime render a deterministic banner without model reasoning, for example:

```text
Memory: provider active, local memories unsynced. Run `codex memory import-local --preview`.
```

### 6.2 `POST /v1/recall`

Returns compact task-relevant memory for pre-turn Codex injection.

Request fields:

- `profile` (string, REQUIRED)
- `workspace` (string, REQUIRED)
- `repo` (object or null)
- `session` (object or null)
- `query` (string, REQUIRED)
- `files` (list of strings, OPTIONAL)
- `max_tokens` (integer, OPTIONAL)
- `include_types` (list of strings, OPTIONAL)
- `exclude_types` (list of strings, OPTIONAL)
- `recency_days` (integer, OPTIONAL)
- `metadata` (object, OPTIONAL)

Response data fields:

- `summary` (string or null)
- `facts` (list of memory snippets)
- `checkpoints` (list)
- `citations` (list)
- `truncated` (boolean)
- `authority` (string) — the reference implementation sets this to
  `"recall_not_authority"` to satisfy the §8.2 requirement that recall output
  MUST mark memory as contextual recall, not instruction. Implementations MAY
  emit it; callers MUST NOT treat its absence as authority.

Recall behavior:

- Provider MUST filter by profile and workspace.
- Provider SHOULD prioritize exact repo matches.
- Provider SHOULD include profile/user preferences when relevant.
- Provider SHOULD include repo conventions, commands, gotchas, decisions, and recent checkpoints when relevant.
- Provider MUST respect `max_tokens` or an implementation-defined default budget.
- Provider MUST NOT return archived records by default.
- Provider MUST NOT return `secret_blocked` records.

### 6.3 `POST /v1/search`

Explicit memory search.

Request fields:

- `profile` (string, REQUIRED)
- `workspace` (string, OPTIONAL)
- `repo` (object or null)
- `query` (string, REQUIRED)
- `scope` (string, OPTIONAL)
- `type` (string, OPTIONAL)
- `limit` (integer, OPTIONAL)
- `include_archived` (boolean, OPTIONAL)

Response data fields:

- `matches` (list of memory records or snippets)
- `next_cursor` (string or null)

### 6.4 `POST /v1/turns`

Writes safe visible turn messages.

Request fields:

- `profile` (string, REQUIRED)
- `workspace` (string, REQUIRED)
- `repo` (object or null)
- `session` (object, REQUIRED)
- `messages` (list, REQUIRED)
  - each message:
    - `actor` (`user` or `assistant`)
    - `content` (string)
    - `created_at` (timestamp or null)
    - `metadata` (object)
- `write_policy` (string, OPTIONAL)

Response data fields:

- `accepted` (integer)
- `rejected` (integer)
- `rejections` (list)
- `source_ids` (list)
- `derived_record_ids` (list, OPTIONAL) — ids of memory records derived from
  accepted messages. The reference implementation derives them synchronously and
  returns them for caller feedback; implementations MAY omit this field.

Rules:

- Hidden reasoning MUST NOT be sent.
- Provider MUST run policy checks before storage.
- Provider SHOULD reject or redact secret-like content.
- Provider MAY store accepted messages as sources before deriving memory records.
- Provider SHOULD derive candidate memory records asynchronously when possible.

### 6.5 `POST /v1/conclusions`

Writes durable memory conclusions.

Request fields:

- `profile` (string, REQUIRED)
- `workspace` (string, REQUIRED)
- `repo` (object or null)
- `target` (string, REQUIRED)
- `conclusions` (list of strings, REQUIRED)
- `metadata` (object, OPTIONAL)

Response data fields:

- `created` (list of conclusion IDs)
- `record_ids` (list, OPTIONAL) — ids of the memory records created from the
  conclusions (since conclusions SHOULD become memory records). The reference
  implementation returns these for caller feedback; implementations MAY omit it.
- `rejected` (list with reasons)

Rules:

- Conclusions MUST pass policy checks.
- Conclusions SHOULD become memory records.
- Conclusions SHOULD preserve metadata and provenance.

### 6.5a `POST /v1/checkpoints`

Stores a resumable task checkpoint (SPEC §4.1.9). Checkpoints are first-class
durable memory: they have their own route rather than riding on `conclusions`,
because the Codex runtime writes them after substantial work and recalls them
for resume-like prompts.

Request fields:

- `profile` (string, REQUIRED)
- `workspace` (string, REQUIRED)
- `repo` (object or null)
- `session` (object or null)
- `summary` (string, REQUIRED)
- `changed_files` (list of strings, OPTIONAL)
- `decisions` (list of strings, OPTIONAL)
- `blockers` (list of strings, OPTIONAL)
- `next_steps` (list of strings, OPTIONAL)
- `tests_run` (list of strings, OPTIONAL)
- `tests_not_run` (list of strings, OPTIONAL)
- `branch` (string or null, OPTIONAL)
- `commit` (string or null, OPTIONAL)

Response data fields:

- `id` (checkpoint ID)
- `created_at` (timestamp)

Rules:

- The `summary` MUST pass policy checks (secret/injection screening).
- Checkpoints SHOULD be repo-scoped when repo identity is available.
- The provider SHOULD also derive a `task_checkpoint` memory record so recall
  can surface the checkpoint as a fact when checkpoints are not separately
  requested.
- Recent checkpoints SHOULD be returned by `POST /v1/recall` (the `checkpoints`
  field), repo-matching checkpoints first.

### 6.6 `POST /v1/sync/local-codex-memory`

Imports existing local Codex memory artifacts.

Request fields:

- `profile` (string, REQUIRED)
- `workspace` (string, REQUIRED)
- `repo` (object or null)
- `source_root` (string, REQUIRED)
- `files` (list, REQUIRED)
  - each file:
    - `path` (string)
    - `kind` (string)
    - `content` (string)
    - `hash` (string)
    - `modified_at` (timestamp or null)
- `mode` (string)
  - `preview`, `apply`
- `metadata` (object, OPTIONAL)

Supported file kinds:

- `memory_summary`
- `memory_registry`
- `rollout_summary`
- `ad_hoc_note`
- `unknown`

Response data fields:

- `mode`
- `proposed`
- `created`
- `updated`
- `skipped`
- `rejected`
- `warnings`
- `sync_cursor`

Rules:

- The Codex runtime is responsible for reading local files and sending file payloads. The provider MUST NOT assume `source_root` is locally readable unless running in provider CLI / local-ingest mode. This matters for Docker, remote, and cross-host provider deployments.
- In provider CLI / local-ingest mode, the provider MAY read `source_root` directly and populate the `files` payload itself.
- `preview` MUST NOT write durable memory records.
- `apply` MUST be idempotent.
- Provider MUST dedupe by source hash and content hash.
- Provider MUST preserve source path and source hash.
- Provider MUST NOT delete local files.
- Provider MUST reject secret-like artifacts.
- Provider SHOULD treat `memory_summary.md` as high-level context.
- Provider SHOULD treat `MEMORY.md` as registry/index material.
- Provider SHOULD parse rollout summaries as session/checkpoint evidence.
- Provider SHOULD parse ad-hoc notes as manual memory updates.

### 6.7 `POST /v1/forget`

Archives or deletes memory.

Request fields:

- `profile` (string, REQUIRED)
- `workspace` (string, OPTIONAL)
- `ids` (list of strings, REQUIRED)
- `mode` (`archive` or `delete`)
- `reason` (string, OPTIONAL)

Response data fields:

- `archived`
- `deleted`
- `not_found`
- `errors`

Rules:

- MVP SHOULD archive by default.
- Hard delete MAY be used for secrets, PII removal, or legal deletion.
- Deletion SHOULD preserve audit metadata when allowed by policy.

### 6.8 `GET /v1/export`

Exports memory records.

Query parameters:

- `profile` (REQUIRED)
- `workspace` (OPTIONAL)
- `repo_id` (OPTIONAL)
- `include_archived` (OPTIONAL)
- `format` (OPTIONAL, default `jsonl`)
- `target_profile` (OPTIONAL) — when set, the destination profile for the
  export, so the provider can apply the profile-boundary matrix (§10.3). When
  omitted, the export is treated as same-profile (no cross-profile filtering).
  This makes the boundary check explicit at the API rather than relying on
  out-of-band caller intent.

Response:

- The reference implementation streams records directly (JSONL/JSON), not
  wrapped in the common envelope, so the response can be piped to a file. Export
  metadata is returned in headers: `x-record-count`, `x-omitted-secret`,
  `x-omitted-boundary`.

Rules:

- Work-profile export to personal profile MUST be denied by default.
- Export MUST omit `secret_blocked` content.
- Export SHOULD include provenance unless redacted by policy.

## 7. Codex Local Memory Import

### 7.1 Supported Local Layout

The provider MUST support importing this Codex memory layout when supplied by the Codex runtime:

```text
~/.codex/memories/
  memory_summary.md
  MEMORY.md
  rollout_summaries/
  extensions/ad_hoc/notes/
```

### 7.2 Import Semantics

Local memory import is source ingestion, not a destructive migration.

Local memory import MUST be a first-class, explicit, previewable, and idempotent flow. The Codex runtime MUST NOT silently upload existing local memory on first run after switching to a provider-backed backend.

Rules:

- Local files remain local.
- Provider creates memory sources and records.
- Provider records sync state.
- Repeated import MUST be idempotent.
- Provider SHOULD prefer preview before apply.
- Provider SHOULD produce clear rejection reasons.
- The Codex runtime MUST NOT auto-apply local import by default.

### 7.3 First-Run Detection and Switchover Flow

When `backend` is `provider` or `hybrid` (regardless of the selected `provider`), the Codex runtime SHOULD perform deterministic, runtime-level detection before the first agent turn:

1. Detect that a provider-backed backend is active.
2. Check provider status (`GET /v1/status`).
3. Check for existing local memory under `~/.codex/memories`.
4. Check whether those local files were already imported (by source hash via the sync cursor).
5. If local memory exists and is not yet imported, surface a safe import path according to `local_import_policy`.

This detection MUST be deterministic and runtime-level. The Codex runtime MUST NOT spend model tokens making the model reason about whether local memory needs import. The model MAY mention an unsynced state if relevant, but detection MUST NOT depend on model reasoning.

When local memory is unsynced, the runtime SHOULD show a deterministic status banner, for example:

```text
Memory: provider active, local memories unsynced. Run `codex memory import-local --preview`.
```

A typical first-run switchover sequence looks like:

```text
brew uninstall codex
brew install josh-codex
export CODEX_MEMORYD_URL=http://127.0.0.1:8787
codex memory status
codex memory import-local --preview
codex memory import-local --apply
codex
```

After apply, fresh sessions use provider-backed memory.

### 7.4 Local Import Policy

The Codex runtime MUST support a `local_import_policy` setting controlling first-run behavior:

- `prompt`
  - Interactive first-run prompt only. RECOMMENDED default.
- `manual`
  - No prompt and no automatic import. Import only via explicit CLI command. RECOMMENDED for work machines.
- `startup_preview`
  - Automatically run a preview on startup, but never apply. Acceptable for trusted personal machines.
- `startup_apply`
  - Automatically apply on startup. NOT RECOMMENDED. SHOULD be used only with high confidence in the policy layer.

Default behavior rules:

- The default policy SHOULD be `prompt`.
- `prompt` MUST be interactive only and MUST NOT trigger in non-interactive/automation contexts; non-interactive contexts SHOULD behave like `manual`.
- The runtime MUST NOT apply import automatically unless `local_import_policy` is `startup_apply`.
- The runtime MUST NOT delete local files under any policy.
- A user choice to never ask SHOULD be persisted per profile/workspace.

### 7.5 First-Run Prompt

Under `local_import_policy = "prompt"`, the first run after switchover SHOULD present a safe, non-destructive choice, for example:

```text
Portable memory provider is active.

Found existing local Codex memories:
  ~/.codex/memories/memory_summary.md
  ~/.codex/memories/MEMORY.md
  ~/.codex/memories/rollout_summaries/

These have not been synced to codex-memoryd.

Options:
  [p] preview import
  [i] import safe memories
  [s] skip for now
  [n] never ask for this profile/workspace
```

The prompt MUST default to a non-destructive action and MUST NOT upload memory without explicit user choice. Selecting `n` SHOULD persist a per-profile/workspace suppression so the prompt does not recur.

### 7.6 CLI Command Path

The Codex runtime SHOULD expose user-facing memory commands, because interactive prompts are unsuitable for automation. The preferred surface is `codex memory ...`, because users think "Codex has memories," not "the daemon has files":

```text
codex memory status
codex memory import-local --preview
codex memory import-local --apply
codex memory import-local --profile personal --workspace josh-personal --apply
codex memory import-local --since last-sync --apply
```

Provider-level commands MAY also exist for operators:

```text
codex-memoryd sync-local --preview ~/.codex/memories
codex-memoryd sync-local --apply ~/.codex/memories
```

Rules:

- `codex memory import-local` MUST be available regardless of `local_import_policy`.
- `--preview` MUST map to `mode = preview` and MUST NOT write durable records.
- `--apply` MUST map to `mode = apply` and MUST be idempotent.
- `--since last-sync` SHOULD limit import to sources changed since the last sync cursor.

### 7.7 What Gets Imported

Local memory MUST NOT be imported as a single raw blob. The Codex runtime sends discrete local artifacts to the provider:

```text
~/.codex/memories/memory_summary.md
~/.codex/memories/MEMORY.md
~/.codex/memories/rollout_summaries/*.md
~/.codex/memories/extensions/ad_hoc/notes/*.md
```

For each artifact, the provider SHOULD:

```text
read file
hash file
classify kind
chunk safely
filter secrets
extract candidate memory records
dedupe by source hash + content hash
store provenance
return preview summary
```

### 7.8 Preview Output

Preview MUST summarize the proposed import without writing durable records. A preview SHOULD report scanned files, candidate counts, create/skip/reject counts, rejection reasons, and a type breakdown, for example:

```text
Local memory import preview

Files scanned: 42
Candidate memories: 118
Will create: 73
Already imported: 31
Rejected: 14

Rejected reasons:
  6 possible secrets
  3 oversized raw logs
  5 prompt-injection-like snippets

Types:
  18 preferences
  12 commands
  16 repo conventions
  9 decisions
  11 gotchas
  7 checkpoints
```

### 7.9 Idempotency

Import MUST be idempotent. Running apply repeatedly MUST NOT create duplicate memory records:

```text
codex memory import-local --apply
codex memory import-local --apply
codex memory import-local --apply
```

To guarantee idempotency, the provider MUST track at least:

```text
source_path
source_hash
content_hash
profile
workspace
repo_id
imported_at
```

If nothing changed since the last import, the provider MUST skip the record.

### 7.10 Local Memory Remains Local

The Codex runtime MUST NOT delete `~/.codex/memories` after import. Local memory remains useful as:

```text
fallback
cache
debug surface
human-readable source
rebuild source
offline mode
```

### 7.11 Hybrid Mode Roles

In `hybrid` mode, local and provider memory have distinct roles:

```text
local    = cache / debug / generated memory
provider = portable durable memory
```

Recommended default posture:

- manual command available: yes
- first-run prompt: yes, interactive only
- automatic preview: acceptable
- automatic apply: no by default
- delete local after import: never
- bidirectional merge: later

### 7.12 Chunking

Imported files SHOULD be chunked by semantic boundaries:

- Markdown headings;
- rollout/session blocks;
- bullet groups;
- paragraphs;
- fallback fixed-size chunks.

Chunks MUST be bounded by implementation-defined size limits.

### 7.13 Classification

Each candidate memory SHOULD be classified for:

- type;
- scope;
- sensitivity;
- portability;
- confidence;
- related files;
- tags.

MVP classification MAY be heuristic.

LLM classification is OPTIONAL.

## 8. Recall Semantics

### 8.1 Recall Inputs

Recall SHOULD consider:

- user prompt;
- profile;
- workspace;
- repo identity;
- current branch;
- current cwd;
- known files;
- recent checkpoints;
- memory type weights;
- recency;
- confidence;
- source provenance.

### 8.2 Recall Output Contract

Recall output MUST be compact.

Recall output MUST include enough provenance for debugging.

Recall output MUST mark memory as contextual recall, not instruction.

Recall output SHOULD group results:

- summary;
- preferences;
- repo conventions;
- commands;
- decisions;
- gotchas;
- recent checkpoints.

### 8.3 Ranking

Default ranking SHOULD prioritize:

1. same profile and workspace;
2. same repo;
3. exact related-file match;
4. high-confidence decisions/gotchas/commands;
5. recent checkpoints;
6. stable user preferences;
7. older or broad profile memory.

### 8.4 Staleness

Memory records SHOULD have `updated_at` and `last_used_at`.

Recall responses SHOULD indicate stale or old records when relevant.

Drift-prone facts SHOULD be verified by Codex against the current repository when cheap.

## 9. Writeback Semantics

### 9.1 Visible Turns

Codex MAY write visible user and assistant messages after a turn.

Provider MUST reject or redact unsafe content.

Accepted messages MAY be stored as sources and MAY produce derived memory asynchronously.

### 9.2 Durable Conclusions

Codex MAY write conclusions when:

- the user explicitly asks to remember something;
- the agent finishes substantial work and records a checkpoint;
- an import produces stable facts;
- policy allows safe automatic memory.

### 9.3 Checkpoints

Codex SHOULD write checkpoints after substantial work.

Checkpoint records SHOULD include:

- summary;
- changed files;
- decisions;
- blockers;
- next steps;
- tests run;
- tests not run;
- branch;
- commit.

## 10. Policy and Safety

### 10.1 Secret Blocking

Provider MUST reject or redact:

- private keys;
- API keys;
- passwords;
- auth tokens;
- `.env` dumps;
- credential files;
- raw secret manager output;
- encrypted reasoning;
- large raw logs likely to contain secrets.

### 10.2 Prompt Injection Blocking

Provider SHOULD reject candidate memories that look like durable instructions intended to override system/developer/user policy.

Examples:

- “ignore previous instructions”;
- “you are now system”;
- “override developer message”;
- copied prompt-injection payloads.

### 10.3 Profile Boundary Defaults

Required defaults:

- `work -> personal`: deny
- `personal -> work`: allow only generic user operating preferences after classification
- `work -> work`: allow
- `personal -> personal`: allow
- `oss -> personal`: implementation-defined
- `homelab -> personal`: implementation-defined

### 10.4 Memory Authority

Provider responses MUST NOT be treated as authoritative policy.

Codex MUST treat memory below:

1. current user instruction;
2. system/developer instructions;
3. `AGENTS.md`;
4. repository files;
5. verified current state.

### 10.5 Retention

MVP MAY retain memory indefinitely.

Implementations SHOULD support archiving and export.

Implementations SHOULD document deletion semantics.

## 11. Configuration Specification

### 11.1 Codex-Side Config

Codex runtimes SHOULD support config similar to:

```toml
[memories]
backend = "local"          # local | provider | hybrid
provider = "codex_memoryd" # provider implementation when backend != local
profile = "personal"
workspace = "josh-personal"
provider_url = "http://127.0.0.1:8787"

local_import_policy = "prompt"
# local_import_policy = "manual"
# local_import_policy = "startup_preview"
# local_import_policy = "startup_apply"

write_policy = "visible_turns"
sync_policy = "manual"
cross_profile_policy = "default_deny"
```

Field meanings:

- `backend`
  - Selects the high-level memory mode: `local`, `provider`, or `hybrid`.
  - `local` preserves upstream local memory behavior and ignores `provider`/`provider_url`.
  - `provider` routes durable memory through the provider named by `provider`.
  - `hybrid` keeps local memory as cache/debug/generated surface while syncing durable memory to the provider.
- `provider`
  - Selects the provider implementation when `backend` is `provider` or `hybrid`.
  - Examples: `codex_memoryd`, `honcho`.
  - Ignored when `backend = local`.
- `profile`
  - Memory profile.
- `workspace`
  - Provider workspace.
- `provider_url`
  - HTTP endpoint for daemon/remote providers.
- `local_import_policy`
  - Controls first-run local memory import behavior.
  - `prompt` (RECOMMENDED default): interactive first-run prompt only.
  - `manual` (RECOMMENDED for work machines): no prompt, import only via explicit CLI command.
  - `startup_preview`: auto-preview on startup, never apply.
  - `startup_apply` (NOT RECOMMENDED): auto-apply on startup; use only with high confidence in the policy layer.
- `write_policy`
  - `off` or `visible_turns`.
- `sync_policy`
  - `manual`, `startup`, or implementation-defined.
- `cross_profile_policy`
  - `default_deny` in MVP.

#### 11.1.1 Backend and Provider Naming Bridge

The `backend` field selects the high-level mode and SHOULD remain a small, stable enum. The `provider` field selects the concrete provider implementation. Keeping these separate avoids `backend` enum explosion as new providers are added.

Required backend values:

- `local`
- `provider`
- `hybrid`

Provider values are implementation-defined and MAY include:

- `codex_memoryd`
- `honcho`

Example configurations:

Local-only (upstream behavior preserved):

```toml
[memories]
backend = "local"
```

`codex-memoryd` as durable provider:

```toml
[memories]
backend = "provider"
provider = "codex_memoryd"
provider_url = "http://127.0.0.1:8787"
```

Honcho as durable provider:

```toml
[memories]
backend = "provider"
provider = "honcho"
```

Hybrid (local cache plus durable provider):

```toml
[memories]
backend = "hybrid"
provider = "codex_memoryd"
```

Compatibility notes:

- Implementations MAY accept legacy `backend` values such as `codex_memoryd` or `honcho` and SHOULD normalize them to `backend = "provider"` with the matching `provider` value.
- When `backend = hybrid`, `provider_url` MAY point at Honcho, `codex-memoryd`, or another provider depending on the selected `provider`.

### 11.2 Provider Config

Provider config SHOULD support:

- bind address;
- storage path;
- log level;
- default profile;
- default workspace;
- max recall tokens;
- max record size;
- secret-filter settings;
- optional embedding settings;
- optional classifier settings.

Example:

```toml
[server]
bind = "127.0.0.1:8787"

[storage]
kind = "sqlite"
path = "~/.codex-memoryd/memory.db"

[policy]
default_profile = "personal"
cross_profile_policy = "default_deny"

[recall]
max_tokens = 1200
```

### 11.3 Dynamic Reload

Dynamic reload is OPTIONAL for MVP.

If implemented:

- invalid reloads MUST NOT crash the service;
- the last known good config SHOULD remain active;
- operator-visible errors SHOULD be emitted.

## 12. Deployment Profiles

### 12.1 In-Process Test Provider

Purpose:

- unit tests;
- Codex integration tests;
- no network dependency.

Requirements:

- satisfy provider trait;
- deterministic behavior;
- inspectable stored messages and records.

### 12.2 Local Daemon

Purpose:

- daily local use;
- cross-Codex-binary continuity on one machine;
- devcontainer access through forwarded localhost or host gateway.

Requirements:

- bind to loopback by default;
- local durable store;
- structured logs;
- status endpoint;
- safe shutdown.

### 12.3 Docker Image

Purpose:

- repeatable deployment;
- devcontainer use;
- homelab hosting;
- work-hosted deployment.

Requirements:

- persistent volume for storage;
- healthcheck;
- explicit bind address;
- no baked-in secrets.

### 12.4 Remote Hosted Provider

Purpose:

- cross-machine sync;
- shared workspace provider;
- possible work-hosted memory.

Requirements:

- authentication;
- TLS;
- explicit profile/workspace policy;
- export/delete story;
- logging without secret leakage.

Remote hosting is not REQUIRED for MVP.

## 13. Observability

### 13.1 Logs

Provider SHOULD emit structured logs for:

- startup;
- status changes;
- recall requests;
- writeback requests;
- sync runs;
- rejected memories;
- policy drops;
- storage errors;
- export/forget operations.

Logs MUST NOT include secrets.

### 13.2 Metrics

Provider MAY expose counters:

- recall requests;
- recall latency;
- records returned;
- turns accepted/rejected;
- sync files processed;
- sync records created/skipped/rejected;
- policy rejections;
- provider errors.

### 13.3 Status

Status MUST include enough information for Codex to display:

- provider availability;
- active profile;
- active workspace;
- storage kind;
- last sync status;
- pending writes;
- provider version.

## 14. Error Model

Errors SHOULD use stable codes.

Recommended error codes:

- `invalid_request`
- `missing_profile`
- `missing_workspace`
- `unknown_profile`
- `unknown_workspace`
- `storage_unavailable`
- `policy_denied`
- `secret_detected`
- `profile_boundary_denied`
- `sync_source_invalid`
- `not_found`
- `unsupported_version`
- `internal_error`

Codex runtime behavior:

- Recall errors MUST fail open.
- Writeback errors MUST NOT fail the user turn.
- Sync errors SHOULD be operator-visible.
- Policy denials SHOULD be visible in status or sync summaries.

## 15. Conformance

### 15.1 MVP Conformance

An MVP provider conforms to this specification if it supports:

- `GET /v1/status`
- `POST /v1/recall`
- `POST /v1/search`
- `POST /v1/turns`
- `POST /v1/conclusions`
- `POST /v1/checkpoints`
- `POST /v1/sync/local-codex-memory`
- `POST /v1/forget`
- `GET /v1/export`
- local durable storage
- profile/workspace isolation
- secret filtering
- local Codex memory import
- idempotent sync
- recall that returns compact context
- safe visible-turn writeback
- export or documented manual backup path

### 15.2 Codex Integration Conformance

A Codex runtime conforms if it:

- can configure the provider endpoint/profile/workspace;
- recalls before non-trivial turns;
- injects memory as contextual recall, not instruction;
- writes only visible turns;
- never sends hidden reasoning;
- supports manual local-memory sync;
- fails open on provider failures;
- exposes provider status.

### 15.3 Test Requirements

Implementations SHOULD test:

- status endpoint;
- recall with profile/workspace/repo filters;
- writeback secret rejection;
- conclusion creation;
- local memory import preview;
- local memory import apply;
- idempotent re-import;
- work-to-personal export denial;
- provider failure fail-open behavior;
- export/forget behavior.

## 16. Extension Points

Implementations MAY add:

- embeddings;
- vector index;
- LLM summarization;
- LLM classification;
- graph memory;
- remote sync;
- bidirectional merge;
- ChatGPT export importer;
- MCP compatibility layer;
- CLI/TUI status view;
- web dashboard.

Extensions MUST document:

- config fields;
- storage effects;
- policy effects;
- failure behavior;
- migration behavior.

## 17. Open Questions

- Should `codex-memoryd` expose an MCP compatibility surface for other agents?
- Should local Codex memory import run automatically on startup or remain manual by default?
- What is the minimum useful status UI inside Codex?
- Should provider recall include citations in the first MVP?
- Should `memory_summary.md` be regenerated from provider state in hybrid mode?
- Should `codex-memoryd` support pull-based sync from Codex local roots, or only accept pushed file payloads from Codex?
- What is the first storage backend: SQLite FTS5 only, SQLite plus embeddings, or SQLite plus Tantivy?
- Should visible assistant messages be written by default, or only checkpoints/conclusions?
- How much of Honcho’s peer/session/conclusion model should be mirrored versus staying Codex-specific?

## 18. Recommended MVP Build Order

1. Define request/response types.
2. Build in-memory provider.
3. Build SQLite storage.
4. Implement status.
5. Implement search over stored records.
6. Implement recall packing.
7. Implement visible-turn writeback with secret filtering.
8. Implement conclusions.
9. Implement local Codex memory import preview.
10. Implement local Codex memory import apply with dedupe.
11. Add export/forget.
12. Add Docker packaging.
13. Add Codex provider adapter.
14. Add status display in Codex.
15. Add optional embeddings/classification.

## 19. Appendix: Example Recall Request

```json
{
  "profile": "personal",
  "workspace": "josh-personal",
  "repo": {
    "repo_id": "git:https://github.com/joshyorko/codex-memory-lab",
    "root": "/workspaces/codex-memory-lab",
    "remote": "https://github.com/joshyorko/codex-memory-lab",
    "branch": "main",
    "commit": null,
    "is_git": true
  },
  "query": "Continue implementing provider-agnostic portable memory in Codex.",
  "files": [
    "codex-rs/ext/memories/src/extension.rs",
    "codex-rs/ext/memories/src/runtime.rs"
  ],
  "max_tokens": 1200
}
```

## 20. Appendix: Example Recall Response

```json
{
  "ok": true,
  "data": {
    "summary": "Portable memory runtime should be provider-agnostic. Honcho is provider #1, not the architecture.",
    "facts": [
      {
        "id": "mem_01",
        "type": "decision",
        "scope": "repo",
        "content": "Use TurnInputContributor for pre-turn recall; tool backend alone is insufficient.",
        "confidence": 0.95
      },
      {
        "id": "mem_02",
        "type": "gotcha",
        "scope": "repo",
        "content": "Do not delete tool schema helper functions when adding portable-memory schema types.",
        "confidence": 0.9
      }
    ],
    "checkpoints": [],
    "citations": [
      {
        "memory_id": "mem_01",
        "source_id": "src_01",
        "source_path": "manual_conclusion"
      }
    ],
    "truncated": false
  },
  "warnings": [],
  "request_id": "req_123"
}
```

## 21. Appendix: Example Local Sync Request

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
      "hash": "sha256:example",
      "modified_at": "2026-06-06T12:00:00Z"
    }
  ]
}
```
