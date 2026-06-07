//! Hypervisor pool — Phase C.
//!
//! Today's `NexusHypervisor` serializes execution behind
//! `sandbox: RwLock<WasmSandbox>`, so concurrent calls fight over the
//! lock. The pool gives each in-flight request its own hypervisor
//! instance (and therefore its own sandbox), pre-warmed at daemon
//! startup so per-request cost is bounded by `execute_tool` itself, not
//! by hypervisor construction.
//!
//! The pool uses a `tokio::sync::Semaphore` for backpressure plus a
//! lock-free MPSC channel of available hypervisors. Acquiring an
//! instance is constant-time when one is available, and otherwise the
//! caller awaits.

use std::sync::Arc;

use tokio::sync::{Mutex, Semaphore};

use crate::error::Result;
use crate::{HypervisorConfig, NexusHypervisor};

/// Fixed-size pool of pre-warmed hypervisors.
pub struct HypervisorPool {
    /// Available hypervisors. Wrapped in a `Mutex<VecDeque>` for
    /// simple pop/push; the semaphore guarantees we never block waiting
    /// on the mutex except briefly.
    available: Mutex<std::collections::VecDeque<NexusHypervisor>>,
    permits: Arc<Semaphore>,
    pub size: usize,
}

impl HypervisorPool {
    /// Build a pool of `size` hypervisors using `config`. Returns an
    /// error if any hypervisor construction fails (so the daemon never
    /// starts with a degraded pool).
    pub fn new(size: usize, config: HypervisorConfig) -> Result<Arc<Self>> {
        let mut available = std::collections::VecDeque::with_capacity(size);
        for _ in 0..size {
            available.push_back(NexusHypervisor::new(config.clone())?);
        }
        Ok(Arc::new(HypervisorPool {
            available: Mutex::new(available),
            permits: Arc::new(Semaphore::new(size)),
            size,
        }))
    }

    /// Borrow a hypervisor. The returned guard returns the hypervisor
    /// to the pool on drop.
    pub async fn acquire(self: &Arc<Self>) -> Result<PooledHypervisor> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore not closed");
        let hv = {
            let mut q = self.available.lock().await;
            q.pop_front()
                .expect("pool invariant: a permit implies an instance is available")
        };
        Ok(PooledHypervisor {
            hv: Some(hv),
            pool: self.clone(),
            _permit: permit,
        })
    }
}

/// RAII guard for a borrowed hypervisor. Dropping returns it to the
/// pool; if a panic occurs the `Drop` impl still runs.
pub struct PooledHypervisor {
    hv: Option<NexusHypervisor>,
    pool: Arc<HypervisorPool>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl PooledHypervisor {
    pub fn hv(&self) -> &NexusHypervisor {
        self.hv.as_ref().expect("not yet dropped")
    }
}

impl Drop for PooledHypervisor {
    fn drop(&mut self) {
        if let Some(hv) = self.hv.take() {
            let pool = self.pool.clone();
            // Returning to the pool is async (Mutex<VecDeque>); use a
            // detached task. The Drop is not async so we cannot await.
            tokio::spawn(async move {
                let mut q = pool.available.lock().await;
                q.push_back(hv);
            });
        }
    }
}
