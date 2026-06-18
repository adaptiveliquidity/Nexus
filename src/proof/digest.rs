use std::{env, fmt::Write};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::proof::{receipt::ProofHmacKey, TypedDigest};

type HmacSha256 = Hmac<Sha256>;

impl TypedDigest {
    pub fn sha256_public(data: &[u8]) -> Self {
        let digest = Sha256::digest(data);

        Self {
            algorithm: "sha256".to_owned(),
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
            algorithm: "hmac-sha256".to_owned(),
            value: hex_lower(&digest),
            public_recomputable: false,
        }
    }

    pub fn redacted() -> Self {
        Self {
            algorithm: "none".to_owned(),
            value: "[REDACTED]".to_owned(),
            public_recomputable: false,
        }
    }
}

pub fn digest_with_key(key: &ProofHmacKey, data: &[u8]) -> TypedDigest {
    match key {
        ProofHmacKey::Disabled => TypedDigest::redacted(),
        ProofHmacKey::FromEnv(var) => {
            let secret = env::var(var).expect("proof HMAC key environment variable must be set");
            TypedDigest::hmac_sha256_private(secret.as_bytes(), data)
        }
        ProofHmacKey::EphemeralTestOnly => TypedDigest::hmac_sha256_private(&[0_u8; 32], data),
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    hex
}
