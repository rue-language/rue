# Repo-root test-suite targets (RUE-144 / RUE-132).
#
# Each sh_test ties a test-harness binary to the rue compiler and the on-disk
# inputs the suite actually reads (cases/, std/, docs/), so Buck owns the binary
# handoff and caches each suite against its real inputs:
#
#   buck2 test //...        # runs unit tests + spec/UI/CLI suites + repo gates
#
# An edit under crates/rue-spec/cases/ re-runs only the spec suite; an edit
# under std/ re-runs the CLI suite (std/ MUST be a declared input here or the
# CLI suite would get false cache hits on std library changes).
#
# Mechanics: the harness binaries already locate everything via env vars
# (rue-test-runner's find_rue_binary / find_dir), `$(exe_target ...)` /
# `$(location ...)` expand to absolute paths, and sh_test runs from the
# project root — so the harness binary itself can be the `test` command and
# no wrapper script is needed. A filegroup's output directory is named after
# the rule and contains its srcs at package-relative paths, hence the
# `$(location ...)/cases` shape.
#
# These suites live at the repo root rather than in the harness crates' BUCK
# files so that `buck2 test //crates/...` (quick-test.sh, test.sh's filtered
# path) still means "unit tests only".

# The std library sources are runtime inputs to the CLI integration tests
# (compiled programs `@import` them via ${REAL_STD} / RUE_STD_DIR).
filegroup(
    name = "std",
    srcs = glob(["std/**"]),
)

# The example programs are runtime inputs to the CLI integration tests: the
# suite compiles+runs every examples/*.rue through the real driver (RUE-48),
# so an edit under examples/ MUST re-run the CLI suite (declared here as an
# input, resolved to an absolute path via RUE_EXAMPLES_DIR below).
filegroup(
    name = "examples",
    srcs = glob(["examples/**"]),
)

# Checked-in repo-relative source_path fixtures for the CLI integration tests.
filegroup(
    name = "cli-test-fixtures",
    srcs = glob(["cli-test-fixtures/**"]),
)

# Tutorial markdown is an input to the snippet checker. The checker only
# compiles fences explicitly marked with `rue check` or `rue compile-fail`.
filegroup(
    name = "tutorial",
    srcs = glob(["website/content/tutorial/**"]),
)

filegroup(
    name = "tutorial-snippet-tool-inputs",
    srcs = [
        "scripts/check-tutorial-snippets.py",
        "test.sh",
    ],
)

filegroup(
    name = "spec-docs",
    srcs = glob(["docs/spec/src/**"]),
)

filegroup(
    name = "adr-designs",
    srcs = glob(["docs/designs/**"]),
)

filegroup(
    name = "benchmark-tool-inputs",
    srcs = [
        "benchmarks/manifest.toml",
        "scripts/append-benchmark.py",
        "scripts/benchmark_validation.py",
        "scripts/generate-charts.py",
        "scripts/validate-benchmark.py",
    ],
)

sh_test(
    name = "spec-tests",
    test = "//crates/rue-spec:rue-spec",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_SPEC_CASES": "$(location //crates/rue-spec:cases)/cases",
    },
)

sh_test(
    name = "spec-traceability",
    test = "//crates/rue-spec:rue-spec",
    args = ["--traceability"],
    env = {
        "RUE_SPEC_CASES": "$(location //crates/rue-spec:cases)/cases",
        "RUE_SPEC_DIR": "$(location :spec-docs)/docs/spec/src",
    },
)

sh_test(
    name = "ui-tests",
    test = "//crates/rue-ui-tests:rue-ui-tests",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_UI_CASES": "$(location //crates/rue-ui-tests:cases)/cases",
    },
)

sh_test(
    name = "cli-tests",
    test = "//crates/rue-cli-tests:rue-cli-tests",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_CLI_CASES": "$(location //crates/rue-cli-tests:cases)/cases",
        "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
        "RUE_REPO_DIR": "$(location :cli-test-fixtures)",
        "RUE_STD_DIR": "$(location :std)/std",
    },
)

sh_test(
    name = "tutorial-snippet-tests",
    test = "scripts/check-tutorial-snippets.py",
    args = [
        "--quiet",
        "$(location :tutorial)/website/content/tutorial",
    ],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_STD_PATH": "$(location :std)/std",
    },
)

sh_test(
    name = "tutorial-snippet-tool-tests",
    test = "scripts/test-tutorial-snippets.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_TUTORIAL_TEST_ROOT": "$(location :tutorial-snippet-tool-inputs)",
    },
)

sh_test(
    name = "adr-registry-validation",
    test = "scripts/validate-adrs.py",
    args = [
        "--adr-dir",
        "$(location :adr-designs)/docs/designs",
    ],
)

sh_test(
    name = "benchmark-tool-tests",
    test = "scripts/test-benchmark-tools.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_BENCHMARK_TEST_ROOT": "$(location :benchmark-tool-inputs)",
    },
)

# The destructive maintenance scripts under test (jj-tidy, worktree-gc). Their
# fail-closed contract is pinned by scripts/test-cleanup-scripts.sh (RUE-567),
# which runs copies of them against fake gh/git/df on PATH — no real repo,
# remote, or disk touched.
filegroup(
    name = "cleanup-script-inputs",
    srcs = [
        "scripts/jj-tidy",
        "scripts/worktree-gc",
    ],
)

sh_test(
    name = "cleanup-script-tests",
    test = "scripts/test-cleanup-scripts.sh",
    env = {
        "RUE_CLEANUP_SCRIPTS_ROOT": "$(location :cleanup-script-inputs)",
    },
)

# The developer wrapper scripts. scripts/test-wrapper-scripts.sh (RUE-537,
# RUE-549, RUE-550, RUE-590) runs copies of them against fake tools — no real
# build — to pin that resolver failures are surfaced (not swallowed), that
# run/exec resolve relative paths from the caller's cwd, that filtered CLI
# examples stay repository-anchored across per-case cwd changes, and that the
# sanitizer gives examples the bundled standard library. The filegroup
# materializes these at package-relative paths, matching the layout expected
# under RUE_WRAPPER_ROOT.
filegroup(
    name = "wrapper-script-inputs",
    srcs = [
        "fmt.sh",
        "scripts/rue",
        "scripts/rue-bin",
        "scripts/run-sanitizer.sh",
        "test.sh",
    ],
)

sh_test(
    name = "wrapper-script-tests",
    test = "scripts/test-wrapper-scripts.sh",
    env = {
        "RUE_WRAPPER_ROOT": "$(location :wrapper-script-inputs)",
    },
)
