#!/usr/bin/env bash
# Negative control 3 of ADR-0070 (RUE-1404): digest sensitivity of the
# rue_program action chain, in the check-reproducible-compiler.sh mould — a CI
# script OUTSIDE the Buck graph, because asserting "this action re-executed"
# requires driving buck2 and reading its execution log, which no Buck-visible
# test in this repository does un-stubbed.
#
# Three assertions, and the third is the one nothing else covers:
#   1. mutating a DECLARED source re-runs the scan (and derive);
#   2. reverting it restores the previous key (cache hit, no rue_* execution);
#   3. mutating an UNDECLARED neighbour (the boundary fixture's extra.rue is
#      deliberately outside its srcs) runs NO rue_* action at all — the exact
#      false-pass window corpus.bzl:19-25 warns about, made a hard check.
#
# Mutations are trap-restored; run from a clean checkout.
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
    # what-ran reports one line per executed (non-cached) action; rue_scan /
    # rue_derive_manifest / rue_compile are this rule's categories.
    "$BUCK2" log what-ran 2>/dev/null | grep -cE "rue_scan|rue_derive_manifest|rue_compile" || true
}

echo "digest-check: baseline build"
"$BUCK2" build "$TARGET" >/dev/null 2>&1

echo "digest-check: (1) mutating declared source must re-run the scan"
printf '\n// digest-sensitivity probe\n' >> "$DECLARED"
"$BUCK2" build "$TARGET" >/dev/null 2>&1
ran="$(rue_actions_of_last_build)"
if [[ "$ran" -lt 1 ]]; then
    echo "FAIL: declared-source mutation executed no rue_* actions" >&2
    exit 1
fi
echo "  ok: $ran rue_* action(s) re-ran"

echo "digest-check: (2) reverting must be a cache hit"
restore
"$BUCK2" build "$TARGET" >/dev/null 2>&1
ran="$(rue_actions_of_last_build)"
if [[ "$ran" -ne 0 ]]; then
    echo "FAIL: reverted tree re-executed $ran rue_* action(s) instead of hitting cache" >&2
    exit 1
fi
echo "  ok: cache hit"

echo "digest-check: (3) mutating an undeclared neighbour must run nothing"
printf '\n// digest-sensitivity probe\n' >> "$UNDECLARED"
"$BUCK2" build "$TARGET" >/dev/null 2>&1
ran="$(rue_actions_of_last_build)"
if [[ "$ran" -ne 0 ]]; then
    echo "FAIL: undeclared-neighbour mutation re-executed $ran rue_* action(s)" >&2
    exit 1
fi
echo "  ok: no rue_* actions"

echo "digest-check: PASS"
