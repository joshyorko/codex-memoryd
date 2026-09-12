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

Recall shares one `timeout_seconds` deadline (default 0.5 seconds) across all
four sequential lanes, including HTTP headers and body reads. Each request gets
the remaining time divided by the number of unattempted lanes. Fast lanes donate
unused time forward; a stalled lane cannot consume the later lanes' entire
budget. This deliberately favors lane coverage over letting an early slow lane
use the whole deadline; it does not promise every lane succeeds under load.
Recall labels
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

Network failures fail open: successful lanes survive partial failure; entirely
failed recall returns empty context. One warning reports `recall partial` or
`recall failed` with completed/failed lane and rendered fact counts. Successful
empty lanes are not failures. This is a recall outcome, not a daemon health
verdict. Transport details are debug-level for recall and warning-level for
writes, with operation path and exception class/HTTP status, never query,
response body or exception text. The provider exposes no model tools; writes happen through
native lifecycle hooks.

Installing updated files does not hot-reload a provider already imported by
Hermes. A fresh CLI process loads the new code; a long-running gateway needs an
operator-coordinated restart after active work finishes. A new conversation in
the same process is not sufficient. Do not mutate live provider objects or
invalidate active prompt caches to activate an update.

For strictly read-only live diagnostics, note that normal `/v1/recall` updates
returned records' `last_used_at` (`src/recall.rs`, `Store::touch_records`). A
no-write probe must exclude **all** record types (verified against the running
release) so no facts can be touched, or use an isolated fixture daemon. Such a
probe measures candidate lookup, not full fact packing/touch latency. Never
print memory contents, checkpoints, queries, or raw error bodies in diagnostics.

Test the adapter from this directory:

```sh
PYTHONPATH=/path/to/hermes-agent uv run --with pytest --with pyyaml pytest tests -q
```
