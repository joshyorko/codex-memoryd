#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PROFILE="${DEMO_PROFILE:-personal}"
WORKSPACE="${DEMO_WORKSPACE:-fixture-demo}"
KEEP="${CODEX_MEMORYD_DEMO_KEEP:-0}"
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: scripts/demo-substrate.sh [--dry-run]

Runs a fixture-only end-to-end codex-memoryd substrate demo against a temporary
SQLite database and temporary loopback daemon. It never reads ~/.codex/memories,
never writes .dogfood/memory.db, and never persists real dogfood data.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

demo_steps() {
  cat <<'EOF'
Demo steps:
1. temp fixture DB
2. sync-local fixture import
3. subject and episode
4. recall with policy metadata
5. card show
6. adapter export
7. git-import fixture
8. procedure preview/apply/recall
9. eval substrate
10. read-only MCP canary
EOF
}

if [ "$DRY_RUN" = "1" ]; then
  demo_steps
  exit 0
fi

need() {
  local name="$1"
  if ! command -v "$name" >/dev/null 2>&1; then
    echo "missing required command: $name" >&2
    exit 2
  fi
}

need cargo
need curl
need jq
need python3
need git
need timeout

BIN="${CODEX_MEMORYD_DEMO_BIN:-$ROOT/target/debug/codex-memoryd}"
if [ ! -x "$BIN" ]; then
  echo "[demo] building target/debug/codex-memoryd"
  cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin codex-memoryd
fi

TMP="$(mktemp -d)"
RUN_DIR="$TMP/run"
mkdir -p "$RUN_DIR"
DB="$TMP/fixture-memory.db"
LOG="$RUN_DIR/server.log"
SERVER_PID=""

cleanup() {
  if [ -n "${SERVER_PID:-}" ] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" >/dev/null 2>&1 || true
  fi
  if [ "$KEEP" != "1" ]; then
    rm -rf "$TMP"
  else
    echo "[demo] kept artifacts at $TMP"
  fi
}
trap cleanup EXIT

PORT="$(
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
BASE_URL="http://127.0.0.1:$PORT"

log() {
  printf '\n[demo] %s\n' "$*"
}

require_json() {
  local file="$1"
  local expr="$2"
  local label="$3"
  if ! jq -e "$expr" "$file" >/dev/null; then
    echo "demo assertion failed: $label" >&2
    echo "file: $file" >&2
    jq . "$file" >&2 || cat "$file" >&2
    exit 1
  fi
}

run_json() {
  local out="$1"
  shift
  "$@" >"$out"
}

log "1. temp fixture DB"
"$BIN" --db "$DB" serve --bind "127.0.0.1:$PORT" >"$LOG" 2>&1 &
SERVER_PID="$!"
for _ in $(seq 1 60); do
  if curl -fsS "$BASE_URL/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
curl -fsS "$BASE_URL/healthz" >"$RUN_DIR/healthz.json"
curl -fsS "$BASE_URL/v1/status" >"$RUN_DIR/status.json"
run_json "$RUN_DIR/doctor.json" "$BIN" --db "$DB" doctor --format json
require_json "$RUN_DIR/doctor.json" '.status == "ok"' "doctor reports ok"

log "2. sync-local fixture import"
MEM_ROOT="$TMP/fixture-memories"
mkdir -p "$MEM_ROOT/notes" "$MEM_ROOT/rollout_summaries"
cat >"$MEM_ROOT/MEMORY.md" <<'EOF'
# Fixture Memory Registry

- Decision: codex-memoryd demo uses a fixture-only temporary database.
- Gotcha: release demos must prove recall-not-authority without reading personal dogfood data.
EOF
cat >"$MEM_ROOT/notes/demo.md" <<'EOF'
# Demo Note

Preference: use preview before apply for every durable memory import.
EOF
cat >"$MEM_ROOT/rollout_summaries/demo.md" <<'EOF'
# Demo Rollout Summary

Checkpoint: demo run imports synthetic local memory, then verifies recall, cards, adapters, and MCP.
EOF

run_json "$RUN_DIR/sync-preview.json" \
  "$BIN" --db "$DB" sync-local --preview --profile "$PROFILE" --workspace "$WORKSPACE" "$MEM_ROOT"
run_json "$RUN_DIR/sync-apply.json" \
  "$BIN" --db "$DB" sync-local --apply --profile "$PROFILE" --workspace "$WORKSPACE" "$MEM_ROOT"
require_json "$RUN_DIR/sync-preview.json" '.rejected == 0 and (.warnings | length) == 0' "sync preview clean"
require_json "$RUN_DIR/sync-apply.json" '.rejected == 0 and (.warnings | length) == 0 and .created > 0' "sync apply created fixture records"

log "3. subject and episode"
run_json "$RUN_DIR/subject.json" \
  "$BIN" --db "$DB" subject create --profile "$PROFILE" --workspace "$WORKSPACE" \
    --key "workflow:fixture-release-demo" --kind workflow --display-name "fixture release demo"
SUBJECT_ID="$(jq -r '.subject.id' "$RUN_DIR/subject.json")"
for idx in 1 2; do
  run_json "$RUN_DIR/episode-$idx.json" \
    "$BIN" --db "$DB" episode create --profile "$PROFILE" --workspace "$WORKSPACE" \
      --subject-id "$SUBJECT_ID" --source-kind session --source-ref "demo:procedure:$idx" \
      --status success --trust-level trusted --ended-at "2030-01-0${idx}T00:00:00Z" \
      --summary "Before opening a release PR, run the fixture demo, capture doctor output, and verify read-only MCP rejection."
done
require_json "$RUN_DIR/episode-1.json" '.episode.subject_id == "'"$SUBJECT_ID"'"' "episode linked to subject"

log "4. recall with policy metadata"
run_json "$RUN_DIR/recall.json" \
  "$BIN" --db "$DB" recall --profile "$PROFILE" --workspace "$WORKSPACE" \
    --pack-mode onboarding --max-tokens 800 --query "fixture release demo preview apply"
require_json "$RUN_DIR/recall.json" '.authority == "recall_not_authority" and (.facts | length) > 0' "recall has authority metadata and facts"

log "5. card show"
run_json "$RUN_DIR/card.json" \
  "$BIN" --db "$DB" card show --profile "$PROFILE" --workspace "$WORKSPACE" \
    --type workspace_summary --format json
require_json "$RUN_DIR/card.json" '.authority == "recall_not_authority" and (.records | length) > 0' "card renders records"

log "6. adapter export"
run_json "$RUN_DIR/adapter-mcp-pack.json" \
  "$BIN" --db "$DB" adapter export --profile "$PROFILE" --workspace "$WORKSPACE" \
    --target mcp-pack --format json
require_json "$RUN_DIR/adapter-mcp-pack.json" '.authority == "recall_not_authority" and (.context_pack.records | length) > 0' "adapter export has context pack"

log "7. git-import fixture"
FIXTURE_REPO="$TMP/fixture-repo"
mkdir -p "$FIXTURE_REPO"
git -C "$FIXTURE_REPO" init -q
REFS_FIXTURE="$TMP/refs-fixture.jsonl"
cat >"$REFS_FIXTURE" <<'EOF'
{"kind":"issue","repo":"joshyorko/codex-memoryd","id":"143","author":"fixture","body":"Memory-Decision: keep demo evidence fixture-only and reviewable"}
{"kind":"pr","repo":"joshyorko/codex-memoryd","number":143,"author":"fixture","body":"Memory-Verify: scripts/demo-substrate.sh completes without real dogfood data"}
EOF
run_json "$RUN_DIR/git-import-preview.json" \
  "$BIN" --db "$DB" git-import --preview --refs-fixture "$REFS_FIXTURE" \
    --profile "$PROFILE" --workspace "$WORKSPACE" "$FIXTURE_REPO"
run_json "$RUN_DIR/git-import-apply.json" \
  "$BIN" --db "$DB" git-import --apply --refs-fixture "$REFS_FIXTURE" \
    --profile "$PROFILE" --workspace "$WORKSPACE" "$FIXTURE_REPO"
require_json "$RUN_DIR/git-import-preview.json" '.mode == "preview" and .proposed >= 2 and .rejected == 0' "git fixture preview clean"
require_json "$RUN_DIR/git-import-apply.json" '.mode == "apply" and .created >= 2 and .rejected == 0' "git fixture apply created evidence"

log "8. procedure preview/apply/recall"
run_json "$RUN_DIR/procedure-preview.json" \
  "$BIN" --db "$DB" procedure preview --profile "$PROFILE" --workspace "$WORKSPACE" \
    --subject-id "$SUBJECT_ID"
require_json "$RUN_DIR/procedure-preview.json" '(.candidates | length) > 0' "procedure preview produced candidate"
jq '{profile:"'"$PROFILE"'", workspace:"'"$WORKSPACE"'", candidates:.candidates}' \
  "$RUN_DIR/procedure-preview.json" >"$RUN_DIR/procedure-apply.request.json"
curl -fsS -X POST "$BASE_URL/v1/procedures/apply" \
  -H 'content-type: application/json' \
  --data @"$RUN_DIR/procedure-apply.request.json" >"$RUN_DIR/procedure-apply.json"
require_json "$RUN_DIR/procedure-apply.json" '.ok == true and (.data.applied | length) > 0' "procedure apply accepted candidate"
run_json "$RUN_DIR/procedure-recall.json" \
  "$BIN" --db "$DB" procedure recall --profile "$PROFILE" --workspace "$WORKSPACE" \
    --subject-id "$SUBJECT_ID" --query "opening a release PR"
require_json "$RUN_DIR/procedure-recall.json" '(.procedures | length) > 0 and .procedures[0].policy.authority == "recall_not_authority"' "procedure recall returns reviewable guidance"

log "9. eval substrate"
run_json "$RUN_DIR/eval-substrate.json" "$BIN" eval substrate --format json
require_json "$RUN_DIR/eval-substrate.json" '.status == "pass"' "substrate eval passes"

log "10. read-only MCP canary"
MCP_REQ="$RUN_DIR/mcp.requests.jsonl"
MCP_RESP="$RUN_DIR/mcp.responses.jsonl"
cat >"$MCP_REQ" <<'JSON'
{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"codex-memoryd-fixture-demo","version":"1"}}}
{"jsonrpc":"2.0","id":"tools","method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":"status","method":"tools/call","params":{"name":"memory_status","arguments":{}}}
{"jsonrpc":"2.0","id":"recall","method":"tools/call","params":{"name":"memory_recall","arguments":{"query":"fixture release demo","profile":"personal","workspace":"fixture-demo"}}}
{"jsonrpc":"2.0","id":"search","method":"tools/call","params":{"name":"memory_search","arguments":{"query":"fixture demo","profile":"personal","workspace":"fixture-demo","limit":3}}}
{"jsonrpc":"2.0","id":"conclude","method":"tools/call","params":{"name":"memory_conclude","arguments":{"query":"canary"}}}
JSON
timeout 20s "$BIN" --db "$DB" mcp stdio --read-only <"$MCP_REQ" >"$MCP_RESP"
python3 - "$MCP_RESP" <<'PY'
import json
import sys

raw = open(sys.argv[1], encoding="utf-8").read()
dec = json.JSONDecoder()

def objects(text):
    i = 0
    while i < len(text):
        while i < len(text) and text[i].isspace():
            i += 1
        if i >= len(text):
            break
        obj, i = dec.raw_decode(text, i)
        yield obj

tools = []
status_ok = False
recall_ok = False
search_ok = False
conclude_error = None
for msg in objects(raw):
    msg_id = msg.get("id")
    if msg_id == "tools":
        tools = sorted(t["name"] for t in msg.get("result", {}).get("tools", []))
    elif msg_id == "status":
        status_ok = bool(msg.get("result", {}).get("content")) and "error" not in msg
    elif msg_id == "recall":
        recall_ok = bool(msg.get("result", {}).get("content")) and "error" not in msg
    elif msg_id == "search":
        search_ok = bool(msg.get("result", {}).get("content")) and "error" not in msg
    elif msg_id == "conclude":
        conclude_error = msg.get("error")

if tools != ["memory_recall", "memory_search", "memory_status"]:
    raise SystemExit(f"unexpected tools: {tools!r}")
if not status_ok or not recall_ok or not search_ok:
    raise SystemExit("read-only MCP read canary failed")
if not conclude_error or conclude_error.get("code") != -32601:
    raise SystemExit(f"memory_conclude was not rejected: {conclude_error!r}")
PY

log "PASS"
printf 'fixture_db=%s\nartifacts=%s\n' "$DB" "$RUN_DIR"
printf 'safety=fixture-only real_dogfood_mutated=0 read_only_mcp=1\n'
