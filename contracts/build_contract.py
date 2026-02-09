#!/usr/bin/env python3
"""Build and optimize the hvym-freenet-service Soroban contract.

Usage:
    python contracts/build_contract.py                 # Build + optimize
    python contracts/build_contract.py --no-optimize   # Build only (faster dev)
"""

import argparse
import os
import shutil
import subprocess
import sys

CONTRACT_DIR = os.path.join(os.path.dirname(__file__), "hvym-freenet-service")
WASM_TARGET = "wasm32-unknown-unknown"
WASM_REL_PATH = os.path.join(
    "target", WASM_TARGET, "release", "hvym_freenet_service.wasm"
)
OUTPUT_DIR = os.path.join(os.path.dirname(__file__), "wasm")
OUTPUT_WASM = os.path.join(OUTPUT_DIR, "hvym_freenet_service.optimized.wasm")


def run(cmd: list[str], cwd: str | None = None) -> None:
    print(f"  > {' '.join(cmd)}")
    subprocess.check_call(cmd, cwd=cwd)


def main() -> None:
    parser = argparse.ArgumentParser(description="Build hvym-freenet-service contract")
    parser.add_argument(
        "--no-optimize",
        action="store_true",
        help="Skip WASM optimization step",
    )
    args = parser.parse_args()

    if not os.path.isdir(CONTRACT_DIR):
        print(f"ERROR: Contract directory not found: {CONTRACT_DIR}", file=sys.stderr)
        sys.exit(1)

    # Step 1: Build
    print("=== Building hvym-freenet-service ===")
    run(["stellar", "contract", "build"], cwd=CONTRACT_DIR)

    wasm_path = os.path.join(CONTRACT_DIR, WASM_REL_PATH)
    if not os.path.isfile(wasm_path):
        print(f"ERROR: WASM not found at {wasm_path}", file=sys.stderr)
        sys.exit(1)

    # Step 2: Optimize (unless --no-optimize)
    if not args.no_optimize:
        print("=== Optimizing WASM ===")
        run(["stellar", "contract", "optimize", "--wasm", wasm_path])

    # Step 3: Copy to output
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    if args.no_optimize:
        dest = os.path.join(OUTPUT_DIR, "hvym_freenet_service.wasm")
        shutil.copy2(wasm_path, dest)
        print(f"=== Copied to {dest} ===")
    else:
        optimized = wasm_path  # stellar optimize overwrites in place
        shutil.copy2(optimized, OUTPUT_WASM)
        print(f"=== Copied to {OUTPUT_WASM} ===")

    size = os.path.getsize(OUTPUT_WASM if not args.no_optimize else dest)
    print(f"=== Done ({size:,} bytes) ===")


if __name__ == "__main__":
    main()
