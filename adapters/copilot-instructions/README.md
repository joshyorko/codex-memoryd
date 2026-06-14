# GitHub Copilot instructions adapter package

This package exports a markdown memory view for Copilot instructions. It does
not enable automatic prompt injection or writeback.

## Install

```sh
mkdir -p .github
"$CODEX_MEMORYD_BIN" --db "$CODEX_MEMORYD_DB" adapter export \
  --target copilot \
  --profile "$CODEX_MEMORYD_PROFILE" \
  --workspace "$CODEX_MEMORYD_WORKSPACE" \
  --format markdown > .github/copilot-instructions.md
```

Use [`templates/copilot-instructions.md`](./templates/copilot-instructions.md)
as the static wrapper if you want to commit a stable explanation around the
generated export.

## Verify

```sh
test -s .github/copilot-instructions.md
rg 'recall_not_authority|Copilot Instructions Memory View' .github/copilot-instructions.md
```

Expected result: the generated file states that recall is not authority.

## Uninstall

Remove `.github/copilot-instructions.md` or replace it with your previous
instructions file. No memory data is deleted.
