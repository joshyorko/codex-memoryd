use codex_memoryd::config::{Config, DreamProviderConfig};
use codex_memoryd::domain::{Checkpoint, Conclusion, VisibleTurn};
use codex_memoryd::protocol::{
    ConclusionsRequest, DreamEvidenceSource, DreamJobBudget, DreamJobRunRequest, TurnMessage,
    TurnSession, TurnsRequest,
};
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;
use serde_json::json;
use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

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
fn capped_scheduled_run_preserves_and_continues_unprocessed_tail() {
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
                "Decision: alpha service uses blue deployment.".into(),
                "Decision: beta database uses green migration.".into(),
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

    let first_created = first.run.as_ref().expect("first run").created.clone();
    let second = service
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .expect("tail continuation");
    let mut created_ids = first_created.clone();
    created_ids.extend(
        second
            .run
            .as_ref()
            .expect("second run")
            .created
            .iter()
            .cloned(),
    );
    let created_contents = created_ids
        .iter()
        .filter_map(|id| store.get_record(id).expect("created record"))
        .map(|record| record.content)
        .collect::<Vec<_>>();
    assert!(created_contents
        .iter()
        .any(|content| content.contains("alpha service uses blue deployment")));
    assert!(created_contents
        .iter()
        .any(|content| content.contains("beta database uses green migration")));
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
fn scheduled_simulated_clock_keeps_current_store_evidence_visible() {
    let store = Store::open(":memory:").expect("store");
    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "simulated-clock".into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.automatic_apply = true;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_scheduler.min_turn_count = 0;
    config.dream_scheduler.scheduled_provider_enabled = false;
    let service = Service::new(store, config);
    service
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("simulated-clock".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec![
                "Decision: simulated clock acceptance keeps current evidence visible.".into(),
            ]),
            metadata: None,
            record_type: Some("decision".into()),
        })
        .expect("current conclusion");

    let result = service
        .scheduled_dream(Some("2000-01-01T00:00:00Z".into()))
        .expect("simulated scheduled run");
    let run = result.run.expect("scheduled run");
    assert!(run.evidence_window.active_memory_records.count > 0);
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

#[test]
fn bounded_window_preserves_temporal_edges_across_all_evidence_streams() {
    let store = Store::open(":memory:").expect("store");
    store
        .ensure_workspace("personal", "temporal-edges")
        .expect("workspace");
    store
        .ensure_session(
            "temporal-edges-session",
            "personal",
            "temporal-edges",
            None,
            None,
            "fixture",
        )
        .expect("session");

    for (id, created_at) in [
        ("equal-visible-a", "2026-09-10T00:00:00Z"),
        ("equal-visible-b", "2026-09-10T00:00:00Z"),
        ("backdated-visible", "2020-01-01T00:00:00Z"),
        ("missing-visible-date", ""),
        ("future-visible", "2040-01-01T00:00:00Z"),
    ] {
        store
            .insert_visible_turn(&VisibleTurn {
                id: id.into(),
                session_id: "temporal-edges-session".into(),
                actor: "user".into(),
                content: format!("evidence {id}"),
                created_at: created_at.into(),
                metadata: json!({}),
            })
            .expect("visible turn");
    }
    for (id, created_at) in [
        ("equal-conclusion-a", "2026-09-10T00:00:00Z"),
        ("equal-conclusion-b", "2026-09-10T00:00:00Z"),
        ("future-conclusion", "2040-01-01T00:00:00Z"),
    ] {
        store
            .insert_conclusion(&Conclusion {
                id: id.into(),
                profile_id: "personal".into(),
                workspace_id: "temporal-edges".into(),
                repo_id: None,
                target: "user".into(),
                content: format!("evidence {id}"),
                source_id: None,
                created_at: created_at.into(),
                metadata: json!({}),
            })
            .expect("conclusion");
    }
    for (id, created_at) in [
        ("equal-checkpoint", "2026-09-10T00:00:00Z"),
        ("future-checkpoint", "2040-01-01T00:00:00Z"),
    ] {
        store
            .insert_checkpoint(&Checkpoint {
                id: id.into(),
                session_id: Some("temporal-edges-session".into()),
                profile_id: "personal".into(),
                workspace_id: "temporal-edges".into(),
                repo_id: None,
                summary: format!("evidence {id}"),
                changed_files: vec![],
                decisions: vec![],
                blockers: vec![],
                next_steps: vec![],
                tests_run: vec![],
                tests_not_run: vec![],
                branch: None,
                commit: None,
                created_at: created_at.into(),
            })
            .expect("checkpoint");
    }
    let (source, created) = store
        .upsert_source(
            "personal",
            "temporal-edges",
            "backdated_import",
            Some("fixture/backdated.json"),
            "temporal-edges-source",
            &json!({"event_time": "2020-01-01T00:00:00Z"}),
        )
        .expect("source");
    assert!(created);

    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "temporal-edges".into();
    let service = Service::new(store, config);
    let response = service
        .run_dream_job(DreamJobRunRequest {
            job_id: Some("temporal-edges-job".into()),
            profile: Some("personal".into()),
            workspace: Some("temporal-edges".into()),
            repo: None,
            now: Some("2030-01-01T00:00:00Z".into()),
            since: None,
            since_explicit: false,
            kind: "dream_preview".into(),
            mode: Some("deterministic".into()),
            budget: DreamJobBudget {
                max_runtime_seconds: 10,
                max_input_records: 16,
                max_candidates: 2,
                ..Default::default()
            },
            provider: None,
        })
        .expect("dream job");

    let window = response.preview.evidence_window;
    let ids = |sources: Vec<DreamEvidenceSource>| {
        sources
            .into_iter()
            .map(|source| source.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(window.visible_turns.sources),
        vec![
            "equal-visible-a",
            "equal-visible-b",
            "backdated-visible",
            "missing-visible-date"
        ]
    );
    assert_eq!(
        ids(window.conclusions.sources),
        vec!["equal-conclusion-a", "equal-conclusion-b"]
    );
    assert_eq!(ids(window.checkpoints.sources), vec!["equal-checkpoint"]);
    assert_eq!(ids(window.imported_memories.sources), vec![source.id]);
}

#[test]
fn busy_unrelated_workspace_does_not_starve_low_volume_scope() {
    let store = Store::open(":memory:").expect("store");

    let mut busy_config = Config::default();
    busy_config.default_profile = "personal".into();
    busy_config.default_workspace = "busy-workspace".into();
    busy_config.dream_scheduler.enabled = true;
    busy_config.dream_scheduler.idle_window_seconds = 3600;
    let busy = Service::new(store.clone(), busy_config);
    busy.turns(TurnsRequest {
        profile: Some("personal".into()),
        workspace: Some("busy-workspace".into()),
        repo: None,
        session: Some(TurnSession {
            id: Some("busy-session".into()),
            thread_id: None,
            source: Some("fixture".into()),
            metadata: None,
        }),
        messages: Some(vec![TurnMessage {
            actor: "user".into(),
            content: "Busy workspace evidence remains active.".into(),
            created_at: Some("2026-09-11T00:00:00Z".into()),
            metadata: None,
        }]),
        write_policy: None,
    })
    .expect("busy turn");

    let mut quiet_config = Config::default();
    quiet_config.default_profile = "personal".into();
    quiet_config.default_workspace = "quiet-workspace".into();
    quiet_config.dream_scheduler.enabled = true;
    quiet_config.dream_scheduler.idle_window_seconds = 0;
    quiet_config.dream_scheduler.min_session_age_seconds = 0;
    quiet_config.dream_scheduler.min_turn_count = 0;
    let quiet = Service::new(store, quiet_config);
    quiet
        .turns(TurnsRequest {
            profile: Some("personal".into()),
            workspace: Some("quiet-workspace".into()),
            repo: None,
            session: Some(TurnSession {
                id: Some("quiet-session".into()),
                thread_id: None,
                source: Some("fixture".into()),
                metadata: None,
            }),
            messages: Some(vec![TurnMessage {
                actor: "user".into(),
                content: "Quiet workspace evidence should progress.".into(),
                created_at: Some("2026-09-11T00:00:00Z".into()),
                metadata: None,
            }]),
            write_policy: None,
        })
        .expect("quiet turn");

    let result = quiet
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .expect("quiet scheduled run");
    assert_eq!(result.status, "ok");
    assert!(result.watermark_after.is_some());
}

#[test]
fn unchanged_idle_tick_skips_scheduled_model_call() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("provider listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let address = listener.local_addr().expect("provider address");
    let (called_tx, called_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = called_tx.send(());
                    let body = r#"{"choices":[{"message":{"content":"[]"}}]}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    return;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                _ => return,
            }
        }
    });

    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "unchanged-idle".into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_scheduler.min_turn_count = 0;
    config.dream_scheduler.scheduled_provider_enabled = true;
    config.dream_provider = DreamProviderConfig {
        enabled: true,
        endpoint: format!("http://{address}/v1"),
        model: "fixture-model".into(),
        ..DreamProviderConfig::default()
    };
    let service = Service::new(Store::open(":memory:").expect("store"), config);
    let result = service
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .expect("unchanged scheduled run");
    assert_eq!(result.status, "ok");
    assert!(called_rx.recv_timeout(Duration::from_millis(50)).is_err());
    server.join().expect("provider server");
}
