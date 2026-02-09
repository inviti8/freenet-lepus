use soroban_sdk::{Address, Env};

use crate::types::{DataKey, DepositRecord};

/// Bump amount for persistent storage entries (roughly 30 days in ledgers).
const LEDGER_BUMP: u32 = 518_400;
/// Threshold for bumping (roughly 15 days).
const LEDGER_THRESHOLD: u32 = 259_200;

// =============================================================================
// Admin
// =============================================================================

pub fn get_admin(env: &Env) -> Address {
    env.storage()
        .persistent()
        .get(&DataKey::Admin)
        .expect("admin not set")
}

pub fn set_admin(env: &Env, admin: &Address) {
    env.storage().persistent().set(&DataKey::Admin, admin);
    env.storage()
        .persistent()
        .extend_ttl(&DataKey::Admin, LEDGER_THRESHOLD, LEDGER_BUMP);
}

// =============================================================================
// Deposits
// =============================================================================

pub fn get_deposit(env: &Env, contract_id: &soroban_sdk::BytesN<32>) -> Option<DepositRecord> {
    let key = DataKey::Deposit(contract_id.clone());
    let record: Option<DepositRecord> = env.storage().persistent().get(&key);
    if record.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
    record
}

pub fn set_deposit(
    env: &Env,
    contract_id: &soroban_sdk::BytesN<32>,
    record: &DepositRecord,
) {
    let key = DataKey::Deposit(contract_id.clone());
    env.storage().persistent().set(&key, record);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
}

pub fn has_deposit(env: &Env, contract_id: &soroban_sdk::BytesN<32>) -> bool {
    env.storage()
        .persistent()
        .has(&DataKey::Deposit(contract_id.clone()))
}

pub fn remove_deposit(env: &Env, contract_id: &soroban_sdk::BytesN<32>) {
    env.storage()
        .persistent()
        .remove(&DataKey::Deposit(contract_id.clone()));
}
