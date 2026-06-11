#!/usr/bin/env bash
# Local-first coding-memory bakeoff harness for codex-memoryd.
#
# Runs the codex-memoryd leg of demos/memory-bakeoff/README.md end to end on
# loopback, with deterministic assertions for every claimed dimension:
# recall-before/after correctness, secret + hidden-reasoning rejection,
# workspace isolation, supersession, import preview/apply idempotency,
# export/forget, provenance, and a kill-the-daemon fail-open step.
#
# No cloud services, no API keys, no Codex fork required. If CODEX_BIN points
# at a joshyorko/codex@tap-release binary, the daemon-down step additionally
# proves a Codex prompt build still exits 0 (otherwise that proof lives in
# scripts/codex-tap-release-smoke.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CODEX_MEMORYD_URL="${CODEX_MEMORYD_URL:-http://127.0.0.1:8788}"
BIND="${CODEX_MEMORYD_URL#http://}"
BIND="${BIND#https://}"
WORKDIR="${WORKDIR:-$(mktemp -d "${TMPDIR:-/tmp}/codex-memoryd-bakeoff.XXXXXX")}"
OUT="$WORKDIR/bakeoff-output.txt"
DB="$WORKDIR/memory.db"
PROFILE="${PROFILE:-personal}"
WORKSPACE_ID="${WORKSPACE_ID:-bakeoff-ws-a}"
OTHER_WORKSPACE_ID="${OTHER_WORKSPACE_ID:-bakeoff-ws-b}"
FIXTURES="$ROOT/demos/memory-bakeoff/fixtures/memories"
HEALTH_CHECK_ATTEMPTS="${HEALTH_CHECK_ATTEMPTS:-50}"
HEALTH_CHECK_INTERVAL="${HEALTH_CHECK_INTERVAL:-0.1}"
RECALL_LATENCY_SAMPLES="${RECALL_LATENCY_SAMPLES:-25}"

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
  local label="$1" path="$2"
  log "$label"
  printf 'GET %s%s\n' "$CODEX_MEMORYD_URL" "$path" | tee -a "$OUT"
  curl -fsS "$CODEX_MEMORYD_URL$path" | python3 -m json.tool | tee -a "$OUT"
}

json_post() {
  local label="$1" path="$2" body="$3"
  log "$label"
  printf 'POST %s%s\n' "$CODEX_MEMORYD_URL" "$path" | tee -a "$OUT"
  curl -fsS -H 'content-type: application/json' -d "$body" \
    "$CODEX_MEMORYD_URL$path" | python3 -m json.tool | tee -a "$OUT"
}

# POST once, print the response, and assert a python expression over the
# parsed envelope (the request is never repeated, so idempotency stays honest).
json_post_assert() {
  local label="$1" path="$2" body="$3" expr="$4"
  local resp="$WORKDIR/last-response.json"
  log "$label"
  printf 'POST %s%s\n' "$CODEX_MEMORYD_URL" "$path" | tee -a "$OUT"
  curl -fsS -H 'content-type: application/json' -d "$body" \
    "$CODEX_MEMORYD_URL$path" >"$resp"
  python3 -m json.tool <"$resp" | tee -a "$OUT"
  python3 -c "
import json, sys
envelope = json.load(open(sys.argv[1]))
data = envelope.get('data') or {}
assert $expr, 'assertion failed: $expr -> ' + json.dumps(envelope)[:600]
print('ASSERT OK: $expr')
" "$resp" | tee -a "$OUT"
}

# Build a /v1/sync/local-codex-memory body from the fixture directory.
sync_body() {
  local mode="$1" root="$2"
  python3 - "$mode" "$root" "$PROFILE" "$WORKSPACE_ID" <<'PY'
import json, pathlib, sys
mode, root, profile, workspace = sys.argv[1:5]
rootp = pathlib.Path(root)
files = [
    {"path": str(p.relative_to(rootp)), "content": p.read_text()}
    for p in sorted(rootp.rglob("*"))
    if p.is_file()
]
print(json.dumps({
    "profile": profile,
    "workspace": workspace,
    "source_root": root,
    "mode": mode,
    "files": files,
}))
PY
}

now_ms() {
  python3 -c 'import time; print(int(time.time() * 1000))'
}

log "0. Build codex-memoryd (debug build of this checkout)"
run cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin codex-memoryd
MEMORYD_BIN="$ROOT/target/debug/codex-memoryd"

# Mutable copy of the fixture pack so the supersession step can edit it.
cp -R "$FIXTURES" "$WORKDIR/memories"

log "1. Setup + time-to-first-recall (daemon start -> first recall answer)"
START_MS="$(now_ms)"
"$MEMORYD_BIN" --db "$DB" serve --bind "$BIND" \
  >"$WORKDIR/codex-memoryd.out" 2>"$WORKDIR/codex-memoryd.err" &
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
  cat "$WORKDIR/codex-memoryd.err" >&2 || true
  exit 1
fi

RECALL_BODY='{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "query": "sqlite bundled storage decision",
  "max_tokens": 1200
}'
curl -fsS -H 'content-type: application/json' -d "$RECALL_BODY" \
  "$CODEX_MEMORYD_URL/v1/recall" >/dev/null
FIRST_RECALL_MS=$(($(now_ms) - START_MS))
echo "time-to-first-recall: ${FIRST_RECALL_MS} ms (machine-dependent; includes daemon start + schema migration)" | tee -a "$OUT"

json_get "2. Local-first proof: /v1/status reports local_only on loopback" "/v1/status"
echo "storage: $DB (plain SQLite file; no cloud account, no API key, no egress)" | tee -a "$OUT"

json_post_assert "3. Recall-before on the coding fixture pack: must be empty" \
  "/v1/recall" "$RECALL_BODY" \
  "len(data['facts']) == 0 and data['authority'] == 'recall_not_authority'"

json_post_assert "4a. Import preview: counts only, no durable writes" \
  "/v1/sync/local-codex-memory" "$(sync_body preview "$WORKDIR/memories")" \
  "data['mode'] == 'preview' and data['created'] == 0 and data['proposed'] > 0 and data['rejected'] >= 1"

json_post_assert "4b. Preview wrote nothing: recall is still empty" \
  "/v1/recall" "$RECALL_BODY" \
  "len(data['facts']) == 0"

json_post_assert "4c. Import apply: creates records, rejects the injection note" \
  "/v1/sync/local-codex-memory" "$(sync_body apply "$WORKDIR/memories")" \
  "data['mode'] == 'apply' and data['created'] > 0 and data['rejected'] >= 1"

json_post_assert "4d. Second apply is idempotent: creates nothing new" \
  "/v1/sync/local-codex-memory" "$(sync_body apply "$WORKDIR/memories")" \
  "data['created'] == 0 and data['updated'] == 0 and data['skipped'] > 0"

json_post_assert "5. Recall-after: the imported coding decision is recalled" \
  "/v1/recall" "$RECALL_BODY" \
  "any('rusqlite' in f['content'] for f in data['facts']) and data['authority'] == 'recall_not_authority'"

log "6. Recall latency over loopback ($RECALL_LATENCY_SAMPLES warm samples)"
python3 - "$CODEX_MEMORYD_URL" "$RECALL_LATENCY_SAMPLES" "$PROFILE" "$WORKSPACE_ID" <<'PY' | tee -a "$OUT"
import json, statistics, sys, time, urllib.request
url, n, profile, workspace = sys.argv[1] + "/v1/recall", int(sys.argv[2]), sys.argv[3], sys.argv[4]
body = json.dumps({
    "profile": profile,
    "workspace": workspace,
    "query": "sqlite bundled storage decision",
    "max_tokens": 1200,
}).encode()
samples = []
for _ in range(n):
    req = urllib.request.Request(url, data=body, headers={"content-type": "application/json"})
    start = time.perf_counter()
    with urllib.request.urlopen(req) as resp:
        resp.read()
    samples.append((time.perf_counter() - start) * 1000)
samples.sort()
print(f"recall latency ms over {n} samples (machine-dependent): "
      f"p50={statistics.median(samples):.1f} p95={samples[int(n * 0.95) - 1]:.1f} max={samples[-1]:.1f}")
PY

FAKE_KEY="sk-bakeoff$(printf 'A%.0s' $(seq 1 24))"
json_post_assert "7. Secret rejection: an API-key turn never becomes durable memory" \
  "/v1/turns" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "session": { "id": "bakeoff", "source": "bakeoff" },
  "write_policy": "visible_turns",
  "messages": [
    { "actor": "user", "content": "Remember my OpenAI key '"$FAKE_KEY"' for later." }
  ]
}' \
  "data['rejected'] == 1 and data['accepted'] == 0 and '$FAKE_KEY' not in json.dumps(envelope)"

json_post_assert "8. Hidden-reasoning rejection: encrypted reasoning markers are blocked" \
  "/v1/turns" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "session": { "id": "bakeoff", "source": "bakeoff" },
  "write_policy": "visible_turns",
  "messages": [
    { "actor": "assistant", "content": "<encrypted_reasoning>internal chain of thought</encrypted_reasoning>" }
  ]
}' \
  "data['rejected'] == 1 and data['accepted'] == 0"

json_post_assert "9a. Isolation setup: write a decision into workspace $OTHER_WORKSPACE_ID" \
  "/v1/conclusions" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$OTHER_WORKSPACE_ID"'",
  "target": "user",
  "conclusions": ["Decision: workspace B uses postgres, never sqlite."]
}' \
  "len(data['record_ids']) >= 1"

json_post_assert "9b. Workspace isolation: workspace A never recalls workspace B's decision" \
  "/v1/recall" '{
  "profile": "'"$PROFILE"'",
  "workspace": "'"$WORKSPACE_ID"'",
  "query": "workspace B postgres decision",
  "max_tokens": 1200
}' \
  "not any('postgres' in f['content'] for f in data['facts'])"

json_post_assert "9c. Profile setup: a work-profile record for the export-boundary check" \
  "/v1/conclusions" '{
  "profile": "work",
  "workspace": "bakeoff-work",
  "target": "user",
  "conclusions": ["Work decision: internal deploy pipeline runs on Tuesdays."]
}' \
  "envelope['ok'] is True"

log "10. Stale/supersession: edit the fixture, re-apply, prior chunks superseded"
printf '# Memory Summary\n\n- Decision: codex-memoryd stores durable memory in SQLite via rusqlite with the bundled feature; the storage decision was re-confirmed during the bakeoff.\n' \
  >"$WORKDIR/memories/memory_summary.md"
json_post_assert "10a. Re-apply with changed content reports updated (superseded) records" \
  "/v1/sync/local-codex-memory" "$(sync_body apply "$WORKDIR/memories")" \
  "data['created'] + data['updated'] >= 1 and data['rejected'] >= 1"

log "11. Kill the daemon: fail-open proof"
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
if curl -fsS --max-time 2 "$CODEX_MEMORYD_URL/healthz" >/dev/null 2>&1; then
  echo "daemon unexpectedly still answering after kill" >&2
  exit 1
fi
echo "daemon (pid=$MEMORYD_PID) is down; $CODEX_MEMORYD_URL/healthz no longer answers" | tee -a "$OUT"
if [[ -n "${CODEX_BIN:-}" && -x "${CODEX_BIN:-}" ]]; then
  log "11a. Codex fail-open: prompt build must still exit 0 with the daemon down"
  run "$CODEX_BIN" debug prompt-input "Daemon is down; this prompt build must still succeed."
else
  echo "CODEX_BIN not set: the Codex-side fail-open proof (prompt build exits 0 with" | tee -a "$OUT"
  echo "the daemon down) is captured by scripts/codex-tap-release-smoke.sh; the" | tee -a "$OUT"
  echo "contract is documented in docs/codex-integration.md (fail-open contract)." | tee -a "$OUT"
fi

log "12. Local-first availability: the CLI keeps working with no daemon at all"
run "$MEMORYD_BIN" --db "$DB" status
run "$MEMORYD_BIN" --db "$DB" recall --profile "$PROFILE" --workspace "$WORKSPACE_ID" \
  --query "sqlite bundled storage decision"

log "13. Export/forget correctness"
run "$MEMORYD_BIN" --db "$DB" export \
  --profile "$PROFILE" --workspace "$WORKSPACE_ID" --format jsonl
"$MEMORYD_BIN" --db "$DB" export --profile "$PROFILE" --workspace "$WORKSPACE_ID" \
  --format jsonl >"$WORKDIR/export.jsonl" 2>/dev/null
if grep -q "$FAKE_KEY" "$WORKDIR/export.jsonl"; then
  echo "FAIL: export leaked the rejected secret" >&2
  exit 1
fi
echo "ASSERT OK: export contains $(wc -l <"$WORKDIR/export.jsonl") records and never the rejected secret" | tee -a "$OUT"

log "13a. Profile boundary: work -> personal export is denied (exit code 5)"
set +e
"$MEMORYD_BIN" --db "$DB" export --profile work --workspace bakeoff-work \
  --target-profile personal >/dev/null 2>"$WORKDIR/export-denied.err"
DENIED_CODE=$?
set -e
tee -a "$OUT" <"$WORKDIR/export-denied.err"
if [[ "$DENIED_CODE" -ne 5 ]]; then
  echo "FAIL: expected exit code 5 (profile boundary denied), got $DENIED_CODE" >&2
  exit 1
fi
echo "ASSERT OK: work -> personal export denied with exit code 5" | tee -a "$OUT"

log "13b. Forget: archived records disappear from recall"
FORGET_ID="$(python3 -c '
import json, sys
with open(sys.argv[1]) as fh:
    record = json.loads(fh.readline())
print(record["id"])
' "$WORKDIR/export.jsonl")"
run "$MEMORYD_BIN" --db "$DB" forget "$FORGET_ID" --profile "$PROFILE" --reason "bakeoff forget demo"
"$MEMORYD_BIN" --db "$DB" recall --profile "$PROFILE" --workspace "$WORKSPACE_ID" \
  --query "sqlite bundled storage decision" 2>/dev/null \
  | python3 -c "
import json, sys
data = json.load(sys.stdin)
assert all(f.get('record_id') != '$FORGET_ID' and f.get('id') != '$FORGET_ID' for f in data.get('facts', [])), 'forgotten record still recalled'
print('ASSERT OK: forgotten record $FORGET_ID no longer appears in recall')
" | tee -a "$OUT"

log "14. Inspectability/provenance: every exported record carries its evidence trail"
python3 -c '
import json, sys
with open(sys.argv[1]) as fh:
    record = json.loads(fh.readline())
keys = ["id", "type", "profile_id", "workspace_id", "source_ids", "content_hash",
        "supersedes", "created_at"]
print(json.dumps({k: record.get(k) for k in keys}, indent=2))
for key in keys:
    assert key in record, f"missing provenance field {key}"
print("ASSERT OK: provenance fields present (source_ids, content_hash, supersedes, timestamps)")
' "$WORKDIR/export.jsonl" | tee -a "$OUT"

log "Bakeoff artifact"
echo "Pasteable bakeoff output: $OUT" | tee -a "$OUT"
