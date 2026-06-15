//! Integration tests for snapshot lineage heads and HLC fork detection.

use nexus::snapshot::sync::{
    AgentId, HlcTimestamp, LineageHead, LineageStore, LineageUpdate, NodeId, SnapshotDigest,
};

fn digest(byte: u8) -> SnapshotDigest {
    SnapshotDigest::from_bytes([byte; 32])
}

fn head(agent: &AgentId, node: &NodeId, id: u8, parent: Option<SnapshotDigest>) -> LineageHead {
    LineageHead::new(
        agent.clone(),
        digest(id),
        parent,
        HlcTimestamp::from_parts(u64::from(id), 0, node.clone()),
        node.clone(),
    )
}

#[test]
fn hlc_tick_is_monotonic_and_update_advances_past_observed_future() {
    let node_a = NodeId::new("node-a");
    let node_b = NodeId::new("node-b");
    let mut local = HlcTimestamp::now(node_a.clone());

    let after_tick = local.tick();
    assert!(after_tick > HlcTimestamp::from_parts(0, 0, node_a));

    let observed_future =
        HlcTimestamp::from_parts(after_tick.physical_millis() + 10_000, 7, node_b);
    let after_update = local.update(&observed_future);

    assert_eq!(
        after_update.physical_millis(),
        observed_future.physical_millis()
    );
    assert_eq!(after_update.logical(), observed_future.logical() + 1);
    assert!(after_update > observed_future);
}

#[test]
fn linear_child_fast_forwards_current_head_without_fork() {
    let agent = AgentId::new("agent-1");
    let node = NodeId::new("node-a");
    let mut store = LineageStore::new();

    assert_eq!(
        store.apply_head(head(&agent, &node, 1, None)),
        LineageUpdate::InsertedRoot
    );

    let update = store.apply_head(head(&agent, &node, 2, Some(digest(1))));
    assert_eq!(
        update,
        LineageUpdate::FastForwarded {
            replaced: vec![digest(1)]
        }
    );
    assert!(store.forks().is_empty());

    let heads = store.heads_for(&agent);
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].head_digest, digest(2));
}

#[test]
fn divergent_children_of_same_parent_are_surfaced_as_fork_not_merged() {
    let agent = AgentId::new("agent-1");
    let node_a = NodeId::new("node-a");
    let node_b = NodeId::new("node-b");
    let mut store = LineageStore::new();

    store.apply_head(head(&agent, &node_a, 1, None));
    store.apply_head(head(&agent, &node_a, 2, Some(digest(1))));
    let update = store.apply_head(head(&agent, &node_b, 3, Some(digest(1))));

    match update {
        LineageUpdate::Forked { forks } => {
            assert_eq!(forks.len(), 1);
            assert_eq!(forks[0].left, digest(2));
            assert_eq!(forks[0].right, digest(3));
        }
        other => panic!("expected fork, got {other:?}"),
    }

    let heads = store.heads_for(&agent);
    let head_digests: Vec<SnapshotDigest> = heads.iter().map(|h| h.head_digest).collect();
    assert_eq!(head_digests, vec![digest(2), digest(3)]);
    assert_eq!(store.forks().len(), 1);
}

#[test]
fn deep_ancestor_checks_follow_parent_links() {
    let agent = AgentId::new("agent-1");
    let node = NodeId::new("node-a");
    let mut store = LineageStore::new();

    store.apply_head(head(&agent, &node, 1, None));
    store.apply_head(head(&agent, &node, 2, Some(digest(1))));
    store.apply_head(head(&agent, &node, 3, Some(digest(2))));
    store.apply_head(head(&agent, &node, 4, Some(digest(3))));

    assert!(store.is_ancestor_digest(digest(1), digest(4)));
    assert!(store.is_ancestor_digest(digest(2), digest(4)));
    assert!(store.is_ancestor_digest(digest(3), digest(4)));
    assert!(!store.is_ancestor_digest(digest(4), digest(1)));
    assert!(!store.is_ancestor_digest(digest(2), digest(99)));
}

#[test]
fn late_arriving_ancestor_does_not_replace_descendant_head() {
    let agent = AgentId::new("agent-1");
    let node = NodeId::new("node-a");
    let mut store = LineageStore::new();

    store.apply_head(head(&agent, &node, 2, Some(digest(1))));
    let update = store.apply_head(head(&agent, &node, 1, None));

    assert_eq!(
        update,
        LineageUpdate::StaleAncestor {
            descendant: digest(2)
        }
    );
    let heads = store.heads_for(&agent);
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].head_digest, digest(2));
}
