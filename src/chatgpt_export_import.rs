use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use serde::de::DeserializeSeed;
use serde::de::Error as _;
use serde::de::SeqAccess;
use serde::de::Visitor;
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
use crate::store::Store;

const MAX_CONVERSATIONS_MEMBER_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_CONVERSATION_MEMBERS: usize = 1_024;
const MAX_MESSAGES: usize = 1_000_000;
const LARGE_ARCHIVE_CONVERSATIONS: usize = 100;

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
    pub since: Option<String>,
    pub until: Option<String>,
    pub max_conversations: Option<usize>,
    pub all: bool,
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
    pub selection_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatgptExportSkippedConversationReport {
    pub conversation_id: String,
    pub title: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub eligible: bool,
    pub selection_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatgptExportRejection {
    pub conversation_id: String,
    pub message_id: String,
    pub code: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatgptExportMemberReport {
    pub name: String,
    pub conversation_count: usize,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
    pub members: Vec<ChatgptExportMemberReport>,
    pub conversations: Vec<ChatgptExportConversationReport>,
    pub skipped_conversations: Vec<ChatgptExportSkippedConversationReport>,
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
    let mut total_conversations = 0usize;
    let mut total_messages = 0usize;
    let mut conversation_ids = HashSet::new();
    let mut message_ids = HashSet::new();
    let mut members = Vec::new();
    for member in &detected.payloads {
        let mut member_conversation_count = 0usize;
        stream_conversations(member, |conversation| {
            member_conversation_count += 1;
            total_conversations = total_conversations.checked_add(1).ok_or_else(|| {
                Error::invalid_request("ChatGPT export has too many conversations")
            })?;
            if total_conversations > 100_000 {
                return Err(Error::invalid_request(
                    "ChatGPT export exceeds the 100000 conversation limit",
                ));
            }
            if !conversation_ids.insert(conversation.id.clone()) {
                return Err(Error::invalid_request(
                    "invalid ChatGPT export: duplicate conversation identity",
                ));
            }
            let mapping = conversation.mapping.as_object().ok_or_else(|| {
                Error::invalid_request(
                    "unsupported ChatGPT export schema: conversation mapping must be an object",
                )
            })?;
            for (fallback_message_id, value) in mapping {
                total_messages = total_messages.checked_add(1).ok_or_else(|| {
                    Error::invalid_request("ChatGPT export has too many messages")
                })?;
                if total_messages > MAX_MESSAGES {
                    return Err(Error::invalid_request(
                        "ChatGPT export exceeds the 1000000 message limit",
                    ));
                }
                let message_id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(fallback_message_id);
                if !message_ids.insert((conversation.id.clone(), message_id.to_string())) {
                    return Err(Error::invalid_request(
                        "invalid ChatGPT export: duplicate message identity",
                    ));
                }
            }
            Ok(())
        })?;
        members.push(ChatgptExportMemberReport {
            name: member.name().to_string(),
            conversation_count: member_conversation_count,
        });
    }

    let mut reports = Vec::new();
    let mut accepted_total = 0usize;
    let mut assistant_total = 0usize;
    let mut user_total = 0usize;
    let mut skipped_total = 0usize;
    let mut rejected_total = 0usize;
    let mut created = 0usize;
    let mut skipped_existing = 0usize;
    let mut rejections = Vec::new();
    let mut skipped_conversations = Vec::new();

    if params.mode == ChatgptExportMode::Apply
        && total_conversations > LARGE_ARCHIVE_CONVERSATIONS
        && !params.selection.all
        && !params.selection.has_filter()
    {
        return Err(Error::invalid_request(format!(
            "ChatGPT archive has {total_conversations} conversations; pass a selection filter or --all to apply every conversation"
        )));
    }
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
    let since = params
        .selection
        .since
        .as_deref()
        .map(parse_filter_timestamp)
        .transpose()?;
    let until = params
        .selection
        .until
        .as_deref()
        .map(parse_filter_timestamp)
        .transpose()?;

    let mut staged_writes = 0usize;
    let mut process_member = |member: &PayloadMember,
                              transaction: Option<&rusqlite::Transaction<'_>>|
     -> Result<()> {
        let payload_path = member.name().to_string();
        stream_conversations(member, |conversation| {
            if params
                .selection
                .max_conversations
                .is_some_and(|max| reports.len() >= max)
            {
                let parsed = parse_conversation(&conversation, &payload_path)?;
                skipped_conversations.push(skipped_conversation_report(
                    &parsed.report,
                    "max-conversations limit reached".to_string(),
                ));
                return Ok(());
            }
            let parsed = parse_conversation(&conversation, &payload_path)?;
            if !matches_selection(
                &conversation.id,
                &parsed.report.title,
                parsed
                    .report
                    .updated_at
                    .as_deref()
                    .or(parsed.report.created_at.as_deref()),
                parsed.report.eligible,
                &selected_filter_ids,
                selected_title.as_deref(),
                since,
                until,
                params.selection.eligible_only,
            ) {
                let reason = selected_title
                    .as_deref()
                    .filter(|title| !parsed.report.title.to_ascii_lowercase().contains(title))
                    .map(|title| format!("title does not contain {title:?}"))
                    .unwrap_or_else(|| "did not match selection filters".to_string());
                skipped_conversations.push(skipped_conversation_report(&parsed.report, reason));
                return Ok(());
            }
            user_total += parsed.report.user_turns;
            assistant_total += parsed.report.assistant_turns;
            skipped_total += parsed.report.skipped_messages;
            rejected_total += parsed.report.rejected_messages;
            if parsed.report.eligible {
                accepted_total += 1;
            }

            if params.mode == ChatgptExportMode::Apply {
                let tx = transaction.expect("apply runs inside an import transaction");
                if !parsed.has_eligible_messages {
                    for rejection in &parsed.rejections {
                        record_rejection_in_transaction(
                            tx,
                            profile.as_str(),
                            &workspace,
                            &conversation.id,
                            &rejection.message_id,
                            &rejection.code,
                            &rejection.reason,
                            &payload_path,
                        )?;
                    }
                    rejections.extend(parsed.rejections);
                    reports.push(parsed.report);
                    return Ok(());
                }
                Store::ensure_workspace_in_transaction(tx, profile.as_str(), &workspace)?;
                inject_chatgpt_apply_failure(&mut staged_writes)?;
                let session_id = format!("chatgpt:{}", conversation.id);
                Store::ensure_session_in_transaction(
                    tx,
                    &session_id,
                    profile.as_str(),
                    &workspace,
                    "chatgpt-export",
                )?;
                inject_chatgpt_apply_failure(&mut staged_writes)?;

                for message in &parsed.accepted {
                    let source_ref = format!(
                        "{}:{}:{}",
                        payload_path, conversation.id, message.message_id
                    );
                    let source_hash = ids::sha256_hex(
                        format!("chatgpt-export:{session_id}:{}", message.message_id).as_bytes(),
                    );
                    let (source, source_created) = Store::upsert_source_in_transaction(
                        tx,
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
                    inject_chatgpt_apply_failure(&mut staged_writes)?;

                    let turn_id = format!(
                        "turn_chatgpt_{}",
                        ids::sha256_hex(format!("{session_id}:{}", message.message_id).as_bytes())
                            .chars()
                            .take(24)
                            .collect::<String>()
                    );
                    Store::insert_visible_turn_in_transaction(
                        tx,
                        &VisibleTurn {
                            id: turn_id.clone(),
                            session_id: session_id.clone(),
                            actor: message.actor.clone(),
                            content: message.content.clone(),
                            created_at: message.created_at.clone(),
                            metadata: message.metadata.clone(),
                        },
                    )?;
                    inject_chatgpt_apply_failure(&mut staged_writes)?;
                    Store::record_evidence_ledger_in_transaction(
                        tx,
                        &EvidenceLedgerEntry {
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
                        },
                    )?;
                    inject_chatgpt_apply_failure(&mut staged_writes)?;
                    created += 1;
                }

                for rejection in &parsed.rejections {
                    record_rejection_in_transaction(
                        tx,
                        profile.as_str(),
                        &workspace,
                        &conversation.id,
                        &rejection.message_id,
                        &rejection.code,
                        &rejection.reason,
                        &payload_path,
                    )?;
                }
            }

            rejections.extend(parsed.rejections);
            reports.push(parsed.report);
            Ok(())
        })?;
        Ok(())
    };
    if params.mode == ChatgptExportMode::Apply {
        service.store.transaction(|tx| {
            for member in &detected.payloads {
                process_member(member, Some(tx))?;
            }
            Ok(())
        })?;
    } else {
        for member in &detected.payloads {
            process_member(member, None)?;
        }
    }

    let mut response = ChatgptExportResponse {
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
        manifest_path: None,
        members,
        conversations: reports,
        skipped_conversations,
        rejections,
    };
    if params.mode == ChatgptExportMode::Apply {
        let manifest_path = import_manifest_path(&service.store.path_display());
        let manifest = json!({
            "manifest_version": 1,
            "source": "chatgpt-export",
            "created_at": ids::now_rfc3339(),
            "payload_path": manifest_payload_path(&response.payload_path),
            "selected_source_ids": response.conversations.iter().map(|conversation| conversation.conversation_id.as_str()).collect::<Vec<_>>(),
            "selected_conversations": response.selected_conversations,
            "created": response.created,
            "skipped_existing": response.skipped_existing,
            "rejected_messages": response.rejected_messages,
        });
        let encoded = serde_json::to_vec_pretty(&manifest)
            .map_err(|err| Error::internal(format!("serialize ChatGPT import manifest: {err}")))?;
        fs::write(&manifest_path, encoded)
            .map_err(|err| Error::storage(format!("write ChatGPT import manifest: {err}")))?;
        response.manifest_path = Some(manifest_path.display().to_string());
    }
    Ok(response)
}

impl ChatgptExportSelection {
    fn has_filter(&self) -> bool {
        !self.conversation_ids.iter().all(|id| id.trim().is_empty())
            || self
                .title_contains
                .as_deref()
                .is_some_and(|title| !title.trim().is_empty())
            || self.since.is_some()
            || self.until.is_some()
            || self.max_conversations.is_some()
            || self.eligible_only
    }
}

fn skipped_conversation_report(
    report: &ChatgptExportConversationReport,
    selection_reason: String,
) -> ChatgptExportSkippedConversationReport {
    ChatgptExportSkippedConversationReport {
        conversation_id: report.conversation_id.clone(),
        title: report.title.clone(),
        created_at: report.created_at.clone(),
        updated_at: report.updated_at.clone(),
        eligible: report.eligible,
        selection_reason,
    }
}

fn import_manifest_path(store_path: &str) -> PathBuf {
    let mut path = PathBuf::from(store_path);
    path.set_extension("chatgpt-import-manifest.json");
    path
}

fn manifest_payload_path(payload_path: &str) -> String {
    Path::new(payload_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("conversations.json")
        .to_string()
}

fn matches_selection(
    conversation_id: &str,
    title: &str,
    updated_at: Option<&str>,
    eligible: bool,
    conversation_ids: &std::collections::BTreeSet<&str>,
    title_contains: Option<&str>,
    since: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
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
    let updated_at = updated_at.and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok());
    if let Some(since) = since {
        if updated_at.is_none_or(|updated_at| updated_at < since) {
            return false;
        }
    }
    if let Some(until) = until {
        if updated_at.is_none_or(|updated_at| updated_at >= until) {
            return false;
        }
    }
    if eligible_only && !eligible {
        return false;
    }
    true
}

fn parse_filter_timestamp(value: &str) -> Result<OffsetDateTime> {
    let value = value.trim();
    let value = if value.len() == 10 {
        format!("{value}T00:00:00Z")
    } else {
        value.to_string()
    };
    OffsetDateTime::parse(&value, &Rfc3339)
        .map_err(|_| Error::invalid_request(format!("invalid date filter: {value}")))
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
            selection_reason: "matched selection filters".to_string(),
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
    payloads: Vec<PayloadMember>,
}

enum PayloadMember {
    Directory { path: PathBuf, name: String },
    Zip { archive: PathBuf, name: String },
}

impl PayloadMember {
    fn name(&self) -> &str {
        match self {
            Self::Directory { name, .. } | Self::Zip { name, .. } => name,
        }
    }
}

fn detect_payload(path: &Path) -> Result<DetectedPayload> {
    if path.is_dir() {
        let payloads = find_conversations_files(path)?;
        if payloads.is_empty() {
            return Err(Error::invalid_request(
                "unsupported ChatGPT export schema: conversations payload not found",
            ));
        }
        let mut payloads = payloads
            .into_iter()
            .map(|payload| {
                let name = payload
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("conversations.json")
                    .to_string();
                let size = fs::metadata(&payload)
                    .map_err(|err| Error::invalid_request(format!("failed to inspect conversations payload: {err}")))?
                    .len();
                if size > MAX_CONVERSATIONS_MEMBER_BYTES {
                    return Err(Error::invalid_request(format!("conversations member exceeds {MAX_CONVERSATIONS_MEMBER_BYTES} bytes: {name}")));
                }
                Ok(PayloadMember::Directory { path: payload, name })
            })
            .collect::<Result<Vec<_>>>()?;
        validate_payload_names(&mut payloads)?;
        let payload_path = payloads
            .first()
            .map(|payload| payload.name().to_string())
            .expect("non-empty conversations payloads");
        return Ok(DetectedPayload {
            payload_path,
            payloads,
        });
    }

    let file = fs::File::open(path)
        .map_err(|err| Error::invalid_request(format!("failed to open ChatGPT export: {err}")))?;
    let mut archive = ZipArchive::new(file).map_err(|_| {
        Error::invalid_request(
            "unsupported ChatGPT export schema: expected a zip archive or extracted directory",
        )
    })?;
    let mut payloads = Vec::new();
    for idx in 0..archive.len() {
        let entry = archive
            .by_index(idx)
            .map_err(|_| Error::invalid_request("failed to read zip entry from ChatGPT export"))?;
        let name = entry.name().to_string();
        validate_zip_member_path(&name)?;
        if entry.size() > MAX_CONVERSATIONS_MEMBER_BYTES {
            return Err(Error::invalid_request(format!(
                "archive member exceeds {MAX_CONVERSATIONS_MEMBER_BYTES} bytes: {name}"
            )));
        }
        if Path::new(&name)
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| conversation_member_name(value).is_some())
        {
            payloads.push(PayloadMember::Zip {
                archive: path.to_path_buf(),
                name,
            });
        }
    }
    validate_payload_names(&mut payloads)?;
    let payload_path = payloads
        .first()
        .map(|member| member.name().to_string())
        .ok_or_else(|| {
            Error::invalid_request(
                "unsupported ChatGPT export schema: conversations payload not found",
            )
        })?;
    Ok(DetectedPayload {
        payload_path,
        payloads,
    })
}

fn find_conversations_files(root: &Path) -> Result<Vec<PathBuf>> {
    let root = root.canonicalize().map_err(|err| {
        Error::invalid_request(format!("failed to resolve export directory: {err}"))
    })?;
    let mut stack = vec![root];
    let mut payloads = Vec::new();
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).map_err(|err| {
            Error::invalid_request(format!("failed to read export directory: {err}"))
        })? {
            let entry = entry.map_err(|err| {
                Error::invalid_request(format!("failed to read export directory entry: {err}"))
            })?;
            let entry_path = entry.path();
            let file_type = entry.file_type().map_err(|err| {
                Error::invalid_request(format!("failed to inspect export directory entry: {err}"))
            })?;
            if file_type.is_symlink() {
                return Err(Error::invalid_request(format!(
                    "unsafe symlink in ChatGPT export directory: {}",
                    entry.file_name().to_string_lossy()
                )));
            }
            if entry_path.is_dir() {
                stack.push(entry_path);
                continue;
            }
            if entry_path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| conversation_member_name(value).is_some())
            {
                payloads.push(entry_path);
            }
        }
    }
    payloads.sort_by(|left, right| {
        conversation_member_name(
            left.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        )
        .cmp(&conversation_member_name(
            right
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        ))
    });
    Ok(payloads)
}

fn conversation_member_name(name: &str) -> Option<Option<u32>> {
    if name == "conversations.json" {
        return Some(None);
    }
    let number = name.strip_prefix("conversations-")?.strip_suffix(".json")?;
    (number.len() == 3 && number.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| Some(number.parse().expect("three digit shard number")))
}

fn validate_payload_names(payloads: &mut Vec<PayloadMember>) -> Result<()> {
    if payloads.len() > MAX_CONVERSATION_MEMBERS {
        return Err(Error::invalid_request(format!(
            "ChatGPT export has more than {MAX_CONVERSATION_MEMBERS} conversation members"
        )));
    }
    let mut legacy = 0usize;
    let mut numbered = Vec::new();
    for member in payloads.iter() {
        match conversation_member_name(
            Path::new(member.name())
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default(),
        ) {
            Some(None) => legacy += 1,
            Some(Some(number)) => numbered.push(number),
            None => unreachable!("only conversation members are collected"),
        }
    }
    if legacy > 0 && !numbered.is_empty() {
        return Err(Error::invalid_request("ambiguous ChatGPT export: found both conversations.json and numbered conversations shards"));
    }
    if legacy > 1 {
        return Err(Error::invalid_request(
            "ambiguous ChatGPT export: duplicate conversations.json members",
        ));
    }
    numbered.sort_unstable();
    if numbered.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::invalid_request(
            "invalid ChatGPT export: duplicate numbered conversations shard",
        ));
    }
    if numbered
        .iter()
        .enumerate()
        .any(|(index, number)| *number as usize != index)
    {
        return Err(Error::invalid_request("incomplete ChatGPT export: numbered conversations shards must start at 000 and be contiguous"));
    }
    payloads.sort_by(|left, right| {
        conversation_member_name(
            Path::new(left.name())
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default(),
        )
        .cmp(&conversation_member_name(
            Path::new(right.name())
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default(),
        ))
    });
    Ok(())
}

fn validate_zip_member_path(name: &str) -> Result<()> {
    if name.contains('\\')
        || Path::new(name).is_absolute()
        || name.split('/').any(|part| part == "..")
    {
        return Err(Error::invalid_request(format!(
            "unsafe zip member path: {name}"
        )));
    }
    Ok(())
}

fn stream_conversations(
    member: &PayloadMember,
    on_conversation: impl FnMut(ExportConversation) -> Result<()>,
) -> Result<()> {
    match member {
        PayloadMember::Directory { path, name } => {
            let file = fs::File::open(path).map_err(|err| {
                Error::invalid_request(format!(
                    "failed to read conversations payload {name}: {err}"
                ))
            })?;
            stream_conversation_array(file, on_conversation, name)
        }
        PayloadMember::Zip { archive, name } => {
            let file = fs::File::open(archive).map_err(|err| {
                Error::invalid_request(format!("failed to open ChatGPT export: {err}"))
            })?;
            let mut archive = ZipArchive::new(file).map_err(|_| Error::invalid_request("unsupported ChatGPT export schema: expected a zip archive or extracted directory"))?;
            let entry = archive.by_name(name).map_err(|_| {
                Error::invalid_request(format!("failed to read conversations member: {name}"))
            })?;
            stream_conversation_array(entry, on_conversation, name)
        }
    }
}

fn stream_conversation_array<R: Read>(
    reader: R,
    mut on_conversation: impl FnMut(ExportConversation) -> Result<()>,
    name: &str,
) -> Result<()> {
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    ConversationSequenceSeed {
        on_conversation: &mut on_conversation,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| {
        Error::invalid_request(format!(
            "unsupported ChatGPT export schema: invalid conversations payload: {name}"
        ))
    })?;
    deserializer.end().map_err(|_| {
        Error::invalid_request(format!(
            "unsupported ChatGPT export schema: invalid conversations payload: {name}"
        ))
    })
}

struct ConversationSequenceSeed<'a, F> {
    on_conversation: &'a mut F,
}

impl<'de, F> DeserializeSeed<'de> for ConversationSequenceSeed<'_, F>
where
    F: FnMut(ExportConversation) -> Result<()>,
{
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ConversationSequenceVisitor {
            on_conversation: self.on_conversation,
        })
    }
}

struct ConversationSequenceVisitor<'a, F> {
    on_conversation: &'a mut F,
}

impl<'de, F> Visitor<'de> for ConversationSequenceVisitor<'_, F>
where
    F: FnMut(ExportConversation) -> Result<()>,
{
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of ChatGPT conversations")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(conversation) = sequence.next_element::<ExportConversation>()? {
            (self.on_conversation)(conversation).map_err(A::Error::custom)?;
        }
        Ok(())
    }
}

fn inject_chatgpt_apply_failure(staged_writes: &mut usize) -> Result<()> {
    *staged_writes += 1;
    #[cfg(debug_assertions)]
    if std::env::var("CODEX_MEMORYD_TEST_FAIL_CHATGPT_AFTER_WRITES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|target| *staged_writes >= target)
    {
        return Err(Error::storage("injected ChatGPT import write failure"));
    }
    Ok(())
}

fn record_rejection_in_transaction(
    tx: &rusqlite::Transaction<'_>,
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
    Store::record_evidence_ledger_in_transaction(
        tx,
        &EvidenceLedgerEntry {
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
        },
    )?;
    Store::record_policy_event_in_transaction(
        tx,
        Some(profile),
        Some(workspace),
        "rejected_turn",
        code,
        reason,
        "chatgpt-export",
    )?;
    Ok(())
}
