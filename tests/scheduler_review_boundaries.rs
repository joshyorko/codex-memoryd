use std::collections::BTreeSet;

use codex_memoryd::config::Config;
use codex_memoryd::domain::{
    Checkpoint, Conclusion, Portability, Profile, Sensitivity, VisibleTurn,
};
use codex_memoryd::ids;
use codex_memoryd::policy;
use codex_memoryd::service::Service;
use codex_memoryd::store::{NewRecord, Store, UpsertOutcome};
use serde_json::json;

const PROFILE: &str = "personal";
const FRONTIER: &str = "2026-09-10T00:00:00Z";
const BEFORE_FRONTIER: &str = "2026-09-09T00:00:00Z";
const RUN_NOW: &str = "2030-01-01T00:00:00Z";

fn scheduler_config(workspace: &str, max_batch_size: usize, automatic_apply: bool) -> Config {
    let mut config = Config::default();
    config.default_profile = PROFILE.into();
    config.default_workspace = workspace.into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.automatic_apply = automatic_apply;
    config.dream_scheduler.scheduled_provider_enabled = false;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_scheduler.min_turn_count = 0;
    config.dream_scheduler.max_batch_size = max_batch_size;
    config.dream_scheduler.max_candidates = 10;
    config
}

fn evidence_ids(run: &codex_memoryd::protocol::DreamResponse) -> BTreeSet<String> {
    run.evidence_window
        .visible_turns
        .sources
        .iter()
        .chain(run.evidence_window.conclusions.sources.iter())
        .chain(run.evidence_window.checkpoints.sources.iter())
        .chain(run.evidence_window.imported_memories.sources.iter())
        .chain(run.evidence_window.active_memory_records.sources.iter())
        .map(|source| source.id.clone())
        .collect()
}

fn insert_chatgpt_turn(
    store: &Store,
    session_id: &str,
    id: &str,
    content: &str,
    created_at: &str,
    repo_id: Option<&str>,
) {
    store
        .ensure_session(
            session_id,
            PROFILE,
            "workspace",
            repo_id,
            Some("thread"),
            "chatgpt-export",
        )
        .expect("session");
    store
        .insert_visible_turn(&VisibleTurn {
            id: id.into(),
            session_id: session_id.into(),
            actor: "user".into(),
            content: content.into(),
            created_at: created_at.into(),
            metadata: json!({
                "origin": "chatgpt-export",
                "conversation_id": "conversation",
                "title": "Imported conversation",
                "message_id": id,
                "turn_index": 1,
            }),
        })
        .expect("visible turn");
}

#[test]
fn scheduler_cursor_keeps_lower_bound_across_all_streams_and_ties() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), scheduler_config("workspace", 3, false));

    service
        .scheduled_dream(Some(FRONTIER.into()))
        .expect("seed watermark");
    store
        .ensure_workspace(PROFILE, "workspace")
        .expect("workspace");
    store
        .ensure_session(
            "frontier-session",
            PROFILE,
            "workspace",
            None,
            None,
            "fixture",
        )
        .expect("session");

    for id in ["visible-a", "visible-b", "visible-c", "visible-d"] {
        store
            .insert_visible_turn(&VisibleTurn {
                id: id.into(),
                session_id: "frontier-session".into(),
                actor: "user".into(),
                content: format!("generic visible evidence {id}"),
                created_at: FRONTIER.into(),
                metadata: json!({}),
            })
            .expect("visible turn");
    }
    store
        .insert_visible_turn(&VisibleTurn {
            id: "visible-old".into(),
            session_id: "frontier-session".into(),
            actor: "user".into(),
            content: "generic visible evidence below the retained frontier".into(),
            created_at: BEFORE_FRONTIER.into(),
            metadata: json!({}),
        })
        .expect("old visible turn");

    for id in ["conclusion-a", "conclusion-b", "conclusion-c"] {
        store
            .insert_conclusion(&Conclusion {
                id: id.into(),
                profile_id: PROFILE.into(),
                workspace_id: "workspace".into(),
                repo_id: None,
                target: "user".into(),
                content: format!("generic conclusion evidence {id}"),
                source_id: None,
                created_at: FRONTIER.into(),
                metadata: json!({}),
            })
            .expect("conclusion");
    }
    store
        .insert_conclusion(&Conclusion {
            id: "conclusion-old".into(),
            profile_id: PROFILE.into(),
            workspace_id: "workspace".into(),
            repo_id: None,
            target: "user".into(),
            content: "generic conclusion below the retained frontier".into(),
            source_id: None,
            created_at: BEFORE_FRONTIER.into(),
            metadata: json!({}),
        })
        .expect("old conclusion");

    for id in ["checkpoint-a", "checkpoint-b", "checkpoint-c"] {
        store
            .insert_checkpoint(&Checkpoint {
                id: id.into(),
                session_id: Some("frontier-session".into()),
                profile_id: PROFILE.into(),
                workspace_id: "workspace".into(),
                repo_id: None,
                summary: format!("generic checkpoint evidence {id}"),
                changed_files: vec![],
                decisions: vec![],
                blockers: vec![],
                next_steps: vec![],
                tests_run: vec![],
                tests_not_run: vec![],
                branch: None,
                commit: None,
                created_at: FRONTIER.into(),
            })
            .expect("checkpoint");
    }
    store
        .insert_checkpoint(&Checkpoint {
            id: "checkpoint-old".into(),
            session_id: Some("frontier-session".into()),
            profile_id: PROFILE.into(),
            workspace_id: "workspace".into(),
            repo_id: None,
            summary: "generic checkpoint below the retained frontier".into(),
            changed_files: vec![],
            decisions: vec![],
            blockers: vec![],
            next_steps: vec![],
            tests_run: vec![],
            tests_not_run: vec![],
            branch: None,
            commit: None,
            created_at: BEFORE_FRONTIER.into(),
        })
        .expect("old checkpoint");

    let (source_a, _) = store
        .upsert_source(
            PROFILE,
            "workspace",
            "fixture",
            Some("source-a"),
            "hash-a",
            &json!({}),
        )
        .expect("source a");
    let (source_old, _) = store
        .upsert_source(
            PROFILE,
            "workspace",
            "fixture",
            Some("source-old"),
            "hash-old",
            &json!({}),
        )
        .expect("old source");
    store
        .transaction(|tx| {
            tx.execute(
                "UPDATE memory_sources SET ingested_at = ?1 WHERE id = ?2",
                rusqlite::params![FRONTIER, source_a.id],
            )?;
            tx.execute(
                "UPDATE memory_sources SET ingested_at = ?1 WHERE id = ?2",
                rusqlite::params![BEFORE_FRONTIER, source_old.id],
            )?;
            Ok(())
        })
        .expect("source timestamps");

    let mut seen = BTreeSet::new();
    for now in [
        "2030-01-01T00:00:00Z",
        "2030-01-01T00:00:01Z",
        "2030-01-01T00:00:02Z",
        "2030-01-01T00:00:03Z",
        "2030-01-01T00:00:04Z",
        "2030-01-01T00:00:05Z",
    ] {
        let result = service
            .scheduled_dream(Some(now.into()))
            .expect("scheduled page");
        if let Some(run) = result.run {
            seen.extend(evidence_ids(&run));
        }
        if result.watermark_after.as_deref() == Some(now) {
            break;
        }
    }

    for old_id in [
        "visible-old",
        "conclusion-old",
        "checkpoint-old",
        source_old.id.as_str(),
    ] {
        assert!(
            !seen.contains(old_id),
            "selected below-bound source {old_id}"
        );
    }
    for current_id in [
        "visible-a",
        "visible-b",
        "visible-c",
        "visible-d",
        "conclusion-a",
        "conclusion-b",
        "conclusion-c",
        "checkpoint-a",
        "checkpoint-b",
        "checkpoint-c",
        source_a.id.as_str(),
    ] {
        assert!(
            seen.contains(current_id),
            "did not visit source {current_id}"
        );
    }
}

#[test]
fn synthetic_repository_evidence_resolves_trusted_session_boundary() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), scheduler_config("workspace", 10, true));
    let content_a = "Preference: keep commit messages terse.";
    let content_b = "Preference: keep commit messages terse and direct.";
    insert_chatgpt_turn(
        &store,
        "repo-session",
        "repo-turn-a",
        content_a,
        "2026-09-01T00:00:00Z",
        Some("repo-a"),
    );
    insert_chatgpt_turn(
        &store,
        "repo-session",
        "repo-turn-b",
        content_b,
        "2026-09-08T00:00:00Z",
        Some("repo-a"),
    );

    let scheduled = service
        .scheduled_dream(Some(RUN_NOW.into()))
        .expect("scheduled adoption");
    let run = scheduled.run.expect("scheduled run");
    let batch = store
        .read_consolidation_batch(&format!("consolidation_{}", run.run_id))
        .expect("read consolidation batch")
        .expect("synthetic repository batch");
    assert_eq!(batch.repo_id.as_deref(), Some("repo-a"));
    assert!(run
        .created
        .iter()
        .filter_map(|id| store.get_record(id).expect("created record"))
        .all(|record| record.repo_id.as_deref() == Some("repo-a")));
}

#[test]
fn archived_exact_hash_does_not_suppress_new_synthetic_evidence() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), scheduler_config("workspace", 10, true));
    let content = "Preference: keep commit messages terse.";
    let class = policy::classify(content, Profile::Personal, false);
    let archived_id = match store
        .upsert_record(&NewRecord {
            profile_id: PROFILE.into(),
            workspace_id: "workspace".into(),
            repo_id: None,
            subject_id: None,
            episode_id: None,
            scope: class.scope,
            record_type: class.record_type,
            content: content.into(),
            related_files: vec![],
            tags: vec![],
            sensitivity: Sensitivity::Personal,
            portability: Portability::ProfileOnly,
            confidence: class.confidence,
            source_ids: vec![],
            content_hash: ids::exact_content_hash(
                PROFILE,
                "workspace",
                None,
                class.record_type.as_str(),
                class.scope.as_str(),
                content,
            ),
            supersedes: vec![],
            metadata: json!({"origin": "fixture"}),
        })
        .expect("archived exact-hash record")
    {
        UpsertOutcome::Created(id) | UpsertOutcome::Skipped(id) => id,
    };
    store
        .archive_records(
            PROFILE,
            Some("workspace"),
            std::slice::from_ref(&archived_id),
        )
        .expect("archive exact-hash record");

    insert_chatgpt_turn(
        &store,
        "global-session",
        "global-turn-a",
        content,
        "2026-09-01T00:00:00Z",
        None,
    );
    insert_chatgpt_turn(
        &store,
        "global-session",
        "global-turn-b",
        content,
        "2026-09-08T00:00:00Z",
        None,
    );

    let scheduled = service
        .scheduled_dream(Some(RUN_NOW.into()))
        .expect("scheduled adoption");
    let run = scheduled.run.expect("scheduled run");
    let batch = store
        .read_consolidation_batch(&format!("consolidation_{}", run.run_id))
        .expect("read consolidation batch")
        .expect("new evidence batch");
    assert_eq!(batch.candidates.len(), 1);
    let readopted = store.get_record(&archived_id).unwrap().unwrap();
    assert!(
        !readopted.archived,
        "fresh evidence must restore default-recall eligibility"
    );
    assert!(
        !readopted.source_ids.is_empty(),
        "re-adoption must retain fresh provenance"
    );
}
