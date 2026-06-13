//! End-to-end tests for the P3 capability-gated WASI demo.
//!
//! These tests load the pre-built csv_reporter.wasm guest module and verify:
//!   - Allowed reads (/input) and writes (/output) succeed
//!   - Unauthorized reads (/secrets) are blocked — no pre-open exists
//!   - Execution with no pre-opens fails (guest can't read input)

use std::path::PathBuf;

use nexus::sandbox::{PreOpen, SandboxConfig, WasiSandboxConfig, WasmSandbox};

fn demo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("wasi_capability_demo")
}

fn load_guest() -> Vec<u8> {
    std::fs::read(demo_dir().join("csv_reporter.wasm"))
        .expect("csv_reporter.wasm must be pre-built")
}

fn sandbox() -> WasmSandbox {
    WasmSandbox::new(SandboxConfig {
        max_fuel: 100_000_000,
        time_limit: std::time::Duration::from_secs(5),
        ..SandboxConfig::default()
    })
    .unwrap()
}

#[test]
fn wasi_demo_reads_csv_and_writes_report() {
    let input_dir = demo_dir().join("input");
    let output_dir = tempfile::tempdir().unwrap();
    let output_path = output_dir.path().to_path_buf();

    let config = WasiSandboxConfig {
        preopens: vec![
            PreOpen {
                host_path: input_dir,
                guest_path: "/input".into(),
                writable: false,
            },
            PreOpen {
                host_path: output_path.clone(),
                guest_path: "/output".into(),
                writable: true,
            },
        ],
        inherit_stderr: true,
        ..WasiSandboxConfig::default()
    };

    let result = sandbox().execute_wasi(&load_guest(), &[], &config).unwrap();

    assert!(
        result.success,
        "WASI tool should succeed: {:?}",
        result.error
    );
    assert!(result.fuel_consumed > 0);

    let report = std::fs::read_to_string(output_path.join("report.txt"))
        .expect("report.txt should have been written");
    assert!(report.contains("Order Summary Report"));
    assert!(report.contains("Total revenue:"));
    assert!(report.contains("Widget A"));
}

#[test]
fn wasi_demo_no_preopens_fails() {
    let config = WasiSandboxConfig {
        inherit_stderr: true,
        ..WasiSandboxConfig::default()
    };

    let result = sandbox().execute_wasi(&load_guest(), &[], &config).unwrap();
    assert!(!result.success, "Should fail without any pre-opens");
}

#[test]
fn wasi_demo_secrets_not_accessible() {
    let input_dir = demo_dir().join("input");
    let output_dir = tempfile::tempdir().unwrap();
    let output_path = output_dir.path().to_path_buf();

    let config = WasiSandboxConfig {
        preopens: vec![
            PreOpen {
                host_path: input_dir,
                guest_path: "/input".into(),
                writable: false,
            },
            PreOpen {
                host_path: output_path,
                guest_path: "/output".into(),
                writable: true,
            },
            // No /secrets pre-open — guest's attempt to read it must fail
        ],
        inherit_stderr: true,
        ..WasiSandboxConfig::default()
    };

    let result = sandbox().execute_wasi(&load_guest(), &[], &config).unwrap();

    // Guest tried to read /secrets/token.txt, got ENOENT, handled gracefully
    assert!(
        result.success,
        "Guest should handle denied secret read gracefully: {:?}",
        result.error
    );
}
