#!/usr/bin/env bash
set -eu

readonly DRIFT=10
readonly PROTOCOL=20
readonly TEST_FAILURE=30
readonly USAGE=64

manifest_dir=$(builtin cd -- "$(dirname -- "$0")" && pwd -P)
manifest="$manifest_dir/seam-manifest.json"
repo_root=${1:-$(builtin cd -- "$manifest_dir/../.." && pwd -P)}

fail() {
  printf '%s: %s\n' "$1" "$2"
  exit "$3"
}

if [ "$#" -gt 1 ] || [ ! -d "$repo_root" ]; then
  fail "MEMORYD_CODEX_SEAM_DRIFT" "usage: $0 [codex-checkout]" "$USAGE"
fi

if ! command -v rg >/dev/null 2>&1; then
  fail "MEMORYD_CODEX_SEAM_DRIFT" "rg is required" "$DRIFT"
fi
if ! command -v jq >/dev/null 2>&1; then
  fail "MEMORYD_CODEX_SEAM_DRIFT" "jq is required" "$DRIFT"
fi
if ! jq -e . "$manifest" >/dev/null 2>&1; then
  fail "MEMORYD_CODEX_SEAM_DRIFT" "invalid seam manifest: $manifest" "$DRIFT"
fi

has_text() {
  rg -q --fixed-strings -- "$2" "$repo_root/$1" 2>/dev/null
}

for path in $(jq -r '.owned_files[]' "$manifest"); do
  [ -f "$repo_root/$path" ] || fail "MEMORYD_CODEX_SEAM_DRIFT" "missing owned path: $path" "$DRIFT"
done

for entry in $(jq -c '.owned_upstream_symbols[]' "$manifest"); do
  path=$(printf '%s\n' "$entry" | jq -r .path)
  symbol=$(printf '%s\n' "$entry" | jq -r .symbol)
  [ -f "$repo_root/$path" ] || fail "MEMORYD_CODEX_SEAM_DRIFT" "missing seam path: $path" "$DRIFT"
  has_text "$path" "$symbol" || fail "MEMORYD_CODEX_SEAM_DRIFT" "missing seam symbol: $path: $symbol" "$DRIFT"
done

protocol_files=$(jq -r '.owned_files[]' "$manifest" | while IFS= read -r path; do
  [ -f "$repo_root/$path" ] && printf '%s\n' "$repo_root/$path"
done)
printf '%s\n' "$protocol_files" | xargs rg -q --fixed-strings -- '/v1/recall' 2>/dev/null ||
  fail "memoryd_protocol_mismatch" "missing POST /v1/recall" "$PROTOCOL"
printf '%s\n' "$protocol_files" | xargs rg -q --fixed-strings -- '/v1/turns' 2>/dev/null ||
  fail "memoryd_protocol_mismatch" "missing POST /v1/turns" "$PROTOCOL"

test_manifest="$repo_root/codex-rs/ext/memoryd/Cargo.toml"
if [ -f "$test_manifest" ] && [ -f "$repo_root/codex-rs/ext/memoryd/tests/conformance.rs" ]; then
  if command -v timeout >/dev/null 2>&1; then
    timeout 120 cargo test --manifest-path "$test_manifest" --test conformance ||
      fail "ordinary_test_failure" "focused memoryd conformance test failed" "$TEST_FAILURE"
  else
    cargo test --manifest-path "$test_manifest" --test conformance ||
      fail "ordinary_test_failure" "focused memoryd conformance test failed" "$TEST_FAILURE"
  fi
fi

printf '%s\n' 'compatible'
