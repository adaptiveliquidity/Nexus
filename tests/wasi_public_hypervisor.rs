use std::path::{Path, PathBuf};
use std::time::Duration;

use nexus::{
    Capability, HypervisorConfig, NexusError, NexusHypervisor, ToolDefinition, WasiAccess,
    WasiToolConfig,
};

fn demo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("wasi_capability_demo")
}

fn load_guest() -> Vec<u8> {
    std::fs::read(demo_dir().join("csv_reporter.wasm"))
        .expect("csv_reporter.wasm must be present for public WASI tests")
}

fn hypervisor() -> NexusHypervisor {
    NexusHypervisor::new(HypervisorConfig::default()).unwrap()
}

fn csv_tool() -> ToolDefinition {
    ToolDefinition::new("csv_reporter".to_string(), load_guest())
}

fn issue_mount_tokens(
    hv: &NexusHypervisor,
    input: &Path,
    output: &Path,
) -> Vec<nexus::CapabilityToken> {
    vec![
        hv.issue_token(
            Capability::ReadFile(input.canonicalize().unwrap()),
            "test",
            Duration::from_secs(60),
        )
        .unwrap(),
        hv.issue_token(
            Capability::ReadFile(output.canonicalize().unwrap()),
            "test",
            Duration::from_secs(60),
        )
        .unwrap(),
        hv.issue_token(
            Capability::WriteFile(output.canonicalize().unwrap()),
            "test",
            Duration::from_secs(60),
        )
        .unwrap(),
    ]
}

fn tool_config(input: &Path, output: &Path) -> WasiToolConfig {
    WasiToolConfig::new()
        .with_mount(input, "/input", WasiAccess::ReadOnly)
        .with_mount(output, "/output", WasiAccess::ReadWrite)
}

#[tokio::test]
async fn execute_tool_wasi_uses_guest_mount_aliases() {
    let hv = hypervisor();
    let input = demo_dir().join("input");
    let output = tempfile::tempdir().unwrap();
    let tokens = issue_mount_tokens(&hv, &input, output.path());

    let result = hv
        .execute_tool_wasi_with_config(
            csv_tool(),
            serde_json::json!({}),
            &tokens,
            tool_config(&input, output.path()),
        )
        .await
        .unwrap();

    assert!(result.success, "guest should succeed: {:?}", result.error);
    let report = std::fs::read_to_string(output.path().join("report.txt")).unwrap();
    assert!(report.contains("Order Summary Report"));
}

#[tokio::test]
async fn missing_read_capability_rejects_mount() {
    let hv = hypervisor();
    let input = demo_dir().join("input");
    let output = tempfile::tempdir().unwrap();

    let tokens_without_input_read = vec![
        hv.issue_token(
            Capability::ReadFile(output.path().canonicalize().unwrap()),
            "test",
            Duration::from_secs(60),
        )
        .unwrap(),
        hv.issue_token(
            Capability::WriteFile(output.path().canonicalize().unwrap()),
            "test",
            Duration::from_secs(60),
        )
        .unwrap(),
    ];

    let result = hv
        .execute_tool_wasi_with_config(
            csv_tool(),
            serde_json::json!({}),
            &tokens_without_input_read,
            tool_config(&input, output.path()),
        )
        .await;

    assert!(matches!(result, Err(NexusError::CapabilityDenied(_))));
}

#[tokio::test]
async fn missing_write_capability_rejects_mount() {
    let hv = hypervisor();
    let input = demo_dir().join("input");
    let output = tempfile::tempdir().unwrap();
    let tokens = vec![
        hv.issue_token(
            Capability::ReadFile(input.canonicalize().unwrap()),
            "test",
            Duration::from_secs(60),
        )
        .unwrap(),
        hv.issue_token(
            Capability::ReadFile(output.path().canonicalize().unwrap()),
            "test",
            Duration::from_secs(60),
        )
        .unwrap(),
    ];

    let result = hv
        .execute_tool_wasi_with_config(
            csv_tool(),
            serde_json::json!({}),
            &tokens,
            tool_config(&input, output.path()),
        )
        .await;

    assert!(matches!(result, Err(NexusError::CapabilityDenied(_))));
}

#[tokio::test]
async fn denied_wasi_config_does_not_create_missing_mount_dir() {
    let hv = hypervisor();
    let tmp = tempfile::tempdir().unwrap();
    let missing_mount = tmp.path().join("not-authorized-output");
    let config = WasiToolConfig::new().with_mount(&missing_mount, "/output", WasiAccess::ReadWrite);

    let result = hv
        .execute_tool_wasi_with_config(csv_tool(), serde_json::json!({}), &[], config)
        .await;

    assert!(matches!(result, Err(NexusError::CapabilityDenied(_))));
    assert!(
        !missing_mount.exists(),
        "missing WASI mount directory must not be created before authorization"
    );
}

#[test]
fn duplicate_guest_path_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let config = WasiToolConfig::new()
        .with_mount(dir.path(), "/input", WasiAccess::ReadOnly)
        .with_mount(dir.path(), "/input", WasiAccess::ReadOnly);

    assert!(matches!(config.validate(), Err(NexusError::ConfigError(_))));
}

#[test]
fn overlapping_guest_path_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let config = WasiToolConfig::new()
        .with_mount(dir.path(), "/input", WasiAccess::ReadOnly)
        .with_mount(dir.path(), "/input/private", WasiAccess::ReadOnly);

    assert!(matches!(config.validate(), Err(NexusError::ConfigError(_))));
}

#[test]
fn guest_path_traversal_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let config =
        WasiToolConfig::new().with_mount(dir.path(), "/input/../secret", WasiAccess::ReadOnly);

    assert!(matches!(config.validate(), Err(NexusError::ConfigError(_))));
}

#[test]
fn host_path_canonicalization_required() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("created-by-validation");
    let validated = WasiToolConfig::new()
        .with_mount(&nested, "/output", WasiAccess::ReadWrite)
        .validate()
        .unwrap();

    assert!(nested.exists());
    assert!(validated.sandbox_config.preopens[0].host_path.is_absolute());
}

#[test]
fn demo_uses_public_hypervisor_api() {
    let host_rs = std::fs::read_to_string(demo_dir().join("host.rs")).unwrap();
    assert!(host_rs.contains("execute_tool_wasi_with_config"));
    assert!(!host_rs.contains(".execute_wasi("));
}
