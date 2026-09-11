# Governed Dreamer consolidation

The versioned `consolidation.v1` contract defines an explicit `off`, `preview`,
or `automatic` policy for declared scopes. Preview remains non-adopting. An
automatic policy is an operator decision; candidate payloads and model output
cannot enable it, widen its scope, or select credentials.

Consolidation proposals carry immutable output identity, attributed subject and
source references. Decisions are closed: adopt statement, adopt inference,
reinforce, supersede, no change, defer, or reject. Application, correction, and
undo are server-owned transactional operations; recall remains
`recall_not_authority`. Synthetic acceptance fixtures are listed in
`tests/fixtures/consolidation/manifest.json`.

The existing `CODEX_MEMORYD_DREAM_AUTOMATIC_APPLY` setting is off by default.
When an operator enables it, deterministic scheduled candidates pass the
governed policy boundary and persist an immutable batch before applying. The
batch-only `/v1/consolidation/apply` and `/v1/consolidation/undo` controls do
not accept policy, mode, credential, or scope overrides. Undo only reverses
untouched records from that batch; later edits are preserved.
For a batch-created replacement that supersedes a current record, undo restores
the recorded prior state only when the replacement and supersession link remain
untouched. If a replacement existed before the batch or a later edit changed
the link, the old record stays historical.

When a scheduled candidate supersedes a current record, the persisted decision
retains the superseded record IDs. Automatic apply validates those targets in
the same SQLite transaction, links the replacement, archives the old records as
superseded, and returns unique applied record IDs. A changed or missing target
rejects the transaction as stale.

The existing `/v1/dream` preview path remains non-adopting. Model-backed
semantic validation and FRIDAY consumer acceptance are separate gates; this
documentation does not claim live activation or production-corpus migration.
