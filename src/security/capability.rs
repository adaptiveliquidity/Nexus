//! Capability Token System
//!
//! Cryptographic capability-based access control for Nexus sandboxes.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

use crate::error::{NexusError, Result};

/// Represents a specific capability that can be granted
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Read a specific file or directory
    ReadFile(PathBuf),
    /// Write to a specific file or directory
    WriteFile(PathBuf),
    /// List contents of a directory
    ListDirectory(PathBuf),
    /// Make HTTP GET requests to matching URLs
    HttpGet(String),
    /// Make HTTP POST requests to matching URLs
    HttpPost(String),
    /// Execute a specific binary
    ExecuteBinary(PathBuf),
    /// Mount tmpfs at a path
    MountTmpfs(PathBuf),
    /// All capabilities (admin)
    All,
    /// No capability (deny all)
    None,
}

impl Capability {
    fn path_contains(parent: &Path, requested: &Path) -> bool {
        let parent = normalize_lexical_path(parent);
        let requested = normalize_lexical_path(requested);
        parent == requested || requested.starts_with(parent)
    }

    fn path_eq(left: &Path, right: &Path) -> bool {
        normalize_lexical_path(left) == normalize_lexical_path(right)
    }

    /// Check if this capability allows access to the requested capability
    pub fn allows(&self, requested: &Capability) -> bool {
        match (self, requested) {
            // Wildcard grants all
            (Capability::All, _) => true,

            // None denies all
            (Capability::None, _) => false,

            // Exact match for ReadFile
            (Capability::ReadFile(p1), Capability::ReadFile(p2)) => Self::path_contains(p1, p2),

            // Write implies read
            (Capability::WriteFile(p1), Capability::ReadFile(p2)) => Self::path_contains(p1, p2),

            // Exact match for WriteFile
            (Capability::WriteFile(p1), Capability::WriteFile(p2)) => Self::path_eq(p1, p2),

            // Exact match for ListDirectory with subdir support
            (Capability::ListDirectory(p1), Capability::ListDirectory(p2)) => {
                Self::path_contains(p1, p2)
            }

            // Exact match for HTTP capabilities
            (Capability::HttpGet(p1), Capability::HttpGet(p2)) => p1 == p2,
            (Capability::HttpPost(p1), Capability::HttpPost(p2)) => p1 == p2,

            // Execute and MountTmpfs - exact match only
            (Capability::ExecuteBinary(p1), Capability::ExecuteBinary(p2)) => p1 == p2,
            (Capability::MountTmpfs(p1), Capability::MountTmpfs(p2)) => p1 == p2,

            // Default deny
            _ => false,
        }
    }

    /// True if `self` grants no more than `parent` — the relation a child
    /// capability must satisfy to be attenuated from `parent`. Strict inverse
    /// of `allows`, with explicit handling for the `None`/`All` lattice ends.
    pub fn is_subset_of(&self, parent: &Capability) -> bool {
        match (self, parent) {
            (Capability::None, _) => true, // deny-all is a subset of everything
            (_, Capability::All) => true,  // everything is a subset of All
            (Capability::All, _) => false, // All is only a subset of All (above)
            // Otherwise: parent must grant self (reuses path/scope logic).
            _ => parent.allows(self),
        }
    }

    /// Get a human-readable description
    pub fn description(&self) -> String {
        match self {
            Capability::ReadFile(p) => format!("read:{}", p.display()),
            Capability::WriteFile(p) => format!("write:{}", p.display()),
            Capability::ListDirectory(p) => format!("list:{}", p.display()),
            Capability::HttpGet(pattern) => format!("http_get:{}", pattern),
            Capability::HttpPost(pattern) => format!("http_post:{}", pattern),
            Capability::ExecuteBinary(p) => format!("exec:{}", p.display()),
            Capability::MountTmpfs(p) => format!("tmpfs:{}", p.display()),
            Capability::All => "all".to_string(),
            Capability::None => "none".to_string(),
        }
    }
}

/// Normalize a path lexically without consulting the filesystem.
///
/// This resolves `.` and `..` components for capability containment checks and
/// WASI preopen derivation while preserving non-existent paths and avoiding
/// symlink resolution.
pub(crate) fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let mut normal_depth = 0usize;
    let mut rooted = false;

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str());
                rooted = true;
            }
            Component::RootDir => {
                normalized.push(component.as_os_str());
                rooted = true;
            }
            Component::CurDir => {}
            Component::Normal(part) => {
                normalized.push(part);
                normal_depth += 1;
            }
            Component::ParentDir => {
                if normal_depth > 0 {
                    normalized.pop();
                    normal_depth -= 1;
                } else if !rooted {
                    normalized.push("..");
                }
            }
        }
    }

    normalized
}

/// A signed capability token with expiration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
    /// Unique identifier for this token
    pub id: Uuid,
    /// The capability this token grants
    pub capability: Capability,
    /// Who granted this capability
    pub granted_by: String,
    /// When this token was created
    pub issued_at: DateTime<Utc>,
    /// When this token expires
    pub expires_at: DateTime<Utc>,
    /// Parent token id when this token was minted by attenuation. `None` for
    /// a root token issued directly by the manager.
    pub parent_id: Option<Uuid>,
    /// Depth in the attenuation chain. `0` for a root token; each attenuation
    /// increments it. Capped by `DEFAULT_MAX_CHAIN_DEPTH`.
    pub chain_depth: u32,
    /// Signature over the token data
    pub signature: Vec<u8>,
}

/// Default maximum attenuation-chain depth (root token = 0).
pub const DEFAULT_MAX_CHAIN_DEPTH: u32 = 5;

impl CapabilityToken {
    /// Create a new capability token
    pub fn new(
        capability: Capability,
        granted_by: &str,
        validity_duration: std::time::Duration,
        signing_key: &SigningKey,
    ) -> Result<Self> {
        let now = Utc::now();
        let mut token = CapabilityToken {
            id: Uuid::new_v4(),
            capability,
            granted_by: granted_by.to_string(),
            issued_at: now,
            expires_at: now + validity_duration,
            parent_id: None,
            chain_depth: 0,
            signature: Vec::new(),
        };

        let data_to_sign = bincode::serialize(&(
            &token.id,
            &token.capability,
            &token.granted_by,
            &token.issued_at,
            &token.expires_at,
            &token.parent_id,
            &token.chain_depth,
        ))
        .map_err(|e| NexusError::SerializationError(format!("token signing: {e}")))?;
        token.signature = signing_key.sign(&data_to_sign).to_bytes().to_vec();
        Ok(token)
    }

    /// Verify the token signature
    pub fn verify_signature(&self, verifying_key: &VerifyingKey) -> bool {
        let Ok(data_to_verify) = bincode::serialize(&(
            &self.id,
            &self.capability,
            &self.granted_by,
            &self.issued_at,
            &self.expires_at,
            &self.parent_id,
            &self.chain_depth,
        )) else {
            return false;
        };

        let signature_array: [u8; 64] = self.signature.clone().try_into().unwrap_or([0u8; 64]);
        let sig = Signature::from_bytes(&signature_array);

        verifying_key.verify(&data_to_verify, &sig).is_ok()
    }

    /// Check if token is valid (not expired)
    pub fn is_valid(&self) -> bool {
        Utc::now() < self.expires_at
    }

    /// Check if token allows a specific capability
    pub fn allows(&self, requested: &Capability) -> bool {
        self.is_valid() && self.capability.allows(requested)
    }

    /// Mint a strictly-weaker child token bound to this token as its parent.
    /// Fails if `narrower` is not a subset of this token's capability, or if
    /// the resulting depth would exceed `max_depth`. The child's expiry is
    /// clamped so it can never outlive its parent.
    pub fn attenuate(
        &self,
        narrower: Capability,
        granted_by: &str,
        validity_duration: std::time::Duration,
        signing_key: &SigningKey,
        max_depth: u32,
    ) -> Result<CapabilityToken> {
        if !narrower.is_subset_of(&self.capability) {
            return Err(NexusError::InvalidCapability(format!(
                "attenuated capability {:?} is not a subset of parent {:?}",
                narrower, self.capability
            )));
        }
        let child_depth = self.chain_depth + 1;
        if child_depth > max_depth {
            return Err(NexusError::InvalidCapability(format!(
                "attenuation chain depth {child_depth} exceeds max {max_depth}"
            )));
        }
        let now = Utc::now();
        let expires_at = (now + validity_duration).min(self.expires_at);
        let mut token = CapabilityToken {
            id: Uuid::new_v4(),
            capability: narrower,
            granted_by: granted_by.to_string(),
            issued_at: now,
            expires_at,
            parent_id: Some(self.id),
            chain_depth: child_depth,
            signature: Vec::new(),
        };
        let data_to_sign = bincode::serialize(&(
            &token.id,
            &token.capability,
            &token.granted_by,
            &token.issued_at,
            &token.expires_at,
            &token.parent_id,
            &token.chain_depth,
        ))
        .map_err(|e| NexusError::SerializationError(format!("attenuate signing: {e}")))?;
        token.signature = signing_key.sign(&data_to_sign).to_bytes().to_vec();
        Ok(token)
    }
}

/// Manages capability tokens and access control
pub struct CapabilityManager {
    /// Signing key for issuing tokens
    signing_key: SigningKey,
    /// Verifying key for validating tokens
    verifying_key: VerifyingKey,
    /// Active tokens by ID
    active_tokens: HashMap<Uuid, CapabilityToken>,
    /// Token blacklist (for revocation)
    revoked_tokens: HashMap<Uuid, DateTime<Utc>>,
}

impl Default for CapabilityManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilityManager {
    /// Create a new capability manager with fresh keys
    pub fn new() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = VerifyingKey::from(&signing_key);

        CapabilityManager {
            signing_key,
            verifying_key,
            active_tokens: HashMap::new(),
            revoked_tokens: HashMap::new(),
        }
    }

    /// Create from existing keys
    pub fn from_keys(signing_key: SigningKey, verifying_key: VerifyingKey) -> Self {
        CapabilityManager {
            signing_key,
            verifying_key,
            active_tokens: HashMap::new(),
            revoked_tokens: HashMap::new(),
        }
    }

    /// Issue a new capability token
    pub fn issue(
        &mut self,
        capability: Capability,
        granted_by: &str,
        validity_duration: std::time::Duration,
    ) -> Result<CapabilityToken> {
        let token =
            CapabilityToken::new(capability, granted_by, validity_duration, &self.signing_key)?;

        self.active_tokens.insert(token.id, token.clone());
        Ok(token)
    }

    /// Attenuate an existing (registered) token into a strictly-weaker child,
    /// signing it with the manager's key and registering it so deeper chains
    /// can be validated later. Fails if `parent_id` is unknown or the
    /// narrowing is invalid (see `CapabilityToken::attenuate`).
    pub fn attenuate(
        &mut self,
        parent_id: Uuid,
        narrower: Capability,
        granted_by: &str,
        validity_duration: std::time::Duration,
    ) -> Result<CapabilityToken> {
        let parent = self.active_tokens.get(&parent_id).ok_or_else(|| {
            NexusError::InvalidCapability(format!("parent token {parent_id} not found"))
        })?;
        let child = parent.attenuate(
            narrower,
            granted_by,
            validity_duration,
            &self.signing_key,
            DEFAULT_MAX_CHAIN_DEPTH,
        )?;
        self.active_tokens.insert(child.id, child.clone());
        Ok(child)
    }

    /// Validate a token and check capability
    pub fn validate(&self, token: &CapabilityToken, requested: &Capability) -> Result<()> {
        // Check if revoked
        if let Some(revoked_at) = self.revoked_tokens.get(&token.id) {
            return Err(NexusError::InvalidCapability(format!(
                "Token {} was revoked at {}",
                token.id, revoked_at
            )));
        }

        // Check expiration
        if !token.is_valid() {
            return Err(NexusError::InvalidCapability(format!(
                "Token {} expired at {}",
                token.id, token.expires_at
            )));
        }

        // Verify signature
        if !token.verify_signature(&self.verifying_key) {
            return Err(NexusError::InvalidCapability(format!(
                "Token {} has invalid signature",
                token.id
            )));
        }

        // Walk and verify the attenuation chain (no-op for root tokens).
        if token.parent_id.is_some() {
            self.validate_chain(token, DEFAULT_MAX_CHAIN_DEPTH)?;
        }

        // Check capability
        if !token.allows(requested) {
            return Err(NexusError::InvalidCapability(format!(
                "Token {} does not grant {:?}",
                token.id, requested
            )));
        }

        Ok(())
    }

    /// Walk an attenuation chain from `token` to its root, verifying each
    /// ancestor's signature, expiry, revocation, depth monotonicity, and that
    /// every link is a subset of its parent. Ancestors must be registered in
    /// `active_tokens` (via `issue`/`attenuate`).
    fn validate_chain(&self, token: &CapabilityToken, max_depth: u32) -> Result<()> {
        if token.chain_depth > max_depth {
            return Err(NexusError::InvalidCapability(format!(
                "chain depth {} exceeds max {max_depth}",
                token.chain_depth
            )));
        }
        let mut child = token.clone();
        while let Some(pid) = child.parent_id {
            // Check revocation before the active_tokens lookup: revoke() removes
            // the token from active_tokens but records it in revoked_tokens.
            if let Some(at) = self.revoked_tokens.get(&pid) {
                return Err(NexusError::InvalidCapability(format!(
                    "ancestor {pid} was revoked at {at}"
                )));
            }
            let parent = self.active_tokens.get(&pid).ok_or_else(|| {
                NexusError::InvalidCapability(format!(
                    "broken attenuation chain: parent {pid} not found"
                ))
            })?;
            if !parent.verify_signature(&self.verifying_key) {
                return Err(NexusError::InvalidCapability(format!(
                    "ancestor {pid} has invalid signature"
                )));
            }
            if !parent.is_valid() {
                return Err(NexusError::InvalidCapability(format!(
                    "ancestor {pid} expired at {}",
                    parent.expires_at
                )));
            }
            if child.chain_depth != parent.chain_depth + 1 {
                return Err(NexusError::InvalidCapability(format!(
                    "non-monotonic chain depth at {pid}"
                )));
            }
            if !child.capability.is_subset_of(&parent.capability) {
                return Err(NexusError::InvalidCapability(format!(
                    "link {:?} is not a subset of parent {:?}",
                    child.capability, parent.capability
                )));
            }
            child = parent.clone();
        }
        Ok(())
    }

    /// Check that every required capability is covered by at least one
    /// valid, non-revoked token with a correct signature. Returns
    /// `CapabilityDenied` on the first unsatisfied requirement.
    pub fn authorize(&self, tokens: &[CapabilityToken], required: &[Capability]) -> Result<()> {
        for cap in required {
            let satisfied = tokens.iter().any(|t| self.validate(t, cap).is_ok());
            if !satisfied {
                return Err(NexusError::CapabilityDenied(format!(
                    "no valid token grants {:?}",
                    cap
                )));
            }
        }
        Ok(())
    }

    /// Revoke a token
    pub fn revoke(&mut self, token_id: Uuid) {
        self.revoked_tokens.insert(token_id, Utc::now());
        self.active_tokens.remove(&token_id);
    }

    /// Get the public key for external verification
    pub fn public_key(&self) -> Vec<u8> {
        self.verifying_key.as_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_allow() {
        let read_home = Capability::ReadFile(PathBuf::from("/home"));
        assert!(read_home.allows(&Capability::ReadFile(PathBuf::from("/home"))));
        assert!(read_home.allows(&Capability::ReadFile(PathBuf::from("/home/user"))));
        assert!(!read_home.allows(&Capability::ReadFile(PathBuf::from("/etc"))));
    }

    #[test]
    fn test_token_lifecycle() {
        let mut manager = CapabilityManager::new();

        let token = manager
            .issue(
                Capability::ReadFile(PathBuf::from("/project")),
                "test-agent",
                std::time::Duration::from_secs(3600),
            )
            .unwrap();

        assert!(token.is_valid());
        assert!(manager
            .validate(&token, &Capability::ReadFile(PathBuf::from("/project")))
            .is_ok());
        assert!(manager
            .validate(&token, &Capability::WriteFile(PathBuf::from("/project")))
            .is_err());

        manager.revoke(token.id);
        assert!(manager
            .validate(&token, &Capability::ReadFile(PathBuf::from("/project")))
            .is_err());
    }

    fn hour() -> std::time::Duration {
        std::time::Duration::from_secs(3600)
    }

    #[test]
    fn subset_path_narrowing() {
        let parent = Capability::ReadFile(PathBuf::from("/home"));
        let child = Capability::ReadFile(PathBuf::from("/home/user"));
        assert!(child.is_subset_of(&parent));
    }

    #[test]
    fn subset_rejects_broader() {
        let child = Capability::ReadFile(PathBuf::from("/home"));
        let parent = Capability::ReadFile(PathBuf::from("/home/user"));
        assert!(!child.is_subset_of(&parent));
    }

    #[test]
    fn subset_rejects_lexical_parent_escape() {
        let parent = Capability::ReadFile(PathBuf::from("/safe"));
        let child = Capability::ReadFile(PathBuf::from("/safe/../outside"));
        assert!(!child.is_subset_of(&parent));
        assert!(!parent.allows(&child));
    }

    #[test]
    fn lexical_normalization_keeps_valid_child_subset() {
        let parent = Capability::ReadFile(PathBuf::from("/safe/./data"));
        let child = Capability::ReadFile(PathBuf::from("/safe/data/nested/.."));
        assert!(child.is_subset_of(&parent));
        assert!(parent.allows(&child));
    }

    #[test]
    fn subset_none_and_all() {
        let read = Capability::ReadFile(PathBuf::from("/home"));
        assert!(Capability::None.is_subset_of(&read)); // deny-all ⊆ anything
        assert!(read.is_subset_of(&Capability::All)); // anything ⊆ All
        assert!(!Capability::All.is_subset_of(&read)); // All ⊄ a narrower cap
    }

    #[test]
    fn subset_read_under_write() {
        // Write implies read, so a read under the write path is a subset.
        let parent = Capability::WriteFile(PathBuf::from("/data"));
        let child = Capability::ReadFile(PathBuf::from("/data/file"));
        assert!(child.is_subset_of(&parent));
    }

    #[test]
    fn attenuate_narrower_ok() {
        let mut m = CapabilityManager::new();
        let root = m
            .issue(Capability::ReadFile(PathBuf::from("/home")), "root", hour())
            .unwrap();
        let child = m
            .attenuate(
                root.id,
                Capability::ReadFile(PathBuf::from("/home/user")),
                "delegate",
                hour(),
            )
            .unwrap();
        assert_eq!(child.parent_id, Some(root.id));
        assert_eq!(child.chain_depth, 1);
    }

    #[test]
    fn attenuate_broader_fails() {
        let mut m = CapabilityManager::new();
        let root = m
            .issue(
                Capability::ReadFile(PathBuf::from("/home/user")),
                "root",
                hour(),
            )
            .unwrap();
        // Broader path than parent → rejected.
        let res = m.attenuate(
            root.id,
            Capability::ReadFile(PathBuf::from("/home")),
            "delegate",
            hour(),
        );
        assert!(res.is_err());
    }

    #[test]
    fn attenuate_depth_cap() {
        let mut m = CapabilityManager::new();
        let mut current = m
            .issue(Capability::ReadFile(PathBuf::from("/a")), "root", hour())
            .unwrap();
        // Depth 0 root → 5 attenuations reach depth 5 (== DEFAULT_MAX_CHAIN_DEPTH).
        for _ in 0..DEFAULT_MAX_CHAIN_DEPTH {
            current = m
                .attenuate(
                    current.id,
                    Capability::ReadFile(PathBuf::from("/a")),
                    "d",
                    hour(),
                )
                .unwrap();
        }
        assert_eq!(current.chain_depth, DEFAULT_MAX_CHAIN_DEPTH);
        // The 6th attenuation would be depth 6 > max → error.
        let res = m.attenuate(
            current.id,
            Capability::ReadFile(PathBuf::from("/a")),
            "d",
            hour(),
        );
        assert!(res.is_err());
    }

    #[test]
    fn validate_full_chain() {
        let mut m = CapabilityManager::new();
        let root = m
            .issue(Capability::ReadFile(PathBuf::from("/home")), "root", hour())
            .unwrap();
        let mid = m
            .attenuate(
                root.id,
                Capability::ReadFile(PathBuf::from("/home/user")),
                "d",
                hour(),
            )
            .unwrap();
        let leaf = m
            .attenuate(
                mid.id,
                Capability::ReadFile(PathBuf::from("/home/user/docs")),
                "d",
                hour(),
            )
            .unwrap();
        assert!(m
            .validate(
                &leaf,
                &Capability::ReadFile(PathBuf::from("/home/user/docs"))
            )
            .is_ok());
    }

    #[test]
    fn validate_revoked_parent() {
        let mut m = CapabilityManager::new();
        let root = m
            .issue(Capability::ReadFile(PathBuf::from("/home")), "root", hour())
            .unwrap();
        let child = m
            .attenuate(
                root.id,
                Capability::ReadFile(PathBuf::from("/home/user")),
                "d",
                hour(),
            )
            .unwrap();
        m.revoke(root.id);
        assert!(m
            .validate(&child, &Capability::ReadFile(PathBuf::from("/home/user")))
            .is_err());
    }

    #[test]
    fn validate_expired_parent() {
        let mut m = CapabilityManager::new();
        // Parent already expired (zero validity); child expiry is clamped to it.
        let root = m
            .issue(
                Capability::ReadFile(PathBuf::from("/home")),
                "root",
                std::time::Duration::from_secs(0),
            )
            .unwrap();
        let child = m
            .attenuate(
                root.id,
                Capability::ReadFile(PathBuf::from("/home/user")),
                "d",
                hour(),
            )
            .unwrap();
        assert!(m
            .validate(&child, &Capability::ReadFile(PathBuf::from("/home/user")))
            .is_err());
    }

    #[test]
    fn child_expiry_clamped_to_parent() {
        let mut m = CapabilityManager::new();
        let root = m
            .issue(Capability::ReadFile(PathBuf::from("/home")), "root", hour())
            .unwrap();
        // Request a far longer validity than the parent has.
        let child = m
            .attenuate(
                root.id,
                Capability::ReadFile(PathBuf::from("/home/user")),
                "d",
                std::time::Duration::from_secs(36_000),
            )
            .unwrap();
        assert_eq!(child.expires_at, root.expires_at);
    }
}
