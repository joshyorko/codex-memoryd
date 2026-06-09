# Dreamer loop — design (Phases 1–2)

Companion to [`dreamer-loop-research.md`](./dreamer-loop-research.md). This
document specifies the **CLI/API contract, storage proposal, staleness and
supersession rules, the synthesis backend boundary, and the eval fixture
format** in enough detail to implement and test Phases 1 (preview) and 2
(apply). It introduces **no schema migration**; everything fits the existing
[`MemoryRecord.metadata`](../src/domain.rs) JSON value and the existing policy /
recall / store boundaries.

> The model proposes; `codex-memoryd` validates and persists. Synthesized memory
> is `recall_not_authority`. See [`dreamer-loop-research.md`](./dreamer-loop-research.md)
> for motivation, non-claims, and threat model.

## 1. CLI surface

Mirrors the existing `sync-local --preview/--apply` ergonomics
([`README.md`](../README.md), [`SPEC.md`](../SPEC.md) §7).

```bash
# Phase 1: report only, write nothing.
codex-memoryd dream --profile personal --workspace josh-personal --preview

# Phase 2: idempotent, policy-gated writes.
codex-memoryd dream --profile personal --workspace josh-personal --apply
```

Optional flags:

| Flag | Meaning | Default |
| --- | --- | --- |
| `--since <rfc3339>` | Lower bound of the evidence window | last run watermark, else all |
| `--until <rfc3339>` | Upper bound of the evidence window | now |
| `--max-candidates <n>` | Cap candidates per run (cost control) | implementation-defined |
| `--repo <repo_id>` | Restrict synthesis to one repo identity | all repos in workspace |
| `--json` | Emit the machine-readable report (default for read commands) | on |

Like the rest of the CLI ([`README.md`](../README.md)), `dream` opens the store
directly and works without a running daemon. JSON goes to stdout, logs to
stderr.

## 2. HTTP surface (optional, Phase 2+)

A daemon-mode equivalent, envelope-aware like every other `/v1` endpoint
([`docs/codex-integration.md`](./codex-integration.md)):

```
POST /v1/dream
```

Request:

```json
{
  "profile": "personal",
  "workspace": "josh-personal",
  "repo": null,
  "mode": "preview",
  "since": null,
  "until": null,
  "max_candidates": 50
}
```

`mode` is `preview` or `apply`. The response `data` is the **dream report**
(§3). The HTTP path is optional for Phase 1 (CLI is sufficient) and is listed
here so the contract is fixed before a daemon exists.

## 3. Dream report (preview and apply output)

Both modes return the same shape; `preview` writes nothing, `apply` fills the
`created`/`archived` counts.

```json
{
  "mode": "preview",
  "run_id": "dream_…",
  "profile": "personal",
  "workspace": "josh-personal",
  "evidence_window": { "start": "…", "end": "…" },
  "evidence_scanned": {
    "visible_turns": 42, "conclusions": 3, "checkpoints": 2,
    "imported_memories": 7, "active_records": 31
  },
  "candidates": [
    {
      "action": "create",
      "proposed_type": "preference",
      "proposed_scope": "profile",
      "content": "Prefers repo-native commands (cargo test over ad-hoc scripts).",
      "confidence": 0.82,
      "state": "active",
      "drift_prone": false,
      "expires_at": null,
      "valid_until": null,
      "historical_reason": null,
      "promotion_reason": "repeated user steering across 3 turns",
      "evidence": [ { "kind": "visible_turn", "id": "turn_…" } ],
      "supersedes": [],
      "policy": "accept"
    },
    {
      "action": "supersede",
      "proposed_type": "decision",
      "content": "Storage uses rusqlite bundled SQLite (replaces earlier 'TBD storage').",
      "confidence": 0.9,
      "state": "completed",
      "drift_prone": false,
      "expires_at": null,
      "valid_until": null,
      "historical_reason": "newer completed evidence supersedes older active state",
      "supersedes": ["mem_old…"],
      "policy": "accept"
    }
  ],
  "rejected": [
    { "reason": "secret_detected", "evidence": [ { "kind": "visible_turn", "id": "turn_…" } ] }
  ],
  "stale": [
    {
      "memory_id": "mem_…",
      "drift_prone": true,
      "state": "planned",
      "expires_at": "2026-01-12T08:00:00Z",
      "valid_until": "2026-01-12T08:00:00Z",
      "suggested_action": "rewrite_historical",
      "historical_reason": "expired relative-time content"
    }
  ],
  "impact": { "records_added": 2, "records_archived": 1, "estimated_tokens": 180 },
  "created": 0,
  "archived": 0,
  "authority": "recall_not_authority"
}
```

- **Preview** sets `created` / `archived` to `0` and persists nothing except,
  optionally, the `dream_runs` audit row (see §6).
- **Apply** is idempotent: re-running over the same evidence window with no new
  evidence yields `created: 0, archived: 0`. Dedupe reuses the existing
  content-hash mechanism in [`src/store.rs`](../src/store.rs) /
  [`src/ingest.rs`](../src/ingest.rs).

## 4. Synthesis backend boundary

```rust
/// Input gathered deterministically by codex-memoryd from existing tables.
pub struct DreamInput {
    pub profile: Profile,
    pub workspace_id: String,
    pub window: EvidenceWindow,
    pub visible_turns: Vec<VisibleTurn>,
    pub conclusions: Vec<Conclusion>,
    pub checkpoints: Vec<Checkpoint>,
    pub imported_sources: Vec<MemorySource>,
    pub active_records: Vec<MemoryRecord>,
}

/// Proposals only — never persisted directly.
pub struct DreamOutput {
    pub candidates: Vec<DreamCandidate>,
}

pub trait DreamSynthesizer {
    fn synthesize(&self, input: DreamInput) -> DreamOutput;
}
```

Pipeline (deterministic gate around a swappable proposer):

```
gather evidence (store)            ── deterministic, codex-memoryd
   → DreamSynthesizer::synthesize  ── heuristic now, LLM later (PROPOSES)
   → policy gate (src/policy.rs)   ── deterministic (VALIDATES)
   → store/supersede (src/store.rs)── deterministic, apply-only (PERSISTS)
```

**Phase 1 ships a heuristic `DreamSynthesizer`** (repetition counting,
adoption detection, drift-language scan). An LLM synthesizer can be added behind
the same trait later **without** changing the policy/storage gate. The model
never persists; it only returns candidates.

## 5. Staleness and supersession rules

These are concrete enough to write fixture tests against.

### 5.1 Drift-prone detection

A candidate is `drift_prone = true` if its content contains relative-time or
planned-event language (case-insensitive, word-boundary):

```
today, tomorrow, tonight, this week, next week, this weekend,
currently, right now, soon, as of (now|today), going to, planning to, will <verb>
```

Planned vs. completed: phrases like "will deploy" / "planning to" are drift-prone
and SHOULD carry `valid_until`; completed past-tense statements ("deployed",
"merged") are not drift-prone on that axis.

### 5.2 Demotion / rewrite

For an existing `drift_prone` record older than its `valid_until`/`expires_at`
(or older than the recall `STALE_DAYS` hint in [`src/recall.rs`](../src/recall.rs)
when no `valid_until` is set), the loop SHOULD propose one of:

- `rewrite_historical` — restate as a dated historical fact ("As of <date>, …");
- `invalidate` — archive when superseded by newer contradicting evidence.

### 5.3 Supersession

When newer evidence contradicts an active record on the same subject:

- create the new record with `supersedes = [old_id]`;
- archive the old record (archive, not hard-delete — recoverable, consistent
  with [`src/store.rs`](../src/store.rs) `archive_stale_path_records` and the
  `/v1/forget` archival default);
- record `promotion_reason` and `evidence_window` provenance.

"Same subject" in the heuristic MVP = same `record_type` + high lexical overlap
within the same profile/workspace (and repo, when scoped to repo). An LLM
synthesizer can refine subject matching later.

### 5.4 Provenance metadata (no migration)

Every synthesized record carries, in the existing `metadata` JSON value:

```json
{
  "origin": "dreamer",
  "run_id": "dream_…",
  "evidence_window": { "start": "…", "end": "…" },
  "state": "completed",
  "drift_prone": false,
  "expires_at": null,
  "valid_after": null,
  "valid_until": null,
  "supersedes": ["mem_…"],
  "historical_reason": "newer completed evidence supersedes older planned state",
  "promotion_reason": "repeated user steering across 3 turns"
}
```

`supersedes` is also set on the first-class `MemoryRecord.supersedes` field; the
metadata copy captures supersessions discovered during the run for audit.

## 6. Storage proposal

**Minimal, no migration in this design's scope.** Two pieces:

1. **`memory_records.metadata.origin = "dreamer"` + provenance** (§5.4) on every
   synthesized record. No new column — `metadata` is already a free JSON value
   ([`src/domain.rs`](../src/domain.rs)).

2. **`dream_runs` audit table** (deferred to the Phase-2 implementation PR, when
   a migration is actually warranted). Proposed shape, for when it lands:

```sql
CREATE TABLE dream_runs (
  id                  TEXT PRIMARY KEY,
  profile_id          TEXT NOT NULL,
  workspace_id        TEXT NOT NULL,
  mode                TEXT NOT NULL,         -- preview | apply
  status              TEXT NOT NULL,         -- ok | error
  started_at          TEXT NOT NULL,
  completed_at        TEXT,
  model               TEXT,                  -- heuristic | <model id>
  input_hash          TEXT NOT NULL,         -- idempotency / replay key
  source_window_start TEXT,
  source_window_end   TEXT,
  summary             TEXT,
  error               TEXT
);
```

A **dream-run watermark** (the latest `source_window_end` per
profile/workspace) bounds the next incremental pass. This avoids a
source-selection table for v1.

**Deferred:** a per-candidate `dream_candidates` table. v1 computes and returns
candidates in the report rather than persisting them; persist only if previews
later need out-of-band replay/approval.

## 7. Eval fixtures

Fixtures live under `tests/fixtures/dreaming/` (seeded by this PR; see that
directory's `README.md`). Each file is **JSONL**: one JSON evidence event per
line, consistent with the existing `tests/fixtures` style.

Event shape:

```json
{ "kind": "visible_turn", "actor": "user", "content": "…", "created_at": "…" }
{ "kind": "conclusion", "content": "…", "created_at": "…" }
{ "kind": "checkpoint", "summary": "…", "created_at": "…" }
{ "kind": "memory_record", "type": "decision", "content": "…", "created_at": "…" }
```

Seeded scenarios:

| Fixture | What it proves |
| --- | --- |
| `repeated_preference.jsonl` | Repeated user steering is promoted to one stable `preference`. |
| `stale_time_sensitive_fact.jsonl` | Relative-time content is marked `drift_prone` and demoted/rewritten. |
| `conflicting_newer_fact.jsonl` | Newer evidence supersedes an older contradicting record. |
| `planned_vs_completed_transition.jsonl` | Planned work becomes historical/superseded after implemented/merged evidence. |
| `relative_time_expiry_tomorrow.jsonl` | `tomorrow` content expires after the deterministic clock advances. |
| `secret_rejection.jsonl` | A repeated secret is **never** synthesized (policy reject). |
| `repo_gotcha.jsonl` | A recurring failure is promoted to a `gotcha` scoped to the repo. |

### Eval assertions

For each scenario the harness checks the dream report:

- stable preference promoted (`repeated_preference`);
- secret never appears in `candidates`, appears in `rejected` with
  `secret_detected` (`secret_rejection`);
- stale fact flagged `drift_prone` with a demotion `suggested_action`
  (`stale_time_sensitive_fact`);
- `tomorrow`/`this week` style facts carry `valid_until`/`expires_at` and become
  `rewrite_historical` candidates after the clock advances
  (`relative_time_expiry_tomorrow`);
- newer evidence produces a `supersede` candidate referencing the old id
  (`conflicting_newer_fact`);
- planned/blocked/active task facts transition to `completed` supersession when
  later conclusions/checkpoints say implemented/fixed/merged/deployed
  (`planned_vs_completed_transition`);
- repo gotcha promoted with `scope = repo` (`repo_gotcha`);
- provenance present on every candidate (`promotion_reason`, `evidence`,
  `evidence_window`);
- **apply is idempotent** — second apply over the same window yields
  `created: 0`.

## 8. Phase boundaries

- **Phase 1 (preview skeleton):** `src/dream.rs`, `codex-memoryd dream
  --preview`, deterministic evidence gathering, heuristic `DreamSynthesizer`,
  report assembly, **no durable writes**. Tests: no-write behavior + policy
  rejection over the seeded fixtures.
- **Phase 2 (apply):** idempotent writes, supersession/archive, provenance
  metadata, `dream_runs` migration, conformance tests. Reuse
  [`src/policy.rs`](../src/policy.rs) and [`src/store.rs`](../src/store.rs);
  add no parallel write path.
- **Phase 3 (daemon)** and **Phase 4 (MCP/App)** are out of scope here; see the
  research doc's phased plan.
