#!/usr/bin/env python3
"""Focused tests for the Bash 3.2 baseline policy."""

from __future__ import annotations

import os
import platform
import subprocess
import sys
import tempfile
import textwrap
import unittest
import unittest.mock
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

SCRIPT = Path(__file__).with_name("validate-shell-bash-baseline.py")
policy = load_script("validate-shell-bash-baseline.py", __file__)

BASELINE_BASH = "/bin/bash"

# The RUE-1511 break, as it shipped: the `"` opened after `covered=` is never
# closed, because the `)` ending the command substitution is followed by
# `; then` instead of `)"`. Every bash rejects it -- verified on 3.2.57 and
# 5.2.15 -- and the construct table says nothing, because an unbalanced quote
# is not a construct. That is the whole case for the parse check (RUE-1512).
UNBALANCED_QUOTE = textwrap.dedent(
    r"""
    buck2=./buck2
    if ! covered="$("$buck2" uquery \
        "attrfilter(labels, a, //crates/...) except (attrfilter(labels, b, //crates/...))" \
        2>/dev/null); then
        exit 1
    fi
    printf '%s\n' "$covered"
    """
)

# The same command, correctly quoted (`)"` before `; then`). Bash 3.2 parses
# this, which is worth asserting: the comment left at the repaired site claims
# 3.2 cannot parse a `$(...)` spanning a line continuation with a quoted
# parenthesised argument, and it can.
BALANCED_QUOTE = textwrap.dedent(
    r"""
    buck2=./buck2
    if ! covered="$("$buck2" uquery \
        "attrfilter(labels, a, //crates/...) except (attrfilter(labels, b, //crates/...))" \
        2>/dev/null)"; then
        exit 1
    fi
    printf '%s\n' "$covered"
    """
)

# A syntax error on Bash 3.2 and valid on Bash 4+, so `bash -n` only rejects it
# when a real 3.2 runs it. This is what a Linux runner's 5.x cannot see, and
# why the macos-15 leg passes --require-baseline-bash.
BASELINE_ONLY_SYNTAX = "case $x in a) run ;;& b) run ;; esac\n"


def baseline_bash_version() -> int | None:
    """The major version of /bin/bash, or None when there is no /bin/bash."""
    if not Path(BASELINE_BASH).exists():
        return None
    result = subprocess.run(
        [BASELINE_BASH, "-c", 'echo "${BASH_VERSINFO[0]}"'],
        capture_output=True,
        text=True,
        check=True,
    )
    return int(result.stdout.strip())


class ScanTests(unittest.TestCase):
    def scan(self, body: str) -> list[str]:
        source = "#!/usr/bin/env bash\nset -euo pipefail\n" + textwrap.dedent(body)
        return [finding.rule.construct for finding in policy.scan(source, "probe.sh")]

    def test_flags_the_builtin_that_caused_rue_1506(self) -> None:
        self.assertEqual(
            self.scan('mapfile -t targets <"$file"\n'),
            ["the `mapfile`/`readarray` builtin"],
        )
        self.assertEqual(
            self.scan("readarray -t targets < <(list)\n"),
            ["the `mapfile`/`readarray` builtin"],
        )

    def test_flags_the_remaining_bash4_constructs(self) -> None:
        cases = {
            "declare -A seen\n": "an associative array",
            "local -Ar seen=()\n": "an associative array",
            'echo "${name^^}"\n': "`${var^^}`/`${var,,}` case modification",
            'echo "${name,,}"\n': "`${var^^}`/`${var,,}` case modification",
            # Positional and special parameters take the same expansions.
            'echo "${1^^}"\n': "`${var^^}`/`${var,,}` case modification",
            'echo "${1@Q}"\n': "a `${var@X}` parameter transformation",
            # A syntax error on 3.2: the script never starts.
            "case $x in a) run ;;& b) run ;; esac\n": "a `;;&`/`;&` case terminator",
            "case $x in a) run ;& b) run ;; esac\n": "a `;;&`/`;&` case terminator",
            "run &>>log\n": "the `&>>` append redirection",
            "coproc reader { cat f; }\n": "the `coproc` keyword",
            "shopt -s globstar\n": "a `shopt` option newer than the baseline",
            "read -t 0.5 -r line\n": "a fractional `read -t` timeout",
            "exec {log}>out\n": "a `{fd}` automatic file-descriptor assignment",
            # Silently returns empty on 3.2 -- a wrong answer, not an error.
            "v=${w:1:-1}\n": "a negative substring length",
            'echo "${a[-1]}"\n': "a negative array subscript",
            'echo "${#a[-1]}"\n': "a negative array subscript",
            "a[-1]=x\n": "a negative array subscript",
            "[[ -v name ]] && echo set\n": "the `-v` set-variable test",
            "[[ ! -v name ]] && echo unset\n": "the `-v` set-variable test",
            "[ -v name ] && echo set\n": "the `-v` set-variable test",
            "declare -g total=0\n": "a `declare -g` global declaration",
            "printf '%(%Y)T\\n' -1\n": "a `printf '%(fmt)T'` time format",
            "declare -n alias=target\n": "a nameref declaration",
            "wait -n\n": "`wait -n`",
            'echo "$EPOCHREALTIME"\n': "the `EPOCHSECONDS`/`EPOCHREALTIME` variable",
        }
        for body, construct in cases.items():
            with self.subTest(body=body.strip()):
                self.assertIn(construct, self.scan(body))

    def test_accepts_the_portable_spellings(self) -> None:
        # The RUE-1506 fix itself, plus the Bash 3.x constructs whose spelling
        # is one character away from a banned one.
        self.assertEqual(
            self.scan(
                """
                targets=()
                while :; do
                    target=""
                    IFS= read -r target || [[ -n "$target" ]] || break
                    targets+=("$target")
                done <"$file"
                declare -a indexed
                local -r pinned=1
                printf -v stamp '%s' "$now"
                run >>log 2>&1
                read -t 5 -r answer
                echo "${#targets[@]}" "${!targets[@]}" "${target:-}" "${!name}"
                echo "${targets[$((${#targets[@]} - 1))]}"
                echo "${path%%/*}" "${path##*/}" "${text//a/b}" "${v: -3}"
                log="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/run.log"
                [[ -n "${name+set}" ]] && wait "$pid"
                case $x in a) run ;; b) run ;; esac
                cmd1 ; cmd2 &
                """
            ),
            [],
        )

    def test_ignores_a_construct_named_in_a_string(self) -> None:
        # A construct inside a string is text: `strip_comment` alone would have
        # flagged all three of these.
        self.assertEqual(self.scan('echo "use mapfile here"\n'), [])
        self.assertEqual(self.scan("die 'readarray unavailable'\n"), [])
        # ...but an expansion inside a double-quoted string is still code.
        self.assertEqual(
            self.scan('echo "the name is ${name^^}"\n'),
            ["`${var^^}`/`${var,,}` case modification"],
        )

    def test_does_not_flag_buck2_test_dash_v(self) -> None:
        # The line this repository is full of, one character from `[ -v x ]`.
        self.assertEqual(self.scan("./buck2 test -v 2 //...\n"), [])

    def test_ignores_the_construct_named_in_a_comment(self) -> None:
        self.assertEqual(self.scan("# mapfile is unavailable on Bash 3.2\n"), [])
        self.assertEqual(self.scan("read -r x  # not mapfile: Bash 3.2\n"), [])

    def test_heredoc_bodies_are_data_but_still_expand(self) -> None:
        # A quoted delimiter makes the body literal text.
        self.assertEqual(
            self.scan("cat <<'EOF'\nmapfile -t a <f\nEOF\n"),
            [],
        )
        # An unquoted one does not: the shell expands the body, so a Bash 4
        # expansion in it fails exactly as it would in code.
        self.assertEqual(
            self.scan("cat <<EOF\n${name^^}\nEOF\n"),
            ["`${var^^}`/`${var,,}` case modification"],
        )
        # A `<<<` herestring is not a heredoc; the line after it is code.
        self.assertEqual(
            self.scan('grep -q x <<<"$text"\nmapfile -t a <f\n'),
            ["the `mapfile`/`readarray` builtin"],
        )

    def test_honours_a_reviewed_allowance(self) -> None:
        self.assertEqual(
            self.scan("mapfile -t a <f  # bash-baseline-ok: linux-only helper\n"), []
        )

    def test_an_allowance_inside_a_string_does_not_silence_the_line(self) -> None:
        self.assertEqual(
            self.scan('X="# bash-baseline-ok: fake"; mapfile -t a <f\n'),
            ["the `mapfile`/`readarray` builtin"],
        )

    def test_the_table_cannot_see_a_syntax_error(self) -> None:
        # The gap the parse check exists to close, asserted rather than
        # described: this file is dead on every bash and the table says
        # nothing, because there is no construct in it to name. `ParseTests`
        # below catches the same source. Do not "fix" this by adding a rule --
        # the class is unbounded, which is exactly why the second check is a
        # parser and not a longer table.
        self.assertEqual(self.scan(UNBALANCED_QUOTE), [])


class WorkflowTests(unittest.TestCase):
    """`run:` steps are held to the baseline on macOS runners only."""

    WORKFLOW = textwrap.dedent(
        """\
        name: ci
        jobs:
          fmt:
            runs-on: ubuntu-latest
            steps:
              - name: Check formatting
                run: |
                  mapfile -t TARGETS < <(list)
                  echo "${#TARGETS[@]}"
          native-platforms:
            strategy:
              matrix:
                include:
                  - os: ubuntu-24.04-arm
                  - os: macos-15
            runs-on: ${{ matrix.os }}
            steps:
              - name: Run native units
                run: |
                  mapfile -t targets <<<"$scope"
              - name: Inline
                run: declare -A seen
        """
    )

    def findings(self, source: str) -> list[tuple[int, str]]:
        return [
            (finding.line, finding.rule.construct)
            for finding in policy.scan_workflow(source, "ci.yml")
        ]

    def test_flags_a_macos_run_step_and_spares_the_linux_one(self) -> None:
        # The Linux `mapfile` at line 8 is correct on ubuntu-latest; the macOS
        # ones at 20 and 22 are the bug that started this policy.
        self.assertEqual(
            self.findings(self.WORKFLOW),
            [
                (20, "the `mapfile`/`readarray` builtin"),
                (22, "an associative array"),
            ],
        )

    def test_a_job_naming_a_macos_runner_directly_is_covered(self) -> None:
        source = self.WORKFLOW.replace("runs-on: ${{ matrix.os }}", "runs-on: macos-15")
        self.assertIn((20, "the `mapfile`/`readarray` builtin"), self.findings(source))

    def test_an_all_linux_workflow_has_no_findings(self) -> None:
        source = self.WORKFLOW.replace("- os: macos-15", "- os: ubuntu-latest").replace(
            "runs-on: ${{ matrix.os }}", "runs-on: ${{ matrix.os }}"
        )
        self.assertEqual(self.findings(source), [])

    def test_the_repository_workflows_are_clean(self) -> None:
        root = SCRIPT.resolve().parent.parent
        workflow = root / ".github/workflows/ci.yml"
        if not workflow.exists():
            self.skipTest("not a repository checkout")
        source = workflow.read_text()
        # The one macOS-capable job is the lane that hit this bug first.
        jobs = policy.macos_runner_jobs(source)
        lines = source.splitlines()
        self.assertEqual([lines[begin].strip() for begin, _ in jobs], ["native-platforms:"])
        self.assertEqual(policy.scan_workflow(source, "ci.yml"), [])


class DiscoveryTests(unittest.TestCase):
    def discover(self, files: dict[str, str]) -> tuple[list[str], list[str]]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, contents in files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents)
            discovered = policy.sources(root)
            found = [
                p.relative_to(root).as_posix()
                for p in discovered.bash + discovered.workflows
            ]
            findings = [str(f) for f in policy.validate(root).findings]
        return found, findings

    BAD = "#!/usr/bin/env bash\nmapfile -t a <f\n"

    def test_skips_build_output_and_vendored_trees(self) -> None:
        found, findings = self.discover(
            {f"{skipped}/nested/probe.sh": self.BAD for skipped in policy.SKIP_DIRECTORIES}
        )
        self.assertEqual(found, [])
        self.assertEqual(findings, [])

    def test_skips_nested_checkouts(self) -> None:
        # The working agreement puts sibling worktrees under `.claude/`, and
        # reporting `.claude/worktrees/rue-1265/test.sh` as a path in THIS tree
        # is worse than not scanning it.
        found, findings = self.discover(
            {
                ".claude/worktrees/other/.git": "gitdir: elsewhere\n",
                ".claude/worktrees/other/test.sh": self.BAD,
                ".claude/hooks/guard.sh": "#!/usr/bin/env bash\ntrue\n",
            }
        )
        self.assertEqual(found, [".claude/hooks/guard.sh"])
        self.assertEqual(findings, [])

    def test_finds_extensionless_scripts_by_shebang(self) -> None:
        found, findings = self.discover({"scripts/tool": self.BAD})
        self.assertEqual(found, ["scripts/tool"])
        self.assertEqual(len(findings), 1)

    def test_accepts_every_bash_shebang_spelling(self) -> None:
        for shebang in ("#!/bin/bash", "#!/usr/bin/env bash", "#!/usr/bin/env -S bash -eu"):
            with self.subTest(shebang=shebang):
                found, _ = self.discover({"tool": f"{shebang}\ntrue\n"})
                self.assertEqual(found, ["tool"])

    def test_ignores_non_bash_interpreters(self) -> None:
        # A `sh` script has a POSIX problem, not a Bash-version one, so the
        # TABLE skips it.
        found, _ = self.discover(
            {
                "posix.sh": "#!/bin/sh\nmapfile -t a <f\n",
                "tool.py": "#!/usr/bin/env python3\n# mapfile\n",
            }
        )
        self.assertEqual(found, [])

    def test_the_parse_set_takes_every_shell_script(self) -> None:
        # ...but the parse check does not skip it: an unbalanced quote in a
        # `#!/bin/sh` script is the same RUE-1511 bug, and before this the
        # repository's one such file was parsed by nothing at all. A bare
        # `.sh` with no shebang counts too, matching the pipefail gate's set
        # so the two cover the same files.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bashy.sh").write_text("#!/usr/bin/env bash\ntrue\n")
            (root / "posix.sh").write_text("#!/bin/sh\ntrue\n")
            (root / "bare.sh").write_text("true\n")
            (root / "tool.py").write_text("#!/usr/bin/env python3\n")
            discovered = policy.sources(root)
            self.assertEqual([p.name for p in discovered.bash], ["bashy.sh"])
            self.assertEqual([p.name for p in discovered.shell], ["bare.sh", "posix.sh"])
            self.assertEqual(
                [p.name for p in discovered.parseable],
                ["bare.sh", "bashy.sh", "posix.sh"],
            )

    def test_workflows_are_scanned_as_workflows(self) -> None:
        found, _ = self.discover(
            {".github/workflows/ci.yml": "jobs:\n  a:\n    runs-on: ubuntu-latest\n"}
        )
        self.assertEqual(found, [".github/workflows/ci.yml"])


class InterpreterTests(unittest.TestCase):
    """Which bash the parse check picks, and what it admits about it.

    A Bash 5 is a usable but weaker parser here, so the question is not whether
    to accept one -- CI's Linux runners have nothing else -- but whether a run
    on one can be mistaken for a run at the baseline. It must not be: `;;&` is
    a syntax error on 3.2 and legal on 5, so a 5.x reporting a clean parse has
    proved strictly less than it appears to. That is the RUE-1506 shape.
    """

    def stub(self, directory: str, body: str) -> Path:
        """A fake `bash` answering the version probe however we like.

        A real Bash 5 is what CI's Linux runners have and this host does not,
        so the cases that matter most are the ones no macOS test could
        otherwise reach.
        """
        path = Path(directory) / "stub-bash"
        path.write_text("#!/bin/sh\n" + body)
        path.chmod(0o755)
        return path

    def resolve(self, body: str):
        with tempfile.TemporaryDirectory() as directory:
            stub = self.stub(directory, body)
            with unittest.mock.patch.dict(
                os.environ, {policy.BASELINE_ENV: str(stub)}
            ):
                return policy.parse_interpreter()

    def test_a_bash_5_is_usable_but_not_at_the_baseline(self) -> None:
        found = self.resolve('echo "5 5.2.37(1)-release"\n')
        self.assertIsNotNone(found)
        self.assertFalse(found.at_baseline)

    def test_a_bash_4_is_usable_but_not_at_the_baseline(self) -> None:
        found = self.resolve('echo "4 4.4.20(1)-release"\n')
        self.assertIsNotNone(found)
        self.assertFalse(found.at_baseline)

    def test_a_bash_3_is_at_the_baseline_and_reports_its_version(self) -> None:
        found = self.resolve('echo "3 3.2.57(1)-release"\n')
        self.assertIsNotNone(found)
        self.assertTrue(found.at_baseline)
        self.assertEqual(found.version, "3.2.57(1)-release")

    def test_a_non_bash_interpreter_is_not_usable(self) -> None:
        # `BASH_VERSINFO` is unset, so the probe answers with neither field.
        self.assertIsNone(self.resolve("echo\n"))

    def test_an_interpreter_that_fails_the_probe_is_not_usable(self) -> None:
        self.assertIsNone(self.resolve('echo "3 3.2.57(1)-release"\nexit 3\n'))

    def test_a_missing_interpreter_is_not_usable(self) -> None:
        with unittest.mock.patch.dict(
            os.environ, {policy.BASELINE_ENV: "/nonexistent/bash"}
        ):
            self.assertIsNone(policy.parse_interpreter())

    def test_the_override_is_not_advisory(self) -> None:
        # A fallback here would let a broken override be silently replaced by
        # whatever bash the host happens to have, which is the same "believed
        # because it passed" failure in miniature.
        with unittest.mock.patch.dict(
            os.environ, {policy.BASELINE_ENV: "/nonexistent/bash"}
        ):
            self.assertIsNone(policy.parse_interpreter())
        self.assertIsNotNone(policy.probe(BASELINE_BASH))


class ParseTests(unittest.TestCase):
    """`bash -n`, on whatever bash this host has.

    The cases split by which interpreter can see them, and the split is the
    design: what every bash rejects is checked on every host, and what only
    3.2 rejects is checked where a 3.2 exists.
    """

    @classmethod
    def setUpClass(cls) -> None:
        cls.interpreter = policy.parse_interpreter()
        if cls.interpreter is None:
            raise unittest.SkipTest("no bash on this host, so `bash -n` cannot run")

    def failures(self, body: str, name: str = "probe.sh") -> list[policy.ParseFailure]:
        failures, _ = self.check(body, name)
        return failures

    def check(self, body: str, name: str = "probe.sh"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root / name
            script.write_text(
                "#!/usr/bin/env bash\nset -euo pipefail\n" + textwrap.dedent(body)
            )
            return policy.parse_check([script], root, self.interpreter)

    def test_the_rue_1511_break_fails_to_parse(self) -> None:
        # The decisive case. The construct table calls this file clean; here it
        # is caught, without any rule describing it, on any bash.
        failures = self.failures(UNBALANCED_QUOTE)
        self.assertEqual([failure.path for failure in failures], ["probe.sh"])
        self.assertIn("unexpected EOF", failures[0].detail)

    def test_the_message_warns_that_the_named_line_is_not_the_mistake(self) -> None:
        # Bash names where the parser gave up, which here is the end of the
        # file. A contributor sent to that line finds nothing wrong with it.
        message = str(self.failures(UNBALANCED_QUOTE)[0])
        self.assertIn("PARSER gave up", message)
        self.assertIn("unbalanced quote", message)

    def test_the_same_command_correctly_quoted_parses(self) -> None:
        # The repair, and the correction to the diagnosis recorded with it: a
        # `$(...)` spanning a line continuation with a quoted parenthesised
        # argument is fine everywhere, including Bash 3.2. The missing `"` was
        # the whole defect.
        self.assertEqual(self.failures(BALANCED_QUOTE), [])

    def test_a_bash_4_builtin_is_not_a_parse_failure(self) -> None:
        # Why the table is not retired: `mapfile` parses perfectly on 3.2 and
        # then exits 127 at run time, and `${v:1:-1}` parses and answers
        # wrongly. Neither is visible to a parser; only the table sees them.
        self.assertEqual(self.failures('mapfile -t a <"$f"\n'), [])
        self.assertEqual(self.failures("v=${w:1:-1}\n"), [])

    def test_syntax_only_bash_3_2_rejects_needs_a_bash_3_2(self) -> None:
        # Why the macos-15 leg exists. `;;&` is on the construct table too, so
        # nothing escapes -- but the parse check's own answer here depends on
        # the interpreter, and that dependence is the reason a Linux run must
        # not be reported as a baseline run.
        failures = self.failures(BASELINE_ONLY_SYNTAX)
        if self.interpreter.at_baseline:
            self.assertEqual(len(failures), 1)
        else:
            self.assertEqual(failures, [])

    def test_the_portable_spellings_parse(self) -> None:
        self.assertEqual(
            self.failures(
                """
                targets=()
                while :; do
                    target=""
                    IFS= read -r target || [[ -n "$target" ]] || break
                    targets+=("$target")
                done <"$file"
                covered="$(list "a (b) c" 2>/dev/null)"
                cat <<'EOF'
                literal $( text
                EOF
                echo "${targets[$((${#targets[@]} - 1))]}" "$covered"
                """
            ),
            [],
        )

    def test_a_runtime_shopt_makes_bash_n_over_catch(self) -> None:
        # The check's one wrong answer, and the reason an escape exists.
        # `bash -n` parses without executing, so it never runs the `shopt`
        # that makes `@(a|b)` legal -- while the file itself RUNS correctly on
        # the same interpreter, asserted below so this cannot rot into a claim
        # about a file that was broken all along.
        body = 'shopt -s extglob\ncase "$x" in @(abc|def)) echo yes ;; esac\n'
        self.assertEqual(len(self.failures(body)), 1)
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "extglob.sh"
            script.write_text("#!/usr/bin/env bash\nx=abc\n" + body)
            ran = subprocess.run(
                [self.interpreter.path, str(script)],
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(ran.returncode, 0, ran.stderr)
        self.assertEqual(ran.stdout.strip(), "yes")

    def test_the_annotation_exempts_the_file(self) -> None:
        failures, exemptions = self.check(
            "# bash-parse-ok: extglob is enabled at run time\n"
            'shopt -s extglob\ncase "$x" in @(abc|def)) echo yes ;; esac\n'
        )
        self.assertEqual(failures, [])
        self.assertEqual([e.reason for e in exemptions], ["extglob is enabled at run time"])

    def test_the_annotation_must_be_a_real_comment(self) -> None:
        # Inside a string it is text, exactly as `# bash-baseline-ok:` is.
        failures, exemptions = self.check(
            'X="# bash-parse-ok: fake"\n'
            'shopt -s extglob\ncase "$x" in @(abc|def)) echo yes ;; esac\n'
        )
        self.assertEqual(len(failures), 1)
        self.assertEqual(exemptions, [])

    def test_a_leading_dash_filename_is_not_read_as_options(self) -> None:
        # Without `--`, bash consumes `-weird.sh` as flags and the usage dump
        # is reported as that file's syntax error.
        self.assertEqual(self.failures("true\n", name="-weird.sh"), [])
        failures = self.failures(UNBALANCED_QUOTE, name="-weird.sh")
        self.assertEqual(len(failures), 1)
        self.assertNotIn("invalid option", failures[0].detail)

    def test_the_summary_names_the_interpreter_and_its_tier(self) -> None:
        # The positive half of the visibility contract: a run that DID parse
        # says which interpreter did it and whether that was the baseline, so
        # "53 scanned" can never be mistaken for "53 parsed at 3.2".
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "probe.sh").write_text("#!/usr/bin/env bash\ntrue\n")
            result = subprocess.run(
                [sys.executable, str(SCRIPT), directory],
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("1 script(s) parsed by", result.stdout)
        self.assertIn(self.interpreter.version, result.stdout)
        expected = "at the baseline" if self.interpreter.at_baseline else "above the"
        self.assertIn(expected, result.stdout)


class ShortfallTests(unittest.TestCase):
    """What a run says when it could not reach the baseline, and how loudly.

    Not a detail. Every host but a Mac is in one of these two states, so this
    IS the check's behaviour almost everywhere it runs. It proceeds rather than
    refusing, because a Bash 5 still catches the RUE-1511 class -- but it never
    proceeds silently, and on the one runner that is supposed to have a 3.2 the
    CI step passes `--require-baseline-bash`, so an interpreter that moved or
    aged into 5.x fails the build instead of downgrading the check unnoticed.
    """

    def run_gate(self, bash: str, *flags: str) -> subprocess.CompletedProcess[str]:
        """The shipped tool, against a one-script tree, on a chosen bash.

        A subprocess rather than a call into `main`, because the contract under
        test is the exit status and the two streams a CI step actually sees.
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "probe.sh").write_text("#!/usr/bin/env bash\ntrue\n")
            interpreter = root / ("absent-bash" if bash == "absent" else "stub-bash")
            if bash != "absent":
                interpreter.write_text(f'#!/bin/sh\necho "{bash}"\n')
                interpreter.chmod(0o755)
            environ = dict(os.environ)
            environ[policy.BASELINE_ENV] = str(interpreter)
            return subprocess.run(
                [sys.executable, str(SCRIPT), directory, *flags],
                capture_output=True,
                text=True,
                check=False,
                env=environ,
            )

    def test_no_bash_notes_what_went_unparsed(self) -> None:
        result = self.run_gate("absent")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("note: no bash at", result.stderr)
        self.assertIn("1 script(s) NOT parsed", result.stdout)

    def test_no_bash_fails_under_require_baseline_bash(self) -> None:
        result = self.run_gate("absent", "--require-baseline-bash")
        self.assertEqual(result.returncode, 1)
        self.assertIn("error: no bash at", result.stderr)

    def test_a_bash_5_parses_but_says_it_is_above_the_baseline(self) -> None:
        # The Linux CI case. The run is real and worth having -- it catches
        # everything RUE-1511 was -- but the summary must not let it read as a
        # baseline run, and the note names what it could not see.
        result = self.run_gate("5 5.2.15(1)-release")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("note: the parse check ran on Bash 5.2.15", result.stderr)
        self.assertIn(";;&", result.stderr)
        self.assertIn("above the baseline", result.stdout)

    def test_a_bash_5_fails_under_require_baseline_bash(self) -> None:
        # The point of the flag: on macos-15 an above-baseline bash is not a
        # weaker pass, it is a broken assumption about the runner.
        result = self.run_gate("5 5.2.15(1)-release", "--require-baseline-bash")
        self.assertEqual(result.returncode, 1)
        self.assertIn("error: the parse check ran on Bash 5.2.15", result.stderr)

    def test_a_bash_3_reports_no_shortfall_and_satisfies_the_flag(self) -> None:
        result = self.run_gate("3 3.2.57(1)-release", "--require-baseline-bash")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("at the baseline", result.stdout)
        self.assertEqual(result.stderr, "")

    def test_an_empty_discovery_set_fails_even_with_the_flag(self) -> None:
        # The one state where every other signal is green and nothing has been
        # proven: the interpreter is at the baseline, the flag is satisfied,
        # and no file was checked. ci.yml makes the same call for the fmt
        # job's target query sixteen lines above this gate's step (RUE-1152).
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                [sys.executable, str(SCRIPT), directory, "--require-baseline-bash"],
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(result.returncode, 1)
        self.assertIn("error: no shell scripts found", result.stderr)
        self.assertNotIn("policy valid", result.stdout)

    def test_the_library_entry_point_keeps_no_such_floor(self) -> None:
        # The floor belongs to the tool, not to `validate()`: the discovery
        # tests scan deliberately empty trees.
        with tempfile.TemporaryDirectory() as directory:
            report = policy.validate(Path(directory))
        self.assertEqual(report.scripts, 0)
        self.assertEqual(report.findings, [])

    def test_the_construct_table_still_runs_without_an_interpreter(self) -> None:
        # The halves are independent: a host with no bash at all still gets
        # every table finding, which is what makes the `fmt` job authoritative
        # for that half.
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "probe.sh").write_text(
                "#!/usr/bin/env bash\nmapfile -t a <f\n"
            )
            environ = dict(os.environ)
            environ[policy.BASELINE_ENV] = str(Path(directory) / "absent-bash")
            result = subprocess.run(
                [sys.executable, str(SCRIPT), directory],
                capture_output=True,
                text=True,
                check=False,
                env=environ,
            )
        self.assertEqual(result.returncode, 1)
        self.assertIn("`mapfile`/`readarray` builtin", result.stderr)


class RepositoryTests(unittest.TestCase):
    def checkout(self) -> Path:
        # These DO run under Buck, despite the sandbox holding only the
        # validator: `SCRIPT.resolve()` follows the sandbox symlink back out
        # to the real file, so `parent.parent` is the checkout and the
        # whole-tree scan is genuine. The guard is for a copy of this file
        # somewhere without a tree around it, where a scan would find nothing
        # and pass vacuously -- the fail-open this policy exists to prevent.
        root = SCRIPT.resolve().parent.parent
        if not (root / "AGENTS.md").exists():
            self.skipTest("not a repository checkout; the scan runs as a CI step")
        return root

    def test_repository_is_clean(self) -> None:
        self.assertEqual(
            [str(finding) for finding in policy.validate(self.checkout()).findings], []
        )

    def test_repository_parses(self) -> None:
        root = self.checkout()
        interpreter = policy.parse_interpreter()
        if interpreter is None:
            self.skipTest("no bash on this host")
        report = policy.validate(root, interpreter)
        self.assertEqual([str(failure) for failure in report.failures], [])
        # A parse check over nothing would pass just as quietly.
        self.assertGreater(report.scripts, 0)


class MechanismTests(unittest.TestCase):
    """The failures this policy exists to prevent, demonstrated end to end.

    These need a Bash 3.2 to demonstrate anything, so they run on macOS and
    skip elsewhere. That is why ci.yml runs this target on the macos-15 leg of
    native-platforms: premerge alone would leave the demonstrations skipped on
    every runner, which is a test that cannot fail.
    """

    @classmethod
    def setUpClass(cls) -> None:
        version = baseline_bash_version()
        if version is None or version >= 4:
            raise unittest.SkipTest(
                f"/bin/bash is {version}.x, not the 3.2 baseline these demonstrate"
            )

    def run_bash(self, body: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "probe.sh"
            script.write_text(
                "#!/usr/bin/env bash\nset -euo pipefail\n" + textwrap.dedent(body)
            )
            # `cwd` matters: these probes write scratch input files, and
            # without it they land in the caller's working directory.
            return subprocess.run(
                [BASELINE_BASH, str(script)],
                cwd=directory,
                capture_output=True,
                text=True,
                check=False,
            )

    def test_mapfile_is_a_command_not_found(self) -> None:
        result = self.run_bash('printf "a\\n" >f\nmapfile -t a <f\necho "${a[0]}"\n')
        self.assertEqual(result.returncode, 127)
        self.assertIn("mapfile: command not found", result.stderr)

    def test_the_portable_read_loop_reads_the_same_lines(self) -> None:
        result = self.run_bash(
            """
            printf 'x\\ny' >f
            a=()
            while :; do
                line=""
                IFS= read -r line || [[ -n "$line" ]] || break
                a+=("$line")
            done <f
            echo "${#a[@]}:${a[*]}"
            """
        )
        self.assertEqual(result.stdout.strip(), "2:x y")

    def test_empty_array_expansion_is_unbound_under_set_u(self) -> None:
        # The second Bash 3.2 hazard, which no scanner can see: this is why the
        # narrowing keeps its full-pattern fallback until the list is non-empty.
        result = self.run_bash('a=()\necho "${a[@]}"\n')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unbound variable", result.stderr)

    def test_read_assigns_nothing_when_it_fails(self) -> None:
        # The third: `read` clears its variable at EOF but leaves it untouched
        # on an error, so `read x || [[ -n "$x" ]]` needs `x` cleared first --
        # otherwise it is either unbound or the previous line, read twice.
        result = self.run_bash(
            'v=preset\nread -r v <. 2>/dev/null || true\necho "[$v]"\n'
        )
        self.assertEqual(result.stdout.strip(), "[preset]")


class HostTests(unittest.TestCase):
    def test_a_mac_always_has_the_baseline_interpreter(self) -> None:
        # Guards both skips above: on a macOS host the demonstrations and the
        # parse check MUST run, so a silently-skipping suite cannot become the
        # normal state there -- which is the state ci.yml's macos-15 leg
        # depends on, and enforces from its side with --require-baseline-bash.
        if platform.system() != "Darwin":
            self.skipTest("not macOS")
        self.assertEqual(baseline_bash_version(), 3)
        with unittest.mock.patch.dict(os.environ):
            os.environ.pop(policy.BASELINE_ENV, None)
            interpreter = policy.parse_interpreter()
        self.assertIsNotNone(interpreter)
        self.assertEqual(interpreter.path, BASELINE_BASH)
        self.assertTrue(interpreter.at_baseline, interpreter.version)


if __name__ == "__main__":
    unittest.main()
