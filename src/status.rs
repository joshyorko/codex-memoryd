//! Status assembly (SPEC §6.1). Builds the `/v1/status` payload from the store,
//! config, and metrics. Never exposes secrets.

use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::Duration;
use time::OffsetDateTime;

use crate::config::Config;
use crate::error::Result;
use crate::metrics::Metrics;
use crate::protocol::LocalImportStatus;
use crate::protocol::ScheduledDreamStatus;
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
    let pending_writes = 0; // writes are synchronous in the MVP
    let dream_scheduler = dream_scheduler_status(store, config)?;

    if !writable {
        degraded_reasons.push("storage is not writable".to_string());
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
        "dream_scheduler": dream_scheduler,
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
        pending_writes,
        local_import,
        features,
        degraded_reasons,
    })
}

fn dream_scheduler_status(store: &Store, config: &Config) -> Result<ScheduledDreamStatus> {
    let last = store.last_scheduled_dream_run()?;
    let (last_run_at, last_status, last_error, last_run_id, last_watermark) = match last {
        Some(summary) => (
            summary.completed_at.clone(),
            Some(summary.status.clone()),
            summary.error.clone(),
            Some(summary.run_id.clone()),
            summary.watermark_after.clone(),
        ),
        None => (None, None, None, None, None),
    };
    let next_eligible_run = if config.dream_scheduler.enabled {
        last_run_at
            .as_deref()
            .and_then(|last| add_seconds(last, config.dream_scheduler.interval_seconds as i64))
            .or_else(|| Some("now".to_string()))
    } else {
        None
    };
    Ok(ScheduledDreamStatus {
        enabled: config.dream_scheduler.enabled,
        last_run_at,
        last_status: last_status.clone(),
        last_error: last_error.clone(),
        last_run_id,
        last_watermark,
        next_eligible_run,
        degraded: matches!(last_status.as_deref(), Some("error")) || last_error.is_some(),
    })
}

fn add_seconds(value: &str, seconds: i64) -> Option<String> {
    let parsed = OffsetDateTime::parse(value, &Rfc3339).ok()?;
    (parsed + Duration::seconds(seconds)).format(&Rfc3339).ok()
}
