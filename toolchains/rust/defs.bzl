# Hermetic Rust toolchain for Buck2
#
# This module downloads prebuilt Rust toolchains and configures them
# for use with Buck2's Rust rules.

load("@prelude//rust:rust_toolchain.bzl", "PanicRuntime", "RustToolchainInfo")

# Rust 1.92.0 release info
RUST_VERSION = "1.92.0"

# Official component archives and SHA256 hashes from Rust's 1.92.0 channel
# manifest. Download only the compiler, Clippy, and rustfmt components: the
# monolithic `rust` archive also carries Cargo, rust-analyzer, LLVM tools, and
# two documentation trees that Rue never consumes.
RUST_RELEASES = {
    "x86_64-unknown-linux-gnu": struct(
        rustc_url = "https://static.rust-lang.org/dist/rustc-1.92.0-x86_64-unknown-linux-gnu.tar.xz",
        rustc_sha256 = "78b2dd9c6b1fcd2621fa81c611cf5e2d6950690775038b585c64f364422886e0",
        clippy_url = "https://static.rust-lang.org/dist/clippy-1.92.0-x86_64-unknown-linux-gnu.tar.xz",
        clippy_sha256 = "2c1bf6e7da8ec50feba03fe188fc9a744ba59e2c6ece7970c13e201d08defa9a",
        rustfmt_url = "https://static.rust-lang.org/dist/rustfmt-1.92.0-x86_64-unknown-linux-gnu.tar.xz",
        rustfmt_sha256 = "38951ee55f21e9170236fc98c8ba373ae4338d863087c6b0a5fa8c4e797d52c4",
    ),
    "aarch64-unknown-linux-gnu": struct(
        rustc_url = "https://static.rust-lang.org/dist/rustc-1.92.0-aarch64-unknown-linux-gnu.tar.xz",
        rustc_sha256 = "7c8706fad4c038b5eacab0092e15db54d2b365d5f3323ca046fe987f814e7826",
        clippy_url = "https://static.rust-lang.org/dist/clippy-1.92.0-aarch64-unknown-linux-gnu.tar.xz",
        clippy_sha256 = "333ab38c673b589468b8293b525e5704fb52515d9d516ee28d3d34dd5a63d3c3",
        rustfmt_url = "https://static.rust-lang.org/dist/rustfmt-1.92.0-aarch64-unknown-linux-gnu.tar.xz",
        rustfmt_sha256 = "1dce37aea2a7cb801f1756ffc531d7140428315a3d2c2f836272547eb7b9dacd",
    ),
    "aarch64-apple-darwin": struct(
        rustc_url = "https://static.rust-lang.org/dist/rustc-1.92.0-aarch64-apple-darwin.tar.xz",
        rustc_sha256 = "15dee753c9217dff4cf45d734b29dc13ce6017d8a55fe34eed75022b39a63ff0",
        clippy_url = "https://static.rust-lang.org/dist/clippy-1.92.0-aarch64-apple-darwin.tar.xz",
        clippy_sha256 = "08c65b6cf8faae3861706f8c97acf2aa6b784ed9455354c3b13495a7cfe5cb84",
        rustfmt_url = "https://static.rust-lang.org/dist/rustfmt-1.92.0-aarch64-apple-darwin.tar.xz",
        rustfmt_sha256 = "5d8ea865a7999dc9141603be8a9352745bf8440da051eb1c43f0fcaaf6845441",
    ),
    "x86_64-apple-darwin": struct(
        rustc_url = "https://static.rust-lang.org/dist/rustc-1.92.0-x86_64-apple-darwin.tar.xz",
        rustc_sha256 = "0facbd5d2742c8e97c53d59c9b5b81db6088cfc285d9ecb99523a50d6765fc5c",
        clippy_url = "https://static.rust-lang.org/dist/clippy-1.92.0-x86_64-apple-darwin.tar.xz",
        clippy_sha256 = "39cce87aab3d8b71350edcb3f943fba7bc59581ce1e65e158ee01e64cf0f1cf5",
        rustfmt_url = "https://static.rust-lang.org/dist/rustfmt-1.92.0-x86_64-apple-darwin.tar.xz",
        rustfmt_sha256 = "e038bda323ed7f4d417efc5be44c4245d74b8394f9f8393b9d464d662c3a9499",
    ),
}

# Standard-library-only components used to cross-compile the no_std Rue
# runtime. Keep these pins next to the compiler distribution pins so a Rust
# version update cannot silently mix toolchain releases.
RUST_STD_RELEASES = {
    "x86_64-unknown-linux-gnu": struct(
        url = "https://static.rust-lang.org/dist/rust-std-1.92.0-x86_64-unknown-linux-gnu.tar.xz",
        sha256 = "5f106805ed86ebf8df287039e53a45cf974391ef4d088c2760776b05b8e48b5d",
    ),
    "aarch64-unknown-linux-gnu": struct(
        url = "https://static.rust-lang.org/dist/rust-std-1.92.0-aarch64-unknown-linux-gnu.tar.xz",
        sha256 = "ce2ab42c09d633b0a8b4b65a297c700ae0fad47aae890f75894782f95be7e36d",
    ),
    "aarch64-apple-darwin": struct(
        url = "https://static.rust-lang.org/dist/rust-std-1.92.0-aarch64-apple-darwin.tar.xz",
        sha256 = "ea619984fcb8e24b05dbd568d599b8e10d904435ab458dfba6469e03e0fd69aa",
    ),
    "x86_64-apple-darwin": struct(
        url = "https://static.rust-lang.org/dist/rust-std-1.92.0-x86_64-apple-darwin.tar.xz",
        sha256 = "6ce143bf9e83c71e200f4180e8774ab22c8c8c2351c88484b13ff13be82c8d57",
    ),
}

def rust_host_archives(name: str, triple: str):
    """Declare the minimal official host components for one Rust platform."""
    release = RUST_RELEASES[triple]
    native.http_archive(
        name = "rustc-{}".format(name),
        urls = [release.rustc_url],
        sha256 = release.rustc_sha256,
        strip_prefix = "rustc-{}-{}".format(RUST_VERSION, triple),
        type = "tar.xz",
        visibility = [],
    )
    native.http_archive(
        name = "clippy-{}".format(name),
        urls = [release.clippy_url],
        sha256 = release.clippy_sha256,
        strip_prefix = "clippy-{}-{}".format(RUST_VERSION, triple),
        type = "tar.xz",
        visibility = [],
    )
    native.http_archive(
        name = "rustfmt-dist-{}".format(name),
        urls = [release.rustfmt_url],
        sha256 = release.rustfmt_sha256,
        strip_prefix = "rustfmt-{}-{}".format(RUST_VERSION, triple),
        type = "tar.xz",
        visibility = [],
    )

# Paths within the extracted Rust component archives. After http_archive strips
# each outer prefix, we have:
#   rustc/bin/rustc, rustc/bin/rustdoc
#   rustc/lib/rustlib/{triple}/bin/rust-lld (linker tools)
#   clippy-preview/bin/clippy-driver
#   rust-std-{triple}/lib/rustlib/{triple}/lib/*.rlib (stdlib - separate directory!)
#
# We need to create a merged sysroot because rustc expects:
#   {sysroot}/lib/rustlib/{triple}/lib/*.rlib
# But the stdlib is in rust-std-{triple}/, not in rustc/

def _hermetic_rust_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    """Implementation of hermetic_rust_toolchain rule."""

    rustc_dist = ctx.attrs.rustc_distribution[DefaultInfo].default_outputs[0]
    std_dist = ctx.attrs.standard_library_distribution[DefaultInfo].default_outputs[0]
    clippy_dist = ctx.attrs.clippy_distribution[DefaultInfo].default_outputs[0]
    triple = ctx.attrs.target_triple

    # Paths to binaries
    rustc_bin = rustc_dist.project("rustc/bin/rustc")
    rustdoc_bin = rustc_dist.project("rustc/bin/rustdoc")
    clippy_bin = clippy_dist.project("clippy-preview/bin/clippy-driver")

    # Create RunInfo for each tool. Include the component trees as hidden inputs
    # so rustc/rustdoc's native $ORIGIN/../lib RPATH resolves under REMOTE
    # execution too: RE materializes the rustc component tree on the worker, so
    # rustc/bin/rustc lands co-located with rustc/lib/ (where librustc_driver
    # lives). The separate standard-library component is also hidden because the
    # merged sysroot contains relative links into it (RUE-316/RUE-1225).
    # This is the relocatable, canonical approach ($ORIGIN RPATH), not an
    # absolute-path LD_LIBRARY_PATH hack.
    compiler = RunInfo(args = cmd_args(rustc_bin, hidden = [rustc_dist, std_dist]))
    rustdoc = RunInfo(args = cmd_args(rustdoc_bin, hidden = [rustc_dist, std_dist]))

    # clippy-driver dynamically loads librustc_driver from rustc/lib/, but
    # unlike rustc that dir isn't on the loader's search path, so a bare
    # invocation dies with "librustc_driver-*.so: cannot open shared object
    # file". Wrap it to set LD_LIBRARY_PATH / DYLD_LIBRARY_PATH to rustc/lib
    # before exec'ing the driver (same trick as the rustfmt wrapper below).
    #
    # Unlike rustfmt, clippy_driver must be a SINGLE executable, not a
    # ["/bin/bash", wrapper, ...] arg list: the prelude re-wraps it with
    # `cmd_args(clippy_driver, format = '{} "$@"')` (rust/context.bzl), and
    # `format` is applied per-argument — a multi-arg RunInfo would emit one
    # broken `<arg> "$@"` line per element. So we bake the paths into a
    # self-contained script and point RunInfo at just that file.
    clippy_lib_dir = rustc_dist.project("rustc/lib")
    clippy_wrapper, _ = ctx.actions.write(
        "clippy_driver_wrapper.sh",
        [
            "#!/usr/bin/env bash",
            # Set library path for both macOS and Linux
            cmd_args(clippy_lib_dir, format = "export DYLD_LIBRARY_PATH=\"{}\""),
            cmd_args(clippy_lib_dir, format = "export LD_LIBRARY_PATH=\"{}\""),
            cmd_args(clippy_bin, format = "exec {} \"$@\""),
        ],
        is_executable = True,
        allow_args = True,
    )
    clippy_driver = RunInfo(args = cmd_args(clippy_wrapper, hidden = [clippy_bin, clippy_lib_dir, std_dist]))

    # Build a merged sysroot by running the checked-in shell script. Keeping the
    # script as an explicit action input ensures content changes invalidate the
    # action cache along with the generated sysroot.
    # The Rust components are separate archives that need merging:
    #   rustc_dist/rustc/                                      - compiler and core libs
    #   std_dist/rust-std-{triple}/lib/rustlib/{triple}/lib/   - stdlib rlibs
    #
    # We use a shell action to create symlinks properly merging these.
    sysroot = ctx.actions.declare_output("sysroot", dir = True)
    merge_script = ctx.attrs.merge_script

    ctx.actions.run(
        cmd_args(
            "/bin/bash",
            merge_script,
            rustc_dist,
            std_dist,
            sysroot.as_output(),
            triple,
        ),
        category = "merge_sysroot",
    )

    return [
        DefaultInfo(),
        RustToolchainInfo(
            compiler = compiler,
            clippy_driver = clippy_driver,
            rustdoc = rustdoc,
            rustc_flags = ctx.attrs.rustc_flags,
            rustc_binary_flags = ctx.attrs.rustc_binary_flags,
            rustc_test_flags = ctx.attrs.rustc_test_flags,
            rustdoc_flags = ctx.attrs.rustdoc_flags,
            default_edition = ctx.attrs.default_edition,
            rustc_target_triple = triple,
            panic_runtime = PanicRuntime("abort"),
            allow_lints = ctx.attrs.allow_lints,
            deny_lints = ctx.attrs.deny_lints,
            warn_lints = ctx.attrs.warn_lints,
            clippy_toml = ctx.attrs.clippy_toml,
            nightly_features = ctx.attrs.nightly_features,
            doctests = ctx.attrs.doctests,
            report_unused_deps = ctx.attrs.report_unused_deps,
            # Use the merged sysroot
            sysroot_path = sysroot,
        ),
    ]

hermetic_rust_toolchain = rule(
    impl = _hermetic_rust_toolchain_impl,
    attrs = {
        # Toolchain payloads are consumed by build actions, so configure them
        # for the execution platform. A target-configured dep would materialize
        # the same archive again for every debug/release target configuration.
        "rustc_distribution": attrs.exec_dep(
            doc = "The downloaded rustc component (from http_archive)",
        ),
        "standard_library_distribution": attrs.exec_dep(
            doc = "The downloaded rust-std component (from http_archive)",
        ),
        "clippy_distribution": attrs.exec_dep(
            doc = "The downloaded Clippy component (from http_archive)",
        ),
        "merge_script": attrs.source(
            doc = "Portable script that assembles a relocatable Rust sysroot",
        ),
        "target_triple": attrs.string(
            doc = "The target triple (e.g., x86_64-unknown-linux-gnu)",
        ),
        "default_edition": attrs.option(attrs.string(), default = None),
        "rustc_flags": attrs.list(attrs.arg(), default = []),
        "rustc_binary_flags": attrs.list(attrs.arg(), default = []),
        "rustc_test_flags": attrs.list(attrs.arg(), default = []),
        "rustdoc_flags": attrs.list(attrs.arg(), default = []),
        "allow_lints": attrs.list(attrs.arg(), default = []),
        "deny_lints": attrs.list(attrs.arg(), default = []),
        "warn_lints": attrs.list(attrs.arg(), default = []),
        "clippy_toml": attrs.option(attrs.source(), default = None),
        "nightly_features": attrs.bool(default = False),
        "doctests": attrs.bool(default = False),
        "report_unused_deps": attrs.bool(default = False),
    },
    is_toolchain_rule = True,
)

# Rule to expose rustfmt from the hermetic toolchain
def _rustfmt_impl(ctx: AnalysisContext) -> list[Provider]:
    """Exposes rustfmt from the separate rustc and rustfmt components.

    rustfmt needs librustc_driver.dylib to be at ../lib/ relative to the binary.
    We create a wrapper script that sets up the environment correctly.
    """
    rustc_dist = ctx.attrs.rustc_distribution[DefaultInfo].default_outputs[0]
    rustfmt_dist = ctx.attrs.rustfmt_distribution[DefaultInfo].default_outputs[0]
    rustfmt_bin = rustfmt_dist.project("rustfmt-preview/bin/rustfmt")
    lib_dir = rustc_dist.project("rustc/lib")

    # Create a wrapper script that sets the library path
    wrapper = ctx.actions.write(
        "rustfmt_wrapper.sh",
        [
            "#!/bin/bash",
            # Set library path for both macOS and Linux
            "export DYLD_LIBRARY_PATH=\"$1\"",
            "export LD_LIBRARY_PATH=\"$1\"",
            "shift",
            "exec \"$@\"",
        ],
        is_executable = True,
    )

    return [
        DefaultInfo(default_output = rustfmt_bin),
        RunInfo(args = ["/bin/bash", wrapper, lib_dir, rustfmt_bin]),
    ]

rustfmt = rule(
    impl = _rustfmt_impl,
    attrs = {
        # rustfmt runs on the execution host; keep its component archives out
        # of each target configuration for the same reason as the toolchain.
        "rustc_distribution": attrs.exec_dep(
            doc = "The downloaded rustc component (from http_archive)",
        ),
        "rustfmt_distribution": attrs.exec_dep(
            doc = "The downloaded rustfmt component (from http_archive)",
        ),
    },
)

def host_rustfmt(name: str, visibility: list[str] = []):
    """Create a rustfmt alias that automatically selects the host platform."""
    os = host_info().os
    arch = host_info().arch

    if os.is_linux and arch.is_x86_64:
        actual = ":rustfmt-linux-x86_64"
    elif os.is_linux and arch.is_aarch64:
        actual = ":rustfmt-linux-aarch64"
    elif os.is_macos and arch.is_aarch64:
        actual = ":rustfmt-macos-aarch64"
    elif os.is_macos and arch.is_x86_64:
        actual = ":rustfmt-macos-x86_64"
    else:
        fail("Unsupported platform for rustfmt: {} {}".format(os, arch))

    native.alias(
        name = name,
        actual = actual,
        visibility = visibility,
    )
