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

load("//:rue_rules.bzl", "rue_program", "rue_program_family", "rue_program_staging", "rue_program_test")
load("//:test_defs.bzl", "rue_sh_test", "rue_test_suite")
load(":corpus.bzl", "cached_corpus_suite")

rue_sh_test(
    name = "compiler-allocator-policy-validation",
    test = "scripts/check-compiler-allocator-policy.sh",
    env = {
        "RUE_ALLOCATOR_CRATE_ROOT": "$(location //crates/rue:allocator-policy-inputs)",
        "RUE_ALLOCATOR_POLICY_NOTE": "$(location :compiler-allocator-policy-note)",
        "RUE_ALLOCATOR_THIRD_PARTY_ROOT": "$(location //third-party:allocator-policy-inputs)",
        "RUE_ALLOCATOR_ZIG_ROOT": "$(location toolchains//zig:policy-inputs)",
    },
)

export_file(
    name = "compiler-allocator-policy-note",
    src = "docs/notes/adr-0071-phase-3-linux-compiler-allocator.md",
)

# Versioned configuration anchors for the repository's Rust quality gates.
# Both files are intentionally policy-neutral today; exporting them makes
# future configuration changes explicit Buck inputs rather than cwd-dependent
# tool discovery.
export_file(
    name = "clippy-config",
    src = "clippy.toml",
    visibility = ["PUBLIC"],
)

filegroup(
    name = "rustfmt-config",
    srcs = [".rustfmt.toml"],
    visibility = ["PUBLIC"],
)

# The per-crate debug-assertion checks emitted by rue_crate/rue_binary
# (RUE-1525) address the gate script and its gatelib helpers cross-package
# through these targets. `mode = "reference"` is load-bearing: the script
# resolves `from gatelib import ...` relative to its own location, so it must
# execute in place under scripts/ rather than as an isolated buck-out copy.
# The gatelib filegroup exists so the emitting macro can declare the helpers
# as resources — a gatelib edit re-runs every crate's check instead of
# serving a stale pass.
export_file(
    name = "debug-assert-policy-script",
    src = "scripts/validate-debug-assert-policy.py",
    mode = "reference",
    visibility = ["PUBLIC"],
)

filegroup(
    name = "gatelib-sources",
    srcs = glob(["scripts/gatelib/*.py"]),
    visibility = ["PUBLIC"],
)

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

# The rue_program tool surface (ADR-0070 / RUE-1404): the scan wrapper, the
# manifest derivation script that owns the declared-boundary check, the two
# advisory precision reports, and the scenario runner. rue_rules.bzl reaches
# them by default attr, so they are PUBLIC.
sh_binary(
    name = "rue-program-scan",
    main = "scripts/rue-program-scan",
    visibility = ["PUBLIC"],
)

# The Python tools run inside cacheable actions that upload their results
# (`allow_cache_upload` in rue_rules.bzl), so the interpreter is a
# toolchain-level decision rather than an undeclared `$PATH` lookup inside a
# cache-keyed action.
[
    python_bootstrap_binary(
        name = _tool,
        main = "scripts/{}".format(_main),
        visibility = ["PUBLIC"],
    )
    for _tool, _main in [
        ("rue-program-derive-manifest", "rue-program-derive-manifest.py"),
        ("rue-program-srcs-precision", "rue-program-srcs-precision.py"),
        ("rue-program-family-report", "rue-program-family-report.py"),
        ("rue-program-test-runner", "rue-program-test-runner.py"),
    ]
]

# The formatting and lint gates are per-crate, not here. A `fmt-check` sh_test
# used to sit at this spot, taking its file list from `glob(["crates/**/*.rs"])`.
# A Buck glob does not descend into subpackages, and every crate owning a BUCK
# file is a subpackage -- so the list resolved to the single source under
# crates/rue-runtime-asan/ (the one crate without a BUCK file). The gate ran
# rustfmt over 1 file of 281 and reported a pass for the other 280 (RUE-1152).
#
# The replacement does not glob across packages at all: rue_crate/rue_binary
# emit a `<name>-fmt-check` and a `<name>-clippy` per crate from the same
# package-local srcs the library compiles (RUE-1153). Coverage is structural --
# a crate is checked because it declares the targets, not because discovery
# reached it.
#
# crates/rue-runtime-asan/ is the one deliberate exception: the cargo-built
# ASan harness (RUE-560) has no BUCK file, so no macro emits its gates. Its
# clippy runs as a dedicated `cargo clippy -D warnings` step in the CI asan
# job; its format check lives here, where the root package can still name its
# sources, so the file the old gate DID cover keeps its coverage. The same
# sources feed its explicit debug-assert check below.
_ASAN_GATE_SRCS = glob(["crates/rue-runtime-asan/src/**/*.rs"])

rue_sh_test(
    name = "rue-runtime-asan-fmt-check",
    test = "toolchains//rust:rustfmt",
    args = [
        "--config-path",
        "$(location :rustfmt-config)",
        "--edition",
        "2024",
        "--check",
    ] + _ASAN_GATE_SRCS,
    resources = _ASAN_GATE_SRCS + [":rustfmt-config"],
    # Excluded from quick iteration like every per-crate gate, so
    # `scripts/rue quick` keeps meaning "unit tests only".
    labels = ["rue_not_quick"],
)

# The same exception for the per-crate debug-assertion checks (RUE-1525): no
# BUCK file means no macro emits `rue-runtime-asan-debug-assert-check`, so the
# root package, which can still name the sources, declares it explicitly.
rue_sh_test(
    name = "rue-runtime-asan-debug-assert-check",
    test = "scripts/validate-debug-assert-policy.py",
    args = [
        "--crate",
        "rue-runtime-asan",
        "--sources",
        "crates/rue-runtime-asan/src",
    ],
    resources = _ASAN_GATE_SRCS + glob(["scripts/gatelib/*.py"]),
    # Excluded from quick iteration like every per-crate gate, so
    # `scripts/rue quick` keeps meaning "unit tests only".
    labels = ["rue_not_quick"],
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
    visibility = ["PUBLIC"],
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
    # RUE-1163: also reached by the //crates/rue-cli-tests:cli developer entry
    # point, which carries the corpus's declared inputs so a filtered run needs
    # no shell to plumb them.
    visibility = ["PUBLIC"],
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
        # RUE-1163: BUCK is an input because the gate membership the tool test
        # checks lives in //:repository-quality-gates now, not in a bash array.
        "BUCK",
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

# RUE-1163: `rue_ci_dedicated_lane` marks a corpus that required CI schedules in
# its own platform-corpus job, so the linux-premerge job's ./test.sh must not
# also run it. It replaces the RUE_CI_DEFER_HEAVY_SUITES environment protocol,
# which named the same two targets in ci.yml and made test.sh re-derive and
# cross-check them against a label query in bash. The set is a Buck fact now,
# and scripts/validate-ci-gate.py fails if it and the workflow's matrix disagree
# — in either direction, so a corpus given the label without a job would be
# caught as surely as one dropped from the matrix.
cached_corpus_suite(
    name = "spec-tests",
    labels = ["rue_heavy_suite", "rue_ci_dedicated_lane"],
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

# ADR-0070 Phase 2 (RUE-1406): the CLI cases that name a checked-in root stop
# compiling it inside the harness. Each root is one `rue_program` build action —
# keyed on its real inputs, uploadable, and shared by every scenario naming it —
# and the corpus actions below declare the staged executables the way they
# declare every other input, through `$(location ...)` in their `attrs.arg()`
# env. The weld this breaks is not a speed problem but a coverage one: the only
# way to run Meridian's sixth scenario was to compile 36k lines a sixth time,
# so RUE-1083 disabled all six rather than pay for them.
#
# 64 of the 73 cases naming a checked-in root migrate, over 9 roots. Only 8 are
# new: `examples/meridian/main.rue` is already a Phase 1 large-example program,
# so one artifact serves both suites — the "many scenarios, one compile"
# property reaching across two suites.
#
# What stays compile-in-harness, all of it deliberate: the 6 cross-target
# `cli-test-fixtures` cases (each (root, target) tuple has exactly one consumer
# and compiles in milliseconds), the repo-relative `source_path` fixture case
# (whose subject IS the TOML resolution mechanism it would be leaving), the one
# `differential_opt` calculator case (four compiles by design, at opt levels the
# runner drives), and the one-scenario wordfreq root. The harness decides this
# structurally rather than from a list — see `case_runs_prebuilt_program`.
#
# The RUE-48 automatic example smokes are untouched: `run_example` still
# compiles each root it discovers through the ordinary driver, so every example
# still proves it compiles the way a user compiles it.
#
# `examples/first/` holds three sibling roots, so `first-stats` names its one
# file instead of globbing the directory; every other root owns its directory
# and takes the directory-bounded glob ADR-0070's over-declaration audit
# documents.
[
    rue_program(
        name = _name,
        root = "examples/{}.rue".format(_root),
        srcs = _srcs,
    )
    for _name, _root, _srcs in [
        ("first-stats", "first/stats", ["examples/first/stats.rue"]),
        ("gazette", "gazette/main", glob(["examples/gazette/**/*.rue"])),
        ("harbor", "harbor/main", glob(["examples/harbor/**/*.rue"])),
        ("jsonfmt", "jsonfmt/main", glob(["examples/jsonfmt/**/*.rue"])),
        ("lattice", "lattice/main", glob(["examples/lattice/**/*.rue"])),
        ("mosaic", "mosaic/main", glob(["examples/mosaic/**/*.rue"])),
        ("rill", "rill/main", glob(["examples/rill/**/*.rue"])),
        ("ruelex", "ruelex/main", glob(["examples/ruelex/**/*.rue"])),
        ("second-calculator", "second/calculator", glob(["examples/second/**/*.rue"])),
    ]
]

# The ten artifacts as one directory keyed by root path, so a corpus action
# declares a single `$(location ...)` and the harness's lookup key is the
# case's own `source_path` string. Consumed by //:cli-tests, //:cli-tests-slow
# and the four shards.
#
# Every consumer declares all ten even though no single corpus target runs
# cases against all ten — mosaic's section is slow-tier, so the premerge
# targets carry it for nothing. That is the simplest correct form ADR-0070
# chose deliberately: it is a mild over-declaration of each action's key (an
# edit to any root already re-runs every CLI corpus action today, since the
# roots live inside the declared `:examples` filegroup) and it fails closed,
# because a case whose program is absent from the staging environment cannot
# silently run a stale one.
rue_program_staging(
    name = "cli-staged-programs",
    programs = [
        ":first-stats",
        ":gazette",
        ":harbor",
        ":jsonfmt",
        ":lattice",
        ":meridian",
        ":mosaic",
        ":rill",
        ":ruelex",
        ":second-calculator",
    ],
)

# Shared verbatim by //:cli-tests and its shards so a slice runs exactly the
# same cases the monolithic target would. What the two skips exclude is the
# automatic RUE-48 smoke over each large application's full root, which still
# COMPILES that root in the harness and cannot fit a per-case budget on a cold
# runner (linux-arm64 killed the meridian one at 300.022s/300s). Reduced
# canaries exercise both applications' core compiler/runtime paths pre-merge,
# and the real roots compile and run in the explicit slow tier.
#
# `cli.examples_meridian` — the six declarative scenarios, not the smoke — is
# no longer skipped. RUE-1083 disabled it because each of its cases paid a full
# 80.7s compile of the same root; they share one staged executable now, so what
# the corpus pays is six runtime scenarios.
_CLI_TEST_ARGS = [
    "--quiet",
    "--skip", "cli.examples::caldera::main",
    "--skip", "cli.examples::meridian::main",
]

_CLI_TEST_BASE_ENV = {
    "RUE_BINARY": "$(exe_target //crates/rue:rue)",
    "RUE_CLI_CASES": "$(location //crates/rue-cli-tests:cases)/cases",
    "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
    "RUE_REPO_DIR": "$(location :cli-test-fixtures)",
    "RUE_STD_DIR": "$(location :std)/std",
}

# The staged-program directory, carried by every corpus target whose inventory
# can contain a case that names one of the ten roots. //:release-smoke is
# deliberately not one of them: it runs the `differential_opt` filter, whose
# cases compile four times each at runner-driven opt levels and so can never
# consume a staged artifact. Declaring it there would add ten
# release-configured program compiles to a deliberately bounded lane (RUE-1129)
# in exchange for nothing.
_CLI_STAGED_PROGRAMS_ENV = {
    "RUE_CLI_STAGED_PROGRAMS": "$(location :cli-staged-programs)",
}

_CLI_TEST_ENV = dict(
    _CLI_TEST_BASE_ENV.items() +
    _CLI_STAGED_PROGRAMS_ENV.items() + [
        ("RUE_CLI_CASE_TIER", "premerge"),
    ],
)

# Every _CLI_TEST_ENV entry is a path the harness hands to a compiler spawned
# with a case's temp directory as cwd, so all of them must be absolute (see
# corpus.bzl). The harness's find_dir fallbacks would otherwise resolve against
# the action's working directory and silently miss the real corpus. The staged
# directory needs it for the same reason one step later: a case runs its
# prebuilt program from that case's temp directory.
_CLI_TEST_BASE_ABSOLUTIZE = [
    "RUE_BINARY",
    "RUE_CLI_CASES",
    "RUE_EXAMPLES_DIR",
    "RUE_REPO_DIR",
    "RUE_STD_DIR",
]

_CLI_TEST_ABSOLUTIZE = _CLI_TEST_BASE_ABSOLUTIZE + ["RUE_CLI_STAGED_PROGRAMS"]

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
    labels = ["rue_heavy_suite", "rue_ci_dedicated_lane"],
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
    env = dict(
        _CLI_TEST_BASE_ENV.items() +
        _CLI_STAGED_PROGRAMS_ENV.items() + [
            ("RUE_CLI_CASE_TIER", "slow"),
        ],
    ),
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
    env = dict(_CLI_TEST_BASE_ENV.items() + [
        ("RUE_CLI_CASE_TIER", "premerge"),
    ]),
    absolutize = _CLI_TEST_BASE_ABSOLUTIZE,
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
        # executor --env path, so they survive a replayed run instead of
        # vanishing with it. RUE-1222: what a hit replays is still the run that
        # wrote the entry, so the weekly cache-free repetitions are where these
        # are actually re-measured. See cached_corpus_suite's case_timings doc.
        case_timings = True,
    )
    for _shard in range(CLI_TEST_SHARD_COUNT)
]

# ADR-0070 Phase 1 (RUE-1405): each large maintained application compiles
# once as a `rue_program` build action — cached, shared across lanes — and
# every runtime scenario below is its own `rue_program_test` consuming that
# executable. A test execution never reaches the action cache, so the compile
# must not live inside one.
#
# Each sibling pair shares a directory glob (main does not import canary.rue,
# or vice versa): the precision trade ADR-0070's over-declaration audit
# documents. The executor's 600s default (RUE-1156) now bounds only a runtime
# scenario, which finishes in seconds even at stress scale.
rue_program_family(
    name = "large-example-caldera",
    srcs = glob(["examples/caldera/**/*.rue"]),
    programs = {
        "caldera": {"root": "examples/caldera/main.rue"},
        "caldera-canary": {"root": "examples/caldera/canary.rue"},
    },
)

# `:meridian` is the ninth staged CLI program as well as the slow-tier
# large-example root (ADR-0070 Phase 2): one compile, consumed by the six
# scheduled scenarios below AND by the six CLI corpus scenarios of
# cases/examples_meridian.toml.
rue_program_family(
    name = "large-example-meridian",
    srcs = glob(["examples/meridian/**/*.rue"]),
    programs = {
        "meridian": {"root": "examples/meridian/main.rue"},
        "meridian-canary": {"root": "examples/meridian/canary.rue"},
    },
)

# The required pre-merge canaries compile a reduced root from each maintained
# application and execute its core path. They are intentionally honest about
# their scope: neither claims to compile the complete generated graph. Each
# canary executable deliberately has two consuming scenarios — the core-path
# check, and a staged-cwd run — which is the "one compile, many scenarios"
# shape scripts/check-rue-program-warm-cache.sh asserts is cache-served.
[
    rue_program_test(
        name = "large-example-{}-canary".format(_program),
        program = ":{}-canary".format(_program),
    )
    for _program in ["caldera", "meridian"]
]

[
    rue_program_test(
        name = "large-example-{}-canary-workdir".format(_program),
        program = ":{}-canary".format(_program),
        files = [{"path": "data/staged.txt", "source": "staged\n"}],
    )
    for _program in ["caldera", "meridian"]
]

# Scheduled slow coverage: the complete application graphs, one compile each,
# reused by every scenario. Every marker lands on stdout.
_LARGE_EXAMPLE_SLOW_SCENARIOS = {
    "caldera": [
        ("selftest", [], ["selftest checks=23", "valid=true"], {}),
        ("demo", ["demo"], ["entities=8", "valid=true"], {}),
        (
            "scenario",
            ["run", "demo.caldera"],
            ["script_oracle=true", "valid=true"],
            {"demo.caldera": "examples/caldera/demo.caldera"},
        ),
        ("stress1", ["stress1"], ["stress scale=1", "valid=true"], {}),
        ("benchmark", ["benchmark"], ["benchmark tiers=2", "valid=true"], {}),
    ],
    "meridian": [
        ("help", [], ["usage: meridian"], {}),
        ("demo", ["demo"], ["database=meridian", "result_valid=true"], {}),
        (
            "workload",
            ["run", "demo.sql"],
            ["plan_valid=true", "result_valid=true"],
            {"demo.sql": "examples/meridian/demo.sql"},
        ),
        ("selftest", ["selftest"], ["selftest checks=24", "valid=true"], {}),
        ("stress1", ["stress1"], ["stress scale=1", "valid=true"], {}),
        (
            "benchmark",
            ["benchmark"],
            ["benchmark=meridian-complete", "valid=true"],
            {},
        ),
    ],
}

[
    rue_program_test(
        name = "large-example-{}-{}".format(_program, _scenario),
        tier = "slow",
        labels = ["rue_scheduled_large_example"],
        program = ":{}".format(_program),
        program_args = _args,
        stdout_contains = _markers,
        data = _data,
    )
    for _program, _scenarios in _LARGE_EXAMPLE_SLOW_SCENARIOS.items()
    for _scenario, _args, _markers, _data in _scenarios
]

# The release workflow's matrix selects one application per job by these
# names, exactly as it selected the retired monolithic sh_tests; each suite
# fans out to the per-scenario tests above, which share one compiled artifact.
[
    rue_test_suite(
        name = "large-example-{}-slow".format(_program),
        tier = "slow",
        labels = ["rue_scheduled_large_example"],
        tests = [
            ":large-example-{}-{}".format(_program, _scenario)
            for _scenario, _args, _markers, _data in _scenarios
        ],
    )
    for _program, _scenarios in _LARGE_EXAMPLE_SLOW_SCENARIOS.items()
]

# The 4x generated workload is an extreme scaling experiment rather than a
# correctness smoke, so it has explicit stress-tier ownership. It consumes
# the same compiled artifact as the slow tier.
[
    rue_program_test(
        name = "large-example-{}-stress".format(_program),
        tier = "stress",
        labels = ["rue_scheduled_large_example"],
        program = ":{}".format(_program),
        program_args = ["stress4"],
        stdout_contains = ["stress scale=4", "valid=true"],
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

# RUE-1163: the lightweight repository gates a filtered `./test.sh <pattern>`
# run executes after the corpora. This used to be a bash array of four target
# names; as a test_suite the membership is a Buck fact, and adding a gate here
# reaches every caller without editing a script.
rue_test_suite(
    name = "repository-quality-gates",
    tests = [
        ":adr-registry-validation",
        ":spec-traceability",
        ":tutorial-snippet-tests",
        ":tutorial-snippet-tool-tests",
    ],
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
    resources = ["scripts/validate-required-ci-container-pins.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# The structural complement of the per-crate debug-assertion checks
# (RUE-1525): those run only for crates that emit them, so deleting a crate
# would leave its ledger entries permanently unchecked. This gate fails when
# any ledger entry names a crate rust-project.json no longer lists.
rue_sh_test(
    name = "debug-assert-ledger-check",
    test = "scripts/validate-debug-assert-policy.py",
    args = [
        "--ledger-crates",
        "rust-project.json",
    ],
    resources = ["rust-project.json"] + glob(["scripts/gatelib/*.py"]),
)

rue_sh_test(
    name = "debug-assert-policy-tool-tests",
    test = "scripts/test-debug-assert-policy.py",
    resources = ["scripts/validate-debug-assert-policy.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "shell-pipefail-pipeline-tool-tests",
    test = "scripts/test-validate-shell-pipefail-pipelines.py",
    resources = ["scripts/validate-shell-pipefail-pipelines.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "shell-bash-baseline-tool-tests",
    test = "scripts/test-validate-shell-bash-baseline.py",
    resources = ["scripts/validate-shell-bash-baseline.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# The interpreter-floor gate (RUE-1509; floor made a uniform 3.9 by RUE-1524).
# Unlike the Bash baseline's tests, these need no particular interpreter to be
# INSTALLED to mean something, so premerge is enough. They are not thereby
# host-independent, and the fixtures are written to keep the difference small
# and asserted rather than assumed: every fixture is 3.9 syntax, so the scan
# itself answers identically everywhere, and the one case that cannot -- what
# a parse error means -- asserts on both sides of the floor. The scan proper is
# only as strict as the interpreter running it, which is why the authoritative
# run is ci.yml's `fmt` step, at or above the floor.
rue_sh_test(
    name = "python-baseline-tool-tests",
    test = "scripts/test-validate-python-baseline.py",
    resources = [
        "scripts/validate-python-baseline.py",
    ] + glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "release-configuration-tool-tests",
    test = "scripts/test-release-configuration.py",
    resources = ["scripts/validate-release-configuration.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# RUE-1264: the scaling probes' committed sources are generator output, and
# their single-axis property lives in the generators. This gate re-runs each
# generator's `--check` so a hand edit to one size fails the pull request
# rather than silently turning a between-sizes ratio into a comparison of two
# different programs. The probe sources are declared inputs so a source edit
# can never be served a cached pass.
rue_sh_test(
    name = "scale-probe-generator-check",
    test = "scripts/test-scale-probe-generators.py",
    resources = glob([
        "performance/workloads/scale_modules/**",
        "performance/workloads/scale_functions/**",
        "performance/workloads/scale_instantiations/**",
    ]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# Shared plumbing for the validate-* gates (RUE-1522): the Rust masker,
# walker prune policy, workflow job splitter, gate skeleton, and the tool
# tests' script loader. The masker tests are the single place the
# lifetime-vs-char-literal contract is pinned.
rue_sh_test(
    name = "gatelib-tests",
    test = "scripts/test-gatelib.py",
    resources = glob(["scripts/gatelib/*.py"]) + [
        "scripts/validate-adrs.py",
    ],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# Same drift contract for the Caldera capacity corpus: the committed
# examples/caldera is generator output, the generator writes in place, and
# nothing else compares the two (RUE-1521 found a 782-file divergence that
# had accrued silently).
rue_sh_test(
    name = "caldera-generator-check",
    test = "scripts/test-caldera-generator.py",
    resources = ["scripts/generate-caldera.py"] + glob([
        "examples/caldera/**",
    ]),
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
    resources = ["scripts/validate-cli-shard-coverage.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# RUE-1265 / ADR-0069 §2. The duplication gate itself needs the live Buck graph
# and every test binary, so it runs as a step in the premerge lane rather than
# as a target inside the graph it interrogates. What belongs here is its
# decision logic, pinned the way //:cli-shard-coverage-tool-tests pins the
# shard gate: the RUE-1262 superset shape is checked in as a fixture, and the
# gate must fail on it and name both owning targets.
#
# `scripts/affected-targets` is a declared resource because the gate reads its
# corpus and lane inventories rather than keeping a second copy — a lane added
# there must be classified here or the gate refuses to run.
rue_sh_test(
    name = "test-duplication-tool-tests",
    test = "scripts/test-validate-test-duplication.py",
    resources = [
        "scripts/affected-targets",
        "scripts/validate-test-duplication.py",
    ] + glob(["scripts/gatelib/*.py"]),
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
    resources = glob(["scripts/gatelib/*.py"]),
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
    resources = ["scripts/validate-tier-ci-selectors.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_TIER_VALIDATION_ROOT": "$(location :tier-ci-selector-inputs)",
    },
)

rue_sh_test(
    name = "ci-required-results-tool-tests",
    test = "scripts/test-ci-required-results.py",
    resources = ["scripts/ci-required-results.py"] +
        glob(["scripts/gatelib/*.py"]),
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
        # RUE-1163: which corpora own a required-CI job is a BUCK label now, so
        # the gate reads BUCK to prove each labeled corpus is actually run by a
        # platform-corpus entry.
        "--buck",
        "$(location :root-buck-file)/BUCK",
    ],
    resources = [
        "scripts/ci-required-results.py",
        "scripts/run-native-platform-corpus.sh",
        # RUE-1265: NATIVE_CLI_FILTERS is imported from here, so the two gates
        # cannot disagree about which `scripts/rue cli` steps the native lanes
        # run.
        "scripts/validate-test-duplication.py",
    ] + glob(["scripts/gatelib/*.py"]),
)

# RUE-1258: the staleness rule, exercised without a repository or a data
# branch. This gate fails every pull request while the series is stalled and
# has no bypass, so what "stalled" means — and that an empty dashboard is not
# stalled — is load-bearing rather than incidental.
rue_sh_test(
    name = "performance-stall-validator-tool-tests",
    test = "scripts/test-validate-performance-stall.py",
    resources = [
        "scripts/validate-performance-stall.py",
    ] + glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# RUE-1261: the homepage Field Report asserts that this project can be checked
# rather than trusted, so a fabricated figure on it is a counterexample, not a
# placeholder. These tests pin the two properties that failed before: a ratio
# carries both of its sides, and a figure that cannot be computed is absent
# rather than defaulted.
rue_sh_test(
    name = "site-status-tool-tests",
    test = "scripts/test-generate-site-status.py",
    resources = [
        "scripts/generate-site-status.py",
    ] + glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# RUE-1495: the runtime page publishes excerpts of the template ports the
# benchmark actually runs, so the property worth a test is that a declaration
# which has stopped describing its source FAILS rather than rendering something
# else — a wrong excerpt looks exactly like a right one. The last two cases run
# against the real ports in the tree, so the shipped declarations are checked
# here rather than only when the site is built.
rue_sh_test(
    name = "source-excerpt-tool-tests",
    test = "scripts/test-extract-source-excerpts.py",
    resources = [
        "scripts/extract-source-excerpts.py",
    ] + glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# RUE-1194: the §11 tooltip needs a commit's subject and its distance from the
# previous measurement, neither of which a run object carries. The ordinal
# follows trunk's first parents, so these tests pin the one property that makes
# subtracting two of them meaningful: a merged topic branch is not a run of
# skipped trunk commits.
rue_sh_test(
    name = "performance-commit-annotation-tool-tests",
    test = "scripts/test-annotate-performance-commits.py",
    resources = [
        "scripts/annotate-performance-commits.py",
    ] + glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "ci-gate-validator-tool-tests",
    test = "scripts/test-validate-ci-gate.py",
    resources = [
        "scripts/ci-required-results.py",
        "scripts/run-native-platform-corpus.sh",
        "scripts/validate-ci-gate.py",
        "scripts/validate-test-duplication.py",
    ] + glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_CI_WORKFLOW": "$(location :required-ci-workflows)/.github/workflows/ci.yml",
        "RUE_TEST_RUNNER_SOURCE": "$(location //crates/rue-test-runner:platform-responsibility-source)/src/lib.rs",
        "RUE_ROOT_BUCK": "$(location :root-buck-file)/BUCK",
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
    resources = ["scripts/generate-cli-shard-weights.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# JSON twin of the TOML-authored policy input, so the Python gate runs on the
# repository's 3.9 floor without `tomllib` (RUE-1524). The TOML stays the
# authored source of truth next to the CLI cases; this artifact is derived
# every build and carries no independent content.
genrule(
    name = "cli-execution-contracts-json",
    out = "execution_contracts.json",
    cmd = "$(exe //crates/rue-toml2json:rue-toml2json) " +
        "$(location //crates/rue-cli-tests:cases)/cases/execution_contracts.toml > $OUT",
)

# RUE-1530: the packing authority for CLI shard deadlines. The harness's emit
# mode performs the same case discovery the //:cli-tests-shard-* targets
# perform (premerge tier, default all-platform selection) and packs it with
# the same runtime LPT rule, once per platform modeled in shard-weights.json.
# scripts/cli-timeout-policy.py applies its policy arithmetic to these
# reported loads instead of reimplementing the packing; the declared inputs
# (cases, examples, weights, harness) re-derive the report whenever the
# corpus population or its measured costs change.
genrule(
    name = "cli-shard-loads-json",
    out = "shard-loads.json",
    cmd = "env " +
        "RUE_CLI_EMIT_SHARD_LOADS={} ".format(CLI_TEST_SHARD_COUNT) +
        "RUE_CLI_CASE_TIER=premerge " +
        "RUE_CLI_CASES=$(location //crates/rue-cli-tests:cases)/cases " +
        "RUE_EXAMPLES_DIR=$(location :examples)/examples " +
        "RUE_CLI_SHARD_WEIGHTS=$(location //crates/rue-cli-tests:shard-weights) " +
        "$(exe //crates/rue-cli-tests:rue-cli-tests) > $OUT",
)

rue_sh_test(
    name = "cli-timeout-policy-validation",
    test = "scripts/cli-timeout-policy.py",
    args = [
        "--policy",
        "$(location :cli-execution-contracts-json)",
        "--shard-loads",
        "$(location :cli-shard-loads-json)",
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
    resources = ["scripts/cli-timeout-policy.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
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

# RUE-802: the nightly fuzz workflow files crashes into Linear. The reporting
# logic that decides whether a crash is filed once, filed twice, or lost — crash
# fingerprinting, dedup against open issues, payload construction, and the
# GitHub fallback when LINEAR_API_KEY is missing — cannot be exercised in CI
# against the real API, so it is driven through its injected transport with a
# mock. The workflow file is an input as well, so deleting the reporting step
# fails this test instead of quietly restoring the silence.
filegroup(
    name = "fuzz-report-test-inputs",
    srcs = [".github/workflows/fuzz.yml"],
)

rue_sh_test(
    name = "fuzz-report-tool-tests",
    test = "scripts/test-fuzz-report-failure.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_FUZZ_REPORT_ROOT": "$(location :fuzz-report-test-inputs)",
    },
    resources = ["scripts/fuzz-report-failure.py"] +
        glob(["scripts/gatelib/*.py"]),
)

# RUE-1507: the health check that notices a scheduled workflow which has never
# succeeded. It runs in required CI, so both of its ways of being wrong are
# expensive — a false red blocks every pull request, and a false green restores
# the silence that let a weekly safeguard fail for its entire existence. The
# run-history API cannot be reached from a test, so the classifier is driven
# through its injected transport with a mock. The real workflow directory is an
# input as well: discovery is by `schedule:` trigger rather than a list, so a
# parser that stops recognizing how the workflows are actually written would
# otherwise report a clean audit of nothing.
filegroup(
    name = "scheduled-workflow-test-inputs",
    # Matches the script's own `*.y*ml` discovery. A narrower glob here would
    # leave a `.yaml` workflow live in CI but invisible to these tests.
    srcs = glob([".github/workflows/*.yml", ".github/workflows/*.yaml"]),
)

rue_sh_test(
    name = "scheduled-workflow-tool-tests",
    test = "scripts/test-check-scheduled-workflows.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_SCHEDULED_WORKFLOWS_ROOT": "$(location :scheduled-workflow-test-inputs)",
    },
    resources = ["scripts/check-scheduled-workflows.py"] +
        glob(["scripts/gatelib/*.py"]),
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
    resources = glob(["scripts/gatelib/*.py"]),
    args = [
        "--source", "rue-air=$(location //crates/rue-air:rue-air-sources)",
        "--source", "rue-builtins=$(location //crates/rue-builtins:rue-builtins-sources)",
        "--source", "rue-cfg=$(location //crates/rue-cfg:rue-cfg-sources)",
        "--source", "rue-codegen=$(location //crates/rue-codegen:rue-codegen-sources)",
        "--source", "rue-compiler=$(location //crates/rue-compiler:rue-compiler-sources)",
        "--source", "rue-linker=$(location //crates/rue-linker:rue-linker-sources)",
        "--source", "rue-oracle=$(location //crates/rue-oracle:rue-oracle-sources)",
    ],
)

rue_sh_test(
    name = "runtime-abi-inventory-tool-tests",
    test = "scripts/test-runtime-abi-inventory.py",
    resources = ["scripts/validate-runtime-abi-inventory.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "type-architecture-inventory-validation",
    test = "scripts/validate-type-architecture.py",
    resources = glob(["scripts/gatelib/*.py"]),
    args = [
        "--source", "rue-air=$(location //crates/rue-air:rue-air-sources)",
        "--source", "rue-cfg=$(location //crates/rue-cfg:rue-cfg-sources)",
        "--source", "rue-codegen=$(location //crates/rue-codegen:rue-codegen-sources)",
        "--source", "rue-compiler=$(location //crates/rue-compiler:rue-compiler-sources)",
        "--source", "rue-oracle=$(location //crates/rue-oracle:rue-oracle-sources)",
    ],
)

rue_sh_test(
    name = "type-architecture-inventory-tool-tests",
    test = "scripts/test-type-architecture.py",
    resources = ["scripts/validate-type-architecture.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "payload-ownership-inventory-validation",
    test = "scripts/validate-payload-ownership.py",
    resources = glob(["scripts/gatelib/*.py"]),
    args = [
        "--source", "rue-rir=$(location //crates/rue-rir:rue-rir-sources)",
        "--source", "rue-air=$(location //crates/rue-air:rue-air-sources)",
        "--source", "rue-cfg=$(location //crates/rue-cfg:rue-cfg-sources)",
        "--source", "rue-codegen=$(location //crates/rue-codegen:rue-codegen-sources)",
    ],
)

rue_sh_test(
    name = "payload-ownership-inventory-tool-tests",
    test = "scripts/test-payload-ownership.py",
    resources = ["scripts/validate-payload-ownership.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_sh_test(
    name = "body-analysis-capability-inventory-validation",
    test = "scripts/validate-body-analysis-capabilities.py",
    resources = glob(["scripts/gatelib/*.py"]),
    args = [
        "--source", "rue-air=$(location //crates/rue-air:rue-air-sources)",
        "--source", "rue-compiler=$(location //crates/rue-compiler:rue-compiler-sources)",
    ],
)

rue_sh_test(
    name = "body-analysis-capability-inventory-tool-tests",
    test = "scripts/test-body-analysis-capabilities.py",
    resources = ["scripts/validate-body-analysis-capabilities.py"] +
        glob(["scripts/gatelib/*.py"]),
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

rue_test_suite(
    name = "payload-ownership-compile-fail-tests",
    tests = ["//crates/rue-rir:rue-rir[doc]"],
)

# Maintenance scripts with deletion behavior. Their fail-closed contract is
# pinned by scripts/test-cleanup-scripts.sh (RUE-567, RUE-1225), which runs
# copies against fake tools — no real repo, remote, or Buck output is touched.
filegroup(
    name = "cleanup-script-inputs",
    srcs = [
        "scripts/jj-tidy",
        "scripts/rue-storage",
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
# Dict form because crates/clippy-gate.sh lives across the crates/BUCK package
# boundary: the root package cannot name it as a plain source, so its exported
# target stands in, keyed by the package-relative path the tests expect.
filegroup(
    name = "wrapper-script-inputs",
    srcs = {
        "crates/clippy-gate.sh": "//crates:clippy-gate",
        "fmt.sh": "fmt.sh",
        "quick-test.sh": "quick-test.sh",
        "scripts/ci-corpus-inventory": "scripts/ci-corpus-inventory",
        "scripts/ci-heavy-suite": "scripts/ci-heavy-suite",
        "scripts/ci-timed": "scripts/ci-timed",
        "scripts/check-cache-probe": "scripts/check-cache-probe",
        "scripts/rue": "scripts/rue",
        "scripts/rue-bin": "scripts/rue-bin",
        "scripts/rue-storage": "scripts/rue-storage",
        "scripts/provision-build-cache": "scripts/provision-build-cache",
        "scripts/run-sanitizer.sh": "scripts/run-sanitizer.sh",
        "test.sh": "test.sh",
    },
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

# RUE-1404: the derive script owns rue_program's declared-boundary check and
# machine-stable re-anchoring, so its set arithmetic is pinned by unit tests
# independently of any fixture building — the fixtures cannot distinguish
# "boundary enforced" from "boundary accidentally never violated".
filegroup(
    name = "rue-program-derive-inputs",
    srcs = ["scripts/rue-program-derive-manifest.py"],
)

rue_sh_test(
    name = "rue-program-derive-manifest-tests",
    test = "scripts/test-rue-program-derive-manifest.py",
    resources = [":rue-program-derive-inputs"],
)

filegroup(
    name = "build-sharing-test-inputs",
    srcs = [
        "buck2",
        "buck2-bin",
        "scripts/ci-heavy-suite",
        "scripts/provision-build-cache",
        "scripts/rue-storage",
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
