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
# Run every crate's unit tests; //... avoids a hand-maintained list that
# silently goes stale as crates are added. (RUE-132)
./buck2 test //...

echo ""
echo "Unit tests passed! Run ./test.sh for full verification before committing."
