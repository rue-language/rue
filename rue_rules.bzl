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

`rue_test` is the third rule and the odd one out: it produces no executable and
runs no scenario, it runs the compiler's OWN `rue test` subcommand over a
declared closure (ADR-0083 / RUE-2004). It reuses the scan and derive actions
for the same boundary reason `rue_program` does — with the manifest widened by
`--include-srcs`, since a run reads its candidate inventory under that policy —
and it is the build-side producer of that `--test-candidates` inventory, the
one input ADR-0083's boundary gives the build system.

`rue_program_test` is not the only consumer. `rue_program_staging` collects
programs into one directory keyed by root path, which the CLI corpus actions
declare as an ordinary input so their harness runs prebuilt executables
instead of compiling a case's root (ADR-0070 Phase 2 / RUE-1406).
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
    # The declared candidate inventory `rue test` reports against (ADR-0083).
    # It is a LIST of the target's declared srcs, not a read policy: the
    # compile action never receives it, so it cannot become a second
    # undeclared-read route past the derive boundary above.
    "test_candidates",
])


# The cache probe's positive warm-cache control asserts that its FIRST build
# executes these actions cold. The nonce reaches Rust actions as an otherwise
# unused `--cfg` (toolchains/rust/BUCK, RUE-1034), but that re-keys only the
# Rust actions: an unused cfg does not change the compiler's emitted bytes, and
# the actions below are keyed on the compiler ARTIFACT, so they kept being
# served from the shared cache and the probe failed its own cold-namespace
# assertion the first time it ran. Carrying the nonce into these actions re-keys
# them directly. Ordinary builds leave the setting empty and keep their existing
# action keys — `_probe_env` returns its argument untouched, and an action that
# declares no env still declares none.
_CACHE_PROBE_NONCE = read_root_config("rue", "cache_probe_nonce", "")


def _probe_env(env = {}):
    if not _CACHE_PROBE_NONCE:
        return env
    tagged = dict(env)
    tagged["RUE_CACHE_PROBE_NONCE"] = _CACHE_PROBE_NONCE
    return tagged


def _resolve(ctx: AnalysisContext):
    toolchain = ctx.attrs._rue_toolchain[RueToolchainInfo]
    resolved_target = ctx.attrs.rue_target or toolchain.native_target
    opt_level = ctx.attrs.opt_level or toolchain.default_opt_level
    # //std:std is declared in the std package, so its output directory is
    # the std root itself, the same artifact `$(location //std:std)` names in
    # the corpus suites.
    std_dir = toolchain.std
    return toolchain, resolved_target, opt_level, std_dir


def _scan_and_derive(
        ctx: AnalysisContext,
        toolchain,
        std_dir,
        expect_violation = None,
        include_srcs = False):
    """The scan and derive actions, shared by rue_program, rue_test, and the
    boundary control."""
    envelope = ctx.actions.declare_output("deps-envelope.json")
    scan_cmd = cmd_args(
        ctx.attrs._scan[RunInfo],
        envelope.as_output(),
        ctx.attrs.root,
        toolchain.compiler,
        # extra_scan_inputs exists only on the boundary-control rule; ordinary
        # rue_programs have no way to smuggle undeclared-but-materialized
        # files into the scan.
        hidden = [ctx.attrs.srcs, toolchain.std, getattr(ctx.attrs, "extra_scan_inputs", [])],
    )
    ctx.actions.run(
        scan_cmd,
        env = _probe_env({"RUE_STD_PATH": std_dir}),
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
        # The envelope embeds per-file content fingerprints, so srcs changes
        # already re-key this action transitively; only the std tree is read
        # directly (the unconditional union walks it).
        hidden = [toolchain.std],
    )
    if include_srcs:
        derive_cmd.add("--include-srcs")
    if expect_violation:
        derive_cmd.add("--expect-violation", expect_violation)
    ctx.actions.run(
        derive_cmd,
        env = _probe_env(),
        category = "rue_derive_manifest",
        identifier = ctx.label.name,
        allow_cache_upload = True,
    )
    return envelope, manifest


def _test_candidate_list(ctx: AnalysisContext):
    """The declared candidate inventory, spelled the way `rue test` reads it.

    TWO DIFFERENT PROJECT ROOTS MEET HERE, which is the whole reason this is a
    function rather than one `ctx.actions.write` of `srcs`. Buck's project root
    is the repository root, and that is what `root` and every `srcs` path are
    spelled against everywhere else in this file. The compiler's is the ROOT
    MODULE'S DIRECTORY — `source_loader.rs` builds the discovery context from
    the root file's parent — so `rue test examples/gazette/main.rue` resolves
    each `--test-candidates` entry under `examples/gazette/`.

    Handing it repository-relative paths instead is not a loud error: every
    candidate resolves to a file that does not exist, an absent candidate is
    not an orphan, and the warning the inventory exists to power silently
    reports nothing. So entries are re-anchored on the root's directory here,
    once, for both rules.

    A source artifact's `short_path` is package-relative, so its repository
    path is the package plus that — which assumes `srcs` are this package's own
    files. A src from elsewhere fails below rather than being re-anchored on a
    prefix that was never its own.
    """
    root_dir = ctx.attrs.root.rsplit("/", 1)[0] if "/" in ctx.attrs.root else ""
    prefix = root_dir + "/" if root_dir else ""
    package = ctx.label.package
    candidates = []
    for src in ctx.attrs.srcs:
        project_path = package + "/" + src.short_path if package else src.short_path
        if not project_path.startswith(prefix):
            fail("{}: src '{}' lies outside the root's directory '{}', so it has no spelling in the test-candidate inventory; declare srcs from the package that owns the root".format(
                ctx.label.name,
                project_path,
                root_dir,
            ))
        candidates.append(project_path[len(prefix):])
    return ctx.actions.write("test-candidates.list", candidates)


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
        # Pinned (ADR-0070 key table): the internal linker is in-process on
        # every supported target (ELF and Mach-O), while an external linker
        # would execute an undeclared $PATH binary. Cases that exist to test
        # external linkers stay harness cases.
        "--linker",
        "internal",
        "-o",
        executable.as_output(),
        hidden = [ctx.attrs.srcs, toolchain.std],
    )
    for feature in ctx.attrs.preview_features:
        compile_cmd.add("--preview", feature)
    for archive in ctx.attrs.link_archives:
        # The archive BYTES are read at link time, so the flag carries the
        # artifact itself — a bare path string would key the path, not the
        # contents (ADR-0070 key table).
        compile_cmd.add("--link-archive", archive)
    ctx.actions.run(
        compile_cmd,
        env = _probe_env({"RUE_STD_PATH": std_dir}),
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

    # The declared candidate inventory (ADR-0083 §1). It is deliberately NOT a
    # compile input: the compile action's read policy is `sources.manifest`,
    # derived from what the scan actually read, and handing the compiler a
    # second file of paths it may read would reopen the out-of-srcs hole the
    # derive step exists to close. Consumers that want the inventory — the
    # `rue test` driver mode — take it from the provider and pass it themselves.
    #
    # One path per line, anchored on the ROOT'S DIRECTORY rather than on Buck's
    # project root, which is what `--test-candidates` parses; see
    # `_test_candidate_list`. `rue_test` writes the identical file.
    test_candidates = _test_candidate_list(ctx)

    runs_natively = resolved_target == toolchain.native_target
    return [
        DefaultInfo(default_output = executable, other_outputs = [manifest, test_candidates]),
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
            test_candidates = test_candidates,
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
    "preview_features": attrs.list(attrs.string(), default = []),
    "link_archives": attrs.list(attrs.source(), default = []),
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
        # Inputs materialized for the scan but NOT part of the declared srcs
        # boundary — the control's whole trick, and deliberately not an
        # attribute ordinary rue_programs carry.
        "extra_scan_inputs": attrs.list(attrs.source(), default = []),
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
    for staged_path, src in ctx.attrs.data.items():
        # Checked-in runtime fixtures, staged into the writable working
        # directory at a declared relative path so scenarios reference them
        # exactly as a user would from their own cwd. Inline fixtures use
        # `files`; `data` is for repository files a scenario consumes.
        command.add(cmd_args(src, format = staged_path + "={}"))
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
        "data": attrs.dict(attrs.string(), attrs.source(), default = {}),
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


def rue_program_test(name, tier = "premerge", platform = None, labels = [], **kwargs):
    """One runtime scenario over a prebuilt rue_program, with tier ownership.

    The rule name keeps the `_test` suffix and exposes `labels` because
    test_tiers.bxl discovers tests by the `^(.*_test|test_suite)$` kind regex
    and reads the labels attr — both conditions are load-bearing (ADR-0070).
    """
    _rue_program_test(
        name = name,
        labels = rue_test_labels(tier, platform, labels),
        **kwargs
    )


def _rue_test_impl(ctx: AnalysisContext) -> list[Provider]:
    """The `rue test` driver over one declared closure (ADR-0083 / RUE-2004).

    This is the build side of ADR-0083's boundary, and it is exactly one input
    wide: the declared candidate inventory, written from `srcs` in the same
    shape `rue_program` writes it. Everything else — discovery, the test image,
    execution, the event stream — is the compiler's.

    The closure is bounded the way a `rue_program` compile is, by the SAME
    scan and derive actions: a test image free to read outside `srcs` would be
    a hermeticity hole the program build does not have, and the derive step
    fails the build on exactly that.

    The two files the run receives are not interchangeable, which is why they
    are written separately. `sources.manifest` is a READ POLICY — what may be
    imported — and `test-candidates.list` is an INVENTORY: what this target
    owns, orphans included, spelled relative to the root's directory. The
    manifest is a superset by construction here, because a candidate is
    observed under the policy; it is still the policy that decides what the
    image may read.
    """
    toolchain = ctx.attrs._rue_toolchain[RueToolchainInfo]
    std_dir = toolchain.std

    # `--include-srcs` because a candidate is observed UNDER the read policy:
    # an orphan test file is by definition not an accepted read, so a manifest
    # of accepted reads alone would make every orphan "could not be parsed"
    # with no test count — one warning for an unimported file and a corrupt
    # one. The declared boundary is unchanged: the derive step still fails the
    # build on a read outside srcs ∪ std, and srcs are this target's declared,
    # keyed inputs either way.
    _envelope, manifest = _scan_and_derive(ctx, toolchain, std_dir, include_srcs = True)

    # The inventory, byte-identical to what `rue_program` puts on
    # RueProgramInternalInfo for the same root and srcs.
    candidates = _test_candidate_list(ctx)

    command = cmd_args(
        ctx.attrs._runner[RunInfo],
        # The compiler reads the roots and their imports from their real
        # project paths, so srcs and std must be materialized even though the
        # command line names only the root, the manifest, and the inventory.
        hidden = [ctx.attrs.srcs, toolchain.std],
    )
    if ctx.attrs.allow_unimported:
        command.add("--allow-unimported")
    for path in ctx.attrs.expect_unimported:
        command.add("--expect-unimported", path)

    # Everything after `--` is the compiler argv the supervisor runs, assembled
    # here so the whole command is a fact of the rule rather than of a script.
    # `--seed 1` because a suite that shuffles differently every run reports a
    # different failure order for the same tree; rerunning under another seed
    # is a deliberate act, not the default.
    command.add("--")
    command.add(toolchain.compiler)
    command.add("test", ctx.attrs.root)
    command.add("--source-manifest", manifest)
    command.add("--test-candidates", candidates)
    command.add("--seed", "1")
    command.add("--format", "json")
    for feature in ctx.attrs.preview_features:
        command.add("--preview", feature)

    return inject_test_run_info(
        ctx,
        ExternalRunnerTestInfo(
            type = "custom",
            command = [command],
            env = {"RUE_STD_PATH": cmd_args(std_dir)},
            labels = ctx.attrs.labels,
            contacts = [],
        ),
    ) + [DefaultInfo(default_output = candidates, other_outputs = [manifest])]


_rue_test = rule(
    impl = _rue_test_impl,
    attrs = {
        # `root` and `srcs` carry the same meaning as on rue_program: a
        # project-relative path string for the root the compiler resolves, and
        # the declared file set. Here srcs is also the candidate inventory.
        "root": attrs.string(),
        "srcs": attrs.list(attrs.source()),
        "preview_features": attrs.list(attrs.string(), default = []),
        # `rue test` exits 0 with the orphan warning on stderr, so a rule that
        # only forwarded the exit code would let an unimported `*_tests.rue`
        # stay invisible — the whole reason to declare the inventory. The
        # target fails on a non-empty report unless this says it is intended.
        "allow_unimported": attrs.bool(default = False),
        # Negative control, the way `--expect-violation` inverts the derive
        # step's boundary check (fixtures/rue-program/BUCK): the target passes
        # if and only if the run WOULD have been failed, for exactly these
        # paths. Not for production targets.
        "expect_unimported": attrs.list(attrs.string(), default = []),
        "labels": attrs.list(attrs.string(), default = []),
        "_derive": attrs.dep(providers = [RunInfo], default = "root//:rue-program-derive-manifest"),
        "_runner": attrs.dep(providers = [RunInfo], default = "root//:rue-test-supervisor"),
        "_scan": attrs.dep(providers = [RunInfo], default = "root//:rue-program-scan"),
        "_rue_toolchain": attrs.toolchain_dep(default = "toolchains//:rue"),
        "_inject_test_env": attrs.default_only(
            attrs.dep(default = "prelude//test/tools:inject_test_env"),
        ),
    },
)


def rue_test(name, tier = "premerge", platform = None, labels = [], **kwargs):
    """One `rue test` run over a declared closure, with tier ownership.

    Like rue_program_test, the rule name keeps the `_test` suffix and exposes
    `labels` so test_tiers.bxl's `^(.*_test|test_suite)$` kind regex and its
    label read both find it (ADR-0070). The name is the language subcommand's,
    not the ecosystem's `rue_binary`/`rue_test` pair ADR-0070 declined: this
    rule runs `rue test`, and a runtime scenario over one prebuilt executable
    is still `rue_program_test`.
    """
    _rue_test(
        name = name,
        labels = rue_test_labels(tier, platform, labels),
        **kwargs
    )


# Name of the inventory file inside a staging directory. Shared with the CLI
# harness, which fails a run when a root listed here has no case that consumes
# it (crates/rue-cli-tests/src/main.rs, STAGED_ROOTS_MANIFEST).
_STAGED_ROOTS_MANIFEST = "staged-roots.txt"


def _rue_program_staging_impl(ctx: AnalysisContext) -> list[Provider]:
    """One directory of prebuilt executables, keyed by each program's root.

    ADR-0070 Phase 2 (RUE-1406): the CLI corpus harness runs the prebuilt
    executable a case names instead of compiling that case's root. The
    artifacts reach the harness the way every other corpus input does — one
    `$(location ...)` in the suite's `attrs.arg()` env — so each staged
    executable is a declared input of every consuming corpus action and an
    edit to any root re-keys them all.

    The key IS the case's `source_path` string, so neither side mangles names:
    `source_path = "examples/mosaic/main.rue"` in the TOML is
    `<dir>/examples/mosaic/main.rue` here. A symlinked dir rather than a copied
    one — the executables are large and the corpus reads them, never writes.
    """
    staged = {}
    for program in ctx.attrs.programs:
        info = program[RueProgramInfo]
        if info.root in staged:
            fail("rue_program_staging({}): two programs stage the root '{}'".format(
                ctx.label.name,
                info.root,
            ))
        if not info.runs_natively:
            # A cross-compiled executable cannot be run by the scenario that
            # names it, and a case that wants one is a cross-target case that
            # stays compile-in-harness by design.
            fail("rue_program_staging({}): '{}' compiles for {}, which this platform cannot run".format(
                ctx.label.name,
                info.root,
                info.rue_target,
            ))
        staged[info.root] = info.executable

    # The directory declares its own inventory. Without it the harness can only
    # ask "is there a file at this case's source_path", and the answer NO is
    # ambiguous — it is the deliberate exclusion for a root no rue_program
    # owns, and it is also what key drift looks like. The manifest separates
    # the two: a root listed here MUST be consumed, so a case edit or a staging
    # key that stops matching fails the run instead of quietly compiling.
    staged[_STAGED_ROOTS_MANIFEST] = ctx.actions.write(
        _STAGED_ROOTS_MANIFEST,
        sorted(staged.keys()),
    )
    return [DefaultInfo(
        default_output = ctx.actions.symlinked_dir("staged-programs", staged),
    )]


rue_program_staging = rule(
    impl = _rue_program_staging_impl,
    attrs = {
        "programs": attrs.list(attrs.dep(providers = [RueProgramInfo])),
    },
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
