# Container MCP Launcher and Homebrew Onboarding

## Context

`codex-memoryd mcp codex preview|apply|status|remove` manages the owned Codex
MCP block. The global `--runtime native|container` option already parses for
these commands, but MCP config rendering always emits the native binary path.
Josh's current Codex config therefore points at a native binary that no longer
exists.

The repository also lacks a short release-user guide. The README contains
source-build, dogfood, daemon, and operator detail, but there is no minimal
Homebrew install-to-first-recall path. There are currently no repository tags,
GitHub releases, release workflow, or `homebrew-tools` formula for this binary,
so Homebrew instructions must be clearly described as the intended release
contract until those artifacts land.

## Decision

Make the existing global runtime selector authoritative for Codex MCP config
generation:

- `--runtime native` preserves the current direct-binary stdio entry.
- `--runtime container` emits a Docker or Podman stdio entry that starts the
  published image on demand and exits when Codex closes stdin.
- Omitting `--runtime` preserves the resolved/default native behavior.

Do not add another runtime option under `mcp codex`. Because the root option is
global, both placements remain valid:

```zsh
codex-memoryd --runtime container mcp codex apply
codex-memoryd mcp codex apply --runtime container
```

The documentation will use the second form because it reads naturally at the
point where the choice matters.

## Generated Container Contract

The container MCP block uses the runtime already resolved by
`codex-memoryd`:

- command: resolved `docker` or `podman` executable
- transport: interactive stdio (`run -i`)
- lifecycle: ephemeral container (`--rm`)
- image acquisition: pull when missing (`--pull=missing`)
- image: resolved `CODEX_MEMORYD_IMAGE`, defaulting to
  `ghcr.io/joshyorko/codex-memoryd:latest`
- identity: resolved host UID/GID so SQLite files remain host-owned
- storage: mount only the database's parent directory read-write at `/data`
- database argument: the corresponding `/data/<filename>` path
- MCP tier: explicit `mcp stdio --read-only`

The existing Codex allowlist, approval mode, startup timeout, and tool timeout
remain unchanged. The database directory mount, rather than a single-file
mount, is required because SQLite may create WAL and shared-memory sidecars.

`preview` and `status` are read-only. `apply` mutates only the owned MCP table,
backs up an existing Codex config before changing it, and remains idempotent.
It does not pull the image or start a container; Codex does that when it starts
the MCP server.

## First-Time User Guide

Add a short `docs/getting-started.md` and link it near the top of the README.
The release-shaped native path is:

```zsh
brew install joshyorko/tools/codex-memoryd
codex-memoryd mcp codex apply
codex-memoryd mcp codex status
```

The guide then tells the user to restart Codex and call `memory_status`.
Homebrew-native is the recommendation because the installed artifact is
already available and requires no container runtime.

The opt-in container path is:

```zsh
codex-memoryd mcp codex preview --runtime container
codex-memoryd mcp codex apply --runtime container
codex-memoryd mcp codex status --runtime container
```

It explains that the Homebrew CLI performs setup while Codex subsequently
launches the MCP image on demand. The guide includes switching back to native,
uninstalling the MCP block, the persistent database location, and a concise
note that removing the MCP block or formula does not delete memory data.

The tap command is marked as a future release command until the formula and
release artifacts exist. Source-build instructions remain available from the
README but are not part of the primary user journey.

## Errors

Container rendering fails before config mutation when:

- neither Docker nor Podman can be resolved;
- the database has no usable file name or parent directory;
- UID/GID or mount values cannot be represented safely.

Errors name the failed requirement and give the next command or configuration
override. Runtime image-pull, permission, and container-start failures remain
visible as MCP startup failures from Docker or Podman; the generated arguments
must not suppress their stderr.

## Tests

Focused CLI tests prove:

- native output remains byte-for-byte compatible;
- container preview contains the resolved engine, image, identity, mount, and
  in-container database path;
- Docker and Podman selections render deterministically;
- spaces and TOML-sensitive characters in host paths are escaped correctly;
- preview and status do not write;
- apply backs up and replaces a drifted block;
- repeated apply is idempotent;
- remove deletes only the owned block;
- missing container runtime and invalid database paths fail before mutation;
- CLI help continues to expose one global `--runtime` option.

Documentation checks assert that the short guide contains native install,
container opt-in, verification, switching, and uninstall/data-retention paths.

## Release Boundary

This slice makes the CLI and documentation release-shaped but does not publish
an artifact, create a GitHub release, modify `homebrew-tools`, or install the
formula. A later packaging slice must produce checksummed platform artifacts,
add the formula to the tap, test installation from the tap, and replace the
guide's future-release label only after those checks pass.
