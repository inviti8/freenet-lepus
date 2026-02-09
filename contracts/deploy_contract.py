#!/usr/bin/env python3
"""Deploy the hvym-freenet-service Soroban contract.

Usage:
    python contracts/deploy_contract.py --deployer-acct testnet_DEPLOYER --network testnet

Steps:
    1. Upload WASM to the network
    2. Deploy contract with constructor args
    3. Save deployment info to contracts/deployments.json
"""

import argparse
import json
import os
import subprocess
import sys

WASM_PATH = os.path.join(
    os.path.dirname(__file__), "wasm", "hvym_freenet_service.optimized.wasm"
)
ARGS_FILE = os.path.join(
    os.path.dirname(__file__), "hvym_freenet_service_args.json"
)
DEPLOYMENTS_FILE = os.path.join(os.path.dirname(__file__), "deployments.json")


def run_capture(cmd: list[str]) -> str:
    print(f"  > {' '.join(cmd)}")
    result = subprocess.run(cmd, capture_output=True, text=True, check=True)
    return result.stdout.strip()


def main() -> None:
    parser = argparse.ArgumentParser(description="Deploy hvym-freenet-service")
    parser.add_argument(
        "--deployer-acct",
        required=True,
        help="Stellar CLI identity name for the deployer account",
    )
    parser.add_argument(
        "--network",
        required=True,
        choices=["testnet", "mainnet", "standalone"],
        help="Target network",
    )
    args = parser.parse_args()

    if not os.path.isfile(WASM_PATH):
        print(
            f"ERROR: WASM not found at {WASM_PATH}. Run build_contract.py first.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Load constructor args
    with open(ARGS_FILE) as f:
        constructor_args = json.load(f)

    admin_identity = constructor_args.get("admin", args.deployer_acct)

    # Step 1: Upload WASM
    print("=== Uploading WASM ===")
    wasm_hash = run_capture([
        "stellar", "contract", "install",
        "--wasm", WASM_PATH,
        "--source", args.deployer_acct,
        "--network", args.network,
    ])
    print(f"  WASM hash: {wasm_hash}")

    # Step 2: Deploy contract
    print("=== Deploying contract ===")

    # Get the deployer public key to use as admin
    deployer_address = run_capture([
        "stellar", "keys", "address", admin_identity,
    ])

    contract_id = run_capture([
        "stellar", "contract", "deploy",
        "--wasm-hash", wasm_hash,
        "--source", args.deployer_acct,
        "--network", args.network,
        "--",
        "--admin", deployer_address,
    ])
    print(f"  Contract ID: {contract_id}")

    # Step 3: Save deployment info
    deployments = {}
    if os.path.isfile(DEPLOYMENTS_FILE):
        with open(DEPLOYMENTS_FILE) as f:
            deployments = json.load(f)

    deployments[f"hvym-freenet-service-{args.network}"] = {
        "contract_id": contract_id,
        "wasm_hash": wasm_hash,
        "admin": deployer_address,
        "network": args.network,
    }

    with open(DEPLOYMENTS_FILE, "w") as f:
        json.dump(deployments, f, indent=2)
        f.write("\n")

    print(f"=== Deployment saved to {DEPLOYMENTS_FILE} ===")
    print(f"=== Done: {contract_id} ===")


if __name__ == "__main__":
    main()
