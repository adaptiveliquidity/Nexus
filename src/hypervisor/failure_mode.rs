//! Typed failure-mode taxonomy for the hypervisor.
//!
//! Replaces the prior string-matching of `wasmtime` error text in
//! `NexusHypervisor::execute_tool` with a typed enum derived from
//! `wasmtime::Trap` and a handful of host-side categories. Both AI scorers
//! in the Phase 3 validation identified the missing taxonomy as the #1
//! systemic defect; this module is the foundation that lets Phase A close
//! every defect-cleanup item: correct `trigger_status`, failure-specific
//! recovery actions, and no spurious rollback for load-time failures.

use serde::{Deserialize, Serialize};
use wasmtime::Trap;

use super::validator::health::HealthStatus;

/// What went wrong, classified precisely enough that:
///   - `HealthStatus` is derivable mechanically
///   - the `RecoveryPolicy` can return distinct advice per variant
///   - the rollback decision is unambiguous (`requires_rollback()`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureMode {
    /// The wall-clock watchdog fired before execution returned.
    Timeout { limit_ms: u64, observed_ms: u64 },

    /// `wasmtime` fuel was consumed before execution returned. Only emitted
    /// when fuel metering is enabled in `wasmtime::Config`.
    FuelExhausted { limit: u64 },

    // --- Deterministic WASM traps (host state unaffected; isolation held) ---
    TrapUnreachable,
    TrapDivByZero,
    TrapIntegerOverflow,
    TrapBadConversionToInteger,
    TrapStackOverflow,
    TrapMemoryOutOfBounds,
    TrapHeapMisaligned,
    TrapTableOutOfBounds,
    TrapIndirectCallToNull,
    TrapBadSignature,
    TrapNullReference,
    TrapCastFailure,
    /// Any other deterministic trap we have not given its own variant yet.
    /// The display string preserves the wasmtime description for telemetry
    /// and the recovery policy still has something to key off.
    TrapOther(String),

    /// The configured WASM memory page cap was exceeded.
    MemoryLimitExceeded { pages: u32, limit_pages: u32 },

    /// Module bytes did not compile or link cleanly (validation error,
    /// instantiation error, etc.). No execution occurred.
    InvalidModule(String),

    /// The configured entrypoint export was absent (e.g. no `_start`).
    /// No execution occurred.
    MissingEntrypoint { expected: String },

    /// Host-side error during snapshot, health check, or other plumbing.
    /// This is the only variant that maps to `HealthStatus::Corrupted`.
    HostError(String),
}

impl FailureMode {
    /// Short stable category for telemetry, logs, and the analyzer.
    pub fn category(&self) -> &'static str {
        match self {
            FailureMode::Timeout { .. } => "TIMEOUT",
            FailureMode::FuelExhausted { .. } => "FUEL_EXHAUSTED",
            FailureMode::TrapUnreachable => "TRAP_UNREACHABLE",
            FailureMode::TrapDivByZero => "TRAP_DIV_BY_ZERO",
            FailureMode::TrapIntegerOverflow => "TRAP_INTEGER_OVERFLOW",
            FailureMode::TrapBadConversionToInteger => "TRAP_BAD_FLOAT_TO_INT",
            FailureMode::TrapStackOverflow => "TRAP_STACK_OVERFLOW",
            FailureMode::TrapMemoryOutOfBounds => "TRAP_MEMORY_OOB",
            FailureMode::TrapHeapMisaligned => "TRAP_HEAP_MISALIGNED",
            FailureMode::TrapTableOutOfBounds => "TRAP_TABLE_OOB",
            FailureMode::TrapIndirectCallToNull => "TRAP_INDIRECT_NULL",
            FailureMode::TrapBadSignature => "TRAP_BAD_SIGNATURE",
            FailureMode::TrapNullReference => "TRAP_NULL_REFERENCE",
            FailureMode::TrapCastFailure => "TRAP_CAST_FAILURE",
            FailureMode::TrapOther(_) => "TRAP_OTHER",
            FailureMode::MemoryLimitExceeded { .. } => "MEMORY_LIMIT_EXCEEDED",
            FailureMode::InvalidModule(_) => "INVALID_MODULE",
            FailureMode::MissingEntrypoint { .. } => "MISSING_ENTRYPOINT",
            FailureMode::HostError(_) => "HOST_ERROR",
        }
    }

    /// Single-line human-readable description suitable for an error log.
    pub fn describe(&self) -> String {
        match self {
            FailureMode::Timeout { limit_ms, observed_ms } => {
                format!("Execution exceeded {limit_ms}ms (observed {observed_ms}ms)")
            }
            FailureMode::FuelExhausted { limit } => {
                format!("Fuel budget of {limit} instructions exhausted")
            }
            FailureMode::TrapUnreachable => "WASM `unreachable` instruction reached".to_string(),
            FailureMode::TrapDivByZero => "Integer division by zero".to_string(),
            FailureMode::TrapIntegerOverflow => "Integer overflow".to_string(),
            FailureMode::TrapBadConversionToInteger => "Invalid float-to-integer conversion".to_string(),
            FailureMode::TrapStackOverflow => "Call stack exhausted (recursion / stack budget)".to_string(),
            FailureMode::TrapMemoryOutOfBounds => "Out-of-bounds linear memory access".to_string(),
            FailureMode::TrapHeapMisaligned => "Unaligned atomic access".to_string(),
            FailureMode::TrapTableOutOfBounds => "Out-of-bounds table access".to_string(),
            FailureMode::TrapIndirectCallToNull => "Indirect call to a null table entry".to_string(),
            FailureMode::TrapBadSignature => "Indirect call signature mismatch".to_string(),
            FailureMode::TrapNullReference => "Null reference dereferenced".to_string(),
            FailureMode::TrapCastFailure => "Type cast failure on a reference".to_string(),
            FailureMode::TrapOther(s) => format!("WASM trap: {s}"),
            FailureMode::MemoryLimitExceeded { pages, limit_pages } => {
                format!("WASM memory grew to {pages} pages; limit is {limit_pages}")
            }
            FailureMode::InvalidModule(s) => format!("Invalid WASM module: {s}"),
            FailureMode::MissingEntrypoint { expected } => {
                format!("Module has no exported `{expected}` function")
            }
            FailureMode::HostError(s) => format!("Host error: {s}"),
        }
    }

    /// Whether retrying with identical inputs can plausibly produce a
    /// different result. Deterministic traps and load-time failures are
    /// non-retryable; resource limits may be retryable with a larger budget.
    pub fn is_deterministic(&self) -> bool {
        matches!(
            self,
            FailureMode::TrapUnreachable
                | FailureMode::TrapDivByZero
                | FailureMode::TrapIntegerOverflow
                | FailureMode::TrapBadConversionToInteger
                | FailureMode::TrapMemoryOutOfBounds
                | FailureMode::TrapHeapMisaligned
                | FailureMode::TrapTableOutOfBounds
                | FailureMode::TrapIndirectCallToNull
                | FailureMode::TrapBadSignature
                | FailureMode::TrapNullReference
                | FailureMode::TrapCastFailure
                | FailureMode::TrapOther(_)
                | FailureMode::InvalidModule(_)
                | FailureMode::MissingEntrypoint { .. }
        )
    }

    /// Load-time failures (no execution occurred) should not roll back
    /// because no state mutation could have happened. The plumbing layer
    /// uses this to skip the rollback path on `MissingEntrypoint` /
    /// `InvalidModule`.
    pub fn requires_rollback(&self) -> bool {
        !matches!(
            self,
            FailureMode::InvalidModule(_) | FailureMode::MissingEntrypoint { .. }
        )
    }

    /// Best-effort classification of a `wasmtime::Error` returned from a
    /// wasmtime call. Downcasts to `wasmtime::Trap` when possible and falls
    /// back to a string-anchored heuristic for the small set of host errors
    /// wasmtime does not type. Returns `None` when the caller should look
    /// at the textual error themselves (and probably classify as
    /// `HostError`).
    pub fn from_anyhow_error(err: &wasmtime::Error) -> Option<FailureMode> {
        if let Some(t) = err.downcast_ref::<Trap>() {
            return Some(FailureMode::from_wasmtime_trap(t));
        }
        // Iterate the error chain for cases where the Trap is wrapped.
        for cause in err.chain() {
            if let Some(t) = cause.downcast_ref::<Trap>() {
                return Some(FailureMode::from_wasmtime_trap(t));
            }
        }
        None
    }

    /// Map a typed `wasmtime::Trap` into our taxonomy.
    pub fn from_wasmtime_trap(trap: &Trap) -> FailureMode {
        match trap {
            Trap::UnreachableCodeReached => FailureMode::TrapUnreachable,
            Trap::IntegerDivisionByZero => FailureMode::TrapDivByZero,
            Trap::IntegerOverflow => FailureMode::TrapIntegerOverflow,
            Trap::BadConversionToInteger => FailureMode::TrapBadConversionToInteger,
            Trap::StackOverflow => FailureMode::TrapStackOverflow,
            Trap::MemoryOutOfBounds => FailureMode::TrapMemoryOutOfBounds,
            Trap::HeapMisaligned => FailureMode::TrapHeapMisaligned,
            Trap::TableOutOfBounds => FailureMode::TrapTableOutOfBounds,
            Trap::IndirectCallToNull => FailureMode::TrapIndirectCallToNull,
            Trap::BadSignature => FailureMode::TrapBadSignature,
            Trap::NullReference => FailureMode::TrapNullReference,
            Trap::CastFailure => FailureMode::TrapCastFailure,
            Trap::OutOfFuel => FailureMode::FuelExhausted { limit: 0 },
            other => FailureMode::TrapOther(format!("{other:?}")),
        }
    }
}

impl From<&FailureMode> for HealthStatus {
    fn from(mode: &FailureMode) -> Self {
        match mode {
            FailureMode::Timeout { .. } => HealthStatus::Timeout,
            FailureMode::FuelExhausted { .. } => HealthStatus::FuelExhausted,
            FailureMode::MemoryLimitExceeded { .. } => HealthStatus::ResourceExhausted,
            FailureMode::TrapStackOverflow => HealthStatus::ResourceExhausted,
            FailureMode::TrapUnreachable
            | FailureMode::TrapDivByZero
            | FailureMode::TrapIntegerOverflow
            | FailureMode::TrapBadConversionToInteger
            | FailureMode::TrapMemoryOutOfBounds
            | FailureMode::TrapHeapMisaligned
            | FailureMode::TrapTableOutOfBounds
            | FailureMode::TrapIndirectCallToNull
            | FailureMode::TrapBadSignature
            | FailureMode::TrapNullReference
            | FailureMode::TrapCastFailure
            | FailureMode::TrapOther(_) => HealthStatus::Trapped,
            FailureMode::InvalidModule(_) | FailureMode::MissingEntrypoint { .. } => {
                HealthStatus::InvalidModule
            }
            FailureMode::HostError(_) => HealthStatus::Corrupted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_categories_for_each_variant() {
        // Sanity check that no two variants share a `category()` string,
        // which would defeat the analyzer's failure-mode-keyed routing.
        let modes = vec![
            FailureMode::Timeout { limit_ms: 500, observed_ms: 502 },
            FailureMode::FuelExhausted { limit: 10_000 },
            FailureMode::TrapUnreachable,
            FailureMode::TrapDivByZero,
            FailureMode::TrapIntegerOverflow,
            FailureMode::TrapBadConversionToInteger,
            FailureMode::TrapStackOverflow,
            FailureMode::TrapMemoryOutOfBounds,
            FailureMode::TrapHeapMisaligned,
            FailureMode::TrapTableOutOfBounds,
            FailureMode::TrapIndirectCallToNull,
            FailureMode::TrapBadSignature,
            FailureMode::TrapNullReference,
            FailureMode::TrapCastFailure,
            FailureMode::TrapOther("anything".into()),
            FailureMode::MemoryLimitExceeded { pages: 10, limit_pages: 1 },
            FailureMode::InvalidModule("bad".into()),
            FailureMode::MissingEntrypoint { expected: "_start".into() },
            FailureMode::HostError("snapshot failed".into()),
        ];
        let mut seen = std::collections::HashSet::new();
        for m in &modes {
            assert!(
                seen.insert(m.category()),
                "duplicate category `{}` for {:?}",
                m.category(),
                m
            );
        }
    }

    #[test]
    fn load_time_failures_dont_require_rollback() {
        assert!(!FailureMode::MissingEntrypoint { expected: "_start".into() }.requires_rollback());
        assert!(!FailureMode::InvalidModule("x".into()).requires_rollback());
        // Genuine runtime failures still do.
        assert!(FailureMode::TrapUnreachable.requires_rollback());
        assert!(FailureMode::Timeout { limit_ms: 500, observed_ms: 600 }.requires_rollback());
    }

    #[test]
    fn deterministic_traps_are_non_retryable() {
        assert!(FailureMode::TrapDivByZero.is_deterministic());
        assert!(FailureMode::TrapUnreachable.is_deterministic());
        assert!(FailureMode::MissingEntrypoint { expected: "_start".into() }.is_deterministic());
        // Resource-limit failures may succeed with a larger budget.
        assert!(!FailureMode::Timeout { limit_ms: 500, observed_ms: 600 }.is_deterministic());
        assert!(!FailureMode::FuelExhausted { limit: 1 }.is_deterministic());
    }

    #[test]
    fn health_status_mapping_is_correct() {
        let cases = [
            (FailureMode::Timeout { limit_ms: 500, observed_ms: 600 }, HealthStatus::Timeout),
            (FailureMode::FuelExhausted { limit: 1 }, HealthStatus::FuelExhausted),
            (FailureMode::TrapStackOverflow, HealthStatus::ResourceExhausted),
            (FailureMode::TrapDivByZero, HealthStatus::Trapped),
            (FailureMode::TrapUnreachable, HealthStatus::Trapped),
            (
                FailureMode::MissingEntrypoint { expected: "_start".into() },
                HealthStatus::InvalidModule,
            ),
            (FailureMode::InvalidModule("x".into()), HealthStatus::InvalidModule),
            (FailureMode::HostError("x".into()), HealthStatus::Corrupted),
            (
                FailureMode::MemoryLimitExceeded { pages: 2, limit_pages: 1 },
                HealthStatus::ResourceExhausted,
            ),
        ];
        for (mode, expected) in cases {
            let got: HealthStatus = (&mode).into();
            assert_eq!(got, expected, "wrong HealthStatus for {:?}", mode);
        }
    }

    #[test]
    fn wasmtime_trap_round_trips() {
        // Spot-check the variants that the Phase 3 validation actually
        // exercises end-to-end.
        assert_eq!(
            FailureMode::from_wasmtime_trap(&Trap::UnreachableCodeReached),
            FailureMode::TrapUnreachable
        );
        assert_eq!(
            FailureMode::from_wasmtime_trap(&Trap::IntegerDivisionByZero),
            FailureMode::TrapDivByZero
        );
        assert_eq!(
            FailureMode::from_wasmtime_trap(&Trap::StackOverflow),
            FailureMode::TrapStackOverflow
        );
        assert_eq!(
            FailureMode::from_wasmtime_trap(&Trap::OutOfFuel),
            FailureMode::FuelExhausted { limit: 0 }
        );
    }
}
