#![cfg(feature = "aeon-memory")]
#![allow(dead_code)]

use base64::{engine::general_purpose::STANDARD, Engine as _};
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

pub(crate) fn cargo_bin(name: &str) -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.push(name);
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

pub(crate) struct McpClient {
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    _child: tokio::process::Child,
}

impl McpClient {
    pub(crate) async fn spawn(server: &MockAeonServer, allowlist: Option<String>) -> Self {
        Self::spawn_with_aeon_base_url(Some(server.base_url()), allowlist).await
    }

    pub(crate) async fn spawn_without_aeon(allowlist: Option<String>) -> Self {
        Self::spawn_with_aeon_base_url(None, allowlist).await
    }

    pub(crate) async fn spawn_with_aeon_base_url(
        aeon_base_url: Option<String>,
        allowlist: Option<String>,
    ) -> Self {
        Self::spawn_with_extra_env(aeon_base_url, allowlist, []).await
    }

    pub(crate) async fn spawn_with_extra_env<const N: usize>(
        aeon_base_url: Option<String>,
        allowlist: Option<String>,
        extra_env: [(&str, String); N],
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
            .env_remove("NEXUS_IQ_ALLOWLIST")
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
        for (key, value) in extra_env {
            command.env(key, value);
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

    pub(crate) async fn send(&mut self, msg: &Value) {
        let mut line = serde_json::to_string(msg).unwrap();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    pub(crate) async fn recv(&mut self) -> Value {
        let mut buf = String::new();
        timeout(Duration::from_secs(10), self.reader.read_line(&mut buf))
            .await
            .expect("timeout waiting for MCP response")
            .expect("IO error reading MCP response");
        serde_json::from_str(buf.trim()).expect("invalid JSON from MCP server")
    }

    pub(crate) async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
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

pub(crate) async fn initialize_client(client: &mut McpClient) {
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

pub(crate) fn tool_json(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response should contain text content");
    serde_json::from_str(text).expect("tool response text should be JSON")
}

pub(crate) fn noop_wasm_b64() -> String {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();
    STANDARD.encode(wasm)
}

pub(crate) fn iq_args(_server: &MockAeonServer) -> Value {
    iq_args_stub()
}

pub(crate) fn iq_args_stub() -> Value {
    json!({
        "tool_name": "nexus_iq_noop",
        "tool_wasm": noop_wasm_b64(),
        "input": serde_json::to_string(&json!({ "message": "hello" })).unwrap(),
        "aeon_agent_id": "agent-1",
        "aeon_session_id": "session-1"
    })
}

#[derive(Debug, Clone)]
pub(crate) struct CapturedRequest {
    pub(crate) path: String,
    pub(crate) body: String,
}

pub(crate) struct MockAeonServer {
    addr: std::net::SocketAddr,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
    shutdown: Arc<AtomicBool>,
}

impl MockAeonServer {
    pub(crate) fn try_new(
        memory_status: u16,
        memory_body: &str,
        timeline_status: u16,
    ) -> Option<Self> {
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

    pub(crate) fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub(crate) fn captured_requests(&self) -> Vec<CapturedRequest> {
        self.captured.lock().unwrap().clone()
    }

    pub(crate) async fn wait_for_path(&self, path: &str, count: usize) {
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
