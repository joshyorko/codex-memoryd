#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ACTION="run"
ACTION_SET=0
DRY_RUN=0
REAL_DB="${CODEX_MEMORYD_REAL_DB:-$ROOT/.dogfood/memory.db}"
SANDBOX_DB="${CODEX_MEMORYD_WRITE_SANDBOX_DB:-$ROOT/.dogfood/write-sandbox-memory.db}"
ARTIFACT_DIR="${CODEX_MEMORYD_WRITE_SANDBOX_ARTIFACT_DIR:-}"
PROFILE="${CODEX_MEMORYD_PROFILE:-personal}"
WORKSPACE="${CODEX_MEMORYD_WORKSPACE:-josh-personal}"
QUERY="write sandbox canary"
BIN="${CODEX_MEMORYD_SANDBOX_BIN:-}"

usage() {
  cat <<'EOF'
Usage: scripts/dogfood-write-sandbox.sh [run|refresh|write-canary|diff|promote-preview] [options]

Options:
  --real-db FILE       Source dogfood DB. Default: .dogfood/memory.db
  --sandbox-db FILE    Write-capable sandbox DB. Default: .dogfood/write-sandbox-memory.db
  --artifact-dir DIR   Content-free reports directory. Default: .dogfood/write-sandbox/<timestamp>
  --profile NAME       Profile for sandbox canaries. Default: personal
  --workspace NAME     Workspace for sandbox canaries. Default: josh-personal
  --query TEXT         Non-secret canary phrase for sandbox-only writes
  --bin FILE           codex-memoryd binary path
  --dry-run            Print the safety contract without touching any DB

Actions:
  run              refresh + write-canary + diff + promote-preview
  refresh          schema preflight, then backup create real DB into sandbox
  write-canary     run CLI and MCP write tools against sandbox only
  diff             write content-free diff report and real DB unchanged check
  promote-preview  write a content-free manual promotion preview
EOF
}

dry_run_steps() {
  cat <<'EOF'
Write sandbox safety steps:
1. schema preflight against the real DB using a read-only SQLite connection
2. backup create refresh from real DB into the sandbox DB
3. write-capable sandbox DB canaries via CLI and MCP --write-tools
4. content-free diff report with counts, hashes, and reason-code summaries only
5. manual promotion preview; no automatic promotion to the real DB
6. real DB unchanged fingerprint check before and after sandbox writes
EOF
}

die() {
  echo "dogfood-write-sandbox: $*" >&2
  exit 1
}

log() {
  printf '[write-sandbox] %s\n' "$*"
}

need() {
  local name="$1"
  command -v "$name" >/dev/null 2>&1 || die "missing required command: $name"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    run|refresh|write-canary|diff|promote-preview)
      if [ "$ACTION_SET" = "1" ]; then
        die "only one action may be supplied"
      fi
      ACTION="$1"
      ACTION_SET=1
      ;;
    --real-db)
      [ "$#" -ge 2 ] || die "--real-db requires a file"
      REAL_DB="$2"
      shift
      ;;
    --sandbox-db)
      [ "$#" -ge 2 ] || die "--sandbox-db requires a file"
      SANDBOX_DB="$2"
      shift
      ;;
    --artifact-dir)
      [ "$#" -ge 2 ] || die "--artifact-dir requires a directory"
      ARTIFACT_DIR="$2"
      shift
      ;;
    --profile)
      [ "$#" -ge 2 ] || die "--profile requires a value"
      PROFILE="$2"
      shift
      ;;
    --workspace)
      [ "$#" -ge 2 ] || die "--workspace requires a value"
      WORKSPACE="$2"
      shift
      ;;
    --query)
      [ "$#" -ge 2 ] || die "--query requires text"
      QUERY="$2"
      shift
      ;;
    --bin)
      [ "$#" -ge 2 ] || die "--bin requires a file"
      BIN="$2"
      shift
      ;;
    --dry-run)
      DRY_RUN=1
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
  dry_run_steps
  exit 0
fi

need python3
need timeout

if [ -z "$ARTIFACT_DIR" ]; then
  ARTIFACT_DIR="$ROOT/.dogfood/write-sandbox/$(date -u +%Y%m%dT%H%M%SZ)"
fi

mkdir -p "$ARTIFACT_DIR"

resolve_bin() {
  if [ -n "$BIN" ]; then
    [ -x "$BIN" ] || die "configured binary is not executable: $BIN"
    return
  fi
  if [ -x "$ROOT/target/debug/codex-memoryd" ]; then
    BIN="$ROOT/target/debug/codex-memoryd"
    return
  fi
  need cargo
  log "building target/debug/codex-memoryd"
  cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin codex-memoryd
  BIN="$ROOT/target/debug/codex-memoryd"
}

same_path() {
  python3 - "$1" "$2" <<'PY'
import os
import sys

print("1" if os.path.abspath(sys.argv[1]) == os.path.abspath(sys.argv[2]) else "0")
PY
}

expected_schema_version() {
  python3 - "$ROOT/src/store.rs" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r"pub const STORAGE_SCHEMA_VERSION:\s*i64\s*=\s*(\d+);", text)
if not match:
    raise SystemExit("could not parse STORAGE_SCHEMA_VERSION from src/store.rs")
print(match.group(1))
PY
}

preflight_db() {
  local db="$1"
  local label="$2"
  local out="$3"
  local expected
  expected="$(expected_schema_version)"

  [ -f "$db" ] || die "$label DB missing at $db"
  python3 - "$db" "$expected" "$label" "$out" <<'PY'
import json
import os
import sqlite3
import sys

db, expected, label, out = sys.argv[1:5]
expected = int(expected)
conn = sqlite3.connect(f"file:{os.path.abspath(db)}?mode=ro", uri=True)
conn.execute("PRAGMA query_only=ON")
integrity = conn.execute("PRAGMA integrity_check(1)").fetchone()[0]
if str(integrity).lower() != "ok":
    raise SystemExit(f"{label} integrity_check failed: {integrity}")

tables = {
    row[0]
    for row in conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    )
}
for required in ["schema_meta", "memory_records", "conclusions", "checkpoints"]:
    if required not in tables:
        raise SystemExit(f"{label} missing required table: {required}")

row = conn.execute(
    "SELECT value FROM schema_meta WHERE key = 'schema_version'"
).fetchone()
if row is None:
    raise SystemExit(f"{label} missing schema_meta.schema_version")
recorded = int(row[0])
if recorded != expected:
    raise SystemExit(
        f"{label} schema version {recorded} differs from compiled {expected}; "
        "run the normal upgrade path before sandbox refresh"
    )

counts = {}
for table in sorted(tables):
    if table.startswith("sqlite_"):
        continue
    counts[table] = conn.execute(f'SELECT count(*) FROM "{table}"').fetchone()[0]

payload = {
    "label": label,
    "path": os.path.abspath(db),
    "integrity_ok": True,
    "schema_version": recorded,
    "expected_schema_version": expected,
    "tables": counts,
}
with open(out, "w", encoding="utf-8") as fh:
    json.dump(payload, fh, indent=2, sort_keys=True)
    fh.write("\n")
PY
}

fingerprint_db() {
  local db="$1"
  local label="$2"
  local out="$3"

  [ -f "$db" ] || die "$label DB missing at $db"
  python3 - "$db" "$label" "$out" <<'PY'
import hashlib
import json
import os
import sqlite3
import sys

db, label, out = sys.argv[1:4]
content_like = {
    "content",
    "summary",
    "safe_summary",
    "metadata",
    "source_metadata",
    "steps",
    "guardrails",
    "termination_condition",
    "negative_examples",
    "decisions",
    "next_steps",
    "tests_run",
    "tests_not_run",
    "error_summary",
}
focus = [
    "profiles",
    "workspaces",
    "memory_records",
    "memory_sources",
    "conclusions",
    "checkpoints",
    "visible_turns",
    "subjects",
    "episodes",
    "procedures",
    "procedure_evidence",
    "evidence_ledger",
    "quarantine_reviews",
    "patch_runs",
]

conn = sqlite3.connect(f"file:{os.path.abspath(db)}?mode=ro", uri=True)
conn.row_factory = sqlite3.Row
conn.execute("PRAGMA query_only=ON")
tables = {
    row[0]
    for row in conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    )
}

def quote_ident(name):
    return '"' + name.replace('"', '""') + '"'

def normalize_cell(column, value):
    if value is None:
        return None
    if isinstance(value, bytes):
        value = value.hex()
    text = str(value)
    lower = column.lower()
    if lower in content_like or ("content" in lower and lower != "content_hash"):
        return {"sha256": hashlib.sha256(text.encode("utf-8")).hexdigest()}
    return text

table_entries = []
for table in focus:
    if table not in tables:
        continue
    columns = [row[1] for row in conn.execute(f"PRAGMA table_info({quote_ident(table)})")]
    order = "id" if "id" in columns else columns[0]
    count = conn.execute(f"SELECT count(*) FROM {quote_ident(table)}").fetchone()[0]
    digest = hashlib.sha256()
    for row in conn.execute(f"SELECT * FROM {quote_ident(table)} ORDER BY {quote_ident(order)}"):
        normalized = {
            column: normalize_cell(column, row[column])
            for column in columns
        }
        digest.update(json.dumps(normalized, sort_keys=True).encode("utf-8"))
        digest.update(b"\n")
    table_entries.append({
        "table": table,
        "rows": count,
        "hash": digest.hexdigest(),
    })

database_digest = hashlib.sha256()
for entry in table_entries:
    database_digest.update(json.dumps(entry, sort_keys=True).encode("utf-8"))
    database_digest.update(b"\n")

payload = {
    "label": label,
    "path": os.path.abspath(db),
    "database_hash": database_digest.hexdigest(),
    "tables": table_entries,
}
with open(out, "w", encoding="utf-8") as fh:
    json.dump(payload, fh, indent=2, sort_keys=True)
    fh.write("\n")
PY
}

write_diff_report() {
  local before="$ARTIFACT_DIR/real-fingerprint.before.json"
  local after="$ARTIFACT_DIR/real-fingerprint.after.json"
  local sandbox_fp="$ARTIFACT_DIR/sandbox-fingerprint.json"
  local report="$ARTIFACT_DIR/sandbox-diff-report.json"

  [ -f "$before" ] || fingerprint_db "$REAL_DB" "real-before" "$before"
  fingerprint_db "$REAL_DB" "real-after" "$after"
  fingerprint_db "$SANDBOX_DB" "sandbox" "$sandbox_fp"

  python3 - "$REAL_DB" "$SANDBOX_DB" "$before" "$after" "$sandbox_fp" "$report" <<'PY'
import datetime as dt
import hashlib
import json
import os
import sqlite3
import sys

real_db, sandbox_db, before_path, after_path, sandbox_fp_path, report_path = sys.argv[1:7]
before = json.load(open(before_path, encoding="utf-8"))
after = json.load(open(after_path, encoding="utf-8"))
sandbox_fp = json.load(open(sandbox_fp_path, encoding="utf-8"))
real_unchanged = before["database_hash"] == after["database_hash"]

def connect(path):
    conn = sqlite3.connect(f"file:{os.path.abspath(path)}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA query_only=ON")
    return conn

def table_counts(fp):
    return {entry["table"]: entry["rows"] for entry in fp["tables"]}

def existing_ids(conn, table):
    tables = {
        row[0]
        for row in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
        )
    }
    if table not in tables:
        return set()
    columns = [row[1] for row in conn.execute(f'PRAGMA table_info("{table}")')]
    if "id" not in columns:
        return set()
    return {row[0] for row in conn.execute(f'SELECT id FROM "{table}"')}

real = connect(real_db)
sandbox = connect(sandbox_db)
real_record_ids = existing_ids(real, "memory_records")
real_conclusion_ids = existing_ids(real, "conclusions")

new_memory_records = []
if existing_ids(sandbox, "memory_records"):
    for row in sandbox.execute(
        """SELECT id, profile_id, workspace_id, repo_id, scope, type,
                  sensitivity, portability, confidence, content_hash,
                  archived, trust_state, quarantine_reason
           FROM memory_records
           ORDER BY created_at, id"""
    ):
        if row["id"] in real_record_ids:
            continue
        new_memory_records.append({
            "id": row["id"],
            "profile_id": row["profile_id"],
            "workspace_id": row["workspace_id"],
            "repo_id": row["repo_id"],
            "scope": row["scope"],
            "type": row["type"],
            "sensitivity": row["sensitivity"],
            "portability": row["portability"],
            "confidence": row["confidence"],
            "content_hash": row["content_hash"],
            "archived": bool(row["archived"]),
            "trust_state": row["trust_state"],
            "quarantine_reason": row["quarantine_reason"],
        })

new_conclusions = []
if existing_ids(sandbox, "conclusions"):
    for row in sandbox.execute(
        """SELECT id, profile_id, workspace_id, repo_id, target, content, source_id, created_at
           FROM conclusions
           ORDER BY created_at, id"""
    ):
        if row["id"] in real_conclusion_ids:
            continue
        new_conclusions.append({
            "id": row["id"],
            "profile_id": row["profile_id"],
            "workspace_id": row["workspace_id"],
            "repo_id": row["repo_id"],
            "target": row["target"],
            "source_id": row["source_id"],
            "created_at": row["created_at"],
            "content_sha256": hashlib.sha256(row["content"].encode("utf-8")).hexdigest(),
        })

policy_states = {}
tables = {
    row[0]
    for row in sandbox.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    )
}
if "evidence_ledger" in tables:
    for row in sandbox.execute(
        "SELECT policy_state, count(*) AS n FROM evidence_ledger GROUP BY policy_state ORDER BY policy_state"
    ):
        policy_states[row["policy_state"]] = row["n"]

real_counts = table_counts(after)
sandbox_counts = table_counts(sandbox_fp)
all_tables = sorted(set(real_counts) | set(sandbox_counts))
report = {
    "generated_at": dt.datetime.now(dt.UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
    "real_db": os.path.abspath(real_db),
    "sandbox_db": os.path.abspath(sandbox_db),
    "real_unchanged": real_unchanged,
    "manual_promotion_required": True,
    "content_policy": "counts, hashes, and reason-code summaries only; no stored content serialized",
    "tables": [
        {
            "table": table,
            "real_rows": real_counts.get(table, 0),
            "sandbox_rows": sandbox_counts.get(table, 0),
            "delta": sandbox_counts.get(table, 0) - real_counts.get(table, 0),
        }
        for table in all_tables
    ],
    "new_memory_records": new_memory_records,
    "new_conclusions": new_conclusions,
    "evidence_policy_states": policy_states,
}
with open(report_path, "w", encoding="utf-8") as fh:
    json.dump(report, fh, indent=2, sort_keys=True)
    fh.write("\n")
if not real_unchanged:
    raise SystemExit("real DB fingerprint changed during sandbox lane")
PY
}

refresh_sandbox() {
  resolve_bin
  if [ "$(same_path "$REAL_DB" "$SANDBOX_DB")" = "1" ]; then
    die "sandbox DB must differ from real DB"
  fi
  [ -f "$REAL_DB" ] || die "real dogfood DB missing at $REAL_DB"
  mkdir -p "$(dirname "$SANDBOX_DB")" "$ARTIFACT_DIR"

  log "schema preflight real DB"
  preflight_db "$REAL_DB" "real" "$ARTIFACT_DIR/preflight-real.json"
  fingerprint_db "$REAL_DB" "real-before" "$ARTIFACT_DIR/real-fingerprint.before.json"

  rm -f "$SANDBOX_DB" "$SANDBOX_DB-wal" "$SANDBOX_DB-shm" "$SANDBOX_DB.manifest.json"

  log "backup create refresh into sandbox DB"
  "$BIN" --db "$REAL_DB" backup create --dest "$SANDBOX_DB" > "$ARTIFACT_DIR/backup-create.json"
  "$BIN" --db "$SANDBOX_DB" backup verify --path "$SANDBOX_DB" > "$ARTIFACT_DIR/backup-verify.json"
  preflight_db "$SANDBOX_DB" "sandbox" "$ARTIFACT_DIR/preflight-sandbox.json"
  fingerprint_db "$REAL_DB" "real-after-refresh" "$ARTIFACT_DIR/real-fingerprint.after-refresh.json"
}

write_canary() {
  resolve_bin
  [ -f "$SANDBOX_DB" ] || die "sandbox DB missing at $SANDBOX_DB; run refresh first"
  if [ "$(same_path "$REAL_DB" "$SANDBOX_DB")" = "1" ]; then
    die "sandbox DB must differ from real DB"
  fi
  mkdir -p "$ARTIFACT_DIR"

  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  local canary_run_id
  canary_run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"

  log "write CLI canary against sandbox DB"
  "$BIN" --db "$SANDBOX_DB" conclude \
    --profile "$PROFILE" \
    --workspace "$WORKSPACE" \
    --content "Decision: write sandbox canary $canary_run_id records stay in the sandbox lane." \
    > "$tmp/cli-write.json"

  python3 - "$tmp/cli-write.json" "$ARTIFACT_DIR/cli-write-summary.json" <<'PY'
import json
import sys

raw_path, out_path = sys.argv[1:3]
payload = json.load(open(raw_path, encoding="utf-8"))
summary = {
    "ok": True,
    "surface": "cli",
    "record_id_count": len(payload.get("record_ids", [])),
    "rejected_count": len(payload.get("rejected", [])),
}
with open(out_path, "w", encoding="utf-8") as fh:
    json.dump(summary, fh, indent=2, sort_keys=True)
    fh.write("\n")
if summary["record_id_count"] < 1:
    raise SystemExit("CLI write did not produce a record id")
PY

  log "write MCP canary against sandbox DB"
  python3 - "$tmp/mcp-write.requests.jsonl" "$PROFILE" "$WORKSPACE" "$QUERY" "$canary_run_id" <<'PY'
import json
import sys

path, profile, workspace, query, canary_run_id = sys.argv[1:6]
requests = [
    {
        "jsonrpc": "2.0",
        "id": "init",
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "clientInfo": {"name": "codex-memoryd-write-sandbox", "version": "1"},
            "capabilities": {},
        },
    },
    {
        "jsonrpc": "2.0",
        "id": "tools",
        "method": "tools/list",
        "params": {},
    },
    {
        "jsonrpc": "2.0",
        "id": "conclude",
        "method": "tools/call",
        "params": {
            "name": "memory_conclude",
            "arguments": {
                "profile": profile,
                "workspace": workspace,
                "content": f"Decision: {query} {canary_run_id} remains sandbox-only.",
            },
        },
    },
]
with open(path, "w", encoding="utf-8") as fh:
    for req in requests:
        fh.write(json.dumps(req, separators=(",", ":")) + "\n")
PY
  timeout 20s "$BIN" --db "$SANDBOX_DB" mcp stdio --write-tools \
    < "$tmp/mcp-write.requests.jsonl" > "$tmp/mcp-write.responses.jsonl"

  python3 - "$tmp/mcp-write.responses.jsonl" "$ARTIFACT_DIR/mcp-write-summary.json" <<'PY'
import json
import sys

raw_path, out_path = sys.argv[1:3]
messages = [json.loads(line) for line in open(raw_path, encoding="utf-8") if line.strip()]
tools = []
record_id_count = 0
for msg in messages:
    if msg.get("id") == "tools":
        tools = [tool.get("name") for tool in msg.get("result", {}).get("tools", [])]
    if msg.get("id") == "conclude":
        structured = msg.get("result", {}).get("structuredContent", {})
        record_id_count = len(structured.get("record_ids", []))
summary = {
    "ok": record_id_count > 0 and "memory_conclude" in tools,
    "surface": "mcp-stdio",
    "write_tools_visible": "memory_conclude" in tools,
    "record_id_count": record_id_count,
}
with open(out_path, "w", encoding="utf-8") as fh:
    json.dump(summary, fh, indent=2, sort_keys=True)
    fh.write("\n")
if not summary["ok"]:
    raise SystemExit(f"MCP write canary failed: {summary}")
PY
}

promote_preview() {
  local report="$ARTIFACT_DIR/sandbox-diff-report.json"
  local preview="$ARTIFACT_DIR/manual-promotion-preview.md"
  [ -f "$report" ] || write_diff_report

  python3 - "$report" "$preview" <<'PY'
import json
import sys

report_path, preview_path = sys.argv[1:3]
report = json.load(open(report_path, encoding="utf-8"))
with open(preview_path, "w", encoding="utf-8") as fh:
    fh.write("# Manual Promotion Preview\n\n")
    fh.write("Promotion is explicit and not performed by this script.\n\n")
    fh.write(f"- real_unchanged: {str(report['real_unchanged']).lower()}\n")
    fh.write(f"- new_memory_records: {len(report.get('new_memory_records', []))}\n")
    fh.write(f"- new_conclusions: {len(report.get('new_conclusions', []))}\n")
    fh.write("- inspect sandbox DB locally before re-entering any accepted memory into the real DB\n")
    fh.write("- keep promotion artifacts content-free; use counts, ids, hashes, and reason codes only\n")
    fh.write("- rejected sandbox writes should become policy/eval fixtures using fragmented secret-shaped strings\n")
PY
}

print_summary() {
  local report="$ARTIFACT_DIR/sandbox-diff-report.json"
  local preview="$ARTIFACT_DIR/manual-promotion-preview.md"
  local unchanged
  unchanged="$(python3 - "$report" <<'PY'
import json
import sys
print(str(json.load(open(sys.argv[1], encoding="utf-8"))["real_unchanged"]).lower())
PY
)"
  log "artifact_dir=$ARTIFACT_DIR"
  log "sandbox_db=$SANDBOX_DB"
  log "real_unchanged=$unchanged"
  log "content-free diff report=$report"
  log "manual promotion preview=$preview"
}

case "$ACTION" in
  refresh)
    refresh_sandbox
    write_diff_report
    print_summary
    ;;
  write-canary)
    write_canary
    write_diff_report
    promote_preview
    print_summary
    ;;
  diff)
    write_diff_report
    print_summary
    ;;
  promote-preview)
    promote_preview
    print_summary
    ;;
  run)
    refresh_sandbox
    write_canary
    write_diff_report
    promote_preview
    print_summary
    ;;
  *)
    die "unsupported action: $ACTION"
    ;;
esac
