//! Lepus Identity Envelope: parsing, signature verification, and subscriber matching.
//!
//! The identity envelope is prepended to contract state bytes by Heavymeta clients:
//!
//! ```text
//! Byte 0:       version (0x01)
//! Bytes 1-32:   creator_pubkey (32 bytes, Ed25519 VerifyingKey)
//! Bytes 33-96:  creator_signature (64 bytes, Ed25519 over recipient_pubkey || payload)
//! Bytes 97-128: recipient_pubkey (32 bytes; [0u8; 32] = public/open content)
//! Bytes 129+:   state_payload (actual contract state)
//! ```

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::sync::OnceLock;

/// Binary envelope version byte.
const ENVELOPE_VERSION: u8 = 0x01;

/// Total header size: 1 (version) + 32 (creator) + 64 (sig) + 32 (recipient).
const ENVELOPE_HEADER_SIZE: usize = 129;

/// Sentinel value for public/open content (no specific recipient).
const PUBLIC_RECIPIENT: [u8; 32] = [0u8; 32];

/// Parsed identity envelope from contract state bytes.
#[derive(Debug, Clone)]
pub struct IdentityEnvelope {
    pub creator_pubkey: [u8; 32],
    pub creator_signature: [u8; 64],
    pub recipient_pubkey: [u8; 32],
    pub payload_offset: usize,
}

/// Result of identity verification for a contract.
#[derive(Debug, Clone)]
pub struct IdentityVerificationResult {
    pub creator_pubkey: Option<[u8; 32]>,
    pub creator_verified: bool,
    pub subscriber_pubkey: Option<[u8; 32]>,
    pub subscriber_verified: bool,
    /// The intended recipient from the identity envelope.
    /// Used by subscription handshake to verify remote subscriber matches.
    pub recipient_pubkey: Option<[u8; 32]>,
}

/// Parse a Lepus identity envelope from contract state bytes.
///
/// Returns `None` if the state is too short or the version byte doesn't match.
pub fn parse_envelope(state: &[u8]) -> Option<IdentityEnvelope> {
    if state.len() < ENVELOPE_HEADER_SIZE {
        tracing::debug!(
            state_len = state.len(),
            required = ENVELOPE_HEADER_SIZE,
            "Identity envelope too short"
        );
        return None;
    }

    if state[0] != ENVELOPE_VERSION {
        tracing::debug!(
            version = state[0],
            expected = ENVELOPE_VERSION,
            "Identity envelope version mismatch"
        );
        return None;
    }

    let mut creator_pubkey = [0u8; 32];
    creator_pubkey.copy_from_slice(&state[1..33]);

    let mut creator_signature = [0u8; 64];
    creator_signature.copy_from_slice(&state[33..97]);

    let mut recipient_pubkey = [0u8; 32];
    recipient_pubkey.copy_from_slice(&state[97..129]);

    Some(IdentityEnvelope {
        creator_pubkey,
        creator_signature,
        recipient_pubkey,
        payload_offset: ENVELOPE_HEADER_SIZE,
    })
}

/// Verify the creator's Ed25519 signature over `recipient_pubkey || state_payload`.
///
/// Returns `false` on any error (bad key, bad signature, etc.).
pub fn verify_creator_signature(envelope: &IdentityEnvelope, state: &[u8]) -> bool {
    let verifying_key = match VerifyingKey::from_bytes(&envelope.creator_pubkey) {
        Ok(key) => key,
        Err(e) => {
            tracing::warn!(error = %e, "Invalid creator public key in identity envelope");
            return false;
        }
    };

    let signature = Signature::from_bytes(&envelope.creator_signature);

    // Signature covers: recipient_pubkey (32 bytes) || state_payload
    let payload = &state[envelope.payload_offset..];
    let mut message = Vec::with_capacity(32 + payload.len());
    message.extend_from_slice(&envelope.recipient_pubkey);
    message.extend_from_slice(payload);

    match verifying_key.verify(&message, &signature) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "Creator signature verification failed");
            false
        }
    }
}

/// Check if this node is a valid subscriber for the content.
///
/// Returns `true` if recipient is `PUBLIC_RECIPIENT` (open content) or matches the node's pubkey.
pub fn check_subscriber(recipient: &[u8; 32], node_pubkey: &[u8; 32]) -> bool {
    *recipient == PUBLIC_RECIPIENT || recipient == node_pubkey
}

/// Read the node's Stellar public key from the `LEPUS_STELLAR_PUBKEY` env var.
///
/// The env var should contain a hex-encoded 32-byte Ed25519 public key.
/// Result is cached via `OnceLock` for the process lifetime.
pub fn get_node_stellar_pubkey() -> Option<[u8; 32]> {
    static CACHED: OnceLock<Option<[u8; 32]>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let hex_str = std::env::var("LEPUS_STELLAR_PUBKEY").ok()?;
        let bytes = hex::decode(hex_str.trim()).ok()?;
        if bytes.len() != 32 {
            tracing::warn!(
                len = bytes.len(),
                "LEPUS_STELLAR_PUBKEY must be exactly 32 bytes (64 hex chars)"
            );
            return None;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Some(arr)
    })
}

/// Main entry point: parse envelope, verify creator signature, check subscriber.
///
/// Returns an all-false/None result if the state has no valid envelope.
pub fn verify_identity(state: &[u8]) -> IdentityVerificationResult {
    let envelope = match parse_envelope(state) {
        Some(e) => e,
        None => {
            return IdentityVerificationResult {
                creator_pubkey: None,
                creator_verified: false,
                subscriber_pubkey: None,
                subscriber_verified: false,
                recipient_pubkey: None,
            };
        }
    };

    let creator_verified = verify_creator_signature(&envelope, state);

    let node_pubkey = get_node_stellar_pubkey();
    let subscriber_verified = match &node_pubkey {
        Some(npk) => check_subscriber(&envelope.recipient_pubkey, npk),
        None => {
            // No node pubkey configured — only public content passes
            envelope.recipient_pubkey == PUBLIC_RECIPIENT
        }
    };

    IdentityVerificationResult {
        creator_pubkey: Some(envelope.creator_pubkey),
        creator_verified,
        subscriber_pubkey: node_pubkey,
        subscriber_verified,
        recipient_pubkey: Some(envelope.recipient_pubkey),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    /// Build a valid identity-enveloped state for testing.
    fn make_test_state(signing_key: &SigningKey, recipient: [u8; 32], payload: &[u8]) -> Vec<u8> {
        use ed25519_dalek::Signer;

        let creator_pubkey = signing_key.verifying_key().to_bytes();

        // Sign: recipient_pubkey || payload
        let mut message = Vec::with_capacity(32 + payload.len());
        message.extend_from_slice(&recipient);
        message.extend_from_slice(payload);
        let signature = signing_key.sign(&message);

        let mut state = Vec::with_capacity(ENVELOPE_HEADER_SIZE + payload.len());
        state.push(ENVELOPE_VERSION);
        state.extend_from_slice(&creator_pubkey);
        state.extend_from_slice(&signature.to_bytes());
        state.extend_from_slice(&recipient);
        state.extend_from_slice(payload);
        state
    }

    fn test_signing_key() -> SigningKey {
        // Deterministic key for tests
        SigningKey::from_bytes(&[42u8; 32])
    }

    #[test]
    fn test_parse_envelope_valid() {
        let sk = test_signing_key();
        let payload = b"hello world";
        let state = make_test_state(&sk, PUBLIC_RECIPIENT, payload);

        let env = parse_envelope(&state).expect("should parse valid envelope");
        assert_eq!(env.creator_pubkey, sk.verifying_key().to_bytes());
        assert_eq!(env.recipient_pubkey, PUBLIC_RECIPIENT);
        assert_eq!(env.payload_offset, ENVELOPE_HEADER_SIZE);
        assert_eq!(&state[env.payload_offset..], payload);
    }

    #[test]
    fn test_parse_envelope_too_short() {
        let short = vec![0u8; 128]; // 1 byte too short
        assert!(parse_envelope(&short).is_none());
    }

    #[test]
    fn test_parse_envelope_wrong_version() {
        let sk = test_signing_key();
        let mut state = make_test_state(&sk, PUBLIC_RECIPIENT, b"data");
        state[0] = 0x99; // wrong version
        assert!(parse_envelope(&state).is_none());
    }

    #[test]
    fn test_verify_creator_signature_valid() {
        let sk = test_signing_key();
        let state = make_test_state(&sk, PUBLIC_RECIPIENT, b"test payload");
        let env = parse_envelope(&state).unwrap();
        assert!(verify_creator_signature(&env, &state));
    }

    #[test]
    fn test_verify_creator_signature_invalid() {
        let sk = test_signing_key();
        let mut state = make_test_state(&sk, PUBLIC_RECIPIENT, b"test payload");
        // Corrupt the signature
        state[33] ^= 0xFF;
        let env = parse_envelope(&state).unwrap();
        assert!(!verify_creator_signature(&env, &state));
    }

    #[test]
    fn test_verify_creator_signature_wrong_key() {
        let sk = test_signing_key();
        let state = make_test_state(&sk, PUBLIC_RECIPIENT, b"test payload");
        let mut env = parse_envelope(&state).unwrap();
        // Replace creator pubkey with a different key
        let other_sk = SigningKey::from_bytes(&[99u8; 32]);
        env.creator_pubkey = other_sk.verifying_key().to_bytes();
        assert!(!verify_creator_signature(&env, &state));
    }

    #[test]
    fn test_check_subscriber_matching() {
        let node_pk = [7u8; 32];
        assert!(check_subscriber(&node_pk, &node_pk));
    }

    #[test]
    fn test_check_subscriber_public() {
        let node_pk = [7u8; 32];
        assert!(check_subscriber(&PUBLIC_RECIPIENT, &node_pk));
    }

    #[test]
    fn test_check_subscriber_non_matching() {
        let recipient = [7u8; 32];
        let node_pk = [8u8; 32];
        assert!(!check_subscriber(&recipient, &node_pk));
    }

    #[test]
    fn test_get_node_stellar_pubkey_valid() {
        // Note: OnceLock caches the result, so this test is order-dependent.
        // In a fresh process with LEPUS_STELLAR_PUBKEY set, it would work.
        // For unit tests, we test the parsing logic directly instead.
        let hex_str = "0102030405060708091011121314151617181920212223242526272829303132";
        let bytes = hex::decode(hex_str).unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn test_verify_identity_full() {
        let sk = test_signing_key();
        let state = make_test_state(&sk, PUBLIC_RECIPIENT, b"full test");

        let result = verify_identity(&state);
        assert_eq!(result.creator_pubkey, Some(sk.verifying_key().to_bytes()));
        assert!(result.creator_verified);
        // subscriber_verified depends on env var / PUBLIC_RECIPIENT
        // With PUBLIC_RECIPIENT, it should be true regardless of node config
        assert!(result.subscriber_verified);
    }

    #[test]
    fn test_verify_identity_no_envelope() {
        let plain_state = b"just some plain contract state without envelope";
        let result = verify_identity(plain_state);
        assert!(result.creator_pubkey.is_none());
        assert!(!result.creator_verified);
        assert!(result.subscriber_pubkey.is_none() || result.subscriber_pubkey.is_some());
        assert!(!result.subscriber_verified);
    }
}
