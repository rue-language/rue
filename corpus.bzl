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

def cached_corpus_suite(
        name,
        harness,
        args = [],
        env = {},
        absolutize = [],
        labels = [],
        timeout_seconds = None):
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
        timeout_seconds: outer bound replacing the test executor's timeout, which
            a build action does not get. The per-case budgets in
            execution_contracts.toml remain the honest gates.
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
    )

    native.sh_test(
        name = name,
        labels = labels,
        test = "//:corpus-stamp-check",
        args = ["$(location :{}-action)".format(name)],
    )

def _corpus_action_impl(ctx: AnalysisContext) -> list[Provider]:
    stamp = ctx.actions.declare_output("stamp.txt")

    action_env = dict(ctx.attrs.corpus_env.items())
    action_env["RUE_CORPUS_ABSOLUTIZE"] = " ".join(ctx.attrs.absolutize)
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

    return [DefaultInfo(default_output = stamp)]

_corpus_action = rule(
    impl = _corpus_action_impl,
    attrs = {
        # `attrs.arg` expands $(location ...) / $(exe_target ...) in the values
        # and registers what they name as inputs of the action — which is what
        # keys the cache entry, and what the input contract above is about.
        "absolutize": attrs.list(attrs.string(), default = []),
        "corpus_env": attrs.dict(attrs.string(), attrs.arg(), default = {}),
        "harness": attrs.dep(providers = [RunInfo]),
        "harness_args": attrs.list(attrs.string(), default = []),
        "timeout_seconds": attrs.option(attrs.int(), default = None),
        "wrapper": attrs.dep(providers = [RunInfo], default = "//:corpus-action"),
    },
)
