#!/usr/bin/env bash
set -euo pipefail

# Run all tests for the rue compiler.
#
# Every suite is a Buck test target, so one `buck2 test //...` runs the unit
# tests for ALL crates plus the spec/UI/CLI harness suites (the root BUCK
# file's sh_tests, which declare the rue binary, cases/, and std/ as inputs —
# Buck owns the binary handoff instead of a shell pipeline). Using //...
# rather than a hand-maintained target list also keeps new crates from being
# silently omitted. (RUE-132, RUE-144)
#
# With a pattern argument, the spec/UI/CLI suites are instead run directly
# with the filter (unit tests still run in full, as before).
#
# RUE_BUCK_MODIFIERS lets a caller inject Buck2 build modifiers into every
# buck2 invocation below (including the rue binary the suites compile with).
# CI uses it to run the whole suite against a RELEASE build of the compiler —
# `RUE_BUCK_MODIFIERS="--modifier //constraints:release" ./test.sh` — so
# release-only miscompiles (e.g. a correctness check that used to be a
# `debug_assert!` and vanished in release) are actually exercised (RUE-45).
# Empty by default, so the debug path is byte-for-byte unchanged.

cd "$(dirname "$0")"

# Split the modifier string into an array so word-splitting is explicit and
# safe under `set -u`; an empty value yields an empty array (expands to
# nothing). `|| true` absorbs read's EOF-nonzero on empty input under `set -e`.
IFS=' ' read -ra BUCK_MODIFIERS <<< "${RUE_BUCK_MODIFIERS:-}" || true

if [[ $# -eq 0 ]]; then
    echo "Running unit tests and spec/UI/CLI suites..."
    ./buck2 test //... "${BUCK_MODIFIERS[@]}"
else
    # Unit tests live under //crates/...; the suite sh_tests are at the repo
    # root, so this scope keeps them out of the unfiltered step below.
    echo "Running unit tests..."
    ./buck2 test //crates/... "${BUCK_MODIFIERS[@]}"

    # Get the path to the rue binary (this also builds it if needed).
    # scripts/rue-bin resolves it to a stable absolute path; the same
    # modifiers must reach it so the suites compile with the release binary.
    RUE_BINARY="$(./scripts/rue-bin "${BUCK_MODIFIERS[@]}")"

    echo "Running spec tests..."
    RUE_BINARY="$RUE_BINARY" \
    RUE_SPEC_CASES="crates/rue-spec/cases" \
    ./buck2 run //crates/rue-spec:rue-spec "${BUCK_MODIFIERS[@]}" -- --quiet "$@"

    echo "Running UI tests..."
    RUE_BINARY="$RUE_BINARY" \
    RUE_UI_CASES="crates/rue-ui-tests/cases" \
    ./buck2 run //crates/rue-ui-tests:rue-ui-tests "${BUCK_MODIFIERS[@]}" -- --quiet "$@"

    echo "Running CLI integration tests..."
    RUE_BINARY="$RUE_BINARY" \
    RUE_CLI_CASES="crates/rue-cli-tests/cases" \
    RUE_EXAMPLES_DIR="examples" \
    RUE_STD_DIR="std" \
    ./buck2 run //crates/rue-cli-tests:rue-cli-tests "${BUCK_MODIFIERS[@]}" -- --quiet "$@"
fi

# Run traceability check (fails if coverage < 100% or orphan references exist)
echo "Running spec traceability check..."
RUE_SPEC_DIR="docs/spec/src" \
RUE_SPEC_CASES="crates/rue-spec/cases" \
./buck2 run //crates/rue-spec:rue-spec "${BUCK_MODIFIERS[@]}" -- --traceability
