use soroban_sdk::{Address, Env};

use crate::types::DataKey;

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
// Burn BPS
// =============================================================================

pub fn get_burn_bps(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::BurnBps)
        .expect("burn_bps not set")
}

pub fn set_burn_bps(env: &Env, bps: u32) {
    env.storage().persistent().set(&DataKey::BurnBps, &bps);
    env.storage()
        .persistent()
        .extend_ttl(&DataKey::BurnBps, LEDGER_THRESHOLD, LEDGER_BUMP);
}

// =============================================================================
// Token Address
// =============================================================================

pub fn get_token(env: &Env) -> Address {
    env.storage()
        .persistent()
        .get(&DataKey::TokenAddress)
        .expect("token address not set")
}

pub fn set_token(env: &Env, token: &Address) {
    env.storage()
        .persistent()
        .set(&DataKey::TokenAddress, token);
    env.storage()
        .persistent()
        .extend_ttl(&DataKey::TokenAddress, LEDGER_THRESHOLD, LEDGER_BUMP);
}
