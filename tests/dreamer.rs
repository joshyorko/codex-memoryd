use std::time::Duration;

use codex_memoryd::config::Config;
use codex_memoryd::protocol::*;
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

fn conclude(svc: &Service, content: &str) -> String {
    let resp = svc
        .conclusions(ConclusionsRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            target: Some("user".to_string()),
            conclusions: Some(vec![content.to_string()]),
            metadata: None,
            record_type: None,
        })
        .unwrap();
    resp.record_ids[0].clone()
}

fn dream(svc: &Service, mode: &str, now: &str) -> DreamResponse {
    svc.dream(DreamRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        mode: Some(mode.to_string()),
        now: Some(now.to_string()),
    })
    .unwrap()
}

#[test]
fn relative_time_fact_is_drift_prone_and_expires() {
    let svc = service();
    let old_id = conclude(
        &svc,
        "Right now the daemon is failing on startup, planning to patch it tomorrow.",
    );

    let report = dream(&svc, "preview", "2030-01-01T00:00:00Z");

    let stale = report
        .stale
        .iter()
        .find(|entry| entry.memory_id == old_id)
        .expect("drift-prone stale entry");
    assert!(stale.drift_prone);
    assert_eq!(stale.suggested_action, "rewrite_historical");
    assert!(stale.valid_until.is_some());
    assert!(report.candidates.iter().any(|candidate| {
        candidate.action == "rewrite_historical"
            && candidate.state == "historical"
            && candidate.supersedes == vec![old_id.clone()]
    }));
}

#[test]
fn newer_same_subject_fact_supersedes_and_archives_old_record() {
    let svc = service();
    let old_id = conclude(&svc, "Storage backend is still TBD; evaluating options.");
    std::thread::sleep(Duration::from_millis(5));
    conclude(
        &svc,
        "Decision: storage uses rusqlite with bundled SQLite. The backend is no longer TBD.",
    );

    let preview = dream(&svc, "preview", "2026-06-09T00:00:00Z");
    assert!(preview.candidates.iter().any(|candidate| {
        candidate.action == "supersede" && candidate.supersedes == vec![old_id.clone()]
    }));

    let applied = dream(&svc, "apply", "2026-06-09T00:00:00Z");
    assert!(applied.archived.contains(&old_id));

    let recall = svc
        .recall(RecallRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            session: None,
            query: Some("storage backend".to_string()),
            files: vec![],
            max_tokens: Some(1000),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            metadata: None,
        })
        .unwrap();
    assert!(
        recall
            .facts
            .iter()
            .all(|fact| !fact.content.contains("still TBD")),
        "superseded active fact must not survive default recall"
    );
    assert!(recall
        .facts
        .iter()
        .any(|fact| fact.content.contains("rusqlite")));

    let archived_search = svc
        .search(SearchRequest {
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None,
            query: Some("storage".to_string()),
            scope: None,
            record_type: None,
            limit: Some(10),
            include_archived: true,
            cursor: None,
        })
        .unwrap();
    assert!(archived_search
        .matches
        .iter()
        .any(|m| m.id == old_id && m.archived));
}

#[test]
fn planned_fact_transitions_to_completed_supersession() {
    let svc = service();
    let old_id = conclude(&svc, "OAuth sync is planned; will implement it next week.");
    std::thread::sleep(Duration::from_millis(5));
    svc.checkpoint(CheckpointRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        session: None,
        summary: Some("Implemented OAuth sync and merged it.".to_string()),
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

    let report = dream(&svc, "preview", "2026-06-09T00:00:00Z");
    assert!(report.candidates.iter().any(|candidate| {
        candidate.action == "supersede"
            && candidate.state == "completed"
            && candidate.supersedes == vec![old_id.clone()]
    }));
}
