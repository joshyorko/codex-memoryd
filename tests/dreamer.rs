use std::time::Duration;

use codex_memoryd::config::Config;
use codex_memoryd::protocol::*;
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;
use rusqlite::Connection;
use tempfile::TempDir;

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
    dream_since(svc, mode, now, None).unwrap()
}

fn dream_since(
    svc: &Service,
    mode: &str,
    now: &str,
    since: Option<&str>,
) -> codex_memoryd::error::Result<DreamResponse> {
    svc.dream(DreamRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        mode: Some(mode.to_string()),
        now: Some(now.to_string()),
        since: since.map(str::to_string),
    })
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

#[test]
fn successful_preview_records_safe_audit_without_memory_writes() {
    let svc = service();
    conclude(
        &svc,
        "Right now the preview audit test is planning to rewrite this tomorrow.",
    );
    let before = svc.store.count_records().unwrap();

    let report = dream(&svc, "preview", "2030-01-01T00:00:00Z");

    assert_eq!(svc.store.count_records().unwrap(), before);
    assert!(!report.candidates.is_empty());
    let last = svc
        .store
        .last_dream_run()
        .unwrap()
        .expect("preview audit row");
    assert_eq!(last.id, report.run_id);
    assert_eq!(last.mode, "preview");
    assert_eq!(last.status, "ok");
    assert_eq!(last.source_window_start, None);
    assert_eq!(
        last.source_window_end.as_deref(),
        Some("2030-01-01T00:00:00Z")
    );
    assert_eq!(last.created_count, 0);
    assert_eq!(last.archived_count, 0);
}

#[test]
fn successful_apply_records_audit_and_advances_watermark() {
    let svc = service();
    let old_id = conclude(&svc, "Storage backend is still TBD; evaluating options.");
    std::thread::sleep(Duration::from_millis(5));
    conclude(
        &svc,
        "Decision: storage uses rusqlite with bundled SQLite. The backend is no longer TBD.",
    );

    let applied = dream(&svc, "apply", "2030-01-01T00:00:00Z");

    assert!(applied.archived.contains(&old_id));
    let last = svc
        .store
        .last_dream_run()
        .unwrap()
        .expect("apply audit row");
    assert_eq!(last.id, applied.run_id);
    assert_eq!(last.mode, "apply");
    assert_eq!(last.status, "ok");
    assert_eq!(last.created_count, applied.created.len() as i64);
    assert_eq!(last.archived_count, applied.archived.len() as i64);
    assert_eq!(
        svc.store.dream_watermark("personal", "ws", None).unwrap(),
        Some("2030-01-01T00:00:00Z".to_string())
    );
}

#[test]
fn failed_run_records_error_without_advancing_watermark() {
    let svc = service();
    conclude(&svc, "Decision: Dreamer audit uses safe aggregate counts.");
    dream(&svc, "apply", "2030-01-01T00:00:00Z");
    assert_eq!(
        svc.store.dream_watermark("personal", "ws", None).unwrap(),
        Some("2030-01-01T00:00:00Z".to_string())
    );

    let err = dream_since(&svc, "preview", "2031-01-01T00:00:00Z", Some("not-rfc3339"))
        .expect_err("invalid since fails");

    assert_eq!(err.code.as_str(), "invalid_request");
    assert_eq!(
        svc.store.dream_watermark("personal", "ws", None).unwrap(),
        Some("2030-01-01T00:00:00Z".to_string())
    );
    let last = svc
        .store
        .last_dream_run()
        .unwrap()
        .expect("error audit row");
    assert_eq!(last.status, "error");
    assert_eq!(
        last.error_summary.as_deref(),
        Some("dream since must be an RFC3339 timestamp")
    );
    let status = svc.status().unwrap();
    assert_eq!(status.status, "degraded");
    assert!(status
        .degraded_reasons
        .iter()
        .any(|reason| reason.contains("last Dreamer run failed")));
}

#[test]
fn explicit_since_overrides_apply_watermark() {
    let svc = service();
    conclude(
        &svc,
        "Right now the watermark override test is planning to ship tomorrow.",
    );
    dream(&svc, "apply", "2030-01-01T00:00:00Z");

    let bounded = dream(&svc, "preview", "2031-01-01T00:00:00Z");
    assert!(
        bounded.candidates.is_empty(),
        "apply watermark should bound the incremental preview"
    );

    let override_run = dream_since(
        &svc,
        "preview",
        "2031-01-01T00:00:00Z",
        Some("2000-01-01T00:00:00Z"),
    )
    .unwrap();

    assert!(
        !override_run.candidates.is_empty(),
        "explicit since should replay older evidence"
    );
    let last = svc.store.last_dream_run().unwrap().unwrap();
    assert_eq!(
        last.source_window_start.as_deref(),
        Some("2000-01-01T00:00:00Z")
    );
}

#[test]
fn audit_row_does_not_store_raw_evidence_or_candidate_text() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("memory.db");
    let store = Store::open(&db).expect("open file store");
    let svc = Service::new(
        store,
        Config {
            default_workspace: "ws".to_string(),
            ..Default::default()
        },
    );
    let sentinel = "AUDIT_SENTINEL_VISIBLE_TEXT";
    conclude(
        &svc,
        &format!("Right now {sentinel} is planning to ship tomorrow."),
    );

    dream(&svc, "preview", "2030-01-01T00:00:00Z");

    let conn = Connection::open(&db).unwrap();
    let audit_text: String = conn
        .query_row(
            "SELECT id || ' ' || profile_id || ' ' || workspace_id || ' ' ||
                    mode || ' ' || status || ' ' || implementation_version || ' ' ||
                    config_hash || ' ' || ruleset_version || ' ' ||
                    COALESCE(source_counts, '') || ' ' ||
                    COALESCE(candidate_counts, '') || ' ' ||
                    COALESCE(error_summary, '')
             FROM dream_runs
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !audit_text.contains(sentinel),
        "dream_runs audit row must not store raw evidence or candidate text"
    );
}
