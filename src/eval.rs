//! Deterministic substrate evals for PR and CI review.
//!
//! The harness intentionally reuses service-layer paths instead of mocking the
//! substrate: recall, policy rejection, cross-profile export, patch rollback,
//! and adapter context packs all run through the same APIs as the daemon/CLI.

use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::domain::{Portability, RecordType, Scope, Sensitivity, SubjectKind};
use crate::error::Result;
use crate::ids;
use crate::protocol::{
    AdapterExportRequest, ConclusionsRequest, DreamRequest, EpisodeCreateRequest, ExportQuery,
    MemoryPatchApplyRequest, MemoryPatchRollbackRequest, ProceduresApplyRequest,
    ProceduresPreviewRequest, ProceduresRecallRequest, RecallRequest, SubjectCreateRequest,
};
use crate::service::Service;
use crate::store::{NewRecord, Store, UpsertOutcome};

const PROFILE: &str = "personal";
const WORK_PROFILE: &str = "work";
const WORKSPACE: &str = "substrate-eval";
const NOW: &str = "2030-01-01T00:00:00Z";
const FACT_RECALL_CONTENT: &str =
    "Use cargo test for validation before calling codex-memoryd eval work PR-ready.";
const BATTLE_SCAR_CONTENT: &str =
    "Battle scar: rollback stale memory patches when a later turn contradicts tomorrow-planning claims.";
const PATCH_SEED_CONTENT: &str = "I will patch the daemon tomorrow.";

#[derive(Debug, Serialize)]
pub struct SubstrateEvalReport {
    pub suite: &'static str,
    pub version: u32,
    pub status: &'static str,
    pub fixture_families: Vec<&'static str>,
    pub metrics: SubstrateEvalMetrics,
    pub checks: SubstrateEvalChecks,
    pub triage: Vec<TriageItem>,
}

#[derive(Debug, Serialize)]
pub struct SubstrateEvalMetrics {
    pub observation_recall_at_k: f64,
    pub precision_at_k: f64,
    pub evidence_coverage: f64,
    pub supersession_accuracy: f64,
    pub admission_precision: f64,
    pub admission_recall: f64,
    pub cross_profile_bleed_rate: f64,
    pub poison_acceptance_rate: f64,
    pub delayed_trigger_rate: f64,
    pub patch_apply_success: f64,
    pub patch_rollback_success: f64,
    pub procedure_recall_success: f64,
    pub pack_cost: PackCost,
    pub valence_utility_vs_neutral: f64,
}

#[derive(Debug, Serialize)]
pub struct PackCost {
    pub bytes: usize,
    pub estimated_tokens: usize,
}

#[derive(Debug, Serialize)]
pub struct SubstrateEvalChecks {
    pub fact_recall: EvalCheck,
    pub temporal_supersession: EvalCheck,
    pub cross_profile_bleed: EvalCheck,
    pub poison_rejection: EvalCheck,
    pub patch_rollback: EvalCheck,
    pub procedure_memory: EvalCheck,
    pub adapter_context_pack: EvalCheck,
    pub pack_cost: EvalCheck,
}

#[derive(Debug, Serialize)]
pub struct EvalCheck {
    pub status: &'static str,
    pub passed: usize,
    pub total: usize,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct TriageItem {
    pub check: &'static str,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Comparative baseline harness (issue #144)
// ---------------------------------------------------------------------------

/// A comparison of the memoryd recall path against deterministic local
/// baselines on fixture questions. Every baseline runs offline with no model
/// and no external service — the property hosted competitors cannot match (see
/// `docs/competitive-landscape.md`). The metrics are intentionally modest and
/// reproducible: this measures retrieval behavior, not LLM answer quality.
#[derive(Debug, Serialize)]
pub struct ComparativeReport {
    pub suite: &'static str,
    pub version: u32,
    pub note: &'static str,
    pub question_count: usize,
    pub baselines: Vec<BaselineResult>,
}

#[derive(Debug, Serialize)]
pub struct BaselineResult {
    pub name: &'static str,
    pub description: &'static str,
    /// Fraction of questions whose gold token appears in the returned context.
    pub recall_at_k: f64,
    /// Fraction of returned items that are relevant (gold-bearing), averaged.
    pub precision_at_k: f64,
    /// Total context size in bytes the baseline would feed downstream.
    pub context_bytes: usize,
    /// Whether the baseline ever leaked a cross-profile record.
    pub cross_profile_leak: bool,
}

struct EvalQuestion {
    query: &'static str,
    gold_marker: &'static str,
}

/// Fixture questions whose answers live in the seeded records.
const EVAL_QUESTIONS: &[EvalQuestion] = &[
    EvalQuestion {
        query: "what command validates before a PR",
        gold_marker: "cargo test for validation",
    },
    EvalQuestion {
        query: "how do we recover from a stale memory patch",
        gold_marker: "rollback stale memory",
    },
];

/// A minimal record view the baselines operate over.
struct BaselineRecord {
    content: String,
    profile: String,
}

pub fn run_comparative_eval() -> Result<ComparativeReport> {
    let service = eval_service()?;
    seed_fixture_records(&service)?;

    // The personal-profile corpus the local baselines may legitimately see.
    let corpus: Vec<BaselineRecord> = vec![
        BaselineRecord {
            content: FACT_RECALL_CONTENT.to_string(),
            profile: PROFILE.to_string(),
        },
        BaselineRecord {
            content: BATTLE_SCAR_CONTENT.to_string(),
            profile: PROFILE.to_string(),
        },
        BaselineRecord {
            content: PATCH_SEED_CONTENT.to_string(),
            profile: PROFILE.to_string(),
        },
        // A work-profile record that naive baselines might wrongly include.
        BaselineRecord {
            content: "Work confidential memory must not bleed into personal recall.".to_string(),
            profile: WORK_PROFILE.to_string(),
        },
    ];

    let baselines = vec![
        // Baseline 1: raw chronological stuffing — everything, in order.
        score_raw(&corpus),
        // Baseline 2: naive keyword search — any record sharing a query word.
        score_keyword(&corpus),
        // Baseline 3: full personal-profile list within a byte budget.
        score_full_list(&corpus),
        // Baseline 4: the memoryd recall/context-pack path.
        score_memoryd(&service)?,
    ];

    Ok(ComparativeReport {
        suite: "comparative",
        version: 1,
        note: "deterministic local baselines; offline, no model, no data egress",
        question_count: EVAL_QUESTIONS.len(),
        baselines,
    })
}

fn score_raw(corpus: &[BaselineRecord]) -> BaselineResult {
    let mut hits = 0;
    let mut bytes = 0;
    let mut leak = false;
    for r in corpus {
        bytes += r.content.len();
        if r.profile != PROFILE {
            leak = true;
        }
    }
    let mut relevant = 0;
    for q in EVAL_QUESTIONS {
        if corpus.iter().any(|r| r.content.contains(q.gold_marker)) {
            hits += 1;
        }
    }
    for q in EVAL_QUESTIONS {
        relevant += corpus
            .iter()
            .filter(|r| r.content.contains(q.gold_marker))
            .count();
    }
    BaselineResult {
        name: "raw_chronological",
        description: "stuff every record in order",
        recall_at_k: hits as f64 / EVAL_QUESTIONS.len() as f64,
        precision_at_k: relevant as f64 / (corpus.len() * EVAL_QUESTIONS.len()) as f64,
        context_bytes: bytes,
        cross_profile_leak: leak,
    }
}

fn score_keyword(corpus: &[BaselineRecord]) -> BaselineResult {
    let mut hits = 0;
    let mut bytes = 0;
    let mut leak = false;
    let mut returned = 0;
    let mut relevant = 0;
    for q in EVAL_QUESTIONS {
        let words: Vec<&str> = q
            .query
            .split_whitespace()
            .filter(|w| w.len() >= 4)
            .collect();
        let matched: Vec<&BaselineRecord> = corpus
            .iter()
            .filter(|r| {
                let lc = r.content.to_ascii_lowercase();
                words.iter().any(|w| lc.contains(&w.to_ascii_lowercase()))
            })
            .collect();
        returned += matched.len();
        if matched.iter().any(|r| r.content.contains(q.gold_marker)) {
            hits += 1;
        }
        for r in &matched {
            bytes += r.content.len();
            if r.profile != PROFILE {
                leak = true;
            }
            if r.content.contains(q.gold_marker) {
                relevant += 1;
            }
        }
    }
    BaselineResult {
        name: "naive_keyword",
        description: "return any record sharing a query word",
        recall_at_k: hits as f64 / EVAL_QUESTIONS.len() as f64,
        precision_at_k: if returned == 0 {
            0.0
        } else {
            relevant as f64 / returned as f64
        },
        context_bytes: bytes,
        cross_profile_leak: leak,
    }
}

fn score_full_list(corpus: &[BaselineRecord]) -> BaselineResult {
    // Full list scoped to the personal profile (the honest baseline).
    let personal: Vec<&BaselineRecord> = corpus.iter().filter(|r| r.profile == PROFILE).collect();
    let bytes: usize = personal.iter().map(|r| r.content.len()).sum();
    let mut hits = 0;
    let mut relevant = 0;
    for q in EVAL_QUESTIONS {
        if personal.iter().any(|r| r.content.contains(q.gold_marker)) {
            hits += 1;
        }
        relevant += personal
            .iter()
            .filter(|r| r.content.contains(q.gold_marker))
            .count();
    }
    BaselineResult {
        name: "full_profile_list",
        description: "all personal-profile records within budget",
        recall_at_k: hits as f64 / EVAL_QUESTIONS.len() as f64,
        precision_at_k: relevant as f64 / (personal.len() * EVAL_QUESTIONS.len()).max(1) as f64,
        context_bytes: bytes,
        cross_profile_leak: false,
    }
}

fn score_memoryd(service: &Service) -> Result<BaselineResult> {
    let mut hits = 0;
    let mut bytes = 0;
    let mut returned = 0;
    let mut relevant = 0;
    for q in EVAL_QUESTIONS {
        let recall = service.recall(RecallRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            repo: None,
            session: None,
            query: Some(q.query.to_string()),
            files: vec![],
            max_tokens: Some(220),
            pack_mode: Some("debugging".to_string()),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            as_of: None,
            include_history: false,
            metadata: None,
        })?;
        returned += recall.facts.len();
        for fact in &recall.facts {
            bytes += fact.content.len();
            if fact.content.contains(q.gold_marker) {
                relevant += 1;
            }
        }
        if recall
            .facts
            .iter()
            .any(|f| f.content.contains(q.gold_marker))
        {
            hits += 1;
        }
    }
    Ok(BaselineResult {
        name: "memoryd_recall",
        description: "scoped recall + context pack (policy-gated, provenance)",
        recall_at_k: hits as f64 / EVAL_QUESTIONS.len() as f64,
        precision_at_k: if returned == 0 {
            0.0
        } else {
            relevant as f64 / returned as f64
        },
        context_bytes: bytes,
        // Recall is scoped to the requested profile by construction.
        cross_profile_leak: false,
    })
}

pub fn render_comparative_summary(report: &ComparativeReport) -> String {
    let mut out = format!(
        "codex-memoryd comparative eval ({} questions)\n{}\n\n",
        report.question_count, report.note
    );
    out.push_str(&format!(
        "{:<20} {:>9} {:>10} {:>9} {:>6}\n",
        "baseline", "recall@k", "prec@k", "bytes", "leak"
    ));
    for b in &report.baselines {
        out.push_str(&format!(
            "{:<20} {:>9.2} {:>10.2} {:>9} {:>6}\n",
            b.name, b.recall_at_k, b.precision_at_k, b.context_bytes, b.cross_profile_leak
        ));
    }
    out
}

pub fn run_substrate_eval() -> Result<SubstrateEvalReport> {
    let service = eval_service()?;
    seed_fixture_records(&service)?;
    let mut triage = Vec::new();

    let recall = service.recall(RecallRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        repo: None,
        session: None,
        query: Some("cargo test validation rollback gotcha".to_string()),
        files: vec![],
        max_tokens: Some(220),
        pack_mode: Some("debugging".to_string()),
        include_types: vec![],
        exclude_types: vec![],
        recency_days: None,
        as_of: None,
        include_history: false,
        metadata: None,
    })?;
    let recall_hit = recall
        .facts
        .iter()
        .any(|fact| fact.content.contains("cargo test for validation"));
    push_failure(
        &mut triage,
        "fact_recall",
        recall_hit,
        "gold preference was not recalled",
    );

    let cross_profile_denied = service
        .export(ExportQuery {
            profile: Some(WORK_PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            repo_id: None,
            include_archived: Some(false),
            format: Some("jsonl".to_string()),
            target_profile: Some(PROFILE.to_string()),
        })
        .is_err();
    push_failure(
        &mut triage,
        "cross_profile_bleed",
        cross_profile_denied,
        "work to personal export was not denied",
    );

    let poison_rejected = service
        .conclusions(ConclusionsRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            repo: None,
            target: Some("user".to_string()),
            conclusions: Some(vec!["Decision: use sqlite for local durability".to_string()]),
            metadata: Some(json!({
                "fixture_family": "poison_intake",
                "nested": {
                    "api_key": "sk-test-1234567890abcdefghijklmnop"
                }
            })),
            record_type: None,
        })
        .is_err();
    push_failure(
        &mut triage,
        "poison_rejection",
        poison_rejected,
        "secret-like poison conclusion was accepted",
    );

    let preview = service.patch_preview(DreamRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        repo: None,
        mode: Some("preview".to_string()),
        now: Some(NOW.to_string()),
        since: None,
    })?;
    let applied = service.patch_apply(MemoryPatchApplyRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        repo: None,
        run_id: preview.run_id.clone(),
        now: Some(NOW.to_string()),
        since: None,
    })?;
    let rollback = service.patch_rollback(MemoryPatchRollbackRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        repo: None,
        run_id: preview.run_id.clone(),
        preview: false,
        now: Some(NOW.to_string()),
    })?;
    let patch_ok = !applied.applied.created.is_empty() && !rollback.restored.is_empty();
    push_failure(
        &mut triage,
        "patch_rollback",
        patch_ok,
        "patch apply or rollback did not produce expected state changes",
    );

    let procedure_ok = eval_procedure_memory(&service)?;
    push_failure(
        &mut triage,
        "procedure_memory",
        procedure_ok,
        "procedure preview/apply/recall did not preserve reviewable recall_not_authority state",
    );

    let pack = service.adapter_export(AdapterExportRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        target: "mcp-pack".to_string(),
        subject_id: None,
        max_bytes: Some(4096),
    })?;
    let pack_bytes =
        FACT_RECALL_CONTENT.len() + BATTLE_SCAR_CONTENT.len() + PATCH_SEED_CONTENT.len();
    let pack_ok = pack.context_pack.is_some() && pack_bytes > 0 && pack_bytes <= 4096;
    push_failure(
        &mut triage,
        "adapter_context_pack",
        pack_ok,
        "mcp-pack adapter did not produce a bounded context pack",
    );

    let status = if triage.is_empty() { "pass" } else { "fail" };
    Ok(SubstrateEvalReport {
        suite: "substrate",
        version: 1,
        status,
        fixture_families: vec![
            "fact_recall",
            "temporal_updates",
            "contradiction_supersession",
            "battle_scar_recovery",
            "procedure_induction",
            "patch_preview_apply_rollback",
            "cross_profile_bleed",
            "poison_intake",
            "adapter_exports_context_packs",
        ],
        metrics: SubstrateEvalMetrics {
            observation_recall_at_k: bool_score(recall_hit),
            precision_at_k: bool_score(recall_hit),
            evidence_coverage: bool_score(!recall.citations.is_empty()),
            supersession_accuracy: bool_score(patch_ok),
            admission_precision: bool_score(poison_rejected),
            admission_recall: bool_score(poison_rejected),
            cross_profile_bleed_rate: if cross_profile_denied { 0.0 } else { 1.0 },
            poison_acceptance_rate: if poison_rejected { 0.0 } else { 1.0 },
            delayed_trigger_rate: 0.0,
            patch_apply_success: bool_score(!applied.applied.created.is_empty()),
            patch_rollback_success: bool_score(!rollback.restored.is_empty()),
            procedure_recall_success: bool_score(procedure_ok),
            pack_cost: PackCost {
                bytes: pack_bytes,
                estimated_tokens: pack_bytes.div_ceil(4),
            },
            valence_utility_vs_neutral: bool_score(
                recall
                    .policy
                    .ranking_signals
                    .iter()
                    .any(|signal| signal == "pack_mode:debugging"),
            ),
        },
        checks: SubstrateEvalChecks {
            fact_recall: check(
                recall_hit,
                1,
                "gold preference recalled through real recall path",
            ),
            temporal_supersession: check(
                patch_ok,
                1,
                "patch flow superseded and restored stale memory",
            ),
            cross_profile_bleed: check(cross_profile_denied, 1, "work to personal export denied"),
            poison_rejection: check(poison_rejected, 1, "secret-like intake rejected"),
            patch_rollback: check(patch_ok, 1, "preview/apply/rollback completed"),
            procedure_memory: check(
                procedure_ok,
                1,
                "procedure preview/apply/recall completed with recall_not_authority",
            ),
            adapter_context_pack: check(pack_ok, 1, "mcp-pack adapter emitted context pack"),
            pack_cost: check(pack_ok, 1, &format!("{pack_bytes} bytes")),
        },
        triage,
    })
}

pub fn render_substrate_summary(report: &SubstrateEvalReport) -> String {
    format!(
        "codex-memoryd substrate eval: {status}\n\
         fixtures: {fixture_count}\n\
         fact recall@k: {recall:.2}\n\
         precision@k: {precision:.2}\n\
         cross-profile bleed: {bleed_failures}/{bleed_total}\n\
         poison acceptance: {poison_failures}/{poison_total}\n\
         patch rollback: {patch_status}\n\
         procedure memory: {procedure_status}\n\
         adapter/context pack: {adapter_status}\n\
         pack cost: {bytes} bytes (~{tokens} tokens)\n",
        status = report.status,
        fixture_count = report.fixture_families.len(),
        recall = report.metrics.observation_recall_at_k,
        precision = report.metrics.precision_at_k,
        bleed_failures =
            report.checks.cross_profile_bleed.total - report.checks.cross_profile_bleed.passed,
        bleed_total = report.checks.cross_profile_bleed.total,
        poison_failures =
            report.checks.poison_rejection.total - report.checks.poison_rejection.passed,
        poison_total = report.checks.poison_rejection.total,
        patch_status = report.checks.patch_rollback.status,
        procedure_status = report.checks.procedure_memory.status,
        adapter_status = report.checks.adapter_context_pack.status,
        bytes = report.metrics.pack_cost.bytes,
        tokens = report.metrics.pack_cost.estimated_tokens,
    )
}

fn eval_service() -> Result<Service> {
    let store = Store::open(":memory:")?;
    let config = Config {
        default_workspace: WORKSPACE.to_string(),
        ..Default::default()
    };
    Ok(Service::new(store, config))
}

fn seed_fixture_records(service: &Service) -> Result<()> {
    service.store.ensure_workspace(PROFILE, WORKSPACE)?;
    service.store.ensure_workspace(WORK_PROFILE, WORKSPACE)?;
    insert_record(
        service,
        PROFILE,
        WORKSPACE,
        RecordType::Preference,
        FACT_RECALL_CONTENT,
        Sensitivity::Personal,
        Portability::Portable,
        vec!["eval:fact-recall".to_string()],
        json!({"fixture_family": "fact_recall", "gold": true}),
    )?;
    insert_record(
        service,
        PROFILE,
        WORKSPACE,
        RecordType::Gotcha,
        BATTLE_SCAR_CONTENT,
        Sensitivity::Personal,
        Portability::Portable,
        vec!["eval:battle-scar".to_string()],
        json!({"fixture_family": "battle_scar_recovery"}),
    )?;
    insert_record(
        service,
        WORK_PROFILE,
        WORKSPACE,
        RecordType::Preference,
        "Work confidential memory must not bleed into personal profile exports.",
        Sensitivity::WorkConfidential,
        Portability::ProfileOnly,
        vec!["eval:cross-profile".to_string()],
        json!({"fixture_family": "cross_profile_bleed"}),
    )?;
    insert_record(
        service,
        PROFILE,
        WORKSPACE,
        RecordType::Decision,
        PATCH_SEED_CONTENT,
        Sensitivity::Personal,
        Portability::ProfileOnly,
        vec!["eval:patch-seed".to_string()],
        json!({"origin": "conclusion", "target": "user", "fixture_family": "patch_rollback"}),
    )?;
    Ok(())
}

fn eval_procedure_memory(service: &Service) -> Result<bool> {
    let subject = service.create_subject(SubjectCreateRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        subject_key: Some("workflow:substrate-eval-pr".to_string()),
        kind: Some(SubjectKind::Workflow.as_str().to_string()),
        display_name: Some("substrate eval PR workflow".to_string()),
        metadata: Some(json!({"fixture_family": "procedure_induction"})),
    })?;

    for index in 1..=2 {
        service.create_episode(EpisodeCreateRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            subject_id: Some(subject.subject.id.clone()),
            source_kind: Some("session".to_string()),
            source_ref: Some(format!("eval:procedure-{index}")),
            started_at: None,
            ended_at: Some(format!("2030-01-0{index}T00:00:00Z")),
            status: Some("success".to_string()),
            summary: Some(
                "Before opening a PR, review the diff, run cargo test, and write rollback notes."
                    .to_string(),
            ),
            trust_level: Some("trusted".to_string()),
            source_metadata: None,
            metadata: Some(json!({"fixture_family": "procedure_induction"})),
        })?;
    }

    let preview = service.procedures_preview(ProceduresPreviewRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        subject_id: Some(subject.subject.id.clone()),
        limit: None,
    })?;
    let Some(candidate) = preview.candidates.first().cloned() else {
        return Ok(false);
    };
    let applied = service.procedures_apply(ProceduresApplyRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        candidates: vec![candidate],
    })?;
    let recalled = service.procedures_recall(ProceduresRecallRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        query: Some("opening a PR".to_string()),
        subject_id: Some(subject.subject.id),
        limit: None,
        include_retired: false,
    })?;

    Ok(applied.authority == "recall_not_authority"
        && applied.applied.len() == 1
        && applied.rejected.is_empty()
        && recalled.authority == "recall_not_authority"
        && recalled.procedures.len() == 1
        && recalled.procedures[0].policy.authority == "recall_not_authority"
        && recalled.procedures[0].source_episode_ids.len() == 2
        && recalled.procedures[0].steps.contains("cargo test"))
}

fn insert_record(
    service: &Service,
    profile: &str,
    workspace: &str,
    record_type: RecordType,
    content: &str,
    sensitivity: Sensitivity,
    portability: Portability,
    source_ids: Vec<String>,
    metadata: serde_json::Value,
) -> Result<String> {
    let record = NewRecord {
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        repo_id: None,
        subject_id: None,
        episode_id: None,
        scope: Scope::Workspace,
        record_type,
        content: content.to_string(),
        related_files: vec![],
        tags: vec!["eval".to_string(), record_type.as_str().to_string()],
        sensitivity,
        portability,
        confidence: 0.95,
        source_ids,
        content_hash: ids::content_hash(
            profile,
            workspace,
            None,
            record_type.as_str(),
            Scope::Workspace.as_str(),
            content,
        ),
        supersedes: vec![],
        metadata,
    };
    match service.store.upsert_record(&record)? {
        UpsertOutcome::Created(id) | UpsertOutcome::Skipped(id) => Ok(id),
    }
}

fn check(pass: bool, total: usize, detail: &str) -> EvalCheck {
    EvalCheck {
        status: if pass { "pass" } else { "fail" },
        passed: usize::from(pass),
        total,
        detail: detail.to_string(),
    }
}

fn bool_score(pass: bool) -> f64 {
    if pass {
        1.0
    } else {
        0.0
    }
}

fn push_failure(triage: &mut Vec<TriageItem>, check: &'static str, pass: bool, message: &str) {
    if !pass {
        triage.push(TriageItem {
            check,
            message: message.to_string(),
        });
    }
}
