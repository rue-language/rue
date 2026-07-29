# Repo-root test-suite targets (RUE-144 / RUE-132).
#
# Each suite ties a test-harness binary to the rue compiler and the on-disk
# inputs it actually reads (cases/, std/, docs/), so Buck owns the binary
# handoff and keys each suite on its real inputs:
#
#   buck2 test //...        # runs unit tests + spec/UI/CLI suites + repo gates
#
# An edit under crates/rue-spec/cases/ re-runs only the spec suite; an edit
# under std/ re-runs the CLI and spec suites (std/ MUST be a declared input here
# or either suite could get false cache hits on standard-library changes).
#
# RUE-1118: the heavy corpora run through `cached_corpus_suite` rather than a
# bare sh_test. buck2 re-executes every test invocation — test executions are
# not actions and never reach the action cache — so a plain sh_test re-ran the
# whole corpus on each merge even when the merge commit's tree was byte-identical
# to the tree the PR run had just validated. The macro moves the harness into a
# cacheable build action and leaves a thin sh_test asserting its stamp, so the
# suite keeps its name, labels, and result line. See corpus.bzl for the input
# contract this makes load-bearing.
#
# Mechanics: the harness binaries already locate everything via env vars
# (rue-test-runner's find_rue_binary / find_dir), and a filegroup's output
# directory is named after the rule and contains its srcs at package-relative
# paths, hence the `$(location ...)/cases` shape. In an sh_test those macros
# expand to absolute paths and the test runs from the project root; in a
# genrule they are relative to the action's working directory, which is why
# cached_corpus_suite routes them through scripts/corpus-action.
#
# These suites live at the repo root rather than in the harness crates' BUCK
# files so that `buck2 test //crates/...` (quick-test.sh, test.sh's filtered
# path) still means "unit tests only".

load("//:test_defs.bzl", "rue_sh_test", "rue_test_suite")
load(":corpus.bzl", "cached_corpus_suite")

# The two halves of a cached corpus suite: the action wrapper that runs a
# harness and writes its stamp, and the thin check that asserts the stamp.
sh_binary(
    name = "corpus-action",
    main = "scripts/corpus-action",
    # RUE-1163: corpora outside the root package (the RUE-205/RUE-204 oracle
    # differentials) run through the same wrapper.
    visibility = ["PUBLIC"],
)

sh_binary(
    name = "corpus-stamp-check",
    main = "scripts/corpus-stamp-check",
    visibility = ["PUBLIC"],
)

# The formatting gate lives in fmt.sh, not here. A `fmt-check` sh_test used to
# sit at this spot, taking its file list from `glob(["crates/**/*.rs"])`. A
# Buck glob does not descend into subpackages, and all 30 crates that own a
# BUCK file are subpackages -- so the list resolved to the single source under
# crates/rue-runtime-asan/ (the one crate without a BUCK file). The gate ran
# rustfmt over 1 file of 281 and reported a pass for the other 280 (RUE-1152).
#
# Restoring cache-aware checking here means enumerating sources per crate,
# which is a real change rather than a glob tweak; until then CI calls
# `./fmt.sh check`, whose `find`-based discovery covers every source and now
# fails rather than exits 0 when it finds none.

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

# Syntax-valid, checked-in Rue programs compared by the independent stage-1
# frontend differential. Keeping the selection explicit excludes intentionally
# malformed UI/spec/CLI fixtures without a filename heuristic.
filegroup(
    name = "frontend-diff-corpus",
    srcs = dict([(path, path) for path in glob([
        "examples/**/*.rue",
        "reproducibility/**/*.rue",
        "std/**/*.rue",
    ])]),
    visibility = ["PUBLIC"],
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

# Required pull-request and merge-group CI must not execute a moving container
# tag. Keep this list explicit so the policy follows branch-protection scope
# rather than accidentally treating an unrelated maintenance workflow as a
# required check.
filegroup(
    name = "required-ci-workflows",
    srcs = [".github/workflows/ci.yml"],
)

cached_corpus_suite(
    name = "spec-tests",
    labels = ["rue_heavy_suite"],
    harness = "//crates/rue-spec:rue-spec",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_REAL_STD_PATH": "$(location :std)/std",
        "RUE_SPEC_CASES": "$(location //crates/rue-spec:cases)/cases",
    },
    absolutize = [
        "RUE_BINARY",
        "RUE_REAL_STD_PATH",
        "RUE_SPEC_CASES",
    ],
)

rue_sh_test(
    name = "spec-traceability",
    test = "//crates/rue-spec:rue-spec",
    args = ["--traceability"],
    env = {
        "RUE_SPEC_CASES": "$(location //crates/rue-spec:cases)/cases",
        "RUE_SPEC_DIR": "$(location :spec-docs)/docs/spec/src",
    },
)

# RUE-1118: RUE_REAL_STD_PATH was missing here. Cases marked `real_std` compile
# against the standard library, and rue-test-runner resolves it through that
# variable with a cwd-relative fallback ("std", "../std", ...). Under the old
# sh_test, which ran from the project root, the fallback silently found the real
# std/ and the suite passed against an input Buck did not know about — the exact
# false-hit hazard this file's header warns about. Declaring it makes std/ a
# tracked input of the UI corpus.
cached_corpus_suite(
    name = "ui-tests",
    labels = ["rue_heavy_suite"],
    harness = "//crates/rue-ui-tests:rue-ui-tests",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_REAL_STD_PATH": "$(location :std)/std",
        "RUE_UI_CASES": "$(location //crates/rue-ui-tests:cases)/cases",
    },
    absolutize = [
        "RUE_BINARY",
        "RUE_REAL_STD_PATH",
        "RUE_UI_CASES",
    ],
)

# Shared verbatim by //:cli-tests and its shards so a slice runs exactly the
# same cases the monolithic target would. The full Caldera and Meridian roots
# deliberately live outside the required pre-merge corpus: reduced canaries
# below exercise their core compiler/runtime paths, while the real applications
# compile and run in the explicit slow tier.
_CLI_TEST_ARGS = [
    "--quiet",
    "--skip", "cli.examples::caldera::main",
    "--skip", "cli.examples::meridian::main",
    "--skip", "cli.examples_meridian",
]

_CLI_TEST_BASE_ENV = {
    "RUE_BINARY": "$(exe_target //crates/rue:rue)",
    "RUE_CLI_CASES": "$(location //crates/rue-cli-tests:cases)/cases",
    "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
    "RUE_REPO_DIR": "$(location :cli-test-fixtures)",
    "RUE_STD_DIR": "$(location :std)/std",
}

_CLI_TEST_ENV = dict(_CLI_TEST_BASE_ENV.items() + [
    ("RUE_CLI_CASE_TIER", "premerge"),
])

# Every _CLI_TEST_ENV entry is a path the harness hands to a compiler spawned
# with a case's temp directory as cwd, so all of them must be absolute (see
# corpus.bzl). The harness's find_dir fallbacks would otherwise resolve against
# the action's working directory and silently miss the real corpus.
_CLI_TEST_ABSOLUTIZE = [
    "RUE_BINARY",
    "RUE_CLI_CASES",
    "RUE_EXAMPLES_DIR",
    "RUE_REPO_DIR",
    "RUE_STD_DIR",
]

# RUE-1083 recalibrated several per-case heavyweight compile budgets upward, so
# the serialized aggregate can exceed a short outer bound. These replace the
# test executor's timeout, which scripts/ci-heavy-suite used to pass and a build
# action does not get; the per-case contracts in execution_contracts.toml remain
# the honest gates. Re-tighten when the per-case budgets come back down.
#
# RUE-1163: these must cover the correctness deadline
# scripts/cli-timeout-policy.py derives from the same measured weights, on every
# platform in shard-weights.json — an action bound that cuts inside it kills
# healthy runs. //:cli-tests sat at 1800s against a 3600s derived deadline (and
# a 2203s measured expected cost) until //:cli-timeout-policy-validation started
# comparing the two.
_CLI_TESTS_TIMEOUT_SECONDS = 3700
_CLI_SHARD_TIMEOUT_SECONDS = 1200

# The bounded premerge CLI corpus in one invocation: the canonical target that a
# local `./test.sh` full run executes and that the RUE-924 corpus-omission audit
# tracks (REQUIRED_CORPUS_HARNESSES in test.sh). Explicit slow sections and
# automatic examples are registered by //:cli-tests-slow instead.
cached_corpus_suite(
    name = "cli-tests",
    labels = ["rue_heavy_suite"],
    harness = "//crates/rue-cli-tests:rue-cli-tests",
    args = _CLI_TEST_ARGS,
    env = _CLI_TEST_ENV,
    absolutize = _CLI_TEST_ABSOLUTIZE,
    timeout_seconds = _CLI_TESTS_TIMEOUT_SECONDS,
)

# Exhaustive behavior for declarative `tier = "slow"` CLI sections. This is a
# separate real Buck target, not a skipped body: standard/full local runs and
# scheduled release coverage execute it, while required premerge shards do not.
# RUE-1163: the last corpus still carrying a test-executor timeout. Its bound
# came from `scripts/cli-timeout-policy.py` invoked at run time inside
# scripts/ci-heavy-suite; for this target the tool returns a fixed slow-suite
# guard rather than a weight-derived value, so stating it here loses no
# derivation and removes the last per-target branch from that script. The
# per-case budgets in execution_contracts.toml remain the honest gates.
cached_corpus_suite(
    name = "cli-tests-slow",
    tier = "slow",
    labels = ["rue_heavy_suite"],
    harness = "//crates/rue-cli-tests:rue-cli-tests",
    args = ["--quiet"],
    env = dict(_CLI_TEST_BASE_ENV.items() + [
        ("RUE_CLI_CASE_TIER", "slow"),
    ]),
    absolutize = _CLI_TEST_ABSOLUTIZE,
    timeout_seconds = 7200,
)

# Required release coverage is deliberately bounded: compile the real driver
# and CLI harness under //platforms:release, then run the representative
# differential-opt corpus through that release-built compiler. The scheduled
# full-release workflow owns exhaustive //... coverage off the PR critical
# path (RUE-1129).
cached_corpus_suite(
    name = "release-smoke",
    harness = "//crates/rue-cli-tests:rue-cli-tests",
    args = ["--quiet", "differential_opt"],
    env = _CLI_TEST_ENV,
    absolutize = _CLI_TEST_ABSOLUTIZE,
)

# RUE-1116: parallel CI shards of the CLI corpus. Same harness and declared
# inputs as //:cli-tests, but each sets RUE_CLI_TEST_SHARD=k/N so it runs one
# deterministic cost-balanced slice; the shards' union is the full premerge
# inventory. They carry BOTH labels deliberately:
#   * rue_heavy_suite — scripts/ci-heavy-suite accepts them unchanged, and the
#     broad `buck2 test //... --exclude rue_heavy_suite` pass skips them;
#   * rue_cli_shard — a local `./test.sh` full run runs the premerge
#     //:cli-tests exactly once instead of re-running every slice (test.sh
#     subtracts rue_cli_shard from its heavy-suite discovery).
# The `platform-corpus` matrix in .github/workflows/ci.yml MUST list all
# CLI_TEST_SHARD_COUNT shards on every platform that runs the CLI corpus;
# //:cli-shard-coverage-validation fails the build if BUCK and the matrix drift.
CLI_TEST_SHARD_COUNT = 4

[
    cached_corpus_suite(
        name = "cli-tests-shard-{}".format(_shard),
        labels = ["rue_heavy_suite", "rue_cli_shard"],
        harness = "//crates/rue-cli-tests:rue-cli-tests",
        args = _CLI_TEST_ARGS,
        env = dict(_CLI_TEST_ENV.items() + [
            ("RUE_CLI_TEST_SHARD", "{}/{}".format(_shard, CLI_TEST_SHARD_COUNT)),
            ("RUE_CLI_SHARD_WEIGHTS", "$(location //crates/rue-cli-tests:shard-weights)"),
        ]),
        absolutize = _CLI_TEST_ABSOLUTIZE,
        timeout_seconds = _CLI_SHARD_TIMEOUT_SECONDS,
        # RUE-1158 rebalances the shards from measured per-case cost. The
        # measurements are a declared output of the action rather than an
        # executor --env path, so a cache hit replays the timings that produced
        # the tree instead of leaving shard-weights.json to refresh only when a
        # shard actually executes. See cached_corpus_suite's case_timings doc.
        case_timings = True,
    )
    for _shard in range(CLI_TEST_SHARD_COUNT)
]

# The required pre-merge canaries compile a reduced root from each maintained
# application and execute its core path. They are intentionally honest about
# their scope: neither target claims to compile the complete generated graph.
[
    rue_sh_test(
        name = "large-example-{}-canary".format(_program),
        test = "scripts/run-large-example.sh",
        args = [_program, "canary"],
        env = {
            "RUE_BINARY": "$(exe_target //crates/rue:rue)",
            "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
            "RUE_STD_DIR": "$(location :std)/std",
        },
        test_rule_timeout_ms = 600000,
    )
    for _program in ["caldera", "meridian"]
]

# Scheduled slow coverage compiles each complete application exactly once and
# reuses the resulting executable across its help/demo/file/selftest/scaling
# and benchmark runtime scenarios. The release workflow selects these targets
# explicitly under //platforms:release.
[
    rue_sh_test(
        name = "large-example-{}-slow".format(_program),
        tier = "slow",
        labels = ["rue_scheduled_large_example"],
        test = "scripts/run-large-example.sh",
        args = [_program, "slow"],
        env = {
            "RUE_BINARY": "$(exe_target //crates/rue:rue)",
            "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
            "RUE_STD_DIR": "$(location :std)/std",
        },
        test_rule_timeout_ms = 7200000,
    )
    for _program in ["caldera", "meridian"]
]

# The 4x generated workload is an extreme scaling experiment rather than a
# correctness smoke, so it has explicit stress-tier ownership.
[
    rue_sh_test(
        name = "large-example-{}-stress".format(_program),
        tier = "stress",
        labels = ["rue_scheduled_large_example"],
        test = "scripts/run-large-example.sh",
        args = [_program, "stress"],
        env = {
            "RUE_BINARY": "$(exe_target //crates/rue:rue)",
            "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
            "RUE_STD_DIR": "$(location :std)/std",
        },
        test_rule_timeout_ms = 7200000,
    )
    for _program in ["caldera", "meridian"]
]

# RUE-1083: `examples/` is a declared input because this suite now also checks a
# real maintained program (rill) for byte-stable output, not just the
# purpose-built fixture. An edit under examples/ must therefore re-run it.
#
# RUE-1163: converted to a cached action. RUE-1118 left it out on the grounds
# that it "reads repository paths directly rather than through declared env
# inputs"; that is no longer true of the current script, which guards all four
# of its inputs with `${VAR:?}` and reads nothing else from the checkout. Its
# only relative path (`../sources.manifest`) resolves inside the temporary copy
# of RUE_REPRO_FIXTURE.
#
# Caching a suite whose subject IS determinism deserves a note: a cache hit
# replays a proof rather than re-running it, so a compiler that became
# nondeterministic only intermittently would not be caught by a replayed run.
# That is the same bargain every corpus here takes, and RUE-1159's repetition
# workflow — which exists for exactly that class — already runs cache-free.
sh_binary(
    name = "reproducible-programs-harness",
    main = "scripts/test-reproducible-output.sh",
)

cached_corpus_suite(
    name = "reproducible-programs",
    labels = ["rue_heavy_suite"],
    harness = ":reproducible-programs-harness",
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
        "RUE_REPRO_FIXTURE": "$(location :reproducibility-fixture)/reproducibility/fixture",
        "RUE_STD_DIR": "$(location :std)/std",
    },
    absolutize = [
        "RUE_BINARY",
        "RUE_EXAMPLES_DIR",
        "RUE_REPRO_FIXTURE",
        "RUE_STD_DIR",
    ],
    timeout_seconds = 1800,
)

# The independent stage-1 frontend differential: compile `examples/ruelex` with
# the production compiler, then diff its token dump and AST shape against the
# production lexer/parser for every corpus file.
#
# RUE-1154 moved this here from crates/rue-frontend-diff/BUCK and labeled it.
# It is a corpus-scale harness — one ruelex compile plus two child processes per
# corpus file, ~2900 in all, about a minute of wall clock — so leaving it in the
# crate package had it running inside `buck2 test //crates/...` (contradicting
# that pattern's unit-only contract, and quick-test.sh's advertised few seconds)
# and inside the broad `--exclude rue_heavy_suite` pass, contending with every
# other test on the runner. Heavy-labeled at the root, it runs alone through
# scripts/ci-heavy-suite like every peer corpus harness.
# RUE-1163: converted to a cached action. Every path this harness reads arrives
# through a declared env input (`RUE_BINARY`, `RUE_FRONTEND_DIFF_CORPUS`,
# `RUE_STD_PATH`); the source-relative fallbacks in its `main` apply only when a
# variable is unset, which cannot happen here. The corpus filegroup enumerates
# its members explicitly, so a new corpus file changes the action's digest.
cached_corpus_suite(
    name = "frontend-diff-test",
    labels = ["rue_heavy_suite"],
    harness = "//crates/rue-frontend-diff:rue-frontend-diff",
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_FRONTEND_DIFF_CORPUS": "$(location :frontend-diff-corpus)",
        "RUE_STD_PATH": "$(location :std)/std",
    },
    absolutize = [
        "RUE_BINARY",
        "RUE_FRONTEND_DIFF_CORPUS",
        "RUE_STD_PATH",
    ],
    timeout_seconds = 900,
)

# A fixed generated differential corpus in every full test run. The generator
# unit contract pins that seeds 0..63 retain every required fragile source
# shape; this target then compiles and runs those programs through both the
# reference oracle and native codegen. It lives at the root so full/no-argument
# `test.sh` and CI include it while `quick-test.sh` remains unit-only.
sh_binary(
    name = "oracle-diff-generated-smoke-harness",
    main = "scripts/oracle-diff-generated-smoke.sh",
)

# RUE-1163: a cached action. Both binaries arrive through declared `$(exe_target
# ...)` inputs and the script reads nothing else; the seed range is fixed, so
# the run is a pure function of its inputs. Caching also stops the fixed
# two-second per-child budget from being re-rolled on every invocation — a
# timeout-only flake under parallel load no longer recurs once the tree has
# passed (AGENTS.md documents that failure mode).
cached_corpus_suite(
    name = "oracle-diff-generated-smoke",
    labels = ["rue_heavy_suite"],
    harness = ":oracle-diff-generated-smoke-harness",
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_ORACLE_DIFF_BINARY": "$(exe_target //crates/rue-oracle-diff:rue-oracle-diff)",
    },
    absolutize = [
        "RUE_BINARY",
        "RUE_ORACLE_DIFF_BINARY",
    ],
    # Preserve enough outer margin for the harness to print all structured
    # findings even if every compiler and native phase consumes its 2s budget.
    timeout_seconds = 600,
)

rue_sh_test(
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

rue_sh_test(
    name = "tutorial-snippet-tool-tests",
    test = "scripts/test-tutorial-snippets.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_TUTORIAL_TEST_ROOT": "$(location :tutorial-snippet-tool-inputs)",
    },
)

rue_sh_test(
    name = "adr-registry-validation",
    test = "scripts/validate-adrs.py",
    args = [
        "--adr-dir",
        "$(location :adr-designs)/docs/designs",
    ],
)

rue_sh_test(
    name = "required-ci-container-pin-validation",
    test = "scripts/validate-required-ci-container-pins.py",
    args = [
        "$(location :required-ci-workflows)/.github/workflows/ci.yml",
        # The remote executor's worker image is required CI's other container
        # (RUE-1165): the merge-group canary runs the compiler build on it. It
        # must carry an immutable digest, not merely avoid a `latest` tag.
        "--digest-pinned",
        "$(location //platforms:remote-execution-platforms)/remote_cache.bzl",
    ],
)

rue_sh_test(
    name = "required-ci-container-pin-tool-tests",
    test = "scripts/test-required-ci-container-pins.py",
    resources = ["scripts/validate-required-ci-container-pins.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "debug-assert-policy-tool-tests",
    test = "scripts/test-debug-assert-policy.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "shell-pipefail-pipeline-tool-tests",
    test = "scripts/test-validate-shell-pipefail-pipelines.py",
    resources = ["scripts/validate-shell-pipefail-pipelines.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "release-configuration-tool-tests",
    test = "scripts/test-release-configuration.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# The root BUCK file, so the CLI-shard coverage gate can read CLI_TEST_SHARD_COUNT
# and the generated shard targets as a declared input.
filegroup(
    name = "root-buck-file",
    srcs = ["BUCK"],
)

# RUE-1116: fail the build if the CLI shard targets in BUCK and the shards
# listed in the required CI matrix drift apart. A shard present in BUCK but
# missing from the matrix would silently drop that fraction of the corpus on CI
# (the RUE-924 false-green failure mode), since nothing else re-runs the slices.
rue_sh_test(
    name = "cli-shard-coverage-validation",
    test = "scripts/validate-cli-shard-coverage.py",
    args = [
        "--buck",
        "$(location :root-buck-file)/BUCK",
        "--workflow",
        "$(location :required-ci-workflows)/.github/workflows/ci.yml",
    ],
)

rue_sh_test(
    name = "cli-shard-coverage-tool-tests",
    test = "scripts/test-cli-shard-coverage.py",
    resources = ["scripts/validate-cli-shard-coverage.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# RUE-1117: the declared inputs of the tier CI-selector gate. The tier
# vocabulary and every workflow that is registered as deliberately selecting a
# tier are inputs, so an edit to any of them re-runs the gate.
filegroup(
    name = "tier-ci-selector-inputs",
    srcs = [
        ".github/workflows/ci.yml",
        ".github/workflows/release.yml",
        "test_defs.bzl",
        "test_tiers.bxl",
    ],
)

# RUE-1117: `//test_tiers.bxl:validate` proves every test target owns exactly one
# tier; it cannot prove any CI job runs that tier. This gate requires each tier
# to be selected by a *named* job, so a target moved into a tier nothing selects
# fails the build instead of quietly leaving required CI — the way the
# RUE-205/RUE-204 codegen differential did.
rue_sh_test(
    name = "tier-ci-selector-validation",
    test = "scripts/validate-tier-ci-selectors.py",
    args = [
        "--test-defs",
        "$(location :tier-ci-selector-inputs)/test_defs.bzl",
        "--test-tiers-bxl",
        "$(location :tier-ci-selector-inputs)/test_tiers.bxl",
        "--workflow",
        "$(location :tier-ci-selector-inputs)/.github/workflows/ci.yml",
        "--workflow",
        "$(location :tier-ci-selector-inputs)/.github/workflows/release.yml",
    ],
)

rue_sh_test(
    name = "tier-ci-selector-tool-tests",
    test = "scripts/test-validate-tier-ci-selectors.py",
    resources = ["scripts/validate-tier-ci-selectors.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_TIER_VALIDATION_ROOT": "$(location :tier-ci-selector-inputs)",
    },
)

rue_sh_test(
    name = "ci-required-results-tool-tests",
    test = "scripts/test-ci-required-results.py",
    resources = ["scripts/ci-required-results.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "ci-gate-validation",
    test = "scripts/validate-ci-gate.py",
    args = [
        "$(location :required-ci-workflows)/.github/workflows/ci.yml",
        # RUE-1161: the harness's declared platform responsibility matrix is a
        # real input, so a lane added to (or removed from) either side without
        # the other fails here instead of silently crediting specification
        # coverage to cases no lane executes.
        "--test-runner-source",
        "$(location //crates/rue-test-runner:platform-responsibility-source)/src/lib.rs",
    ],
    resources = [
        "scripts/ci-required-results.py",
        "scripts/run-native-platform-corpus.sh",
    ],
)

rue_sh_test(
    name = "ci-gate-validator-tool-tests",
    test = "scripts/test-validate-ci-gate.py",
    resources = [
        "scripts/ci-required-results.py",
        "scripts/run-native-platform-corpus.sh",
        "scripts/validate-ci-gate.py",
    ],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_CI_WORKFLOW": "$(location :required-ci-workflows)/.github/workflows/ci.yml",
        "RUE_TEST_RUNNER_SOURCE": "$(location //crates/rue-test-runner:platform-responsibility-source)/src/lib.rs",
    },
)

rue_sh_test(
    name = "cli-shard-weights-validation",
    test = "scripts/generate-cli-shard-weights.py",
    args = [
        "--check",
        "--output",
        "$(location //crates/rue-cli-tests:shard-weights)",
    ],
)

rue_sh_test(
    name = "cli-shard-weight-tool-tests",
    test = "scripts/test-cli-shard-weights.py",
    resources = ["scripts/generate-cli-shard-weights.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "cli-timeout-policy-validation",
    test = "scripts/cli-timeout-policy.py",
    args = [
        "--policy",
        "$(location //crates/rue-cli-tests:cases)/cases/execution_contracts.toml",
        "--weights",
        "$(location //crates/rue-cli-tests:shard-weights)",
        # RUE-1163: a corpus action gets no test-executor timeout, so the
        # `timeout_seconds` spelled here is the only bound on a wedged harness.
        # Declaring this file as an input makes the two sources of truth fail
        # closed when they disagree, instead of a static number silently
        # tightening below the deadline the policy derives.
        "--buck",
        "$(location :root-buck-file)/BUCK",
    ],
)

rue_sh_test(
    name = "cli-timeout-policy-tool-tests",
    test = "scripts/test-cli-timeout-policy.py",
    resources = ["scripts/cli-timeout-policy.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_CLI_CASES": "$(location //crates/rue-cli-tests:cases)/cases",
    },
)

rue_sh_test(
    name = "correctness-repetition-script-tests",
    test = "scripts/test-ci-repeat-correctness.sh",
    resources = ["scripts/ci-repeat-correctness"],
)

filegroup(
    name = "timeout-workflow-test-inputs",
    srcs = [
        ".github/workflows/ci.yml",
        ".github/workflows/correctness-repetitions.yml",
        "scripts/ci-repeat-correctness",
    ],
)

rue_sh_test(
    name = "timeout-workflow-contract-tests",
    test = "scripts/test-timeout-workflow-contracts.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_TIMEOUT_WORKFLOW_ROOT": "$(location :timeout-workflow-test-inputs)",
    },
)

# RUE-1119: pin the deterministic, coverage-deciding logic of the affected-
# corpus selection — the out-of-graph force-full matcher in
# scripts/affected-targets and the fail-open gate in scripts/ci-corpus-selected.
# The test uses local stubs for the BTD/Buck contract, so it proves a selective
# decision without requiring a network download or a real Buck graph.
rue_sh_test(
    name = "affected-targets-tool-tests",
    test = "scripts/test-affected-targets.sh",
    resources = [
        "scripts/affected-targets",
        "scripts/ci-corpus-decision",
        "scripts/ci-corpus-selected",
        "scripts/parse-btd-impacted.py",
    ],
)

rue_sh_test(
    name = "runtime-abi-inventory-validation",
    test = "scripts/validate-runtime-abi-inventory.py",
    args = [
        "--source", "rue-air=$(location //crates/rue-air:runtime-abi-inventory-sources)",
        "--source", "rue-builtins=$(location //crates/rue-builtins:runtime-abi-inventory-sources)",
        "--source", "rue-cfg=$(location //crates/rue-cfg:runtime-abi-inventory-sources)",
        "--source", "rue-codegen=$(location //crates/rue-codegen:runtime-abi-inventory-sources)",
        "--source", "rue-compiler=$(location //crates/rue-compiler:runtime-abi-inventory-sources)",
        "--source", "rue-linker=$(location //crates/rue-linker:runtime-abi-inventory-sources)",
        "--source", "rue-oracle=$(location //crates/rue-oracle:runtime-abi-inventory-sources)",
    ],
)

rue_sh_test(
    name = "runtime-abi-inventory-tool-tests",
    test = "scripts/test-runtime-abi-inventory.py",
    resources = ["scripts/validate-runtime-abi-inventory.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "type-architecture-inventory-validation",
    test = "scripts/validate-type-architecture.py",
    args = [
        "--source", "rue-air=$(location //crates/rue-air:type-architecture-inventory-sources)",
        "--source", "rue-cfg=$(location //crates/rue-cfg:type-architecture-inventory-sources)",
        "--source", "rue-codegen=$(location //crates/rue-codegen:type-architecture-inventory-sources)",
        "--source", "rue-compiler=$(location //crates/rue-compiler:type-architecture-inventory-sources)",
        "--source", "rue-oracle=$(location //crates/rue-oracle:type-architecture-inventory-sources)",
    ],
)

rue_sh_test(
    name = "type-architecture-inventory-tool-tests",
    test = "scripts/test-type-architecture.py",
    resources = ["scripts/validate-type-architecture.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "payload-ownership-inventory-validation",
    test = "scripts/validate-payload-ownership.py",
    args = [
        "--source", "rue-rir=$(location //crates/rue-rir:payload-ownership-inventory-sources)",
        "--source", "rue-air=$(location //crates/rue-air:payload-ownership-inventory-sources)",
        "--source", "rue-cfg=$(location //crates/rue-cfg:payload-ownership-inventory-sources)",
        "--source", "rue-codegen=$(location //crates/rue-codegen:payload-ownership-inventory-sources)",
    ],
)

rue_sh_test(
    name = "payload-ownership-inventory-tool-tests",
    test = "scripts/test-payload-ownership.py",
    resources = ["scripts/validate-payload-ownership.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "body-analysis-capability-inventory-validation",
    test = "scripts/validate-body-analysis-capabilities.py",
    args = [
        "--source", "rue-air=$(location //crates/rue-air:body-analysis-capability-inventory-sources)",
        "--source", "rue-compiler=$(location //crates/rue-compiler:body-analysis-capability-inventory-sources)",
    ],
)

rue_sh_test(
    name = "body-analysis-capability-inventory-tool-tests",
    test = "scripts/test-body-analysis-capabilities.py",
    resources = ["scripts/validate-body-analysis-capabilities.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_test_suite(
    name = "payload-ownership-compile-fail-tests",
    tests = ["//crates/rue-rir:rue-rir[doc]"],
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

rue_sh_test(
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
        "scripts/ci-heavy-suite",
        "scripts/cli-timeout-policy.py",
        "scripts/ci-timed",
        "scripts/check-cache-probe",
        "scripts/rue",
        "scripts/rue-bin",
        "scripts/provision-build-cache",
        "scripts/with-full-suite-lock",
        "scripts/run-large-example.sh",
        "scripts/run-sanitizer.sh",
        "test.sh",
    ],
)

rue_sh_test(
    name = "wrapper-script-tests",
    test = "scripts/test-wrapper-scripts.sh",
    env = {
        "RUE_WRAPPER_ROOT": "$(location :wrapper-script-inputs)",
    },
)

# RUE-1118: corpus-action decides whether a corpus suite's result is written to
# the action cache, so its stamp-only-on-success and absolutization contracts
# are pinned independently of any corpus actually running.
filegroup(
    name = "corpus-script-inputs",
    srcs = [
        "scripts/corpus-action",
        "scripts/corpus-stamp-check",
    ],
)

rue_sh_test(
    name = "corpus-action-tests",
    test = "scripts/test-corpus-action.sh",
    env = {
        "RUE_CORPUS_SCRIPTS_ROOT": "$(location :corpus-script-inputs)",
    },
)

filegroup(
    name = "build-sharing-test-inputs",
    srcs = [
        "buck2",
        "buck2-bin",
        "scripts/ci-heavy-suite",
        "scripts/cli-timeout-policy.py",
        "scripts/provision-build-cache",
        "scripts/with-full-suite-lock",
        "test.sh",
    ],
)

rue_sh_test(
    name = "build-sharing-tests",
    test = "scripts/test-build-sharing.sh",
    env = {
        "RUE_BUILD_SHARING_ROOT": "$(location :build-sharing-test-inputs)",
    },
)
