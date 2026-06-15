//! Integration tests for the nexus-mcp binary.
//!
//! Spawns the MCP server as a child process and communicates via JSON-RPC
//! over stdio (newline-delimited JSON, per the MCP spec).

use serde_json::{json, Value};
use std::fs;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

fn cargo_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove `deps`
    path.push("nexus-mcp");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

struct McpClient {
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    _child: tokio::process::Child,
}

impl McpClient {
    async fn spawn() -> Self {
        let bin = cargo_bin();
        assert!(
            bin.exists(),
            "nexus-mcp binary not found at {:?} — run `cargo build --bin nexus-mcp` first",
            bin
        );

        let mut child = Command::new(&bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn nexus-mcp");

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        McpClient {
            stdin,
            reader: BufReader::new(stdout),
            _child: child,
        }
    }

    async fn send(&mut self, msg: &Value) {
        let mut line = serde_json::to_string(msg).unwrap();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut buf = String::new();
        timeout(Duration::from_secs(10), self.reader.read_line(&mut buf))
            .await
            .expect("timeout waiting for MCP response")
            .expect("IO error reading MCP response");
        serde_json::from_str(buf.trim()).expect("invalid JSON from MCP server")
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send(&msg).await;
        self.recv().await
    }
}

#[tokio::test]
async fn initialize_and_list_tools() {
    let mut client = McpClient::spawn().await;

    // Send initialize request (MCP handshake)
    let resp = client
        .request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1.0" }
            }),
        )
        .await;

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert!(
        resp["result"].is_object(),
        "expected result object, got: {resp}"
    );

    let server_info = &resp["result"]["serverInfo"];
    assert!(server_info.is_object());

    // Send initialized notification
    client
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }))
        .await;

    // List tools
    let resp = client.request(2, "tools/list", json!({})).await;
    assert_eq!(resp["id"], 2);

    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");
    assert_eq!(tools.len(), 6, "expected 6 tools, got: {:?}", tools);

    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"nexus_execute"));
    assert!(tool_names.contains(&"nexus_execute_wasi"));
    assert!(tool_names.contains(&"nexus_snapshot_create"));
    assert!(tool_names.contains(&"nexus_snapshot_rollback"));
    assert!(tool_names.contains(&"nexus_issue_token"));
    assert!(tool_names.contains(&"nexus_fork_and_race"));
}

#[tokio::test]
async fn snapshot_create_returns_uuid() {
    let mut client = McpClient::spawn().await;

    // Initialize
    client
        .request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1.0" }
            }),
        )
        .await;

    client
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }))
        .await;

    // Call nexus_snapshot_create
    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_snapshot_create",
                "arguments": { "label": "test-checkpoint" }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2);
    let content = &resp["result"]["content"];
    assert!(content.is_array(), "expected content array, got: {resp}");

    let text = content[0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    // Verify the snapshot_id is a valid UUID
    let snap_id = parsed["snapshot_id"].as_str().unwrap();
    uuid::Uuid::parse_str(snap_id).expect("snapshot_id should be a valid UUID");
}

#[tokio::test]
async fn issue_token_returns_token_info() {
    let mut client = McpClient::spawn().await;

    // Initialize
    client
        .request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1.0" }
            }),
        )
        .await;

    client
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }))
        .await;

    // Issue a read_file token
    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_issue_token",
                "arguments": {
                    "capability": "read_file",
                    "path": "/tmp/test",
                    "validity_secs": 300
                }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();

    assert!(parsed["token_id"].is_string());
    uuid::Uuid::parse_str(parsed["token_id"].as_str().unwrap())
        .expect("token_id should be a valid UUID");
    assert_eq!(parsed["expires_in_secs"], 300);
    assert!(parsed["capability"].as_str().unwrap().contains("ReadFile"));
}

#[tokio::test]
async fn execute_wasi_grants_requested_read_file_capability() {
    let tmp = tempfile::tempdir().unwrap();
    let wasm_path = tmp.path().join("wasi_grant_regression.wasm");
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();
    fs::write(&wasm_path, wasm).unwrap();

    let mut client = McpClient::spawn().await;

    client
        .request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1.0" }
            }),
        )
        .await;

    client
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }))
        .await;

    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_execute_wasi",
                "arguments": {
                    "wasm_path": wasm_path,
                    "capabilities": [
                        { "type": "read_file", "path": tmp.path() }
                    ]
                }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();

    assert_eq!(parsed["success"], true, "expected success, got: {parsed}");
    assert_eq!(parsed["error"], Value::Null, "unexpected error: {parsed}");
}

#[tokio::test]
async fn execute_with_missing_wasm_returns_error() {
    let mut client = McpClient::spawn().await;

    // Initialize
    client
        .request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1.0" }
            }),
        )
        .await;

    client
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }))
        .await;

    // Try to execute a non-existent wasm file
    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_execute",
                "arguments": { "wasm_path": "/nonexistent/fake.wasm" }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("error"),
        "expected error for missing wasm, got: {text}"
    );
}
