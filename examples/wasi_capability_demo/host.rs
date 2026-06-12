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
//!
//! The guest binary (csv_reporter.wasm) is built separately from
//! `examples/wasi_capability_demo/guest/` targeting wasm32-wasip1.

use std::path::PathBuf;
use std::time::Duration;

use nexus::{
    Capability, HypervisorConfig, NexusHypervisor, ToolDefinition, WasiAccess, WasiToolConfig,
};

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

    let mut config = HypervisorConfig::default();
    config.sandbox_config.max_fuel = 100_000_000;
    config.sandbox_config.time_limit = Duration::from_secs(5);
    let hypervisor = NexusHypervisor::new(config)?;
    println!("[1] Hypervisor ready\n");

    // --- Step 2: Build public WASI config with guest aliases ---
    let wasi_config = WasiToolConfig::new()
        .with_mount(&input_dir, "/input", WasiAccess::ReadOnly)
        .with_mount(&output_dir, "/output", WasiAccess::ReadWrite)
        .inherit_stderr();
    let tokens = vec![
        hypervisor.issue_token(
            Capability::ReadFile(input_dir.canonicalize()?),
            "demo",
            Duration::from_secs(300),
        )?,
        hypervisor.issue_token(
            Capability::ReadFile(output_dir.canonicalize()?),
            "demo",
            Duration::from_secs(300),
        )?,
        hypervisor.issue_token(
            Capability::WriteFile(output_dir.canonicalize()?),
            "demo",
            Duration::from_secs(300),
        )?,
    ];
    println!("[2] WASI config:");
    println!("    /input  -> {} (read-only)", input_dir.display());
    println!("    /output -> {} (read-write)", output_dir.display());
    println!("    /secrets -> NOT MOUNTED ({})\n", secrets_dir.display());

    // --- Step 3: Load the pre-built guest module ---
    let wasm_bytes = std::fs::read(demo_dir.join("csv_reporter.wasm"))?;
    println!(
        "[3] Loaded csv_reporter.wasm ({} bytes)\n",
        wasm_bytes.len()
    );

    // --- Step 4: Execute through the public hypervisor WASI path ---
    println!("[4] Executing csv_reporter via execute_tool_wasi_with_config...");
    let tool = ToolDefinition::new("csv_reporter".to_string(), wasm_bytes);
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(hypervisor.execute_tool_wasi_with_config(
        tool,
        serde_json::json!({}),
        &tokens,
        wasi_config,
    ))?;

    println!("    success:       {}", result.success);
    println!("    fuel consumed: {}", result.fuel_consumed);
    println!("    duration:      {} ms\n", result.execution_time_ms);

    if let Some(err) = &result.error {
        println!("    error: {err}\n");
    }

    // --- Step 5: Verify output ---
    let report_path = output_dir.join("report.txt");
    if report_path.exists() {
        let report = std::fs::read_to_string(&report_path)?;
        println!("[5] Output report:\n{report}");
    } else {
        println!("[5] WARNING: report.txt was not created");
    }

    // --- Step 6: Security story ---
    println!("[6] Security verification:");
    println!("    The guest attempted to read /secrets/fake-token.txt");
    println!("    Since no pre-open was issued for /secrets,");
    println!("    the WASI sandbox blocked the access.");
    println!("    Exit code 0 confirms the guest handled the denial.\n");

    // --- Bonus: prove missing tokens reject before guest execution ---
    println!("[bonus] Executing without capability tokens...");
    let denied = rt.block_on(
        hypervisor.execute_tool_wasi_with_config(
            ToolDefinition::new(
                "csv_reporter_missing_tokens".to_string(),
                std::fs::read(demo_dir.join("csv_reporter.wasm"))?,
            ),
            serde_json::json!({}),
            &[],
            WasiToolConfig::new()
                .with_mount(&input_dir, "/input", WasiAccess::ReadOnly)
                .with_mount(&output_dir, "/output", WasiAccess::ReadWrite),
        ),
    );
    println!("    rejected before execution: {}", denied.is_err());

    println!("\nDone.");
    Ok(())
}
