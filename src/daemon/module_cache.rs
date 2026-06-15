//! Precompiled WASM module cache.
//!
//! Moved to [`crate::sandbox::module_cache`] so both the daemon and the
//! sandbox pool share one implementation. This module re-exports it to keep
//! the `crate::daemon::module_cache::ModuleCache` path stable for existing
//! callers (e.g. `nexus-agentd`).

pub use crate::sandbox::module_cache::ModuleCache;
