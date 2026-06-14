# Dogfood Write Sandbox

This runbook is the supported lane for exercising write-capable dogfood tools
without writing to the real dogfood database.

## Safety Contract

- Real dogfood DB: `.dogfood/memory.db`.
- Write sandbox DB: `.dogfood/write-sandbox-memory.db`.
- Refresh uses `codex-memoryd backup create`, not an ad hoc file copy.
- The real DB is read for schema preflight and backup source only.
- CLI and MCP write canaries always target the sandbox DB.
- Diff reports serialize counts, hashes, ids, and reason-code summaries only.
- No stored memory content is written to reports or PR artifacts.
- Promotion is previewed only. Nothing automatically writes back to the real DB.

## One Command

```bash
scripts/dogfood-write-sandbox.sh run
```

The command performs:

1. read-only schema preflight against the real DB;
2. `backup create` refresh into the sandbox DB;
3. `backup verify` on the sandbox copy;
4. CLI write canary against the sandbox;
5. MCP stdio canary with `--write-tools` against the sandbox;
6. content-free diff report;
7. manual promotion preview.

Generated files live under `.dogfood/write-sandbox/<timestamp>/` by default:

- `preflight-real.json`
- `backup-create.json`
- `backup-verify.json`
- `preflight-sandbox.json`
- `real-fingerprint.before.json`
- `real-fingerprint.after.json`
- `sandbox-fingerprint.json`
- `sandbox-diff-report.json`
- `manual-promotion-preview.md`

The reports are safe to inspect and share because they do not include stored
memory content. They still describe local paths and record ids, so do not commit
them unless a specific release task asks for an artifact.

## Explicit Paths

Use explicit paths when testing a non-default dogfood store:

```bash
scripts/dogfood-write-sandbox.sh run \
  --real-db .dogfood/memory.db \
  --sandbox-db .dogfood/write-sandbox-memory.db \
  --artifact-dir .dogfood/write-sandbox/latest \
  --profile personal \
  --workspace josh-personal
```

Dry-run prints the safety contract without touching any database:

```bash
scripts/dogfood-write-sandbox.sh --dry-run
```

## Manual Promotion

Promotion is intentionally not automated. After a sandbox session:

1. inspect `sandbox-diff-report.json`;
2. inspect the sandbox DB locally with recall, search, card, procedure, or patch
   preview commands;
3. decide which records, procedures, cards, or memory patches are accepted;
4. re-enter accepted changes through the normal real-DB operator path;
5. convert rejected sandbox writes into policy or eval fixtures using
   content-free reason codes.

Secret-shaped fixture values must use the fragmented `content_parts` convention
from `tests/fixtures/policy/README.md`; never commit contiguous token-shaped
strings.
