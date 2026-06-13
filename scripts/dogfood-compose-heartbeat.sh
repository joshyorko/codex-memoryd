#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SERVICE="codex-memoryd"
COMPOSE_PROJECT="${DOGFOOD_COMPOSE_PROJECT:-codex-memoryd}"
HOST_BIND="127.0.0.1:8787"
BASE_URL="http://127.0.0.1:8787"
DOGFOOD_PROFILE="${DOGFOOD_PROFILE:-personal}"
DOGFOOD_WORKSPACE="${DOGFOOD_WORKSPACE:-josh-personal}"
HOST_MEMORIES_MOUNT="/host-codex-memories"
REAL_DB="$ROOT/.dogfood/memory.db"
SANDBOX_DB="$ROOT/.dogfood/mcp-sandbox-memory.db"
ARTIFACT_DIR="$ROOT/.dogfood/heartbeats"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="$ARTIFACT_DIR/$RUN_ID"
MCP_BIN="$ROOT/target/debug/codex-memoryd"
MCP_REQ="$RUN_DIR/mcp-canary.requests.jsonl"
MCP_RESP="$RUN_DIR/mcp-canary.responses.jsonl"
SYNC_PREVIEW="$RUN_DIR/sync-preview.json"
SYNC_APPLY="$RUN_DIR/sync-apply.json"
SYNC_APPLY2="$RUN_DIR/sync-apply-second.json"

need() {
  local name="$1"
  if ! command -v "$name" >/dev/null 2>&1; then
    echo "missing required command: $name" >&2
    exit 2
  fi
}

need docker
need curl
need sqlite3
need python3

COMPOSE=(docker compose --project-name "$COMPOSE_PROJECT")

mkdir -p "$RUN_DIR" "$ROOT/.dogfood"

log() {
  printf '\n[dogfood-compose-heartbeat] %s\n' "$*"
}

die() {
  echo "ERROR: $*" >&2
  exit 1
}

if [ ! -x "$MCP_BIN" ]; then
  log "Building target/debug/codex-memoryd for MCP canary"
  cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin codex-memoryd
fi

log "Rebuild and relaunch Compose from checked-out source"
"${COMPOSE[@]}" up -d --build

log "Wait for compose container to become healthy"
for _ in $(seq 1 60); do
  CID="$("${COMPOSE[@]}" ps -q "$SERVICE" || true)"
  if [ -n "${CID:-}" ]; then
    STATE="$(docker inspect -f '{{.State.Running}}' "$CID")"
    HEALTH="$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}no-healthcheck{{end}}' "$CID")"
    if [ "$STATE" = "true" ] && [ "$HEALTH" = "healthy" ]; then
      break
    fi
  fi
  sleep 1
done

if [ -z "${CID:-}" ]; then
  die "compose service '$SERVICE' container did not appear"
fi
if [ "${STATE:-false}" != "true" ] || [ "${HEALTH:-no-healthcheck}" != "healthy" ]; then
  die "compose container health failed: state=$STATE health=$HEALTH"
fi

log "Verify host publish is localhost-only"
PUBLISHED="$("${COMPOSE[@]}" port "$SERVICE" 8787 || true)"
if [ "$PUBLISHED" != "$HOST_BIND" ]; then
  die "unexpected compose publish for codex-memoryd/8787: '${PUBLISHED:-<none>}'"
fi
if command -v ss >/dev/null 2>&1; then
  if ! ss -ltn | awk '{print $4}' | grep -Fx "$HOST_BIND" >/dev/null; then
    die "host listener not bound to $HOST_BIND"
  fi
fi

wait_for_http() {
  local endpoint="$1"
  local label="$2"
  local attempts="${3:-30}"
  for _ in $(seq 1 "$attempts"); do
    if curl -fsS "$endpoint" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  die "timed out waiting for $label at $endpoint"
}

log "Validate health and status endpoints"
wait_for_http "$BASE_URL/healthz" "/healthz"
wait_for_http "$BASE_URL/v1/status" "/v1/status"
curl -fsS "$BASE_URL/v1/status" | tee "$RUN_DIR/status.json" >/dev/null

log "Run doctor check via container command"
"${COMPOSE[@]}" exec -T "$SERVICE" codex-memoryd --db /data/memory.db doctor | tee "$RUN_DIR/doctor.json" >/dev/null

run_sync() {
  local mode="$1"
  local out="$2"
  "${COMPOSE[@]}" exec -T "$SERVICE" codex-memoryd \
    sync-local --"${mode}" \
    --profile "$DOGFOOD_PROFILE" \
    --workspace "$DOGFOOD_WORKSPACE" \
    "$HOST_MEMORIES_MOUNT" >"$out"
}

extract_sync_metric() {
  local file="$1"
  if command -v jq >/dev/null 2>&1; then
    jq -r '(.mode // "") + " " + ((.created // 0) | tostring) + " " + ((.updated // 0) | tostring) + " " + ((.rejected // 0) | tostring) + " " + (((.warnings // []) | length) | tostring)' "$file"
    return
  fi

  python3 - "$file" <<'PY'
import json
import sys
text = open(sys.argv[1]).read()
dec = json.JSONDecoder()

def decode_first_json(payload):
    for i in range(len(payload)):
        if not payload[i].isspace():
            try:
                obj, _ = dec.raw_decode(payload, i)
                return obj
            except json.JSONDecodeError:
                continue
    return None

obj = decode_first_json(text)
if obj is None:
    sys.exit(2)

mode = obj.get("mode", "")
created = int(obj.get("created", 0) or 0)
updated = int(obj.get("updated", 0) or 0)
rejected = int(obj.get("rejected", 0) or 0)
warnings = obj.get("warnings")
if warnings is None:
    warnings_len = 0
elif isinstance(warnings, list):
    warnings_len = len(warnings)
else:
    warnings_len = 0

print(mode, created, updated, rejected, warnings_len)
PY
}

require_sync_zero_rejections_and_warnings() {
  local file="$1"
  local label="$2"

  local mode created updated rejected warnings_len
  read -r mode created updated rejected warnings_len < <(extract_sync_metric "$file")
  if [ "$rejected" != "0" ] || [ "$warnings_len" != "0" ]; then
    die "sync ${label} reported rejected=${rejected}, warnings_len=${warnings_len}"
  fi

  echo "[$label] mode=$mode created=$created updated=$updated rejected=$rejected warnings_len=$warnings_len"
}

require_sync_second_apply_idempotent() {
  local file="$1"
  local mode created updated rejected warnings_len
  read -r mode created updated rejected warnings_len < <(extract_sync_metric "$file")
  if [ "$created" != "0" ] || [ "$updated" != "0" ] || [ "$rejected" != "0" ] || [ "$warnings_len" != "0" ]; then
    die "second sync-local apply not idempotent: mode=$mode created=$created updated=$updated rejected=$rejected warnings_len=$warnings_len"
  fi
  echo "[second-apply] mode=$mode created=$created updated=$updated rejected=$rejected warnings_len=$warnings_len"
}

log "Run sync-local preview + apply + second apply via compose mount"
run_sync preview "$SYNC_PREVIEW" >"$RUN_DIR/sync-preview.log" 2>&1
run_sync apply "$SYNC_APPLY" >"$RUN_DIR/sync-apply.log" 2>&1
run_sync apply "$SYNC_APPLY2" >"$RUN_DIR/sync-apply-second.log" 2>&1

preview_summary="$(require_sync_zero_rejections_and_warnings "$SYNC_PREVIEW" "preview")"
apply_summary="$(require_sync_zero_rejections_and_warnings "$SYNC_APPLY" "apply")"
second_apply_summary="$(require_sync_second_apply_idempotent "$SYNC_APPLY2")"

log "Refresh sandbox DB from real DB"
if [ ! -f "$REAL_DB" ]; then
  die "real dogfood DB missing at $REAL_DB"
fi
sqlite3 "$REAL_DB" ".backup '$SANDBOX_DB'"

db_check() {
  local file="$1"
  local label="$2"

  if ! sqlite3 "$file" "pragma integrity_check;" | grep -qx "ok"; then
    die "integrity check failed for $label at $file"
  fi

  local schema_version
  schema_version="$(sqlite3 "$file" "PRAGMA schema_version;")"
  if [ -z "$schema_version" ]; then
    die "schema_version missing for $label"
  fi

  for table in memory_records checkpoints conclusions; do
    if ! sqlite3 "$file" "select name from sqlite_master where type='table' and name='${table}';" | grep -qx "$table"; then
      die "missing $table in $label"
    fi
    local count
    count="$(sqlite3 "$file" "select count(*) from ${table};")"
    echo "${label}:${table}=${count}"
  done
}

log "Verify real and sandbox DB integrity/schema/record counts"
real_summary="$(db_check "$REAL_DB" "real")"
sandbox_summary="$(db_check "$SANDBOX_DB" "sandbox")"

log "Run direct MCP stdio read-only canary against sandbox DB"
cat <<'JSON' > "$MCP_REQ"
{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"codex-memoryd-dogfood-heartbeat","version":"1"}}}
{"jsonrpc":"2.0","id":"tools","method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":"status","method":"tools/call","params":{"name":"memory_status","arguments":{}}}
{"jsonrpc":"2.0","id":"recall-safe","method":"tools/call","params":{"name":"memory_recall","arguments":{"query":"safe dogfood mode","profile":"personal","workspace":"josh-personal"}}}
{"jsonrpc":"2.0","id":"recall-stack","method":"tools/call","params":{"name":"memory_recall","arguments":{"query":"current PR stack","profile":"personal","workspace":"josh-personal"}}}
{"jsonrpc":"2.0","id":"search-safe","method":"tools/call","params":{"name":"memory_search","arguments":{"query":"safe dogfood","profile":"personal","workspace":"josh-personal","limit":5}}}
{"jsonrpc":"2.0","id":"conclude","method":"tools/call","params":{"name":"memory_conclude","arguments":{"query":"canary"}}}
JSON

if ! timeout 20s "$MCP_BIN" --db "$SANDBOX_DB" mcp stdio --read-only < "$MCP_REQ" > "$MCP_RESP"; then
  die "MCP stdio read-only canary exited non-zero"
fi

validate_mcp_canary() {
  python3 - "$MCP_RESP" <<'PY'
import json
import sys

path = sys.argv[1]
raw = open(path).read()
dec = json.JSONDecoder()

def iter_objects(text):
    i = 0
    n = len(text)
    while i < n:
        while i < n and text[i].isspace():
            i += 1
        if i >= n:
            break
        try:
            obj, j = dec.raw_decode(text, i)
        except json.JSONDecodeError:
            i += 1
            continue
        yield obj
        i = j

tools = []
status_ok = False
recall_ok = 0
search_ok = False
conclude_error = None
for msg in iter_objects(raw):
    msg_id = msg.get("id")
    if msg_id == "tools":
        tools = [
            tool.get("name")
            for tool in msg.get("result", {}).get("tools", [])
            if tool.get("name")
        ]
    if msg_id == "status":
        content = msg.get("result", {}).get("content", [])
        status_ok = bool(content) and msg.get("error") is None
    if msg_id in {"recall-safe", "recall-stack"}:
        content = msg.get("result", {}).get("content", [])
        if content and msg.get("error") is None:
            recall_ok += 1
    if msg_id == "search-safe":
        content = msg.get("result", {}).get("content", [])
        search_ok = bool(content) and msg.get("error") is None
    if msg_id == "conclude":
        conclude_error = msg.get("error")

tools = sorted(set(tools))
expected = ["memory_recall", "memory_search", "memory_status"]
if tools != expected:
    print(f"unexpected MCP tool list: {tools!r}", file=sys.stderr)
    sys.exit(1)
if not status_ok:
    print("memory_status canary did not return content", file=sys.stderr)
    sys.exit(1)
if recall_ok != 2:
    print(f"memory_recall canaries did not both return content: {recall_ok}", file=sys.stderr)
    sys.exit(1)
if not search_ok:
    print("memory_search canary did not return content", file=sys.stderr)
    sys.exit(1)
if not conclude_error or conclude_error.get("code") != -32601:
    print(f"memory_conclude was not rejected as read-only: {conclude_error!r}", file=sys.stderr)
    sys.exit(1)
print(" ".join(tools))
PY
}

mcp_tool_line="$(validate_mcp_canary)"

log "Compose dogfood heartbeat PASS"
printf 'host=%s\nservice=%s\ndb=%s\nsandbox=%s\n\n' \
  "$HOST_BIND" "$SERVICE" "$REAL_DB" "$SANDBOX_DB"
printf '%s\n' "$preview_summary"
printf '%s\n' "$apply_summary"
printf '%s\n' "$second_apply_summary"
printf 'Real DB summary:\n%s\n' "$real_summary"
printf 'Sandbox DB summary:\n%s\n' "$sandbox_summary"
printf 'MCP tools: %s\nMCP canary result: memory_conclude_rejected=1\n' \
  "$mcp_tool_line"
printf 'MCP read canaries: memory_status=1 memory_recall=2 memory_search=1\n'
printf 'Artifacts: %s\n' "$RUN_DIR"
