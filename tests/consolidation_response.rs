use codex_memoryd::config::Config;
use codex_memoryd::protocol::{ConclusionsRequest, ConsolidationUndoRequest};
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;

#[test]
fn automatic_response_returns_batch_id_usable_for_undo() {
    let store = Store::open(":memory:").expect("store");
    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "ws".into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.automatic_apply = true;
    config.dream_scheduler.scheduled_provider_enabled = false;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_scheduler.min_turn_count = 0;
    let service = Service::new(store, config);
    service
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("ws".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec!["Decision: use concise summaries".into()]),
            metadata: None,
            record_type: Some("decision".into()),
        })
        .expect("source conclusion");

    let scheduled = service
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .expect("scheduled adoption");
    let run = scheduled.run.expect("scheduled run");
    let response = serde_json::to_value(&run).expect("response json");
    let batch_id = response["consolidation_batch_id"]
        .as_str()
        .expect("canonical batch ID in response")
        .to_string();

    let undo = service
        .undo_stored_consolidation(ConsolidationUndoRequest {
            batch_id: batch_id.clone(),
        })
        .expect("undo through response batch ID");
    assert_eq!(undo.batch_id, batch_id);
}
