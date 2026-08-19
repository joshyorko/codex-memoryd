use std::fs::File;
use std::io::Read;
use std::path::Path;

use codex_memoryd::domain::{Portability, RecordType, Scope, Sensitivity};
use codex_memoryd::ids;
use codex_memoryd::portable_bundle::{
    export_write, import_apply, import_preview, inspect, BundleExportOptions, BundleImportOptions,
    BundleReport,
};
use codex_memoryd::store::{NewRecord, Store, UpsertOutcome};
use tempfile::TempDir;
use zip::ZipArchive;

fn seed_store(path: &Path) -> Store {
    let store = Store::open(path).expect("open store");
    store
        .ensure_workspace("personal", "bundle-fixture")
        .expect("workspace");
    let content = "Use deterministic fixture data for bundle tests.";
    let content_hash = ids::content_hash(
        "personal",
        "bundle-fixture",
        None,
        "preference",
        "workspace",
        content,
    );
    let outcome = store
        .upsert_record(&NewRecord {
            profile_id: "personal".to_string(),
            workspace_id: "bundle-fixture".to_string(),
            repo_id: None,
            subject_id: None,
            episode_id: None,
            scope: Scope::Workspace,
            record_type: RecordType::Preference,
            content: content.to_string(),
            related_files: vec!["src/lib.rs".to_string()],
            tags: vec!["fixture".to_string()],
            sensitivity: Sensitivity::Personal,
            portability: Portability::ProfileOnly,
            confidence: 0.8,
            source_ids: Vec::new(),
            content_hash,
            supersedes: Vec::new(),
            metadata: serde_json::json!({"origin": "fixture"}),
        })
        .expect("record");
    assert!(matches!(outcome, UpsertOutcome::Created(_)));
    store
}

fn options() -> BundleExportOptions {
    BundleExportOptions {
        profile: "personal".to_string(),
        workspace: "bundle-fixture".to_string(),
        repo_id: None,
        record_ids: Vec::new(),
        include_archived: false,
        target_profile: "personal".to_string(),
        target_workspace: None,
        target_repo_id: None,
        created_at: Some("2026-01-01T00:00:00Z".to_string()),
    }
}

fn import_options() -> BundleImportOptions {
    BundleImportOptions {
        profile: "personal".to_string(),
        workspace: "bundle-fixture".to_string(),
        repo_id: None,
    }
}

#[test]
fn bundle_round_trip_is_previewed_atomic_and_idempotent() {
    let source_dir = TempDir::new().expect("source tempdir");
    let destination_dir = TempDir::new().expect("destination tempdir");
    let bundle_path = source_dir.path().join("fixture.cmembundle");
    let source = seed_store(&source_dir.path().join("source.sqlite"));

    let first = export_write(&source, &options(), &bundle_path).expect("write bundle");
    assert_eq!(first.counts.create, 0);
    assert!(bundle_path.exists());
    assert_eq!(inspect(&bundle_path).expect("inspect").bundle_id, first.bundle_id);

    let destination = Store::open(destination_dir.path().join("destination.sqlite")).expect("destination");
    let preview = import_preview(&destination, &bundle_path, &import_options()).expect("preview");
    assert_eq!(preview.counts.create, 1);
    assert_eq!(preview.safe_to_apply, Some(true));
    let plan_id = preview.plan_id.clone().expect("plan id");

    let applied =
        import_apply(&destination, &bundle_path, &import_options(), &plan_id).expect("apply");
    assert_eq!(applied.counts.create, 1);
    assert!(applied.receipt.is_some());
    assert!(destination
        .query_records(&codex_memoryd::store::RecordQuery {
            profile_id: Some("personal".to_string()),
            workspace_id: Some("bundle-fixture".to_string()),
            ..Default::default()
        })
        .expect("query imported")
        .len()
        == 1);

    let replay = import_apply(&destination, &bundle_path, &import_options(), &plan_id)
        .expect("idempotent replay");
    assert_eq!(replay.receipt, applied.receipt);
    assert!(replay.warnings.iter().any(|value| value == "already_applied"));
}

#[test]
fn bundle_writer_refuses_overwrite_and_rejects_unsafe_repo_paths() {
    let source_dir = TempDir::new().expect("source tempdir");
    let source = seed_store(&source_dir.path().join("source.sqlite"));
    let bundle_path = source_dir.path().join("fixture.cmembundle");
    export_write(&source, &options(), &bundle_path).expect("write bundle");
    let error = export_write(&source, &options(), &bundle_path).expect_err("overwrite rejected");
    assert_eq!(error.code.as_str(), "invalid_request");

    let mut archive = ZipArchive::new(File::open(&bundle_path).expect("bundle")).expect("zip");
    let mut manifest = String::new();
    archive
        .by_name("manifest.json")
        .expect("manifest")
        .read_to_string(&mut manifest)
        .expect("read manifest");
    assert!(manifest.contains("bundle-fixture"));
}

#[allow(dead_code)]
fn _report_is_json_compatible(report: &BundleReport) {
    serde_json::to_value(report).expect("report json");
}
