#!/usr/bin/env bash
# RUE-616: black-box reproducibility contract for programs produced by Rue.
#
# Compile the same logical project in deliberately different environments and
# compare the complete native artifacts.  This is intentionally an end-to-end
# test: neither output is stripped, normalized, re-linked, or otherwise changed
# before cmp sees it.
set -euo pipefail

: "${RUE_BINARY:?RUE_BINARY must point to the Rue compiler}"
: "${RUE_REPRO_FIXTURE:?RUE_REPRO_FIXTURE must point to the reproducibility fixture}"

scratch="$(mktemp -d)"
cleanup() {
    rm -rf "$scratch"
}
trap cleanup EXIT

# Different-length roots catch accidental absolute-path capture.  Keep the
# output basenames equal: filename-derived macOS signing identity is covered by
# the dedicated RUE-619 test, while this test varies the containing path.
root_a="$scratch/a"
root_b="$scratch/a-deliberately-much-longer-reproducibility-root"
mkdir -p "$root_a/project" "$root_b/project" "$root_a/tmp" "$root_b/tmp"
cp -R "$RUE_REPRO_FIXTURE/." "$root_a/project/"
cp -R "$RUE_REPRO_FIXTURE/." "$root_b/project/"

# Source mtimes and manifest order are not semantic inputs.  Perturb both while
# declaring the same source set to the compiler.
find "$root_a/project" -type f -name '*.rue' -exec touch -t 200001010000 {} +
find "$root_b/project" -type f -name '*.rue' -exec touch -t 203001010000 {} +
(
    cd "$root_a/project"
    find . -type f -name '*.rue' -print | sed 's#^\./##' | LC_ALL=C sort > sources.manifest
)
(
    cd "$root_b/project"
    find . -type f -name '*.rue' -print | sed 's#^\./##' | LC_ALL=C sort -r > sources.manifest
)

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

assert_identical() {
    local label="$1" left="$2" right="$3"
    if cmp -s "$left" "$right"; then
        printf 'ok: %s (%s)\n' "$label" "$(sha256 "$left")"
        return
    fi

    local first_diff
    first_diff="$(cmp -l "$left" "$right" 2>/dev/null | awk 'NR == 1 { print $1; exit }' || true)"
    printf 'FAIL: %s is not byte-reproducible\n' "$label" >&2
    printf '  first:  %s (%s bytes, sha256 %s)\n' \
        "$left" "$(wc -c < "$left" | tr -d ' ')" "$(sha256 "$left")" >&2
    printf '  second: %s (%s bytes, sha256 %s)\n' \
        "$right" "$(wc -c < "$right" | tr -d ' ')" "$(sha256 "$right")" >&2
    if [ -n "$first_diff" ]; then
        printf '  first differing byte offset (1-based): %s\n' "$first_diff" >&2
    fi
    return 1
}

assert_fixture_runs() {
    local program="$1" actual status expected
    set +e
    actual="$("$program" 2>&1)"
    status=$?
    set -e
    expected="$(printf '1017\n2033\nleft-before-right')"

    if [ "$status" -ne 78 ] || [ "$actual" != "$expected" ]; then
        printf 'FAIL: reproducibility fixture produced an invalid program\n' >&2
        printf '  exit: %s (expected 78)\n' "$status" >&2
        printf '  output:\n%s\n' "$actual" >&2
        return 1
    fi
}

compile_pair() {
    local opt="$1" name="program-o$1"
    local output_a="$root_a/project/$name" output_b="$root_b/project/$name"

    # First build: relative root spelling, forward manifest, serial compiler,
    # old epoch, permissive umask, and one ambient timezone.
    (
        umask 022
        cd "$root_a/project"
        env \
            LC_ALL=C \
            SOURCE_DATE_EPOCH=1 \
            TMPDIR="$root_a/tmp" \
            TZ=UTC \
            "$RUE_BINARY" "-O$opt" -j1 \
            --source-manifest sources.manifest \
            main.rue -o "$output_a"
    )

    # Second build: absolute spellings, reverse manifest, parallel compiler,
    # future epoch, restrictive umask, and a different timezone.  Each command
    # is a fresh process, so Rust HashMap seeds are independently randomized.
    (
        umask 077
        env \
            LC_ALL=C \
            SOURCE_DATE_EPOCH=2000000000 \
            TMPDIR="$root_b/tmp" \
            TZ=Pacific/Honolulu \
            "$RUE_BINARY" "-O$opt" -j32 \
            --source-manifest "$root_b/project/sources.manifest" \
            "$root_b/project/main.rue" -o "$output_b"
    )

    assert_identical "native -O$opt program" "$output_a" "$output_b"
    # Keep the adversarial fixture honest: byte-identical but broken artifacts
    # must not make this test vacuously green.
    assert_fixture_runs "$output_a"
}

compile_pair 0
compile_pair 2
