#!/usr/bin/env bash
set -euo pipefail

: "${RUE_ORACLE_DIFF_BINARY:?missing differential harness}"
: "${RUE_ORACLE_DIFF_FAKE_COMPILER:?missing fake compiler}"
: "${RUE_ORACLE_DIFF_FAKE_PROGRAM:?missing fake compiled program}"

scratch="$(mktemp -d)"
cleanup() {
    rm -rf "$scratch"
}
trap cleanup EXIT

set +e
RUE_BINARY="$RUE_ORACLE_DIFF_FAKE_COMPILER" \
RUE_ORACLE_DIFF_FAKE_PROGRAM="$RUE_ORACLE_DIFF_FAKE_PROGRAM" \
RUE_ORACLE_DIFF_OPT_LOG="$scratch/optimizations" \
RUE_ORACLE_DIFF_TESTING=1 \
    "$RUE_ORACLE_DIFF_BINARY" fuzz \
        --start 0 \
        --seeds 1 \
        --timeout 5 \
        --crash-dir "$scratch/crashes" \
        --test-plant-miscompile O2 >"$scratch/output" 2>&1
status=$?
set -e

if [ "$status" -eq 0 ]; then
    echo "planted miscompile unexpectedly passed"
    cat "$scratch/output"
    exit 1
fi

if [ "$(grep -c 'DISAGREEMENT (seed 0, O2)' "$scratch/output")" -ne 1 ]; then
    echo "expected exactly one O2 disagreement"
    cat "$scratch/output"
    exit 1
fi
if grep -E 'DISAGREEMENT \(seed 0, O(0|1|3)\)' "$scratch/output" >/dev/null; then
    echo "the planted observation mutation escaped its selected lane"
    cat "$scratch/output"
    exit 1
fi
grep -F 'agreeing lanes:   3' "$scratch/output" >/dev/null
grep -F 'DISAGREEMENTS:    1' "$scratch/output" >/dev/null

if [ ! -f "$scratch/optimizations" ]; then
    echo "fake compiler did not record optimization arguments"
    cat "$scratch/output"
    exit 1
fi
if [ "$(wc -l <"$scratch/optimizations" | tr -d ' ')" -ne 4 ]; then
    echo "expected exactly four compiler optimization arguments"
    cat "$scratch/optimizations"
    exit 1
fi
for optimization in -O0 -O1 -O2 -O3; do
    if [ "$(grep -Fxc -- "$optimization" "$scratch/optimizations")" -ne 1 ]; then
        echo "expected exactly one compiler invocation with $optimization"
        cat "$scratch/optimizations"
        exit 1
    fi
done
if grep -Ev '^-O[0-3]$' "$scratch/optimizations" >/dev/null; then
    echo "fake compiler received an unsupported optimization argument"
    cat "$scratch/optimizations"
    exit 1
fi
