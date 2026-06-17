//! Integration tests for the nexus-mcp binary.
//!
//! Spawns the MCP server as a child process and communicates via JSON-RPC
//! over stdio (newline-delimited JSON, per the MCP spec).

use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

const NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV: &str = "NEXUS_MCP_CAPABILITY_ALLOWLIST";
const WASM_PAGE_SIZE: usize = 65_536;

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
        Self::spawn_with_module_dir_and_allowlist(None, None).await
    }

    async fn spawn_with_module_dir(module_dir: Option<&std::path::Path>) -> Self {
        Self::spawn_with_module_dir_and_allowlist(module_dir, None).await
    }

    async fn spawn_with_module_dir_and_allowlist(
        module_dir: Option<&std::path::Path>,
        capability_allowlist: Option<&str>,
    ) -> Self {
        let bin = cargo_bin();
        assert!(
            bin.exists(),
            "nexus-mcp binary not found at {:?} — run `cargo build --bin nexus-mcp` first",
            bin
        );

        let mut command = Command::new(&bin);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.env_remove(NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV);
        if let Some(module_dir) = module_dir {
            command.env("NEXUS_MCP_MODULE_DIR", module_dir);
        }
        if let Some(capability_allowlist) = capability_allowlist {
            command.env(NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV, capability_allowlist);
        }

        let mut child = command.spawn().expect("failed to spawn nexus-mcp");

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

fn read_file_allowlist(path: &Path) -> String {
    json!([
        {
            "type": "read_file",
            "path": path.display().to_string()
        }
    ])
    .to_string()
}

fn tool_json(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response should contain text content");
    serde_json::from_str(text).expect("tool response text should be JSON")
}

async fn initialize_client(client: &mut McpClient) {
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
}

fn module_with_data(data: &str, global_value: i32) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 0) "{data}")
            (global (export "marker") (mut i32) (i32.const {global_value}))
            (func (export "_start"))
        )"#
    ))
    .unwrap()
}

fn module_sets_marker(marker_value: i32) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 1)
            (global $marker (export "marker") (mut i32) (i32.const 0))
            (func (export "_start")
                i32.const {marker_value}
                global.set $marker)
        )"#
    ))
    .unwrap()
}

fn module_requires_restored_marker_and_true_input(marker_value: i32) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 1)
            (global $marker (export "marker") (mut i32) (i32.const 0))
            (func (export "_start")
                global.get $marker
                i32.const {marker_value}
                i32.ne
                if
                    unreachable
                end

                ;; Runtime input is [len: u32 LE][JSON bytes]. The first byte
                ;; of JSON `true` is ASCII 't' at offset 4.
                i32.const 4
                i32.load8_u
                i32.const 116
                i32.ne
                if
                    unreachable
                end)
        )"#
    ))
    .unwrap()
}

fn expected_memory(data: &[u8]) -> Vec<u8> {
    let mut memory = vec![0u8; WASM_PAGE_SIZE];
    memory[..data.len()].copy_from_slice(data);
    memory
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
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
async fn snapshot_create_latest_runtime_rolls_back_restored_state() {
    let tmp = tempfile::tempdir().unwrap();
    let base_path = tmp.path().join("base_runtime_snapshot.wasm");
    let diff_path = tmp.path().join("diff_runtime_snapshot.wasm");
    fs::write(&base_path, module_with_data("base", 7)).unwrap();
    fs::write(&diff_path, module_with_data("diff", 11)).unwrap();

    let base_memory = expected_memory(b"base");
    let diff_memory = expected_memory(b"diff");
    let base_checksum = sha256_hex(&base_memory);
    let diff_checksum = sha256_hex(&diff_memory);

    let mut client = McpClient::spawn_with_module_dir(Some(tmp.path())).await;
    initialize_client(&mut client).await;

    let base_exec = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_execute",
                "arguments": { "wasm_path": base_path }
            }),
        )
        .await;
    let base_exec = tool_json(&base_exec);
    assert_eq!(
        base_exec["success"], true,
        "base execute failed: {base_exec}"
    );
    let execute_snapshot_id = base_exec["snapshot_id"]
        .as_str()
        .expect("execute response should expose the runtime snapshot id");

    let create_resp = client
        .request(
            3,
            "tools/call",
            json!({
                "name": "nexus_snapshot_create",
                "arguments": {
                    "label": "base-runtime",
                    "source": "latest_runtime"
                }
            }),
        )
        .await;
    let created = tool_json(&create_resp);
    assert_eq!(
        created["snapshot_id"].as_str(),
        Some(execute_snapshot_id),
        "latest_runtime snapshot_create must return the real execution snapshot"
    );
    let base_snapshot_id = created["snapshot_id"].as_str().unwrap();

    let diff_exec = client
        .request(
            4,
            "tools/call",
            json!({
                "name": "nexus_execute",
                "arguments": { "wasm_path": diff_path }
            }),
        )
        .await;
    let diff_exec = tool_json(&diff_exec);
    assert_eq!(
        diff_exec["success"], true,
        "diff execute failed: {diff_exec}"
    );
    assert_ne!(
        diff_exec["snapshot_id"].as_str(),
        Some(base_snapshot_id),
        "second execution should create a distinct runtime snapshot"
    );

    let rollback_resp = client
        .request(
            5,
            "tools/call",
            json!({
                "name": "nexus_snapshot_rollback",
                "arguments": {
                    "snapshot_id": base_snapshot_id,
                    "include_restored_state": true
                }
            }),
        )
        .await;
    let rollback = tool_json(&rollback_resp);
    assert_eq!(rollback["snapshot_id"].as_str(), Some(base_snapshot_id));
    assert_eq!(rollback["fs_operations"], 0);
    let restored = &rollback["restored_state"];
    assert!(
        restored.is_object(),
        "rollback should include restored_state when requested: {rollback}"
    );

    let memory = &restored["memory"];
    assert_eq!(memory["byte_len"], WASM_PAGE_SIZE);
    assert_eq!(memory["sha256"], base_checksum);
    assert_ne!(memory["sha256"], diff_checksum);

    let preview = memory["preview_base64"]
        .as_str()
        .expect("restored memory preview should be present");
    let preview = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, preview)
        .expect("memory preview should be valid base64");
    assert_eq!(&preview[..4], b"base");
    assert_ne!(&preview[..4], b"diff");

    assert!(
        restored["execution_state"]["captured_globals"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "restored execution state summary should report captured globals: {rollback}"
    );
}

#[tokio::test]
async fn fork_and_race_from_snapshot_seeds_each_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let base_path = tmp.path().join("fork_base_marker.wasm");
    let branch_path = tmp.path().join("fork_branch_requires_marker.wasm");
    fs::write(&base_path, module_sets_marker(1234)).unwrap();
    fs::write(
        &branch_path,
        module_requires_restored_marker_and_true_input(1234),
    )
    .unwrap();

    let mut client = McpClient::spawn_with_module_dir(Some(tmp.path())).await;
    initialize_client(&mut client).await;

    let base_exec = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_execute",
                "arguments": { "wasm_path": base_path }
            }),
        )
        .await;
    let base_exec = tool_json(&base_exec);
    assert_eq!(
        base_exec["success"], true,
        "base execute failed: {base_exec}"
    );
    let base_snapshot_id = base_exec["snapshot_id"]
        .as_str()
        .expect("base execution should expose a runtime snapshot id");
    uuid::Uuid::parse_str(base_snapshot_id).expect("base snapshot id should be a UUID");

    let race_resp = client
        .request(
            3,
            "tools/call",
            json!({
                "name": "nexus_fork_and_race",
                "arguments": {
                    "wasm_path": branch_path,
                    "base_snapshot_id": base_snapshot_id,
                    "strategy": "wait_all",
                    "branches": [
                        { "input": true },
                        { "entry": "_start", "input": true }
                    ]
                }
            }),
        )
        .await;
    let race = tool_json(&race_resp);

    assert_eq!(race["base_snapshot_id"].as_str(), Some(base_snapshot_id));
    assert_eq!(
        race["base_snapshot_source"].as_str(),
        Some("explicit_snapshot_id")
    );
    assert_eq!(
        race["semantics"].as_str(),
        Some("fork_from_captured_runtime_snapshot")
    );
    assert_eq!(race["branches_tried"].as_u64(), Some(2));
    assert_eq!(
        race["branches_succeeded"].as_u64(),
        Some(2),
        "wait_all should prove every branch observed restored marker state: {race}"
    );
    assert_eq!(race["winner_output"]["success"], true);
    assert_eq!(race["winner_output"]["error"], Value::Null);
}

#[tokio::test]
async fn fork_and_race_latest_runtime_source_uses_real_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let base_path = tmp.path().join("fork_latest_runtime_base.wasm");
    let branch_path = tmp.path().join("fork_latest_runtime_branch.wasm");
    fs::write(&base_path, module_sets_marker(4321)).unwrap();
    fs::write(
        &branch_path,
        module_requires_restored_marker_and_true_input(4321),
    )
    .unwrap();

    let mut client = McpClient::spawn_with_module_dir(Some(tmp.path())).await;
    initialize_client(&mut client).await;

    let base_exec = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_execute",
                "arguments": { "wasm_path": base_path }
            }),
        )
        .await;
    let base_exec = tool_json(&base_exec);
    assert_eq!(base_exec["success"], true);
    let base_snapshot_id = base_exec["snapshot_id"]
        .as_str()
        .expect("base execution should expose a runtime snapshot id");

    let race_resp = client
        .request(
            3,
            "tools/call",
            json!({
                "name": "nexus_fork_and_race",
                "arguments": {
                    "wasm_path": branch_path,
                    "source": "latest_runtime",
                    "strategy": "wait_all",
                    "branches": [
                        { "input": true },
                        { "input": true }
                    ]
                }
            }),
        )
        .await;
    let race = tool_json(&race_resp);

    assert_eq!(race["base_snapshot_id"].as_str(), Some(base_snapshot_id));
    assert_eq!(
        race["base_snapshot_source"].as_str(),
        Some("latest_runtime")
    );
    assert_eq!(
        race["semantics"].as_str(),
        Some("fork_from_captured_runtime_snapshot")
    );
    assert_eq!(race["branches_tried"].as_u64(), Some(2));
    assert_eq!(race["branches_succeeded"].as_u64(), Some(2));
}

#[tokio::test]
async fn fork_and_race_without_snapshot_labels_from_scratch() {
    let tmp = tempfile::tempdir().unwrap();
    let branch_path = tmp.path().join("fork_from_scratch_noop.wasm");
    fs::write(
        &branch_path,
        wat::parse_str(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start"))
            )"#,
        )
        .unwrap(),
    )
    .unwrap();

    let mut client = McpClient::spawn_with_module_dir(Some(tmp.path())).await;
    initialize_client(&mut client).await;

    let race_resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_fork_and_race",
                "arguments": {
                    "wasm_path": branch_path,
                    "strategy": "wait_all",
                    "branches": [{}, {}]
                }
            }),
        )
        .await;
    let race = tool_json(&race_resp);

    assert_eq!(race["base_snapshot_id"], Value::Null);
    assert_eq!(race["base_snapshot_source"].as_str(), Some("from_scratch"));
    assert_eq!(
        race["semantics"].as_str(),
        Some("from_scratch_no_snapshot_restore")
    );
    assert_eq!(race["branches_tried"].as_u64(), Some(2));
    assert_eq!(race["branches_succeeded"].as_u64(), Some(2));
}

#[tokio::test]
async fn issue_token_returns_token_info() {
    let allowlist = read_file_allowlist(Path::new("/tmp/test"));
    let mut client = McpClient::spawn_with_module_dir_and_allowlist(None, Some(&allowlist)).await;

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
async fn issue_token_rejects_capability_without_operator_allowlist() {
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
    let parsed = tool_json(&resp);
    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("requires operator allowlist")
            && error.contains(NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV),
        "expected non-allowlisted token issuance rejection, got: {parsed}"
    );
}

#[tokio::test]
async fn execute_wasi_grants_allowlisted_read_file_capability() {
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

    let allowlist = read_file_allowlist(tmp.path());
    let mut client =
        McpClient::spawn_with_module_dir_and_allowlist(Some(tmp.path()), Some(&allowlist)).await;

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
    let parsed = tool_json(&resp);

    assert_eq!(parsed["success"], true, "expected success, got: {parsed}");
    assert_eq!(parsed["error"], Value::Null, "unexpected error: {parsed}");
}

#[tokio::test]
async fn execute_wasi_rejects_caller_chosen_capability_without_parent_token_or_allowlist() {
    let tmp = tempfile::tempdir().unwrap();
    let wasm_path = tmp.path().join("wasi_self_grant_expected_rejection.wasm");
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();
    fs::write(&wasm_path, wasm).unwrap();

    let mut client = McpClient::spawn_with_module_dir(Some(tmp.path())).await;

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
    let parsed = tool_json(&resp);
    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("parent_token_id")
            && error.contains(NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV),
        "execute_wasi must reject self-granted caller-chosen capabilities without a parent token or allowlist; got: {parsed}"
    );
}

#[tokio::test]
async fn execute_wasi_rejects_capability_not_in_operator_allowlist() {
    let tmp = tempfile::tempdir().unwrap();
    let wasm_path = tmp.path().join("wasi_allowlist_rejection.wasm");
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();
    fs::write(&wasm_path, wasm).unwrap();

    let allowed_dir = tmp.path().join("allowed");
    let denied_dir = tmp.path().join("denied");
    fs::create_dir_all(&allowed_dir).unwrap();
    fs::create_dir_all(&denied_dir).unwrap();

    let allowlist = read_file_allowlist(&allowed_dir);
    let mut client =
        McpClient::spawn_with_module_dir_and_allowlist(Some(tmp.path()), Some(&allowlist)).await;

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
                        { "type": "read_file", "path": denied_dir }
                    ]
                }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2);
    let parsed = tool_json(&resp);
    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("not allowed") && error.contains(NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV),
        "expected non-allowlisted capability rejection, got: {parsed}"
    );
}

#[tokio::test]
async fn execute_wasi_accepts_capability_attenuated_from_parent_token() {
    let tmp = tempfile::tempdir().unwrap();
    let wasm_path = tmp.path().join("wasi_parent_token_success.wasm");
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();
    fs::write(&wasm_path, wasm).unwrap();

    let allowlist = read_file_allowlist(tmp.path());
    let mut client =
        McpClient::spawn_with_module_dir_and_allowlist(Some(tmp.path()), Some(&allowlist)).await;

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

    let token_resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_issue_token",
                "arguments": {
                    "capability": "read_file",
                    "path": tmp.path(),
                    "validity_secs": 300
                }
            }),
        )
        .await;
    let token = tool_json(&token_resp);
    let parent_token_id = token["token_id"].as_str().unwrap();

    let resp = client
        .request(
            3,
            "tools/call",
            json!({
                "name": "nexus_execute_wasi",
                "arguments": {
                    "wasm_path": wasm_path,
                    "parent_token_id": parent_token_id,
                    "capabilities": [
                        { "type": "read_file", "path": tmp.path() }
                    ]
                }
            }),
        )
        .await;

    assert_eq!(resp["id"], 3);
    let parsed = tool_json(&resp);
    assert_eq!(parsed["success"], true, "expected success, got: {parsed}");
    assert_eq!(parsed["error"], Value::Null, "unexpected error: {parsed}");
}

#[tokio::test]
async fn execute_wasi_rejects_capability_outside_parent_token_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let wasm_path = tmp.path().join("wasi_parent_token_rejection.wasm");
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();
    fs::write(&wasm_path, wasm).unwrap();

    let allowed_dir = tmp.path().join("allowed");
    let denied_dir = tmp.path().join("denied");
    fs::create_dir_all(&allowed_dir).unwrap();
    fs::create_dir_all(&denied_dir).unwrap();

    let allowlist = read_file_allowlist(&allowed_dir);
    let mut client =
        McpClient::spawn_with_module_dir_and_allowlist(Some(tmp.path()), Some(&allowlist)).await;

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

    let token_resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_issue_token",
                "arguments": {
                    "capability": "read_file",
                    "path": allowed_dir,
                    "validity_secs": 300
                }
            }),
        )
        .await;
    let token = tool_json(&token_resp);
    let parent_token_id = token["token_id"].as_str().unwrap();

    let resp = client
        .request(
            3,
            "tools/call",
            json!({
                "name": "nexus_execute_wasi",
                "arguments": {
                    "wasm_path": wasm_path,
                    "parent_token_id": parent_token_id,
                    "capabilities": [
                        { "type": "read_file", "path": denied_dir }
                    ]
                }
            }),
        )
        .await;

    assert_eq!(resp["id"], 3);
    let parsed = tool_json(&resp);
    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("parent_token_id") && error.contains("not a subset"),
        "expected parent token scope rejection, got: {parsed}"
    );
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
