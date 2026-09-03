"""C tool providers for the prelude's C++ toolchain, chosen per execution platform.

The prelude's `cxx_tools_info_toolchain` consumes a `CxxToolsInfo` through an
exec-dep, so a select on the provider target's attributes sees the resolved
execution platform's constraints rather than the output configuration's. The
two rules here exploit that:

- `zig_cxx_tools` builds a `CxxToolsInfo` whose compiler, archiver, and linker
  are wrapper scripts around the hermetic Zig distribution's `zig cc`,
  `zig c++`, and `zig ar`. Every Rust link on Linux runs through it, so the
  linker, its bundled lld, compiler_rt, libunwind, and the glibc it links
  against all come from the SHA-pinned Zig tree instead of whatever the host
  happens to install. That is what makes a Linux link identical between a
  developer machine, the remote cache, and the remote-execution worker.
- `exec_cxx_tools` forwards the `CxxToolsInfo` of a selectable dependency, so
  `toolchains//BUCK` can pick the Zig tools on Linux and keep the prelude's
  path-discovered clang tools on macOS from one exec-configured target.
"""

load("@prelude//cxx:cxx_toolchain_types.bzl", "LinkerType")
load("@prelude//toolchains:cxx.bzl", "CxxToolsInfo")
load("@prelude//utils:cmd_script.bzl", "cmd_script")
load("@toolchains//zig:defs.bzl", "ZigToolchainInfo")

def _zig_cxx_tools_impl(ctx: AnalysisContext) -> list[Provider]:
    # `ZigToolchainInfo.zig` is the cache-directory wrapper around the Zig
    # binary with the whole distribution as a hidden input. Reusing it keeps
    # one definition of where Zig may write (ZIG_LOCAL_CACHE_DIR and
    # ZIG_GLOBAL_CACHE_DIR under BUCK_SCRATCH_PATH) and makes the bundled
    # libc, compiler_rt, and libunwind sources part of every action key.
    zig = ctx.attrs._zig[ZigToolchainInfo].zig
    target = ["-target", ctx.attrs.target]

    def tool(name: str, args: list) -> cmd_args:
        # The prelude passes each tool to rustc as a single `-Clinker=` path
        # and to its own C rules as one executable, so bundle the multi-word
        # Zig command into a script. `cmd_script` carries the command's hidden
        # inputs on the returned cmd_args.
        return cmd_script(
            actions = ctx.actions,
            name = name,
            cmd = cmd_args(zig, args),
        )

    # The target triple is explicit on every compiler and linker invocation so
    # the glibc symbol-version floor is a reviewed property of this file rather
    # than of the execution host. `-fuse-ld=lld`, which the prelude appends to
    # Linux clang links, is accepted by `zig cc` as an unused argument: Zig
    # always links with its bundled lld.
    #
    # The linker is `zig cc`, not `zig c++`: `zig c++` compiles libc++ from
    # source into the Zig cache on first use, which under a per-action scratch
    # directory would repeat for every link. Rust links need no C++ runtime.
    #
    # The linker also carries a linker script that discards the debug sections
    # of the runtime objects Zig compiles from source during the link; the
    # script explains why those objects, and only those, would otherwise make
    # the link depend on the checkout path.
    cc = tool("zig-cc", ["cc"] + target)
    cxx = tool("zig-c++", ["c++"] + target)
    ar = tool("zig-ar", ["ar"])
    linker = tool("zig-link", ["cc"] + target + [
        cmd_args("-Wl,-T,", ctx.attrs._runtime_debug_discard, delimiter = ""),
    ])

    return [
        DefaultInfo(),
        CxxToolsInfo(
            compiler = cc,
            compiler_type = "clang",
            cxx_compiler = cxx,
            asm_compiler = cc,
            asm_compiler_type = "clang",
            rc_compiler = None,
            cvtres_compiler = None,
            archiver = ar,
            archiver_type = "gnu",
            linker = linker,
            linker_type = LinkerType("gnu"),
        ),
    ]

zig_cxx_tools = rule(
    impl = _zig_cxx_tools_impl,
    attrs = {
        # Zig target triple, including the glibc version to link against, e.g.
        # `x86_64-linux-gnu.2.17`. Selected on the execution platform's CPU.
        "target": attrs.string(),
        "_runtime_debug_discard": attrs.source(default = "toolchains//zig:runtime-debug-discard.ld"),
        "_zig": attrs.toolchain_dep(default = "toolchains//:zig"),
    },
)

def _exec_cxx_tools_impl(ctx: AnalysisContext) -> list[Provider]:
    return [
        DefaultInfo(),
        ctx.attrs.tools[CxxToolsInfo],
    ]

exec_cxx_tools = rule(
    impl = _exec_cxx_tools_impl,
    attrs = {
        "tools": attrs.dep(providers = [CxxToolsInfo]),
    },
)
