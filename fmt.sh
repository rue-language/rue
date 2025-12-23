#!/bin/bash
set -euo pipefail

# Format or check Rust code formatting using the hermetic Rust toolchain
#
# Usage:
#   ./fmt.sh        # Format all Rust files
#   ./fmt.sh check  # Check formatting (for CI)

MODE="${1:-format}"

# Get absolute paths to all Rust files (buck2 run changes working directory)
RUST_FILES=$(find "$(pwd)/crates" -name "*.rs" -type f)

if [ -z "$RUST_FILES" ]; then
    echo "No Rust files found"
    exit 0
fi

if [ "$MODE" = "check" ]; then
    echo "Checking Rust formatting..."
    echo "$RUST_FILES" | xargs ./buck2 run toolchains//rust:rustfmt -- --edition 2024 --check
    echo "All files formatted correctly!"
else
    echo "Formatting Rust files..."
    echo "$RUST_FILES" | xargs ./buck2 run toolchains//rust:rustfmt -- --edition 2024
    echo "Done!"
fi
