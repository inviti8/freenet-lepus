use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

use crate::{FreenetService, FreenetServiceClient};

/// Set up the test environment with a native token, admin, and the FreenetService contract.
///
/// Returns (env, service_client, admin_address, token_address, token_admin_client).
fn setup_env(
    burn_bps: u32,
) -> (
    Env,
    FreenetServiceClient<'static>,
    Address,
    Address,
    StellarAssetClient<'static>,
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);

    // Create a SAC token for testing (stands in for native XLM)
    let token_admin = Address::generate(&env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_address = token_contract.address();
    let token_admin_client = StellarAssetClient::new(&env, &token_address);

    let contract_id = env.register(FreenetService, (&admin, burn_bps, &token_address));
    let client = FreenetServiceClient::new(&env, &contract_id);

    (env, client, admin, token_address, token_admin_client)
}

fn make_contract_id(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

fn token_balance(env: &Env, token_address: &Address, account: &Address) -> i128 {
    TokenClient::new(env, token_address).balance(account)
}

// =============================================================================
// Constructor
// =============================================================================

#[test]
fn test_constructor_sets_admin() {
    let (env, client, admin, _, _) = setup_env(3000);
    let new_admin = Address::generate(&env);
    client.set_admin(&admin, &new_admin);
    // New admin should be able to call set_admin
    client.set_admin(&new_admin, &admin);
}

#[test]
fn test_constructor_sets_burn_bps() {
    let (_env, client, admin, _, _) = setup_env(5000);
    // Verify by changing it — if constructor didn't set it, set_burn_bps would work
    // but the deposit split would be different. We test via a deposit below.
    client.set_burn_bps(&admin, &2000_u32);
}

#[test]
#[should_panic(expected = "burn_bps must be <= 10000")]
fn test_constructor_rejects_invalid_burn_bps() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin);
    let token_address = token_contract.address();
    env.register(FreenetService, (&admin, 10_001_u32, &token_address));
}

// =============================================================================
// Deposit
// =============================================================================

#[test]
fn test_deposit_splits_burn_and_treasury() {
    let (env, client, _admin, token_address, token_admin_client) = setup_env(3000);

    let depositor = Address::generate(&env);
    let contract_id = make_contract_id(&env, 1);
    let deposit_amount: i128 = 10_000;

    // Mint tokens to depositor
    token_admin_client.mint(&depositor, &deposit_amount);
    assert_eq!(
        token_balance(&env, &token_address, &depositor),
        deposit_amount
    );

    // Deposit
    client.deposit(&depositor, &contract_id, &deposit_amount);

    // 30% burned = 3000, 70% treasury = 7000
    // Depositor should have 0 (all spent: 7000 transferred + 3000 burned)
    assert_eq!(token_balance(&env, &token_address, &depositor), 0);

    // Contract (treasury) should have 7000
    let service_addr = client.address.clone();
    assert_eq!(token_balance(&env, &token_address, &service_addr), 7_000);
}

#[test]
fn test_deposit_zero_burn() {
    let (env, client, _admin, token_address, token_admin_client) = setup_env(0);

    let depositor = Address::generate(&env);
    let contract_id = make_contract_id(&env, 1);
    let deposit_amount: i128 = 10_000;

    token_admin_client.mint(&depositor, &deposit_amount);
    client.deposit(&depositor, &contract_id, &deposit_amount);

    // 0% burned, 100% treasury
    assert_eq!(token_balance(&env, &token_address, &depositor), 0);
    let service_addr = client.address.clone();
    assert_eq!(token_balance(&env, &token_address, &service_addr), 10_000);
}

#[test]
fn test_deposit_full_burn() {
    let (env, client, _admin, token_address, token_admin_client) = setup_env(10_000);

    let depositor = Address::generate(&env);
    let contract_id = make_contract_id(&env, 1);
    let deposit_amount: i128 = 10_000;

    token_admin_client.mint(&depositor, &deposit_amount);
    client.deposit(&depositor, &contract_id, &deposit_amount);

    // 100% burned, 0% treasury
    assert_eq!(token_balance(&env, &token_address, &depositor), 0);
    let service_addr = client.address.clone();
    assert_eq!(token_balance(&env, &token_address, &service_addr), 0);
}

#[test]
#[should_panic(expected = "amount must be positive")]
fn test_deposit_requires_positive_amount() {
    let (env, client, _, _, _) = setup_env(3000);
    let depositor = Address::generate(&env);
    let contract_id = make_contract_id(&env, 1);
    client.deposit(&depositor, &contract_id, &0);
}

#[test]
fn test_multiple_deposits_accumulate_treasury() {
    let (env, client, _admin, token_address, token_admin_client) = setup_env(3000);

    let depositor = Address::generate(&env);
    let contract_id_a = make_contract_id(&env, 1);
    let contract_id_b = make_contract_id(&env, 2);

    token_admin_client.mint(&depositor, &20_000);

    client.deposit(&depositor, &contract_id_a, &10_000);
    client.deposit(&depositor, &contract_id_b, &10_000);

    // 2 × 7000 = 14000 in treasury
    let service_addr = client.address.clone();
    assert_eq!(token_balance(&env, &token_address, &service_addr), 14_000);
    // 2 × 3000 = 6000 burned, depositor spent all 20000
    assert_eq!(token_balance(&env, &token_address, &depositor), 0);
}

// =============================================================================
// Admin Withdraw
// =============================================================================

#[test]
fn test_admin_withdraw() {
    let (env, client, admin, token_address, token_admin_client) = setup_env(3000);

    let depositor = Address::generate(&env);
    let contract_id = make_contract_id(&env, 1);
    token_admin_client.mint(&depositor, &10_000);

    client.deposit(&depositor, &contract_id, &10_000);

    // Treasury has 7000. Admin withdraws 5000 to a recipient.
    let recipient = Address::generate(&env);
    client.admin_withdraw(&admin, &recipient, &5_000);

    assert_eq!(token_balance(&env, &token_address, &recipient), 5_000);
    let service_addr = client.address.clone();
    assert_eq!(token_balance(&env, &token_address, &service_addr), 2_000);
}

#[test]
#[should_panic(expected = "only admin can withdraw")]
fn test_non_admin_cannot_withdraw() {
    let (env, client, _admin, _, token_admin_client) = setup_env(3000);

    let depositor = Address::generate(&env);
    let contract_id = make_contract_id(&env, 1);
    token_admin_client.mint(&depositor, &10_000);
    client.deposit(&depositor, &contract_id, &10_000);

    let not_admin = Address::generate(&env);
    let recipient = Address::generate(&env);
    client.admin_withdraw(&not_admin, &recipient, &5_000);
}

#[test]
#[should_panic(expected = "amount must be positive")]
fn test_admin_withdraw_requires_positive_amount() {
    let (env, client, admin, _, _) = setup_env(3000);
    let recipient = Address::generate(&env);
    client.admin_withdraw(&admin, &recipient, &0);
}

// =============================================================================
// Set Burn BPS
// =============================================================================

#[test]
fn test_set_burn_bps() {
    let (env, client, admin, token_address, token_admin_client) = setup_env(3000);

    // Change burn to 50%
    client.set_burn_bps(&admin, &5000_u32);

    let depositor = Address::generate(&env);
    let contract_id = make_contract_id(&env, 1);
    token_admin_client.mint(&depositor, &10_000);

    client.deposit(&depositor, &contract_id, &10_000);

    // 50% burned = 5000, 50% treasury = 5000
    let service_addr = client.address.clone();
    assert_eq!(token_balance(&env, &token_address, &service_addr), 5_000);
    assert_eq!(token_balance(&env, &token_address, &depositor), 0);
}

#[test]
#[should_panic(expected = "only admin can set burn ratio")]
fn test_non_admin_cannot_set_burn_bps() {
    let (env, client, _admin, _, _) = setup_env(3000);
    let not_admin = Address::generate(&env);
    client.set_burn_bps(&not_admin, &5000_u32);
}

#[test]
#[should_panic(expected = "burn_bps must be <= 10000")]
fn test_set_burn_bps_rejects_invalid() {
    let (_env, client, admin, _, _) = setup_env(3000);
    client.set_burn_bps(&admin, &10_001_u32);
}

// =============================================================================
// Set Admin
// =============================================================================

#[test]
#[should_panic(expected = "only admin can transfer admin")]
fn test_only_admin_can_set_admin() {
    let (env, client, _admin, _, _) = setup_env(3000);
    let not_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    client.set_admin(&not_admin, &new_admin);
}
