use std::fmt::Write;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::error::{NexusError, Result};
use crate::proof::{canonical_bytes, ProofCapsule, SignatureEnvelope, TypedDigest};

const SIGNER: &str = "nexus-hypervisor";

pub fn sign_capsule(mut capsule: ProofCapsule, key: &SigningKey) -> ProofCapsule {
    let payload =
        canonical_bytes(&capsule).expect("proof capsule canonical serialization must succeed");
    let signature = key.sign(&payload);
    let verifying_key = VerifyingKey::from(key);
    let key_id = hex_lower(verifying_key.as_bytes());

    capsule.signature = Some(SignatureEnvelope {
        signer: SIGNER.to_owned(),
        key_id,
        signature: hex_lower(&signature.to_bytes()),
        signed_payload_digest: TypedDigest::sha256_public(&payload),
    });

    capsule
}

pub fn verify_capsule(capsule: &ProofCapsule, vk: &VerifyingKey) -> Result<()> {
    let envelope = capsule.signature.as_ref().ok_or_else(|| {
        NexusError::InvalidCapability("proof capsule missing signature".to_owned())
    })?;

    let payload = canonical_bytes(capsule).map_err(|e| {
        NexusError::SerializationError(format!("proof capsule canonical serialization: {e}"))
    })?;
    let payload_digest = TypedDigest::sha256_public(&payload);
    if envelope.signed_payload_digest != payload_digest {
        return Err(NexusError::InvalidCapability(
            "proof capsule signed payload digest mismatch".to_owned(),
        ));
    }

    let expected_key_id = hex_lower(vk.as_bytes());
    if envelope.key_id != expected_key_id {
        return Err(NexusError::InvalidCapability(
            "proof capsule key id does not match verifying key".to_owned(),
        ));
    }

    let signature_bytes = decode_hex(&envelope.signature)?;
    let signature_array = <[u8; 64]>::try_from(signature_bytes.as_slice()).map_err(|_| {
        NexusError::InvalidCapability("proof capsule signature has invalid length".to_owned())
    })?;
    let signature = Signature::from_bytes(&signature_array);

    vk.verify(&payload, &signature).map_err(|_| {
        NexusError::InvalidCapability("proof capsule signature verification failed".to_owned())
    })
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    hex
}

fn decode_hex(input: &str) -> Result<Vec<u8>> {
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(NexusError::InvalidCapability(
            "proof capsule signature is not valid hex".to_owned(),
        ));
    }

    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        decoded.push((high << 4) | low);
    }

    Ok(decoded)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(NexusError::InvalidCapability(
            "proof capsule signature is not valid hex".to_owned(),
        )),
    }
}
