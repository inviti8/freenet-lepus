#![no_std]

mod storage;
mod types;

#[cfg(test)]
mod test;

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, BytesN, Env, Vec};
use types::DepositRecord;

#[contract]
pub struct FreenetService;

#[contractimpl]
impl FreenetService {
    /// Initialize the contract with an admin address.
    pub fn __constructor(env: Env, admin: Address) {
        storage::set_admin(&env, &admin);
    }

    /// Deposit native XLM for a Freenet contract ID.
    ///
    /// Creates a new deposit or tops up an existing one.
    /// The caller must have approved the token transfer.
    pub fn deposit(
        env: Env,
        caller: Address,
        contract_id: BytesN<32>,
        amount: i128,
    ) -> DepositRecord {
        caller.require_auth();
        assert!(amount > 0, "amount must be positive");

        // Transfer native XLM from caller to this contract
        let native_token = token::StellarAssetClient::new(&env, &env.current_contract_address());
        // We use the token client for the transfer
        let token_client = token::Client::new(&env, &native_token.address);
        token_client.transfer(&caller, &env.current_contract_address(), &amount);

        let ledger_seq = env.ledger().sequence();

        let record = if let Some(existing) = storage::get_deposit(&env, &contract_id) {
            // Topup: increase amount
            DepositRecord {
                depositor: existing.depositor,
                amount: existing.amount + amount,
                created_at: existing.created_at,
                updated_at: ledger_seq,
            }
        } else {
            // New deposit
            DepositRecord {
                depositor: caller.clone(),
                amount,
                created_at: ledger_seq,
                updated_at: ledger_seq,
            }
        };

        storage::set_deposit(&env, &contract_id, &record);

        env.events()
            .publish((symbol_short!("DEPOSIT"), contract_id), record.clone());

        record
    }

    /// Withdraw the full deposit for a Freenet contract ID.
    ///
    /// Only the original depositor can withdraw. Returns the withdrawn amount.
    pub fn withdraw(env: Env, caller: Address, contract_id: BytesN<32>) -> i128 {
        caller.require_auth();

        let record = storage::get_deposit(&env, &contract_id)
            .expect("no deposit found for this contract ID");

        assert!(
            record.depositor == caller,
            "only the depositor can withdraw"
        );

        let amount = record.amount;

        // Transfer XLM back to the depositor
        let native_token = token::StellarAssetClient::new(&env, &env.current_contract_address());
        let token_client = token::Client::new(&env, &native_token.address);
        token_client.transfer(&env.current_contract_address(), &caller, &amount);

        storage::remove_deposit(&env, &contract_id);

        env.events()
            .publish((symbol_short!("WITHDRAW"), contract_id), amount);

        amount
    }

    /// Query the deposit for a single Freenet contract ID.
    ///
    /// Returns None if no deposit exists.
    pub fn get_deposit(env: Env, contract_id: BytesN<32>) -> Option<DepositRecord> {
        storage::get_deposit(&env, &contract_id)
    }

    /// Batch query deposits for multiple Freenet contract IDs.
    ///
    /// Returns a vector of (contract_id, deposit_record) pairs for
    /// contracts that have deposits. Contracts without deposits are omitted.
    pub fn get_deposits(
        env: Env,
        contract_ids: Vec<BytesN<32>>,
    ) -> Vec<(BytesN<32>, DepositRecord)> {
        let mut results = Vec::new(&env);

        for id in contract_ids.iter() {
            if let Some(record) = storage::get_deposit(&env, &id) {
                results.push_back((id.clone(), record));
            }
        }

        results
    }

    /// Transfer admin to a new address. Admin-only.
    pub fn set_admin(env: Env, caller: Address, new_admin: Address) {
        caller.require_auth();
        let admin = storage::get_admin(&env);
        assert!(caller == admin, "only admin can transfer admin");
        storage::set_admin(&env, &new_admin);
    }
}
