# Phase 3 AI Telemetry Validation — Kimi (post-Phase-A rescore)

Model: kimi-k2.5
Date (UTC): 2026-06-07T21:44:00Z
Input: artifacts/raw/phase3_index.json (5 scenarios, post-Phase-A)
Prior verdicts for comparison: claude 28%, gpt 24%

## Per-scenario analysis

### 1. div_by_zero
- Observed: Integer division by zero trap; execution_time_ms=1; rollback_performed=true; fuel_consumed=0
- failure_mode / trigger_status / rollback_performed correctness: **Yes.** `TrapDivByZero` is correctly typed, `Trapped` status accurately classifies this as a guest-side trap (not host corruption), and rollback=true is appropriate for runtime trap cleanup.
- Recovery actions evaluated:
  1. "Integer divide-by-zero — guard the divisor (`if denom == 0 { return … }`) or use checked-division at the call site." — **Correct and specific.** Targets the exact fault with concrete code pattern. `non_retryable=true` is appropriate for deterministic trap.
  2. "Audit the input contract that allowed a zero divisor through; validation at the boundary is the right fix." — **Correct and depth-appropriate.** Addresses root cause (input validation) vs symptom (trap). Confidence 0.9 reflects this is a design review action.
  3. "Do NOT auto-retry with identical inputs; this failure is deterministic." — **Correct and operationally critical.** Prevents pointless retry loops. High confidence (1.0) appropriate.
- Better alternatives / missing steps: Could suggest surfacing the actual divisor value in telemetry for debugging. Could distinguish integer vs floating-point divide-by-zero if the engine supports both.
- Score: 9/10
- Justification: Classification is precise, recovery actions are technically correct and failure-specific, and the `non_retryable` flag is set appropriately. Minor gap: could include trap-specific debugging info (operand values).

### 2. infinite_loop
- Observed: Fuel budget of 10000000 instructions exhausted; execution_time_ms=6; rollback_performed=true; fuel_consumed=10000000
- failure_mode / trigger_status / rollback_performed correctness: **Yes.** `FuelExhausted` with limit payload is correctly structured, `FuelExhausted` status is accurate (was incorrectly `Corrupted` in pre-Phase-A), and rollback=true is appropriate for runtime termination.
- Recovery actions evaluated:
  1. "Fuel budget of 10000000 instructions was consumed; the guard worked as designed." — **Correct framing.** Acknowledges this is expected containment, not failure. `non_retryable=false` is appropriate—retry with adjusted fuel is valid.
  2. "Profile the WASM to find the hot loop, then either reduce its iteration count or raise the per-tool fuel cap with an explicit business rationale." — **Actionable and optimal.** Offers both fix approaches (optimize guest code or raise limit with justification). Good confidence (0.9) reflects this requires profiling.
  3. "Consider iterative or memoized algorithms instead of naive recursion/iteration over large inputs." — **Reasonable but lower confidence (0.7).** Applicable to some cases but may not be relevant if the loop is legitimately long-running. Non_retryable=false is correct.
- Better alternatives / missing steps: Could suggest partial result preservation if the loop was producing incremental output. Could recommend cooperative yield points for long-running legitimate workloads.
- Score: 8/10
- Justification: Strong improvement from pre-Phase-A generic boilerplate. Status is now accurate, and guidance is failure-specific. Confidence grading on actions is sensible. Minor: action 3 is slightly generic and may not apply to all infinite loop scenarios.

### 3. missing_start
- Observed: Module has no exported `_start` function; execution_time_ms=1; rollback_performed=false; fuel_consumed=0
- failure_mode / trigger_status / rollback_performed correctness: **Yes.** `MissingEntrypoint` with expected field is correctly structured, `InvalidModule` is the right classification for load-time validation failure, and rollback=false is correct (nothing executed, no state to roll back).
- Recovery actions evaluated:
  1. "Module exports no `_start` (or fallback `main`) function. Verify entrypoint at build time, or configure the executor to call the correct export explicitly." — **Correct and specific.** Identifies both the problem and two resolution paths (fix build or reconfigure executor). `non_retryable=true` is correct—this is a configuration error.
  2. "Do NOT roll back; nothing executed." — **Correct observation.** Reinforces why rollback_performed=false. Good confidence (1.0).
- Better alternatives / missing steps: Could suggest adding a pre-flight validation check to catch this before attempting instantiation. Could recommend configurable entrypoint names for flexibility.
- Score: 9/10
- Justification: Excellent handling of the load-time vs runtime distinction. Recovery actions are specific to the entrypoint failure mode. The explicit "do not roll back" guidance is operationally useful.

### 4. stack_overflow
- Observed: Call stack exhausted (recursion / stack budget); execution_time_ms=3; rollback_performed=true; fuel_consumed=32758
- failure_mode / trigger_status / rollback_performed correctness: **Yes.** `TrapStackOverflow` is correctly typed, `ResourceExhausted` is accurate classification for stack budget exhaustion, and rollback=true is appropriate for runtime trap cleanup.
- Recovery actions evaluated:
  1. "Call stack exhausted — convert the recursive function to iteration, add an explicit depth bound, or restructure into trampolines." — **Technically correct and specific.** Three concrete remediation patterns for stack exhaustion. `non_retryable=true` is appropriate—retrying same code hits same limit.
  2. "Reject modules with unbounded recursion at validation; only legitimately deep, bounded workloads should raise the stack limit." — **Excellent preventive guidance.** Shifts left to catch at load time. Confidence 0.85 reflects this is a policy recommendation.
- Better alternatives / missing steps: Could suggest increasing stack limit as a last resort with explicit rationale (similar to fuel cap adjustment). Could recommend tail-call optimization if the engine supports it.
- Score: 8/10
- Justification: Strong failure-specific guidance. Both actions are technically sound. Minor: missing the "increase limit with justification" option that was present in infinite_loop's guidance.

### 5. trap_unreachable
- Observed: WASM `unreachable` instruction reached; execution_time_ms=2; rollback_performed=true; fuel_consumed=1
- failure_mode / trigger_status / rollback_performed correctness: **Yes.** `TrapUnreachable` is correctly typed, `Trapped` status is accurate for guest-side abort, and rollback=true is appropriate for runtime trap cleanup.
- Recovery actions evaluated:
  1. "Deterministic `unreachable` instruction reached — likely an assertion failure or unhandled enum arm in the guest module." — **Correct interpretation.** Frames this as guest logic error (assertion/unhandled case). `non_retryable=true` is appropriate.
  2. "Locate the failing instruction via the wasm backtrace (function index + offset). Fix the guest code or the invariant it expected." — **Actionable debugging guidance.** Specific technique for locating the fault. Confidence 0.9 reflects need for debug info.
  3. "Do NOT auto-retry with identical inputs; this failure is deterministic." — **Correct.** Prevents pointless retries. Appropriate confidence (1.0).
- Better alternatives / missing steps: Could surface the actual function name if debug names are available in the module. Could suggest differential testing if this is an optimization-related unreachable (rare edge case).
- Score: 9/10
- Justification: Excellent failure-specific guidance. Classification is precise, and the "do not retry" safeguard is present. The debugging tip (backtrace with function index + offset) is actionable.

## Overall verdict
- Aggregate accuracy rate: 86%
- Average score: 8.6 / 10
- Delta vs prior verdicts: **+58 points vs claude (28% -> 86%), +62 points vs gpt (24% -> 86%)**
- Remaining defects:
  1. Confidence values (0.7-1.0) are somewhat arbitrary with no documented methodology
  2. Could include more debugging context (operand values, function names, source maps)
  3. Minor inconsistency: infinite_loop suggests algorithmic change while stack_overflow omits the "increase limit" option
  4. All `source` fields are "Static" — no dynamic/telemetry-driven action ranking yet
- Recommendations for the Nexus team going into Phase B:
  1. **Add confidence methodology**: Document how confidence values are derived (e.g., action provenance, past success rate, static vs dynamic analysis)
  2. **Enrich debugging context**: Include trap operand values, accessible locals, and stack trace depth in telemetry for deeper diagnostics
  3. **Consistency pass**: Ensure similar failure modes (resource exhaustion: fuel, stack, memory) have consistent recovery option coverage
  4. **Dynamic source support**: Implement `source: "Telemetry"` or `source: "Historical"` for actions ranked by actual recovery success rates from production data
  5. **Add regression test**: Assert that each distinct failure mode produces distinct recovery action sets (would have caught pre-Phase-A boilerplate bug)
  6. **Consider retry policy encoding**: Move `non_retryable` from per-action to a policy field that can be consumed by the orchestrator directly

## Methodology note
Same scale as prior verdicts (1-10 where 10 = correct classification + optimal failure-specific recovery). Phase-A changes show dramatic improvement: `failure_mode` is now strongly typed with payloads, `trigger_status` is failure-specific rather than blanket `Corrupted`, `rollback_performed` correctly distinguishes load-time from runtime failures, and `recovery_actions` are now structured objects with `confidence` and `non_retryable` flags tailored to each failure mode rather than identical generic strings. The system has moved from boilerplate decoupled from error signals to precise, actionable telemetry that enables correct operational responses.
