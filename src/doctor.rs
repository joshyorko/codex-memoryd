//! Operator diagnostics (issue #149).
//!
//! `doctor` answers "is this substrate healthy, and if not, why" in one place.
//! It assembles a machine-readable report covering storage/schema, backup
//! readiness, policy corpus presence, MCP read/write tiers, quarantine counts,
//! procedure states, and the adapter target registry.
//!
//! Diagnostics are content-free by construction: only counts, versions, and
//! structural facts appear — never stored memory text — so the output is safe
//! to print, log, and snapshot in CI.

use serde::Serialize;

use crate::conformance;
use crate::error::Result;
use crate::mcp;
use crate::service::Service;
use crate::store::SchemaReport;

/// Top-level diagnostics report.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub tool_version: &'static str,
    /// Overall health: "ok" when no section reports a blocking problem.
    pub status: &'static str,
    pub storage: StorageSection,
    pub schema: SchemaReport,
    pub backup: BackupSection,
    pub policy_corpus: PolicyCorpusSection,
    pub mcp: McpSection,
    pub quarantine: QuarantineSection,
    pub procedures: ProcedureSection,
    pub adapters: AdapterSection,
    /// Actionable problems found (empty when `status == "ok"`).
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageSection {
    pub path: String,
    pub writable: bool,
    pub integrity_ok: bool,
    pub record_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupSection {
    /// Whether the store can be backed up right now (writable + sound).
    pub ready: bool,
    /// How to create a backup. Static guidance, no secrets.
    pub command: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct PolicyCorpusSection {
    /// Whether the committed allow/deny/redaction corpus is present.
    pub present: bool,
    pub categories: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpSection {
    pub read_only_tools: Vec<&'static str>,
    pub write_tools: Vec<&'static str>,
    /// The default stdio surface is read-only.
    pub default_read_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuarantineSection {
    pub quarantined_records: i64,
    pub policy_denials: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcedureSection {
    pub total: i64,
    pub by_state: Vec<ProcedureStateCount>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcedureStateCount {
    pub state: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterSection {
    pub targets: Vec<&'static str>,
}

/// Build the full diagnostics report for a service.
pub fn run(service: &Service) -> Result<DoctorReport> {
    let store = &service.store;
    let mut issues = Vec::new();

    let writable = store.writable();
    if !writable {
        issues.push("storage is not writable".to_string());
    }
    let integrity_ok = store.integrity_ok().unwrap_or(false);
    if !integrity_ok {
        issues.push("storage failed integrity_check".to_string());
    }

    let schema = store.schema_report()?;
    if !schema.up_to_date {
        issues.push(format!(
            "schema version {:?} != expected {}",
            schema.recorded_version, schema.expected_version
        ));
    }

    let policy_corpus_present = policy_corpus_present();
    if !policy_corpus_present {
        issues.push("policy corpus fixtures missing".to_string());
    }

    let procedures_by_state = store.count_procedures_by_state()?;
    let procedure_total: i64 = procedures_by_state.iter().map(|(_, n)| n).sum();

    let status = if issues.is_empty() { "ok" } else { "degraded" };

    Ok(DoctorReport {
        tool_version: env!("CARGO_PKG_VERSION"),
        status,
        storage: StorageSection {
            path: store.path_display(),
            writable,
            integrity_ok,
            record_count: store.count_records().unwrap_or(-1),
        },
        schema,
        backup: BackupSection {
            ready: writable && integrity_ok,
            command: "codex-memoryd backup create --dest <file>",
        },
        policy_corpus: PolicyCorpusSection {
            present: policy_corpus_present,
            categories: vec!["allow", "deny", "redact"],
        },
        mcp: McpSection {
            read_only_tools: mcp::READ_ONLY_TOOL_NAMES.to_vec(),
            write_tools: mcp::WRITE_TOOL_NAMES.to_vec(),
            default_read_only: true,
        },
        quarantine: QuarantineSection {
            quarantined_records: store.count_quarantined_records().unwrap_or(-1),
            policy_denials: store.count_policy_denials().unwrap_or(-1),
        },
        procedures: ProcedureSection {
            total: procedure_total,
            by_state: procedures_by_state
                .into_iter()
                .map(|(state, count)| ProcedureStateCount { state, count })
                .collect(),
        },
        adapters: AdapterSection {
            targets: conformance::ADAPTER_TARGETS.to_vec(),
        },
        issues,
    })
}

/// Whether the committed policy corpus fixtures are present in the source tree.
/// Best-effort: looks relative to the manifest dir, which exists in dev/CI.
fn policy_corpus_present() -> bool {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("policy");
    ["allow.json", "deny.json", "redact.json"]
        .iter()
        .all(|f| dir.join(f).exists())
}

/// Render a short human summary of the report.
pub fn render_summary(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "codex-memoryd doctor: {} (v{})\n",
        report.status, report.tool_version
    ));
    out.push_str(&format!(
        "storage: writable={} integrity={} records={} path={}\n",
        report.storage.writable,
        report.storage.integrity_ok,
        report.storage.record_count,
        report.storage.path
    ));
    out.push_str(&format!(
        "schema: recorded={:?} expected={} up_to_date={} fts5={}\n",
        report.schema.recorded_version,
        report.schema.expected_version,
        report.schema.up_to_date,
        report.schema.fts_enabled
    ));
    out.push_str(&format!("backup: ready={}\n", report.backup.ready));
    out.push_str(&format!(
        "policy corpus: present={} ({} categories)\n",
        report.policy_corpus.present,
        report.policy_corpus.categories.len()
    ));
    out.push_str(&format!(
        "mcp: {} read-only tools, {} write tools (default read-only={})\n",
        report.mcp.read_only_tools.len(),
        report.mcp.write_tools.len(),
        report.mcp.default_read_only
    ));
    out.push_str(&format!(
        "quarantine: {} records, {} policy denials\n",
        report.quarantine.quarantined_records, report.quarantine.policy_denials
    ));
    out.push_str(&format!(
        "procedures: {} total, {} states\n",
        report.procedures.total,
        report.procedures.by_state.len()
    ));
    out.push_str(&format!(
        "adapters: {} targets\n",
        report.adapters.targets.len()
    ));
    if !report.issues.is_empty() {
        out.push_str("issues:\n");
        for issue in &report.issues {
            out.push_str(&format!("  - {issue}\n"));
        }
    }
    out
}
