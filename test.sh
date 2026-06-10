#!/usr/bin/env bash
set -euo pipefail

# Run all tests for the rue compiler

cd "$(dirname "$0")"

# Run unit tests for ALL crates. Use //... rather than a hand-maintained target
# list: the old list silently omitted ~300 tests across 7 crates (rue,
# rue-runtime, rue-fuzz, rue-test-runner, rue-builtins, rue-spec, rue-cli-tests)
# because nobody remembered to add new crates here. (RUE-132)
echo "Running unit tests..."
./buck2 test //...

# Get the path to the rue binary (this also builds it if needed).
# scripts/rue-bin resolves it to a stable absolute path.
RUE_BINARY="$(./scripts/rue-bin)"

# Run spec tests (buck2 run will build rue-spec if needed)
echo "Running spec tests..."
RUE_BINARY="$RUE_BINARY" \
RUE_SPEC_CASES="crates/rue-spec/cases" \
./buck2 run //crates/rue-spec:rue-spec -- --quiet "$@"

# Run traceability check (fails if coverage < 100% or orphan references exist)
echo "Running spec traceability check..."
RUE_SPEC_DIR="docs/spec/src" \
RUE_SPEC_CASES="crates/rue-spec/cases" \
./buck2 run //crates/rue-spec:rue-spec -- --traceability

# Run UI tests (compiler-specific tests like warnings)
echo "Running UI tests..."
RUE_BINARY="$RUE_BINARY" \
RUE_UI_CASES="crates/rue-ui-tests/cases" \
./buck2 run //crates/rue-ui-tests:rue-ui-tests -- --quiet "$@"

# Run CLI integration tests (end-to-end through the real driver: files on
# disk, relative paths, stdin, exit codes; catches ICEs and driver-only bugs)
echo "Running CLI integration tests..."
RUE_BINARY="$RUE_BINARY" \
RUE_CLI_CASES="crates/rue-cli-tests/cases" \
RUE_STD_DIR="std" \
./buck2 run //crates/rue-cli-tests:rue-cli-tests -- --quiet "$@"
