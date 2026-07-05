#!/usr/bin/env bash
set -euo pipefail

# Run all tests for the rue compiler.
#
# Every suite is a Buck test target, so one `buck2 test //...` runs the unit
# tests for ALL crates plus the spec/UI/CLI harness suites, tutorial snippet
# checker, spec traceability gate, and ADR registry validator (the root BUCK
# file's sh_tests, which declare the rue binary, cases/, std/, docs/, and
# tutorial markdown as inputs — Buck owns the binary handoff instead of a shell
# pipeline). Using //...
# rather than a hand-maintained target list also keeps new crates from being
# silently omitted. (RUE-132, RUE-144)
#
# With a pattern argument, the spec/UI/CLI suites are instead run directly
# with the filter (unit tests still run in full, as before).

cd "$(dirname "$0")"

if [[ $# -eq 0 ]]; then
    echo "Running unit tests and spec/UI/CLI suites..."
    ./buck2 test //...
else
    # Unit tests live under //crates/...; the suite sh_tests are at the repo
    # root, so this scope keeps them out of the unfiltered step below.
    echo "Running unit tests..."
    ./buck2 test //crates/...

    # Get the path to the rue binary (this also builds it if needed).
    # scripts/rue-bin resolves it to a stable absolute path.
    RUE_BINARY="$(./scripts/rue-bin)"

    echo "Running spec tests..."
    RUE_BINARY="$RUE_BINARY" \
    RUE_SPEC_CASES="crates/rue-spec/cases" \
    ./buck2 run //crates/rue-spec:rue-spec -- --quiet "$@"

    echo "Running UI tests..."
    RUE_BINARY="$RUE_BINARY" \
    RUE_UI_CASES="crates/rue-ui-tests/cases" \
    ./buck2 run //crates/rue-ui-tests:rue-ui-tests -- --quiet "$@"

    echo "Running CLI integration tests..."
    RUE_BINARY="$RUE_BINARY" \
    RUE_CLI_CASES="crates/rue-cli-tests/cases" \
    RUE_EXAMPLES_DIR="examples" \
    RUE_STD_DIR="std" \
    ./buck2 run //crates/rue-cli-tests:rue-cli-tests -- --quiet "$@"

    echo "Running repository quality gates..."
    ./buck2 test //:spec-traceability //:adr-registry-validation
fi
