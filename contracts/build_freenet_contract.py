#!/usr/bin/env python3
"""Build a Freenet WASM contract (deposit-index, datapod, etc.).

Usage:
    python contracts/build_freenet_contract.py --contract deposit-index
    python contracts/build_freenet_contract.py --contract datapod
"""

import argparse
import os
import re
import shutil
import subprocess
import sys

CONTRACTS_DIR = os.path.dirname(os.path.abspath(__file__))
WASM_TARGET = "wasm32-unknown-unknown"
OUTPUT_DIR = os.path.join(CONTRACTS_DIR, "wasm")


def run(cmd: list[str], cwd: str | None = None) -> None:
    print(f"  > {' '.join(cmd)}")
    subprocess.check_call(cmd, cwd=cwd)


def read_package_name(contract_dir: str) -> str:
    """Read the package name from Cargo.toml."""
    cargo_toml = os.path.join(contract_dir, "Cargo.toml")
    with open(cargo_toml) as f:
        content = f.read()
    match = re.search(r'\[package\].*?name\s*=\s*"([^"]+)"', content, re.DOTALL)
    if not match:
        print(f"ERROR: Could not find package name in {cargo_toml}", file=sys.stderr)
        sys.exit(1)
    return match.group(1)


def main() -> None:
    parser = argparse.ArgumentParser(description="Build a Freenet WASM contract")
    parser.add_argument(
        "--contract",
        required=True,
        help="Contract directory name under contracts/ (e.g. deposit-index, datapod)",
    )
    args = parser.parse_args()

    contract_dir = os.path.join(CONTRACTS_DIR, args.contract)
    if not os.path.isdir(contract_dir):
        print(f"ERROR: Contract directory not found: {contract_dir}", file=sys.stderr)
        sys.exit(1)

    cargo_toml = os.path.join(contract_dir, "Cargo.toml")
    if not os.path.isfile(cargo_toml):
        print(f"ERROR: No Cargo.toml in {contract_dir}", file=sys.stderr)
        sys.exit(1)

    pkg_name = read_package_name(contract_dir)
    # WASM filename: hyphens become underscores
    wasm_filename = pkg_name.replace("-", "_") + ".wasm"
    wasm_build_path = os.path.join(
        contract_dir, "target", WASM_TARGET, "release", wasm_filename
    )

    # Step 1: Build
    print(f"=== Building {args.contract} ({pkg_name}) ===")
    run(
        ["cargo", "build", "--target", WASM_TARGET, "--release"],
        cwd=contract_dir,
    )

    if not os.path.isfile(wasm_build_path):
        print(f"ERROR: WASM not found at {wasm_build_path}", file=sys.stderr)
        sys.exit(1)

    # Step 2: Copy to output
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    dest = os.path.join(OUTPUT_DIR, wasm_filename)
    shutil.copy2(wasm_build_path, dest)

    size = os.path.getsize(dest)
    print(f"=== Copied to {dest} ({size:,} bytes) ===")


if __name__ == "__main__":
    main()
