#!/usr/bin/env bash
#
# run-sanitizer.sh — compile a corpus of Rue programs and run every resulting
# binary under Valgrind's memcheck, failing if any program triggers a memory
# error or a fatal signal.
#
# This is the local driver for the `sanitizer.yml` CI job (RUE-49). Run it from
# anywhere in the repo:
#
#   scripts/run-sanitizer.sh                 # examples/*.rue + curated corpus
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
# calls libc; it mmaps 64 KiB arenas and bump-allocates within them
# (crates/rue-runtime/src/heap.rs), with free() a no-op. To memcheck every Rue
# program therefore does "0 allocs, 0 frees" — the whole arena is one opaque,
# addressable, mmap'd region. Consequences:
#
#   CAUGHT: wild pointers / accesses outside any mapped region, stack overflow,
#           jumps to bad addresses, uninitialised values used in branches or
#           passed to syscalls, bad syscall buffers — i.e. CODEGEN and
#           generated-code correctness bugs, which the fuzz job (source-level,
#           never runs a binary) cannot see.
#
#   MISSED: intra-arena heap overflow / use-after-free such as RUE-34 (a String
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
// Many push_str calls -> repeated String realloc (the RUE-34 grow path).
fn main() -> i32 {
    let mut s = String::new();
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
fn main() -> i32 {
    let mut s = String::new();
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
fn main() -> i32 {
    let mut s = String::with_capacity(4);
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
    sum % 256
}
RUE

# --- Assemble the program list: examples/ + curated + any CLI args.
programs=()
for f in "$repo_root"/examples/*.rue; do
    [[ -e "$f" ]] && programs+=("$f")
done
for f in "$work"/san_*.rue; do
    programs+=("$f")
done
for f in "$@"; do
    programs+=("$f")
done

# --- Verdict from a valgrind log (NOT from the program's own exit code: Rue
#     programs legitimately exit with codes up to 255, e.g. examples/arrays.rue
#     exits 157). A fatal signal under valgrind still prints
#     "ERROR SUMMARY: 0 errors", so we must check for it separately.
verdict_ok() {
    local log="$1"
    if grep -q "Process terminating with default action of signal" "$log"; then
        return 1
    fi
    if grep -qE "ERROR SUMMARY: [1-9][0-9]* errors" "$log"; then
        return 1
    fi
    grep -q "ERROR SUMMARY: 0 errors" "$log"
}

failures=0
checked=0
for src in "${programs[@]}"; do
    name="$(basename "$src" .rue)"
    bin="$work/$name.bin"
    log="$work/$name.vglog"

    if ! "$rue" "$src" -o "$bin" > "$work/$name.compile" 2>&1; then
        echo "FAIL(compile) $name"
        sed 's/^/    /' "$work/$name.compile"
        failures=$((failures + 1))
        continue
    fi

    # Cap runtime: valgrind adds heavy slowdown and a codegen bug could loop.
    timeout 120 valgrind \
        --error-exitcode=1 \
        --leak-check=full \
        --show-leak-kinds=all \
        --track-origins=yes \
        --log-file="$log" \
        "$bin" > /dev/null 2>&1 || true

    checked=$((checked + 1))
    if verdict_ok "$log"; then
        echo "PASS $name"
    else
        echo "FAIL(memcheck) $name"
        sed 's/^/    /' "$log"
        failures=$((failures + 1))
    fi
done

echo
echo "run-sanitizer: checked $checked program(s), $failures failure(s)."
if [[ "$failures" -ne 0 ]]; then
    exit 1
fi
