"""Hermetic Zig toolchain and focused C archive rule.

Zig is distributed as one relocatable tree.  The RunInfo keeps that complete
tree as a hidden input so the compiler executable, bundled libc descriptions,
and archive implementation all participate in action keys and remote inputs.
"""

load("@toolchains//:distribution.bzl", "toolchain_distribution")

ZIG_VERSION = "0.16.0"

ZIG_RELEASES = {
    "x86_64-linux": struct(
        url = "https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz",
        sha256 = "70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00",
    ),
    "aarch64-linux": struct(
        url = "https://ziglang.org/download/0.16.0/zig-aarch64-linux-0.16.0.tar.xz",
        sha256 = "ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17",
    ),
    "x86_64-macos": struct(
        url = "https://ziglang.org/download/0.16.0/zig-x86_64-macos-0.16.0.tar.xz",
        sha256 = "0387557ed1877bc6a2e1802c8391953baddba76081876301c522f52977b52ba7",
    ),
    "aarch64-macos": struct(
        url = "https://ziglang.org/download/0.16.0/zig-aarch64-macos-0.16.0.tar.xz",
        sha256 = "b23d70deaa879b5c2d486ed3316f7eaa53e84acf6fc9cc747de152450d401489",
    ),
}

ZigToolchainInfo = provider(fields = [
    # RunInfo for the relocatable Zig distribution.
    "zig",
    # Execution-host identity, retained for structural assertions and consumers
    # which must choose an explicit cross-compilation target.
    "host_platform",
])

def zig_host_archive(name: str, platform: str):
    """Declare one official, SHA-pinned Zig host distribution.

    `toolchain_distribution` rather than `http_archive`: the unpacked tree is
    19,546 files, and serving one from the remote CAS is what RUE-2003 traced
    the merge queue's `materialize_inputs_failed` ejections to. See
    toolchains/distribution.bzl.
    """
    release = ZIG_RELEASES[platform]
    toolchain_distribution(
        name = "dist-{}".format(name),
        url = release.url,
        sha256 = release.sha256,
        strip_prefix = "zig-{}-{}".format(platform, ZIG_VERSION),
        visibility = [],
    )

def _hermetic_zig_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    distribution = ctx.attrs.distribution[DefaultInfo].default_outputs[0]
    zig_binary = distribution.project("zig")
    cache_wrapper = ctx.actions.write(
        "zig-cache-wrapper.sh",
        [
            "#!/bin/sh",
            "set -eu",
            "cache_root=\"${BUCK_SCRATCH_PATH:-${TMPDIR:-/tmp}/rue-zig-${PPID}}\"",
            "export ZIG_LOCAL_CACHE_DIR=\"${cache_root}/zig-local-cache\"",
            "export ZIG_GLOBAL_CACHE_DIR=\"${cache_root}/zig-global-cache\"",
            "zig=$1",
            "shift",
            "exec \"${zig}\" \"$@\"",
        ],
        is_executable = True,
    )
    zig = RunInfo(args = cmd_args("/bin/sh", cache_wrapper, zig_binary, hidden = [distribution]))
    return [
        DefaultInfo(default_output = zig_binary),
        RunInfo(args = zig),
        ZigToolchainInfo(
            zig = zig,
            host_platform = ctx.attrs.host_platform,
        ),
    ]

hermetic_zig_toolchain = rule(
    impl = _hermetic_zig_toolchain_impl,
    attrs = {
        "distribution": attrs.exec_dep(),
        "host_platform": attrs.string(),
    },
    is_toolchain_rule = True,
)

def _zig_c_static_archive_impl(ctx: AnalysisContext) -> list[Provider]:
    toolchain = ctx.attrs._zig_toolchain[ZigToolchainInfo]
    object_file = ctx.actions.declare_output("{}.o".format(ctx.label.name))
    archive = ctx.actions.declare_output("lib{}.a".format(ctx.label.name))

    compile_args = cmd_args(toolchain.zig)
    compile_args.add(
        "cc",
        "-c",
        ctx.attrs.src,
        "-o",
        object_file.as_output(),
        "-target",
        ctx.attrs.target,
        "-mcpu={}".format(ctx.attrs.cpu),
        # Native archives are release inputs even when their Rust consumer is
        # a debug build. Keep checkout paths out by default; a caller may opt
        # into debug metadata with a later explicit `-g` compiler flag.
        "-g0",
    )
    for include_dir in ctx.attrs.include_directories:
        # A Buck action runs from the project root, while include directories
        # follow the declaring target's package. Label.path is a cell-aware
        # path, so this also preserves the `toolchains` cell's root prefix.
        compile_args.add("-I", ctx.label.path.add(include_dir))
    compile_args.add(ctx.attrs.compiler_flags)
    compile_args.add(cmd_args(hidden = ctx.attrs.headers))
    ctx.actions.run(
        compile_args,
        category = "zig_c_compile",
        identifier = ctx.label.name,
    )

    archive_args = cmd_args(toolchain.zig)
    archive_args.add("ar", "rcs", archive.as_output(), object_file)
    ctx.actions.run(
        archive_args,
        category = "zig_archive",
        identifier = ctx.label.name,
    )

    return [DefaultInfo(default_output = archive)]

zig_c_static_archive = rule(
    impl = _zig_c_static_archive_impl,
    attrs = {
        "src": attrs.source(),
        "headers": attrs.list(attrs.source(), default = []),
        "include_directories": attrs.list(attrs.string(), default = []),
        "compiler_flags": attrs.list(attrs.arg(), default = []),
        "target": attrs.string(),
        "cpu": attrs.string(default = "baseline"),
        "_zig_toolchain": attrs.toolchain_dep(default = "toolchains//:zig"),
    },
)
