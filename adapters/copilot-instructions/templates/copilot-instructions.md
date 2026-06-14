# Copilot Instructions Memory Adapter

This file is generated from `codex-memoryd adapter export --target copilot`.

Safety contract:

- Recall is `recall_not_authority`.
- Repository files, current user instructions, and tests override memory.
- The adapter is read-only; it does not write memories.
- The source of truth remains the configured `codex-memoryd` store.

Regenerate:

```sh
codex-memoryd --db "$CODEX_MEMORYD_DB" adapter export \
  --target copilot \
  --profile "$CODEX_MEMORYD_PROFILE" \
  --workspace "$CODEX_MEMORYD_WORKSPACE" \
  --format markdown > .github/copilot-instructions.md
```
