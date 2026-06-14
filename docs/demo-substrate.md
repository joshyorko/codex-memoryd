# Fixture Substrate Demo

`scripts/demo-substrate.sh` is the one-command fixture-only walkthrough for
`codex-memoryd`.

It creates a temporary SQLite database and temporary loopback daemon, then runs
the current substrate path end to end:

1. start a temp fixture DB and daemon
2. `sync-local` preview/apply from synthetic fixture memories
3. create a subject and successful episodes
4. recall with `recall_not_authority` policy metadata
5. render a workspace card
6. export an `mcp-pack` adapter view
7. import Git refs fixture evidence
8. preview/apply/recall a procedure
9. run `eval substrate`
10. run a read-only MCP stdio canary and verify `memory_conclude` is rejected

Run:

```bash
scripts/demo-substrate.sh
```

Show the plan without side effects:

```bash
scripts/demo-substrate.sh --dry-run
```

Keep temporary artifacts for inspection:

```bash
CODEX_MEMORYD_DEMO_KEEP=1 scripts/demo-substrate.sh
```

## Safety

- Does not read `~/.codex/memories`.
- Does not read or write `.dogfood/memory.db`.
- Does not require real dogfood MCP.
- Uses only synthetic fixture content.
- Uses read-only MCP for the canary.
- Keeps recall as context: `recall_not_authority`, not source of truth.

Fixtures must not contain contiguous token-shaped values. If a future demo needs
to include a secret-looking string, store it as safe fragments and join it only
inside a runtime test process, following the policy corpus convention.
