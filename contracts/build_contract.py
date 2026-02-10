#!/usr/bin/env python3
"""Build and optimize the hvym-freenet-service Soroban contract.

Usage:
    python contracts/build_contract.py                 # Build + optimize
    python contracts/build_contract.py --no-optimize   # Build only (faster dev)
"""

import argparse
import os
import subprocess
import sys

CONTRACT_DIR = os.path.join(os.path.dirname(__file__), "hvym-freenet-service")
OUTPUT_DIR = os.path.join(os.path.dirname(__file__), "wasm")


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

    os.makedirs(OUTPUT_DIR, exist_ok=True)

    # Build (and optionally optimize) in one step.
    # stellar contract build --optimize produces <name>.optimized.wasm in --out-dir.
    # Without --optimize it produces <name>.wasm.
    cmd = ["stellar", "contract", "build", "--out-dir", OUTPUT_DIR]
    if not args.no_optimize:
        cmd.append("--optimize")

    print("=== Building hvym-freenet-service ===")
    run(cmd, cwd=CONTRACT_DIR)

    if args.no_optimize:
        output = os.path.join(OUTPUT_DIR, "hvym_freenet_service.wasm")
    else:
        output = os.path.join(OUTPUT_DIR, "hvym_freenet_service.optimized.wasm")

    if not os.path.isfile(output):
        print(f"ERROR: Expected WASM not found at {output}", file=sys.stderr)
        sys.exit(1)

    size = os.path.getsize(output)
    print(f"=== Done: {output} ({size:,} bytes) ===")


if __name__ == "__main__":
    main()
