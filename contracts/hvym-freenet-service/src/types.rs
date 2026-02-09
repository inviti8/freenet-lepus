use soroban_sdk::contracttype;

/// Storage keys for the contract.
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Admin address (persistent storage).
    Admin,
    /// Burn ratio in basis points, e.g. 3000 = 30% (persistent storage).
    BurnBps,
    /// Native XLM SAC token address (persistent storage).
    TokenAddress,
}
