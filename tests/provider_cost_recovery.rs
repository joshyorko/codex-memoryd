use codex_memoryd::{
    config::Config,
    protocol::{DreamJobBudget, DreamJobRunRequest},
    service::Service,
    store::Store,
};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

fn request(job_id: &str) -> DreamJobRunRequest {
    DreamJobRunRequest {
        job_id: Some(job_id.into()),
        profile: Some("personal".into()),
        workspace: Some("ws".into()),
        repo: None,
        now: Some("2030-01-01T00:00:00Z".into()),
        since: None,
        since_explicit: false,
        kind: "dream_preview".into(),
        mode: Some("command".into()),
        budget: DreamJobBudget {
            max_runtime_seconds: 5,
            max_input_records: 5,
            max_candidates: 3,
            max_provider_calls: 1,
            max_cost_micros: 100,
            ..Default::default()
        },
        provider: None,
    }
}

fn rolling_cost(svc: &Service) -> u64 {
    let daily_start = (OffsetDateTime::now_utc() - Duration::days(1))
        .format(&Rfc3339)
        .unwrap();
    svc.store
        .dream_provider_cost_since(&daily_start, None)
        .unwrap()
}

#[test]
fn repeated_ceiling_rejections_retain_each_billed_call_once() {
    let mut config = Config::default();
    config.default_workspace = "ws".into();
    config.dream_provider.enabled = true;
    config.dream_provider.adapter = "command".into();
    config.dream_provider.model = "configured-model".into();
    config.dream_provider.daily_cost_ceiling_micros = Some(100);
    config.dream_provider.command = vec![
        "/bin/sh".into(),
        "-c".into(),
        r#"cat >/dev/null; printf '{"schema_version":"dream-preview-v1","profile":"personal","workspace":"ws","candidates":[],"cost_micros":60}'"#.into(),
    ];
    let svc = Service::new(Store::open(":memory:").unwrap(), config);

    let first = svc.run_dream_job(request("provider-cost-one")).unwrap();
    assert_eq!(first.budget_usage.unwrap().cost_micros, 60);

    for job_id in ["provider-cost-two", "provider-cost-three"] {
        let error = svc.run_dream_job(request(job_id)).unwrap_err();
        assert!(error.message.contains("daily cost ceiling"), "{error}");
    }

    assert_eq!(rolling_cost(&svc), 180);
}

#[test]
fn per_job_cost_rejections_retain_each_billed_call_once() {
    let mut config = Config::default();
    config.default_workspace = "ws".into();
    config.dream_provider.enabled = true;
    config.dream_provider.adapter = "command".into();
    config.dream_provider.model = "configured-model".into();
    config.dream_provider.command = vec![
        "/bin/sh".into(),
        "-c".into(),
        r#"cat >/dev/null; printf '{"schema_version":"dream-preview-v1","profile":"personal","workspace":"ws","candidates":[],"cost_micros":60}'"#.into(),
    ];
    let svc = Service::new(Store::open(":memory:").unwrap(), config);

    for job_id in ["provider-budget-one", "provider-budget-two"] {
        let mut job = request(job_id);
        job.budget.max_cost_micros = 50;
        job.budget.daily_cost_ceiling_micros = None;
        let error = svc.run_dream_job(job).unwrap_err();
        assert!(error.message.contains("cost budget"), "{error}");
    }

    assert_eq!(rolling_cost(&svc), 120);
}
