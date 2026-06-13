//! Integration tests for WASI sandbox execution gated by capability tokens.

use nexus::sandbox::{SandboxConfig, WasiSandboxConfig, WasmSandbox};
use nexus::security::Capability;
use std::path::PathBuf;

fn sandbox() -> WasmSandbox {
    WasmSandbox::new(SandboxConfig::default()).unwrap()
}

/// A minimal WASI module that calls `proc_exit(0)` — proves the WASI
/// linker is wired up (an empty-linker sandbox would trap on the import).
fn wasi_exit_module() -> Vec<u8> {
    wat::parse_str(
        r#"(module
            (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
            (memory (export "memory") 1)
            (func (export "_start") (call $exit (i32.const 0)))
        )"#,
    )
    .unwrap()
}

/// A pure-compute module (no WASI imports) should still work through the
/// WASI path — the linker has WASI bindings but the module doesn't use them.
fn pure_compute_module() -> Vec<u8> {
    wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "g") (mut i32) (i32.const 0))
            (func (export "_start") (global.set 0 (i32.const 42)))
        )"#,
    )
    .unwrap()
}

#[test]
fn wasi_execution_with_empty_config() {
    let sb = sandbox();
    let config = WasiSandboxConfig::default();
    let result = sb
        .execute_wasi(&pure_compute_module(), &[], &config)
        .unwrap();
    assert!(result.success, "pure-compute module succeeds on WASI path");
}

#[test]
fn wasi_execution_with_wasi_imports() {
    let sb = sandbox();
    let config = WasiSandboxConfig::default();
    let result = sb.execute_wasi(&wasi_exit_module(), &[], &config).unwrap();
    // proc_exit(0) is a success exit
    assert!(
        result.success || result.fuel_consumed > 0,
        "WASI module ran (may trap on proc_exit, but linker resolved the import)"
    );
}

#[test]
fn from_capabilities_maps_read_file() {
    let caps = vec![Capability::ReadFile(PathBuf::from("/data"))];
    let config = WasiSandboxConfig::from_capabilities(&caps);
    assert_eq!(config.preopens.len(), 1);
    assert!(!config.preopens[0].writable);
    assert!(!config.inherit_stdout);
}

#[test]
fn from_capabilities_maps_write_file() {
    let caps = vec![Capability::WriteFile(PathBuf::from("/out"))];
    let config = WasiSandboxConfig::from_capabilities(&caps);
    assert_eq!(config.preopens.len(), 1);
    assert!(config.preopens[0].writable);
}

#[test]
fn from_capabilities_read_then_write_upgrades_to_writable() {
    let caps = vec![
        Capability::ReadFile(PathBuf::from("/data")),
        Capability::WriteFile(PathBuf::from("/data")),
    ];
    let config = WasiSandboxConfig::from_capabilities(&caps);
    assert_eq!(config.preopens.len(), 1, "deduped to one preopen");
    assert!(config.preopens[0].writable, "upgraded to writable");
}

#[test]
fn from_capabilities_all_inherits_stdio() {
    let caps = vec![Capability::All];
    let config = WasiSandboxConfig::from_capabilities(&caps);
    assert!(config.inherit_stdout);
    assert!(config.inherit_stderr);
}

#[test]
fn from_capabilities_ignores_non_fs() {
    let caps = vec![
        Capability::HttpGet("https://example.com".into()),
        Capability::None,
    ];
    let config = WasiSandboxConfig::from_capabilities(&caps);
    assert!(config.preopens.is_empty());
    assert!(!config.inherit_stdout);
}

#[test]
fn wasi_invalid_module_returns_failure() {
    let sb = sandbox();
    let config = WasiSandboxConfig::default();
    let result = sb.execute_wasi(b"not valid wasm", &[], &config).unwrap();
    assert!(!result.success);
}

#[test]
fn wasi_missing_entrypoint_returns_failure() {
    let wasm = wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap();
    let sb = sandbox();
    let config = WasiSandboxConfig::default();
    let result = sb.execute_wasi(&wasm, &[], &config).unwrap();
    assert!(!result.success, "no _start or main → failure");
}
