#!/usr/bin/env python3
"""Focused tests for the Bash 3.2 baseline policy."""

from __future__ import annotations

import importlib.util
import platform
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("validate-shell-bash-baseline.py")
SPEC = importlib.util.spec_from_file_location("bash_baseline", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
policy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(policy)

BASELINE_BASH = "/bin/bash"


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
            scripts, workflows = policy.sources(root)
            found = [p.relative_to(root).as_posix() for p in scripts + workflows]
            findings = [str(f) for f in policy.validate(root)[0]]
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
        # A `sh` script has a POSIX problem, not a Bash-version one.
        found, _ = self.discover(
            {
                "posix.sh": "#!/bin/sh\nmapfile -t a <f\n",
                "tool.py": "#!/usr/bin/env python3\n# mapfile\n",
            }
        )
        self.assertEqual(found, [])

    def test_workflows_are_scanned_as_workflows(self) -> None:
        found, _ = self.discover(
            {".github/workflows/ci.yml": "jobs:\n  a:\n    runs-on: ubuntu-latest\n"}
        )
        self.assertEqual(found, [".github/workflows/ci.yml"])


class RepositoryTests(unittest.TestCase):
    def test_repository_is_clean(self) -> None:
        # Under Buck this test runs from a sandbox holding only the validator,
        # where a whole-tree scan would find nothing and pass vacuously -- the
        # exact shape of fail-open this policy exists to prevent. Skip loudly
        # instead; the authoritative scan is the `fmt` job's CI step.
        root = SCRIPT.resolve().parent.parent
        if not (root / "AGENTS.md").exists():
            self.skipTest("not a repository checkout; scan runs as a CI step")
        findings, _ = policy.validate(root)
        self.assertEqual([str(finding) for finding in findings], [])


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
        # Guards the skip above: on a macOS host the demonstrations MUST run,
        # so a silently-skipping suite cannot become the normal state there.
        if platform.system() != "Darwin":
            self.skipTest("not macOS")
        self.assertEqual(baseline_bash_version(), 3)


if __name__ == "__main__":
    unittest.main()
