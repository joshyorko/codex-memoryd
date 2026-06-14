//! Procedure-focused eval suite (issues #145, #146, #150).
//!
//! ProcMEM-style quality measurement for procedural memory, run through the
//! real service paths (preview → apply → recall → lifecycle). Unlike the field's
//! existing procedure benchmarks — which only score retrieval ranking over a
//! corpus where every query has a relevant match — this suite measures the
//! decisions nobody else benchmarks (see `docs/competitive-landscape.md`):
//!
//! - **activation precision / recall** and **false-activation rate** on a fixture
//!   set that includes negatives (queries where no procedure should fire),
//! - **unsafe promotion rate** (vague/poisoned candidates must not become active),
//! - **evidence coverage** (active procedures must cite ≥2 source episodes),
//! - **reuse utility vs. neutral** (procedure recall beats a neutral baseline),
//! - **stale retirement accuracy** (changed-environment procedures retire and
//!   drop out of default recall).
//!
//! Every metric is deterministic and computed offline with no model in the loop.

use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::domain::SubjectKind;
use crate::error::Result;
use crate::protocol::{
    EpisodeCreateRequest, ProceduresApplyRequest, ProceduresPreviewRequest,
    ProceduresRecallRequest, SubjectCreateRequest,
};
use crate::service::Service;
use crate::store::Store;

const PROFILE: &str = "personal";
const WORKSPACE: &str = "proc-eval";

#[derive(Debug, Serialize)]
pub struct ProcedureEvalReport {
    pub suite: &'static str,
    pub version: u32,
    pub status: &'static str,
    pub metrics: ProcedureEvalMetrics,
    pub fixture_families: Vec<&'static str>,
    pub triage: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProcedureEvalMetrics {
    /// Of the queries where the procedure fired, the fraction that should have.
    pub activation_precision: f64,
    /// Of the queries where the procedure should fire, the fraction it did.
    pub activation_recall: f64,
    /// Of the negative queries, the fraction that wrongly fired.
    pub false_activation_rate: f64,
    /// Of weak/unsafe candidates, the fraction that wrongly became active.
    pub unsafe_promotion_rate: f64,
    /// Of active procedures, the fraction citing >= 2 source episodes.
    pub evidence_coverage: f64,
    /// 1.0 when query-conditioned recall beats neutral recall, else 0.0.
    pub reuse_utility_vs_neutral: f64,
    /// 1.0 when a stale (retired) procedure is correctly withheld from recall.
    pub stale_retirement_accuracy: f64,
}

/// An activation fixture: a query and whether the procedure should fire.
struct ActivationCase {
    query: &'static str,
    should_activate: bool,
}

pub fn run_procedure_eval() -> Result<ProcedureEvalReport> {
    let service = eval_service()?;
    let mut triage = Vec::new();

    // --- Fixture 1: a well-supported procedure with negative examples. ---
    let subject = service.create_subject(SubjectCreateRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        subject_key: Some("workflow:pr-review".to_string()),
        kind: Some(SubjectKind::Workflow.as_str().to_string()),
        display_name: Some("PR review workflow".to_string()),
        metadata: Some(json!({"fixture_family": "repeated_success"})),
    })?;

    // Two successful episodes => eligible candidate (single success would not).
    for i in 1..=2 {
        service.create_episode(EpisodeCreateRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            subject_id: Some(subject.subject.id.clone()),
            source_kind: Some("session".to_string()),
            source_ref: Some(format!("proc-eval:pr-{i}")),
            started_at: None,
            ended_at: Some(format!("2030-01-0{i}T00:00:00Z")),
            status: Some("success".to_string()),
            summary: Some(
                "Before opening a pull request, review the diff, run cargo test, and write rollback notes."
                    .to_string(),
            ),
            trust_level: Some("trusted".to_string()),
            source_metadata: None,
            metadata: None,
        })?;
    }

    let preview = service.procedures_preview(ProceduresPreviewRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        subject_id: Some(subject.subject.id.clone()),
        limit: None,
    })?;

    let Some(mut candidate) = preview.candidates.first().cloned() else {
        triage.push("no candidate produced from repeated successful episodes".to_string());
        return Ok(fail_report(triage));
    };
    // Attach a negative example: the procedure must NOT fire on a deploy task.
    candidate
        .negative_examples
        .push("deploying the release to production servers".to_string());

    let applied = service.procedures_apply(ProceduresApplyRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        candidates: vec![candidate],
    })?;
    if applied.applied.len() != 1 {
        triage.push("well-supported candidate was not applied as active".to_string());
    }

    // --- Activation precision / recall / false-activation. ---
    let cases = [
        ActivationCase {
            query: "opening a pull request",
            should_activate: true,
        },
        ActivationCase {
            query: "review the diff before a pull request",
            should_activate: true,
        },
        ActivationCase {
            query: "deploying the release to production servers",
            should_activate: false, // negative example => veto
        },
        ActivationCase {
            query: "schedule the office lunch order",
            should_activate: false, // unrelated => below threshold
        },
    ];

    let (mut tp, mut fp, mut fn_, mut neg_total, mut false_fire) = (0, 0, 0, 0, 0);
    for case in &cases {
        let recalled = service.procedures_recall(ProceduresRecallRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            query: Some(case.query.to_string()),
            subject_id: Some(subject.subject.id.clone()),
            limit: None,
            include_retired: false,
        })?;
        let fired = !recalled.procedures.is_empty();
        if !case.should_activate {
            neg_total += 1;
            if fired {
                false_fire += 1;
            }
        }
        match (case.should_activate, fired) {
            (true, true) => tp += 1,
            (true, false) => fn_ += 1,
            (false, true) => fp += 1,
            (false, false) => {}
        }
    }
    let activation_precision = ratio(tp, tp + fp);
    let activation_recall = ratio(tp, tp + fn_);
    let false_activation_rate = ratio(false_fire, neg_total);
    if false_activation_rate > 0.0 {
        triage.push(format!(
            "false activation rate {false_activation_rate:.2} (procedure fired on a negative query)"
        ));
    }

    // --- Evidence coverage of active procedures. ---
    let active = service.procedures_recall(ProceduresRecallRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        query: None,
        subject_id: Some(subject.subject.id.clone()),
        limit: None,
        include_retired: false,
    })?;
    let covered = active
        .procedures
        .iter()
        .filter(|p| p.source_episode_ids.len() >= 2)
        .count();
    let evidence_coverage = ratio(covered, active.procedures.len().max(1));

    // --- Unsafe promotion: a weak (single-episode) candidate must not promote. ---
    let weak_subject = service.create_subject(SubjectCreateRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        subject_key: Some("workflow:weak".to_string()),
        kind: Some(SubjectKind::Workflow.as_str().to_string()),
        display_name: Some("weak workflow".to_string()),
        metadata: None,
    })?;
    service.create_episode(EpisodeCreateRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        subject_id: Some(weak_subject.subject.id.clone()),
        source_kind: Some("session".to_string()),
        source_ref: Some("proc-eval:weak-1".to_string()),
        started_at: None,
        ended_at: Some("2030-02-01T00:00:00Z".to_string()),
        status: Some("success".to_string()),
        summary: Some("Try restarting the flaky integration once and hope it passes.".to_string()),
        trust_level: Some("trusted".to_string()),
        source_metadata: None,
        metadata: None,
    })?;
    let weak_preview = service.procedures_preview(ProceduresPreviewRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        subject_id: Some(weak_subject.subject.id.clone()),
        limit: None,
    })?;
    // A single-episode subject should yield no eligible candidate (weak_support).
    let unsafe_promotion_rate = if weak_preview.candidates.is_empty() {
        0.0
    } else {
        // Even if surfaced, applying must quarantine it (not activate).
        let weak_apply = service.procedures_apply(ProceduresApplyRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            candidates: weak_preview.candidates.clone(),
        })?;
        ratio(weak_apply.applied.len(), weak_preview.candidates.len())
    };
    if unsafe_promotion_rate > 0.0 {
        triage.push("weak-evidence candidate was promoted to active".to_string());
    }

    // --- Reuse utility vs neutral: query recall returns the procedure, a
    // neutral (unrelated) query does not. ---
    let neutral = service.procedures_recall(ProceduresRecallRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        query: Some("unrelated neutral topic about gardening".to_string()),
        subject_id: Some(subject.subject.id.clone()),
        limit: None,
        include_retired: false,
    })?;
    let targeted = service.procedures_recall(ProceduresRecallRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        query: Some("opening a pull request".to_string()),
        subject_id: Some(subject.subject.id.clone()),
        limit: None,
        include_retired: false,
    })?;
    let reuse_utility_vs_neutral = bool_score(targeted.procedures.len() > neutral.procedures.len());
    if reuse_utility_vs_neutral == 0.0 {
        triage.push("query recall did not beat neutral recall".to_string());
    }

    // --- Stale retirement: retire the procedure, confirm it leaves recall. ---
    let proc_id = applied
        .applied
        .first()
        .map(|p| p.id.clone())
        .unwrap_or_default();
    service.procedure_retire(Some(PROFILE), Some(WORKSPACE), &proc_id)?;
    let after_retire = service.procedures_recall(ProceduresRecallRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        query: Some("opening a pull request".to_string()),
        subject_id: Some(subject.subject.id.clone()),
        limit: None,
        include_retired: false,
    })?;
    let still_present = after_retire.procedures.iter().any(|p| p.id == proc_id);
    // It must still be inspectable when include_retired is set.
    let inspectable = service
        .procedures_recall(ProceduresRecallRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            query: None,
            subject_id: Some(subject.subject.id.clone()),
            limit: None,
            include_retired: true,
        })?
        .procedures
        .iter()
        .any(|p| p.id == proc_id);
    let stale_retirement_accuracy = bool_score(!still_present && inspectable);
    if stale_retirement_accuracy == 0.0 {
        triage.push("retired procedure not correctly withheld/inspectable".to_string());
    }

    let metrics = ProcedureEvalMetrics {
        activation_precision,
        activation_recall,
        false_activation_rate,
        unsafe_promotion_rate,
        evidence_coverage,
        reuse_utility_vs_neutral,
        stale_retirement_accuracy,
    };
    let status = if triage.is_empty() { "pass" } else { "fail" };
    Ok(ProcedureEvalReport {
        suite: "procedure",
        version: 1,
        status,
        metrics,
        fixture_families: vec![
            "repeated_success_promotes",
            "single_success_does_not_promote",
            "negative_example_abstention",
            "similar_but_wrong_no_activation",
            "evidence_coverage",
            "reuse_utility_vs_neutral",
            "stale_retirement",
        ],
        triage,
    })
}

pub fn render_summary(report: &ProcedureEvalReport) -> String {
    let m = &report.metrics;
    format!(
        "codex-memoryd procedure eval: {status}\n\
         activation precision: {ap:.2}\n\
         activation recall: {ar:.2}\n\
         false activation rate: {far:.2}\n\
         unsafe promotion rate: {upr:.2}\n\
         evidence coverage: {ec:.2}\n\
         reuse utility vs neutral: {ru:.2}\n\
         stale retirement accuracy: {sr:.2}\n",
        status = report.status,
        ap = m.activation_precision,
        ar = m.activation_recall,
        far = m.false_activation_rate,
        upr = m.unsafe_promotion_rate,
        ec = m.evidence_coverage,
        ru = m.reuse_utility_vs_neutral,
        sr = m.stale_retirement_accuracy,
    )
}

fn eval_service() -> Result<Service> {
    let store = Store::open(":memory:")?;
    let config = Config {
        default_workspace: WORKSPACE.to_string(),
        ..Default::default()
    };
    let service = Service::new(store, config);
    service.store.ensure_workspace(PROFILE, WORKSPACE)?;
    Ok(service)
}

fn fail_report(triage: Vec<String>) -> ProcedureEvalReport {
    ProcedureEvalReport {
        suite: "procedure",
        version: 1,
        status: "fail",
        metrics: ProcedureEvalMetrics {
            activation_precision: 0.0,
            activation_recall: 0.0,
            false_activation_rate: 1.0,
            unsafe_promotion_rate: 1.0,
            evidence_coverage: 0.0,
            reuse_utility_vs_neutral: 0.0,
            stale_retirement_accuracy: 0.0,
        },
        fixture_families: vec![],
        triage,
    }
}

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

fn bool_score(pass: bool) -> f64 {
    if pass {
        1.0
    } else {
        0.0
    }
}
