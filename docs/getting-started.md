# Getting started

This guide connects Codex to the read-only `codex-memoryd` MCP server. The
recommended release-shaped path uses the native binary:

```zsh
# Future release command: the formula and release artifacts are not published yet.
brew install joshyorko/tools/codex-memoryd
codex-memoryd mcp codex apply
codex-memoryd mcp codex status
```

Restart Codex after applying the configuration, then call `memory_status` to
verify that the server starts and responds. `preview` and `status` inspect the
owned Codex MCP block; `apply` updates only that block and backs up the existing
Codex configuration before changing it.

## Opt in to the container launcher

Use the container runtime when Docker or Podman should launch the MCP image:

```zsh
codex-memoryd mcp codex preview --runtime container
codex-memoryd mcp codex apply --runtime container
codex-memoryd mcp codex status --runtime container
```

The CLI performs setup and does not pull an image or start a container during
`apply`. When Codex later starts the MCP server, the generated stdio command
launches the configured Docker or Podman image on demand. The default image is
`ghcr.io/joshyorko/codex-memoryd:latest`; `CODEX_MEMORYD_IMAGE` can override it.
The container mounts the parent directory of the persistent SQLite database at
`/data`, using the corresponding database filename inside the container. This
directory mount preserves SQLite sidecar files such as WAL and shared-memory
files. The database remains persistent host data; its exact path is determined
by the resolved configuration.
The selected database parent must already exist; run codex-memoryd init or
create the configured directory before previewing the container launcher.

Restart Codex after applying the container configuration and call
`memory_status` again. Docker or Podman must be installed and available to the
CLI and to Codex when the MCP server starts.

## Switch runtimes or remove the integration

Switch back to the native binary with:

```zsh
codex-memoryd mcp codex preview --runtime native
codex-memoryd mcp codex apply --runtime native
codex-memoryd mcp codex status --runtime native
```

Restart Codex and verify with `memory_status`. To remove only the owned
`codex-memoryd` MCP block:

```zsh
codex-memoryd mcp codex remove
codex-memoryd mcp codex status
```

Removing the MCP block, uninstalling the Homebrew formula, or removing the
container image does not delete the persistent memory database. Delete that
database separately only if you intentionally want to discard stored memory.
To uninstall the future-release Homebrew installation after removing the MCP
block, run `brew uninstall codex-memoryd`.

## Source-build fallback

If the future-release Homebrew command is unavailable, build the binary from a
checkout and use `target/release/codex-memoryd` in the commands above:

```zsh
cargo build --release
target/release/codex-memoryd mcp codex apply
target/release/codex-memoryd mcp codex status
```

Homebrew, release, image, tag, and tap artifacts are not published in this
feature slice. The Homebrew command above becomes usable only after a later
release and tap publication completes.
