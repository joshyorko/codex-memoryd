# Temporal record fixtures (#155)

Scenario fixtures for the valid-time / as-of recall design
(`docs/temporal-records.md`). These are **data only** in the design PR — they
define the expected behavior the implementation must satisfy, and are wired
into a test (`tests/temporal_recall.rs`) when the schema slice lands after root
review.

Each file is one scenario:

```jsonc
{
  "scenario": "short-name",
  "description": "what this proves",
  "now": "2026-06-14T00:00:00Z",      // the clock the recall runs at
  "records": [
    {
      "id": "rec_a",
      "content": "...",
      "type": "preference",            // RecordType (snake_case)
      // temporal fields (any may be omitted = NULL / default):
      "temporal_state": "current",     // current|planned|completed|superseded|invalidated
      "valid_from": "2026-01-01T00:00:00Z",
      "valid_until": null,
      "observed_at": "2026-01-01T00:00:00Z",
      "invalidated_at": null,
      "superseded_by": null,
      "historical_reason": null
    }
  ],
  "queries": [
    {
      "name": "default-now",
      "as_of": null,                   // null = default current recall
      "expect_visible": ["rec_b"],     // record ids expected in recall facts
      "expect_withheld_reasons": ["temporal_historical"]
    },
    {
      "name": "as-of-march",
      "as_of": "2026-03-01T00:00:00Z",
      "expect_visible": ["rec_a"]
    }
  ]
}
```

## Conventions

- All timestamps are RFC3339 UTC.
- Omitting a temporal field means NULL — the `backfill_default_current`
  scenario relies on this to prove existing (pre-temporal) data is unaffected.
- `expect_visible` is the exact set of record ids that default/as-of recall
  must return (order-independent).
- `expect_withheld_reasons` (optional) asserts the content-free reasons; the
  unsafe/historical content must never appear in the withheld diagnostics.
- Fixtures are synthetic; no real data, no secrets.

## Scenario files

| File | Proves |
| --- | --- |
| `changed_preference.json` | Default recall returns only the current preference; as-of before the switch returns the old one. |
| `repo_state_change.json` | Default branch master→main resolves correctly per as-of date. |
| `completed_work.json` | Completed work is withheld from default current recall, inspectable on request. |
| `contradicted_claim.json` | A newer trusted contradiction invalidates the older current claim. |
| `relative_time_record.json` | A planned ("tomorrow") record is not emitted as current before its `valid_from`. |
| `backfill_default_current.json` | Records with no temporal fields behave exactly as today (back-compat regression guard). |
