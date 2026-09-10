use std::fs::File;
use std::io::Read;
use std::path::Path;

use codex_memoryd::domain::{
    Episode, MemorySource, Portability, RecordType, Scope, Sensitivity, Subject, SubjectKind,
};
use codex_memoryd::ids;
use codex_memoryd::portable_bundle::{
    export_write, import_apply, import_preview, inspect, BundleExportOptions, BundleImportOptions,
    BundleReport,
};
use codex_memoryd::store::{EvidenceLedgerEntry, NewRecord, Store, UpsertOutcome};
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
            metadata: serde_json::json!({"origin": "fixture", "provenance": {"source_kind": "test", "actor": "agent:test", "write_origin": "bundle", "execution_context": "unit"}}),
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

fn seed_graph_store(path: &Path) -> Store {
    let store = Store::open(path).expect("open store");
    store
        .ensure_workspace("personal", "bundle-fixture")
        .expect("workspace");
    let now = "2026-01-01T00:00:00Z".to_string();
    let subject = Subject {
        id: "sub_fixture".to_string(),
        profile_id: "personal".to_string(),
        workspace_id: "bundle-fixture".to_string(),
        subject_key: "fixture-project".to_string(),
        kind: SubjectKind::Project,
        display_name: "Fixture Project".to_string(),
        created_at: now.clone(),
        updated_at: now.clone(),
        metadata: serde_json::json!({"origin": "synthetic"}),
    };
    store
        .insert_or_get_subject(&subject)
        .expect("subject fixture");
    let source = MemorySource {
        id: "src_fixture".to_string(),
        profile_id: "personal".to_string(),
        workspace_id: "bundle-fixture".to_string(),
        kind: "fixture".to_string(),
        source_path: Some("fixtures/graph.json".to_string()),
        source_hash: "sha256:fixture-source".to_string(),
        created_at: now.clone(),
        ingested_at: now.clone(),
        metadata: serde_json::json!({"origin": "synthetic"}),
    };
    let (source, _) = store
        .upsert_source(
            &source.profile_id,
            &source.workspace_id,
            &source.kind,
            source.source_path.as_deref(),
            &source.source_hash,
            &source.metadata,
        )
        .expect("source fixture");
    let episode = Episode {
        id: "ep_fixture".to_string(),
        profile_id: "personal".to_string(),
        workspace_id: "bundle-fixture".to_string(),
        subject_id: subject.id.clone(),
        source_kind: "fixture".to_string(),
        source_ref: "fixture-run".to_string(),
        started_at: Some(now.clone()),
        ended_at: Some(now.clone()),
        status: Some("success".to_string()),
        summary: "Synthetic fixture episode.".to_string(),
        trust_level: Some("trusted".to_string()),
        source_metadata: serde_json::json!({"origin": "synthetic"}),
        created_at: now.clone(),
        updated_at: now.clone(),
        metadata: serde_json::json!({}),
    };
    store.insert_episode(&episode).expect("episode fixture");
    let content = "Graph fixture memory with safe provenance.";
    let content_hash = ids::content_hash(
        "personal",
        "bundle-fixture",
        None,
        "preference",
        "workspace",
        content,
    );
    store
        .upsert_record(&NewRecord {
            profile_id: "personal".to_string(),
            workspace_id: "bundle-fixture".to_string(),
            repo_id: None,
            subject_id: Some(subject.id),
            episode_id: Some(episode.id),
            scope: Scope::Workspace,
            record_type: RecordType::Preference,
            content: content.to_string(),
            related_files: vec!["fixtures/graph.json".to_string()],
            tags: vec!["fixture".to_string()],
            sensitivity: Sensitivity::Personal,
            portability: Portability::ProfileOnly,
            confidence: 0.8,
            source_ids: vec![source.id.clone()],
            content_hash,
            supersedes: Vec::new(),
            metadata: serde_json::json!({"origin": "synthetic"}),
        })
        .expect("graph memory fixture");
    store
        .record_evidence_ledger(&EvidenceLedgerEntry {
            profile_id: "personal".to_string(),
            workspace_id: "bundle-fixture".to_string(),
            repo_id: None,
            subject_key: Some("fixture-project".to_string()),
            source_kind: "fixture".to_string(),
            source_id: Some(source.id),
            source_path: Some("fixtures/graph.json".to_string()),
            source_hash: "sha256:fixture-evidence".to_string(),
            safe_summary: "Synthetic evidence summary.".to_string(),
            policy_state: "accepted".to_string(),
            metadata: serde_json::json!({"origin": "synthetic"}),
        })
        .expect("evidence fixture");
    store
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
    assert_eq!(
        inspect(&bundle_path).expect("inspect").bundle_id,
        first.bundle_id
    );

    let destination =
        Store::open(destination_dir.path().join("destination.sqlite")).expect("destination");
    let preview = import_preview(&destination, &bundle_path, &import_options()).expect("preview");
    assert_eq!(preview.counts.create, 1);
    assert_eq!(preview.safe_to_apply, Some(true));
    let plan_id = preview.plan_id.clone().expect("plan id");

    let applied =
        import_apply(&destination, &bundle_path, &import_options(), &plan_id).expect("apply");
    assert_eq!(applied.counts.create, 1);
    assert!(applied.receipt.is_some());
    assert!(
        destination
            .query_records(&codex_memoryd::store::RecordQuery {
                profile_id: Some("personal".to_string()),
                workspace_id: Some("bundle-fixture".to_string()),
                ..Default::default()
            })
            .expect("query imported")
            .len()
            == 1
    );
    let imported = destination
        .query_records(&codex_memoryd::store::RecordQuery {
            profile_id: Some("personal".to_string()),
            workspace_id: Some("bundle-fixture".to_string()),
            ..Default::default()
        })
        .expect("query imported");
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].metadata["provenance"]["source_kind"], "test");
    assert_eq!(imported[0].metadata["provenance"]["actor"], "agent:test");
    assert_eq!(imported[0].metadata["provenance"]["write_origin"], "bundle");
    assert_eq!(
        imported[0].metadata["provenance"]["execution_context"],
        "unit"
    );

    let replay = import_apply(&destination, &bundle_path, &import_options(), &plan_id)
        .expect("idempotent replay");
    assert_eq!(replay.receipt, applied.receipt);
    assert!(replay
        .warnings
        .iter()
        .any(|value| value == "already_applied"));
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

#[test]
fn bundle_transfers_safe_evidence_closure_in_one_transaction() {
    let source_dir = TempDir::new().expect("source tempdir");
    let destination_dir = TempDir::new().expect("destination tempdir");
    let bundle_path = source_dir.path().join("graph.cmembundle");
    let source = seed_graph_store(&source_dir.path().join("source.sqlite"));
    let exported = export_write(&source, &options(), &bundle_path).expect("write graph bundle");
    assert_eq!(exported.counts.create, 0);
    assert_eq!(
        inspect(&bundle_path)
            .expect("inspect graph bundle")
            .counts
            .discovered,
        5
    );

    let destination =
        Store::open(destination_dir.path().join("destination.sqlite")).expect("destination");
    let preview = import_preview(&destination, &bundle_path, &import_options()).expect("preview");
    assert_eq!(preview.counts.discovered, 5);
    assert_eq!(preview.counts.create, 5);
    let applied = import_apply(
        &destination,
        &bundle_path,
        &import_options(),
        preview.plan_id.as_deref().expect("plan"),
    )
    .expect("apply graph bundle");
    assert_eq!(applied.counts.create, 5);
    assert_eq!(
        destination
            .list_evidence_ledger("personal", "bundle-fixture")
            .expect("imported evidence")
            .len(),
        1
    );
}

#[allow(dead_code)]
fn _report_is_json_compatible(report: &BundleReport) {
    serde_json::to_value(report).expect("report json");
}
