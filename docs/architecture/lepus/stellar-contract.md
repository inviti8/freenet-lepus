# Stellar Contract: hvym-freenet-service

## Overview

`hvym-freenet-service` is a Soroban smart contract deployed on the Stellar network. It holds XLM persistence deposits that back Freenet contracts with economic commitment. Nodes query this contract via the oracle to populate the CWP commitment sub-score.

- **SDK:** Soroban SDK 22.0.0
- **Currency:** Native XLM (via Stellar Asset Contract)
- **Contract IDs:** `BytesN<32>` — Freenet contract key hashes

## Contract Functions

| Function | Auth | Args | Returns | Description |
|----------|------|------|---------|-------------|
| `__constructor` | Deploy | `admin: Address` | — | Initialize with admin address |
| `deposit` | Caller | `caller, contract_id, amount` | `DepositRecord` | Deposit XLM for a Freenet contract |
| `withdraw` | Depositor | `caller, contract_id` | `i128` | Withdraw full deposit (depositor only) |
| `get_deposit` | None | `contract_id` | `Option<DepositRecord>` | Query single deposit |
| `get_deposits` | None | `contract_ids: Vec<BytesN<32>>` | `Vec<(BytesN<32>, DepositRecord)>` | Batch query deposits |
| `set_admin` | Admin | `caller, new_admin` | — | Transfer admin role |

**Code reference:** `contracts/hvym-freenet-service/src/lib.rs:16-131`

### deposit

Creates a new deposit or tops up an existing one. The caller must have pre-approved the XLM transfer. On topup, the `amount` is added to the existing balance and `updated_at` is refreshed.

Emits a `DEPOSIT` event with the full `DepositRecord`.

### withdraw

Withdraws the full deposit amount. Only the original depositor can withdraw. The deposit record is removed after withdrawal.

Emits a `WITHDRAW` event with the withdrawn amount.

### get_deposits

Batch query used by the oracle. Takes a vector of Freenet contract ID hashes and returns `(contract_id, DepositRecord)` pairs for contracts that have deposits. Contracts without deposits are omitted from the result.

## Data Model

### DepositRecord

```rust
pub struct DepositRecord {
    pub depositor: Address,   // Who deposited the XLM
    pub amount: i128,         // Amount in stroops (1 XLM = 10^7 stroops)
    pub created_at: u32,      // Ledger sequence when created
    pub updated_at: u32,      // Ledger sequence of last topup
}
```

**Code reference:** `contracts/hvym-freenet-service/src/types.rs:14-25`

### DataKey

```rust
pub enum DataKey {
    Admin,                    // Admin address (persistent)
    Deposit(BytesN<32>),      // Deposit keyed by Freenet contract ID hash
}
```

**Code reference:** `contracts/hvym-freenet-service/src/types.rs:4-11`

### Storage TTL

All persistent storage entries are automatically bumped on access:

| Constant | Value | Meaning |
|----------|-------|---------|
| `LEDGER_BUMP` | 518,400 | ~30 days of ledger sequences |
| `LEDGER_THRESHOLD` | 259,200 | Bump triggered when TTL drops below ~15 days |

**Code reference:** `contracts/hvym-freenet-service/src/storage.rs:5-8`

## Integration with Freenet Nodes

The hvym-freenet-service Soroban contract emits `DEPOSIT` events when users deposit XLM for Freenet contracts. These events are bridged to the Freenet network via the **deposit-index contract** and **relayer nodes**.

### Data Flow

```mermaid
flowchart LR
    User["User deposits XLM"] --> Soroban["hvym-freenet-service<br/>(Soroban)"]
    Soroban -->|"DEPOSIT event"| Relayer["Relayer Node"]
    Relayer -->|"SCP proof as UPDATE delta"| DepIdx["Deposit-Index<br/>WASM Contract"]
    DepIdx -->|"subscription push"| Nodes["All Lepus Nodes"]
    Nodes -->|"update_commitments_batch"| CWP["CWP Scores"]
```

1. Users deposit XLM via the Soroban contract
2. Relayer nodes poll Stellar for new ledgers with DEPOSIT events
3. Relayers fetch SCP proofs and submit them as UPDATE deltas to the deposit-index Freenet contract
4. The deposit-index contract verifies the SCP proofs and updates its state
5. All lepus nodes receive the updated state via subscription
6. Each node matches deposits to locally hosted contracts and updates CWP commitment scores

This replaces the original design where each node polled Soroban directly. The deposit-index contract acts as a verified bridge, reducing Stellar RPC load and leveraging Freenet's subscription infrastructure.

See [Deposit-Index Contract](deposit-index-contract.md) for details on the verification pipeline and node-side integration.

### Failure Handling (Relayer)

| Scenario | Behavior |
|----------|----------|
| Stellar RPC timeout / error | Increment `consecutive_failures`, exponential backoff |
| Backoff schedule | 1s, 2s, 4s, 8s, ... capped at 300s (5 minutes) |
| Recovery | First successful query resets backoff to 0 |
| UPDATE submission failure | Skip ledger, retry next cycle |

**Code reference:** `crates/core/src/ring/hosting/oracle.rs`

## Deployment

### Build

```bash
# Using stellar CLI directly
stellar contract build
stellar contract optimize --wasm target/wasm32-unknown-unknown/release/hvym_freenet_service.wasm

# Using the build script
python contracts/build_contract.py
```

The build produces an optimized `.wasm` binary suitable for Soroban deployment.

### Deploy

```bash
# Using the deploy script
python contracts/deploy_contract.py
```

The deploy script handles contract installation and initialization with an admin address.

### CI Workflows

| Workflow | File | Trigger | Purpose |
|----------|------|---------|---------|
| Contract Release | `.github/workflows/contract-release.yml` | Tag push | Build and publish WASM artifact |
| Contract Deploy | `.github/workflows/contract-deploy.yml` | Tag push | Deploy to Soroban testnet/mainnet |

## Source Files

| File | Purpose |
|------|---------|
| `contracts/hvym-freenet-service/src/lib.rs` | Contract functions (deposit, withdraw, query) |
| `contracts/hvym-freenet-service/src/types.rs` | DepositRecord, DataKey |
| `contracts/hvym-freenet-service/src/storage.rs` | Persistent storage with TTL management |
| `contracts/hvym-freenet-service/src/test.rs` | Contract unit tests |
| `contracts/hvym-freenet-service/Cargo.toml` | Standalone crate (not in workspace) |
| `contracts/build_contract.py` | Build script |
| `contracts/deploy_contract.py` | Deploy script |
| `crates/core/src/ring/hosting/oracle.rs` | Node-side oracle worker |
| `.github/workflows/contract-release.yml` | CI build workflow |
| `.github/workflows/contract-deploy.yml` | CI deploy workflow |

## Related Documentation

- [Lepus Overview](README.md) — CWP scoring and architecture
- [Deposit-Index Contract](deposit-index-contract.md) — Freenet WASM contract for SCP-verified deposits
- [Datapod Contract](datapod-contract.md) — WASM identity validator
