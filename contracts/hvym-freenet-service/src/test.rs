#![cfg(test)]

use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env, Vec,
};

use crate::{FreenetService, FreenetServiceClient};

fn setup_env() -> (Env, FreenetServiceClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(FreenetService, (&admin,));
    let client = FreenetServiceClient::new(&env, &contract_id);

    (env, client, admin)
}

fn make_contract_id(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

fn setup_native_token(env: &Env, admin: &Address, user: &Address, amount: i128) {
    let sac = StellarAssetClient::new(env, &env.current_contract_address());
    // Mint native XLM to user for testing
    let token_admin = Address::generate(env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin.clone());
    let sac_client = StellarAssetClient::new(env, &token_contract.address());
    sac_client.mint(user, &amount);
}

#[test]
fn test_constructor_sets_admin() {
    let (env, client, admin) = setup_env();
    // Admin is set — calling set_admin with admin should work
    let new_admin = Address::generate(&env);
    client.set_admin(&admin, &new_admin);
    // New admin should now be able to call set_admin
    client.set_admin(&new_admin, &admin);
}

#[test]
fn test_get_deposit_returns_none_for_unknown() {
    let (env, client, _admin) = setup_env();
    let contract_id = make_contract_id(&env, 1);
    let result = client.get_deposit(&contract_id);
    assert_eq!(result, None);
}

#[test]
fn test_get_deposits_batch_returns_empty_for_unknown() {
    let (env, client, _admin) = setup_env();
    let ids = Vec::from_array(
        &env,
        [make_contract_id(&env, 1), make_contract_id(&env, 2)],
    );
    let result = client.get_deposits(&ids);
    assert_eq!(result.len(), 0);
}

#[test]
#[should_panic(expected = "only admin can transfer admin")]
fn test_only_admin_can_set_admin() {
    let (env, client, _admin) = setup_env();
    let not_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    client.set_admin(&not_admin, &new_admin);
}
