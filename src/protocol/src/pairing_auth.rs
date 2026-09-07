use data_encoding::HEXLOWER;
use iroh::{EndpointId, SecretKey, Signature};

use crate::control::AUTH_LABEL;

pub fn device_auth_challenge(keying_material: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(AUTH_LABEL.len() + keying_material.len());
    out.extend_from_slice(AUTH_LABEL);
    out.extend_from_slice(keying_material);
    out
}

pub fn sign_challenge(secret: &SecretKey, keying_material: &[u8]) -> String {
    let challenge = device_auth_challenge(keying_material);
    let sig = secret.sign(&challenge);
    HEXLOWER.encode(&sig.to_bytes())
}

pub fn verify_challenge(
    endpoint_id: &EndpointId,
    keying_material: &[u8],
    signature_hex: &str,
) -> bool {
    let Ok(sig_bytes) = HEXLOWER.decode(signature_hex.as_bytes()) else {
        return false;
    };
    if sig_bytes.len() != Signature::LENGTH {
        return false;
    }
    let mut sig_array = [0u8; Signature::LENGTH];
    sig_array.copy_from_slice(&sig_bytes);
    let signature = Signature::from_bytes(&sig_array);
    let challenge = device_auth_challenge(keying_material);
    endpoint_id.verify(&challenge, &signature).is_ok()
}

pub fn export_connection_keying_material(
    conn: &iroh::endpoint::Connection,
) -> anyhow::Result<Vec<u8>> {
    let mut out = [0u8; 32];
    conn.export_keying_material(&mut out, AUTH_LABEL, b"")
        .map_err(|e| anyhow::anyhow!("export keying material: {e:?}"))?;
    Ok(out.to_vec())
}
