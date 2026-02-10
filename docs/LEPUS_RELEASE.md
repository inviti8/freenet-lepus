# Lepus Release Guide

How to create an official release of Freenet Lepus, including core binaries, crate publishing, and contract builds.

---

## GitHub Repository Setup (One-Time)

Before your first release, configure the following in your GitHub repository settings.

### 1. Workflow Permissions

**Settings > Actions > General > Workflow permissions:**
- Select **Read and write permissions**
- Check **Allow GitHub Actions to create and approve pull requests**

These are required because the release workflow creates a version-bump PR and pushes tags, and the contract workflows commit built WASMs back to `main`.

### 2. Repository Secrets

**Settings > Secrets and variables > Actions > Repository secrets:**

| Secret | Required For | How to Obtain |
|--------|-------------|---------------|
| `CARGO_REGISTRY_TOKEN` | Publishing to crates.io | Go to [crates.io/settings/tokens](https://crates.io/settings/tokens), create a token with publish scope for `freenet` and `fdev` |
| `RELEASE_PAT` | Release PR creation (optional) | Create a GitHub [Personal Access Token](https://github.com/settings/tokens) with `repo` scope. If not set, the workflow falls back to `GITHUB_TOKEN`, which may not trigger other CI workflows on the PR |
| `STELLAR_DEPLOYER_SECRET` | Soroban contract deployment | Stellar secret key (starts with `S...`). Generate with `stellar keys generate deployer --network testnet --fund`, retrieve with `stellar keys show deployer` |

> `GITHUB_TOKEN` is provided automatically — you do not need to create it.

---

## Release Process Overview

A full release has three independent parts. Only step 1 is required; steps 2 and 3 are done when contracts have changed.

| Step | What | Trigger | Workflow |
|------|------|---------|----------|
| 1 | Core release (freenet + fdev) | Manual dispatch | `release.yml` + `cross-compile.yml` |
| 2 | Freenet WASM contracts | Tag push | `freenet-contract-release.yml` |
| 3 | Soroban contract | Tag push | `contract-release.yml` + `contract-deploy.yml` |

---

## Step 1: Core Release (freenet + fdev)

This is the main release. It bumps versions, publishes crates, creates a GitHub Release, and attaches cross-compiled binaries.

### 1a. Trigger the Release Workflow

Go to **Actions > Release > Run workflow** and fill in:

| Input | Description | Example |
|-------|-------------|---------|
| `version` | Semver version to release | `0.1.31` |
| `skip_tests` | Skip CI tests before merge (optional) | `false` |
| `dry_run` | Preview what would happen without publishing | `false` |

Or use the GitHub CLI:
```bash
gh workflow run release.yml -f version=0.1.31
```

### 1b. What Happens Automatically

The `release.yml` workflow runs through these stages:

1. **Validate** — Checks version format (must be `X.Y.Z`)
2. **Version bump** — Updates `Cargo.toml` versions for `freenet` and `fdev`, creates a PR (e.g., `release/v0.1.31`)
3. **Auto-merge** — Enables squash auto-merge on the PR; waits for CI to pass and merge
4. **Publish to crates.io** — Publishes `freenet` then `fdev` (with a 30s propagation wait between them)
5. **Tag and release** — Creates git tag `v0.1.31`, pushes it, creates a GitHub Release with auto-generated release notes

### 1c. Cross-Compiled Binaries (Automatic)

The tag push from step 1b automatically triggers `cross-compile.yml`, which:

1. Builds `freenet` and `fdev` for four targets in parallel:
   - `x86_64-unknown-linux-musl`
   - `aarch64-unknown-linux-musl`
   - `aarch64-apple-darwin`
   - `x86_64-pc-windows-msvc`
2. Packages binaries as `.tar.gz` (Linux/macOS) and `.zip` (Windows)
3. Generates `SHA256SUMS.txt`
4. Uploads all archives to the GitHub Release created in step 1b

The complete set of release assets:
```
freenet-x86_64-unknown-linux-musl.tar.gz
fdev-x86_64-unknown-linux-musl.tar.gz
freenet-aarch64-unknown-linux-musl.tar.gz
fdev-aarch64-unknown-linux-musl.tar.gz
freenet-aarch64-apple-darwin.tar.gz
fdev-aarch64-apple-darwin.tar.gz
freenet-x86_64-pc-windows-msvc.zip
fdev-x86_64-pc-windows-msvc.zip
SHA256SUMS.txt
```

### 1d. Verify

After both workflows complete (~10–15 minutes total):

1. Check the [Releases page](../../releases) — the release should have all 9 assets attached
2. Verify crates.io:
   - https://crates.io/crates/freenet
   - https://crates.io/crates/fdev

---

## Step 2: Freenet WASM Contract Release (deposit-index, datapod)

Only needed when the contract code has changed.

### Tag and Push

```bash
# deposit-index
git tag release-deposit-index-v0.1.0
git push --tags

# datapod
git tag release-datapod-v0.1.0
git push --tags
```

**Trigger:** Tag matching `release-deposit-index-v*` or `release-datapod-v*`

**What happens** (`freenet-contract-release.yml`):
1. Builds the WASM with `cargo build --target wasm32-unknown-unknown --release`
2. Commits the built WASM to `contracts/wasm/` on `main`
3. Creates a GitHub Release with the WASM attached

---

## Step 3: Soroban Contract Release + Deploy (hvym-freenet-service)

Only needed when the Soroban contract code has changed. This is a two-step process: release (build), then deploy.

### 3a. Release (Build)

```bash
git tag release-hvym-freenet-service-v0.1.0
git push --tags
```

**Trigger:** Tag matching `release-hvym-freenet-service-v*`

**What happens** (`contract-release.yml`):
1. Installs Stellar CLI v25.1.0
2. Runs `stellar contract build --optimize`
3. Creates a GitHub Release with `hvym_freenet_service.optimized.wasm` attached

### 3b. Deploy

Wait for the release workflow to complete, then:

```bash
# Deploy to testnet
git tag deploy-hvym-freenet-service-v0.1.0-testnet
git push --tags

# Deploy to mainnet
git tag deploy-hvym-freenet-service-v0.1.0-mainnet
git push --tags
```

**Trigger:** Tag matching `deploy-hvym-freenet-service-v*-testnet` or `deploy-hvym-freenet-service-v*-mainnet`

**Requires:** `STELLAR_DEPLOYER_SECRET` repository secret.

**What happens** (`contract-deploy.yml`):
1. Downloads the optimized WASM from the matching GitHub Release
2. Sets up deployer identity from `STELLAR_DEPLOYER_SECRET`
3. Deploys to the Stellar network
4. Commits updated `contracts/deployments.json` to `main`

---

## Putting It All Together

A full release where everything has changed:

```bash
# 1. Core release — trigger via GitHub UI or CLI
gh workflow run release.yml -f version=0.1.31
# Wait for release.yml to finish (creates tag v0.1.31)
# Wait for cross-compile.yml to finish (attaches binaries)

# 2. Freenet WASM contracts (if changed)
git tag release-deposit-index-v0.1.0 && git push --tags
git tag release-datapod-v0.1.0 && git push --tags

# 3. Soroban contract (if changed)
git tag release-hvym-freenet-service-v0.1.0 && git push --tags
# Wait for contract-release.yml to finish...
git tag deploy-hvym-freenet-service-v0.1.0-testnet && git push --tags
```

---

## Troubleshooting

### Binaries not attached to GitHub Release
The `cross-compile.yml` `attach-to-release` job only runs on `v*` tag pushes. If triggered by a plain push to `main`, binaries are built but not attached. This is by design — use the `release.yml` workflow to create proper versioned releases.

### Release PR not triggering CI
If the version-bump PR doesn't trigger CI workflows, you need to set `RELEASE_PAT` (a Personal Access Token). PRs created by `GITHUB_TOKEN` don't trigger other workflows as a GitHub security measure.

### crates.io publish fails
Ensure `CARGO_REGISTRY_TOKEN` is set and the token has publish scope for both `freenet` and `fdev` crates. The token owner must be listed as an owner on the crates.

### Soroban deploy fails
Ensure `STELLAR_DEPLOYER_SECRET` is set and the account has sufficient XLM balance on the target network. For testnet, fund via `stellar keys generate <name> --network testnet --fund`.

### Contract release tag pushed before code is on main
Tag-triggered workflows check out the tagged commit. Make sure your contract changes are merged to `main` before pushing the release tag.
