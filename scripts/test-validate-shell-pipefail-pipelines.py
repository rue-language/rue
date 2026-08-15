#!/usr/bin/env python3
"""Focused tests for the pipefail early-exit pipeline policy."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

SCRIPT = Path(__file__).with_name("validate-shell-pipefail-pipelines.py")
policy = load_script("validate-shell-pipefail-pipelines.py", __file__)


class ScanTests(unittest.TestCase):
    def scan(self, body: str, *, pipefail: bool = True) -> list[str]:
        header = "#!/usr/bin/env bash\n"
        if pipefail:
            header += "set -euo pipefail\n"
        source = header + textwrap.dedent(body)
        return [finding.stage for finding in policy.scan(source, "probe.sh")]

    def test_flags_grep_q_behind_a_pipe(self) -> None:
        self.assertEqual(self.scan('printf "%s" "$x" | grep -q needle\n'), ["grep"])

    def test_flags_every_short_circuiting_grep_flag(self) -> None:
        for flags in ("-q", "-qE", "-Fxq", "-l", "-L", "-m 1"):
            with self.subTest(flags=flags):
                self.assertEqual(self.scan(f'cat f | grep {flags} needle\n'), ["grep"])

    def test_flags_grep_variants_and_absolute_paths(self) -> None:
        for tool in ("egrep", "fgrep", "zgrep", "rg", "/usr/bin/grep"):
            with self.subTest(tool=tool):
                self.assertEqual(
                    self.scan(f'cat f | {tool} -q needle\n'), [tool]
                )

    def test_flags_head_behind_a_pipe(self) -> None:
        self.assertEqual(self.scan('sed -n "s/a//p" f | head -n 1\n'), ["head"])

    def test_ignores_scripts_without_pipefail(self) -> None:
        # Without pipefail the pipeline reports grep's status, which is correct.
        self.assertEqual(self.scan('cat f | grep -q x\n', pipefail=False), [])

    def test_accepts_a_draining_grep(self) -> None:
        # A plain grep reads its input to exhaustion, so no EPIPE is possible.
        self.assertEqual(self.scan('cat f | grep needle\n'), [])

    def test_accepts_the_herestring_fix(self) -> None:
        self.assertEqual(self.scan('grep -q needle <<<"$text"\n'), [])
        self.assertEqual(self.scan('grep -q needle <<<"$(cat f)"\n'), [])

    def test_accepts_an_unpiped_early_exit_consumer(self) -> None:
        self.assertEqual(self.scan('grep -q needle f\n'), [])
        self.assertEqual(self.scan('head -n 1 f\n'), [])

    def test_does_not_confuse_logical_or_with_a_pipe(self) -> None:
        self.assertEqual(self.scan('grep -q needle f || head -n 1 f\n'), [])

    def test_ignores_comments_and_quoted_pipes(self) -> None:
        # This policy's own fix notes name the construct they ban.
        self.assertEqual(self.scan('# replaced cat f | grep -q x with a herestring\n'), [])
        self.assertEqual(self.scan("""grep -E 'a|head -n 1' f\n"""), [])

    def test_honours_a_reviewed_allowance(self) -> None:
        self.assertEqual(
            self.scan('cat f | grep -q x  # pipefail-pipeline-ok: reviewed\n'), []
        )

    def test_flags_a_stage_inside_a_command_substitution(self) -> None:
        self.assertEqual(self.scan('v="$(cat f | grep -q x && echo y)"\n'), ["grep"])


class DiscoveryTests(unittest.TestCase):
    def discover(self, files: dict[str, str]) -> tuple[list[str], list[str]]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, contents in files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents)
            scanned = [
                p.relative_to(root).as_posix() for p in policy.shell_scripts(root)
            ]
            findings = [str(f) for f in policy.validate(root)]
        return scanned, findings

    BAD = "#!/usr/bin/env bash\nset -euo pipefail\ncat f | grep -q x\n"

    def test_skips_build_output_and_vendored_trees(self) -> None:
        # Scanning buck-out made the gate's runtime and result depend on
        # whether a build had run.
        scanned, findings = self.discover(
            {f"{skipped}/nested/probe.sh": self.BAD for skipped in policy.SKIP_DIRECTORIES}
        )
        self.assertEqual(scanned, [])
        self.assertEqual(findings, [])

    def test_skips_nested_checkouts(self) -> None:
        # A sibling worktree carries its own copy of every script, and the
        # working agreement puts those under `.claude/worktrees/` -- inside the
        # tree this gate walks (RUE-1506).
        scanned, findings = self.discover(
            {
                ".claude/worktrees/other/.git": "gitdir: elsewhere\n",
                ".claude/worktrees/other/probe.sh": self.BAD,
                ".claude/hooks/guard.sh": "#!/usr/bin/env bash\ntrue\n",
            }
        )
        self.assertEqual(scanned, [".claude/hooks/guard.sh"])
        self.assertEqual(findings, [])

    def test_finds_extensionless_scripts_by_shebang(self) -> None:
        scanned, findings = self.discover({"scripts/tool": self.BAD})
        self.assertEqual(scanned, ["scripts/tool"])
        self.assertEqual(len(findings), 1)

    def test_ignores_non_shell_files(self) -> None:
        scanned, _ = self.discover({"a.py": "# cat f | grep -q x\n", "b.rs": "fn f() {}\n"})
        self.assertEqual(scanned, [])


class RepositoryTests(unittest.TestCase):
    def test_repository_is_clean(self) -> None:
        # Under Buck this test runs from a sandbox holding only the validator,
        # where a whole-tree scan would find nothing and pass vacuously -- the
        # exact shape of fail-open this policy exists to prevent. Skip loudly
        # instead; the authoritative scan is the `fmt` job's CI step.
        root = SCRIPT.resolve().parent.parent
        # `clippy.sh` used to be the sentinel; it was deleted, so this test
        # skipped in real checkouts too and stopped scanning anything at all
        # (RUE-1506). AGENTS.md is the repository's own canonical file.
        if not (root / "AGENTS.md").exists():
            self.skipTest("not a repository checkout; scan runs as a CI step")
        findings = policy.validate(root)
        self.assertEqual([str(finding) for finding in findings], [])


class MechanismTests(unittest.TestCase):
    """The bug this policy exists to prevent, demonstrated end to end."""

    def run_bash(self, body: str) -> str:
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "probe.sh"
            script.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + textwrap.dedent(body))
            return subprocess.run(
                ["bash", str(script)], capture_output=True, text=True, check=False
            ).stdout.strip()

    # A payload large enough that the producer is still writing when `grep -q`
    # matches on the first line and closes the pipe.
    PAYLOAD = 'CLEANED="$(printf \'error: boom\\n\'; seq 1 200000)"\n'

    def test_pipe_under_pipefail_discards_the_match(self) -> None:
        self.assertEqual(
            self.run_bash(
                self.PAYLOAD
                + 'if printf "%s\\n" "$CLEANED" | grep -qE "^error"; then\n'
                '    echo DETECTED\nelse\n    echo MISSED\nfi\n'
            ),
            "MISSED",
        )

    def test_herestring_reports_the_match(self) -> None:
        self.assertEqual(
            self.run_bash(
                self.PAYLOAD
                + 'if grep -qE "^error" <<<"$CLEANED"; then\n'
                '    echo DETECTED\nelse\n    echo MISSED\nfi\n'
            ),
            "DETECTED",
        )


if __name__ == "__main__":
    unittest.main()
