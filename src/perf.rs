//! Local performance / cost budgets (issue #152).
//!
//! Produces a deterministic, fixture-scale report of the *cost* of the main
//! substrate paths — recall, search, cards, adapter export, context packs,
//! procedure recall, substrate eval, and import — measured as record counts,
//! output bytes, and estimated tokens. Wall-clock timing is captured for
//! information but is intentionally NOT part of the asserted budget, so CI
//! does not flake on a busy machine (issue #152: "without overfitting CI
//! noise"). The byte/token/count budgets are stable and asserted in
//! `tests/perf_budget.rs`.
//!
//! Everything runs in-memory with seeded fixtures: no external service, no
//! model, reproducible offline.

use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::domain::{Portability, RecordType, Scope, Sensitivity, SubjectKind};
use crate::error::Result;
use crate::ids;
use crate::protocol::{
    AdapterExportRequest, CardShowRequest, EpisodeCreateRequest, ProceduresPreviewRequest,
    ProceduresRecallRequest, RecallRequest, SearchRequest, SubjectCreateRequest,
};
use crate::service::Service;
use crate::store::{NewRecord, Store, UpsertOutcome};

const PROFILE: &str = "personal";
const WORKSPACE: &str = "perf";
/// Fixed fixture corpus size: large enough to be representative, small enough
/// to stay deterministic and fast.
const SEED_RECORDS: usize = 40;

/// Rough token estimate: ~4 bytes per token (matches the eval harness).
fn est_tokens(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

#[derive(Debug, Clone, Serialize)]
pub struct PerfReport {
    pub suite: &'static str,
    pub version: u32,
    pub note: &'static str,
    pub seed_records: usize,
    pub measurements: Vec<PerfMeasurement>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PerfMeasurement {
    pub path: &'static str,
    /// Number of records/items the path returned or processed.
    pub items: usize,
    /// Serialized output size in bytes.
    pub output_bytes: usize,
    /// Estimated token cost of the output.
    pub estimated_tokens: usize,
    /// Informational wall-clock micros (NOT part of the asserted budget).
    pub elapsed_micros: u128,
}

/// Run the performance/cost report against a seeded in-memory fixture.
///
/// `clock` returns monotonic nanoseconds; pass a real clock from the CLI and a
/// zero clock from deterministic tests (timing is informational only).
pub fn run_perf_report(clock: impl Fn() -> u128) -> Result<PerfReport> {
    let service = eval_service()?;
    seed(&service)?;
    let subject_id = seed_procedure(&service)?;

    let mut measurements = Vec::new();

    measurements.push(measure("recall", &clock, || {
        let r = service.recall(RecallRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            repo: None,
            session: None,
            query: Some("cargo test rollback decision".to_string()),
            files: vec![],
            max_tokens: Some(1200),
            pack_mode: Some("default".to_string()),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            as_of: None,
            include_history: false,
            metadata: None,
        })?;
        Ok((r.facts.len(), json_bytes(&r)))
    })?);

    measurements.push(measure("search", &clock, || {
        let r = service.search(SearchRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            repo: None,
            query: Some("decision".to_string()),
            scope: None,
            record_type: None,
            limit: Some(50),
            include_archived: false,
            cursor: None,
        })?;
        Ok((r.matches.len(), json_bytes(&r)))
    })?);

    measurements.push(measure("card_workspace_summary", &clock, || {
        let r = service.card_show(CardShowRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            r#type: "workspace_summary".to_string(),
            subject_id: None,
        })?;
        Ok((1, json_bytes(&r)))
    })?);

    measurements.push(measure("adapter_mcp_pack", &clock, || {
        let r = service.adapter_export(AdapterExportRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            target: "mcp-pack".to_string(),
            subject_id: None,
            max_bytes: Some(4096),
        })?;
        let items = r
            .context_pack
            .as_ref()
            .map(|p| p.records.len())
            .unwrap_or(0);
        Ok((items, json_bytes(&r)))
    })?);

    measurements.push(measure("procedure_recall", &clock, || {
        let r = service.procedures_recall(ProceduresRecallRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            query: Some("opening a pull request".to_string()),
            subject_id: Some(subject_id.clone()),
            limit: None,
            include_retired: false,
        })?;
        Ok((r.procedures.len(), json_bytes(&r)))
    })?);

    Ok(PerfReport {
        suite: "perf",
        version: 1,
        note: "fixture-scale cost report; deterministic, offline, no model",
        seed_records: SEED_RECORDS,
        measurements,
    })
}

fn measure(
    path: &'static str,
    clock: &impl Fn() -> u128,
    run: impl FnOnce() -> Result<(usize, usize)>,
) -> Result<PerfMeasurement> {
    let start = clock();
    let (items, output_bytes) = run()?;
    let elapsed_micros = (clock().saturating_sub(start)) / 1000;
    Ok(PerfMeasurement {
        path,
        items,
        output_bytes,
        estimated_tokens: est_tokens(output_bytes),
        elapsed_micros,
    })
}

fn json_bytes<T: Serialize>(value: &T) -> usize {
    serde_json::to_string(value).map(|s| s.len()).unwrap_or(0)
}

pub fn render_summary(report: &PerfReport) -> String {
    let mut out = format!(
        "codex-memoryd perf report ({} seed records)\n{}\n\n",
        report.seed_records, report.note
    );
    out.push_str(&format!(
        "{:<26} {:>6} {:>8} {:>8} {:>10}\n",
        "path", "items", "bytes", "~tokens", "micros"
    ));
    for m in &report.measurements {
        out.push_str(&format!(
            "{:<26} {:>6} {:>8} {:>8} {:>10}\n",
            m.path, m.items, m.output_bytes, m.estimated_tokens, m.elapsed_micros
        ));
    }
    out
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

fn seed(service: &Service) -> Result<()> {
    for i in 0..SEED_RECORDS {
        let (record_type, content) = match i % 4 {
            0 => (
                RecordType::Decision,
                format!("Decision {i}: use bundled SQLite for local durability and FTS5 search."),
            ),
            1 => (
                RecordType::Gotcha,
                format!("Gotcha {i}: rollback stale memory when a later turn contradicts it."),
            ),
            2 => (
                RecordType::Preference,
                format!("Preference {i}: run cargo test for validation before opening a PR."),
            ),
            _ => (
                RecordType::RepoConvention,
                format!("Convention {i}: keep recall scoped to the requested profile."),
            ),
        };
        let record = NewRecord {
            profile_id: PROFILE.to_string(),
            workspace_id: WORKSPACE.to_string(),
            repo_id: None,
            subject_id: None,
            episode_id: None,
            scope: Scope::Workspace,
            record_type,
            content: content.clone(),
            related_files: vec![],
            tags: vec!["perf".to_string()],
            sensitivity: Sensitivity::Personal,
            portability: Portability::Portable,
            confidence: 0.9,
            source_ids: vec![],
            content_hash: ids::content_hash(
                PROFILE,
                WORKSPACE,
                None,
                record_type.as_str(),
                Scope::Workspace.as_str(),
                &content,
            ),
            supersedes: vec![],
            metadata: json!({}),
        };
        match service.store.upsert_record(&record)? {
            UpsertOutcome::Created(_) | UpsertOutcome::Skipped(_) => {}
        }
    }
    Ok(())
}

fn seed_procedure(service: &Service) -> Result<String> {
    let subject = service.create_subject(SubjectCreateRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        subject_key: Some("workflow:pr".to_string()),
        kind: Some(SubjectKind::Workflow.as_str().to_string()),
        display_name: Some("PR workflow".to_string()),
        metadata: None,
    })?;
    for i in 1..=2 {
        service.create_episode(EpisodeCreateRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            subject_id: Some(subject.subject.id.clone()),
            source_kind: Some("session".to_string()),
            source_ref: Some(format!("perf-{i}")),
            started_at: None,
            ended_at: Some(format!("2030-01-0{i}T00:00:00Z")),
            status: Some("success".to_string()),
            summary: Some(
                "Before opening a pull request, review the diff and run cargo test.".to_string(),
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
    if let Some(candidate) = preview.candidates.first().cloned() {
        service.procedures_apply(crate::protocol::ProceduresApplyRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            candidates: vec![candidate],
        })?;
    }
    Ok(subject.subject.id)
}
