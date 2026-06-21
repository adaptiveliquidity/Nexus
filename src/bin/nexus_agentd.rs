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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use tokio::io::{BufReader, BufWriter};
#[cfg(feature = "aeon-memory")]
use tracing::debug;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use nexus::daemon::module_cache::ModuleCache;
use nexus::daemon::pool::HypervisorPool;
use nexus::daemon::protocol::{read_frame, write_frame};
use nexus::daemon::{default_socket_path, DaemonRequest, DaemonResponse};
use nexus::profile::load_and_validate as load_profile;
use nexus::{HypervisorConfig, ToolDefinition};

const AUTH_TOKEN_ENV: &str = "NEXUS_AGENTD_AUTH_TOKEN";
const MODULE_DIR_ENV: &str = "NEXUS_AGENTD_MODULE_DIR";
/// Profile env var for the daemon (Slice 3 — daemon_auth_required enforcement).
const AGENTD_PROFILE_ENV: &str = "NEXUS_AGENTD_PROFILE";
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized: daemon request authentication failed";
const WASM_PATH_REJECTED_MESSAGE: &str =
    "wasm_path rejected: configure an allowed module directory";

type AuthToken = Option<Arc<str>>;

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
    #[cfg(feature = "aeon-memory")]
    match nexus::aeon::AeonConfig::from_env() {
        Ok(config) => {
            cfg.aeon_config = Some(config);
        }
        Err(error) => {
            debug!(
                target: "nexus.agentd",
                error = %error,
                "AEON-IQ config failed to load; daemon proof memory/timeline integration disabled"
            );
        }
    }
    let pool = HypervisorPool::new(pool_size, cfg)?;
    let module_cache = Arc::new(ModuleCache::new());
    let auth_token = configured_auth_token()?;

    // Slice 3: if a profile is configured and it requires daemon authentication,
    // refuse to start without an auth token — fail loudly rather than silently
    // running unauthenticated against a posture that requires it.
    enforce_profile_auth_requirement(&auth_token)?;

    run(socket_path, pool, module_cache, auth_token).await
}

fn configured_auth_token() -> anyhow::Result<AuthToken> {
    match std::env::var(AUTH_TOKEN_ENV) {
        Ok(token) => Ok(Some(Arc::from(token))),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{AUTH_TOKEN_ENV} must be valid Unicode")
        }
    }
}

fn enforce_profile_auth_requirement(auth_token: &AuthToken) -> anyhow::Result<()> {
    let Some(profile_path) = std::env::var_os(AGENTD_PROFILE_ENV) else {
        return Ok(());
    };
    let manifest = load_profile(&profile_path).map_err(|errors| {
        let joined = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::anyhow!(
            "invalid {AGENTD_PROFILE_ENV} '{}': {joined}",
            std::path::Path::new(&profile_path).display()
        )
    })?;

    if manifest.execution_policy().daemon_auth_required && auth_token.is_none() {
        anyhow::bail!(
            "profile '{}' requires daemon authentication ({AUTH_TOKEN_ENV} must be set)",
            manifest.name
        );
    }

    info!(
        target: "nexus.agentd",
        profile = %manifest.name,
        daemon_auth_required = %manifest.execution_policy().daemon_auth_required,
        "Loaded agentd capability profile"
    );
    Ok(())
}

#[cfg(unix)]
async fn run(
    socket: PathBuf,
    pool: Arc<HypervisorPool>,
    module_cache: Arc<ModuleCache>,
    auth_token: AuthToken,
) -> anyhow::Result<()> {
    use tokio::net::UnixListener;

    // Clean up a stale socket if a previous daemon crashed without removing it.
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }
    let listener = UnixListener::bind(&socket)?;

    // Restrict the socket to the owning user (0600). Without this, on a shared
    // host any local user who can reach the socket path could submit Ping
    // requests. Execute and Shutdown additionally require the optional
    // per-request auth token when configured. Fail closed if we cannot secure
    // the socket rather than serving on a world-accessible path.
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
                let auth_token = auth_token.clone();
                tokio::spawn(async move {
                    let (rd, wr) = stream.into_split();
                    if let Err(e) = handle_connection(rd, wr, pool, mc, stx, auth_token).await {
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
    auth_token: AuthToken,
) -> anyhow::Result<()> {
    let pipe_name = socket
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("pipe path is not valid UTF-8"))?;

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    // Create the first pipe instance before logging "ready" so clients can
    // connect immediately after the message appears.
    let mut server = create_restricted_named_pipe_server(pipe_name, true)?;

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
                server = create_restricted_named_pipe_server(pipe_name, false)?;

                let pool = pool.clone();
                let mc = module_cache.clone();
                let stx = shutdown_tx.clone();
                let auth_token = auth_token.clone();
                tokio::spawn(async move {
                    let (rd, wr) = tokio::io::split(connected);
                    if let Err(e) = handle_connection(rd, wr, pool, mc, stx, auth_token).await {
                        error!("conn: {e}");
                    }
                });
            }
        }
    }
}

#[cfg(windows)]
fn create_restricted_named_pipe_server(
    pipe_name: &str,
    first_pipe_instance: bool,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut options = ServerOptions::new();
    if first_pipe_instance {
        options.first_pipe_instance(true);
    }

    let mut security = RestrictedPipeSecurity::new()?;
    // The descriptor is consumed by CreateNamedPipeW during this call; the
    // wrapper keeps SECURITY_ATTRIBUTES valid for the syscall.
    unsafe { options.create_with_security_attributes_raw(pipe_name, security.as_mut_ptr()) }
}

#[cfg(windows)]
struct RestrictedPipeSecurity {
    descriptor: *mut std::ffi::c_void,
    attrs: SecurityAttributes,
}

#[cfg(windows)]
impl RestrictedPipeSecurity {
    fn new() -> std::io::Result<Self> {
        let descriptor = security_descriptor_from_sddl(RESTRICTED_PIPE_SDDL)?;
        Ok(Self {
            descriptor,
            attrs: SecurityAttributes {
                n_length: std::mem::size_of::<SecurityAttributes>() as u32,
                lp_security_descriptor: descriptor,
                b_inherit_handle: 0,
            },
        })
    }

    fn as_mut_ptr(&mut self) -> *mut std::ffi::c_void {
        (&mut self.attrs as *mut SecurityAttributes).cast()
    }
}

#[cfg(windows)]
impl Drop for RestrictedPipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe {
                let _ = LocalFree(self.descriptor);
            }
        }
    }
}

#[cfg(windows)]
#[repr(C)]
struct SecurityAttributes {
    n_length: u32,
    lp_security_descriptor: *mut std::ffi::c_void,
    b_inherit_handle: i32,
}

#[cfg(windows)]
// Protected DACL: explicitly deny network logons and grant the object owner.
// Other SIDs, including Everyone, receive no allow ACE.
const RESTRICTED_PIPE_SDDL: &str = "D:P(D;;GA;;;NU)(A;;GA;;;OW)";

#[cfg(windows)]
fn security_descriptor_from_sddl(sddl: &str) -> std::io::Result<*mut std::ffi::c_void> {
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = std::ffi::OsStr::new(sddl)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(descriptor)
}

#[cfg(windows)]
const SDDL_REVISION_1: u32 = 1;

#[cfg(windows)]
#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_security_descriptor: *const u16,
        string_sd_revision: u32,
        security_descriptor: *mut *mut std::ffi::c_void,
        security_descriptor_size: *mut u32,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

#[cfg(all(test, windows))]
mod windows_pipe_acl_tests {
    use super::*;

    #[test]
    fn restricted_pipe_security_descriptor_is_buildable() {
        let security = RestrictedPipeSecurity::new().unwrap();
        assert!(!security.descriptor.is_null());
        assert_eq!(security.attrs.b_inherit_handle, 0);
    }

    #[test]
    fn restricted_pipe_sddl_denies_network_and_allows_owner_only() {
        assert!(RESTRICTED_PIPE_SDDL.contains("(D;;GA;;;NU)"));
        assert!(RESTRICTED_PIPE_SDDL.contains("(A;;GA;;;OW)"));
        assert!(!RESTRICTED_PIPE_SDDL.contains(";;;WD)"));
    }
}

async fn handle_connection<R, W>(
    rd: R,
    wr: W,
    pool: Arc<HypervisorPool>,
    module_cache: Arc<ModuleCache>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    auth_token: AuthToken,
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
        let resp = serve(
            req,
            &pool,
            &module_cache,
            &shutdown_tx,
            auth_token.as_deref(),
        )
        .await;
        write_frame(&mut wr, &resp).await?;
    }
}

async fn serve(
    req: DaemonRequest,
    pool: &Arc<HypervisorPool>,
    module_cache: &Arc<ModuleCache>,
    shutdown_tx: &tokio::sync::watch::Sender<bool>,
    auth_token: Option<&str>,
) -> DaemonResponse {
    if let Some(resp) = unauthorized_response(&req, auth_token) {
        return resp;
    }

    match req {
        DaemonRequest::Ping => DaemonResponse::Pong {
            version: env!("CARGO_PKG_VERSION").into(),
        },
        DaemonRequest::Shutdown { .. } => {
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
            #[cfg(feature = "aeon-memory")]
            aeon,
            ..
        } => {
            let bytes = match (wasm_bytes, wasm_path) {
                (Some(_), _) if auth_token.is_none() => {
                    // Raw bytes bypass the path allowlist. Require configured auth
                    // so that unauthenticated callers cannot submit arbitrary WASM.
                    return DaemonResponse::Error {
                        message:
                            "wasm_bytes submission requires daemon authentication to be configured"
                                .into(),
                        #[cfg(feature = "aeon-memory")]
                        events: vec![],
                        #[cfg(feature = "aeon-memory")]
                        nexusiq: None,
                    };
                }
                (Some(b), _) => b,
                (None, Some(p)) => match read_allowlisted_wasm_path(&p) {
                    Ok(b) => b,
                    Err(e) => {
                        return DaemonResponse::Error {
                            message: e.to_string(),
                            #[cfg(feature = "aeon-memory")]
                            events: vec![],
                            #[cfg(feature = "aeon-memory")]
                            nexusiq: None,
                        }
                    }
                },
                (None, None) => {
                    return DaemonResponse::Error {
                        message: "request missing wasm_bytes and wasm_path".into(),
                        #[cfg(feature = "aeon-memory")]
                        events: vec![],
                        #[cfg(feature = "aeon-memory")]
                        nexusiq: None,
                    }
                }
            };
            let guard = match pool.acquire().await {
                Ok(g) => g,
                Err(e) => {
                    return DaemonResponse::Error {
                        message: format!("pool acquire failed: {e}"),
                        #[cfg(feature = "aeon-memory")]
                        events: vec![],
                        #[cfg(feature = "aeon-memory")]
                        nexusiq: None,
                    }
                }
            };
            let engine = guard.hv().sandbox_engine();
            let module = match module_cache.get_or_compile(&engine, &bytes) {
                Ok(m) => m,
                Err(e) => {
                    return DaemonResponse::Error {
                        message: format!("module compile failed: {e}"),
                        #[cfg(feature = "aeon-memory")]
                        events: vec![],
                        #[cfg(feature = "aeon-memory")]
                        nexusiq: None,
                    }
                }
            };
            #[cfg_attr(not(feature = "aeon-memory"), allow(unused_mut))]
            let mut tool = ToolDefinition::new(name, bytes).with_entry(&entry);
            #[cfg(feature = "aeon-memory")]
            {
                tool = tool
                    .with_aeon_context(aeon.aeon_agent_id.clone(), aeon.aeon_session_id.clone())
                    .with_aeon_memory_evidence_digest(aeon.aeon_memory_evidence_digest.clone());
                if let Some(capabilities) = aeon.required_capabilities.clone() {
                    tool = tool.with_capabilities(capabilities);
                }
            }
            #[cfg(feature = "aeon-memory")]
            if aeon.emit_proof {
                let proof_result = match aeon.caller_capabilities.as_deref() {
                    Some(tokens) => {
                        guard
                            .hv()
                            .execute_tool_proof_with_tokens(tool, input, tokens)
                            .await
                    }
                    None => guard.hv().execute_tool_proof(tool, input).await,
                };

                return match proof_result {
                    Ok((output, proof_capsule)) => {
                        let negotiation_rounds = proof_capsule.capabilities.negotiation_rounds;
                        let events = daemon_proof_events(
                            &output,
                            proof_capsule.capsule_id,
                            negotiation_rounds,
                        );
                        let timeline_delivery_status = queue_timeline_delivery(
                            aeon.aeon_agent_id.as_deref(),
                            aeon.aeon_session_id.as_deref(),
                            aeon.attestation_mode.as_deref(),
                            &events,
                        );
                        let nexusiq = nexus::daemon::DaemonNexusIqEvidence {
                            proof_capsule_ref: Some(proof_capsule.capsule_id.to_string()),
                            memory_evidence_ref: proof_capsule.memory_evidence.clone(),
                            timeline_delivery_status,
                            denial_negotiation: negotiation_rounds.map(|rounds| {
                                nexus::daemon::DaemonDenialNegotiation {
                                    negotiated: true,
                                    rounds: Some(rounds),
                                }
                            }),
                            proof_capsule: Some(Box::new(proof_capsule)),
                        };
                        DaemonResponse::Executed {
                            output: Box::new(output),
                            events,
                            nexusiq: Some(nexusiq),
                        }
                    }
                    Err(ref e) => {
                        let events = capability_denied_events(e);
                        DaemonResponse::Error {
                            message: e.to_string(),
                            events,
                            nexusiq: None,
                        }
                    }
                };
            }
            match guard
                .hv()
                .execute_tool_precompiled(tool, input, module)
                .await
            {
                Ok(output) => {
                    #[cfg(feature = "aeon-memory")]
                    let events = {
                        let mut evts = Vec::new();
                        if let Some(id) = output.snapshot_id {
                            evts.push(nexus::daemon::NexusExecutionEvent::SnapshotCreated {
                                snapshot_id: id,
                            });
                        }
                        evts
                    };
                    DaemonResponse::Executed {
                        output: Box::new(output),
                        #[cfg(feature = "aeon-memory")]
                        events,
                        #[cfg(feature = "aeon-memory")]
                        nexusiq: None,
                    }
                }
                Err(ref e) => {
                    #[cfg(feature = "aeon-memory")]
                    let events = capability_denied_events(e);
                    DaemonResponse::Error {
                        message: e.to_string(),
                        #[cfg(feature = "aeon-memory")]
                        events,
                        #[cfg(feature = "aeon-memory")]
                        nexusiq: None,
                    }
                }
            }
        }
    }
}

fn unauthorized_response(req: &DaemonRequest, auth_token: Option<&str>) -> Option<DaemonResponse> {
    let configured = auth_token?;
    let supplied = match req {
        DaemonRequest::Ping => return None,
        DaemonRequest::Execute { auth_token, .. } | DaemonRequest::Shutdown { auth_token } => {
            auth_token.as_deref()
        }
    };

    if supplied
        .map(|token| constant_time_eq(token.as_bytes(), configured.as_bytes()))
        .unwrap_or(false)
    {
        None
    } else {
        Some(DaemonResponse::Error {
            message: UNAUTHORIZED_MESSAGE.into(),
            #[cfg(feature = "aeon-memory")]
            events: vec![],
            #[cfg(feature = "aeon-memory")]
            nexusiq: None,
        })
    }
}

#[cfg(feature = "aeon-memory")]
fn capability_denied_events(error: &nexus::NexusError) -> Vec<nexus::daemon::NexusExecutionEvent> {
    match error {
        nexus::NexusError::CapabilityDenied(message) => {
            vec![nexus::daemon::NexusExecutionEvent::CapabilityDenied {
                denied_category: message.clone(),
            }]
        }
        _ => vec![],
    }
}

#[cfg(feature = "aeon-memory")]
fn daemon_proof_events(
    output: &nexus::ToolOutput,
    capsule_id: uuid::Uuid,
    negotiation_rounds: Option<u32>,
) -> Vec<nexus::daemon::NexusExecutionEvent> {
    let mut events = Vec::new();
    if negotiation_rounds.is_some() {
        events.push(nexus::daemon::NexusExecutionEvent::CapabilityDenied {
            denied_category: "capability_denial_negotiated".to_string(),
        });
    }
    if let Some(snapshot_id) = output.snapshot_id {
        events.push(nexus::daemon::NexusExecutionEvent::SnapshotCreated { snapshot_id });
    }
    events.push(nexus::daemon::NexusExecutionEvent::ProofCapsuleEmitted { capsule_id });
    events
}

#[cfg(feature = "aeon-memory")]
fn queue_timeline_delivery(
    agent_id: Option<&str>,
    session_id: Option<&str>,
    mode: Option<&str>,
    events: &[nexus::daemon::NexusExecutionEvent],
) -> Option<nexus::aeon::TimelineDeliveryStatus> {
    let agent_id = agent_id?.to_string();
    let session_id = session_id.map(str::to_string);
    let mode = nexus::aeon::TimelineDeliveryMode::parse(mode);
    let config = nexus::aeon::AeonConfig::from_env().unwrap_or_default();
    let sink = if matches!(mode, nexus::aeon::TimelineDeliveryMode::Offline) {
        Some(nexus::aeon::AeonTimelineSink::from_config(&config).with_mode(mode))
    } else {
        nexus::aeon::AeonTimelineSink::from_enabled_config(&config).map(|sink| sink.with_mode(mode))
    };

    let Some(sink) = sink else {
        return Some(nexus::aeon::TimelineDeliveryStatus::FailedOpen);
    };
    let events = events.to_vec();
    tokio::spawn(async move {
        let _ = sink
            .deliver(&agent_id, session_id.as_deref(), &events)
            .await;
    });

    Some(nexus::aeon::TimelineDeliveryStatus::FireAndForget)
}

fn read_allowlisted_wasm_path(wasm_path: &Path) -> anyhow::Result<Vec<u8>> {
    let (mut file, _) = open_agentd_wasm_path(wasm_path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!(WASM_PATH_REJECTED_MESSAGE))?;
    Ok(bytes)
}

fn allowed_agentd_module_dirs() -> anyhow::Result<Vec<PathBuf>> {
    let raw_dirs: Vec<PathBuf> = match std::env::var_os(MODULE_DIR_ENV) {
        Some(value) => std::env::split_paths(&value).collect(),
        None => return Err(anyhow::anyhow!(WASM_PATH_REJECTED_MESSAGE)),
    };

    if raw_dirs.is_empty() {
        return Err(anyhow::anyhow!(WASM_PATH_REJECTED_MESSAGE));
    }

    raw_dirs
        .into_iter()
        .map(|dir| {
            let canonical = std::fs::canonicalize(&dir)
                .map_err(|_| anyhow::anyhow!(WASM_PATH_REJECTED_MESSAGE))?;
            if !canonical.is_dir() {
                anyhow::bail!(WASM_PATH_REJECTED_MESSAGE);
            }
            Ok(canonical)
        })
        .collect()
}

fn open_agentd_wasm_path(wasm_path: &Path) -> anyhow::Result<(std::fs::File, PathBuf)> {
    let allowed_dirs = allowed_agentd_module_dirs()?;
    open_agentd_wasm_path_with_dirs(wasm_path, &allowed_dirs)
}

fn open_agentd_wasm_path_with_dirs(
    wasm_path: &Path,
    allowed_dirs: &[PathBuf],
) -> anyhow::Result<(std::fs::File, PathBuf)> {
    let file = open_untrusted_agentd_file(wasm_path)?;
    let metadata = file
        .metadata()
        .map_err(|_| anyhow::anyhow!(WASM_PATH_REJECTED_MESSAGE))?;

    if !metadata.is_file() {
        anyhow::bail!(WASM_PATH_REJECTED_MESSAGE);
    }

    let canonical = canonicalize_open_agentd_file(&file)?;

    if allowed_dirs.iter().any(|dir| canonical.starts_with(dir)) {
        return Ok((file, canonical));
    }

    anyhow::bail!(WASM_PATH_REJECTED_MESSAGE)
}

#[cfg(target_os = "linux")]
fn open_untrusted_agentd_file(wasm_path: &Path) -> anyhow::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    const LINUX_O_NONBLOCK: i32 = 0o0004000;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(LINUX_O_NONBLOCK)
        .open(wasm_path)
        .map_err(|_| anyhow::anyhow!(WASM_PATH_REJECTED_MESSAGE))
}

#[cfg(not(target_os = "linux"))]
fn open_untrusted_agentd_file(_wasm_path: &Path) -> anyhow::Result<std::fs::File> {
    // The daemon's wasm_path loading path is Linux-targeted. On other
    // platforms, reject instead of falling back to a path-based reopen.
    anyhow::bail!(WASM_PATH_REJECTED_MESSAGE)
}

#[cfg(target_os = "linux")]
fn canonicalize_open_agentd_file(file: &std::fs::File) -> anyhow::Result<PathBuf> {
    use std::os::unix::io::AsRawFd;

    let fd_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
    std::fs::canonicalize(fd_path).map_err(|_| anyhow::anyhow!(WASM_PATH_REJECTED_MESSAGE))
}

#[cfg(not(target_os = "linux"))]
fn canonicalize_open_agentd_file(_file: &std::fs::File) -> anyhow::Result<PathBuf> {
    // A path-based fallback would reintroduce the check-then-open race. Until a
    // platform-specific handle canonicalization path is available, reject
    // wasm_path inputs on unsupported platforms.
    anyhow::bail!(WASM_PATH_REJECTED_MESSAGE)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let a_byte = a.get(i).copied().unwrap_or(0);
        let b_byte = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(a_byte ^ b_byte);
    }
    diff == 0
}

fn num_logical_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    fn shutdown_request(auth_token: Option<String>) -> DaemonRequest {
        DaemonRequest::Shutdown { auth_token }
    }

    #[test]
    fn configured_auth_rejects_execute_without_token() {
        let req: DaemonRequest =
            serde_json::from_str(r#"{"type":"Execute","name":"tool","wasm_bytes":"","input":{}}"#)
                .unwrap();
        let resp = unauthorized_response(&req, Some("secret"));

        assert!(matches!(
            resp,
            Some(DaemonResponse::Error { message, .. }) if message == UNAUTHORIZED_MESSAGE
        ));
    }

    #[test]
    fn configured_auth_rejects_shutdown_with_wrong_token() {
        let resp = unauthorized_response(&shutdown_request(Some("wrong".into())), Some("secret"));

        assert!(matches!(
            resp,
            Some(DaemonResponse::Error { message, .. }) if message == UNAUTHORIZED_MESSAGE
        ));
    }

    #[test]
    fn configured_auth_accepts_shutdown_with_correct_token() {
        let resp = unauthorized_response(&shutdown_request(Some("secret".into())), Some("secret"));

        assert!(resp.is_none());
    }

    #[test]
    fn unconfigured_auth_accepts_tokenless_shutdown() {
        let req: DaemonRequest = serde_json::from_str(r#"{"type":"Shutdown"}"#).unwrap();
        let resp = unauthorized_response(&req, None);

        assert!(resp.is_none());
    }

    #[test]
    fn rejects_wasm_path_outside_allowed_module_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let wasm_path = outside.join("tool.wasm");
        std::fs::write(&wasm_path, b"\0asm").unwrap();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let err = open_agentd_wasm_path_with_dirs(&wasm_path, &allowed_dirs).unwrap_err();

        assert_eq!(err.to_string(), WASM_PATH_REJECTED_MESSAGE);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn accepts_wasm_path_inside_allowed_module_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let wasm_path = allowed.join("tool.wasm");
        std::fs::write(&wasm_path, b"\0asm").unwrap();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let (_file, resolved) = open_agentd_wasm_path_with_dirs(&wasm_path, &allowed_dirs).unwrap();

        assert_eq!(resolved, std::fs::canonicalize(wasm_path).unwrap());
    }

    #[test]
    fn rejects_wasm_path_directory_inside_allowed_module_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        let module_dir = allowed.join("tool_dir");
        std::fs::create_dir_all(&module_dir).unwrap();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let err = open_agentd_wasm_path_with_dirs(&module_dir, &allowed_dirs).unwrap_err();

        assert_eq!(err.to_string(), WASM_PATH_REJECTED_MESSAGE);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn validated_handle_reads_original_bytes_after_path_replacement() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let wasm_path = allowed.join("tool.wasm");
        let replacement_path = allowed.join("replacement.wasm");
        std::fs::write(&wasm_path, b"original").unwrap();
        std::fs::write(&replacement_path, b"replacement").unwrap();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let (mut file, resolved) =
            open_agentd_wasm_path_with_dirs(&wasm_path, &allowed_dirs).unwrap();

        assert_eq!(resolved, std::fs::canonicalize(&wasm_path).unwrap());

        // This deterministic rename covers the security property better than a
        // flaky race test: once validation returns an open descriptor, later
        // path replacement must not redirect the bytes read by the daemon.
        std::fs::rename(&replacement_path, &wasm_path).unwrap();

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes, b"original");

        let mut replacement_bytes = Vec::new();
        std::fs::File::open(&wasm_path)
            .unwrap()
            .read_to_end(&mut replacement_bytes)
            .unwrap();
        assert_eq!(replacement_bytes, b"replacement");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_wasm_path_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let outside_wasm = outside.join("tool.wasm");
        let linked_wasm = allowed.join("linked.wasm");
        std::fs::write(&outside_wasm, b"\0asm").unwrap();
        symlink(&outside_wasm, &linked_wasm).unwrap();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let err = open_agentd_wasm_path_with_dirs(&linked_wasm, &allowed_dirs).unwrap_err();

        assert_eq!(err.to_string(), WASM_PATH_REJECTED_MESSAGE);
    }

    #[test]
    fn rejects_wasm_path_parent_traversal_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let outside_wasm = outside.join("tool.wasm");
        std::fs::write(&outside_wasm, b"\0asm").unwrap();
        let traversal = allowed.join("..").join("outside").join("tool.wasm");

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let err = open_agentd_wasm_path_with_dirs(&traversal, &allowed_dirs).unwrap_err();

        assert_eq!(err.to_string(), WASM_PATH_REJECTED_MESSAGE);
    }
}

#[cfg(all(test, feature = "aeon-memory"))]
mod proof_tests {
    use super::*;
    use nexus::daemon::NexusExecutionEvent;

    fn trivial_wasm() -> Vec<u8> {
        wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).unwrap()
    }

    fn execute_request() -> DaemonRequest {
        DaemonRequest::Execute {
            name: "proof_daemon".to_string(),
            wasm_bytes: Some(trivial_wasm()),
            wasm_path: None,
            entry: "_start".to_string(),
            input: serde_json::json!({}),
            auth_token: Some("secret".to_string()),
            aeon: Box::new(nexus::daemon::DaemonAeonExecuteOptions {
                emit_proof: true,
                ..nexus::daemon::DaemonAeonExecuteOptions::default()
            }),
        }
    }

    async fn serve_request(req: DaemonRequest, pool: Arc<HypervisorPool>) -> DaemonResponse {
        let module_cache = Arc::new(ModuleCache::new());
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        serve(req, &pool, &module_cache, &shutdown_tx, Some("secret")).await
    }

    #[tokio::test]
    async fn proof_mode_returns_signed_capsule_and_events() {
        let pool = HypervisorPool::new(1, HypervisorConfig::default()).unwrap();

        let response = serve_request(execute_request(), pool).await;

        match response {
            DaemonResponse::Executed {
                output,
                events,
                nexusiq: Some(nexusiq),
            } => {
                assert!(output.success);
                assert!(events
                    .iter()
                    .any(|event| matches!(event, NexusExecutionEvent::ProofCapsuleEmitted { .. })));
                let capsule = nexusiq.proof_capsule.expect("proof capsule");
                assert!(capsule.signature.is_some());
                assert_eq!(
                    nexusiq.proof_capsule_ref.as_deref(),
                    Some(capsule.capsule_id.to_string().as_str())
                );
            }
            other => panic!("expected proof execution response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn legacy_execute_path_omits_proof_section() {
        let pool = HypervisorPool::new(1, HypervisorConfig::default()).unwrap();
        let mut req = execute_request();
        let DaemonRequest::Execute { aeon, .. } = &mut req else {
            unreachable!();
        };
        aeon.emit_proof = false;

        let response = serve_request(req, pool).await;

        match response {
            DaemonResponse::Executed {
                output,
                events,
                nexusiq,
            } => {
                assert!(output.success);
                assert!(nexusiq.is_none());
                assert!(!events
                    .iter()
                    .any(|event| matches!(event, NexusExecutionEvent::ProofCapsuleEmitted { .. })));
            }
            other => panic!("expected legacy execution response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn proof_mode_consumes_aeon_memory_evidence_digest() {
        let digest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let cfg = HypervisorConfig {
            aeon_config: Some(nexus::aeon::AeonConfig {
                enabled: true,
                base_url: "http://127.0.0.1:1".to_string(),
                agent_id: "agent-1".to_string(),
                session_id: Some("session-1".to_string()),
                timeout_ms: 1,
                management_key: Some("mgmt-key".to_string()),
                hmac_key: Some(vec![0x11, 0x22, 0x33]),
            }),
            ..HypervisorConfig::default()
        };
        let pool = HypervisorPool::new(1, cfg).unwrap();
        let mut req = execute_request();
        let DaemonRequest::Execute { aeon, .. } = &mut req else {
            unreachable!();
        };
        aeon.aeon_agent_id = Some("agent-1".to_string());
        aeon.aeon_session_id = Some("session-1".to_string());
        aeon.aeon_memory_evidence_digest = Some(digest.to_string());

        let response = serve_request(req, pool).await;

        match response {
            DaemonResponse::Executed {
                nexusiq: Some(nexusiq),
                ..
            } => {
                let capsule = nexusiq.proof_capsule.expect("proof capsule");
                let evidence = capsule.memory_evidence.expect("memory evidence");
                assert_eq!(evidence.digest.value, digest);
                assert_eq!(
                    capsule.memory_mode,
                    Some(nexus::proof::schema::MemoryAttestationMode::Advisory)
                );
                assert_eq!(
                    nexusiq
                        .memory_evidence_ref
                        .expect("memory ref")
                        .digest
                        .value,
                    digest
                );
            }
            other => panic!("expected proof execution response, got {other:?}"),
        }
    }
}
