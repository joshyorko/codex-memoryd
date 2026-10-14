use std::collections::BTreeSet;

use codex_memoryd::config::Config;
use codex_memoryd::domain::{
    Checkpoint, Conclusion, Portability, Profile, Sensitivity, VisibleTurn,
};
use codex_memoryd::ids;
use codex_memoryd::policy;
use codex_memoryd::service::Service;
use codex_memoryd::store::{DreamRunRecord, NewRecord, Store, UpsertOutcome};
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
fn scheduler_cursor_keeps_new_evidence_eligible_while_draining() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), scheduler_config("workspace", 2, false));
    service
        .scheduled_dream(Some(BEFORE_FRONTIER.into()))
        .expect("seed watermark");
    store
        .ensure_workspace(PROFILE, "workspace")
        .expect("workspace");

    for (id, created_at) in [
        ("visible-newest-before-drain", "2026-09-10T00:00:00Z"),
        ("visible-middle-before-drain", "2026-09-09T00:00:00Z"),
        ("visible-oldest-before-drain", "2026-09-08T00:00:00Z"),
    ] {
        insert_chatgpt_turn(
            &store,
            "cursor-session",
            id,
            &format!("generic evidence {id}"),
            created_at,
            None,
        );
    }

    let first = service
        .scheduled_dream(Some(RUN_NOW.into()))
        .expect("first bounded scheduler page");
    assert!(first.limits_hit.contains(&"max_input_records".to_string()));
    assert_eq!(
        evidence_ids(first.run.as_ref().expect("first run")).len(),
        2,
        "first page should be bounded to two sources"
    );

    insert_chatgpt_turn(
        &store,
        "cursor-session",
        "visible-arrived-during-drain",
        "generic evidence arrived during the bounded scan",
        "2030-01-01T00:00:01Z",
        None,
    );

    let mut drained = false;
    for tick in 2..=10 {
        let now = format!("2030-01-01T00:00:{tick:02}Z");
        let page = service
            .scheduled_dream(Some(now.into()))
            .expect("drain bounded scheduler page");
        let input_limited = page.limits_hit.contains(&"max_input_records".to_string());
        if !input_limited {
            drained = true;
            break;
        }
    }
    assert!(
        drained,
        "the original bounded window should eventually drain"
    );

    let arrived = service
        .scheduled_dream(Some("2030-01-01T00:00:11Z".into()))
        .expect("scan evidence that arrived during the drain");
    let seen = evidence_ids(arrived.run.as_ref().expect("arrival scan"));
    assert!(
        seen.contains("visible-arrived-during-drain"),
        "newer evidence must remain eligible until the cursor reaches its frontier; evidence={seen:?}; limits={:?}",
        arrived.limits_hit
    );
}

#[test]
fn scheduler_cursor_applies_fixed_frontier_to_checkpoint_queries() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), scheduler_config("workspace", 1, false));
    service
        .scheduled_dream(Some(BEFORE_FRONTIER.into()))
        .expect("seed watermark");
    store
        .ensure_workspace(PROFILE, "workspace")
        .expect("workspace");
    insert_chatgpt_turn(
        &store,
        "checkpoint-frontier-session",
        "visible-before-frontier",
        "generic evidence before the checkpoint frontier",
        "2026-09-10T00:00:00Z",
        None,
    );

    let first = service
        .scheduled_dream(Some(RUN_NOW.into()))
        .expect("first bounded page");
    assert!(
        first.limits_hit.contains(&"max_input_records".to_string()),
        "first page should retain a bounded cursor"
    );
    assert!(
        evidence_ids(first.run.as_ref().expect("first run")).contains("visible-before-frontier")
    );

    store
        .insert_checkpoint(&Checkpoint {
            id: "checkpoint-arrived-after-frontier".into(),
            session_id: None,
            profile_id: PROFILE.into(),
            workspace_id: "workspace".into(),
            repo_id: None,
            summary: "Synthetic checkpoint beyond the saved scan frontier".into(),
            changed_files: vec![],
            decisions: vec![],
            blockers: vec![],
            next_steps: vec![],
            tests_run: vec![],
            tests_not_run: vec![],
            branch: None,
            commit: None,
            created_at: RUN_NOW.into(),
        })
        .expect("late checkpoint");

    let drain = service
        .scheduled_dream(Some("2030-01-01T00:00:02Z".into()))
        .expect("drain the original bounded window");
    let drain_evidence = drain.run.as_ref().map(evidence_ids).unwrap_or_default();
    assert!(
        !drain_evidence.contains("checkpoint-arrived-after-frontier"),
        "a checkpoint after the saved upper frontier must not enter the old scan"
    );

    let next_band = service
        .scheduled_dream(Some("2030-01-01T00:00:03Z".into()))
        .expect("scan the later bounded interval");
    let next_evidence = evidence_ids(next_band.run.as_ref().expect("next band run"));
    assert!(next_evidence.contains("checkpoint-arrived-after-frontier"));
}

#[test]
fn scheduler_cursor_does_not_reselect_the_inclusive_frontier_boundary() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), scheduler_config("workspace", 1, false));
    service
        .scheduled_dream(Some(BEFORE_FRONTIER.into()))
        .expect("seed watermark");
    store
        .ensure_workspace(PROFILE, "workspace")
        .expect("workspace");
    insert_chatgpt_turn(
        &store,
        "boundary-session",
        "visible-at-original-frontier",
        "generic evidence exactly at the original upper frontier",
        RUN_NOW,
        None,
    );

    let first = service
        .scheduled_dream(Some(RUN_NOW.into()))
        .expect("scan inclusive upper frontier");
    assert!(evidence_ids(first.run.as_ref().expect("first run"))
        .contains("visible-at-original-frontier"));

    service
        .scheduled_dream(Some("2030-01-01T00:00:01Z".into()))
        .expect("finish the original band");
    let next_band = service
        .scheduled_dream(Some("2030-01-01T00:00:02Z".into()))
        .expect("scan the next band");
    let next_evidence = next_band.run.as_ref().map(evidence_ids).unwrap_or_default();
    assert!(
        !next_evidence.contains("visible-at-original-frontier"),
        "the inclusive end of the prior band must be exclusive at the next band's start"
    );
}

#[test]
fn scheduler_cursor_picks_up_late_row_before_frontier_keyset_position() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), scheduler_config("workspace", 1, false));
    service
        .scheduled_dream(Some(BEFORE_FRONTIER.into()))
        .expect("seed watermark");
    store
        .ensure_workspace(PROFILE, "workspace")
        .expect("workspace");
    insert_chatgpt_turn(
        &store,
        "frontier-session",
        "visible-z-frontier",
        "generic evidence already at the saved upper frontier",
        RUN_NOW,
        None,
    );
    insert_chatgpt_turn(
        &store,
        "older-session",
        "visible-old-frontier-band",
        "generic evidence below the saved upper frontier",
        "2029-12-31T00:00:00Z",
        None,
    );

    let first = service
        .scheduled_dream(Some(RUN_NOW.into()))
        .expect("scan first row at inclusive frontier");
    assert!(evidence_ids(first.run.as_ref().expect("first run")).contains("visible-z-frontier"));

    insert_chatgpt_turn(
        &store,
        "late-session",
        "visible-a-late-frontier",
        "generic evidence inserted late at the saved upper frontier",
        RUN_NOW,
        None,
    );

    let drain = service
        .scheduled_dream(Some("2030-01-01T00:00:01Z".into()))
        .expect("continue draining the saved band");
    let old_band = evidence_ids(drain.run.as_ref().expect("drain run"));
    assert!(
        old_band.contains("visible-old-frontier-band"),
        "the old bounded band must continue after its rowid position"
    );
    assert!(
        !old_band.contains("visible-a-late-frontier"),
        "a row inserted beyond the saved rowid frontier must wait for the next band"
    );

    service
        .scheduled_dream(Some("2030-01-01T00:00:02Z".into()))
        .expect("finish the original bounded band");
    let next_band = service
        .scheduled_dream(Some("2030-01-01T00:00:03Z".into()))
        .expect("scan the band after the saved frontier");
    assert!(evidence_ids(next_band.run.as_ref().expect("next band run"))
        .contains("visible-a-late-frontier"));
}

#[test]
fn scheduler_cursor_rewinds_persisted_id_only_cursor_before_rowid_paging() {
    let store = Store::open(":memory:").expect("store");
    let service = Service::new(store.clone(), scheduler_config("workspace", 1, false));
    store
        .ensure_workspace(PROFILE, "workspace")
        .expect("workspace");
    store
        .ensure_session(
            "legacy-session",
            PROFILE,
            "workspace",
            None,
            None,
            "fixture",
        )
        .expect("session");
    for id in ["visible-z-legacy-position", "visible-a-legacy-unvisited"] {
        store
            .insert_visible_turn(&VisibleTurn {
                id: id.into(),
                session_id: "legacy-session".into(),
                actor: "user".into(),
                content: format!("generic evidence {id}"),
                created_at: RUN_NOW.into(),
                metadata: json!({}),
            })
            .expect("visible turn");
    }

    let legacy_cursor = format!(
        "dream-cursor-v1:{}",
        json!({
            "lower_bound": BEFORE_FRONTIER,
            "lower_bound_exclusive": false,
            "upper_bound": RUN_NOW,
            "visible_turns": {
                "timestamp": RUN_NOW,
                "id": "visible-z-legacy-position"
            }
        })
    );
    store
        .record_dream_run(&DreamRunRecord {
            run_id: "legacy-cursor-seed".into(),
            profile_id: PROFILE.into(),
            workspace_id: "workspace".into(),
            mode: "preview".into(),
            kind: "scheduled".into(),
            status: "ok_with_limits".into(),
            started_at: "2030-01-01T00:00:00Z".into(),
            completed_at: Some("2030-01-01T00:00:00Z".into()),
            watermark_after: Some(legacy_cursor),
            ..Default::default()
        })
        .expect("persist legacy cursor");

    service
        .scheduled_dream(Some("2030-01-01T00:00:01Z".into()))
        .expect("migrate and rewind the legacy cursor");
    service
        .scheduled_dream(Some("2030-01-01T00:00:02Z".into()))
        .expect("restart the bounded scan with rowid ordering");
    let resumed = service
        .scheduled_dream(Some("2030-01-01T00:00:03Z".into()))
        .expect("continue beyond the legacy ID keyset position");
    assert!(evidence_ids(resumed.run.as_ref().expect("resumed run"))
        .contains("visible-a-legacy-unvisited"));
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
