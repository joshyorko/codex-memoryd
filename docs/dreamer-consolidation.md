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
