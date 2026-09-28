# Native OMP MemoryD adapter

This package contains the MemoryD-side implementation of an Oh My Pi native
`MemoryBackend`: bounded loopback transport, `beforeAgentStartPrompt` recall,
staged commit protection, `/memory` status/search/save, and bounded
pre-compaction recall. Recall is contextual evidence and is always rendered as
`recall_not_authority`.

## Capability gate: NEEDS_OMP_SEAM

The exact OMP `main` audited for this package is `can1357/oh-my-pi`
`cabfa74e1f6b4e8f53d1afb71b652c024db339fd` (`v18.4.2-18-gcabfa74e1f`). Its
`MemoryBackendId` in `packages/coding-agent/src/memory-backend/types.ts`,
the `memory.backend` enum in
`packages/coding-agent/src/memory-backend/settings.ts`, and the hard-coded
`resolveMemoryBackend()` branches in
`packages/coding-agent/src/memory-backend/resolve.ts` remain closed; upstream
issues [#2148](https://github.com/can1357/oh-my-pi/issues/2148) and
[#7902](https://github.com/can1357/oh-my-pi/issues/7902) are still open. The
extension `ExtensionAPI` has no `registerMemoryBackend` method. The adapter
therefore does **not** add extension lifecycle hooks, shadow memory tools, or
an OMP fork. It exports the smallest registration packet required by the
intended public seam:

```ts
registerMemoryBackend({
  id, label, description,
  settings,
  capabilities,
  create({ session, settings, agentDir, cwd, taskDepth, parent }),
});
```

`registerMemoryD(api)` calls only `api.registerMemoryBackend(registration)`.
When OMP exposes that API, the package can be installed without changing the
MemoryD client or creating a competing prompt hook. Until then, the native
OMP acceptance gate is `NEEDS_OMP_SEAM`; this repository cannot make OMP's
closed resolver select an external backend.

## Configuration

The registration declares these trusted settings:

```yaml
memory:
  backend: codex-memoryd
codexMemoryd:
  baseUrl: http://127.0.0.1:8787
  profile: personal
  workspace: josh-personal
  autoRecall: true
  recallTimeoutMs: 500
  maxTokens: 1200
```

`profile` and `workspace` are required and never come from prompt, repository,
or recalled content. Only loopback HTTP(S) origins without credentials,
redirects, paths, or query strings are accepted. The default aggregate recall
deadline is 500 ms and the response body cap is 4 MiB.

Missing scope produces an inactive backend with an actionable diagnostic.
MemoryD outage, timeout, malformed envelopes, oversized bodies, scope denial,
and cancellation fail open for the coding turn. Abort signals reach both the
fetch and streamed response-body read. A late result cannot commit after the
session generation changes; cancellation does not consume staged state.

## Write policy

The adapter has no automatic assistant-turn writeback. `autoObserve: true` is
rejected until MemoryD #233's governed host-observation receipt protocol is
available. Ordinary `/v1/turns` is never called by this package. The explicit
`ctx.memory.save()` path uses `/v1/conclusions` only for an operator-requested
strong save. `/memory clear` clears adapter state and never deletes server
MemoryD; `/memory enqueue` truthfully reports that no local queue exists.

No local database, transcript cache, hidden reasoning, tool scratchpad, or fake
`memory://` resource is introduced.

## Install after the seam lands

Install this package as an OMP extension and call `registerMemoryD(pi)` from the
extension entrypoint. Do not register a `recall` tool or a `before_agent_start`
extension handler as a substitute for the native registration API.

## Verify

From this directory, use the exact OMP/Bun toolchain for the selected release:

```sh
bun test tests
```

The tests cover loopback validation, bounded recall formatting, authority and
freshness metadata, cancellation/stale commit handling, fail-open transport,
explicit-save-only write policy, and registration metadata. Automatic OMP
end-to-end smoke remains `SKIPPED (NEEDS_OMP_SEAM)` until the upstream public
registration API lands.
