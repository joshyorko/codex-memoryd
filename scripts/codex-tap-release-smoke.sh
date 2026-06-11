#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CODEX_BIN="${CODEX_BIN:-$(command -v codex || true)}"
CODEX_MEMORYD_URL="${CODEX_MEMORYD_URL:-http://127.0.0.1:8787}"
BIND="${CODEX_MEMORYD_URL#http://}"
BIND="${BIND#https://}"
WORKDIR="${WORKDIR:-$(mktemp -d "${TMPDIR:-/tmp}/codex-memoryd-tap-smoke.XXXXXX")}"
OUT="$WORKDIR/smoke-output.txt"
PROFILE="${PROFILE:-personal}"
WORKSPACE_ID="${WORKSPACE_ID:-codex-memoryd-live-smoke}"
HEALTH_CHECK_ATTEMPTS="${HEALTH_CHECK_ATTEMPTS:-50}"
HEALTH_CHECK_INTERVAL="${HEALTH_CHECK_INTERVAL:-0.1}"
MEMORY_SUMMARY_CONTENT=$'# Memory Summary\n- Prefer repo-native commands when working in codex-memoryd.\n- Decision: use codex-memoryd provider mode for the tap-release smoke.'

if [[ -z "$CODEX_BIN" || ! -x "$CODEX_BIN" ]]; then
  cat >&2 <<'EOF'
Set CODEX_BIN to a codex executable built from joshyorko/codex@tap-release, for example:
  git clone --branch tap-release https://github.com/joshyorko/codex /tmp/codex-tap-release
  # Debug build is sufficient; "tap-release" is the branch name.
  cargo build --manifest-path /tmp/codex-tap-release/codex-rs/Cargo.toml -p codex-cli --bin codex
  CODEX_BIN=/tmp/codex-tap-release/codex-rs/target/debug/codex scripts/codex-tap-release-smoke.sh
EOF
  exit 2
fi

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 2
  fi
}

need curl
need python3

mkdir -p "$WORKDIR"
: >"$OUT"
MEMORY_SUMMARY_JSON="$(printf '%s' "$MEMORY_SUMMARY_CONTENT" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')"

log() {
  printf '\n### %s\n' "$*" | tee -a "$OUT"
}

run() {
  printf '\n> ' | tee -a "$OUT"
  printf '%q ' "$@" | tee -a "$OUT"
  printf '\n' | tee -a "$OUT"
  "$@" 2>&1 | tee -a "$OUT"
}

json_get() {
  local label="$1"
  local path="$2"
  log "$label"
  printf 'GET %s%s\n' "$CODEX_MEMORYD_URL" "$path" | tee -a "$OUT"
  curl -fsS "$CODEX_MEMORYD_URL$path" \
    | python3 -m json.tool \
    | tee -a "$OUT"
}

json_post() {
  local label="$1"
  local path="$2"
  local body="$3"
  log "$label"
  printf 'POST %s%s\n' "$CODEX_MEMORYD_URL" "$path" | tee -a "$OUT"
  curl -fsS \
    -H 'content-type: application/json' \
    -d "$body" \
    "$CODEX_MEMORYD_URL$path" \
    | python3 -m json.tool \
    | tee -a "$OUT"
}

log "Build codex-memoryd"
run cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin codex-memoryd
MEMORYD_BIN="$ROOT/target/debug/codex-memoryd"

export CODEX_HOME="$WORKDIR/codex-home"
mkdir -p "$CODEX_HOME/memories"
printf '%s' "$MEMORY_SUMMARY_CONTENT" >"$CODEX_HOME/memories/memory_summary.md"

log "Start codex-memoryd on loopback"
"$MEMORYD_BIN" --db "$WORKDIR/memory.db" serve --bind "$BIND" \
  >"$WORKDIR/codex-memoryd.out" \
  2>"$WORKDIR/codex-memoryd.err" &
MEMORYD_PID=$!
cleanup() {
  if kill -0 "$MEMORYD_PID" >/dev/null 2>&1; then
    kill "$MEMORYD_PID" >/dev/null 2>&1 || true
    wait "$MEMORYD_PID" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

for _ in $(seq 1 "$HEALTH_CHECK_ATTEMPTS"); do
  if curl -fsS "$CODEX_MEMORYD_URL/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep "$HEALTH_CHECK_INTERVAL"
done
if ! curl -fsS "$CODEX_MEMORYD_URL/healthz" >/dev/null 2>&1; then
  echo "codex-memoryd did not become healthy at $CODEX_MEMORYD_URL" >&2
  cat "$WORKDIR/codex-memoryd.out" >&2 || true
  cat "$WORKDIR/codex-memoryd.err" >&2 || true
  exit 1
fi
echo "daemon pid=$MEMORYD_PID url=$CODEX_MEMORYD_URL db=$WORKDIR/memory.db" | tee -a "$OUT"

json_get "Captured /v1/status output" "/v1/status"

json_post "Seed /v1/conclusions through the provider contract" "/v1/conclusions" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "target": "user",
  "conclusions": [
    "Decision: tap-release provider smoke talks to codex-memoryd on loopback."
  ],
  "metadata": { "source": "codex-tap-release-smoke" }
}'

json_post "Captured recall output; authority must be recall_not_authority" "/v1/recall" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "query": "tap-release provider smoke loopback",
  "max_tokens": 1200
}'

json_post "Captured /v1/turns writeback counts" "/v1/turns" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "session": { "id": "tap-release-smoke", "source": "codex-cli" },
  "write_policy": "visible_turns",
  "messages": [
    { "actor": "user", "content": "Run the codex-memoryd provider smoke." },
    { "actor": "assistant", "content": "Verified provider and hybrid memory paths." }
  ]
}'

json_post "Captured local import preview" "/v1/sync/local-codex-memory" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "source_root": "'"$CODEX_HOME"'/memories",
  "mode": "preview",
  "files": [
    {
      "path": "memory_summary.md",
      "kind": "memory_summary",
      "content": '"$MEMORY_SUMMARY_JSON"'
    }
  ]
}'

json_post "Captured local import apply" "/v1/sync/local-codex-memory" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "source_root": "'"$CODEX_HOME"'/memories",
  "mode": "apply",
  "files": [
    {
      "path": "memory_summary.md",
      "kind": "memory_summary",
      "content": '"$MEMORY_SUMMARY_JSON"'
    }
  ]
}'

json_post "Captured local import apply idempotency" "/v1/sync/local-codex-memory" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "source_root": "'"$CODEX_HOME"'/memories",
  "mode": "apply",
  "files": [
    {
      "path": "memory_summary.md",
      "kind": "memory_summary",
      "content": '"$MEMORY_SUMMARY_JSON"'
    }
  ]
}'

log "Codex tap-release provider mode"
run "$CODEX_BIN" memory setup \
  --provider codex-memoryd \
  --backend provider \
  --provider-url "$CODEX_MEMORYD_URL" \
  --profile "$PROFILE" \
  --workspace "$WORKSPACE_ID"
run "$CODEX_BIN" memory status
run "$CODEX_BIN" debug prompt-input "Recall the tap-release provider smoke decision."
run "$CODEX_BIN" memory import-local --preview
run "$CODEX_BIN" memory import-local --apply

log "Codex tap-release hybrid mode"
run "$CODEX_BIN" memory setup \
  --provider codex-memoryd \
  --backend hybrid \
  --provider-url "$CODEX_MEMORYD_URL" \
  --profile "$PROFILE" \
  --workspace "$WORKSPACE_ID"
run "$CODEX_BIN" memory status
run "$CODEX_BIN" debug prompt-input "Recall the tap-release hybrid smoke decision."

log "Daemon-down fail-open check"
trap - EXIT
kill -TERM "$MEMORYD_PID"
for _ in $(seq 1 "$HEALTH_CHECK_ATTEMPTS"); do
  if ! kill -0 "$MEMORYD_PID" >/dev/null 2>&1; then
    break
  fi
  sleep "$HEALTH_CHECK_INTERVAL"
done
if kill -0 "$MEMORYD_PID" >/dev/null 2>&1; then
  kill -KILL "$MEMORYD_PID" >/dev/null 2>&1 || true
fi
wait "$MEMORYD_PID" >/dev/null 2>&1 || true
if "$CODEX_BIN" debug prompt-input "Daemon is down; this prompt build must still succeed." >"$WORKDIR/fail-open.json" 2>"$WORKDIR/fail-open.err"; then
  echo "fail-open: codex debug prompt-input exited 0 with daemon down" | tee -a "$OUT"
  tee -a "$OUT" <"$WORKDIR/fail-open.json"
else
  cat "$WORKDIR/fail-open.err" >&2
  exit 1
fi

log "Smoke artifact"
echo "Pasteable smoke output: $OUT" | tee -a "$OUT"
