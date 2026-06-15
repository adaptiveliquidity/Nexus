//! Snapshot lineage heads and parent-linked fork detection (RFC 0001 §5.1).
//!
//! This module is pure lineage bookkeeping. It records content-addressed
//! snapshot heads in a parent-linked DAG and detects divergent heads for an
//! agent. HLC values provide monotonic event timestamps and deterministic
//! display ordering; ancestry is established only by `parent_digest` links.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::snapshot::sync::digest::SnapshotDigest;

/// Stable identifier for the agent whose current snapshot lineage is tracked.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AgentId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for AgentId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Stable identifier for the node that produced a lineage-head update.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for NodeId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for NodeId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Hybrid logical clock timestamp.
///
/// Ordering is `(physical_millis, logical, node_id)`. The physical component is
/// not a correctness proof for lineage ancestry; it only feeds the HLC event
/// algorithm and provides a human-meaningful sort key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HlcTimestamp {
    physical_millis: u64,
    logical: u64,
    node_id: NodeId,
}

impl HlcTimestamp {
    /// Create a fresh local timestamp for `node_id`.
    pub fn now(node_id: NodeId) -> Self {
        Self {
            physical_millis: physical_millis_now(),
            logical: 0,
            node_id,
        }
    }

    /// Build a timestamp from explicit parts.
    pub fn from_parts(physical_millis: u64, logical: u64, node_id: NodeId) -> Self {
        Self {
            physical_millis,
            logical,
            node_id,
        }
    }

    pub fn physical_millis(&self) -> u64 {
        self.physical_millis
    }

    pub fn logical(&self) -> u64 {
        self.logical
    }

    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// Advance the clock for a local event.
    pub fn tick(&mut self) -> Self {
        let now = physical_millis_now();
        if now > self.physical_millis {
            self.physical_millis = now;
            self.logical = 0;
        } else {
            self.logical = self.logical.saturating_add(1);
        }
        self.clone()
    }

    /// Incorporate an observed timestamp and advance for the receive event.
    pub fn update(&mut self, observed: &Self) -> Self {
        let now = physical_millis_now();
        let local_physical = self.physical_millis;
        let observed_physical = observed.physical_millis;
        let max_physical = now.max(local_physical).max(observed_physical);

        self.logical = if max_physical == local_physical && max_physical == observed_physical {
            self.logical.max(observed.logical).saturating_add(1)
        } else if max_physical == local_physical {
            self.logical.saturating_add(1)
        } else if max_physical == observed_physical {
            observed.logical.saturating_add(1)
        } else {
            0
        };
        self.physical_millis = max_physical;
        self.clone()
    }
}

impl Ord for HlcTimestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.physical_millis, self.logical, &self.node_id).cmp(&(
            other.physical_millis,
            other.logical,
            &other.node_id,
        ))
    }
}

impl PartialOrd for HlcTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A per-agent lineage-head update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageHead {
    pub agent_id: AgentId,
    pub head_digest: SnapshotDigest,
    pub parent_digest: Option<SnapshotDigest>,
    pub hlc: HlcTimestamp,
    pub node_id: NodeId,
    pub update_id: Uuid,
}

impl LineageHead {
    pub fn new(
        agent_id: AgentId,
        head_digest: SnapshotDigest,
        parent_digest: Option<SnapshotDigest>,
        hlc: HlcTimestamp,
        node_id: NodeId,
    ) -> Self {
        Self {
            agent_id,
            head_digest,
            parent_digest,
            hlc,
            node_id,
            update_id: Uuid::new_v4(),
        }
    }
}

/// A surfaced divergent-head condition for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageFork {
    pub agent_id: AgentId,
    pub left: SnapshotDigest,
    pub right: SnapshotDigest,
}

/// Result of applying one lineage-head update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineageUpdate {
    /// The update created the first head for the agent.
    InsertedRoot,
    /// The update was already known.
    AlreadyKnown,
    /// The update is an ancestor of an existing head, so the descendant remains
    /// the visible head.
    StaleAncestor { descendant: SnapshotDigest },
    /// The update descended from existing head(s), so they were fast-forwarded.
    FastForwarded { replaced: Vec<SnapshotDigest> },
    /// The update introduced or preserved divergent heads.
    Forked { forks: Vec<LineageFork> },
}

/// In-memory lineage DAG keyed by snapshot digest, with surfaced fork records.
#[derive(Debug, Default)]
pub struct LineageStore {
    nodes: HashMap<SnapshotDigest, LineageHead>,
    heads_by_agent: HashMap<AgentId, HashSet<SnapshotDigest>>,
    forks: Vec<LineageFork>,
}

impl LineageStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, digest: &SnapshotDigest) -> bool {
        self.nodes.contains_key(digest)
    }

    pub fn get(&self, digest: &SnapshotDigest) -> Option<&LineageHead> {
        self.nodes.get(digest)
    }

    pub fn forks(&self) -> &[LineageFork] {
        &self.forks
    }

    /// Current heads for an agent, sorted by HLC for stable presentation.
    pub fn heads_for(&self, agent_id: &AgentId) -> Vec<&LineageHead> {
        let mut heads: Vec<&LineageHead> = self
            .heads_by_agent
            .get(agent_id)
            .into_iter()
            .flat_map(|digests| digests.iter())
            .filter_map(|digest| self.nodes.get(digest))
            .collect();
        heads.sort_by(|a, b| a.hlc.cmp(&b.hlc).then(a.head_digest.cmp(&b.head_digest)));
        heads
    }

    /// Append a lineage node and update the agent's visible head set.
    ///
    /// Parent links decide whether an update is a fast-forward or a fork. HLC is
    /// intentionally not used to merge divergent heads.
    pub fn apply_head(&mut self, head: LineageHead) -> LineageUpdate {
        if self.nodes.contains_key(&head.head_digest) {
            return LineageUpdate::AlreadyKnown;
        }

        let agent_id = head.agent_id.clone();
        let head_digest = head.head_digest;
        self.nodes.insert(head_digest, head);

        let current_heads: Vec<SnapshotDigest> = self
            .heads_by_agent
            .get(&agent_id)
            .map(|heads| heads.iter().copied().collect())
            .unwrap_or_default();

        if current_heads.is_empty() {
            self.heads_by_agent
                .entry(agent_id)
                .or_default()
                .insert(head_digest);
            return LineageUpdate::InsertedRoot;
        }

        let mut replaced = Vec::new();
        let mut forks = Vec::new();
        let mut descendants = Vec::new();

        for current in current_heads {
            if self.is_ancestor_digest(current, head_digest) {
                replaced.push(current);
            } else if self.is_ancestor_digest(head_digest, current) {
                descendants.push(current);
            } else {
                forks.push(LineageFork {
                    agent_id: agent_id.clone(),
                    left: current,
                    right: head_digest,
                });
            }
        }

        let heads = self.heads_by_agent.entry(agent_id).or_default();
        if replaced.is_empty() && !descendants.is_empty() && forks.is_empty() {
            return LineageUpdate::StaleAncestor {
                descendant: descendants[0],
            };
        }

        for digest in &replaced {
            heads.remove(digest);
        }
        if descendants.is_empty() {
            heads.insert(head_digest);
        }

        if forks.is_empty() {
            LineageUpdate::FastForwarded { replaced }
        } else {
            self.forks.extend(forks.clone());
            LineageUpdate::Forked { forks }
        }
    }

    /// True when `ancestor` is reachable by following `descendant` parent links.
    pub fn is_ancestor_digest(&self, ancestor: SnapshotDigest, descendant: SnapshotDigest) -> bool {
        let mut cursor = Some(descendant);
        let mut seen = HashSet::new();

        while let Some(digest) = cursor {
            if digest == ancestor {
                return true;
            }
            if !seen.insert(digest) {
                return false;
            }
            cursor = self.nodes.get(&digest).and_then(|head| head.parent_digest);
        }

        false
    }
}

fn physical_millis_now() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    millis.min(u128::from(u64::MAX)) as u64
}
