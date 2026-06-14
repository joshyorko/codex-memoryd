# Procedure maintenance: score, compact, prune — design (#158)

> **Status: DESIGN ONLY — nothing here is implemented yet.** There is no
> maintenance score, no `procedure maintenance` command, no
> `success_count`/`reuse_count`/`last_used_at` columns, and no merge/split logic
> in the code today. The procedures table currently has `confidence`,
> `counter_evidence_count`, `last_validated`, `version`, `state`,
> `source_episode_ids`, `negative_examples` (migrations 0007/0008). Everything
> below ("add", "new", "proposed") is a *proposal* for an implementation PR that
> lands **only after root review**. The fixture plan describes files to be added
> with the implementation; they are not wired into any test in this PR.

This document designs a procedure *maintenance* layer on top of the existing
procedure substrate (migrations 0007/0008): a deterministic **score** per
procedure, a **maintenance report**, and **preview-only** maintenance actions
(merge / split / retire / quarantine / promote). It is the ProcMEM-competitive
"keep the skill library healthy" layer. No automatic apply.

It reuses everything already built — `confidence`, `counter_evidence_count`,
`last_validated`, `version`, `state`, `source_episode_ids`, `negative_examples`,
and the `retire_procedure` / `supersede_procedure` /
`record_procedure_counter_evidence` store methods — and adds only scoring and a
reviewable maintenance surface.

## Non-goals

- No automatic apply. Maintenance is **preview-only** in this design; an
  operator/agent reviews and applies explicitly (mirrors `procedures apply`,
  `patch apply`).
- No model-only arbitration of which procedure wins.
- No deletion of procedure history (retired/superseded stay inspectable).
- Not Async Dreamer (#157). Maintenance produces a report and candidate
  actions; orchestrating them on a schedule is #157's job, out of scope here.

## Why

The procedure substrate can derive, activate (with abstention), retire, and
supersede procedures. What it cannot yet do is answer "which procedures are
**healthy**, which are **stale or low-value**, and which are **duplicates** that
should be merged?" Without that, a long-lived store accumulates near-duplicate
and stale procedures that dilute recall — the exact failure mode the procedural
literature (Voyager/AWM additive-only libraries) calls out and that Skill-Pro's
prune-by-`freq × avg_gain` addresses. We do it locally and reviewably.

## Score model

A deterministic per-procedure score computed from existing + a few new signals.
No model in the loop; reproducible offline.

### Signals

| Signal | Source | Notes |
| --- | --- | --- |
| `success_count` | NEW counter, bumped on validated reuse | complements `last_validated` |
| `failure_count` | existing `counter_evidence_count` | already tracked |
| `reuse_count` | NEW counter, bumped on every recall activation | total times the procedure fired |
| `last_used_at` | NEW timestamp, set on recall activation | distinct from `last_validated` (used vs. confirmed-good) |
| `evidence_count` | `source_episode_ids.len()` | already present |
| `confidence` | existing `confidence` column | current point estimate |
| `confidence_trend` | NEW: derived from a small append-only `procedure_score_events` log | up / flat / down over the last N events |
| `age_days` | from `created_at` / `first_seen` | staleness input |
| `staleness` | `now - last_validated` (or `last_used_at`) | drives stale-retire candidates |

`success_count`, `reuse_count`, `last_used_at`, and the `procedure_score_events`
log are the only new storage. They are added via the proven idempotent
`ensure_*_columns` pattern + an optional new table (design choice for root: a
few counters on the `procedures` row vs. a separate append-only events table —
recommendation below).

### Score formula (deterministic, tunable)

```
quality = w_reuse   * normalize(reuse_count)
        + w_success * success_rate            // success_count / (success_count + failure_count)
        + w_evidence* normalize(evidence_count)
        + w_conf    * confidence
        - w_stale   * staleness_penalty(last_validated, now)
        - w_fail    * normalize(failure_count)
```

Weights live in config (defaults documented), so the score is explainable and
ablatable — the same philosophy as the recall ranking and the comparative eval.
The report shows each component so a low score is actionable, never a black box.

## Maintenance report

`codex-memoryd procedure maintenance` (design; preview-only) emits, per scope:

```jsonc
{
  "suite": "procedure_maintenance",
  "version": 1,
  "scored": [
    {
      "id": "proc_...",
      "name": "...",
      "state": "active",
      "score": 0.71,
      "components": { "reuse": 0.4, "success_rate": 1.0, "evidence": 0.5,
                      "confidence": 0.8, "staleness_penalty": 0.1, "failure": 0.0 },
      "signals": { "success_count": 4, "failure_count": 0, "reuse_count": 9,
                   "last_used_at": "...", "last_validated": "...",
                   "evidence_count": 2, "confidence_trend": "flat" }
    }
  ],
  "candidate_actions": [ /* see below — preview only */ ]
}
```

JSON and summary output, deterministic, content-free where it matters
(procedure names are already non-secret; no raw episode content is dumped).

## Maintenance actions (preview only)

Each is a **candidate** with evidence and a reason; nothing applies without an
explicit second step. Apply reuses existing store methods where possible.

| Action | Trigger (candidate when…) | Apply path |
| --- | --- | --- |
| **retire** | `staleness` over threshold AND `reuse_count` low | existing `retire_procedure` |
| **quarantine** | `failure_count` ≥ threshold (counter-evidence) | existing `record_procedure_counter_evidence` semantics |
| **promote** | quarantined/candidate procedure now has strong success_rate + evidence | new: candidate→active (reviewed) |
| **merge** | two procedures in the same subject with near-identical `activation_query`+`steps` (deterministic similarity, e.g. token Jaccard ≥ threshold) | new: keep higher-score one, `supersede_procedure(loser → winner)`, union `negative_examples` + `source_episode_ids` |
| **split** | one procedure whose `source_episode_ids` cluster into two distinct activation intents (the activation matcher fires on two disjoint query families) | new: propose two candidates, original retired on apply |

Merge and split are the genuinely new logic; both are **proposals** in the
report. Merge leans on `supersede_procedure` so history is preserved (the loser
becomes `superseded`, links to the winner). No silent rewrites.

### Recommended storage choice (for root review)

- Add `success_count INTEGER DEFAULT 0`, `reuse_count INTEGER DEFAULT 0`,
  `last_used_at TEXT` to `procedures` via `ensure_procedure_maintenance_columns`
  (idempotent, mirrors 0008).
- Add an append-only `procedure_score_events(procedure_id, event_kind,
  occurred_at, delta, evidence_ref)` table **only if** `confidence_trend` proves
  worth the extra surface in fixtures. Otherwise derive trend from the counters
  and skip the table (smaller is better).

Recommendation: ship counters first; defer the events table until a fixture
shows trend needs per-event history.

## Regression hooks (required by the issue)

The design must not weaken the safety properties proven in #145/#148:

- **False-activation hook**: maintenance must never promote a procedure whose
  `negative_examples` would be dropped. Merge **unions** negative examples; a
  fixture asserts a merged procedure still abstains on both parents' negatives.
- **Stale-procedure hook**: a retired/superseded procedure must stay out of
  default recall after maintenance (reuses the #146/#148 assertions). A fixture
  asserts maintenance never resurrects a retired procedure into active recall.
- **Unsafe-promotion hook**: promote must re-run the `unsafe_content` guard;
  a procedure that would fail procedure-apply validation cannot be promoted.

These hooks are listed here so the implementation PR wires them as tests, and
so the `proc_eval` suite (#150) gains maintenance metrics (e.g.
`merge_precision`, `false_resurrection_rate = 0`).

## Fixture plan

`tests/fixtures/procedure_maintenance/` (data only when implemented):

| File | Proves |
| --- | --- |
| `score_ranking.json` | high-reuse/high-success procedure scores above a stale/failed one |
| `merge_candidate.json` | two near-identical procedures yield one merge candidate; merged keeps both negative examples |
| `retire_stale.json` | a stale, low-reuse procedure becomes a retire candidate; never auto-applied |
| `promote_recovered.json` | a quarantined procedure with new success evidence becomes a promote candidate, re-running the unsafe guard |
| `no_false_resurrection.json` | maintenance never moves a retired procedure back into default recall |

## Migration strategy (if approved)

Mirror 0008 exactly: `migrations/00NN_procedure_maintenance.sql` marker +
`ensure_procedure_maintenance_columns` (adds `success_count`, `reuse_count`,
`last_used_at`); optional `procedure_score_events` as a real `CREATE TABLE IF
NOT EXISTS`. Bump `STORAGE_SCHEMA_VERSION`, reference the constant in tests,
extend `tests/schema_upgrade.rs`. Additive under `docs/compatibility-policy.md`.

## Open questions for root review

1. **Counters on the row vs. events table** for `confidence_trend`.
   Recommendation: counters first, events table only if a fixture needs it.
2. **Who bumps `reuse_count`/`last_used_at`?** This touches the recall path
   (procedures_recall activation). That is the one place maintenance would need
   a tiny retrieval-code change — explicitly flagged because the current task
   says "do not touch retrieval unless needed." Proposed: a single increment on
   activation, behind the maintenance feature, deferred to the impl PR.
3. **Merge/split similarity threshold** — deterministic token similarity reuses
   the `activation` module's tokenizer. Confirm threshold belongs in config.
4. **Sequencing with #155/#154** — independent; can land after the temporal/
   semantic design is reviewed, or in parallel since it only touches procedures.
