#![no_std]

mod storage;
mod types;

#[cfg(test)]
mod test;

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, BytesN, Env};

#[contract]
pub struct FreenetService;

#[contractimpl]
impl FreenetService {
    /// Initialize the contract with an admin address, burn ratio, and token address.
    ///
    /// `burn_bps` is in basis points (0–10000, e.g. 3000 = 30%).
    /// `token` is the native XLM SAC address.
    pub fn __constructor(env: Env, admin: Address, burn_bps: u32, token: Address) {
        assert!(burn_bps <= 10_000, "burn_bps must be <= 10000");
        storage::set_admin(&env, &admin);
        storage::set_burn_bps(&env, burn_bps);
        storage::set_token(&env, &token);
    }

    /// Deposit native XLM for a Freenet contract ID. Non-refundable.
    ///
    /// Splits between SAC burn and contract treasury per `burn_bps`.
    /// Emits: `("DEPOSIT", contract_id) → (caller, amount, burn_amount, ledger_seq)`
    pub fn deposit(env: Env, caller: Address, contract_id: BytesN<32>, amount: i128) {
        caller.require_auth();
        assert!(amount > 0, "amount must be positive");

        let burn_bps = storage::get_burn_bps(&env) as i128;
        let burn_amount = amount * burn_bps / 10_000;
        let treasury_amount = amount - burn_amount;

        let token_addr = storage::get_token(&env);
        let xlm_client = token::Client::new(&env, &token_addr);

        // Transfer treasury portion to this contract
        if treasury_amount > 0 {
            xlm_client.transfer(&caller, &env.current_contract_address(), &treasury_amount);
        }

        // Burn the burn portion via SAC burn()
        if burn_amount > 0 {
            xlm_client.burn(&caller, &burn_amount);
        }

        env.events().publish(
            (symbol_short!("DEPOSIT"), contract_id),
            (caller, amount, burn_amount, env.ledger().sequence()),
        );
    }

    /// Admin-only: withdraw XLM from the contract treasury.
    pub fn admin_withdraw(env: Env, caller: Address, to: Address, amount: i128) {
        caller.require_auth();
        let admin = storage::get_admin(&env);
        assert!(caller == admin, "only admin can withdraw");
        assert!(amount > 0, "amount must be positive");

        let token_addr = storage::get_token(&env);
        let xlm_client = token::Client::new(&env, &token_addr);
        xlm_client.transfer(&env.current_contract_address(), &to, &amount);

        env.events().publish(
            (symbol_short!("WITHDRAW"),),
            (to, amount, env.ledger().sequence()),
        );
    }

    /// Admin-only: update the burn ratio (basis points, 0–10000).
    pub fn set_burn_bps(env: Env, caller: Address, burn_bps: u32) {
        caller.require_auth();
        let admin = storage::get_admin(&env);
        assert!(caller == admin, "only admin can set burn ratio");
        assert!(burn_bps <= 10_000, "burn_bps must be <= 10000");
        storage::set_burn_bps(&env, burn_bps);
    }

    /// Transfer admin to a new address. Admin-only.
    pub fn set_admin(env: Env, caller: Address, new_admin: Address) {
        caller.require_auth();
        let admin = storage::get_admin(&env);
        assert!(caller == admin, "only admin can transfer admin");
        storage::set_admin(&env, &new_admin);
    }
}
