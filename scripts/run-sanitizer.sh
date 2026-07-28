#!/usr/bin/env bash
#
# run-sanitizer.sh — compile a corpus of Rue programs and run every resulting
# binary under Valgrind's memcheck, failing if any program triggers a memory
# error or a fatal signal.
#
# This is the local driver for the Valgrind job in `ci.yml` (RUE-49). Run it from
# anywhere in the repo:
#
#   scripts/run-sanitizer.sh                 # recursive examples + curated corpus
#   scripts/run-sanitizer.sh path/to/x.rue   # ...plus extra programs
#
# Requirements: `valgrind` on PATH (CI installs it via apt-get). The rue
# compiler is built on demand via scripts/rue-bin; set RUE_BINARY to reuse an
# already-built binary.
#
# ---------------------------------------------------------------------------
# What Valgrind does and does NOT catch on Rue binaries (verified 2026-07-02,
# valgrind-3.22 on x86-64):
#
# Rue emits static, no-libc, direct-syscall ELF executables. Valgrind's dynamic
# binary instrumentation loads and runs them WITHOUT recompilation, so memcheck
# works out of the box — no ASAN build needed.
#
# HOWEVER: memcheck's heap tracking hooks libc malloc/free. Rue's runtime never
# calls libc; small allocations use recycled blocks in 64 KiB mmap'd arenas,
# while large allocations use dedicated mappings
# (crates/rue-allocator/src/lib.rs). To memcheck every Rue program therefore
# still does "0 allocs, 0 frees" — each arena is one opaque, addressable mmap'd
# region. Consequences:
#
#   CAUGHT: wild pointers / accesses outside any mapped region, stack overflow,
#           jumps to bad addresses, uninitialised values used in branches or
#           passed to syscalls, bad syscall buffers — i.e. CODEGEN and
#           generated-code correctness bugs, which the fuzz job (source-level,
#           never runs a binary) cannot see.
#
#   MISSED: intra-arena heap overflow / use-after-free such as RUE-34 (a StrBuf
#           grow path recording more capacity than it allocated). The overflow
#           write lands inside the mmap'd arena, which memcheck considers valid,
#           so no error fires. Catching that class needs the runtime's own unit
#           tests built under AddressSanitizer against a real allocator; that is
#           a separate surface tracked as follow-up work (blocked on a cargo /
#           buck2 sanitizer toolchain for rue-runtime — see the RUE-49 PR).
#
# So this job is a codegen/generated-code memory-safety net, complementary to
# (not a replacement for) an ASAN pass over rue-runtime.
# ---------------------------------------------------------------------------
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Examples may import the bundled standard library. Match the developer
# wrappers' default while preserving an explicit caller override; without this
# the sanitizer fails during compilation and never reaches Valgrind (RUE-590).
export RUE_STD_PATH="${RUE_STD_PATH:-$repo_root/std}"

# Full Caldera and Meridian roots are expensive compile inputs, so their
# Valgrind ownership is explicit. Required pre-merge sanitizer coverage sets
# `none`; manual sanitizer dispatch can select either program or both. This
# avoids an unannounced directory-specific skip while keeping the ordinary
# recursive corpus bounded.
large_programs="${RUE_SANITIZER_LARGE_PROGRAMS:-none}"
case "$large_programs" in
    none|caldera|meridian|all) ;;
    *)
        echo "run-sanitizer: RUE_SANITIZER_LARGE_PROGRAMS must be none, caldera, meridian, or all" >&2
        exit 2
        ;;
esac
echo "run-sanitizer: large-program policy is $large_programs" >&2

large_program_selected() {
    local program="$1"
    [[ "$large_programs" == "all" || "$large_programs" == "$program" ]]
}

if ! command -v valgrind >/dev/null 2>&1; then
    echo "run-sanitizer: valgrind not found on PATH." >&2
    echo "run-sanitizer: install it (Debian/Ubuntu: apt-get install valgrind)." >&2
    exit 127
fi

# Build (or reuse) the compiler.
if [[ -n "${RUE_BINARY:-}" ]]; then
    rue="$RUE_BINARY"
else
    echo "run-sanitizer: building rue compiler..." >&2
    rue="$(scripts/rue-bin)"
fi
if [[ ! -x "$rue" ]]; then
    echo "run-sanitizer: rue binary is not executable: $rue" >&2
    exit 1
fi

work="$(mktemp -d)"
echo "run-sanitizer: workdir $work" >&2

# --- Curated corpus: small programs that lean on heap/aggregate paths the
#     stock examples touch only lightly. Kept here (not in examples/) so they
#     can hammer growth loops without cluttering the user-facing examples.
cat > "$work/san_str_grow.rue" <<'RUE'
// Many push_str calls -> repeated StrBuf realloc (the RUE-34 grow path).
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let mut s = StrBuf.new();
    let mut i = 0;
    while i < 500 {
        s.push_str("abcdefghij");
        i = i + 1;
    }
    let n: u64 = 5000;
    if s.len() == n { 0 } else { 1 }
}
RUE

cat > "$work/san_str_push_char.rue" <<'RUE'
// Byte-at-a-time growth: exercises the smallest grow increments.
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let mut s = StrBuf.new();
    let mut i = 0;
    while i < 300 {
        s.push(65);
        i = i + 1;
    }
    let n: u64 = 300;
    if s.len() == n { 0 } else { 1 }
}
RUE

cat > "$work/san_str_mixed.rue" <<'RUE'
// with_capacity + mixed appends: pre-sized buffer then repeated growth.
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let mut s = StrBuf.with_capacity(4);
    s.push_str("hello");
    s.push_str(" world");
    let mut i = 0;
    while i < 50 {
        s.push_str("xyz");
        i = i + 1;
    }
    let expected: u64 = 161;
    if s.len() == expected { 0 } else { 1 }
}
RUE

cat > "$work/san_struct_heavy.rue" <<'RUE'
// Struct construction / field access in a loop (aggregate slot codegen).
@copy
struct Point { x: i32, y: i32 }
fn main() -> i32 {
    let mut sum = 0;
    let mut i = 0;
    while i < 100 {
        let p = Point { x: i, y: i + 1 };
        sum = sum + p.x + p.y;
        i = i + 1;
    }
    if sum == 10000 { 0 } else { 1 }
}
RUE

cat > "$work/san_fallible_initializer.rue" <<'RUE'
// EOF returns from the initializer before `line` owns a StrBuf. Cleanup must
// end its storage without reading or dropping the uninitialized slot.
// `?` on a fallible intrinsic resolves the trusted std Option by identity
// (RUE-1112), so the helper returns std.option.Option rather than a local
// structurally Option-shaped enum.
const std = @import("std");

fn read_num() -> std.option.Option(i64) {
    let line = @read_line()?;
    @parse_i64(line)
}

fn main() -> i32 {
    let O = std.option.Option(i64);
    match read_num() {
        O.Some(_) => 1,
        O.None => 0,
    }
}
RUE

# --- Assemble the program list. Match the CLI smoke-test's root-module rule:
#     a directory containing main.rue contributes that root and is not searched
#     further; elsewhere, every .rue file is a standalone example. This keeps
#     helper modules from being compiled as programs while still finding nested
#     root-module and ordinary examples.
programs=()
contracts=()
labels=()

add_program() {
    programs+=("$1")
    contracts+=("$2")
    labels+=("$3")
}

discover_examples() {
    local dir="$1"
    local entry
    if [[ "$dir" == "$repo_root/examples/caldera" ]] &&
        ! large_program_selected caldera; then
        echo "run-sanitizer: excluding examples/caldera/main.rue by explicit policy" >&2
        return
    fi
    if [[ "$dir" == "$repo_root/examples/meridian" ]] &&
        ! large_program_selected meridian; then
        echo "run-sanitizer: excluding examples/meridian/main.rue by explicit policy" >&2
        return
    fi
    if [[ -f "$dir/main.rue" ]]; then
        add_program "$dir/main.rue" "memory-only" "${dir#"$repo_root/"}/main.rue"
        return
    fi

    # Include hidden entries just as std::fs::read_dir does in the CLI harness.
    # nullglob keeps unmatched patterns from becoming literal path strings.
    for entry in "$dir"/* "$dir"/.[!.]* "$dir"/..?*; do
        if [[ -d "$entry" ]]; then
            discover_examples "$entry"
        elif [[ -f "$entry" && "$entry" == *.rue ]]; then
            add_program "$entry" "memory-only" "${entry#"$repo_root/"}"
        fi
    done
}

shopt -s nullglob
if [[ ! -d "$repo_root/examples" ]]; then
    echo "run-sanitizer: examples directory is missing: $repo_root/examples" >&2
    exit 1
fi
discover_examples "$repo_root/examples"
example_count=${#programs[@]}
if [[ "$example_count" -eq 0 ]]; then
    echo "run-sanitizer: no .rue examples discovered under $repo_root/examples" >&2
    exit 1
fi

# These nested examples are coverage canaries for both discovery branches: a
# dogfood root-module tree and an ordinary source under the std examples tree.
required_examples=(
    "$repo_root/examples/calculator/main.rue"
    "$repo_root/examples/std/arraybuf_demo.rue"
)
for required in "${required_examples[@]}"; do
    found=0
    for src in "${programs[@]}"; do
        if [[ "$src" == "$required" ]]; then
            found=1
            break
        fi
    done
    if [[ "$found" -eq 0 ]]; then
        echo "run-sanitizer: required example sentinel was not discovered: ${required#"$repo_root/"}" >&2
        exit 1
    fi
done

# Curated self-checks must return zero. Repository examples and caller-supplied
# programs use the memory-only contract: any normal exit code is accepted once
# Memcheck reports clean, because many examples intentionally return nonzero.
for f in "$work"/san_*.rue; do
    add_program "$f" "exit:0" "curated/$(basename "$f")"
done
for f in "$@"; do
    add_program "$f" "memory-only" "$f"
done

# GNU timeout's status 124 is also a valid program exit. Run Valgrind through a
# tiny completion recorder so an actual timeout (no marker) is distinguishable
# from a child that completed normally with status 124 (marker contains 124).
cat > "$work/run-and-record-status.sh" <<'SH'
#!/usr/bin/env bash
status_file="$1"
shift
set +e
"$@"
status=$?
printf '%s\n' "$status" > "$status_file"
exit "$status"
SH
chmod +x "$work/run-and-record-status.sh"

failures=0
checked=0
for ((i = 0; i < ${#programs[@]}; i++)); do
    src="${programs[$i]}"
    contract="${contracts[$i]}"
    label="${labels[$i]}"
    artifact="$(printf '%04d' "$i")"
    bin="$work/$artifact.bin"
    log="$work/$artifact.vglog"
    compile_log="$work/$artifact.compile"
    status_file="$work/$artifact.status"

    if ! "$rue" "$src" -o "$bin" > "$compile_log" 2>&1; then
        echo "FAIL(compile) $label"
        sed 's/^/    /' "$compile_log"
        failures=$((failures + 1))
        continue
    fi

    # Cap runtime: valgrind adds heavy slowdown and a codegen bug could loop.
    # Capture timeout/Valgrind's status explicitly; `set -e` is suspended only
    # around this expected-to-be-nonzero probe.
    set +e
    timeout 120 "$work/run-and-record-status.sh" "$status_file" valgrind \
        --error-exitcode=125 \
        --leak-check=full \
        --show-leak-kinds=all \
        --track-origins=yes \
        --log-file="$log" \
        "$bin" > /dev/null 2>&1
    timeout_status=$?
    set -e

    checked=$((checked + 1))

    if [[ ! -f "$status_file" && "$timeout_status" -eq 124 ]]; then
        echo "FAIL(timeout) $label (status $timeout_status; no completion marker)"
        failures=$((failures + 1))
        continue
    elif [[ ! -f "$status_file" ]]; then
        echo "FAIL(valgrind) $label (wrapper status $timeout_status; no completion marker)"
        failures=$((failures + 1))
        continue
    fi

    status="$(sed -n '1p' "$status_file")"
    if [[ ! "$status" =~ ^[0-9]+$ ]]; then
        echo "FAIL(valgrind) $label (invalid recorded status: $status)"
        failures=$((failures + 1))
    elif [[ ! -f "$log" ]]; then
        echo "FAIL(valgrind) $label (status $status; no Memcheck log)"
        failures=$((failures + 1))
    elif grep -q "Process terminating with default action of signal" "$log"; then
        echo "FAIL(signal) $label (status $status)"
        sed 's/^/    /' "$log"
        failures=$((failures + 1))
    elif grep -qE "ERROR SUMMARY: [1-9][0-9]* errors" "$log"; then
        echo "FAIL(memcheck) $label (Valgrind status $status)"
        sed 's/^/    /' "$log"
        failures=$((failures + 1))
    elif ! grep -q "ERROR SUMMARY: 0 errors" "$log"; then
        echo "FAIL(valgrind) $label (status $status; missing clean summary)"
        sed 's/^/    /' "$log"
        failures=$((failures + 1))
    elif [[ "$contract" == exit:* && "$status" -ne "${contract#exit:}" ]]; then
        echo "FAIL(program) $label (expected exit ${contract#exit:}, got $status)"
        failures=$((failures + 1))
    else
        echo "PASS $label (program status $status; contract $contract)"
    fi
done

echo
echo "run-sanitizer: checked $checked program(s), $failures failure(s)."
if [[ "$failures" -ne 0 ]]; then
    exit 1
fi
