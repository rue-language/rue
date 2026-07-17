# Hermetic Rust toolchain for Buck2
#
# This module downloads prebuilt Rust toolchains and configures them
# for use with Buck2's Rust rules.

load("@prelude//rust:rust_toolchain.bzl", "PanicRuntime", "RustToolchainInfo")

# Rust 1.92.0 release info
RUST_VERSION = "1.92.0"

# SHA256 hashes for each platform's Rust distribution
# These are the official hashes from static.rust-lang.org
RUST_RELEASES = {
    "x86_64-unknown-linux-gnu": struct(
        url = "https://static.rust-lang.org/dist/rust-1.92.0-x86_64-unknown-linux-gnu.tar.xz",
        sha256 = "d2ccef59dd9f7439f2c694948069f789a044dc1addcc0803613232af8f88ee0c",
        triple = "x86_64-unknown-linux-gnu",
    ),
    "aarch64-unknown-linux-gnu": struct(
        url = "https://static.rust-lang.org/dist/rust-1.92.0-aarch64-unknown-linux-gnu.tar.xz",
        sha256 = "3e383f8b4fca710d0600d0c1de97b78281672be2cda6575ecbe1c183a12e3822",
        triple = "aarch64-unknown-linux-gnu",
    ),
    "aarch64-apple-darwin": struct(
        url = "https://static.rust-lang.org/dist/rust-1.92.0-aarch64-apple-darwin.tar.xz",
        sha256 = "22276ecf826b22e718f099d7bf7ddb8c88aa46230fdba74962ab3c5031472268",
        triple = "aarch64-apple-darwin",
    ),
    "x86_64-apple-darwin": struct(
        url = "https://static.rust-lang.org/dist/rust-1.92.0-x86_64-apple-darwin.tar.xz",
        sha256 = "ef71fcdcd50efd3301144e701faf15124113a1b2efe9a111175d7d1e4f2d31d2",
        triple = "x86_64-apple-darwin",
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
}

# Paths within the extracted Rust distribution
# After http_archive extracts and strips the prefix, we have:
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

    dist = ctx.attrs.distribution[DefaultInfo].default_outputs[0]
    triple = ctx.attrs.target_triple

    # Paths to binaries
    rustc_bin = dist.project("rustc/bin/rustc")
    rustdoc_bin = dist.project("rustc/bin/rustdoc")
    clippy_bin = dist.project("clippy-preview/bin/clippy-driver")

    # Create RunInfo for each tool. Include the full distribution as a hidden
    # input so rustc/rustdoc's native $ORIGIN/../lib RPATH resolves under REMOTE
    # execution too: RE materializes the toolchain tree on the remote worker, so
    # rustc/bin/rustc must land co-located with rustc/lib/ (where librustc_driver
    # lives). Locally the http_archive tree is already co-located, so this is a
    # no-op there; on RE it's what makes rustc find its shared libs (RUE-316).
    # This is the relocatable, canonical approach ($ORIGIN RPATH), not an
    # absolute-path LD_LIBRARY_PATH hack.
    compiler = RunInfo(args = cmd_args(rustc_bin, hidden = [dist]))
    rustdoc = RunInfo(args = cmd_args(rustdoc_bin, hidden = [dist]))

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
    clippy_lib_dir = dist.project("rustc/lib")
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
    clippy_driver = RunInfo(args = cmd_args(clippy_wrapper, hidden = [clippy_bin, clippy_lib_dir]))

    # Build a merged sysroot by running a shell script
    # The Rust distribution has components in separate directories that need merging:
    #   rustc/                                      - compiler and core libs
    #   rust-std-{triple}/lib/rustlib/{triple}/lib/ - stdlib rlibs
    #
    # We use a shell action to create symlinks properly merging these.
    sysroot = ctx.actions.declare_output("sysroot", dir = True)
    merge_script = ctx.actions.write(
        "merge_sysroot.sh",
        [
            "#!/bin/bash",
            "set -e",
            "DIST=$(cd \"$1\" && pwd)",  # Convert to absolute path
            "SYSROOT=$2",
            "TRIPLE=$3",
            "",
            "# Create sysroot structure",
            "mkdir -p \"$SYSROOT\"",
            "",
            "# Link top-level dirs from rustc",
            "ln -s \"$DIST/rustc/bin\" \"$SYSROOT/bin\"",
            "ln -s \"$DIST/rustc/libexec\" \"$SYSROOT/libexec\"",
            "",
            "# Create lib structure manually to merge rustlib",
            "mkdir -p \"$SYSROOT/lib/rustlib/$TRIPLE\"",
            "",
            "# Link compiler libs (dylibs at lib/)",
            "for f in \"$DIST/rustc/lib/\"*; do",
            "    name=$(basename \"$f\")",
            "    if [ \"$name\" != \"rustlib\" ]; then",
            "        ln -s \"$f\" \"$SYSROOT/lib/$name\"",
            "    fi",
            "done",
            "",
            "# Link rustlib/etc",
            "ln -s \"$DIST/rustc/lib/rustlib/etc\" \"$SYSROOT/lib/rustlib/etc\"",
            "",
            "# Link target bin (rust-lld)",
            "ln -s \"$DIST/rustc/lib/rustlib/$TRIPLE/bin\" \"$SYSROOT/lib/rustlib/$TRIPLE/bin\"",
            "",
            "# Link target lib (stdlib from rust-std)",
            "ln -s \"$DIST/rust-std-$TRIPLE/lib/rustlib/$TRIPLE/lib\" \"$SYSROOT/lib/rustlib/$TRIPLE/lib\"",
        ],
        is_executable = True,
    )

    ctx.actions.run(
        cmd_args(
            "/bin/bash",
            merge_script,
            dist,
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
        "distribution": attrs.dep(
            doc = "The downloaded Rust distribution (from http_archive)",
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
        "clippy_toml": attrs.option(attrs.dep(), default = None),
        "nightly_features": attrs.bool(default = False),
        "doctests": attrs.bool(default = False),
        "report_unused_deps": attrs.bool(default = False),
    },
    is_toolchain_rule = True,
)

# Rule to expose rustfmt from the hermetic toolchain
def _rustfmt_impl(ctx: AnalysisContext) -> list[Provider]:
    """Exposes rustfmt binary from the Rust distribution.

    rustfmt needs librustc_driver.dylib to be at ../lib/ relative to the binary.
    We create a wrapper script that sets up the environment correctly.
    """
    dist = ctx.attrs.distribution[DefaultInfo].default_outputs[0]
    rustfmt_bin = dist.project("rustfmt-preview/bin/rustfmt")
    lib_dir = dist.project("rustc/lib")

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
        "distribution": attrs.dep(
            doc = "The downloaded Rust distribution (from http_archive)",
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
