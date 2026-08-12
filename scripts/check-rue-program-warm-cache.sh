#!/usr/bin/env bash
# The positive warm-cache control of ADR-0070 Phase 1 (RUE-1405): a second
# build from a RELOCATED checkout root must show the canary rue_program
# scan/derive/compile actions served by the remote action cache while more
# than one scenario consumes each executable — the cross-root property the
# derive step's manifest re-anchoring guarantees, and what lets a
# pull_request run's compile serve the merge_group run.
#
# Lives outside the Buck graph (asserting cache hits means reading buck2's
# execution log) and REQUIRES the remote cache — without one the check is
# vacuous, so a missing provisioned config is a hard failure. The caller
# supplies RUE_CACHE_PROBE_NONCE (absolute RUE_BUCK2 optional), the toolchain
# nonce cache-probe.yml already uses, so the first build is provably cold.
#
# Shape:
#   1. cold build of //:caldera-canary and //:meridian-canary in this
#      checkout, under the nonce — populates the remote cache;
#   2. clone the checkout to a different absolute root, run the four canary
#      scenario tests there under the same nonce;
#   3. assert every rue_scan/rue_derive_manifest/rue_compile action for the
#      canary programs in the relocated invocation was a cache hit, and that
#      scan and compile hits exist for BOTH programs.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

BUCK2="${RUE_BUCK2:-./buck2}"
: "${RUE_CACHE_PROBE_NONCE:?RUE_CACHE_PROBE_NONCE must carry a fresh per-run nonce}"

NONCE_ARGS=(-c "rue.cache_probe_nonce=$RUE_CACHE_PROBE_NONCE")
PROGRAMS=(caldera-canary meridian-canary)
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
canary_rue_lines() {
    (cd "$1" && "$BUCK2" log what-ran 2>/dev/null) |
        grep -E "\((rue_scan|rue_derive_manifest|rue_compile) (caldera-canary|meridian-canary)\)" || true
}

echo "warm-cache: (1) cold canary build under nonce $RUE_CACHE_PROBE_NONCE"
"$BUCK2" build "${NONCE_ARGS[@]}" //:caldera-canary //:meridian-canary >/dev/null
cold_lines="$(canary_rue_lines .)"
if [[ -z "$cold_lines" ]]; then
    echo "FAIL: cold build executed no canary rue_* actions — the nonce did not create a cold namespace" >&2
    exit 1
fi
if awk -F'\t' '$3 ~ /^cache/ { found = 1 } END { exit !found }' <<<"$cold_lines"; then
    echo "FAIL: cold build was cache-served; the nonce did not create a cold namespace:" >&2
    sed 's/^/    /' <<<"$cold_lines" | cut -c1-200 >&2
    exit 1
fi
echo "  ok: $(wc -l <<<"$cold_lines") rue_* action(s) executed cold"

echo "warm-cache: (2) relocated checkout runs all four canary scenarios"
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
(cd "$reloc" && "${RUE_BUCK2:-./buck2}" test "${NONCE_ARGS[@]}" "${SCENARIOS[@]}" >/dev/null 2>&1) || {
    echo "FAIL: canary scenario tests failed in the relocated checkout" >&2
    exit 1
}
echo "  ok"

echo "warm-cache: (3) relocated scan/derive/compile must all be cache hits"
warm_lines="$(canary_rue_lines "$reloc")"
if [[ -z "$warm_lines" ]]; then
    echo "FAIL: relocated invocation log shows no canary rue_* actions" >&2
    exit 1
fi
misses="$(awk -F'\t' '$3 !~ /^cache/' <<<"$warm_lines" || true)"
if [[ -n "$misses" ]]; then
    echo "FAIL: relocated build re-executed canary rue_* actions instead of taking cache hits:" >&2
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
echo "  ok: $(wc -l <<<"$warm_lines") cache-served rue_* action(s) across ${#PROGRAMS[@]} programs, ${#SCENARIOS[@]} consuming scenarios"

echo "warm-cache: PASS"
