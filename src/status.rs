//! Status assembly (SPEC §6.1). Builds the `/v1/status` payload from the store,
//! config, and metrics. Never exposes secrets.

use serde_json::json;

use crate::config::Config;
use crate::error::Result;
use crate::metrics::Metrics;
use crate::protocol::DreamRunStatus;
use crate::protocol::LocalImportStatus;
use crate::protocol::StatusResponse;
use crate::protocol::StorageStatus;
use crate::store::Store;
use crate::store::STORAGE_SCHEMA_VERSION;
use crate::API_VERSION;
use crate::PROVIDER_NAME;
use crate::PROVIDER_VERSION;

/// Build the status response. `degraded_reasons` from the store (e.g. FTS5
/// fallback) downgrade the status to `degraded`.
pub fn build_status(store: &Store, config: &Config, metrics: &Metrics) -> Result<StatusResponse> {
    let writable = store.writable();
    let mut degraded_reasons: Vec<String> = store.degraded_reasons().to_vec();

    let active_profiles = store.active_profiles().unwrap_or_default();
    let active_workspaces = store.active_workspaces().unwrap_or_default();
    let last_sync = store.last_sync_completed().unwrap_or(None);
    let last_dream = store.last_dream_run().unwrap_or(None);
    let pending_writes = 0; // writes are synchronous in the MVP

    if !writable {
        degraded_reasons.push("storage is not writable".to_string());
    }
    if let Some(run) = &last_dream {
        if run.status == "error" {
            degraded_reasons.push(match &run.error_summary {
                Some(summary) if !summary.is_empty() => {
                    format!("last Dreamer run failed: {summary}")
                }
                _ => "last Dreamer run failed".to_string(),
            });
        }
    }

    let status = if !writable {
        "unavailable"
    } else if degraded_reasons.is_empty() {
        "ok"
    } else {
        "degraded"
    };

    // Local import status: derive from sync cursor presence.
    let local_import = LocalImportStatus {
        status: if last_sync.is_some() {
            "synced".to_string()
        } else {
            "unknown".to_string()
        },
        last_preview_at: None,
        last_apply_at: last_sync.clone(),
        unsynced_count: 0,
    };

    let features = json!({
        "fts5": store.fts_enabled(),
        "search_mode": if store.fts_enabled() { "fts5" } else { "like" },
        "recall": true,
        "import_local": true,
        "checkpoints": true,
        "export": true,
        "metrics": metrics.snapshot(),
        "cross_profile_policy": config.cross_profile_policy,
        "max_recall_tokens": config.max_recall_tokens,
    });

    Ok(StatusResponse {
        provider_name: PROVIDER_NAME.to_string(),
        provider_version: PROVIDER_VERSION.to_string(),
        api_version: API_VERSION.to_string(),
        storage_schema_version: STORAGE_SCHEMA_VERSION,
        status: status.to_string(),
        storage: StorageStatus {
            kind: config.storage_kind.clone(),
            path: store.path_display(),
            writable,
        },
        active_profiles,
        active_workspaces,
        last_sync,
        last_dream: last_dream.map(|run| DreamRunStatus {
            run_id: run.id,
            profile: run.profile_id,
            workspace: run.workspace_id,
            repo_id: run.repo_id,
            mode: run.mode,
            status: run.status,
            started_at: run.started_at,
            completed_at: run.completed_at,
            source_window_start: run.source_window_start,
            source_window_end: run.source_window_end,
            created: run.created_count,
            archived: run.archived_count,
            rejected: run.rejected_count,
            error_summary: run.error_summary,
        }),
        pending_writes,
        local_import,
        features,
        degraded_reasons,
    })
}
