#!/usr/bin/env bash
# test-corpus-action.sh — regression tests for scripts/corpus-action (RUE-1118).
#
# corpus-action is what makes a corpus suite's result cacheable, so its failure
# modes are asymmetric: a spurious failure is loud and self-correcting, but
# writing a stamp when the harness did not pass would cache a false green and
# every later run of that tree would report success without executing anything.
# These tests pin the stamp-only-on-success contract, and the absolutization
# contract that keeps a harness from silently resolving a relative path against
# the action's working directory instead of the real corpus.
#
# Each test runs the real script in a throwaway sandbox against a fake harness.
set -uo pipefail

if [ -n "${RUE_CORPUS_SCRIPTS_ROOT:-}" ]; then
    SCRIPTS_DIR="$RUE_CORPUS_SCRIPTS_ROOT/scripts"
else
    SCRIPTS_DIR="$(cd "$(dirname "$0")" && pwd)"
fi
CORPUS_ACTION="$SCRIPTS_DIR/corpus-action"

failures=0
tests=0

check() {
    local label="$1" expected="$2" actual="$3"
    tests=$((tests + 1))
    if [ "$expected" != "$actual" ]; then
        echo "FAIL: $label: expected '$expected', got '$actual'" >&2
        failures=$((failures + 1))
    fi
}

sandbox() {
    mktemp -d "${TMPDIR:-/tmp}/rue-corpus-action-test.XXXXXX"
}

# A harness that passes writes a stamp.
t_success() {
    local dir stamp status
    dir="$(sandbox)"
    printf '#!/usr/bin/env bash\nexit 0\n' >"$dir/harness"
    chmod +x "$dir/harness"
    stamp="$dir/stamp.txt"
    (cd "$dir" && "$CORPUS_ACTION" "$stamp" ./harness >/dev/null 2>&1)
    status=$?
    check "passing harness exits 0" "0" "$status"
    check "passing harness writes a stamp" "yes" "$([ -s "$stamp" ] && echo yes || echo no)"
    rm -rf "$dir"
}

# A harness that fails writes NO stamp: the action fails, nothing is cached,
# and the next run re-executes the corpus.
t_failure_writes_no_stamp() {
    local dir stamp status
    dir="$(sandbox)"
    printf '#!/usr/bin/env bash\nexit 3\n' >"$dir/harness"
    chmod +x "$dir/harness"
    stamp="$dir/stamp.txt"
    (cd "$dir" && "$CORPUS_ACTION" "$stamp" ./harness >/dev/null 2>&1)
    status=$?
    check "failing harness propagates its status" "3" "$status"
    check "failing harness writes no stamp" "no" "$([ -e "$stamp" ] && echo yes || echo no)"
    rm -rf "$dir"
}

# Declared path variables reach the harness as absolute paths. The corpus
# harnesses spawn the compiler with a case temp dir as cwd, so a relative value
# would resolve somewhere else entirely — or, worse, resolve to a real but wrong
# directory and quietly test nothing.
t_absolutize() {
    local dir status observed
    dir="$(sandbox)"
    mkdir -p "$dir/cases"
    printf '#!/usr/bin/env bash\nprintf "%%s" "$RUE_TEST_CASES" > "$OBSERVED"\nexit 0\n' >"$dir/harness"
    chmod +x "$dir/harness"
    (
        cd "$dir" &&
            OBSERVED="$dir/observed" \
                RUE_TEST_CASES="cases" \
                RUE_CORPUS_ABSOLUTIZE="RUE_TEST_CASES" \
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness >/dev/null 2>&1
    )
    status=$?
    observed="$(cat "$dir/observed" 2>/dev/null)"
    check "absolutized run succeeds" "0" "$status"
    check "declared path reaches the harness absolute" "$(cd "$dir/cases" && pwd)" "$observed"
    rm -rf "$dir"
}

# An unset variable named in RUE_CORPUS_ABSOLUTIZE is a BUCK wiring bug. Failing
# closed matters more than usual here: continuing would let the harness fall back
# to its own relative default and pass against the wrong corpus.
t_missing_absolutize_target_fails() {
    local dir status
    dir="$(sandbox)"
    printf '#!/usr/bin/env bash\nexit 0\n' >"$dir/harness"
    chmod +x "$dir/harness"
    (
        cd "$dir" &&
            RUE_CORPUS_ABSOLUTIZE="RUE_NOT_SET_ANYWHERE" \
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness >/dev/null 2>&1
    )
    status=$?
    check "unset absolutize target fails" "2" "$status"
    check "unset absolutize target writes no stamp" "no" "$([ -e "$dir/stamp.txt" ] && echo yes || echo no)"
    rm -rf "$dir"
}

# The harness must not see this action's own plumbing variables.
t_plumbing_is_hidden() {
    local dir observed
    dir="$(sandbox)"
    printf '#!/usr/bin/env bash\nprintf "%%s|%%s" "${RUE_CORPUS_ABSOLUTIZE:-unset}" "${RUE_CORPUS_TIMEOUT_SECONDS:-unset}" > "$OBSERVED"\nexit 0\n' >"$dir/harness"
    chmod +x "$dir/harness"
    (
        cd "$dir" &&
            OBSERVED="$dir/observed" \
                RUE_CORPUS_ABSOLUTIZE="" \
                RUE_CORPUS_TIMEOUT_SECONDS="60" \
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness >/dev/null 2>&1
    )
    observed="$(cat "$dir/observed" 2>/dev/null)"
    check "plumbing variables are unset for the harness" "unset|unset" "$observed"
    rm -rf "$dir"
}

# A wedged harness is bounded, and a timeout is a failure, so no stamp is
# written and the timed-out result is never cached.
t_timeout() {
    local dir status
    dir="$(sandbox)"
    if ! command -v timeout >/dev/null 2>&1; then
        echo "skip: no timeout(1) on this host"
        return
    fi
    printf '#!/usr/bin/env bash\nsleep 30\n' >"$dir/harness"
    chmod +x "$dir/harness"
    (
        cd "$dir" &&
            RUE_CORPUS_TIMEOUT_SECONDS=1 \
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness >/dev/null 2>&1
    )
    status=$?
    check "timed-out harness fails" "124" "$status"
    check "timed-out harness writes no stamp" "no" "$([ -e "$dir/stamp.txt" ] && echo yes || echo no)"
    rm -rf "$dir"
}

t_usage() {
    local status
    "$CORPUS_ACTION" >/dev/null 2>&1
    status=$?
    check "no arguments is a usage error" "2" "$status"
}

t_success
t_failure_writes_no_stamp
t_absolutize
t_missing_absolutize_target_fails
t_plumbing_is_hidden
t_timeout
t_usage

if [ "$failures" -ne 0 ]; then
    echo "corpus-action: $failures/$tests checks failed" >&2
    exit 1
fi
echo "corpus-action: $tests checks passed"
