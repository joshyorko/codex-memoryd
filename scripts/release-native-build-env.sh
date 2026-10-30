#!/usr/bin/env bash
# Source before a release native build, from the checked-out repository root.
# Cargo's encoded form preserves paths containing spaces as single rustc args.
release_native_source="$(pwd -P)"
release_native_cargo="${CARGO_HOME:-$HOME/.cargo}"
case "$release_native_cargo" in
  /*) ;;
  *) echo "CARGO_HOME must be an absolute path for release builds" >&2; return 1 ;;
esac
case "$release_native_source$release_native_cargo" in
  *$'\037'*|*$'\n'*|*=*) echo "release paths contain unsupported characters" >&2; return 1 ;;
esac
printf -v CARGO_ENCODED_RUSTFLAGS '%s\037%s' \
  "--remap-path-prefix=$release_native_source=/usr/src/codex-memoryd" \
  "--remap-path-prefix=$release_native_cargo=/usr/src/cargo"
export CARGO_ENCODED_RUSTFLAGS
unset release_native_source release_native_cargo
