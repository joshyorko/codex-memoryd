# Semantic / multi-hop fixtures (#154)

Decision-instrument fixtures for the semantic-layer design
(`docs/semantic-layer.md`). These are **data only** in the design PR. Their
purpose is to answer one question before any code is written: **does
relation-aware recall (Option B) beat chained plain recall (Option A) on
multi-hop questions?** If not, Option A ships as a documented non-goal.

Each file defines synthetic subjects, episodes (evidence), proposed relations
and aliases, and a multi-hop question with the expected answer under both
retrieval modes.

```jsonc
{
  "scenario": "short-name",
  "description": "the multi-hop question and what it proves",
  "profile": "personal",
  "workspace": "semantic-eval",
  "subjects": [
    { "id": "alice", "subject_key": "person:alice", "kind": "person", "display_name": "Alice" }
  ],
  "aliases": [
    { "alias_key": "person:al", "subject_id": "alice", "source_evidence": "ep_intro" }
  ],
  "episodes": [
    { "id": "ep_owns", "subject_id": "alice", "summary": "Alice owns the billing project.", "status": "success" }
  ],
  "relations": [
    {
      "from_subject_id": "alice",
      "relation_type": "owns",
      "to_subject_id": "billing",
      "source_episode_ids": ["ep_owns"],
      "state": "active"
    }
  ],
  "question": "What does the project Alice owns depend on?",
  "expect": {
    "chained_recall": { "answerable": true,  "note": "agent must issue 2 recalls and join" },
    "relation_aware": { "answerable": true,  "answer_subject_ids": ["stripe"], "max_depth": 2 }
  }
}
```

## Relation vocabulary

Closed set (from the issue): `uses`, `owns`, `prefers`, `works_on`,
`depends_on`, `supersedes`, `blocked_by`.

## Conventions

- Synthetic only; no real people/orgs, no secrets.
- `expect.relation_aware.answer_subject_ids` is the exact set the bounded
  traversal must return.
- `max_depth` documents the hop count needed (≤ 2–3 per the design).
- `scope_isolation` proves relations never traverse across profile/workspace.
- `no_relation_baseline` proves we do not over-claim: a single-hop question
  that chained recall already answers, which relation-aware recall must not
  regress.

## Scenario files

| File | Multi-hop question |
| --- | --- |
| `owns_depends_on.json` | What does the project Alice owns depend on? (2-hop) |
| `alias_resolution.json` | A fact stored on "Al" answers a question about "Alice". |
| `works_on_blocked_by.json` | Is anything Bob works on blocked? (2-hop) |
| `scope_isolation.json` | A `work`-profile relation must not expand from `personal`. |
| `no_relation_baseline.json` | Single-hop question chained recall already answers. |
