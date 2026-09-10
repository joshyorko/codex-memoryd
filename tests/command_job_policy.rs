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
