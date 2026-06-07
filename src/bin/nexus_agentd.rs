//! `nexus-agentd` — long-lived daemon for the Phase C hot-path.
//!
//! Listens on a Unix socket (POSIX) or named pipe (Windows; see TODO),
//! accepts framed JSON `DaemonRequest`s, and executes them on a pooled
//! `NexusHypervisor` so per-invocation cost is dominated by the
//! `execute_tool` body rather than hypervisor construction. Runs the
//! event loop on a multi-threaded tokio runtime.
//!
//! Usage:
//!   nexus-agentd                        # default socket, pool size = nproc
//!   nexus-agentd --pool 8                # custom pool size
//!   nexus-agentd --socket /tmp/foo.sock  # custom socket path

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use nexus::daemon::module_cache::ModuleCache;
use nexus::daemon::pool::HypervisorPool;
use nexus::daemon::protocol::{read_frame, write_frame};
use nexus::daemon::{default_socket_path, DaemonRequest, DaemonResponse};
use nexus::{HypervisorConfig, ToolDefinition};

#[derive(Parser, Debug)]
#[command(name = "nexus-agentd", version, about = "Nexus daemon (Phase C)")]
struct Cli {
    /// Listening socket / pipe path. Defaults to NEXUS_AGENTD_SOCKET or
    /// the platform default.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Hypervisor pool size. Defaults to `num_cpus`.
    #[arg(long)]
    pool: Option<usize>,
    /// Per-call fuel budget (WASM instructions). Defaults to 1 billion
    /// so non-trivial benchmarks like fib(30)*10 can complete. Lower it
    /// for production tool-execution use cases.
    #[arg(long, default_value_t = 1_000_000_000)]
    fuel: u64,
    /// Per-call wall-clock timeout in milliseconds. Defaults to 5000.
    #[arg(long, default_value_t = 5000)]
    timeout_ms: u64,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nexus_agentd=info,nexus=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let pool_size = cli.pool.unwrap_or_else(num_logical_cpus);
    let socket_path = cli.socket.unwrap_or_else(default_socket_path);

    info!(target: "nexus.agentd", "pool size  = {pool_size}");
    info!(target: "nexus.agentd", "socket     = {}", socket_path.display());
    info!(target: "nexus.agentd", "fuel       = {}", cli.fuel);
    info!(target: "nexus.agentd", "timeout_ms = {}", cli.timeout_ms);

    let mut cfg = HypervisorConfig::default();
    cfg.sandbox_config.max_fuel = cli.fuel;
    cfg.sandbox_config.time_limit = std::time::Duration::from_millis(cli.timeout_ms);
    let pool = HypervisorPool::new(pool_size, cfg)?;
    let module_cache = Arc::new(ModuleCache::new());

    run(socket_path, pool, module_cache).await
}

#[cfg(unix)]
async fn run(
    socket: PathBuf,
    pool: Arc<HypervisorPool>,
    module_cache: Arc<ModuleCache>,
) -> anyhow::Result<()> {
    use tokio::net::UnixListener;

    // Clean up a stale socket if a previous daemon crashed without removing it.
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }
    let listener = UnixListener::bind(&socket)?;
    info!(target: "nexus.agentd", "ready");

    // Watchdog channel so a Shutdown request can stop the accept loop.
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!(target: "nexus.agentd", "shutdown requested");
                let _ = std::fs::remove_file(&socket);
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, _addr) = match accepted {
                    Ok(s) => s,
                    Err(e) => { error!("accept: {e}"); continue; }
                };
                let pool = pool.clone();
                let mc = module_cache.clone();
                let stx = shutdown_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_unix(stream, pool, mc, stx).await {
                        error!("conn: {e}");
                    }
                });
            }
        }
    }
}

#[cfg(windows)]
async fn run(
    _socket: PathBuf,
    _pool: Arc<HypervisorPool>,
    _module_cache: Arc<ModuleCache>,
) -> anyhow::Result<()> {
    // Windows named-pipe support is intentionally deferred to a follow-up.
    // tokio::net::windows::named_pipe is gated behind a separate feature;
    // for the Phase C benchmarks we run the daemon on the Linux side of
    // WSL2 where the Unix-socket path works as-is.
    anyhow::bail!(
        "nexus-agentd: Windows named-pipe transport not yet implemented; run on Linux/WSL2"
    )
}

#[cfg(unix)]
async fn handle_unix(
    stream: tokio::net::UnixStream,
    pool: Arc<HypervisorPool>,
    module_cache: Arc<ModuleCache>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()> {
    let (rd, wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    let mut wr = BufWriter::new(wr);

    loop {
        let req: DaemonRequest = match read_frame(&mut rd).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let resp = serve(req, &pool, &module_cache, &shutdown_tx).await;
        write_frame(&mut wr, &resp).await?;
    }
}

async fn serve(
    req: DaemonRequest,
    pool: &Arc<HypervisorPool>,
    _module_cache: &Arc<ModuleCache>,
    shutdown_tx: &tokio::sync::watch::Sender<bool>,
) -> DaemonResponse {
    match req {
        DaemonRequest::Ping => DaemonResponse::Pong {
            version: env!("CARGO_PKG_VERSION").into(),
        },
        DaemonRequest::Shutdown => {
            let _ = shutdown_tx.send(true);
            DaemonResponse::Pong {
                version: env!("CARGO_PKG_VERSION").into(),
            }
        }
        DaemonRequest::Execute { name, wasm_bytes, wasm_path, entry, input } => {
            let bytes = match (wasm_bytes, wasm_path) {
                (Some(b), _) => b,
                (None, Some(p)) => match std::fs::read(&p) {
                    Ok(b) => b,
                    Err(e) => {
                        return DaemonResponse::Error {
                            message: format!("read {}: {e}", p.display()),
                        }
                    }
                },
                (None, None) => {
                    return DaemonResponse::Error {
                        message: "request missing wasm_bytes and wasm_path".into(),
                    }
                }
            };
            // Note: the ModuleCache speeds up sandbox-internal compile in
            // a follow-up; today's hypervisor still calls Module::from_binary
            // inside execute_tool. The cache will land when the sandbox
            // exposes an "execute with precompiled module" entry point.
            let guard = match pool.acquire().await {
                Ok(g) => g,
                Err(e) => {
                    return DaemonResponse::Error {
                        message: format!("pool acquire failed: {e}"),
                    }
                }
            };
            let tool = ToolDefinition::new(name, bytes).with_entry(&entry);
            match guard.hv().execute_tool(tool, input).await {
                Ok(output) => DaemonResponse::Executed {
                    output: Box::new(output),
                },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }
    }
}

fn num_logical_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

// Suppress unused-import warnings on Windows where the Unix code paths
// above are cfg-gated out.
#[cfg(windows)]
#[allow(dead_code, unused_imports)]
mod _unused {
    use super::*;
    pub fn _force_link(_: BufReader<tokio::io::Stdin>, _: BufWriter<tokio::io::Stdout>) {}
}

// Pull AsyncReadExt/AsyncWriteExt into scope on Windows too so the
// imports are not flagged as unused.
#[cfg(windows)]
fn _silence_unused_async_imports() {
    fn _t<R: AsyncReadExt + AsyncWriteExt>(_: &mut R) {}
}
