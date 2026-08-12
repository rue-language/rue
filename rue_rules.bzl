"""Rue program compilation as declared Buck actions (ADR-0070 / RUE-1404).

`rue_program` owns exactly one compilation and produces the executable as a
first-class artifact; `rue_program_test` consumes it and runs one runtime
scenario. Many scenarios share one compile. The names deliberately stay
`rue_program`/`rue_program_test` rather than the ecosystem's
`rue_binary`/`rue_test`, so a future published ruleset is free to differ
(ADR-0070, "Forward positioning").

The compile is THREE STATIC ACTIONS — scan, derive, compile — not a
`dynamic_output`: the manifest is consumed by path and every command line is
known at analysis time.

  scan     rue --emit deps ROOT           (no manifest; no -O; no --target —
                                           the read closure is target-invariant,
                                           so one scan serves every flag set)
  derive   scripts/rue-program-derive-manifest.py
                                          (manifest = accepted ∪ absent ∪ std;
                                           FAILS the build when any accepted
                                           read lies outside srcs ∪ std;
                                           entries machine-stable)
  compile  rue ROOT --source-manifest M --target T -ON -o OUT

Hermeticity lives in the derive step, not in a downstream audit: an
out-of-srcs read is an in-band build failure on every build that re-runs the
scan, local and remote. The over-declaration report is advisory only and
attaches as an OPTIONAL ValidationInfo validation (run it with
`--enable-optional-validations srcs-precision`).

Both cacheable actions set allow_cache_upload for the same reason
corpus.bzl's action does: this repository's genrule upload gating is driven by
a Meta-internal label allowlist, and the whole point of the rule is that a PR
run's compile is served to the merge-group run.
"""

load("@prelude//test:inject_test_run_info.bzl", "inject_test_run_info")
load("//:test_defs.bzl", "rue_test_labels")
load("@toolchains//:rue.bzl", "RueToolchainInfo")

RueProgramInfo = provider(fields = [
    # The compiled executable artifact.
    "executable",
    # Project-relative root source path (string).
    "root",
    # The RESOLVED Rue target this executable was compiled for.
    "rue_target",
    # Optimization level the compile used, as the string after -O.
    "opt_level",
    # True when rue_target equals the configured platform's native target.
    # Computed, never asserted (ADR-0070 open question 1).
    "runs_natively",
])

# Mechanism internals, deliberately NOT part of the consumer-facing provider:
# the envelope is machine-unstable bytes in an internal format, and exposing it
# on RueProgramInfo would bake that format into a compatibility contract
# (ADR-0070, "Forward positioning"). The family aggregate below is its one
# intended consumer.
RueProgramInternalInfo = provider(fields = [
    "manifest",
    "deps_envelope",
    "srcs",
])


def _resolve(ctx: AnalysisContext):
    toolchain = ctx.attrs._rue_toolchain[RueToolchainInfo]
    resolved_target = ctx.attrs.rue_target or toolchain.native_target
    opt_level = ctx.attrs.opt_level or toolchain.default_opt_level
    # A filegroup's output directory contains its srcs at package-relative
    # paths, hence the `/std` suffix — the same shape as `$(location :std)/std`
    # in the corpus suites.
    std_dir = cmd_args(toolchain.std, format = "{}/std")
    return toolchain, resolved_target, opt_level, std_dir


def _scan_and_derive(ctx: AnalysisContext, toolchain, std_dir, expect_violation = None):
    """The scan and derive actions, shared by rue_program and the boundary control."""
    envelope = ctx.actions.declare_output("deps-envelope.json")
    scan_cmd = cmd_args(
        ctx.attrs._scan[RunInfo],
        envelope.as_output(),
        toolchain.compiler,
        ctx.attrs.root,
        hidden = [ctx.attrs.srcs, toolchain.std, ctx.attrs.extra_scan_inputs],
    )
    ctx.actions.run(
        scan_cmd,
        env = {"RUE_STD_PATH": std_dir},
        category = "rue_scan",
        identifier = ctx.label.name,
        allow_cache_upload = True,
    )

    srcs_list = ctx.actions.write("srcs.list", ctx.attrs.srcs)
    out_name = "boundary-marker" if expect_violation else "sources.manifest"
    manifest = ctx.actions.declare_output(out_name)
    derive_cmd = cmd_args(
        ctx.attrs._derive[RunInfo],
        "--envelope",
        envelope,
        "--root",
        ctx.attrs.root,
        "--srcs-list",
        srcs_list,
        "--std-dir",
        std_dir,
        "--out",
        manifest.as_output(),
        hidden = [ctx.attrs.srcs, toolchain.std],
    )
    if expect_violation:
        derive_cmd.add("--expect-violation", expect_violation)
    ctx.actions.run(
        derive_cmd,
        category = "rue_derive_manifest",
        identifier = ctx.label.name,
        allow_cache_upload = True,
    )
    return envelope, manifest


def _rue_program_impl(ctx: AnalysisContext) -> list[Provider]:
    toolchain, resolved_target, opt_level, std_dir = _resolve(ctx)
    envelope, manifest = _scan_and_derive(ctx, toolchain, std_dir)

    executable = ctx.actions.declare_output(ctx.label.name)
    compile_cmd = cmd_args(
        toolchain.compiler,
        ctx.attrs.root,
        "--source-manifest",
        manifest,
        "--target",
        resolved_target,
        "-O{}".format(opt_level),
        "-o",
        executable.as_output(),
        hidden = [ctx.attrs.srcs, toolchain.std],
    )
    ctx.actions.run(
        compile_cmd,
        env = {"RUE_STD_PATH": std_dir},
        category = "rue_compile",
        identifier = ctx.label.name,
        allow_cache_upload = True,
    )

    # Advisory precision report: srcs the scan never read. Optional, so it
    # costs nothing on ordinary builds and never fails one; the directory-wide
    # judgement (is an unread file dead, or a sibling's?) belongs to the
    # family aggregate, which is why this per-target report only LISTS extras.
    report = ctx.actions.declare_output("srcs-precision.json")
    ctx.actions.run(
        cmd_args(
            ctx.attrs._precision[RunInfo],
            "--envelope",
            envelope,
            "--root",
            ctx.attrs.root,
            "--srcs-list",
            ctx.actions.write("precision-srcs.list", ctx.attrs.srcs),
            "--out",
            report.as_output(),
        ),
        category = "rue_srcs_precision",
        identifier = ctx.label.name,
    )

    runs_natively = resolved_target == toolchain.native_target
    return [
        DefaultInfo(default_output = executable, other_outputs = [manifest]),
        RunInfo(args = cmd_args(executable)),
        RueProgramInfo(
            executable = executable,
            root = ctx.attrs.root,
            rue_target = resolved_target,
            opt_level = opt_level,
            runs_natively = runs_natively,
        ),
        RueProgramInternalInfo(
            manifest = manifest,
            deps_envelope = envelope,
            srcs = ctx.attrs.srcs,
        ),
        ValidationInfo(validations = [ValidationSpec(
            name = "srcs-precision",
            validation_result = report,
            optional = True,
        )]),
    ]


_PROGRAM_COMMON_ATTRS = {
    # Project-relative path string, not attrs.source(): the compiler resolves
    # the root against its cwd (the project root), and the same string anchors
    # the derive script's re-anchoring. srcs carries the artifact.
    "root": attrs.string(),
    "srcs": attrs.list(attrs.source()),
    "rue_target": attrs.option(attrs.string(), default = None),
    "opt_level": attrs.option(attrs.string(), default = None),
    # Negative-control plumbing: inputs materialized for the scan but NOT part
    # of the declared srcs boundary. Ordinary programs never set this.
    "extra_scan_inputs": attrs.list(attrs.source(), default = []),
    "_derive": attrs.dep(providers = [RunInfo], default = "root//:rue-program-derive-manifest"),
    "_precision": attrs.dep(providers = [RunInfo], default = "root//:rue-program-srcs-precision"),
    "_scan": attrs.dep(providers = [RunInfo], default = "root//:rue-program-scan"),
    "_rue_toolchain": attrs.toolchain_dep(default = "toolchains//:rue"),
}

rue_program = rule(
    impl = _rue_program_impl,
    attrs = _PROGRAM_COMMON_ATTRS,
)


def _rue_program_boundary_control_impl(ctx: AnalysisContext) -> list[Provider]:
    """Negative control 1 (ADR-0070): the derive boundary check, contained.

    Runs scan + derive with the out-of-srcs module materialized as a hidden
    scan input — so the failure stage is the derive boundary in EVERY
    execution environment, not an unresolved import in sandboxed ones — and
    inverts the derive step: the action succeeds, writing a marker, iff the
    boundary check rejects exactly `violating_path`. Building the target is
    the test; the sh_test wrapper only asserts the marker so the control has a
    test surface and a tier.
    """
    toolchain, _resolved_target, _opt_level, std_dir = _resolve(ctx)
    _envelope, marker = _scan_and_derive(
        ctx,
        toolchain,
        std_dir,
        expect_violation = ctx.attrs.violating_path,
    )
    return [DefaultInfo(default_output = marker)]


rue_program_boundary_control = rule(
    impl = _rue_program_boundary_control_impl,
    attrs = _PROGRAM_COMMON_ATTRS | {
        "violating_path": attrs.string(),
    },
)


def _rue_program_test_impl(ctx: AnalysisContext) -> list[Provider]:
    program = ctx.attrs.program[RueProgramInfo]
    spec = ctx.actions.write_json("scenario.json", {
        "exit_code": ctx.attrs.exit_code,
        "files": ctx.attrs.files,
        "program_args": ctx.attrs.program_args,
        "program_env": ctx.attrs.program_env,
        "stderr_contains": ctx.attrs.stderr_contains,
        "stdin": ctx.attrs.stdin,
        "stdout": ctx.attrs.stdout,
        "stdout_contains": ctx.attrs.stdout_contains,
    })
    command = cmd_args(
        ctx.attrs._runner[RunInfo],
        spec,
        program.executable,
    )
    return inject_test_run_info(
        ctx,
        ExternalRunnerTestInfo(
            type = "custom",
            command = [command],
            env = {},
            labels = ctx.attrs.labels,
            contacts = [],
        ),
    ) + [DefaultInfo()]


_rue_program_test = rule(
    impl = _rue_program_test_impl,
    attrs = {
        "program": attrs.dep(providers = [RueProgramInfo]),
        "program_args": attrs.list(attrs.string(), default = []),
        "program_env": attrs.dict(attrs.string(), attrs.string(), default = {}),
        "files": attrs.list(attrs.dict(attrs.string(), attrs.string()), default = []),
        "stdin": attrs.option(attrs.string(), default = None),
        "exit_code": attrs.int(default = 0),
        "stdout": attrs.option(attrs.string(), default = None),
        "stdout_contains": attrs.list(attrs.string(), default = []),
        "stderr_contains": attrs.list(attrs.string(), default = []),
        "labels": attrs.list(attrs.string(), default = []),
        "_runner": attrs.dep(providers = [RunInfo], default = "root//:rue-program-test-runner"),
        "_inject_test_env": attrs.default_only(
            attrs.dep(default = "prelude//test/tools:inject_test_env"),
        ),
    },
)


def rue_program_test(name, tier = "premerge", labels = [], **kwargs):
    """One runtime scenario over a prebuilt rue_program, with tier ownership.

    The rule name keeps the `_test` suffix and exposes `labels` because
    test_tiers.bxl discovers tests by the `^(.*_test|test_suite)$` kind regex
    and reads the labels attr — both conditions are load-bearing (ADR-0070).
    """
    _rue_program_test(
        name = name,
        labels = rue_test_labels(tier, labels),
        **kwargs
    )


def _rue_program_family_report_impl(ctx: AnalysisContext) -> list[Provider]:
    """Directory-scoped over-declaration report (ADR-0070, Steve's pass 2).

    A per-target validation sees one program's envelope and would report a
    sibling root's tree as false positives; this aggregate unions the
    accepted-read sets of every sibling sharing the glob and reports files
    that NO sibling reads. Its deps are exactly the macro call's programs —
    ownership is the argument list, because a rule cannot discover its
    siblings.
    """
    report = ctx.actions.declare_output("family-srcs-report.json")
    cmd = cmd_args(ctx.attrs._family_report[RunInfo], "--out", report.as_output())
    for program in ctx.attrs.programs:
        internal = program[RueProgramInternalInfo]
        cmd.add("--envelope", internal.deps_envelope)
        srcs_list = ctx.actions.write(
            "family-srcs-{}.list".format(program.label.name),
            internal.srcs,
        )
        cmd.add("--srcs-list", srcs_list)
        cmd.add("--root", program[RueProgramInfo].root)
    ctx.actions.run(cmd, category = "rue_family_srcs_report", identifier = ctx.label.name)
    return [
        DefaultInfo(default_output = report),
        ValidationInfo(validations = [ValidationSpec(
            name = "family-srcs-report",
            validation_result = report,
            optional = True,
        )]),
    ]


rue_program_family_report = rule(
    impl = _rue_program_family_report_impl,
    attrs = {
        "programs": attrs.list(attrs.dep(providers = [RueProgramInfo])),
        "_family_report": attrs.dep(providers = [RunInfo], default = "root//:rue-program-family-report"),
    },
)


def rue_program_family(name, programs, **kwargs):
    """Declare sibling rue_programs sharing one directory glob, plus their
    `<name>-srcs-report` aggregate. `programs` maps program name -> kwargs for
    rue_program; shared attrs (srcs, rue_target, ...) come from **kwargs."""
    for program_name, program_kwargs in programs.items():
        rue_program(
            name = program_name,
            **(kwargs | program_kwargs)
        )
    rue_program_family_report(
        name = name + "-srcs-report",
        programs = [":" + program_name for program_name in programs],
    )
