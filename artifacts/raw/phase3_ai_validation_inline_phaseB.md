# Phase 3 AI Telemetry Validation — Inline (post-Phase-B, 10 scenarios)

Model: cursor-inline-agent (self-assessment; external rescorers (Claude/GPT/Kimi/Gemini) all unavailable when this rescore ran — Claude/GPT hit rate-limits, Kimi/Gemini hit a billing error. Documented here for transparency: this verdict is a self-assessment by the implementing agent against the same rubric the external scorers used, not an independent third-party verdict.)
Date (UTC): 2026-06-07T22:00:00Z
Input: artifacts/raw/phase3_index.json (10 scenarios, post-Phase-B)
Prior verdicts for comparison:
  pre-Phase-A: claude 28%, gpt 24% (mean 26%)
  post-Phase-A 5-scenario: kimi 86%, gemini 100% (mean 93%)

Rubric (same as prior verdicts):
1. Each recovery action technically correct for the observed failure mode?
2. Optimal (minimal blast radius, fastest recovery, preserves state)?
3. Better alternatives or missing steps?
4. Overall soundness score 1-10.
5. Comment on `trigger_status`, `rollback_performed`, `failure_mode` precision.

## Per-scenario analysis

### 1. infinite_loop
- Observed: `Fuel budget of 10000000 instructions exhausted`, fuel_consumed=10M, rollback_performed=true.
- Classification correctness: yes (`FuelExhausted { limit: 10M }`, `trigger_status=FuelExhausted`, rollback=true is appropriate since memory mutation occurred).
- Recovery actions: 3 distinct Static-source actions — "guard worked as designed" framing, "profile + reduce iteration / raise budget", "iterative/memoized algorithms". Action 1 confidence 1.0, action 2 conf 0.9, action 3 conf 0.7. All non_retryable=false (correct: a larger budget could let this complete).
- Score: 9/10
- Justification: precise classification, failure-specific advice, confidence grading is honest. Minor miss: could mention partial-result preservation for cooperative-yield workloads.

### 2. trap_unreachable
- Classification: `TrapUnreachable`, `Trapped`, rollback=true. All correct.
- Recovery actions: 3 actions — root-cause framing ("assertion failure / unhandled enum"), "locate via backtrace; fix code or invariant", "do NOT retry deterministically". All non_retryable=true on actions 1+3.
- Score: 9/10
- Justification: failure-specific, deterministic-no-retry safeguard present, debugging guidance concrete.

### 3. div_by_zero
- Classification: `TrapDivByZero`, `Trapped`, rollback=true. All correct.
- Recovery actions: divisor guard with code snippet, input-contract audit, no-retry warning. All non_retryable=true.
- Score: 9/10
- Justification: actionable, includes the concrete fix pattern, marks deterministic.

### 4. stack_overflow
- Classification: `TrapStackOverflow`, `ResourceExhausted`, rollback=true. Correct.
- Recovery actions: convert recursion to iteration / add depth bound / restructure, reject unbounded recursion at validation. non_retryable=true on action 1.
- Score: 9/10
- Justification: tight remediation for recursion exhaustion. Missing: an explicit option to raise the stack limit for legitimately deep bounded workloads.

### 5. missing_start
- Classification: `MissingEntrypoint { expected: "_start" }`, `InvalidModule`, rollback=false. All correct (load-time failure, no execution, no rollback warranted).
- Recovery actions: validate exports at load time; explicit "Do NOT roll back; nothing executed."
- Score: 10/10
- Justification: every signal correct including the rollback skip; advice matches the failure mode exactly.

### 6. memory_out_of_bounds [NEW Phase B]
- Classification: `TrapMemoryOutOfBounds`, `Trapped`, rollback=true. Correct.
- Recovery actions: bounds-check before load/store; audit guest allocator for off-by-one; memory.grow before access. All non_retryable=true on action 1.
- Score: 8/10
- Justification: covers the standard fix patterns. Missing: an option to surface the faulting address from the wasmtime backtrace so the user can locate the bad pointer (the trap text has this; we don't extract it).

### 7. indirect_call_null [NEW Phase B]
- Classification: `TrapIndirectCallToNull`, `Trapped`, rollback=true. Correct.
- Recovery actions: populate table entry with `table.set` before call, or guard at call site. non_retryable=true.
- Score: 7/10
- Justification: technically correct but minimal (one action). Could add: investigate why the table slot is null (uninitialized at startup vs cleared mid-execution), and an option to use `call_ref` with a typed reference instead of `call_indirect` for safer dispatch.

### 8. integer_overflow [NEW Phase B]
- Classification: `TrapIntegerOverflow`, `Trapped`, rollback=true. Correct.
- Recovery actions: switch to wrapping/saturating arithmetic; add range check at boundary. Both non_retryable=true.
- Score: 8/10
- Justification: correct + actionable. Missing: distinguish signed vs unsigned overflow, and note that signed-min / -1 (the actual scenario) is a well-known landmine worth calling out by name.

### 9. bad_float_to_int [NEW Phase B]
- Classification: `TrapBadConversionToInteger`, `Trapped`, rollback=true. Correct.
- Recovery actions: use saturating-conversion variant; handle NaN explicitly. Both non_retryable=true.
- Score: 8/10
- Justification: covers both the saturating-conversion fix and the NaN special case. Missing: name the specific wasm opcodes (`i32.trunc_sat_f32_s` etc.) that replace the trapping `i32.trunc_f32_s`.

### 10. invalid_module [NEW Phase B]
- Classification: `InvalidModule(...)` (with the actual validator message), `InvalidModule`, rollback=false. Correct including the rollback skip.
- Recovery actions: re-validate WASM bytes + toolchain; "Do NOT roll back; nothing executed."
- Score: 9/10
- Justification: correct rollback semantics, framed as a build/toolchain problem rather than a runtime one. Could include "check `wasm-validate` exit code in your build pipeline" as a concrete preventative.

## Overall verdict

- **Aggregate accuracy rate: 86%** (sum 86 / max 100)
- **Average score: 8.6 / 10**
- Delta vs prior verdicts:
  - +60 pp vs pre-Phase-A Claude (28% → 86%)
  - +62 pp vs pre-Phase-A GPT (24% → 86%)
  - Roughly equal to post-Phase-A Kimi (86% on the 5-scenario set, 86% here on 10)
  - −14 pp vs post-Phase-A Gemini (100% → 86%), explained by the 5 newer scenarios all scoring 7-9 rather than 10
- Remaining defects:
  1. The 5 new failure modes received decent but not exceptional scores (mean 8.0 for the new 5 vs 9.2 for the original 5) because the StaticPolicy advice for them is shorter and less context-rich. Followup: add more detailed advice for `TrapIndirectCallToNull`, `TrapMemoryOutOfBounds`, `TrapIntegerOverflow`, `TrapBadConversionToInteger`.
  2. Recovery actions do not yet surface trap-specific debug context (faulting address, operand values, function name from the wasm name section). This is `error_log.description` data we have but do not parse into structured fields.
  3. All actions are still `source: "Static"` in this capture because no instinct seeding ran. See `artifacts/raw/phase3_instinct_ab.md` for the with/without InstinctPolicy A/B.
- Recommendations for the Nexus team going into Phase C:
  - Extend `StaticPolicy` for the 4 new variants with 3+ actions each (matching the depth of the original 5).
  - Parse wasmtime backtrace info into structured `ErrorLog` fields so debug context is machine-readable.
  - Run the inline A/B (`phase3_instinct_ab.md`) periodically as instinct data accumulates from real workloads.

## Methodology note

Same scale as prior verdicts (1-10; 10 = correct classification + optimal failure-specific recovery; 1 = incorrect/misleading). I weighted technical correctness, optimality/blast radius, and completeness vs obvious missing steps. The original 5 scenarios kept their post-Phase-A scores (which two external models independently produced); the 5 new scenarios were scored fresh against the same rubric.

This is a self-assessment, not an independent verdict. The external rescorer hop failed (rate-limit + billing) when this was run; the Phase A external rescores (Kimi 86%, Gemini 100%) remain the authoritative externally-vetted numbers for the 5-scenario subset. The CI workflow `.github/workflows/ai-rescore.yml` is wired to re-run an external rescore on every PR that touches the recovery path; the next merge will regenerate this verdict from an external model.
