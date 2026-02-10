# Contract Build & Deploy Guide

## Quick Reference

| Contract | Type | Location | Build Tool | CI Workflow |
|----------|------|----------|------------|-------------|
| hvym-freenet-service | Soroban (Stellar) | `contracts/hvym-freenet-service/` | `stellar contract build` | `contract-release.yml`, `contract-deploy.yml` |
| deposit-index | Freenet WASM | `contracts/deposit-index/` | `build_freenet_contract.py` | `freenet-contract-release.yml` |
| datapod | Freenet WASM | `contracts/datapod/` | `build_freenet_contract.py` | `freenet-contract-release.yml` |

All three contract crates are **standalone** (excluded from the workspace) because they target `wasm32-unknown-unknown` and have incompatible dependency trees.

## Prerequisites

- **Rust stable** with `wasm32-unknown-unknown` target:
  ```
  rustup target add wasm32-unknown-unknown
  ```
- **Stellar CLI v22.0.0** (for hvym-freenet-service only):
  ```
  cargo install stellar-cli --version 22.0.0 --locked
  ```
- **Python 3** (for build/deploy scripts)
- **Stellar testnet account** with funded identity (for deployment)

---

## hvym-freenet-service (Soroban Contract)

### Local Build

```bash
python contracts/build_contract.py              # Build + optimize
python contracts/build_contract.py --no-optimize # Build only (faster)
```

The script runs:
1. `stellar contract build` in `contracts/hvym-freenet-service/`
2. `stellar contract optimize --wasm target/wasm32-unknown-unknown/release/hvym_freenet_service.wasm`
3. Copies the result to `contracts/wasm/`

**Output:**
- Optimized: `contracts/wasm/hvym_freenet_service.optimized.wasm`
- Unoptimized (`--no-optimize`): `contracts/wasm/hvym_freenet_service.wasm`

### Local Deploy

```bash
# Set up deployer identity (first time only)
stellar keys generate testnet_DEPLOYER --network testnet --fund

# Deploy
python contracts/deploy_contract.py --deployer-acct testnet_DEPLOYER --network testnet
```

The deploy script:
1. Loads constructor args from `contracts/hvym_freenet_service_args.json`
2. Uploads WASM: `stellar contract install --wasm [path] --source [deployer] --network [network]`
3. Resolves the deployer address: `stellar keys address [admin_identity]`
4. Gets the native XLM SAC address: `stellar contract id asset --asset native --network [network]`
5. Deploys: `stellar contract deploy --wasm-hash [hash] --source [deployer] --network [network] -- --admin [address] --burn_bps [bps] --token [xlm_address]`
6. Saves results to `contracts/deployments.json`

### Constructor Args

File: `contracts/hvym_freenet_service_args.json`

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

### GitHub CI — Release (`contract-release.yml`)

**Trigger:** Push tag matching `release-hvym-freenet-service-v*`

**Steps:**
1. Install Rust + `wasm32-unknown-unknown` target
2. Install Stellar CLI v22.0.0
3. `stellar contract build` → `stellar contract optimize`
4. Copy optimized WASM to `contracts/wasm/`
5. Create GitHub Release with `hvym_freenet_service.optimized.wasm` attached

**Example:**
```bash
git tag release-hvym-freenet-service-v0.1.0
git push --tags
```

### GitHub CI — Deploy (`contract-deploy.yml`)

**Trigger:** Push tag matching `deploy-hvym-freenet-service-v*-testnet` or `deploy-hvym-freenet-service-v*-mainnet`

**Requires:**
- A prior release build (the workflow downloads the WASM from the matching GitHub Release)
- `STELLAR_DEPLOYER_SECRET` repository secret (deployer's Stellar secret key)

**Steps:**
1. Extract version and network from the tag name
2. Download `hvym_freenet_service.optimized.wasm` from the corresponding release
3. Install Stellar CLI v22.0.0
4. Set up deployer identity from `STELLAR_DEPLOYER_SECRET`
5. Run `deploy_contract.py`
6. Commit updated `contracts/deployments.json` to main

**Example:**
```bash
git tag deploy-hvym-freenet-service-v0.1.0-testnet
git push --tags
```

### Full Release + Deploy Workflow

```bash
# 1. Build: CI creates GitHub Release with WASM artifact
git tag release-hvym-freenet-service-v0.1.0
git push --tags

# 2. Deploy: CI deploys to testnet and commits deployments.json
git tag deploy-hvym-freenet-service-v0.1.0-testnet
git push --tags
```

---

## deposit-index (Freenet WASM Contract)

### Local Build

```bash
python contracts/build_freenet_contract.py --contract deposit-index
```

**Output:** `contracts/wasm/deposit_index.wasm` (~367 KB)

The crate has aggressive size optimizations in its release profile (`opt-level = "z"`, LTO, single codegen unit, symbol stripping).

Or build directly:
```bash
cd contracts/deposit-index
cargo build --target wasm32-unknown-unknown --release
```

### Tests

```bash
cd contracts/deposit-index
cargo test
```

19 unit tests covering mock SCP envelopes with real Ed25519 signatures.

### GitHub CI — Release (`freenet-contract-release.yml`)

**Trigger:** Push tag matching `release-deposit-index-v*`

**Example:**
```bash
git tag release-deposit-index-v0.1.0
git push --tags
```

Creates a GitHub Release with `deposit_index.wasm` attached.

### Deployment

Via `fdev` tool or Freenet node API (contract PUT operation). Deployment requires specifying `DepositIndexParams` containing validator public keys and quorum configuration.

---

## datapod (Freenet WASM Contract)

### Local Build

```bash
python contracts/build_freenet_contract.py --contract datapod
```

**Output:** `contracts/wasm/datapod_contract.wasm`

Has `freenet.toml` with `[contract] lang = "rust"`.

Or build directly:
```bash
cd contracts/datapod
cargo build --target wasm32-unknown-unknown --release
```

### GitHub CI — Release (`freenet-contract-release.yml`)

**Trigger:** Push tag matching `release-datapod-v*`

**Example:**
```bash
git tag release-datapod-v0.1.0
git push --tags
```

Creates a GitHub Release with `datapod_contract.wasm` attached.

### Deployment

Via `fdev` tool or Freenet node API. Each instance uses different `DatapodParams` (creator pubkey, recipient pubkey) to produce a unique `ContractKey`.

---

## Directory Structure

```
contracts/
├── hvym-freenet-service/              # Soroban contract (standalone crate)
├── deposit-index/                     # Freenet WASM contract (standalone crate)
├── datapod/                           # Freenet WASM contract (standalone crate)
├── wasm/                              # Build output directory
├── build_contract.py                  # Build hvym-freenet-service (Soroban)
├── build_freenet_contract.py          # Build Freenet WASM contracts (deposit-index, datapod)
├── deploy_contract.py                 # Deploy hvym-freenet-service
├── hvym_freenet_service_args.json     # Constructor args
├── deployments.json                   # Deployment tracking (generated by deploy)
└── STELLAR_CONTRACTS.md               # This file
```

## Environment Variables & Secrets

| Variable / Secret | Scope | Purpose |
|-------------------|-------|---------|
| `STELLAR_DEPLOYER_SECRET` | GitHub Actions secret | Deployer Stellar secret key |
| `GITHUB_TOKEN` | Auto-provided by GitHub | Release creation, WASM download |

## Architecture Docs

- [`docs/architecture/lepus/README.md`](../docs/architecture/lepus/README.md) — Lepus overview
- [`docs/architecture/lepus/stellar-contract.md`](../docs/architecture/lepus/stellar-contract.md) — Soroban contract design
- [`docs/architecture/lepus/deposit-index-contract.md`](../docs/architecture/lepus/deposit-index-contract.md) — Deposit-index contract design
- [`docs/architecture/lepus/datapod-contract.md`](../docs/architecture/lepus/datapod-contract.md) — Datapod contract design
