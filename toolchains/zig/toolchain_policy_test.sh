#!/usr/bin/env bash
set -euo pipefail

defs="$1"
version="$2"

test "$version" = "0.16.0"
grep -Fq 'x86_64-linux' "$defs"
grep -Fq 'aarch64-linux' "$defs"
grep -Fq 'sha256 = "70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00"' "$defs"
grep -Fq 'sha256 = "ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17"' "$defs"
grep -Fq 'hidden = [distribution]' "$defs"
grep -Fq 'BUCK_SCRATCH_PATH' "$defs"
grep -Fq 'ZIG_LOCAL_CACHE_DIR' "$defs"
grep -Fq 'ZIG_GLOBAL_CACHE_DIR' "$defs"
grep -Fq '"-target"' "$defs"
grep -Fq '"-mcpu={}"' "$defs"
grep -Fq '"-g0"' "$defs"
grep -Fq 'ctx.label.path.add(include_dir)' "$defs"
default_debug_line="$(grep -nF '"-g0"' "$defs" | cut -d: -f1)"
caller_flags_line="$(grep -nF 'compile_args.add(ctx.attrs.compiler_flags)' "$defs" | cut -d: -f1)"
test "$default_debug_line" -lt "$caller_flags_line"
