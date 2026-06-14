//! Contract snapshot tests (issue #151).
//!
//! Locks the shape of the substrate's public contracts — HTTP/CLI response
//! envelopes, MCP tool registry, eval reports, adapter export, backup manifest,
//! and the doctor report — so accidental breaking changes are caught before
//! merge. The compatibility policy (docs/compatibility-policy.md) defines what
//! "breaking" means: removing or renaming a documented key, or changing its
//! type, is breaking; adding a new key is additive and allowed.
//!
//! These tests assert the REQUIRED key set is present (and, for enums like the
//! MCP tool registry, that the documented members exist). They intentionally do
//! NOT assert exact values — timestamps, ids, and counts vary — so additive
//! evolution stays green while a dropped/renamed field fails loudly.

use codex_memoryd::config::Config;
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;
use serde_json::Value;

fn service() -> Service {
    let store = Store::open(":memory:").expect("open store");
    let config = Config {
        default_workspace: "contract".to_string(),
        ..Default::default()
    };
    let svc = Service::new(store, config);
    svc.store.ensure_workspace("personal", "contract").unwrap();
    svc
}

/// Assert that `value` is an object containing every key in `required`.
fn assert_keys(value: &Value, required: &[&str], ctx: &str) {
    let obj = value
        .as_object()
        .unwrap_or_else(|| panic!("{ctx}: expected a JSON object, got {value}"));
    for key in required {
        assert!(
            obj.contains_key(*key),
            "{ctx}: missing required key `{key}` (removing/renaming a key is a breaking change; see docs/compatibility-policy.md)"
        );
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("serialize")
}

#[test]
fn status_contract_shape() {
    let svc = service();
    let status = svc.status().expect("status");
    let json = to_json(&status);
    assert_keys(
        &json,
        &[
            "provider_name",
            "provider_version",
            "api_version",
            "storage_schema_version",
            "status",
            "storage",
            "features",
            "degraded_reasons",
        ],
        "status",
    );
    assert_keys(
        &json["storage"],
        &["kind", "path", "writable"],
        "status.storage",
    );
}

#[test]
fn recall_contract_shape() {
    use codex_memoryd::protocol::RecallRequest;
    let svc = service();
    let resp = svc
        .recall(RecallRequest {
            profile: Some("personal".to_string()),
            workspace: Some("contract".to_string()),
            repo: None,
            session: None,
            query: Some("anything".to_string()),
            files: vec![],
            max_tokens: Some(1000),
            pack_mode: Some("default".to_string()),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            metadata: None,
        })
        .expect("recall");
    let json = to_json(&resp);
    assert_keys(
        &json,
        &[
            "summary",
            "facts",
            "checkpoints",
            "citations",
            "truncated",
            "authority",
            "policy",
            "pack",
        ],
        "recall",
    );
    // recall-not-authority is a load-bearing contract invariant.
    assert_eq!(json["authority"], "recall_not_authority");
    assert_keys(
        &json["pack"],
        &["mode", "template", "max_tokens"],
        "recall.pack",
    );
    assert_keys(&json["policy"], &["authority"], "recall.policy");
}

#[test]
fn mcp_tool_registry_contract() {
    use codex_memoryd::mcp;
    // Read-only is the default surface; the documented members must be present.
    for name in ["memory_status", "memory_recall", "memory_search"] {
        assert!(
            mcp::READ_ONLY_TOOL_NAMES.contains(&name),
            "read-only MCP tool `{name}` missing from registry"
        );
    }
    assert_eq!(
        mcp::READ_ONLY_TOOL_NAMES.len(),
        3,
        "read-only MCP tool count changed — adding a tool is additive but update the contract doc; removing one is breaking"
    );
    for name in [
        "memory_create",
        "memory_conclude",
        "memory_checkpoint",
        "memory_import_preview",
        "memory_import_apply",
    ] {
        assert!(
            mcp::WRITE_TOOL_NAMES.contains(&name),
            "write MCP tool `{name}` missing from registry"
        );
    }
}

#[test]
fn substrate_eval_contract_shape() {
    let report = codex_memoryd::eval::run_substrate_eval().expect("eval");
    let json = to_json(&report);
    assert_keys(
        &json,
        &[
            "suite",
            "version",
            "status",
            "fixture_families",
            "metrics",
            "checks",
            "triage",
        ],
        "eval.substrate",
    );
    assert_keys(
        &json["metrics"],
        &[
            "observation_recall_at_k",
            "precision_at_k",
            "cross_profile_bleed_rate",
            "poison_acceptance_rate",
            "pack_cost",
        ],
        "eval.substrate.metrics",
    );
}

#[test]
fn comparative_eval_contract_shape() {
    let report = codex_memoryd::eval::run_comparative_eval().expect("comparative");
    let json = to_json(&report);
    assert_keys(
        &json,
        &["suite", "version", "note", "question_count", "baselines"],
        "eval.comparative",
    );
    let baseline = &json["baselines"][0];
    assert_keys(
        baseline,
        &[
            "name",
            "recall_at_k",
            "precision_at_k",
            "context_bytes",
            "cross_profile_leak",
        ],
        "eval.comparative.baseline",
    );
}

#[test]
fn procedure_eval_contract_shape() {
    let report = codex_memoryd::proc_eval::run_procedure_eval().expect("proc eval");
    let json = to_json(&report);
    assert_keys(
        &json,
        &[
            "suite",
            "version",
            "status",
            "metrics",
            "fixture_families",
            "triage",
        ],
        "eval.procedures",
    );
    assert_keys(
        &json["metrics"],
        &[
            "activation_precision",
            "activation_recall",
            "false_activation_rate",
            "unsafe_promotion_rate",
            "evidence_coverage",
            "stale_retirement_accuracy",
        ],
        "eval.procedures.metrics",
    );
}

#[test]
fn adapter_export_contract_shape() {
    use codex_memoryd::protocol::AdapterExportRequest;
    let svc = service();
    let resp = svc
        .adapter_export(AdapterExportRequest {
            profile: Some("personal".to_string()),
            workspace: Some("contract".to_string()),
            target: "mcp-pack".to_string(),
            subject_id: None,
            max_bytes: Some(4096),
        })
        .expect("adapter export");
    let json = to_json(&resp);
    assert_keys(
        &json,
        &[
            "target",
            "adapter_version",
            "profile",
            "workspace",
            "generated_at",
            "authority",
            "source_card_type",
            "content_hash",
            "budget",
            "markdown",
        ],
        "adapter.export",
    );
    assert_keys(
        &json["budget"],
        &["max_bytes", "rendered_bytes", "truncated"],
        "adapter.export.budget",
    );
}

#[test]
fn backup_manifest_contract_shape() {
    use codex_memoryd::backup;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src.db");
    {
        let store = Store::open(&src).unwrap();
        store.ensure_workspace("personal", "contract").unwrap();
    }
    let store = Store::open(&src).unwrap();
    let dest = dir.path().join("backup.db");
    let result = backup::create_backup(&store, &dest, "2030-01-01T00:00:00Z").expect("backup");
    let json = to_json(&result.manifest);
    assert_keys(
        &json,
        &[
            "manifest_version",
            "tool_version",
            "created_at",
            "database_file",
            "sha256",
            "size_bytes",
            "schema_version",
            "expected_schema_version",
            "tables",
        ],
        "backup.manifest",
    );
}

#[test]
fn doctor_contract_shape() {
    let svc = service();
    let report = codex_memoryd::doctor::run(&svc).expect("doctor");
    let json = to_json(&report);
    assert_keys(
        &json,
        &[
            "tool_version",
            "status",
            "storage",
            "schema",
            "backup",
            "policy_corpus",
            "mcp",
            "quarantine",
            "procedures",
            "adapters",
            "issues",
        ],
        "doctor",
    );
}

#[test]
fn perf_report_contract_shape() {
    let report = codex_memoryd::perf::run_perf_report(|| 0).expect("perf");
    let json = to_json(&report);
    assert_keys(
        &json,
        &["suite", "version", "note", "seed_records", "measurements"],
        "perf",
    );
    let m = &json["measurements"][0];
    assert_keys(
        m,
        &[
            "path",
            "items",
            "output_bytes",
            "estimated_tokens",
            "elapsed_micros",
        ],
        "perf.measurement",
    );
}
