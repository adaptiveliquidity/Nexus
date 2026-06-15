//! Authenticated daemon-framed snapshot-sync transport.
//!
//! This is the RFC 0001 daemon-framing profile: it reuses the daemon's outer
//! `[u32 BE length][payload]` framing, but the payload is a binary frame with a
//! fixed header, bincode body, and HMAC-SHA256. The body is never deserialized
//! until the MAC and sequence number have been validated.

use std::io;
use std::time::Duration;

use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::daemon::protocol::{read_raw_frame_with_limit, write_raw_frame, DEFAULT_MAX_PAYLOAD};
use crate::error::{NexusError, Result};
use crate::snapshot::manager::Snapshot;
use crate::snapshot::sync::digest::SnapshotDigest;
use crate::snapshot::sync::protocol::{NackReason, SyncMessage, SyncNode};

type HmacSha256 = Hmac<Sha256>;

const FRAME_MAC_DOMAIN: &[u8] = b"NEXUS-SNAPSHOT-SYNC-FRAME-HMAC-v1";
const HANDSHAKE_MAGIC: &[u8] = b"NEXUS-SYNC-HELLO-v1";
const SESSION_DOMAIN: &[u8] = b"NEXUS-SYNC-v1";

pub const SYNC_FRAME_PROTO_VERSION: u16 = 1;
pub const SYNC_FRAME_MAC_ALG_HMAC_SHA256: u8 = 1;
pub const SYNC_FRAME_SESSION_LEN: usize = 16;
pub const SYNC_FRAME_NONCE_LEN: usize = 32;
pub const SYNC_FRAME_MAC_LEN: usize = 32;

const FRAME_HEADER_LEN: usize = 2 + 1 + 2 + SYNC_FRAME_SESSION_LEN + 8 + 1 + 4;
const FRAME_MIN_LEN: usize = FRAME_HEADER_LEN + SYNC_FRAME_MAC_LEN;
const HANDSHAKE_LEN: usize = 19 + 2 + 2 + SYNC_FRAME_NONCE_LEN;
const REPLICATE_IDLE_TIMEOUT: Duration = Duration::from_millis(10);

/// Authentication settings for the daemon-framed sync transport.
#[derive(Clone)]
pub struct SyncAuthConfig {
    pub node_key: [u8; 32],
    pub key_id: u16,
    pub max_payload: usize,
}

// Custom Debug that never prints the pre-shared `node_key`, so the secret can't
// leak into logs when this config is wired into daemon/CLI startup.
impl std::fmt::Debug for SyncAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncAuthConfig")
            .field("node_key", &"[redacted]")
            .field("key_id", &self.key_id)
            .field("max_payload", &self.max_payload)
            .finish()
    }
}

impl SyncAuthConfig {
    pub fn new(node_key: [u8; 32]) -> Self {
        Self {
            node_key,
            key_id: 0,
            max_payload: DEFAULT_MAX_PAYLOAD,
        }
    }

    pub fn with_key_id(mut self, key_id: u16) -> Self {
        self.key_id = key_id;
        self
    }

    pub fn with_max_payload(mut self, max_payload: usize) -> Self {
        self.max_payload = max_payload;
        self
    }

    /// Load the shared node key from `NEXUS_SYNC_NODE_KEY`.
    ///
    /// Accepts either 64 hex characters or base64-encoded 32 raw bytes. Optional
    /// `NEXUS_SYNC_KEY_ID` supplies the `u16` key id; absent means `0`.
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var("NEXUS_SYNC_NODE_KEY")
            .map_err(|_| NexusError::ConfigError("NEXUS_SYNC_NODE_KEY is not set".to_string()))?;
        let key = parse_key(&raw)?;
        let key_id = match std::env::var("NEXUS_SYNC_KEY_ID") {
            Ok(s) => s
                .parse::<u16>()
                .map_err(|e| NexusError::ConfigError(format!("invalid NEXUS_SYNC_KEY_ID: {e}")))?,
            Err(_) => 0,
        };
        Ok(Self::new(key).with_key_id(key_id))
    }
}

/// Async, fallible authenticated sync transport.
///
/// This intentionally does not implement the synchronous `SyncTransport`
/// trait. Authenticated framing is async I/O with meaningful failure modes.
pub struct FramedSyncTransport<R, W> {
    reader: R,
    writer: W,
    auth: SyncAuthConfig,
    session_id: [u8; SYNC_FRAME_SESSION_LEN],
    tx_seq: u64,
    expected_rx_seq: u64,
}

impl<R, W> FramedSyncTransport<R, W>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    /// Initiate a connection by sending a client nonce and deriving the session
    /// id from the peer's server nonce.
    pub async fn connect_initiator(
        mut reader: R,
        mut writer: W,
        auth: SyncAuthConfig,
    ) -> Result<Self> {
        let client_nonce = random_nonce();
        write_handshake(&mut writer, auth.key_id, &client_nonce).await?;
        let server_nonce = read_handshake(&mut reader, auth.key_id, auth.max_payload).await?;
        let session_id = derive_session_id(&client_nonce, &server_nonce);
        Ok(Self::from_parts(reader, writer, auth, session_id))
    }

    /// Accept a connection by reading the client nonce and replying with a
    /// fresh server nonce.
    pub async fn accept(mut reader: R, mut writer: W, auth: SyncAuthConfig) -> Result<Self> {
        let client_nonce = read_handshake(&mut reader, auth.key_id, auth.max_payload).await?;
        let server_nonce = random_nonce();
        write_handshake(&mut writer, auth.key_id, &server_nonce).await?;
        let session_id = derive_session_id(&client_nonce, &server_nonce);
        Ok(Self::from_parts(reader, writer, auth, session_id))
    }

    fn from_parts(
        reader: R,
        writer: W,
        auth: SyncAuthConfig,
        session_id: [u8; SYNC_FRAME_SESSION_LEN],
    ) -> Self {
        Self {
            reader,
            writer,
            auth,
            session_id,
            tx_seq: 0,
            expected_rx_seq: 0,
        }
    }

    pub async fn send(&mut self, msg: SyncMessage) -> Result<()> {
        let wire = WireSyncMessage::from(msg);
        let kind = wire.kind();
        let body = bincode::serialize(&wire)
            .map_err(|e| NexusError::SerializationError(format!("sync body serialize: {e}")))?;
        let frame = encode_frame(
            &self.auth.node_key,
            self.auth.key_id,
            self.session_id,
            self.tx_seq,
            kind,
            &body,
        )?;
        write_raw_frame(&mut self.writer, &frame)
            .await
            .map_err(io_err)?;
        self.tx_seq = self
            .tx_seq
            .checked_add(1)
            .ok_or_else(|| NexusError::ConfigError("sync tx sequence overflow".to_string()))?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Option<SyncMessage>> {
        let frame = match read_raw_frame_with_limit(&mut self.reader, self.auth.max_payload).await {
            Ok(frame) => frame,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(io_err(e)),
        };

        let (seq, kind, body) = verify_frame(
            &self.auth.node_key,
            self.auth.key_id,
            self.session_id,
            &frame,
        )?;
        if seq != self.expected_rx_seq {
            return Err(NexusError::ConfigError(format!(
                "sync frame out of order: got seq {seq}, expected {}",
                self.expected_rx_seq
            )));
        }
        self.expected_rx_seq = self
            .expected_rx_seq
            .checked_add(1)
            .ok_or_else(|| NexusError::ConfigError("sync rx sequence overflow".to_string()))?;

        let wire: WireSyncMessage = bincode::deserialize(body)
            .map_err(|e| NexusError::SerializationError(format!("sync body deserialize: {e}")))?;
        if wire.kind() != kind {
            return Err(NexusError::ConfigError(format!(
                "sync frame kind mismatch: header={kind:?}, body={:?}",
                wire.kind()
            )));
        }
        Ok(Some(wire.into_sync_message()))
    }
}

/// Drive two authenticated framed transports until the Phase 2 state machine
/// reaches quiescence.
pub async fn replicate_framed<RA, WA, RB, WB>(
    a: &mut SyncNode,
    ta: &mut FramedSyncTransport<RA, WA>,
    b: &mut SyncNode,
    tb: &mut FramedSyncTransport<RB, WB>,
    max_steps: usize,
) -> Result<()>
where
    RA: AsyncReadExt + Unpin,
    WA: AsyncWriteExt + Unpin,
    RB: AsyncReadExt + Unpin,
    WB: AsyncWriteExt + Unpin,
{
    ta.send(a.advertise()).await?;

    for _ in 0..max_steps {
        let mut progressed = false;

        if let Some(msg) = recv_if_ready(tb).await? {
            progressed = true;
            for out in b.handle(msg) {
                tb.send(out).await?;
            }
        }

        if let Some(msg) = recv_if_ready(ta).await? {
            progressed = true;
            for out in a.handle(msg) {
                ta.send(out).await?;
            }
        }

        if !progressed {
            return Ok(());
        }
    }

    Err(NexusError::ConfigError(format!(
        "framed snapshot-sync did not reach quiescence within {max_steps} steps"
    )))
}

async fn recv_if_ready<R, W>(
    transport: &mut FramedSyncTransport<R, W>,
) -> Result<Option<SyncMessage>>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    match tokio::time::timeout(REPLICATE_IDLE_TIMEOUT, transport.recv()).await {
        Ok(result) => result,
        Err(_) => Ok(None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncFrameKind {
    Advertise = 1,
    Want = 2,
    Snapshot = 3,
    Ack = 4,
    Nack = 5,
}

impl SyncFrameKind {
    fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(Self::Advertise),
            2 => Ok(Self::Want),
            3 => Ok(Self::Snapshot),
            4 => Ok(Self::Ack),
            5 => Ok(Self::Nack),
            _ => Err(NexusError::ConfigError(format!(
                "unknown sync frame kind {v}"
            ))),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum WireNackReason {
    DigestMismatch,
    Gone,
}

// Internal wire mirror of SyncMessage. The Snapshot variant is inherently large;
// the enum is serialized immediately (never held in a collection), so the size
// difference is irrelevant here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize)]
enum WireSyncMessage {
    Advertise {
        digests: Vec<[u8; 32]>,
    },
    Want {
        digests: Vec<[u8; 32]>,
    },
    Snapshot {
        digest: [u8; 32],
        snapshot: Snapshot,
    },
    Ack {
        digest: [u8; 32],
    },
    Nack {
        digest: [u8; 32],
        reason: WireNackReason,
    },
}

impl WireSyncMessage {
    fn kind(&self) -> SyncFrameKind {
        match self {
            Self::Advertise { .. } => SyncFrameKind::Advertise,
            Self::Want { .. } => SyncFrameKind::Want,
            Self::Snapshot { .. } => SyncFrameKind::Snapshot,
            Self::Ack { .. } => SyncFrameKind::Ack,
            Self::Nack { .. } => SyncFrameKind::Nack,
        }
    }

    fn into_sync_message(self) -> SyncMessage {
        match self {
            Self::Advertise { digests } => SyncMessage::Advertise {
                digests: digests
                    .into_iter()
                    .map(SnapshotDigest::from_bytes)
                    .collect(),
            },
            Self::Want { digests } => SyncMessage::Want {
                digests: digests
                    .into_iter()
                    .map(SnapshotDigest::from_bytes)
                    .collect(),
            },
            Self::Snapshot { digest, snapshot } => SyncMessage::Snapshot {
                digest: SnapshotDigest::from_bytes(digest),
                snapshot: Box::new(snapshot),
            },
            Self::Ack { digest } => SyncMessage::Ack {
                digest: SnapshotDigest::from_bytes(digest),
            },
            Self::Nack { digest, reason } => SyncMessage::Nack {
                digest: SnapshotDigest::from_bytes(digest),
                reason: match reason {
                    WireNackReason::DigestMismatch => NackReason::DigestMismatch,
                    WireNackReason::Gone => NackReason::Gone,
                },
            },
        }
    }
}

impl From<SyncMessage> for WireSyncMessage {
    fn from(msg: SyncMessage) -> Self {
        match msg {
            SyncMessage::Advertise { digests } => Self::Advertise {
                digests: digests.into_iter().map(|d| *d.as_bytes()).collect(),
            },
            SyncMessage::Want { digests } => Self::Want {
                digests: digests.into_iter().map(|d| *d.as_bytes()).collect(),
            },
            SyncMessage::Snapshot { digest, snapshot } => Self::Snapshot {
                digest: *digest.as_bytes(),
                snapshot: *snapshot,
            },
            SyncMessage::Ack { digest } => Self::Ack {
                digest: *digest.as_bytes(),
            },
            SyncMessage::Nack { digest, reason } => Self::Nack {
                digest: *digest.as_bytes(),
                reason: match reason {
                    NackReason::DigestMismatch => WireNackReason::DigestMismatch,
                    NackReason::Gone => WireNackReason::Gone,
                },
            },
        }
    }
}

fn encode_frame(
    key: &[u8; 32],
    key_id: u16,
    session_id: [u8; SYNC_FRAME_SESSION_LEN],
    seq: u64,
    kind: SyncFrameKind,
    body: &[u8],
) -> Result<Vec<u8>> {
    let body_len = u32::try_from(body.len()).map_err(|_| {
        NexusError::ConfigError("sync frame body exceeds u32 length prefix".to_string())
    })?;
    let mut frame = Vec::with_capacity(FRAME_MIN_LEN + body.len());
    frame.extend_from_slice(&SYNC_FRAME_PROTO_VERSION.to_be_bytes());
    frame.push(SYNC_FRAME_MAC_ALG_HMAC_SHA256);
    frame.extend_from_slice(&key_id.to_be_bytes());
    frame.extend_from_slice(&session_id);
    frame.extend_from_slice(&seq.to_be_bytes());
    frame.push(kind as u8);
    frame.extend_from_slice(&body_len.to_be_bytes());
    frame.extend_from_slice(body);
    let mac = compute_mac(key, key_id, session_id, seq, kind, body_len, body)?;
    frame.extend_from_slice(&mac);
    Ok(frame)
}

fn verify_frame<'a>(
    key: &[u8; 32],
    expected_key_id: u16,
    expected_session_id: [u8; SYNC_FRAME_SESSION_LEN],
    frame: &'a [u8],
) -> Result<(u64, SyncFrameKind, &'a [u8])> {
    if frame.len() < FRAME_MIN_LEN {
        return Err(NexusError::ConfigError(format!(
            "sync frame too short: {} < {FRAME_MIN_LEN}",
            frame.len()
        )));
    }

    let proto_version = u16::from_be_bytes([frame[0], frame[1]]);
    if proto_version != SYNC_FRAME_PROTO_VERSION {
        return Err(NexusError::ConfigError(format!(
            "unsupported sync frame proto_version {proto_version}"
        )));
    }

    let mac_alg = frame[2];
    if mac_alg != SYNC_FRAME_MAC_ALG_HMAC_SHA256 {
        return Err(NexusError::ConfigError(format!(
            "unsupported sync frame mac_alg {mac_alg}"
        )));
    }

    let key_id = u16::from_be_bytes([frame[3], frame[4]]);
    if key_id != expected_key_id {
        return Err(NexusError::ConfigError(format!(
            "sync frame key_id {key_id} did not match expected {expected_key_id}"
        )));
    }

    let mut session_id = [0u8; SYNC_FRAME_SESSION_LEN];
    session_id.copy_from_slice(&frame[5..21]);
    if session_id != expected_session_id {
        return Err(NexusError::ConfigError(
            "sync frame session_id mismatch".to_string(),
        ));
    }

    let seq = u64::from_be_bytes(frame[21..29].try_into().unwrap());
    let kind = SyncFrameKind::from_u8(frame[29])?;
    let body_len = u32::from_be_bytes(frame[30..34].try_into().unwrap());
    let body_start = FRAME_HEADER_LEN;
    let body_end = body_start + body_len as usize;
    let mac_end = body_end + SYNC_FRAME_MAC_LEN;
    if mac_end != frame.len() {
        return Err(NexusError::ConfigError(format!(
            "sync frame body_len mismatch: body_len={body_len}, frame_len={}",
            frame.len()
        )));
    }

    let body = &frame[body_start..body_end];
    let received_mac = &frame[body_end..mac_end];
    HmacSha256::new_from_slice(key)
        .map_err(|e| NexusError::ConfigError(format!("invalid sync HMAC key: {e}")))?
        .chain_update(mac_preimage(key_id, session_id, seq, kind, body_len, body))
        .verify_slice(received_mac)
        .map_err(|_| NexusError::ConfigError("sync frame MAC verification failed".to_string()))?;

    Ok((seq, kind, body))
}

fn compute_mac(
    key: &[u8; 32],
    key_id: u16,
    session_id: [u8; SYNC_FRAME_SESSION_LEN],
    seq: u64,
    kind: SyncFrameKind,
    body_len: u32,
    body: &[u8],
) -> Result<[u8; SYNC_FRAME_MAC_LEN]> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|e| NexusError::ConfigError(format!("invalid sync HMAC key: {e}")))?;
    mac.update(&mac_preimage(key_id, session_id, seq, kind, body_len, body));
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; SYNC_FRAME_MAC_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn mac_preimage(
    key_id: u16,
    session_id: [u8; SYNC_FRAME_SESSION_LEN],
    seq: u64,
    kind: SyncFrameKind,
    body_len: u32,
    body: &[u8],
) -> Vec<u8> {
    let body_hash = Sha256::digest(body);
    let mut out = Vec::with_capacity(FRAME_MAC_DOMAIN.len() + 2 + 1 + 2 + 16 + 8 + 1 + 4 + 32);
    out.extend_from_slice(FRAME_MAC_DOMAIN);
    out.extend_from_slice(&SYNC_FRAME_PROTO_VERSION.to_be_bytes());
    out.push(SYNC_FRAME_MAC_ALG_HMAC_SHA256);
    out.extend_from_slice(&key_id.to_be_bytes());
    out.extend_from_slice(&session_id);
    out.extend_from_slice(&seq.to_be_bytes());
    out.push(kind as u8);
    out.extend_from_slice(&body_len.to_be_bytes());
    out.extend_from_slice(&body_hash);
    out
}

async fn write_handshake<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    key_id: u16,
    nonce: &[u8; SYNC_FRAME_NONCE_LEN],
) -> Result<()> {
    let mut body = Vec::with_capacity(HANDSHAKE_LEN);
    body.extend_from_slice(HANDSHAKE_MAGIC);
    body.extend_from_slice(&SYNC_FRAME_PROTO_VERSION.to_be_bytes());
    body.extend_from_slice(&key_id.to_be_bytes());
    body.extend_from_slice(nonce);
    write_raw_frame(writer, &body).await.map_err(io_err)
}

async fn read_handshake<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    expected_key_id: u16,
    max_payload: usize,
) -> Result<[u8; SYNC_FRAME_NONCE_LEN]> {
    let body = read_raw_frame_with_limit(reader, max_payload)
        .await
        .map_err(io_err)?;
    if body.len() != HANDSHAKE_LEN || !body.starts_with(HANDSHAKE_MAGIC) {
        return Err(NexusError::ConfigError(
            "invalid sync handshake frame".to_string(),
        ));
    }
    let offset = HANDSHAKE_MAGIC.len();
    let proto_version = u16::from_be_bytes([body[offset], body[offset + 1]]);
    if proto_version != SYNC_FRAME_PROTO_VERSION {
        return Err(NexusError::ConfigError(format!(
            "unsupported sync handshake proto_version {proto_version}"
        )));
    }
    let key_id = u16::from_be_bytes([body[offset + 2], body[offset + 3]]);
    if key_id != expected_key_id {
        return Err(NexusError::ConfigError(format!(
            "sync handshake key_id {key_id} did not match expected {expected_key_id}"
        )));
    }
    let mut nonce = [0u8; SYNC_FRAME_NONCE_LEN];
    nonce.copy_from_slice(&body[offset + 4..offset + 4 + SYNC_FRAME_NONCE_LEN]);
    Ok(nonce)
}

fn derive_session_id(
    client_nonce: &[u8; SYNC_FRAME_NONCE_LEN],
    server_nonce: &[u8; SYNC_FRAME_NONCE_LEN],
) -> [u8; SYNC_FRAME_SESSION_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(client_nonce);
    hasher.update(server_nonce);
    hasher.update(SESSION_DOMAIN);
    let digest = hasher.finalize();
    let mut session = [0u8; SYNC_FRAME_SESSION_LEN];
    session.copy_from_slice(&digest[..SYNC_FRAME_SESSION_LEN]);
    session
}

fn random_nonce() -> [u8; SYNC_FRAME_NONCE_LEN] {
    let mut nonce = [0u8; SYNC_FRAME_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

fn parse_key(raw: &str) -> Result<[u8; 32]> {
    let trimmed = raw.trim();
    if trimmed.len() == 64 && trimmed.as_bytes().iter().all(|b| b.is_ascii_hexdigit()) {
        let mut out = [0u8; 32];
        let bytes = trimmed.as_bytes();
        for (i, byte) in out.iter_mut().enumerate() {
            let hi = hex_val(bytes[i * 2])?;
            let lo = hex_val(bytes[i * 2 + 1])?;
            *byte = (hi << 4) | lo;
        }
        return Ok(out);
    }

    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(trimmed.as_bytes())
        .map_err(|e| NexusError::ConfigError(format!("invalid NEXUS_SYNC_NODE_KEY: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| NexusError::ConfigError("NEXUS_SYNC_NODE_KEY must be 32 bytes".to_string()))
}

fn hex_val(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(NexusError::ConfigError(
            "invalid hex digit in NEXUS_SYNC_NODE_KEY".to_string(),
        )),
    }
}

fn io_err(e: io::Error) -> NexusError {
    NexusError::ConfigError(format!("sync framed I/O: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::manager::{ExecutionState, FilesystemDiff, SnapshotMetadata};

    fn auth() -> SyncAuthConfig {
        SyncAuthConfig::new([0x42; 32])
    }

    fn digest(byte: u8) -> SnapshotDigest {
        SnapshotDigest::from_bytes([byte; 32])
    }

    #[test]
    fn frame_round_trip_requires_matching_seq_and_mac() {
        let msg = WireSyncMessage::Want {
            digests: vec![*digest(7).as_bytes()],
        };
        let body = bincode::serialize(&msg).unwrap();
        let session = [1u8; SYNC_FRAME_SESSION_LEN];
        let frame = encode_frame(
            &auth().node_key,
            auth().key_id,
            session,
            0,
            SyncFrameKind::Want,
            &body,
        )
        .unwrap();
        let (seq, kind, decoded_body) =
            verify_frame(&auth().node_key, auth().key_id, session, &frame).unwrap();
        assert_eq!(seq, 0);
        assert_eq!(kind, SyncFrameKind::Want);
        assert_eq!(decoded_body, body.as_slice());
    }

    #[test]
    fn tampered_body_fails_before_deserialize() {
        let body = bincode::serialize(&WireSyncMessage::Ack {
            digest: *digest(8).as_bytes(),
        })
        .unwrap();
        let session = [2u8; SYNC_FRAME_SESSION_LEN];
        let mut frame = encode_frame(
            &auth().node_key,
            auth().key_id,
            session,
            0,
            SyncFrameKind::Ack,
            &body,
        )
        .unwrap();
        frame[FRAME_HEADER_LEN] ^= 0xff;
        assert!(verify_frame(&auth().node_key, auth().key_id, session, &frame).is_err());
    }

    #[test]
    fn mismatched_session_fails() {
        let body = bincode::serialize(&WireSyncMessage::Ack {
            digest: *digest(9).as_bytes(),
        })
        .unwrap();
        let frame = encode_frame(
            &auth().node_key,
            auth().key_id,
            [3u8; SYNC_FRAME_SESSION_LEN],
            0,
            SyncFrameKind::Ack,
            &body,
        )
        .unwrap();
        assert!(verify_frame(
            &auth().node_key,
            auth().key_id,
            [4u8; SYNC_FRAME_SESSION_LEN],
            &frame
        )
        .is_err());
    }

    #[test]
    fn body_kind_mismatch_is_detectable_after_auth() {
        let body = bincode::serialize(&WireSyncMessage::Ack {
            digest: *digest(10).as_bytes(),
        })
        .unwrap();
        let session = [5u8; SYNC_FRAME_SESSION_LEN];
        let frame = encode_frame(
            &auth().node_key,
            auth().key_id,
            session,
            0,
            SyncFrameKind::Want,
            &body,
        )
        .unwrap();
        let (_, kind, decoded_body) =
            verify_frame(&auth().node_key, auth().key_id, session, &frame).unwrap();
        let wire: WireSyncMessage = bincode::deserialize(decoded_body).unwrap();
        assert_ne!(wire.kind(), kind);
    }

    #[test]
    fn derive_session_id_changes_with_nonce() {
        let client = [1u8; SYNC_FRAME_NONCE_LEN];
        let a = derive_session_id(&client, &[2u8; SYNC_FRAME_NONCE_LEN]);
        let b = derive_session_id(&client, &[3u8; SYNC_FRAME_NONCE_LEN]);
        assert_ne!(a, b);
    }

    #[test]
    fn wire_snapshot_round_trips() {
        let snapshot = Snapshot::new(
            vec![1, 2, 3],
            FilesystemDiff::new(),
            ExecutionState::default(),
            SnapshotMetadata::new("op".into(), "input".into()),
        )
        .unwrap();
        let msg = SyncMessage::Snapshot {
            digest: digest(11),
            snapshot: Box::new(snapshot),
        };
        let wire = WireSyncMessage::from(msg);
        let encoded = bincode::serialize(&wire).unwrap();
        let decoded: WireSyncMessage = bincode::deserialize(&encoded).unwrap();
        assert!(matches!(
            decoded.into_sync_message(),
            SyncMessage::Snapshot { .. }
        ));
    }

    #[tokio::test]
    async fn duplicate_sequence_is_rejected() {
        let (client, server) = tokio::io::duplex(8192);
        let (cr, cw) = tokio::io::split(client);
        let (sr, sw) = tokio::io::split(server);
        let auth = auth();

        let (initiator, acceptor) = tokio::join!(
            FramedSyncTransport::connect_initiator(cr, cw, auth.clone()),
            FramedSyncTransport::accept(sr, sw, auth)
        );
        let mut initiator = initiator.unwrap();
        let mut acceptor = acceptor.unwrap();

        initiator
            .send(SyncMessage::Ack { digest: digest(12) })
            .await
            .unwrap();
        assert!(acceptor.recv().await.unwrap().is_some());

        initiator.tx_seq = 0;
        initiator
            .send(SyncMessage::Ack { digest: digest(13) })
            .await
            .unwrap();
        assert!(acceptor.recv().await.is_err());
    }
}
