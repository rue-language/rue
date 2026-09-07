"""Hermetic Zig toolchain, primed runtime cache, and focused C archive rule.

Zig is distributed as one relocatable tree.  The RunInfo keeps that complete
tree as a hidden input so the compiler executable, bundled libc descriptions,
and archive implementation all participate in action keys and remote inputs.
"""

load("@toolchains//:distribution.bzl", "toolchain_distribution")

ZIG_VERSION = "0.16.0"

# The Linux triple every `zig cc` invocation names, chosen on the execution
# platform's CPU. The glibc floor is `2.17`, the oldest glibc Rust's
# `*-unknown-linux-gnu` targets support; `third-party/reindeer_rules.bzl` pins
# the same floor for the mimalloc archive these links consume. The C tools
# (toolchains//:zig-cxx-tools) and the primed runtime cache
# (toolchains//zig:runtime-cache) must name the same triple or the cache would
# hold objects no link asks for, so both read it from here.
ZIG_LINUX_TARGET = select({
    "prelude//cpu:arm64": "aarch64-linux-gnu.2.17",
    "prelude//cpu:x86_64": "x86_64-linux-gnu.2.17",
})

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
    # RunInfo for the relocatable Zig distribution, run with an empty cache.
    "zig",
    # The three pieces `zig` is assembled from, so a consumer can build the
    # same command with a primed global cache. See zig_with_primed_cache.
    "cache_wrapper",
    "binary",
    "distribution",
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

# The cache wrapper's first argument when an action wants an empty global
# cache; anything else is an archive to prime the cache from.
_EMPTY_CACHE = "-"

def _zig_command(cache_wrapper: Artifact, binary: Artifact, distribution: Artifact, cache) -> cmd_args:
    """The one shape of a Zig command line: wrapper, cache seed, then Zig.

    `cache` is a zig_runtime_cache archive to prime the action's global cache
    from, or `_EMPTY_CACHE`. The whole distribution is hidden behind the
    executable so it reaches the action as an input.
    """
    return cmd_args("/bin/sh", cache_wrapper, cache, binary, hidden = [distribution])

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
            # $1 is a primed global cache (a zig_runtime_cache archive) or
            # _EMPTY_CACHE; $2 is the Zig executable.
            "seed=$1",
            "zig=$2",
            "shift 2",
            # The primed cache is unpacked into the action's scratch rather
            # than read where it lies: Zig needs a writable cache and an action
            # must not write to its inputs. It unpacks under the
            # `zig-global-cache` name, so the objects in it still match the
            # input patterns in toolchains/zig/runtime-debug-discard.ld.
            "if [ \"${seed}\" != \"" + _EMPTY_CACHE + "\" ] && [ ! -d \"${ZIG_GLOBAL_CACHE_DIR}\" ]; then",
            "    mkdir -p \"${ZIG_GLOBAL_CACHE_DIR}\"",
            "    tar -xf \"${seed}\" -C \"${ZIG_GLOBAL_CACHE_DIR}\"",
            "fi",
            "exec \"${zig}\" \"$@\"",
        ],
        is_executable = True,
    )
    zig = RunInfo(args = _zig_command(cache_wrapper, zig_binary, distribution, _EMPTY_CACHE))
    return [
        DefaultInfo(default_output = zig_binary),
        RunInfo(args = zig),
        ZigToolchainInfo(
            zig = zig,
            cache_wrapper = cache_wrapper,
            binary = zig_binary,
            distribution = distribution,
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

def zig_with_primed_cache(toolchain: ZigToolchainInfo, cache: Artifact) -> cmd_args:
    """The Zig command with the action's global cache primed from `cache`.

    `cache` is a zig_runtime_cache archive. Same wrapper, same
    scratch-directory caches, and same hidden distribution as
    `ZigToolchainInfo.zig`; the wrapper unpacks the primed cache before handing
    over to Zig.
    """
    return _zig_command(
        toolchain.cache_wrapper,
        toolchain.binary,
        toolchain.distribution,
        cache,
    )

# The link shapes a Linux Rust build produces, and with them every piece of Zig
# runtime such a build can ask for. Zig compiles that runtime from source the
# first time a global cache needs it, which costs a link about 15 s of wall
# time against 0.13 s when it starts from a primed cache. Priming performs each
# shape once, in an action that is itself cached, and every link action then
# starts from the result. Each shape earns its place — dropping one leaves
# entries no other shape builds — and there is no fourth to add: a shared
# object, as a proc-macro crate or a cdylib links, needs nothing these three
# already provide.
_RUNTIME_CACHE_LINK_SHAPES = [
    # A plain executable: the glibc start files, the non-shared stubs, and
    # compiler_rt.
    [],
    # A position-independent executable, which starts at Scrt1.o rather than
    # crt1.o and brings its own init and ABI-note objects.
    ["-pie", "-fPIE"],
    # Every Rust link passes `-lgcc_s`, which `zig cc` resolves to the bundled
    # libunwind — compiled from source like the rest, and keyed differently
    # from anything the other two shapes build.
    ["-lgcc_s"],
]

def _zig_runtime_cache_impl(ctx: AnalysisContext) -> list[Provider]:
    toolchain = ctx.attrs._zig_toolchain[ZigToolchainInfo]

    # One archive rather than the cache tree itself. The tree is 926 files, and
    # it would be an input to every link action in the build; a tree of small
    # files is the one artifact shape Rue has had to keep out of the remote CAS
    # (see toolchains/distribution.bzl and RUE-2003). Unpacking costs the same
    # as copying the tree would.
    cache = ctx.actions.declare_output("zig-runtime-cache.tar")

    prime = [
        "#!/bin/sh",
        "set -eu",
        "archive=$1",
        "zig=$2",
        "target=$3",
        "source=$4",
        "scratch=\"${BUCK_SCRATCH_PATH:-${TMPDIR:-/tmp}}\"",
        # Building this global cache is the action's whole purpose, but it is
        # still scratch: only its archived form is the output. The local cache
        # holds nothing worth keeping.
        "export ZIG_GLOBAL_CACHE_DIR=\"${scratch}/zig-global-cache\"",
        "export ZIG_LOCAL_CACHE_DIR=\"${scratch}/zig-local-cache\"",
        "mkdir -p \"${ZIG_GLOBAL_CACHE_DIR}\" \"${scratch}/links\"",
    ]
    for index, flags in enumerate(_RUNTIME_CACHE_LINK_SHAPES):
        link = (
            "\"${zig}\" cc -target \"${target}\" \"${source}\"" +
            " -o \"${{scratch}}/links/{}\"".format(index)
        )
        prime.append(" ".join([link] + flags))

    # Sorted names and zeroed metadata so the archive depends on the cache
    # contents alone.
    prime.append(
        "tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner" +
        " -cf \"${archive}\" -C \"${ZIG_GLOBAL_CACHE_DIR}\" .",
    )
    prime_script = ctx.actions.write(
        "prime-zig-runtime-cache.sh",
        prime,
        is_executable = True,
    )

    ctx.actions.run(
        cmd_args(
            "/bin/sh",
            prime_script,
            cache.as_output(),
            toolchain.binary,
            ctx.attrs.target,
            ctx.attrs.src,
            hidden = [toolchain.distribution],
        ),
        category = "zig_runtime_cache",
        identifier = ctx.label.name,
    )

    return [DefaultInfo(default_output = cache)]

zig_runtime_cache = rule(
    impl = _zig_runtime_cache_impl,
    attrs = {
        "src": attrs.source(
            doc = "Throwaway translation unit to link; only its Zig cache is kept.",
        ),
        "target": attrs.string(
            doc = "Zig target triple to build the runtime for, glibc pin included.",
        ),
        "_zig_toolchain": attrs.toolchain_dep(default = "toolchains//:zig"),
    },
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
