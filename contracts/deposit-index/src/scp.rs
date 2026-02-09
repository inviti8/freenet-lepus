use crate::types::{DepositIndexParams, ValidatorOrg};
use ed25519_dalek::{Signature, VerifyingKey};
use freenet_stdlib::prelude::*;
use stellar_xdr::curr::{
    EnvelopeType, Limits, ReadXdr, ScpEnvelope, ScpStatementPledges, StellarValue, WriteXdr,
};

/// Decode base64-encoded XDR SCP envelopes.
pub fn decode_envelopes(
    b64_envelopes: &[String],
) -> Result<Vec<ScpEnvelope>, ContractError> {
    b64_envelopes
        .iter()
        .map(|b64| {
            let bytes = base64::decode(b64)
                .map_err(|e| ContractError::Deser(format!("base64 decode: {e}")))?;
            ScpEnvelope::from_xdr(bytes, Limits::none())
                .map_err(|e| ContractError::Deser(format!("XDR decode ScpEnvelope: {e}")))
        })
        .collect()
}

/// Verify an SCP envelope's Ed25519 signature.
///
/// The signed message is: `network_id(32) || ENVELOPE_TYPE_SCP(4 bytes) || XDR(statement)`.
pub fn verify_envelope_signature(
    envelope: &ScpEnvelope,
    network_id: &[u8; 32],
) -> Result<[u8; 32], ContractError> {
    // Extract the signer's public key from NodeId(PublicKey::PublicKeyTypeEd25519(Uint256))
    let stellar_xdr::curr::PublicKey::PublicKeyTypeEd25519(ref pk_bytes) =
        envelope.statement.node_id.0;

    let signer_bytes: [u8; 32] = pk_bytes.0;

    let vk = VerifyingKey::from_bytes(&signer_bytes)
        .map_err(|e| ContractError::Other(format!("invalid validator pubkey: {e}")))?;

    // Build the signed message: network_id || envelope_type_scp || xdr(statement)
    let envelope_type_scp = EnvelopeType::Scp
        .to_xdr(Limits::none())
        .map_err(|e| ContractError::Other(format!("XDR encode envelope type: {e}")))?;

    let statement_xdr = envelope
        .statement
        .to_xdr(Limits::none())
        .map_err(|e| ContractError::Other(format!("XDR encode statement: {e}")))?;

    let mut msg = Vec::with_capacity(32 + 4 + statement_xdr.len());
    msg.extend_from_slice(network_id);
    msg.extend_from_slice(&envelope_type_scp);
    msg.extend_from_slice(&statement_xdr);

    // Extract the 64-byte signature
    let sig_bytes: &[u8] = envelope.signature.as_ref();
    let sig_array: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| ContractError::Other("signature not 64 bytes".into()))?;
    let sig = Signature::from_bytes(&sig_array);

    use ed25519_dalek::Verifier;
    vk.verify(&msg, &sig)
        .map_err(|_| ContractError::Other("envelope signature verification failed".into()))?;

    Ok(signer_bytes)
}

/// Extract the consensus StellarValue from an externalize statement's commit ballot.
pub fn extract_consensus_value(
    envelope: &ScpEnvelope,
) -> Result<StellarValue, ContractError> {
    match &envelope.statement.pledges {
        ScpStatementPledges::Externalize(ext) => {
            // The ballot value is opaque bytes that encode a StellarValue
            let value_bytes: &[u8] = ext.commit.value.as_ref();
            StellarValue::from_xdr(value_bytes, Limits::none())
                .map_err(|e| ContractError::Deser(format!("XDR decode StellarValue: {e}")))
        }
        _ => Err(ContractError::Other(
            "envelope is not an externalize statement".into(),
        )),
    }
}

/// Check that a quorum of validators signed the same consensus value.
///
/// Per-org majority (>1/2 validators signed), then org threshold (default >2/3 of orgs).
/// Returns the agreed-upon StellarValue if quorum is met.
pub fn check_quorum(
    envelopes: &[ScpEnvelope],
    params: &DepositIndexParams,
    network_id: &[u8; 32],
) -> Result<StellarValue, ContractError> {
    if envelopes.is_empty() {
        return Err(ContractError::Other("no SCP envelopes provided".into()));
    }

    // Collect (signer_pubkey, consensus_value_hash) for valid envelopes
    let mut valid_signers: Vec<([u8; 32], [u8; 32])> = Vec::new();

    for envelope in envelopes {
        // Only process externalize statements
        if !matches!(
            envelope.statement.pledges,
            ScpStatementPledges::Externalize(_)
        ) {
            continue;
        }

        // Verify the signature; skip invalid ones
        let signer = match verify_envelope_signature(envelope, network_id) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Extract the tx_set_hash as the "value" they agreed on
        let stellar_value = match extract_consensus_value(envelope) {
            Ok(v) => v,
            Err(_) => continue,
        };

        valid_signers.push((signer, stellar_value.tx_set_hash.0));
    }

    if valid_signers.is_empty() {
        return Err(ContractError::Other(
            "no valid externalize signatures found".into(),
        ));
    }

    // Find the most common tx_set_hash (there should be exactly one in practice)
    let consensus_hash = valid_signers[0].1;
    // Verify all valid signers agree on the same hash
    for (_, hash) in &valid_signers {
        if hash != &consensus_hash {
            return Err(ContractError::Other(
                "validators disagree on consensus value".into(),
            ));
        }
    }

    // Check per-org majority
    let threshold = if params.quorum_org_threshold == 0 {
        (params.organizations.len() * 2 / 3) + 1
    } else {
        params.quorum_org_threshold
    };

    let mut orgs_with_majority = 0;
    for org in &params.organizations {
        let org_signer_count = count_org_signers(org, &valid_signers);
        let majority = (org.validators.len() / 2) + 1;
        if org_signer_count >= majority {
            orgs_with_majority += 1;
        }
    }

    if orgs_with_majority < threshold {
        return Err(ContractError::Other(format!(
            "insufficient quorum: {orgs_with_majority} orgs signed, need {threshold}"
        )));
    }

    // Re-extract the full StellarValue from the first valid envelope
    for envelope in envelopes {
        if let ScpStatementPledges::Externalize(_) = &envelope.statement.pledges {
            if let Ok(sv) = extract_consensus_value(envelope) {
                if sv.tx_set_hash.0 == consensus_hash {
                    return Ok(sv);
                }
            }
        }
    }

    Err(ContractError::Other(
        "failed to extract consensus value".into(),
    ))
}

/// Count how many of an org's validators appear in the valid signers list.
fn count_org_signers(
    org: &ValidatorOrg,
    valid_signers: &[([u8; 32], [u8; 32])],
) -> usize {
    let mut count = 0;
    for validator_hex in &org.validators {
        if let Ok(vk_bytes) = crate::types::hex_decode_32(validator_hex) {
            if valid_signers.iter().any(|(signer, _)| signer == &vk_bytes) {
                count += 1;
            }
        }
    }
    count
}
