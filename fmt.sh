#!/bin/bash
set -euo pipefail

# Format or check Rust code formatting using the hermetic Rust toolchain
#
# Usage:
#   ./fmt.sh        # Format all Rust files
#   ./fmt.sh check  # Check formatting (for CI)
#
# BUCK/.bzl files are NOT formatted: no Starlark formatter is vendored in
# this repo (`buck2 starlark` offers only lint/typecheck, and buildifier is
# not a dependency), so this script covers Rust sources only.

cd "$(dirname "$0")"

MODE="${1:-format}"

# Keep file discovery here because format mode intentionally edits the source
# tree. Toolchain selection and its runtime environment belong to Buck's
# rustfmt RunInfo, which `buck2 run` applies on every supported host.
RUST_FILES=()
while IFS= read -r -d '' rust_file; do
    RUST_FILES+=("$rust_file")
done < <(find "$(pwd)/crates" -name "*.rs" -type f -print0)

# Finding no sources is a broken checkout or a bad discovery change, never a
# clean tree. Exiting 0 here would report a pass for work never done, which is
# how //:fmt-check silently covered 1 file of 281 (RUE-1152).
if [ "${#RUST_FILES[@]}" -eq 0 ]; then
    echo "fmt.sh: no Rust files found under crates/ -- discovery is broken." >&2
    exit 1
fi

if [ "$MODE" = "check" ]; then
    echo "Checking Rust formatting (${#RUST_FILES[@]} files)..."
    ./buck2 run toolchains//rust:rustfmt -- \
        --edition 2024 --check "${RUST_FILES[@]}"
    echo "All ${#RUST_FILES[@]} files formatted correctly!"
else
    echo "Formatting Rust files..."
    ./buck2 run toolchains//rust:rustfmt -- \
        --edition 2024 "${RUST_FILES[@]}"
    echo "Done!"
fi
