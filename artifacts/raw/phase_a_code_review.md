# Phase A Code Review — Nexus WASM Sandbox Defect Cleanup

**Reviewer:** Cursor `code-reviewer` subagent (Claude Opus 4.7)
**Plan:** `c:\Users\Benna\.cursor\plans\nexus_next-benchmark_build_f3c30e30.plan.md`
**Scope:** files touched by Phase A todos `phaseA-failuremode` … `phaseA-regression-test`
**Date:** Sun Jun 7 2026

---

## Summary verdict

**Approve with changes.**

Phase A delivers what the plan promised: a typed `FailureMode` taxonomy, a `RecoveryPolicy` trait with a failure-specific `StaticPolicy`, real `pre_call_memory` capture, correct `rollback_performed` semantics on load-time failures, real `ResourceSnapshot`s in `ExecutionRecord`, and a regression test that would have failed by construction on the pre-Phase-A code. The taxonomy is internally consistent (`From<&FailureMode> for HealthStatus`, `is_deterministic()`, `requires_rollback()` all agree), the `StaticPolicy` is exhaustive over `FailureMode`, and deterministic traps are correctly marked `non_retryable`.

The blockers for Phase B are all medium severity:

1. **Untrusted-WASM error text is interpolated into `RecoveryAction.description` and `ErrorLog.description` without a length cap or sanitization.** This is the prompt-injection surface that `phaseB-prompt-injection-threat-model` calls out, but Phase B can't safely build the LLMPolicy on top of it until the cap is in place.
2. **`Snapshot::new` panics on compression failure** (`.expect("compression should not fail")`). Phase A now drives this path with real (potentially 32 MiB at default config) memory, so this turns a guest-side memory-growth pattern into a host-side panic vector.
3. **The hypervisor's `RwLock<WasmSandbox>` is taken as a *read* lock around `execute()`, but `health_validator.start_execution()` / `current_resources()` race across concurrent calls.** Pre-existing, but Phase A's added reliance on `current_resources()` makes the race observable in the failure-record's resource snapshot.

The first one is worth tightening before Phase B begins; the others can be lifted into a Phase B follow-up issue with an explicit owner.

---

## High-severity findings (must-fix before Phase B)

### H1. Attacker-controlled `wasmtime` error text flows verbatim into recovery actions and error logs.

**Files:** `src/sandbox/wasm_runtime.rs:172`, `:208`, `:255`; `src/hypervisor/failure_mode.rs:191`, `:114`, `:118`; `src/hypervisor/recovery.rs:191`, `:204`.

`FailureMode::InvalidModule(String)`, `TrapOther(String)`, and `HostError(String)` carry the *raw* `wasmtime` error text — including the unbounded chain `format!("{e:#}")` (line 255). That string is then:

* concatenated into `RecoveryAction.description` via `format!` in `StaticPolicy::recover` (`recovery.rs` lines 191, 204);
* concatenated into `ErrorLog.description` via `FailureMode::describe()`;
* serialized into `phase3_<scenario>.json` and from there into `phase3_index.json`, which is the *input* to the AI scorers (`scripts/_rebuild_phase3_index.py` trims `description` and `error_type` but **does not trim `recovery_actions`**).

This is exactly the surface `phaseB-prompt-injection-threat-model` flags: `wasmtime` trap text is attacker-controlled when the WASM is untrusted, and an attacker can embed arbitrary Markdown / prompt fragments into module names, import strings, and custom-section data that wasmtime echoes back in its error.

**Minimum action before Phase B starts:** cap variant payload strings at construction time. Concretely, in `FailureMode::from_wasmtime_trap` / `from_anyhow_error` and at every callsite that constructs `InvalidModule(...)`, `TrapOther(...)`, `HostError(...)`, wrap the string in something like:

```rust
fn cap_error_text(s: String) -> String {
    const MAX: usize = 2048;
    if s.len() <= MAX { s } else {
        let mut out = s.chars().take(MAX).collect::<String>();
        out.push_str("…[truncated]");
        out
    }
}
```

Also drop the `{e:#}` (alternate "chain") format at `wasm_runtime.rs:255` — `{e}` is enough, and `:#` is what produces the multi-line attacker-controllable chain.

Optional but recommended: strip control characters and backtick fences from the captured strings before they reach `format!` in `StaticPolicy`. The structured `RecoveryAction { description, .. }` shape lets you store the sanitized text in `description` and the raw text in a sibling `raw_error` field if you ever need it for debugging.

### H2. The regression test asserts `runtime_failures_do_trigger_rollback`, but the rollback machinery is currently theater.

**Files:** `src/snapshot/manager.rs:410-435`, `src/hypervisor/mod.rs:288-295`, `tests/phase3_distinct_outputs.rs:183-195`.

`SnapshotManager::rollback_to` returns a `RollbackResult` containing the decompressed memory and a list of FS revert ops, but **nothing in the production path applies it**. `manual_rollback` does `let _ = result;`. `execute_tool` sets `rollback_performed = true` purely because `rollback_to` returned `Ok`. Each WASM execution already gets a fresh `Store` and `Instance`, so memory state is implicitly reset; the rollback machinery's only observable effect today is bumping `total_rollbacks`.

This is **not a regression introduced by Phase A** — it pre-dates this diff — but the new `runtime_failures_do_trigger_rollback` test now *codifies* the behavior as expected. That's a problem because:

* It locks in the "rollback is a flag flip" semantics as a passing test.
* The validation report (`VALIDATION_REPORT.md`) and Phase 3 scoring rubric read "rollback performed" as a real recovery action; the AI scorers will reward that even though no state was actually restored.
* When Phase C adds the daemon mode and the `WasmSandbox` actually persists across calls, this test will silently pass even if the rollback application code is broken.

**Recommended action:** the regression test should assert *the contract* (`rollback_performed == true` when the failure requires rollback and a snapshot was captured) rather than "any non-load-time failure produces rollback=true regardless of whether rolling back did anything." Either:

* (a) Drop the `runtime_failures_do_trigger_rollback` test for now and add it back in Phase C when rollback actually applies; or
* (b) Keep the test but rename it `rollback_flag_is_set_for_runtime_failures` so the contract is unambiguous, and add a `// TODO(phaseC): assert memory actually restored once apply_rollback exists` comment.

Option (b) is the smaller change and surfaces the gap explicitly in the test name.

### H3. `Snapshot::new` panics on compression failure, called unconditionally with real memory whenever the module exports `"memory"`.

**Files:** `src/snapshot/manager.rs:52`, `src/hypervisor/mod.rs:241-247`.

```rust
zstd::stream::copy_encode(&memory[..], &mut compressed, compression_level)
    .expect("compression should not fail");
```

Phase A increased the realistic input size from `vec![0u8; 65536]` (always 64 KiB) to "whatever the WASM declares as its initial memory" (up to `max_memory_pages * 65536` = 32 MiB at default `SandboxConfig`, unbounded for modules that declare larger initial memories — wasmtime caps at the engine limit, not the sandbox config, since `max_memory_pages` is currently unused). With real allocation behavior, `copy_encode` *can* fail (allocator returns Err for the intermediate buffer; encoder rejects an inconsistent state) and the `.expect` aborts the process, taking the hypervisor and any pooled callers (Phase C) with it.

**Recommended action:** change `Snapshot::new` to return `Result<Snapshot>`, propagate the compression error as `NexusError::SerializationError`, and let `create_snapshot` / `execute_tool` map it to `FailureMode::HostError` so the structured `ToolOutput` path still owns the failure. This also gives us a place to enforce a per-snapshot memory budget so a `(memory 4096)` module (256 MiB initial) can't OOM the host with one call.

While at it, `max_memory_pages` in `SandboxConfig` is currently *declared but not wired into the wasmtime engine* (no `ResourceLimiter` or `Memory::new` cap). Phase A's "real memory" snapshot path makes this gap exploitable.

---

## Medium-severity findings (should fix in Phase B or follow-up)

### M1. `HealthValidator` baseline races across concurrent `execute_tool` calls.

**Files:** `src/hypervisor/mod.rs:220-224`, `src/hypervisor/validator/health.rs:117-121`.

`execute_tool` takes a *read* lock on `RwLock<WasmSandbox>`, so two `.execute_tool().await` calls on the same hypervisor can overlap. They both call `health_validator.start_execution()`, which overwrites the singleton `baseline`/`start` RwLocks. The second caller's resource readings will be anchored to the wrong baseline.

Pre-existing race, but Phase A's `record_failure(resources)` and `record_success(..., resources)` now use this baseline for *every* execution record. Before Phase A, the records were zero-filled, so the race was invisible.

**Recommended action:** either (a) per-execution `HealthBaseline` returned as a value from `start_execution()` and passed back into `current_resources(&baseline)`, or (b) defer the fix to Phase C where the `HypervisorPool` will own per-instance health state anyway, but add a `// TODO(phaseC)` near `start_execution` and document the limitation.

### M2. The comment in `execute_tool` misdescribes the `MissingEntrypoint` path.

**File:** `src/hypervisor/mod.rs:229-233`.

```rust
// Build the snapshot from the *real* pre-call memory whenever we
// have it. For load-time failures the worker did not capture any
// memory (instantiation never succeeded), so there is nothing to
// snapshot ...
```

This is wrong for `FailureMode::MissingEntrypoint`. The worker *does* capture `pre_call_memory` in that path (`wasm_runtime.rs:235` — instantiation succeeded, only `get_typed_func` failed). The hypervisor therefore *does* call `create_snapshot` and updates `current_snapshot` for missing-entrypoint failures. Behavior is still correct (`requires_rollback()` returns false, so the rollback is skipped) but the snapshot is created and discarded — wasted work, and the comment lies.

**Recommended action:** fix the comment, and either (a) move the `requires_rollback()` check above the snapshot creation so no snapshot is built for load-time failures, or (b) add a `failure_mode.snapshot_useful()` predicate that excludes `MissingEntrypoint` + `InvalidModule` from snapshotting. Option (a) is cleaner.

### M3. `Trap::OutOfFuel → FailureMode::FuelExhausted { limit: 0 }` then patched at the callsite.

**Files:** `src/hypervisor/failure_mode.rs:190`, `src/sandbox/wasm_runtime.rs:258-263`.

`from_wasmtime_trap` returns `FuelExhausted { limit: 0 }` because the trap variant carries no limit. The sandbox worker then re-wraps the variant to fill in `max_fuel`. This works, but the placeholder `0` is observable in any other caller of `from_wasmtime_trap` (currently none, but the function is public).

**Recommended action:** make `from_wasmtime_trap` take `&Trap` and `&Context` (or just `fuel_limit: u64`), or rename/split the API so the "limit-less" variant has a separate constructor like `FailureMode::fuel_exhausted_unknown_limit()` that emits a typed sentinel value.

### M4. `set_recovery_policy` takes `&mut self`, but the hypervisor is consumed via `Arc`.

**File:** `src/hypervisor/mod.rs:166-168`.

The documented Phase B use case is "outcome-feedback loop and tests want to swap the policy at runtime" — but every caller in the codebase holds `Arc<NexusHypervisor>` (regression test, future daemon). With `Arc`, you can't get `&mut self` without `Arc::get_mut` (which requires the unique reference) or interior mutability.

**Recommended action:** change `recovery_policy: Arc<dyn RecoveryPolicy>` to `RwLock<Arc<dyn RecoveryPolicy>>` and make `set_recovery_policy(&self, ...)`. Cheap; matches the existing pattern for `capability_manager` and `current_snapshot`.

### M5. The detached-thread timeout path leaks the worker thread on every timeout.

**File:** `src/sandbox/wasm_runtime.rs:283-292`.

```rust
Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
    drop(handle);
    ...
}
```

`drop(JoinHandle)` detaches the OS thread; the WASM is sandboxed so it can't escape, but with fuel metering enabled it will eventually trap (`OutOfFuel`) and then `let _ = tx.send(...)` no-ops because the receiver is gone. Memory and CPU stay pinned until the worker finishes naturally. In the test/bench harness this leaks threads on every timeout; under load it can accumulate.

In practice with fuel set to 10M instructions, the worker dies in ~tens of milliseconds. But the `time_limit: Duration::from_millis(500)` and `max_fuel: 10_000_000` combo means a tight WASM loop (1 fuel per branch) hits `OutOfFuel` *before* the 500 ms timeout, so the timeout path is rarely taken. If anyone raises `max_fuel` (Phase B's `oom_growth` scenario, Phase C's daemon mode) the timeout becomes the active limiter and this leak matters.

**Recommended action:** use wasmtime's `epoch_interruption` instead of a worker thread + mpsc. The engine config gains `cfg.epoch_interruption(true)`, the store gets a deadline via `store.set_epoch_deadline(N)`, and a background ticker increments the epoch on a wall-clock cadence. Trap on deadline emits `Trap::Interrupt` (which today is `FailureMode::TrapOther("Interrupt")` — would need a dedicated `FailureMode::Timeout` mapping). This is a bigger lift; tag as Phase B.

### M6. `current_snapshot` is written but never read on the hot path.

**Files:** `src/hypervisor/mod.rs:106, 247, 397-403`.

`current_snapshot: RwLock<Option<Snapshot>>` is updated on every successful snapshot creation and read only by `manual_rollback` (which itself discards the result). Dead-ish field that gets clobbered by every call, including the `MissingEntrypoint` waste mentioned in M2.

**Recommended action:** drop the field, or move it behind a `last_successful_snapshot: RwLock<Option<Uuid>>` that's only set on successful executions (not failed-with-snapshot ones), and document that `manual_rollback` is the only consumer.

### M7. `LayeredPolicy::recover` clones the description into a `HashSet<String>` per call.

**File:** `src/hypervisor/recovery.rs:241-253`.

Allocation per dedup-check. With Phase A's single `StaticPolicy` layer this is a measurable allocation per failure (3 to 5 descriptions, each ~100 bytes). With Phase B's three-layer `LayeredPolicy` it's the same allocation pattern × 3. Not a hot path today but worth knowing.

**Recommended action:** none for Phase A. If profile shows it, swap `HashSet<String>` for `HashSet<u64>` of `seahash::hash(description.as_bytes())`.

---

## Low-severity findings (nice to have)

### L1. Residual dead test in `hypervisor::tests`.

**File:** `src/hypervisor/mod.rs:429-438`.

```rust
#[test]
fn test_recovery_suggestions() {
    let suggestions = vec![
        "Break the operation into smaller steps".to_string(),
        "Add validation before execution".to_string(),
    ];
    assert!(!suggestions.is_empty());
}
```

Asserts that a hand-built `Vec` is non-empty. Tests nothing about the code. Delete; the real coverage now lives in `recovery::tests` and `tests/phase3_distinct_outputs.rs`.

### L2. `ExecutionResult::failure` shim is still public and not `#[deprecated]`.

**File:** `src/sandbox/wasm_runtime.rs:109-122`.

The docstring says "Back-compat shim. New code should call `failure_from_mode`." Mark it `#[deprecated(note = "use failure_from_mode")]` so any new caller gets a compiler nudge.

### L3. `Trap::Interrupt` and `Trap::AtomicWaitNonSharedMemory` fall through to `TrapOther`.

**File:** `src/hypervisor/failure_mode.rs:177-192`.

`wasmtime::Trap` (37.x) has ~48 variants and is `#[non_exhaustive]`. The explicit match covers the 12 the validation cares about. `Trap::Interrupt` will matter when Phase B / M5 above switch to epoch interruption — at that point it should map to `FailureMode::Timeout { .. }`, not `TrapOther("Interrupt")`. Tag with a `// TODO(phaseB/epoch)` near the fallback arm.

### L4. `_input: serde_json::Value` is silently ignored.

**File:** `src/hypervisor/mod.rs:201`.

The hypervisor accepts a JSON input parameter and never threads it into the WASM call. Pre-existing; not new in Phase A. If Phase B / C plans to use it (the daemon protocol mentions `input` in the framed message), this becomes a real gap. Either remove the parameter and have a follow-up todo restore it when there's actual plumbing, or document the "currently ignored" status in the docstring.

### L5. `_temp_token` is bound and immediately dropped without grant tracking.

**File:** `src/hypervisor/mod.rs:208-215`.

```rust
let _temp_token = manager.issue(
    cap.clone(),
    "validation",
    Duration::from_secs(60),
);
```

The token is issued and dropped on the same line. If the `CapabilityManager` records issuance side-effects (revocation lists, audit logs), this still "works"; if not, the loop is a no-op. Doc the intent or remove. Not a Phase A regression.

### L6. The phase3 distinct-outputs regression test isn't wired into `validate.sh`.

**File:** `validate.sh:30-33`, `tests/phase3_distinct_outputs.rs`.

The plan's `harness-ci-gate` todo (still pending) is supposed to wire `cargo test` into the validation flow. Today the regression test is reachable only via `cargo test --test phase3_distinct_outputs`. The plan's Phase A acceptance bullet says the test exists; nothing currently asserts it *runs* during validation. Suggest adding a `cargo test --test phase3_distinct_outputs` step inside Phase 3 of `validate.sh` (before the `capture_error` runs), so a future regression on the recovery path fails fast rather than producing bad JSON that the AI scorer then has to discover.

### L7. `mem.data(&store).to_vec()` allocates a fresh copy every call.

**File:** `src/sandbox/wasm_runtime.rs:220-222`.

For a 64 KiB memory this is fine (microseconds). For a 32 MiB memory (Phase C's `bench_snapshot_real_wasm_memory`) it's a ~10 ms `memcpy` on top of the zstd compression. Worth flagging now so Phase C can land a zero-copy path (e.g. `Bytes::copy_from_slice` + `Arc<[u8]>` shared into the snapshot) rather than discovering it during the perf push.

### L8. Doc comment on `FailureMode::FuelExhausted` is slightly misleading.

**File:** `src/hypervisor/failure_mode.rs:25-27`.

> "Only emitted when fuel metering is enabled in `wasmtime::Config`."

Phase A enabled fuel metering globally (`wasm_runtime.rs:147`), so the conditional is always true now. Either drop the conditional or rephrase to "Emitted by the fuel-metered runtime when `store.get_fuel()` reaches 0."

---

## Test coverage gaps

### T1. No test exercises the worker-disconnected `HostError` branch.

**File:** `src/sandbox/wasm_runtime.rs:294-300`.

The branch `Err(_) => FailureMode::HostError("worker thread disconnected …")` is unreachable from the existing test set. A panicking host function would hit it. Add a unit test that hands the worker a wasm module that imports a host function configured to panic, and assert we land in `HostError(...)` instead of bubbling the panic.

### T2. No test asserts `pre_call_memory.is_none()` for memory-less modules.

The test scenarios all declare `(memory (export "memory") 1)`. There's no scenario asserting that a module without a memory export produces `pre_call_memory: None` and skips snapshot creation. Add a one-liner to `phase3_distinct_outputs.rs`:

```rust
#[test]
fn no_memory_export_means_no_snapshot() {
    let wat = r#"(module (func (export "_start") nop))"#;
    let wasm = wat::parse_str(wat).unwrap();
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let tool = ToolDefinition::new("no_mem".into(), wasm);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let out = rt.block_on(hv.execute_tool(tool, serde_json::json!({}))).unwrap();
    assert!(out.success);
    // Indirectly: snapshot stats shouldn't bump.
    assert_eq!(hv.get_snapshot_stats().total_snapshots, 0);
}
```

### T3. No test for `FailureMode::FuelExhausted` end-to-end with `limit` correctly filled.

The `failure_mode.rs` unit tests assert the round-trip with `limit: 0`. The `wasm_runtime.rs` callsite re-fills it to `max_fuel`. Nothing asserts that the *observable* `error_log.failure_mode` for `infinite_loop` carries `limit == 10_000_000` (the default `SandboxConfig.max_fuel`). Easy to drop into the existing `classification_matches_phase3_expectations` test.

### T4. No proptest over `FailureMode → HealthStatus → requires_rollback` consistency.

`Cargo.toml` already has `proptest` in `[dev-dependencies]`. A 20-line property: for every `FailureMode` variant generator, `HealthStatus::from(&mode).requires_rollback() == mode.requires_rollback()` (or document why they intentionally differ). Catches the next "InvalidModule case got added to one enum and not the other" drift.

### T5. No test covers the `host_health.is_healthy() == false` reconciliation branch.

**File:** `src/hypervisor/mod.rs:263-270`.

```rust
(None, None, h) if !h.is_healthy() => Some(FailureMode::HostError(...)),
```

Only reachable when the sandbox reports success but the host's `HealthValidator::validate()` reports a degraded state. Synthesizable by injecting a mock `HealthValidator` (would need a trait). Tag as Phase B follow-up; not blocking.

### T6. The regression test never invokes `set_recovery_policy`.

If M4 stays as-is, there's no end-to-end test that the policy is actually swappable. Add a test that injects a `StubPolicy` returning `RecoveryAction::new("STUB", RecoverySource::Instinct)` and asserts that `error_log.recovery_actions[0].description == "STUB"`. Will also catch any future regression where the field is shadowed by a static.

---

## Security observations

### S1. New public surface area: `FailureMode::*(String)` variants and `RecoveryAction.description`.

Covered in H1. Summary: every `(String)` variant of `FailureMode` is a sink for attacker-controlled wasmtime text. The `serde(Serialize)` impl on `FailureMode` is what turns this into a downstream injection vector for the Phase 3 AI scorer, since `phase3_index.json` includes `"failure_mode": el.get("failure_mode")` *with no trim* (`scripts/_rebuild_phase3_index.py:41`). Cap the strings at construction time.

### S2. `_temp_token` capability check doesn't enforce anything.

**File:** `src/hypervisor/mod.rs:207-215`.

`execute_tool` "validates capabilities" by issuing temporary tokens it then drops. Nothing checks whether the tool *had* the capability before issuance. This is a pre-existing semantic gap, but Phase A's new failure path puts more weight on the `ToolDefinition.required_capabilities` field (the AI scorer reads it). Either implement the check or document it as a stub.

### S3. `RecoveryPolicy::recover(&self, mode: &FailureMode, operation: &str)` — `operation` is operator-controlled.

Phase A's `StaticPolicy` ignores `operation`. Phase B's `InstinctPolicy` is planned to persist instincts to `~/.nexus/instincts/<operation>.json`. If `operation` is not sanitized as a filename (`../../etc/passwd`, `con`, `aux`, etc.), the instinct store is a path-traversal vector.

**Recommended action:** add an "operation must be `[a-zA-Z0-9_-]{1,64}`" invariant at `ToolDefinition::new()` and assert it in `execute_tool`. Better caught now (one-line validator) than during a Phase B security review.

### S4. The detached worker thread can keep referencing the snapshot ring buffer.

**File:** `src/sandbox/wasm_runtime.rs:286`.

When the timeout fires and the worker is detached (M5), the worker thread still holds `engine: Arc<Engine>` (via move), `module: Module` (via move), and `pre_call_memory: Option<Vec<u8>>` (allocated). It will eventually drop them when it finishes naturally. If the hypervisor itself is dropped before the worker finishes (process shutdown, pool eviction in Phase C), there's no `join`. Not a soundness issue (Rust drops are well-defined), but it's a measurable shutdown-latency issue and a potential issue for snapshot persistence if a worker holds a SnapshotManager handle (it doesn't today, but Phase B's outcome-feedback loop will want one).

### S5. No bound on `RecoveryAction.description` length flowing into JSON.

Cosmetic, but `serde_json` will happily serialize a 1 GB string into `error_log.recovery_actions`. Cap at 4 KiB.

### S6. `consume_fuel(true)` is on; `epoch_interruption` is off.

The plan's Phase A says "fuel metering + wall-clock combination." Today the wall-clock is a worker-thread + mpsc with `recv_timeout` — i.e. host-side detection, not guest-side interruption. If the WASM yields control (e.g. via WASI suspend, future component model), fuel may not advance and the host-side timer is the only backstop. Move to `epoch_interruption` in Phase B (also fixes M5).

---

## What was done well

* The `From<&FailureMode> for HealthStatus` impl and the `is_deterministic` / `requires_rollback` predicates are mechanically consistent — same variants in each match in the same order, easy to audit, and the `health_status_mapping_is_correct` test pins every case the validation exercises.
* `StaticPolicy` is exhaustive over `FailureMode`. Adding a new variant is a *compile* error, not a runtime "default recovery" path. This is exactly the right shape for a registry that the AI scorers grade against.
* The regression test is genuinely the test Claude asked for: it asserts distinct `(FailureMode, category, recovery_actions[0].description)` tuples across all five Phase 3 scenarios, and it would fail by construction on the pre-Phase-A code (which produced `HealthStatus::Corrupted` for every variant and the same two recovery strings).
* Phase A doesn't introduce any `unsafe` blocks, doesn't change the public crate root in a breaking way for the snapshot/sandbox/security modules, and re-exports the new types cleanly through `lib.rs`. Phase B can swap `StaticPolicy` for a `LayeredPolicy` via `set_recovery_policy` (modulo M4) without touching downstream code.
* Tests are colocated and readable: `failure_mode::tests` and `recovery::tests` carry the unit-level invariants, `phase3_distinct_outputs.rs` carries the integration-level one. No magic mocks, no shared mutable test state.

---

## Recommended go/no-go for Phase B

Phase A meets every acceptance criterion in the plan:

* AI rescore target ≥70%: testable once `validate.sh 3 report` completes; the precondition (distinct, failure-specific, attribute-marked recovery actions) is in place.
* Methodology §6.2 caveat drop: the four caveats (fuel metering, fake snapshot, generic recovery, spurious rollback) are all closed by the diff — fuel metering wired in `wasm_runtime.rs:147`, real memory in `wasm_runtime.rs:220-222` + `hypervisor/mod.rs:234-251`, failure-specific recovery in `recovery.rs`, no-rollback on load failure in `hypervisor/mod.rs:288-295`.
* Distinct-output regression test: present and would fail on the prior commit; minor wiring gap into `validate.sh` (L6) is non-blocking.

Recommendation: **approve Phase A**, file H1 and H3 as Phase B-blocking issues (cap untrusted strings, make snapshot compression fallible), file H2 as a test-rename issue, and start Phase B once those three land. M1–M7 and L1–L8 can fold into Phase B's natural touch-points.
