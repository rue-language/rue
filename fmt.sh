#!/bin/bash
set -euo pipefail

# Format or check Rust code formatting
#
# Usage:
#   ./fmt.sh        # Format all Rust files
#   ./fmt.sh check  # Check formatting (for CI)

MODE="${1:-format}"

RUST_FILES=$(find crates -name "*.rs" -type f)

if [ -z "$RUST_FILES" ]; then
    echo "No Rust files found"
    exit 0
fi

if [ "$MODE" = "check" ]; then
    echo "Checking Rust formatting..."
    echo "$RUST_FILES" | xargs rustfmt --edition 2024 --check
    echo "All files formatted correctly!"
else
    echo "Formatting Rust files..."
    echo "$RUST_FILES" | xargs rustfmt --edition 2024
    echo "Done!"
fi
