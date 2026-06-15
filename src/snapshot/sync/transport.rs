//! Test-only transport abstraction for the Phase 2 sync state machine.
//!
//! This exists to exercise [`SyncNode`](super::protocol::SyncNode) message flow
//! end to end. It is **not** production transport — there is no real socket,
//! framing, authentication, or anti-replay. Production daemon/gRPC transport is
//! intentionally deferred to later RFC-0001 phases.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::error::{NexusError, Result};
use crate::snapshot::sync::protocol::{SyncMessage, SyncNode};

/// A bidirectional message channel endpoint: send to the peer, poll for inbound.
pub trait SyncTransport {
    fn send(&mut self, msg: SyncMessage);
    fn try_recv(&mut self) -> Option<SyncMessage>;
}

/// A pair of in-memory endpoints wired together by two queues. Whatever one
/// endpoint sends, the other receives. Test-only.
pub struct InMemoryTransport {
    outbound: Rc<RefCell<VecDeque<SyncMessage>>>,
    inbound: Rc<RefCell<VecDeque<SyncMessage>>>,
}

impl InMemoryTransport {
    /// Create two cross-wired endpoints `(a, b)`: `a.send` is read by
    /// `b.try_recv` and vice versa.
    pub fn pair() -> (Self, Self) {
        let a_to_b = Rc::new(RefCell::new(VecDeque::new()));
        let b_to_a = Rc::new(RefCell::new(VecDeque::new()));
        let a = InMemoryTransport {
            outbound: a_to_b.clone(),
            inbound: b_to_a.clone(),
        };
        let b = InMemoryTransport {
            outbound: b_to_a,
            inbound: a_to_b,
        };
        (a, b)
    }
}

impl SyncTransport for InMemoryTransport {
    fn send(&mut self, msg: SyncMessage) {
        self.outbound.borrow_mut().push_back(msg);
    }

    fn try_recv(&mut self) -> Option<SyncMessage> {
        self.inbound.borrow_mut().pop_front()
    }
}

/// Drive replication between two nodes over their transport endpoints until the
/// protocol goes quiescent (no node has anything left to say).
///
/// `a` advertises first; messages are pumped both directions each round. Bounded
/// by `max_steps` rounds so a misbehaving protocol cannot loop forever —
/// exceeding the bound returns an error rather than hanging.
pub fn replicate<TA, TB>(
    a: &mut SyncNode,
    ta: &mut TA,
    b: &mut SyncNode,
    tb: &mut TB,
    max_steps: usize,
) -> Result<()>
where
    TA: SyncTransport,
    TB: SyncTransport,
{
    ta.send(a.advertise());

    let mut steps = 0;
    loop {
        let mut progressed = false;

        // Messages a -> b.
        while let Some(msg) = tb.try_recv() {
            for out in b.handle(msg) {
                tb.send(out);
            }
            progressed = true;
        }
        // Messages b -> a.
        while let Some(msg) = ta.try_recv() {
            for out in a.handle(msg) {
                ta.send(out);
            }
            progressed = true;
        }

        if !progressed {
            return Ok(()); // quiescent
        }

        steps += 1;
        if steps >= max_steps {
            return Err(NexusError::ConfigError(format!(
                "snapshot-sync replicate did not reach quiescence within {max_steps} steps"
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::manager::{ExecutionState, FilesystemDiff, Snapshot, SnapshotMetadata};

    fn snap(mem: &[u8]) -> Snapshot {
        Snapshot::new(
            mem.to_vec(),
            FilesystemDiff::new(),
            ExecutionState::default(),
            SnapshotMetadata::new("op".into(), "in".into()),
        )
        .unwrap()
    }

    #[test]
    fn in_memory_pair_delivers_both_directions() {
        let (mut a, mut b) = InMemoryTransport::pair();
        a.send(SyncMessage::Ack {
            digest: crate::snapshot::sync::digest_of(&snap(b"x")).unwrap(),
        });
        assert!(matches!(b.try_recv(), Some(SyncMessage::Ack { .. })));
        assert!(a.try_recv().is_none());
    }
}
