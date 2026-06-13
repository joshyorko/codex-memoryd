//! Contract tests over the shared integration fixtures in `tests/fixtures/`.
//!
//! These guarantee the documented Codex ⇄ codex-memoryd wire shapes
//! (docs/codex-integration.md) actually deserialize into the protocol request
//! types and drive the service to the documented outcomes. The Codex side can
//! serialize its own payloads and diff them against the same fixtures to stay
//! aligned with this provider.

use std::path::PathBuf;

use codex_memoryd::config::Config;
use codex_memoryd::domain::VisibleTurn;
use codex_memoryd::protocol::*;
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;
use serde_json::json;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read_fixture(name: &str) -> String {
    let path = fixtures_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

fn service() -> Service {
    let store = Store::open(":memory:").expect("open store");
    let config = Config {
        default_workspace: "josh-personal".to_string(),
        ..Default::default()
    };
    Service::new(store, config)
}

#[test]
fn recall_fixture_deserializes_and_runs() {
    let raw = read_fixture("recall.request.json");
    let req: RecallRequest = serde_json::from_str(&raw).expect("recall fixture must deserialize");
    assert_eq!(req.profile.as_deref(), Some("personal"));
    assert_eq!(req.workspace.as_deref(), Some("josh-personal"));
    assert_eq!(req.max_tokens, Some(1200));
    // It must drive the service without error (empty store → empty facts).
    let svc = service();
    let resp = svc.recall(req).expect("recall runs");
    assert_eq!(resp.authority, "recall_not_authority");
    assert_eq!(resp.policy.authority, "recall_not_authority");
    assert!(resp
        .policy
        .admission_gates
        .contains(&"profile_workspace".to_string()));
    assert!(resp
        .policy
        .ranking_signals
        .contains(&"repo_match".to_string()));
}

#[test]
fn turns_fixture_deserializes_and_runs() {
    let raw = read_fixture("turns.request.json");
    let req: TurnsRequest = serde_json::from_str(&raw).expect("turns fixture must deserialize");
    let session = req.session.as_ref().expect("session present");
    assert_eq!(session.id.as_deref(), Some("thread-123"));
    assert_eq!(req.messages.as_ref().unwrap().len(), 2);

    let svc = service();
    let resp = svc.turns(req).expect("turns runs");
    assert_eq!(resp.accepted, 2, "both safe messages accepted");
    assert_eq!(resp.rejected, 0);
}

#[test]
fn sync_preview_fixture_writes_nothing() {
    let raw = read_fixture("sync_local.preview.request.json");
    let req: SyncRequest = serde_json::from_str(&raw).expect("sync preview fixture deserializes");
    assert_eq!(req.mode.as_deref(), Some("preview"));

    let svc = service();
    let resp = svc.sync_local(req).expect("sync preview runs");
    assert_eq!(resp.mode, "preview");
    assert!(resp.proposed > 0);
    assert_eq!(resp.created, 0);
    assert_eq!(svc.store.count_records().unwrap(), 0);
}

#[test]
fn sync_apply_fixture_writes_and_is_idempotent() {
    let raw = read_fixture("sync_local.apply.request.json");
    let svc = service();

    // First apply writes records.
    let req: SyncRequest = serde_json::from_str(&raw).unwrap();
    let first = svc.sync_local(req).expect("apply runs");
    assert_eq!(first.mode, "apply");
    assert!(first.created >= 1, "apply must create records");
    let after_first = svc.store.count_records().unwrap();

    // Re-applying the identical fixture creates nothing new (idempotent).
    let req2: SyncRequest = serde_json::from_str(&raw).unwrap();
    let second = svc.sync_local(req2).expect("re-apply runs");
    assert_eq!(second.created, 0, "re-apply must be idempotent");
    assert_eq!(svc.store.count_records().unwrap(), after_first);

    // The second file omits `kind` and `hash`; the daemon must still ingest it
    // by inferring the kind from the path (rollout_summaries/...).
    assert!(after_first >= 2, "both files contributed records");
}

#[test]
fn status_response_fixture_matches_protocol_shape() {
    // The documented status response must round-trip through the live status
    // object, proving the fixture's field set matches what the daemon emits.
    let raw = read_fixture("status.response.json");
    let fixture: serde_json::Value =
        serde_json::from_str(&raw).expect("status fixture is valid JSON");

    let svc = service();
    let live = svc.status().expect("status");
    let live_json = serde_json::to_value(&live).expect("serialize status");

    // Every top-level key documented in the fixture's `data` must exist in the
    // live status payload (the fixture is the Codex-side parsing contract).
    let fixture_data = fixture["data"].as_object().expect("fixture data object");
    let live_obj = live_json.as_object().expect("live status object");
    for key in fixture_data.keys() {
        assert!(
            live_obj.contains_key(key),
            "status response is missing documented key `{key}`"
        );
    }
    // Spot-check stable identity fields.
    assert_eq!(live_json["provider_name"], "codex-memoryd");
    assert_eq!(live_json["api_version"], "v1");
    assert_eq!(live_json["storage_schema_version"], 4);
}

#[test]
fn dream_preview_report_fixture_matches_stable_shape() {
    let svc = service();
    svc.store
        .ensure_session("sess_fixture", "personal", "test", None, None, "test")
        .unwrap();
    svc.store
        .insert_visible_turn(&VisibleTurn {
            id: "turn_user_pref".to_string(),
            session_id: "sess_fixture".to_string(),
            actor: "user".to_string(),
            content: "Prefer cargo test for validation".to_string(),
            created_at: "2026-01-01T00:01:00Z".to_string(),
            metadata: json!({}),
        })
        .unwrap();

    let report = svc
        .dream(DreamRequest {
            profile: Some("personal".to_string()),
            workspace: Some("test".to_string()),
            repo: None,
            mode: Some("preview".to_string()),
            now: Some("2026-01-02T00:00:00Z".to_string()),
            since: None,
        })
        .expect("dream preview runs");
    let live = serde_json::to_value(report).expect("serialize report");
    let fixture: serde_json::Value = serde_json::from_str(&read_fixture(
        "dreaming/preview_user_preference.report.json",
    ))
    .expect("fixture JSON");
    assert_eq!(live, fixture);
}
