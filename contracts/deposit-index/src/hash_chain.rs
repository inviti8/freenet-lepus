use freenet_stdlib::prelude::*;
use sha2::{Digest, Sha256};
use stellar_xdr::curr::{GeneralizedTransactionSet, Limits, ReadXdr, WriteXdr};

/// Decode a base64-encoded generalized transaction set and verify its hash
/// matches the `tx_set_hash` from the SCP consensus value.
pub fn verify_tx_set_hash(
    b64_tx_set: &str,
    expected_hash: &[u8; 32],
) -> Result<GeneralizedTransactionSet, ContractError> {
    let tx_set_bytes = base64::decode(b64_tx_set)
        .map_err(|e| ContractError::Deser(format!("base64 decode tx set: {e}")))?;

    let tx_set = GeneralizedTransactionSet::from_xdr(&tx_set_bytes, Limits::none())
        .map_err(|e| ContractError::Deser(format!("XDR decode GeneralizedTransactionSet: {e}")))?;

    // Re-serialize to canonical XDR for hashing
    let canonical_xdr = tx_set
        .to_xdr(Limits::none())
        .map_err(|e| ContractError::Other(format!("XDR encode tx set: {e}")))?;

    let mut hasher = Sha256::new();
    hasher.update(&canonical_xdr);
    let computed: [u8; 32] = hasher.finalize().into();

    if computed != *expected_hash {
        return Err(ContractError::Other(
            "tx_set_hash mismatch: computed hash does not match consensus value".into(),
        ));
    }

    Ok(tx_set)
}
