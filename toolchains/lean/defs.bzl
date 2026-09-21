"""Hermetic Lean toolchain for the mechanized formal core (ADR-0097).

Lean is distributed as one relocatable tree per platform: `bin/` (lean, lake,
leanc, leanchecker), `lib/lean/` (the compiled core library), `include/`, and
`share/`. The tree is what `elan` would install; fetching it through
`toolchain_distribution` makes it an ordinary SHA-pinned build input instead
of a user-local install, and keeps it out of the remote CAS (RUE-2003): one
unpacked toolchain is about 17,500 files and 2.7 GB, eight times the Zig
distribution by bytes.

The pinned version must agree with `docs/formal/lean/lean-toolchain`, the file
`lake` and the editor extension read; `scripts/validate-lean-toolchain-pin.py`
holds the two together.
"""

load("@toolchains//:distribution.bzl", "toolchain_distribution")

LEAN_VERSION = "4.33.1"

_RELEASE_BASE = "https://github.com/leanprover/lean4/releases/download/v{v}/lean-{v}-{p}.tar.zst"

# SHA256 digests as published on the GitHub release (asset `digest` field),
# 2026-09-20.
LEAN_RELEASES = {
    "linux": struct(
        sha256 = "890afd185370f85666025b883914ab4f4b339136f8c96167b69cfb62aecaf235",
    ),
    "linux_aarch64": struct(
        sha256 = "f7353a8b2a8741c84558523e450556f9a1c45e3cafcf54399ce68c6a24c55f07",
    ),
    "darwin": struct(
        sha256 = "93c475c1600360df35471bf6ed1c7fe118d7fb42be6915ead67724f7ad58dfaf",
    ),
    "darwin_aarch64": struct(
        sha256 = "88c45aad985b5d2a8d925fe10bd1296bd35f66f408480ab182d3facccd065a9d",
    ),
}

def lean_host_archive(name: str, platform: str):
    """Declare one official, SHA-pinned Lean host distribution.

    `platform` is Lean's own release suffix (`linux`, `linux_aarch64`,
    `darwin`, `darwin_aarch64`); the archive's top-level directory repeats it.
    """
    release = LEAN_RELEASES[platform]
    toolchain_distribution(
        name = "dist-{}".format(name),
        url = _RELEASE_BASE.format(v = LEAN_VERSION, p = platform),
        sha256 = release.sha256,
        strip_prefix = "lean-{}-{}".format(LEAN_VERSION, platform),
        compression = "zstd",
        visibility = ["toolchains//:lean-distribution"],
    )

def _lean_package_impl(ctx: AnalysisContext) -> list[Provider]:
    """Build a Lake package against the hermetic toolchain and report on it.

    The package sources are copied into the action's scratch space, because
    `lake build` writes `.lake/` beside them and an action must not write to
    its inputs. The outputs are reports, not build products: a stamp, the
    `#print axioms` listing for the theorems `trust` names, the toolchain's
    own `leanchecker` re-check of the compiled modules, and whatever
    `corpus_exe` and `report_exes` print. The action fails if any listed
    theorem depends on an axiom outside `allowed_axioms`, so the trust
    boundary of the mechanization is a build error, not a note.
    """
    distribution = ctx.attrs.toolchain[DefaultInfo].default_outputs[0]
    sources = ctx.attrs.srcs[DefaultInfo].default_outputs[0]
    output = ctx.actions.declare_output(ctx.label.name, dir = True)
    trust = ctx.actions.write(
        "Trust.lean",
        ["import " + ctx.attrs.module] + [
            "#print axioms " + theorem
            for theorem in ctx.attrs.trust
        ],
    )
    allowed = " ".join(ctx.attrs.allowed_axioms)
    corpus_exe = ctx.attrs.corpus_exe or ""
    extra_exes = " ".join(ctx.attrs.extra_exes)

    # Reports the package generates about itself (the statement digest and the
    # trust report, RUE-2247): built, run, and their stdout captured beside
    # `corpus.json`. A report that exits non-zero fails the action, so the
    # output directory is not produced at all; the report's own stderr is what
    # says which check failed, and rerunning the exe by hand reproduces it.
    report_lines = []
    for report, argv in ctx.attrs.report_exes.items():
        if not argv:
            fail("lean_package: report_exes[\"{}\"] is empty; give the exe name first".format(report))
        report_lines.append('lake build "{}" >> "$out/build.log" 2>&1'.format(argv[0]))
        report_lines.append('lake exe {} > "$out/{}"'.format(
            " ".join(['"{}"'.format(arg) for arg in argv]),
            report,
        ))

    script = ctx.actions.write(
        "lean-package.sh",
        [
            "#!/bin/sh",
            "set -eu",
            # $1 distribution, $2 package sources, $3 Trust.lean, $4 output
            # directory, $5 root module; all project-relative, so anchor them
            # before changing directory.
            'root="$PWD"',
            'dist="$root/$1"; src="$root/$2"; trust="$root/$3"; out="$root/$4"; module="$5"',
            'export PATH="$dist/bin:$PATH"',
            'work="${BUCK_SCRATCH_PATH:-$(mktemp -d)}/pkg"',
            'rm -rf "$work"; mkdir -p "$work" "$out"',
            'cp -R "$src"/. "$work"/',
            'cp "$trust" "$work/Trust.lean"',
            'cd "$work"',
            'lake build "$module" > "$out/build.log" 2>&1',
            'lake env lean Trust.lean > "$out/axioms.txt" 2>&1',
            'lake env leanchecker "$module" > "$out/leanchecker.txt" 2>&1',
            # The bridge corpus (RUE-2227), when the package declares an
            # exporter: built and run here so `corpus.json` is a Buck
            # artifact the rue-oracle-diff consumer can take by $(location).
            'corpus_exe="$6"',
            'if [ -n "$corpus_exe" ]; then',
            '  lake build "$corpus_exe" >> "$out/build.log" 2>&1',
            '  lake exe "$corpus_exe" > "$out/corpus.json"',
            'fi',
            # Executables the package ships that are not exporters (the
            # explainability renderer, RUE-2246): built so they keep
            # compiling, never run, because their output is a rendering for
            # a reader rather than a build input.
            'extra_exes="' + extra_exes + '"',
            'for exe in $extra_exes; do',
            '  lake build "$exe" >> "$out/build.log" 2>&1',
            'done',
        ] + report_lines + [
            # Every `#print axioms` line reads `'<theorem>' depends on axioms: [a, b]`
            # (or `does not depend on any axioms`); reject any axiom outside the
            # allowed set.
            'allowed="' + allowed + '"',
            'status=0',
            'while IFS= read -r line; do',
            '  case "$line" in',
            '    *"depends on axioms: ["*)',
            '      axioms="${line#*depends on axioms: [}"; axioms="${axioms%]*}"',
            '      for axiom in $(printf %s "$axioms" | tr "," " "); do',
            '        case " $allowed " in',
            '          *" $axiom "*) ;;',
            '          *) echo "unexpected axiom $axiom in: $line" >&2; status=1 ;;',
            '        esac',
            '      done ;;',
            '  esac',
            'done < "$out/axioms.txt"',
            '[ "$status" -eq 0 ] || exit "$status"',
            'lean --version > "$out/stamp"',
        ],
        is_executable = True,
    )
    ctx.actions.run(
        cmd_args(
            "/bin/sh",
            script,
            distribution,
            sources,
            trust,
            output.as_output(),
            ctx.attrs.module,
            corpus_exe,
        ),
        category = "lean_package",
        identifier = ctx.label.name,
        # Scratch-space builds of a 2.7 GB toolchain input: keep the action
        # where the toolchain was extracted rather than shipping it.
        prefer_local = True,
    )
    return [DefaultInfo(default_output = output)]

lean_package = rule(
    impl = _lean_package_impl,
    attrs = {
        "allowed_axioms": attrs.list(
            attrs.string(),
            default = ["propext", "Quot.sound"],
            doc = "Axioms the `trust` theorems may depend on; anything else fails the build.",
        ),
        "corpus_exe": attrs.option(
            attrs.string(),
            default = None,
            doc = "A `lean_exe` of the package whose stdout is written to `corpus.json`.",
        ),
        "extra_exes": attrs.list(
            attrs.string(),
            default = [],
            doc = "Further `lean_exe`s of the package to build (not run), so they keep compiling.",
        ),
        "module": attrs.string(doc = "Root module `lake build` and `leanchecker` are given."),
        "report_exes": attrs.dict(
            attrs.string(),
            attrs.list(attrs.string()),
            sorted = True,
            default = {},
            doc = "Output file name -> a `lean_exe` of the package and its arguments, run " +
                  "with its stdout captured into that file beside the other reports. The " +
                  "exe name comes first and the list is never empty.",
        ),
        "srcs": attrs.dep(doc = "The Lake package directory (a dict-form filegroup)."),
        "toolchain": attrs.exec_dep(doc = "The Lean distribution for the execution platform."),
        "trust": attrs.list(
            attrs.string(),
            doc = "Fully qualified theorem names whose axioms are listed and checked.",
        ),
    },
)
