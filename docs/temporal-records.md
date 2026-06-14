# Temporal records and as-of recall — design (#155)

> **Status: DESIGN ONLY — nothing here is implemented yet.** No temporal columns,
> migration, `ensure_temporal_columns` helper, recall predicate, or withheld
> reason described below exists in the code today. `memory_records` currently
> has no valid-time fields and `STORAGE_SCHEMA_VERSION` is 7. Every "add",
> "proposed", "would", and field/method name in this document is a *proposal*
> for an implementation PR that lands **only after root review**. This file is
> the spec the fixtures in `tests/fixtures/temporal/` will be validated against
> once that implementation exists; the fixtures are not wired into any test yet.

This document proposes a general time-aware record model so changing facts
become *historical* instead of staying active forever, and an *as-of* recall
mode that answers "what was current on date X".

It deliberately mirrors the **procedure lifecycle** pattern already shipped
(migration 0008 + `ensure_procedure_lifecycle_columns` + `retire`/`supersede`/
`counter_evidence` store methods), because that pattern is proven, reviewable,
and recall-not-authority. Temporal records are the same idea generalized from
procedures to ordinary records.

## Non-goals (from the issue)

- No full temporal graph database.
- No hidden model-only arbitration of which fact wins.
- No deletion of historical evidence — history stays inspectable.

## Why now

`memory_records` already has `created_at`, `updated_at`, `last_used_at`,
`archived`, a `supersedes` JSON array, and trust/quarantine columns
(`src/store.rs` `RECORD_COLS`). Recall already de-prioritizes stale records
(`STALE_DAYS = 120` in `src/recall.rs`) and withholds records whose metadata
marks them `superseded` (`policy_superseded` withheld reason). What is missing
is **valid-time semantics**: the difference between *when we learned a fact*
and *when the fact was true in the world*, and the ability to query memory
as-of a past instant.

Today "archived" conflates several distinct meanings (deleted-ish, stale,
superseded). The temporal model separates them.

## Concepts: two clocks

Borrowed from bi-temporal databases, kept minimal:

- **Valid time** — when the fact is true *in the world* (`valid_from`,
  `valid_until`). "I prefer tabs" became true on 2026-01-01 and stopped being
  true on 2026-06-01.
- **Transaction/observation time** — when the substrate *learned/recorded* it
  (`observed_at`, and the existing `created_at`/`updated_at`).

Separating these is what lets as-of recall answer both "what did I believe on
date X" (transaction time) and "what was actually true on date X" (valid time).
The default and fixtures below use **valid time** for as-of unless stated.

## Proposed fields

Added to `memory_records` via an idempotent `ensure_temporal_columns` helper
(see Migration strategy). All nullable / defaulted so existing rows upgrade
losslessly — exactly like the trust and procedure-lifecycle columns.

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `valid_from` | TEXT (RFC3339) | NULL | When the fact became true in the world. NULL = "as long as we've known it" (falls back to `created_at`). |
| `valid_until` | TEXT (RFC3339) | NULL | When the fact stopped being true. NULL = still valid ("open interval"). |
| `observed_at` | TEXT (RFC3339) | NULL | When the substrate observed/recorded the fact. NULL falls back to `created_at`. |
| `invalidated_at` | TEXT (RFC3339) | NULL | When the substrate marked this record no longer current (transaction-time event). Distinct from `valid_until`. |
| `superseded_by` | TEXT | NULL | Id of the record that replaced this one (scalar link, mirrors procedures). Complements the existing `supersedes` array on the *new* record. |
| `historical_reason` | TEXT | NULL | Stable code for *why* it became historical: `expired` \| `contradicted` \| `superseded` \| `completed` \| `manual`. |
| `temporal_state` | TEXT | `'current'` | Lifecycle state (see below). Defaults to `current` so all existing rows are treated as current, preserving today's behavior. |

`temporal_state` is the one new "status" column. Everything else is timestamps
or a link. Per the proven pattern (#149/#151 reviews), we avoid scattering
state across many booleans.

## Lifecycle states and transitions

```
            (create with future valid_from)
                        │
                        ▼
   planned ──(valid_from reached)──▶ active/current ──┐
                                       │   │           │
                  (work finished)──────┘   │           │
                        │                  │           │
                        ▼                  │           │
                   completed               │           │
                                           │           │
        (newer record supersedes)──────────┘           │
                        │                               │
                        ▼                               │
                  superseded                            │
                                                        │
   (contradicted by newer trusted evidence)────────────┘
                        │
                        ▼
                  invalidated ──────────────▶ historical (umbrella: inspectable, not current)
```

State definitions:

| State | In default recall? | Set by |
| --- | --- | --- |
| `planned` | No (future fact) | create with `valid_from` in the future |
| `current` | **Yes** | default for all records |
| `completed` | No (done, not current context) | explicit transition with `historical_reason='completed'` |
| `superseded` | No | `supersede` transition; sets `superseded_by`, `historical_reason='superseded'` |
| `invalidated` | No | `invalidate` transition; sets `invalidated_at`, `historical_reason='contradicted'|'expired'` |

`historical` is the umbrella for any non-`current`, non-`planned` state when
talking about recall behavior — all of them are inspectable but withheld from
default current recall.

These states would map onto recall withholding the same way the existing
withheld reasons do (`policy_superseded`, `quarantined`, … — see `build_withheld`
in `src/recall.rs`, which today emits none of the temporal reasons). The proposal
would **add** two new withheld reason strings, `temporal_historical` and
`temporal_planned` — an additive change under `docs/compatibility-policy.md`.
Neither exists in `recall.rs` yet.

## Default current-recall behavior (proposed; unchanged for existing data)

Proposed predicate once implemented: default recall (no `as_of` given) would
emit a record only if **all** hold (this logic is not in `recall.rs` today):

1. `temporal_state = 'current'` (the migration would default existing rows to
   this), AND
2. `valid_from` is NULL or ≤ now, AND
3. `valid_until` is NULL or > now, AND
4. `invalidated_at` is NULL, AND
5. it already passes today's gates (not archived, not quarantined, not
   secret-blocked, profile/workspace boundary).

The back-compat goal: because the migration would default every existing row to
`temporal_state='current'` with NULL temporal timestamps, default recall *would*
behave exactly as today until temporal fields are explicitly set. This is the
same back-compat property the schema upgrade matrix (#140) enforces for other
column additions; the `backfill_default_current` fixture is written to assert it
once the implementation exists. (Today, with no temporal columns and no temporal
checks in `recall.rs`, default recall is of course already the current behavior —
the design's job is to *keep* it so after the columns are added.)

Historical/invalidated/planned records are **withheld** from default recall
with a content-free reason (never leaking the historical content), and remain
fully visible via `include_historical` / as-of / inspection paths.

## As-of recall

A new optional recall parameter `as_of: <RFC3339>` (and CLI `--as-of`) changes
the predicate to "what was current at instant T":

1. `valid_from` is NULL or ≤ T, AND
2. `valid_until` is NULL or > T, AND
3. `observed_at` is NULL or ≤ T (we only "knew" facts observed by T — this is
   the bi-temporal guard that prevents leaking future knowledge into a past
   query), AND
4. the record was not invalidated *before* T (`invalidated_at` is NULL or > T).

Crucially, as-of recall **can** surface a record that is `superseded`/
`invalidated` *today*, as long as it was current at T. That is the whole point:
"on 2026-03-01, my editor preference was spaces" even though it is tabs now.

As-of is read-only and recall-not-authority; it never mutates state.

## Newer contradictory evidence retires older current records

Mirrors `supersede_procedure`. A proposed store method (design only):

```
fn supersede_record(profile, workspace, old_id, new_id, reason, now)
  -> old: temporal_state='superseded', superseded_by=new_id,
          valid_until=now (if open), historical_reason=reason,
          invalidated_at unchanged (valid_until is the world-clock close)
  -> new: supersedes += old_id (existing array)

fn invalidate_record(profile, workspace, id, reason, now)
  -> temporal_state='invalidated', invalidated_at=now,
     valid_until=now (if open), historical_reason=reason
```

Both are **preview/apply**, evidence-backed, and never auto-applied — the
contradiction is detected (e.g. by the Dreamer or an explicit conclude that
contradicts), proposed as a candidate, and a human/agent reviews before apply.
This satisfies "no hidden model-only arbitration."

## As-of recall fixtures

`tests/fixtures/temporal/` (data only in this PR; wired into tests when
implementation lands). Each fixture is a scenario: a set of records with
temporal fields, a query (optionally with `as_of`), and the expected visible
record ids + withheld reasons. See `tests/fixtures/temporal/README.md`.

Fixture families (one file each):

| File | Scenario | Asserts |
| --- | --- | --- |
| `changed_preference.json` | Editor pref spaces→tabs over time | default recall returns only tabs; as-of(before switch) returns spaces |
| `repo_state_change.json` | Default branch master→main | as-of resolves the branch correct for each date |
| `completed_work.json` | A task planned→active→completed | completed work withheld from default current recall, visible on inspect |
| `contradicted_claim.json` | "tests pass" then "tests fail" | newer trusted contradiction invalidates the older current claim |
| `relative_time_record.json` | "I will deploy tomorrow" with a `now` | planned record not emitted as current before its `valid_from` |
| `backfill_default_current.json` | Records with NO temporal fields | behaves exactly as today (regression guard for back-compat) |

## Migration strategy

Follows the 0006/0008 convention exactly:

1. Add `migrations/0009_temporal_records.sql` as a **no-op marker** (comment
   only), like `0008_procedure_lifecycle.sql`.
2. Add `ensure_temporal_columns(conn)` in `src/store.rs`, called from
   `migrate()` after the procedure-lifecycle back-fill. It `ensure_column`s
   each field above with safe defaults (`temporal_state TEXT NOT NULL DEFAULT
   'current'`, the rest NULL).
3. Bump `STORAGE_SCHEMA_VERSION` 7 → 8. Per the lesson from PR #160, reference
   the constant in tests, never a literal; update `tests/fixtures/status.response.json`.
4. Extend the `tests/schema_upgrade.rs` matrix with a v7→v8 case proving a
   pre-temporal DB upgrades losslessly and existing rows read back as
   `temporal_state='current'` with NULL temporal timestamps.
5. Indexing is an **implementation-planning open question**, not part of this
   design PR: the impl PR should add `valid_from`/`valid_until`/`temporal_state`
   to the recall indexes (likely a partial index `WHERE temporal_state='current'`
   for the hot path) and confirm as-of queries stay within the perf budgets
   (#152). No index work happens until then.

No existing column changes type or meaning, so this is an **additive** change
under `docs/compatibility-policy.md`. The `superseded`/historical withheld
behavior is additive to the recall contract (new withheld reason strings).

## Open questions for root review

1. **Valid-time vs transaction-time default for as-of.** Proposed: valid time.
   Do we also expose `--as-of-known` (transaction time)?
2. **Who proposes invalidation?** When implemented, the first slice would be
   fields + the as-of read path only (read-only). Auto-contradiction detection
   is a candidate for the Dreamer (#157) — out of scope here, but the
   `supersede_record`/`invalidate_record` preview/apply shape should be agreed
   now so #157 can build on it.
3. **`temporal_state` vs reusing `archived`.** Proposed: separate column.
   `archived` stays "soft-deleted"; `temporal_state` carries time semantics.
   Confirm we don't want to overload `archived`.
4. **Interaction with the existing `supersedes` array.** Proposed: keep the
   array on the new record (provenance: "I replace these"), add scalar
   `superseded_by` on the old record (lifecycle: "I was replaced by"). Both
   directions, mirroring procedures. Confirm.
