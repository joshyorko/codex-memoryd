use codex_memoryd::config::Config;
use codex_memoryd::domain::RepoIdentity;
use codex_memoryd::protocol::{ConclusionsRequest, RecallRequest};
use codex_memoryd::service::Service;
use codex_memoryd::store::{RecordQuery, Store};

fn config() -> Config {
    let mut config = Config::default();
    config.default_profile = "personal".into();
    config.default_workspace = "scope".into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.automatic_apply = true;
    config.dream_scheduler.scheduled_provider_enabled = false;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_scheduler.min_turn_count = 0;
    config
}

fn repo(repo_id: &str) -> RepoIdentity {
    RepoIdentity {
        repo_id: repo_id.into(),
        is_git: true,
        ..Default::default()
    }
}

#[test]
fn scheduled_adoption_preserves_repository_scope() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), config());
    let source = service
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("scope".into()),
            repo: Some(repo("repo-a")),
            target: Some("user".into()),
            conclusions: Some(vec![
                "Decision: repository alpha uses the crimson deployment.".into(),
            ]),
            metadata: None,
            record_type: Some("decision".into()),
        })
        .expect("repo-scoped source");

    let source_id = source.record_ids.first().expect("source record").clone();
    assert_eq!(
        store
            .get_record(&source_id)
            .expect("read source")
            .expect("source")
            .repo_id
            .as_deref(),
        Some("repo-a")
    );

    let scheduled = service
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .expect("scheduled adoption");
    let run_id = scheduled.run.expect("scheduled run").run_id;
    let batch = store
        .read_consolidation_batch(&format!("consolidation_{run_id}"))
        .expect("read consolidation batch")
        .expect("consolidation batch");
    assert_eq!(batch.repo_id.as_deref(), Some("repo-a"));

    let records = store
        .query_records(&RecordQuery {
            profile_id: Some("personal".into()),
            workspace_id: Some("scope".into()),
            ..Default::default()
        })
        .expect("read adopted records");
    assert_eq!(
        records.len(),
        1,
        "scheduled adoption must not clone the source globally"
    );
    assert_eq!(records[0].repo_id.as_deref(), Some("repo-a"));

    let recall = service
        .recall(RecallRequest {
            profile: Some("personal".into()),
            workspace: Some("scope".into()),
            repo: Some(repo("repo-b")),
            session: None,
            query: Some("crimson deployment".into()),
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
        .expect("unrelated repository recall");
    assert!(recall
        .facts
        .iter()
        .all(|fact| fact.repo_id.as_deref() == Some("repo-a")));
}

#[test]
fn scheduled_adoption_rejects_mixed_repository_batch() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), config());
    for (repo_id, content) in [
        (
            "repo-a",
            "Decision: repository alpha uses the crimson deployment.",
        ),
        (
            "repo-b",
            "Decision: repository beta uses the amber deployment.",
        ),
    ] {
        service
            .conclusions(ConclusionsRequest {
                profile: Some("personal".into()),
                workspace: Some("scope".into()),
                repo: Some(repo(repo_id)),
                target: Some("user".into()),
                conclusions: Some(vec![content.into()]),
                metadata: None,
                record_type: Some("decision".into()),
            })
            .expect("repo-scoped source");
    }

    let error = service
        .scheduled_dream(Some("2030-01-01T00:00:00Z".into()))
        .expect_err("mixed repository boundaries must fail closed");
    assert!(error.message.contains("mixes repository boundaries"));

    let records = store
        .query_records(&RecordQuery {
            profile_id: Some("personal".into()),
            workspace_id: Some("scope".into()),
            ..Default::default()
        })
        .expect("read source records");
    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| record.repo_id.as_deref().is_some()));
}
