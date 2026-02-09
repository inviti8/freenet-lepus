//! Datapod contract for Lepus — validates identity envelopes and merges state.
//!
//! One WASM binary handles all datapods. Parameters encode the creator/recipient
//! pubkeys. State is the identity envelope (129-byte header + NINJS JSON payload).

use freenet_stdlib::prelude::*;
use serde::{Deserialize, Serialize};

/// Parameters baked into the ContractKey — same for the life of the contract.
#[derive(Serialize, Deserialize)]
struct DatapodParams {
    /// Creator's Ed25519 public key (32 bytes, hex-encoded)
    creator_pubkey: String,
    /// Intended recipient's Ed25519 public key (hex), or "00..00" for public
    recipient_pubkey: String,
}

/// Identity envelope header layout (matches identity.rs in freenet-lepus):
///   byte  0:      version (0x01)
///   bytes 1-32:   creator_pubkey (32 bytes)
///   bytes 33-96:  creator_signature (64 bytes)
///   bytes 97-128: recipient_pubkey (32 bytes)
///   bytes 129+:   payload (NINJS JSON)
const ENVELOPE_HEADER_SIZE: usize = 129;

/// Decode a hex string into bytes. Avoids pulling in the `hex` crate.
fn hex_decode(s: &str) -> Result<Vec<u8>, ContractError> {
    if s.len() % 2 != 0 {
        return Err(ContractError::Deser("odd-length hex string".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| ContractError::Deser(e.to_string()))
        })
        .collect()
}

pub struct Contract;

#[contract]
impl ContractInterface for Contract {
    fn validate_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        let bytes = state.as_ref();
        if bytes.is_empty() {
            return Ok(ValidateResult::Valid);
        }

        // Must have at least the envelope header
        if bytes.len() < ENVELOPE_HEADER_SIZE {
            return Ok(ValidateResult::Invalid);
        }

        // Parse parameters to get expected creator/recipient
        let params: DatapodParams = serde_json::from_slice(parameters.as_ref())
            .map_err(|e| ContractError::Deser(e.to_string()))?;

        // Verify envelope version
        if bytes[0] != 0x01 {
            return Ok(ValidateResult::Invalid);
        }

        // Extract envelope fields
        let creator_pubkey = &bytes[1..33];
        let signature = &bytes[33..97];
        let recipient_pubkey = &bytes[97..129];
        let payload = &bytes[129..];

        // Verify creator_pubkey matches parameters
        let expected_creator = hex_decode(&params.creator_pubkey)?;
        if creator_pubkey != expected_creator.as_slice() {
            return Ok(ValidateResult::Invalid);
        }

        // Verify recipient_pubkey matches parameters
        let expected_recipient = hex_decode(&params.recipient_pubkey)?;
        if recipient_pubkey != expected_recipient.as_slice() {
            return Ok(ValidateResult::Invalid);
        }

        // Verify Ed25519 signature: sign(recipient_pubkey || payload)
        let vk = ed25519_dalek::VerifyingKey::from_bytes(
            creator_pubkey
                .try_into()
                .map_err(|_| ContractError::Other("invalid creator pubkey length".into()))?,
        )
        .map_err(|e| ContractError::Other(e.to_string()))?;

        let sig = ed25519_dalek::Signature::from_bytes(
            signature
                .try_into()
                .map_err(|_| ContractError::Other("invalid signature length".into()))?,
        );

        // Message = recipient_pubkey || payload (matches identity.rs)
        let mut msg = Vec::with_capacity(32 + payload.len());
        msg.extend_from_slice(recipient_pubkey);
        msg.extend_from_slice(payload);

        use ed25519_dalek::Verifier;
        match vk.verify(&msg, &sig) {
            Ok(()) => Ok(ValidateResult::Valid),
            Err(_) => Ok(ValidateResult::Invalid),
        }
    }

    fn update_state(
        parameters: Parameters<'static>,
        _state: State<'static>,
        data: Vec<UpdateData<'static>>,
    ) -> Result<UpdateModification<'static>, ContractError> {
        // For datapods, an update replaces the entire state (new gallery version).
        // The newest valid state wins.
        for ud in data {
            let raw: Vec<u8> = match ud {
                UpdateData::State(s) if !s.is_empty() => s.into_bytes(),
                UpdateData::Delta(d) if !d.is_empty() => d.into_bytes(),
                UpdateData::StateAndDelta { state, .. } if !state.is_empty() => state.into_bytes(),
                _ => continue,
            };
            let new_state = State::from(raw);
            let result = Self::validate_state(
                parameters.clone(),
                new_state.clone(),
                RelatedContracts::new(),
            )?;
            if matches!(result, ValidateResult::Valid) {
                return Ok(UpdateModification::valid(new_state));
            }
        }
        Err(ContractError::InvalidUpdate)
    }

    fn summarize_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
    ) -> Result<StateSummary<'static>, ContractError> {
        if state.is_empty() {
            return Ok(StateSummary::from(vec![]));
        }
        // Datapods are small (~2 KB), so use the full state as the summary.
        Ok(StateSummary::from(state.as_ref().to_vec()))
    }

    fn get_state_delta(
        _parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<StateDelta<'static>, ContractError> {
        // If summary matches current state, no delta needed
        if state.as_ref() == summary.as_ref() {
            return Ok(StateDelta::from(vec![]));
        }
        // Otherwise, send the full state as the delta (datapods are small)
        Ok(StateDelta::from(state.as_ref().to_vec()))
    }
}
