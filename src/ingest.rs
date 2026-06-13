//! Local Codex memory ingestion (SPEC §7). Implements chunking, per-chunk
//! classification, hashing, dedupe, and idempotent preview/apply.
//!
//! Preview writes nothing durable; apply writes memory sources + records and is
//! idempotent via source-hash (whole file) and content-hash (per chunk) dedupe.

use serde_json::json;
use serde_json::Value;

use crate::domain::Profile;
use crate::domain::RecordType;
use crate::error::Error;
use crate::error::Result;
use crate::ids;
use crate::policy;
use crate::policy::PolicyDecision;
use crate::protocol::SyncFile;
use crate::protocol::SyncRejection;
use crate::protocol::SyncResponse;
use crate::protocol::SyncTypeBreakdown;
use crate::store::ledger_safe_summary;
use crate::store::EvidenceLedgerEntry;
use crate::store::NewRecord;
use crate::store::Store;

/// Maximum characters per chunk before fixed-size splitting kicks in.
const MAX_CHUNK_CHARS: usize = 1_500;
/// Minimum meaningful chunk length; shorter fragments are dropped.
const MIN_CHUNK_CHARS: usize = 8;
/// Hard cap on chunks derived from a single file (defensive).
const MAX_CHUNKS_PER_FILE: usize = 200;

/// Accepted artifact kinds (SPEC §6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    MemorySummary,
    MemoryRegistry,
    RolloutSummary,
    AdHocNote,
    Unknown,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::MemorySummary => "memory_summary",
            ArtifactKind::MemoryRegistry => "memory_registry",
            ArtifactKind::RolloutSummary => "rollout_summary",
            ArtifactKind::AdHocNote => "ad_hoc_note",
            ArtifactKind::Unknown => "unknown",
        }
    }

    /// The `memory_sources.kind` value for this artifact.
    pub fn source_kind(self) -> &'static str {
        match self {
            ArtifactKind::MemorySummary => "local_memory_summary",
            ArtifactKind::MemoryRegistry => "local_memory_registry",
            ArtifactKind::RolloutSummary => "rollout_summary",
            ArtifactKind::AdHocNote => "ad_hoc_note",
            ArtifactKind::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> ArtifactKind {
        match value.trim().to_ascii_lowercase().as_str() {
            "memory_summary" => ArtifactKind::MemorySummary,
            "memory_registry" => ArtifactKind::MemoryRegistry,
            "rollout_summary" => ArtifactKind::RolloutSummary,
            "ad_hoc_note" => ArtifactKind::AdHocNote,
            _ => ArtifactKind::Unknown,
        }
    }

    /// Infer the artifact kind from a path when not provided (SPEC §7.1 layout).
    pub fn infer_from_path(path: &str) -> ArtifactKind {
        let lower = path.to_ascii_lowercase();
        let base = lower.rsplit('/').next().unwrap_or(&lower);
        if base == "memory_summary.md" {
            ArtifactKind::MemorySummary
        } else if base == "memory.md" {
            ArtifactKind::MemoryRegistry
        } else if lower.contains("rollout_summaries/") || lower.contains("rollout_summary") {
            ArtifactKind::RolloutSummary
        } else if lower.contains("ad_hoc") || lower.contains("/notes/") {
            ArtifactKind::AdHocNote
        } else if lower.ends_with(".md") {
            // A markdown file under memories that isn't a known artifact is an
            // ad-hoc note by default.
            ArtifactKind::AdHocNote
        } else {
            ArtifactKind::Unknown
        }
    }
}

/// Sync mode (SPEC §6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    Preview,
    Apply,
}

impl SyncMode {
    pub fn parse(value: &str) -> Result<SyncMode> {
        match value.trim().to_ascii_lowercase().as_str() {
            "preview" => Ok(SyncMode::Preview),
            "apply" => Ok(SyncMode::Apply),
            other => Err(Error::invalid_request(format!(
                "invalid sync mode '{other}'"
            ))),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            SyncMode::Preview => "preview",
            SyncMode::Apply => "apply",
        }
    }
}

/// A classified candidate chunk derived from a file.
#[derive(Debug, Clone)]
pub struct CandidateChunk {
    pub content: String,
    pub classification: policy::Classification,
}

/// Parameters for a sync run, resolved from the request by the server/CLI.
pub struct SyncParams<'a> {
    pub profile: Profile,
    pub workspace: &'a str,
    pub repo_id: Option<&'a str>,
    pub source_root: &'a str,
    pub mode: SyncMode,
    pub files: &'a [SyncFile],
    pub max_record_chars: usize,
}

/// Run a local Codex memory sync. The store is only mutated in apply mode.
pub fn run_sync(store: &Store, params: &SyncParams) -> Result<SyncResponse> {
    let profile_str = params.profile.as_str();

    if matches!(params.mode, SyncMode::Apply) {
        store.start_sync_cursor(profile_str, params.workspace, params.source_root)?;
        store.ensure_workspace(profile_str, params.workspace)?;
    }

    let mut files_scanned = 0usize;
    let mut proposed = 0usize;
    let mut created = 0usize;
    let mut updated = 0usize; // count of stale records superseded on re-import
    let mut skipped = 0usize;
    let mut rejected = 0usize;
    let mut rejections: Vec<SyncRejection> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut types = SyncTypeBreakdown::default();

    for file in params.files {
        files_scanned += 1;
        let kind = match &file.kind {
            Some(k) if !k.trim().is_empty() => {
                let parsed = ArtifactKind::parse(k);
                if parsed == ArtifactKind::Unknown {
                    // Fall back to inference; unknown declared kind that we still
                    // can't classify is skipped per SPEC.
                    let inferred = ArtifactKind::infer_from_path(&file.path);
                    if inferred == ArtifactKind::Unknown {
                        if matches!(params.mode, SyncMode::Apply) {
                            let safe_summary = ledger_safe_summary(&format!(
                                "rejected sync import {}: unsupported artifact kind",
                                file.path
                            ));
                            let source_hash = ledger_hash(&[
                                profile_str,
                                params.workspace,
                                &file.path,
                                "sync_source_invalid",
                                &ids::sha256_hex(file.content.as_bytes()),
                            ]);
                            let _ = store.record_evidence_ledger(&EvidenceLedgerEntry {
                                profile_id: profile_str.to_string(),
                                workspace_id: params.workspace.to_string(),
                                repo_id: params.repo_id.map(|s| s.to_string()),
                                subject_key: None,
                                source_kind: "sync_local".to_string(),
                                source_id: None,
                                source_path: Some(file.path.clone()),
                                source_hash,
                                safe_summary,
                                policy_state: "sync_source_invalid".to_string(),
                                metadata: json!({
                                    "artifact_kind": k,
                                    "source_root": params.source_root,
                                }),
                            });
                        }
                        rejected += 1;
                        rejections.push(SyncRejection {
                            path: file.path.clone(),
                            reason: format!("unsupported artifact kind '{k}'"),
                            code: "sync_source_invalid".to_string(),
                        });
                        if matches!(params.mode, SyncMode::Apply) {
                            let _ = store.record_policy_event(
                                Some(profile_str),
                                Some(params.workspace),
                                "unsupported_kind",
                                "sync_source_invalid",
                                &format!("unsupported artifact kind '{k}'"),
                                "sync",
                            );
                        }
                        continue;
                    }
                    inferred
                } else {
                    parsed
                }
            }
            _ => ArtifactKind::infer_from_path(&file.path),
        };

        // Whole-file secret screen first: a file containing secrets is rejected
        // wholesale (SPEC §7.7 "filter secrets").
        let trimmed = file.content.trim();
        if trimmed.is_empty() {
            rejected += 1;
            rejections.push(SyncRejection {
                path: file.path.clone(),
                reason: "empty file".to_string(),
                code: "invalid_request".to_string(),
            });
            if matches!(params.mode, SyncMode::Apply) {
                let safe_summary =
                    ledger_safe_summary(&format!("rejected sync import {}: empty file", file.path));
                let source_hash = ledger_hash(&[
                    profile_str,
                    params.workspace,
                    &file.path,
                    "invalid_request",
                    "empty_file",
                    &ids::sha256_hex(file.content.as_bytes()),
                ]);
                let _ = store.record_evidence_ledger(&EvidenceLedgerEntry {
                    profile_id: profile_str.to_string(),
                    workspace_id: params.workspace.to_string(),
                    repo_id: params.repo_id.map(|s| s.to_string()),
                    subject_key: None,
                    source_kind: "sync_local".to_string(),
                    source_id: None,
                    source_path: Some(file.path.clone()),
                    source_hash,
                    safe_summary,
                    policy_state: "invalid_request".to_string(),
                    metadata: json!({
                        "source_root": params.source_root,
                    }),
                });
            }
            continue;
        }
        if let Some(label) = policy::detect_secret(trimmed) {
            rejected += 1;
            rejections.push(SyncRejection {
                path: file.path.clone(),
                reason: format!("secret-like content detected: {label}"),
                code: "secret_detected".to_string(),
            });
            if matches!(params.mode, SyncMode::Apply) {
                let safe_summary = ledger_safe_summary(&format!(
                    "rejected sync import {}: secret-like content detected",
                    file.path
                ));
                let source_hash = ledger_hash(&[
                    profile_str,
                    params.workspace,
                    &file.path,
                    "secret_detected",
                    label,
                    &ids::sha256_hex(trimmed.as_bytes()),
                ]);
                let _ = store.record_evidence_ledger(&EvidenceLedgerEntry {
                    profile_id: profile_str.to_string(),
                    workspace_id: params.workspace.to_string(),
                    repo_id: params.repo_id.map(|s| s.to_string()),
                    subject_key: None,
                    source_kind: "sync_local".to_string(),
                    source_id: None,
                    source_path: Some(file.path.clone()),
                    source_hash,
                    safe_summary,
                    policy_state: "secret_detected".to_string(),
                    metadata: json!({
                        "artifact_kind": kind.as_str(),
                        "source_root": params.source_root,
                        "label": label,
                    }),
                });
            }
            if matches!(params.mode, SyncMode::Apply) {
                let _ = store.record_policy_event(
                    Some(profile_str),
                    Some(params.workspace),
                    "secret_detected",
                    "secret_detected",
                    &format!("file {} contained {label}", file.path),
                    "sync",
                );
            }
            continue;
        }
        if policy::detect_injection(trimmed) {
            rejected += 1;
            rejections.push(SyncRejection {
                path: file.path.clone(),
                reason: "prompt-injection-like content detected".to_string(),
                code: "policy_denied".to_string(),
            });
            if matches!(params.mode, SyncMode::Apply) {
                let safe_summary = ledger_safe_summary(&format!(
                    "rejected sync import {}: prompt-injection-like content detected",
                    file.path
                ));
                let source_hash = ledger_hash(&[
                    profile_str,
                    params.workspace,
                    &file.path,
                    "policy_denied",
                    "injection",
                    &ids::sha256_hex(trimmed.as_bytes()),
                ]);
                let _ = store.record_evidence_ledger(&EvidenceLedgerEntry {
                    profile_id: profile_str.to_string(),
                    workspace_id: params.workspace.to_string(),
                    repo_id: params.repo_id.map(|s| s.to_string()),
                    subject_key: None,
                    source_kind: "sync_local".to_string(),
                    source_id: None,
                    source_path: Some(file.path.clone()),
                    source_hash,
                    safe_summary,
                    policy_state: "policy_denied".to_string(),
                    metadata: json!({
                        "artifact_kind": kind.as_str(),
                        "source_root": params.source_root,
                    }),
                });
            }
            if matches!(params.mode, SyncMode::Apply) {
                let _ = store.record_policy_event(
                    Some(profile_str),
                    Some(params.workspace),
                    "injection",
                    "policy_denied",
                    &format!("file {} looked like prompt injection", file.path),
                    "sync",
                );
            }
            continue;
        }

        // Source-hash dedupe: unchanged file → whole file skipped (SPEC §7.9).
        let raw_hash = file
            .hash
            .clone()
            .filter(|h| !h.trim().is_empty())
            .unwrap_or_else(|| {
                ids::source_hash(profile_str, params.workspace, &file.path, trimmed)
            });

        let already_imported = store
            .find_source(profile_str, params.workspace, Some(&file.path), &raw_hash)?
            .is_some();

        // Build source metadata (provenance).
        let source_metadata = json!({
            "origin": "codex-local-memory",
            "artifact_kind": kind.as_str(),
            "local_path": file.path,
            "source_root": params.source_root,
            "modified_at": file.modified_at,
            "idempotency_key": file.idempotency_key,
        });

        // Derive candidate chunks regardless of mode (preview needs counts).
        let chunks = derive_chunks(trimmed, kind, params.profile, params.repo_id.is_some());
        let chunk_count = chunks.len();
        proposed += chunk_count;
        for chunk in &chunks {
            tally_type(&mut types, chunk.classification.record_type);
        }

        if matches!(params.mode, SyncMode::Preview) {
            // Preview writes nothing. Note already-imported files as skipped so
            // the operator sees the idempotent picture.
            if already_imported {
                skipped += chunk_count;
            }
            continue;
        }

        // Apply mode: persist the source then each chunk.
        let (source, source_created) = store.upsert_source(
            profile_str,
            params.workspace,
            kind.source_kind(),
            Some(&file.path),
            &raw_hash,
            &source_metadata,
        )?;

        let source_ledger_summary = ledger_safe_summary(&format!(
            "imported {} chunk(s) from {}",
            chunk_count, file.path
        ));
        store.record_evidence_ledger(&EvidenceLedgerEntry {
            profile_id: profile_str.to_string(),
            workspace_id: params.workspace.to_string(),
            repo_id: params.repo_id.map(|s| s.to_string()),
            subject_key: None,
            source_kind: "sync_local".to_string(),
            source_id: Some(source.id.clone()),
            source_path: Some(file.path.clone()),
            source_hash: raw_hash.clone(),
            safe_summary: source_ledger_summary,
            policy_state: "accepted".to_string(),
            metadata: json!({
                "artifact_kind": kind.as_str(),
                "source_root": params.source_root,
                "source_created": source_created,
                "already_imported": already_imported,
                "chunk_count": chunk_count,
                "created": created,
                "updated": updated,
                "skipped": skipped,
                "rejected": rejected,
            }),
        })?;

        if already_imported && !source_created {
            // Whole file already imported and unchanged → skip all its chunks.
            skipped += chunk_count;
            continue;
        }

        // If this path already had records, its content changed (different
        // source hash). Track the fresh content hashes so stale chunks can be
        // superseded after writing (SPEC §4.1.7).
        let path_had_records = store
            .count_active_records_for_path(profile_str, params.workspace, &file.path)
            .unwrap_or(0)
            > 0;
        let mut fresh_hashes: Vec<String> = Vec::new();

        for chunk in chunks {
            let decision = policy::screen_content(&chunk.content, params.max_record_chars);
            let content = match decision {
                PolicyDecision::Accept(c) => c,
                PolicyDecision::Reject { code, reason } => {
                    rejected += 1;
                    rejections.push(SyncRejection {
                        path: file.path.clone(),
                        reason,
                        code,
                    });
                    continue;
                }
            };
            let class = &chunk.classification;
            let content_hash = ids::content_hash(
                profile_str,
                params.workspace,
                params.repo_id,
                class.record_type.as_str(),
                class.scope.as_str(),
                &content,
            );
            fresh_hashes.push(content_hash.clone());
            let metadata = json!({
                "origin": "codex-local-memory",
                "artifact_kind": kind.as_str(),
                "local_path": file.path,
                "source_id": source.id,
            });
            let new_record = NewRecord {
                profile_id: profile_str.to_string(),
                workspace_id: params.workspace.to_string(),
                repo_id: params.repo_id.map(|s| s.to_string()),
                subject_id: None,
                episode_id: None,
                scope: class.scope,
                record_type: class.record_type,
                content,
                related_files: class.related_files.clone(),
                tags: class.tags.clone(),
                sensitivity: class.sensitivity,
                portability: class.portability,
                confidence: class.confidence,
                source_ids: vec![source.id.clone()],
                content_hash,
                supersedes: vec![],
                metadata,
            };
            match store.upsert_record(&new_record)? {
                crate::store::UpsertOutcome::Created(_) => created += 1,
                crate::store::UpsertOutcome::Skipped(_) => skipped += 1,
            }
        }

        // Supersede stale chunks: if the file previously produced records that
        // are no longer present in the fresh import, archive them.
        if path_had_records {
            updated += store
                .archive_stale_path_records(
                    profile_str,
                    params.workspace,
                    &file.path,
                    &fresh_hashes,
                )
                .unwrap_or(0);
        }
    }

    if matches!(params.mode, SyncMode::Apply) {
        store.complete_sync_cursor(profile_str, params.workspace, params.source_root, None)?;
    }
    let cursor = store
        .get_sync_cursor(profile_str, params.workspace, params.source_root)
        .ok()
        .flatten();
    let sync_cursor = crate::protocol::SyncCursorView {
        source_root: params.source_root.to_string(),
        last_started_at: cursor.as_ref().and_then(|c| c.0.clone()),
        last_completed_at: cursor.as_ref().and_then(|c| c.1.clone()),
        last_error: cursor.as_ref().and_then(|c| c.2.clone()),
    };

    if rejected > 0 {
        warnings.push(format!("{rejected} item(s) rejected by safety policy"));
    }

    Ok(SyncResponse {
        mode: params.mode.as_str().to_string(),
        files_scanned,
        proposed,
        created,
        updated,
        skipped,
        rejected,
        rejections,
        types,
        warnings,
        sync_cursor,
    })
}

fn tally_type(types: &mut SyncTypeBreakdown, t: RecordType) {
    match t {
        RecordType::Preference => types.preference += 1,
        RecordType::Command => types.command += 1,
        RecordType::RepoConvention => types.repo_convention += 1,
        RecordType::Decision => types.decision += 1,
        RecordType::Gotcha => types.gotcha += 1,
        RecordType::TaskCheckpoint => types.task_checkpoint += 1,
        _ => types.other += 1,
    }
}

fn ledger_hash(parts: &[&str]) -> String {
    ids::sha256_hex(parts.join("\u{1f}").as_bytes())
}

/// Derive classified candidate chunks from a file's content using the artifact
/// kind to choose a chunking strategy (SPEC §7.12).
pub fn derive_chunks(
    content: &str,
    kind: ArtifactKind,
    profile: Profile,
    repo_present: bool,
) -> Vec<CandidateChunk> {
    let raw_chunks = match kind {
        // Registries and summaries are heading/bullet structured.
        ArtifactKind::MemoryRegistry | ArtifactKind::MemorySummary => chunk_markdown(content),
        // Rollouts are session/heading blocks.
        ArtifactKind::RolloutSummary => chunk_markdown(content),
        // Ad-hoc notes: bullets/paragraphs.
        ArtifactKind::AdHocNote | ArtifactKind::Unknown => chunk_markdown(content),
    };

    raw_chunks
        .into_iter()
        .take(MAX_CHUNKS_PER_FILE)
        .filter(|c| c.trim().chars().count() >= MIN_CHUNK_CHARS)
        .map(|c| {
            let classification = policy::classify(&c, profile, repo_present);
            CandidateChunk {
                content: c,
                classification,
            }
        })
        .collect()
}

/// Chunk markdown by heading sections, then bullet groups / paragraphs within,
/// finally fixed-size fallback for oversized blocks.
fn chunk_markdown(content: &str) -> Vec<String> {
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in content.lines() {
        let is_heading = line.trim_start().starts_with('#');
        if is_heading && !current.trim().is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        sections.push(current);
    }
    if sections.is_empty() {
        sections.push(content.to_string());
    }

    let mut chunks = Vec::new();
    for section in sections {
        for piece in split_section(&section) {
            for sized in split_fixed(&piece) {
                let trimmed = sized.trim();
                if !trimmed.is_empty() {
                    chunks.push(trimmed.to_string());
                }
            }
        }
    }
    chunks
}

/// Within a heading section, split into a heading-led intro + individual bullet
/// items + paragraphs. A heading line is attached to the following block so
/// each chunk has context.
fn split_section(section: &str) -> Vec<String> {
    let mut heading = String::new();
    let mut body_lines: Vec<&str> = Vec::new();
    for (i, line) in section.lines().enumerate() {
        if i == 0 && line.trim_start().starts_with('#') {
            heading = line.trim().to_string();
        } else {
            body_lines.push(line);
        }
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut paragraph = String::new();

    let flush = |paragraph: &mut String, chunks: &mut Vec<String>, heading: &str| {
        if !paragraph.trim().is_empty() {
            let chunk = if heading.is_empty() {
                paragraph.trim().to_string()
            } else {
                format!("{}\n{}", heading, paragraph.trim())
            };
            chunks.push(chunk);
        }
        paragraph.clear();
    };

    for line in body_lines {
        let t = line.trim_start();
        let is_bullet = t.starts_with("- ") || t.starts_with("* ") || is_numbered_bullet(t);
        if is_bullet {
            // bullets break paragraph; each bullet is its own chunk (with heading).
            flush(&mut paragraph, &mut chunks, &heading);
            let bullet = t.to_string();
            let chunk = if heading.is_empty() {
                bullet
            } else {
                format!("{heading}\n{bullet}")
            };
            chunks.push(chunk);
        } else if t.is_empty() {
            flush(&mut paragraph, &mut chunks, &heading);
        } else {
            paragraph.push_str(line);
            paragraph.push('\n');
        }
    }
    flush(&mut paragraph, &mut chunks, &heading);

    // If a heading had no body, keep the heading itself as a (weak) chunk.
    if chunks.is_empty() && !heading.is_empty() {
        chunks.push(heading);
    }
    chunks
}

fn is_numbered_bullet(t: &str) -> bool {
    let mut chars = t.chars();
    let mut saw_digit = false;
    for c in chars.by_ref() {
        if c.is_ascii_digit() {
            saw_digit = true;
        } else if c == '.' || c == ')' {
            return saw_digit;
        } else {
            return false;
        }
    }
    false
}

/// Fixed-size fallback: split overly-long chunks on character boundaries
/// (SPEC §7.12 "fallback fixed-size chunks").
fn split_fixed(chunk: &str) -> Vec<String> {
    if chunk.chars().count() <= MAX_CHUNK_CHARS {
        return vec![chunk.to_string()];
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut count = 0;
    for ch in chunk.chars() {
        buf.push(ch);
        count += 1;
        if count >= MAX_CHUNK_CHARS && (ch == '\n' || ch == '.' || ch == ' ') {
            out.push(std::mem::take(&mut buf));
            count = 0;
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf);
    }
    out
}

/// Build the unused-metadata placeholder used in tests/diagnostics.
pub fn _unused() -> Value {
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn store() -> Store {
        Store::open(":memory:").unwrap()
    }

    fn file(path: &str, content: &str) -> SyncFile {
        SyncFile {
            path: path.to_string(),
            kind: None,
            content: content.to_string(),
            hash: None,
            modified_at: None,
            idempotency_key: None,
            metadata: None,
        }
    }

    fn params<'a>(
        ws: &'a str,
        root: &'a str,
        mode: SyncMode,
        files: &'a [SyncFile],
    ) -> SyncParams<'a> {
        SyncParams {
            profile: Profile::Personal,
            workspace: ws,
            repo_id: None,
            source_root: root,
            mode,
            files,
            max_record_chars: 8000,
        }
    }

    #[test]
    fn chunking_splits_bullets() {
        let md = "# Preferences\n- prefer repo-native commands\n- use cargo test\n";
        let chunks = derive_chunks(md, ArtifactKind::MemorySummary, Profile::Personal, false);
        assert!(chunks.len() >= 2, "expected at least two bullet chunks");
    }

    #[test]
    fn preview_writes_nothing() {
        let s = store();
        let files = vec![file(
            "memory_summary.md",
            "# Prefs\n- prefer repo-native workflows\n",
        )];
        let p = params("ws", "/root", SyncMode::Preview, &files);
        let resp = run_sync(&s, &p).unwrap();
        assert_eq!(resp.mode, "preview");
        assert!(resp.proposed > 0);
        assert_eq!(resp.created, 0);
        assert_eq!(s.count_records().unwrap(), 0);
    }

    #[test]
    fn apply_is_idempotent() {
        let s = store();
        let files = vec![file(
            "rollout_summaries/2026-06-05.md",
            "# Checkpoint\n- implemented sync endpoint\n",
        )];
        let p = params("ws", "/root", SyncMode::Apply, &files);
        let first = run_sync(&s, &p).unwrap();
        let second = run_sync(&s, &p).unwrap();
        assert!(first.created >= 1);
        assert_eq!(second.created, 0);
        assert!(second.skipped >= 1);
        assert_eq!(s.count_records().unwrap(), first.created as i64);
    }

    #[test]
    fn secret_file_is_rejected() {
        let s = store();
        let files = vec![file(
            "extensions/ad_hoc/notes/secret.md",
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIabcdEFGH1234\n",
        )];
        let p = params("ws", "/root", SyncMode::Apply, &files);
        let resp = run_sync(&s, &p).unwrap();
        assert_eq!(resp.created, 0);
        assert_eq!(resp.rejected, 1);
        assert_eq!(s.count_records().unwrap(), 0);
    }
}
