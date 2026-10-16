# Native OMP MemoryD adapter

This package contains the MemoryD-side implementation of an Oh My Pi native
`MemoryBackend`: bounded loopback transport, `beforeAgentStartPrompt` recall,
staged commit protection, `/memory` status/search/save, and bounded
pre-compaction recall. Recall is contextual evidence and is always rendered as
`recall_not_authority`.

## Native registration: stock and Review-derived OMP

Stock OMP remains `NEEDS_OMP_SEAM`. The audited upstream `main` is
`ba801aa6d2bdc491f78faf5ccead8fa94e5c109b`. Its
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
Stock OMP's closed resolver cannot select this external backend.

[Review #295](https://github.com/joshyorko/review/pull/295) supplies a generic
registration patch for a Review-derived OMP build. The current source contract
is [Review `6635a150`](https://github.com/joshyorko/review/tree/6635a1501fc236a1ce3c7a15e02a8af04c365d3f),
whose `image/appliance/Containerfile:33` pins OMP `18.4.12`, upstream source
`7318a70cf4ed04133366884d2723f72d9d490a15`, and Bun `1.4.2`.
`patches/omp/memory-backend-registration.patch` adds native registration,
trusted backend settings, factory/start routing, and optional instance
disposal on rebind and session teardown. Consume that derived contract without
adding a shadow prompt hook. This source audit is not a claim that the current
packaged appliance or stock upstream supports the adapter.

## Configuration

The Review-derived seam reads user-owned `memory.backendSettings` and passes
the selected backend's dotted setting IDs into the factory. Its resolver
rejects project-owned settings as authority. Use the configuration shape in
Review's `scripts/derived-omp-canary.ts`:

```yaml
memory:
  backend: codex-memoryd
  backendSettings:
    codex-memoryd:
      codexMemoryd.baseUrl: http://127.0.0.1:8787
      codexMemoryd.profile: personal
      codexMemoryd.workspace: josh-personal
      codexMemoryd.autoRecall: true
      codexMemoryd.autoObserve: false
      codexMemoryd.recallTimeoutMs: 500
      codexMemoryd.maxTokens: 1200
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
strong save. `/memory clear` resets transient recall state, cancels outstanding
reads, and invalidates staged preparations while preserving initialized scope
and automatic recall settings. OMP calls clear then refreshes its base prompt,
without calling start again, in
`packages/coding-agent/src/slash-commands/builtin-lifecycle.ts`'s clear handler.
It never deletes server MemoryD. `/memory enqueue` truthfully reports that no
local queue exists.

`dispose()` is terminal and idempotent. It cancels outstanding reads and
invalidates all staged prompt/compaction work. Rebinding requires a fresh
factory instance; calling start on a disposed instance cannot reactivate it.

No local database, transcript cache, hidden reasoning, tool scratchpad, or fake
`memory://` resource is introduced.

## Install with a verified native seam

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
explicit-save-only write policy, and registration metadata. Reset/disposal
regressions use a real HTTP client against a synthetic loopback server.
These tests do not prove a current Review-derived executable or appliance.
Review's `docs/issue-293-needs-omp-seam.md` records an earlier derived OMP
18.4.3 first-turn recall and daemon-down canary against synthetic fixtures.
Exact-release native status/search/save, cancellation, rebind, resume, and
compaction acceptance remain separate integration checks. Stock OMP native
smoke remains `SKIPPED (NEEDS_OMP_SEAM)`. Automatic writeback remains gated on
MemoryD #233, and release packaging remains owned by #245.
