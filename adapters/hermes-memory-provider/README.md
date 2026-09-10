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

Recall shares one `timeout_seconds` deadline across all four lanes, including
HTTP headers and body reads, and labels
injected blocks `recall_not_authority`. Provenance uses the protocol's record ID,
profile/workspace, trust level, evidence references and response citations; it
also renders conclusion origin, target, source kind, actor, write origin and
session metadata when the protocol returns them. Agent-authored conclusions
keep empty evidence references when no independent source exists; the adapter
does not invent external source IDs or source-kind fields that recall does not
return. Availability
checks `/healthz` with a timeout capped at 0.5 seconds and caches the result for
two seconds. Prefetch remains independently fail-open and can recover later.
The transport uses numeric IP endpoints (or `localhost`, mapped to IPv4 loopback)
to avoid unbounded DNS lookups, does not follow redirects or environment proxies,
and caps responses at 4 MiB. No per-request worker threads are created.
`max_tokens` (default 1200) is one total rendered-block budget, including lane
headers and provenance, measured by Hermes' native rough token estimator. It is
not an exact model-tokenizer limit. Facts that do not fit are omitted whole.

Native add/replace/remove hooks mirror both memory targets. A profile-local
`state/codex_memoryd/mirrors.sqlite3` stores entry text and only newly created
MemoryD record IDs, scoped to endpoint/profile/workspace/target. Back it up with
the profile and keep it private. Replace/remove use Hermes' unique `old_text`
substring semantics and `/v1/forget` with explicit `mode: archive`; ambiguous
or unknown mappings never trigger broad deletion. Deduplicated records are not
adopted, and bootstrap origins and unrelated writers' records are not archived.
Writes remain best-effort: a failed archive retains its mapping for retry and
blocks the replacement; callers may retry the same hook after recovery. Lost
acknowledgements are not an exactly-once protocol. MemoryD's archive is retained:
re-adding identical previously archived content does not promise resurrection.

Built-in `MEMORY.md` writes are mirrored with actor/source metadata. With
`bootstrap_origin` enabled, the first acknowledged bootstrap for an endpoint,
profile and self-workspace is recorded in profile-local
`state/codex_memoryd/bootstrap.sqlite3`. Concurrent initializations serialize
before posting; later MEMORY.md edits are not reinterpreted as a new origin.
The receipt stores a SHA-256 digest of the original bytes, not memory contents.
Back up this receipt with the profile when relocating the same service. A new
endpoint is a new destination; review bootstrap configuration before relocation.

Bootstrap submits UTF-8 text without newline conversion, but `/v1/conclusions`
normalizes whitespace and may truncate to MemoryD's record limit. This is a
**normalized recall snapshot, not an exact identity archive**. Original bytes
remain in the built-in file; the digest is provenance, not a claim that recall
preserves them. The adapter does not change that file. Disable bootstrap when
an exact import is required or the destination already contains the origin.
An unacknowledged request is retryable; a connection lost after server commit
can still duplicate a conclusion because the daemon has no idempotency key.
No Codex, ChatGPT, or prior-assistant archive is imported.

All network failures fail open: normal Hermes operation continues with empty
external recall. The provider exposes no model tools; writes happen through
native lifecycle hooks.

Test the adapter from this directory:

```sh
PYTHONPATH=/path/to/hermes-agent uv run --with pytest --with pyyaml pytest tests -q
```
