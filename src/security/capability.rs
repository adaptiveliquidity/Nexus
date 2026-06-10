//! Capability Token System
//!
//! Cryptographic capability-based access control for Nexus sandboxes.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
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
    /// Check if this capability allows access to the requested capability
    pub fn allows(&self, requested: &Capability) -> bool {
        match (self, requested) {
            // Wildcard grants all
            (Capability::All, _) => true,

            // None denies all
            (Capability::None, _) => false,

            // Exact match for ReadFile
            (Capability::ReadFile(p1), Capability::ReadFile(p2)) => p1 == p2 || p2.starts_with(p1),

            // Write implies read
            (Capability::WriteFile(p1), Capability::ReadFile(p2)) => p2.starts_with(p1),

            // Exact match for WriteFile
            (Capability::WriteFile(p1), Capability::WriteFile(p2)) => p1 == p2,

            // Exact match for ListDirectory with subdir support
            (Capability::ListDirectory(p1), Capability::ListDirectory(p2)) => {
                p1 == p2 || p2.starts_with(p1)
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
    /// Signature over the token data
    pub signature: Vec<u8>,
}

impl CapabilityToken {
    /// Create a new capability token
    pub fn new(
        capability: Capability,
        granted_by: &str,
        validity_duration: std::time::Duration,
        signing_key: &SigningKey,
    ) -> Self {
        let now = Utc::now();
        let token = CapabilityToken {
            id: Uuid::new_v4(),
            capability,
            granted_by: granted_by.to_string(),
            issued_at: now,
            expires_at: now + validity_duration,
            signature: Vec::new(),
        };

        // Sign the token
        let data_to_sign = bincode::serialize(&(
            &token.id,
            &token.capability,
            &token.granted_by,
            &token.issued_at,
            &token.expires_at,
        ))
        .expect("serialization should not fail");
        let sig = signing_key.sign(&data_to_sign);

        let mut result = token;
        result.signature = sig.to_bytes().to_vec();
        result
    }

    /// Verify the token signature
    pub fn verify_signature(&self, verifying_key: &VerifyingKey) -> bool {
        let data_to_verify = bincode::serialize(&(
            &self.id,
            &self.capability,
            &self.granted_by,
            &self.issued_at,
            &self.expires_at,
        ))
        .expect("serialization should not fail");

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
    ) -> CapabilityToken {
        let token =
            CapabilityToken::new(capability, granted_by, validity_duration, &self.signing_key);

        self.active_tokens.insert(token.id, token.clone());
        token
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

        // Check capability
        if !token.allows(requested) {
            return Err(NexusError::InvalidCapability(format!(
                "Token {} does not grant {:?}",
                token.id, requested
            )));
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

        let token = manager.issue(
            Capability::ReadFile(PathBuf::from("/project")),
            "test-agent",
            std::time::Duration::from_secs(3600),
        );

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
}
