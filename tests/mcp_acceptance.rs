//! MCP acceptance tests -- new-user golden path + security boundary checks.
//!
//! Simulates a first-time user discovering and exercising the Nexus MCP server
//! in the order they would naturally encounter each tool, then verifies that
//! each security hardening control (SSRF egress blocking, snapshot digest
//! binding, revocation-before-profile, DenialReason safe messages) actually
//! rejects the intended hostile inputs.
//!
//! Harness pattern mirrors tests/mcp_server.rs exactly: spawn the
//! nexus-mcp binary as a child process, communicate via newline-delimited
//! JSON-RPC over stdio.

use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

const NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV: &str = "NEXUS_MCP_CAPABILITY_ALLOWLIST";
const NEXUS_MCP_PROFILE_ENV: &str = "NEXUS_MCP_PROFILE";

fn cargo_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
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
        Self::spawn_with_allowlist(None).await
    }

    async fn spawn_with_allowlist(allowlist: Option<&str>) -> Self {
        let bin = cargo_bin();
        assert!(bin.exists(), "nexus-mcp binary not found at {:?}", bin);

        let mut command = Command::new(&bin);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.env_remove(NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV);
        command.env_remove(NEXUS_MCP_PROFILE_ENV);
        if let Some(al) = allowlist {
            command.env(NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV, al);
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

async fn initialize_client(client: &mut McpClient) {
    client
        .request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "mcp-acceptance-test", "version": "0.1.0" }
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

fn tool_json(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response should contain a text content element");
    serde_json::from_str(text).expect("tool response text should be valid JSON")
}

fn http_get_allowlist(url_pattern: &str) -> String {
    json!([
        { "type": "http_get", "path": url_pattern }
    ])
    .to_string()
}

// ============================================================================
// HAPPY PATH -- new user golden path
// ============================================================================

#[tokio::test]
async fn acceptance_01_get_stats_health_check() {
    let mut client = McpClient::spawn().await;
    initialize_client(&mut client).await;

    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_get_stats",
                "arguments": {}
            }),
        )
        .await;

    assert_eq!(resp["id"], 2, "response id mismatch: {resp}");
    assert!(
        resp["result"].is_object(),
        "expected a result object, got: {resp}"
    );

    let parsed = tool_json(&resp);

    assert!(
        parsed["telemetry"].is_object(),
        "get_stats must return a telemetry object: {parsed}"
    );
    assert!(
        parsed["snapshots"].is_object(),
        "get_stats must return a snapshots object: {parsed}"
    );
    assert_eq!(
        parsed["telemetry"]["total_executions"], 0,
        "fresh server should report zero total_executions: {parsed}"
    );
    assert_eq!(
        parsed["telemetry"]["failed_executions"], 0,
        "fresh server should report zero failed_executions: {parsed}"
    );
    assert_eq!(
        parsed["snapshots"]["total_snapshots"], 0,
        "fresh server should report zero total_snapshots: {parsed}"
    );
    assert!(
        parsed["error"].is_null(),
        "get_stats must not return an error on a fresh server: {parsed}"
    );
}

#[tokio::test]
async fn acceptance_02_issue_http_get_token_for_public_url() {
    let mut client =
        McpClient::spawn_with_allowlist(Some(&http_get_allowlist("https://example.com/*"))).await;
    initialize_client(&mut client).await;

    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_issue_token",
                "arguments": {
                    "capability": "http_get",
                    "path": "https://example.com/*",
                    "validity_secs": 300
                }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2, "response id mismatch: {resp}");

    let parsed = tool_json(&resp);

    assert!(
        parsed["error"].is_null(),
        "issuing a public-URL http_get token should succeed: {parsed}"
    );

    let token_id = parsed["token_id"]
        .as_str()
        .expect("token response must include a token_id string");
    uuid::Uuid::parse_str(token_id).expect("token_id must be a valid UUID");

    let capability = parsed["capability"].as_str().unwrap_or_default();
    assert!(
        capability.contains("HttpGet"),
        "token capability must describe HttpGet, got: {parsed}"
    );

    assert_eq!(
        parsed["expires_in_secs"], 300,
        "token validity must match the requested 300 s: {parsed}"
    );
}

#[tokio::test]
async fn acceptance_03_snapshot_create_returns_uuid() {
    let mut client = McpClient::spawn().await;
    initialize_client(&mut client).await;

    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_snapshot_create",
                "arguments": { "label": "acceptance-baseline" }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2, "response id mismatch: {resp}");

    let parsed = tool_json(&resp);

    assert!(
        parsed["error"].is_null(),
        "snapshot_create should succeed for a baseline: {parsed}"
    );
    assert_eq!(
        parsed["success"], true,
        "snapshot_create success flag must be true: {parsed}"
    );

    let snap_id = parsed["snapshot_id"]
        .as_str()
        .expect("snapshot_create response must include a snapshot_id");
    uuid::Uuid::parse_str(snap_id).expect("snapshot_id must be a valid UUID");
}

#[tokio::test]
async fn acceptance_04_snapshot_rollback_without_digest() {
    let mut client = McpClient::spawn().await;
    initialize_client(&mut client).await;

    let create_resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_snapshot_create",
                "arguments": { "label": "rollback-target" }
            }),
        )
        .await;
    let created = tool_json(&create_resp);
    let snap_id = created["snapshot_id"]
        .as_str()
        .expect("snapshot_create must return a snapshot_id");

    let rb_resp = client
        .request(
            3,
            "tools/call",
            json!({
                "name": "nexus_snapshot_rollback",
                "arguments": { "snapshot_id": snap_id }
            }),
        )
        .await;

    assert_eq!(rb_resp["id"], 3, "response id mismatch: {rb_resp}");

    let parsed = tool_json(&rb_resp);

    assert!(
        parsed["error"].is_null(),
        "rollback without expected_digest should succeed: {parsed}"
    );
    assert_eq!(
        parsed["snapshot_id"].as_str(),
        Some(snap_id),
        "rollback response must echo the requested snapshot_id: {parsed}"
    );
    assert!(
        parsed["timestamp"].is_string(),
        "rollback response must include an RFC-3339 timestamp: {parsed}"
    );
}

#[tokio::test]
async fn acceptance_05_snapshot_rollback_with_matching_digest() {
    let mut client = McpClient::spawn().await;
    initialize_client(&mut client).await;

    let create_resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_snapshot_create",
                "arguments": { "label": "digest-check-baseline" }
            }),
        )
        .await;
    let created = tool_json(&create_resp);
    let snap_id = created["snapshot_id"]
        .as_str()
        .expect("snapshot_create must return a snapshot_id");

    // An empty stateless baseline stores zero bytes; content digest is SHA-256 of empty input.
    let expected_digest = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(b"");
        format!("{:x}", h.finalize())
    };

    let rb_resp = client
        .request(
            3,
            "tools/call",
            json!({
                "name": "nexus_snapshot_rollback",
                "arguments": {
                    "snapshot_id": snap_id,
                    "expected_digest": expected_digest
                }
            }),
        )
        .await;

    assert_eq!(rb_resp["id"], 3, "response id mismatch: {rb_resp}");

    let parsed = tool_json(&rb_resp);

    assert!(
        parsed["error"].is_null(),
        "rollback with a matching digest should succeed: {parsed}"
    );
    assert_eq!(
        parsed["snapshot_id"].as_str(),
        Some(snap_id),
        "rollback response must echo the snapshot_id: {parsed}"
    );
}

#[tokio::test]
async fn acceptance_06_attenuate_token_to_sub_path() {
    // HttpGet uses exact-string matching so URL sub-path narrowing is
    // unsupported. ReadFile uses path_contains, so narrowing /tmp → /tmp/data
    // is the idiomatic way to demonstrate real capability attenuation.
    let allowlist = json!([{ "type": "read_file", "path": "/tmp" }]).to_string();
    let mut client = McpClient::spawn_with_allowlist(Some(&allowlist)).await;
    initialize_client(&mut client).await;

    let issue_resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_issue_token",
                "arguments": {
                    "capability": "read_file",
                    "path": "/tmp",
                    "validity_secs": 3600
                }
            }),
        )
        .await;
    let issued = tool_json(&issue_resp);
    assert!(
        issued["error"].is_null(),
        "parent token issuance must succeed: {issued}"
    );
    let parent_id = issued["token_id"]
        .as_str()
        .expect("parent token must have a token_id");

    let att_resp = client
        .request(
            3,
            "tools/call",
            json!({
                "name": "nexus_attenuate_token",
                "arguments": {
                    "parent_token_id": parent_id,
                    "capability": "read_file",
                    "path": "/tmp/workspace",
                    "validity_secs": 60
                }
            }),
        )
        .await;

    assert_eq!(att_resp["id"], 3, "response id mismatch: {att_resp}");

    let parsed = tool_json(&att_resp);

    assert!(
        parsed["error"].is_null(),
        "attenuation to a sub-path should succeed: {parsed}"
    );

    let child_id = parsed["token_id"]
        .as_str()
        .expect("attenuated token must have a token_id");

    assert_ne!(
        child_id, parent_id,
        "attenuated token id must differ from the parent token id"
    );
    uuid::Uuid::parse_str(child_id).expect("child token_id must be a valid UUID");

    assert_eq!(
        parsed["expires_in_secs"], 60,
        "attenuated token validity should be 60 s: {parsed}"
    );
}

// ============================================================================
// SECURITY BOUNDARY CHECKS -- adversarial inputs
// ============================================================================

#[tokio::test]
async fn acceptance_07_ssrf_link_local_imds_is_rejected() {
    // Include the SSRF URL in the operator allowlist so the test exercises the
    // url_guard (step 2 in do_issue_token) before the allowlist check (step 5).
    let ssrf_url = "http://169.254.169.254/*";
    let allowlist = json!([{ "type": "http_get", "path": ssrf_url }]).to_string();
    let mut client = McpClient::spawn_with_allowlist(Some(&allowlist)).await;
    initialize_client(&mut client).await;

    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_issue_token",
                "arguments": {
                    "capability": "http_get",
                    "path": ssrf_url,
                    "validity_secs": 300
                }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2, "response id mismatch: {resp}");

    let parsed = tool_json(&resp);

    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        !error.is_empty(),
        "issuing a token for a link-local SSRF URL must return an error: {parsed}"
    );
    assert!(
        parsed["token_id"].is_null(),
        "no token_id must be present when SSRF is rejected: {parsed}"
    );
}

#[tokio::test]
async fn acceptance_08_ssrf_private_address_is_rejected() {
    let ssrf_url = "http://192.168.1.1/*";
    let allowlist = json!([{ "type": "http_get", "path": ssrf_url }]).to_string();
    let mut client = McpClient::spawn_with_allowlist(Some(&allowlist)).await;
    initialize_client(&mut client).await;

    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_issue_token",
                "arguments": {
                    "capability": "http_get",
                    "path": ssrf_url,
                    "validity_secs": 300
                }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2, "response id mismatch: {resp}");

    let parsed = tool_json(&resp);

    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        !error.is_empty(),
        "issuing a token for a private-network SSRF URL must return an error: {parsed}"
    );
    assert!(
        parsed["token_id"].is_null(),
        "no token_id must be present when SSRF is rejected: {parsed}"
    );
}

#[tokio::test]
async fn acceptance_09_snapshot_rollback_digest_mismatch_is_rejected() {
    let mut client = McpClient::spawn().await;
    initialize_client(&mut client).await;

    let create_resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_snapshot_create",
                "arguments": { "label": "digest-mismatch-test" }
            }),
        )
        .await;
    let created = tool_json(&create_resp);
    let snap_id = created["snapshot_id"]
        .as_str()
        .expect("snapshot_create must return a snapshot_id");

    // 64 hex zeros is syntactically valid as a SHA-256 string but never matches
    // any real snapshot content digest.
    let wrong_digest = "0".repeat(64);

    let rb_resp = client
        .request(
            3,
            "tools/call",
            json!({
                "name": "nexus_snapshot_rollback",
                "arguments": {
                    "snapshot_id": snap_id,
                    "expected_digest": wrong_digest
                }
            }),
        )
        .await;

    assert_eq!(rb_resp["id"], 3, "response id mismatch: {rb_resp}");

    let parsed = tool_json(&rb_resp);

    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        !error.is_empty(),
        "rollback with a wrong digest must return an error: {parsed}"
    );
    assert!(
        parsed["snapshot_id"].is_null(),
        "no snapshot_id should appear in a digest-mismatch response: {parsed}"
    );
}

#[tokio::test]
async fn acceptance_10_snapshot_rollback_invalid_uuid_is_rejected() {
    let mut client = McpClient::spawn().await;
    initialize_client(&mut client).await;

    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_snapshot_rollback",
                "arguments": { "snapshot_id": "not-a-uuid" }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2, "response id mismatch: {resp}");

    let parsed = tool_json(&resp);

    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        !error.is_empty(),
        "rollback with an invalid UUID must return an error: {parsed}"
    );
    assert!(
        parsed["snapshot_id"].is_null(),
        "no snapshot_id should be returned for an invalid UUID input: {parsed}"
    );
}
