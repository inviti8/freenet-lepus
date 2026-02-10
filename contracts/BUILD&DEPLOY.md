# Contract Build & Deploy Guide

This repository contains three smart contracts. Two are **Freenet WASM** contracts deployed to the Freenet network, and one is a **Soroban** contract deployed to the Stellar blockchain.

| Contract | Type | Location | CI Workflow |
|----------|------|----------|-------------|
| hvym-freenet-service | Soroban (Stellar) | `hvym-freenet-service/` | `contract-release.yml` + `contract-deploy.yml` |
| deposit-index | Freenet WASM | `deposit-index/` | `freenet-contract-release.yml` |
| datapod | Freenet WASM | `datapod/` | `freenet-contract-release.yml` |

All three are **standalone crates** (excluded from the workspace) because they target `wasm32-unknown-unknown` with incompatible dependency trees.

---

## GitHub Repository Setup (Required for CI)

Before using the CI workflows, configure these settings in your GitHub repository:

### 1. Workflow Permissions

Go to **Settings > Actions > General > Workflow permissions** and select:
- **Read and write permissions** (required because the Freenet contract release and Soroban deploy workflows commit build artifacts back to `main`)

### 2. Repository Secrets

Go to **Settings > Secrets and variables > Actions > Repository secrets** and add:

| Secret | Required By | How to Obtain |
|--------|-------------|---------------|
| `STELLAR_DEPLOYER_SECRET` | `contract-deploy.yml` (Soroban deploy only) | The Stellar secret key (starts with `S...`) of the account that will deploy the Soroban contract. Generate one locally with `stellar keys generate deployer --network testnet --fund` then retrieve it with `stellar keys show deployer` |

> `GITHUB_TOKEN` is provided automatically by GitHub Actions — you do not need to create it.

---

## Local Prerequisites

- **Rust stable** with the WASM target:
  ```bash
  rustup target add wasm32-unknown-unknown
  ```
- **Python 3** — for build/deploy scripts
- **Stellar CLI v22.0.0** — only needed for hvym-freenet-service:
  ```bash
  cargo install stellar-cli --version 22.0.0 --locked
  ```

---

## Freenet WASM Contracts (deposit-index, datapod)

These two contracts share the same build script and CI workflow.

### Local Build

```bash
# Build deposit-index → contracts/wasm/deposit_index.wasm
python contracts/build_freenet_contract.py --contract deposit-index

# Build datapod → contracts/wasm/datapod_contract.wasm
python contracts/build_freenet_contract.py --contract datapod
```

The script reads the crate's `Cargo.toml` to derive the WASM filename, runs `cargo build --target wasm32-unknown-unknown --release`, and copies the output to `contracts/wasm/`.

You can also build directly without the script:
```bash
cd contracts/deposit-index
cargo build --target wasm32-unknown-unknown --release
# Output: target/wasm32-unknown-unknown/release/deposit_index.wasm
```

### Tests

```bash
cd contracts/deposit-index && cargo test    # 19 tests (SCP envelope + Ed25519 sig verification)
cd contracts/datapod && cargo test
```

### CI Release (`freenet-contract-release.yml`)

Push a tag to build, commit the WASM to `contracts/wasm/` on `main`, and create a GitHub Release:

```bash
# deposit-index
git tag release-deposit-index-v0.1.0
git push --tags

# datapod
git tag release-datapod-v0.1.0
git push --tags
```

**Tag pattern:** `release-<contract-name>-v<version>` (e.g., `release-deposit-index-v0.1.0`)

**What the workflow does:**
1. Extracts the contract directory name from the tag
2. Installs Rust + `wasm32-unknown-unknown` target
3. Runs `build_freenet_contract.py --contract <name>`
4. Commits the built WASM to `contracts/wasm/` on `main`
5. Creates a GitHub Release with the WASM attached

### Deployment to Freenet

Freenet WASM contracts are deployed via the `fdev` tool or the Freenet node API (contract PUT operation), not through CI. Each contract requires its own parameters:

- **deposit-index** — `DepositIndexParams` (validator public keys, quorum configuration)
- **datapod** — `DatapodParams` (creator pubkey, recipient pubkey) which produce a unique `ContractKey` per instance

---

## Soroban Contract (hvym-freenet-service)

### Local Build

```bash
python contracts/build_contract.py              # Build + optimize
python contracts/build_contract.py --no-optimize # Build only (faster)
```

**Output:**
- `contracts/wasm/hvym_freenet_service.optimized.wasm` (default, with optimization)
- `contracts/wasm/hvym_freenet_service.wasm` (with `--no-optimize`)

The script runs `stellar contract build` then `stellar contract optimize`, and copies the result to `contracts/wasm/`.

### Local Deploy

```bash
# 1. Create a funded deployer identity (first time only)
stellar keys generate testnet_DEPLOYER --network testnet --fund

# 2. Deploy
python contracts/deploy_contract.py --deployer-acct testnet_DEPLOYER --network testnet
```

The deploy script reads constructor args from `hvym_freenet_service_args.json`:

```json
{
  "admin": "TESTNET_DEPLOYER",
  "burn_bps": 3000
}
```

| Field | Description |
|-------|-------------|
| `admin` | Stellar CLI identity name for the admin role |
| `burn_bps` | Burn ratio in basis points (3000 = 30%) |

It then uploads the WASM, resolves the deployer address and native XLM SAC address, deploys with the constructor args, and saves the result to `contracts/deployments.json`.

### CI Release (`contract-release.yml`)

Push a tag to build the optimized WASM and create a GitHub Release:

```bash
git tag release-hvym-freenet-service-v0.1.0
git push --tags
```

**Tag pattern:** `release-hvym-freenet-service-v<version>`

**What the workflow does:**
1. Installs Rust + `wasm32-unknown-unknown` + Stellar CLI v22.0.0
2. Runs `stellar contract build` + `stellar contract optimize`
3. Creates a GitHub Release with `hvym_freenet_service.optimized.wasm` attached

### CI Deploy (`contract-deploy.yml`)

Push a tag to deploy a previously released WASM to Stellar testnet or mainnet:

```bash
# Deploy to testnet
git tag deploy-hvym-freenet-service-v0.1.0-testnet
git push --tags

# Deploy to mainnet
git tag deploy-hvym-freenet-service-v0.1.0-mainnet
git push --tags
```

**Tag pattern:** `deploy-hvym-freenet-service-v<version>-<network>`

> **Prerequisite:** The matching release tag (`release-hvym-freenet-service-v0.1.0`) must already exist — the deploy workflow downloads the WASM from that GitHub Release.

> **Required secret:** `STELLAR_DEPLOYER_SECRET` must be set in the repository secrets (see [GitHub Repository Setup](#github-repository-setup-required-for-ci) above).

**What the workflow does:**
1. Downloads `hvym_freenet_service.optimized.wasm` from the matching release
2. Installs Stellar CLI v22.0.0
3. Sets up the deployer identity from `STELLAR_DEPLOYER_SECRET`
4. Runs `deploy_contract.py`
5. Commits updated `contracts/deployments.json` to `main`

### Full Soroban Release + Deploy Sequence

```bash
# Step 1: Build — CI creates GitHub Release with WASM artifact
git tag release-hvym-freenet-service-v0.1.0
git push --tags
# Wait for the "Contract Release" workflow to complete...

# Step 2: Deploy — CI deploys to testnet and commits deployments.json
git tag deploy-hvym-freenet-service-v0.1.0-testnet
git push --tags
```

---

## Directory Layout

```
contracts/
├── hvym-freenet-service/              # Soroban contract crate
├── deposit-index/                     # Freenet WASM contract crate
├── datapod/                           # Freenet WASM contract crate
├── wasm/                              # Built WASM output (committed by CI)
├── build_contract.py                  # Build script — hvym-freenet-service (Soroban)
├── build_freenet_contract.py          # Build script — Freenet WASM contracts
├── deploy_contract.py                 # Deploy script — hvym-freenet-service (Soroban)
├── hvym_freenet_service_args.json     # Soroban constructor args
├── deployments.json                   # Soroban deployment records (committed by CI)
└── BUILD&DEPLOY.md                    # This file
```

## Architecture Docs

- [`docs/architecture/lepus/README.md`](../docs/architecture/lepus/README.md) — Lepus overview
- [`docs/architecture/lepus/stellar-contract.md`](../docs/architecture/lepus/stellar-contract.md) — Soroban contract design
- [`docs/architecture/lepus/deposit-index-contract.md`](../docs/architecture/lepus/deposit-index-contract.md) — Deposit-index contract design
- [`docs/architecture/lepus/datapod-contract.md`](../docs/architecture/lepus/datapod-contract.md) — Datapod contract design
