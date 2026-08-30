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

_RUSTFMT_CONFIG = "//:rustfmt-config"

def _canonical_package_target_name():
    """Returns the target name that owns package-wide structural gates."""
    return package_name().split("/")[-1]

def _sources_filegroup(name, srcs):
    """Emits one `<package>-sources` PUBLIC filegroup for compiled sources.

    Structural gates that live outside this package (the root-BUCK inventory
    validators, RUE-1525) take a crate's sources as a `$(location ...)` input.
    Emitting the filegroup from the macro every crate already calls replaces
    the per-gate hand-written `*-inventory-sources` filegroups — one identical
    `glob(["src/**/*.rs"])` copy per gate per crate — with a single sources
    target that exists because the crate declares itself, and that new gates
    can reuse without touching the crate's BUCK file.

    A package may declare several library or binary targets over the same
    sources. The target matching the package directory owns this package-wide
    filegroup, just as it owns the formatting and debug-assertion gates.
    """
    if name != _canonical_package_target_name():
        return
    native.filegroup(
        name = name + "-sources",
        srcs = srcs,
        visibility = ["PUBLIC"],
    )

def _fmt_check(name, srcs):
    """Emits one `<package>-fmt-check` over this package's compiled sources.

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
    serving a stale pass. A package with several macro targets still emits one
    byte-identical formatting action, owned by its directory-named target.
    """
    if name != _canonical_package_target_name():
        return
    native.sh_test(
        name = name + "-fmt-check",
        test = "toolchains//rust:rustfmt",
        args = [
            "--config-path",
            "$(location {})".format(_RUSTFMT_CONFIG),
            "--edition",
            "2024",
            "--check",
        ] + [
            package_name() + "/" + src
            for src in srcs
        ],
        resources = srcs + [_RUSTFMT_CONFIG],
        # Premerge tier; excluded from quick iteration so `scripts/rue quick`
        # keeps meaning "unit tests only" (see quick-test.sh).
        labels = rue_test_labels("premerge", labels = ["rue_not_quick"]),
    )

_DEBUG_ASSERT_GATE = "//:debug-assert-policy-script"
_GATELIB = "//:gatelib-sources"

def _debug_assert_check(name, srcs):
    """Emits `<name>-debug-assert-check`: the reviewed debug-assertion ledger,
    scoped to this crate's sources.

    Coverage is structural, like `_fmt_check`: a crate is checked because it
    declares the target, and only that crate's edit re-runs it. The gate
    script is addressed cross-package through a root `export_file` (mode =
    "reference", so it executes in place next to scripts/gatelib/); the
    gatelib filegroup rides along as a resource so a helper change re-runs
    the check rather than serving a stale pass. The sh_test runs from the
    project root, so `--sources` is the package-relative src/ directory and
    `srcs` are declared as resources for the same staleness reason.

    The check is keyed by the crate DIRECTORY name — that is the path the
    ledger's `crates/<name>/src/...` entries use — so a package defining
    several macro targets from the same sources (crates/rue: rue-driver, rue,
    rue-benchmark) emits exactly one check, from the target whose name
    matches the directory. Orphaned ledger entries for a crate that stopped
    emitting a check entirely are caught by //:debug-assert-ledger-check.
    """
    crate = _canonical_package_target_name()
    if name != crate:
        return
    native.sh_test(
        name = name + "-debug-assert-check",
        test = _DEBUG_ASSERT_GATE,
        args = [
            "--crate",
            crate,
            "--sources",
            package_name() + "/src",
        ],
        resources = srcs + [_GATELIB],
        # Premerge tier; excluded from quick iteration so `scripts/rue quick`
        # keeps meaning "unit tests only" (see quick-test.sh).
        labels = rue_test_labels("premerge", labels = ["rue_not_quick"]),
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
        labels = rue_test_labels("premerge", labels = ["rue_not_quick"]),
    )

def rue_crate(
        name,
        deps = [],
        test_deps = [],
        test_tier = "premerge",
        test_platform = None,
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
    _sources_filegroup(name, srcs)
    _fmt_check(name, srcs)
    _clippy_check(name)
    _debug_assert_check(name, srcs)
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
            labels = rue_test_labels(test_tier, test_platform),
            **kwargs
        )

def rue_binary(
        name,
        deps = [],
        test_deps = [],
        test_tier = "premerge",
        test_platform = None,
        tests = True,
        visibility = ["PUBLIC"],
        **kwargs):
    """Defines a first-party binary crate: rust_binary + <name>-test rust_test.

    Same conventions as rue_crate. Binaries whose sources contain no #[test]
    functions should pass `tests = False` (an empty test target only wastes
    build time).
    """
    srcs = glob(["src/**/*.rs"])
    _sources_filegroup(name, srcs)
    _fmt_check(name, srcs)
    _clippy_check(name)
    _debug_assert_check(name, srcs)
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
            labels = rue_test_labels(test_tier, test_platform),
            **kwargs
        )
