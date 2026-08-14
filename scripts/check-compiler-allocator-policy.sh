#!/usr/bin/env bash
set -euo pipefail

zig_root="${RUE_ALLOCATOR_ZIG_ROOT:?missing Zig policy inputs}"
crate_root="${RUE_ALLOCATOR_CRATE_ROOT:?missing compiler policy inputs}"
third_party_root="${RUE_ALLOCATOR_THIRD_PARTY_ROOT:?missing third-party policy inputs}"
note="${RUE_ALLOCATOR_POLICY_NOTE:?missing policy note}"

require() {
  local pattern="$1"
  local file="$2"
  grep -Fq -- "$pattern" "$file" || {
    echo "missing allocator policy '$pattern' in $file" >&2
    exit 1
  }
}

zig_defs="$zig_root/defs.bzl"
third_party_buck="$third_party_root/BUCK"
reindeer_rules="$third_party_root/reindeer_rules.bzl"
crate_buck="$crate_root/BUCK"

require 'ZIG_VERSION = "0.16.0"' "$zig_defs"
require 'hidden = [distribution]' "$zig_defs"
require 'BUCK_SCRATCH_PATH' "$zig_defs"
require 'ZIG_LOCAL_CACHE_DIR' "$zig_defs"
require 'ZIG_GLOBAL_CACHE_DIR' "$zig_defs"
require 'compile_args.add(ctx.attrs.compiler_flags)' "$zig_defs"
require 'archive_args.add("ar", "rcs"' "$zig_defs"
require 'ctx.label.path.add(include_dir)' "$zig_defs"

require 'name = "libmimalloc-sys-native-archive"' "$reindeer_rules"
require '"vendor/libmimalloc-sys-0.1.49/c_src/mimalloc/v2/include"' "$reindeer_rules"
require '"vendor/libmimalloc-sys-0.1.49/c_src/mimalloc/v2/src"' "$reindeer_rules"
for argument in \
  '-O3' \
  '-g0' \
  '-fPIC' \
  '-ftls-model=initial-exec' \
  '-DMI_STATIC_LIB' \
  '-DMI_DEBUG=0' \
  '-DMI_BUILD_RELEASE' \
  '-DNDEBUG' \
  '-D__DATE__=\"Jan 01 1970\"' \
  '-D__TIME__=\"00:00:00\"' \
  '-Wno-builtin-macro-redefined' \
  'x86_64-linux-gnu.2.17' \
  'aarch64-linux-gnu.2.17'; do
  require "$argument" "$reindeer_rules"
done
require 'name = "libmimalloc-sys-native"' "$reindeer_rules"
require 'static_lib = ":libmimalloc-sys-native-archive"' "$reindeer_rules"
require 'deps = [":libmimalloc-sys-native"]' "$third_party_buck"
if grep -Eq -- '-DMI_(SECURE|MALLOC_OVERRIDE)' "$reindeer_rules"; then
  echo "mimalloc secure/override mode must not be enabled" >&2
  exit 1
fi

require 'name = "mimalloc"' "$third_party_root/Cargo.lock"
require 'version = "0.1.52"' "$third_party_root/Cargo.lock"
require 'name = "libmimalloc-sys"' "$third_party_root/Cargo.lock"
require 'version = "0.1.49"' "$third_party_root/Cargo.lock"
require '#define MI_MALLOC_VERSION 20302' "$third_party_root/mimalloc.h"
require 'extra_deps = [":libmimalloc-sys-native"]' "$third_party_root/fixups.toml"
require 'name = "libtest2-harness-scheduler-test"' "$reindeer_rules"

require '_COMPILER_ALLOCATOR_DEPS = select({' "$crate_buck"
require '"prelude//os:linux": select({' "$crate_buck"
require '"prelude//cpu:arm64": ["//third-party:mimalloc"]' "$crate_buck"
require '"prelude//cpu:x86_64": ["//third-party:mimalloc"]' "$crate_buck"
require 'deps = _DEPS + _COMPILER_ALLOCATOR_DEPS + [' "$crate_buck"
benchmark_block="$(grep -A 12 -F 'name = "rue-benchmark"' "$crate_buck")"
if grep -Fq 'mimalloc' <<<"$benchmark_block"; then
  echo "rue-benchmark must not depend on mimalloc" >&2
  exit 1
fi
require '#[cfg(not(rue_benchmark_allocations))]' "$crate_root/main.rs"
require 'static ALLOCATOR: mimalloc::MiMalloc' "$crate_root/compiler_allocator.rs"
require 'static ALLOCATOR: std::alloc::System' "$crate_root/compiler_allocator.rs"
require 'System.alloc(layout)' "$crate_root/allocation.rs"

require 'does not claim the 500 ms / 256 MiB milestone' "$note"
require 'remove every inherited' "$note"
require 'mimalloc action also states permanent' "$note"
require 'contains neither debug information nor' "$note"
require 'source paths carried by' "$note"
