//! Selective, content-addressed memory bundles.
//!
//! A bundle is deliberately a closed wire projection.  It never contains
//! storage rows, filesystem locations, executable data, or an authority-bearing
//! reference.  ZIP is only the transport; all identity and integrity decisions
//! are made from the canonical manifest and JSONL payloads.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path};

use rusqlite::{params, OptionalExtension, Transaction};
use serde::de::{DeserializeOwned, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::domain::{
    Episode, MemoryRecord, MemorySource, Portability, Profile, RecordType, Scope, Sensitivity,
    Subject, SubjectKind, TemporalState,
};
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{self, PublicHandleKind};
use crate::policy::{self, BoundaryDecision, PolicyDecision};
use crate::store::{self, RecordQuery, Store};

pub const FORMAT_VERSION: u32 = 1;
pub const MEDIA_TYPE: &str = "application/vnd.codex-memoryd.bundle.v1+json";
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_MEMBER_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_OBJECTS: usize = 20_000;
pub const MAX_ROOT_MEMORIES: usize = 5_000;
pub const MAX_JSON_DEPTH: usize = 32;
pub const MAX_DETAIL_ITEMS: usize = 200;
pub const MAX_ALIASES: usize = 16;
pub const MAX_REFS: usize = 256;
pub const MAX_TAGS: usize = 128;
pub const MAX_FILES: usize = 128;
pub const MAX_RECORD_CHARS: usize = policy::MAX_RECORD_CHARS;
const MAX_COMPRESSION_RATIO: u64 = 100;
const SIGNATURE_MEMBER: &str = "signatures/manifest.dsse.json";
const MANIFEST_MEMBER: &str = "manifest.json";
const OBJECT_MEMBERS: [&str; 5] = [
    "objects/subjects.jsonl",
    "objects/episodes.jsonl",
    "objects/sources.jsonl",
    "objects/evidence.jsonl",
    "objects/memories.jsonl",
];

const RULESET_FINGERPRINT: &str = "portable-bundle-policy-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleMode {
    Inspect,
    ExportPreview,
    ExportWrite,
    ImportPreview,
    ImportApply,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct PortableRef {
    pub origin_instance_id: String,
    pub kind: String,
    pub id: String,
}

impl PortableRef {
    fn validate(&self, expected_kind: Option<&str>) -> Result<()> {
        if self.origin_instance_id.len() > 128
            || self.origin_instance_id.is_empty()
            || !self
                .origin_instance_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "invalid portable origin instance",
            ));
        }
        if expected_kind.is_some_and(|kind| kind != self.kind) {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "portable reference kind does not match its member",
            ));
        }
        let handle_kind = match self.kind.as_str() {
            "memory" => PublicHandleKind::MemoryRef,
            "subject" => PublicHandleKind::SubjectRef,
            "episode" => PublicHandleKind::EpisodeRef,
            "source" => PublicHandleKind::SourceRef,
            "evidence" => PublicHandleKind::EvidenceRef,
            _ => {
                return Err(bundle_error(
                    ErrorCode::BundleSchemaUnsupported,
                    "unsupported portable object kind",
                ))
            }
        };
        if ids::parse_public_handle(&self.id) != Some(handle_kind) {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "portable object id is not an opaque public handle",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectEnvelope<T> {
    pub schema: String,
    pub portable_ref: PortableRef,
    #[serde(default)]
    pub aliases: Vec<PortableRef>,
    pub digest: String,
    pub body: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Producer {
    pub instance_id: String,
    pub tool_version: String,
    pub storage_schema_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleIntent {
    pub source_profile: String,
    pub source_workspace: String,
    #[serde(default)]
    pub source_repo_id: Option<String>,
    pub target_profile: String,
    #[serde(default)]
    pub target_workspace: Option<String>,
    #[serde(default)]
    pub target_repo_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSelection {
    #[serde(default)]
    pub record_ids: Vec<String>,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePolicy {
    pub ruleset_version: String,
    pub boundary_decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub path: String,
    pub media_type: String,
    pub digest: String,
    pub size_bytes: u64,
    pub object_count: usize,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BundleManifestCounts {
    pub root_memories: usize,
    pub dependency_objects: usize,
    pub omitted_secret: usize,
    pub omitted_quarantined: usize,
    pub omitted_portability: usize,
    pub omitted_boundary: usize,
    pub omitted_unsafe_path: usize,
    pub external_references: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    pub media_type: String,
    pub format_version: u32,
    pub bundle_id: String,
    pub created_at: String,
    pub producer: Producer,
    pub intent: BundleIntent,
    pub selection: BundleSelection,
    pub policy: BundlePolicy,
    pub descriptors: Vec<Descriptor>,
    pub counts: BundleManifestCounts,
    #[serde(default)]
    pub required_features: Vec<String>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SafeMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub historical_reason: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_artifact_stored: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectBody {
    pub profile: String,
    pub workspace: String,
    pub subject_key: String,
    pub kind: String,
    pub display_name: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub metadata: SafeMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeBody {
    pub profile: String,
    pub workspace: String,
    pub subject_ref: PortableRef,
    pub source_kind: String,
    pub source_ref: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub trust_level: Option<String>,
    #[serde(default)]
    pub source_metadata: SafeMetadata,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub metadata: SafeMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBody {
    pub profile: String,
    pub workspace: String,
    pub kind: String,
    #[serde(default)]
    pub source_path: Option<String>,
    pub source_hash: String,
    pub created_at: String,
    pub ingested_at: String,
    #[serde(default)]
    pub metadata: SafeMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBody {
    pub profile: String,
    pub workspace: String,
    #[serde(default)]
    pub repo_id: Option<String>,
    #[serde(default)]
    pub subject_key: Option<String>,
    pub source_kind: String,
    #[serde(default)]
    pub source_ref: Option<PortableRef>,
    #[serde(default)]
    pub subject_ref: Option<PortableRef>,
    #[serde(default)]
    pub source_path: Option<String>,
    pub source_hash: String,
    pub safe_summary: String,
    pub policy_state: String,
    pub created_at: String,
    #[serde(default)]
    pub metadata: SafeMetadata,
    #[serde(default)]
    pub external_refs: Vec<PortableRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBody {
    pub profile: String,
    pub workspace: String,
    #[serde(default)]
    pub repo_id: Option<String>,
    pub scope: String,
    #[serde(rename = "type")]
    pub record_type: String,
    pub content: String,
    #[serde(default)]
    pub related_files: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub sensitivity: String,
    pub portability: String,
    pub confidence: f64,
    #[serde(default)]
    pub subject_ref: Option<PortableRef>,
    #[serde(default)]
    pub episode_ref: Option<PortableRef>,
    #[serde(default)]
    pub source_refs: Vec<PortableRef>,
    #[serde(default)]
    pub supersedes: Vec<PortableRef>,
    #[serde(default)]
    pub superseded_by: Option<PortableRef>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub valid_until: Option<String>,
    #[serde(default)]
    pub invalidated_at: Option<String>,
    pub archived: bool,
    pub temporal_state: String,
    pub trust_state: String,
    pub trust_score: f64,
    #[serde(default)]
    pub historical_reason: Option<String>,
    #[serde(default)]
    pub metadata: SafeMetadata,
    #[serde(default)]
    pub external_refs: Vec<PortableRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleCounts {
    pub discovered: usize,
    pub validated: usize,
    pub create: usize,
    pub reuse_identity_exact: usize,
    pub reuse_content_exact: usize,
    pub external_reference: usize,
    pub rejected: usize,
    pub conflicts: usize,
}

impl Default for BundleCounts {
    fn default() -> Self {
        Self {
            discovered: 0,
            validated: 0,
            create: 0,
            reuse_identity_exact: 0,
            reuse_content_exact: 0,
            external_reference: 0,
            rejected: 0,
            conflicts: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BundleReport {
    pub mode: String,
    pub format_version: u32,
    pub bundle_id: String,
    pub manifest_digest: String,
    pub integrity_status: String,
    pub signature_status: String,
    pub source_instance_id: String,
    #[serde(default)]
    pub destination_instance_id: Option<String>,
    pub source_scope: ScopeSummary,
    #[serde(default)]
    pub destination_scope: Option<ScopeSummary>,
    pub policy_fingerprint: String,
    #[serde(default)]
    pub mapping_digest: Option<String>,
    #[serde(default)]
    pub plan_id: Option<String>,
    #[serde(default)]
    pub safe_to_apply: Option<bool>,
    pub counts: BundleCounts,
    pub omissions: BTreeMap<String, usize>,
    pub decisions: Vec<DecisionDetail>,
    pub decision_details_truncated: usize,
    pub conflicts: Vec<DecisionDetail>,
    pub warnings: Vec<String>,
    pub details_truncated: usize,
    pub recall_not_authority: bool,
    #[serde(default)]
    pub receipt: Option<ReceiptSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ScopeSummary {
    pub profile: String,
    pub workspace: String,
    #[serde(default)]
    pub repo_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionDetail {
    pub portable_ref: PortableRef,
    pub decision: String,
    pub reason_code: String,
    #[serde(default)]
    pub destination_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReceiptSummary {
    pub receipt_id: String,
    pub status: String,
    pub applied_at: String,
    pub counts: BundleCounts,
}

#[derive(Debug, Clone)]
pub struct BundleExportOptions {
    pub profile: String,
    pub workspace: String,
    pub repo_id: Option<String>,
    pub record_ids: Vec<String>,
    pub include_archived: bool,
    pub target_profile: String,
    pub target_workspace: Option<String>,
    pub target_repo_id: Option<String>,
    /// A fixed timestamp is useful for deterministic fixtures. CLI callers
    /// leave it unset and use the current UTC time.
    pub created_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BundleImportOptions {
    pub profile: String,
    pub workspace: String,
    pub repo_id: Option<String>,
}

#[derive(Debug, Clone)]
struct BundlePayload {
    manifest: BundleManifest,
    manifest_bytes: Vec<u8>,
    members: BTreeMap<String, Vec<u8>>,
    signature_status: String,
}

#[derive(Debug, Clone)]
struct ExportGraph {
    subjects: Vec<ObjectEnvelope<SubjectBody>>,
    episodes: Vec<ObjectEnvelope<EpisodeBody>>,
    sources: Vec<ObjectEnvelope<SourceBody>>,
    evidence: Vec<ObjectEnvelope<EvidenceBody>>,
    memories: Vec<ObjectEnvelope<MemoryBody>>,
    counts: BundleManifestCounts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ObjectKind {
    Subject,
    Episode,
    Source,
    Evidence,
    Memory,
}

impl ObjectKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Episode => "episode",
            Self::Source => "source",
            Self::Evidence => "evidence",
            Self::Memory => "memory",
        }
    }

    fn schema(self) -> &'static str {
        match self {
            Self::Subject => "codex-memoryd.subject.v1",
            Self::Episode => "codex-memoryd.episode.v1",
            Self::Source => "codex-memoryd.source.v1",
            Self::Evidence => "codex-memoryd.evidence.v1",
            Self::Memory => "codex-memoryd.memory.v1",
        }
    }

    fn media_type(self) -> &'static str {
        match self {
            Self::Subject => "application/vnd.codex-memoryd.subject.v1+jsonl",
            Self::Episode => "application/vnd.codex-memoryd.episode.v1+jsonl",
            Self::Source => "application/vnd.codex-memoryd.source.v1+jsonl",
            Self::Evidence => "application/vnd.codex-memoryd.evidence.v1+jsonl",
            Self::Memory => "application/vnd.codex-memoryd.memory.v1+jsonl",
        }
    }
}

impl BundleExportOptions {
    fn validate(&self) -> Result<(Profile, Profile)> {
        let source = Profile::parse(&self.profile)
            .ok_or_else(|| Error::invalid_request("bundle export profile is unsupported"))?;
        let target = Profile::parse(&self.target_profile)
            .ok_or_else(|| Error::invalid_request("bundle export target profile is unsupported"))?;
        validate_scalar(&self.profile, "profile")?;
        validate_scalar(&self.workspace, "workspace")?;
        if let Some(value) = &self.repo_id {
            validate_scalar(value, "repo_id")?;
        }
        if let Some(value) = &self.target_workspace {
            validate_scalar(value, "target_workspace")?;
        }
        if let Some(value) = &self.target_repo_id {
            validate_scalar(value, "target_repo_id")?;
        }
        if let Some(created_at) = &self.created_at {
            validate_timestamp(created_at)?;
        }
        if let BoundaryDecision::Deny { reason } = policy::export_boundary(source, target) {
            return Err(bundle_error(ErrorCode::BundlePolicyDenied, reason));
        }
        Ok((source, target))
    }
}

impl BundleImportOptions {
    fn validate(&self) -> Result<()> {
        Profile::parse(&self.profile).ok_or_else(|| {
            Error::invalid_request("bundle import destination profile is unsupported")
        })?;
        validate_scalar(&self.profile, "to-profile")?;
        validate_scalar(&self.workspace, "to-workspace")?;
        if let Some(value) = &self.repo_id {
            validate_scalar(value, "to-repo-id")?;
        }
        Ok(())
    }
}

/// Export a source snapshot without writing an artifact.
pub fn export_preview(store: &Store, options: &BundleExportOptions) -> Result<BundleReport> {
    let payload = build_export_payload(store, options)?;
    Ok(export_report(&payload, BundleMode::ExportPreview))
}

/// Export one atomically-created owner-only `.cmembundle` file.
pub fn export_write(
    store: &Store,
    options: &BundleExportOptions,
    destination: &Path,
) -> Result<BundleReport> {
    if destination.exists() {
        return Err(Error::invalid_request(
            "bundle destination already exists; refusing to overwrite",
        ));
    }
    let payload = build_export_payload(store, options)?;
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::invalid_request("bundle destination must have a valid filename"))?;
    let temp_name = format!(".{file_name}.{}.tmp", ids::new_id("bundle"));
    let temp = parent.join(temp_name);
    let result = (|| -> Result<()> {
        let mut options_file = OpenOptions::new();
        options_file.write(true).create_new(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options_file.mode(0o600);
        }
        let file = options_file.open(&temp)?;
        write_zip(file, &payload)?;
        let mut verify = File::open(&temp)?;
        verify.sync_all()?;
        let verified = read_bundle_from_reader(&mut verify)?;
        if verified.manifest.bundle_id != payload.manifest.bundle_id {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "bundle self-verification changed the logical artifact",
            ));
        }
        if destination.exists() {
            return Err(Error::invalid_request(
                "bundle destination appeared during export; refusing to overwrite",
            ));
        }
        fs::rename(&temp, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(export_report(&payload, BundleMode::ExportWrite))
}

/// Inspect a bundle without opening configuration or a memory database.
pub fn inspect(path: &Path) -> Result<BundleReport> {
    let mut file = File::open(path)?;
    let payload = read_bundle_from_reader(&mut file)?;
    Ok(inspect_report(&payload))
}

/// Build a destination-specific, zero-write import plan.
pub fn import_preview(
    store: &Store,
    path: &Path,
    options: &BundleImportOptions,
) -> Result<BundleReport> {
    options.validate()?;
    let mut file = File::open(path)?;
    let payload = read_bundle_from_reader(&mut file)?;
    let destination_instance_id = store.instance_id()?;
    let plan = plan_import(store, &payload, options, None, &destination_instance_id)?;
    Ok(plan.report)
}

/// Revalidate and apply a previously-previewed plan in one immediate
/// transaction. The supplied plan id is never treated as an authority token.
pub fn import_apply(
    store: &Store,
    path: &Path,
    options: &BundleImportOptions,
    expected_plan_id: &str,
) -> Result<BundleReport> {
    options.validate()?;
    if !expected_plan_id.starts_with("sha256:") {
        return Err(Error::invalid_request(
            "bundle --plan-id must be a sha256 digest",
        ));
    }
    let mut file = File::open(path)?;
    let payload = read_bundle_from_reader(&mut file)?;
    let destination_instance_id = store.instance_id()?;
    let mut existing_receipt = None;
    let report = store.transaction_immediate(|tx| {
        existing_receipt = lookup_receipt(
            tx,
            &payload.manifest.bundle_id,
            &payload.manifest.producer.instance_id,
            &destination_instance_id,
            &mapping_digest(options, &payload.manifest)?,
        )?;
        if let Some(receipt) = existing_receipt.clone() {
            let mut report = receipt_report(&payload, options, receipt);
            report.warnings.push("already_applied".to_string());
            return Ok(report);
        }
        let plan = plan_import(store, &payload, options, Some(tx), &destination_instance_id)?;
        if plan.report.plan_id.as_deref() != Some(expected_plan_id) {
            return Err(bundle_error(
                ErrorCode::BundlePlanStale,
                "bundle plan no longer matches the validated destination state",
            ));
        }
        if plan.report.safe_to_apply != Some(true) {
            return Err(bundle_error(
                ErrorCode::BundlePolicyDenied,
                "bundle plan contains blocking conflicts or policy rejections",
            ));
        }
        apply_plan(tx, store, &payload, options, &plan)?;
        let receipt = insert_receipt(tx, &payload, options, &plan)?;
        Ok(receipt_report(&payload, options, receipt))
    })?;
    Ok(report)
}

// ---------------------------------------------------------------------------
// Canonical JSON (RFC 8785-compatible subset)
// ---------------------------------------------------------------------------

fn canonical_json(value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_canonical(value, &mut out)?;
    Ok(out)
}

fn write_canonical(value: &Value, out: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(value) => out.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::String(value) => out.extend_from_slice(serde_json::to_string(value)?.as_bytes()),
        Value::Array(values) => {
            out.push(b'[');
            for (idx, value) in values.iter().enumerate() {
                if idx != 0 {
                    out.push(b',');
                }
                write_canonical(value, out)?;
            }
            out.push(b']');
        }
        Value::Object(values) => {
            let mut keys: Vec<&String> = values.keys().collect();
            keys.sort_by(|a, b| utf16_cmp(a, b));
            out.push(b'{');
            for (idx, key) in keys.iter().enumerate() {
                if idx != 0 {
                    out.push(b',');
                }
                out.extend_from_slice(serde_json::to_string(*key)?.as_bytes());
                out.push(b':');
                write_canonical(
                    values.get(*key).ok_or_else(|| {
                        bundle_error(
                            ErrorCode::BundleIntegrityFailed,
                            "canonical object key missing",
                        )
                    })?,
                    out,
                )?;
            }
            out.push(b'}');
        }
        Value::Number(number) => out.extend_from_slice(canonical_number(number)?.as_bytes()),
    }
    Ok(())
}

fn utf16_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn canonical_number(number: &Number) -> Result<String> {
    let value = number
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| bundle_error(ErrorCode::BundleIntegrityFailed, "non-I-JSON number"))?;
    let raw = Number::from_f64(value)
        .ok_or_else(|| bundle_error(ErrorCode::BundleIntegrityFailed, "non-I-JSON number"))?
        .to_string();
    if raw == "-0" || raw == "-0.0" {
        return Ok("0".to_string());
    }
    let Some(exp_pos) = raw.find(['e', 'E']) else {
        if let Some(dot) = raw.find('.') {
            let (whole, fraction) = raw.split_at(dot);
            let fraction = fraction.trim_end_matches('0');
            if fraction == "." {
                return Ok(whole.to_string());
            }
            return Ok(format!("{whole}{fraction}"));
        }
        return Ok(raw);
    };
    let mantissa = &raw[..exp_pos];
    let exponent: i32 = raw[exp_pos + 1..]
        .parse()
        .map_err(|_| bundle_error(ErrorCode::BundleIntegrityFailed, "invalid JSON number"))?;
    let negative = mantissa.starts_with('-');
    let unsigned = mantissa.strip_prefix('-').unwrap_or(mantissa);
    let mut digits = unsigned.replace('.', "");
    let decimal_pos = unsigned.find('.').unwrap_or(unsigned.len()) as i32 + exponent;
    while digits.ends_with('0') && digits.len() > 1 {
        digits.pop();
    }
    let abs_digits = if decimal_pos <= 0 {
        format!("0.{}{}", "0".repeat((-decimal_pos) as usize), digits)
    } else if decimal_pos >= digits.len() as i32 {
        format!(
            "{}{}",
            digits,
            "0".repeat((decimal_pos - digits.len() as i32) as usize)
        )
    } else {
        let at = decimal_pos as usize;
        format!("{}.{}", &digits[..at], &digits[at..])
    };
    let abs_value = if abs_digits.contains('.') {
        abs_digits
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        abs_digits
    };
    let abs_value = if abs_value.is_empty() {
        "0".to_string()
    } else {
        abs_value
    };
    let use_decimal = exponent >= -6 && exponent < 21;
    if use_decimal {
        return Ok(if negative && abs_value != "0" {
            format!("-{abs_value}")
        } else {
            abs_value
        });
    }
    let first = digits.chars().next().unwrap_or('0');
    let rest = digits.chars().skip(1).collect::<String>();
    let scientific_exp = decimal_pos - 1;
    let mut result = if rest.is_empty() {
        first.to_string()
    } else {
        format!("{first}.{rest}")
    };
    result.push('e');
    result.push_str(if scientific_exp >= 0 { "+" } else { "" });
    result.push_str(&scientific_exp.to_string());
    Ok(if negative {
        format!("-{result}")
    } else {
        result
    })
}

fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = deserializer
        .deserialize_any(NoDuplicateValueVisitor)
        .map_err(|err| {
            bundle_error(
                ErrorCode::BundleIntegrityFailed,
                format!("invalid JSON: {err}"),
            )
        })?;
    deserializer.end().map_err(|err| {
        bundle_error(
            ErrorCode::BundleIntegrityFailed,
            format!("trailing JSON: {err}"),
        )
    })?;
    validate_json_limits(&value, 0)?;
    serde_json::from_value(value).map_err(|err| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            format!("object schema validation failed: {err}"),
        )
    })
}

struct NoDuplicateValueSeed;

impl<'de> DeserializeSeed<'de> for NoDuplicateValueSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateValueVisitor)
    }
}

struct NoDuplicateValueVisitor;

impl<'de> Visitor<'de> for NoDuplicateValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E>(self, value: i64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E>(self, value: u64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> std::result::Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| serde::de::Error::custom("non-finite JSON number"))
    }
    fn visit_str<E>(self, value: &str) -> std::result::Result<Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Value::String(value.to_string()))
    }
    fn visit_string<E>(self, value: String) -> std::result::Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_none<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(NoDuplicateValueSeed)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A>(self, mut map: A) -> std::result::Result<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        let mut seen = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            let value = map.next_value_seed(NoDuplicateValueSeed)?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

fn validate_json_limits(value: &Value, depth: usize) -> Result<()> {
    if depth > MAX_JSON_DEPTH {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "JSON nesting limit exceeded",
        ));
    }
    match value {
        Value::String(value) if value.len() > MAX_MEMBER_BYTES => Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "JSON string value limit exceeded",
        )),
        Value::Array(values) => {
            for value in values {
                validate_json_limits(value, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for (key, value) in values {
                if key.len() > 1024 {
                    return Err(bundle_error(
                        ErrorCode::BundleLimitExceeded,
                        "JSON object key limit exceeded",
                    ));
                }
                validate_json_limits(value, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// ZIP validation and manifest verification
// ---------------------------------------------------------------------------

fn read_bundle_from_reader<R: Read + Seek>(reader: &mut R) -> Result<BundlePayload> {
    let mut archive = ZipArchive::new(reader).map_err(|err| {
        bundle_error(
            ErrorCode::BundleIntegrityFailed,
            format!("invalid ZIP archive: {err}"),
        )
    })?;
    let mut names = BTreeSet::new();
    let mut members = BTreeMap::new();
    let mut total = 0usize;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|err| {
            bundle_error(
                ErrorCode::BundleIntegrityFailed,
                format!("invalid ZIP member: {err}"),
            )
        })?;
        let name = file.name().to_string();
        validate_member_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(bundle_error(
                ErrorCode::BundleUnsafeMember,
                "duplicate ZIP member name",
            ));
        }
        if file.is_dir() || file.encrypted() {
            return Err(bundle_error(
                ErrorCode::BundleUnsafeMember,
                "directory, encrypted, or symlink ZIP members are not allowed",
            ));
        }
        if file
            .unix_mode()
            .is_some_and(|mode| (mode & 0o170000) != 0 && (mode & 0o170000) != 0o100000)
        {
            return Err(bundle_error(
                ErrorCode::BundleUnsafeMember,
                "only regular ZIP members are allowed",
            ));
        }
        if !matches!(
            file.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(bundle_error(
                ErrorCode::BundleUnsafeMember,
                "unsupported ZIP compression method",
            ));
        }
        let declared = file.size();
        let compressed = file.compressed_size();
        if declared > MAX_MEMBER_BYTES as u64
            || (compressed > 0 && declared > compressed.saturating_mul(MAX_COMPRESSION_RATIO))
        {
            return Err(bundle_error(
                ErrorCode::BundleLimitExceeded,
                "ZIP member size or compression ratio exceeds the bundle limit",
            ));
        }
        total = total
            .checked_add(declared as usize)
            .ok_or_else(|| bundle_error(ErrorCode::BundleLimitExceeded, "bundle size overflow"))?;
        if total > MAX_TOTAL_BYTES {
            return Err(bundle_error(
                ErrorCode::BundleLimitExceeded,
                "bundle uncompressed size limit exceeded",
            ));
        }
        let mut bytes = Vec::with_capacity(declared as usize);
        file.read_to_end(&mut bytes)?;
        if bytes.len() != declared as usize {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "ZIP member was truncated",
            ));
        }
        members.insert(name, bytes);
    }
    let manifest_bytes = members.get(MANIFEST_MEMBER).cloned().ok_or_else(|| {
        bundle_error(ErrorCode::BundleIntegrityFailed, "manifest.json is missing")
    })?;
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "manifest size limit exceeded",
        ));
    }
    let manifest_value: Value = parse_json(&manifest_bytes)?;
    let manifest: BundleManifest =
        serde_json::from_value(manifest_value.clone()).map_err(|err| {
            bundle_error(
                ErrorCode::BundleSchemaUnsupported,
                format!("manifest schema validation failed: {err}"),
            )
        })?;
    validate_manifest(&manifest)?;
    let canonical_manifest = canonical_json(&manifest_value)?;
    if canonical_manifest != manifest_bytes {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "manifest is not RFC 8785 canonical JSON",
        ));
    }
    let expected_bundle_id = manifest_digest_without_id_value(&manifest_value)?;
    if expected_bundle_id != manifest.bundle_id {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "bundle_id does not match the unsigned canonical manifest",
        ));
    }
    for descriptor in &manifest.descriptors {
        let bytes = members.get(&descriptor.path).ok_or_else(|| {
            bundle_error(
                ErrorCode::BundleMissingDependency,
                "manifest-declared payload member is missing",
            )
        })?;
        if bytes.len() as u64 != descriptor.size_bytes
            || ids::sha256_hex(bytes) != descriptor.digest
        {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "payload descriptor digest or size does not match",
            ));
        }
    }
    let expected_names: BTreeSet<String> = std::iter::once(MANIFEST_MEMBER.to_string())
        .chain(
            manifest
                .descriptors
                .iter()
                .map(|descriptor| descriptor.path.clone()),
        )
        .chain(
            members
                .keys()
                .filter(|name| *name == SIGNATURE_MEMBER)
                .cloned(),
        )
        .collect();
    if names != expected_names {
        return Err(bundle_error(
            ErrorCode::BundleUnsafeMember,
            "archive contains an unlisted or missing member",
        ));
    }
    for descriptor in &manifest.descriptors {
        let bytes = members.get(&descriptor.path).expect("descriptor checked");
        if bytes.is_empty() && descriptor.object_count != 0 {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "empty JSONL member has a non-zero object count",
            ));
        }
        if bytes.iter().filter(|byte| **byte == b'\n').count() != descriptor.object_count {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "descriptor object count does not match JSONL lines",
            ));
        }
    }
    let signature_status = if members.contains_key(SIGNATURE_MEMBER) {
        "present_unverified".to_string()
    } else {
        "absent".to_string()
    };
    validate_payload_objects(&manifest, &members)?;
    Ok(BundlePayload {
        manifest,
        manifest_bytes,
        members,
        signature_status,
    })
}

fn validate_member_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('\0')
        || name.contains('\\')
        || name.starts_with('/')
        || name.split('/').any(|part| part == ".." || part.is_empty())
        || Path::new(name)
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(bundle_error(
            ErrorCode::BundleUnsafeMember,
            "unsafe ZIP member path",
        ));
    }
    Ok(())
}

fn validate_manifest(manifest: &BundleManifest) -> Result<()> {
    if manifest.media_type != MEDIA_TYPE || manifest.format_version != FORMAT_VERSION {
        return Err(bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "unsupported portable bundle format",
        ));
    }
    validate_timestamp(&manifest.created_at)?;
    validate_scalar(&manifest.producer.instance_id, "instance_id")?;
    if !manifest
        .producer
        .instance_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
    {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "instance_id contains an unsafe character",
        ));
    }
    validate_scalar(&manifest.producer.tool_version, "tool_version")?;
    validate_scalar(&manifest.intent.source_profile, "source_profile")?;
    validate_scalar(&manifest.intent.source_workspace, "source_workspace")?;
    validate_scalar(&manifest.intent.target_profile, "target_profile")?;
    if let Some(value) = &manifest.intent.source_repo_id {
        validate_scalar(value, "source_repo_id")?;
    }
    if let Some(value) = &manifest.intent.target_workspace {
        validate_scalar(value, "target_workspace")?;
    }
    if let Some(value) = &manifest.intent.target_repo_id {
        validate_scalar(value, "target_repo_id")?;
    }
    validate_scalar(&manifest.policy.ruleset_version, "ruleset_version")?;
    validate_scalar(&manifest.policy.boundary_decision, "boundary_decision")?;
    for feature in &manifest.required_features {
        validate_scalar(feature, "required_feature")?;
    }
    for record_id in &manifest.selection.record_ids {
        validate_scalar(record_id, "selection record id")?;
        if ids::parse_public_handle(record_id) != Some(PublicHandleKind::MemoryRef) {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "selection contains a non-memory public handle",
            ));
        }
    }
    if manifest.producer.storage_schema_version < 0
        || manifest.descriptors.len() != OBJECT_MEMBERS.len()
        || !manifest.required_features.is_empty()
    {
        return Err(bundle_error(
            if !manifest.required_features.is_empty() {
                ErrorCode::BundleRequiredFeatureUnsupported
            } else {
                ErrorCode::BundleIntegrityFailed
            },
            "manifest contains unsupported required data",
        ));
    }
    let expected_paths: BTreeSet<&str> = OBJECT_MEMBERS.iter().copied().collect();
    let actual_paths: BTreeSet<&str> = manifest
        .descriptors
        .iter()
        .map(|descriptor| descriptor.path.as_str())
        .collect();
    if actual_paths != expected_paths
        || manifest
            .descriptors
            .iter()
            .map(|descriptor| descriptor.path.as_str())
            .collect::<Vec<_>>()
            != OBJECT_MEMBERS.as_slice()
    {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "manifest descriptors do not match the fixed bundle member set",
        ));
    }
    for descriptor in &manifest.descriptors {
        let kind = kind_for_member(&descriptor.path).ok_or_else(|| {
            bundle_error(
                ErrorCode::BundleSchemaUnsupported,
                "unsupported bundle member",
            )
        })?;
        if !descriptor.required
            || descriptor.media_type != kind.media_type()
            || descriptor.size_bytes > MAX_MEMBER_BYTES as u64
            || descriptor.object_count > MAX_OBJECTS
        {
            return Err(bundle_error(
                ErrorCode::BundleSchemaUnsupported,
                "unsupported or oversized bundle descriptor",
            ));
        }
    }
    let object_count = manifest
        .counts
        .root_memories
        .checked_add(manifest.counts.dependency_objects)
        .ok_or_else(|| {
            bundle_error(
                ErrorCode::BundleLimitExceeded,
                "bundle object count overflow",
            )
        })?;
    let count_values = [
        manifest.counts.root_memories,
        manifest.counts.dependency_objects,
        manifest.counts.omitted_secret,
        manifest.counts.omitted_quarantined,
        manifest.counts.omitted_portability,
        manifest.counts.omitted_boundary,
        manifest.counts.omitted_unsafe_path,
        manifest.counts.external_references,
    ];
    if manifest.counts.root_memories > MAX_ROOT_MEMORIES
        || object_count > MAX_OBJECTS
        || count_values.iter().any(|value| *value > MAX_OBJECTS)
    {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "bundle object count limit exceeded",
        ));
    }
    if manifest.selection.record_ids.len() > MAX_ROOT_MEMORIES {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "bundle selection limit exceeded",
        ));
    }
    Ok(())
}

fn validate_payload_objects(
    manifest: &BundleManifest,
    members: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let mut refs = BTreeSet::new();
    let mut present = BTreeSet::new();
    let mut member_counts = BTreeMap::<ObjectKind, usize>::new();
    for descriptor in &manifest.descriptors {
        let kind = kind_for_member(&descriptor.path).expect("manifest checked");
        let bytes = members.get(&descriptor.path).expect("member checked");
        let mut previous = None;
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if !line.ends_with(b"\n") {
                return Err(bundle_error(
                    ErrorCode::BundleIntegrityFailed,
                    "JSONL member must end each object with a newline",
                ));
            }
            if line.len() == 1 {
                return Err(bundle_error(
                    ErrorCode::BundleIntegrityFailed,
                    "JSONL member contains an empty line",
                ));
            }
            let json_bytes = &line[..line.len() - 1];
            let value: Value = parse_json(json_bytes)?;
            let canonical = canonical_json(&value)?;
            if canonical != json_bytes {
                return Err(bundle_error(
                    ErrorCode::BundleIntegrityFailed,
                    "JSONL object is not RFC 8785 canonical JSON",
                ));
            }
            let envelope: ObjectEnvelope<Value> = parse_json(json_bytes)?;
            if envelope.schema != kind.schema() {
                return Err(bundle_error(
                    ErrorCode::BundleSchemaUnsupported,
                    "unsupported object schema",
                ));
            }
            envelope.portable_ref.validate(Some(kind.as_str()))?;
            if envelope.aliases.len() > MAX_ALIASES {
                return Err(bundle_error(
                    ErrorCode::BundleLimitExceeded,
                    "object alias limit exceeded",
                ));
            }
            let body_digest = ids::sha256_hex(&canonical_json(&envelope.body)?);
            if body_digest != envelope.digest {
                return Err(bundle_error(
                    ErrorCode::BundleIntegrityFailed,
                    "object body digest does not match",
                ));
            }
            let envelope_ref_key = ref_key(&envelope.portable_ref);
            if !refs.insert(envelope_ref_key.clone()) {
                return Err(bundle_error(
                    ErrorCode::BundleDuplicateIdentity,
                    "duplicate portable object identity",
                ));
            }
            for alias in &envelope.aliases {
                alias.validate(Some(kind.as_str()))?;
                if !refs.insert(ref_key(alias)) {
                    return Err(bundle_error(
                        ErrorCode::BundleDuplicateIdentity,
                        "duplicate portable alias identity",
                    ));
                }
            }
            if previous.is_some_and(|prior: PortableRef| prior >= envelope.portable_ref) {
                return Err(bundle_error(
                    ErrorCode::BundleIntegrityFailed,
                    "JSONL objects are not sorted by portable reference",
                ));
            }
            previous = Some(envelope.portable_ref.clone());
            present.insert(envelope_ref_key);
            *member_counts.entry(kind).or_default() += 1;
        }
    }
    if member_counts.get(&ObjectKind::Memory).copied().unwrap_or(0) != manifest.counts.root_memories
        || member_counts
            .get(&ObjectKind::Subject)
            .copied()
            .unwrap_or(0)
            + member_counts
                .get(&ObjectKind::Episode)
                .copied()
                .unwrap_or(0)
            + member_counts.get(&ObjectKind::Source).copied().unwrap_or(0)
            + member_counts
                .get(&ObjectKind::Evidence)
                .copied()
                .unwrap_or(0)
            != manifest.counts.dependency_objects
        || manifest.selection.record_ids.len() != manifest.counts.root_memories
    {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "manifest object counts do not match the payload",
        ));
    }
    let subjects =
        parse_member::<SubjectBody>(members, "objects/subjects.jsonl", ObjectKind::Subject)?;
    for subject in subjects {
        sanitized_scalar(&subject.body.profile, 128)?;
        sanitized_scalar(&subject.body.workspace, 512)?;
        sanitized_scalar(&subject.body.subject_key, 512)?;
        SubjectKind::parse(&subject.body.kind).ok_or_else(|| {
            bundle_error(
                ErrorCode::BundleSchemaUnsupported,
                "unsupported subject kind",
            )
        })?;
        sanitized_scalar(&subject.body.display_name, 512)?;
        validate_safe_metadata(&subject.body.metadata)?;
        validate_timestamp(&subject.body.created_at)?;
        validate_timestamp(&subject.body.updated_at)?;
    }
    let episodes =
        parse_member::<EpisodeBody>(members, "objects/episodes.jsonl", ObjectKind::Episode)?;
    for episode in episodes {
        episode.body.subject_ref.validate(Some("subject"))?;
        validate_timestamp(&episode.body.created_at)?;
        validate_timestamp(&episode.body.updated_at)?;
        for value in [&episode.body.started_at, &episode.body.ended_at] {
            if let Some(value) = value {
                validate_timestamp(value)?;
            }
        }
        screened_string(&episode.body.summary, MAX_RECORD_CHARS)?;
        validate_safe_metadata(&episode.body.source_metadata)?;
        validate_safe_metadata(&episode.body.metadata)?;
    }
    let sources = parse_member::<SourceBody>(members, "objects/sources.jsonl", ObjectKind::Source)?;
    for source in sources {
        if let Some(path) = &source.body.source_path {
            if !valid_repo_path(path) {
                return Err(bundle_error(
                    ErrorCode::BundleIntegrityFailed,
                    "source contains an unsafe repository-relative path",
                ));
            }
        }
        validate_timestamp(&source.body.created_at)?;
        validate_timestamp(&source.body.ingested_at)?;
        validate_safe_metadata(&source.body.metadata)?;
    }
    let evidence =
        parse_member::<EvidenceBody>(members, "objects/evidence.jsonl", ObjectKind::Evidence)?;
    for entry in &evidence {
        sanitized_scalar(&entry.body.profile, 128)?;
        sanitized_scalar(&entry.body.workspace, 512)?;
        if let Some(repo_id) = &entry.body.repo_id {
            sanitized_scalar(repo_id, 512)?;
        }
        if let Some(subject_key) = &entry.body.subject_key {
            sanitized_scalar(subject_key, 512)?;
        }
        sanitized_scalar(&entry.body.source_kind, 128)?;
        sanitized_scalar(&entry.body.source_hash, 256)?;
        sanitized_scalar(&entry.body.policy_state, 128)?;
        if let Some(subject_ref) = &entry.body.subject_ref {
            subject_ref.validate(Some("subject"))?;
            if !present.contains(&ref_key(subject_ref))
                && !entry
                    .body
                    .external_refs
                    .iter()
                    .any(|external| external == subject_ref)
            {
                return Err(bundle_error(
                    ErrorCode::BundleMissingDependency,
                    "evidence references a missing subject",
                ));
            }
        }
        if let Some(reference) = &entry.body.source_ref {
            reference.validate(Some("source"))?;
            if !present.contains(&ref_key(reference))
                && !entry
                    .body
                    .external_refs
                    .iter()
                    .any(|external| external == reference)
            {
                return Err(bundle_error(
                    ErrorCode::BundleMissingDependency,
                    "evidence references a missing source",
                ));
            }
        }
        if entry.body.external_refs.len() > MAX_REFS {
            return Err(bundle_error(
                ErrorCode::BundleLimitExceeded,
                "evidence external-reference limit exceeded",
            ));
        }
        for external in &entry.body.external_refs {
            external.validate(None)?;
        }
        if let Some(path) = &entry.body.source_path {
            if !valid_repo_path(path) {
                return Err(bundle_error(
                    ErrorCode::BundleIntegrityFailed,
                    "evidence contains an unsafe repository-relative path",
                ));
            }
        }
        screened_string(&entry.body.safe_summary, MAX_RECORD_CHARS)?;
        validate_safe_metadata(&entry.body.metadata)?;
        validate_timestamp(&entry.body.created_at)?;
    }
    let memories =
        parse_member::<MemoryBody>(members, "objects/memories.jsonl", ObjectKind::Memory)?;
    let external_reference_count = memories
        .iter()
        .map(|memory| memory.body.external_refs.len())
        .chain(
            evidence
                .iter()
                .map(|evidence| evidence.body.external_refs.len()),
        )
        .try_fold(0usize, |total, count| total.checked_add(count))
        .ok_or_else(|| {
            bundle_error(
                ErrorCode::BundleLimitExceeded,
                "external-reference count overflow",
            )
        })?;
    let memory_ids: BTreeSet<String> = memories
        .iter()
        .map(|memory| memory.portable_ref.id.clone())
        .collect();
    let selection_ids: BTreeSet<String> = manifest.selection.record_ids.iter().cloned().collect();
    if selection_ids.len() != manifest.selection.record_ids.len()
        || selection_ids != memory_ids
        || external_reference_count != manifest.counts.external_references
    {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "manifest selection or external-reference counts do not match the payload",
        ));
    }
    for memory in memories {
        validate_memory_body(&memory.body)?;
        validate_safe_metadata(&memory.body.metadata)?;
        if let Some(reference) = &memory.body.subject_ref {
            validate_reference_present(reference, "subject", &present, &memory.body.external_refs)?;
        }
        if let Some(reference) = &memory.body.episode_ref {
            validate_reference_present(reference, "episode", &present, &memory.body.external_refs)?;
        }
        for reference in &memory.body.source_refs {
            validate_reference_present(reference, "source", &present, &memory.body.external_refs)?;
        }
        for reference in &memory.body.supersedes {
            validate_reference_present(reference, "memory", &present, &memory.body.external_refs)?;
        }
        if let Some(reference) = &memory.body.superseded_by {
            validate_reference_present(reference, "memory", &present, &memory.body.external_refs)?;
        }
        for reference in &memory.body.external_refs {
            reference.validate(None)?;
        }
    }
    Ok(())
}

fn validate_reference_present(
    reference: &PortableRef,
    expected_kind: &str,
    present: &BTreeSet<String>,
    external_refs: &[PortableRef],
) -> Result<()> {
    reference.validate(Some(expected_kind))?;
    if !present.contains(&ref_key(reference))
        && !external_refs.iter().any(|external| external == reference)
    {
        return Err(bundle_error(
            ErrorCode::BundleMissingDependency,
            "portable object references a missing dependency",
        ));
    }
    Ok(())
}

fn parse_member<T: DeserializeOwned>(
    members: &BTreeMap<String, Vec<u8>>,
    path: &str,
    kind: ObjectKind,
) -> Result<Vec<ObjectEnvelope<T>>> {
    let bytes = members.get(path).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleMissingDependency,
            "required member missing",
        )
    })?;
    let mut result = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.len() <= 1 {
            continue;
        }
        let envelope: ObjectEnvelope<T> = parse_json(&line[..line.len() - 1])?;
        if envelope.schema != kind.schema() {
            return Err(bundle_error(
                ErrorCode::BundleSchemaUnsupported,
                "object schema does not match its member",
            ));
        }
        result.push(envelope);
    }
    Ok(result)
}

fn kind_for_member(path: &str) -> Option<ObjectKind> {
    match path {
        "objects/subjects.jsonl" => Some(ObjectKind::Subject),
        "objects/episodes.jsonl" => Some(ObjectKind::Episode),
        "objects/sources.jsonl" => Some(ObjectKind::Source),
        "objects/evidence.jsonl" => Some(ObjectKind::Evidence),
        "objects/memories.jsonl" => Some(ObjectKind::Memory),
        _ => None,
    }
}

fn manifest_digest_without_id_value(value: &Value) -> Result<String> {
    let mut value = value.clone();
    value
        .as_object_mut()
        .ok_or_else(|| {
            bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "manifest is not an object",
            )
        })?
        .remove("bundle_id");
    Ok(ids::sha256_hex(&canonical_json(&value)?))
}

fn ref_key(reference: &PortableRef) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        reference.origin_instance_id, reference.kind, reference.id
    )
}

fn bundle_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

fn validate_scalar(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 512
        || value.contains('\0')
        || value.chars().any(|c| c.is_control())
    {
        return Err(Error::invalid_request(format!("invalid bundle {label}")));
    }
    Ok(())
}

fn validate_timestamp(value: &str) -> Result<()> {
    let parsed = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| {
        bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "invalid RFC3339 timestamp",
        )
    })?;
    if parsed.offset().whole_seconds() != 0 || !value.ends_with('Z') {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "bundle timestamps must be UTC RFC3339 values",
        ));
    }
    Ok(())
}

fn write_zip(mut file: File, payload: &BundlePayload) -> Result<()> {
    let mut writer = ZipWriter::new(&mut file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    writer.start_file(MANIFEST_MEMBER, options).map_err(|err| {
        bundle_error(
            ErrorCode::BundleIntegrityFailed,
            format!("zip write: {err}"),
        )
    })?;
    writer.write_all(&payload.manifest_bytes)?;
    for name in OBJECT_MEMBERS {
        writer.start_file(name, options).map_err(|err| {
            bundle_error(
                ErrorCode::BundleIntegrityFailed,
                format!("zip write: {err}"),
            )
        })?;
        writer.write_all(payload.members.get(name).expect("object member exists"))?;
    }
    writer.finish().map_err(|err| {
        bundle_error(
            ErrorCode::BundleIntegrityFailed,
            format!("zip write: {err}"),
        )
    })?;
    file.sync_all()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Source selection, closure, and sanitization
// ---------------------------------------------------------------------------

fn build_export_payload(store: &Store, options: &BundleExportOptions) -> Result<BundlePayload> {
    let (source_profile, target_profile) = options.validate()?;
    let instance_id = store.instance_id()?;
    let query = RecordQuery {
        profile_id: Some(options.profile.clone()),
        workspace_id: Some(options.workspace.clone()),
        repo_id: options.repo_id.clone(),
        include_archived: options.include_archived,
        limit: 0,
        offset: 0,
        ..RecordQuery::default()
    };
    let (records, omitted_secret, omitted_quarantined) = store.export_records(&query)?;
    let explicit_ids: BTreeSet<String> = options
        .record_ids
        .iter()
        .map(|value| value.trim().to_string())
        .collect();
    let mut roots = Vec::new();
    let mut root_ids = BTreeSet::new();
    let mut counts = BundleManifestCounts {
        omitted_secret,
        omitted_quarantined,
        ..BundleManifestCounts::default()
    };
    let target_workspace = options
        .target_workspace
        .as_deref()
        .unwrap_or(&options.workspace);
    let target_repo = options
        .target_repo_id
        .as_deref()
        .or(options.repo_id.as_deref());
    let dependencies_portable = matches!(
        policy::export_boundary(source_profile, target_profile),
        BoundaryDecision::Allow
    );

    for record in records {
        let public_id = ids::public_handle(PublicHandleKind::MemoryRef, &record.id);
        if !explicit_ids.is_empty()
            && !explicit_ids.contains(&record.id)
            && !explicit_ids.contains(&public_id)
        {
            continue;
        }
        if record.portability == Portability::NeverExport {
            counts.omitted_portability += 1;
            continue;
        }
        if matches!(
            policy::export_boundary(source_profile, target_profile),
            BoundaryDecision::AllowGenericPreferencesOnly
        ) && !policy::is_generic_preference(record.record_type, record.sensitivity)
        {
            counts.omitted_boundary += 1;
            continue;
        }
        if record.portability == Portability::ProfileOnly
            && options.profile != options.target_profile
        {
            counts.omitted_portability += 1;
            continue;
        }
        if record.portability == Portability::WorkspaceOnly && options.workspace != target_workspace
        {
            counts.omitted_portability += 1;
            continue;
        }
        if record.scope == Scope::Repo
            && (record.repo_id.as_deref() != options.repo_id.as_deref() || target_repo.is_none())
        {
            counts.omitted_portability += 1;
            continue;
        }
        match policy::screen_content(&record.content, MAX_RECORD_CHARS) {
            PolicyDecision::Accept(_) => {}
            PolicyDecision::Reject { .. } => {
                counts.omitted_boundary += 1;
                continue;
            }
        }
        root_ids.insert(record.id.clone());
        roots.push(record);
    }
    if roots.len() > MAX_ROOT_MEMORIES {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "selected root memory limit exceeded",
        ));
    }
    if !explicit_ids.is_empty() && roots.len() != explicit_ids.len() {
        // Missing explicit ids are deliberately not reported with the supplied
        // value: an opaque reference is enough for an actionable diagnosis.
        return Err(bundle_error(
            ErrorCode::BundlePolicyDenied,
            "one or more explicitly selected memories are not exportable",
        ));
    }
    roots.sort_by(|a, b| a.id.cmp(&b.id));
    counts.root_memories = roots.len();

    let mut subjects = BTreeMap::new();
    let mut episodes = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for record in &roots {
        if !dependencies_portable {
            continue;
        }
        if let Some(id) = &record.subject_id {
            if let Some(subject) =
                store.get_subject(&record.profile_id, &record.workspace_id, id)?
            {
                if export_scope_matches(&subject.profile_id, &subject.workspace_id, options)
                    && safe_subject(&subject).is_ok()
                {
                    subjects.insert(subject.id.clone(), subject);
                }
            } else {
                return Err(bundle_error(
                    ErrorCode::BundleMissingDependency,
                    "selected memory references a missing subject",
                ));
            }
        }
        if let Some(id) = &record.episode_id {
            if let Some(episode) =
                store.get_episode(&record.profile_id, &record.workspace_id, id)?
            {
                if subjects.contains_key(&episode.subject_id)
                    && export_scope_matches(&episode.profile_id, &episode.workspace_id, options)
                    && safe_episode(&episode).is_ok()
                {
                    episodes.insert(episode.id.clone(), episode);
                }
            } else {
                return Err(bundle_error(
                    ErrorCode::BundleMissingDependency,
                    "selected memory references a missing episode",
                ));
            }
        }
        for source_id in &record.source_ids {
            if let Some(source) = store.get_source(source_id)? {
                if export_scope_matches(&source.profile_id, &source.workspace_id, options)
                    && safe_source(&source).is_ok()
                {
                    sources.insert(source.id.clone(), source);
                }
            } else {
                return Err(bundle_error(
                    ErrorCode::BundleMissingDependency,
                    "selected memory references a missing source",
                ));
            }
        }
    }

    let mut subject_refs = BTreeMap::new();
    let mut subject_objects = Vec::new();
    for subject in subjects.values() {
        let body = subject_body(subject)?;
        let digest = body_digest(&body)?;
        let reference = choose_export_ref(store, ObjectKind::Subject, &subject.id, digest.clone())?;
        subject_refs.insert(subject.id.clone(), reference.clone());
        subject_objects.push(ObjectEnvelope {
            schema: ObjectKind::Subject.schema().to_string(),
            portable_ref: reference,
            aliases: Vec::new(),
            digest,
            body,
        });
    }

    let mut source_refs = BTreeMap::new();
    let mut source_objects = Vec::new();
    for source in sources.values() {
        let body = source_body(source)?;
        let digest = body_digest(&body)?;
        let reference = choose_export_ref(store, ObjectKind::Source, &source.id, digest.clone())?;
        source_refs.insert(source.id.clone(), reference.clone());
        source_objects.push(ObjectEnvelope {
            schema: ObjectKind::Source.schema().to_string(),
            portable_ref: reference,
            aliases: Vec::new(),
            digest,
            body,
        });
    }

    let mut episode_refs = BTreeMap::new();
    let mut episode_objects = Vec::new();
    for episode in episodes.values() {
        let body = episode_body(episode, subject_refs.get(&episode.subject_id))?;
        let digest = body_digest(&body)?;
        let reference = choose_export_ref(store, ObjectKind::Episode, &episode.id, digest.clone())?;
        episode_refs.insert(episode.id.clone(), reference.clone());
        episode_objects.push(ObjectEnvelope {
            schema: ObjectKind::Episode.schema().to_string(),
            portable_ref: reference,
            aliases: Vec::new(),
            digest,
            body,
        });
    }

    let included_source_ids: BTreeSet<String> = sources.keys().cloned().collect();
    let included_subject_keys: BTreeSet<String> = subjects
        .values()
        .map(|subject| subject.subject_key.clone())
        .collect();
    let subject_key_refs: BTreeMap<String, PortableRef> = subjects
        .values()
        .filter_map(|subject| {
            subject_refs
                .get(&subject.id)
                .cloned()
                .map(|reference| (subject.subject_key.clone(), reference))
        })
        .collect();
    let mut evidence_objects = Vec::new();
    if dependencies_portable {
        for evidence in store.list_evidence_ledger(&options.profile, &options.workspace)? {
            let linked_to_source = evidence
                .source_id
                .as_ref()
                .is_some_and(|source_id| included_source_ids.contains(source_id));
            let linked_to_subject = evidence
                .subject_key
                .as_ref()
                .is_some_and(|subject_key| included_subject_keys.contains(subject_key));
            if !linked_to_source && !linked_to_subject {
                continue;
            }
            if evidence.trust_state == "quarantined" {
                counts.omitted_quarantined += 1;
                continue;
            }
            if evidence.policy_state != "accepted"
                || matches!(
                    policy::screen_content(&evidence.safe_summary, MAX_RECORD_CHARS),
                    PolicyDecision::Reject { .. }
                )
            {
                counts.omitted_boundary += 1;
                continue;
            }
            let body = evidence_body(
                &evidence,
                &source_refs,
                &subject_key_refs,
                &instance_id,
                &mut counts,
            )?;
            let digest = body_digest(&body)?;
            counts.external_references += body.external_refs.len();
            let reference =
                choose_export_ref(store, ObjectKind::Evidence, &evidence.id, digest.clone())?;
            evidence_objects.push(ObjectEnvelope {
                schema: ObjectKind::Evidence.schema().to_string(),
                portable_ref: reference,
                aliases: Vec::new(),
                digest,
                body,
            });
        }
    }
    sort_envelopes(&mut evidence_objects);

    let mut memory_objects = Vec::new();
    for record in &roots {
        let body = memory_body(
            record,
            &subject_refs,
            &episode_refs,
            &source_refs,
            &root_ids,
            store,
            &instance_id,
            &mut counts,
        )?;
        let digest = body_digest(&body)?;
        counts.external_references += body.external_refs.len();
        let reference = choose_export_ref(store, ObjectKind::Memory, &record.id, digest.clone())?;
        memory_objects.push(ObjectEnvelope {
            schema: ObjectKind::Memory.schema().to_string(),
            portable_ref: reference,
            aliases: Vec::new(),
            digest,
            body,
        });
    }
    sort_envelopes(&mut subject_objects);
    sort_envelopes(&mut episode_objects);
    sort_envelopes(&mut source_objects);
    memory_objects.sort_by(|a, b| a.portable_ref.cmp(&b.portable_ref));
    counts.dependency_objects = subject_objects.len()
        + episode_objects.len()
        + source_objects.len()
        + evidence_objects.len();
    if counts.external_references > MAX_OBJECTS {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "portable external-reference limit exceeded",
        ));
    }
    let graph = ExportGraph {
        subjects: subject_objects,
        episodes: episode_objects,
        sources: source_objects,
        evidence: evidence_objects,
        memories: memory_objects,
        counts,
    };
    let members = graph_members(&graph)?;
    let descriptors = descriptors_for(&members)?;
    let created_at = options.created_at.clone().unwrap_or_else(ids::now_rfc3339);
    validate_timestamp(&created_at)?;
    let boundary_decision = match policy::export_boundary(source_profile, target_profile) {
        BoundaryDecision::Allow => "allow",
        BoundaryDecision::AllowGenericPreferencesOnly => "allow_generic_preferences_only",
        BoundaryDecision::Deny { .. } => "deny",
    };
    let manifest = BundleManifest {
        media_type: MEDIA_TYPE.to_string(),
        format_version: FORMAT_VERSION,
        bundle_id: String::new(),
        created_at,
        producer: Producer {
            instance_id,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            storage_schema_version: store::STORAGE_SCHEMA_VERSION,
        },
        intent: BundleIntent {
            source_profile: options.profile.clone(),
            source_workspace: options.workspace.clone(),
            source_repo_id: options.repo_id.clone(),
            target_profile: options.target_profile.clone(),
            target_workspace: options.target_workspace.clone(),
            target_repo_id: options.target_repo_id.clone(),
        },
        selection: BundleSelection {
            record_ids: graph
                .memories
                .iter()
                .map(|memory| memory.portable_ref.id.clone())
                .collect(),
            include_archived: options.include_archived,
        },
        policy: BundlePolicy {
            ruleset_version: RULESET_FINGERPRINT.to_string(),
            boundary_decision: boundary_decision.to_string(),
        },
        descriptors,
        counts: graph.counts,
        required_features: Vec::new(),
        extensions: BTreeMap::new(),
    };
    let mut manifest = manifest;
    let mut manifest_value = serde_json::to_value(&manifest)?;
    let bundle_id = manifest_digest_without_id_value(&manifest_value)?;
    manifest.bundle_id = bundle_id;
    manifest_value = serde_json::to_value(&manifest)?;
    let manifest_bytes = canonical_json(&manifest_value)?;
    Ok(BundlePayload {
        manifest,
        manifest_bytes,
        members,
        signature_status: "absent".to_string(),
    })
}

fn export_scope_matches(profile: &str, workspace: &str, options: &BundleExportOptions) -> bool {
    profile == options.profile
        && workspace == options.workspace
        && options.repo_id.as_deref().is_none_or(|_| true)
}

fn subject_body(subject: &Subject) -> Result<SubjectBody> {
    validate_timestamp(&subject.created_at)?;
    validate_timestamp(&subject.updated_at)?;
    Ok(SubjectBody {
        profile: subject.profile_id.clone(),
        workspace: subject.workspace_id.clone(),
        subject_key: sanitized_scalar(&subject.subject_key, 512)?,
        kind: subject.kind.as_str().to_string(),
        display_name: sanitized_scalar(&subject.display_name, 512)?,
        created_at: subject.created_at.clone(),
        updated_at: subject.updated_at.clone(),
        metadata: safe_metadata(&subject.metadata),
    })
}

fn episode_body(episode: &Episode, subject_ref: Option<&PortableRef>) -> Result<EpisodeBody> {
    let subject_ref = subject_ref.ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleMissingDependency,
            "episode subject dependency was omitted",
        )
    })?;
    validate_timestamp(&episode.created_at)?;
    validate_timestamp(&episode.updated_at)?;
    for value in [&episode.started_at, &episode.ended_at] {
        if let Some(value) = value {
            validate_timestamp(value)?;
        }
    }
    Ok(EpisodeBody {
        profile: episode.profile_id.clone(),
        workspace: episode.workspace_id.clone(),
        subject_ref: subject_ref.clone(),
        source_kind: sanitized_scalar(&episode.source_kind, 128)?,
        source_ref: sanitized_scalar(&episode.source_ref, 512)?,
        started_at: episode.started_at.clone(),
        ended_at: episode.ended_at.clone(),
        status: episode
            .status
            .as_deref()
            .map(|value| sanitized_scalar(value, 128))
            .transpose()?,
        summary: screened_string(&episode.summary, MAX_RECORD_CHARS)?,
        trust_level: episode
            .trust_level
            .as_deref()
            .map(|value| sanitized_scalar(value, 128))
            .transpose()?,
        source_metadata: safe_metadata(&episode.source_metadata),
        created_at: episode.created_at.clone(),
        updated_at: episode.updated_at.clone(),
        metadata: safe_metadata(&episode.metadata),
    })
}

fn source_body(source: &MemorySource) -> Result<SourceBody> {
    validate_timestamp(&source.created_at)?;
    validate_timestamp(&source.ingested_at)?;
    Ok(SourceBody {
        profile: source.profile_id.clone(),
        workspace: source.workspace_id.clone(),
        kind: sanitized_scalar(&source.kind, 128)?,
        source_path: source
            .source_path
            .as_deref()
            .map(|path| {
                if valid_repo_path(path) {
                    Ok(path.to_string())
                } else {
                    Err(bundle_error(
                        ErrorCode::BundleIntegrityFailed,
                        "source contains an unsafe repository-relative path",
                    ))
                }
            })
            .transpose()?,
        source_hash: sanitized_scalar(&source.source_hash, 256)?,
        created_at: source.created_at.clone(),
        ingested_at: source.ingested_at.clone(),
        metadata: safe_metadata(&source.metadata),
    })
}

fn evidence_body(
    evidence: &store::EvidenceLedgerRecord,
    source_refs: &BTreeMap<String, PortableRef>,
    subject_refs: &BTreeMap<String, PortableRef>,
    instance_id: &str,
    counts: &mut BundleManifestCounts,
) -> Result<EvidenceBody> {
    let source_ref = evidence
        .source_id
        .as_ref()
        .and_then(|source_id| source_refs.get(source_id))
        .cloned();
    let mut external_refs = Vec::new();
    if let Some(source_id) = &evidence.source_id {
        if source_ref.is_none() {
            external_refs.push(PortableRef {
                origin_instance_id: instance_id.to_string(),
                kind: "source".to_string(),
                id: ids::public_handle(PublicHandleKind::SourceRef, source_id),
            });
        }
    }
    let subject_ref = evidence
        .subject_key
        .as_ref()
        .and_then(|subject_key| subject_refs.get(subject_key))
        .cloned();
    let source_path = if let Some(path) = &evidence.source_path {
        if valid_repo_path(path) {
            Some(path.clone())
        } else {
            counts.omitted_unsafe_path += 1;
            None
        }
    } else {
        None
    };
    let safe_summary = screened_string(&evidence.safe_summary, MAX_RECORD_CHARS)?;
    let metadata = safe_metadata(&evidence.metadata);
    Ok(EvidenceBody {
        profile: evidence.profile_id.clone(),
        workspace: evidence.workspace_id.clone(),
        repo_id: evidence.repo_id.clone(),
        subject_key: evidence.subject_key.clone(),
        source_kind: sanitized_scalar(&evidence.source_kind, 128)?,
        source_ref,
        subject_ref,
        source_path,
        source_hash: sanitized_scalar(&evidence.source_hash, 256)?,
        safe_summary,
        policy_state: sanitized_scalar(&evidence.policy_state, 128)?,
        created_at: evidence.created_at.clone(),
        metadata,
        external_refs,
    })
}

fn memory_body(
    record: &MemoryRecord,
    subject_refs: &BTreeMap<String, PortableRef>,
    episode_refs: &BTreeMap<String, PortableRef>,
    source_refs: &BTreeMap<String, PortableRef>,
    selected_ids: &BTreeSet<String>,
    store: &Store,
    instance_id: &str,
    counts: &mut BundleManifestCounts,
) -> Result<MemoryBody> {
    let content = screened_string(&record.content, MAX_RECORD_CHARS)?;
    let mut related_files = Vec::new();
    if record.related_files.len() > MAX_FILES {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "memory related-file limit exceeded",
        ));
    }
    for path in &record.related_files {
        if valid_repo_path(path) {
            related_files.push(path.clone());
        } else {
            counts.omitted_unsafe_path += 1;
        }
    }
    if record.tags.len() > MAX_TAGS {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "memory tag limit exceeded",
        ));
    }
    let tags = record
        .tags
        .iter()
        .map(|tag| sanitized_scalar(tag, 256))
        .collect::<Result<Vec<_>>>()?;
    let mut source_ref_values = Vec::new();
    let mut external_refs = Vec::new();
    for id in &record.source_ids {
        if let Some(reference) = source_refs.get(id) {
            source_ref_values.push(reference.clone());
        } else {
            external_refs.push(PortableRef {
                origin_instance_id: instance_id.to_string(),
                kind: "source".to_string(),
                id: ids::public_handle(PublicHandleKind::SourceRef, id),
            });
        }
    }
    let subject_ref = record
        .subject_id
        .as_ref()
        .and_then(|id| subject_refs.get(id))
        .cloned();
    if record.subject_id.is_some() && subject_ref.is_none() {
        external_refs.push(PortableRef {
            origin_instance_id: instance_id.to_string(),
            kind: "subject".to_string(),
            id: ids::public_handle(
                PublicHandleKind::SubjectRef,
                record.subject_id.as_deref().unwrap_or_default(),
            ),
        });
    }
    let episode_ref = record
        .episode_id
        .as_ref()
        .and_then(|id| episode_refs.get(id))
        .cloned();
    if record.episode_id.is_some() && episode_ref.is_none() {
        external_refs.push(PortableRef {
            origin_instance_id: instance_id.to_string(),
            kind: "episode".to_string(),
            id: ids::public_handle(
                PublicHandleKind::EpisodeRef,
                record.episode_id.as_deref().unwrap_or_default(),
            ),
        });
    }
    let mut supersedes = Vec::new();
    for id in &record.supersedes {
        if selected_ids.contains(id) {
            supersedes.push(PortableRef {
                origin_instance_id: instance_id.to_string(),
                kind: "memory".to_string(),
                id: ids::public_handle(PublicHandleKind::MemoryRef, id),
            });
        } else {
            external_refs.push(PortableRef {
                origin_instance_id: instance_id.to_string(),
                kind: "memory".to_string(),
                id: ids::public_handle(PublicHandleKind::MemoryRef, id),
            });
        }
    }
    let superseded_by = record.superseded_by.as_ref().and_then(|id| {
        selected_ids.contains(id).then(|| PortableRef {
            origin_instance_id: instance_id.to_string(),
            kind: "memory".to_string(),
            id: ids::public_handle(PublicHandleKind::MemoryRef, id),
        })
    });
    if let Some(id) = &record.superseded_by {
        if !selected_ids.contains(id) {
            external_refs.push(PortableRef {
                origin_instance_id: instance_id.to_string(),
                kind: "memory".to_string(),
                id: ids::public_handle(PublicHandleKind::MemoryRef, id),
            });
        }
    }
    for reference in portable_external_refs_from_metadata(&record.metadata)? {
        if !external_refs
            .iter()
            .any(|existing: &PortableRef| existing == &reference)
        {
            external_refs.push(reference);
        }
    }
    validate_timestamp(&record.created_at)?;
    validate_timestamp(&record.updated_at)?;
    for value in [
        &record.observed_at,
        &record.valid_from,
        &record.valid_until,
        &record.invalidated_at,
    ] {
        if let Some(value) = value {
            validate_timestamp(value)?;
        }
    }
    if !record.confidence.is_finite() || !record.trust_score.is_finite() {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "memory confidence or trust score is non-finite",
        ));
    }
    if source_ref_values.len() > MAX_REFS
        || supersedes.len() > MAX_REFS
        || external_refs.len() > MAX_REFS
    {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "memory portable-reference limit exceeded",
        ));
    }
    let _ = store; // retained in the signature for future dependency screening.
    Ok(MemoryBody {
        profile: record.profile_id.clone(),
        workspace: record.workspace_id.clone(),
        repo_id: record.repo_id.clone(),
        scope: record.scope.as_str().to_string(),
        record_type: record.record_type.as_str().to_string(),
        content,
        related_files,
        tags,
        sensitivity: record.sensitivity.as_str().to_string(),
        portability: record.portability.as_str().to_string(),
        confidence: record.confidence.clamp(0.0, 1.0),
        subject_ref,
        episode_ref,
        source_refs: source_ref_values,
        supersedes,
        superseded_by,
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
        observed_at: record.observed_at.clone(),
        valid_from: record.valid_from.clone(),
        valid_until: record.valid_until.clone(),
        invalidated_at: record.invalidated_at.clone(),
        archived: record.archived,
        temporal_state: record.temporal_state.as_str().to_string(),
        trust_state: record.trust_state.clone(),
        trust_score: record.trust_score.clamp(0.0, 1.0),
        historical_reason: record.historical_reason.clone(),
        metadata: safe_metadata(&record.metadata),
        external_refs,
    })
}

fn portable_external_refs_from_metadata(metadata: &Value) -> Result<Vec<PortableRef>> {
    let Some(value) = metadata
        .as_object()
        .and_then(|object| object.get("portable_external_refs"))
    else {
        return Ok(Vec::new());
    };
    let references: Vec<PortableRef> = serde_json::from_value(value.clone()).map_err(|_| {
        bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "stored portable external references are invalid",
        )
    })?;
    if references.len() > MAX_REFS {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "stored portable external-reference limit exceeded",
        ));
    }
    for reference in &references {
        reference.validate(None)?;
    }
    Ok(references)
}

fn safe_subject(subject: &Subject) -> Result<()> {
    let body = subject_body(subject)?;
    screened_string(&body.subject_key, 512)?;
    screened_string(&body.display_name, 512)?;
    Ok(())
}

fn safe_episode(episode: &Episode) -> Result<()> {
    let _ = screened_string(&episode.summary, MAX_RECORD_CHARS)?;
    Ok(())
}

fn safe_source(source: &MemorySource) -> Result<()> {
    let _ = source_body(source)?;
    Ok(())
}

fn validate_safe_metadata(metadata: &SafeMetadata) -> Result<()> {
    let value = serde_json::to_value(metadata)?;
    if sanitize_metadata_value(&value).is_none() {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "portable metadata contains an unsafe value",
        ));
    }
    Ok(())
}

fn safe_metadata(value: &Value) -> SafeMetadata {
    let Some(object) = value.as_object() else {
        return SafeMetadata::default();
    };
    SafeMetadata {
        origin: object.get("origin").and_then(sanitize_metadata_value),
        state: object.get("state").and_then(sanitize_metadata_value),
        candidate_state: object
            .get("candidate_state")
            .and_then(sanitize_metadata_value),
        historical_reason: object
            .get("historical_reason")
            .and_then(sanitize_metadata_value),
        temporal_state: object
            .get("temporal_state")
            .and_then(sanitize_metadata_value),
        redaction_state: object
            .get("redaction_state")
            .and_then(sanitize_metadata_value),
        raw_artifact_stored: object
            .get("raw_artifact_stored")
            .and_then(sanitize_metadata_value),
    }
}

fn sanitize_metadata_value(value: &Value) -> Option<Value> {
    let bytes = serde_json::to_vec(value).ok()?;
    if bytes.len() > 64 * 1024 || validate_json_limits(value, 0).is_err() {
        return None;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::String(text) => {
            if matches!(
                policy::screen_content(text, MAX_RECORD_CHARS),
                PolicyDecision::Reject { .. }
            ) || !valid_metadata_string(text)
            {
                None
            } else {
                Some(Value::String(text.clone()))
            }
        }
        Value::Array(values) => {
            let values = values
                .iter()
                .map(sanitize_metadata_value)
                .collect::<Option<Vec<_>>>()?;
            Some(Value::Array(values))
        }
        Value::Object(values) => {
            if values.len() > 128 {
                return None;
            }
            let mut sanitized = Map::new();
            for (key, value) in values {
                if unsafe_metadata_key(key) {
                    return None;
                }
                sanitized.insert(key.clone(), sanitize_metadata_value(value)?);
            }
            Some(Value::Object(sanitized))
        }
    }
}

fn valid_metadata_string(value: &str) -> bool {
    !value.contains('\0')
        && !value.starts_with('/')
        && !value.starts_with('~')
        && !value.starts_with("//")
        && !value.contains("://")
        && !value
            .chars()
            .nth(1)
            .is_some_and(|character| character == ':')
}

fn unsafe_metadata_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "path",
        "root",
        "url",
        "remote",
        "token",
        "secret",
        "credential",
        "password",
        "loader",
        "query",
        "sql",
        "code",
        "command",
        "env",
        "file",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn screened_string(value: &str, max_chars: usize) -> Result<String> {
    match policy::screen_content(value, max_chars) {
        PolicyDecision::Accept(value) => Ok(value),
        PolicyDecision::Reject { code, .. } => Err(bundle_error(
            if code == "secret_detected" {
                ErrorCode::BundlePolicyDenied
            } else {
                ErrorCode::BundleIntegrityFailed
            },
            "portable object content failed policy screening",
        )),
    }
}

fn sanitized_scalar(value: &str, max_len: usize) -> Result<String> {
    validate_scalar(value, "object value")?;
    if value.chars().count() > max_len {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "portable object string limit exceeded",
        ));
    }
    Ok(value.to_string())
}

fn valid_repo_path(path: &str) -> bool {
    if path.is_empty()
        || path.contains('\0')
        || path.contains('\\')
        || path.starts_with('/')
        || path.starts_with('~')
        || path.contains("://")
        || path
            .chars()
            .nth(1)
            .is_some_and(|character| character == ':')
        || path.starts_with("//")
    {
        return false;
    }
    let mut depth = 0i32;
    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            depth -= 1;
            if depth < 0 {
                return false;
            }
        } else {
            depth += 1;
        }
    }
    depth > 0
}

fn body_digest<T: Serialize>(body: &T) -> Result<String> {
    Ok(ids::sha256_hex(&canonical_json(&serde_json::to_value(
        body,
    )?)?))
}

fn choose_export_ref(
    store: &Store,
    kind: ObjectKind,
    local_id: &str,
    digest: String,
) -> Result<PortableRef> {
    for origin in store.portable_origins_for_local(kind.as_str(), local_id)? {
        if origin.canonical && origin.imported_object_digest == digest {
            return Ok(PortableRef {
                origin_instance_id: origin.origin_instance_id,
                kind: kind.as_str().to_string(),
                id: origin.origin_object_id,
            });
        }
    }
    let handle_kind = match kind {
        ObjectKind::Subject => PublicHandleKind::SubjectRef,
        ObjectKind::Episode => PublicHandleKind::EpisodeRef,
        ObjectKind::Source => PublicHandleKind::SourceRef,
        ObjectKind::Memory => PublicHandleKind::MemoryRef,
        ObjectKind::Evidence => PublicHandleKind::EvidenceRef,
    };
    Ok(PortableRef {
        origin_instance_id: store.instance_id()?,
        kind: kind.as_str().to_string(),
        id: ids::public_handle(handle_kind, local_id),
    })
}

fn sort_envelopes<T>(values: &mut [ObjectEnvelope<T>]) {
    values.sort_by(|a, b| a.portable_ref.cmp(&b.portable_ref));
}

fn graph_members(graph: &ExportGraph) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut members = BTreeMap::new();
    members.insert(
        "objects/subjects.jsonl".to_string(),
        jsonl_bytes(&graph.subjects)?,
    );
    members.insert(
        "objects/episodes.jsonl".to_string(),
        jsonl_bytes(&graph.episodes)?,
    );
    members.insert(
        "objects/sources.jsonl".to_string(),
        jsonl_bytes(&graph.sources)?,
    );
    members.insert(
        "objects/evidence.jsonl".to_string(),
        jsonl_bytes(&graph.evidence)?,
    );
    members.insert(
        "objects/memories.jsonl".to_string(),
        jsonl_bytes(&graph.memories)?,
    );
    Ok(members)
}

fn jsonl_bytes<T: Serialize>(values: &[T]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(&canonical_json(&serde_json::to_value(value)?)?);
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn descriptors_for(members: &BTreeMap<String, Vec<u8>>) -> Result<Vec<Descriptor>> {
    OBJECT_MEMBERS
        .iter()
        .map(|path| {
            let bytes = members.get(*path).expect("all object members are present");
            let kind = kind_for_member(path).expect("fixed object member");
            Ok(Descriptor {
                path: (*path).to_string(),
                media_type: kind.media_type().to_string(),
                digest: ids::sha256_hex(bytes),
                size_bytes: bytes.len() as u64,
                object_count: bytes.iter().filter(|byte| **byte == b'\n').count(),
                required: true,
            })
        })
        .collect()
}

fn validate_memory_body(body: &MemoryBody) -> Result<()> {
    Profile::parse(&body.profile).ok_or_else(|| {
        bundle_error(ErrorCode::BundleSchemaUnsupported, "invalid memory profile")
    })?;
    Scope::parse(&body.scope)
        .ok_or_else(|| bundle_error(ErrorCode::BundleSchemaUnsupported, "invalid memory scope"))?;
    RecordType::parse(&body.record_type)
        .ok_or_else(|| bundle_error(ErrorCode::BundleSchemaUnsupported, "invalid memory type"))?;
    Sensitivity::parse(&body.sensitivity).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "invalid memory sensitivity",
        )
    })?;
    Portability::parse(&body.portability).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "invalid memory portability",
        )
    })?;
    TemporalState::parse(&body.temporal_state).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "invalid memory temporal state",
        )
    })?;
    if !body.confidence.is_finite()
        || !body.trust_score.is_finite()
        || body.related_files.len() > MAX_FILES
        || body.tags.len() > MAX_TAGS
        || body.source_refs.len() > MAX_REFS
        || body.supersedes.len() > MAX_REFS
        || body.external_refs.len() > MAX_REFS
    {
        return Err(bundle_error(
            ErrorCode::BundleLimitExceeded,
            "memory reference or numeric limit exceeded",
        ));
    }
    for path in &body.related_files {
        if !valid_repo_path(path) {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "memory contains an unsafe repository-relative path",
            ));
        }
    }
    validate_timestamp(&body.created_at)?;
    validate_timestamp(&body.updated_at)?;
    for value in [
        &body.observed_at,
        &body.valid_from,
        &body.valid_until,
        &body.invalidated_at,
    ] {
        if let Some(value) = value {
            validate_timestamp(value)?;
        }
    }
    Ok(())
}

fn export_report(payload: &BundlePayload, mode: BundleMode) -> BundleReport {
    let manifest = &payload.manifest;
    let counts = BundleCounts {
        discovered: manifest.counts.root_memories + manifest.counts.dependency_objects,
        validated: manifest.counts.root_memories + manifest.counts.dependency_objects,
        ..BundleCounts::default()
    };
    let mut omissions = BTreeMap::new();
    omissions.insert("secret_blocked".to_string(), manifest.counts.omitted_secret);
    omissions.insert(
        "quarantined".to_string(),
        manifest.counts.omitted_quarantined,
    );
    omissions.insert(
        "portability".to_string(),
        manifest.counts.omitted_portability,
    );
    omissions.insert("boundary".to_string(), manifest.counts.omitted_boundary);
    omissions.insert(
        "unsafe_path".to_string(),
        manifest.counts.omitted_unsafe_path,
    );
    omissions.insert(
        "external_reference".to_string(),
        manifest.counts.external_references,
    );
    BundleReport {
        mode: match mode {
            BundleMode::ExportPreview => "export_preview",
            BundleMode::ExportWrite => "export_write",
            _ => "export",
        }
        .to_string(),
        format_version: manifest.format_version,
        bundle_id: manifest.bundle_id.clone(),
        manifest_digest: ids::sha256_hex(&payload.manifest_bytes),
        integrity_status: "verified".to_string(),
        signature_status: payload.signature_status.clone(),
        source_instance_id: manifest.producer.instance_id.clone(),
        destination_instance_id: None,
        source_scope: ScopeSummary {
            profile: manifest.intent.source_profile.clone(),
            workspace: manifest.intent.source_workspace.clone(),
            repo_id: manifest.intent.source_repo_id.clone(),
        },
        destination_scope: None,
        policy_fingerprint: manifest.policy.ruleset_version.clone(),
        mapping_digest: None,
        plan_id: None,
        safe_to_apply: None,
        counts,
        omissions,
        decisions: Vec::new(),
        decision_details_truncated: 0,
        conflicts: Vec::new(),
        warnings: Vec::new(),
        details_truncated: 0,
        receipt: None,
        recall_not_authority: true,
    }
}

#[derive(Debug, Clone)]
struct PlanObject {
    kind: ObjectKind,
    portable_ref: PortableRef,
    digest: String,
    body: Value,
    decision: String,
    reason_code: String,
    destination_id: Option<String>,
    destination_digest: Option<String>,
}

#[derive(Debug, Clone)]
struct ImportPlan {
    report: BundleReport,
    objects: Vec<PlanObject>,
    subject_ids: BTreeMap<String, String>,
    episode_ids: BTreeMap<String, String>,
    source_ids: BTreeMap<String, String>,
    memory_ids: BTreeMap<String, String>,
    mapping_digest: String,
}

#[derive(Debug, Clone)]
struct StoredReceipt {
    receipt_id: String,
    status: String,
    applied_at: String,
    destination_instance_id: String,
    counts: BundleCounts,
}

fn inspect_report(payload: &BundlePayload) -> BundleReport {
    let mut report = export_report(payload, BundleMode::Inspect);
    report.mode = "inspect".to_string();
    report
        .warnings
        .push(if payload.signature_status == "absent" {
            "authenticity_unverified".to_string()
        } else {
            "signature_present_unverified".to_string()
        });
    report
}

fn mapping_digest(options: &BundleImportOptions, manifest: &BundleManifest) -> Result<String> {
    let value = serde_json::json!({
        "source_profile": manifest.intent.source_profile,
        "source_workspace": manifest.intent.source_workspace,
        "source_repo_id": manifest.intent.source_repo_id,
        "target_profile": manifest.intent.target_profile,
        "target_workspace": manifest.intent.target_workspace,
        "target_repo_id": manifest.intent.target_repo_id,
        "destination_profile": options.profile,
        "destination_workspace": options.workspace,
        "destination_repo_id": options.repo_id,
    });
    Ok(ids::sha256_hex(&canonical_json(&value)?))
}

fn plan_import(
    store: &Store,
    payload: &BundlePayload,
    options: &BundleImportOptions,
    tx: Option<&Transaction<'_>>,
    destination_instance_id: &str,
) -> Result<ImportPlan> {
    validate_import_mapping(payload, options)?;
    let map_digest = mapping_digest(options, &payload.manifest)?;
    let subjects = parse_member::<SubjectBody>(
        &payload.members,
        "objects/subjects.jsonl",
        ObjectKind::Subject,
    )?;
    let episodes = parse_member::<EpisodeBody>(
        &payload.members,
        "objects/episodes.jsonl",
        ObjectKind::Episode,
    )?;
    let sources = parse_member::<SourceBody>(
        &payload.members,
        "objects/sources.jsonl",
        ObjectKind::Source,
    )?;
    let evidence = parse_member::<EvidenceBody>(
        &payload.members,
        "objects/evidence.jsonl",
        ObjectKind::Evidence,
    )?;
    let memories = parse_member::<MemoryBody>(
        &payload.members,
        "objects/memories.jsonl",
        ObjectKind::Memory,
    )?;
    let mut objects = Vec::new();
    let mut subject_ids = BTreeMap::new();
    let mut episode_ids = BTreeMap::new();
    let mut source_ids = BTreeMap::new();
    let mut memory_ids = BTreeMap::new();
    let mut counts = BundleCounts {
        discovered: subjects.len()
            + episodes.len()
            + sources.len()
            + evidence.len()
            + memories.len(),
        validated: subjects.len()
            + episodes.len()
            + sources.len()
            + evidence.len()
            + memories.len(),
        ..BundleCounts::default()
    };
    let mut blocking = Vec::new();

    for envelope in subjects {
        let body = envelope.body;
        let (decision, reason, destination_id, destination_digest) = plan_subject(
            store,
            tx,
            &envelope.portable_ref,
            &envelope.digest,
            &body,
            options,
        )?;
        update_plan_counts(&mut counts, &decision, &reason);
        if decision.starts_with("conflict_") || decision == "reject_destination_policy" {
            blocking.push(decision_detail(
                &envelope.portable_ref,
                decision.clone(),
                reason.clone(),
                destination_id.clone(),
            ));
        }
        if let Some(id) = &destination_id {
            subject_ids.insert(ref_key(&envelope.portable_ref), id.clone());
        }
        objects.push(PlanObject {
            kind: ObjectKind::Subject,
            portable_ref: envelope.portable_ref,
            digest: envelope.digest,
            body: serde_json::to_value(body)?,
            decision,
            reason_code: reason,
            destination_id,
            destination_digest,
        });
    }
    for envelope in sources {
        let body = envelope.body;
        let (decision, reason, destination_id, destination_digest) = plan_source(
            store,
            tx,
            &envelope.portable_ref,
            &envelope.digest,
            &body,
            options,
        )?;
        update_plan_counts(&mut counts, &decision, &reason);
        if decision.starts_with("conflict_") || decision == "reject_destination_policy" {
            blocking.push(decision_detail(
                &envelope.portable_ref,
                decision.clone(),
                reason.clone(),
                destination_id.clone(),
            ));
        }
        if let Some(id) = &destination_id {
            source_ids.insert(ref_key(&envelope.portable_ref), id.clone());
        }
        objects.push(PlanObject {
            kind: ObjectKind::Source,
            portable_ref: envelope.portable_ref,
            digest: envelope.digest,
            body: serde_json::to_value(body)?,
            decision,
            reason_code: reason,
            destination_id,
            destination_digest,
        });
    }
    for envelope in episodes {
        let body = envelope.body;
        if !subject_ref_can_resolve(&body.subject_ref, &objects, &subject_ids) {
            return Err(bundle_error(
                ErrorCode::BundleMissingDependency,
                "episode subject dependency is not resolvable",
            ));
        }
        let (decision, reason, destination_id, destination_digest) = plan_episode(
            store,
            tx,
            &envelope.portable_ref,
            &envelope.digest,
            &body,
            options,
            &subject_ids,
        )?;
        update_plan_counts(&mut counts, &decision, &reason);
        if decision.starts_with("conflict_") || decision == "reject_destination_policy" {
            blocking.push(decision_detail(
                &envelope.portable_ref,
                decision.clone(),
                reason.clone(),
                destination_id.clone(),
            ));
        }
        if let Some(id) = &destination_id {
            episode_ids.insert(ref_key(&envelope.portable_ref), id.clone());
        }
        objects.push(PlanObject {
            kind: ObjectKind::Episode,
            portable_ref: envelope.portable_ref,
            digest: envelope.digest,
            body: serde_json::to_value(body)?,
            decision,
            reason_code: reason,
            destination_id,
            destination_digest,
        });
    }
    for envelope in evidence {
        let body = envelope.body;
        let (decision, reason, destination_id, destination_digest) = plan_evidence(
            store,
            tx,
            &envelope.portable_ref,
            &envelope.digest,
            &body,
            &payload.manifest.intent,
            options,
            &subject_ids,
            &source_ids,
            &objects,
        )?;
        update_plan_counts(&mut counts, &decision, &reason);
        if decision.starts_with("conflict_") || decision == "reject_destination_policy" {
            blocking.push(decision_detail(
                &envelope.portable_ref,
                decision.clone(),
                reason.clone(),
                destination_id.clone(),
            ));
        }
        counts.external_reference += body.external_refs.len();
        objects.push(PlanObject {
            kind: ObjectKind::Evidence,
            portable_ref: envelope.portable_ref,
            digest: envelope.digest,
            body: serde_json::to_value(body)?,
            decision,
            reason_code: reason,
            destination_id,
            destination_digest,
        });
    }
    for envelope in memories {
        let body = envelope.body;
        let (decision, reason, destination_id, destination_digest) = if let Some(reason) =
            validate_destination_memory(&body, &payload.manifest.intent, options)?
        {
            ("reject_destination_policy".to_string(), reason, None, None)
        } else {
            plan_memory(
                store,
                tx,
                &envelope.portable_ref,
                &envelope.digest,
                &body,
                options,
                &payload.manifest.intent,
                &subject_ids,
                &episode_ids,
                &source_ids,
            )?
        };
        update_plan_counts(&mut counts, &decision, &reason);
        if decision.starts_with("conflict_") || decision == "reject_destination_policy" {
            blocking.push(decision_detail(
                &envelope.portable_ref,
                decision.clone(),
                reason.clone(),
                destination_id.clone(),
            ));
        }
        if let Some(id) = &destination_id {
            memory_ids.insert(ref_key(&envelope.portable_ref), id.clone());
        }
        counts.external_reference += body.external_refs.len();
        objects.push(PlanObject {
            kind: ObjectKind::Memory,
            portable_ref: envelope.portable_ref,
            digest: envelope.digest,
            body: serde_json::to_value(body)?,
            decision,
            reason_code: reason,
            destination_id,
            destination_digest,
        });
    }
    objects.sort_by(|a, b| a.portable_ref.cmp(&b.portable_ref));
    let plan_id = compute_plan_id(
        payload,
        options,
        destination_instance_id,
        &map_digest,
        &objects,
    )?;
    let mut decisions = Vec::new();
    for object in &objects {
        if decisions.len() < MAX_DETAIL_ITEMS {
            decisions.push(decision_detail(
                &object.portable_ref,
                object.decision.clone(),
                object.reason_code.clone(),
                object.destination_id.clone(),
            ));
        }
    }
    let decision_details_truncated = objects.len().saturating_sub(decisions.len());
    let safe_to_apply = blocking.is_empty();
    counts.conflicts = blocking.len();
    let destination_scope = ScopeSummary {
        profile: options.profile.clone(),
        workspace: options.workspace.clone(),
        repo_id: destination_repo_id(&payload.manifest.intent, options),
    };
    let mut report = BundleReport {
        mode: "import_preview".to_string(),
        format_version: payload.manifest.format_version,
        bundle_id: payload.manifest.bundle_id.clone(),
        manifest_digest: ids::sha256_hex(&payload.manifest_bytes),
        integrity_status: "verified".to_string(),
        signature_status: payload.signature_status.clone(),
        source_instance_id: payload.manifest.producer.instance_id.clone(),
        destination_instance_id: Some(destination_instance_id.to_string()),
        source_scope: ScopeSummary {
            profile: payload.manifest.intent.source_profile.clone(),
            workspace: payload.manifest.intent.source_workspace.clone(),
            repo_id: payload.manifest.intent.source_repo_id.clone(),
        },
        destination_scope: Some(destination_scope),
        policy_fingerprint: RULESET_FINGERPRINT.to_string(),
        mapping_digest: Some(map_digest.clone()),
        plan_id: Some(plan_id),
        safe_to_apply: Some(safe_to_apply),
        counts,
        omissions: BTreeMap::new(),
        decisions,
        decision_details_truncated,
        conflicts: blocking,
        warnings: Vec::new(),
        details_truncated: 0,
        receipt: None,
        recall_not_authority: true,
    };
    if report.signature_status == "absent" {
        report.warnings.push("authenticity_unverified".to_string());
    }
    Ok(ImportPlan {
        report,
        objects,
        subject_ids,
        episode_ids,
        source_ids,
        memory_ids,
        mapping_digest: map_digest,
    })
}

fn validate_import_mapping(payload: &BundlePayload, options: &BundleImportOptions) -> Result<()> {
    let intent = &payload.manifest.intent;
    if options.profile != intent.target_profile {
        return Err(bundle_error(
            ErrorCode::BundlePolicyDenied,
            "destination profile does not match bundle target profile",
        ));
    }
    let expected_workspace = intent
        .target_workspace
        .as_deref()
        .unwrap_or(&intent.source_workspace);
    if options.workspace != expected_workspace {
        return Err(bundle_error(
            ErrorCode::BundlePolicyDenied,
            "destination workspace does not match the explicit bundle mapping",
        ));
    }
    let expected_repo = intent
        .target_repo_id
        .as_deref()
        .or(intent.source_repo_id.as_deref());
    if options
        .repo_id
        .as_deref()
        .is_some_and(|repo| Some(repo) != expected_repo)
    {
        return Err(bundle_error(
            ErrorCode::BundlePolicyDenied,
            "destination repository does not match the explicit bundle mapping",
        ));
    }
    Ok(())
}

fn subject_ref_can_resolve(
    reference: &PortableRef,
    objects: &[PlanObject],
    subject_ids: &BTreeMap<String, String>,
) -> bool {
    subject_ids.contains_key(&ref_key(reference))
        || objects
            .iter()
            .any(|object| object.kind == ObjectKind::Subject && object.portable_ref == *reference)
}

fn plan_subject(
    store: &Store,
    tx: Option<&Transaction<'_>>,
    reference: &PortableRef,
    digest: &str,
    body: &SubjectBody,
    options: &BundleImportOptions,
) -> Result<(String, String, Option<String>, Option<String>)> {
    let kind = SubjectKind::parse(&body.kind).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "unsupported subject kind",
        )
    })?;
    let existing_origin = lookup_origin(store, tx, reference)?;
    if let Some(origin) = existing_origin {
        if origin.imported_object_digest != digest {
            return Ok((
                "conflict_origin_revision".to_string(),
                "origin digest differs from the existing canonical mapping".to_string(),
                Some(origin.local_object_id),
                Some(origin.imported_object_digest),
            ));
        }
        return Ok((
            "reuse_identity_exact".to_string(),
            "same portable origin and digest already mapped".to_string(),
            Some(origin.local_object_id),
            Some(origin.imported_object_digest),
        ));
    }
    let existing = lookup_subject(store, tx, options, &body.subject_key)?;
    if let Some((id, existing_kind, display_name, metadata)) = existing {
        if existing_kind != kind.as_str() {
            return Ok((
                "conflict_subject_key".to_string(),
                "subject key is already used by an incompatible subject kind".to_string(),
                Some(id),
                None,
            ));
        }
        let reason =
            if display_name != body.display_name || metadata != metadata_value(&body.metadata) {
                "subject metadata differs; destination values win".to_string()
            } else {
                "same destination subject key and kind".to_string()
            };
        return Ok((
            "reuse_subject_destination_wins".to_string(),
            if reason.starts_with("subject metadata") {
                "subject_metadata_drift".to_string()
            } else {
                reason
            },
            Some(id),
            None,
        ));
    }
    Ok((
        "create".to_string(),
        "no matching destination subject".to_string(),
        None,
        None,
    ))
}

fn plan_source(
    store: &Store,
    tx: Option<&Transaction<'_>>,
    reference: &PortableRef,
    digest: &str,
    body: &SourceBody,
    options: &BundleImportOptions,
) -> Result<(String, String, Option<String>, Option<String>)> {
    let existing_origin = lookup_origin(store, tx, reference)?;
    if let Some(origin) = existing_origin {
        if origin.imported_object_digest != digest {
            return Ok((
                "conflict_origin_revision".to_string(),
                "source origin digest differs from the existing canonical mapping".to_string(),
                Some(origin.local_object_id),
                Some(origin.imported_object_digest),
            ));
        }
        return Ok((
            "reuse_identity_exact".to_string(),
            "same portable origin and digest already mapped".to_string(),
            Some(origin.local_object_id),
            Some(origin.imported_object_digest),
        ));
    }
    let existing = lookup_source(
        store,
        tx,
        options,
        body.source_path.as_deref(),
        &body.source_hash,
    )?;
    if let Some(id) = existing {
        return Ok((
            "reuse_content_exact".to_string(),
            "same destination source hash and path already exists".to_string(),
            Some(id),
            Some(digest.to_string()),
        ));
    }
    Ok((
        "create".to_string(),
        "no matching destination source".to_string(),
        None,
        None,
    ))
}

fn plan_episode(
    store: &Store,
    tx: Option<&Transaction<'_>>,
    reference: &PortableRef,
    digest: &str,
    _body: &EpisodeBody,
    _options: &BundleImportOptions,
    _subject_ids: &BTreeMap<String, String>,
) -> Result<(String, String, Option<String>, Option<String>)> {
    if let Some(origin) = lookup_origin(store, tx, reference)? {
        if origin.imported_object_digest != digest {
            return Ok((
                "conflict_origin_revision".to_string(),
                "episode origin digest differs from the existing canonical mapping".to_string(),
                Some(origin.local_object_id),
                Some(origin.imported_object_digest),
            ));
        }
        return Ok((
            "reuse_identity_exact".to_string(),
            "same portable origin and digest already mapped".to_string(),
            Some(origin.local_object_id),
            Some(origin.imported_object_digest),
        ));
    }
    Ok((
        "create".to_string(),
        "no matching destination episode origin".to_string(),
        None,
        None,
    ))
}

fn plan_evidence(
    store: &Store,
    tx: Option<&Transaction<'_>>,
    reference: &PortableRef,
    digest: &str,
    body: &EvidenceBody,
    intent: &BundleIntent,
    options: &BundleImportOptions,
    subject_ids: &BTreeMap<String, String>,
    source_ids: &BTreeMap<String, String>,
    objects: &[PlanObject],
) -> Result<(String, String, Option<String>, Option<String>)> {
    if body.profile != intent.source_profile || body.workspace != intent.source_workspace {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "evidence scope does not match the bundle source intent",
        ));
    }
    if let Some(reason) = policy_rejection_for_boundary(intent, body.safe_summary.as_str()) {
        return Ok(("reject_destination_policy".to_string(), reason, None, None));
    }
    if let Some(source_ref) = &body.source_ref {
        if !source_ids.contains_key(&ref_key(source_ref))
            && !objects.iter().any(|object| {
                object.kind == ObjectKind::Source && object.portable_ref == *source_ref
            })
            && !body
                .external_refs
                .iter()
                .any(|external| external == source_ref)
        {
            return Err(bundle_error(
                ErrorCode::BundleMissingDependency,
                "evidence source is not resolvable during import planning",
            ));
        }
    }
    if let Some(subject_ref) = &body.subject_ref {
        if !subject_ids.contains_key(&ref_key(subject_ref))
            && !objects.iter().any(|object| {
                object.kind == ObjectKind::Subject && object.portable_ref == *subject_ref
            })
            && !body
                .external_refs
                .iter()
                .any(|external| external == subject_ref)
        {
            return Err(bundle_error(
                ErrorCode::BundleMissingDependency,
                "evidence subject is not resolvable during import planning",
            ));
        }
    }
    if let Some(origin) = lookup_origin(store, tx, reference)? {
        if origin.imported_object_digest != digest {
            return Ok((
                "conflict_origin_revision".to_string(),
                "evidence origin digest differs from the existing canonical mapping".to_string(),
                Some(origin.local_object_id),
                Some(origin.imported_object_digest),
            ));
        }
        return Ok((
            "reuse_identity_exact".to_string(),
            "same portable origin and digest already mapped".to_string(),
            Some(origin.local_object_id),
            Some(origin.imported_object_digest),
        ));
    }
    let _ = options;
    Ok((
        "create".to_string(),
        "no matching destination evidence".to_string(),
        None,
        None,
    ))
}

fn plan_memory(
    store: &Store,
    tx: Option<&Transaction<'_>>,
    reference: &PortableRef,
    digest: &str,
    body: &MemoryBody,
    options: &BundleImportOptions,
    intent: &BundleIntent,
    _subject_ids: &BTreeMap<String, String>,
    _episode_ids: &BTreeMap<String, String>,
    _source_ids: &BTreeMap<String, String>,
) -> Result<(String, String, Option<String>, Option<String>)> {
    if let Some(origin) = lookup_origin(store, tx, reference)? {
        if origin.imported_object_digest != digest {
            return Ok((
                "conflict_origin_revision".to_string(),
                "memory origin digest differs from the existing canonical mapping".to_string(),
                Some(origin.local_object_id),
                Some(origin.imported_object_digest),
            ));
        }
        return Ok((
            "reuse_identity_exact".to_string(),
            "same portable origin and digest already mapped".to_string(),
            Some(origin.local_object_id),
            Some(origin.imported_object_digest),
        ));
    }
    let repo_id = mapped_repo_id(body.repo_id.as_deref(), intent, options);
    let content_hash = ids::content_hash(
        &options.profile,
        &options.workspace,
        repo_id.as_deref(),
        &body.record_type,
        &body.scope,
        &body.content,
    );
    if let Some(existing) = lookup_memory_by_hash(store, tx, &content_hash)? {
        if existing.record_type != body.record_type
            || existing.scope != body.scope
            || existing.sensitivity != body.sensitivity
            || existing.temporal_state != body.temporal_state
        {
            return Ok((
                "conflict_content_semantics".to_string(),
                "destination content hash matches incompatible memory semantics".to_string(),
                Some(existing.id),
                Some(existing.content_hash),
            ));
        }
        return Ok((
            "reuse_content_exact".to_string(),
            "same mapped destination content hash already exists".to_string(),
            Some(existing.id),
            Some(existing.content_hash),
        ));
    }
    Ok((
        "create".to_string(),
        "no matching destination memory".to_string(),
        None,
        Some(content_hash),
    ))
}

fn validate_destination_memory(
    body: &MemoryBody,
    intent: &BundleIntent,
    options: &BundleImportOptions,
) -> Result<Option<String>> {
    if body.profile != intent.source_profile || body.workspace != intent.source_workspace {
        return Err(bundle_error(
            ErrorCode::BundleIntegrityFailed,
            "memory scope does not match the bundle source intent",
        ));
    }
    let portability = Portability::parse(&body.portability).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "unsupported portability",
        )
    })?;
    if portability == Portability::ProfileOnly && options.profile != intent.source_profile {
        let generic = RecordType::parse(&body.record_type)
            .zip(Sensitivity::parse(&body.sensitivity))
            .is_some_and(|(record_type, sensitivity)| {
                policy::is_generic_preference(record_type, sensitivity)
            });
        if !generic {
            return Ok(Some("profile_only_cross_profile".to_string()));
        }
    }
    if portability == Portability::WorkspaceOnly && options.workspace != intent.source_workspace {
        return Ok(Some("workspace_only_cross_workspace".to_string()));
    }
    if body.scope == "repo" {
        let source_repo = intent.source_repo_id.as_deref();
        if body.repo_id.as_deref() != source_repo {
            return Err(bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "repo-scoped memory does not match source repository intent",
            ));
        }
        if mapped_repo_id(body.repo_id.as_deref(), intent, options).is_none() {
            return Ok(Some("repo_mapping_required".to_string()));
        }
    }
    if let Some(reason) = policy_rejection_for_boundary(intent, &body.content) {
        let generic = RecordType::parse(&body.record_type)
            .zip(Sensitivity::parse(&body.sensitivity))
            .is_some_and(|(record_type, sensitivity)| {
                policy::is_generic_preference(record_type, sensitivity)
            });
        if reason != "generic_preferences_only" || !generic {
            return Ok(Some(reason));
        }
    }
    match policy::screen_content(&body.content, MAX_RECORD_CHARS) {
        PolicyDecision::Accept(_) => Ok(None),
        PolicyDecision::Reject { code, .. } => Ok(Some(code)),
    }
}

fn policy_rejection_for_boundary(intent: &BundleIntent, _content: &str) -> Option<String> {
    let source = Profile::parse(&intent.source_profile)?;
    let target = Profile::parse(&intent.target_profile)?;
    match policy::export_boundary(source, target) {
        BoundaryDecision::Allow => None,
        BoundaryDecision::AllowGenericPreferencesOnly => {
            Some("generic_preferences_only".to_string())
        }
        BoundaryDecision::Deny { .. } => Some("profile_boundary_denied".to_string()),
    }
}

fn destination_repo_id(intent: &BundleIntent, options: &BundleImportOptions) -> Option<String> {
    options
        .repo_id
        .clone()
        .or_else(|| intent.target_repo_id.clone())
        .or_else(|| intent.source_repo_id.clone())
}

fn mapped_repo_id(
    source_repo: Option<&str>,
    intent: &BundleIntent,
    options: &BundleImportOptions,
) -> Option<String> {
    source_repo
        .map(|_| destination_repo_id(intent, options))
        .flatten()
}

fn update_plan_counts(counts: &mut BundleCounts, decision: &str, reason: &str) {
    match decision {
        "create" => counts.create += 1,
        "reuse_identity_exact" => counts.reuse_identity_exact += 1,
        "reuse_content_exact" | "reuse_subject_destination_wins" => counts.reuse_content_exact += 1,
        "external_reference" => counts.external_reference += 1,
        value if value.starts_with("conflict_") || value == "reject_destination_policy" => {
            counts.rejected += 1;
            let _ = reason;
        }
        _ => {}
    }
}

fn decision_detail(
    reference: &PortableRef,
    decision: String,
    reason_code: String,
    destination_id: Option<String>,
) -> DecisionDetail {
    DecisionDetail {
        portable_ref: reference.clone(),
        decision,
        reason_code,
        destination_id,
    }
}

fn compute_plan_id(
    payload: &BundlePayload,
    options: &BundleImportOptions,
    destination_instance_id: &str,
    mapping_digest: &str,
    objects: &[PlanObject],
) -> Result<String> {
    let mut rows = Vec::new();
    for object in objects {
        rows.push(serde_json::json!({
            "portable_ref": object.portable_ref,
            "object_digest": object.digest,
            "destination_content_hash": object.destination_digest,
            "decision": object.decision,
            "reason_code": object.reason_code,
            "destination_id": object.destination_id,
        }));
    }
    let value = serde_json::json!({
        "bundle_id": payload.manifest.bundle_id,
        "manifest_digest": ids::sha256_hex(&payload.manifest_bytes),
        "destination_instance_id": destination_instance_id,
        "destination_profile": options.profile,
        "destination_workspace": options.workspace,
        "destination_repo_id": options.repo_id,
        "storage_schema_version": store::STORAGE_SCHEMA_VERSION,
        "policy_fingerprint": RULESET_FINGERPRINT,
        "mapping_digest": mapping_digest,
        "objects": rows,
    });
    Ok(ids::sha256_hex(&canonical_json(&value)?))
}

fn lookup_origin(
    store: &Store,
    tx: Option<&Transaction<'_>>,
    reference: &PortableRef,
) -> Result<Option<store::PortableObjectOrigin>> {
    if let Some(tx) = tx {
        tx.query_row(
            "SELECT origin_instance_id, object_kind, origin_object_id, local_object_id,
                    imported_object_digest, canonical, first_bundle_id, imported_at
             FROM portable_object_origins
             WHERE origin_instance_id = ?1 AND object_kind = ?2 AND origin_object_id = ?3",
            params![reference.origin_instance_id, reference.kind, reference.id],
            |row| {
                Ok(store::PortableObjectOrigin {
                    origin_instance_id: row.get(0)?,
                    object_kind: row.get(1)?,
                    origin_object_id: row.get(2)?,
                    local_object_id: row.get(3)?,
                    imported_object_digest: row.get(4)?,
                    canonical: row.get::<_, i64>(5)? != 0,
                    first_bundle_id: row.get(6)?,
                    imported_at: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(Error::from)
    } else {
        store.portable_origin(
            &reference.origin_instance_id,
            &reference.kind,
            &reference.id,
        )
    }
}

fn lookup_subject(
    store: &Store,
    tx: Option<&Transaction<'_>>,
    options: &BundleImportOptions,
    subject_key: &str,
) -> Result<Option<(String, String, String, Value)>> {
    if let Some(tx) = tx {
        return tx
            .query_row(
                "SELECT id, kind, display_name, metadata
                 FROM subjects
                 WHERE profile_id = ?1 AND workspace_id = ?2 AND subject_key = ?3",
                params![options.profile, options.workspace, subject_key],
                |row| {
                    let metadata: String = row.get(3)?;
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        serde_json::from_str(&metadata).unwrap_or(Value::Null),
                    ))
                },
            )
            .optional()
            .map_err(Error::from);
    }
    store
        .find_subject_by_key(&options.profile, &options.workspace, subject_key)
        .map(|subject| {
            subject.map(|subject| {
                (
                    subject.id,
                    subject.kind.as_str().to_string(),
                    subject.display_name,
                    subject.metadata,
                )
            })
        })
}

fn lookup_source(
    store: &Store,
    tx: Option<&Transaction<'_>>,
    options: &BundleImportOptions,
    source_path: Option<&str>,
    source_hash: &str,
) -> Result<Option<String>> {
    if let Some(tx) = tx {
        return tx
            .query_row(
                "SELECT id FROM memory_sources
                 WHERE profile_id = ?1 AND workspace_id = ?2 AND source_hash = ?3
                   AND (source_path IS ?4 OR source_path = ?4)",
                params![options.profile, options.workspace, source_hash, source_path],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from);
    }
    store
        .find_source(
            &options.profile,
            &options.workspace,
            source_path,
            source_hash,
        )
        .map(|source| source.map(|source| source.id))
}

#[derive(Debug, Clone)]
struct ExistingMemory {
    id: String,
    record_type: String,
    scope: String,
    sensitivity: String,
    temporal_state: String,
    content_hash: String,
}

fn lookup_memory_by_hash(
    store: &Store,
    tx: Option<&Transaction<'_>>,
    content_hash: &str,
) -> Result<Option<ExistingMemory>> {
    if let Some(tx) = tx {
        return tx
            .query_row(
                "SELECT id, type, scope, sensitivity, temporal_state, content_hash
                 FROM memory_records WHERE content_hash = ?1",
                params![content_hash],
                |row| {
                    Ok(ExistingMemory {
                        id: row.get(0)?,
                        record_type: row.get(1)?,
                        scope: row.get(2)?,
                        sensitivity: row.get(3)?,
                        temporal_state: row.get(4)?,
                        content_hash: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from);
    }
    store.find_by_content_hash(content_hash).map(|record| {
        record.map(|record| ExistingMemory {
            id: record.id,
            record_type: record.record_type.as_str().to_string(),
            scope: record.scope.as_str().to_string(),
            sensitivity: record.sensitivity.as_str().to_string(),
            temporal_state: record.temporal_state.as_str().to_string(),
            content_hash: record.content_hash,
        })
    })
}

fn metadata_value(metadata: &SafeMetadata) -> Value {
    let mut object = Map::new();
    if let Some(value) = &metadata.origin {
        object.insert("origin".to_string(), value.clone());
    }
    if let Some(value) = &metadata.state {
        object.insert("state".to_string(), value.clone());
    }
    if let Some(value) = &metadata.candidate_state {
        object.insert("candidate_state".to_string(), value.clone());
    }
    if let Some(value) = &metadata.historical_reason {
        object.insert("historical_reason".to_string(), value.clone());
    }
    if let Some(value) = &metadata.temporal_state {
        object.insert("temporal_state".to_string(), value.clone());
    }
    if let Some(value) = &metadata.redaction_state {
        object.insert("redaction_state".to_string(), value.clone());
    }
    if let Some(value) = &metadata.raw_artifact_stored {
        object.insert("raw_artifact_stored".to_string(), value.clone());
    }
    Value::Object(object)
}

fn lookup_receipt(
    tx: &Transaction<'_>,
    bundle_id: &str,
    source_instance_id: &str,
    destination_instance_id: &str,
    mapping_digest: &str,
) -> Result<Option<StoredReceipt>> {
    tx.query_row(
        "SELECT receipt_id, status, applied_at, destination_instance_id,
                created_count, reused_identity_count, reused_content_count,
                external_reference_count
         FROM bundle_import_receipts
         WHERE bundle_id = ?1 AND source_instance_id = ?2
           AND destination_instance_id = ?3 AND mapping_digest = ?4",
        params![
            bundle_id,
            source_instance_id,
            destination_instance_id,
            mapping_digest
        ],
        |row| {
            let created_count = row.get::<_, i64>(4)? as usize;
            let reused_identity_count = row.get::<_, i64>(5)? as usize;
            let reused_content_count = row.get::<_, i64>(6)? as usize;
            let external_reference_count = row.get::<_, i64>(7)? as usize;
            Ok(StoredReceipt {
                receipt_id: row.get(0)?,
                status: row.get(1)?,
                applied_at: row.get(2)?,
                destination_instance_id: row.get(3)?,
                counts: BundleCounts {
                    discovered: created_count
                        + reused_identity_count
                        + reused_content_count
                        + external_reference_count,
                    validated: created_count
                        + reused_identity_count
                        + reused_content_count
                        + external_reference_count,
                    create: created_count,
                    reuse_identity_exact: reused_identity_count,
                    reuse_content_exact: reused_content_count,
                    external_reference: external_reference_count,
                    ..BundleCounts::default()
                },
            })
        },
    )
    .optional()
    .map_err(Error::from)
}

fn receipt_report(
    payload: &BundlePayload,
    options: &BundleImportOptions,
    receipt: StoredReceipt,
) -> BundleReport {
    let mut report = export_report(payload, BundleMode::ImportApply);
    report.mode = "import_apply".to_string();
    report.destination_instance_id = Some(receipt.destination_instance_id.clone());
    report.destination_scope = Some(ScopeSummary {
        profile: options.profile.clone(),
        workspace: options.workspace.clone(),
        repo_id: destination_repo_id(&payload.manifest.intent, options),
    });
    report.mapping_digest = mapping_digest(options, &payload.manifest).ok();
    report.safe_to_apply = Some(true);
    report.receipt = Some(ReceiptSummary {
        receipt_id: receipt.receipt_id,
        status: receipt.status,
        applied_at: receipt.applied_at,
        counts: receipt.counts.clone(),
    });
    report.counts = receipt.counts;
    report
}

fn insert_receipt(
    tx: &Transaction<'_>,
    payload: &BundlePayload,
    options: &BundleImportOptions,
    plan: &ImportPlan,
) -> Result<StoredReceipt> {
    let receipt_id = ids::new_id("receipt");
    let applied_at = ids::now_rfc3339();
    let destination_instance_id: String = tx.query_row(
        "SELECT instance_id FROM instance_metadata WHERE singleton_key = 'default'",
        [],
        |row| row.get(0),
    )?;
    let counts = &plan.report.counts;
    tx.execute(
        "INSERT INTO bundle_import_receipts(
            receipt_id, bundle_id, manifest_digest, source_instance_id,
            destination_instance_id, mapping_digest, plan_id, status,
            created_count, reused_identity_count, reused_content_count,
            external_reference_count, applied_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,'applied',?8,?9,?10,?11,?12)",
        params![
            receipt_id,
            payload.manifest.bundle_id,
            ids::sha256_hex(&payload.manifest_bytes),
            payload.manifest.producer.instance_id,
            destination_instance_id,
            plan.mapping_digest,
            plan.report.plan_id.as_deref().unwrap_or_default(),
            counts.create as i64,
            counts.reuse_identity_exact as i64,
            counts.reuse_content_exact as i64,
            counts.external_reference as i64,
            applied_at,
        ],
    )?;
    let _ = options;
    Ok(StoredReceipt {
        receipt_id,
        status: "applied".to_string(),
        applied_at,
        destination_instance_id,
        counts: counts.clone(),
    })
}

fn insert_origin(
    tx: &Transaction<'_>,
    reference: &PortableRef,
    local_object_id: &str,
    digest: &str,
    bundle_id: &str,
    canonical: bool,
) -> Result<()> {
    tx.execute(
        "INSERT INTO portable_object_origins(
            origin_instance_id, object_kind, origin_object_id, local_object_id,
            imported_object_digest, canonical, first_bundle_id, imported_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(origin_instance_id, object_kind, origin_object_id) DO NOTHING",
        params![
            reference.origin_instance_id,
            reference.kind,
            reference.id,
            local_object_id,
            digest,
            canonical as i64,
            bundle_id,
            ids::now_rfc3339(),
        ],
    )?;
    Ok(())
}

fn apply_plan(
    tx: &Transaction<'_>,
    _store: &Store,
    payload: &BundlePayload,
    options: &BundleImportOptions,
    plan: &ImportPlan,
) -> Result<()> {
    Store::ensure_workspace_in_transaction(tx, &options.profile, &options.workspace)?;
    let mut subject_ids = BTreeMap::new();
    let mut source_ids = BTreeMap::new();
    let mut episode_ids = BTreeMap::new();
    let mut memory_ids = BTreeMap::new();

    let subjects = parse_member::<SubjectBody>(
        &payload.members,
        "objects/subjects.jsonl",
        ObjectKind::Subject,
    )?;
    for envelope in subjects {
        let object = plan_object(plan, &envelope.portable_ref)?;
        let local_id = object
            .destination_id
            .clone()
            .unwrap_or_else(|| ids::new_id("sub"));
        if object.decision == "create" {
            let body = envelope.body;
            let subject = Subject {
                id: local_id.clone(),
                profile_id: options.profile.clone(),
                workspace_id: options.workspace.clone(),
                subject_key: body.subject_key,
                kind: SubjectKind::parse(&body.kind).ok_or_else(|| {
                    bundle_error(
                        ErrorCode::BundleSchemaUnsupported,
                        "unsupported subject kind",
                    )
                })?,
                display_name: body.display_name,
                created_at: body.created_at,
                updated_at: body.updated_at,
                metadata: metadata_value(&body.metadata),
            };
            Store::insert_subject_in_transaction(tx, &subject)?;
        }
        subject_ids.insert(ref_key(&envelope.portable_ref), local_id.clone());
        insert_origin(
            tx,
            &envelope.portable_ref,
            &local_id,
            &envelope.digest,
            &payload.manifest.bundle_id,
            true,
        )?;
    }

    let sources = parse_member::<SourceBody>(
        &payload.members,
        "objects/sources.jsonl",
        ObjectKind::Source,
    )?;
    for envelope in sources {
        let object = plan_object(plan, &envelope.portable_ref)?;
        let local_id = object
            .destination_id
            .clone()
            .unwrap_or_else(|| ids::new_id("src"));
        if object.decision == "create" {
            let body = envelope.body;
            let source = MemorySource {
                id: local_id.clone(),
                profile_id: options.profile.clone(),
                workspace_id: options.workspace.clone(),
                kind: body.kind,
                source_path: body.source_path,
                source_hash: body.source_hash,
                created_at: body.created_at,
                ingested_at: body.ingested_at,
                metadata: metadata_value(&body.metadata),
            };
            Store::insert_source_in_transaction(tx, &source)?;
        }
        source_ids.insert(ref_key(&envelope.portable_ref), local_id.clone());
        insert_origin(
            tx,
            &envelope.portable_ref,
            &local_id,
            &envelope.digest,
            &payload.manifest.bundle_id,
            true,
        )?;
    }

    let episodes = parse_member::<EpisodeBody>(
        &payload.members,
        "objects/episodes.jsonl",
        ObjectKind::Episode,
    )?;
    for envelope in episodes {
        let object = plan_object(plan, &envelope.portable_ref)?;
        let local_id = object
            .destination_id
            .clone()
            .unwrap_or_else(|| ids::new_id("ep"));
        if object.decision == "create" {
            let body = envelope.body;
            let subject_id = subject_ids
                .get(&ref_key(&body.subject_ref))
                .ok_or_else(|| {
                    bundle_error(
                        ErrorCode::BundleMissingDependency,
                        "episode subject is not available during apply",
                    )
                })?
                .clone();
            let episode = Episode {
                id: local_id.clone(),
                profile_id: options.profile.clone(),
                workspace_id: options.workspace.clone(),
                subject_id,
                source_kind: body.source_kind,
                source_ref: body.source_ref,
                started_at: body.started_at,
                ended_at: body.ended_at,
                status: body.status,
                summary: body.summary,
                trust_level: body.trust_level,
                source_metadata: metadata_value(&body.source_metadata),
                created_at: body.created_at,
                updated_at: body.updated_at,
                metadata: metadata_value(&body.metadata),
            };
            Store::insert_episode_in_transaction(tx, &episode)?;
        }
        episode_ids.insert(ref_key(&envelope.portable_ref), local_id.clone());
        insert_origin(
            tx,
            &envelope.portable_ref,
            &local_id,
            &envelope.digest,
            &payload.manifest.bundle_id,
            true,
        )?;
    }

    let evidence = parse_member::<EvidenceBody>(
        &payload.members,
        "objects/evidence.jsonl",
        ObjectKind::Evidence,
    )?;
    for envelope in evidence {
        let object = plan_object(plan, &envelope.portable_ref)?;
        let mut local_id = object
            .destination_id
            .clone()
            .unwrap_or_else(|| ids::new_id("led"));
        if object.decision == "create" {
            let body = envelope.body;
            let source_id = body
                .source_ref
                .as_ref()
                .and_then(|reference| source_ids.get(&ref_key(reference)).cloned());
            let record = store::EvidenceLedgerRecord {
                id: local_id.clone(),
                profile_id: options.profile.clone(),
                workspace_id: options.workspace.clone(),
                repo_id: mapped_repo_id(body.repo_id.as_deref(), &payload.manifest.intent, options),
                subject_key: body.subject_key,
                source_kind: body.source_kind,
                source_id: source_id.clone(),
                source_path: body.source_path,
                source_hash: body.source_hash,
                safe_summary: body.safe_summary,
                policy_state: body.policy_state,
                created_at: body.created_at,
                trust_state: "trusted".to_string(),
                trust_score: 1.0,
                metadata: metadata_value(&body.metadata),
            };
            local_id = Store::insert_evidence_ledger_record_in_transaction(
                tx,
                &record,
                source_id.as_deref(),
                &record.metadata,
            )?;
        }
        insert_origin(
            tx,
            &envelope.portable_ref,
            &local_id,
            &envelope.digest,
            &payload.manifest.bundle_id,
            true,
        )?;
    }

    let memories = parse_member::<MemoryBody>(
        &payload.members,
        "objects/memories.jsonl",
        ObjectKind::Memory,
    )?;
    for envelope in &memories {
        let object = plan_object(plan, &envelope.portable_ref)?;
        let local_id = object
            .destination_id
            .clone()
            .unwrap_or_else(|| ids::new_id("mem"));
        memory_ids.insert(ref_key(&envelope.portable_ref), local_id);
    }
    for envelope in memories {
        let object = plan_object(plan, &envelope.portable_ref)?;
        let local_id = memory_ids
            .get(&ref_key(&envelope.portable_ref))
            .cloned()
            .ok_or_else(|| {
                bundle_error(ErrorCode::BundleIntegrityFailed, "memory plan id missing")
            })?;
        if object.decision == "create" {
            let body = envelope.body;
            let record = memory_record_from_body(
                &body,
                &local_id,
                options,
                &payload.manifest.intent,
                &subject_ids,
                &episode_ids,
                &source_ids,
                &memory_ids,
            )?;
            Store::insert_record_in_transaction(tx, &record)?;
        }
        insert_origin(
            tx,
            &envelope.portable_ref,
            &local_id,
            &envelope.digest,
            &payload.manifest.bundle_id,
            true,
        )?;
    }
    Ok(())
}

fn plan_object<'a>(plan: &'a ImportPlan, reference: &PortableRef) -> Result<&'a PlanObject> {
    plan.objects
        .iter()
        .find(|object| object.portable_ref == *reference)
        .ok_or_else(|| {
            bundle_error(
                ErrorCode::BundleIntegrityFailed,
                "object missing from import plan",
            )
        })
}

fn mapped_local_id(
    reference: Option<&PortableRef>,
    subjects: &BTreeMap<String, String>,
    episodes: &BTreeMap<String, String>,
    sources: &BTreeMap<String, String>,
    memories: &BTreeMap<String, String>,
) -> Option<String> {
    let reference = reference?;
    match reference.kind.as_str() {
        "subject" => subjects.get(&ref_key(reference)),
        "episode" => episodes.get(&ref_key(reference)),
        "source" => sources.get(&ref_key(reference)),
        "memory" => memories.get(&ref_key(reference)),
        _ => None,
    }
    .cloned()
}

fn memory_record_from_body(
    body: &MemoryBody,
    id: &str,
    options: &BundleImportOptions,
    intent: &BundleIntent,
    subjects: &BTreeMap<String, String>,
    episodes: &BTreeMap<String, String>,
    sources: &BTreeMap<String, String>,
    memories: &BTreeMap<String, String>,
) -> Result<MemoryRecord> {
    Profile::parse(&body.profile).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "unsupported memory profile",
        )
    })?;
    let scope = Scope::parse(&body.scope).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "unsupported memory scope",
        )
    })?;
    let record_type = RecordType::parse(&body.record_type).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "unsupported memory type",
        )
    })?;
    let sensitivity = Sensitivity::parse(&body.sensitivity).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "unsupported sensitivity",
        )
    })?;
    let portability = Portability::parse(&body.portability).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "unsupported portability",
        )
    })?;
    let temporal_state = TemporalState::parse(&body.temporal_state).ok_or_else(|| {
        bundle_error(
            ErrorCode::BundleSchemaUnsupported,
            "unsupported temporal state",
        )
    })?;
    let repo_id = mapped_repo_id(body.repo_id.as_deref(), intent, options);
    let source_ids = body
        .source_refs
        .iter()
        .filter_map(|reference| sources.get(&ref_key(reference)).cloned())
        .collect::<Vec<_>>();
    let supersedes = body
        .supersedes
        .iter()
        .filter_map(|reference| memories.get(&ref_key(reference)).cloned())
        .collect::<Vec<_>>();
    let superseded_by = mapped_local_id(
        body.superseded_by.as_ref(),
        subjects,
        episodes,
        sources,
        memories,
    );
    let mut metadata = metadata_value(&body.metadata);
    if let Value::Object(object) = &mut metadata {
        if !body.external_refs.is_empty() {
            object.insert(
                "portable_external_refs".to_string(),
                serde_json::to_value(&body.external_refs)?,
            );
        }
    }
    Ok(MemoryRecord {
        id: id.to_string(),
        profile_id: options.profile.clone(),
        workspace_id: options.workspace.clone(),
        repo_id,
        subject_id: mapped_local_id(
            body.subject_ref.as_ref(),
            subjects,
            episodes,
            sources,
            memories,
        ),
        episode_id: mapped_local_id(
            body.episode_ref.as_ref(),
            subjects,
            episodes,
            sources,
            memories,
        ),
        scope,
        record_type,
        content: body.content.clone(),
        related_files: body.related_files.clone(),
        tags: body.tags.clone(),
        sensitivity,
        portability,
        confidence: body.confidence.clamp(0.0, 1.0),
        source_ids,
        content_hash: ids::content_hash(
            &options.profile,
            &options.workspace,
            mapped_repo_id(body.repo_id.as_deref(), intent, options).as_deref(),
            &body.record_type,
            &body.scope,
            &body.content,
        ),
        supersedes,
        created_at: body.created_at.clone(),
        updated_at: body.updated_at.clone(),
        last_used_at: None,
        archived: body.archived,
        trust_state: body.trust_state.clone(),
        trust_score: body.trust_score.clamp(0.0, 1.0),
        quarantine_reason: None,
        quarantined_at: None,
        promoted_at: None,
        valid_from: body.valid_from.clone(),
        valid_until: body.valid_until.clone(),
        observed_at: body.observed_at.clone(),
        invalidated_at: body.invalidated_at.clone(),
        superseded_by,
        historical_reason: body.historical_reason.clone(),
        temporal_state,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_the_rfc8785_primitive_vector() {
        let value: Value = parse_json(
            br#"{
              "numbers": [333333333.33333329, 1E30, 4.50,
                          2e-3, 0.000000000000000000000000001],
              "string": "\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
              "literals": [null, true, false]
            }"#,
        )
        .expect("RFC 8785 fixture parses");
        let canonical =
            String::from_utf8(canonical_json(&value).expect("canonical bytes")).expect("UTF-8");
        assert_eq!(
            canonical,
            r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"€$\u000f\nA'B\"\\\\\"/"}"#
        );
    }

    #[test]
    fn canonicalizes_number_boundaries_and_negative_zero() {
        let value: Value =
            parse_json(br#"[1e-6,1e-7,1e20,1e21,-0,9007199254740993]"#).expect("numbers parse");
        assert_eq!(
            String::from_utf8(canonical_json(&value).expect("canonical bytes")).expect("UTF-8"),
            "[0.000001,1e-7,100000000000000000000,1e+21,0,9007199254740992]"
        );
    }

    #[test]
    fn canonicalizes_object_keys_by_utf16_code_units() {
        let value: Value =
            parse_json(br#"{"\ue000":1,"\ud83d\ude00":2}"#).expect("Unicode object parses");
        assert_eq!(
            String::from_utf8(canonical_json(&value).expect("canonical bytes")).expect("UTF-8"),
            r#"{"😀":2,"":1}"#
        );
    }

    #[test]
    fn rejects_duplicate_json_object_keys() {
        let error = parse_json::<Value>(br#"{"duplicate":1,"duplicate":2}"#)
            .expect_err("duplicate keys must fail");
        assert_eq!(error.code, ErrorCode::BundleIntegrityFailed);
    }
}
