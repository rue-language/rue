#!/usr/bin/env bash
set -euo pipefail

: "${RUE_BINARY:?RUE_BINARY must point to the Rue compiler}"
: "${RUE_ORACLE_DIFF_BINARY:?RUE_ORACLE_DIFF_BINARY must point to the differential harness}"

scratch="$(mktemp -d)"
# Buck retains the harness log, which includes every failing seed and its full
# source. Scratch repro files are intentionally ephemeral here; the nightly
# fuzz workflow owns durable artifact upload.
cleanup() {
    rm -rf "$scratch"
}
trap cleanup EXIT

mkdir -p "$scratch/tmp" "$scratch/crashes"
TMPDIR="$scratch/tmp" "$RUE_ORACLE_DIFF_BINARY" fuzz \
    --start 0 \
    --seeds 64 \
    --timeout 2 \
    --crash-dir "$scratch/crashes"
