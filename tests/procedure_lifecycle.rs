//! Procedure lifecycle + activation integration tests (issues #145, #146).
//!
//! Exercises the real service paths: preview → apply → recall (with activation
//! abstention) → retire / supersede / counter-evidence. Asserts that historical
//! procedures drop out of default recall but stay inspectable, and that
//! counter-evidence quarantines a procedure.

use codex_memoryd::config::Config;
use codex_memoryd::domain::SubjectKind;
use codex_memoryd::protocol::{
    EpisodeCreateRequest, ProceduresApplyRequest, ProceduresPreviewRequest,
    ProceduresRecallRequest, SubjectCreateRequest,
};
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;

const PROFILE: &str = "personal";
const WORKSPACE: &str = "lifecycle";

fn service() -> Service {
    let store = Store::open(":memory:").unwrap();
    let config = Config {
        default_workspace: WORKSPACE.to_string(),
        ..Default::default()
    };
    let svc = Service::new(store, config);
    svc.store.ensure_workspace(PROFILE, WORKSPACE).unwrap();
    svc
}

/// Create a subject with two successful episodes and apply the resulting
/// procedure candidate. Returns (subject_id, procedure_id).
fn seed_active_procedure(svc: &Service, key: &str, summary: &str) -> (String, String) {
    let subject = svc
        .create_subject(SubjectCreateRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            subject_key: Some(key.to_string()),
            kind: Some(SubjectKind::Workflow.as_str().to_string()),
            display_name: Some(key.to_string()),
            metadata: None,
        })
        .unwrap();
    for i in 1..=2 {
        svc.create_episode(EpisodeCreateRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            subject_id: Some(subject.subject.id.clone()),
            source_kind: Some("session".to_string()),
            source_ref: Some(format!("{key}-{i}")),
            started_at: None,
            ended_at: Some(format!("2030-01-0{i}T00:00:00Z")),
            status: Some("success".to_string()),
            summary: Some(summary.to_string()),
            trust_level: Some("trusted".to_string()),
            source_metadata: None,
            metadata: None,
        })
        .unwrap();
    }
    let preview = svc
        .procedures_preview(ProceduresPreviewRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            subject_id: Some(subject.subject.id.clone()),
            limit: None,
        })
        .unwrap();
    let candidate = preview.candidates.first().cloned().expect("a candidate");
    let applied = svc
        .procedures_apply(ProceduresApplyRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            candidates: vec![candidate],
        })
        .unwrap();
    let proc_id = applied.applied.first().expect("applied").id.clone();
    (subject.subject.id, proc_id)
}

fn recall(svc: &Service, subject_id: &str, query: Option<&str>, include_retired: bool) -> usize {
    svc.procedures_recall(ProceduresRecallRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        query: query.map(str::to_string),
        subject_id: Some(subject_id.to_string()),
        limit: None,
        include_retired,
    })
    .unwrap()
    .procedures
    .len()
}

#[test]
fn applied_procedure_carries_lifecycle_defaults() {
    let svc = service();
    let (subject_id, _proc_id) = seed_active_procedure(
        &svc,
        "workflow:pr",
        "Before opening a pull request, review the diff and run cargo test.",
    );
    let view = svc
        .procedures_recall(ProceduresRecallRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            query: None,
            subject_id: Some(subject_id),
            limit: None,
            include_retired: false,
        })
        .unwrap();
    let p = &view.procedures[0];
    assert_eq!(p.state, "active");
    assert_eq!(p.version, 1);
    assert!(p.first_seen.is_some(), "first_seen set on apply");
    assert!(p.last_validated.is_some(), "last_validated set on apply");
    assert_eq!(p.counter_evidence_count, 0);
}

#[test]
fn retire_removes_from_default_recall_but_stays_inspectable() {
    let svc = service();
    let (subject_id, proc_id) = seed_active_procedure(
        &svc,
        "workflow:pr",
        "Before opening a pull request, review the diff and run cargo test.",
    );
    assert_eq!(
        recall(&svc, &subject_id, Some("opening a pull request"), false),
        1
    );

    let retired = svc
        .procedure_retire(Some(PROFILE), Some(WORKSPACE), &proc_id)
        .unwrap();
    assert_eq!(retired.state, "retired");
    assert!(retired.retired_at.is_some());

    // Gone from default recall, present when include_retired.
    assert_eq!(
        recall(&svc, &subject_id, Some("opening a pull request"), false),
        0
    );
    assert_eq!(recall(&svc, &subject_id, None, true), 1);
}

#[test]
fn supersede_links_old_to_new_and_bumps_version() {
    let svc = service();
    let (subject_id, old_id) = seed_active_procedure(
        &svc,
        "workflow:deploy",
        "When deploying, run the release pipeline and tag the version.",
    );
    let (_subject2, new_id) = seed_active_procedure(
        &svc,
        "workflow:deploy-v2",
        "When deploying, run the canary pipeline, verify health, then tag the version.",
    );

    let new_view = svc
        .procedure_supersede(Some(PROFILE), Some(WORKSPACE), &old_id, &new_id)
        .unwrap();
    assert_eq!(new_view.id, new_id);
    assert_eq!(new_view.version, 2, "successor version bumped past old");

    // The old procedure is superseded and links to the successor.
    let old = svc
        .store
        .get_procedure(PROFILE, WORKSPACE, &old_id)
        .unwrap()
        .unwrap();
    assert_eq!(old.state, "superseded");
    assert_eq!(old.superseded_by.as_deref(), Some(new_id.as_str()));

    // Superseded procedure is withheld from default recall on the old subject.
    assert_eq!(recall(&svc, &subject_id, None, false), 0);
}

#[test]
fn counter_evidence_quarantines_after_threshold() {
    let svc = service();
    let (subject_id, proc_id) = seed_active_procedure(
        &svc,
        "workflow:flaky",
        "When the build fails, clear the cache and rerun the failing job.",
    );
    assert_eq!(recall(&svc, &subject_id, None, false), 1);

    // First counter-evidence: still active (threshold 2).
    let after_one = svc
        .procedure_counter_evidence(Some(PROFILE), Some(WORKSPACE), &proc_id, 2)
        .unwrap();
    assert_eq!(after_one.counter_evidence_count, 1);
    assert_eq!(after_one.state, "active");

    // Second counter-evidence reaches the threshold: quarantined.
    let after_two = svc
        .procedure_counter_evidence(Some(PROFILE), Some(WORKSPACE), &proc_id, 2)
        .unwrap();
    assert_eq!(after_two.counter_evidence_count, 2);
    assert_eq!(after_two.state, "quarantined");

    // Quarantined procedure withheld from default recall.
    assert_eq!(recall(&svc, &subject_id, None, false), 0);
}

#[test]
fn recall_abstains_on_negative_example() {
    let svc = service();
    let subject = svc
        .create_subject(SubjectCreateRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            subject_key: Some("workflow:prod-deploy".to_string()),
            kind: Some(SubjectKind::Workflow.as_str().to_string()),
            display_name: Some("prod deploy".to_string()),
            metadata: None,
        })
        .unwrap();
    for i in 1..=2 {
        svc.create_episode(EpisodeCreateRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            subject_id: Some(subject.subject.id.clone()),
            source_kind: Some("session".to_string()),
            source_ref: Some(format!("prod-{i}")),
            started_at: None,
            ended_at: Some(format!("2030-03-0{i}T00:00:00Z")),
            status: Some("success".to_string()),
            summary: Some(
                "When deploying to production, run the release pipeline and tag the version."
                    .to_string(),
            ),
            trust_level: Some("trusted".to_string()),
            source_metadata: None,
            metadata: None,
        })
        .unwrap();
    }
    let preview = svc
        .procedures_preview(ProceduresPreviewRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            subject_id: Some(subject.subject.id.clone()),
            limit: None,
        })
        .unwrap();
    let mut candidate = preview.candidates.first().cloned().expect("candidate");
    candidate
        .negative_examples
        .push("deploying to a local development sandbox".to_string());
    svc.procedures_apply(ProceduresApplyRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        candidates: vec![candidate],
    })
    .unwrap();

    // Fires on the production deploy task...
    assert_eq!(
        recall(
            &svc,
            &subject.subject.id,
            Some("deploying to production"),
            false
        ),
        1
    );
    // ...but abstains on the negative (sandbox) task.
    assert_eq!(
        recall(
            &svc,
            &subject.subject.id,
            Some("deploying to a local development sandbox"),
            false
        ),
        0
    );
}
