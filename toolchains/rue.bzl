"""The Rue compiler toolchain (ADR-0070 / RUE-1404).

`rue_program` consumes the compiler, the standard library, and the configured
platform's native Rue target as ONE resolved unit, mirroring how
crates/rue-runtime/runtime.bzl consumes `toolchains//:rust`. Routing these
through a toolchain rather than per-rule attributes is what lets the same rule
later run against a released compiler (Forward positioning, ADR-0070).

`compiler` and `std` are ordinary target-configuration deps, not exec deps, on
purpose: `//platforms:release` puts the opt-level constraint in the TARGET
configuration (RUE-277), and a release-configured `rue_program` must compile
with a release-built compiler exactly as `$(exe_target //crates/rue:rue)` does
for the corpus suites today.
"""

RueToolchainInfo = provider(fields = [
    # RunInfo of the Rue compiler.
    "compiler",
    # The //std:std filegroup's output directory artifact, which IS the std
    # root (a filegroup's output directory holds its srcs at package-relative
    # paths, and the std package is the std directory).
    "std",
    # The configured platform's native Rue target ("x86-64-linux",
    # "aarch64-linux", "aarch64-macos"). ADR-0070 open question 1: a
    # `rue_program` that sets no `rue_target` compiles for this; the resolved
    # target is always passed explicitly so the action key never depends on
    # host detection.
    "native_target",
    # Default optimization level when a program does not choose one.
    "default_opt_level",
])

def _rue_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    return [
        DefaultInfo(),
        RueToolchainInfo(
            compiler = ctx.attrs.compiler[RunInfo],
            std = ctx.attrs.std[DefaultInfo].default_outputs[0],
            native_target = ctx.attrs.native_target,
            default_opt_level = ctx.attrs.default_opt_level,
        ),
    ]

rue_toolchain = rule(
    impl = _rue_toolchain_impl,
    attrs = {
        "compiler": attrs.dep(providers = [RunInfo]),
        "std": attrs.dep(),
        "native_target": attrs.string(),
        "default_opt_level": attrs.string(default = "0"),
    },
    is_toolchain_rule = True,
)
