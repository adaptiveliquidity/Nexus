use serde_json::Value;

use crate::proof::{ProofCapsule, TypedDigest};

pub fn canonical_bytes(capsule: &ProofCapsule) -> Result<Vec<u8>, serde_json::Error> {
    let mut unsigned = capsule.clone();
    unsigned.signature = None;

    let mut value = serde_json::to_value(unsigned)?;
    sort_json_value(&mut value);

    serde_json::to_vec(&value)
}

pub fn capsule_digest(capsule: &ProofCapsule) -> Result<TypedDigest, serde_json::Error> {
    Ok(TypedDigest::sha256_public(&canonical_bytes(capsule)?))
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
