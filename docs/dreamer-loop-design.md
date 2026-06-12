# Dreamer loop — design (Phases 1–2 with service surface)

Companion to [`dreamer-loop-research.md`](./dreamer-loop-research.md). This
document specifies the **CLI/API contract, storage proposal, staleness and
supersession rules, the synthesis backend boundary, and the eval fixture
format** in enough detail to implement and run Phases 1 (preview) and 2 (apply).
It introduces **no schema migration**; everything fits the existing
[`MemoryRecord.metadata`](../src/domain.rs) JSON value and the existing policy /
recall / store boundaries.

> The model proposes; `codex-memoryd` validates and persists. Synthesized memory
> is `recall_not_authority`. See [`dreamer-loop-research.md`](./dreamer-loop-research.md)
> for motivation, non-claims, and threat model.

## Implementation status (2026-06-12)

- Implemented now:
  - Dreamer core exists in `src/dream.rs`.
  - CLI supports `dream --preview` and `dream --apply`.
  - Service entrypoints `Service::dream` and `Service::scheduled_dream` in
    `src/service.rs`.
  - HTTP endpoint `/v1/dream` exists in `src/server.rs`.
  - Durable dream run audit + watermark rows are persisted in the store.
  - Status includes last Dreamer run and scheduler state.
  - Test coverage includes promotion/rejection/supersession/stale-facts/secrets/user
    adoption/explicit conclusions/repeated steering/self-reinforcement blocking.
- Remaining work:
  - The loop is not fully productized; the report now exposes a first-class
    evidence window with per-stream counts and safe source refs, but synthesis
    is still record-centric and not yet a full product surface.
  - MCP Dreamer tooling is incomplete.
  - Upstream Codex native-memory parity is still missing in areas like idle/session
    eligibility, generated memory files, workspace-native semantics, and provider
    conformance.
  - Hardening follow-ups remain: windowing/supersession edge cases and atomic
    durable evidence writes.

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

## 2. HTTP surface (implemented)

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
(§3). The HTTP path is now implemented alongside CLI paths; no separate daemon-only
futures are required for local preview/apply.

## 3. Dream report (preview and apply output)

Both modes return the same shape; `preview` writes nothing, `apply` fills the
`created`/`archived` counts. Candidate state is report-local
(`accepted`, `quarantined`, `rejected`). Memory state is the proposed record
state (`planned`, `active`, `blocked`, `completed`, `historical`,
`superseded`) carried in metadata on apply.

```json
{
  "mode": "preview",
  "run_id": "dream_…",
  "profile": "personal",
  "workspace": "josh-personal",
  "evidence_window": {
    "start": null,
    "end": "…",
    "visible_turns": {
      "count": 42,
      "sources": [{ "id": "turn_…", "kind": "visible_turn" }]
    },
    "conclusions": { "count": 3, "sources": [] },
    "checkpoints": { "count": 2, "sources": [] },
    "imported_memories": { "count": 7, "sources": [] },
    "active_memory_records": { "count": 31, "sources": [] }
  },
  "candidates": [
    {
      "subject_key": "preference:profile:…",
      "action": "create",
      "proposed_type": "preference",
      "proposed_scope": "profile",
      "content": "Prefers repo-native commands (cargo test over ad-hoc scripts).",
      "confidence": 0.82,
      "candidate_state": "accepted",
      "threshold_reason": "strong user evidence weight 3.0 >= preference threshold",
      "evidence": [ { "kind": "visible_turn", "id": "turn_…", "class": "user_turn", "weight": 1.0 } ],
      "evidence_counts": {
        "visible_turns": 3, "conclusions": 0, "checkpoints": 0,
        "imported_memories": 0, "active_records": 0
      },
      "evidence_weights": {
        "user_turns": 3.0, "assistant_turns": 0.0, "conclusions": 0.0,
        "checkpoints": 0.0, "imported_memories": 0.0, "active_records": 0.0,
        "total_primary": 3.0, "total_corrob": 0.0
      },
      "promotion_reason": "repeated user steering across 3 turns",
      "state": "active",
      "drift_prone": false,
      "expires_at": null,
      "valid_until": null,
      "historical_reason": null,
      "supersedes": [],
      "policy": "accept"
    },
    {
      "subject_key": "decision:workspace:…",
      "action": "supersede",
      "proposed_type": "decision",
      "proposed_scope": "workspace",
      "content": "Storage uses rusqlite bundled SQLite (replaces earlier 'TBD storage').",
      "confidence": 0.9,
      "candidate_state": "accepted",
      "threshold_reason": "explicit conclusion plus active-record conflict on same subject_key",
      "evidence": [ { "kind": "conclusion", "id": "concl_…", "class": "conclusion", "weight": 2.0 } ],
      "evidence_counts": {
        "visible_turns": 0, "conclusions": 1, "checkpoints": 0,
        "imported_memories": 0, "active_records": 1
      },
      "evidence_weights": {
        "user_turns": 0.0, "assistant_turns": 0.0, "conclusions": 2.0,
        "checkpoints": 0.0, "imported_memories": 0.0, "active_records": 0.0,
        "total_primary": 2.0, "total_corrob": 0.0
      },
      "promotion_reason": "newer explicit conclusion supersedes stale active record",
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
    {
      "subject_key": "rejected:workspace:…",
      "action": "reject",
      "proposed_type": "other",
      "proposed_scope": "workspace",
      "content": "[redacted rejected evidence]",
      "confidence": 0.0,
      "candidate_state": "rejected",
      "threshold_reason": "policy rejected secret-like content before promotion",
      "evidence": [ { "kind": "visible_turn", "id": "turn_…", "class": "user_turn", "weight": 1.0 } ],
      "evidence_counts": {
        "visible_turns": 1, "conclusions": 0, "checkpoints": 0,
        "imported_memories": 0, "active_records": 0
      },
      "evidence_weights": {
        "user_turns": 1.0, "assistant_turns": 0.0, "conclusions": 0.0,
        "checkpoints": 0.0, "imported_memories": 0.0, "active_records": 0.0,
        "total_primary": 1.0, "total_corrob": 0.0
      },
      "promotion_reason": "secret-like content detected",
      "drift_prone": false,
      "policy": "secret_detected",
      "supersedes": []
    }
  ],
  "quarantined": [
    {
      "subject_key": "command:workspace:…",
      "action": "quarantine",
      "proposed_type": "command",
      "proposed_scope": "workspace",
      "content": "Run `cargo test` before merging.",
      "confidence": 0.42,
      "candidate_state": "quarantined",
      "threshold_reason": "assistant-only proposal has weak evidence without adoption",
      "evidence": [ { "kind": "visible_turn", "id": "turn_…", "class": "assistant_turn", "weight": 0.25 } ],
      "evidence_counts": {
        "visible_turns": 1, "conclusions": 0, "checkpoints": 0,
        "imported_memories": 0, "active_records": 0
      },
      "evidence_weights": {
        "user_turns": 0.0, "assistant_turns": 0.25, "conclusions": 0.0,
        "checkpoints": 0.0, "imported_memories": 0.0, "active_records": 0.0,
        "total_primary": 0.0, "total_corrob": 0.0
      },
      "promotion_reason": "assistant-only proposal requires user adoption",
      "drift_prone": false,
      "policy": "assistant_only",
      "supersedes": []
    }
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

- **Preview** sets `created` / `archived` to `0` and persists nothing: no memory
  records, conclusions, checkpoints, visible turns, archives, or `dream_runs`
  audit rows are written unless a later audit issue explicitly adds
  preview-run audit rows.
- **Apply** is idempotent: re-running over the same evidence window with no new
  evidence yields `created: 0, archived: 0`. Dedupe reuses the existing
  content-hash mechanism in [`src/store.rs`](../src/store.rs) /
  [`src/ingest.rs`](../src/ingest.rs).

### 3.1 Deterministic evidence classes and thresholds

`subject_key` is required on every candidate and is the deterministic anchor for
grouping, thresholding, and supersession. The normalizer lowercases, removes
volatile words, keeps the record family and scope, and includes repo identity
when repo-scoped so unrelated workspaces do not collide.

Initial evidence classes are weighted asymmetrically:

| Class | Weight | Role |
| --- | --- | --- |
| `user_turn` | `1.0` | Strong primary evidence. |
| `conclusion` | `2.0` | Strong explicit evidence. |
| `checkpoint` | `1.5` | Strong for task/repo state, next steps, gotchas, and conventions. |
| `assistant_turn` | `0.25` | Weak unless adopted by later user/checkpoint/conclusion evidence. |
| `imported_memory` | `0.5` | Corroborating only; cannot create active memory alone. |
| `active_record` | `0.0` | Conflict/supersession/expiry input only; never self-reinforcement. |

Threshold rules are deterministic and family-specific. Examples: repeated user
steering must cross the preference threshold across distinct evidence; durable
project decisions may promote from explicit conclusions; checkpoints can promote
task state; assistant-only proposals and imported-summary-only candidates are
quarantined or rejected. Same-turn repetition does not boost, explicit user
adoption boosts, and hedging language lowers confidence.

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

"Same subject" in the heuristic MVP = same deterministic `subject_key` within
the same profile/workspace (and repo, when scoped to repo), with lexical overlap
as a guardrail for early heuristics. An LLM synthesizer can refine wording later,
but it must not bypass the `subject_key` grouping and supersession anchor.

### 5.4 Provenance metadata (no migration)

Every synthesized record carries, in the existing `metadata` JSON value:

```json
{
  "origin": "dreamer",
  "dream_run_id": "dream_…",
  "subject_key": "decision:workspace:storage-backend-rusqlite",
  "evidence_ids": ["concl_…", "ckpt_…"],
  "evidence_count": 2,
  "user_evidence_count": 0,
  "assistant_evidence_count": 0,
  "first_seen_at": "…",
  "last_seen_at": "…",
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

Each `scenario.jsonl` SHOULD have a `scenario.expected.json` sidecar. Sidecars
make the proof explicit instead of relying on ad-hoc test prose:

```json
{
  "expect_preview": { "accepted": [], "rejected": [], "quarantined": [], "stale": [] },
  "expect_apply": { "created": 0, "archived": 0, "idempotent_second_apply": true },
  "expect_recall_before": { "query": "…", "must_not_contain": [] },
  "expect_recall_after": { "query": "…", "must_contain": [], "must_not_contain": [] }
}
```

Recall-before/after assertions are the key proof metric: accepted memory must
improve later coding-agent recall for the scenario query while forbidden content
(secrets, stale active facts, boundary-crossing facts) remains absent.

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
| `user_adopts_assistant_proposal.jsonl` | Assistant proposal plus explicit user adoption is promoted to a durable `command`. |
| `assistant_proposal_without_adoption.jsonl` | Assistant-only proposal is quarantined until user validates/adopts it. |
| `single_mention_preference_not_promoted.jsonl` | A single preference statement remains quarantined as unconfirmed. |
| `imported_memory_self_reinforcement_blocked.jsonl` | Imported memory cannot self-reinforce into active candidates without fresh evidence. |
| `explicit_conclusion_promotes.jsonl` | Explicit conclusion evidence promotes a `decision` when clear. |
| `repeated_user_steering_promotes.jsonl` | Repeated user steering promotes to a durable `command` candidate. |

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
- provenance present on every candidate (`subject_key`, `promotion_reason`,
  `threshold_reason`, `evidence`, `evidence_counts`, `evidence_weights`,
  `evidence_window`);
- sidecar recall-before/after assertions show recall improves where expected and
  does not leak forbidden content;
- **apply is idempotent** — second apply over the same window yields
  `created: 0`.

## 8. Implementation order

This design is implemented and hardened in the issue order below, so preview,
thresholds, apply, audit, reliability, and portability do not collapse into an
autonomous writer:

1. #10 deterministic preview-only source gathering and report shape.
2. #11 fixture sidecars with preview/apply and recall-before/after assertions.
3. #19 asymmetric evidence weighting, `subject_key`, thresholds, and candidate
   states.
4. #13 staleness, memory state transitions, drift/expiry metadata, and
   supersession.
5. #12 idempotent, policy-gated apply with required provenance metadata.
6. #20 safe dream-run audit rows and incremental watermarks.
7. #14 live Codex provider/hybrid smoke and daemon-down fail-open proof.
8. #15 loopback/auth/status/fail-open reliability contract.
9. #17 one-command local install and first-run demo.
10. #16 local-first coding-memory bakeoff with defensible public claims.
11. #18 local MCP recall/search/conclude/checkpoint/dream-preview tools.
