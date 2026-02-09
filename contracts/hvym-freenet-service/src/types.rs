use soroban_sdk::{contracttype, Address, BytesN};

/// Storage keys for the contract.
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Admin address (persistent storage).
    Admin,
    /// Deposit record keyed by Freenet contract ID hash (persistent storage).
    Deposit(BytesN<32>),
}

/// A persistence deposit record.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DepositRecord {
    /// Who deposited the XLM.
    pub depositor: Address,
    /// Amount in stroops (native XLM smallest unit).
    pub amount: i128,
    /// Ledger sequence when the deposit was created.
    pub created_at: u32,
    /// Ledger sequence of the last topup.
    pub updated_at: u32,
}
