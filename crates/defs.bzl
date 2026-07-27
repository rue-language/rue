# Macros for defining Rue crates.
#
# Every crate under crates/ uses one of these macros instead of writing
# rust_library/rust_binary + rust_test pairs by hand. They keep the library
# and its unit-test target in sync: same glob() of sources, same shared deps.
#
# Usage (from a crates/<name>/BUCK file):
#
#     load("//crates:defs.bzl", "rue_crate")
#
#     rue_crate(
#         name = "rue-foo",
#         deps = [...],        # shared by the library and the test target
#         test_deps = [...],   # extra deps only the unit tests need
#     )
#
# Opting out of the generated `<name>-test` target requires `tests = False`
# and a comment at the call site explaining why.
#
# Lint policy note: `-Dwarnings` is applied at the toolchain level
# (toolchains/rust/BUCK sets `deny_lints = ["warnings"]`), so these macros
# do not pass any per-target lint flags.

load("//:test_defs.bzl", "rue_test_labels")

def rue_crate(
        name,
        deps = [],
        test_deps = [],
        test_tier = "premerge",
        tests = True,
        visibility = ["PUBLIC"],
        **kwargs):
    """Defines a first-party library crate: rust_library + <name>-test rust_test.

    Both targets compile glob(["src/**/*.rs"]). The test target gets
    `deps + test_deps`. Extra kwargs (e.g. mapped_srcs) are forwarded to
    both targets; if the two targets must diverge (different rustc_flags,
    different srcs), use `tests = False` and write the rust_test by hand.
    """
    srcs = glob(["src/**/*.rs"])
    native.rust_library(
        name = name,
        srcs = srcs,
        deps = deps,
        visibility = visibility,
        **kwargs
    )
    if tests:
        native.rust_test(
            name = name + "-test",
            srcs = srcs,
            deps = deps + test_deps,
            labels = rue_test_labels(test_tier),
            **kwargs
        )

def rue_binary(
        name,
        deps = [],
        test_deps = [],
        test_tier = "premerge",
        tests = True,
        visibility = ["PUBLIC"],
        **kwargs):
    """Defines a first-party binary crate: rust_binary + <name>-test rust_test.

    Same conventions as rue_crate. Binaries whose sources contain no #[test]
    functions should pass `tests = False` (an empty test target only wastes
    build time).
    """
    srcs = glob(["src/**/*.rs"])
    native.rust_binary(
        name = name,
        srcs = srcs,
        deps = deps,
        visibility = visibility,
        **kwargs
    )
    if tests:
        native.rust_test(
            name = name + "-test",
            srcs = srcs,
            deps = deps + test_deps,
            labels = rue_test_labels(test_tier),
            **kwargs
        )
