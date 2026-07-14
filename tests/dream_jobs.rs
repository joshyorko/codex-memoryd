use codex_memoryd::config::Config;
use codex_memoryd::domain::RepoIdentity;
use codex_memoryd::protocol::{
    CheckpointRequest, ConclusionsRequest, DreamJobBudget, DreamJobProvider, DreamJobRunRequest,
    DreamProviderCommand,
};
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;

fn service() -> Service {
    let store = Store::open(":memory:").expect("open store");
    let config = Config {
        default_workspace: "ws".to_string(),
        ..Default::default()
    };
    Service::new(store, config)
}

fn conclude(svc: &Service, content: &str) {
    svc.conclusions(ConclusionsRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        target: Some("user".to_string()),
        conclusions: Some(vec![content.to_string()]),
        metadata: None,
        record_type: None,
    })
    .unwrap();
}

fn checkpoint(svc: &Service, summary: &str) {
    svc.checkpoint(CheckpointRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        session: None,
        summary: Some(summary.to_string()),
        changed_files: vec![],
        decisions: vec![],
        blockers: vec![],
        next_steps: vec![],
        tests_run: vec![],
        tests_not_run: vec![],
        branch: None,
        commit: None,
    })
    .unwrap();
}

fn base_request() -> DreamJobRunRequest {
    DreamJobRunRequest {
        job_id: Some("job_default".to_string()),
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None::<RepoIdentity>,
        now: Some("2030-01-01T00:00:00Z".to_string()),
        since: None,
        kind: "dream_preview".to_string(),
        mode: Some("deterministic".to_string()),
        budget: DreamJobBudget {
            max_runtime_seconds: 30,
            max_input_records: 500,
            max_candidates: 5,
        },
        provider: None,
    }
}

#[test]
fn deterministic_job_run_is_preview_only_and_persists_budgeted_job_record() {
    let svc = service();
    conclude(
        &svc,
        "Right now the daemon is failing on startup, planning to patch it tomorrow.",
    );
    conclude(
        &svc,
        "OAuth sync is planned; will implement it next week.",
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    checkpoint(&svc, "Implemented OAuth sync and merged it.");
    let before = svc.store.count_records().unwrap();

    let run = svc
        .run_dream_job(DreamJobRunRequest {
            job_id: Some("job_det_preview".to_string()),
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None::<RepoIdentity>,
            now: Some("2030-01-01T00:00:00Z".to_string()),
            since: None,
            kind: "dream_preview".to_string(),
            mode: Some("deterministic".to_string()),
            budget: DreamJobBudget {
                max_runtime_seconds: 30,
                max_input_records: 500,
                max_candidates: 5,
            },
            provider: Some(DreamJobProvider {
                command: Some(DreamProviderCommand {
                    argv: vec!["/bin/false".to_string(), "--never-run".to_string()],
                }),
            }),
        })
        .unwrap();

    assert_eq!(run.status, "ok");
    assert_eq!(run.mode, "preview");
    assert_eq!(run.preview.mode, "preview");
    assert!(!run.preview.candidates.is_empty());
    assert_eq!(svc.store.count_records().unwrap(), before);
    assert_eq!(svc.store.count_table_rows("dream_jobs").unwrap(), 1);

    let job = svc.store.get_dream_job("job_det_preview").unwrap().unwrap();
    assert_eq!(job.kind, "dream_preview");
    assert_eq!(job.mode, "deterministic");
    assert_eq!(job.budget.max_candidates, 5);
    assert_eq!(
        job.provider.command.unwrap().argv,
        vec!["/bin/false".to_string(), "--never-run".to_string()]
    );
    assert_eq!(job.last_run_id.as_deref(), Some(run.run_id.as_str()));
}

#[test]
fn deterministic_job_run_reuses_dream_run_audit_and_enforces_candidate_budget() {
    let svc = service();
    conclude(
        &svc,
        "Right now the daemon is failing on startup, planning to patch it tomorrow.",
    );
    conclude(
        &svc,
        "OAuth sync is planned; will implement it next week.",
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    checkpoint(&svc, "Implemented OAuth sync and merged it.");

    let run = svc
        .run_dream_job(DreamJobRunRequest {
            job_id: Some("job_limit".to_string()),
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None::<RepoIdentity>,
            now: Some("2030-01-01T00:00:00Z".to_string()),
            since: None,
            kind: "dream_preview".to_string(),
            mode: Some("deterministic".to_string()),
            budget: DreamJobBudget {
                max_runtime_seconds: 30,
                max_input_records: 500,
                max_candidates: 1,
            },
            provider: None,
        })
        .unwrap();

    assert_eq!(run.status, "ok_with_limits");
    assert!(run.limits_hit.contains(&"max_candidates".to_string()));
    assert!(run.preview.candidates.len() <= 1);

    let last = svc.store.last_dream_run().unwrap().unwrap();
    assert_eq!(last.id, run.run_id);
    assert_eq!(last.mode, "preview");
}

#[test]
fn deterministic_job_run_rejects_zero_runtime_budget() {
    let svc = service();

    let mut req = base_request();
    req.job_id = Some("job_zero_runtime".to_string());
    req.budget.max_runtime_seconds = 0;

    let err = svc
        .run_dream_job(req)
        .expect_err("zero runtime budget should be rejected");

    assert!(
        err.message.contains("max_runtime_seconds must be > 0"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn deterministic_job_run_rejects_invalid_mode_kind_and_timestamps() {
    let svc = service();

    let mut bad_mode = base_request();
    bad_mode.job_id = Some("job_bad_mode".to_string());
    bad_mode.mode = Some("model".to_string());
    let mode_err = svc
        .run_dream_job(bad_mode)
        .expect_err("non-deterministic mode should be rejected");
    assert!(mode_err.message.contains("mode must be deterministic"));

    let mut bad_kind = base_request();
    bad_kind.job_id = Some("job_bad_kind".to_string());
    bad_kind.kind = "compact_cards".to_string();
    let kind_err = svc
        .run_dream_job(bad_kind)
        .expect_err("non-preview kind should be rejected");
    assert!(kind_err.message.contains("kind must be dream_preview"));

    let mut bad_now = base_request();
    bad_now.job_id = Some("job_bad_now".to_string());
    bad_now.now = Some("not-a-time".to_string());
    let now_err = svc
        .run_dream_job(bad_now)
        .expect_err("invalid now must be rejected");
    assert!(now_err.message.contains("now must be an RFC3339"));

    let mut bad_since = base_request();
    bad_since.job_id = Some("job_bad_since".to_string());
    bad_since.since = Some("not-a-time".to_string());
    let since_err = svc
        .run_dream_job(bad_since)
        .expect_err("invalid since must be rejected");
    assert!(since_err.message.contains("since must be an RFC3339"));
}

#[test]
fn deterministic_job_run_preview_preserves_evidence_refs() {
    let svc = service();
    conclude(
        &svc,
        "I prefer concise commit messages and deterministic release scripts.",
    );
    let run = svc
        .run_dream_job(DreamJobRunRequest {
            job_id: Some("job_evidence_refs".to_string()),
            ..base_request()
        })
        .expect("job run should succeed");

    let has_candidate_refs = run
        .preview
        .candidates
        .iter()
        .any(|candidate| !candidate.evidence_refs.is_empty());
    assert!(
        has_candidate_refs,
        "preview candidates should preserve evidence refs"
    );
}
