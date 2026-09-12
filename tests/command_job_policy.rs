use codex_memoryd::{config::Config, protocol::DreamJobRunRequest, service::Service, store::Store};
use serde_json::json;

#[test]
fn command_jobs_cannot_choose_a_different_model_endpoint_or_executable() {
    let mut config = Config::default();
    config.dream_provider.enabled = true;
    config.dream_provider.adapter = "command".into();
    config.dream_provider.model = "configured-model".into();
    config.dream_provider.command = vec!["/this-must-never-be-executed".into()];
    let svc = Service::new(Store::open(":memory:").unwrap(), config);
    for provider in [
        json!({"model":"other-model"}),
        json!({"endpoint":"https://example.invalid"}),
        json!({"command":{"argv":["other"]}}),
        json!({"provider":"spoofed-provider"}),
        json!({"adapter_version":"spoofed-version"}),
    ] {
        let request: DreamJobRunRequest = serde_json::from_value(json!({
            "profile":"personal","workspace":"ws","kind":"dream_preview","mode":"command",
            "budget":{"max_runtime_seconds":1,"max_input_records":5,"max_candidates":3,"max_provider_calls":1,"max_input_tokens":1000,"max_output_tokens":500,"max_input_bytes":4000,"max_output_bytes":4000},
            "provider":provider
        })).unwrap();
        let error = svc.run_dream_job(request).unwrap_err();
        assert!(
            error.message.contains("cannot override") || error.message.contains("command"),
            "{error}"
        );
        assert!(
            !error.message.contains("could not start"),
            "policy failed after launching command"
        );
    }
}

#[test]
fn disabled_command_provider_is_rejected_before_execution() {
    let mut config = Config::default();
    config.dream_provider.enabled = false;
    config.dream_provider.adapter = "command".into();
    config.dream_provider.model = "configured-model".into();
    config.dream_provider.command = vec!["/this-must-never-be-executed".into()];
    let svc = Service::new(Store::open(":memory:").unwrap(), config);
    let request: DreamJobRunRequest = serde_json::from_value(json!({
        "profile":"personal", "workspace":"ws", "kind":"dream_preview", "mode":"command",
        "budget":{"max_runtime_seconds":1,"max_input_records":5,"max_candidates":3,"max_provider_calls":1}
    })).unwrap();
    let error = svc.run_dream_job(request).unwrap_err();
    assert!(error.message.contains("enabled runtime provider"));
}

#[test]
fn command_provider_uses_the_rolling_daily_cost_ceiling() {
    let mut config = Config::default();
    config.default_workspace = "ws".into();
    config.dream_provider.enabled = true;
    config.dream_provider.adapter = "command".into();
    config.dream_provider.model = "configured-model".into();
    config.dream_provider.daily_cost_ceiling_micros = Some(100);
    config.dream_provider.command = vec![
        "/bin/sh".into(), "-c".into(),
        r#"cat >/dev/null; printf '{"schema_version":"dream-preview-v1","profile":"personal","workspace":"ws","candidates":[],"cost_micros":60}'"#.into(),
    ];
    let svc = Service::new(Store::open(":memory:").unwrap(), config);
    let request = |job_id: &str| DreamJobRunRequest {
        job_id: Some(job_id.into()),
        profile: Some("personal".into()),
        workspace: Some("ws".into()),
        repo: None,
        now: Some("2030-01-01T00:00:00Z".into()),
        since: None,
        since_explicit: false,
        kind: "dream_preview".into(),
        mode: Some("command".into()),
        budget: codex_memoryd::protocol::DreamJobBudget {
            max_runtime_seconds: 5,
            max_input_records: 5,
            max_candidates: 3,
            max_provider_calls: 1,
            max_cost_micros: 100,
            ..Default::default()
        },
        provider: None,
    };
    let first = svc.run_dream_job(request("command-cost-one")).unwrap();
    assert_eq!(
        first.provenance.unwrap().adapter_version,
        "native-command-v1"
    );
    let error = svc.run_dream_job(request("command-cost-two")).unwrap_err();
    assert!(error.message.contains("daily cost ceiling"), "{error}");
}
