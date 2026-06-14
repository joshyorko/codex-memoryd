use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::json;
use serde_json::Value;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("codex-memoryd").expect("binary built")
}

fn db_path(dir: &TempDir) -> PathBuf {
    dir.path().join("memory.db")
}

fn run_mcp(db: &PathBuf, extra_args: &[&str], requests: &[Value]) -> Vec<Value> {
    let stdin = requests
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    let output = bin()
        .arg("--db")
        .arg(db)
        .args(["mcp", "stdio"])
        .args(extra_args)
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    String::from_utf8(output)
        .expect("stdout is utf8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid json-rpc response"))
        .collect()
}

fn tool_names(response: &Value) -> Vec<&str> {
    response["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect()
}

#[test]
fn mcp_stdio_initializes_lists_tools_and_status() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let responses = run_mcp(
        &db,
        &[],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "codex-memoryd-test", "version": "0.1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "memory_status",
                    "arguments": {}
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "codex-memoryd"
    );
    assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");

    assert_eq!(
        tool_names(&responses[1]),
        vec!["memory_status", "memory_recall", "memory_search"]
    );
    let recall_tool = responses[1]["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|tool| tool["name"] == "memory_recall")
        .expect("memory_recall tool");
    assert_eq!(
        recall_tool["inputSchema"]["properties"]["packMode"]["enum"],
        json!([
            "default",
            "debugging",
            "onboarding",
            "planning",
            "active_task",
            "review",
            "personal_context"
        ])
    );

    assert_eq!(
        responses[2]["result"]["structuredContent"]["provider_name"],
        "codex-memoryd"
    );
}

#[test]
fn mcp_stdio_conclude_roundtrip_surfaces_in_recall() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let responses = run_mcp(
        &db,
        &["--write-tools"],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "codex-memoryd-test", "version": "0.1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "memory_conclude",
                    "arguments": {
                        "profile": "personal",
                        "workspace": "mcp-smoke",
                        "content": "Decision: use bundled sqlite for storage"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "memory_recall",
                    "arguments": {
                        "profile": "personal",
                        "workspace": "mcp-smoke",
                        "query": "bundled sqlite",
                        "packMode": "debugging"
                    }
                }
            }),
        ],
    );

    assert_eq!(
        responses[1]["result"]["structuredContent"]["record_ids"]
            .as_array()
            .expect("record ids")
            .len(),
        1
    );
    assert!(responses[2]["result"]["structuredContent"]["facts"]
        .as_array()
        .expect("facts array")
        .iter()
        .any(|fact| fact["content"]
            .as_str()
            .expect("fact content")
            .contains("bundled sqlite")));
    assert_eq!(
        responses[2]["result"]["structuredContent"]["pack"]["mode"],
        "debugging"
    );
}

#[test]
fn mcp_stdio_rejects_unknown_tool_args_field() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let responses = run_mcp(
        &db,
        &[],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "codex-memoryd-test", "version": "0.1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "memory_recall",
                    "arguments": {
                        "query": "secret",
                        "workspace": "mcp-smoke",
                        "profile": "personal",
                        "extraneous": "rejected"
                    }
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["error"]["code"], -32602);
}

#[test]
fn mcp_stdio_accepts_tool_call_meta() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let responses = run_mcp(
        &db,
        &[],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "codex-memoryd-test", "version": "0.1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "memory_status",
                    "arguments": {},
                    "_meta": { "progressToken": "codex-current" }
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 2);
    assert_eq!(
        responses[1]["result"]["structuredContent"]["provider_name"],
        "codex-memoryd"
    );
}

#[test]
fn mcp_stdio_defaults_to_read_only_tools_and_rejects_writes() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let responses = run_mcp(
        &db,
        &[],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "codex-memoryd-test", "version": "0.1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "memory_status",
                    "arguments": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "memory_conclude",
                    "arguments": {
                        "profile": "personal",
                        "workspace": "mcp-smoke",
                        "content": "should be blocked"
                    }
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 4);

    assert_eq!(
        tool_names(&responses[1]),
        vec!["memory_status", "memory_recall", "memory_search"]
    );

    assert_eq!(
        responses[2]["result"]["structuredContent"]["provider_name"],
        "codex-memoryd"
    );
    assert_eq!(responses[3]["error"]["code"], -32601);
    assert!(responses[3]["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("read-only mode"));
}

#[test]
fn mcp_stdio_write_tools_are_explicit_opt_in_and_policy_gated() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let responses = run_mcp(
        &db,
        &["--write-tools"],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "codex-memoryd-test", "version": "0.1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "memory_create",
                    "arguments": {
                        "profile": "personal",
                        "workspace": "mcp-write",
                        "content": "Decision: MCP write tools require explicit opt in"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "memory_create",
                    "arguments": {
                        "profile": "personal",
                        "workspace": "mcp-write",
                        "content": "OPENAI_API_KEY=sk-test-1234567890abcdefghijklmnop"
                    }
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 4);
    assert_eq!(
        tool_names(&responses[1]),
        vec![
            "memory_status",
            "memory_recall",
            "memory_search",
            "memory_create",
            "memory_conclude",
            "memory_checkpoint",
            "memory_import_preview",
            "memory_import_apply",
        ]
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"]["record_ids"]
            .as_array()
            .expect("record ids")
            .len(),
        1
    );
    let rejected = responses[3]["result"]["structuredContent"]["rejected"]
        .as_array()
        .expect("rejections");
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0]["reason"]
        .as_str()
        .expect("rejection reason")
        .contains("secret"));
}

#[test]
fn mcp_stdio_import_preview_and_apply_use_existing_sync_policy() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let file = json!({
        "path": "MEMORY.md",
        "kind": "memory_registry",
        "content": "- Prefer MCP schema snapshots for adapter reviews."
    });
    let args = json!({
        "profile": "personal",
        "workspace": "mcp-import",
        "sourceRoot": "/tmp/codex-memoryd-mcp-import",
        "files": [file]
    });

    let responses = run_mcp(
        &db,
        &["--write-tools"],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "codex-memoryd-test", "version": "0.1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "memory_import_preview",
                    "arguments": args
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "memory_import_apply",
                    "arguments": args
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "memory_search",
                    "arguments": {
                        "profile": "personal",
                        "workspace": "mcp-import",
                        "query": "schema snapshots",
                        "limit": 3
                    }
                }
            }),
        ],
    );

    assert_eq!(
        responses[1]["result"]["structuredContent"]["mode"],
        "preview"
    );
    assert_eq!(responses[1]["result"]["structuredContent"]["created"], 0);
    assert_eq!(responses[2]["result"]["structuredContent"]["mode"], "apply");
    assert_eq!(responses[2]["result"]["structuredContent"]["created"], 1);
    assert!(responses[3]["result"]["structuredContent"]["matches"]
        .as_array()
        .expect("search matches")
        .iter()
        .any(|item| item["content"]
            .as_str()
            .expect("item content")
            .contains("schema snapshots")));
}

#[test]
fn mcp_stdio_tool_schema_snapshot_matches_fixture() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    let responses = run_mcp(
        &db,
        &["--write-tools"],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "codex-memoryd-test", "version": "0.1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }),
        ],
    );

    let actual = &responses[1]["result"]["tools"];
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/mcp_tools.write.json")).unwrap();
    assert_eq!(actual, &expected);
}
