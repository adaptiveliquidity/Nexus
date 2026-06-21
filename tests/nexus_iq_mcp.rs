#![cfg(feature = "aeon-memory")]

use base64::{engine::general_purpose::STANDARD, Engine as _};
use nexus::aeon::{MemoryEvidenceV1, MemoryHit};
use nexus::proof::schema::MemoryAttestationMode;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::{sleep, timeout};

fn cargo_bin(name: &str) -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.push(name);
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
    async fn spawn(server: &MockAeonServer, allowlist: Option<String>) -> Self {
        Self::spawn_with_aeon_base_url(Some(server.base_url()), allowlist).await
    }

    async fn spawn_without_aeon(allowlist: Option<String>) -> Self {
        Self::spawn_with_aeon_base_url(None, allowlist).await
    }

    async fn spawn_with_aeon_base_url(
        aeon_base_url: Option<String>,
        allowlist: Option<String>,
    ) -> Self {
        let bin = cargo_bin("nexus-mcp");
        assert!(bin.exists(), "nexus-mcp binary not found at {:?}", bin);

        let mut command = Command::new(&bin);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .env_remove("NEXUS_AEON_ENABLED")
            .env_remove("NEXUS_AEON_BASE_URL")
            .env_remove("NEXUS_AEON_AGENT_ID")
            .env_remove("NEXUS_AEON_SESSION_ID")
            .env_remove("NEXUS_AEON_TIMEOUT_MS")
            .env_remove("NEXUS_AEON_MANAGEMENT_KEY")
            .env_remove("NEXUS_AEON_HMAC_KEY")
            .env_remove("NEXUS_MCP_CAPABILITY_ALLOWLIST")
            .env_remove("NEXUS_MCP_PROFILE");
        if let Some(aeon_base_url) = aeon_base_url {
            command
                .env("NEXUS_AEON_ENABLED", "true")
                .env("NEXUS_AEON_BASE_URL", aeon_base_url)
                .env("NEXUS_AEON_AGENT_ID", "agent-1")
                .env("NEXUS_AEON_SESSION_ID", "session-1")
                .env("NEXUS_AEON_TIMEOUT_MS", "200")
                .env("NEXUS_AEON_MANAGEMENT_KEY", test_management_key())
                .env("NEXUS_AEON_HMAC_KEY", test_hmac_key_hex());
        }
        if let Some(allowlist) = allowlist {
            command.env("NEXUS_MCP_CAPABILITY_ALLOWLIST", allowlist);
        }

        let mut child = command.spawn().expect("failed to spawn nexus-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        Self {
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
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await;
        self.recv().await
    }
}

fn test_hmac_key_hex() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn test_management_key() -> String {
    format!("test-mgmt-{}", uuid::Uuid::new_v4())
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

fn tool_json(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response should contain text content");
    serde_json::from_str(text).expect("tool response text should be JSON")
}

fn noop_wasm_b64() -> String {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();
    STANDARD.encode(wasm)
}

fn iq_args(_server: &MockAeonServer) -> Value {
    iq_args_stub()
}

fn iq_args_stub() -> Value {
    json!({
        "tool_name": "nexus_iq_noop",
        "tool_wasm": noop_wasm_b64(),
        "input": serde_json::to_string(&json!({ "message": "hello" })).unwrap(),
        "aeon_agent_id": "agent-1",
        "aeon_session_id": "session-1"
    })
}

#[derive(Debug, Clone)]
struct CapturedRequest {
    path: String,
    body: String,
}

struct MockAeonServer {
    addr: std::net::SocketAddr,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
    shutdown: Arc<AtomicBool>,
}

impl MockAeonServer {
    fn try_new(memory_status: u16, memory_body: &str, timeline_status: u16) -> Option<Self> {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping loopback AEON mock test: {error}");
                return None;
            }
            Err(error) => panic!("failed to bind loopback AEON mock: {error}"),
        };
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();

        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_thread = Arc::clone(&captured);
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_thread = Arc::clone(&shutdown);
        let memory_body = memory_body.to_string();

        std::thread::spawn(move || {
            while !shutdown_for_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => handle_http(
                        stream,
                        memory_status,
                        &memory_body,
                        timeline_status,
                        &captured_for_thread,
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Some(Self {
            addr,
            captured,
            shutdown,
        })
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    async fn wait_for_path(&self, path: &str, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let seen = self
                .captured
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.path == path)
                .count();
            if seen >= count {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {count} request(s) to {path}");
    }
}

impl Drop for MockAeonServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }
}

fn handle_http(
    mut stream: TcpStream,
    memory_status: u16,
    memory_body: &str,
    timeline_status: u16,
    captured: &Arc<Mutex<Vec<CapturedRequest>>>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 1024];
    while !buf.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => return,
        }
    }

    let header_end = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
        .unwrap_or(buf.len());
    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let path = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let content_len = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buf.len().saturating_sub(header_end) < content_len {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    captured.lock().unwrap().push(CapturedRequest {
        path: path.clone(),
        body,
    });

    let (status, response_body) = if path == "/api/v1/memories/search" {
        (memory_status, memory_body)
    } else if path == "/api/v1/agents/agent-1/timeline" {
        (timeline_status, "{}")
    } else {
        (404, r#"{"error":"not found"}"#)
    };
    let status_text = if (200..300).contains(&status) {
        "OK"
    } else {
        "ERROR"
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
        response_body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

#[tokio::test]
async fn nexus_iq_execute_full_loop() {
    let Some(server) = MockAeonServer::try_new(
        200,
        r#"{"results":[{"id":"mem-1","content":"previous context","score":0.91}]}"#,
        200,
    ) else {
        return;
    };
    let mut client = McpClient::spawn(&server, None).await;
    initialize_client(&mut client).await;

    let mut args = iq_args(&server);
    args["memory_query"] = json!("recall context");
    args["memory_limit"] = json!(5);

    let resp = client
        .request(
            2,
            "tools/call",
            json!({ "name": "nexus_iq_execute", "arguments": args }),
        )
        .await;
    let parsed = tool_json(&resp);

    assert_eq!(parsed["denied"], false, "unexpected denial: {parsed}");
    assert!(
        parsed["proof_capsule_ref"].is_string(),
        "missing proof ref: {parsed}"
    );
    assert_eq!(parsed["memory_hits_count"], 1);
    assert_eq!(
        parsed["memory_evidence_ref"]["attestation"],
        "AttestedWithRecall"
    );
    assert!(parsed["memory_evidence_ref"]["capsule_digest"].is_string());
    assert_eq!(parsed["timeline_status"], "fire_and_forget");
    server.wait_for_path("/api/v1/memories/search", 1).await;
    server
        .wait_for_path("/api/v1/agents/agent-1/timeline", 1)
        .await;
    let captured = server.captured.lock().unwrap();
    let search = captured
        .iter()
        .find(|request| request.path == "/api/v1/memories/search")
        .expect("search request should be captured");
    assert!(
        search.body.contains("recall context"),
        "memory recall query should be forwarded: {}",
        search.body
    );
}

#[tokio::test]
async fn nexus_iq_execute_no_memory() {
    let mut client = McpClient::spawn_without_aeon(None).await;
    initialize_client(&mut client).await;

    let resp = client
        .request(
            2,
            "tools/call",
            json!({ "name": "nexus_iq_execute", "arguments": iq_args_stub() }),
        )
        .await;
    let parsed = tool_json(&resp);

    assert_eq!(parsed["denied"], false, "unexpected denial: {parsed}");
    assert!(
        parsed["proof_capsule_ref"].is_string(),
        "missing proof ref: {parsed}"
    );
    assert_eq!(parsed["memory_hits_count"], 0);
    assert_eq!(parsed["memory_evidence_ref"]["attestation"], "Absent");
}

#[tokio::test]
async fn nexus_iq_execute_capability_denied() {
    let mut client = McpClient::spawn_without_aeon(None).await;
    initialize_client(&mut client).await;

    let mut args = iq_args_stub();
    args["required_capabilities"] = json!(["read:/denied"]);

    let resp = client
        .request(
            2,
            "tools/call",
            json!({ "name": "nexus_iq_execute", "arguments": args }),
        )
        .await;
    let parsed = tool_json(&resp);

    assert_eq!(parsed["denied"], true, "expected denial: {parsed}");
    assert_eq!(parsed["denial_negotiation"]["denied"], true);
    assert_eq!(parsed["denial_negotiation"]["negotiated"], false);
    assert!(
        parsed["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "capability_denied"),
        "expected CapabilityDenied event: {parsed}"
    );
}

#[tokio::test]
async fn nexus_iq_execute_attested_delivery_failure() {
    let mut client = McpClient::spawn_without_aeon(None).await;
    initialize_client(&mut client).await;

    let mut args = iq_args_stub();
    args["attestation_mode"] = json!("attested");

    let resp = client
        .request(
            2,
            "tools/call",
            json!({ "name": "nexus_iq_execute", "arguments": args }),
        )
        .await;
    let parsed = tool_json(&resp);

    assert_eq!(parsed["denied"], false, "unexpected denial: {parsed}");
    assert_eq!(parsed["attestation_mode"], "attested");
    assert_eq!(parsed["timeline_status"], "required_but_failed");
}

#[tokio::test]
async fn verify_memory_evidence_cli_valid() {
    let tmp = tempfile::tempdir().unwrap();
    let evidence_path = tmp.path().join("memory-evidence-valid.json");
    let hit = MemoryHit {
        id: "mem-1".to_string(),
        content: "context".to_string(),
        score: Some(0.9),
    };
    let evidence =
        MemoryEvidenceV1::new("context", &[hit], MemoryAttestationMode::AttestedWithRecall)
            .with_capsule_digest(Some("capsule-1".to_string()));
    let mut evidence_json = serde_json::to_value(&evidence).unwrap();
    evidence_json["capsule_id"] = json!("capsule-1");
    std::fs::write(&evidence_path, serde_json::to_vec(&evidence_json).unwrap()).unwrap();

    let output = Command::new(cargo_bin("nexus"))
        .arg("aeon")
        .arg("verify-memory-evidence")
        .arg("--capsule-id")
        .arg("capsule-1")
        .arg(&evidence_path)
        .output()
        .await
        .expect("run nexus verifier");

    assert!(output.status.success(), "verifier should exit successfully");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "VALID");
}

#[tokio::test]
async fn verify_memory_evidence_cli_capsule_id_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let evidence_path = tmp.path().join("memory-evidence-capsule-mismatch.json");
    let evidence = MemoryEvidenceV1::new("context", &[], MemoryAttestationMode::AttestedNoHit);
    let mut evidence_json = serde_json::to_value(&evidence).unwrap();
    evidence_json["capsule_id"] = json!("capsule-actual");
    std::fs::write(&evidence_path, serde_json::to_vec(&evidence_json).unwrap()).unwrap();

    let output = Command::new(cargo_bin("nexus"))
        .arg("aeon")
        .arg("verify-memory-evidence")
        .arg("--capsule-id")
        .arg("capsule-expected")
        .arg(&evidence_path)
        .output()
        .await
        .expect("run nexus verifier");

    assert!(output.status.success(), "verifier should exit successfully");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("INVALID: capsule_id mismatch:"),
        "expected capsule mismatch output, got: {stdout}"
    );
}

#[tokio::test]
async fn verify_memory_evidence_cli_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    let evidence_path = tmp.path().join("memory-evidence-invalid.json");
    std::fs::write(
        &evidence_path,
        serde_json::to_vec(&json!({
            "version": 1,
            "query": "context",
            "hit_count": 2,
            "hit_digests": ["abc"],
            "attestation": "Attested"
        }))
        .unwrap(),
    )
    .unwrap();

    let output = Command::new(cargo_bin("nexus"))
        .arg("aeon")
        .arg("verify-memory-evidence")
        .arg(&evidence_path)
        .output()
        .await
        .expect("run nexus verifier");

    assert!(output.status.success(), "verifier should exit successfully");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("INVALID:"),
        "expected INVALID output, got: {stdout}"
    );
}
