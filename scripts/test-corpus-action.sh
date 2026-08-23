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
TIMEOUT_RUNNER="$SCRIPTS_DIR/corpus-timeout.py"

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

pid_is_running() {
    local pid state
    pid="$1"
    state="$(ps -p "$pid" -o stat= 2>/dev/null | tr -d '[:space:]')"
    case "$state" in
        ""|Z*) return 1 ;;
        *) return 0 ;;
    esac
}

wait_pid_gone() {
    local pid attempts
    pid="$1"
    attempts=0
    while [ "$attempts" -lt 40 ]; do
        if ! pid_is_running "$pid"; then
            return 0
        fi
        sleep 0.1
        attempts=$((attempts + 1))
    done
    return 1
}

wait_for_file() {
    local path attempts
    path="$1"
    attempts=0
    while [ "$attempts" -lt 40 ]; do
        if [ -s "$path" ]; then
            return 0
        fi
        sleep 0.1
        attempts=$((attempts + 1))
    done
    return 1
}

cleanup_recorded_processes() {
    local dir pid
    dir="$1"
    for path in "$dir/group.pid" "$dir/child.pid" "$dir/grandchild.pid"; do
        if [ -s "$path" ]; then
            pid="$(cat "$path")"
            kill -KILL "$pid" 2>/dev/null || true
        fi
    done
    if [ -s "$dir/group.pid" ]; then
        pid="$(cat "$dir/group.pid")"
        kill -KILL "-$pid" 2>/dev/null || true
    fi
}

cleanup_fixture() {
    local dir
    dir="$1"
    cleanup_recorded_processes "$dir"
    rm -rf "$dir"
}

# A harness that passes writes a stamp.
t_success() {
    local dir stamp status
    dir="$(sandbox)"
    printf '#!/usr/bin/env bash\nexit 0\n' >"$dir/harness"
    chmod +x "$dir/harness"
    stamp="$dir/stamp.txt"
    (cd "$dir" && "$CORPUS_ACTION" "$stamp" ./harness --timeout-runner "$TIMEOUT_RUNNER" -- >/dev/null 2>&1)
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
    (cd "$dir" && "$CORPUS_ACTION" "$stamp" ./harness --timeout-runner "$TIMEOUT_RUNNER" -- >/dev/null 2>&1)
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
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness --timeout-runner "$TIMEOUT_RUNNER" -- >/dev/null 2>&1
    )
    status=$?
    observed="$(cat "$dir/observed" 2>/dev/null)"
    check "absolutized run succeeds" "0" "$status"
    check "declared path reaches the harness absolute" "$(cd "$dir/cases" && pwd)" "$observed"
    rm -rf "$dir"
}

# RUE-1158's case-timings variable names a declared *output*, so the file does
# not exist when the harness starts. It must still absolutize — the harness
# creates it after the compiler has moved cwd into a case temp directory, so a
# relative value would write the measurements somewhere else and the action
# would fail its declared output.
t_absolutize_declared_output() {
    local dir status observed
    dir="$(sandbox)"
    mkdir -p "$dir/out"
    printf '#!/usr/bin/env bash\nprintf "%%s" "$RUE_CLI_CASE_TIMINGS" > "$OBSERVED"\n: > "$RUE_CLI_CASE_TIMINGS"\nexit 0\n' >"$dir/harness"
    chmod +x "$dir/harness"
    (
        cd "$dir" &&
            OBSERVED="$dir/observed" \
                RUE_CLI_CASE_TIMINGS="out/case-timings.jsonl" \
                RUE_CORPUS_ABSOLUTIZE="RUE_CLI_CASE_TIMINGS" \
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness --timeout-runner "$TIMEOUT_RUNNER" -- >/dev/null 2>&1
    )
    status=$?
    observed="$(cat "$dir/observed" 2>/dev/null)"
    check "not-yet-created output absolutizes" "0" "$status"
    check "declared output path reaches the harness absolute" \
        "$(cd "$dir/out" && pwd)/case-timings.jsonl" "$observed"
    check "harness wrote through the absolutized output path" \
        "yes" "$([ -e "$dir/out/case-timings.jsonl" ] && echo yes || echo no)"
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
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness --timeout-runner "$TIMEOUT_RUNNER" -- >/dev/null 2>&1
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
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness --timeout-runner "$TIMEOUT_RUNNER" -- >/dev/null 2>&1
    )
    observed="$(cat "$dir/observed" 2>/dev/null)"
    check "plumbing variables are unset for the harness" "unset|unset" "$observed"
    rm -rf "$dir"
}

# A wedged harness is bounded, and a timeout is a failure, so no stamp is
# written and the timed-out result is never cached. This invokes the same
# source helper that the Buck `python_bootstrap_binary` wraps, so the check
# exercises the macOS path even though GNU timeout is not installed here.
t_timeout() {
    local dir status output started finished elapsed diagnostic
    dir="$(sandbox)"
    printf '#!/usr/bin/env bash\nwhile :; do sleep 1; done\n' >"$dir/harness"
    chmod +x "$dir/harness"
    started="$(date +%s)"
    output="$(
        cd "$dir" &&
            RUE_CORPUS_TIMEOUT_SECONDS=1 \
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness --timeout-runner "$TIMEOUT_RUNNER" -- 2>&1
    )"
    status=$?
    finished="$(date +%s)"
    elapsed=$((finished - started))
    check "timed-out harness fails" "124" "$status"
    check "timed-out harness writes no stamp" "no" "$([ -e "$dir/stamp.txt" ] && echo yes || echo no)"
    case "$output" in
        *"corpus harness exceeded 1s"*) diagnostic=yes ;;
        *) diagnostic=no ;;
    esac
    check "timed-out harness reports the focused diagnostic" "yes" "$diagnostic"
    check "timed-out harness is bounded" "yes" "$([ "$elapsed" -le 5 ] && echo yes || echo no)"
    rm -rf "$dir"
}

# An ordinary exit 124 is still an ordinary harness result, not a timeout.
# The private marker keeps the focused timeout diagnostic specific.
t_exit_124_is_not_timeout() {
    local dir status output diagnostic
    dir="$(sandbox)"
    printf '#!/usr/bin/env bash\nexit 124\n' >"$dir/harness"
    chmod +x "$dir/harness"
    output="$(
        cd "$dir" &&
            RUE_CORPUS_TIMEOUT_SECONDS=5 \
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness --timeout-runner "$TIMEOUT_RUNNER" -- 2>&1
    )" || status=$?
    status="${status:-0}"
    check "ordinary exit 124 is preserved" "124" "$status"
    case "$output" in
        *"corpus harness exceeded"*) diagnostic=yes ;;
        *) diagnostic=no ;;
    esac
    check "ordinary exit 124 has no timeout diagnostic" "no" "$diagnostic"
    check "ordinary exit 124 writes no stamp" "no" "$([ -e "$dir/stamp.txt" ] && echo yes || echo no)"
    rm -rf "$dir"
}

# TERM-ignoring descendants must be killed with the harness process group.
t_timeout_kills_descendants() {
    local dir status group_pid child_pid grandchild_pid output diagnostic
    dir="$(sandbox)"
    printf '#!/usr/bin/env bash\nprintf "%%s" "$$" > "$GROUP_PID_FILE"\n(\n    ( trap "" TERM; while :; do sleep 1; done ) &\n    printf "%%s" "$!" > "$GRANDCHILD_PID_FILE"\n    trap "" TERM\n    while :; do sleep 1; done\n) &\nprintf "%%s" "$!" > "$CHILD_PID_FILE"\ntrap "" TERM\nwhile :; do sleep 1; done\n' >"$dir/harness"
    chmod +x "$dir/harness"
    output="$(
        cd "$dir" &&
            GROUP_PID_FILE="$dir/group.pid" CHILD_PID_FILE="$dir/child.pid" \
                GRANDCHILD_PID_FILE="$dir/grandchild.pid" RUE_CORPUS_TIMEOUT_SECONDS=1 \
                "$CORPUS_ACTION" "$dir/stamp.txt" ./harness --timeout-runner "$TIMEOUT_RUNNER" -- 2>&1
    )" || status=$?
    status="${status:-0}"
    group_pid="$(cat "$dir/group.pid" 2>/dev/null)"
    child_pid="$(cat "$dir/child.pid" 2>/dev/null)"
    grandchild_pid="$(cat "$dir/grandchild.pid" 2>/dev/null)"
    check "TERM-ignoring harness times out" "124" "$status"
    check "TERM-ignoring process group is recorded" "yes" "$([ -n "$group_pid" ] && echo yes || echo no)"
    check "TERM-ignoring child is killed" "yes" \
        "$([ -n "$child_pid" ] && wait_pid_gone "$child_pid" && echo yes || echo no)"
    check "TERM-ignoring grandchild is killed" "yes" \
        "$([ -n "$grandchild_pid" ] && wait_pid_gone "$grandchild_pid" && echo yes || echo no)"
    check "TERM-ignoring process group is gone" "yes" \
        "$([ -n "$group_pid" ] && wait_pid_gone "$group_pid" && echo yes || echo no)"
    case "$output" in
        *"corpus harness exceeded 1s"*) diagnostic=yes ;;
        *) diagnostic=no ;;
    esac
    check "forced cleanup retains focused diagnostic" "yes" "$diagnostic"
    cleanup_fixture "$dir"
}

# Signalling only corpus-action must be forwarded to the Python runner. The
# runner then cleans the entire harness group before the wrapper returns 143.
t_wrapper_cancellation() {
    local dir status action_pid group_pid child_pid grandchild_pid output diagnostic
    dir="$(sandbox)"
    printf '#!/usr/bin/env bash\nprintf "%%s" "$$" > "$GROUP_PID_FILE"\n(\n    ( trap "" TERM INT; while :; do sleep 1; done ) &\n    printf "%%s" "$!" > "$GRANDCHILD_PID_FILE"\n    trap "" TERM INT\n    while :; do sleep 1; done\n) &\nprintf "%%s" "$!" > "$CHILD_PID_FILE"\ntrap "" TERM INT\nwhile :; do sleep 1; done\n' >"$dir/harness"
    chmod +x "$dir/harness"
    GROUP_PID_FILE="$dir/group.pid" CHILD_PID_FILE="$dir/child.pid" \
        GRANDCHILD_PID_FILE="$dir/grandchild.pid" RUE_CORPUS_TIMEOUT_SECONDS=30 \
        "$CORPUS_ACTION" "$dir/stamp.txt" "$dir/harness" --timeout-runner "$TIMEOUT_RUNNER" -- \
        >"$dir/output" 2>&1 &
    action_pid=$!

    if ! wait_for_file "$dir/group.pid" || ! wait_for_file "$dir/grandchild.pid"; then
        check "cancellable harness records its process group" "yes" "no"
        kill -TERM "$action_pid" 2>/dev/null || true
        wait "$action_pid" 2>/dev/null || true
        cleanup_fixture "$dir"
        return
    fi
    kill -TERM "$action_pid" 2>/dev/null || true
    # The harness ignores TERM, so runner cleanup remains active long enough
    # for this second signal to exercise the wrapper's signal-immune reap.
    sleep 0.1
    kill -TERM "$action_pid" 2>/dev/null || true
    wait "$action_pid" 2>/dev/null
    status=$?
    output="$(cat "$dir/output" 2>/dev/null)"
    group_pid="$(cat "$dir/group.pid" 2>/dev/null)"
    child_pid="$(cat "$dir/child.pid" 2>/dev/null)"
    grandchild_pid="$(cat "$dir/grandchild.pid" 2>/dev/null)"
    check "repeated wrapper cancellation returns SIGTERM status" "143" "$status"
    check "wrapper cancellation removes the stamp" "no" "$([ -e "$dir/stamp.txt" ] && echo yes || echo no)"
    check "wrapper cancellation removes the harness" "yes" \
        "$([ -n "$group_pid" ] && wait_pid_gone "$group_pid" && echo yes || echo no)"
    check "wrapper cancellation removes the child" "yes" \
        "$([ -n "$child_pid" ] && wait_pid_gone "$child_pid" && echo yes || echo no)"
    check "wrapper cancellation removes the grandchild" "yes" \
        "$([ -n "$grandchild_pid" ] && wait_pid_gone "$grandchild_pid" && echo yes || echo no)"
    case "$output" in
        *"corpus harness exceeded"*) diagnostic=yes ;;
        *) diagnostic=no ;;
    esac
    check "wrapper cancellation has no timeout diagnostic" "no" "$diagnostic"
    cleanup_fixture "$dir"
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
t_absolutize_declared_output
t_missing_absolutize_target_fails
t_plumbing_is_hidden
t_timeout
t_exit_124_is_not_timeout
t_timeout_kills_descendants
t_wrapper_cancellation
t_usage

if [ "$failures" -ne 0 ]; then
    echo "corpus-action: $failures/$tests checks failed" >&2
    exit 1
fi
echo "corpus-action: $tests checks passed"
