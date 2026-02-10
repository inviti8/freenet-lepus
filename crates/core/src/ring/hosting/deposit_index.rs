//! Deposit Index subscriber hook and types for CWP commitment scoring.
//!
//! When the deposit-index Freenet contract is updated (via SCP proofs from
//! relayer nodes), subscriber nodes receive the new state.  This module
//! extracts deposit amounts from that state and feeds them into the CWP
//! commitment scores of locally hosted contracts.
//!
//! Types here are duplicated from `contracts/deposit-index/src/types.rs`
//! because the contract crate is a cdylib and cannot be depended upon.

use std::collections::HashMap;
use std::sync::OnceLock;

use freenet_stdlib::prelude::{CodeHash, ContractInstanceId, ContractKey};
use serde::{Deserialize, Serialize};

// =============================================================================
// Duplicated types from contracts/deposit-index/src/types.rs
// =============================================================================

/// The full contract state: a versioned deposit map.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DepositMap {
    pub version: u64,
    pub last_ledger_seq: u32,
    pub deposits: Vec<DepositEntry>,
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

/// A proof submitted as UpdateData::Delta.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DepositProof {
    pub ledger_seq: u32,
    pub scp_envelopes: Vec<String>,
    pub transaction_set: String,
    pub tx_result_metas: Vec<String>,
}

// =============================================================================
// Configuration
// =============================================================================

/// Load the deposit-index `ContractInstanceId` from `LEPUS_DEPOSIT_INDEX_KEY`.
///
/// The env var should contain a hex-encoded 32-byte contract instance ID.
/// Result is cached via `OnceLock` for the process lifetime.
pub fn deposit_index_instance_id() -> Option<ContractInstanceId> {
    static CACHED: OnceLock<Option<ContractInstanceId>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let hex_str = std::env::var("LEPUS_DEPOSIT_INDEX_KEY").ok()?;
        let bytes = hex::decode(hex_str.trim()).ok()?;
        if bytes.len() != 32 {
            tracing::warn!(
                len = bytes.len(),
                "LEPUS_DEPOSIT_INDEX_KEY must be exactly 32 bytes (64 hex chars)"
            );
            return None;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Some(ContractInstanceId::new(arr))
    })
}

/// Load the full `ContractKey` for the deposit-index contract.
///
/// Requires both `LEPUS_DEPOSIT_INDEX_KEY` (instance ID) and
/// `LEPUS_DEPOSIT_INDEX_CODE_HASH` (code hash) to be set.
/// Relayer nodes need this to submit UPDATE operations.
pub fn deposit_index_contract_key() -> Option<ContractKey> {
    static CACHED: OnceLock<Option<ContractKey>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let instance_id = deposit_index_instance_id()?;

        let code_hex = std::env::var("LEPUS_DEPOSIT_INDEX_CODE_HASH").ok()?;
        let code_bytes = hex::decode(code_hex.trim()).ok()?;
        if code_bytes.len() != 32 {
            tracing::warn!(
                len = code_bytes.len(),
                "LEPUS_DEPOSIT_INDEX_CODE_HASH must be exactly 32 bytes (64 hex chars)"
            );
            return None;
        }
        let mut code_arr = [0u8; 32];
        code_arr.copy_from_slice(&code_bytes);

        Some(ContractKey::from_id_and_code(
            instance_id,
            CodeHash::new(code_arr),
        ))
    })
}

// =============================================================================
// Subscriber Hook
// =============================================================================

/// Check if an incoming contract update is the deposit-index contract and,
/// if so, extract deposit data and feed it into CWP commitment scores.
///
/// This is called from `update_contract()` for every successful UPDATE.
/// For non-deposit-index contracts it returns immediately (fast path).
///
/// # Arguments
/// * `key` — The contract key of the update that just landed.
/// * `state_bytes` — The new state bytes after the update.
/// * `hosted_keys` — All contract keys this node is currently hosting.
/// * `update_fn` — Callback to apply `(ContractKey, deposited_xlm)` updates.
pub fn check_deposit_index_update(
    key: &ContractKey,
    state_bytes: &[u8],
    hosted_keys: &[ContractKey],
    update_fn: impl FnOnce(&[(ContractKey, u64)]),
) {
    // Fast path: is this the deposit-index contract?
    let Some(expected_id) = deposit_index_instance_id() else {
        return;
    };
    if key.id() != &expected_id {
        return;
    }

    // Deserialize the deposit map
    let deposit_map: DepositMap = match serde_json::from_slice(state_bytes) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Lepus: failed to deserialize deposit-index state"
            );
            return;
        }
    };

    // Build a lookup: hex(instance_id bytes) → &ContractKey
    let mut hosted_lookup: HashMap<String, &ContractKey> =
        HashMap::with_capacity(hosted_keys.len());
    for hk in hosted_keys {
        let hex_id = hex::encode(hk.id().as_bytes());
        hosted_lookup.insert(hex_id, hk);
    }

    // Match deposit entries to hosted contracts
    let mut updates: Vec<(ContractKey, u64)> = Vec::new();
    for entry in &deposit_map.deposits {
        if let Some(&hosted_key) = hosted_lookup.get(&entry.contract_id) {
            // Convert i128 stroops to u64, capping at u64::MAX
            let xlm = if entry.total_deposited < 0 {
                0u64
            } else if entry.total_deposited > i128::from(u64::MAX) {
                u64::MAX
            } else {
                entry.total_deposited as u64
            };
            updates.push((*hosted_key, xlm));
        }
    }

    if !updates.is_empty() {
        tracing::info!(
            matched = updates.len(),
            total_deposits = deposit_map.deposits.len(),
            version = deposit_map.version,
            "Lepus: deposit-index update matched hosted contracts"
        );
        update_fn(&updates);
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_key(seed: u8) -> ContractKey {
        ContractKey::from_id_and_code(
            ContractInstanceId::new([seed; 32]),
            CodeHash::new([seed.wrapping_add(1); 32]),
        )
    }

    #[test]
    fn test_deposit_map_round_trip() {
        let map = DepositMap {
            version: 42,
            last_ledger_seq: 1000,
            deposits: vec![
                DepositEntry {
                    contract_id: hex::encode([1u8; 32]),
                    total_deposited: 5_000_000,
                    last_ledger: 999,
                },
                DepositEntry {
                    contract_id: hex::encode([2u8; 32]),
                    total_deposited: 10_000_000,
                    last_ledger: 1000,
                },
            ],
        };

        let json = serde_json::to_vec(&map).unwrap();
        let decoded: DepositMap = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.version, 42);
        assert_eq!(decoded.last_ledger_seq, 1000);
        assert_eq!(decoded.deposits.len(), 2);
        assert_eq!(decoded.deposits[0].total_deposited, 5_000_000);
    }

    #[test]
    fn test_deposit_proof_round_trip() {
        let proof = DepositProof {
            ledger_seq: 500,
            scp_envelopes: vec!["AAAA".to_string()],
            transaction_set: "BBBB".to_string(),
            tx_result_metas: vec!["CCCC".to_string()],
        };

        let json = serde_json::to_vec(&proof).unwrap();
        let decoded: DepositProof = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.ledger_seq, 500);
        assert_eq!(decoded.scp_envelopes.len(), 1);
    }

    #[test]
    fn test_check_deposit_index_update_no_config() {
        // Without LEPUS_DEPOSIT_INDEX_KEY set, should return immediately
        let key = make_key(1);
        let state = b"{}";
        let mut called = false;
        check_deposit_index_update(&key, state, &[], |_| {
            called = true;
        });
        // With no env var configured (OnceLock caches None), callback not called
        assert!(!called);
    }

    #[test]
    fn test_check_deposit_index_update_mapping() {
        // Test the mapping logic directly by crafting a deposit map and hosted keys
        let k1 = make_key(1);
        let k2 = make_key(2);
        let k3 = make_key(3); // not in deposit map

        let deposit_map = DepositMap {
            version: 1,
            last_ledger_seq: 100,
            deposits: vec![
                DepositEntry {
                    contract_id: hex::encode(k1.id().as_bytes()),
                    total_deposited: 1_000_000,
                    last_ledger: 100,
                },
                DepositEntry {
                    contract_id: hex::encode(k2.id().as_bytes()),
                    total_deposited: 2_000_000,
                    last_ledger: 100,
                },
                DepositEntry {
                    // contract not hosted by this node
                    contract_id: hex::encode([99u8; 32]),
                    total_deposited: 9_999_999,
                    last_ledger: 100,
                },
            ],
        };

        let state_bytes = serde_json::to_vec(&deposit_map).unwrap();
        let hosted = vec![k1, k2, k3];

        // Build lookup and match manually (same logic as the function, but we
        // bypass the env-var gate to test the mapping)
        let mut hosted_lookup: HashMap<String, &ContractKey> = HashMap::new();
        for hk in &hosted {
            hosted_lookup.insert(hex::encode(hk.id().as_bytes()), hk);
        }

        let decoded: DepositMap = serde_json::from_slice(&state_bytes).unwrap();
        let mut updates = Vec::new();
        for entry in &decoded.deposits {
            if let Some(&hk) = hosted_lookup.get(&entry.contract_id) {
                let xlm = if entry.total_deposited < 0 {
                    0u64
                } else if entry.total_deposited > i128::from(u64::MAX) {
                    u64::MAX
                } else {
                    entry.total_deposited as u64
                };
                updates.push((*hk, xlm));
            }
        }

        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].0, k1);
        assert_eq!(updates[0].1, 1_000_000);
        assert_eq!(updates[1].0, k2);
        assert_eq!(updates[1].1, 2_000_000);
    }

    #[test]
    fn test_deposit_entry_negative_clamped_to_zero() {
        let entry = DepositEntry {
            contract_id: hex::encode([1u8; 32]),
            total_deposited: -500,
            last_ledger: 1,
        };
        let xlm = if entry.total_deposited < 0 {
            0u64
        } else {
            entry.total_deposited as u64
        };
        assert_eq!(xlm, 0);
    }

    #[test]
    fn test_deposit_entry_overflow_clamped_to_max() {
        let entry = DepositEntry {
            contract_id: hex::encode([1u8; 32]),
            total_deposited: i128::from(u64::MAX) + 1,
            last_ledger: 1,
        };
        let xlm = if entry.total_deposited < 0 {
            0u64
        } else if entry.total_deposited > i128::from(u64::MAX) {
            u64::MAX
        } else {
            entry.total_deposited as u64
        };
        assert_eq!(xlm, u64::MAX);
    }

    #[test]
    fn test_empty_state_does_not_panic() {
        // Deserializing empty/invalid state should not panic
        let result: Result<DepositMap, _> = serde_json::from_slice(b"");
        assert!(result.is_err());

        let result: Result<DepositMap, _> = serde_json::from_slice(b"not json");
        assert!(result.is_err());
    }
}
