use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use nexus::{
    Capability, HypervisorConfig, NexusError, NexusHypervisor, ToolDefinition, WasiAccess,
    WasiToolConfig,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

const NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV: &str = "NEXUS_MCP_CAPABILITY_ALLOWLIST";
const NEXUS_MCP_MODULE_DIR_ENV: &str = "NEXUS_MCP_MODULE_DIR";
const NEXUS_MCP_PROFILE_ENV: &str = "NEXUS_MCP_PROFILE";

fn nexus_mcp_bin() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_nexus-mcp") {
        return PathBuf::from(path);
    }

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
    async fn spawn_with_module_dir(module_dir: &Path) -> Self {
        let bin = nexus_mcp_bin();
        assert!(
            bin.exists(),
            "nexus-mcp binary not found at {:?}; run `cargo build --bin nexus-mcp` first",
            bin
        );

        let mut command = Command::new(bin);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .env(NEXUS_MCP_MODULE_DIR_ENV, module_dir)
            .env_remove(NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV)
            .env_remove(NEXUS_MCP_PROFILE_ENV);

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

async fn initialize_client(client: &mut McpClient) {
    client
        .request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "security-hardening-test", "version": "0.1.0" }
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

fn noop_wasm() -> Vec<u8> {
    wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap()
}

#[tokio::test]
async fn wasm_path_outside_module_dir_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let allowed = tmp.path().join("modules");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let outside_wasm = outside.join("evil.wasm");
    std::fs::write(&outside_wasm, noop_wasm()).unwrap();

    let mut client = McpClient::spawn_with_module_dir(&allowed).await;
    initialize_client(&mut client).await;

    let resp = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "nexus_execute",
                "arguments": { "wasm_path": outside_wasm }
            }),
        )
        .await;

    assert_eq!(resp["id"], 2);
    let parsed = tool_json(&resp);
    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("wasm path is not accessible"),
        "expected MCP module-dir allowlist rejection, got: {parsed}"
    );
}

#[tokio::test]
async fn wasi_mount_path_traversal_is_rejected() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let allowed = tmp.path().join("allowed");
    let denied = tmp.path().join("denied");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&denied).unwrap();

    let traversal_mount = allowed.join("..").join("denied");
    let token = hv
        .issue_token(
            Capability::ReadFile(allowed.canonicalize().unwrap()),
            "security-test",
            Duration::from_secs(60),
        )
        .unwrap();
    let tool = ToolDefinition::new("wasi_traversal".to_string(), noop_wasm());
    let config = WasiToolConfig::new().with_mount(&traversal_mount, "/data", WasiAccess::ReadOnly);

    let result = hv
        .execute_tool_wasi_with_config(tool, serde_json::json!({}), &[token], config)
        .await;

    match result {
        Err(NexusError::CapabilityDenied(message)) => {
            let denied_canonical = denied.canonicalize().unwrap();
            assert!(
                message.contains(&denied_canonical.display().to_string()),
                "denial should be for the canonical escaped path, got: {message}"
            );
        }
        other => panic!("expected canonicalized traversal mount to be denied, got: {other:?}"),
    }
}

#[test]
fn required_capabilities_does_not_create_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let missing_mount = tmp.path().join("nexus_test_dir_that_should_not_exist");
    assert!(
        !missing_mount.exists(),
        "test setup expected a nonexistent mount path"
    );

    let config = WasiToolConfig::new().with_mount(&missing_mount, "/output", WasiAccess::ReadWrite);
    let capabilities = config
        .required_capabilities()
        .expect("required_capabilities should derive caps without creating dirs");
    let expected_mount = tmp
        .path()
        .canonicalize()
        .unwrap()
        .join("nexus_test_dir_that_should_not_exist");

    assert!(
        capabilities.contains(&Capability::ReadFile(expected_mount.clone())),
        "expected read capability for missing mount, got: {capabilities:?}"
    );
    assert!(
        capabilities.contains(&Capability::WriteFile(expected_mount)),
        "expected write capability for missing mount, got: {capabilities:?}"
    );
    assert!(
        !missing_mount.exists(),
        "required_capabilities must not create host mount directories before authorization"
    );
}

#[tokio::test]
async fn infinite_wasm_is_cancelled_within_timeout() {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start")
                (loop $spin
                    br $spin))
        )"#,
    )
    .unwrap();

    let mut config = HypervisorConfig::default();
    config.sandbox_config.max_fuel = u64::MAX / 4;
    config.sandbox_config.time_limit = Duration::from_millis(100);

    let hv = NexusHypervisor::new(config).unwrap();
    let tool = ToolDefinition::new("infinite_loop_timeout".to_string(), wasm);

    let start = Instant::now();
    let output = timeout(
        Duration::from_secs(3),
        hv.execute_tool(tool, serde_json::json!({})),
    )
    .await
    .expect("infinite wasm execution should not block past the outer timeout")
    .expect("hypervisor should return a tool output for timeout failures");

    assert!(!output.success, "infinite loop should fail by timeout");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "timeout cancellation should finish within the bounded join window"
    );
}
