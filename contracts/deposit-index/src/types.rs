use freenet_stdlib::prelude::*;
use serde::{Deserialize, Serialize};

/// Validator organization for quorum checking.
/// Each org has multiple validators; org-level majority is checked first,
/// then org-level quorum threshold.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ValidatorOrg {
    /// Human-readable org name (e.g. "SDF", "Blockdaemon")
    pub name: String,
    /// Ed25519 public keys of this org's validators (hex 32 bytes each)
    pub validators: Vec<String>,
}

/// Contract parameters baked into the ContractKey — immutable for the life of the contract.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DepositIndexParams {
    /// SHA-256 of the Stellar network passphrase (hex 32 bytes)
    pub network_id: String,
    /// Tier 1 validators grouped by organization
    pub organizations: Vec<ValidatorOrg>,
    /// Minimum number of orgs that must have majority signing.
    /// 0 = default `(orgs.len() * 2 / 3) + 1`
    pub quorum_org_threshold: usize,
    /// hvym-freenet-service Soroban contract address (hex 32 bytes)
    pub hvym_contract_address: String,
}

/// A single deposit entry in the contract state.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DepositEntry {
    /// Freenet contract ID (hex 32 bytes)
    pub contract_id: String,
    /// Cumulative deposited amount (stroops)
    pub total_deposited: i128,
    /// Ledger sequence of the most recent deposit for this contract
    pub last_ledger: u32,
}

/// The full contract state: a versioned deposit map.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DepositMap {
    /// Incremented on every state change
    pub version: u64,
    /// Highest ledger sequence processed
    pub last_ledger_seq: u32,
    /// Sorted by contract_id (ascending)
    pub deposits: Vec<DepositEntry>,
}

/// Summary for delta computation.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DepositMapSummary {
    pub version: u64,
    pub entry_count: usize,
    pub last_ledger_seq: u32,
}

/// A proof submitted as UpdateData::Delta.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DepositProof {
    /// Ledger sequence number being proven
    pub ledger_seq: u32,
    /// SCP externalize envelopes (base64-encoded XDR)
    pub scp_envelopes: Vec<String>,
    /// The transaction set for this ledger (base64-encoded XDR)
    pub transaction_set: String,
    /// Transaction result metas containing events (base64-encoded XDR)
    pub tx_result_metas: Vec<String>,
}

/// Decode a hex string into bytes.
pub fn hex_decode(s: &str) -> Result<Vec<u8>, ContractError> {
    if !s.len().is_multiple_of(2) {
        return Err(ContractError::Deser("odd-length hex string".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| ContractError::Deser(e.to_string()))
        })
        .collect()
}

/// Decode a hex string into exactly 32 bytes.
pub fn hex_decode_32(s: &str) -> Result<[u8; 32], ContractError> {
    let bytes = hex_decode(s)?;
    bytes
        .try_into()
        .map_err(|_| ContractError::Deser("expected 32 bytes".into()))
}

/// Encode bytes as lowercase hex.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
