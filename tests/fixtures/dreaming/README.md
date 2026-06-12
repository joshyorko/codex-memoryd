# Dreamer loop eval fixtures

Specified by [`../../../docs/dreamer-loop-design.md`](../../../docs/dreamer-loop-design.md)
§7. Each `*.jsonl` file is a **scenario**: one JSON evidence event per line, in
chronological order. They are the offline evidence a Dreamer pass would scan.

These fixtures are **seeded in Phase 0 (research/design)** and are consumed by
the Phase 1+ implementation. They contain no real secrets — the
`secret_rejection.jsonl` fixture uses an obviously fake credential shape solely
to assert the policy gate rejects it.

Each `scenario.jsonl` is paired with a `scenario.expected.json` sidecar when the
scenario is executable. The sidecar is the contract for preview buckets, apply
counts/idempotency, and recall-before/after assertions. Recall-before/after is
the primary proof metric: Dreamer is useful only when apply improves future
recall for the scenario query while forbidden or stale content remains absent.

## Event shape

```json
{ "kind": "visible_turn", "actor": "user", "content": "…", "created_at": "…" }
{ "kind": "conclusion", "content": "…", "created_at": "…" }
{ "kind": "checkpoint", "summary": "…", "created_at": "…" }
{ "kind": "memory_record", "type": "decision", "content": "…", "created_at": "…" }
```

`kind` is one of `visible_turn`, `conclusion`, `checkpoint`, `memory_record`.
Additional optional fields: `actor` (turns), `type` (records), `repo_id`,
`id` (so supersession fixtures can reference an existing record).

## Sidecar shape

```json
{
  "expect_preview": { "accepted": [], "rejected": [], "quarantined": [], "stale": [] },
  "expect_apply": { "created": 0, "archived": 0, "idempotent_second_apply": true },
  "expect_recall_before": { "query": "…", "must_not_contain": [] },
  "expect_recall_after": { "query": "…", "must_contain": [], "must_not_contain": [] }
}
```

Preview expectations match deterministic candidates by `subject_key` plus the
same-subject grouping/bridge heuristics used by the Dreamer core, candidate
state (`accepted`, `quarantined`, or `rejected`), evidence/provenance fields,
policy result, and drift/supersession metadata. Apply expectations assert
created/archived counts and that a second apply over the same evidence window is
idempotent. Recall expectations compare the store before and after apply.

## Scenarios

| Fixture | What a Dreamer pass should do |
| --- | --- |
| `repeated_preference.jsonl` | Promote repeated user steering into one stable `preference`. |
| `stale_time_sensitive_fact.jsonl` | Mark relative-time content `drift_prone`; propose demotion/rewrite. |
| `conflicting_newer_fact.jsonl` | Supersede an older contradicting record with the newer fact. |
| `planned_vs_completed_transition.jsonl` | Turn planned work historical/superseded once completion evidence appears. |
| `relative_time_expiry_tomorrow.jsonl` | Expire `tomorrow`/relative-time content after the clock advances. |
| `secret_rejection.jsonl` | Never synthesize a repeated secret; reject with `secret_detected`. |
| `repo_gotcha.jsonl` | Promote a recurring failure into a `gotcha` scoped to the repo. |
| `user_adopts_assistant_proposal.jsonl` | Assistant proposal plus explicit user adoption is promoted into `command`. |
| `assistant_proposal_without_adoption.jsonl` | Assistant-only proposal is quarantined without user adoption. |
| `single_mention_preference_not_promoted.jsonl` | Single user preference mention stays low-confidence and does not promote. |
| `imported_memory_self_reinforcement_blocked.jsonl` | Imported/active memory self-reinforcement is blocked without fresh primary evidence. |
| `explicit_conclusion_promotes.jsonl` | Explicit conclusion in evidence is accepted when it is clear and authoritative. |
| `repeated_user_steering_promotes.jsonl` | Repeated user steering is promoted to one stable `command`. |

See the design doc §7 for the per-scenario eval assertions, including
sidecar-required provenance (`subject_key`, evidence counts, promotion/threshold
reason) and recall-before/after checks.
