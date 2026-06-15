//! Precompiled WASM module cache with LRU eviction.
//!
//! SHA-256-keyed `Arc<Module>` reuse avoids recompilation for repeat
//! invocations of the same WASM bytes. Used by both the daemon and
//! the sandbox pool.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};
use wasmtime::{Engine, Module};

pub struct ModuleCache {
    entries: RwLock<CacheInner>,
}

struct CacheInner {
    map: HashMap<[u8; 32], Arc<Module>>,
    order: VecDeque<[u8; 32]>,
    max_entries: usize,
    hits: u64,
    misses: u64,
}

impl ModuleCache {
    pub fn new() -> Self {
        Self::with_capacity(256)
    }

    pub fn with_capacity(max_entries: usize) -> Self {
        ModuleCache {
            entries: RwLock::new(CacheInner {
                map: HashMap::new(),
                order: VecDeque::new(),
                max_entries,
                hits: 0,
                misses: 0,
            }),
        }
    }

    /// Return a compiled `Module` for these WASM bytes, compiling if
    /// not already cached. Evicts LRU entries when capacity is exceeded.
    pub fn get_or_compile(&self, engine: &Engine, bytes: &[u8]) -> wasmtime::Result<Arc<Module>> {
        let hash = Self::hash(bytes);

        // Fast path: cache hit.
        {
            let mut inner = self.entries.write().unwrap();
            if let Some(m) = inner.map.get(&hash).cloned() {
                inner.hits += 1;
                // Move to back (most recently used).
                if let Some(pos) = inner.order.iter().position(|h| h == &hash) {
                    inner.order.remove(pos);
                }
                inner.order.push_back(hash);
                return Ok(m);
            }
            inner.misses += 1;
        }

        // Slow path: compile outside lock contention.
        let compiled = Arc::new(Module::from_binary(engine, bytes)?);

        let mut inner = self.entries.write().unwrap();
        // Evict LRU if at capacity.
        while inner.map.len() >= inner.max_entries && !inner.order.is_empty() {
            if let Some(evicted) = inner.order.pop_front() {
                inner.map.remove(&evicted);
            }
        }
        inner.map.insert(hash, compiled.clone());
        inner.order.push_back(hash);
        Ok(compiled)
    }

    pub fn len(&self) -> usize {
        self.entries.read().unwrap().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().unwrap().map.is_empty()
    }

    pub fn clear(&self) {
        let mut inner = self.entries.write().unwrap();
        inner.map.clear();
        inner.order.clear();
    }

    pub fn hits(&self) -> u64 {
        self.entries.read().unwrap().hits
    }

    pub fn misses(&self) -> u64 {
        self.entries.read().unwrap().misses
    }

    fn hash(bytes: &[u8]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(bytes);
        h.finalize().into()
    }
}

impl Default for ModuleCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::Config;

    fn engine() -> Engine {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        Engine::new(&cfg).unwrap()
    }

    #[test]
    fn cache_hit_reuses_module() {
        let cache = ModuleCache::new();
        let eng = engine();
        let wasm =
            wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#)
                .unwrap();
        let a = cache.get_or_compile(&eng, &wasm).unwrap();
        let b = cache.get_or_compile(&eng, &wasm).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
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
        assert_eq!(cache.misses(), 2);
    }

    #[test]
    fn lru_eviction_at_capacity() {
        let cache = ModuleCache::with_capacity(2);
        let eng = engine();
        let w1 = wat::parse_str(r#"(module (func (export "_start")))"#).unwrap();
        let w2 =
            wat::parse_str(r#"(module (func (export "_start") (nop)))"#).unwrap();
        let w3 =
            wat::parse_str(r#"(module (func (export "_start") (nop) (nop)))"#).unwrap();

        cache.get_or_compile(&eng, &w1).unwrap();
        cache.get_or_compile(&eng, &w2).unwrap();
        assert_eq!(cache.len(), 2);

        // Adding w3 should evict w1 (LRU).
        cache.get_or_compile(&eng, &w3).unwrap();
        assert_eq!(cache.len(), 2);

        // w1 should be a miss now.
        cache.get_or_compile(&eng, &w1).unwrap();
        assert_eq!(cache.misses(), 4); // w1, w2, w3, w1-again
    }

    #[test]
    fn access_refreshes_lru_position() {
        let cache = ModuleCache::with_capacity(2);
        let eng = engine();
        let w1 = wat::parse_str(r#"(module (func (export "_start")))"#).unwrap();
        let w2 =
            wat::parse_str(r#"(module (func (export "_start") (nop)))"#).unwrap();
        let w3 =
            wat::parse_str(r#"(module (func (export "_start") (nop) (nop)))"#).unwrap();

        cache.get_or_compile(&eng, &w1).unwrap();
        cache.get_or_compile(&eng, &w2).unwrap();
        // Access w1 again to refresh it.
        cache.get_or_compile(&eng, &w1).unwrap();
        // Now w2 is LRU. Adding w3 should evict w2, not w1.
        cache.get_or_compile(&eng, &w3).unwrap();
        assert_eq!(cache.len(), 2);
        // w1 should still be cached (hit).
        let before_hits = cache.hits();
        cache.get_or_compile(&eng, &w1).unwrap();
        assert_eq!(cache.hits(), before_hits + 1);
    }
}
