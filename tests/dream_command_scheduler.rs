use codex_memoryd::protocol::ConclusionsRequest;
use codex_memoryd::{config::Config, service::Service, store::Store};
use serde_json::json;

fn service(script: &str) -> (Service, Store) {
    let store = Store::open(":memory:").unwrap();
    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "ws".into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.scheduled_provider_enabled = true;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_provider.enabled = true;
    config.dream_provider.adapter = "command".into();
    config.dream_provider.model = "synthetic-model".into();
    config.dream_provider.command = vec!["/bin/sh".into(), "-c".into(), script.into()];
    let svc = Service::new(store.clone(), config);
    let req: ConclusionsRequest = serde_json::from_value(json!({
        "profile":"personal", "workspace":"ws", "target":"user",
        "conclusions":["Preference: concise summaries"]
    }))
    .unwrap();
    svc.conclusions(req).unwrap();
    (svc, store)
}

#[cfg(target_os = "linux")]
#[test]
fn scheduled_command_uses_typed_preview_without_promoting_memory() {
    let (svc, _) = service(
        r#"cat >/dev/null; printf '{"schema_version":"dream-preview-v1","profile":"personal","workspace":"ws","candidates":[]}'"#,
    );
    let result = svc.scheduled_dream(None).unwrap();
    assert_eq!(result.status, "ok");
    let preview = result.run.unwrap();
    assert!(preview.created.is_empty());
    assert!(preview.archived.is_empty());
    assert_eq!(
        serde_json::to_value(preview).unwrap()["provenance"]["adapter"],
        "command"
    );
    assert!(result.watermark_after.is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn failed_scheduled_command_does_not_advance_success_watermark() {
    let (svc, store) = service("cat >/dev/null; exit 7");
    let error = svc.scheduled_dream(None).unwrap_err();
    assert!(error.message.contains("provider command"));
    assert!(store
        .scheduled_dream_watermark("personal", "ws", None)
        .unwrap()
        .is_none());
}

#[test]
fn deterministic_automatic_schedule_uses_governed_apply_boundary() {
    let store = Store::open(":memory:").unwrap();
    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "ws".into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.automatic_apply = true;
    config.dream_scheduler.scheduled_provider_enabled = false;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_scheduler.min_turn_count = 0;
    let svc = codex_memoryd::service::Service::new(store.clone(), config);
    svc.conclusions(codex_memoryd::protocol::ConclusionsRequest {
        profile: Some("personal".into()),
        workspace: Some("ws".into()),
        repo: None,
        target: Some("user".into()),
        conclusions: Some(vec!["Decision: use concise summaries".into()]),
        metadata: None,
        record_type: Some("decision".into()),
    })
    .unwrap();
    let result = svc
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .unwrap();
    assert_eq!(result.status, "ok");
    let run = result.run.unwrap();
    assert_eq!(run.mode, "apply");
    assert_eq!(run.created.len(), 1);
    assert!(store.get_record(&run.created[0]).unwrap().is_some());
    let control = svc
        .apply_stored_consolidation(codex_memoryd::protocol::ConsolidationApplyRequest {
            batch_id: format!("consolidation_{}", run.run_id),
        })
        .unwrap();
    assert_eq!(control.status, "applied");
    assert_eq!(control.record_ids, run.created);
}
