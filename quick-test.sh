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

echo "Checking production debug-assertion policy..."
scripts/validate-debug-assert-policy.py

echo "Checking Buck test-tier ownership..."
./buck2 bxl //test_tiers.bxl:validate

echo "Running unit tests (quick mode)..."
# Run every crate's unit tests; //... avoids a hand-maintained list that
# silently goes stale as crates are added; scoping to //crates/ keeps vendored
# third-party targets out of the test set. Non-unit integration harnesses and
# dedicated suites such as the structural scaling matrix run in full/CI
# coverage, not fast iteration.
# (RUE-132, RUE-1157)
./buck2 test //crates/... \
    --include rue_test_tier_premerge \
    --exclude rue_not_quick \
    --exclude rue_dedicated_suite \
    --always-exclude \
    --ignore-tests-attribute

echo ""
echo "Unit tests passed! Run ./test.sh for full verification before committing."
