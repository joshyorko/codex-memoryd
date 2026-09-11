use codex_memoryd::config::Config;
use codex_memoryd::domain::{Checkpoint, Conclusion, VisibleTurn};
use codex_memoryd::protocol::{
    ConclusionsRequest, DreamJobBudget, DreamJobRunRequest, TurnMessage, TurnSession, TurnsRequest,
};
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;
use serde_json::json;

fn source_store() -> (Service, Store) {
    let store = Store::open(":memory:").expect("store");
    store
        .ensure_workspace("personal", "acceptance")
        .expect("workspace");
    store
        .ensure_session(
            "session-acceptance",
            "personal",
            "acceptance",
            None,
            None,
            "fixture",
        )
        .expect("session");
    store
        .insert_visible_turn(&VisibleTurn {
            id: "turn-acceptance".into(),
            session_id: "session-acceptance".into(),
            actor: "user".into(),
            content: "The user source is retained for the acceptance window.".into(),
            created_at: "2026-09-11T00:00:00Z".into(),
            metadata: json!({}),
        })
        .expect("visible turn");
    store
        .insert_conclusion(&Conclusion {
            id: "conclusion-acceptance".into(),
            profile_id: "personal".into(),
            workspace_id: "acceptance".into(),
            repo_id: None,
            target: "user".into(),
            content: "The conclusion source is retained for the acceptance window.".into(),
            source_id: None,
            created_at: "2026-09-11T00:01:00Z".into(),
            metadata: json!({}),
        })
        .expect("conclusion");
    store
        .insert_checkpoint(&Checkpoint {
            id: "checkpoint-acceptance".into(),
            session_id: Some("session-acceptance".into()),
            profile_id: "personal".into(),
            workspace_id: "acceptance".into(),
            repo_id: None,
            summary: "The checkpoint source is retained for the acceptance window.".into(),
            changed_files: vec![],
            decisions: vec![],
            blockers: vec![],
            next_steps: vec![],
            tests_run: vec![],
            tests_not_run: vec![],
            branch: None,
            commit: None,
            created_at: "2026-09-11T00:02:00Z".into(),
        })
        .expect("checkpoint");
    store
        .upsert_source(
            "personal",
            "acceptance",
            "imported_fixture",
            Some("fixture:acceptance"),
            "acceptance-source-hash",
            &json!({}),
        )
        .expect("imported source");

    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "acceptance".into();
    (Service::new(store.clone(), config), store)
}

#[test]
fn dream_job_combines_all_evidence_streams_within_input_record_budget() {
    let (service, _) = source_store();
    let response = service
        .run_dream_job(DreamJobRunRequest {
            job_id: Some("job-acceptance".into()),
            profile: Some("personal".into()),
            workspace: Some("acceptance".into()),
            repo: None,
            now: Some("2026-09-11T01:00:00Z".into()),
            since: Some("2026-09-10T00:00:00Z".into()),
            since_explicit: true,
            kind: "dream_preview".into(),
            mode: Some("deterministic".into()),
            budget: DreamJobBudget {
                max_runtime_seconds: 10,
                max_input_records: 2,
                max_candidates: 2,
                max_input_tokens: 0,
                max_output_tokens: 0,
                max_input_bytes: 0,
                max_output_bytes: 0,
                max_provider_calls: 0,
                max_retries: 0,
                max_cost_micros: 0,
                daily_cost_ceiling_micros: None,
            },
            provider: None,
        })
        .expect("dream job");
    let window = response.preview.evidence_window;
    let total = window.visible_turns.count
        + window.conclusions.count
        + window.checkpoints.count
        + window.imported_memories.count
        + window.active_memory_records.count;
    assert!(total <= 2, "evidence window used {total} records");
}

#[test]
fn capped_scheduled_run_does_not_advance_past_unprocessed_tail() {
    let store = Store::open(":memory:").expect("store");
    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "tail".into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.automatic_apply = true;
    config.dream_scheduler.scheduled_provider_enabled = false;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_scheduler.min_turn_count = 0;
    config.dream_scheduler.max_batch_size = 20;
    config.dream_scheduler.max_candidates = 1;
    let service = Service::new(store.clone(), config);
    service
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("tail".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec![
                "Decision: alpha uses the blue release path.".into(),
                "Decision: beta uses the green release path.".into(),
            ]),
            metadata: None,
            record_type: Some("decision".into()),
        })
        .expect("conclusions");

    let first = service
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .expect("capped scheduled run");

    assert_eq!(first.status, "ok_with_limits");
    assert!(first.limits_hit.contains(&"max_candidates".to_string()));
    assert!(first.watermark_after.is_none());
    assert!(store
        .scheduled_dream_watermark("personal", "tail", None)
        .expect("watermark")
        .is_none());
}

#[test]
fn low_volume_idle_scope_progresses_with_an_explicit_zero_turn_threshold() {
    let store = Store::open(":memory:").expect("store");
    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "low-volume".into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_scheduler.min_turn_count = 0;
    let service = Service::new(store.clone(), config);
    service
        .turns(TurnsRequest {
            profile: Some("personal".into()),
            workspace: Some("low-volume".into()),
            repo: None,
            session: Some(TurnSession {
                id: Some("low-volume-session".into()),
                thread_id: None,
                source: Some("fixture".into()),
                metadata: None,
            }),
            messages: Some(vec![TurnMessage {
                actor: "user".into(),
                content: "I prefer concise low-volume updates.".into(),
                created_at: Some("2026-09-11T00:00:00Z".into()),
                metadata: None,
            }]),
            write_policy: None,
        })
        .expect("low-volume turn");

    let result = service
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .expect("scheduled run");

    assert_eq!(result.status, "ok");
    assert!(result.watermark_after.is_some());
}

#[test]
fn dream_job_does_not_project_future_dated_sources_into_the_frontier() {
    let store = Store::open(":memory:").expect("store");
    store
        .ensure_workspace("personal", "time")
        .expect("workspace");
    store
        .ensure_session("time-session", "personal", "time", None, None, "fixture")
        .expect("session");
    for (id, created_at) in [
        ("time-past", "2026-09-10T23:00:00Z"),
        ("time-future", "2026-09-12T00:00:00Z"),
    ] {
        store
            .insert_visible_turn(&VisibleTurn {
                id: id.into(),
                session_id: "time-session".into(),
                actor: "user".into(),
                content: format!("source {id}"),
                created_at: created_at.into(),
                metadata: json!({}),
            })
            .expect("visible turn");
    }
    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "time".into();
    let service = Service::new(store, config);

    let response = service
        .run_dream_job(DreamJobRunRequest {
            job_id: Some("time-job".into()),
            profile: Some("personal".into()),
            workspace: Some("time".into()),
            repo: None,
            now: Some("2026-09-11T00:00:00Z".into()),
            since: Some("2026-09-09T00:00:00Z".into()),
            since_explicit: true,
            kind: "dream_preview".into(),
            mode: Some("deterministic".into()),
            budget: DreamJobBudget {
                max_runtime_seconds: 10,
                max_input_records: 10,
                max_candidates: 2,
                ..Default::default()
            },
            provider: None,
        })
        .expect("dream job");
    let visible_ids = response
        .preview
        .evidence_window
        .visible_turns
        .sources
        .into_iter()
        .map(|source| source.id)
        .collect::<Vec<_>>();
    assert_eq!(visible_ids, vec!["time-past"]);
}
