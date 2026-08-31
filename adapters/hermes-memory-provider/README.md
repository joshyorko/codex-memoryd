# Hermes native provider adapter

This adapter lets a Hermes profile use a local `codex-memoryd` daemon through
Hermes' `MemoryProvider` interface. It does not add Hermes core code and does
not make MCP the integration path.

Install into an active profile (Hermes user providers live directly under
`$HERMES_HOME/plugins/`, unlike bundled providers):

```sh
PROFILE_HOME="${HERMES_HOME:?set HERMES_HOME to the active profile home}"
mkdir -p "$PROFILE_HOME/plugins"
cp -R plugins/memory/codex_memoryd "$PROFILE_HOME/plugins/"
```

For FRIDAY, `HERMES_HOME` is normally `~/.hermes/profiles/friday`.

Configure the active profile with Hermes' config command (do not put these
settings in `.env`):

```sh
hermes -p friday config set memory.provider codex_memoryd
hermes -p friday config set plugins.codex_memoryd.endpoint http://127.0.0.1:8787
hermes -p friday config set plugins.codex_memoryd.profile personal
hermes -p friday config set plugins.codex_memoryd.bootstrap_origin true
```

Start MemoryD separately using its native runtime. FRIDAY remains host-native;
no FRIDAY container is required.

The provider uses four MemoryD workspaces by default:

- `josh-personal`: Josh history and preferences
- `friday-self`: FRIDAY post-origin memory and the imported built-in origin
- `josh-friday`: explicit shared decisions and relationship history
- `friday-evidence`: scoped world/work evidence and visible-turn writeback

Recall performs bounded calls for each lane and labels every injected block as
`recall_not_authority`. Built-in `MEMORY.md` writes are mirrored with actor and
source metadata. On first initialization, the existing profile
`memories/MEMORY.md` is sent byte-for-byte as an `identity` conclusion with
`source_kind=hermes_builtin_memory_import`; MemoryD deduplication makes restart
bootstrap idempotent. No Codex, ChatGPT, or prior-assistant archive is imported.

All network failures fail open: normal Hermes operation continues with empty
external recall. The provider exposes no model tools; writes happen through
native lifecycle hooks.

Test the adapter from this directory:

```sh
PYTHONPATH=/path/to/hermes-agent uv run --with pytest pytest tests -q
```
