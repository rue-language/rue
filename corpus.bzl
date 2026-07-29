"""Corpus suites that run as cacheable build actions (RUE-1118).

buck2 re-executes every `buck2 test` invocation. Test executions are not
actions: they are handed to the test executor, they never appear in buck2's
`Commands: N (cached: ...)` accounting, and OSS buck2 ships no test-result
cache. Measured on three CI runs of one byte-identical tree (two `pull_request`,
one `merge_group`), build actions converged to 465/465 cache hits while the
corpora re-ran in full every time, at 91-97% of the merge queue's critical path.

`cached_corpus_suite` splits each corpus in two:

  * a `genrule` that runs the harness and writes a stamp on success. This is an
    ordinary action, keyed on its declared inputs, so a PR run uploads it and
    the merge_group run reads it back.
  * a thin `sh_test` that asserts the stamp exists. It keeps the target's name,
    labels, and `Pass: root//:NAME (time)` result line, so `scripts/ci-heavy-suite`
    and test.sh's RUE-924 corpus-omission audit keep working unchanged.

THE INPUT CONTRACT IS NOW LOAD-BEARING. Under a plain `sh_test` an undeclared
input was merely untracked — the suite re-ran regardless. Here an undeclared
input is a false pass: change a file the action does not name, and the corpus
reports success against the previous tree's result. Every path a harness reads
at runtime MUST reach it through `env` (or `args`), and therefore through
`$(location ...)`. When adding a corpus input, add it here, not just to the
harness.
"""

load("//:test_defs.bzl", "rue_test_labels")

def cached_corpus_suite(
        name,
        harness,
        args = [],
        env = {},
        absolutize = [],
        labels = [],
        tier = "premerge",
        timeout_seconds = None,
        case_timings = False):
    """Define a corpus suite whose expensive run is a cacheable build action.

    Args:
        name: the test target's name. The action becomes `NAME-action`.
        harness: the corpus harness binary target.
        args: arguments passed to the harness.
        env: environment for the harness, as on the equivalent `sh_test`.
        absolutize: the `env` keys holding paths. Buck expands `$(location ...)`
            inside a genrule relative to the action's working directory, which is
            not the project root, and the harnesses spawn the compiler with each
            case's temp directory as cwd. `scripts/corpus-action` resolves these
            to absolute paths immediately before the harness runs.
        labels: labels for the test target. The action carries none: it is not a
            test, and `rue_heavy_suite` drives test discovery.
        tier: execution-tier ownership (RUE-1157), validated and applied by
            `rue_test_labels`. It belongs on the test target rather than the
            action for the same reason `labels` does: tier selection is how CI
            and the local wrappers discover *tests*, and the action is not one.
        timeout_seconds: outer bound replacing the test executor's timeout, which
            a build action does not get. The per-case budgets in
            execution_contracts.toml remain the honest gates.
        case_timings: emit RUE-1158's per-case measurements as a declared
            *output* of the action, exposed as the `[timings]` sub-target. It
            must be an output rather than an `--env` path: the harness now runs
            inside the action, where a test-executor `--env` never reaches it,
            and a per-run mktemp path would change the action's digest on every
            run and defeat the caching this rule exists for. As an output it is
            stored with the stamp and materialized on a cache hit, so
            shard-weights.json keeps refreshing on replayed runs rather than
            only when a corpus actually executes.
    """
    for key in absolutize:
        if key not in env:
            fail("cached_corpus_suite({}): absolutize names '{}', which is not in env".format(name, key))

    _corpus_action(
        name = name + "-action",
        harness = harness,
        harness_args = args,
        corpus_env = env,
        absolutize = absolutize,
        timeout_seconds = timeout_seconds,
        case_timings = case_timings,
    )

    native.sh_test(
        name = name,
        labels = rue_test_labels(tier, labels),
        test = "//:corpus-stamp-check",
        args = ["$(location :{}-action)".format(name)],
    )

def _corpus_action_impl(ctx: AnalysisContext) -> list[Provider]:
    stamp = ctx.actions.declare_output("stamp.txt")

    action_env = dict(ctx.attrs.corpus_env.items())
    absolutize = list(ctx.attrs.absolutize)

    # RUE-1158's per-case measurements. This is an action *output*, not an
    # `--env` path: the harness runs inside the action now, so a test-executor
    # --env never reaches it, and the old per-run mktemp/RUNNER_TEMP path would
    # change the action's digest on every run and defeat the caching entirely.
    # Declared as an output it travels with the stamp in the cache entry, so a
    # replayed merge_group run materializes the timings the PR run measured and
    # shard-weights.json keeps refreshing on cached runs.
    #
    # The path is derived from the target name, so it is stable across runs and
    # digest-neutral. It is absolutized for the same reason every other corpus
    # path is: the harness File::create()s it after the compiler has already
    # moved cwd into a case temp directory.
    timings = None
    extra_outputs = []
    if ctx.attrs.case_timings:
        timings = ctx.actions.declare_output("case-timings.jsonl")
        action_env["RUE_CLI_CASE_TIMINGS"] = cmd_args(timings.as_output())
        absolutize.append("RUE_CLI_CASE_TIMINGS")
        extra_outputs.append(timings)

    action_env["RUE_CORPUS_ABSOLUTIZE"] = " ".join(absolutize)
    if ctx.attrs.timeout_seconds != None:
        action_env["RUE_CORPUS_TIMEOUT_SECONDS"] = str(ctx.attrs.timeout_seconds)

    ctx.actions.run(
        cmd_args(
            ctx.attrs.wrapper[RunInfo],
            stamp.as_output(),
            ctx.attrs.harness[RunInfo],
            ctx.attrs.harness_args,
        ),
        env = action_env,
        category = "rue_corpus",
        identifier = ctx.label.name,
        # The corpus spawns the real compiler and links native binaries, so it
        # belongs on the lane's own platform; this matches the repository's
        # ordinary --prefer-local policy. It does not affect cache *reads* —
        # lookup still happens before a miss executes.
        local_only = True,
        # RUE-1163: a corpus harness runs its own cases in parallel and spawns
        # the compiler per case, so one of them can saturate the machine. Say so
        # here and buck2's scheduler takes it from there — it will not start a
        # second corpus alongside this one, but it will still overlap a corpus
        # with unit tests and rustc compiles.
        #
        # This replaced a bash loop in test.sh that ran the corpora one at a time
        # after the broad pass. That loop could only serialize what it knew
        # about, only for callers that went through test.sh, and it serialized
        # corpora against everything rather than against each other. Measured on
        # this pair (oracle-diff-generated-smoke ~7s, reproducible-programs
        # ~67s): 66.8s with no weight, both executing at once; 74.2s with the
        # weight, one at a time.
        weight_percentage = 100,
        # THE POINT OF THIS RULE. `genrule` computes
        #     cacheable = attrs.cacheable and (local_only or prefer_local)
        # and passes it as allow_cache_upload, where local_only/prefer_local are
        # driven purely by a Meta-internal label allowlist (uses_sudo, qt_moc,
        # yarn_install, ...). A plain genrule therefore never uploads its result
        # in this repository: the first merge_group run of RUE-1118 re-executed
        # every corpus on a tree byte-identical to the one the PR run had just
        # built, because nothing had ever been written to the cache. Stating the
        # intent here is both honest and immune to a prelude bump reorganizing
        # that label list.
        allow_cache_upload = True,
    )

    sub_targets = {}
    if timings != None:
        sub_targets["timings"] = [DefaultInfo(default_output = timings)]

    # The stamp stays the default output: `//:NAME`'s sh_test asserts it, and a
    # corpus's result is the pass, not the measurement.
    return [DefaultInfo(
        default_output = stamp,
        other_outputs = extra_outputs,
        sub_targets = sub_targets,
    )]

_corpus_action = rule(
    impl = _corpus_action_impl,
    attrs = {
        # `attrs.arg` expands $(location ...) / $(exe_target ...) in the values
        # and registers what they name as inputs of the action — which is what
        # keys the cache entry, and what the input contract above is about.
        "absolutize": attrs.list(attrs.string(), default = []),
        "case_timings": attrs.bool(default = False),
        "corpus_env": attrs.dict(attrs.string(), attrs.arg(), default = {}),
        "harness": attrs.dep(providers = [RunInfo]),
        "harness_args": attrs.list(attrs.string(), default = []),
        "timeout_seconds": attrs.option(attrs.int(), default = None),
        "wrapper": attrs.dep(providers = [RunInfo], default = "//:corpus-action"),
    },
)
