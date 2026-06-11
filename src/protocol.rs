//! Wire protocol types: the typed request/response structs and the common
//! response envelope (SPEC §5, §6). These mirror the HTTP JSON contract that
//! the Codex runtime calls.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::domain::RepoIdentity;
use crate::error::Error;

// ---------------------------------------------------------------------------
// Common envelope (SPEC §5.5)
// ---------------------------------------------------------------------------

/// Provider identity attached to responses where useful.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderTag {
    pub name: String,
    pub version: String,
}

/// The structured error body inside an envelope.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl From<&Error> for ErrorBody {
    fn from(err: &Error) -> Self {
        ErrorBody {
            code: err.code.as_str().to_string(),
            message: err.message.clone(),
        }
    }
}

/// The universal response envelope (SPEC §5.5). `data` is present on success,
/// `error` on failure; `warnings` may accompany either.
#[derive(Debug, Clone, Serialize)]
pub struct Envelope<T: Serialize> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    pub warnings: Vec<String>,
    pub request_id: String,
    pub provider: ProviderTag,
}

impl<T: Serialize> Envelope<T> {
    pub fn success(
        data: T,
        warnings: Vec<String>,
        request_id: String,
        provider: ProviderTag,
    ) -> Self {
        Envelope {
            ok: true,
            data: Some(data),
            error: None,
            warnings,
            request_id,
            provider,
        }
    }
}

// ---------------------------------------------------------------------------
// Status (SPEC §6.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct StorageStatus {
    pub kind: String,
    pub path: String,
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalImportStatus {
    /// `unknown` | `not_found` | `unsynced` | `synced` | `error`
    pub status: String,
    pub last_preview_at: Option<String>,
    pub last_apply_at: Option<String>,
    pub unsynced_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    pub provider_name: String,
    pub provider_version: String,
    pub api_version: String,
    pub storage_schema_version: i64,
    /// `ok` | `degraded` | `unavailable` | `local_only` | `auth_required` | `auth_missing`
    pub status: String,
    pub storage: StorageStatus,
    pub active_profiles: Vec<String>,
    pub active_workspaces: Vec<String>,
    pub last_sync: Option<String>,
    pub pending_writes: i64,
    pub local_import: LocalImportStatus,
    pub features: Value,
    pub degraded_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduledDreamStatus {
    pub enabled: bool,
    pub last_run_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub last_run_id: Option<String>,
    pub last_watermark: Option<String>,
    pub next_eligible_run: Option<String>,
    pub degraded: bool,
}

// ---------------------------------------------------------------------------
// Recall (SPEC §6.2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct RecallRequest {
    pub profile: Option<String>,
    pub workspace: Option<String>,
    #[serde(default)]
    pub repo: Option<RepoIdentity>,
    #[serde(default)]
    pub session: Option<Value>,
    pub query: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub include_types: Vec<String>,
    #[serde(default)]
    pub exclude_types: Vec<String>,
    #[serde(default)]
    pub recency_days: Option<i64>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallFact {
    pub id: String,
    #[serde(rename = "type")]
    pub record_type: String,
    pub scope: String,
    pub content: String,
    pub confidence: f64,
    pub repo_id: Option<String>,
    pub related_files: Vec<String>,
    pub updated_at: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallCheckpoint {
    pub id: String,
    pub summary: String,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub next_steps: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Citation {
    pub memory_id: String,
    pub source_id: Option<String>,
    pub source_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallResponse {
    pub summary: Option<String>,
    pub facts: Vec<RecallFact>,
    pub checkpoints: Vec<RecallCheckpoint>,
    pub citations: Vec<Citation>,
    pub truncated: bool,
    /// Always true: provider context is recall, not authority (SPEC §10.4).
    pub authority: String,
}

// ---------------------------------------------------------------------------
// Search (SPEC §6.3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SearchRequest {
    pub profile: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub repo: Option<RepoIdentity>,
    pub query: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(rename = "type", default)]
    pub record_type: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchMatch {
    pub id: String,
    #[serde(rename = "type")]
    pub record_type: String,
    pub scope: String,
    pub content: String,
    pub confidence: f64,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub tags: Vec<String>,
    pub archived: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub matches: Vec<SearchMatch>,
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// Turns (SPEC §6.4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TurnMessage {
    pub actor: String,
    pub content: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TurnSession {
    pub id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TurnsRequest {
    pub profile: Option<String>,
    pub workspace: Option<String>,
    #[serde(default)]
    pub repo: Option<RepoIdentity>,
    pub session: Option<TurnSession>,
    pub messages: Option<Vec<TurnMessage>>,
    #[serde(default)]
    pub write_policy: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rejection {
    pub index: Option<usize>,
    pub reason: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnsResponse {
    pub accepted: usize,
    pub rejected: usize,
    pub rejections: Vec<Rejection>,
    pub source_ids: Vec<String>,
    pub derived_record_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Conclusions (SPEC §6.5)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ConclusionsRequest {
    pub profile: Option<String>,
    pub workspace: Option<String>,
    #[serde(default)]
    pub repo: Option<RepoIdentity>,
    pub target: Option<String>,
    pub conclusions: Option<Vec<String>>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(rename = "type", default)]
    pub record_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConclusionRejection {
    pub content: String,
    pub reason: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConclusionsResponse {
    pub created: Vec<String>,
    pub record_ids: Vec<String>,
    pub rejected: Vec<ConclusionRejection>,
}

// ---------------------------------------------------------------------------
// Checkpoints (SPEC §6 addition — see SPEC §6.9)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CheckpointRequest {
    pub profile: Option<String>,
    pub workspace: Option<String>,
    #[serde(default)]
    pub repo: Option<RepoIdentity>,
    #[serde(default)]
    pub session: Option<TurnSession>,
    pub summary: Option<String>,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
    #[serde(default)]
    pub tests_run: Vec<String>,
    #[serde(default)]
    pub tests_not_run: Vec<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckpointResponse {
    pub id: String,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Dreamer loop (preview/apply)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct DreamRequest {
    pub profile: Option<String>,
    pub workspace: Option<String>,
    #[serde(default)]
    pub repo: Option<RepoIdentity>,
    #[serde(default)]
    pub mode: Option<String>,
    /// Deterministic clock override for evals/tests.
    #[serde(default)]
    pub now: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DreamCandidate {
    pub action: String,
    #[serde(rename = "type")]
    pub proposed_type: String,
    pub content: String,
    pub confidence: f64,
    pub state: String,
    pub drift_prone: bool,
    pub expires_at: Option<String>,
    pub valid_until: Option<String>,
    pub historical_reason: Option<String>,
    pub supersedes: Vec<String>,
    pub policy: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DreamStaleRecord {
    pub memory_id: String,
    pub drift_prone: bool,
    pub state: String,
    pub expires_at: Option<String>,
    pub valid_until: Option<String>,
    pub suggested_action: String,
    pub historical_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DreamRejection {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DreamResponse {
    pub run_id: String,
    pub mode: String,
    pub profile: String,
    pub workspace: String,
    pub repo_id: Option<String>,
    pub now: String,
    pub candidates: Vec<DreamCandidate>,
    pub stale: Vec<DreamStaleRecord>,
    pub rejected: Vec<DreamRejection>,
    pub archived: Vec<String>,
    pub created: Vec<String>,
    pub authority: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduledDreamResponse {
    pub status: String,
    pub reason: Option<String>,
    pub run: Option<DreamResponse>,
    pub watermark_before: Option<String>,
    pub watermark_after: Option<String>,
    pub limits_hit: Vec<String>,
}

// ---------------------------------------------------------------------------
// Local Codex memory sync (SPEC §6.6)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SyncFile {
    pub path: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub content: String,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub modified_at: Option<String>,
    /// Codex sends an idempotency key in metadata; accept it directly too.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SyncRequest {
    pub profile: Option<String>,
    pub workspace: Option<String>,
    #[serde(default)]
    pub repo: Option<RepoIdentity>,
    pub source_root: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    pub files: Option<Vec<SyncFile>>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncRejection {
    pub path: String,
    pub reason: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncTypeBreakdown {
    pub preference: usize,
    pub command: usize,
    pub repo_convention: usize,
    pub decision: usize,
    pub gotcha: usize,
    pub task_checkpoint: usize,
    pub other: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncCursorView {
    pub source_root: String,
    pub last_started_at: Option<String>,
    pub last_completed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncResponse {
    pub mode: String,
    pub files_scanned: usize,
    pub proposed: usize,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub rejected: usize,
    pub rejections: Vec<SyncRejection>,
    pub types: SyncTypeBreakdown,
    pub warnings: Vec<String>,
    pub sync_cursor: SyncCursorView,
}

// ---------------------------------------------------------------------------
// Forget (SPEC §6.7)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ForgetRequest {
    pub profile: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    pub ids: Option<Vec<String>>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForgetResponse {
    pub archived: Vec<String>,
    pub deleted: Vec<String>,
    pub not_found: Vec<String>,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Export (SPEC §6.8)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ExportQuery {
    pub profile: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub repo_id: Option<String>,
    #[serde(default)]
    pub include_archived: Option<bool>,
    #[serde(default)]
    pub format: Option<String>,
    /// Optional target profile for export; work->personal is denied.
    #[serde(default)]
    pub target_profile: Option<String>,
}
