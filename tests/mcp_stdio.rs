use std::path::PathBuf;

use assert_cmd::Command;
use rmcp::model::{CallToolRequestParams, ProtocolVersion};
use rmcp::transport::TokioChildProcess;
use rmcp::{serve_client_with_lifecycle, ClientLifecycleMode};
use serde_json::json;
use serde_json::Value;
use tempfile::TempDir;
use tokio::process::Command as TokioCommand;

fn bin() -> Command {
    Command::cargo_bin("codex-memoryd").expect("binary built")
}

fn db_path(dir: &TempDir) -> PathBuf {
    dir.path().join("memory.db")
}

fn run_mcp(db: &PathBuf, extra_args: &[&str], requests: &[Value]) -> Vec<Value> {
    let mut messages = Vec::new();
    for request in requests {
        messages.push(request.clone());
        if request["method"] == "initialize" {
            messages.push(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }));
        }
    }
    run_mcp_raw(
        db,
        extra_args,
        &messages
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn run_mcp_raw(db: &PathBuf, extra_args: &[&str], stdin: &str) -> Vec<Value> {
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
                    "protocolVersion": "2025-11-25",
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
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-11-25");

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
                    "protocolVersion": "2025-11-25",
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
                    "protocolVersion": "2025-11-25",
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
fn mcp_stdio_tool_execution_errors_use_mcp_error_results() {
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
                    "protocolVersion": "2025-11-25",
                    "clientInfo": { "name": "codex-memoryd-test", "version": "0.1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "failed-call",
                "method": "tools/call",
                "params": {
                    "name": "memory_recall",
                    "arguments": {
                        "profile": "not-configured",
                        "workspace": "not-configured",
                        "query": "synthetic"
                    }
                }
            }),
        ],
    );

    assert_eq!(responses[1]["id"], "failed-call");
    assert!(responses[1].get("error").is_none());
    assert_eq!(responses[1]["result"]["isError"], true);
    assert_eq!(
        responses[1]["result"]["structuredContent"]["error"]["code"],
        "unknown_profile"
    );
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
                    "protocolVersion": "2025-11-25",
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
                    "protocolVersion": "2025-11-25",
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
                    "protocolVersion": "2025-11-25",
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
                    "protocolVersion": "2025-11-25",
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
                    "protocolVersion": "2025-11-25",
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

#[test]
fn mcp_stdio_legacy_lifecycle_and_notifications_are_protocol_correct() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let responses = run_mcp_raw(
        &db,
        &[],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "before",
                "method": "tools/list",
                "params": {}
            })
            .to_string(),
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "clientInfo": { "name": "legacy-test", "version": "1.0" },
                    "capabilities": {}
                }
            })
            .to_string(),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/list",
                "params": {}
            })
            .to_string(),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })
            .to_string(),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/not-supported"
            })
            .to_string(),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/list",
                "params": {}
            })
            .to_string(),
        ]
        .join("\n")
        .as_str(),
    );

    assert_eq!(responses.len(), 4);
    assert_eq!(responses[0]["id"], "before");
    assert_eq!(responses[0]["error"]["code"], -32002);
    assert_eq!(responses[1]["id"], "init");
    assert_eq!(responses[1]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(responses[2]["id"], 3);
    assert_eq!(responses[2]["error"]["code"], -32002);
    assert_eq!(responses[3]["id"], 4);
    assert_eq!(tool_names(&responses[3]).len(), 3);
}

#[test]
fn mcp_stdio_legacy_version_negotiation_reports_the_supported_revision() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);

    for requested in ["2024-11-05", "2026-08-01"] {
        let request = json!({
            "jsonrpc": "2.0",
            "id": requested,
            "method": "initialize",
            "params": {
                "protocolVersion": requested,
                "clientInfo": { "name": "version-test", "version": "1.0" },
                "capabilities": {}
            }
        });
        let responses = run_mcp_raw(&db, &[], &request.to_string());
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], requested);
        assert_eq!(responses[0]["result"]["protocolVersion"], "2025-11-25");
    }

    let responses = run_mcp(
        &db,
        &[],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "codex-legacy",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "clientInfo": { "name": "codex-legacy-test", "version": "1.0" },
                    "capabilities": {}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "codex-tools",
                "method": "tools/list",
                "params": {}
            }),
        ],
    );
    assert_eq!(responses[0]["id"], "codex-legacy");
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(responses[1]["id"], "codex-tools");
    assert!(responses[1]["result"].get("resultType").is_none());

    let malformed = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "initialize",
        "params": {
            "protocolVersion": 2025,
            "clientInfo": { "name": "version-test", "version": "1.0" },
            "capabilities": {}
        }
    });
    let responses = run_mcp_raw(&db, &[], &malformed.to_string());
    assert_eq!(responses[0]["id"], 7);
    assert_eq!(responses[0]["error"]["code"], -32602);

    let current_modern = json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "initialize",
        "params": {
            "protocolVersion": "2026-07-28",
            "clientInfo": { "name": "version-test", "version": "1.0" },
            "capabilities": {}
        }
    });
    let responses = run_mcp_raw(&db, &[], &current_modern.to_string());
    assert_eq!(responses[0]["id"], 8);
    assert_eq!(responses[0]["error"]["code"], -32601);
    assert!(responses[0]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("server/discover"));
}

#[test]
fn mcp_stdio_modern_metadata_discovery_and_error_ids() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let metadata = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "modern-test", "version": "1.0" },
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    let mut discovery = json!({ "jsonrpc": "2.0", "id": "discovery", "method": "server/discover" });
    discovery["params"] = json!({ "_meta": metadata });
    let mut unsupported = json!({ "jsonrpc": "2.0", "id": 25, "method": "tools/list" });
    unsupported["params"] = json!({
        "_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-08-01",
            "io.modelcontextprotocol/clientInfo": { "name": "modern-test", "version": "1.0" },
            "io.modelcontextprotocol/clientCapabilities": {}
        }
    });
    let missing_metadata =
        json!({ "jsonrpc": "2.0", "id": 26, "method": "server/discover", "params": {} });
    let mut malformed_client_info = json!({
        "jsonrpc": "2.0",
        "id": 27,
        "method": "tools/list"
    });
    malformed_client_info["params"] = json!({
        "_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": {},
            "io.modelcontextprotocol/clientCapabilities": {}
        }
    });
    let mut unknown = json!({ "jsonrpc": "2.0", "id": "unknown", "method": "not/a/method" });
    unknown["params"] = json!({ "_meta": metadata });
    let notifications = json!({
        "jsonrpc": "2.0",
        "method": "notifications/not-supported",
        "params": { "_meta": metadata }
    });
    let responses = run_mcp_raw(
        &db,
        &[],
        &[
            discovery,
            unsupported,
            missing_metadata,
            malformed_client_info,
            unknown,
            notifications,
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n"),
    );

    assert_eq!(responses.len(), 5);
    assert_eq!(responses[0]["id"], "discovery");
    assert_eq!(responses[0]["result"]["resultType"], "complete");
    assert_eq!(
        responses[0]["result"]["supportedVersions"],
        json!(["2026-07-28", "2025-11-25", "2025-06-18"])
    );
    assert_eq!(responses[1]["id"], 25);
    assert_eq!(responses[1]["error"]["code"], -32022);
    assert_eq!(responses[1]["error"]["data"]["requested"], "2026-08-01");
    assert_eq!(
        responses[1]["error"]["data"]["supported"],
        json!(["2026-07-28", "2025-11-25", "2025-06-18"])
    );
    assert_eq!(responses[2]["id"], 26);
    assert_eq!(responses[2]["error"]["code"], -32602);
    assert_eq!(responses[3]["id"], 27);
    assert_eq!(responses[3]["error"]["code"], -32602);
    assert_eq!(responses[4]["id"], "unknown");
    assert_eq!(responses[4]["error"]["code"], -32601);
}

#[test]
fn mcp_stdio_malformed_json_and_oversized_lines_are_bounded() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let oversized = " ".repeat(1024 * 1024 + 1);
    let valid = json!({
        "jsonrpc": "2.0",
        "id": "after-oversized",
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "limit-test", "version": "1.0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    });
    let responses = run_mcp_raw(
        &db,
        &[],
        &format!(
            "{{malformed\n{{\"jsonrpc\":\"1.0\",\"method\":\"broken\"}}\n{oversized}\n{}\n",
            valid
        ),
    );

    assert_eq!(responses.len(), 4);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(responses[1]["error"]["code"], -32600);
    assert_eq!(responses[1]["id"], Value::Null);
    assert_eq!(responses[2]["error"]["code"], -32700);
    assert!(responses[2]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("1048576-byte"));
    assert_eq!(responses[3]["id"], "after-oversized");
    assert_eq!(responses[3]["result"]["resultType"], "complete");
}

#[test]
fn mcp_stdio_exits_cleanly_on_eof_without_messages() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    assert!(run_mcp_raw(&db, &[], "").is_empty());
}

#[tokio::test]
async fn mcp_stdio_interoperates_with_the_official_rmcp_client() {
    let dir = TempDir::new().unwrap();
    let db = db_path(&dir);
    let mut command = TokioCommand::new(assert_cmd::cargo::cargo_bin!("codex-memoryd"));
    command.args(["--db", db.to_str().unwrap(), "mcp", "stdio"]);
    let transport = TokioChildProcess::new(command).expect("stdio child transport");
    let client = serve_client_with_lifecycle(
        (),
        transport,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .expect("2026-07-28 rmcp discovery succeeds");

    let tools = client
        .peer()
        .list_tools(None)
        .await
        .expect("rmcp discovers tools");
    assert!(tools.tools.iter().any(|tool| tool.name == "memory_status"));

    let arguments = json!({}).as_object().unwrap().clone();
    let status = client
        .peer()
        .call_tool(CallToolRequestParams::new("memory_status").with_arguments(arguments))
        .await
        .expect("rmcp calls memory_status");
    assert_eq!(
        status.structured_content.as_ref().unwrap()["provider_name"],
        "codex-memoryd"
    );

    client.cancel().await.expect("client closes stdio cleanly");
}
