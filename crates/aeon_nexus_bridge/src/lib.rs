//! Shared AEON-IQ/Nexus memory evidence wire types.
//!
//! The crate intentionally does not depend on Nexus or AEON-IQ internals. It
//! owns the stable wire records and crypto helpers both sides can share when a
//! Nexus proof capsule later attests AEON-IQ memory recall evidence.

use std::error::Error;
use std::fmt::{self, Write as _};

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const SHA256_ALGORITHM: &str = "sha256";
pub const HMAC_SHA256_ALGORITHM: &str = "hmac-sha256";
pub const MEMORY_EVIDENCE_VERSION: &str = "aeon-nexus-memory-evidence-v1";

/// Digest metadata matching Nexus proof-capsule digest semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedDigest {
    pub algorithm: String,
    pub value: String,
    pub public_recomputable: bool,
}

impl TypedDigest {
    pub fn sha256_public(data: &[u8]) -> Self {
        let digest = Sha256::digest(data);

        Self {
            algorithm: SHA256_ALGORITHM.to_owned(),
            value: hex_lower(&digest),
            public_recomputable: true,
        }
    }

    pub fn hmac_sha256_private(key: &[u8], data: &[u8]) -> Self {
        let mut mac =
            HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts keys of any length");
        mac.update(data);
        let digest = mac.finalize().into_bytes();

        Self {
            algorithm: HMAC_SHA256_ALGORITHM.to_owned(),
            value: hex_lower(&digest),
            public_recomputable: false,
        }
    }
}

/// Fixed-point memory-recall score serialized as integer micros.
///
/// AEON-IQ currently returns an optional floating-point score. The bridge stores
/// the value as integer micros so canonical JSON digests never depend on float
/// formatting or NaN/Infinity edge cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemoryScore {
    micros: i64,
}

impl MemoryScore {
    pub const MICROS_PER_UNIT: f64 = 1_000_000.0;

    pub fn new(score: f64) -> Result<Self, BridgeError> {
        if !score.is_finite() {
            return Err(BridgeError::InvalidScore(score));
        }

        let micros = (score * Self::MICROS_PER_UNIT).round();
        if micros < i64::MIN as f64 || micros > i64::MAX as f64 {
            return Err(BridgeError::InvalidScore(score));
        }

        Ok(Self {
            micros: micros as i64,
        })
    }

    pub fn from_micros(micros: i64) -> Self {
        Self { micros }
    }

    pub fn as_micros(self) -> i64 {
        self.micros
    }

    pub fn as_f64(self) -> f64 {
        self.micros as f64 / Self::MICROS_PER_UNIT
    }
}

/// A single AEON-IQ memory result injected into a Nexus decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEvidenceHit {
    pub memory_id: String,
    pub score: Option<MemoryScore>,
    pub content_digest: TypedDigest,
}

impl MemoryEvidenceHit {
    pub fn new(
        memory_id: impl Into<String>,
        content: impl AsRef<[u8]>,
        score: Option<f64>,
    ) -> Result<Self, BridgeError> {
        Ok(Self {
            memory_id: memory_id.into(),
            score: score.map(MemoryScore::new).transpose()?,
            content_digest: content_digest(content),
        })
    }

    pub fn with_content_digest(
        memory_id: impl Into<String>,
        content_digest: TypedDigest,
        score: Option<MemoryScore>,
    ) -> Self {
        Self {
            memory_id: memory_id.into(),
            score,
            content_digest,
        }
    }
}

/// Canonical record of AEON-IQ memory evidence injected into a Nexus decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEvidence {
    pub version: String,
    pub agent_handle: TypedDigest,
    pub session_id: Option<String>,
    pub injected_hits: Vec<MemoryEvidenceHit>,
}

impl MemoryEvidence {
    pub const VERSION: &'static str = MEMORY_EVIDENCE_VERSION;

    pub fn new(
        agent_handle: TypedDigest,
        session_id: Option<String>,
        injected_hits: Vec<MemoryEvidenceHit>,
    ) -> Self {
        Self {
            version: Self::VERSION.to_owned(),
            agent_handle,
            session_id,
            injected_hits,
        }
    }

    pub fn to_ref(&self) -> Result<MemoryEvidenceRef, BridgeError> {
        Ok(MemoryEvidenceRef {
            evidence_version: self.version.clone(),
            digest: memory_evidence_digest(self)?,
            agent_handle: self.agent_handle.clone(),
            session_id: self.session_id.clone(),
            injected_count: self.injected_hits.len(),
        })
    }
}

/// Lightweight capsule-ready reference to full memory evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEvidenceRef {
    pub evidence_version: String,
    pub digest: TypedDigest,
    pub agent_handle: TypedDigest,
    pub session_id: Option<String>,
    pub injected_count: usize,
}

/// Explicit mapping between Nexus' local agent/session naming and AEON-IQ's
/// memory-tenant `agent_id` namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionMapping {
    nexus_agent_id: String,
    session_id: Option<String>,
    aeon_agent_id: String,
}

impl AgentSessionMapping {
    pub fn new(
        nexus_agent_id: impl Into<String>,
        session_id: Option<String>,
        aeon_agent_id: impl Into<String>,
    ) -> Self {
        Self {
            nexus_agent_id: nexus_agent_id.into(),
            session_id,
            aeon_agent_id: aeon_agent_id.into(),
        }
    }

    pub fn nexus_agent_id(&self) -> &str {
        &self.nexus_agent_id
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn aeon_agent_id(&self) -> &str {
        &self.aeon_agent_id
    }

    pub fn agent_handle(&self, key: &[u8]) -> TypedDigest {
        hmac_agent_id(key, &self.aeon_agent_id)
    }

    pub fn memory_evidence(
        &self,
        key: &[u8],
        injected_hits: Vec<MemoryEvidenceHit>,
    ) -> MemoryEvidence {
        MemoryEvidence::new(
            self.agent_handle(key),
            self.session_id.clone(),
            injected_hits,
        )
    }
}

#[derive(Debug)]
pub enum BridgeError {
    Serialization(serde_json::Error),
    InvalidScore(f64),
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialization(error) => {
                write!(f, "failed to serialize canonical memory evidence: {error}")
            }
            Self::InvalidScore(score) => {
                write!(
                    f,
                    "memory score must be finite and in i64 micro range: {score}"
                )
            }
        }
    }
}

impl Error for BridgeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialization(error) => Some(error),
            Self::InvalidScore(_) => None,
        }
    }
}

impl From<serde_json::Error> for BridgeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

pub fn content_digest(content: impl AsRef<[u8]>) -> TypedDigest {
    TypedDigest::sha256_public(content.as_ref())
}

pub fn hmac_agent_id(key: &[u8], agent_id: &str) -> TypedDigest {
    TypedDigest::hmac_sha256_private(key, agent_id.as_bytes())
}

pub fn verify_agent_id_hmac(key: &[u8], agent_id: &str, expected: &TypedDigest) -> bool {
    if expected.algorithm != HMAC_SHA256_ALGORITHM || expected.public_recomputable {
        return false;
    }

    let Ok(expected_bytes) = decode_hex(&expected.value) else {
        return false;
    };

    let Ok(mut mac) = HmacSha256::new_from_slice(key) else {
        return false;
    };
    mac.update(agent_id.as_bytes());
    mac.verify_slice(&expected_bytes).is_ok()
}

pub fn canonical_bytes<T>(value: &T) -> Result<Vec<u8>, BridgeError>
where
    T: Serialize + ?Sized,
{
    let mut value = serde_json::to_value(value)?;
    sort_json_value(&mut value);
    Ok(serde_json::to_vec(&value)?)
}

pub fn canonical_sha256_digest<T>(value: &T) -> Result<TypedDigest, BridgeError>
where
    T: Serialize + ?Sized,
{
    Ok(TypedDigest::sha256_public(&canonical_bytes(value)?))
}

pub fn memory_evidence_digest(evidence: &MemoryEvidence) -> Result<TypedDigest, BridgeError> {
    canonical_sha256_digest(evidence)
}

fn sort_json_value(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                sort_json_value(value);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                sort_json_value(value);
            }
            map.sort_keys();
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    hex
}

fn decode_hex(input: &str) -> Result<Vec<u8>, ()> {
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(());
    }

    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        decoded.push((high << 4) | low);
    }

    Ok(decoded)
}

fn hex_value(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
    }
}
