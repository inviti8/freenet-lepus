use crate::types::hex_encode;
use freenet_stdlib::prelude::*;
use stellar_xdr::curr::{
    ContractEvent, ContractEventBody, ContractEventType, Int128Parts, Limits, ReadXdr, ScVal,
    TransactionMeta, TransactionResultMeta,
};

/// A deposit event extracted from transaction metadata.
#[derive(Debug, Clone)]
pub struct ExtractedDeposit {
    /// Freenet contract ID (hex 32 bytes)
    pub contract_id: String,
    /// Amount in stroops
    pub amount: i128,
    /// Ledger sequence where the event was emitted
    pub ledger_seq: u32,
}

/// Decode base64-encoded TransactionResultMeta entries and extract DEPOSIT events
/// that match the given hvym contract address.
pub fn extract_deposits(
    b64_metas: &[String],
    hvym_contract_addr: &[u8; 32],
    ledger_seq: u32,
) -> Result<Vec<ExtractedDeposit>, ContractError> {
    let mut deposits = Vec::new();

    for b64 in b64_metas {
        let meta_bytes = base64::decode(b64)
            .map_err(|e| ContractError::Deser(format!("base64 decode tx result meta: {e}")))?;

        let result_meta = TransactionResultMeta::from_xdr(meta_bytes, Limits::none())
            .map_err(|e| ContractError::Deser(format!("XDR decode TransactionResultMeta: {e}")))?;

        // Extract events from the transaction meta
        let events = extract_events_from_meta(&result_meta.tx_apply_processing);

        for event in events {
            if let Some(deposit) = try_extract_deposit(event, hvym_contract_addr, ledger_seq) {
                deposits.push(deposit);
            }
        }
    }

    Ok(deposits)
}

/// Extract ContractEvent references from TransactionMeta.
fn extract_events_from_meta(meta: &TransactionMeta) -> Vec<&ContractEvent> {
    match meta {
        TransactionMeta::V3(v3) => {
            // Soroban events are in soroban_meta.events
            if let Some(ref soroban) = v3.soroban_meta {
                soroban.events.iter().collect()
            } else {
                Vec::new()
            }
        }
        // V0/V1/V2 don't have Soroban events
        _ => Vec::new(),
    }
}

/// Try to extract a DEPOSIT event from a ContractEvent.
///
/// Matches events where:
/// - type == Contract
/// - contract_id == hvym_contract_address
/// - topics[0] == Symbol("DEPOSIT")
/// - topics[1] == Bytes(freenet_contract_id)
/// - data is a tuple containing amount (i128) and ledger_seq (u32)
fn try_extract_deposit(
    event: &ContractEvent,
    hvym_contract_addr: &[u8; 32],
    ledger_seq: u32,
) -> Option<ExtractedDeposit> {
    // Must be a Contract event type
    if event.type_ != ContractEventType::Contract {
        return None;
    }

    // Must match hvym contract address
    let event_contract_id = event.contract_id.as_ref()?;
    if event_contract_id.0 .0 != *hvym_contract_addr {
        return None;
    }

    // Extract topics and data from the event body
    let ContractEventBody::V0(ref v0) = event.body;

    let topics = &v0.topics;
    if topics.len() < 2 {
        return None;
    }

    // topics[0] must be Symbol("DEPOSIT")
    match &topics[0] {
        ScVal::Symbol(sym) => {
            let sym_bytes: &[u8] = sym.as_ref();
            if sym_bytes != b"DEPOSIT" {
                return None;
            }
        }
        _ => return None,
    }

    // topics[1] is the Freenet contract ID (as Bytes)
    let freenet_contract_id = match &topics[1] {
        ScVal::Bytes(bytes) => {
            let b: &[u8] = bytes.as_ref();
            if b.len() != 32 {
                return None;
            }
            hex_encode(b)
        }
        _ => return None,
    };

    // data is a tuple: (depositor: Address, amount: i128, burned: i128, ledger: u32)
    // We care about `amount` (index 1 in the tuple)
    let amount = extract_amount_from_data(&v0.data)?;

    Some(ExtractedDeposit {
        contract_id: freenet_contract_id,
        amount,
        ledger_seq,
    })
}

/// Extract the deposit amount from the event data.
///
/// The event data from hvym-freenet-service `deposit()` is:
/// `(caller: Address, amount: i128, burn_amount: i128, ledger_seq: u32)`
///
/// In Soroban, tuples are encoded as ScVal::Vec.
fn extract_amount_from_data(data: &ScVal) -> Option<i128> {
    match data {
        ScVal::Vec(Some(vec)) => {
            // Element at index 1 is the total amount
            let items: &[ScVal] = vec.as_ref();
            if items.len() < 2 {
                return None;
            }
            match &items[1] {
                ScVal::I128(parts) => Some(i128_from_parts(parts)),
                _ => None,
            }
        }
        // Single i128 value (simpler format)
        ScVal::I128(parts) => Some(i128_from_parts(parts)),
        _ => None,
    }
}

/// Convert Int128Parts to i128.
fn i128_from_parts(parts: &Int128Parts) -> i128 {
    ((parts.hi as i128) << 64) | (parts.lo as i128)
}
