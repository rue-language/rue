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

# Resolve the hermetic rustfmt binary once and invoke it directly. The old
# `xargs ./buck2 run toolchains//rust:rustfmt` form paid a buck2 CLI
# round-trip per xargs chunk; one `--show-output` resolve avoids that.
# --show-output prints a repo-root-relative path, so make it absolute.
RUSTFMT="$(./buck2 build toolchains//rust:rustfmt --show-output 2>/dev/null \
    | awk '/rust:rustfmt /{print $2}' | tail -1)"
if [ -z "$RUSTFMT" ]; then
    echo "fmt.sh: failed to resolve rustfmt via buck2 --show-output; re-running for diagnosis..." >&2
    ./buck2 build toolchains//rust:rustfmt --show-output >&2
    exit 1
fi
RUSTFMT="$(pwd)/$RUSTFMT"

# rustfmt needs librustc_driver from the distribution's rustc/lib directory
# (the toolchains//rust rustfmt rule's RunInfo wrapper does the same; see
# _rustfmt_impl in toolchains/rust/defs.bzl). The binary lives at
# <dist>/rustfmt-preview/bin/rustfmt, the libs at <dist>/rustc/lib.
DIST_ROOT="${RUSTFMT%/rustfmt-preview/bin/rustfmt}"
export LD_LIBRARY_PATH="$DIST_ROOT/rustc/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export DYLD_LIBRARY_PATH="$DIST_ROOT/rustc/lib${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"

# Absolute paths to all Rust files (no filenames with spaces in this repo).
RUST_FILES=$(find "$(pwd)/crates" -name "*.rs" -type f)

if [ -z "$RUST_FILES" ]; then
    echo "No Rust files found"
    exit 0
fi

if [ "$MODE" = "check" ]; then
    echo "Checking Rust formatting..."
    echo "$RUST_FILES" | xargs "$RUSTFMT" --edition 2024 --check
    echo "All files formatted correctly!"
else
    echo "Formatting Rust files..."
    echo "$RUST_FILES" | xargs "$RUSTFMT" --edition 2024
    echo "Done!"
fi
