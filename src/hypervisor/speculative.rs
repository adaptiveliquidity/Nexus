//! Speculative Execution with Snapshot Forking
//!
//! Races N recovery branches and returns the first branch that succeeds.
//! Losing branches are cancelled (their futures dropped) and any state they
//! produced is discarded — only the winner's result is returned to the caller.
//!
//! ## Claim taxonomy (anti-overclaim)
//! - [`fork_and_race`] is a **benchmarked-primitive**: a self-contained,
//!   unit-tested racing core that is generic over an async branch executor,
//!   applies a per-branch timeout, and selects a winner by strategy. It does
//!   not restore snapshots itself; the injected executor owns those semantics.
//!   It has no dependency on wasmtime, so its tests are fast and deterministic.
//! - [`NexusHypervisor::speculative_execute`](super::NexusHypervisor::speculative_execute)
//!   is the **opt-in** integration that feeds real tool execution into the
//!   racer. Branches race concurrently, but because they currently share a
//!   single sandbox the wall-clock parallelism is bounded; multi-sandbox
//!   pooling for true parallel branches is **roadmap** (Phase C).
//!
//! ## Snapshot seeding
//! Every branch carries a `base_snapshot_id`, but restoring that state is part
//! of the executor closure. The real hypervisor integration uses that id to
//! seed a fresh instance before branch execution; tests for this generic module
//! may inject in-memory futures that ignore the id.

use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::recovery::RecoveryAction;
use super::{ToolDefinition, ToolOutput};
use crate::error::{NexusError, Result};

/// How the winner is chosen when more than one branch can finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SelectionStrategy {
    /// Return as soon as *any* branch succeeds and cancel the rest. Lowest
    /// latency; this is the headline speculative-recovery behaviour.
    #[default]
    FirstSuccess,
    /// Await every branch, then return the first success in completion order.
    /// Slower, but lets callers measure how many branches succeeded — useful
    /// for benchmarking fork overhead versus sequential retry.
    WaitAll,
}

/// Configuration for a single speculative round.
#[derive(Debug, Clone)]
pub struct SpeculativeConfig {
    /// Maximum number of branches to race. Branches beyond this are dropped
    /// before execution (and counted out of `branches_tried`).
    pub max_branches: usize,
    /// Per-branch wall-clock timeout. A branch that exceeds it is recorded as
    /// `timed_out` and can never win.
    pub branch_timeout: Duration,
    /// How to select the winner.
    pub selection_strategy: SelectionStrategy,
}

impl Default for SpeculativeConfig {
    fn default() -> Self {
        SpeculativeConfig {
            max_branches: 4,
            branch_timeout: Duration::from_secs(5),
            selection_strategy: SelectionStrategy::FirstSuccess,
        }
    }
}

/// One speculative recovery branch. Hypervisor-backed executors restore
/// `base_snapshot_id` before running `tool`; generic test executors may treat
/// it as metadata.
#[derive(Debug, Clone)]
pub struct SpeculativeBranch {
    /// Unique id for this branch (used to identify the winner).
    pub id: Uuid,
    /// The base snapshot every sibling branch forks from.
    pub base_snapshot_id: Uuid,
    /// The tool this branch executes.
    pub tool: ToolDefinition,
    /// The recovery action that motivated this branch.
    pub strategy: RecoveryAction,
}

impl SpeculativeBranch {
    /// Create a branch with a freshly generated id.
    pub fn new(base_snapshot_id: Uuid, tool: ToolDefinition, strategy: RecoveryAction) -> Self {
        SpeculativeBranch {
            id: Uuid::new_v4(),
            base_snapshot_id,
            tool,
            strategy,
        }
    }
}

/// The outcome of a single branch after it finishes, fails, or times out.
#[derive(Debug, Clone)]
pub struct BranchOutcome {
    pub branch_id: Uuid,
    pub succeeded: bool,
    pub output: Option<ToolOutput>,
    pub error: Option<String>,
    pub elapsed: Duration,
    pub timed_out: bool,
}

/// The result of [`fork_and_race`].
#[derive(Debug, Clone)]
pub struct SpeculativeResult {
    /// The winning branch's outcome.
    pub winner: BranchOutcome,
    /// How many branches were actually raced (after `max_branches` capping).
    pub branches_tried: usize,
    /// How many branches succeeded (≥ 1 when `Ok`).
    pub branches_succeeded: usize,
}

/// Fork the given branches and race them, returning the first success.
///
/// `exec` maps a branch to a future producing its [`ToolOutput`]. The racer is
/// agnostic to *how* a branch runs: tests inject in-memory futures; the
/// hypervisor injects real WASM execution. Each branch is wrapped in a
/// per-branch timeout from `config.branch_timeout`.
///
/// Returns `Err` when there are no branches, or when every branch fails or
/// times out without a single success.
pub async fn fork_and_race<F, Fut>(
    branches: Vec<SpeculativeBranch>,
    config: &SpeculativeConfig,
    exec: F,
) -> Result<SpeculativeResult>
where
    F: Fn(SpeculativeBranch) -> Fut,
    Fut: std::future::Future<Output = Result<ToolOutput>>,
{
    if branches.is_empty() {
        return Err(NexusError::ConfigError(
            "speculative execution requires at least one branch".into(),
        ));
    }

    // Cap the fan-out. `max(1)` guards against a misconfigured `max_branches`
    // of 0 silently dropping every branch.
    let cap = config.max_branches.max(1);
    let branches: Vec<SpeculativeBranch> = branches.into_iter().take(cap).collect();
    let tried = branches.len();
    let timeout = config.branch_timeout;

    // Launch every branch concurrently. Each future carries its own timeout
    // and reports (branch_id, elapsed, result). Dropping `running` early —
    // which happens on a `FirstSuccess` return — cancels the losers.
    let mut running = FuturesUnordered::new();
    for branch in branches {
        let branch_id = branch.id;
        let fut = exec(branch);
        running.push(async move {
            let start = Instant::now();
            let res = tokio::time::timeout(timeout, fut).await;
            (branch_id, start.elapsed(), res)
        });
    }

    let mut branches_succeeded = 0usize;
    let mut completed: Vec<BranchOutcome> = Vec::new();

    while let Some((branch_id, elapsed, res)) = running.next().await {
        let outcome = match res {
            // Branch finished within the timeout.
            Ok(Ok(output)) => {
                let succeeded = output.success;
                let error = if succeeded {
                    None
                } else {
                    output.error.clone()
                };
                BranchOutcome {
                    branch_id,
                    succeeded,
                    output: Some(output),
                    error,
                    elapsed,
                    timed_out: false,
                }
            }
            // Branch finished within the timeout but returned an error.
            Ok(Err(e)) => BranchOutcome {
                branch_id,
                succeeded: false,
                output: None,
                error: Some(e.to_string()),
                elapsed,
                timed_out: false,
            },
            // Branch exceeded its timeout; it is cancelled and can never win.
            Err(_) => BranchOutcome {
                branch_id,
                succeeded: false,
                output: None,
                error: Some(format!("branch timed out after {timeout:?}")),
                elapsed,
                timed_out: true,
            },
        };

        if outcome.succeeded {
            branches_succeeded += 1;
        }

        match config.selection_strategy {
            SelectionStrategy::FirstSuccess => {
                if outcome.succeeded {
                    // First success wins; returning here drops `running`,
                    // cancelling the remaining in-flight branches.
                    return Ok(SpeculativeResult {
                        winner: outcome,
                        branches_tried: tried,
                        branches_succeeded,
                    });
                }
                completed.push(outcome);
            }
            SelectionStrategy::WaitAll => completed.push(outcome),
        }
    }

    // No early winner. Under `WaitAll` (or a `FirstSuccess` round where the
    // success arrived... already returned above), pick the first success in
    // completion order. If none succeeded, the whole round failed.
    if let Some(winner) = completed.iter().find(|o| o.succeeded).cloned() {
        return Ok(SpeculativeResult {
            winner,
            branches_tried: tried,
            branches_succeeded,
        });
    }

    let reasons: Vec<String> = completed
        .iter()
        .map(|o| {
            let detail = o.error.as_deref().unwrap_or("unknown");
            if o.timed_out {
                format!("{} (timed out)", o.branch_id)
            } else {
                format!("{}: {}", o.branch_id, detail)
            }
        })
        .collect();
    Err(NexusError::WasmError(format!(
        "all {tried} speculative branches failed or timed out without a success [{}]",
        reasons.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hypervisor::recovery::RecoverySource;
    use std::sync::{Arc, Mutex};

    fn ok_output() -> ToolOutput {
        ToolOutput {
            success: true,
            result: Some(b"ok".to_vec()),
            error: None,
            rollback_performed: false,
            execution_time_ms: 0,
            fuel_consumed: 0,
            error_log: None,
            snapshot_id: None,
        }
    }

    fn fail_output() -> ToolOutput {
        ToolOutput {
            success: false,
            result: None,
            error: Some("branch failed".into()),
            rollback_performed: true,
            execution_time_ms: 0,
            fuel_consumed: 0,
            error_log: None,
            snapshot_id: None,
        }
    }

    fn branch(base: Uuid) -> SpeculativeBranch {
        SpeculativeBranch::new(
            base,
            ToolDefinition::new("speculative_tool".into(), vec![]),
            RecoveryAction::new("retry", RecoverySource::Static),
        )
    }

    fn cfg(strategy: SelectionStrategy, timeout: Duration) -> SpeculativeConfig {
        SpeculativeConfig {
            max_branches: 8,
            branch_timeout: timeout,
            selection_strategy: strategy,
        }
    }

    /// Three branches: a fast success, an immediate failure, and a slow
    /// success. FirstSuccess must return the fast success and not wait for
    /// the slow one.
    #[tokio::test]
    async fn first_success_wins_and_is_fast() {
        let base = Uuid::new_v4();
        let fast = branch(base);
        let failing = branch(base);
        let slow = branch(base);
        let fast_id = fast.id;
        let slow_id = slow.id;
        let failing_id = failing.id;

        let result = fork_and_race(
            vec![fast, failing, slow],
            &cfg(SelectionStrategy::FirstSuccess, Duration::from_secs(5)),
            move |b| {
                let is_fast = b.id == fast_id;
                let is_failing = b.id == failing_id;
                async move {
                    if is_failing {
                        Ok(fail_output())
                    } else if is_fast {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        Ok(ok_output())
                    } else {
                        // slow success
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        Ok(ok_output())
                    }
                }
            },
        )
        .await
        .expect("a branch should succeed");

        assert!(result.winner.succeeded);
        assert_eq!(result.winner.branch_id, fast_id, "fast branch should win");
        assert_ne!(result.winner.branch_id, slow_id);
        assert_eq!(result.branches_tried, 3);
        assert!(result.winner.elapsed < Duration::from_millis(400));
    }

    /// If every branch fails, the racer returns an error.
    #[tokio::test]
    async fn all_failures_return_error() {
        let base = Uuid::new_v4();
        let branches = vec![branch(base), branch(base), branch(base)];
        let result = fork_and_race(
            branches,
            &cfg(SelectionStrategy::FirstSuccess, Duration::from_secs(5)),
            |_b| async move { Ok(fail_output()) },
        )
        .await;
        assert!(result.is_err(), "all-failure round must be an error");
    }

    /// A branch that exceeds its timeout never wins; a faster sibling does.
    #[tokio::test]
    async fn timeout_branch_never_wins() {
        let base = Uuid::new_v4();
        let quick = branch(base);
        let stuck = branch(base);
        let quick_id = quick.id;

        let result = fork_and_race(
            vec![quick, stuck],
            &cfg(SelectionStrategy::FirstSuccess, Duration::from_millis(50)),
            move |b| {
                let is_quick = b.id == quick_id;
                async move {
                    if is_quick {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        Ok(ok_output())
                    } else {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        Ok(ok_output())
                    }
                }
            },
        )
        .await
        .expect("quick branch should win");

        assert_eq!(result.winner.branch_id, quick_id);
        assert!(!result.winner.timed_out);
    }

    /// When the only branch times out, the round errors.
    #[tokio::test]
    async fn sole_timeout_is_error() {
        let base = Uuid::new_v4();
        let result = fork_and_race(
            vec![branch(base)],
            &cfg(SelectionStrategy::FirstSuccess, Duration::from_millis(20)),
            |_b| async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(ok_output())
            },
        )
        .await;
        assert!(result.is_err());
    }

    /// WaitAll tallies every success and still surfaces a winner.
    #[tokio::test]
    async fn wait_all_counts_successes() {
        let base = Uuid::new_v4();
        let a = branch(base);
        let b = branch(base);
        let c = branch(base);
        let fail_id = b.id;

        let result = fork_and_race(
            vec![a, b, c],
            &cfg(SelectionStrategy::WaitAll, Duration::from_secs(5)),
            move |br| {
                let is_fail = br.id == fail_id;
                async move {
                    if is_fail {
                        Ok(fail_output())
                    } else {
                        Ok(ok_output())
                    }
                }
            },
        )
        .await
        .expect("two branches succeed");

        assert_eq!(result.branches_tried, 3);
        assert_eq!(result.branches_succeeded, 2);
        assert!(result.winner.succeeded);
    }

    /// `max_branches` caps how many branches actually run.
    #[tokio::test]
    async fn respects_max_branches() {
        let base = Uuid::new_v4();
        let branches: Vec<_> = (0..5).map(|_| branch(base)).collect();
        let mut config = cfg(SelectionStrategy::WaitAll, Duration::from_secs(5));
        config.max_branches = 2;

        let result = fork_and_race(branches, &config, |_b| async move { Ok(ok_output()) })
            .await
            .expect("succeeds");
        assert_eq!(result.branches_tried, 2);
    }

    /// An empty branch set is an error, not a panic.
    #[tokio::test]
    async fn empty_branches_is_error() {
        let result = fork_and_race(Vec::new(), &SpeculativeConfig::default(), |_b| async move {
            Ok(ok_output())
        })
        .await;
        assert!(result.is_err());
    }

    /// Every branch forks from the *same* base snapshot id — the property
    /// that makes forking O(dirty pages) rather than O(total memory).
    #[tokio::test]
    async fn all_branches_share_one_base_snapshot() {
        let base = Uuid::new_v4();
        let seen: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_c = Arc::clone(&seen);

        let _ = fork_and_race(
            vec![branch(base), branch(base), branch(base)],
            &cfg(SelectionStrategy::WaitAll, Duration::from_secs(5)),
            move |b| {
                seen_c.lock().unwrap().push(b.base_snapshot_id);
                async move { Ok(ok_output()) }
            },
        )
        .await;

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|id| *id == base));
    }
}
