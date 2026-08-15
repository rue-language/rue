"""Rue extensions to Reindeer's generated Rust-library rule.

Reindeer owns third-party/BUCK completely. Keeping the two hand-maintained
targets in this macro makes `reindeer buckify` idempotent without weakening the
native mimalloc link edge or the patched scheduler's regression coverage.
"""

load("@prelude//rust:cargo_package.bzl", "cargo")
load("@toolchains//zig:defs.bzl", "zig_c_static_archive")

def _mimalloc_native_targets():
    zig_c_static_archive(
        name = "libmimalloc-sys-native-archive",
        src = "vendor/libmimalloc-sys-0.1.49/c_src/mimalloc/v2/src/static.c",
        headers = native.glob([
            "vendor/libmimalloc-sys-0.1.49/c_src/mimalloc/v2/include/**/*.h",
            "vendor/libmimalloc-sys-0.1.49/c_src/mimalloc/v2/src/**/*.c",
            "vendor/libmimalloc-sys-0.1.49/c_src/mimalloc/v2/src/**/*.h",
        ]),
        include_directories = [
            "vendor/libmimalloc-sys-0.1.49/c_src/mimalloc/v2/include",
            "vendor/libmimalloc-sys-0.1.49/c_src/mimalloc/v2/src",
        ],
        compiler_flags = [
            "-O3",
            "-g0",
            "-fPIC",
            "-ftls-model=initial-exec",
            "-DMI_STATIC_LIB",
            "-DMI_DEBUG=0",
            "-DMI_BUILD_RELEASE",
            "-DNDEBUG",
            "-D__DATE__=\"Jan 01 1970\"",
            "-D__TIME__=\"00:00:00\"",
            "-Wno-builtin-macro-redefined",
        ],
        target = select({
            "DEFAULT": "x86_64-linux-gnu.2.17",
            "prelude//os:linux": select({
                "DEFAULT": "x86_64-linux-gnu.2.17",
                "prelude//cpu:arm64": "aarch64-linux-gnu.2.17",
                "prelude//cpu:x86_64": "x86_64-linux-gnu.2.17",
            }),
        }),
        compatible_with = ["prelude//os:linux"],
        visibility = [],
    )

    native.prebuilt_cxx_library(
        name = "libmimalloc-sys-native",
        static_lib = ":libmimalloc-sys-native-archive",
        preferred_linkage = "static",
        compatible_with = ["prelude//os:linux"],
        visibility = [],
    )

    native.filegroup(
        name = "allocator-policy-inputs",
        srcs = {
            "BUCK": "BUCK",
            "Cargo.lock": "Cargo.lock",
            "Cargo.toml": "Cargo.toml",
            "fixups.toml": "fixups/libmimalloc-sys/fixups.toml",
            "mimalloc.h": "vendor/libmimalloc-sys-0.1.49/c_src/mimalloc/v2/include/mimalloc.h",
            "reindeer.toml": "reindeer.toml",
            "reindeer_rules.bzl": "reindeer_rules.bzl",
        },
        visibility = ["PUBLIC"],
    )

def _libtest2_scheduler_test():
    # Rue carries a scheduler fix for exclusive CLI-test lanes. Keep a native
    # unit target beside the vendored library so fail-fast worker draining
    # remains executable regression coverage rather than an untested patch.
    native.rust_test(
        name = "libtest2-harness-scheduler-test",
        srcs = [
            "vendor/libtest2-harness-0.0.3/src/case.rs",
            "vendor/libtest2-harness-0.0.3/src/cli.rs",
            "vendor/libtest2-harness-0.0.3/src/context.rs",
            "vendor/libtest2-harness-0.0.3/src/harness.rs",
            "vendor/libtest2-harness-0.0.3/src/lib.rs",
            "vendor/libtest2-harness-0.0.3/src/notify/json.rs",
            "vendor/libtest2-harness-0.0.3/src/notify/mod.rs",
            "vendor/libtest2-harness-0.0.3/src/notify/no_style.rs",
            "vendor/libtest2-harness-0.0.3/src/notify/pretty.rs",
            "vendor/libtest2-harness-0.0.3/src/notify/style.rs",
            "vendor/libtest2-harness-0.0.3/src/notify/summary.rs",
            "vendor/libtest2-harness-0.0.3/src/notify/terse.rs",
        ],
        crate = "libtest2_harness",
        crate_root = "vendor/libtest2-harness-0.0.3/src/lib.rs",
        edition = "2021",
        features = [
            "color",
            "default",
            "threads",
        ],
        deps = [
            ":anstream-0.6.21",
            ":anstyle-1.0.13",
            ":lexarg-error-0.0.2",
            ":lexarg-parser-0.0.2",
            ":libtest-json-0.0.2",
            ":libtest-lexarg-0.0.3",
        ],
    )

def _is_linux_mimalloc_target(name):
    return name == "libmimalloc-sys" or name == "libmimalloc-sys-0.1.49" or name == "mimalloc" or name == "mimalloc-0.1.52"

def rue_alias(name, **kwargs):
    # Reindeer emits public aliases in addition to versioned Rust libraries.
    # Both layers must declare the native archive's platform: a broad `//...`
    # pattern skips an incompatible target, but an otherwise-compatible alias
    # is still configured and then fails when its `actual` is Linux-only.
    if _is_linux_mimalloc_target(name):
        kwargs["compatible_with"] = ["prelude//os:linux"]
    native.alias(name = name, **kwargs)

def rue_rust_library(name, **kwargs):
    # The versioned Rust wrappers are as Linux-specific as their native archive.
    if _is_linux_mimalloc_target(name):
        kwargs["compatible_with"] = ["prelude//os:linux"]
    cargo.rust_library(name = name, **kwargs)
    if name == "libmimalloc-sys-0.1.49":
        _mimalloc_native_targets()
    elif name == "libtest2-harness-0.0.3":
        _libtest2_scheduler_test()
