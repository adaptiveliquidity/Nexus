#![cfg(feature = "aeon-memory")]

use base64::{engine::general_purpose::STANDARD, Engine as _};
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
    async fn spawn_with_aeon(server: &MockAeonServer) -> Self {
        Self::spawn_with_aeon_options(Some(AeonEnv {
            base_url: server.base_url(),
            hmac_key: Some(test_hmac_key_hex()),
            management_key: Some(test_management_key()),
        }))
        .await
    }

    async fn spawn_without_hmac(server: &MockAeonServer) -> Self {
        Self::spawn_with_aeon_options(Some(AeonEnv {
            base_url: server.base_url(),
            hmac_key: None,
            management_key: Some(test_management_key()),
        }))
        .await
    }

    async fn spawn_with_unreachable_aeon() -> Self {
        Self::spawn_with_aeon_options(Some(AeonEnv {
            base_url: "http://127.0.0.1:1".to_string(),
            hmac_key: Some(test_hmac_key_hex()),
            management_key: Some(test_management_key()),
        }))
        .await
    }

    async fn spawn_with_aeon_options(aeon: Option<AeonEnv>) -> Self {
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

        if let Some(aeon) = aeon {
            command
                .env("NEXUS_AEON_ENABLED", "true")
                .env("NEXUS_AEON_BASE_URL", aeon.base_url)
                .env("NEXUS_AEON_AGENT_ID", "agent-1")
                .env("NEXUS_AEON_SESSION_ID", "session-1")
                .env("NEXUS_AEON_TIMEOUT_MS", "200");
            if let Some(management_key) = aeon.management_key {
                command.env("NEXUS_AEON_MANAGEMENT_KEY", management_key);
            }
            if let Some(hmac_key) = aeon.hmac_key {
                command.env("NEXUS_AEON_HMAC_KEY", hmac_key);
            }
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

struct AeonEnv {
    base_url: String,
    hmac_key: Option<String>,
    management_key: Option<String>,
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

fn iq_args() -> Value {
    json!({
        "tool_name": "nexus_iq_noop",
        "tool_wasm": noop_wasm_b64(),
        "input": serde_json::to_string(&json!({ "message": "hello" })).unwrap(),
        "aeon_agent_id": "agent-1",
        "aeon_session_id": "session-1"
    })
}

fn mode_from_response(parsed: &Value) -> MemoryAttestationMode {
    serde_json::from_value(parsed["memory_evidence_ref"]["attestation"].clone())
        .expect("memory_evidence_ref.attestation should deserialize")
}

async fn call_nexus_iq_execute(client: &mut McpClient, args: Value) -> Value {
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
    parsed
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

    fn path_count(&self, path: &str) -> usize {
        self.captured
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.path == path)
            .count()
    }

    fn first_body_for_path(&self, path: &str) -> Option<String> {
        self.captured
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.path == path)
            .map(|request| request.body.clone())
    }

    async fn wait_for_path(&self, path: &str, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.path_count(path) >= count {
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
async fn no_memory_query_gives_advisory() {
    let Some(server) = MockAeonServer::try_new(200, r#"{"results":[]}"#, 200) else {
        return;
    };
    let mut client = McpClient::spawn_with_aeon(&server).await;
    initialize_client(&mut client).await;

    let parsed = call_nexus_iq_execute(&mut client, iq_args()).await;

    assert_eq!(mode_from_response(&parsed), MemoryAttestationMode::Absent);
    assert_eq!(parsed["memory_hits_count"], 0);
    assert_eq!(server.path_count("/api/v1/memories/search"), 0);
}

#[tokio::test]
async fn memory_query_no_hits_gives_attested_no_hit() {
    let Some(server) = MockAeonServer::try_new(200, r#"{"results":[]}"#, 200) else {
        return;
    };
    let mut client = McpClient::spawn_with_aeon(&server).await;
    initialize_client(&mut client).await;

    let mut args = iq_args();
    args["memory_query"] = json!("recall without hits");
    let parsed = call_nexus_iq_execute(&mut client, args).await;

    assert_eq!(
        mode_from_response(&parsed),
        MemoryAttestationMode::AttestedNoHit
    );
    assert_eq!(parsed["memory_hits_count"], 0);
    server.wait_for_path("/api/v1/memories/search", 1).await;
    let search_body = server
        .first_body_for_path("/api/v1/memories/search")
        .expect("memory search request should be captured");
    assert!(
        search_body.contains("recall without hits"),
        "memory recall query should be forwarded: {search_body}"
    );
}

#[tokio::test]
async fn memory_query_with_hits_gives_attested_with_recall() {
    let Some(server) = MockAeonServer::try_new(
        200,
        r#"{"results":[{"id":"mem-1","content":"first context","score":0.91},{"id":"mem-2","content":"second context","score":0.87}]}"#,
        200,
    ) else {
        return;
    };
    let mut client = McpClient::spawn_with_aeon(&server).await;
    initialize_client(&mut client).await;

    let mut args = iq_args();
    args["memory_query"] = json!("recall with hits");
    let parsed = call_nexus_iq_execute(&mut client, args).await;

    assert_eq!(
        mode_from_response(&parsed),
        MemoryAttestationMode::AttestedWithRecall
    );
    assert_eq!(parsed["memory_hits_count"], 2);
    server.wait_for_path("/api/v1/memories/search", 1).await;
}

#[tokio::test]
async fn aeon_down_degrades_gracefully() {
    let mut client = McpClient::spawn_with_unreachable_aeon().await;
    initialize_client(&mut client).await;

    let mut args = iq_args();
    args["memory_query"] = json!("recall while aeon is down");
    let parsed = call_nexus_iq_execute(&mut client, args).await;
    let mode = mode_from_response(&parsed);

    assert!(
        matches!(
            mode,
            MemoryAttestationMode::Degraded | MemoryAttestationMode::Advisory
        ),
        "unexpected memory attestation mode when AEON is down: {mode:?}; response: {parsed}"
    );
    assert_eq!(parsed["memory_hits_count"], 0);
}

#[tokio::test]
async fn hit_digests_populated_on_recall() {
    let Some(server) = MockAeonServer::try_new(
        200,
        r#"{"results":[{"id":"mem-1","content":"first context","score":0.91},{"id":"mem-2","content":"second context","score":0.87}]}"#,
        200,
    ) else {
        return;
    };
    let mut client = McpClient::spawn_with_aeon(&server).await;
    initialize_client(&mut client).await;

    let mut args = iq_args();
    args["memory_query"] = json!("recall with digests");
    let parsed = call_nexus_iq_execute(&mut client, args).await;
    let hit_digests = parsed["memory_evidence_ref"]["hit_digests"]
        .as_array()
        .expect("hit_digests should be an array");

    assert_eq!(
        mode_from_response(&parsed),
        MemoryAttestationMode::AttestedWithRecall
    );
    assert_eq!(hit_digests.len(), 2);
    assert!(
        hit_digests
            .iter()
            .all(|digest| digest.as_str().is_some_and(|value| !value.is_empty())),
        "hit_digests should contain non-empty digest strings: {parsed}"
    );
}

#[tokio::test]
async fn absent_mode_when_no_hmac_key() {
    let Some(server) = MockAeonServer::try_new(
        200,
        r#"{"results":[{"id":"mem-1","content":"unattested context","score":0.91}]}"#,
        200,
    ) else {
        return;
    };
    let mut client = McpClient::spawn_without_hmac(&server).await;
    initialize_client(&mut client).await;

    let mut args = iq_args();
    args["memory_query"] = json!("recall without hmac key");
    let parsed = call_nexus_iq_execute(&mut client, args).await;
    let mode = mode_from_response(&parsed);

    assert!(
        matches!(
            mode,
            MemoryAttestationMode::Absent | MemoryAttestationMode::Advisory
        ),
        "unexpected memory attestation mode without NEXUS_AEON_HMAC_KEY: {mode:?}; response: {parsed}"
    );
}
