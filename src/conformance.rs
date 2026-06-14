//! Local conformance report helpers for adapter/export semantics.

use serde::Serialize;

use crate::config::Config;
use crate::error::Result;
use crate::protocol::AdapterExportRequest;
use crate::protocol::SyncFile;
use crate::protocol::SyncRequest;
use crate::service::Service;
use crate::store::Store;

const ADAPTER_TARGETS: &[&str] = &[
    "agents-md",
    "claude-code",
    "copilot",
    "github-instructions",
    "markdown",
    "mcp-pack",
];

const PROFILE: &str = "personal";
const WORKSPACE: &str = "conformance";

#[derive(Debug, Clone, Serialize)]
pub struct AdapterConformanceReport {
    pub report: String,
    pub status: String,
    pub authority: String,
    pub profile: String,
    pub workspace: String,
    pub targets: Vec<AdapterTargetReport>,
    pub checks: Vec<ConformanceCheck>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterTargetReport {
    pub target: String,
    pub status: String,
    pub adapter_version: String,
    pub source_card_type: String,
    pub source_ids: usize,
    pub context_pack: bool,
    pub truncated_budget_checked: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConformanceCheck {
    pub name: String,
    pub status: String,
}

pub fn run_adapter_conformance() -> Result<AdapterConformanceReport> {
    let service = seeded_service()?;
    let mut targets = Vec::new();
    let mut checks = Vec::new();

    for target in ADAPTER_TARGETS {
        let export = service.adapter_export(AdapterExportRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            target: (*target).to_string(),
            subject_id: None,
            max_bytes: None,
        })?;
        let has_context_pack = export.context_pack.is_some();
        let source_ids = export.source_ids.len();
        let context_pack_required = context_pack_required(target);
        let target_ok = export.target == *target
            && export.authority == "recall_not_authority"
            && export.source_card_type == "workspace_summary"
            && source_ids > 0
            && (!context_pack_required || has_context_pack);
        checks.push(ConformanceCheck {
            name: format!("{target}:authority_and_provenance"),
            status: pass_fail(target_ok),
        });

        let budgeted = service.adapter_export(AdapterExportRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            target: (*target).to_string(),
            subject_id: None,
            max_bytes: Some(160),
        })?;
        let budget_ok = budgeted.budget.max_bytes == Some(160)
            && budgeted.budget.truncated
            && budgeted.budget.rendered_bytes <= 160
            && (!context_pack_required
                || budgeted
                    .context_pack
                    .as_ref()
                    .is_some_and(|pack| pack.budget.truncated && pack.records.is_empty()));
        checks.push(ConformanceCheck {
            name: format!("{target}:budget_truncation"),
            status: pass_fail(budget_ok),
        });

        targets.push(AdapterTargetReport {
            target: (*target).to_string(),
            status: pass_fail(target_ok && budget_ok),
            adapter_version: export.adapter_version,
            source_card_type: export.source_card_type,
            source_ids,
            context_pack: has_context_pack,
            truncated_budget_checked: budget_ok,
        });
    }

    let all_passed = checks.iter().all(|check| check.status == "passed");
    Ok(AdapterConformanceReport {
        report: "adapter_conformance_v1".to_string(),
        status: pass_fail(all_passed),
        authority: "recall_not_authority".to_string(),
        profile: PROFILE.to_string(),
        workspace: WORKSPACE.to_string(),
        targets,
        checks,
    })
}

pub fn render_adapter_conformance_markdown(report: &AdapterConformanceReport) -> String {
    let mut lines = vec![
        "# Adapter conformance report".to_string(),
        String::new(),
        format!("- Status: `{}`", report.status),
        format!("- Authority: `{}`", report.authority),
        format!("- Profile: `{}`", report.profile),
        format!("- Workspace: `{}`", report.workspace),
        String::new(),
        "## Targets".to_string(),
    ];

    for target in &report.targets {
        lines.push(format!(
            "- `{}`: {} (version `{}`, card `{}`, source ids {}, context pack {}, budget {})",
            target.target,
            target.status,
            target.adapter_version,
            target.source_card_type,
            target.source_ids,
            target.context_pack,
            target.truncated_budget_checked
        ));
    }

    lines.push(String::new());
    lines.push("## Checks".to_string());
    for check in &report.checks {
        lines.push(format!("- `{}`: {}", check.name, check.status));
    }
    lines.join("\n")
}

fn seeded_service() -> Result<Service> {
    let store = Store::open(":memory:")?;
    let service = Service::new(
        store,
        Config {
            default_workspace: WORKSPACE.to_string(),
            ..Default::default()
        },
    );
    service.sync_local(SyncRequest {
        profile: Some(PROFILE.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        repo: None,
        source_root: Some("/fixtures/conformance".to_string()),
        mode: Some("apply".to_string()),
        files: Some(vec![SyncFile {
            path: "adapter-conformance.md".to_string(),
            kind: Some("memory_summary".to_string()),
            content: [
                "# Adapter conformance",
                "- Decision: adapter conformance exports preserve recall-not-authority metadata.",
                "- Gotcha: adapter conformance must keep provenance and deterministic budgets.",
            ]
            .join("\n"),
            hash: None,
            modified_at: None,
            idempotency_key: None,
            metadata: None,
        }]),
        metadata: None,
    })?;
    Ok(service)
}

fn context_pack_required(target: &str) -> bool {
    matches!(target, "agents-md" | "claude-code" | "copilot" | "mcp-pack")
}

fn pass_fail(passed: bool) -> String {
    if passed {
        "passed".to_string()
    } else {
        "failed".to_string()
    }
}
