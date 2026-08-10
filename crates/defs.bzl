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

def _fmt_check(name, srcs):
    """Emits `<name>-fmt-check`: rustfmt --check over this crate's own sources.

    Coverage is structural (RUE-1153). The gate this replaces took its file
    list from filesystem discovery — and before that from a root-package
    `glob(["crates/**/*.rs"])`, which cannot descend into subpackages, so it
    checked 1 file of 281 while passing for the rest (RUE-1152). Emitting the
    target from the macro every crate already calls means a crate is checked
    because it declares the target, not because discovery reached it.

    `srcs` is the same package-relative glob the library target compiles.
    sh_test runs from the project root, so each path is prefixed with the
    package to address the file from there. Declaring `srcs` as resources is
    what makes Buck re-run the check when a source changes rather than
    serving a stale pass.
    """
    native.sh_test(
        name = name + "-fmt-check",
        test = "toolchains//rust:rustfmt",
        args = ["--edition", "2024", "--check"] + [
            package_name() + "/" + src
            for src in srcs
        ],
        resources = srcs,
        # Premerge tier; excluded from quick iteration so `scripts/rue quick`
        # keeps meaning "unit tests only" (see quick-test.sh).
        labels = rue_test_labels("premerge", ["rue_not_quick"]),
    )

def _clippy_check(name):
    """Emits `<name>-clippy`: fails when this crate's clippy diagnostics contain an error.

    Buck's Rust rules capture clippy output into an always-succeeding
    `[clippy.txt]` subtarget, so a build can never fail on a lint and
    something must read the file and decide. Expressing that as a test rather
    than a top-level script means Buck must materialize the diagnostics
    before the test can run — the property clippy.sh had to request
    explicitly with --materializations=all, because a remote cache hit could
    satisfy its build without ever writing the file, leaving the script to
    score unread targets as clean (RUE-1152).
    """
    native.sh_test(
        name = name + "-clippy",
        test = "//crates:clippy-gate",
        args = ["$(location :" + name + "[clippy.txt])"],
        # Premerge tier; excluded from quick iteration so `scripts/rue quick`
        # keeps meaning "unit tests only" (see quick-test.sh).
        labels = rue_test_labels("premerge", ["rue_not_quick"]),
    )

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
    _fmt_check(name, srcs)
    _clippy_check(name)
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
    _fmt_check(name, srcs)
    _clippy_check(name)
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
