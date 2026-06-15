//! `nexus-agentd` — long-lived daemon for the Phase C hot-path.
//!
//! Listens on a Unix socket (POSIX) or named pipe (Windows),
//! accepts framed JSON `DaemonRequest`s, and executes them on a pooled
//! `NexusHypervisor` so per-invocation cost is dominated by the
//! `execute_tool` body rather than hypervisor construction. Runs the
//! event loop on a multi-threaded tokio runtime.
//!
//! Usage:
//!   nexus-agentd                        # default socket, pool size = nproc
//!   nexus-agentd --pool 8                # custom pool size
//!   nexus-agentd --socket /tmp/foo.sock  # custom socket path (or \\.\pipe\name on Windows)

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio::io::{BufReader, BufWriter};
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

    // Restrict the socket to the owning user (0600). Without this, on a shared
    // host any local user who can reach the socket path could submit Execute or
    // Shutdown requests (the protocol has no per-request auth). Fail closed if
    // we cannot secure it rather than serving on a world-accessible socket.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    }

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
                    let (rd, wr) = stream.into_split();
                    if let Err(e) = handle_connection(rd, wr, pool, mc, stx).await {
                        error!("conn: {e}");
                    }
                });
            }
        }
    }
}

#[cfg(windows)]
async fn run(
    socket: PathBuf,
    pool: Arc<HypervisorPool>,
    module_cache: Arc<ModuleCache>,
) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_name = socket
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("pipe path is not valid UTF-8"))?;

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    // Create the first pipe instance before logging "ready" so clients can
    // connect immediately after the message appears.
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(pipe_name)?;

    info!(target: "nexus.agentd", "ready");

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!(target: "nexus.agentd", "shutdown requested");
                return Ok(());
            }
            result = server.connect() => {
                if let Err(e) = result {
                    error!("pipe connect: {e}");
                    continue;
                }

                // Hand the connected pipe to a task and create a fresh
                // instance for the next client.
                let connected = server;
                server = ServerOptions::new().create(pipe_name)?;

                let pool = pool.clone();
                let mc = module_cache.clone();
                let stx = shutdown_tx.clone();
                tokio::spawn(async move {
                    let (rd, wr) = tokio::io::split(connected);
                    if let Err(e) = handle_connection(rd, wr, pool, mc, stx).await {
                        error!("conn: {e}");
                    }
                });
            }
        }
    }
}

async fn handle_connection<R, W>(
    rd: R,
    wr: W,
    pool: Arc<HypervisorPool>,
    module_cache: Arc<ModuleCache>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
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
    module_cache: &Arc<ModuleCache>,
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
        DaemonRequest::Execute {
            name,
            wasm_bytes,
            wasm_path,
            entry,
            input,
        } => {
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
            let guard = match pool.acquire().await {
                Ok(g) => g,
                Err(e) => {
                    return DaemonResponse::Error {
                        message: format!("pool acquire failed: {e}"),
                    }
                }
            };
            let engine = guard.hv().sandbox_engine();
            let module = match module_cache.get_or_compile(&engine, &bytes) {
                Ok(m) => m,
                Err(e) => {
                    return DaemonResponse::Error {
                        message: format!("module compile failed: {e}"),
                    }
                }
            };
            let tool = ToolDefinition::new(name, bytes).with_entry(&entry);
            match guard
                .hv()
                .execute_tool_precompiled(tool, input, module)
                .await
            {
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

