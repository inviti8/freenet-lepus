#!/usr/bin/env python3
"""Build and optimize the hvym-freenet-service Soroban contract.

Usage:
    python contracts/build_contract.py                 # Build + optimize
    python contracts/build_contract.py --no-optimize   # Build only (faster dev)

Requires: stellar-cli v25.1.0 (wasm-opt included by default)
    cargo install stellar-cli --version 25.1.0 --locked
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

    # CLI v25.1.0: --optimize applies wasm-opt in-place; output is always
    # <name>.wasm (no .optimized suffix like the old separate optimize command).
    build_cmd = ["stellar", "contract", "build"]
    if not args.no_optimize:
        build_cmd.append("--optimize")
        print("=== Building and optimizing hvym-freenet-service ===")
    else:
        print("=== Building hvym-freenet-service ===")

    run(build_cmd, cwd=CONTRACT_DIR)

    # The WASM is in the target dir; copy to output dir with the expected name
    import glob as _glob

    wasm_pattern = os.path.join(CONTRACT_DIR, "target", "*", "release", "hvym_freenet_service.wasm")
    matches = _glob.glob(wasm_pattern)
    if not matches:
        print(f"ERROR: WASM not found matching {wasm_pattern}", file=sys.stderr)
        sys.exit(1)

    built_wasm = matches[0]

    if args.no_optimize:
        output = os.path.join(OUTPUT_DIR, "hvym_freenet_service.wasm")
    else:
        output = os.path.join(OUTPUT_DIR, "hvym_freenet_service.optimized.wasm")

    import shutil
    shutil.copy2(built_wasm, output)

    size = os.path.getsize(output)
    print(f"=== Done: {output} ({size:,} bytes) ===")


if __name__ == "__main__":
    main()
