#!/usr/bin/env bash
# The linker script discards debug sections by input path, and the paths it
# names are the cache directories the Zig cache wrapper exports. The two files
# are edited independently; this keeps their names from drifting apart.
set -euo pipefail

script="$1"
defs="$2"

for cache in zig-global-cache zig-local-cache; do
    grep -Fq "\${cache_root}/${cache}" "$defs"
    grep -Fq "*${cache}/*(.debug_*)" "$script"
done

# Only debug sections may be discarded, and only under those directories: a
# broader pattern would strip Rust objects or drop code. Pattern lines start
# with `*` followed by a path; comment lines start with `*` and a space.
if grep -E '^[[:space:]]*\*[^[:space:]*/]' "$script" |
    grep -Ev '^[[:space:]]*\*zig-(global|local)-cache/\*\(\.debug_\*\)[[:space:]]*$'; then
    echo "unexpected discard pattern in $script" >&2
    exit 1
fi

# A SECTIONS command without INSERT replaces LLD's built-in layout; the probe
# for this file produced a binary that did not run.
grep -Eq '^INSERT AFTER \.text;$' "$script"
