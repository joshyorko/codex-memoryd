use codex_memoryd::config::Config;
use codex_memoryd::protocol::{ConclusionsRequest, RecallRequest};
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;

fn config(path: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "journey".into();
    config.storage_path = path.to_path_buf();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.automatic_apply = true;
    config.dream_scheduler.scheduled_provider_enabled = false;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_scheduler.min_turn_count = 0;
    config
}

#[test]
fn governed_adoption_survives_restart_and_fresh_recall() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journey.sqlite");
    let first_config = config(&path);
    let first_store = Store::open(path.to_str().unwrap()).unwrap();
    let first = Service::new(first_store, first_config);
    first
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("journey".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec!["Decision: use concise summaries".into()]),
            metadata: None,
            record_type: Some("decision".into()),
        })
        .unwrap();
    let applied = first
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .unwrap();
    assert_eq!(applied.run.unwrap().created.len(), 1);
    drop(first);

    let second = Service::new(Store::open(path.to_str().unwrap()).unwrap(), config(&path));
    let recall = second
        .recall(RecallRequest {
            profile: Some("personal".into()),
            workspace: Some("journey".into()),
            repo: None,
            session: None,
            query: Some("concise summaries".into()),
            files: vec![],
            max_tokens: Some(500),
            pack_mode: Some("default".into()),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            as_of: None,
            include_history: false,
            metadata: None,
        })
        .unwrap();
    assert!(recall
        .facts
        .iter()
        .any(|fact| fact.content.contains("concise summaries")));
    assert_eq!(recall.authority, "recall_not_authority");
}

#[test]
fn plain_preference_is_attributed_and_recalled_after_governed_adoption() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("plain-preference.sqlite");
    let content = "I prefer concise status updates.";

    let first = Service::new(Store::open(path.to_str().unwrap()).unwrap(), config(&path));
    let captured = first
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("journey".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec![content.into()]),
            metadata: None,
            record_type: Some("preference".into()),
        })
        .unwrap();
    let run = first
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .unwrap()
        .run
        .unwrap();

    assert_eq!(run.created, captured.record_ids);
    assert_eq!(first.store.count_records().unwrap(), 1);
    let record = first
        .store
        .get_record(&captured.record_ids[0])
        .unwrap()
        .unwrap();
    assert_eq!(record.content, content);
    assert_eq!(record.metadata["target"], "user");
    assert_eq!(record.metadata["governed_consolidation_applied"], true);
    drop(first);

    let fresh = Service::new(Store::open(path.to_str().unwrap()).unwrap(), config(&path));
    let recall = fresh
        .recall(RecallRequest {
            profile: Some("personal".into()),
            workspace: Some("journey".into()),
            repo: None,
            session: None,
            query: Some("concise status updates".into()),
            files: vec![],
            max_tokens: Some(500),
            pack_mode: Some("default".into()),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            as_of: None,
            include_history: false,
            metadata: None,
        })
        .unwrap();
    assert!(recall.facts.iter().any(|fact| fact.content == content));
    assert_eq!(recall.authority, "recall_not_authority");
}

#[test]
fn scheduled_adoption_fresh_consumer_correction_wins_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("correction.sqlite");
    let old = "Decision: project cobalt uses violet lanterns for release ceremonies.";
    let new =
        "Correction: project cobalt uses amber lanterns for release ceremonies instead of violet lanterns.";

    let first = Service::new(Store::open(path.to_str().unwrap()).unwrap(), config(&path));
    first
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("journey".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec![old.into()]),
            metadata: None,
            record_type: Some("decision".into()),
        })
        .unwrap();
    let first_run = first
        .scheduled_dream(Some("2000-01-01T00:00:00Z".into()))
        .unwrap()
        .run
        .unwrap();
    let old_id = first_run.created[0].clone();
    drop(first);

    let second = Service::new(Store::open(path.to_str().unwrap()).unwrap(), config(&path));
    let before = second
        .recall(RecallRequest {
            profile: Some("personal".into()),
            workspace: Some("journey".into()),
            repo: None,
            session: None,
            query: Some("cobalt release ceremonies".into()),
            files: vec![],
            max_tokens: Some(500),
            pack_mode: Some("default".into()),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            as_of: None,
            include_history: false,
            metadata: None,
        })
        .unwrap();
    assert!(before.facts.iter().any(|fact| fact.content == old));

    let correction = second
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("journey".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec![new.into()]),
            metadata: None,
            record_type: Some("decision".into()),
        })
        .unwrap();
    let new_id = correction.record_ids[0].clone();
    let second_run = second
        .scheduled_dream(Some("2100-01-01T00:00:00Z".into()))
        .unwrap()
        .run
        .unwrap();
    assert!(second_run.archived.contains(&old_id));
    assert!(
        second.store.get_record(&old_id).unwrap().unwrap().archived,
        "scheduled correction must archive the superseded record"
    );
    assert_eq!(
        second
            .store
            .get_record(&new_id)
            .unwrap()
            .unwrap()
            .supersedes,
        vec![old_id.clone()]
    );
    drop(second);

    let fresh = Service::new(Store::open(path.to_str().unwrap()).unwrap(), config(&path));
    let after = fresh
        .recall(RecallRequest {
            profile: Some("personal".into()),
            workspace: Some("journey".into()),
            repo: None,
            session: None,
            query: Some("cobalt release ceremonies".into()),
            files: vec![],
            max_tokens: Some(500),
            pack_mode: Some("default".into()),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            as_of: None,
            include_history: false,
            metadata: None,
        })
        .unwrap();
    assert!(after.facts.iter().any(|fact| fact.content == new));
    assert!(!after.facts.iter().any(|fact| fact.content == old));
    assert_eq!(after.authority, "recall_not_authority");
}

#[test]
fn review_underlying_source_ids_remain_resolvable_during_adoption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source-ids.sqlite");
    let service = Service::new(Store::open(path.to_str().unwrap()).unwrap(), config(&path));
    let captured = service
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("journey".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec!["I prefer concise status updates.".into()]),
            metadata: None,
            record_type: Some("preference".into()),
        })
        .unwrap();
    service
        .store
        .transaction(|tx| {
            tx.execute(
                "UPDATE memory_records SET source_ids = ?1 WHERE id = ?2",
                rusqlite::params!["[\"source-user-event\"]", &captured.record_ids[0]],
            )?;
            Ok(())
        })
        .unwrap();
    let run = service
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .unwrap()
        .run
        .unwrap();
    assert_eq!(run.created, captured.record_ids);
    let adopted = service.store.get_record(&run.created[0]).unwrap().unwrap();
    assert!(adopted.source_ids.contains(&"source-user-event".into()));
    assert_eq!(adopted.metadata["governed_consolidation_applied"], true);
}

#[test]
fn review_full_input_window_does_not_skip_unselected_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input-cap.sqlite");
    let mut cfg = config(&path);
    cfg.dream_scheduler.max_batch_size = 1;
    let service = Service::new(Store::open(path.to_str().unwrap()).unwrap(), cfg);
    for content in [
        "I prefer concise status updates.",
        "I prefer stable interfaces.",
        "I prefer explicit test results.",
    ] {
        service
            .conclusions(ConclusionsRequest {
                profile: Some("personal".into()),
                workspace: Some("journey".into()),
                repo: None,
                target: Some("user".into()),
                conclusions: Some(vec![content.into()]),
                metadata: None,
                record_type: Some("preference".into()),
            })
            .unwrap();
    }
    let result = service
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .unwrap();
    assert!(result.limits_hit.contains(&"max_input_records".into()));
    // A full page must retain a bounded continuation cursor, not advance to
    // the run's end timestamp and skip unselected records.
    assert!(result
        .watermark_after
        .as_deref()
        .is_some_and(codex_memoryd::dream::is_scheduler_cursor));
}

#[test]
fn consolidation_controls_cannot_activate_disabled_policy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disabled.sqlite");
    let mut disabled = config(&path);
    disabled.dream_scheduler.automatic_apply = false;
    let service = Service::new(Store::open(path.to_str().unwrap()).unwrap(), disabled);
    let apply =
        service.apply_stored_consolidation(codex_memoryd::protocol::ConsolidationApplyRequest {
            batch_id: "synthetic".into(),
        });
    assert!(apply
        .unwrap_err()
        .message
        .contains("operator automatic policy"));
    let undo =
        service.undo_stored_consolidation(codex_memoryd::protocol::ConsolidationUndoRequest {
            batch_id: "synthetic".into(),
        });
    assert!(undo
        .unwrap_err()
        .message
        .contains("operator automatic policy"));
}
