//! Instinct store — Phase B port of ECC `skills/continuous-learning-v2/` to Rust.
//!
//! An `Instinct` is a single failure-mode-keyed recovery suggestion learned
//! from past attempts. When a recovery action is applied and the next
//! attempt succeeds, the matching instinct's `confidence` is reinforced;
//! when it fails, the instinct erodes. The store is persisted as one
//! JSON file per `FailureMode::category()` under
//! `$NEXUS_HOME/instincts/<category>.json` (default `~/.nexus/instincts/`).
//!
//! The store implements `RecoveryPolicy` so it can be slotted into the
//! existing `LayeredPolicy` (Static + Instinct + LLM) without any
//! hypervisor-side changes.
//!
//! Confidence math (mirrors continuous-learning-v2 in spirit):
//!   - new instinct: confidence = 0.5, support = 0, failure = 0
//!   - on success: support += 1; confidence = bayes_smooth(support, failure)
//!   - on failure: failure += 1; confidence = bayes_smooth(support, failure)
//!   - bayes_smooth = (support + 1) / (support + failure + 2)   (Laplace)
//!
//! This guarantees confidence stays in (0, 1) and converges to the empirical
//! success rate as evidence accumulates.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{NexusError, Result};
use crate::hypervisor::failure_mode::FailureMode;
use crate::hypervisor::recovery::{RecoveryAction, RecoveryPolicy, RecoverySource};

/// One learned recovery suggestion for a specific failure category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instinct {
    pub id: Uuid,
    /// `FailureMode::category()` value (e.g. `"TRAP_DIV_BY_ZERO"`).
    pub failure_category: String,
    /// Optional glob-ish operation pattern (`"*"` matches all). Stored
    /// alongside so a per-tool instinct does not pollute the global pool.
    pub operation_pattern: String,
    /// Human-readable recovery advice. This is what gets surfaced as a
    /// `RecoveryAction.description` when the instinct is consulted.
    pub recovery_description: String,
    /// Bayes-smoothed success probability in `(0, 1)`.
    pub confidence: f32,
    /// Number of times this instinct was applied and the next attempt
    /// succeeded.
    pub support_count: u64,
    /// Number of times this instinct was applied and the next attempt
    /// still failed (same or different failure mode).
    pub failure_count: u64,
    pub created_at: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
}

impl Instinct {
    fn new(failure_category: String, operation_pattern: String, recovery_description: String) -> Self {
        let now = Utc::now();
        Instinct {
            id: Uuid::new_v4(),
            failure_category,
            operation_pattern,
            recovery_description,
            confidence: 0.5,
            support_count: 0,
            failure_count: 0,
            created_at: now,
            last_updated: now,
        }
    }

    /// Laplace-smoothed success probability. Always in (0, 1).
    fn recompute_confidence(&mut self) {
        let s = self.support_count as f32;
        let f = self.failure_count as f32;
        self.confidence = (s + 1.0) / (s + f + 2.0);
        self.last_updated = Utc::now();
    }

    fn record_success(&mut self) {
        self.support_count = self.support_count.saturating_add(1);
        self.recompute_confidence();
    }

    fn record_failure(&mut self) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.recompute_confidence();
    }

    /// Returns true if this instinct's `operation_pattern` matches the
    /// given operation name. The matcher is intentionally trivial: exact
    /// match, or `"*"` to match anything.
    fn matches_operation(&self, operation: &str) -> bool {
        self.operation_pattern == "*" || self.operation_pattern == operation
    }
}

/// Snapshot statistics for the `nexus instinct status` subcommand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstinctStats {
    pub total_instincts: u64,
    pub categories: HashMap<String, u64>,
    pub avg_confidence: f32,
    pub highest_confidence: Option<(String, f32)>, // (description, conf)
    pub total_support: u64,
    pub total_failures: u64,
}

/// File-backed instinct store. Cached in memory; persisted lazily on every
/// mutation. Multi-process access is not supported (single hypervisor
/// process per `dir` at a time).
pub struct InstinctStore {
    dir: PathBuf,
    cache: RwLock<HashMap<String, Vec<Instinct>>>,
}

impl InstinctStore {
    /// Open (or create) a store at `dir`. The directory is created if it
    /// does not exist; existing `<category>.json` files are loaded into
    /// the in-memory cache.
    pub fn open(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir).map_err(|e| {
            NexusError::FilesystemError(format!("instinct dir {dir:?}: {e}"))
        })?;

        let mut cache: HashMap<String, Vec<Instinct>> = HashMap::new();
        for entry in fs::read_dir(&dir).map_err(|e| {
            NexusError::FilesystemError(format!("read_dir {dir:?}: {e}"))
        })? {
            let entry = entry.map_err(|e| {
                NexusError::FilesystemError(format!("entry: {e}"))
            })?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path).map_err(|e| {
                NexusError::FilesystemError(format!("read {path:?}: {e}"))
            })?;
            let parsed: Vec<Instinct> = serde_json::from_slice(&bytes).map_err(|e| {
                NexusError::SerializationError(format!("parse {path:?}: {e}"))
            })?;
            if let Some(first) = parsed.first() {
                cache.insert(first.failure_category.clone(), parsed);
            }
        }

        Ok(InstinctStore {
            dir,
            cache: RwLock::new(cache),
        })
    }

    /// Default location: `$NEXUS_HOME/instincts/` or `~/.nexus/instincts/`.
    pub fn default_dir() -> PathBuf {
        if let Ok(custom) = std::env::var("NEXUS_HOME") {
            PathBuf::from(custom).join("instincts")
        } else {
            dirs_like_home()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".nexus")
                .join("instincts")
        }
    }

    /// Open at the default location.
    pub fn open_default() -> Result<Self> {
        Self::open(Self::default_dir())
    }

    /// Query instincts matching a failure mode (and optionally an
    /// operation name). Returns by descending confidence.
    pub fn query(&self, mode: &FailureMode, operation: &str) -> Vec<Instinct> {
        let cache = self.cache.read().unwrap();
        let key = mode.category();
        let mut out: Vec<Instinct> = cache
            .get(key)
            .map(|v| v.iter().filter(|i| i.matches_operation(operation)).cloned().collect())
            .unwrap_or_default();
        out.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// Register a brand-new instinct (or return an existing one with the
    /// same `(category, operation_pattern, recovery_description)` triple).
    /// Returns the instinct id.
    pub fn register(
        &self,
        mode: &FailureMode,
        operation_pattern: &str,
        recovery_description: &str,
    ) -> Result<Uuid> {
        let key = mode.category().to_string();
        let id;
        {
            let mut cache = self.cache.write().unwrap();
            let bucket = cache.entry(key.clone()).or_default();
            if let Some(existing) = bucket.iter().find(|i| {
                i.operation_pattern == operation_pattern
                    && i.recovery_description == recovery_description
            }) {
                id = existing.id;
            } else {
                let inst = Instinct::new(
                    key.clone(),
                    operation_pattern.to_string(),
                    recovery_description.to_string(),
                );
                id = inst.id;
                bucket.push(inst);
            }
        }
        self.persist_category(&key)?;
        Ok(id)
    }

    /// Reinforce an instinct after a successful retry. Returns `true` if
    /// the instinct existed, `false` otherwise (caller may want to
    /// register-then-record-success in that case).
    pub fn record_success(&self, instinct_id: &Uuid) -> Result<bool> {
        let mut found_category: Option<String> = None;
        {
            let mut cache = self.cache.write().unwrap();
            for (category, bucket) in cache.iter_mut() {
                if let Some(i) = bucket.iter_mut().find(|i| &i.id == instinct_id) {
                    i.record_success();
                    found_category = Some(category.clone());
                    break;
                }
            }
        }
        if let Some(cat) = found_category {
            self.persist_category(&cat)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Erode an instinct after a failed retry.
    pub fn record_failure(&self, instinct_id: &Uuid) -> Result<bool> {
        let mut found_category: Option<String> = None;
        {
            let mut cache = self.cache.write().unwrap();
            for (category, bucket) in cache.iter_mut() {
                if let Some(i) = bucket.iter_mut().find(|i| &i.id == instinct_id) {
                    i.record_failure();
                    found_category = Some(category.clone());
                    break;
                }
            }
        }
        if let Some(cat) = found_category {
            self.persist_category(&cat)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Aggregate statistics for the `instinct status` subcommand.
    pub fn stats(&self) -> InstinctStats {
        let cache = self.cache.read().unwrap();
        let mut stats = InstinctStats::default();
        let mut conf_sum: f32 = 0.0;
        let mut best: Option<(String, f32)> = None;
        for (cat, bucket) in cache.iter() {
            stats.categories.insert(cat.clone(), bucket.len() as u64);
            for i in bucket {
                stats.total_instincts += 1;
                stats.total_support += i.support_count;
                stats.total_failures += i.failure_count;
                conf_sum += i.confidence;
                if best.as_ref().is_none_or(|(_, c)| i.confidence > *c) {
                    best = Some((i.recovery_description.clone(), i.confidence));
                }
            }
        }
        stats.avg_confidence = if stats.total_instincts > 0 {
            conf_sum / stats.total_instincts as f32
        } else {
            0.0
        };
        stats.highest_confidence = best;
        stats
    }

    /// Export every instinct as a single JSON array. Useful for sharing
    /// learned knowledge across hosts.
    pub fn export_all(&self) -> Result<String> {
        let cache = self.cache.read().unwrap();
        let all: Vec<&Instinct> = cache.values().flat_map(|v| v.iter()).collect();
        serde_json::to_string_pretty(&all).map_err(|e| {
            NexusError::SerializationError(format!("export: {e}"))
        })
    }

    /// Import an array of instincts produced by `export_all`. Returns
    /// `(added, merged)` counts. An identical `(category, pattern,
    /// description)` triple is merged (counts and confidence preserved
    /// from the existing entry).
    pub fn import_all(&self, json: &str) -> Result<(usize, usize)> {
        let parsed: Vec<Instinct> = serde_json::from_str(json).map_err(|e| {
            NexusError::SerializationError(format!("import: {e}"))
        })?;
        let mut added = 0;
        let mut merged = 0;
        let mut touched: std::collections::HashSet<String> = Default::default();
        {
            let mut cache = self.cache.write().unwrap();
            for inc in parsed {
                let bucket = cache.entry(inc.failure_category.clone()).or_default();
                let duplicate = bucket.iter().any(|i| {
                    i.operation_pattern == inc.operation_pattern
                        && i.recovery_description == inc.recovery_description
                });
                if duplicate {
                    merged += 1;
                } else {
                    added += 1;
                    touched.insert(inc.failure_category.clone());
                    bucket.push(inc);
                }
            }
        }
        for cat in touched {
            self.persist_category(&cat)?;
        }
        Ok((added, merged))
    }

    fn persist_category(&self, category: &str) -> Result<()> {
        let cache = self.cache.read().unwrap();
        let bucket = match cache.get(category) {
            Some(b) => b,
            None => return Ok(()),
        };
        let path = self.dir.join(format!("{category}.json"));
        let bytes = serde_json::to_vec_pretty(bucket).map_err(|e| {
            NexusError::SerializationError(format!("serialize {category}: {e}"))
        })?;
        fs::write(&path, bytes).map_err(|e| {
            NexusError::FilesystemError(format!("write {path:?}: {e}"))
        })?;
        Ok(())
    }
}

/// `RecoveryPolicy` adapter that consults the instinct store and emits
/// `RecoveryAction`s tagged `Instinct`. Returns instincts sorted by
/// confidence so the merge step in `LayeredPolicy` keeps the strongest
/// recommendation first.
///
/// The action's `non_retryable` flag is left `false` here because the
/// instinct store does not know whether the failure mode is deterministic;
/// `StaticPolicy` (which does know) is the authoritative source for that
/// flag in the merged output.
pub struct InstinctPolicy {
    store: std::sync::Arc<InstinctStore>,
    min_confidence: f32,
}

impl InstinctPolicy {
    pub fn new(store: std::sync::Arc<InstinctStore>) -> Self {
        InstinctPolicy { store, min_confidence: 0.0 }
    }

    /// Filter out instincts below this confidence threshold.
    pub fn with_min_confidence(mut self, c: f32) -> Self {
        self.min_confidence = c.clamp(0.0, 1.0);
        self
    }
}

impl RecoveryPolicy for InstinctPolicy {
    fn recover(&self, mode: &FailureMode, operation: &str) -> Vec<RecoveryAction> {
        self.store
            .query(mode, operation)
            .into_iter()
            .filter(|i| i.confidence >= self.min_confidence)
            .map(|i| RecoveryAction {
                description: i.recovery_description,
                confidence: i.confidence,
                source: RecoverySource::Instinct,
                non_retryable: false,
                instinct_id: Some(i.id),
            })
            .collect()
    }
}

/// Cross-platform home-dir lookup that does not need the `dirs` crate.
fn dirs_like_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        if !userprofile.is_empty() {
            return Some(PathBuf::from(userprofile));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn mode() -> FailureMode {
        FailureMode::TrapDivByZero
    }

    #[test]
    fn empty_store_returns_nothing() {
        let tmp = tempdir().unwrap();
        let s = InstinctStore::open(tmp.path().to_path_buf()).unwrap();
        assert!(s.query(&mode(), "any").is_empty());
        let st = s.stats();
        assert_eq!(st.total_instincts, 0);
    }

    #[test]
    fn register_then_query_round_trips() {
        let tmp = tempdir().unwrap();
        let s = InstinctStore::open(tmp.path().to_path_buf()).unwrap();
        let id = s.register(&mode(), "*", "Guard divisor != 0").unwrap();
        let hits = s.query(&mode(), "anything");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
        assert!((hits[0].confidence - 0.5).abs() < 1e-6);
    }

    #[test]
    fn confidence_increases_with_support() {
        let tmp = tempdir().unwrap();
        let s = InstinctStore::open(tmp.path().to_path_buf()).unwrap();
        let id = s.register(&mode(), "*", "guard").unwrap();
        for _ in 0..10 {
            assert!(s.record_success(&id).unwrap());
        }
        let q = s.query(&mode(), "x");
        // (10 + 1) / (10 + 0 + 2) = 11/12 ~ 0.9167
        assert!(q[0].confidence > 0.9);
    }

    #[test]
    fn failures_erode_but_dont_zero() {
        let tmp = tempdir().unwrap();
        let s = InstinctStore::open(tmp.path().to_path_buf()).unwrap();
        let id = s.register(&mode(), "*", "guard").unwrap();
        for _ in 0..3 { s.record_success(&id).unwrap(); }
        for _ in 0..10 { s.record_failure(&id).unwrap(); }
        let q = s.query(&mode(), "x");
        // (3 + 1) / (3 + 10 + 2) = 4/15 ~ 0.267 — eroded, not zero
        assert!(q[0].confidence > 0.0 && q[0].confidence < 0.4);
    }

    #[test]
    fn persistence_round_trips() {
        let tmp = tempdir().unwrap();
        let id;
        {
            let s = InstinctStore::open(tmp.path().to_path_buf()).unwrap();
            id = s.register(&mode(), "tool_a", "guard divisor").unwrap();
            s.record_success(&id).unwrap();
            s.record_success(&id).unwrap();
        }
        // Reopen and confirm the instinct survived.
        let s2 = InstinctStore::open(tmp.path().to_path_buf()).unwrap();
        let q = s2.query(&mode(), "tool_a");
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].id, id);
        assert_eq!(q[0].support_count, 2);
    }

    #[test]
    fn export_import_round_trips() {
        let tmp_a = tempdir().unwrap();
        let tmp_b = tempdir().unwrap();
        let a = InstinctStore::open(tmp_a.path().to_path_buf()).unwrap();
        a.register(&mode(), "*", "guard").unwrap();
        a.register(&FailureMode::TrapStackOverflow, "*", "convert to iteration").unwrap();
        let json = a.export_all().unwrap();

        let b = InstinctStore::open(tmp_b.path().to_path_buf()).unwrap();
        let (added, merged) = b.import_all(&json).unwrap();
        assert_eq!(added, 2);
        assert_eq!(merged, 0);

        // Re-importing the same payload should merge, not duplicate.
        let (added2, merged2) = b.import_all(&json).unwrap();
        assert_eq!(added2, 0);
        assert_eq!(merged2, 2);
        assert_eq!(b.stats().total_instincts, 2);
    }

    #[test]
    fn instinct_policy_only_emits_above_threshold() {
        use std::sync::Arc;
        let tmp = tempdir().unwrap();
        let s = Arc::new(InstinctStore::open(tmp.path().to_path_buf()).unwrap());
        let id_good = s.register(&mode(), "*", "good").unwrap();
        let id_bad = s.register(&mode(), "*", "bad").unwrap();
        for _ in 0..10 { s.record_success(&id_good).unwrap(); }
        for _ in 0..10 { s.record_failure(&id_bad).unwrap(); }

        let policy = InstinctPolicy::new(s).with_min_confidence(0.7);
        let actions = policy.recover(&mode(), "x");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].description, "good");
        assert_eq!(actions[0].source, RecoverySource::Instinct);
    }
}
