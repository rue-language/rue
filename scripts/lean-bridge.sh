#!/usr/bin/env bash
#
# lean-bridge — ADR-0097's differential bridge (RUE-2228).
#
# Runs the Lean mechanization's exported corpus (`//:lean-ruecore`) through
# `rue-oracle-diff lean-corpus`: the compiler's accept/reject decision, the
# reference oracle, and native binaries at O1/O2/O3, against what the verified
# checker and interpreter say. Every pairwise disagreement is reported; the
# exit status is non-zero when there is one.
#
#   ./buck2 run //:lean-bridge -- [--case NAME]... [--report-json PATH]
#
# Deliberately a `buck2 run` entry point rather than a test target: no CI lane
# requests it until ADR-0097's gate is met (RUE-2241).
set -euo pipefail

: "${RUE_BINARY:?RUE_BINARY must point to the Rue compiler}"
: "${RUE_ORACLE_DIFF_BINARY:?RUE_ORACLE_DIFF_BINARY must point to the differential harness}"
: "${RUE_ORACLE_DIFF_STD:?RUE_ORACLE_DIFF_STD must point to the standard library sources}"
: "${RUE_LEAN_CORPUS:?RUE_LEAN_CORPUS must point to the exported corpus.json}"

# Buck expands `$(location ...)` to a project-relative path, and the harness
# compiles each case in a temporary directory of its own — so every declared
# input is resolved against the invocation directory (the repository root,
# where `./buck2` lives) before anything changes directory, exactly as
# `scripts/corpus-action` does for the cached corpora.
absolutize() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s/%s\n' "$PWD" "$1" ;;
    esac
}

require() {
    if [ ! -e "$2" ]; then
        echo "lean-bridge: $1 does not exist: $2" >&2
        echo "lean-bridge: run this from the repository root (./buck2 run //:lean-bridge)" >&2
        exit 2
    fi
}

RUE_BINARY="$(absolutize "$RUE_BINARY")"
RUE_ORACLE_DIFF_STD="$(absolutize "$RUE_ORACLE_DIFF_STD")"
RUE_LEAN_CORPUS="$(absolutize "$RUE_LEAN_CORPUS")"
harness="$(absolutize "$RUE_ORACLE_DIFF_BINARY")"

require RUE_BINARY "$RUE_BINARY"
require RUE_ORACLE_DIFF_STD "$RUE_ORACLE_DIFF_STD"
require RUE_LEAN_CORPUS "$RUE_LEAN_CORPUS"
require RUE_ORACLE_DIFF_BINARY "$harness"

export RUE_BINARY RUE_ORACLE_DIFF_STD

exec "$harness" lean-corpus --corpus "$RUE_LEAN_CORPUS" "$@"
