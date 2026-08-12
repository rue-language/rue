#!/usr/bin/env bash
# The positive warm-cache control of ADR-0070 (RUE-1405 Phase 1, extended by
# RUE-1406 Phase 2): a second build from a RELOCATED checkout root must show
# the rue_program scan/derive/compile actions served by the remote action cache
# while more than one scenario consumes each executable — the cross-root
# property the derive step's manifest re-anchoring guarantees, and what lets a
# pull_request run's compile serve the merge_group run.
#
# Two consumer shapes are covered, because ADR-0070 has two:
#   * the large-example canaries, each consumed by two `rue_program_test`
#     scenarios (Phase 1);
#   * the nine CLI roots in //:cli-staged-programs, consumed by the CLI corpus
#     ACTIONS — //:cli-tests, //:cli-tests-slow and the four shards all declare
#     that one directory, and the 64 TOML cases naming those roots run the
#     staged executables instead of compiling them (Phase 2).
#
# Lives outside the Buck graph (asserting cache hits means reading buck2's
# execution log) and REQUIRES the remote cache — without one the check is
# vacuous, so a missing provisioned config is a hard failure. The caller
# supplies RUE_CACHE_PROBE_NONCE (absolute RUE_BUCK2 optional), the toolchain
# nonce cache-probe.yml already uses, so the first build is provably cold.
#
# Shape:
#   1. cold build of the two canaries and the staged CLI programs in this
#      checkout, under the nonce — populates the remote cache;
#   2. clone the checkout to a different absolute root, run the four canary
#      scenario tests AND rebuild the staged directory there under the same
#      nonce;
#   3. assert every rue_scan/rue_derive_manifest/rue_compile action for those
#      programs in the relocated invocation was a cache hit, and that scan and
#      compile hits exist for EVERY program.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

BUCK2="${RUE_BUCK2:-./buck2}"
: "${RUE_CACHE_PROBE_NONCE:?RUE_CACHE_PROBE_NONCE must carry a fresh per-run nonce}"

NONCE_ARGS=(-c "rue.cache_probe_nonce=$RUE_CACHE_PROBE_NONCE")
# Phase 1's canaries, then Phase 2's nine staged CLI roots. `meridian` is in
# both populations by design: one artifact serves the slow-tier large-example
# scenarios and the six cases of cases/examples_meridian.toml.
PROGRAMS=(
    caldera-canary
    meridian-canary
    first-stats
    harbor
    jsonfmt
    lattice
    meridian
    mosaic
    rill
    ruelex
    second-calculator
)
BUILD_TARGETS=(//:caldera-canary //:meridian-canary //:cli-staged-programs)
SCENARIOS=(
    //:large-example-caldera-canary
    //:large-example-caldera-canary-workdir
    //:large-example-meridian-canary
    //:large-example-meridian-canary-workdir
)

if ! grep -Eq '^[[:space:]]*execution_platforms[[:space:]]*=[[:space:]]*root//platforms:remote_cache([[:space:]]|$)' .buckconfig.local 2>/dev/null; then
    echo "FAIL: no provisioned remote cache (.buckconfig.local); a warm-cache check without one is vacuous" >&2
    exit 1
fi

reloc_parent="$(mktemp -d)"
reloc="$reloc_parent/rue-relocated"
cleanup() {
    if [[ -d "$reloc" ]]; then
        (cd "$reloc" && "${RUE_BUCK2:-./buck2}" kill >/dev/null 2>&1) || true
    fi
    rm -rf "$reloc_parent"
}
trap cleanup EXIT

# what-ran identities render as "(<category> <identifier>)"; the identifier is
# the program target's name. $1 is the invocation's project root.
program_alternation="$(IFS='|'; echo "${PROGRAMS[*]}")"
program_rue_lines() {
    (cd "$1" && "$BUCK2" log what-ran 2>/dev/null) |
        grep -E "\((rue_scan|rue_derive_manifest|rue_compile) ($program_alternation)\)" || true
}

echo "warm-cache: (1) cold program build under nonce $RUE_CACHE_PROBE_NONCE"
"$BUCK2" build "${NONCE_ARGS[@]}" "${BUILD_TARGETS[@]}" >/dev/null
cold_lines="$(program_rue_lines .)"
if [[ -z "$cold_lines" ]]; then
    echo "FAIL: cold build executed no rue_* actions — the nonce did not create a cold namespace" >&2
    exit 1
fi
if awk -F'\t' '$3 ~ /^cache/ { found = 1 } END { exit !found }' <<<"$cold_lines"; then
    echo "FAIL: cold build was cache-served; the nonce did not create a cold namespace:" >&2
    sed 's/^/    /' <<<"$cold_lines" | cut -c1-200 >&2
    exit 1
fi
echo "  ok: $(wc -l <<<"$cold_lines") rue_* action(s) executed cold"

echo "warm-cache: (2) relocated checkout stages the CLI programs and runs the canary scenarios"
git clone --quiet --no-hardlinks . "$reloc"
# .buckconfig.local is private and untracked, so the clone starts without a
# remote cache; provision it explicitly rather than relying on the wrapper's
# auto-follow, and fail here — not at the misleading cache-miss assertion —
# if the relocated root still has no cache config.
scripts/provision-build-cache apply "$reloc" >/dev/null
if ! grep -Eq '^[[:space:]]*execution_platforms[[:space:]]*=[[:space:]]*root//platforms:remote_cache([[:space:]]|$)' "$reloc/.buckconfig.local" 2>/dev/null; then
    echo "FAIL: relocated checkout has no provisioned remote cache" >&2
    exit 1
fi
# `buck2 log what-ran` reports the LAST invocation, so the two consumer shapes
# are driven separately and their logs are collected as each finishes. The two
# invocations name disjoint programs, so neither hides the other's actions.
(cd "$reloc" && "${RUE_BUCK2:-./buck2}" build "${NONCE_ARGS[@]}" //:cli-staged-programs >/dev/null 2>&1) || {
    echo "FAIL: staging the CLI programs failed in the relocated checkout" >&2
    exit 1
}
warm_lines="$(program_rue_lines "$reloc")"
(cd "$reloc" && "${RUE_BUCK2:-./buck2}" test "${NONCE_ARGS[@]}" "${SCENARIOS[@]}" >/dev/null 2>&1) || {
    echo "FAIL: canary scenario tests failed in the relocated checkout" >&2
    exit 1
}
warm_lines="$(printf '%s\n%s\n' "$warm_lines" "$(program_rue_lines "$reloc")" | grep -v '^$' || true)"
echo "  ok"

echo "warm-cache: (3) relocated scan/derive/compile must all be cache hits"
if [[ -z "$warm_lines" ]]; then
    echo "FAIL: relocated invocations log no rue_* actions" >&2
    exit 1
fi
misses="$(awk -F'\t' '$3 !~ /^cache/' <<<"$warm_lines" || true)"
if [[ -n "$misses" ]]; then
    echo "FAIL: relocated build re-executed rue_* actions instead of taking cache hits:" >&2
    sed 's/^/    /' <<<"$misses" | cut -c1-200 >&2
    exit 1
fi
for program in "${PROGRAMS[@]}"; do
    for category in rue_scan rue_compile; do
        if ! grep -qF "($category $program)" <<<"$warm_lines"; then
            echo "FAIL: relocated log has no cache-served $category for $program" >&2
            exit 1
        fi
    done
done
echo "  ok: $(wc -l <<<"$warm_lines") cache-served rue_* action(s) across ${#PROGRAMS[@]} programs, ${#SCENARIOS[@]} consuming scenarios and the staged CLI corpus inputs"

echo "warm-cache: PASS"
