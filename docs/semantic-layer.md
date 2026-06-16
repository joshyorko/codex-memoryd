# Semantic layer: entity resolution and relation graph — decision (#154)

> **Status: MVP IMPLEMENTED (relation substrate, eval slice, and explicit JSON preview/apply path).**
> `subject_aliases` and `relations` are live in the storage schema (v8), with
> explicit store apply methods, scoped reads, and relation-expanded retrieval in
> the deterministic eval path. This is an incremental slice, not the full
> default `/recall` integration: explicit import is user-facing, while default
> recall remains conservative.

This document compares three approaches for adding a semantic layer (entity
aliases + relations) to `codex-memoryd`, records the local-first decision, and
documents the implemented explicit preview/apply import path plus the multi-hop
fixture evidence.

## Non-goals (from the issue)

- No mandatory graph database.
- No opaque private graph reasoning.
- No automatic authority from inferred relations (recall-not-authority holds).

## The competitive context

Zep/Graphiti is strong precisely here: a temporal knowledge graph with
entity resolution and multi-hop traversal (see `docs/competitive-landscape.md`).
But its strength comes bundled with weaknesses we can exploit: it requires an
LLM for extraction, a graph backend (Neo4j/FalkorDB), and its contradiction
handling is "newest wins" with no policy/poison layer. Our advantage is
**local-first, deterministic, evidence-backed, reviewable**. The decision below
keeps that advantage rather than chasing graph parity.

## What we already have

- **Subjects** (`subjects` table): scoped, keyed entities — `(profile_id,
  workspace_id, subject_key)` unique, with `kind` (Person/Agent/Org/Project/
  Repo/Routine/Workflow/Device/Concept/Other), `display_name`, `metadata`.
- **Episodes**: append-only events linked to a subject.
- **Evidence ledger** (`evidence_ledger` table, full column set):
  `(id, event_key [UNIQUE], profile_id, workspace_id, repo_id, subject_key,
  source_kind, source_id, source_path, source_hash, safe_summary, policy_state,
  created_at, metadata)` plus the trust columns back-filled by
  `ensure_trust_columns`. The `event_key` is a SHA-256 over
  profile/workspace/repo/subject_key/source_kind/source_id/source_path/
  source_hash/policy_state and is the dedupe + reference handle relations and
  aliases will cite. This is the provenance spine relations reuse.
- **Procedures**: already link `source_episode_ids` and carry preview/apply +
  lifecycle. The relation candidate flow mirrors this.

So we already have **entities** (subjects). #154 is really two smaller things:
(1) entity *aliasing* (two subject_keys are the same thing), and (2) typed
*relations* between subjects, evidence-backed and reviewable.

## Option comparison

### Option A — No graph (explicit non-goal, prove we win elsewhere)

Keep subjects/episodes; add nothing. Answer multi-hop questions by chaining
recall calls at the agent layer.

| | |
| --- | --- |
| Pros | Zero new surface, zero risk, nothing to poison or mis-rank. |
| Cons | Multi-hop ("what does the project Alice owns depend on?") requires the agent to issue N recalls and join manually; we can't *measure* or improve it. No alias resolution, so "Al" and "Alice" stay separate subjects. |
| Verdict | Honest baseline. If the multi-hop fixtures show chained recall already answers the questions, we ship this as a documented non-goal with proof. |

### Option B — Lightweight local relation tables (recommended)

Two small SQLite tables on top of subjects: `subject_aliases` and
`relations`. No graph engine — traversal is a bounded recursive SQL query (or
N indexed lookups) over local rows. Relations are **candidates** until reviewed
(preview/apply), evidence-backed, scoped by profile/workspace, recall-not-authority.

| | |
| --- | --- |
| Pros | Local-first, deterministic, no new dependency, reuses the proven preview/apply + evidence pattern. Bounded multi-hop (depth ≤ 2–3) is cheap with indexes. Reviewable and poison-resistant (relations go through the same policy gate). |
| Cons | Not a "real" graph — no arbitrary-depth pathfinding, no graph algorithms. Recursive CTE depth must be capped to stay within perf budgets (#152). |
| Verdict | **Recommended.** Matches local-first + recall-not-authority, and is the smallest thing that lets us *measure* whether relation-aware recall helps. |

### Option C — External graph database (Neo4j/FalkorDB/etc.)

Embed or require a graph backend.

| | |
| --- | --- |
| Pros | True multi-hop, mature traversal. Parity with Zep/Graphiti. |
| Cons | Violates "no mandatory graph database" and local-first: new heavy dependency, separate process/storage, harder backup/restore (breaks #141), harder to keep deterministic and poison-gated. Massive surface for a feature we haven't proven we need. |
| Verdict | **Rejected for v0.x.** Revisit only if Option B's multi-hop fixtures show relation tables are insufficient AND there's measured demand. |

## Recommendation

**Option B, proven by the deterministic eval slice.** The local relation tables
and semantic fixtures show relation-aware retrieval improves the multi-hop
`q_multihop_evidence` case while preserving cross-profile isolation. The
remaining #154 delivery is an explicit reviewed JSON preview/apply path for
aliases and relations, which keeps graph mutation user-facing and evidence
backed.

## Implemented shape (Option B)

### Subject aliases

Idempotent table; an alias maps an alternate key to a canonical subject.

```
subject_aliases(
  id, profile_id, workspace_id,
  subject_id        -> canonical subjects.id
  alias_key         TEXT  -- normalized alternate key ("al" -> subject "alice")
  source_evidence   TEXT  -- evidence ledger event_key or episode id
  created_at,
  UNIQUE(profile_id, workspace_id, alias_key)
)
```

Resolution is deterministic: look up `alias_key`; if found, resolve to
`subject_id`. No fuzzy/embedding matching in v0.x (that would be #156 territory
and is explicitly out of scope here). Aliases are applied only through explicit
import, never through silent merges.

### Relations

```
relations(
  id, profile_id, workspace_id,
  from_subject_id   -> subjects.id
  relation_type     TEXT  -- uses|owns|prefers|works_on|depends_on|supersedes|blocked_by
  to_subject_id     -> subjects.id
  confidence        REAL
  state             TEXT  -- candidate|active|retired   (mirrors procedures)
  source_episode_ids TEXT -- JSON array, evidence provenance
  source_evidence   TEXT  -- evidence ledger event_key
  created_at, retired_at,
  metadata
)
```

Closed relation vocabulary (from the issue): `uses`, `owns`, `prefers`,
`works_on`, `depends_on`, `supersedes`, `blocked_by`. A closed set keeps it
deterministic and reviewable; new types are an additive change.

**Scope enforcement.** `relations` carries its own `profile_id`/`workspace_id`,
and the invariant is that `from_subject_id` and `to_subject_id` both resolve to
subjects in that same scope. SQLite cannot express that as a cross-table `CHECK`
constraint, so enforcement is at the **application layer** in `relations apply`:
the apply path resolves both endpoints with the existing
`subject_exists_in_scope(profile, workspace, id)` check and rejects (does not
insert) any relation whose endpoints are out of scope — the same way procedure
apply already validates `subject_id` scope. Relation-aware recall additionally
filters traversal to the requested scope, so even a hypothetically mis-inserted
cross-scope row could never be traversed across the boundary. The
`scope_isolation` fixture asserts this.

### Preview / apply (explicit JSON import)

Reviewed aliases and relations are previewed and applied from a JSON file:

```bash
codex-memoryd semantic preview --file semantic.json
codex-memoryd semantic apply --file semantic.json
```

Preview validates the file and prints `would_apply`, `already_present`, and
`rejected` entries. It performs **no writes**. Apply runs the same validation,
writes valid reviewed entries, reports rejected entries, and remains idempotent.

The import file uses one top-level scope:

```json
{
  "profile_id": "personal",
  "workspace_id": "josh-personal",
  "aliases": [
    {
      "subject_id": "subj_alice",
      "alias_key": "al",
      "source_evidence": "episode:ep_alias"
    }
  ],
  "relations": [
    {
      "from_subject_id": "subj_alice",
      "relation_type": "owns",
      "to_subject_id": "subj_billing",
      "confidence": 0.92,
      "source_episode_ids": ["episode:ep_owns_billing"],
      "source_evidence": "episode:ep_owns_billing"
    }
  ]
}
```

All evidence-backed: aliases require `source_evidence`, and relations require
`source_episode_ids` plus `source_evidence`. Relations are recall-not-authority:
they inform deterministic discovery/eval paths but never command.

### Evidence refs and profile/workspace boundaries

- Every relation and alias carries provenance (`source_episode_ids` and/or an
  evidence-ledger `event_key`), reusing the existing ledger spine.
- Relations are strictly scoped: `from_subject` and `to_subject` must be in the
  **same** profile/workspace. No cross-profile relations (would be a bleed
  vector — the boundary matrix from the security work applies).
- Relation-aware recall, if built, only traverses relations in the requested
  scope, and withheld/quarantined subjects' relations are not traversed.

### Relation-aware recall (only if proven)

Bounded expansion: given a query that resolves to a subject, the deterministic
eval path can pull in records of subjects within depth <= 2 along `active`
relations, with relation evidence preserved in the expansion result. Capped
depth and capped fan-out keep it inside the perf budget.

## Multi-hop fixtures and eval

`tests/fixtures/semantic/` defines subjects, episodes (evidence), proposed
relations, and multi-hop questions with expected answers under two retrieval
modes:

- **`chained_recall`** (Option A baseline): can the answer be assembled by
  issuing plain recalls per subject and joining?
- **`relation_aware`** (Option B): does traversing relations return the answer
  set directly / more precisely?

The fixtures are the **decision instrument**. The retrieval eval reports
`relation_aware_recall` improving `q_multihop_evidence` while keeping
`cross_profile_leak` false.

| File | Multi-hop question | Tests |
| --- | --- | --- |
| `owns_depends_on.json` | "What does the project Alice owns depend on?" (alice —owns→ billing —depends_on→ stripe) | 2-hop traversal returns `stripe` |
| `alias_resolution.json` | "Al" and "Alice" are the same person; a fact on "Al" answers a question about "Alice" | alias resolves, fact recalled |
| `works_on_blocked_by.json` | "Is anything Bob works on blocked?" (bob —works_on→ api, api —blocked_by→ auth-migration) | 2-hop returns the blocker |
| `scope_isolation.json` | A relation in the `work` profile must not be traversed from `personal` | cross-profile relation never expands |
| `no_relation_baseline.json` | A single-hop question where chained recall already suffices | proves we don't over-claim; relation-aware must not regress it |

See `tests/fixtures/semantic/README.md` for the file format.

## Migration status

Schema v8 includes `subject_aliases` and `relations` via
`migrations/0009_semantic_relations.sql`. The migration is additive under
`docs/compatibility-policy.md`, with schema-upgrade coverage asserting both
tables exist and start empty after upgrade.

## Follow-up boundaries

- Alias merge semantics stay pointer-only: no destructive subject row collapse.
- Relation extraction is not automatic in this slice. Reviewed JSON import is
  the user-facing mutation path.
- #155 temporal schema/version work must coordinate with the current storage
  version, but temporal fields and as-of recall stay out of #154.
