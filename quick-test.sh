#!/usr/bin/env bash
set -euo pipefail

# Quick test script for fast development iteration
# Runs only unit tests (no spec tests, no UI tests)
#
# Use this for:
# - Fast feedback during development (~2-5 seconds)
# - Iterating on code changes before full verification
#
# Before committing, run ./test.sh for full verification.

cd "$(dirname "$0")"

echo "Running unit tests (quick mode)..."
./buck2 test \
    //crates/rue-span:rue-span-test \
    //crates/rue-intern:rue-intern-test \
    //crates/rue-lexer:rue-lexer-test \
    //crates/rue-parser:rue-parser-test \
    //crates/rue-rir:rue-rir-test \
    //crates/rue-air:rue-air-test \
    //crates/rue-codegen:rue-codegen-test \
    //crates/rue-linker:rue-linker-test \
    //crates/rue-compiler:rue-compiler-test

echo ""
echo "Unit tests passed! Run ./test.sh for full verification before committing."
