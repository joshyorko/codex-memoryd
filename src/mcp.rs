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
use crate::service::Service;
use crate::PROVIDER_NAME;
use crate::PROVIDER_VERSION;

const JSONRPC_VERSION: &str = "2.0";
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const TOOL_TEXT_TYPE: &str = "text";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

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

struct ServerState {
    initialized: bool,
}

impl ServerState {
    fn new() -> Self {
        Self { initialized: false }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDefinition {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
}

pub fn run_stdio(service: Service) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut state = ServerState::new();
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = handle_message(&service, &mut state, trimmed);
        if let Some(response) = response {
            let text = serde_json::to_string(&response)?;
            writeln!(writer, "{text}")?;
            writer.flush()?;
        }
    }

    Ok(())
}

fn handle_message(service: &Service, state: &mut ServerState, raw: &str) -> Option<RpcResponse> {
    let parsed = match serde_json::from_str::<RpcRequest>(raw) {
        Ok(request) => request,
        Err(_) => return Some(parse_error(Value::Null, "invalid JSON request")),
    };
    let id = parsed.id.unwrap_or(Value::Null);
    if parsed
        .jsonrpc
        .as_deref()
        .is_some_and(|version| version != JSONRPC_VERSION)
    {
        return Some(invalid_params(id, "unsupported JSON-RPC version"));
    }

    match parsed.method.as_str() {
        "initialize" => {
            state.initialized = true;
            Some(ok(
                id,
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
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
            ))
        }
        "initialized" => None,
        "tools/list" => Some(match ensure_initialized(state, id.clone()) {
            Some(error) => error,
            None => ok(id, json!({ "tools": tool_definitions() })),
        }),
        "tools/call" => Some(match ensure_initialized(state, id.clone()) {
            Some(error) => error,
            None => match parsed
                .params
                .and_then(|params| serde_json::from_value::<ToolCallParams>(params).ok())
            {
                Some(params) => handle_tool_call(service, id, params),
                None => invalid_params(id, "invalid tools/call params"),
            },
        }),
        _ => Some(method_not_found(id, parsed.method)),
    }
}

fn ensure_initialized(state: &ServerState, id: Value) -> Option<RpcResponse> {
    if state.initialized {
        None
    } else {
        Some(server_error(
            id,
            -32002,
            "MCP session is not initialized",
            Some(json!({ "code": "server_not_initialized" })),
        ))
    }
}

fn handle_tool_call(service: &Service, id: Value, params: ToolCallParams) -> RpcResponse {
    match params.name.as_str() {
        "memory_status" => {
            let args = parse_tool_args::<StatusArgs>(params.arguments).unwrap_or(StatusArgs {});
            let _ = args;
            match service.status() {
                Ok(status) => ok_tool_result(id, json!(status)),
                Err(err) => service_error(id, err),
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
                    include_types: vec![],
                    exclude_types: vec![],
                    recency_days: None,
                    metadata: None,
                };
                match service.recall(req) {
                    Ok(resp) => ok_tool_result(id, json!(resp)),
                    Err(err) => service_error(id, err),
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
                    Ok(resp) => ok_tool_result(id, json!(resp)),
                    Err(err) => service_error(id, err),
                }
            }
            Err(err) => invalid_params(id, err),
        },
        "memory_conclude" => match parse_tool_args::<ConcludeArgs>(params.arguments) {
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
                    record_type: None,
                };
                match service.conclusions(req) {
                    Ok(resp) => ok_tool_result(id, json!(resp)),
                    Err(err) => service_error(id, err),
                }
            }
            Err(err) => invalid_params(id, err),
        },
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
                    Ok(resp) => ok_tool_result(id, json!(resp)),
                    Err(err) => service_error(id, err),
                }
            }
            Err(err) => invalid_params(id, err),
        },
        _ => method_not_found(id, params.name),
    }
}

fn parse_tool_args<T: for<'de> Deserialize<'de>>(
    arguments: Option<Value>,
) -> std::result::Result<T, String> {
    let value = arguments.unwrap_or_else(|| json!({}));
    serde_json::from_value(value).map_err(|err| err.to_string())
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "memory_status",
            description: "Probe provider health and status.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
        ToolDefinition {
            name: "memory_recall",
            description: "Recall task-relevant memory for a profile and workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string" },
                    "workspace": { "type": "string" },
                    "repoId": { "type": "string" },
                    "query": { "type": "string" },
                    "maxTokens": { "type": "integer", "minimum": 1 }
                },
                "required": ["query"],
                "additionalProperties": false,
            }),
        },
        ToolDefinition {
            name: "memory_search",
            description: "Search safe memory records with existing privacy filters.",
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
        },
        ToolDefinition {
            name: "memory_conclude",
            description: "Write a durable conclusion using the existing write policy.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string" },
                    "workspace": { "type": "string" },
                    "repoId": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["content"],
                "additionalProperties": false,
            }),
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
        },
    ]
}

fn ok(id: Value, result: Value) -> RpcResponse {
    RpcResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: Some(result),
        error: None,
    }
}

fn ok_tool_result(id: Value, structured: Value) -> RpcResponse {
    let content = serde_json::to_string(&structured).unwrap_or_else(|_| "{}".to_string());
    ok(
        id,
        json!({
            "content": [
                {
                    "type": TOOL_TEXT_TYPE,
                    "text": content
                }
            ],
            "structuredContent": structured,
        }),
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

fn service_error(id: Value, err: error::Error) -> RpcResponse {
    let code = match err.code {
        error::ErrorCode::InvalidRequest
        | error::ErrorCode::MissingProfile
        | error::ErrorCode::MissingWorkspace
        | error::ErrorCode::UnknownProfile
        | error::ErrorCode::UnknownWorkspace
        | error::ErrorCode::NotFound
        | error::ErrorCode::SecretDetected
        | error::ErrorCode::PolicyDenied
        | error::ErrorCode::ProfileBoundaryDenied
        | error::ErrorCode::SyncSourceInvalid
        | error::ErrorCode::UnsupportedVersion => -32602,
        error::ErrorCode::AuthMissing => -32001,
        error::ErrorCode::StorageUnavailable | error::ErrorCode::InternalError => -32603,
    };

    server_error(
        id,
        code,
        err.message,
        Some(json!({ "code": err.code.as_str() })),
    )
}
