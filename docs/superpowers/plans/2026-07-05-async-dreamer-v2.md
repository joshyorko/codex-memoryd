# Async Dreamer v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the smallest deterministic Dreamer job primitive with explicit budgets and durable job records.

**Architecture:** Reuse the existing Dreamer preview engine and existing `dream_runs` audit rows. Add a single `dream_jobs` table plus transport-agnostic request/response types and a narrow service method for deterministic preview-only execution.

**Tech Stack:** Rust, rusqlite, serde, existing `dream`/`service`/`store` layers

---

### Task 1: Pin the public MVP contract

**Files:**
- Modify: `src/protocol.rs`
- Test: `tests/dream_jobs.rs`

- [x] **Step 1: Write the failing test**

```rust
let run = svc.run_dream_job(DreamJobRunRequest { ... }).unwrap();
assert_eq!(run.preview.mode, "preview");
assert_eq!(svc.store.count_table_rows("dream_jobs").unwrap(), 1);
```

- [x] **Step 2: Run test to verify it fails**

Run: `docker run --rm -v /workspaces/remote-box/src/codex-memoryd-157-async-dreamer-v2:/work -w /work rust:1-bookworm cargo test --test dream_jobs`
Expected: FAIL because Dream job request/response types and service method do not exist yet.

- [x] **Step 3: Write minimal implementation**

```rust
pub struct DreamJobRunRequest { ... }
pub struct DreamJobRunResponse { ... }
pub struct DreamJobBudget { ... }
```

- [x] **Step 4: Run test to verify it passes**

Run: `docker run --rm -v /workspaces/remote-box/src/codex-memoryd-157-async-dreamer-v2:/work -w /work rust:1-bookworm cargo test --test dream_jobs`
Expected: PASS

### Task 2: Add the durable job seam

**Files:**
- Create: `migrations/0011_dream_jobs.sql`
- Modify: `src/store.rs`
- Test: `tests/dream_jobs.rs`

- [x] **Step 1: Write the failing test**

```rust
let job = svc.store.get_dream_job("job_det_preview").unwrap().unwrap();
assert_eq!(job.budget.max_candidates, 5);
```

- [x] **Step 2: Run test to verify it fails**

Run: `docker run --rm -v /workspaces/remote-box/src/codex-memoryd-157-async-dreamer-v2:/work -w /work rust:1-bookworm cargo test --test dream_jobs`
Expected: FAIL because `dream_jobs` migration and store accessors do not exist yet.

- [x] **Step 3: Write minimal implementation**

```rust
CREATE TABLE dream_jobs (...);
pub fn upsert_dream_job(&self, job: &DreamJobRecord) -> Result<()>
pub fn get_dream_job(&self, id: &str) -> Result<Option<DreamJobRecord>>
```

- [x] **Step 4: Run test to verify it passes**

Run: `docker run --rm -v /workspaces/remote-box/src/codex-memoryd-157-async-dreamer-v2:/work -w /work rust:1-bookworm cargo test --test dream_jobs`
Expected: PASS

### Task 3: Wire the deterministic preview-only runner

**Files:**
- Modify: `src/service.rs`
- Modify: `src/store.rs`
- Test: `tests/dream_jobs.rs`

- [x] **Step 1: Write the failing test**

```rust
assert_eq!(run.status, "ok_with_limits");
assert!(run.limits_hit.contains(&"max_candidates".to_string()));
```

- [x] **Step 2: Run test to verify it fails**

Run: `docker run --rm -v /workspaces/remote-box/src/codex-memoryd-157-async-dreamer-v2:/work -w /work rust:1-bookworm cargo test --test dream_jobs`
Expected: FAIL until the service method persists job state and forwards bounded Dreamer preview runs.

- [x] **Step 3: Write minimal implementation**

```rust
pub fn run_dream_job(&self, req: DreamJobRunRequest) -> Result<DreamJobRunResponse> {
    // validate deterministic-only constraints
    // persist running job row
    // call dream::run(... mode: "preview")
    // write dream_runs audit row
    // update job row with last_run_id/status
}
```

- [x] **Step 4: Run test to verify it passes**

Run: `docker run --rm -v /workspaces/remote-box/src/codex-memoryd-157-async-dreamer-v2:/work -w /work rust:1-bookworm cargo test --test dream_jobs`
Expected: PASS

### Task 4: Document the first PR boundary

**Files:**
- Create: `docs/async-dreamer-v2.md`

- [x] **Step 1: Write the design note**

```markdown
Status: first-PR design for issue #157.
```

- [x] **Step 2: Capture non-goals and follow-up path**

Run: `sed -n '1,220p' docs/async-dreamer-v2.md`
Expected: document covers job types, budgets, audit reuse, preview-only execution, and failure states.
