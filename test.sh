#!/usr/bin/env bash
set -euo pipefail

# Run the canonical Rue test selection.
#
# Every suite is a Buck test target. With no environment override this retains
# the standard full-suite behavior (premerge + slow, with resource-stress tests
# opt in); `RUE_TEST_TIER=premerge|slow|stress|all` selects a canonical
# execution tier or their complete union from the same Buck metadata used by
# `//test_tiers.bxl:{premerge,slow,stress,all}`.
#
# One broad `buck2 test //...` runs the unit
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
#
# CORPUS COMPLETENESS (RUE-924, reworked by RUE-1163). A green summary is only
# trustworthy if the compiler-consuming corpus harnesses actually EXECUTED: a
# suite that never ran cannot fail, and one such omission once let a Pass tally
# hide five real CLI-case failures that CI caught. This script used to defend
# that by tee'ing every invocation into a log and grepping for a result line per
# entry in a hand-maintained required-corpus list.
#
# That defense is now structural instead. Heavy suites come from
# `//test_tiers.bxl:heavy_suites`, which computes membership from the same
# labels that define the tiers, so the list cannot go stale against BUCK. Each
# one is then run as a NAMED target: a misspelled or deleted target is a Buck
# error rather than a quietly missing log line, and a `//...` pattern can no
# longer narrow underneath the run. And since RUE-1118/RUE-1163 every corpus is
# a build action whose stamp its test asserts, so buck2 exiting 0 for a named
# corpus means that corpus passed — in this run or in a cached one over the same
# inputs. Nothing here parses output to decide what really ran.

cd "$(dirname "$0")"
repo_root="$PWD"

requested_test_tier="${RUE_TEST_TIER:-}"
test_tier="${requested_test_tier:-standard}"
case "$test_tier" in
    standard|all|premerge|slow|stress) ;;
    *)
        echo "error: unknown RUE_TEST_TIER '$test_tier' (expected all, premerge, slow, or stress)" >&2
        exit 2
        ;;
esac
if [[ $# -gt 0 && -n "$requested_test_tier" ]]; then
    echo "error: filtered test.sh runs cannot be combined with RUE_TEST_TIER=$test_tier" >&2
    exit 2
fi

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
    //:tutorial-snippet-tool-tests
    //:tutorial-snippet-tests
    //:spec-traceability
    //:adr-registry-validation
)

if [[ $# -eq 0 ]]; then
    # Fail closed before executing anything if a target is unowned, multiply
    # owned, or a named tier has no real members.
    ./buck2 bxl //test_tiers.bxl:validate

    # RUE-1163: heavy-suite membership is computed once, by Buck. This used to
    # be two `buck2 uquery` invocations whose results were filtered and
    # cross-checked in bash, duplicating the tier vocabulary the BXL already
    # owns. The selector subtracts the CLI shards (a local full run executes the
    # monolithic //:cli-tests once rather than re-running every 1/N slice) and,
    # on required CI, the corpora that have their own platform job.
    heavy_selector_args=(--tier "$test_tier" --exclude_label rue_cli_shard)
    if [[ "${CI:-}" == "true" ]]; then
        heavy_selector_args+=(--exclude_label rue_ci_dedicated_lane)
    fi
    # Not piped into the loop: a BXL failure (an unknown tier, a stale
    # exclusion, an unowned target) must abort under `set -e` rather than
    # produce an empty selection that silently runs no corpus at all.
    heavy_selection="$(./buck2 bxl //test_tiers.bxl:heavy_suites -- "${heavy_selector_args[@]}")"
    HEAVY_SUITES=()
    while IFS= read -r suite; do
        [[ -n "$suite" ]] || continue
        HEAVY_SUITES+=("$suite")
    done <<<"$heavy_selection"
    if [[ ${#HEAVY_SUITES[@]} -eq 0 && "$test_tier" != stress ]]; then
        echo "error: Buck selected no heavy suites for tier $test_tier" >&2
        exit 1
    fi

    tier_label=""
    if [[ "$test_tier" != standard && "$test_tier" != all ]]; then
        tier_label="rue_test_tier_$test_tier"
    fi

    # The worst status across the broad pass and every heavy suite is what this
    # script reports (RUE-579: the exit code is authoritative).
    #
    # RUE-1163 retired the RUE-924 log audit that used to live here. It tee'd
    # every invocation into one file and grepped for a `<Status>: root<target>`
    # line per hand-maintained required-corpus entry, because a `//...` pattern
    # could silently narrow and a suite that never ran could not fail. Neither
    # premise survives: every corpus is now named explicitly from Buck's own
    # membership (an unknown target is a Buck error, not a missing log line),
    # and every corpus is a build action whose stamp the test asserts, so buck2
    # exiting 0 for a named target means that corpus passed. Shell no longer
    # parses output to decide whether the suite really ran.
    overall_status=0
    set +e
    echo "Running $test_tier unit tests and lightweight repository checks..."
    broad_test_args=(//... toolchains//... --exclude rue_heavy_suite --always-exclude --ignore-tests-attribute)
    if [[ "$test_tier" == standard ]]; then
        broad_test_args+=(--exclude rue_test_tier_stress)
    elif [[ -n "$tier_label" ]]; then
        broad_test_args+=(--include "$tier_label")
    fi
    ./buck2 test "${broad_test_args[@]}"
    step_status=$?
    [[ "$step_status" -ne 0 && "$overall_status" -eq 0 ]] && overall_status=$step_status
    for suite in "${HEAVY_SUITES[@]}"; do
        echo "Running heavy suite $suite..."
        ./scripts/ci-heavy-suite "$suite"
        step_status=$?
        [[ "$step_status" -ne 0 && "$overall_status" -eq 0 ]] && overall_status=$step_status
    done
    set -e

    exit "$overall_status"
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
