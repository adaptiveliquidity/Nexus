//! Precompiled WASM module cache — Phase C.
//!
//! Today's `WasmSandbox::execute` calls `Module::from_binary` on every
//! invocation; for non-trivial modules that means a cranelift compile
//! per call. The daemon serves the same WASM many times, so caching the
//! compiled `Module` keyed on SHA-256 of the bytes turns a repeat call
//! into an `Arc<Module>` clone.
//!
//! This module exposes `ModuleCache::get_or_compile(engine, bytes)` and
//! a `clear()` for tests. The hash is computed every call (cheap
//! relative to compilation) so the caller doesn't have to pre-hash.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};
use wasmtime::{Engine, Module};

#[derive(Default)]
pub struct ModuleCache {
    entries: RwLock<HashMap<[u8; 32], Arc<Module>>>,
}

impl ModuleCache {
    pub fn new() -> Self {
        ModuleCache::default()
    }

    /// Return a compiled `Module` for these WASM bytes, compiling if
    /// not already cached. Cloning an `Arc<Module>` is cheap; cloning
    /// the underlying `Module` is also cheap (it is internally Arc-d).
    pub fn get_or_compile(&self, engine: &Engine, bytes: &[u8]) -> wasmtime::Result<Arc<Module>> {
        let hash = Self::hash(bytes);
        if let Some(m) = self.entries.read().unwrap().get(&hash) {
            return Ok(m.clone());
        }
        // Compile outside the lock so concurrent compiles do not
        // serialize. The lock is only re-taken to insert. If two
        // requests race to compile the same module, the later one
        // simply replaces; both end up with valid Modules.
        let compiled = Arc::new(Module::from_binary(engine, bytes)?);
        let mut entries = self.entries.write().unwrap();
        entries.insert(hash, compiled.clone());
        Ok(compiled)
    }

    pub fn len(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().unwrap().is_empty()
    }

    pub fn clear(&self) {
        self.entries.write().unwrap().clear();
    }

    fn hash(bytes: &[u8]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(bytes);
        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::{Config, Engine};

    fn engine() -> Engine {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        Engine::new(&cfg).unwrap()
    }

    #[test]
    fn second_call_reuses_compiled_module() {
        let cache = ModuleCache::new();
        let eng = engine();
        let wasm =
            wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#)
                .unwrap();
        let a = cache.get_or_compile(&eng, &wasm).unwrap();
        let b = cache.get_or_compile(&eng, &wasm).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "cache should return the same Arc");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn distinct_bytes_distinct_entries() {
        let cache = ModuleCache::new();
        let eng = engine();
        let w1 = wat::parse_str(r#"(module (func (export "_start")))"#).unwrap();
        let w2 =
            wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#)
                .unwrap();
        cache.get_or_compile(&eng, &w1).unwrap();
        cache.get_or_compile(&eng, &w2).unwrap();
        assert_eq!(cache.len(), 2);
    }
}
