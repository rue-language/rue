load("@prelude//toolchains:cxx.bzl", "CxxToolsInfo")

def _cxx_tools_with_linker_impl(ctx):
    base = ctx.attrs.base[CxxToolsInfo]
    return [
        DefaultInfo(),
        CxxToolsInfo(
            compiler = base.compiler,
            compiler_type = base.compiler_type,
            cxx_compiler = base.cxx_compiler,
            asm_compiler = base.asm_compiler,
            asm_compiler_type = base.asm_compiler_type,
            rc_compiler = base.rc_compiler,
            cvtres_compiler = base.cvtres_compiler,
            archiver = base.archiver,
            archiver_type = base.archiver_type,
            linker = base.linker if ctx.attrs.linker == None else ctx.attrs.linker,
            linker_type = base.linker_type,
        ),
    ]

# cxx_tools_info_toolchain consumes this target as an exec-dep. Its selectable
# linker attribute therefore sees the resolved execution-platform constraints,
# unlike an attribute on the output-configured C++ toolchain itself.
cxx_tools_with_linker = rule(
    impl = _cxx_tools_with_linker_impl,
    attrs = {
        "base": attrs.dep(
            providers = [CxxToolsInfo],
            default = "prelude//toolchains/msvc:msvc_tools" if host_info().os.is_windows else "prelude//toolchains/cxx/clang:path_clang_tools",
        ),
        "linker": attrs.option(attrs.string(), default = None),
    },
)
