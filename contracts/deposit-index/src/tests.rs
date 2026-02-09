use super::Contract as DepositContract;
use crate::scp;
use crate::types::{
    hex_encode, DepositEntry, DepositIndexParams, DepositMap, DepositMapSummary, DepositProof,
    ValidatorOrg,
};
use ed25519_dalek::{Signer, SigningKey};
use freenet_stdlib::prelude::*;
use sha2::{Digest, Sha256};
use stellar_xdr::curr::{
    ContractEvent, ContractEventBody, ContractEventType, ContractEventV0, ContractId,
    EnvelopeType, ExtensionPoint, GeneralizedTransactionSet, Hash, Int128Parts,
    LedgerEntryChanges, Limits, NodeId, PublicKey, ScVal, ScpBallot, ScpEnvelope, ScpStatement,
    ScpStatementExternalize, ScpStatementPledges, SorobanTransactionMeta,
    SorobanTransactionMetaExt, StellarValue, StellarValueExt, TransactionMeta, TransactionMetaV3,
    TransactionResultMeta, TransactionResultPair, Uint256, Value, VecM, WriteXdr,
};

// --- Test helpers ---

const NETWORK_PASSPHRASE: &str = "Test SDF Network ; September 2015";
const SLOT_INDEX: u64 = 100;

fn test_network_id() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(NETWORK_PASSPHRASE.as_bytes());
    hasher.finalize().into()
}

fn test_network_id_hex() -> String {
    hex_encode(&test_network_id())
}

fn make_keypair(seed: u8) -> SigningKey {
    let mut secret = [0u8; 32];
    secret[0] = seed;
    SigningKey::from_bytes(&secret)
}

fn make_hvym_address() -> [u8; 32] {
    let mut addr = [0u8; 32];
    addr[0] = 0xAA;
    addr[1] = 0xBB;
    addr
}

fn make_hvym_address_hex() -> String {
    hex_encode(&make_hvym_address())
}

fn make_freenet_contract_id() -> [u8; 32] {
    let mut id = [0u8; 32];
    id[0] = 0x01;
    id[1] = 0x02;
    id[2] = 0x03;
    id
}

fn make_freenet_contract_id_hex() -> String {
    hex_encode(&make_freenet_contract_id())
}

fn make_stellar_value(tx_set_hash: [u8; 32]) -> StellarValue {
    StellarValue {
        tx_set_hash: Hash(tx_set_hash),
        close_time: stellar_xdr::curr::TimePoint(1000),
        upgrades: VecM::default(),
        ext: StellarValueExt::Basic,
    }
}

fn make_signed_envelope(
    signing_key: &SigningKey,
    stellar_value: &StellarValue,
    network_id: &[u8; 32],
) -> ScpEnvelope {
    let value_xdr = stellar_value.to_xdr(Limits::none()).unwrap();

    let statement = ScpStatement {
        node_id: NodeId(PublicKey::PublicKeyTypeEd25519(Uint256(
            signing_key.verifying_key().to_bytes(),
        ))),
        slot_index: SLOT_INDEX,
        pledges: ScpStatementPledges::Externalize(ScpStatementExternalize {
            commit: ScpBallot {
                counter: 1,
                value: Value(value_xdr.try_into().unwrap()),
            },
            n_h: 1,
            commit_quorum_set_hash: Hash([0u8; 32]),
        }),
    };

    let envelope_type_xdr = EnvelopeType::Scp.to_xdr(Limits::none()).unwrap();
    let statement_xdr = statement.to_xdr(Limits::none()).unwrap();

    let mut msg = Vec::with_capacity(32 + 4 + statement_xdr.len());
    msg.extend_from_slice(network_id);
    msg.extend_from_slice(&envelope_type_xdr);
    msg.extend_from_slice(&statement_xdr);

    let signature = signing_key.sign(&msg);

    ScpEnvelope {
        statement,
        signature: stellar_xdr::curr::Signature(
            signature.to_bytes().to_vec().try_into().unwrap(),
        ),
    }
}

fn make_tx_set() -> (String, [u8; 32]) {
    let tx_set = GeneralizedTransactionSet::V1(stellar_xdr::curr::TransactionSetV1 {
        previous_ledger_hash: Hash([0u8; 32]),
        phases: VecM::default(),
    });

    let xdr_bytes = tx_set.to_xdr(Limits::none()).unwrap();
    let hash: [u8; 32] = Sha256::digest(&xdr_bytes).into();
    let b64 = base64::encode(&xdr_bytes);
    (b64, hash)
}

fn make_tx_result_meta_with_deposit(
    hvym_addr: &[u8; 32],
    freenet_id: &[u8; 32],
    amount: i128,
) -> String {
    let deposit_event = ContractEvent {
        ext: ExtensionPoint::V0,
        contract_id: Some(ContractId(Hash(*hvym_addr))),
        type_: ContractEventType::Contract,
        body: ContractEventBody::V0(ContractEventV0 {
            topics: vec![
                ScVal::Symbol(stellar_xdr::curr::ScSymbol(
                    "DEPOSIT".as_bytes().try_into().unwrap(),
                )),
                ScVal::Bytes(stellar_xdr::curr::ScBytes(
                    freenet_id.to_vec().try_into().unwrap(),
                )),
            ]
            .try_into()
            .unwrap(),
            data: ScVal::Vec(Some(
                vec![
                    ScVal::Void,
                    ScVal::I128(Int128Parts {
                        hi: (amount >> 64) as i64,
                        lo: amount as u64,
                    }),
                    ScVal::I128(Int128Parts { hi: 0, lo: 0 }),
                    ScVal::U32(100),
                ]
                .try_into()
                .unwrap(),
            )),
        }),
    };

    let soroban_meta = SorobanTransactionMeta {
        ext: SorobanTransactionMetaExt::V0,
        events: vec![deposit_event].try_into().unwrap(),
        return_value: ScVal::Void,
        diagnostic_events: VecM::default(),
    };

    let tx_meta = TransactionMeta::V3(TransactionMetaV3 {
        ext: ExtensionPoint::V0,
        tx_changes_before: LedgerEntryChanges(VecM::default()),
        operations: VecM::default(),
        tx_changes_after: LedgerEntryChanges(VecM::default()),
        soroban_meta: Some(soroban_meta),
    });

    let result_meta = TransactionResultMeta {
        result: TransactionResultPair {
            transaction_hash: Hash([0u8; 32]),
            result: stellar_xdr::curr::TransactionResult {
                fee_charged: 100,
                result: stellar_xdr::curr::TransactionResultResult::TxSuccess(VecM::default()),
                ext: stellar_xdr::curr::TransactionResultExt::V0,
            },
        },
        fee_processing: LedgerEntryChanges(VecM::default()),
        tx_apply_processing: tx_meta,
    };

    let xdr_bytes = result_meta.to_xdr(Limits::none()).unwrap();
    base64::encode(&xdr_bytes)
}

fn make_params(
    org_keypairs: &[Vec<SigningKey>],
    quorum_org_threshold: usize,
) -> DepositIndexParams {
    let organizations: Vec<ValidatorOrg> = org_keypairs
        .iter()
        .enumerate()
        .map(|(i, keys)| ValidatorOrg {
            name: format!("Org{i}"),
            validators: keys
                .iter()
                .map(|k| hex_encode(&k.verifying_key().to_bytes()))
                .collect(),
        })
        .collect();

    DepositIndexParams {
        network_id: test_network_id_hex(),
        organizations,
        quorum_org_threshold,
        hvym_contract_address: make_hvym_address_hex(),
    }
}

fn make_valid_proof(signers: &[&SigningKey], ledger_seq: u32, amount: i128) -> DepositProof {
    let (tx_set_b64, tx_set_hash) = make_tx_set();
    let stellar_value = make_stellar_value(tx_set_hash);
    let network_id = test_network_id();

    let scp_envelopes: Vec<String> = signers
        .iter()
        .map(|sk| {
            let env = make_signed_envelope(sk, &stellar_value, &network_id);
            let xdr = env.to_xdr(Limits::none()).unwrap();
            base64::encode(&xdr)
        })
        .collect();

    let meta_b64 =
        make_tx_result_meta_with_deposit(&make_hvym_address(), &make_freenet_contract_id(), amount);

    DepositProof {
        ledger_seq,
        scp_envelopes,
        transaction_set: tx_set_b64,
        tx_result_metas: vec![meta_b64],
    }
}

fn make_state(map: &DepositMap) -> State<'static> {
    State::from(serde_json::to_vec(map).unwrap())
}

fn make_empty_state() -> State<'static> {
    State::from(vec![])
}

fn make_params_bytes(params: &DepositIndexParams) -> Parameters<'static> {
    Parameters::from(serde_json::to_vec(params).unwrap())
}

// --- Validation tests ---

#[test]
fn test_validate_empty_state() {
    let result = DepositContract::validate_state(
        Parameters::from(vec![]),
        make_empty_state(),
        RelatedContracts::new(),
    );
    assert!(matches!(result, Ok(ValidateResult::Valid)));
}

#[test]
fn test_validate_well_formed() {
    let map = DepositMap {
        version: 1,
        last_ledger_seq: 100,
        deposits: vec![
            DepositEntry {
                contract_id: "aa".repeat(32),
                total_deposited: 1000,
                last_ledger: 100,
            },
            DepositEntry {
                contract_id: "bb".repeat(32),
                total_deposited: 2000,
                last_ledger: 100,
            },
        ],
    };
    let result = DepositContract::validate_state(
        Parameters::from(vec![]),
        make_state(&map),
        RelatedContracts::new(),
    );
    assert!(matches!(result, Ok(ValidateResult::Valid)));
}

#[test]
fn test_validate_unsorted() {
    let map = DepositMap {
        version: 1,
        last_ledger_seq: 100,
        deposits: vec![
            DepositEntry {
                contract_id: "bb".repeat(32),
                total_deposited: 2000,
                last_ledger: 100,
            },
            DepositEntry {
                contract_id: "aa".repeat(32),
                total_deposited: 1000,
                last_ledger: 100,
            },
        ],
    };
    let result = DepositContract::validate_state(
        Parameters::from(vec![]),
        make_state(&map),
        RelatedContracts::new(),
    );
    assert!(matches!(result, Ok(ValidateResult::Invalid)));
}

#[test]
fn test_validate_negative_amount() {
    let map = DepositMap {
        version: 1,
        last_ledger_seq: 100,
        deposits: vec![DepositEntry {
            contract_id: "aa".repeat(32),
            total_deposited: -100,
            last_ledger: 100,
        }],
    };
    let result = DepositContract::validate_state(
        Parameters::from(vec![]),
        make_state(&map),
        RelatedContracts::new(),
    );
    assert!(matches!(result, Ok(ValidateResult::Invalid)));
}

// --- SCP signature tests ---

#[test]
fn test_scp_signature_roundtrip() {
    let sk = make_keypair(1);
    let network_id = test_network_id();
    let (_, tx_set_hash) = make_tx_set();
    let stellar_value = make_stellar_value(tx_set_hash);
    let envelope = make_signed_envelope(&sk, &stellar_value, &network_id);

    let signer = scp::verify_envelope_signature(&envelope, &network_id).unwrap();
    assert_eq!(signer, sk.verifying_key().to_bytes());
}

#[test]
fn test_scp_invalid_signature() {
    let sk = make_keypair(1);
    let network_id = test_network_id();
    let (_, tx_set_hash) = make_tx_set();
    let stellar_value = make_stellar_value(tx_set_hash);
    let mut envelope = make_signed_envelope(&sk, &stellar_value, &network_id);

    // Corrupt the signature
    let mut sig_bytes: Vec<u8> = envelope.signature.0.to_vec();
    sig_bytes[0] ^= 0xFF;
    envelope.signature = stellar_xdr::curr::Signature(sig_bytes.try_into().unwrap());

    let result = scp::verify_envelope_signature(&envelope, &network_id);
    assert!(result.is_err());
}

// --- Quorum tests ---

#[test]
fn test_quorum_sufficient() {
    let network_id = test_network_id();
    let (_, tx_set_hash) = make_tx_set();
    let stellar_value = make_stellar_value(tx_set_hash);

    // 3 orgs, 2 validators each, threshold 2 orgs
    let org_keys: Vec<Vec<SigningKey>> = (0..3u8)
        .map(|org| {
            (0..2u8)
                .map(|v| make_keypair(org * 10 + v))
                .collect()
        })
        .collect();

    let params = make_params(&org_keys, 2);

    // Sign with majority from 2 orgs
    let envelopes: Vec<ScpEnvelope> = [
        &org_keys[0][0],
        &org_keys[0][1],
        &org_keys[1][0],
        &org_keys[1][1],
    ]
    .iter()
    .map(|sk| make_signed_envelope(sk, &stellar_value, &network_id))
    .collect();

    let result = scp::check_quorum(&envelopes, &params, &network_id);
    assert!(result.is_ok());
}

#[test]
fn test_quorum_insufficient() {
    let network_id = test_network_id();
    let (_, tx_set_hash) = make_tx_set();
    let stellar_value = make_stellar_value(tx_set_hash);

    // 3 orgs, 2 validators each, threshold 2 orgs
    let org_keys: Vec<Vec<SigningKey>> = (0..3u8)
        .map(|org| {
            (0..2u8)
                .map(|v| make_keypair(org * 10 + v))
                .collect()
        })
        .collect();

    let params = make_params(&org_keys, 2);

    // Sign with majority from only 1 org
    let envelopes: Vec<ScpEnvelope> = [&org_keys[0][0], &org_keys[0][1]]
        .iter()
        .map(|sk| make_signed_envelope(sk, &stellar_value, &network_id))
        .collect();

    let result = scp::check_quorum(&envelopes, &params, &network_id);
    assert!(result.is_err());
}

// --- Hash chain tests ---

#[test]
fn test_tx_set_hash_match() {
    let (b64, hash) = make_tx_set();
    let result = crate::hash_chain::verify_tx_set_hash(&b64, &hash);
    assert!(result.is_ok());
}

#[test]
fn test_tx_set_hash_mismatch() {
    let (b64, _) = make_tx_set();
    let wrong_hash = [0xFFu8; 32];
    let result = crate::hash_chain::verify_tx_set_hash(&b64, &wrong_hash);
    assert!(result.is_err());
}

// --- Full pipeline tests ---

#[test]
fn test_update_valid_proof() {
    let org_keys: Vec<Vec<SigningKey>> = (0..3u8)
        .map(|org| {
            (0..2u8)
                .map(|v| make_keypair(org * 10 + v))
                .collect()
        })
        .collect();
    let params = make_params(&org_keys, 0);

    let all_signers: Vec<&SigningKey> = org_keys.iter().flat_map(|org| org.iter()).collect();
    let proof = make_valid_proof(&all_signers, 100, 5_000_000);

    let proof_bytes = serde_json::to_vec(&proof).unwrap();
    let update_data = vec![UpdateData::Delta(StateDelta::from(proof_bytes))];

    let result =
        DepositContract::update_state(make_params_bytes(&params), make_empty_state(), update_data);
    assert!(result.is_ok());

    let modification = result.unwrap();
    let new_state = modification.new_state.unwrap();
    let map: DepositMap = serde_json::from_slice(new_state.as_ref()).unwrap();

    assert_eq!(map.deposits.len(), 1);
    assert_eq!(map.deposits[0].contract_id, make_freenet_contract_id_hex());
    assert_eq!(map.deposits[0].total_deposited, 5_000_000);
    assert_eq!(map.last_ledger_seq, 100);
    assert!(map.version > 0);
}

#[test]
fn test_update_invalid_signature() {
    let org_keys: Vec<Vec<SigningKey>> = (0..3u8)
        .map(|org| {
            (0..2u8)
                .map(|v| make_keypair(org * 10 + v))
                .collect()
        })
        .collect();
    let params = make_params(&org_keys, 0);

    // Signed by unknown keys
    let rogue_keys: Vec<SigningKey> = (0..6u8).map(|v| make_keypair(200 + v)).collect();
    let rogue_refs: Vec<&SigningKey> = rogue_keys.iter().collect();
    let proof = make_valid_proof(&rogue_refs, 100, 5_000_000);

    let proof_bytes = serde_json::to_vec(&proof).unwrap();
    let update_data = vec![UpdateData::Delta(StateDelta::from(proof_bytes))];

    let result =
        DepositContract::update_state(make_params_bytes(&params), make_empty_state(), update_data);

    assert!(result.is_ok());
    let map: DepositMap =
        serde_json::from_slice(result.unwrap().new_state.unwrap().as_ref()).unwrap();
    assert_eq!(map.deposits.len(), 0);
}

#[test]
fn test_update_insufficient_quorum() {
    let org_keys: Vec<Vec<SigningKey>> = (0..3u8)
        .map(|org| {
            (0..3u8)
                .map(|v| make_keypair(org * 10 + v))
                .collect()
        })
        .collect();
    let params = make_params(&org_keys, 3);

    // Only org0 validators
    let signers: Vec<&SigningKey> = org_keys[0].iter().collect();
    let proof = make_valid_proof(&signers, 100, 5_000_000);

    let proof_bytes = serde_json::to_vec(&proof).unwrap();
    let update_data = vec![UpdateData::Delta(StateDelta::from(proof_bytes))];

    let result =
        DepositContract::update_state(make_params_bytes(&params), make_empty_state(), update_data);

    assert!(result.is_ok());
    let map: DepositMap =
        serde_json::from_slice(result.unwrap().new_state.unwrap().as_ref()).unwrap();
    assert_eq!(map.deposits.len(), 0);
}

#[test]
fn test_update_stale_ledger() {
    let org_keys: Vec<Vec<SigningKey>> = (0..3u8)
        .map(|org| {
            (0..2u8)
                .map(|v| make_keypair(org * 10 + v))
                .collect()
        })
        .collect();
    let params = make_params(&org_keys, 0);

    let existing_map = DepositMap {
        version: 5,
        last_ledger_seq: 200,
        deposits: vec![],
    };

    let all_signers: Vec<&SigningKey> = org_keys.iter().flat_map(|org| org.iter()).collect();
    let proof = make_valid_proof(&all_signers, 100, 5_000_000); // ledger 100 < 200

    let proof_bytes = serde_json::to_vec(&proof).unwrap();
    let update_data = vec![UpdateData::Delta(StateDelta::from(proof_bytes))];

    let result = DepositContract::update_state(
        make_params_bytes(&params),
        make_state(&existing_map),
        update_data,
    );
    assert!(result.is_ok());

    let map: DepositMap =
        serde_json::from_slice(result.unwrap().new_state.unwrap().as_ref()).unwrap();
    assert_eq!(map.last_ledger_seq, 200);
    assert_eq!(map.deposits.len(), 0);
    assert_eq!(map.version, 5);
}

#[test]
fn test_update_monotonic_merge() {
    let org_keys: Vec<Vec<SigningKey>> = (0..3u8)
        .map(|org| {
            (0..2u8)
                .map(|v| make_keypair(org * 10 + v))
                .collect()
        })
        .collect();
    let params = make_params(&org_keys, 0);
    let all_signers: Vec<&SigningKey> = org_keys.iter().flat_map(|org| org.iter()).collect();

    // First proof
    let proof1 = make_valid_proof(&all_signers, 100, 1_000_000);
    let proof1_bytes = serde_json::to_vec(&proof1).unwrap();
    let update1 = vec![UpdateData::Delta(StateDelta::from(proof1_bytes))];

    let result1 =
        DepositContract::update_state(make_params_bytes(&params), make_empty_state(), update1);
    let state1 = result1.unwrap().new_state.unwrap();
    let map1: DepositMap = serde_json::from_slice(state1.as_ref()).unwrap();
    assert_eq!(map1.deposits[0].total_deposited, 1_000_000);

    // Second proof at later ledger
    let proof2 = make_valid_proof(&all_signers, 200, 2_000_000);
    let proof2_bytes = serde_json::to_vec(&proof2).unwrap();
    let update2 = vec![UpdateData::Delta(StateDelta::from(proof2_bytes))];

    let result2 = DepositContract::update_state(make_params_bytes(&params), state1, update2);
    let map2: DepositMap =
        serde_json::from_slice(result2.unwrap().new_state.unwrap().as_ref()).unwrap();

    assert_eq!(map2.deposits[0].total_deposited, 3_000_000);
    assert_eq!(map2.last_ledger_seq, 200);
}

#[test]
fn test_update_idempotent() {
    let org_keys: Vec<Vec<SigningKey>> = (0..3u8)
        .map(|org| {
            (0..2u8)
                .map(|v| make_keypair(org * 10 + v))
                .collect()
        })
        .collect();
    let params = make_params(&org_keys, 0);
    let all_signers: Vec<&SigningKey> = org_keys.iter().flat_map(|org| org.iter()).collect();
    let proof = make_valid_proof(&all_signers, 100, 5_000_000);

    // Apply once
    let proof_bytes = serde_json::to_vec(&proof).unwrap();
    let update1 = vec![UpdateData::Delta(StateDelta::from(proof_bytes.clone()))];
    let result1 =
        DepositContract::update_state(make_params_bytes(&params), make_empty_state(), update1);
    let state1 = result1.unwrap().new_state.unwrap();

    // Apply same proof again
    let update2 = vec![UpdateData::Delta(StateDelta::from(proof_bytes))];
    let result2 = DepositContract::update_state(make_params_bytes(&params), state1, update2);
    let map2: DepositMap =
        serde_json::from_slice(result2.unwrap().new_state.unwrap().as_ref()).unwrap();

    assert_eq!(map2.deposits.len(), 1);
    assert_eq!(map2.deposits[0].total_deposited, 5_000_000);
}

#[test]
fn test_update_wrong_contract_addr() {
    let org_keys: Vec<Vec<SigningKey>> = (0..3u8)
        .map(|org| {
            (0..2u8)
                .map(|v| make_keypair(org * 10 + v))
                .collect()
        })
        .collect();
    let mut params = make_params(&org_keys, 0);
    params.hvym_contract_address = hex_encode(&[0xCC; 32]);

    let all_signers: Vec<&SigningKey> = org_keys.iter().flat_map(|org| org.iter()).collect();
    let proof = make_valid_proof(&all_signers, 100, 5_000_000);

    let proof_bytes = serde_json::to_vec(&proof).unwrap();
    let update_data = vec![UpdateData::Delta(StateDelta::from(proof_bytes))];

    let result =
        DepositContract::update_state(make_params_bytes(&params), make_empty_state(), update_data);
    assert!(result.is_ok());

    let map: DepositMap =
        serde_json::from_slice(result.unwrap().new_state.unwrap().as_ref()).unwrap();
    assert_eq!(map.deposits.len(), 0);
    assert_eq!(map.last_ledger_seq, 100);
}

// --- Summarize and delta tests ---

#[test]
fn test_summarize_and_delta() {
    let map = DepositMap {
        version: 3,
        last_ledger_seq: 150,
        deposits: vec![
            DepositEntry {
                contract_id: "aa".repeat(32),
                total_deposited: 1000,
                last_ledger: 100,
            },
            DepositEntry {
                contract_id: "bb".repeat(32),
                total_deposited: 2000,
                last_ledger: 150,
            },
        ],
    };

    let state = make_state(&map);
    let summary =
        DepositContract::summarize_state(Parameters::from(vec![]), state.clone()).unwrap();

    let summary_data: DepositMapSummary = serde_json::from_slice(summary.as_ref()).unwrap();
    assert_eq!(summary_data.version, 3);
    assert_eq!(summary_data.entry_count, 2);
    assert_eq!(summary_data.last_ledger_seq, 150);

    let delta =
        DepositContract::get_state_delta(Parameters::from(vec![]), state, summary).unwrap();
    assert_eq!(delta.as_ref().len(), 0);
}

#[test]
fn test_delta_different_version() {
    let map = DepositMap {
        version: 5,
        last_ledger_seq: 200,
        deposits: vec![DepositEntry {
            contract_id: "aa".repeat(32),
            total_deposited: 3000,
            last_ledger: 200,
        }],
    };

    let state = make_state(&map);

    let old_summary = DepositMapSummary {
        version: 3,
        entry_count: 1,
        last_ledger_seq: 150,
    };
    let summary = StateSummary::from(serde_json::to_vec(&old_summary).unwrap());

    let delta =
        DepositContract::get_state_delta(Parameters::from(vec![]), state, summary).unwrap();

    assert!(!delta.as_ref().is_empty());
    let delta_map: DepositMap = serde_json::from_slice(delta.as_ref()).unwrap();
    assert_eq!(delta_map.version, 5);
}
