#!/usr/bin/env bash
# Compile one large maintained Rue application once, then reuse the resulting
# executable across the runtime scenarios owned by the selected test tier.
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: scripts/run-large-example.sh {caldera|meridian} {canary|slow|stress}" >&2
    exit 2
fi

program="$1"
tier="$2"
case "$program" in
    caldera|meridian) ;;
    *)
        echo "error: unknown large example: $program" >&2
        exit 2
        ;;
esac
case "$tier" in
    canary|slow|stress) ;;
    *)
        echo "error: unknown large-example tier: $tier" >&2
        exit 2
        ;;
esac

: "${RUE_BINARY:?RUE_BINARY must name the compiler to test}"
: "${RUE_EXAMPLES_DIR:?RUE_EXAMPLES_DIR must name the examples tree}"
: "${RUE_STD_DIR:?RUE_STD_DIR must name the standard library}"

rue="$(cd "$(dirname "$RUE_BINARY")" && pwd)/$(basename "$RUE_BINARY")"
examples="$(cd "$RUE_EXAMPLES_DIR" && pwd)"
export RUE_STD_PATH="$(cd "$RUE_STD_DIR" && pwd)"

work="$(mktemp -d)"
cleanup() {
    rm -rf "$work"
}
trap cleanup EXIT

root="$examples/$program/main.rue"
if [[ "$tier" == "canary" ]]; then
    root="$examples/$program/canary.rue"
fi
binary="$work/$program"

echo "large-example: compiling $program $tier root once with $rue"
"$rue" "$root" -o "$binary"

run_expect() {
    local label="$1"
    local expected_status="$2"
    shift 2
    local output="$work/$label.out"
    local status=0
    set +e
    (cd "$work" && "$binary" "$@") >"$output" 2>&1
    status=$?
    set -e
    if [[ "$status" -ne "$expected_status" ]]; then
        echo "FAIL $program/$tier/$label: expected exit $expected_status, got $status" >&2
        sed 's/^/    /' "$output" >&2
        exit 1
    fi
    echo "PASS $program/$tier/$label"
}

require_output() {
    local label="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$work/$label.out"; then
        echo "FAIL $program/$tier/$label: missing output marker: $expected" >&2
        sed 's/^/    /' "$work/$label.out" >&2
        exit 1
    fi
}

if [[ "$tier" == "canary" ]]; then
    run_expect canary 0
    exit 0
fi

if [[ "$tier" == "stress" ]]; then
    run_expect stress4 0 stress4
    require_output stress4 "stress scale=4"
    require_output stress4 "valid=true"
    exit 0
fi

if [[ "$program" == "caldera" ]]; then
    run_expect selftest 0
    require_output selftest "selftest checks=23"
    require_output selftest "valid=true"
    run_expect demo 0 demo
    require_output demo "entities=8"
    require_output demo "valid=true"
    run_expect scenario 0 run "$examples/caldera/demo.caldera"
    require_output scenario "script_oracle=true"
    require_output scenario "valid=true"
    run_expect stress1 0 stress1
    require_output stress1 "stress scale=1"
    require_output stress1 "valid=true"
    run_expect benchmark 0 benchmark
    require_output benchmark "benchmark tiers=2"
    require_output benchmark "valid=true"
else
    run_expect help 0
    require_output help "usage: meridian"
    run_expect demo 0 demo
    require_output demo "database=meridian"
    require_output demo "result_valid=true"
    run_expect workload 0 run "$examples/meridian/demo.sql"
    require_output workload "plan_valid=true"
    require_output workload "result_valid=true"
    run_expect selftest 0 selftest
    require_output selftest "selftest checks=24"
    require_output selftest "valid=true"
    run_expect stress1 0 stress1
    require_output stress1 "stress scale=1"
    require_output stress1 "valid=true"
    run_expect benchmark 0 benchmark
    require_output benchmark "benchmark=meridian-complete"
    require_output benchmark "valid=true"
fi
