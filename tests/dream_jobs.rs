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
    assert_eq!(run.mode, "deterministic");
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
}
