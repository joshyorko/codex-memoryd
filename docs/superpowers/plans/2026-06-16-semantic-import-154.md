# Semantic Import #154 Implementation Plan

**For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an explicit `codex-memoryd semantic preview/apply` JSON import path for reviewed aliases and relations so #154 can close without broad retrieval, temporal, Dreamer, or graph work.

**Architecture:** Keep semantic import in a focused module that validates JSON against the existing `Store` boundary. Add tiny store lookup helpers for preview idempotency, wire a CLI subcommand, then update docs and tests. Store-level insert methods remain final enforcement for scope and evidence.

**Tech Stack:** Rust, clap, serde/serde_json, rusqlite-backed `Store`, assert_cmd CLI tests.

---

## File Structure

- Create `src/semantic_import.rs`
  - Owns input structs, output report structs, validation, preview, and apply.
  - Does not depend on HTTP, MCP, Dreamer, eval, or temporal code.
- Modify `src/store.rs`
  - Add read-only lookup helpers for existing aliases and active relations.
  - Do not weaken existing insert validators.
- Modify `src/lib.rs`
  - Export `semantic_import`.
- Modify `src/cli.rs`
  - Add `Semantic` top-level subcommand with `preview` and `apply`.
  - Parse JSON file, call semantic import functions, print JSON.
- Create `tests/semantic_import.rs`
  - Unit/integration tests around preview/apply behavior at store level.
- Modify `tests/cli_smoke.rs`
  - Add one end-to-end CLI smoke covering preview and apply.
- Modify `docs/semantic-layer.md`
  - Update pending status and add exact commands.
- Modify `README.md`
  - Add short operator pointer if semantic-layer docs are listed from README.

## Task 1: Add Store Lookup Helpers

**Files:**
- Modify: `src/store.rs`
- Test: `tests/semantic_import.rs` in Task 2

- [ ] **Step 1: Add read-only alias lookup helper**

Add this public method near `resolve_subject_alias`:

```rust
pub fn get_subject_alias(
    &self,
    profile_id: &str,
    workspace_id: &str,
    alias_key: &str,
) -> Result<Option<SubjectAlias>> {
    let conn = self.conn()?;
    let alias = conn
        .query_row(
            &format!(
                "SELECT {SUBJECT_ALIAS_COLS} FROM subject_aliases
                 WHERE profile_id = ?1 AND workspace_id = ?2 AND alias_key = ?3"
            ),
            params![profile_id, workspace_id, alias_key],
            row_to_subject_alias,
        )
        .optional()?;
    Ok(alias)
}
```

- [ ] **Step 2: Add read-only active relation lookup helper**

Add this public method before `relation_expanded_subjects`:

```rust
pub fn get_active_relation(
    &self,
    profile_id: &str,
    workspace_id: &str,
    from_subject_id: &str,
    relation_type: &str,
    to_subject_id: &str,
) -> Result<Option<Relation>> {
    let conn = self.conn()?;
    let relation = conn
        .query_row(
            &format!(
                "SELECT {RELATION_COLS} FROM relations
                 WHERE profile_id = ?1
                   AND workspace_id = ?2
                   AND from_subject_id = ?3
                   AND relation_type = ?4
                   AND to_subject_id = ?5
                   AND retired_at IS NULL
                   AND state != 'retired'"
            ),
            params![
                profile_id,
                workspace_id,
                from_subject_id,
                relation_type,
                to_subject_id
            ],
            row_to_relation,
        )
        .optional()?;
    Ok(relation)
}
```

- [ ] **Step 3: Run format/check for store compile**

Run:

```bash
rtk cargo check
```

Expected: compiles or fails only because later semantic import references are not added yet. If this task is run alone, it should compile.

## Task 2: Create Semantic Import Module

**Files:**
- Create: `src/semantic_import.rs`
- Modify: `src/lib.rs`
- Test: `tests/semantic_import.rs`

- [ ] **Step 1: Export module**

Add to `src/lib.rs`:

```rust
pub mod semantic_import;
```

- [ ] **Step 2: Define input and report structs**

Create `src/semantic_import.rs` with these public shapes:

```rust
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::domain::{Relation, SubjectAlias};
use crate::error::{Error, Result};
use crate::ids;
use crate::store::Store;

const RELATION_TYPES: &[&str] = &[
    "uses",
    "owns",
    "prefers",
    "works_on",
    "depends_on",
    "supersedes",
    "blocked_by",
];

#[derive(Debug, Clone, Deserialize)]
pub struct SemanticImportRequest {
    pub profile_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub aliases: Vec<SemanticAliasInput>,
    #[serde(default)]
    pub relations: Vec<SemanticRelationInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SemanticAliasInput {
    pub subject_id: String,
    pub alias_key: String,
    pub source_evidence: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SemanticRelationInput {
    pub from_subject_id: String,
    pub relation_type: String,
    pub to_subject_id: String,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    pub source_episode_ids: Vec<String>,
    pub source_evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticImportReport {
    pub profile_id: String,
    pub workspace_id: String,
    pub would_apply: Vec<SemanticImportReportEntry>,
    pub applied: Vec<SemanticImportReportEntry>,
    pub already_present: Vec<SemanticImportReportEntry>,
    pub rejected: Vec<SemanticImportReportEntry>,
    pub counts: SemanticImportCounts,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticImportReportEntry {
    pub kind: String,
    pub key: String,
    pub status: String,
    pub reason: Option<String>,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticImportCounts {
    pub would_apply: usize,
    pub applied_aliases: usize,
    pub applied_relations: usize,
    pub already_present: usize,
    pub rejected: usize,
}

fn default_confidence() -> f64 {
    1.0
}
```

- [ ] **Step 3: Add preview/apply entrypoints**

Implement:

```rust
pub fn preview_semantic_import(
    store: &Store,
    request: &SemanticImportRequest,
) -> Result<SemanticImportReport> {
    validate_request_shape(request)?;
    let mut report = empty_report(request);
    validate_aliases(store, request, &mut report)?;
    validate_relations(store, request, &mut report)?;
    refresh_counts(&mut report);
    Ok(report)
}

pub fn apply_semantic_import(
    store: &Store,
    request: &SemanticImportRequest,
) -> Result<SemanticImportReport> {
    let preview = preview_semantic_import(store, request)?;
    let mut report = empty_report(request);
    report.already_present = preview.already_present;
    report.rejected = preview.rejected;

    for entry in preview.would_apply {
        match entry.kind.as_str() {
            "alias" => {
                let alias: SubjectAlias = serde_json::from_value(entry.value.clone())?;
                let (stored, inserted) = store.insert_or_get_subject_alias(&alias)?;
                let mut applied = report_entry_for_alias(&stored, "applied", None);
                if !inserted {
                    applied.status = "already_present".to_string();
                    report.already_present.push(applied);
                } else {
                    report.applied.push(applied);
                }
            }
            "relation" => {
                let relation: Relation = serde_json::from_value(entry.value.clone())?;
                let (stored, inserted) = store.insert_or_get_relation(&relation)?;
                let mut applied = report_entry_for_relation(&stored, "applied", None);
                if !inserted {
                    applied.status = "already_present".to_string();
                    report.already_present.push(applied);
                } else {
                    report.applied.push(applied);
                }
            }
            _ => {}
        }
    }

    refresh_counts(&mut report);
    Ok(report)
}
```

- [ ] **Step 4: Add validation helpers**

Implement helpers with these exact reason strings:

- `missing_items`
- `missing_profile`
- `missing_workspace`
- `missing_alias_subject`
- `missing_alias_key`
- `missing_alias_evidence`
- `alias_subject_out_of_scope`
- `alias_conflict`
- `missing_relation_from_subject`
- `missing_relation_to_subject`
- `missing_relation_type`
- `unknown_relation_type`
- `invalid_relation_confidence`
- `missing_relation_episode_evidence`
- `missing_relation_source_evidence`
- `relation_endpoint_out_of_scope`

Core behavior:

```rust
fn validate_request_shape(request: &SemanticImportRequest) -> Result<()> {
    if request.profile_id.trim().is_empty() {
        return Err(Error::invalid_request("missing_profile"));
    }
    if request.workspace_id.trim().is_empty() {
        return Err(Error::invalid_request("missing_workspace"));
    }
    if request.aliases.is_empty() && request.relations.is_empty() {
        return Err(Error::invalid_request("missing_items"));
    }
    Ok(())
}
```

Alias validation must:

- reject empty fields into `report.rejected`
- call `store.subject_exists_in_scope`
- call `store.get_subject_alias`
- mark same subject duplicate as `already_present`
- mark different subject duplicate as `rejected` with `alias_conflict`
- create `SubjectAlias` values with `id: ids::new_id("alias")`, `created_at: ids::now_rfc3339()`, and metadata `{"origin":"semantic_import"}`

Relation validation must:

- reject empty fields into `report.rejected`
- reject relation types not in `RELATION_TYPES`
- reject confidence outside `0.0..=1.0`
- require non-empty `source_episode_ids`
- require non-empty `source_evidence`
- call `store.subject_exists_in_scope` for both endpoints
- call `store.get_active_relation`
- mark duplicates as `already_present`
- create `Relation` values with `id: ids::new_id("relation")`, `state: "active"`, `created_at: ids::now_rfc3339()`, `retired_at: None`, and metadata `{"origin":"semantic_import"}`

- [ ] **Step 5: Run compile**

Run:

```bash
rtk cargo check
```

Expected: compiles.

## Task 3: Add Semantic Import Tests

**Files:**
- Create: `tests/semantic_import.rs`

- [ ] **Step 1: Add test helpers**

Create helpers mirroring `tests/semantic_relations.rs`:

```rust
use codex_memoryd::domain::{Subject, SubjectKind};
use codex_memoryd::ids;
use codex_memoryd::semantic_import::{
    apply_semantic_import, preview_semantic_import, SemanticAliasInput, SemanticImportRequest,
    SemanticRelationInput,
};
use codex_memoryd::store::Store;
use serde_json::json;

fn subject(id: &str, profile: &str, workspace: &str, key: &str) -> Subject {
    Subject {
        id: id.to_string(),
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        subject_key: key.to_string(),
        kind: SubjectKind::Project,
        display_name: key.to_string(),
        created_at: ids::now_rfc3339(),
        updated_at: ids::now_rfc3339(),
        metadata: json!({}),
    }
}

fn seeded_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .insert_or_get_subject(&subject("subj_alice", "personal", "semantic-ws", "alice"))
        .unwrap();
    store
        .insert_or_get_subject(&subject("subj_billing", "personal", "semantic-ws", "billing"))
        .unwrap();
    store
        .insert_or_get_subject(&subject("subj_work", "work", "semantic-ws", "work-secret"))
        .unwrap();
    store
}

fn valid_request() -> SemanticImportRequest {
    SemanticImportRequest {
        profile_id: "personal".to_string(),
        workspace_id: "semantic-ws".to_string(),
        aliases: vec![SemanticAliasInput {
            subject_id: "subj_alice".to_string(),
            alias_key: "al".to_string(),
            source_evidence: "episode:ep_alias".to_string(),
        }],
        relations: vec![SemanticRelationInput {
            from_subject_id: "subj_alice".to_string(),
            relation_type: "owns".to_string(),
            to_subject_id: "subj_billing".to_string(),
            confidence: 0.92,
            source_episode_ids: vec!["episode:ep_owns".to_string()],
            source_evidence: Some("episode:ep_owns".to_string()),
        }],
    }
}

fn rejected_reasons(report: &codex_memoryd::semantic_import::SemanticImportReport) -> Vec<String> {
    report
        .rejected
        .iter()
        .filter_map(|entry| entry.reason.clone())
        .collect()
}
```

- [ ] **Step 2: Test preview is no-write**

Add:

```rust
#[test]
fn preview_valid_semantic_import_does_not_write() {
    let store = seeded_store();
    let req = valid_request();

    let report = preview_semantic_import(&store, &req).unwrap();

    assert_eq!(report.counts.would_apply, 2);
    assert_eq!(report.counts.rejected, 0);
    assert!(store
        .resolve_subject_alias("personal", "semantic-ws", "al")
        .unwrap()
        .is_none());
    assert!(store
        .relation_expanded_subjects("personal", "semantic-ws", &["subj_alice".to_string()], 1)
        .unwrap()
        .is_empty());
}
```

- [ ] **Step 3: Test apply writes and duplicate apply is idempotent**

Add:

```rust
#[test]
fn apply_valid_semantic_import_writes_and_is_idempotent() {
    let store = seeded_store();
    let req = valid_request();

    let first = apply_semantic_import(&store, &req).unwrap();
    assert_eq!(first.counts.applied_aliases, 1);
    assert_eq!(first.counts.applied_relations, 1);
    assert_eq!(first.counts.rejected, 0);

    let alias = store
        .resolve_subject_alias("personal", "semantic-ws", "al")
        .unwrap()
        .expect("alias resolves after apply");
    assert_eq!(alias.id, "subj_alice");

    let expanded = store
        .relation_expanded_subjects("personal", "semantic-ws", &["subj_alice".to_string()], 1)
        .unwrap();
    assert_eq!(expanded.len(), 1);
    assert_eq!(expanded[0].subject_id, "subj_billing");

    let second = apply_semantic_import(&store, &req).unwrap();
    assert_eq!(second.counts.applied_aliases, 0);
    assert_eq!(second.counts.applied_relations, 0);
    assert_eq!(second.counts.already_present, 2);
}
```

- [ ] **Step 4: Test missing evidence and unknown relation type rejections**

Add:

```rust
#[test]
fn rejects_missing_evidence_and_unknown_relation_type() {
    let store = seeded_store();
    let mut req = valid_request();
    req.aliases[0].source_evidence = "".to_string();
    req.relations[0].relation_type = "invented".to_string();
    req.relations[0].source_episode_ids.clear();
    req.relations[0].source_evidence = None;

    let report = preview_semantic_import(&store, &req).unwrap();
    let reasons = rejected_reasons(&report);

    assert!(reasons.iter().any(|reason| reason == "missing_alias_evidence"));
    assert!(reasons.iter().any(|reason| reason == "unknown_relation_type"));
    assert!(reasons
        .iter()
        .any(|reason| reason == "missing_relation_episode_evidence"));
    assert!(reasons
        .iter()
        .any(|reason| reason == "missing_relation_source_evidence"));
}
```

- [ ] **Step 5: Test cross-profile relation endpoint rejection**

Add:

```rust
#[test]
fn rejects_cross_profile_relation_endpoint() {
    let store = seeded_store();
    let mut req = valid_request();
    req.relations[0].to_subject_id = "subj_work".to_string();

    let report = preview_semantic_import(&store, &req).unwrap();
    let reasons = rejected_reasons(&report);

    assert!(reasons
        .iter()
        .any(|reason| reason == "relation_endpoint_out_of_scope"));
    assert_eq!(report.counts.rejected, 1);
}
```

- [ ] **Step 6: Test alias conflict rejection**

Add:

```rust
#[test]
fn rejects_alias_conflict() {
    let store = seeded_store();
    apply_semantic_import(&store, &valid_request()).unwrap();

    let mut conflict = valid_request();
    conflict.aliases[0].subject_id = "subj_billing".to_string();
    conflict.relations.clear();

    let report = preview_semantic_import(&store, &conflict).unwrap();
    let reasons = rejected_reasons(&report);

    assert!(reasons.iter().any(|reason| reason == "alias_conflict"));
    assert_eq!(report.counts.rejected, 1);
}
```

- [ ] **Step 7: Run semantic import tests**

Run:

```bash
rtk test cargo test semantic_import
```

Expected: all tests pass.

## Task 4: Wire CLI Commands

**Files:**
- Modify: `src/cli.rs`
- Test: `tests/cli_smoke.rs`

- [ ] **Step 1: Add imports**

Add:

```rust
use codex_memoryd::semantic_import;
use codex_memoryd::semantic_import::SemanticImportRequest;
```

- [ ] **Step 2: Add top-level command**

Add to `Command`:

```rust
/// Preview or apply reviewed semantic aliases and relations.
Semantic {
    #[command(subcommand)]
    command: SemanticCommand,
},
```

Add enum near other command enums:

```rust
#[derive(Subcommand, Debug)]
pub enum SemanticCommand {
    /// Validate semantic import JSON without writing.
    Preview {
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
    },
    /// Apply reviewed semantic import JSON.
    Apply {
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
    },
}
```

- [ ] **Step 3: Add file loader helper**

Add near other CLI helpers:

```rust
fn read_semantic_import_file(path: &PathBuf) -> Result<SemanticImportRequest> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        error::Error::invalid_request(format!(
            "failed to read semantic import file {}: {e}",
            path.display()
        ))
    })?;
    serde_json::from_str(&raw).map_err(|e| {
        error::Error::invalid_request(format!(
            "failed to parse semantic import file {}: {e}",
            path.display()
        ))
    })
}
```

- [ ] **Step 4: Add command match arm**

Add in `run` match:

```rust
Command::Semantic { command } => {
    let service = cli.open_service(None)?;
    match command {
        SemanticCommand::Preview { file } => {
            let req = read_semantic_import_file(file)?;
            let report = semantic_import::preview_semantic_import(&service.store, &req)?;
            print_json(&report)?;
        }
        SemanticCommand::Apply { file } => {
            let req = read_semantic_import_file(file)?;
            let report = semantic_import::apply_semantic_import(&service.store, &req)?;
            print_json(&report)?;
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Run CLI compile**

Run:

```bash
rtk cargo check
```

Expected: compiles.

## Task 5: Add CLI Smoke Coverage

**Files:**
- Modify: `tests/cli_smoke.rs`

- [ ] **Step 1: Add helper to write JSON file**

Use `std::fs::write` inside the test body; no shared helper needed unless existing file helpers are nearby.

- [ ] **Step 2: Add smoke test**

Add a focused test:

```rust
#[test]
fn semantic_preview_and_apply_json_import() {
    let tmp = TempDir::new().unwrap();
    let db = db_path(&tmp);

    bin()
        .args([
            "--db",
            db.to_str().unwrap(),
            "subject",
            "create",
            "--profile",
            "personal",
            "--workspace",
            "semantic-ws",
            "--key",
            "alice",
            "--kind",
            "person",
            "--display-name",
            "Alice",
        ])
        .assert()
        .success();

    bin()
        .args([
            "--db",
            db.to_str().unwrap(),
            "subject",
            "create",
            "--profile",
            "personal",
            "--workspace",
            "semantic-ws",
            "--key",
            "billing",
            "--kind",
            "project",
            "--display-name",
            "Billing",
        ])
        .assert()
        .success();

    let import_path = tmp.path().join("semantic.json");
    std::fs::write(
        &import_path,
        r#"{
          "profile_id":"personal",
          "workspace_id":"semantic-ws",
          "aliases":[{"subject_id":"subj_personal_semantic_ws_alice","alias_key":"al","source_evidence":"episode:ep_alias"}],
          "relations":[{"from_subject_id":"subj_personal_semantic_ws_alice","relation_type":"owns","to_subject_id":"subj_personal_semantic_ws_billing","source_episode_ids":["episode:ep_owns"],"source_evidence":"episode:ep_owns"}]
        }"#,
    )
    .unwrap();

    bin()
        .args([
            "--db",
            db.to_str().unwrap(),
            "semantic",
            "preview",
            "--file",
            import_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"would_apply\""))
        .stdout(predicate::str::contains("\"rejected\":0"));

    bin()
        .args([
            "--db",
            db.to_str().unwrap(),
            "semantic",
            "apply",
            "--file",
            import_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"applied_aliases\":1"))
        .stdout(predicate::str::contains("\"applied_relations\":1"));
}
```

If actual subject IDs generated by `subject create` differ from the example, parse the JSON command output to get IDs before writing `semantic.json`.

- [ ] **Step 3: Run CLI smoke target**

Run:

```bash
rtk test cargo test semantic_preview_and_apply_json_import
```

Expected: pass.

## Task 6: Update Docs

**Files:**
- Modify: `docs/semantic-layer.md`
- Modify: `README.md`

- [ ] **Step 1: Update semantic-layer status**

Change the top status note from pending user-facing UX to:

```markdown
> **Status: MVP IMPLEMENTED (relation substrate, eval slice, and explicit JSON preview/apply path).**
```

Then add a section:

```markdown
## Explicit semantic import

Reviewed aliases and relations can be previewed and applied from a JSON file:

```bash
codex-memoryd semantic preview --file semantic.json
codex-memoryd semantic apply --file semantic.json
```

Preview performs no writes. Apply validates the same rules, writes valid reviewed entries, reports rejected entries, and remains idempotent. Relations are evidence-backed discovery aids; they are not authoritative recall facts.
```

- [ ] **Step 2: Add README pointer**

If README has a docs index, add or update one line pointing to `docs/semantic-layer.md` as the semantic alias/relation import reference.

- [ ] **Step 3: Run docs diff check**

Run:

```bash
rtk git diff --check
```

Expected: no whitespace errors.

## Task 7: Final Verification and Commit

**Files:**
- All changed files

- [ ] **Step 1: Run formatting**

Run:

```bash
rtk cargo fmt --all --check
```

Expected: pass. If it fails on formatting, run `cargo fmt --all`, then re-run check.

- [ ] **Step 2: Run compile**

Run:

```bash
rtk cargo check
```

Expected: pass.

- [ ] **Step 3: Run targeted tests**

Run:

```bash
rtk test cargo test semantic_import
rtk test cargo test semantic_relations
rtk test cargo test semantic_preview_and_apply_json_import
rtk test cargo test retrieval_eval_relation_aware
```

Expected: pass. If exact eval test name differs, run the nearest relation-aware eval smoke shown by `rtk grep -n "relation_aware_recall" tests/cli_smoke.rs src/retrieval_eval.rs`.

- [ ] **Step 4: Run diff check**

Run:

```bash
rtk git diff --check
```

Expected: pass.

- [ ] **Step 5: Commit**

Stage only intended files, not `.serena/`:

```bash
rtk git add src/semantic_import.rs src/lib.rs src/store.rs src/cli.rs tests/semantic_import.rs tests/cli_smoke.rs docs/semantic-layer.md README.md docs/superpowers/plans/2026-06-16-semantic-import-154.md
rtk git commit -m "feat: add semantic import preview apply"
```

Expected: commit succeeds. PR body should use `Closes #154` only if the issue closure standard in the design spec is satisfied.
