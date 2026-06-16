# Semantic Layer #154 Completion Design

## Context

PR #177 landed the bootstrap/runtime UX slice. The next feature tranche starts with issue #154 before #155 temporal work.

Issue #154 asks for a local semantic layer decision and MVP for entity aliases and lightweight relations. The current code already has the relation substrate: `subject_aliases` and `relations` tables, store-level insert/read APIs, scope checks, evidence requirements, semantic fixtures, and the relation-aware retrieval eval slice. The current gap is the user-facing explicit preview/apply path documented as pending in `docs/semantic-layer.md`.

## Decision

Close #154 with a small explicit semantic import surface:

- `codex-memoryd semantic preview --file <semantic.json>`
- `codex-memoryd semantic apply --file <semantic.json>`

The JSON file contains reviewed alias and relation facts. The CLI does not infer facts from memory text, does not call a model, and does not mutate state during preview. Apply reuses the existing store validators so evidence, profile/workspace scope, relation vocabulary, and idempotency stay centralized.

Default recall remains conservative. Relations may support deterministic eval and later retrieval expansion, but this PR does not make relation rows authoritative and does not turn relation facts into synthetic recall records.

## Non-Goals

- No graph database.
- No embeddings or hybrid retrieval.
- No model-only extraction.
- No Async Dreamer work.
- No #155 temporal fields or as-of recall.
- No default `/v1/recall` relation expansion in this PR.
- No silent alias merge or relation mutation.

## Input Format

The CLI accepts one JSON object:

```json
{
  "profile_id": "personal",
  "workspace_id": "josh-personal",
  "aliases": [
    {
      "subject_id": "subject_alice",
      "alias_key": "al",
      "source_evidence": "episode:ep_alias"
    }
  ],
  "relations": [
    {
      "from_subject_id": "subject_alice",
      "relation_type": "owns",
      "to_subject_id": "subject_billing",
      "confidence": 0.92,
      "source_episode_ids": ["episode:ep_owns_billing"],
      "source_evidence": "episode:ep_owns_billing"
    }
  ]
}
```

Rules:

- Top-level `profile_id` and `workspace_id` apply to every entry.
- Entry-level profile/workspace are not accepted in v1. One file applies to one scope.
- `aliases` and `relations` are optional arrays, but at least one item must be present.
- `alias_key` must be non-empty and normalized by existing subject-key conventions where available.
- `relation_type` must be one of `uses`, `owns`, `prefers`, `works_on`, `depends_on`, `supersedes`, or `blocked_by`.
- `confidence` defaults to `1.0` when absent.
- `source_evidence` is required for aliases and relations.
- `source_episode_ids` is required and non-empty for relations.

## Preview Behavior

`semantic preview` validates the input and prints a JSON report. It performs no durable writes.

The report groups entries by action:

- `would_apply`: valid entries not already present.
- `already_present`: idempotent duplicates that apply would skip.
- `rejected`: invalid entries with a stable reason string.

Preview validation checks:

- Required fields.
- Known relation type.
- Subject endpoints exist in the requested profile/workspace.
- Alias target exists in the requested profile/workspace.
- Evidence fields are present.
- Relation state would be `active`.
- The file does not mix scopes.

## Apply Behavior

`semantic apply` runs the same validation as preview, then writes only entries in `would_apply`.

Apply returns the same report shape plus counts:

- `applied_aliases`
- `applied_relations`
- `already_present`
- `rejected`

If any entries are rejected, apply still writes valid entries unless the input file is structurally invalid JSON or missing the top-level scope. This mirrors import-style behavior and keeps one bad candidate from blocking a whole reviewed batch. The report must make partial success explicit.

## Safety Invariants

- Relations remain recall-not-authority.
- Preview never writes.
- Apply never creates subjects.
- Apply never crosses profile/workspace boundaries.
- Apply never accepts relations without evidence.
- Apply never accepts relation types outside the closed vocabulary.
- Duplicate apply is idempotent.
- Store-level checks remain the final enforcement point.
- Default recall still returns memory records, not relation claims.

## Code Shape

Add a focused semantic import module rather than growing CLI parsing into a large blob.

Planned files:

- `src/semantic_import.rs`: input structs, validation, preview/apply report builders.
- `src/cli.rs`: `Semantic` subcommand and output plumbing.
- `src/lib.rs`: module export.
- `tests/semantic_import.rs`: service/store-level validation and apply tests.
- `tests/cli_smoke.rs`: CLI preview/apply smoke tests.
- `docs/semantic-layer.md`: update status from pending user-facing UX to explicit JSON preview/apply path.
- `README.md`: add operator command examples if current docs point operators there.

The import module depends on domain types and the store/service boundary already used by CLI commands. It should not know about HTTP or MCP.

## Tests

Required tests:

- Preview with valid aliases/relations returns `would_apply` and leaves store unchanged.
- Apply with valid aliases/relations writes both tables.
- Apply repeated against the same file returns `already_present`.
- Missing alias evidence is rejected.
- Missing relation evidence is rejected.
- Unknown relation type is rejected.
- Relation whose endpoint belongs to another profile/workspace is rejected.
- Preview and apply produce stable JSON reports for CLI callers.
- Existing relation-aware eval still shows `q_multihop_evidence` improvement and no cross-profile leak.

Validation commands for the PR:

```bash
rtk cargo fmt --all --check
rtk cargo check
rtk test cargo test semantic_import
rtk test cargo test semantic_relations
rtk test cargo test cli_smoke
rtk git diff --check
```

Run full `rtk cargo test` if feasible before PR. If it is too slow in the current environment, report that honestly and include targeted test coverage.

## Issue Closure Standard

Close #154 only if the PR provides:

- The existing design comparison and local-first decision.
- Prototype relation import path through explicit preview/apply.
- Multi-hop eval evidence that relation-aware recall helps.
- Evidence refs and profile/workspace boundaries.
- Green targeted tests.
- Docs with exact commands and safety notes.

If any of these are missing after implementation, the PR should say it advances #154 but not close it.
