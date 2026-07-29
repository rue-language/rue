#!/usr/bin/env bash
# RUE-616/RUE-624: black-box reproducibility contract for programs produced by Rue.
#
# Compile the same logical project in deliberately different environments and
# compare the complete native artifacts.  This is intentionally an end-to-end
# test: neither output is stripped, normalized, re-linked, or otherwise changed
# before cmp sees it.
set -euo pipefail

: "${RUE_BINARY:?RUE_BINARY must point to the Rue compiler}"
: "${RUE_REPRO_FIXTURE:?RUE_REPRO_FIXTURE must point to the reproducibility fixture}"
: "${RUE_STD_DIR:?RUE_STD_DIR must point to the Rue standard library}"
: "${RUE_EXAMPLES_DIR:?RUE_EXAMPLES_DIR must point to the examples directory}"
export RUE_STD_PATH="$RUE_STD_DIR"

scratch="$(mktemp -d)"
cleanup() {
    rm -rf "$scratch"
}
trap cleanup EXIT

# Different-length roots catch accidental absolute-path capture. Different
# output basenames additionally catch filename-derived macOS signing identity:
# the output path selects a destination, not different program semantics.
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
    {
        find . -type f -name '*.rue' -print | sed 's#^\./##'
        find "$RUE_STD_DIR" -type f -name '*.rue' -print
        # ADR-0051 read policy: resolution observes these absent
        # importer-relative arms before the root-relative shared module.
        printf '%s\n' \
            semantic-order/left/shared.rue \
            semantic-order/right/shared.rue
    } | LC_ALL=C sort > sources.manifest
)
(
    cd "$root_b/project"
    {
        find . -type f -name '*.rue' -print | sed 's#^\./##'
        find "$RUE_STD_DIR" -type f -name '*.rue' -print
        printf '%s\n' \
            semantic-order/left/shared.rue \
            semantic-order/right/shared.rue
    } | LC_ALL=C sort -r > sources.manifest
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

assert_semantic_fixture_runs() {
    local program="$1" actual status expected
    set +e
    actual="$("$program" 2>&1)"
    status=$?
    set -e
    expected="$(printf 'left-destructor\n38\n43')"

    if [ "$status" -ne 43 ] || [ "$actual" != "$expected" ]; then
        printf 'FAIL: semantic-order fixture produced an invalid program\n' >&2
        printf '  program: %s\n' "$program" >&2
        printf '  exit: %s (expected 43)\n' "$status" >&2
        printf '  output:\n%s\n' "$actual" >&2
        return 1
    fi
}

assert_macos_signature() {
    local program="$1" metadata
    if [ "$(uname -s)" != "Darwin" ]; then
        return
    fi

    if ! codesign --verify --strict --verbose=4 "$program"; then
        printf 'FAIL: reproducible Mach-O artifact has an invalid code signature: %s\n' \
            "$program" >&2
        return 1
    fi
    metadata="$(codesign --display --verbose=4 "$program" 2>&1)"
    if ! grep -Fqx 'Identifier=dev.rue-lang.program' <<< "$metadata"; then
        printf 'FAIL: Mach-O signing identifier is not stable\n%s\n' "$metadata" >&2
        return 1
    fi
}

assert_relocated_symbol_names() {
    local symbols_a="$root_a/tmp/air-symbols.txt"
    local symbols_b="$root_b/tmp/air-symbols.txt"

    # RUE-618: physical source paths must not enter the machine-level names
    # carried from AIR through assembly and object relocations. Compare only
    # generated names here: complete textual IR ordering is covered by RUE-620.
    (
        cd "$root_a/project"
        "$RUE_BINARY" -j1 --emit air \
            --source-manifest sources.manifest \
            main.rue \
            | sed -n 's/^function \(__rue_fn_[^(: ]*\).*/\1/p' \
            | LC_ALL=C sort -u > "$symbols_a"
    )
    "$RUE_BINARY" -j32 --emit air \
        --source-manifest "$root_b/project/sources.manifest" \
        "$root_b/project/main.rue" \
        | sed -n 's/^function \(__rue_fn_[^(: ]*\).*/\1/p' \
        | LC_ALL=C sort -u > "$symbols_b"

    assert_identical "relocated AIR symbol names" "$symbols_a" "$symbols_b"

    local expected actual
    expected="$(printf '%s\n' \
        '__rue_fn_left_2fentry_2erue__compute' \
        '__rue_fn_left_2fshared_2erue__make' \
        '__rue_fn_right_2fentry_2erue__compute' \
        '__rue_fn_right_2fshared_2erue__make')"
    actual="$(< "$symbols_a")"
    if [ "$actual" != "$expected" ]; then
        printf 'FAIL: reproducibility fixture generated unexpected AIR symbols\n' >&2
        printf '  expected:\n%s\n' "$expected" >&2
        printf '  actual:\n%s\n' "$actual" >&2
        return 1
    fi
}

assert_source_order_independent_ir() {
    local ir_a="$root_a/tmp/source-order-ir.txt"
    local ir_b="$root_b/tmp/source-order-ir.txt"

    # RUE-620 / RUE-767: the unrelated noise siblings are discovered through the
    # root's @import graph (the legacy flat positional listing was removed with
    # flat mode, ADR-0046). main.rue is the sole positional source; it @imports
    # noise-a and noise-b so they still enter the compilation and perturb the
    # interner. Perturbation now comes from parallelism (-j1 vs -j32) and path
    # spelling rather than positional order, which no longer exists.
    (
        cd "$root_a/project/source-order"
        "$RUE_BINARY" -j1 \
            --emit air --emit cfg --emit lowering \
            main.rue > "$ir_a"
    )
    "$RUE_BINARY" -j32 \
        --emit air --emit cfg --emit lowering \
        "$root_b/project/source-order/./main.rue" > "$ir_b"

    assert_identical "source-order root-only AIR/CFG/lowering" "$ir_a" "$ir_b"

    # Make the comparison non-vacuous: each adapter must have resolved the
    # symbols exercised by ordinary, generic, and intrinsic calls.
    local expected_symbol
    for expected_symbol in '@rotate(' '@finish(' '@identity.i32(' '@assert('; do
        if ! grep -Fq "$expected_symbol" "$ir_a"; then
            printf 'FAIL: stable IR omitted resolved symbol %s\n' "$expected_symbol" >&2
            return 1
        fi
    done
}

emit_semantic_order_pair() {
    local opt="$1" target="$2"
    shift 2
    local target_tag="${target//-/_}"
    local output_a="$root_a/tmp/semantic-order-${target_tag}-o${opt}-a.txt"
    local output_b="$root_b/tmp/semantic-order-${target_tag}-o${opt}-b.txt"

    # The siblings are discovered through the root's imports (RUE-920/spec
    # 6.1:38 moved the entry point into the designated root, retiring the
    # positional sibling listing). Perturbation now comes from manifest order,
    # parallelism, environment, and path spelling. shared.rue is deliberately
    # discovered through each nested sibling's root-relative import.
    (
        umask 022
        cd "$root_a/project/semantic-order"
        env \
            LC_ALL=C \
            SOURCE_DATE_EPOCH=1 \
            TMPDIR="$root_a/tmp" \
            TZ=UTC \
            "$RUE_BINARY" "-O$opt" -j1 --target "$target" \
            --source-manifest ../sources.manifest \
            "$@" \
            root.rue > "$output_a"
    )
    (
        umask 077
        env \
            LC_ALL=C \
            SOURCE_DATE_EPOCH=2000000000 \
            TMPDIR="$root_b/tmp" \
            TZ=Pacific/Honolulu \
            "$RUE_BINARY" "-O$opt" -j32 --target "$target" \
            --source-manifest "$root_b/project/sources.manifest" \
            "$@" \
            "$root_b/project/semantic-order/./root.rue" > "$output_b"
    )

    assert_identical \
        "semantic-order $target -O$opt output" \
        "$output_a" "$output_b"
}

assert_semantic_order_symbols() {
    local output="$1" expected_name
    for expected_name in \
        'Payload$left_2ftypes_2erue.__drop' \
        'Payload$left_2ftypes_2erue.score' \
        '__rue_drop_Payload$left_2ftypes_2erue'; do
        if ! grep -Fq "function ${expected_name}:" "$output"; then
            printf 'FAIL: semantic-order output omitted stable symbol %s\n' \
                "$expected_name" >&2
            return 1
        fi
    done

    if grep -Eq '(Payload|Choice|Clash)\$([0-9]+|file[0-9]+)' "$output"; then
        printf 'FAIL: semantic-order output leaked a numeric FileId identity\n' >&2
        return 1
    fi
}

assert_semantic_stage_markers() {
    local output="$1" marker
    for marker in \
        '=== AIR ===' \
        '=== CFG ===' \
        '=== Instruction Selection (' \
        '=== MIR (x86-64-linux) ===' \
        '=== Liveness Analysis (x86-64-linux) ===' \
        '=== Register Allocation (x86-64-linux) ===' \
        '=== Assembly (x86-64-linux) ===' \
        '=== Stack Frame ('; do
        if ! grep -Fq "$marker" "$output"; then
            printf 'FAIL: semantic-order output omitted stage marker %s\n' \
                "$marker" >&2
            return 1
        fi
    done
    if ! grep -Fq '.globl __rue_sem_v1_' "$output"; then
        printf 'FAIL: semantic-order assembly stage emitted no pinned function body\n' >&2
        return 1
    fi
}

assert_cross_target_assembly() {
    local target="$1" output="$2"
    if ! grep -Fq "=== Assembly ($target) ===" "$output" ||
        ! grep -Fq '.globl __rue_sem_v1_' "$output"; then
        printf 'FAIL: %s assembly output is empty or incomplete\n' "$target" >&2
        return 1
    fi
}

compile_pair() {
    local opt="$1"
    local output_a="$root_a/project/program-left-o$1"
    local output_b="$root_b/project/program-right-with-long-name-o$1"

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
    assert_macos_signature "$output_a"
    assert_macos_signature "$output_b"
}

compile_semantic_order_pair() {
    local opt="$1"
    local output_a="$root_a/project/semantic-order/program-forward-o$1"
    local output_b="$root_b/project/semantic-order/program-reverse-with-long-name-o$1"

    (
        umask 022
        cd "$root_a/project/semantic-order"
        env \
            LC_ALL=C \
            SOURCE_DATE_EPOCH=1 \
            TMPDIR="$root_a/tmp" \
            TZ=UTC \
            "$RUE_BINARY" "-O$opt" -j1 \
            --source-manifest ../sources.manifest \
            root.rue -o "$output_a"
    )
    (
        umask 077
        env \
            LC_ALL=C \
            SOURCE_DATE_EPOCH=2000000000 \
            TMPDIR="$root_b/tmp" \
            TZ=Pacific/Honolulu \
            "$RUE_BINARY" "-O$opt" -j32 \
            --source-manifest "$root_b/project/sources.manifest" \
            "$root_b/project/semantic-order/./root.rue" -o "$output_b"
    )

    assert_identical "semantic-order native -O$opt program" "$output_a" "$output_b"
    assert_semantic_fixture_runs "$output_a"
    assert_semantic_fixture_runs "$output_b"
    assert_macos_signature "$output_a"
    assert_macos_signature "$output_b"
}

# RUE-1083: a real, maintained multi-module program, checked the same way.
#
# Everything above compiles `reproducibility/fixture`, a corpus built for this
# test. That left a gap: no program anyone actually maintains was checked for
# byte-stable output, and a genuine nondeterminism bug lived in drop-flag slot
# allocation without any suite noticing. It surfaced only when the incrementality
# value audit compared two compiles of `examples/rill/main.rue` and got different
# executables.
#
# Rill is the cheap end of the regressed-example set and exercises what the
# purpose-built fixture does not: a 19-file import graph with real destructors
# and moved-out struct fields, which is exactly the shape that drives
# `arm_field_drop_flags`. This deliberately reuses the same perturbations as
# `compile_pair` — root-path length, mtimes, manifest spelling, parallelism,
# umask, TMPDIR, TZ, and SOURCE_DATE_EPOCH — so a failure means the compiler,
# not the environment.
assert_example_reproducible() {
    local name="$1" opt="$2"
    local src="$RUE_EXAMPLES_DIR/$name"
    if [ ! -d "$src" ]; then
        printf 'FAIL: example corpus %s is missing at %s\n' "$name" "$src" >&2
        return 1
    fi

    local dir_a="$root_a/examples/$name"
    local dir_b="$root_b/examples/$name"
    rm -rf "$dir_a" "$dir_b"
    mkdir -p "$dir_a" "$dir_b"
    cp -R "$src/." "$dir_a/"
    cp -R "$src/." "$dir_b/"
    find "$dir_a" -type f -name '*.rue' -exec touch -t 200001010000 {} +
    find "$dir_b" -type f -name '*.rue' -exec touch -t 203001010000 {} +

    local output_a="$root_a/tmp/example-$name-o$opt"
    local output_b="$root_b/tmp/example-$name-o$opt"
    (
        umask 022
        cd "$dir_a"
        env \
            LC_ALL=C \
            SOURCE_DATE_EPOCH=1 \
            TMPDIR="$root_a/tmp" \
            TZ=UTC \
            "$RUE_BINARY" "-O$opt" -j1 main.rue -o "$output_a"
    )
    (
        umask 077
        env \
            LC_ALL=C \
            SOURCE_DATE_EPOCH=2000000000 \
            TMPDIR="$root_b/tmp" \
            TZ=Pacific/Honolulu \
            "$RUE_BINARY" "-O$opt" -j32 "$dir_b/./main.rue" -o "$output_b"
    )

    assert_identical "example $name native -O$opt program" "$output_a" "$output_b"
    assert_macos_signature "$output_a"
    assert_macos_signature "$output_b"
}

assert_relocated_symbol_names
assert_source_order_independent_ir

# RUE-624: compare every semantic/backend textual stage without filtering or
# sorting, at both optimization levels. Early tokens/AST/RIR/deps intentionally
# remain caller/discovery ordered and are therefore outside this equality set.
semantic_stages=(
    --emit air
    --emit cfg
    --emit lowering
    --emit mir
    --emit liveness
    --emit regalloc
    --emit asm
    --emit stackframe
)
emit_semantic_order_pair 0 x86-64-linux "${semantic_stages[@]}"
emit_semantic_order_pair 2 x86-64-linux "${semantic_stages[@]}"
assert_semantic_order_symbols \
    "$root_a/tmp/semantic-order-x86_64_linux-o0-a.txt"
assert_semantic_stage_markers \
    "$root_a/tmp/semantic-order-x86_64_linux-o0-a.txt"
assert_semantic_stage_markers \
    "$root_a/tmp/semantic-order-x86_64_linux-o2-a.txt"

# Foreign-target emission never links, so every CI host can verify the two
# other supported object-model/codegen paths as exact assembly text.
for cross_target in aarch64-linux aarch64-macos; do
    emit_semantic_order_pair 0 "$cross_target" --emit asm
    emit_semantic_order_pair 2 "$cross_target" --emit asm
    cross_target_tag="${cross_target//-/_}"
    assert_cross_target_assembly "$cross_target" \
        "$root_a/tmp/semantic-order-${cross_target_tag}-o0-a.txt"
    assert_cross_target_assembly "$cross_target" \
        "$root_a/tmp/semantic-order-${cross_target_tag}-o2-a.txt"
done

compile_pair 0
compile_pair 2
compile_semantic_order_pair 0
compile_semantic_order_pair 2

assert_example_reproducible rill 0
assert_example_reproducible rill 2
