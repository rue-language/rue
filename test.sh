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
# CORPUS-OMISSION AUDIT (RUE-924). A green `buck2 test //...` summary is only
# trustworthy if the compiler-consuming corpus harnesses (cli/spec/ui/oracle/
# reproducibility/tutorial) actually EXECUTED. A suite that never ran cannot
# fail, so the exit code alone cannot catch its disappearance — a served
# test-result cache entry, an accidental target-pattern narrowing, or a
# platform gate can all silently drop a suite, and one such omission once let a
# Pass tally hide five real CLI-case failures that CI caught. The unfiltered
# path therefore captures the run, streams it live, and asserts each required
# corpus harness produced a Pass/Fail result line; a missing one is a HARD
# failure even when buck2 exited 0.

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
    //:benchmark-tool-tests
    //:tutorial-snippet-tool-tests
    //:tutorial-snippet-tests
    //:spec-traceability
    //:adr-registry-validation
)

# The compiler-consuming corpus harnesses that an unfiltered run MUST actually
# execute (RUE-924). Each compiles and/or runs real programs through the rue
# binary, so a silent omission (cache, pattern narrowing, platform gate) is
# exactly the failure that hides miscompilations behind a green tally. The
# unfiltered path audits that every one of these produced a result line.
REQUIRED_CORPUS_HARNESSES=(
    //:cli-tests
    //:cli-tests-caldera
    //:spec-tests
    //:ui-tests
    //:oracle-diff-generated-smoke
    //:reproducible-programs
    //:tutorial-snippet-tests
    //:frontend-diff-test
)

if [[ $# -eq 0 ]]; then
    # Fail closed before executing anything if a target is unowned, multiply
    # owned, or a named tier has no real members.
    ./buck2 bxl //test_tiers.bxl:validate

    # Keep discovery broad so new crate and repository tests cannot disappear
    # behind a hand-maintained list. Heavy opaque harnesses are labeled in BUCK:
    # query that label from the live graph, exclude it from the broad pass, then
    # run every returned target alone. A newly labeled target is automatically
    # included without ever putting the whole heavy set in one test invocation.
    # RUE-1116: the CI-only CLI shards are labeled rue_heavy_suite (so
    # ci-heavy-suite accepts them and the broad pass excludes them) AND
    # rue_cli_shard. A local full run must execute the monolithic //:cli-tests
    # exactly once instead of re-running every 1/N slice, so subtract the
    # rue_cli_shard set from heavy-suite discovery here.
    cli_shard_targets="$(./buck2 uquery 'attrfilter(labels, rue_cli_shard, //...)')"
    heavy_query='attrfilter(labels, rue_heavy_suite, //...)'
    tier_label=""
    if [[ "$test_tier" == standard ]]; then
        heavy_query='attrfilter(labels, rue_heavy_suite, //...) except attrfilter(labels, rue_test_tier_stress, //...)'
    elif [[ "$test_tier" != all ]]; then
        tier_label="rue_test_tier_$test_tier"
        heavy_query="attrfilter(labels, rue_heavy_suite, attrfilter(labels, $tier_label, //...))"
    fi

    HEAVY_SUITES=()
    while IFS= read -r suite; do
        [[ -n "$suite" ]] || continue
        grep -Fxq "$suite" <<<"$cli_shard_targets" && continue
        HEAVY_SUITES+=("$suite")
    done < <(./buck2 uquery "$heavy_query")
    if [[ ${#HEAVY_SUITES[@]} -eq 0 && "$test_tier" != slow && "$test_tier" != stress ]]; then
        echo "error: Buck query found no rue_heavy_suite targets" >&2
        exit 1
    fi

    # Required CI may move selected heavy suites to their own platform runner
    # so expensive corpus harnesses overlap instead of serializing the merge
    # queue. This is deliberately CI-only: an ordinary local `./test.sh` still
    # owns and audits the complete corpus in one invocation. Every deferred
    # target must be present in Buck's live heavy-suite query; a stale or
    # misspelled shard therefore fails here instead of becoming a false green.
    DEFERRED_HEAVY_SUITES=()
    if [[ -n "${RUE_CI_DEFER_HEAVY_SUITES:-}" ]]; then
        if [[ "${CI:-}" != "true" ]]; then
            echo "error: RUE_CI_DEFER_HEAVY_SUITES is reserved for required CI" >&2
            exit 1
        fi
        read -r -a DEFERRED_HEAVY_SUITES <<<"$RUE_CI_DEFER_HEAVY_SUITES"
        for deferred in "${DEFERRED_HEAVY_SUITES[@]}"; do
            found=0
            for suite in "${HEAVY_SUITES[@]}"; do
                [[ "${suite#root}" == "${deferred#root}" ]] && found=1
            done
            if [[ "$found" -ne 1 ]]; then
                echo "error: deferred CI suite is not labeled rue_heavy_suite: $deferred" >&2
                exit 1
            fi
        done
    fi

    suite_is_deferred() {
        local candidate="$1" deferred
        # Bash 3.2 treats an empty array expansion as an unbound variable under
        # `set -u`. macOS runners normally provide a newer Bash, but keeping the
        # no-deferral path portable also makes relocated/cache-free validation
        # behave exactly like the canonical worktree.
        [[ -z "${RUE_CI_DEFER_HEAVY_SUITES:-}" ]] && return 1
        for deferred in "${DEFERRED_HEAVY_SUITES[@]}"; do
            [[ "${candidate#root}" == "${deferred#root}" ]] && return 0
        done
        return 1
    }

    # Stream every invocation live AND capture it into one log, so we can audit
    # afterward that no corpus harness was silently omitted (RUE-924).
    # PIPESTATUS[0] is buck2's real exit code (tee always exits 0), preserving
    # the RUE-579 "exit code is authoritative" contract; the worst status across
    # the broad pass and every heavy suite is what this script reports.
    run_log="$(mktemp)"
    overall_status=0
    set +e
    echo "Running $test_tier unit tests and lightweight repository checks..."
    broad_test_args=(//... toolchains//... --exclude rue_heavy_suite --always-exclude --ignore-tests-attribute)
    if [[ "$test_tier" == standard ]]; then
        broad_test_args+=(--exclude rue_test_tier_stress)
    elif [[ -n "$tier_label" ]]; then
        broad_test_args+=(--include "$tier_label")
    fi
    if [[ -n "${RUE_CI_DEFER_DEDICATED_SUITES:-}" ]]; then
        if [[ "${CI:-}" != "true" ]]; then
            echo "error: RUE_CI_DEFER_DEDICATED_SUITES is reserved for required CI" >&2
            exit 1
        fi

        # Dedicated targets remain pre-merge coverage, but their explicit CI
        # jobs must not also execute inside every platform's broad pass. Require
        # the workflow's declared set to match Buck's live scheduling metadata
        # exactly before excluding the label, so a newly tagged target cannot
        # disappear behind a stale environment variable.
        dedicated_query='attrfilter(labels, rue_dedicated_suite, //...)'
        if [[ "$test_tier" == standard ]]; then
            dedicated_query='attrfilter(labels, rue_dedicated_suite, //...) except attrfilter(labels, rue_test_tier_stress, //...)'
        elif [[ -n "$tier_label" ]]; then
            dedicated_query="attrfilter(labels, rue_dedicated_suite, attrfilter(labels, $tier_label, //...))"
        fi
        dedicated_targets="$(./buck2 uquery "$dedicated_query")"
        read -r -a deferred_dedicated <<<"$RUE_CI_DEFER_DEDICATED_SUITES"
        for deferred in "${deferred_dedicated[@]}"; do
            if ! grep -Fxq "root${deferred#root}" <<<"$dedicated_targets"; then
                echo "error: deferred CI target is not labeled rue_dedicated_suite: $deferred" >&2
                exit 1
            fi
        done
        while IFS= read -r dedicated; do
            [[ -n "$dedicated" ]] || continue
            found=0
            for deferred in "${deferred_dedicated[@]}"; do
                [[ "${dedicated#root}" == "${deferred#root}" ]] && found=1
            done
            if [[ "$found" -ne 1 ]]; then
                echo "error: dedicated Buck target has no explicit CI owner: $dedicated" >&2
                exit 1
            fi
        done <<<"$dedicated_targets"
        broad_test_args+=(--exclude rue_dedicated_suite)
    fi
    ./buck2 test "${broad_test_args[@]}" 2>&1 | tee "$run_log"
    step_status=${PIPESTATUS[0]}
    [[ "$step_status" -ne 0 && "$overall_status" -eq 0 ]] && overall_status=$step_status
    if [[ ${#HEAVY_SUITES[@]} -gt 0 ]]; then
        for suite in "${HEAVY_SUITES[@]}"; do
            if suite_is_deferred "$suite"; then
                echo "Deferring heavy suite $suite to its required CI shard..."
                continue
            fi
            echo "Running heavy suite $suite..."
            # Normalize Buck's configured-cell spelling before handing execution
            # to the single audited heavy-suite runner. It owns target-specific
            # executor arguments as well as the per-target result check.
            ./scripts/ci-heavy-suite "${suite#root}" 2>&1 | tee -a "$run_log"
            step_status=${PIPESTATUS[0]}
            [[ "$step_status" -ne 0 && "$overall_status" -eq 0 ]] && overall_status=$step_status
        done
    fi
    set -e

    # buck2 prints exactly one "<Status>: root<target> (<time>)" summary line
    # per executed test (Pass/Fail/Skip/Timeout/...), including cache hits. No
    # such line for a required harness means it never ran under this invocation
    # — neither in the broad pass nor as a heavy suite.
    missing_corpus=()
    for target in "${REQUIRED_CORPUS_HARNESSES[@]}"; do
        # The current omission sentinels are all premerge corpora. Slow and
        # stress selections have their own explicit targets and must not claim
        # these premerge harnesses executed.
        if [[ "$test_tier" == slow || "$test_tier" == stress ]]; then
            continue
        fi
        if suite_is_deferred "$target"; then
            continue
        fi
        if ! grep -qE "(Pass|Fail|Skip|Timeout|Fatal|Omit|Flaky): root${target} " "$run_log"; then
            missing_corpus+=("$target")
        fi
    done
    rm -f "${run_log:?}"

    if [[ ${#missing_corpus[@]} -gt 0 ]]; then
        echo "=== CORPUS OMITTED (RUE-924): ${missing_corpus[*]} ===" >&2
        echo "test.sh: the corpus harness(es) above produced no Pass/Fail result in" >&2
        echo "test.sh: this unfiltered run; refusing to report success on a partial" >&2
        echo "test.sh: run. If a stale test-result cache is serving them, re-run with" >&2
        echo "test.sh: a busted cache, e.g. './buck2 test //... --no-remote-cache'." >&2
        exit 1
    fi

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
