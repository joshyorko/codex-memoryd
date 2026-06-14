# Native Codex Memory Migration Plan

This plan migrates existing native Codex memory files into `codex-memoryd`
without changing upstream Codex. Native files remain a fallback until parity,
canaries, and operator confidence are good enough to make `codex-memoryd` the
canonical local memory service.

The migration is intentionally manual. There is no hidden memory injection, no
automatic disabling of native Codex writes, and no tap-release provider
requirement for this first cut.

## Goals

- Keep current Codex memory files usable as a fallback.
- Import `~/.codex/memories` through `sync-local --preview` before `--apply`.
- Prove recall parity with a small operator-run canary set.
- Avoid duplicate write loops between native files, imports, MCP recall, and
  future provider paths.
- Define the gate for `memoryd-canonical` mode before native writes are
  disabled.

## Non-Goals

- No upstream Codex changes are required.
- No automatic disabling of native memory writes.
- No hidden prompt-context injection.
- No automatic merge of personal, work, OSS, or homelab profiles.
- No tap-release provider path requirement. Provider integration can remain
  optional while local import and read-only MCP dogfood harden.

## Migration Phases

### Phase 1: Dual-Read / Memoryd-Import

Use this phase for ordinary dogfood. Native Codex memory files remain the
human-readable fallback, while `codex-memoryd` imports them into its local
SQLite store for recall, search, cards, exports, and MCP read-only dogfood.

Operator contract:

1. Run the local daemon in loopback-only mode.
2. Run `sync-local --preview ~/.codex/memories`.
3. Review rejected, skipped, created, and warning counts.
4. Run `sync-local --apply ~/.codex/memories` only after the preview is
   expected.
5. Run `sync-local --apply ~/.codex/memories` a second time and expect the same
   content to be skipped, not duplicated.
6. Run recall canaries against native-memory topics and compare whether
   `codex-memoryd` returns useful, cited, `recall_not_authority` context.

Codex may still read native files directly. `codex-memoryd` recall is advisory
and must not override current repo files, issue bodies, user instructions, or
policy.

### Phase 2: Native Files Fallback

Use this phase after repeated import and recall canaries are clean. Native files
remain available for emergency recovery and manual inspection, but normal
operator workflows prefer `codex-memoryd` for recall, search, cards, and exports.

Required behavior:

- `sync-local --preview` remains the first step before every import.
- Re-import is idempotent and policy-gated.
- Deleted or changed source files are handled by the existing sync cursor and
  stale-path archiving behavior, not by ad hoc deletion.
- Read-only MCP dogfood continues to expose only `memory_status`,
  `memory_recall`, and `memory_search`.
- Native memory files are not imported back from `codex-memoryd` exports unless
  an operator explicitly performs a one-way recovery.

This is still not canonical mode. Native writes are allowed, and the import path
continues to treat native files as an upstream source.

### Phase 3: Memoryd-Canonical

Use this phase only after the prerequisite checklist below is complete.
`codex-memoryd` becomes the canonical local memory service. Native Codex files
become fallback/recovery artifacts instead of an ordinary write target.

Canonical-mode constraints:

- Disable native memory writes only through an explicit Codex-side setting or
  documented operator action.
- Keep provider failure fail-open. A down daemon must not block normal Codex
  turns.
- Keep recall `recall_not_authority`; canonical storage does not make recall
  authoritative.
- Keep imports preview/apply/idempotent for recovery and historical migration.
- Keep tap-release provider integration optional until it proves parity and
  fail-open behavior in normal dogfood.

## Safe Import and Re-Import Flow

Run this from the `codex-memoryd` checkout on the Bluefin host or inside a
project container with the repo build available. The database path should point
at the intended dogfood store.

```bash
export CODEX_MEMORYD_DB="$PWD/.dogfood/memory.db"
export DOGFOOD_PROFILE=personal
export DOGFOOD_WORKSPACE=josh-personal

target/debug/codex-memoryd sync-local --preview \
  --profile "$DOGFOOD_PROFILE" \
  --workspace "$DOGFOOD_WORKSPACE" \
  ~/.codex/memories > .dogfood/sync-preview.json

target/debug/codex-memoryd sync-local --apply \
  --profile "$DOGFOOD_PROFILE" \
  --workspace "$DOGFOOD_WORKSPACE" \
  ~/.codex/memories > .dogfood/sync-apply.json

target/debug/codex-memoryd sync-local --apply \
  --profile "$DOGFOOD_PROFILE" \
  --workspace "$DOGFOOD_WORKSPACE" \
  ~/.codex/memories > .dogfood/sync-apply-second.json
```

Expected result:

- Preview writes no durable rows.
- First apply imports only policy-accepted records.
- Second apply skips unchanged records and does not create duplicates.
- Secret-like, prompt-injection-like, empty, unsupported, or invalid sources are
  rejected or skipped without storing raw unsafe content.
- Evidence ledger rows are append-only and idempotent for apply-mode imports.

## Parity Canaries

Run the canaries after import and after daemon restart. They are intentionally
small and inspectable; the operator compares usefulness and safety, not byte-for
byte output.

```bash
for query in \
  "What is codex-memoryd's north star?" \
  "What must never become durable memory?" \
  "What is the safe dogfood mode?" \
  "How should native Codex memory migrate to codex-memoryd?" \
  "What repo boundary should apply between codex-memoryd and codex?"
do
  target/debug/codex-memoryd recall \
    --profile "$DOGFOOD_PROFILE" \
    --workspace "$DOGFOOD_WORKSPACE" \
    --query "$query" \
    --max-tokens 1200 | jq '{summary, authority, facts: .facts[0:3], checkpoints: .checkpoints[0:1]}'
done
```

Passing canary criteria:

- `authority` is `recall_not_authority`.
- Results are relevant to the query, profile, and workspace.
- Results include provenance/citation fields where records are returned.
- Results do not expose secrets, private keys, auth files, hidden reasoning, raw
  confidential logs, or `.env` dumps.
- Results do not instruct Codex to ignore current user, repo, system, or
  developer instructions.
- Restarting the daemon preserves the useful recall set.

## Duplicate-Loop Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| Native file imported, exported, then re-imported as a new native file | Treat adapter exports as downstream renderings, not import sources. Do not point `sync-local` at generated export directories. |
| Native Codex writes and `codex-memoryd` conclusions record the same fact | Accept duplicates during Phase 1 only when provenance differs; rely on content hash dedupe, recall ranking, and later Dreamer consolidation rather than unsafe automatic deletion. |
| Read-only MCP recall is copied into a new durable memory | Recall is always `recall_not_authority`; imported or active memory cannot self-reinforce without fresh primary evidence. |
| Tap-release provider and `sync-local` both write from the same turn | Keep tap-release provider optional and disabled during import parity runs. Do not enable provider writeback until canaries show no duplicate loops. |
| Cross-profile imports leak work into personal or personal into work | Run imports with explicit `--profile` and `--workspace`; enforce existing profile/workspace boundary policy. |
| Re-import of changed files leaves stale content active | Use the sync cursor and stale-path archiving behavior by re-running preview/apply over the same source root. |

## When to Disable Native Memory Writes

Do not disable native Codex writes until all of this is true:

- `sync-local --preview` and two consecutive `--apply` runs are clean and
  idempotent for the real memory source root.
- Parity canaries pass before and after daemon restart.
- Read-only MCP dogfood exposes only read tools and rejects write tools.
- Provider or MCP failures fail open and do not block Codex turns.
- Operators have a documented recovery path from native files and from
  `codex-memoryd export`.
- Duplicate-loop risks above have been checked in the current dogfood setup.
- Current `codex/` integration choice is explicit: no upstream changes, optional
  tap-release provider, or a reviewed Codex-side setting.

## Acceptance Checklist for Issue #84

- [x] Migration phases are documented:
  dual-read / memoryd-import, native files fallback, and memoryd-canonical.
- [x] Parity canary list exists.
- [x] Duplicate-loop risks and mitigations are documented.
- [x] Safe import/re-import flow keeps `sync-local` preview/apply and
  idempotency front and center.
- [x] Memoryd-canonical mode has a prerequisite checklist.
- [x] Dogfood runbooks reference this migration plan.
- [x] Existing tests remain the verification gate for import idempotency and
  CLI/doc hooks.

## Verification Hooks

Use these as the PR gate for doc-only changes:

```bash
cargo test --test cli_smoke readme_keeps_first_run_path_documented
cargo test --test cli_smoke cli_help_lists_all_commands
cargo test --test evidence_ledger sync_apply_is_idempotent_for_ledger_rows
```

Use these as the operational gate before changing a real dogfood posture:

```bash
scripts/dogfood-compose-heartbeat.sh
target/debug/codex-memoryd sync-local --preview ~/.codex/memories \
  --profile personal --workspace josh-personal
target/debug/codex-memoryd sync-local --apply ~/.codex/memories \
  --profile personal --workspace josh-personal
target/debug/codex-memoryd sync-local --apply ~/.codex/memories \
  --profile personal --workspace josh-personal
```
