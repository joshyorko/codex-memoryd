#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DRY_RUN=0
INCLUDE_DOGFOOD=0
SKIP_CARGO_TEST=0
ARTIFACT_DIR="${CODEX_MEMORYD_RELEASE_GATE_DIR:-}"
BIN="${CODEX_MEMORYD_RELEASE_GATE_BIN:-}"

usage() {
  cat <<'EOF'
Usage: scripts/v0.1-release-gate.sh [--dry-run] [--include-dogfood] [--skip-cargo-test] [--artifact-dir DIR] [--bin FILE]

Runs the practical v0.1 local release gate. The default path is synthetic and
temp-DB only: it does not read or write the real dogfood database.

Options:
  --dry-run           Print the full gate/checklist without running commands
  --include-dogfood   Also run operator dogfood checks that touch .dogfood/
  --skip-cargo-test   Skip full cargo test when an operator has already run it
  --artifact-dir DIR  Write release-gate reports under DIR
  --bin FILE          Use an existing codex-memoryd binary
EOF
}

dry_run() {
  cat <<'EOF'
v0.1 local release gate:
- cargo fmt --all --check
- git diff --check
- cargo test
- codex-memoryd doctor --format json against a temp DB
- codex-memoryd eval substrate --compare --format json
- codex-memoryd eval procedures --format json
- codex-memoryd perf --format json
- scripts/demo-substrate.sh --dry-run
- scripts/dogfood-write-sandbox.sh --dry-run

Tag-required operator checks (opt in with --include-dogfood when the local dogfood lane is available):
- scripts/dogfood-compose-heartbeat.sh
- scripts/codex-memoryd-local-runtime.sh start/status/smoke/restart-survival/stop
- read-only MCP dogfood canary via the compose heartbeat/runbook
- write-capable sandbox canary via scripts/dogfood-write-sandbox.sh run

Landed hardening coverage:
- #140 storage schema upgrade/downgrade safety
- #141 backup, verify, and restore workflow
- #142 policy allow/deny/redaction corpus
- #143 fixture-only one-command substrate demo
- #144 comparative recall/context-pack eval baseline
- #145 procedure activation/guardrail/termination evals
- #146 procedure lifecycle versioning, retirement, counter-evidence
- #147 dogfood write-sandbox and manual promotion lane
- #148 memory poisoning and delayed-trigger red-team suite
- #149 doctor diagnostics and release artifacts
- #150 procedure eval fixtures and quality report
- #151 API/CLI/MCP/eval/adapter contract snapshots and compatibility policy
- #152 recall/card/adapter/procedure perf and cost budgets
EOF
}

die() {
  echo "v0.1-release-gate: $*" >&2
  exit 1
}

log() {
  printf '[release-gate] %s\n' "$*"
}

need() {
  local name="$1"
  command -v "$name" >/dev/null 2>&1 || die "missing required command: $name"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      ;;
    --include-dogfood)
      INCLUDE_DOGFOOD=1
      ;;
    --skip-cargo-test)
      SKIP_CARGO_TEST=1
      ;;
    --artifact-dir)
      [ "$#" -ge 2 ] || die "--artifact-dir requires a directory"
      ARTIFACT_DIR="$2"
      shift
      ;;
    --bin)
      [ "$#" -ge 2 ] || die "--bin requires a file"
      BIN="$2"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
  shift
done

if [ "$DRY_RUN" = "1" ]; then
  dry_run
  exit 0
fi

need cargo
need git
need python3

if [ -z "$ARTIFACT_DIR" ]; then
  ARTIFACT_DIR="$ROOT/target/release-gate/v0.1-$(date -u +%Y%m%dT%H%M%SZ)"
fi
mkdir -p "$ARTIFACT_DIR/logs"

run() {
  local label="$1"
  shift
  local log_file="$ARTIFACT_DIR/logs/$label.log"
  log "$label"
  if "$@" >"$log_file" 2>&1; then
    printf '{"label":"%s","status":"pass","log":"%s"}\n' "$label" "$log_file" >>"$ARTIFACT_DIR/steps.jsonl"
  else
    tail -n 80 "$log_file" >&2 || true
    die "$label failed; see $log_file"
  fi
}

run_to_file() {
  local label="$1"
  local out="$2"
  shift 2
  local log_file="$ARTIFACT_DIR/logs/$label.log"
  log "$label"
  if "$@" >"$out" 2>"$log_file"; then
    printf '{"label":"%s","status":"pass","output":"%s","log":"%s"}\n' "$label" "$out" "$log_file" >>"$ARTIFACT_DIR/steps.jsonl"
  else
    tail -n 80 "$log_file" >&2 || true
    die "$label failed; see $log_file"
  fi
}

resolve_bin() {
  if [ -n "$BIN" ]; then
    [ -x "$BIN" ] || die "configured binary is not executable: $BIN"
    return
  fi
  run "cargo-build-codex-memoryd" cargo build --bin codex-memoryd
  BIN="$ROOT/target/debug/codex-memoryd"
}

validate_json_stream() {
  local label="$1"
  local path="$2"
  shift 2
  python3 - "$label" "$path" "$@" <<'PY'
import json
import sys

label = sys.argv[1]
path = sys.argv[2]
required_keys = sys.argv[3:]
text = open(path, encoding="utf-8").read()
decoder = json.JSONDecoder()
objects = []
i = 0
while i < len(text):
    while i < len(text) and text[i].isspace():
        i += 1
    if i >= len(text):
        break
    obj, end = decoder.raw_decode(text, i)
    objects.append(obj)
    i = end

if not objects:
    raise SystemExit(f"{label}: no JSON object found")

for key in required_keys:
    if not any(isinstance(obj, dict) and key in obj for obj in objects):
        raise SystemExit(f"{label}: missing key {key!r}")

for obj in objects:
    if isinstance(obj, dict) and obj.get("status") not in (None, "pass", "ok"):
        raise SystemExit(f"{label}: non-pass status {obj.get('status')!r}")
    if isinstance(obj, dict) and obj.get("issues"):
        raise SystemExit(f"{label}: reported issues {obj['issues']!r}")
PY
}

validate_required_files() {
  python3 - "$ROOT" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
required = [
    "docs/release/v0.1-hardening.md",
    "docs/compatibility-policy.md",
    "docs/dogfood-write-sandbox.md",
    "docs/demo-substrate.md",
    "tests/policy_corpus.rs",
    "tests/backup_restore.rs",
    "tests/redteam_suite.rs",
    "tests/contract_snapshots.rs",
    "tests/perf_budget.rs",
    "tests/procedure_lifecycle.rs",
]
missing = [path for path in required if not (root / path).exists()]
if missing:
    raise SystemExit("missing release-gate files: " + ", ".join(missing))
PY
}

: >"$ARTIFACT_DIR/steps.jsonl"

run "cargo-fmt" cargo fmt --all --check
run "git-diff-check" git diff --check

if [ "$SKIP_CARGO_TEST" = "0" ]; then
  run "cargo-test" cargo test
else
  log "cargo-test skipped by operator flag"
  printf '{"label":"cargo-test","status":"skipped","reason":"--skip-cargo-test"}\n' >>"$ARTIFACT_DIR/steps.jsonl"
fi

resolve_bin

TEMP_DB="$ARTIFACT_DIR/temp-release-gate.db"
run_to_file "doctor-json" "$ARTIFACT_DIR/doctor.json" "$BIN" --db "$TEMP_DB" doctor --format json
validate_json_stream "doctor-json" "$ARTIFACT_DIR/doctor.json" status storage schema backup policy_corpus mcp quarantine procedures adapters

run_to_file "eval-substrate-compare-json" "$ARTIFACT_DIR/eval-substrate-compare.json" "$BIN" eval substrate --compare --format json
validate_json_stream "eval-substrate-compare-json" "$ARTIFACT_DIR/eval-substrate-compare.json" suite metrics checks

run_to_file "eval-procedures-json" "$ARTIFACT_DIR/eval-procedures.json" "$BIN" eval procedures --format json
validate_json_stream "eval-procedures-json" "$ARTIFACT_DIR/eval-procedures.json" suite metrics

run_to_file "perf-json" "$ARTIFACT_DIR/perf.json" "$BIN" perf --format json
validate_json_stream "perf-json" "$ARTIFACT_DIR/perf.json" suite measurements

run "demo-substrate-dry-run" scripts/demo-substrate.sh --dry-run
run "dogfood-write-sandbox-dry-run" scripts/dogfood-write-sandbox.sh --dry-run
run "release-required-files" validate_required_files

if [ "$INCLUDE_DOGFOOD" = "1" ]; then
  run "dogfood-compose-heartbeat" scripts/dogfood-compose-heartbeat.sh
  run "dogfood-compose-down-before-native" docker compose down
  run "native-runtime-start" scripts/codex-memoryd-local-runtime.sh start
  run "native-runtime-status" scripts/codex-memoryd-local-runtime.sh status
  run "native-runtime-smoke" scripts/codex-memoryd-local-runtime.sh smoke
  run "native-runtime-restart-survival" scripts/codex-memoryd-local-runtime.sh restart-survival
  run "native-runtime-stop" scripts/codex-memoryd-local-runtime.sh stop
  run "dogfood-write-sandbox-run" scripts/dogfood-write-sandbox.sh run
else
  log "operator dogfood checks skipped; rerun with --include-dogfood before tagging"
  printf '{"label":"operator-dogfood","status":"skipped","reason":"rerun with --include-dogfood before tagging"}\n' >>"$ARTIFACT_DIR/steps.jsonl"
fi

python3 - "$ARTIFACT_DIR/steps.jsonl" "$ARTIFACT_DIR/release-gate-summary.json" "$INCLUDE_DOGFOOD" <<'PY'
import json
import sys

steps_path, out_path, include_dogfood = sys.argv[1:4]
steps = [json.loads(line) for line in open(steps_path, encoding="utf-8") if line.strip()]
failed = [step for step in steps if step.get("status") == "fail"]
summary = {
    "suite": "v0.1-release-gate",
    "status": "pass" if not failed else "fail",
    "operator_dogfood_included": include_dogfood == "1",
    "steps": steps,
    "tag_caveat": None
    if include_dogfood == "1"
    else "operator dogfood checks were skipped; rerun with --include-dogfood before tagging",
}
with open(out_path, "w", encoding="utf-8") as fh:
    json.dump(summary, fh, indent=2, sort_keys=True)
    fh.write("\n")
if failed:
    raise SystemExit("release gate failed")
PY

log "pass"
log "artifacts=$ARTIFACT_DIR"
