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

if [[ $# -eq 0 ]]; then
    # Fail closed before executing anything if a target is unowned, multiply
    # owned, or a named tier has no real members.
    ./buck2 bxl //test_tiers.bxl:validate

    # RUE-1163: one invocation. Selection is label filters that buck2 evaluates,
    # and scheduling is buck2's — each corpus action declares that it needs the
    # whole machine (see corpus.bzl), so two corpora never run at once while a
    # corpus still overlaps unit tests and compiles.
    #
    # What used to be here: a BXL query for heavy-suite membership, a bash loop
    # running each corpus alone through scripts/ci-heavy-suite, and worst-status
    # aggregation across N invocations. The loop existed to keep the opaque
    # harnesses from contending for the runner; they are declared actions now,
    # so the scheduler does that better than a sequence could — it can overlap a
    # corpus with everything that is not another corpus.
    #
    # The CLI shards are excluded because they are scheduling alternatives for
    # the monolithic //:cli-tests, not additional coverage: running both would
    # execute the same cases five times.
    test_args=(//... toolchains//... --always-exclude --ignore-tests-attribute --exclude rue_cli_shard)
    if [[ "$test_tier" == standard ]]; then
        test_args+=(--exclude rue_test_tier_stress)
    elif [[ "$test_tier" != all ]]; then
        test_args+=(--include "rue_test_tier_$test_tier")
    fi
    # Required CI gives these corpora their own platform-corpus job; the label
    # is the only place that set is written down (scripts/validate-ci-gate.py
    # fails if a labeled corpus has no job).
    if [[ "${CI:-}" == "true" ]]; then
        test_args+=(--exclude rue_ci_dedicated_lane)
    fi

    echo "Running the $test_tier tier..."
    ./buck2 test "${test_args[@]}"
else
    # Unit tests live under //crates/...; the suite sh_tests are at the repo
    # root, so this scope keeps them out of the unfiltered step below.
    echo "Running unit tests..."
    ./buck2 test //crates/...

    # RUE-1163: each harness has a command_alias carrying the corpus's declared
    # inputs, so a filtered run plumbs no environment by hand. The previous
    # spelling set a smaller set than BUCK does — it never passed
    # RUE_REAL_STD_PATH, so `real_std` spec and UI cases resolved the standard
    # library through a cwd-relative fallback that only worked because this
    # script cd's to the repository root. A filtered run now uses exactly the
    # inputs //:spec-tests, //:ui-tests, and //:cli-tests use.
    echo "Running spec tests..."
    ./buck2 run //crates/rue-spec:spec -- --quiet "$@"

    echo "Running UI tests..."
    ./buck2 run //crates/rue-ui-tests:ui -- --quiet "$@"

    echo "Running CLI integration tests..."
    ./buck2 run //crates/rue-cli-tests:cli -- --quiet "$@"

    echo "Running repository quality gates..."
    ./buck2 test //:repository-quality-gates
fi
