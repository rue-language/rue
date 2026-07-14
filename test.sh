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
#
# READING THE RESULT (RUE-579). This script's EXIT CODE is authoritative:
# `buck2 test` returns non-zero when any target fails and `set -e` propagates
# it, so plain `./test.sh` (as CI runs it) and `./test.sh && ...` are correct.
# BUT a shell PIPE discards the exit code — `./test.sh 2>&1 | tail` reports the
# pipe's status (usually 0), not the suite's, so the `Tests finished: ... Fail
# N` tally can scroll past while `$?` reads 0. To guard against that, the last
# line this script prints is an unambiguous, greppable sentinel:
#
#     === TEST SUITE: PASSED ===
#     === TEST SUITE: FAILED (exit N) ===
#
# When you pipe or capture this script's output, grep for that line instead of
# trusting `$?`. (The tally text alone is not a verdict: a partial run can print
# a green-looking count and still have failed a later gate.)

cd "$(dirname "$0")"
repo_root="$PWD"

# Direct full-suite invocations also bootstrap an already-installed private
# cache config. This is a no-op when the user has not opted in.
if [[ -x scripts/provision-build-cache ]]; then
    scripts/provision-build-cache auto
fi

# A no-filter run is the host-wide full suite. Serialize it across independent
# Buck project roots before starting any build work; filtered runs stay free to
# run concurrently. The environment marker prevents recursion after the lock
# wrapper re-enters this script.
if [[ $# -eq 0 ]] && [[ "${RUE_FULL_SUITE_LOCK_HELD:-}" != 1 ]]; then
    exec ./scripts/with-full-suite-lock env RUE_FULL_SUITE_LOCK_HELD=1 "$0"
fi

# Always print the result sentinel, even on an early `set -e` exit, so a piped
# or captured run is self-describing (RUE-579).
print_test_suite_result() {
    local rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "=== TEST SUITE: PASSED ==="
    else
        echo "=== TEST SUITE: FAILED (exit $rc) ==="
    fi
}
trap print_test_suite_result EXIT

REPOSITORY_QUALITY_GATES=(
    //:benchmark-tool-tests
    //:tutorial-snippet-tool-tests
    //:tutorial-snippet-tests
    //:spec-traceability
    //:adr-registry-validation
)

if [[ $# -eq 0 ]]; then
    # Keep discovery broad so new crate and repository tests cannot disappear
    # behind a hand-maintained list. Heavy opaque harnesses are labeled in BUCK:
    # query that label from the live graph, exclude it from the broad pass, then
    # run every returned target alone. A newly labeled target is automatically
    # included without ever putting the whole heavy set in one test invocation.
    HEAVY_SUITES=()
    while IFS= read -r suite; do
        [[ -n "$suite" ]] && HEAVY_SUITES+=("$suite")
    done < <(./buck2 uquery 'attrfilter(labels, rue_heavy_suite, //...)')
    if [[ ${#HEAVY_SUITES[@]} -eq 0 ]]; then
        echo "error: Buck query found no rue_heavy_suite targets" >&2
        exit 1
    fi

    echo "Running unit tests and lightweight repository checks..."
    ./buck2 test //... --exclude rue_heavy_suite --always-exclude
    for suite in "${HEAVY_SUITES[@]}"; do
        echo "Running heavy suite $suite..."
        ./buck2 test "$suite"
    done
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
    RUE_EXAMPLES_DIR="$repo_root/examples" \
    RUE_STD_DIR="std" \
    ./buck2 run //crates/rue-cli-tests:rue-cli-tests -- --quiet "$@"

    echo "Running repository quality gates..."
    ./buck2 test "${REPOSITORY_QUALITY_GATES[@]}"
fi
