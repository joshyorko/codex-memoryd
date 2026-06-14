//! Operator backup / verify / restore workflow tests (issue #141).
//!
//! Exercises the full runbook against temporary databases: create a verified
//! backup with a manifest, verify it, detect tampering, preview a restore
//! without mutating the target, and apply a restore that takes a safety copy.

use std::fs;

use codex_memoryd::backup;
use codex_memoryd::domain::Portability;
use codex_memoryd::domain::RecordType;
use codex_memoryd::domain::Scope;
use codex_memoryd::domain::Sensitivity;
use codex_memoryd::ids;
use codex_memoryd::store::NewRecord;
use codex_memoryd::store::Store;
use tempfile::TempDir;

const NOW: &str = "2030-01-01T00:00:00Z";

fn seed_store(path: &std::path::Path, n: usize) -> Store {
    let store = Store::open(path).unwrap();
    store.ensure_workspace("personal", "default").unwrap();
    for i in 0..n {
        let content = format!("Durable decision number {i}.");
        let record = NewRecord {
            profile_id: "personal".to_string(),
            workspace_id: "default".to_string(),
            repo_id: None,
            subject_id: None,
            episode_id: None,
            scope: Scope::Workspace,
            record_type: RecordType::Decision,
            content: content.clone(),
            related_files: vec![],
            tags: vec![],
            sensitivity: Sensitivity::Personal,
            portability: Portability::Portable,
            confidence: 0.9,
            source_ids: vec![],
            content_hash: ids::content_hash(
                "personal",
                "default",
                None,
                "decision",
                "workspace",
                &content,
            ),
            supersedes: vec![],
            metadata: serde_json::json!({}),
        };
        store.upsert_record(&record).unwrap();
    }
    store
}

#[test]
fn create_then_verify_succeeds() {
    let dir = TempDir::new().unwrap();
    let src_path = dir.path().join("source.db");
    let store = seed_store(&src_path, 3);

    let dest = dir.path().join("backups").join("backup.db");
    let result = backup::create_backup(&store, &dest, NOW).unwrap();

    assert!(result.database_path.exists(), "backup db written");
    assert!(result.manifest_path.exists(), "manifest written");
    assert_eq!(
        result.manifest.expected_schema_version,
        result.manifest.schema_version.unwrap()
    );
    let mem = result
        .manifest
        .tables
        .iter()
        .find(|t| t.table == "memory_records")
        .unwrap();
    assert_eq!(mem.rows, 3, "manifest records the row count");

    let verify = backup::verify_backup(&dest).unwrap();
    assert!(verify.ok, "fresh backup verifies: {:?}", verify.issues);
    assert!(verify.digest_matches);
    assert!(verify.integrity_ok);
    assert!(verify.schema_matches);
}

#[test]
fn verify_detects_tampering() {
    let dir = TempDir::new().unwrap();
    let src_path = dir.path().join("source.db");
    let store = seed_store(&src_path, 2);
    let dest = dir.path().join("backup.db");
    backup::create_backup(&store, &dest, NOW).unwrap();

    // Corrupt the backup bytes after the manifest was written.
    let mut bytes = fs::read(&dest).unwrap();
    let len = bytes.len();
    bytes[len / 2] ^= 0xFF;
    fs::write(&dest, &bytes).unwrap();

    let verify = backup::verify_backup(&dest).unwrap();
    assert!(!verify.ok, "tampered backup must not verify");
    assert!(
        !verify.digest_matches || !verify.integrity_ok,
        "tamper detected via digest or integrity"
    );
    assert!(!verify.issues.is_empty(), "tamper reports an issue");
}

#[test]
fn restore_preview_does_not_mutate_target() {
    let dir = TempDir::new().unwrap();
    // Backup has 5 records.
    let backup_src = dir.path().join("backup_src.db");
    let backup_store = seed_store(&backup_src, 5);
    let backup_db = dir.path().join("backup.db");
    backup::create_backup(&backup_store, &backup_db, NOW).unwrap();
    drop(backup_store);

    // Target currently has 2 records.
    let target = dir.path().join("target.db");
    let _target_store = seed_store(&target, 2);

    let preview = backup::restore_preview(&backup_db, &target).unwrap();
    assert!(preview.target_exists);
    assert!(preview.safe_to_apply, "verified backup is safe to apply");
    let mem_delta = preview
        .table_deltas
        .iter()
        .find(|d| d.table == "memory_records")
        .unwrap();
    assert_eq!(mem_delta.current_rows, 2);
    assert_eq!(mem_delta.backup_rows, 5);
    assert_eq!(mem_delta.delta, 3);

    // The target must be untouched by preview.
    let after = Store::open(&target).unwrap();
    assert_eq!(after.count_records().unwrap(), 2, "preview did not write");
}

#[test]
fn restore_apply_replaces_target_and_keeps_safety_copy() {
    let dir = TempDir::new().unwrap();
    let backup_src = dir.path().join("backup_src.db");
    let backup_store = seed_store(&backup_src, 7);
    let backup_db = dir.path().join("backup.db");
    backup::create_backup(&backup_store, &backup_db, NOW).unwrap();
    drop(backup_store);

    let target = dir.path().join("target.db");
    let target_store = seed_store(&target, 1);
    // Restore requires exclusive access: an operator stops the daemon first.
    drop(target_store);

    let result = backup::restore_apply(&backup_db, &target, NOW).unwrap();
    assert!(result.restored);
    let prior = result.prior_backup.expect("safety copy taken");
    assert!(prior.exists(), "prior target preserved as safety copy");

    let restored = Store::open(&target).unwrap();
    assert_eq!(
        restored.count_records().unwrap(),
        7,
        "target now holds backup contents"
    );
    assert!(restored.integrity_ok().unwrap());
}

#[test]
fn restore_apply_refuses_tampered_backup() {
    let dir = TempDir::new().unwrap();
    let backup_src = dir.path().join("backup_src.db");
    let backup_store = seed_store(&backup_src, 3);
    let backup_db = dir.path().join("backup.db");
    backup::create_backup(&backup_store, &backup_db, NOW).unwrap();
    drop(backup_store);

    let mut bytes = fs::read(&backup_db).unwrap();
    let len = bytes.len();
    bytes[len / 2] ^= 0xFF;
    fs::write(&backup_db, &bytes).unwrap();

    let target = dir.path().join("target.db");
    let _target_store = seed_store(&target, 4);

    let err = backup::restore_apply(&backup_db, &target, NOW).unwrap_err();
    assert!(
        err.to_string().contains("verification"),
        "restore refused with verification error: {err}"
    );
    // Target unchanged.
    let after = Store::open(&target).unwrap();
    assert_eq!(after.count_records().unwrap(), 4);
}
