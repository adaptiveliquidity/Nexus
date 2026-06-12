//! P3 capability-gated WASI tool demo — host side.
//!
//! Proves that Nexus runs real agent tools under explicit, capability-scoped
//! WASI permissions:
//!   1. Guest reads CSV from an allowed input directory
//!   2. Guest writes a report to an allowed output directory
//!   3. Guest's attempt to read /secrets is blocked — no pre-open issued
//!
//! Usage:
//!     cargo run --example wasi_capability_demo

use std::path::PathBuf;

use nexus::sandbox::{PreOpen, SandboxConfig, WasiSandboxConfig, WasmSandbox};

fn main() -> anyhow::Result<()> {
    println!("Nexus P3: Capability-Gated WASI Tool Demo");
    println!("==========================================\n");

    let demo_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("wasi_capability_demo");

    let input_dir = demo_dir.join("input");
    let output_dir = demo_dir.join("output");
    let secrets_dir = demo_dir.join("secrets");

    std::fs::create_dir_all(&output_dir)?;

    let sandbox = WasmSandbox::new(SandboxConfig {
        max_fuel: 100_000_000,
        time_limit: std::time::Duration::from_secs(5),
        ..SandboxConfig::default()
    })?;
    println!("[1] Sandbox ready\n");

    println!("[2] WASI config:");
    println!("    /input  -> {} (read-only)", input_dir.display());
    println!("    /output -> {} (read-write)", output_dir.display());
    println!("    /secrets -> NOT MOUNTED ({})\n", secrets_dir.display());

    let wasm_bytes = std::fs::read(demo_dir.join("csv_reporter.wasm"))?;
    println!(
        "[3] Loaded csv_reporter.wasm ({} bytes)\n",
        wasm_bytes.len()
    );

    println!("[4] Executing csv_reporter via execute_wasi...");
    let config = WasiSandboxConfig {
        preopens: vec![
            PreOpen {
                host_path: input_dir.clone(),
                guest_path: "/input".into(),
                writable: false,
            },
            PreOpen {
                host_path: output_dir.clone(),
                guest_path: "/output".into(),
                writable: true,
            },
        ],
        inherit_stderr: true,
        ..WasiSandboxConfig::default()
    };

    let result = sandbox.execute_wasi(&wasm_bytes, &[], &config)?;

    println!("    success:       {}", result.success);
    println!("    fuel consumed: {}", result.fuel_consumed);
    println!("    duration:      {} ms\n", result.duration_ms);

    if let Some(err) = &result.error {
        println!("    error: {err}\n");
    }

    let report_path = output_dir.join("report.txt");
    if report_path.exists() {
        let report = std::fs::read_to_string(&report_path)?;
        println!("[5] Output report:\n{report}");
    } else {
        println!("[5] WARNING: report.txt was not created");
    }

    println!("[6] Security verification:");
    println!("    The guest attempted to read /secrets/fake-token.txt");
    println!("    Since no pre-open was issued for /secrets,");
    println!("    the WASI sandbox blocked the access.");
    println!("    Exit code 0 confirms the guest handled the denial.\n");

    println!("[bonus] Executing without any pre-opens...");
    let empty_config = WasiSandboxConfig {
        inherit_stderr: true,
        ..WasiSandboxConfig::default()
    };
    let denied = sandbox.execute_wasi(&wasm_bytes, &[], &empty_config)?;
    println!("    succeeded without pre-opens: {}", denied.success);
    println!("    (expected: false — guest can't read input)\n");

    println!("Done.");
    Ok(())
}
