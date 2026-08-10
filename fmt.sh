#!/bin/bash
set -euo pipefail

# Format Rust code using the hermetic Rust toolchain.
#
# Usage:
#   ./fmt.sh        # Format all Rust files
#
# Write mode only, and it stays a script for one reason: build and test
# actions must not mutate the source tree, so formatting-in-place cannot be a
# Buck target. Checking has no such constraint and is not here — each crate
# emits its own `<name>-fmt-check` test from the rue_crate/rue_binary macros,
# so check coverage follows the build graph instead of this script's file
# discovery (RUE-1152, RUE-1153).
#
# BUCK/.bzl files are NOT formatted: no Starlark formatter is vendored in
# this repo (`buck2 starlark` offers only lint/typecheck, and buildifier is
# not a dependency), so this script covers Rust sources only.

cd "$(dirname "$0")"

if [ "$#" -gt 0 ]; then
    echo "fmt.sh: check mode has moved to the per-crate <name>-fmt-check tests (RUE-1153)." >&2
    echo "Run: ./buck2 test \$(./buck2 uquery \"kind('sh_test', 'root//...')\" | grep -- '-fmt-check\$')" >&2
    exit 2
fi

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

echo "Formatting ${#RUST_FILES[@]} Rust files..."
./buck2 run toolchains//rust:rustfmt -- \
    --edition 2024 "${RUST_FILES[@]}"
echo "Done!"
