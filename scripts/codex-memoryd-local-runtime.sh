#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${CODEX_MEMORYD_BIN:-$ROOT/target/debug/codex-memoryd}"
DB="${CODEX_MEMORYD_DB:-$ROOT/.dogfood/memory.db}"
BIND="${CODEX_MEMORYD_BIND:-127.0.0.1:8787}"
PROFILE="${CODEX_MEMORYD_PROFILE:-personal}"
WORKSPACE="${CODEX_MEMORYD_WORKSPACE:-josh-personal}"
LOG_LEVEL="${CODEX_MEMORYD_LOG:-info}"
RUNTIME_DIR="${CODEX_MEMORYD_RUNTIME_DIR:-$ROOT/.dogfood}"
LOG_DIR="$RUNTIME_DIR/logs"
PID_FILE="${CODEX_MEMORYD_PID_FILE:-$RUNTIME_DIR/codex-memoryd.pid}"
LOG_FILE="${CODEX_MEMORYD_LOG_FILE:-$LOG_DIR/codex-memoryd.log}"
BASE_URL="${CODEX_MEMORYD_BASE_URL:-http://127.0.0.1:8787}"
ALLOW_NON_LOOPBACK="${CODEX_MEMORYD_ALLOW_NON_LOOPBACK:-0}"

usage() {
  cat <<'EOF'
Usage: scripts/codex-memoryd-local-runtime.sh <command>

Canonical local/self-host runtime helper for codex-memoryd.

Commands:
  start             Build if needed and start a native loopback daemon.
  stop              Stop the daemon recorded in .dogfood/codex-memoryd.pid.
  status            Print process, health, and provider status.
  smoke             Run health, status, doctor, import preview, recall, and export checks.
  restart-survival  Restart the daemon and prove status plus recall still work.
  systemd-unit      Print a systemd --user unit for this checkout.
  help              Show this help.

Environment:
  CODEX_MEMORYD_BIN          Binary path. Default: target/debug/codex-memoryd
  CODEX_MEMORYD_DB           SQLite DB path. Default: .dogfood/memory.db
  CODEX_MEMORYD_BIND         Bind address. Default: 127.0.0.1:8787
  CODEX_MEMORYD_BASE_URL     HTTP base URL. Default: http://127.0.0.1:8787
  CODEX_MEMORYD_PROFILE      Adapter profile name. Default: personal
  CODEX_MEMORYD_WORKSPACE    Adapter workspace name. Default: josh-personal
  CODEX_MEMORYD_LOG          Log level. Default: info
  CODEX_MEMORYD_ALLOW_NON_LOOPBACK=1 permits non-loopback binds for a
                               self-host deployment behind HTTPS auth.

Safety:
  Local defaults are loopback-only. Non-loopback binds are rejected unless
  CODEX_MEMORYD_ALLOW_NON_LOOPBACK=1 is set; use that only behind a normal
  authenticated HTTPS front door.
EOF
}

die() {
  echo "ERROR: $*" >&2
  exit 1
}

need() {
  local name="$1"
  command -v "$name" >/dev/null 2>&1 || die "missing required command: $name"
}

is_loopback_bind() {
  case "$BIND" in
    127.*:* | localhost:* | "[::1]":* | "::1":*) return 0 ;;
    *) return 1 ;;
  esac
}

require_safe_bind() {
  if is_loopback_bind; then
    return 0
  fi
  if [ "$ALLOW_NON_LOOPBACK" = "1" ]; then
    return 0
  fi
  die "refusing non-loopback bind '$BIND'; set CODEX_MEMORYD_ALLOW_NON_LOOPBACK=1 only behind HTTPS auth"
}

ensure_binary() {
  if [ -x "$BIN" ]; then
    return 0
  fi
  need cargo
  cargo build --manifest-path "$ROOT/Cargo.toml" --bin codex-memoryd
}

pid_is_running() {
  [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" >/dev/null 2>&1
}

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

start() {
  require_safe_bind
  ensure_binary
  mkdir -p "$LOG_DIR" "$(dirname "$DB")"
  if pid_is_running; then
    echo "codex-memoryd already running pid=$(cat "$PID_FILE") bind=$BIND db=$DB"
    return 0
  fi

  env \
    CODEX_MEMORYD_DB="$DB" \
    CODEX_MEMORYD_BIND="$BIND" \
    CODEX_MEMORYD_PROFILE="$PROFILE" \
    CODEX_MEMORYD_WORKSPACE="$WORKSPACE" \
    CODEX_MEMORYD_LOG="$LOG_LEVEL" \
    "$BIN" serve >>"$LOG_FILE" 2>&1 &
  echo "$!" >"$PID_FILE"

  wait_for_http "$BASE_URL/healthz" "/healthz"
  echo "codex-memoryd started pid=$(cat "$PID_FILE") bind=$BIND db=$DB log=$LOG_FILE"
}

stop() {
  if ! pid_is_running; then
    rm -f "$PID_FILE"
    echo "codex-memoryd not running"
    return 0
  fi

  local pid
  pid="$(cat "$PID_FILE")"
  kill "$pid"
  for _ in $(seq 1 20); do
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      rm -f "$PID_FILE"
      echo "codex-memoryd stopped pid=$pid"
      return 0
    fi
    sleep 0.2
  done
  die "codex-memoryd did not stop cleanly pid=$pid"
}

status() {
  need curl
  if pid_is_running; then
    echo "process=running pid=$(cat "$PID_FILE")"
  else
    echo "process=stopped"
  fi
  echo "bind=$BIND"
  echo "base_url=$BASE_URL"
  echo "db=$DB"
  curl -fsS "$BASE_URL/healthz"
  echo
  curl -fsS "$BASE_URL/v1/status"
  echo
}

smoke() {
  need curl
  start
  wait_for_http "$BASE_URL/healthz" "/healthz"
  wait_for_http "$BASE_URL/v1/status" "/v1/status"
  "$BIN" --db "$DB" doctor
  "$BIN" --db "$DB" sync-local --preview "$HOME/.codex/memories" \
    --profile "$PROFILE" --workspace "$WORKSPACE" >/dev/null
  "$BIN" --db "$DB" recall \
    --profile "$PROFILE" --workspace "$WORKSPACE" \
    --query "safe dogfood mode" --max-tokens 800 >/dev/null
  "$BIN" --db "$DB" export \
    --profile "$PROFILE" --workspace "$WORKSPACE" >/dev/null
  echo "smoke=pass profile=$PROFILE workspace=$WORKSPACE db=$DB"
}

restart_survival() {
  start
  stop
  start
  "$BIN" --db "$DB" recall \
    --profile "$PROFILE" --workspace "$WORKSPACE" \
    --query "safe dogfood mode" --max-tokens 800 >/dev/null
  echo "restart_survival=pass"
}

systemd_unit() {
  require_safe_bind
  cat <<EOF
[Unit]
Description=codex-memoryd local memory runtime
After=network.target

[Service]
Type=simple
WorkingDirectory=$ROOT
Environment=CODEX_MEMORYD_DB=$DB
Environment=CODEX_MEMORYD_BIND=$BIND
Environment=CODEX_MEMORYD_PROFILE=$PROFILE
Environment=CODEX_MEMORYD_WORKSPACE=$WORKSPACE
Environment=CODEX_MEMORYD_LOG=$LOG_LEVEL
ExecStart=$BIN serve
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
EOF
}

cmd="${1:-help}"
case "$cmd" in
  start) start ;;
  stop) stop ;;
  status) status ;;
  smoke) smoke ;;
  restart-survival) restart_survival ;;
  systemd-unit) systemd_unit ;;
  help | --help | -h) usage ;;
  *) usage >&2; exit 2 ;;
esac
