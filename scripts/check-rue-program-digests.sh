#!/usr/bin/env bash
# Negative control 3 of ADR-0070 (RUE-1404): digest sensitivity of the
# rue_program action chain, in the check-reproducible-compiler.sh mould — a CI
# script OUTSIDE the Buck graph, because asserting "this action re-executed"
# requires driving buck2 and reading its execution log, which no Buck-visible
# test in this repository does un-stubbed.
#
# The assertions are chosen against what buck2 actually guarantees locally,
# which an earlier draft of this script got wrong twice:
#
#   * a mutate-then-build can race the async file watcher and no-op, so the
#     declared-mutation assertion RETRIES until the invalidation is observed;
#   * "revert then build is a cache hit" is NOT a local guarantee — OSS buck2
#     has no persistent digest-keyed local action cache, and DICE may or may
#     not retain the pre-mutation state — so this script asserts steady-state
#     convergence instead: after any mutation settles, an identical rebuild
#     executes nothing.
#
# What is asserted:
#   1. steady state: an unchanged tree rebuilds with zero rue_* executions;
#   2. mutating a DECLARED source re-runs the chain (retried past the watcher);
#   3. after reverting and settling, steady state holds again;
#   4. mutating an UNDECLARED neighbour (the boundary fixture's extra.rue is
#      deliberately outside :hello's srcs) leaves steady state undisturbed —
#      the corpus.bzl:19-25 false-pass window as a hard check.
#
# Mutations are trap-restored; run from a clean checkout, with no concurrent
# buck2 invocations (`log what-ran` reads the latest invocation's log).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# CI and developers use the repo wrapper; RUE_BUCK2 exists so environments
# without dotslash can point at a bare binary.
BUCK2="${RUE_BUCK2:-./buck2}"

TARGET="fixtures/rue-program:hello"
DECLARED="fixtures/rue-program/hello/shared.rue"
UNDECLARED="fixtures/rue-program/boundary/extra.rue"

restore() {
    git checkout --quiet -- "$DECLARED" "$UNDECLARED" 2>/dev/null || true
}
trap restore EXIT

rue_actions_of_last_build() {
    # what-ran reports the latest invocation's executed actions; rue_scan /
    # rue_derive_manifest / rue_compile are this rule's categories. A failed
    # log query is a failure of this check, not an empty action list: treating
    # it as zero is a false pass for the undeclared-input control.
    local output status=0
    output="$("$BUCK2" log what-ran 2>&1)" || status=$?
    if [[ "$status" -ne 0 ]]; then
        printf '%s\n' "$output" >&2
        return "$status"
    fi
    awk '/rue_scan|rue_derive_manifest|rue_compile/ { count += 1 }
         END { print count + 0 }' <<<"$output"
}

build() {
    # Keep successful probes quiet, but send a failed Buck diagnostic through to
    # ci-timed so its preserved failure artifact contains the actionable error.
    local output status=0
    output="$("$BUCK2" build "$TARGET" 2>&1)" || status=$?
    if [[ "$status" -ne 0 ]]; then
        printf '%s\n' "$output" >&2
        return "$status"
    fi
}

# Build until an identical rebuild executes nothing, so later assertions start
# from an unambiguous state. Bounded: a tree that never converges is itself a
# digest-sensitivity failure.
settle() {
    for _ in 1 2 3 4 5; do
        if build; then
            :
        else
            local status=$?
            echo "FAIL: Buck build failed while settling the digest check" >&2
            return "$status"
        fi
        local ran
        if ran="$(rue_actions_of_last_build)"; then
            :
        else
            local status=$?
            echo "FAIL: could not read Buck's execution log while settling the digest check" >&2
            return "$status"
        fi
        if [[ "$ran" -eq 0 ]]; then
            return 0
        fi
    done
    echo "FAIL: tree did not converge to a no-op rebuild" >&2
    exit 1
}

echo "digest-check: (1) steady state on an unchanged tree"
settle
echo "  ok"

echo "digest-check: (2) mutating a declared source must re-run the chain"
ran=0
# The file watcher is asynchronous AND lossy under load: an inotify queue
# overflow during the settle builds can drop the write's event outright, in
# which case no amount of rebuilding will ever observe it. Each retry
# therefore APPENDS A FRESH PROBE — a new event the watcher gets a new chance
# to deliver — rather than rebuilding after one write and hoping.
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    printf '\n// digest-sensitivity probe %s\n' "$attempt" >> "$DECLARED"
    sleep 1
    if build; then
        :
    else
        status=$?
        echo "FAIL: Buck build failed during the declared-source probe" >&2
        exit "$status"
    fi
    if ran="$(rue_actions_of_last_build)"; then
        :
    else
        status=$?
        echo "FAIL: could not read Buck's execution log during the declared-source probe" >&2
        exit "$status"
    fi
    if [[ "$ran" -ge 1 ]]; then
        break
    fi
done
if [[ "$ran" -lt 1 ]]; then
    echo "FAIL: declared-source mutation never re-ran a rue_* action" >&2
    exit 1
fi
echo "  ok: $ran rue_* action(s) re-ran"

echo "digest-check: (3) reverting must converge back to steady state"
restore
settle
echo "  ok"

echo "digest-check: (4) mutating an undeclared neighbour must not disturb it"
printf '\n// digest-sensitivity probe\n' >> "$UNDECLARED"
# Give the watcher time to deliver the event we are asserting is IGNORED, so a
# pass means "keyed by nothing", not "not seen yet".
sleep 2
if build; then
    :
else
    status=$?
    echo "FAIL: Buck build failed during the undeclared-neighbour probe" >&2
    exit "$status"
fi
if ran="$(rue_actions_of_last_build)"; then
    :
else
    status=$?
    echo "FAIL: could not read Buck's execution log during the undeclared-neighbour probe" >&2
    exit "$status"
fi
if [[ "$ran" -ne 0 ]]; then
    echo "FAIL: undeclared-neighbour mutation re-executed $ran rue_* action(s)" >&2
    exit 1
fi
echo "  ok: no rue_* actions"

echo "digest-check: PASS"
