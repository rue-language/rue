"""Hermetic per-target build rules for the Rue runtime."""

load("@prelude//rust:rust_toolchain.bzl", "RustToolchainInfo")

# Keep the flags shared with the native rust_library target below. The runtime
# is linked into freestanding executables, so none of these may depend on the
# host platform or its C toolchain.
RUNTIME_COMMON_RUSTC_FLAGS = [
    "-Cpanic=abort",
    "-Copt-level=z",
    "-Clto=true",
    "-Crelocation-model=static",
]

# AArch64's LSE atomics use a compiler-rt runtime-detection symbol that Rue
# does not provide. These flags must not leak into the x86-64 build.
RUNTIME_AARCH64_RUSTC_FLAGS = [
    "-Ctarget-feature=-lse,-lse2,-outline-atomics",
]

def _runtime_staticlib_impl(ctx: AnalysisContext) -> list[Provider]:
    target = ctx.attrs.target_triple
    toolchain = ctx.attrs._rust_toolchain[RustToolchainInfo]
    target_std = ctx.attrs.target_std[DefaultInfo].default_outputs[0]

    # A rust-std component contains an installable `rust-std-$target` tree
    # below the archive's outer directory. That tree is a complete sysroot for
    # compiling the no_std ABI and runtime crates for the selected target.
    target_sysroot = target_std.project("rust-std-{}".format(target))
    abi_rlib = ctx.actions.declare_output("librue_runtime_abi-{}.rlib".format(target))
    allocator_rlib = ctx.actions.declare_output("librue_allocator-{}.rlib".format(target))
    zmij_rlib = ctx.actions.declare_output("libzmij-{}.rlib".format(target))
    archive = ctx.actions.declare_output("librue_runtime-{}.a".format(target))
    zmij_source_dir = ctx.actions.symlinked_dir(
        "zmij-src-{}".format(target),
        {
            "lib.rs": ctx.attrs.zmij_crate_root,
            "traits.rs": ctx.attrs.zmij_srcs[0],
        },
    )
    zmij_crate_root = zmij_source_dir.project("lib.rs")

    abi_args = cmd_args(toolchain.compiler)
    abi_args.add(
        "--crate-name",
        "rue_runtime_abi",
        "--crate-type",
        "rlib",
        "--edition",
        "2024",
        "--target",
        target,
        "--sysroot",
        target_sysroot,
        ctx.attrs.abi_crate_root,
        "-o",
        abi_rlib.as_output(),
    )

    ctx.actions.run(
        abi_args,
        category = "runtime_abi_rlib",
        identifier = target,
    )

    allocator_args = cmd_args(toolchain.compiler)
    allocator_args.add(
        "--crate-name",
        "rue_allocator",
        "--crate-type",
        "rlib",
        "--edition",
        "2024",
        "--target",
        target,
        "--sysroot",
        target_sysroot,
    )
    allocator_args.add(ctx.attrs.rustc_flags)
    # Generic allocator code is instantiated into the runtime static library,
    # including source locations used by panic paths. The exported source
    # artifact lives under checkout-specific buck-out roots, so give rustc a
    # stable virtual filename before those bytes are embedded in the compiler.
    allocator_args.add(
        "--remap-path-prefix",
        cmd_args(
            ctx.attrs.allocator_crate_root,
            format = "{}=/rue/crates/rue-allocator/src/lib.rs",
        ),
    )
    allocator_args.add(
        ctx.attrs.allocator_crate_root,
        "-o",
        allocator_rlib.as_output(),
    )

    ctx.actions.run(
        allocator_args,
        category = "runtime_allocator_rlib",
        identifier = target,
    )

    zmij_args = cmd_args(toolchain.compiler)
    zmij_args.add(
        "--crate-name",
        "zmij",
        "--crate-type",
        "rlib",
        "--edition",
        "2021",
        "--target",
        target,
        "--sysroot",
        target_sysroot,
    )
    zmij_args.add(ctx.attrs.rustc_flags)
    zmij_args.add(
        "--remap-path-prefix",
        cmd_args(
            zmij_crate_root,
            format = "{}=/rue/third-party/vendor/zmij-0.1.7/src/lib.rs",
        ),
    )
    zmij_args.add(zmij_crate_root)
    zmij_args.add("-o", zmij_rlib.as_output())

    ctx.actions.run(
        zmij_args,
        category = "runtime_zmij_rlib",
        identifier = target,
    )

    args = cmd_args(toolchain.compiler)
    args.add(
        "--crate-name",
        "rue_runtime",
        "--crate-type",
        "staticlib",
        "--edition",
        "2024",
        "--target",
        target,
        "--sysroot",
        target_sysroot,
    )
    args.add(ctx.attrs.rustc_flags)
    args.add(
        "--extern",
        cmd_args("rue_allocator=", allocator_rlib, delimiter = ""),
    )
    args.add(
        "--extern",
        cmd_args("rue_runtime_abi=", abi_rlib, delimiter = ""),
    )
    args.add(
        "--extern",
        cmd_args("zmij=", zmij_rlib, delimiter = ""),
    )
    args.add(cmd_args(ctx.attrs.crate_root, hidden = ctx.attrs.srcs))
    args.add("-o", archive.as_output())

    ctx.actions.run(
        args,
        category = "runtime_staticlib",
        identifier = target,
    )

    return [DefaultInfo(default_output = archive)]

_runtime_staticlib = rule(
    impl = _runtime_staticlib_impl,
    attrs = {
        "allocator_crate_root": attrs.source(),
        "abi_crate_root": attrs.source(),
        "crate_root": attrs.source(),
        "srcs": attrs.list(attrs.source()),
        "target_std": attrs.dep(),
        "target_triple": attrs.string(),
        "rustc_flags": attrs.list(attrs.arg()),
        "zmij_crate_root": attrs.source(),
        "zmij_srcs": attrs.list(attrs.source()),
        "_rust_toolchain": attrs.toolchain_dep(default = "toolchains//:rust"),
    },
)

def runtime_staticlib(name: str, target_triple: str, target_std: str, visibility: list[str] = []):
    """Build the no_std Rue runtime for a fixed target with the host rustc."""
    flags = RUNTIME_COMMON_RUSTC_FLAGS
    if target_triple.startswith("aarch64-"):
        flags = flags + RUNTIME_AARCH64_RUSTC_FLAGS

    _runtime_staticlib(
        name = name,
        allocator_crate_root = "//crates/rue-allocator:lib.rs",
        abi_crate_root = "//crates/rue-runtime-abi:lib.rs",
        crate_root = "src/lib.rs",
        srcs = glob(["src/**/*.rs"]),
        target_std = target_std,
        target_triple = target_triple,
        rustc_flags = flags,
        zmij_crate_root = "//third-party:zmij-0.1.7-lib.rs",
        zmij_srcs = [
            "//third-party:zmij-0.1.7-traits.rs",
        ],
        visibility = visibility,
    )
