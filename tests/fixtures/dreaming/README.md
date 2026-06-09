# Dreamer loop eval fixtures

Specified by [`../../../docs/dreamer-loop-design.md`](../../../docs/dreamer-loop-design.md)
§7. Each `*.jsonl` file is a **scenario**: one JSON evidence event per line, in
chronological order. They are the offline evidence a Dreamer pass would scan.

These fixtures are **seeded in Phase 0 (research/design)** and are consumed by
the Phase 1+ implementation. They contain no real secrets — the
`secret_rejection.jsonl` fixture uses an obviously fake credential shape solely
to assert the policy gate rejects it.

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

See the design doc §7 for the per-scenario eval assertions.
