use std::io::BufRead;
use std::io::Write;

use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

use crate::domain;
use crate::error;
use crate::error::Result;
use crate::protocol::CheckpointRequest;
use crate::protocol::ConclusionsRequest;
use crate::protocol::RecallRequest;
use crate::protocol::SearchRequest;
use crate::protocol::SyncFile;
use crate::protocol::SyncRequest;
use crate::service::Service;
use crate::PROVIDER_NAME;
use crate::PROVIDER_VERSION;

const JSONRPC_VERSION: &str = "2.0";
const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
const CODEX_LEGACY_PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    MCP_PROTOCOL_VERSION,
    LEGACY_PROTOCOL_VERSION,
    CODEX_LEGACY_PROTOCOL_VERSION,
];
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const PROTOCOL_VERSION_META: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_INFO_META: &str = "io.modelcontextprotocol/clientInfo";
const CLIENT_CAPABILITIES_META: &str = "io.modelcontextprotocol/clientCapabilities";
const TOOL_TEXT_TYPE: &str = "text";
/// Read-only MCP tools, exposed by default. Public so diagnostics and contract
/// tests reference the same source of truth as the dispatcher.
pub const READ_ONLY_TOOL_NAMES: &[&str] = &["memory_status", "memory_recall", "memory_search"];
/// Write-capable MCP tools, exposed only with `--write_tools`.
pub const WRITE_TOOL_NAMES: &[&str] = &[
    "memory_create",
    "memory_conclude",
    "memory_checkpoint",
    "memory_import_preview",
    "memory_import_apply",
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default, rename = "_meta")]
    _meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct StatusArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct RecallArgs {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    query: String,
    #[serde(default)]
    max_tokens: Option<usize>,
    #[serde(default)]
    pack_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct SearchArgs {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    query: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(rename = "type", default)]
    record_type: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    include_archived: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ConcludeArgs {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    content: String,
    #[serde(rename = "type", default)]
    record_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct CheckpointArgs {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    summary: String,
    #[serde(default)]
    changed_files: Vec<String>,
    #[serde(default)]
    decisions: Vec<String>,
    #[serde(default)]
    blockers: Vec<String>,
    #[serde(default)]
    next_steps: Vec<String>,
    #[serde(default)]
    tests_run: Vec<String>,
    #[serde(default)]
    tests_not_run: Vec<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ImportArgs {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    source_root: String,
    files: Vec<SyncFile>,
}

struct ServerState {
    connection_mode: ConnectionMode,
    legacy_phase: LegacyPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionMode {
    Undetermined,
    Modern,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyPhase {
    NotInitialized,
    AwaitingInitialized,
    Ready,
}

impl ServerState {
    fn new() -> Self {
        Self {
            connection_mode: ConnectionMode::Undetermined,
            legacy_phase: LegacyPhase::NotInitialized,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDefinition {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
    annotations: ToolAnnotations,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolAnnotations {
    read_only_hint: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    destructive_hint: Option<bool>,
}

pub fn run_stdio(service: Service, write_tools: bool) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut state = ServerState::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        let oversized = match read_bounded_line(&mut reader, &mut line)
            .map_err(|err| error::Error::internal(format!("failed to read MCP stdin: {err}")))?
        {
            Some(oversized) => oversized,
            None => break,
        };
        if oversized {
            let text = serde_json::to_string(&parse_error(
                Value::Null,
                "MCP message exceeds the 1048576-byte stdio limit",
            ))
            .map_err(|err| error::Error::internal(format!("failed to encode MCP error: {err}")))?;
            writeln!(writer, "{text}")
                .and_then(|_| writer.flush())
                .map_err(|err| {
                    error::Error::internal(format!("failed to write MCP response: {err}"))
                })?;
            continue;
        }

        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let raw = match std::str::from_utf8(&line) {
            Ok(raw) => raw,
            Err(_) => {
                let text = serde_json::to_string(&parse_error(
                    Value::Null,
                    "MCP messages must be valid UTF-8 JSON",
                ))
                .map_err(|err| {
                    error::Error::internal(format!("failed to encode MCP error: {err}"))
                })?;
                writeln!(writer, "{text}")
                    .and_then(|_| writer.flush())
                    .map_err(|err| {
                        error::Error::internal(format!("failed to write MCP response: {err}"))
                    })?;
                continue;
            }
        };

        if raw.trim().is_empty() {
            continue;
        }

        let response = handle_message(&service, &mut state, raw, write_tools);
        if let Some(response) = response {
            let text = serde_json::to_string(&response).map_err(|err| {
                error::Error::internal(format!("failed to encode MCP response: {err}"))
            })?;
            writeln!(writer, "{text}")
                .and_then(|_| writer.flush())
                .map_err(|err| {
                    error::Error::internal(format!("failed to write MCP response: {err}"))
                })?;
        }
    }

    Ok(())
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
) -> std::io::Result<Option<bool>> {
    let mut oversized = false;
    let mut read_any = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if read_any { Some(oversized) } else { None });
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        read_any = true;
        if !oversized {
            if line.len().saturating_add(consumed) > MAX_MESSAGE_BYTES {
                line.clear();
                oversized = true;
            } else {
                line.extend_from_slice(&available[..consumed]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(oversized));
        }
    }
}

fn handle_message(
    service: &Service,
    state: &mut ServerState,
    raw: &str,
    write_tools: bool,
) -> Option<RpcResponse> {
    let message = match serde_json::from_str::<Value>(raw) {
        Ok(message) => message,
        Err(_) => return Some(parse_error(Value::Null, "invalid JSON request")),
    };
    let Some(object) = message.as_object() else {
        return Some(invalid_request(
            Value::Null,
            "JSON-RPC message must be an object",
        ));
    };

    let id = object.get("id");
    let response_id = id
        .filter(|id| valid_request_id(id))
        .cloned()
        .unwrap_or(Value::Null);
    if object.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return Some(invalid_request(response_id, "jsonrpc must be '2.0'"));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(invalid_request(
            response_id,
            "request method must be a string",
        ));
    };
    if id.is_some_and(|id| !valid_request_id(id)) {
        return Some(invalid_request(
            Value::Null,
            "request id must be a string or integer",
        ));
    }
    let is_notification = id.is_none();
    let params = object.get("params").cloned();
    if params.as_ref().is_some_and(|params| !params.is_object()) {
        return if is_notification {
            None
        } else {
            Some(invalid_params(
                response_id,
                "request params must be an object",
            ))
        };
    }

    if is_notification {
        if method == "notifications/initialized"
            && state.legacy_phase == LegacyPhase::AwaitingInitialized
        {
            state.legacy_phase = LegacyPhase::Ready;
        }
        return None;
    }

    if method == "initialize" {
        return Some(initialize(state, response_id, params));
    }

    let request_meta = params
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|params| params.get("_meta"))
        .and_then(Value::as_object);
    if let Some(version) = request_meta
        .and_then(|meta| meta.get(PROTOCOL_VERSION_META))
        .and_then(Value::as_str)
    {
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
            return Some(unsupported_protocol_version(response_id, version));
        }
        if version == MCP_PROTOCOL_VERSION {
            let missing = [CLIENT_INFO_META, CLIENT_CAPABILITIES_META]
                .into_iter()
                .find(|key| {
                    request_meta.is_none_or(|meta| !meta.get(*key).is_some_and(Value::is_object))
                });
            if let Some(missing) = missing {
                return Some(invalid_params(
                    response_id,
                    format!("missing required MCP request metadata '{missing}'"),
                ));
            }
            if let Some(client_info) = request_meta.and_then(|meta| meta.get(CLIENT_INFO_META)) {
                if !valid_client_info(client_info) {
                    return Some(invalid_params(
                        response_id,
                        format!("invalid MCP client identity metadata '{CLIENT_INFO_META}'"),
                    ));
                }
            }
            if state.connection_mode == ConnectionMode::Legacy {
                return Some(invalid_request(
                    response_id,
                    "cannot mix modern and legacy MCP lifecycle on one stdio connection",
                ));
            }
            state.connection_mode = ConnectionMode::Modern;
            if method == "server/discover" {
                return Some(ok_versioned(
                    response_id,
                    json!({
                        "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS,
                        "capabilities": { "tools": {} },
                        "ttlMs": 0,
                        "cacheScope": "private",
                        "_meta": {
                            "io.modelcontextprotocol/serverInfo": {
                                "name": PROVIDER_NAME,
                                "version": PROVIDER_VERSION,
                            }
                        }
                    }),
                    true,
                ));
            }
            return Some(handle_request(
                service,
                response_id,
                method,
                params,
                write_tools,
                true,
            ));
        }
    }
    if method == "server/discover" {
        return Some(invalid_params(
            response_id,
            format!("server/discover requires '{PROTOCOL_VERSION_META}' metadata"),
        ));
    }

    if state.connection_mode == ConnectionMode::Modern {
        return Some(invalid_params(
            response_id,
            format!("modern MCP requests require '{PROTOCOL_VERSION_META}' metadata"),
        ));
    }
    if state.legacy_phase != LegacyPhase::Ready {
        return Some(server_error(
            response_id,
            -32002,
            "MCP session is not initialized",
            Some(json!({ "code": "server_not_initialized" })),
        ));
    }
    Some(handle_request(
        service,
        response_id,
        method,
        params,
        write_tools,
        false,
    ))
}

fn initialize(state: &mut ServerState, id: Value, params: Option<Value>) -> RpcResponse {
    if state.connection_mode == ConnectionMode::Modern {
        return invalid_request(
            id,
            "cannot mix modern and legacy MCP lifecycle on one stdio connection",
        );
    }
    if state.legacy_phase != LegacyPhase::NotInitialized {
        return invalid_request(id, "MCP initialize may only be sent once");
    }
    let Some(params) = params.and_then(|params| params.as_object().cloned()) else {
        return invalid_params(id, "initialize params must be an object");
    };
    let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
        return invalid_params(id, "initialize requires a protocolVersion string");
    };
    if !params.get("clientInfo").is_some_and(valid_client_info)
        || !params.get("capabilities").is_some_and(Value::is_object)
    {
        return invalid_params(
            id,
            "initialize requires clientInfo and capabilities objects",
        );
    }
    if requested == MCP_PROTOCOL_VERSION {
        return server_error(
            id,
            -32601,
            "initialize is not part of MCP 2026-07-28; use server/discover and per-request metadata",
            Some(json!({ "supported": SUPPORTED_PROTOCOL_VERSIONS })),
        );
    }
    let negotiated = match requested {
        LEGACY_PROTOCOL_VERSION | CODEX_LEGACY_PROTOCOL_VERSION => requested,
        _ => LEGACY_PROTOCOL_VERSION,
    };
    state.connection_mode = ConnectionMode::Legacy;
    state.legacy_phase = LegacyPhase::AwaitingInitialized;
    ok(
        id,
        json!({
            "protocolVersion": negotiated,
            "serverInfo": {
                "name": PROVIDER_NAME,
                "version": PROVIDER_VERSION,
            },
            "capabilities": {
                "tools": {
                    "listChanged": false,
                }
            }
        }),
    )
}

fn valid_client_info(value: &Value) -> bool {
    value.as_object().is_some_and(|info| {
        info.get("name").is_some_and(Value::is_string)
            && info.get("version").is_some_and(Value::is_string)
    })
}

fn handle_request(
    service: &Service,
    id: Value,
    method: &str,
    params: Option<Value>,
    write_tools: bool,
    modern: bool,
) -> RpcResponse {
    match method {
        "tools/list" => {
            let mut result = json!({ "tools": tool_definitions(write_tools) });
            if modern {
                result["ttlMs"] = json!(0);
                result["cacheScope"] = json!("private");
            }
            ok_versioned(id, result, modern)
        }
        "tools/call" => {
            let params = params.and_then(|mut params| {
                params.as_object_mut()?.remove("_meta");
                serde_json::from_value::<ToolCallParams>(params).ok()
            });
            match params {
                Some(params) => handle_tool_call(service, id, params, write_tools, modern),
                None => invalid_params(id, "invalid tools/call params"),
            }
        }
        _ => method_not_found(id, method),
    }
}

fn valid_request_id(id: &Value) -> bool {
    id.as_str().is_some() || id.as_i64().is_some() || id.as_u64().is_some()
}

fn unsupported_protocol_version(id: Value, requested: &str) -> RpcResponse {
    server_error(
        id,
        -32022,
        "Unsupported protocol version",
        Some(json!({
            "supported": SUPPORTED_PROTOCOL_VERSIONS,
            "requested": requested,
        })),
    )
}

fn invalid_request(id: Value, message: impl Into<String>) -> RpcResponse {
    server_error(id, -32600, message, None)
}

fn handle_tool_call(
    service: &Service,
    id: Value,
    params: ToolCallParams,
    write_tools: bool,
    modern: bool,
) -> RpcResponse {
    if !write_tools && !READ_ONLY_TOOL_NAMES.contains(&params.name.as_str()) {
        return read_only_tool_disabled(id, params.name);
    }
    if write_tools
        && !READ_ONLY_TOOL_NAMES.contains(&params.name.as_str())
        && !WRITE_TOOL_NAMES.contains(&params.name.as_str())
    {
        return method_not_found(id, params.name);
    }

    match params.name.as_str() {
        "memory_status" => {
            if let Err(err) = parse_tool_args::<StatusArgs>(params.arguments) {
                return invalid_params(id, err);
            }
            match service.status() {
                Ok(status) => ok_tool_result(id, json!(status), modern),
                Err(err) => service_error(id, err, modern),
            }
        }
        "memory_recall" => match parse_tool_args::<RecallArgs>(params.arguments) {
            Ok(args) => {
                let repo = args.repo_id.map(|repo_id| domain::RepoIdentity {
                    repo_id,
                    ..Default::default()
                });
                let req = RecallRequest {
                    profile: args.profile,
                    workspace: args.workspace,
                    repo,
                    session: None,
                    query: Some(args.query),
                    files: vec![],
                    max_tokens: args.max_tokens,
                    pack_mode: args.pack_mode,
                    include_types: vec![],
                    exclude_types: vec![],
                    recency_days: None,
                    as_of: None,
                    include_history: false,
                    metadata: None,
                };
                match service.recall(req) {
                    Ok(resp) => ok_tool_result(id, json!(resp), modern),
                    Err(err) => service_error(id, err, modern),
                }
            }
            Err(err) => invalid_params(id, err),
        },
        "memory_search" => match parse_tool_args::<SearchArgs>(params.arguments) {
            Ok(args) => {
                let repo = args.repo_id.map(|repo_id| domain::RepoIdentity {
                    repo_id,
                    ..Default::default()
                });
                let req = SearchRequest {
                    profile: args.profile,
                    workspace: args.workspace,
                    repo,
                    query: Some(args.query),
                    scope: args.scope,
                    record_type: args.record_type,
                    limit: args.limit,
                    include_archived: args.include_archived,
                    cursor: None,
                };
                match service.search(req) {
                    Ok(resp) => ok_tool_result(id, json!(resp), modern),
                    Err(err) => service_error(id, err, modern),
                }
            }
            Err(err) => invalid_params(id, err),
        },
        "memory_create" | "memory_conclude" => {
            match parse_tool_args::<ConcludeArgs>(params.arguments) {
                Ok(args) => {
                    let repo = args.repo_id.map(|repo_id| domain::RepoIdentity {
                        repo_id,
                        ..Default::default()
                    });
                    let req = ConclusionsRequest {
                        profile: args.profile,
                        workspace: args.workspace,
                        repo,
                        target: Some("user".to_string()),
                        conclusions: Some(vec![args.content]),
                        metadata: None,
                        record_type: args.record_type,
                    };
                    match service.conclusions(req) {
                        Ok(resp) => ok_tool_result(id, json!(resp), modern),
                        Err(err) => service_error(id, err, modern),
                    }
                }
                Err(err) => invalid_params(id, err),
            }
        }
        "memory_checkpoint" => match parse_tool_args::<CheckpointArgs>(params.arguments) {
            Ok(args) => {
                let repo = args.repo_id.map(|repo_id| domain::RepoIdentity {
                    repo_id,
                    ..Default::default()
                });
                let req = CheckpointRequest {
                    profile: args.profile,
                    workspace: args.workspace,
                    repo,
                    session: match (args.session_id, args.thread_id) {
                        (None, None) => None,
                        (session_id, thread_id) => Some(crate::protocol::TurnSession {
                            id: session_id,
                            thread_id,
                            source: None,
                            metadata: None,
                        }),
                    },
                    summary: Some(args.summary),
                    changed_files: args.changed_files,
                    decisions: args.decisions,
                    blockers: args.blockers,
                    next_steps: args.next_steps,
                    tests_run: args.tests_run,
                    tests_not_run: args.tests_not_run,
                    branch: args.branch,
                    commit: args.commit,
                };
                match service.checkpoint(req) {
                    Ok(resp) => ok_tool_result(id, json!(resp), modern),
                    Err(err) => service_error(id, err, modern),
                }
            }
            Err(err) => invalid_params(id, err),
        },
        "memory_import_preview" => {
            handle_import_tool(service, id, params.arguments, "preview", modern)
        }
        "memory_import_apply" => handle_import_tool(service, id, params.arguments, "apply", modern),
        _ => method_not_found(id, params.name),
    }
}

fn handle_import_tool(
    service: &Service,
    id: Value,
    arguments: Option<Value>,
    mode: &'static str,
    modern: bool,
) -> RpcResponse {
    match parse_tool_args::<ImportArgs>(arguments) {
        Ok(args) => {
            let repo = args.repo_id.map(|repo_id| domain::RepoIdentity {
                repo_id,
                ..Default::default()
            });
            let req = SyncRequest {
                profile: args.profile,
                workspace: args.workspace,
                repo,
                source_root: Some(args.source_root),
                mode: Some(mode.to_string()),
                files: Some(args.files),
                metadata: None,
            };
            match service.sync_local(req) {
                Ok(resp) => ok_tool_result(id, json!(resp), modern),
                Err(err) => service_error(id, err, modern),
            }
        }
        Err(err) => invalid_params(id, err),
    }
}

fn parse_tool_args<T: for<'de> Deserialize<'de>>(
    arguments: Option<Value>,
) -> std::result::Result<T, String> {
    let value = arguments.unwrap_or_else(|| json!({}));
    serde_json::from_value(value).map_err(|err| err.to_string())
}

fn tool_definitions(write_tools: bool) -> Vec<ToolDefinition> {
    let mut tools = vec![
        ToolDefinition {
            name: "memory_status",
            description: "Probe provider health and status.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
            annotations: read_annotations(),
        },
        ToolDefinition {
            name: "memory_recall",
            description: "Recall task-relevant memory for a profile and workspace. Results are recall-not-authority and include evidence/provenance metadata where available.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string" },
                    "workspace": { "type": "string" },
                    "repoId": { "type": "string" },
                    "query": { "type": "string" },
                    "maxTokens": { "type": "integer", "minimum": 1 },
                    "packMode": { "type": "string", "enum": ["default", "debugging", "onboarding", "planning", "active_task", "review", "personal_context"] }
                },
                "required": ["query"],
                "additionalProperties": false,
            }),
            annotations: read_annotations(),
        },
        ToolDefinition {
            name: "memory_search",
            description: "Search safe memory records with existing privacy filters. Results are recall-not-authority and include record provenance.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string" },
                    "workspace": { "type": "string" },
                    "repoId": { "type": "string" },
                    "query": { "type": "string" },
                    "scope": { "type": "string" },
                    "type": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1 },
                    "includeArchived": { "type": "boolean" }
                },
                "required": ["query"],
                "additionalProperties": false,
            }),
            annotations: read_annotations(),
        },
    ];

    if write_tools {
        tools.extend([
            ToolDefinition {
                name: "memory_create",
                description: "Write a durable memory conclusion through the existing policy and provenance gates.",
                input_schema: conclusion_schema(),
                annotations: write_annotations(None),
            },
            ToolDefinition {
                name: "memory_conclude",
                description: "Write a durable conclusion using the existing write policy.",
                input_schema: conclusion_schema(),
                annotations: write_annotations(None),
            },
            ToolDefinition {
                name: "memory_checkpoint",
                description: "Write checkpoint-backed task state using the existing write policy.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "profile": { "type": "string" },
                        "workspace": { "type": "string" },
                        "repoId": { "type": "string" },
                        "summary": { "type": "string" },
                        "changedFiles": { "type": "array", "items": { "type": "string" } },
                        "decisions": { "type": "array", "items": { "type": "string" } },
                        "blockers": { "type": "array", "items": { "type": "string" } },
                        "nextSteps": { "type": "array", "items": { "type": "string" } },
                        "testsRun": { "type": "array", "items": { "type": "string" } },
                        "testsNotRun": { "type": "array", "items": { "type": "string" } },
                        "branch": { "type": "string" },
                        "commit": { "type": "string" },
                        "sessionId": { "type": "string" },
                        "threadId": { "type": "string" }
                    },
                    "required": ["summary"],
                    "additionalProperties": false,
                }),
                annotations: write_annotations(None),
            },
            ToolDefinition {
                name: "memory_import_preview",
                description: "Preview a local Codex memory import. This never writes durable records.",
                input_schema: import_schema(),
                annotations: write_annotations(Some(false)),
            },
            ToolDefinition {
                name: "memory_import_apply",
                description: "Apply a local Codex memory import through the existing sync policy and idempotency gates.",
                input_schema: import_schema(),
                annotations: write_annotations(Some(true)),
            },
        ]);
    }

    tools
}

fn read_annotations() -> ToolAnnotations {
    ToolAnnotations {
        read_only_hint: true,
        destructive_hint: None,
    }
}

fn write_annotations(destructive_hint: Option<bool>) -> ToolAnnotations {
    ToolAnnotations {
        read_only_hint: false,
        destructive_hint,
    }
}

fn conclusion_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "profile": { "type": "string" },
            "workspace": { "type": "string" },
            "repoId": { "type": "string" },
            "content": { "type": "string" },
            "type": { "type": "string" }
        },
        "required": ["content"],
        "additionalProperties": false,
    })
}

fn import_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "profile": { "type": "string" },
            "workspace": { "type": "string" },
            "repoId": { "type": "string" },
            "sourceRoot": { "type": "string" },
            "files": {
                "type": "array",
                "items": { "$ref": "#/$defs/syncFile" }
            }
        },
        "required": ["sourceRoot", "files"],
        "additionalProperties": false,
        "$defs": {
            "syncFile": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "kind": { "type": "string" },
                    "content": { "type": "string" },
                    "hash": { "type": "string" },
                    "modifiedAt": { "type": "string" },
                    "idempotencyKey": { "type": "string" },
                    "metadata": { "type": "object" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }
        }
    })
}

fn ok(id: Value, result: Value) -> RpcResponse {
    RpcResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: Some(result),
        error: None,
    }
}

fn ok_versioned(id: Value, mut result: Value, modern: bool) -> RpcResponse {
    if modern {
        if let Some(object) = result.as_object_mut() {
            object.insert(
                "resultType".to_string(),
                Value::String("complete".to_string()),
            );
        }
    }
    ok(id, result)
}

fn ok_tool_result(id: Value, structured: Value, modern: bool) -> RpcResponse {
    let content = serde_json::to_string(&structured).unwrap_or_else(|_| "{}".to_string());
    ok_versioned(
        id,
        json!({
            "content": [
                {
                    "type": TOOL_TEXT_TYPE,
                    "text": content
                }
            ],
            "structuredContent": structured,
            "isError": false,
        }),
        modern,
    )
}

fn parse_error(id: Value, message: impl Into<String>) -> RpcResponse {
    RpcResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: None,
        error: Some(RpcError {
            code: -32700,
            message: message.into(),
            data: None,
        }),
    }
}

fn invalid_params(id: Value, message: impl Into<String>) -> RpcResponse {
    RpcResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: None,
        error: Some(RpcError {
            code: -32602,
            message: message.into(),
            data: None,
        }),
    }
}

fn method_not_found(id: Value, method: impl Into<String>) -> RpcResponse {
    RpcResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: None,
        error: Some(RpcError {
            code: -32601,
            message: format!("unknown method '{}'", method.into()),
            data: None,
        }),
    }
}

fn read_only_tool_disabled(id: Value, tool: impl Into<String>) -> RpcResponse {
    RpcResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: None,
        error: Some(RpcError {
            code: -32601,
            message: format!("tool '{}' is disabled in read-only mode", tool.into()),
            data: None,
        }),
    }
}

fn server_error(
    id: Value,
    code: i32,
    message: impl Into<String>,
    data: Option<Value>,
) -> RpcResponse {
    RpcResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: None,
        error: Some(RpcError {
            code,
            message: message.into(),
            data,
        }),
    }
}

fn service_error(id: Value, err: error::Error, modern: bool) -> RpcResponse {
    let message = format!("[{}] {}", err.code, err.message);
    let structured = json!({
        "error": {
            "code": err.code.as_str(),
            "message": err.message,
        }
    });
    ok_versioned(
        id,
        json!({
            "content": [{ "type": TOOL_TEXT_TYPE, "text": message }],
            "structuredContent": structured,
            "isError": true,
        }),
        modern,
    )
}
