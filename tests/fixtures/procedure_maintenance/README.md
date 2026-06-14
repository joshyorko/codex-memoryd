# Procedure maintenance fixtures (#158)

Scenario fixtures for the procedure-maintenance design
(`docs/procedure-maintenance.md`). **Data only** in the design PR — they define
the behavior the maintenance scorer and preview actions must satisfy, and are
wired into tests when the implementation lands after root review.

Each file describes a set of existing procedures (with their current + proposed
maintenance signals), and the maintenance outcome expected — a score ordering
and/or a set of preview candidate actions. No action is auto-applied.

```jsonc
{
  "scenario": "short-name",
  "description": "what this proves",
  "now": "2026-06-14T00:00:00Z",
  "procedures": [
    {
      "id": "proc_a",
      "name": "open a pull request",
      "state": "active",                  // candidate|active|retired|superseded|quarantined
      "activation_query": "opening a pull request",
      "steps": "review the diff; run cargo test",
      "negative_examples": ["deploying to production"],
      "source_episode_ids": ["ep1", "ep2"],
      "confidence": 0.8,
      // proposed maintenance signals (#158):
      "success_count": 4,
      "failure_count": 0,
      "reuse_count": 9,
      "last_used_at": "2026-06-10T00:00:00Z",
      "last_validated": "2026-06-10T00:00:00Z"
    }
  ],
  "expect": {
    "score_order": ["proc_a", "proc_b"],          // optional: highest score first
    "candidate_actions": [                          // preview only; never auto-applied
      { "action": "retire", "procedure_id": "proc_b", "reason": "stale_low_reuse" }
    ],
    "must_not": [                                   // safety regression hooks
      { "guarantee": "no_false_resurrection", "procedure_id": "proc_b" }
    ]
  }
}
```

## Conventions

- Synthetic only; no secrets.
- `candidate_actions` are **previews**; a fixture never expects an automatic
  mutation of stored procedures.
- `must_not` encodes the safety regression hooks from the design: no false
  resurrection of retired procedures, merges preserve both parents'
  `negative_examples`, and promote re-runs the unsafe-content guard.

## Scenario files

| File | Proves |
| --- | --- |
| `score_ranking.json` | A high-reuse/high-success procedure scores above a stale, failed one. |
| `merge_candidate.json` | Two near-identical procedures yield one merge candidate; the merged result must keep both negative examples. |
| `retire_stale.json` | A stale, low-reuse procedure becomes a retire candidate; never auto-applied. |
| `promote_recovered.json` | A quarantined procedure with fresh success evidence becomes a promote candidate, re-running the unsafe guard. |
| `no_false_resurrection.json` | Maintenance never moves a retired procedure back into default recall. |
