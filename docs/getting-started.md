# Getting started

`codex-memoryd` v0.1.0 is distributed as native binaries. Homebrew is the
primary install path; native mode is the default and does not require Docker,
Podman, Compose, Rust, or a repository checkout.

## Install with Homebrew

```zsh
brew install joshyorko/tools/codex-memoryd
codex-memoryd --version
codex-memoryd init
codex-memoryd up
codex-memoryd status
codex-memoryd down
```

The persistent database is `~/.codex-memoryd/memory.db`. `up`, `status`, and
`down` manage the loopback-only native daemon; the default bind is
`127.0.0.1:8787`.

### Native process identity and recovery

On Linux, the managed lifecycle requires a kernel with `pidfd_open` and
`pidfd_send_signal`, a readable `/proc`, and permission to use those operations.
The numeric PID file remains available for inspection, and a private
`<pid-file>.identity` file records the launch identity. A per-runtime lock
serializes lifecycle commands. The runtime state directory and its parents
must not be writable by untrusted users. `down` holds a pidfd while checking
identity and sends SIGTERM through that handle; it never signals a bare PID.
Do not copy these files between runtime homes or use them as a security boundary
against another process with the same user privileges.

Legacy PID files contain no trustworthy identity. If their PID is still live,
the managed status reports `unverified`, and `up`, `down`, and `restart` refuse
to act on it. Malformed, inaccessible, or inconsistent state is also treated
as unverified. An exited or demonstrably different process is treated as stale.
An explicit `--url` status remains an HTTP endpoint query.

To recover, first identify and stop any actual older daemon through the process
supervisor or terminal that originally started it. Do not send a signal using
the contents of the PID file or infer ownership from a process name. Once no
daemon is using that runtime and its configured endpoint, remove the PID file
and its adjacent `.identity` file, then run `codex-memoryd up`. The persistent
database must not be removed. If ownership cannot be determined safely, leave
the state in place and use the original supervisor or an operator to resolve it.

On macOS and other non-Linux platforms, this version fails closed for managed
native `up`, `down`, and `restart` because a race-free process handle is not
available through this implementation. Foreground `codex-memoryd serve` can be
managed by the operator's service supervisor; container mode is unaffected.

To connect Codex to the read-only MCP server:

```zsh
codex-memoryd mcp codex apply
codex-memoryd mcp codex status
```

Restart Codex after applying the configuration, then call `memory_status` to
verify that the server starts and responds. `preview` and `status` inspect the
owned Codex MCP block; `apply` updates only that block and backs up the
existing Codex configuration before changing it.

## Supported native platforms

v0.1.0 publishes and verifies these release targets:

| Platform | Target |
| --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu` |
| Linux aarch64 | `aarch64-unknown-linux-gnu` |
| macOS x86_64 | `x86_64-apple-darwin` |
| macOS arm64 | `aarch64-apple-darwin` |

No musl-specific Linux archive is published in v0.1.0.

## Direct GitHub binary download

Choose the archive matching the host target, then verify it against the
release checksum manifest before installing:

```zsh
version=0.1.0
target=x86_64-unknown-linux-gnu
archive="codex-memoryd-v${version}-${target}.tar.gz"
base="https://github.com/joshyorko/codex-memoryd/releases/download/v${version}"
curl -fsSLO "${base}/${archive}"
curl -fsSLO "${base}/SHA256SUMS"
grep "  ${archive}$" SHA256SUMS | sha256sum -c -
tar -xzf "$archive"
mkdir -p "$HOME/.local/bin"
install -m 0755 codex-memoryd "$HOME/.local/bin/codex-memoryd"
```

On macOS, replace the checksum command with:

```zsh
grep "  ${archive}$" SHA256SUMS | shasum -a 256 -c -
```

The release also publishes [`provenance.json`](https://github.com/joshyorko/codex-memoryd/releases/download/v0.1.0/provenance.json), which binds the archive names, targets, sizes, package version, tag, and source commit. Verify the expected tag and version before trusting an archive:

```zsh
curl -fsSLO "${base}/provenance.json"
jq -e '.ref == "refs/tags/v0.1.0" and .package_version == "0.1.0" and .commit == "45b5ef31e0d027bd03260d0731d7d5da09a611ef"' provenance.json
```

## Upgrade and uninstall

```zsh
brew update
brew upgrade codex-memoryd
codex-memoryd --version
brew uninstall codex-memoryd
```

Stopping or uninstalling the formula does not delete
`~/.codex-memoryd/memory.db`. For a direct install, stop the daemon, replace
or remove `~/.local/bin/codex-memoryd`; the database is retained in the same
way. Delete the database separately only when you intentionally want to
discard stored memory.

## Optional managed container

Native mode remains the default. Use the managed container runtime only when
Docker or Podman should launch the daemon:

```zsh
export CODEX_MEMORYD_IMAGE=ghcr.io/joshyorko/codex-memoryd:v0.1.0
codex-memoryd init --runtime container
codex-memoryd up
codex-memoryd status
codex-memoryd down
```

The host publish remains loopback-only. The container mounts the parent
directory of the persistent SQLite database at `/data`, preserving SQLite
sidecar files such as WAL and shared-memory files. Docker or Podman must be
installed and available to the CLI.

When `--db` or `CODEX_MEMORYD_DB` selects a custom database, managed container
startup mounts that database's parent directory and configures the matching
`/data/<filename>` path inside the container. The host root cannot be used as
the database parent; choose a dedicated data directory instead.

To connect Codex to the container-backed read-only MCP server:

```zsh
codex-memoryd mcp codex preview --runtime container
codex-memoryd mcp codex apply --runtime container
codex-memoryd mcp codex status --runtime container
```

The CLI performs setup and does not pull an image or start a container during
`apply`. When Codex later starts the MCP server, the generated stdio command
launches the configured image on demand. The selected database parent must
already exist; run `codex-memoryd init` or create the configured directory
before previewing the container launcher.

Restart Codex after applying the container configuration and call
`memory_status` again.

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
container image does not delete the persistent memory database.

## Source-build fallback

If a release binary is unavailable, build from a checkout and use
`target/release/codex-memoryd` in the commands above:

```zsh
cargo build --release
target/release/codex-memoryd mcp codex apply
target/release/codex-memoryd mcp codex status
```
