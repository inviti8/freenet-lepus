//! Deposit Index — Freenet WASM contract that verifies Stellar SCP proofs
//! and maintains a map of `{ freenet_contract_id → total_deposited_xlm }`.
//!
//! Relaying nodes submit SCP proofs as contract updates; all subscribing nodes
//! receive the verified deposit map via normal Freenet state sync.

mod events;
mod hash_chain;
mod scp;
mod types;

use freenet_stdlib::prelude::*;
use types::{DepositEntry, DepositIndexParams, DepositMap, DepositMapSummary, DepositProof};

pub struct Contract;

#[contract]
impl ContractInterface for Contract {
    fn validate_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        let bytes = state.as_ref();
        if bytes.is_empty() {
            return Ok(ValidateResult::Valid);
        }

        let map: DepositMap = serde_json::from_slice(bytes)
            .map_err(|e| ContractError::Deser(e.to_string()))?;

        // Verify sorted by contract_id, no duplicates
        for i in 1..map.deposits.len() {
            if map.deposits[i].contract_id <= map.deposits[i - 1].contract_id {
                return Ok(ValidateResult::Invalid);
            }
        }

        // Verify no negative amounts and valid hex IDs
        for entry in &map.deposits {
            if entry.total_deposited < 0 {
                return Ok(ValidateResult::Invalid);
            }
            if entry.contract_id.len() != 64 {
                return Ok(ValidateResult::Invalid);
            }
            if types::hex_decode_32(&entry.contract_id).is_err() {
                return Ok(ValidateResult::Invalid);
            }
        }

        Ok(ValidateResult::Valid)
    }

    fn update_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        data: Vec<UpdateData<'static>>,
    ) -> Result<UpdateModification<'static>, ContractError> {
        let params: DepositIndexParams = serde_json::from_slice(parameters.as_ref())
            .map_err(|e| ContractError::Deser(format!("params: {e}")))?;

        let mut map: DepositMap = if state.as_ref().is_empty() {
            DepositMap::default()
        } else {
            serde_json::from_slice(state.as_ref())
                .map_err(|e| ContractError::Deser(format!("state: {e}")))?
        };

        let network_id = types::hex_decode_32(&params.network_id)
            .map_err(|e| ContractError::Deser(format!("network_id: {e}")))?;

        let hvym_addr = types::hex_decode_32(&params.hvym_contract_address)
            .map_err(|e| ContractError::Deser(format!("hvym_contract_address: {e}")))?;

        let mut changed = false;

        for ud in data {
            match ud {
                UpdateData::Delta(delta) => {
                    let proof: DepositProof = serde_json::from_slice(delta.as_ref())
                        .map_err(|e| ContractError::Deser(format!("proof: {e}")))?;

                    if let Ok(did_change) =
                        apply_proof(&proof, &params, &network_id, &hvym_addr, &mut map)
                    {
                        if did_change {
                            changed = true;
                        }
                    }
                    // Invalid proofs are silently skipped (not an error)
                }
                UpdateData::State(new_state_data) if !new_state_data.is_empty() => {
                    // Full state replacement (network sync): accept if higher version
                    let incoming: DepositMap =
                        serde_json::from_slice(new_state_data.as_ref())
                            .map_err(|e| ContractError::Deser(format!("incoming state: {e}")))?;
                    if incoming.version > map.version {
                        map = incoming;
                        changed = true;
                    }
                }
                _ => {}
            }
        }

        if changed {
            map.version += 1;
        }

        let new_bytes =
            serde_json::to_vec(&map).map_err(|e| ContractError::Other(e.to_string()))?;
        Ok(UpdateModification::valid(State::from(new_bytes)))
    }

    fn summarize_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
    ) -> Result<StateSummary<'static>, ContractError> {
        if state.as_ref().is_empty() {
            let summary = DepositMapSummary {
                version: 0,
                entry_count: 0,
                last_ledger_seq: 0,
            };
            let bytes =
                serde_json::to_vec(&summary).map_err(|e| ContractError::Other(e.to_string()))?;
            return Ok(StateSummary::from(bytes));
        }

        let map: DepositMap = serde_json::from_slice(state.as_ref())
            .map_err(|e| ContractError::Deser(e.to_string()))?;

        let summary = DepositMapSummary {
            version: map.version,
            entry_count: map.deposits.len(),
            last_ledger_seq: map.last_ledger_seq,
        };

        let bytes =
            serde_json::to_vec(&summary).map_err(|e| ContractError::Other(e.to_string()))?;
        Ok(StateSummary::from(bytes))
    }

    fn get_state_delta(
        _parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<StateDelta<'static>, ContractError> {
        let summary_data: DepositMapSummary = serde_json::from_slice(summary.as_ref())
            .map_err(|e| ContractError::Deser(e.to_string()))?;

        if state.as_ref().is_empty() && summary_data.version == 0 {
            return Ok(StateDelta::from(Vec::new()));
        }

        let map: DepositMap = serde_json::from_slice(state.as_ref())
            .map_err(|e| ContractError::Deser(e.to_string()))?;

        if map.version == summary_data.version {
            return Ok(StateDelta::from(Vec::new()));
        }

        // Different versions → send full state as delta (deposit maps are small)
        Ok(StateDelta::from(state.as_ref().to_vec()))
    }
}

/// Apply a single deposit proof to the map. Returns whether the map changed.
fn apply_proof(
    proof: &DepositProof,
    params: &DepositIndexParams,
    network_id: &[u8; 32],
    hvym_addr: &[u8; 32],
    map: &mut DepositMap,
) -> Result<bool, ContractError> {
    // Skip already-processed ledgers
    if proof.ledger_seq <= map.last_ledger_seq {
        return Ok(false);
    }

    // Stage 1: Decode SCP envelopes
    let envelopes = scp::decode_envelopes(&proof.scp_envelopes)?;

    // Stage 2+3: Verify signatures and check quorum
    let stellar_value = scp::check_quorum(&envelopes, params, network_id)?;

    // Stage 4: Verify tx_set_hash matches consensus value
    let _tx_set =
        hash_chain::verify_tx_set_hash(&proof.transaction_set, &stellar_value.tx_set_hash.0)?;

    // Stage 5: Extract DEPOSIT events from transaction result metas
    let deposits = events::extract_deposits(&proof.tx_result_metas, hvym_addr, proof.ledger_seq)?;

    if deposits.is_empty() {
        // Valid proof but no deposits in this ledger — update ledger tracking
        map.last_ledger_seq = proof.ledger_seq;
        return Ok(true);
    }

    // Merge deposits additively (monotonic: amounts only increase)
    for deposit in deposits {
        merge_deposit(map, deposit.contract_id, deposit.amount, deposit.ledger_seq);
    }

    map.last_ledger_seq = proof.ledger_seq;
    Ok(true)
}

/// Merge a deposit into the map. Amounts are cumulative (additive).
fn merge_deposit(map: &mut DepositMap, contract_id: String, amount: i128, ledger_seq: u32) {
    match map
        .deposits
        .binary_search_by(|e| e.contract_id.cmp(&contract_id))
    {
        Ok(idx) => {
            // Existing entry: add amount (monotonic)
            map.deposits[idx].total_deposited += amount;
            if ledger_seq > map.deposits[idx].last_ledger {
                map.deposits[idx].last_ledger = ledger_seq;
            }
        }
        Err(idx) => {
            // New entry: insert at sorted position
            map.deposits.insert(
                idx,
                DepositEntry {
                    contract_id,
                    total_deposited: amount,
                    last_ledger: ledger_seq,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests;
