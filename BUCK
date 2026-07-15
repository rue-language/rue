# Repo-root test-suite targets (RUE-144 / RUE-132).
#
# Each sh_test ties a test-harness binary to the rue compiler and the on-disk
# inputs the suite actually reads (cases/, std/, docs/), so Buck owns the binary
# handoff and caches each suite against its real inputs:
#
#   buck2 test //...        # runs unit tests + spec/UI/CLI suites + repo gates
#
# An edit under crates/rue-spec/cases/ re-runs only the spec suite; an edit
# under std/ re-runs the CLI and spec suites (std/ MUST be a declared input here
# or either suite could get false cache hits on standard-library changes).
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

# Formatting is a first-class repository check. The source files are both
# passed to rustfmt and declared as resources, so Buck invalidates the cached
# result when any Rust source changes. The rustfmt target's RunInfo owns host
# selection and dynamic-library setup; write-mode formatting remains in
# fmt.sh because build/test actions must not mutate the source tree.
_RUST_SOURCES = glob(["crates/**/*.rs"])

sh_test(
    name = "fmt-check",
    test = "toolchains//rust:rustfmt",
    args = [
        "--edition",
        "2024",
        "--check",
    ] + _RUST_SOURCES,
    resources = _RUST_SOURCES,
)

# The std library sources are runtime inputs to CLI integration tests and spec
# cases that opt into the real std (compiled programs `@import` them via
# ${REAL_STD}, RUE_STD_DIR, or RUE_REAL_STD_PATH).
filegroup(
    name = "std",
    srcs = glob(["std/**"]),
    visibility = ["PUBLIC"],
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

# A deliberately adversarial multi-module project used to assert that Rue's
# complete native output is byte-reproducible across relocated source roots and
# scheduling/environment perturbations (RUE-616).
filegroup(
    name = "reproducibility-fixture",
    srcs = glob(["reproducibility/fixture/**"]),
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

benchmark_tool_local_inputs = glob(["benchmarks/**"]) + [
        "scripts/append-benchmark.py",
        "scripts/benchmark_collection.py",
        "scripts/benchmark_manifest.py",
        "scripts/benchmark_scaling.py",
        "scripts/benchmark_annotations.py",
        "scripts/benchmark_evolution.py",
        "scripts/benchmark_history.py",
        "scripts/benchmark_metrics.py",
        "scripts/benchmark_recent.py",
        "scripts/benchmark_scenarios.py",
        "scripts/benchmark_validation.py",
        "scripts/generate-charts.py",
        "scripts/generate-site-status.py",
        "scripts/parser-profile.py",
        "scripts/perf-baseline.py",
        "scripts/scaling_workloads.py",
        "scripts/validate-benchmark.py",
        "website/build.sh",
        "website/templates/performance.html",
        "website/templates/index.html",
    ]

filegroup(
    name = "benchmark-tool-inputs",
    srcs = dict([(path, path) for path in benchmark_tool_local_inputs] + [
        ("benchmarks/scenarios/representative/labels.rue", "//benchmarks/scenarios/representative:labels.rue"),
        ("benchmarks/scenarios/representative/labels_alt.rue", "//benchmarks/scenarios/representative:labels_alt.rue"),
        ("benchmarks/scenarios/representative/main.rue", "//benchmarks/scenarios/representative:main.rue"),
        ("benchmarks/scenarios/representative/model.rue", "//benchmarks/scenarios/representative:model.rue"),
        ("benchmarks/scenarios/representative/report.rue", "//benchmarks/scenarios/representative:report.rue"),
        ("benchmarks/scenarios/representative/std/_std.rue", "//benchmarks/scenarios/representative:std_root.rue"),
        ("benchmarks/scenarios/representative/std/math.rue", "//benchmarks/scenarios/representative:std_math.rue"),
        ("benchmarks/scenarios/representative/worker.rue", "//benchmarks/scenarios/representative:worker.rue"),
        ("crates/rue/src/main.rs", "//crates/rue:benchmark-definition"),
        ("crates/rue-compiler-session-bench/src/main.rs", "//crates/rue-compiler-session-bench:benchmark-main-definition"),
        ("crates/rue-compiler-session-bench/src/representative.rs", "//crates/rue-compiler-session-bench:benchmark-representative-definition"),
    ]),
)

sh_test(
    name = "spec-tests",
    labels = ["rue_heavy_suite"],
    test = "//crates/rue-spec:rue-spec",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_REAL_STD_PATH": "$(location :std)/std",
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
    labels = ["rue_heavy_suite"],
    test = "//crates/rue-ui-tests:rue-ui-tests",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_UI_CASES": "$(location //crates/rue-ui-tests:cases)/cases",
    },
)

sh_test(
    name = "cli-tests",
    labels = ["rue_heavy_suite"],
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
    name = "reproducible-programs",
    labels = ["rue_heavy_suite"],
    test = "scripts/test-reproducible-output.sh",
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_REPRO_FIXTURE": "$(location :reproducibility-fixture)/reproducibility/fixture",
        "RUE_STD_DIR": "$(location :std)/std",
    },
)

# A fixed generated differential corpus in every full test run. The generator
# unit contract pins that seeds 0..63 retain every required fragile source
# shape; this target then compiles and runs those programs through both the
# reference oracle and native codegen. It lives at the root so full/no-argument
# `test.sh` and CI include it while `quick-test.sh` remains unit-only.
sh_test(
    name = "oracle-diff-generated-smoke",
    labels = ["rue_heavy_suite"],
    test = "scripts/oracle-diff-generated-smoke.sh",
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_ORACLE_DIFF_BINARY": "$(exe_target //crates/rue-oracle-diff:rue-oracle-diff)",
    },
    # Preserve enough outer margin for the harness to print all structured
    # findings even if every compiler and native phase consumes its 2s budget.
    test_rule_timeout_ms = 600000,
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
        "RUE_DEEP_NESTING_CASE": "$(location //crates/rue-cli-tests:deep-nesting-case)/cases/deep_nesting.toml",
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
        "scripts/provision-build-cache",
        "scripts/with-full-suite-lock",
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

filegroup(
    name = "build-sharing-test-inputs",
    srcs = [
        "buck2",
        "buck2-bin",
        "scripts/provision-build-cache",
        "scripts/with-full-suite-lock",
        "test.sh",
    ],
)

sh_test(
    name = "build-sharing-tests",
    test = "scripts/test-build-sharing.sh",
    env = {
        "RUE_BUILD_SHARING_ROOT": "$(location :build-sharing-test-inputs)",
    },
)
