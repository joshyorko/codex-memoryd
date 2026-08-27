# Codex MemoryD JIT seam contract

This directory is a renewable maintenance artifact for `joshyorko/codex#317`.
It is not a replacement Codex fork, an autonomous conflict resolver, or a
dependency on the historical `tap-release` branch. The implementation remains
a disposable `codex-rs/ext/memoryd/` shim that is recreated on a fresh upstream
checkout.

## Refresh procedure

From a fresh Codex checkout, create a disposable integration branch, port only
the owned `codex-rs/ext/memoryd/` files and the smallest required extension
registration/config wiring, then run:

```sh
./contrib/codex-memoryd/check.sh .
```

The checker reads the manifest beside itself. It does not fetch, rebase, push,
apply an unbounded patch, or modify the checkout. If an upstream seam moved,
repair the disposable shim manually and update this manifest with the new
source-backed interface; do not add a conflict resolver.

The recorded reference is `df9f537a6e105ac7e457f2e4ebc7431d8614393e`.
`seam-manifest.json` is the source of truth for the shim identifier, owned
paths, symbols, configuration assumptions, protocol, and verification commands.

## Expected current seams

The planned extension owns `codex-rs/ext/memoryd/` and uses the official
extension API contributors `ContextContributor`, `TurnInputContributor`,
`TurnLifecycleContributor`, `TurnItemContributor`,
`ThreadLifecycleContributor`, and `ConfigContributor`. Registration is through
the corresponding methods in `extension-api/src/registry.rs`; the existing
memories extension's `pub fn install` is the registration-pattern canary.

The temporary MemoryD protocol assumes `POST /v1/recall` and `POST /v1/turns`,
with explicit `enabled`, `base_url`, `profile`, and `workspace` configuration.
Recall is bounded and fail-open; writeback contains visible user/assistant
content only. Native Codex memory remains enabled or disabled independently.

## Focused verification

Run from the Codex checkout:

```sh
./contrib/codex-memoryd/check.sh .
cargo test --manifest-path codex-rs/ext/memoryd/Cargo.toml --test conformance
cargo test --manifest-path codex-rs/ext/memoryd/Cargo.toml
```

The last two commands are the focused commands recorded in the manifest. The
script runs the conformance command automatically when both its manifest and
test exist; no live MemoryD server is required by this maintenance check.

## Classifications

- `compatible`: all owned paths/symbols and protocol markers are present; any
  present focused conformance test passed.
- `MEMORYD_CODEX_SEAM_DRIFT`: an owned path, expected upstream interface,
  manifest, or required checker dependency is absent. This includes a checkout
  that has not acquired the extension, so it never silently reports compatible.
- `memoryd_protocol_mismatch`: the owned shim does not expose one of the two
  temporary v1 endpoint markers.
- `ordinary_test_failure`: the focused extension conformance test ran and
  failed. Its exit status is distinct from seam drift.

Exit status is nonzero for every failure classification. The check is bounded
to manifest-listed paths and has a 120-second timeout when `timeout` is
available.
