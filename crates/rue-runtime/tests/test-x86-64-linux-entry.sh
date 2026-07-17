#!/bin/sh
set -eu

if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != x86_64 ]; then
    exit 0
fi

for tool in awk cc ld objdump nm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "missing required x86-64 Linux entry-test tool: $tool" >&2
        exit 1
    fi
done

case_dir="crates/rue-runtime/tests"
work_dir="${TMPDIR:-/tmp}/rue-entry-test-$$"
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
mkdir -p "$work_dir"

# Pin the complete prologue-free shim encoding in the same optimized, LTO'd
# staticlib embedded by the compiler. The shim first copies the untouched entry
# %rsp (which points at argc) into %rdi so the Rust helper can capture
# argc/argv/envp (RUE-935), then aligns the stack and calls the helper. The call
# displacement is relocated, so its four-byte placeholder is zero in the archive
# member.
start_disassembly="$work_dir/start.disassembly"
objdump -dr --disassemble=_start "$RUE_RUNTIME" > "$start_disassembly"
grep -Eq '^[[:space:]]*0:[[:space:]]+48 89 e7[[:space:]]+mov' "$start_disassembly"
grep -Eq '^[[:space:]]*3:[[:space:]]+48 83 e4 f0[[:space:]]+and' "$start_disassembly"
grep -Eq '^[[:space:]]*7:[[:space:]]+e8 00 00 00 00[[:space:]]+call' "$start_disassembly"
grep -Eq '^[[:space:]]*c:[[:space:]]+0f 0b[[:space:]]+ud2' "$start_disassembly"
grep -q '__rue_x86_64_linux_start' "$start_disassembly"
nm -g --defined-only "$RUE_RUNTIME" | grep -Eq '[[:space:]]_start$'
nm -g --defined-only "$RUE_RUNTIME" | grep -Eq '[[:space:]]__rue_x86_64_linux_start$'

# Compile an unmodified Rue executable with the system linker so its symbols
# remain available. A small ptrace parent observes the child's registers at the
# kernel entry stop and at software breakpoints on the real helper and main.
"$RUE_BINARY" --linker ld "$case_dir/entry-main.rue" -o "$work_dir/entry-probe"
cc -O2 -Wall -Wextra -o "$work_dir/alignment-probe" "$case_dir/alignment-probe.c"

start_address="$(nm "$work_dir/entry-probe" | awk '$3 == "_start" { print $1; exit }')"
helper_address="$(nm "$work_dir/entry-probe" | awk '$3 == "__rue_x86_64_linux_start" { print $1; exit }')"
main_address="$(nm "$work_dir/entry-probe" | awk '$3 == "main" { print $1; exit }')"
if [ -z "$start_address" ] || [ -z "$helper_address" ] || [ -z "$main_address" ]; then
    echo "entry alignment probe could not locate start/helper/main symbols" >&2
    exit 1
fi
"$work_dir/alignment-probe" \
    "$work_dir/entry-probe" \
    "$start_address" \
    "$helper_address" \
    "$main_address"
