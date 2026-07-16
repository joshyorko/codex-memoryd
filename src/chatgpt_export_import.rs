use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use zip::ZipArchive;

use crate::domain::VisibleTurn;
use crate::error::Error;
use crate::error::Result;
use crate::ids;
use crate::policy;
use crate::policy::PolicyDecision;
use crate::service::Service;
use crate::store::ledger_safe_summary;
use crate::store::EvidenceLedgerEntry;

const MAX_CONVERSATIONS_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatgptExportMode {
    List,
    Preview,
    Apply,
}

impl ChatgptExportMode {
    fn as_str(self) -> &'static str {
        match self {
            ChatgptExportMode::List => "list",
            ChatgptExportMode::Preview => "preview",
            ChatgptExportMode::Apply => "apply",
        }
    }
}

pub struct ChatgptExportParams<'a> {
    pub export_path: &'a Path,
    pub profile: Option<String>,
    pub workspace: Option<String>,
    pub mode: ChatgptExportMode,
    pub selection: ChatgptExportSelection,
}

#[derive(Debug, Clone, Default)]
pub struct ChatgptExportSelection {
    pub conversation_ids: Vec<String>,
    pub title_contains: Option<String>,
    pub eligible_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatgptExportConversationReport {
    pub conversation_id: String,
    pub title: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub user_turns: usize,
    pub assistant_turns: usize,
    pub skipped_messages: usize,
    pub rejected_messages: usize,
    pub eligible: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatgptExportRejection {
    pub conversation_id: String,
    pub message_id: String,
    pub code: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatgptExportResponse {
    pub mode: String,
    pub source_path: String,
    pub payload_path: String,
    pub conversation_count: usize,
    pub selected_conversations: usize,
    pub filtered_out_conversations: usize,
    pub eligible_conversations: usize,
    pub user_turns: usize,
    pub assistant_turns: usize,
    pub skipped_messages: usize,
    pub rejected_messages: usize,
    pub created: usize,
    pub skipped_existing: usize,
    pub conversations: Vec<ChatgptExportConversationReport>,
    pub rejections: Vec<ChatgptExportRejection>,
}

#[derive(Debug, Deserialize)]
struct ExportConversation {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    create_time: Option<Value>,
    #[serde(default)]
    update_time: Option<Value>,
    mapping: Value,
}

#[derive(Debug, Deserialize)]
struct MappingEntry {
    id: Option<String>,
    #[allow(dead_code)]
    parent: Option<String>,
    message: Option<ExportMessage>,
}

#[derive(Debug, Deserialize)]
struct ExportMessage {
    #[serde(default)]
    author: ExportAuthor,
    #[serde(default)]
    create_time: Option<Value>,
    #[serde(default)]
    content: Option<ExportContent>,
}

#[derive(Debug, Default, Deserialize)]
struct ExportAuthor {
    #[serde(default)]
    role: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExportContent {
    #[serde(default)]
    parts: Option<Vec<Value>>,
}

#[derive(Debug)]
struct ParsedConversation {
    report: ChatgptExportConversationReport,
    accepted: Vec<AcceptedMessage>,
    rejections: Vec<ChatgptExportRejection>,
    has_eligible_messages: bool,
}

#[derive(Debug)]
struct AcceptedMessage {
    message_id: String,
    actor: String,
    content: String,
    created_at: String,
    metadata: Value,
}

pub fn run(service: &Service, params: ChatgptExportParams<'_>) -> Result<ChatgptExportResponse> {
    let profile = service.resolve_profile(&params.profile)?;
    let workspace = service.resolve_workspace(&params.workspace);
    let source_path = params.export_path.display().to_string();
    let detected = detect_payload(params.export_path)?;
    let conversations: Vec<ExportConversation> =
        serde_json::from_slice(&detected.bytes).map_err(|_| {
            Error::invalid_request(
                "unsupported ChatGPT export schema: invalid conversations payload",
            )
        })?;

    let mut reports = Vec::new();
    let mut accepted_total = 0usize;
    let mut assistant_total = 0usize;
    let mut user_total = 0usize;
    let mut skipped_total = 0usize;
    let mut rejected_total = 0usize;
    let mut created = 0usize;
    let mut skipped_existing = 0usize;
    let mut rejections = Vec::new();

    let total_conversations = conversations.len();
    let selected_filter_ids = params
        .selection
        .conversation_ids
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    let selected_title = params
        .selection
        .title_contains
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());

    for conversation in conversations {
        let parsed = parse_conversation(&conversation, &detected.payload_path)?;
        if !matches_selection(
            &conversation.id,
            &parsed.report.title,
            parsed.report.eligible,
            &selected_filter_ids,
            selected_title.as_deref(),
            params.selection.eligible_only,
        ) {
            continue;
        }
        user_total += parsed.report.user_turns;
        assistant_total += parsed.report.assistant_turns;
        skipped_total += parsed.report.skipped_messages;
        rejected_total += parsed.report.rejected_messages;
        if parsed.report.eligible {
            accepted_total += 1;
        }

        if params.mode == ChatgptExportMode::Apply {
            if !parsed.has_eligible_messages {
                for rejection in &parsed.rejections {
                    record_rejection(
                        service,
                        profile.as_str(),
                        &workspace,
                        &conversation.id,
                        &rejection.message_id,
                        &rejection.code,
                        &rejection.reason,
                        &detected.payload_path,
                    )?;
                }
                rejections.extend(parsed.rejections);
                reports.push(parsed.report);
                continue;
            }
            service
                .store
                .ensure_workspace(profile.as_str(), &workspace)?;
            let session_id = format!("chatgpt:{}", conversation.id);
            service.store.ensure_session(
                &session_id,
                profile.as_str(),
                &workspace,
                None,
                None,
                "chatgpt-export",
            )?;

            for message in &parsed.accepted {
                let source_ref = format!(
                    "{}:{}:{}",
                    detected.payload_path, conversation.id, message.message_id
                );
                let source_hash = ids::sha256_hex(
                    format!("chatgpt-export:{session_id}:{}", message.message_id).as_bytes(),
                );
                let (source, source_created) = service.store.upsert_source(
                    profile.as_str(),
                    &workspace,
                    "visible_turn",
                    Some(&source_ref),
                    &source_hash,
                    &message.metadata,
                )?;
                if !source_created {
                    skipped_existing += 1;
                    continue;
                }

                let turn_id = format!(
                    "turn_chatgpt_{}",
                    ids::sha256_hex(format!("{session_id}:{}", message.message_id).as_bytes())
                        .chars()
                        .take(24)
                        .collect::<String>()
                );
                service.store.insert_visible_turn(&VisibleTurn {
                    id: turn_id.clone(),
                    session_id: session_id.clone(),
                    actor: message.actor.clone(),
                    content: message.content.clone(),
                    created_at: message.created_at.clone(),
                    metadata: message.metadata.clone(),
                })?;
                service.store.record_evidence_ledger(&EvidenceLedgerEntry {
                    profile_id: profile.as_str().to_string(),
                    workspace_id: workspace.clone(),
                    repo_id: None,
                    subject_key: None,
                    source_kind: "visible_turn".to_string(),
                    source_id: Some(source.id),
                    source_path: Some(source_ref),
                    source_hash,
                    safe_summary: ledger_safe_summary(&message.content),
                    policy_state: "accepted".to_string(),
                    metadata: json!({
                        "actor": message.actor,
                        "conversation_id": conversation.id,
                        "message_id": message.message_id,
                        "session_id": session_id,
                        "source": "chatgpt-export",
                    }),
                })?;
                created += 1;
            }

            for rejection in &parsed.rejections {
                record_rejection(
                    service,
                    profile.as_str(),
                    &workspace,
                    &conversation.id,
                    &rejection.message_id,
                    &rejection.code,
                    &rejection.reason,
                    &detected.payload_path,
                )?;
            }
        }

        rejections.extend(parsed.rejections);
        reports.push(parsed.report);
    }

    Ok(ChatgptExportResponse {
        mode: params.mode.as_str().to_string(),
        source_path,
        payload_path: detected.payload_path,
        conversation_count: total_conversations,
        selected_conversations: reports.len(),
        filtered_out_conversations: total_conversations.saturating_sub(reports.len()),
        eligible_conversations: accepted_total,
        user_turns: user_total,
        assistant_turns: assistant_total,
        skipped_messages: skipped_total,
        rejected_messages: rejected_total,
        created,
        skipped_existing,
        conversations: reports,
        rejections,
    })
}

fn matches_selection(
    conversation_id: &str,
    title: &str,
    eligible: bool,
    conversation_ids: &std::collections::BTreeSet<&str>,
    title_contains: Option<&str>,
    eligible_only: bool,
) -> bool {
    if !conversation_ids.is_empty() && !conversation_ids.contains(conversation_id) {
        return false;
    }
    if let Some(title_filter) = title_contains {
        if !title.to_ascii_lowercase().contains(title_filter) {
            return false;
        }
    }
    if eligible_only && !eligible {
        return false;
    }
    true
}

fn parse_conversation(
    conversation: &ExportConversation,
    payload_path: &str,
) -> Result<ParsedConversation> {
    let mapping = conversation.mapping.as_object().ok_or_else(|| {
        Error::invalid_request(
            "unsupported ChatGPT export schema: conversation mapping must be an object",
        )
    })?;
    let mut accepted = Vec::new();
    let mut rejections = Vec::new();
    let mut skipped_messages = 0usize;
    let mut user_turns = 0usize;
    let mut assistant_turns = 0usize;
    let mut turn_index = 0usize;

    let mut items = mapping
        .iter()
        .map(|(key, value)| {
            let entry: MappingEntry = serde_json::from_value(value.clone()).map_err(|_| {
                Error::invalid_request(
                    "unsupported ChatGPT export schema: mapping entry must contain a message object",
                )
            })?;
            Ok((key.clone(), entry))
        })
        .collect::<Result<Vec<_>>>()?;

    items.sort_by(|(left_key, left), (right_key, right)| {
        let left_time = entry_time(left);
        let right_time = entry_time(right);
        left_time
            .cmp(&right_time)
            .then_with(|| left_key.cmp(right_key))
    });

    for (fallback_id, entry) in items {
        let Some(message) = entry.message else {
            continue;
        };
        let Some(role) = message
            .author
            .role
            .as_deref()
            .map(|role| role.trim().to_ascii_lowercase())
        else {
            skipped_messages += 1;
            continue;
        };
        if role != "user" && role != "assistant" {
            skipped_messages += 1;
            continue;
        }
        let message_id = entry.id.unwrap_or(fallback_id);
        let Some(content) = extract_text(&message) else {
            skipped_messages += 1;
            continue;
        };
        turn_index += 1;
        match policy::screen_content(&content, usize::MAX) {
            PolicyDecision::Accept(cleaned) => {
                if role == "user" {
                    user_turns += 1;
                } else {
                    assistant_turns += 1;
                }
                accepted.push(AcceptedMessage {
                    message_id: message_id.clone(),
                    actor: role,
                    created_at: message
                        .create_time
                        .as_ref()
                        .and_then(timestamp_to_rfc3339)
                        .unwrap_or_else(ids::now_rfc3339),
                    metadata: json!({
                        "origin": "chatgpt-export",
                        "conversation_id": conversation.id,
                        "message_id": message_id,
                        "turn_index": turn_index,
                        "title": conversation_title(&conversation.title),
                        "conversation_created_at": conversation.create_time.as_ref().and_then(timestamp_to_rfc3339),
                        "conversation_updated_at": conversation.update_time.as_ref().and_then(timestamp_to_rfc3339),
                        "source_file_path": payload_path,
                    }),
                    content: cleaned,
                });
            }
            PolicyDecision::Reject { code, reason } => {
                rejections.push(ChatgptExportRejection {
                    conversation_id: conversation.id.clone(),
                    message_id,
                    code,
                    reason,
                });
            }
        }
    }

    let has_eligible_messages = !accepted.is_empty();
    Ok(ParsedConversation {
        report: ChatgptExportConversationReport {
            conversation_id: conversation.id.clone(),
            title: conversation_title(&conversation.title),
            created_at: conversation
                .create_time
                .as_ref()
                .and_then(timestamp_to_rfc3339),
            updated_at: conversation
                .update_time
                .as_ref()
                .and_then(timestamp_to_rfc3339),
            user_turns,
            assistant_turns,
            skipped_messages,
            rejected_messages: rejections.len(),
            eligible: !accepted.is_empty(),
        },
        accepted,
        rejections,
        has_eligible_messages,
    })
}

fn extract_text(message: &ExportMessage) -> Option<String> {
    let parts = message.content.as_ref()?.parts.as_ref()?;
    let text = parts
        .iter()
        .filter_map(|part| part.as_str().map(str::trim))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn entry_time(entry: &MappingEntry) -> Option<i128> {
    entry
        .message
        .as_ref()
        .and_then(|message| message.create_time.as_ref())
        .and_then(timestamp_sort_key)
}

fn timestamp_sort_key(value: &Value) -> Option<i128> {
    if let Some(seconds) = value.as_i64() {
        return Some((seconds as i128) * 1000);
    }
    if let Some(seconds) = value.as_f64() {
        return Some((seconds * 1000.0).round() as i128);
    }
    None
}

fn timestamp_to_rfc3339(value: &Value) -> Option<String> {
    let seconds = value
        .as_i64()
        .map(|seconds| seconds as f64)
        .or_else(|| value.as_f64())?;
    let whole = seconds.trunc() as i64;
    let mut nanos = ((seconds.fract().abs()) * 1_000_000_000.0).round() as u32;
    if nanos >= 1_000_000_000 {
        nanos = 999_999_999;
    }
    let timestamp = OffsetDateTime::from_unix_timestamp(whole)
        .ok()?
        .replace_nanosecond(nanos)
        .ok()?;
    timestamp.format(&Rfc3339).ok()
}

fn conversation_title(title: &Option<String>) -> String {
    let candidate = title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Untitled conversation");
    let (redacted, _) = policy::redact_secret_like(candidate);
    match policy::screen_string_value(&redacted) {
        PolicyDecision::Accept(cleaned) => cleaned,
        PolicyDecision::Reject { .. } => "Untitled conversation".to_string(),
    }
}

struct DetectedPayload {
    payload_path: String,
    bytes: Vec<u8>,
}

fn detect_payload(path: &Path) -> Result<DetectedPayload> {
    if path.is_dir() {
        let payload = find_conversations_file(path)?.ok_or_else(|| {
            Error::invalid_request(
                "unsupported ChatGPT export schema: conversations payload not found",
            )
        })?;
        return Ok(DetectedPayload {
            payload_path: payload.display().to_string(),
            bytes: fs::read(&payload)
                .map_err(|err| {
                    Error::invalid_request(format!("failed to read conversations payload: {err}"))
                })
                .and_then(|bytes| enforce_payload_size(bytes, &payload.display().to_string()))?,
        });
    }

    let file = fs::File::open(path)
        .map_err(|err| Error::invalid_request(format!("failed to open ChatGPT export: {err}")))?;
    let mut archive = ZipArchive::new(file).map_err(|_| {
        Error::invalid_request(
            "unsupported ChatGPT export schema: expected a zip archive or extracted directory",
        )
    })?;
    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|_| Error::invalid_request("failed to read zip entry from ChatGPT export"))?;
        let name = entry.name().to_string();
        if Path::new(&name)
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value == "conversations.json")
        {
            let bytes = read_zip_entry_capped(&mut entry, &name)?;
            return Ok(DetectedPayload {
                payload_path: name,
                bytes,
            });
        }
    }
    Err(Error::invalid_request(
        "unsupported ChatGPT export schema: conversations payload not found",
    ))
}

fn find_conversations_file(root: &Path) -> Result<Option<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).map_err(|err| {
            Error::invalid_request(format!("failed to read export directory: {err}"))
        })? {
            let entry = entry.map_err(|err| {
                Error::invalid_request(format!("failed to read export directory entry: {err}"))
            })?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
                continue;
            }
            if entry_path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value == "conversations.json")
            {
                return Ok(Some(entry_path));
            }
        }
    }
    Ok(None)
}

fn enforce_payload_size(bytes: Vec<u8>, payload_path: &str) -> Result<Vec<u8>> {
    if bytes.len() > MAX_CONVERSATIONS_PAYLOAD_BYTES {
        return Err(Error::invalid_request(format!(
            "conversations payload exceeds {} bytes: {payload_path}",
            MAX_CONVERSATIONS_PAYLOAD_BYTES
        )));
    }
    Ok(bytes)
}

fn read_zip_entry_capped<R: Read>(entry: &mut R, payload_path: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = entry
            .read(&mut chunk)
            .map_err(|_| Error::invalid_request("failed to read conversations payload from zip"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_CONVERSATIONS_PAYLOAD_BYTES {
            return Err(Error::invalid_request(format!(
                "conversations payload exceeds {} bytes: {payload_path}",
                MAX_CONVERSATIONS_PAYLOAD_BYTES
            )));
        }
    }
    Ok(bytes)
}

fn record_rejection(
    service: &Service,
    profile: &str,
    workspace: &str,
    conversation_id: &str,
    message_id: &str,
    code: &str,
    reason: &str,
    payload_path: &str,
) -> Result<()> {
    let source_path = format!("{payload_path}:{conversation_id}:{message_id}");
    let source_hash = ids::sha256_hex(
        format!("{profile}\n{workspace}\n{conversation_id}\n{message_id}\n{code}").as_bytes(),
    );
    service.store.record_evidence_ledger(&EvidenceLedgerEntry {
        profile_id: profile.to_string(),
        workspace_id: workspace.to_string(),
        repo_id: None,
        subject_key: None,
        source_kind: "visible_turn".to_string(),
        source_id: None,
        source_path: Some(source_path),
        source_hash,
        safe_summary: ledger_safe_summary(&format!(
            "rejected chatgpt export message {conversation_id}/{message_id}: {reason}"
        )),
        policy_state: code.to_string(),
        metadata: json!({
            "conversation_id": conversation_id,
            "message_id": message_id,
            "source": "chatgpt-export",
        }),
    })?;
    service.store.record_policy_event(
        Some(profile),
        Some(workspace),
        "rejected_turn",
        code,
        reason,
        "chatgpt-export",
    )?;
    Ok(())
}
