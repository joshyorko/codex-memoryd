# PR 217 Blocking Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore deterministic runtime-source precedence and prevent `paths` from loading an unselected default config when an explicitly selected config is absent.

**Architecture:** Keep CLI flags as explicit values and resolve environment/runtime.env values in one runtime-resolution layer that records the selected runtime source. Make status mode selection use that source, so an explicit managed runtime cannot be displaced by a lower-precedence URL environment variable while an explicit URL remains a deliberate client override. Model config loading with three named states and use the optional state only for path introspection.

**Tech Stack:** Rust 2021, clap, Cargo integration tests, assert_cmd, tempfile, std TCP listeners.

## Global Constraints

- Do not contact, stop, restart, reconfigure, or write to the daemon at `127.0.0.1:8787`.
- Preserve explicit `--local`/`--db` direct mode, explicit `--url` client mode, managed runtime selection, runtime.env discovery, and the default endpoint contract.
- Keep operational explicit-config validation required; only `paths` may tolerate an absent explicitly selected config file.
- Add regression coverage before production changes and retain the existing #212-#216 exact regressions.
- Do not merge PR #217.

### Task 1: Add the failing runtime precedence matrix

**Files:**
- Modify: `tests/cli_smoke.rs` near the existing runtime status tests.

**Interfaces:** The real `codex-memoryd` binary, `CODEX_MEMORYD_URL`, `CODEX_MEMORYD_RUNTIME`, `CODEX_MEMORYD_HOME`, `CODEX_MEMORYD_HOST`, `CODEX_MEMORYD_PORT`, and generated `runtime.env` values.

- [ ] **Step 1: Add a disposable sentinel listener helper and a matrix test.** The helper must return a loopback URL, record whether it received a request, and return a sentinel HTTP body. The matrix must cover: default/unused endpoint behavior, URL from the environment, URL from an explicit `--url`, runtime selected from `runtime.env`, and explicit `--runtime` with an environment URL. The explicit-runtime case must assert a managed `RuntimeStatusReport`, must not contain the sentinel body, and must prove the listener was not selected.
- [ ] **Step 2: Run the exact new matrix test and confirm it fails because the current Clap-populated `cli.url` sends explicit runtime status to the sentinel URL.** Do not change production code until this failure is observed.

### Task 2: Add failing selected-config state regressions

**Files:**
- Modify: `tests/cli_smoke.rs` near the existing `paths` tests.

**Interfaces:** `--config`, `CODEX_MEMORYD_HOME`, `paths --format json`, operational `status`, and `Config::load` error reporting.

- [ ] **Step 1: Add a valid-default isolation test.** Put a valid default config under the isolated `CODEX_MEMORYD_HOME` with a distinctive enabled adjacent runtime, pass a different missing `--config` path, and assert `paths` succeeds while reporting the selected path as absent and the adjacent runtime as disabled.
- [ ] **Step 2: Add a malformed-default isolation test.** Put invalid TOML at the isolated default config, pass a different missing `--config` path, and assert `paths` succeeds rather than parsing the unselected default.
- [ ] **Step 3: Add selected-config and operational safeguards.** Assert a malformed selected config still fails with its selected path and an operational command with a missing selected config still fails with `config file not found`.
- [ ] **Step 4: Run the exact new config tests and confirm the valid default is loaded or malformed default is parsed by the current `exists().then(Config::load(None))` path.

### Task 3: Implement source-aware runtime resolution

**Files:**
- Modify: `src/cli.rs` option declarations, status dispatch, runtime-resolution helpers, and source registry output.
- Modify: `src/native_runtime.rs` runtime resolution source tracking and endpoint precedence.

**Interfaces:** `Cli::runtime_options`, `RuntimeOptions::resolve`, status routing, and `config show` source metadata.

- [ ] **Step 1: Remove source-collapsing env population from URL/runtime CLI fields while retaining environment support in the runtime resolver.** `cli.url` and `cli.runtime` must represent explicit command-line values; `RuntimeOptions::resolve` must continue resolving process environment and runtime.env values.
- [ ] **Step 2: Define and implement the precedence model.** Direct `--local`/`--db` status remains first; explicit `--url` remains the client endpoint override; explicit `--runtime` selects managed runtime status ahead of process `CODEX_MEMORYD_URL`; process `CODEX_MEMORYD_RUNTIME` precedes runtime.env runtime discovery; runtime.env endpoint values precede the derived host/port endpoint when they belong to the selected managed runtime; the default endpoint remains the final fallback. Expose enough source information for status dispatch and config introspection to use the same decision.
- [ ] **Step 3: Run the runtime matrix test until it passes, then run the existing explicit URL client-mode test to prove explicit URL behavior remains intact.

### Task 4: Implement three-state config loading

**Files:**
- Modify: `src/config.rs` config selection API and loader implementation.
- Modify: `src/cli.rs` `paths_inventory` selection call.

**Interfaces:** Existing `Config::load` required/default behavior plus an explicit optional-for-paths state.

- [ ] **Step 1: Add a named config selection state with `Default`, `ExplicitRequired`, and `ExplicitOptional` variants.** Keep `Config::load(None, ...)` as default discovery and `Config::load(Some(path), ...)` as required explicit loading for existing callers.
- [ ] **Step 2: Route `paths_inventory` through `ExplicitOptional` when `--config` is present and through `Default` when it is absent.** An absent optional selected file must start from built-in defaults plus env/CLI overrides and must not inspect the default config path.
- [ ] **Step 3: Run the selected-config regression tests and the existing paths and operational missing-config tests; verify malformed selected files still fail.

### Task 5: Verify, review, commit, and push

**Files:** Only the scoped source/tests/plan files above.

- [ ] **Step 1: Run the exact new tests, all five exact #212-#216 regression tests, `cargo test --test dreamer`, `cargo test --test cli_smoke`, `cargo test --test dream_jobs`, `cargo fmt --check`, `cargo check`, and `git diff --check`.
- [ ] **Step 2: Review `git diff`, `git diff --stat`, and `git status` for unintended public-contract or environment-routing changes. Rerun any focused check affected by review.
- [ ] **Step 3: Commit with an imperative message such as `Fix CLI runtime and selected-config precedence`.
- [ ] **Step 4: Push `HEAD` to `origin/agent/issues-212-216-test-regressions` without merging PR #217 and capture live push output.
