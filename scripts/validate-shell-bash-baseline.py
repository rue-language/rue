#!/usr/bin/env python3
"""Ban Bash 4+ constructs in shell that runs on macOS's Bash 3.2.

macOS ships GNU Bash 3.2.57 as `/bin/bash` and has since 2007, because Bash 4
is GPLv3. `#!/usr/bin/env bash` resolves to it on a stock Mac, and a GitHub
`macos-*` runner's default step shell is that same interpreter. A Bash 4
builtin is therefore not a portability nicety here: it is `command not found`,
exit 127, before the script does any of its work -- or, worse, a construct like
`${v:1:-1}` that Bash 3.2 parses and answers WRONGLY.

That is exactly how it has failed, twice. `mapfile -t` in test.sh's narrowing
path made //:affected-targets-tool-tests fail two premerge checks on every Mac
while CI stayed green, because the only place that sets RUE_TEST_TARGETS_FILE
is the Linux premerge job (RUE-1506). The same builtin had already killed every
narrowed macOS run in ci.yml's native lane. Twice is a policy.

The portable spellings:

    mapfile -t a <f          ->  a=(); while IFS= read -r l || [[ -n "$l" ]]
                                 do a+=("$l"); done <f
    declare -A m             ->  parallel arrays, or a `case`
    "${v^^}" / "${v,,}"      ->  tr '[:lower:]' '[:upper:]'
    "${v:1:-1}"              ->  "${v:1:$((${#v} - 2))}"
    case x in a) ;;& b) ;;   ->  repeat the body, or restructure the `case`
    cmd &>>log               ->  cmd >>log 2>&1
    "${a[-1]}"               ->  "${a[$((${#a[@]} - 1))]}"

Bash 3.2 has a second trap this scanner cannot see and reviewers must: under
`set -u`, expanding an EMPTY array as `"${a[@]}"` is an unbound-variable error,
not an empty list. Keep a fallback in the array or guard on `${#a[@]}`, which
is always safe. `read` is a third: it assigns nothing when it FAILS, as against
reaching EOF, so `read x || [[ -n "$x" ]]` needs `x` cleared before each read.

WHAT IS SCANNED. Files carrying a bash shebang, and the `run:` steps of any
workflow job that can land on a macOS runner -- that job is where this bug was
first reported, so excluding workflow YAML wholesale would leave the original
crime scene unguarded. A `run:` step in a `ubuntu-*` job is NOT scanned: it
executes on Bash 5 and `mapfile` there is correct, which is why ci.yml's fmt
job may use the builtin sixteen lines above the step that runs this gate. A
`#!/bin/sh` script is a different policy (POSIX, not a Bash version).

Annotate a reviewed exception with `# bash-baseline-ok: <reason>`. It has to
sit in a real comment -- inside a string it is text, not an annotation -- and
it silences its whole line.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path
from typing import NamedTuple


ROOT = Path(__file__).resolve().parent.parent

# Build outputs and vendored trees are not ours to police, and `buck-out` in
# particular makes the scan's cost and result depend on whether a build has run.
SKIP_DIRECTORIES = {
    ".buckd",
    ".git",
    ".jj",
    "buck-out",
    "buck2-bin",
    "node_modules",
    "target",
    "third-party",
}

# `#!/usr/bin/env bash`, `#!/bin/bash`, `#!/usr/bin/env -S bash -eu`.
BASH_SHEBANG = re.compile(r"^#!\s*\S*(?:\s+-\S+)*\s*(?:\S*/)?bash\b|^#!\S*/bash\b")

ALLOW = re.compile(r"#\s*bash-baseline-ok:\s*(?P<reason>\S.*?)\s*$")

# A parameter name, including the positional and special parameters: `${1^^}`
# and `${@Q}` are as unavailable on 3.2 as `${name^^}` is.
NAME = r"(?:[A-Za-z_]\w*|\d+|[@*#?$!-])"


class Rule(NamedTuple):
    pattern: re.Pattern[str]
    since: str
    construct: str
    fix: str
    # An expansion still fires inside an unquoted heredoc body, where the shell
    # expands but does not execute. A command does not.
    expansion: bool = False
    # A construct that lives inside a quoted string -- a `printf` format -- and
    # so has to be matched before literal text is blanked out.
    quoted: bool = False


# Only constructs whose introducing version is unambiguous. A rule that has to
# hedge would be answered with an annotation rather than a fix, which teaches
# reviewers to ignore the gate.
RULES = (
    Rule(
        re.compile(r"(?<![\w./-])(?:mapfile|readarray)(?![\w-])"),
        "4.0",
        "the `mapfile`/`readarray` builtin",
        'read the lines with `while IFS= read -r line || [[ -n "$line" ]]`',
    ),
    Rule(
        re.compile(r"(?<![\w-])(?:declare|typeset|local|readonly)\s+-[A-Za-z]*A[A-Za-z]*(?![\w-])"),
        "4.0",
        "an associative array",
        "use parallel indexed arrays or a `case`",
    ),
    Rule(
        re.compile(r"\$\{[!#]?" + NAME + r"(?:\[[^]]*\])?(?:\^\^?|,,?)"),
        "4.0",
        "`${var^^}`/`${var,,}` case modification",
        "pipe through `tr '[:lower:]' '[:upper:]'`",
        expansion=True,
    ),
    Rule(
        # `;;&` resumes matching, `;&` falls through. Both are a SYNTAX error on
        # 3.2, so the script dies at parse time rather than at the branch.
        re.compile(r";;?&(?!&)"),
        "4.0",
        "a `;;&`/`;&` case terminator",
        "repeat the body, or restructure the `case`",
    ),
    Rule(
        re.compile(r"&>>"),
        "4.0",
        "the `&>>` append redirection",
        "write `>>file 2>&1`",
    ),
    Rule(
        re.compile(r"(?<![\w-])coproc(?![\w-])"),
        "4.0",
        "the `coproc` keyword",
        "use explicit FIFOs or a background job",
    ),
    Rule(
        re.compile(r"(?<![\w-])shopt\s+-[a-z]*s\b[^\n]*(?<![\w-])(?:globstar|lastpipe)(?![\w-])"),
        "4.0",
        "a `shopt` option newer than the baseline",
        "use `find`, or restructure the pipeline",
    ),
    Rule(
        # Integer `-t` is ancient; a fractional timeout is 4.0.
        re.compile(r"(?<![\w-])read\s+(?:-\S+\s+)*-t\s*\d*\.\d"),
        "4.0",
        "a fractional `read -t` timeout",
        "round the timeout to whole seconds",
    ),
    Rule(
        re.compile(r"(?<![\w-])exec\s+[^#\n]*(?<![$])\{[A-Za-z_]\w*\}\s*[<>]"),
        "4.1",
        "a `{fd}` automatic file-descriptor assignment",
        "name the descriptor explicitly (`exec 3>...`)",
    ),
    Rule(
        # `${v:off:-len}`. A negative OFFSET (`${v: -3}`) is fine on 3.2; a
        # negative LENGTH is 4.2, and 3.2 answers it as empty rather than
        # failing -- a wrong answer, not a crash.
        re.compile(
            r"\$\{[!#]?" + NAME + r"(?:\[[^]]*\])?:\s*(?![-=?+])[^:{}]*:\s*-\s*\d"
        ),
        "4.2",
        "a negative substring length",
        "compute the length: `${v:1:$((${#v} - 2))}`",
        expansion=True,
    ),
    Rule(
        re.compile(r"\$\{[!#]?[A-Za-z_]\w*\[\s*-\s*\d"),
        "4.2",
        "a negative array subscript",
        'index from the length: `"${a[$((${#a[@]} - 1))]}"`',
        expansion=True,
    ),
    Rule(
        re.compile(r"(?<![\w-])[A-Za-z_]\w*\[\s*-\s*\d+\s*\]\s*="),
        "4.2",
        "a negative array subscript",
        "index from the length",
    ),
    Rule(
        # `[[ -v x ]]`, `[ -v x ]`, `[[ ! -v x ]]`. A bare `test -v` is left to
        # review: `./buck2 test -v ...` is the far more common line in this
        # repository, and a rule that flagged it would be turned off.
        re.compile(r"\[\[?\s+(?:!\s+)?-v\s"),
        "4.2",
        "the `-v` set-variable test",
        'test `[[ -n "${var+set}" ]]`',
    ),
    Rule(
        re.compile(r"(?<![\w-])(?:declare|typeset)\s+-[A-Za-z]*g[A-Za-z]*(?![\w-])"),
        "4.2",
        "a `declare -g` global declaration",
        "assign at the top level, or drop the declaration",
    ),
    Rule(
        re.compile(r"%\([^)]*\)T"),
        "4.2",
        "a `printf '%(fmt)T'` time format",
        "call `date`",
        quoted=True,
    ),
    Rule(
        re.compile(r"(?<![\w-])(?:declare|typeset|local)\s+-[A-Za-z]*n[A-Za-z]*(?![\w-])"),
        "4.3",
        "a nameref declaration",
        "pass the value, or use indirect `${!name}` expansion",
    ),
    Rule(
        re.compile(r"(?<![\w-])wait\s+-n(?![\w-])"),
        "4.3",
        "`wait -n`",
        "wait on recorded PIDs individually",
    ),
    Rule(
        re.compile(r"\$\{[!#]?" + NAME + r"(?:\[[^]]*\])?@[QEPAKaLUu]\}"),
        "4.4",
        "a `${var@X}` parameter transformation",
        "use `printf %q` or an explicit expansion",
        expansion=True,
    ),
    Rule(
        re.compile(r"(?<![\w-])EPOCH(?:SECONDS|REALTIME)(?![\w-])"),
        "5.0",
        "the `EPOCHSECONDS`/`EPOCHREALTIME` variable",
        "call `date +%s`",
        expansion=True,
    ),
)

# `<<WORD`, `<<-WORD`, `<<'WORD'`. Not `<<<`, which is a herestring.
HEREDOC = re.compile(r"<<(?!<)(-?)\s*(['\"]?)([A-Za-z_]\w*)\2")


class Finding(NamedTuple):
    path: str
    line: int
    rule: Rule

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: {self.rule.construct} needs Bash "
            f"{self.rule.since}; macOS runs this with Bash 3.2, where it fails. "
            f"Instead, {self.rule.fix} -- or annotate with "
            "`# bash-baseline-ok: <reason>`"
        )


def split_code(line: str) -> tuple[str, str]:
    """Return the line's executable text and its trailing comment.

    Literal text is blanked out, because a construct named in a string is not
    a construct: `die "readarray unavailable"` is a fine Bash 3.2 line, and so
    is this policy's own prose. An expansion or command substitution INSIDE a
    double-quoted string is code again -- `"${v^^}"` is how case modification
    is almost always written -- so quoting is tracked as a stack rather than a
    flag. A `#` only opens a comment in code context, never inside `${v#pat}`.
    """
    code: list[str] = []
    stack: list[str] = []
    index = 0
    while index < len(line):
        character = line[index]
        top = stack[-1] if stack else ""
        literal = top in ("'", '"')
        if top == "'":
            code.append(" ")
            if character == "'":
                stack.pop()
            index += 1
            continue
        if character == "\\":
            code.append(" " if literal else character)
            if index + 1 < len(line):
                code.append(" " if literal else line[index + 1])
            index += 2
            continue
        if literal and character == "$":
            # A plain `$NAME` inside double quotes is an expansion, not text.
            name = re.match(r"\$(?:[A-Za-z_]\w*|[0-9@*#?!])", line[index:])
            if name:
                code.append(name.group(0))
                index += len(name.group(0))
                continue
        if line[index : index + 2] in ("$(", "${"):
            stack.append(")" if line[index + 1] == "(" else "}")
            code.append(line[index : index + 2])
            index += 2
            continue
        if top in (")", "}") and character == top:
            stack.pop()
            code.append(character)
            index += 1
            continue
        if character in "'\"":
            if character == top:
                stack.pop()
            else:
                stack.append(character)
            code.append(" ")
            index += 1
            continue
        if not stack and character == "#" and (index == 0 or line[index - 1].isspace()):
            return "".join(code), line[index:]
        code.append(" " if literal else character)
        index += 1
    return "".join(code), ""


def scan(source: str, path: str) -> list[Finding]:
    """Findings in one shell script."""
    findings: list[Finding] = []
    # A heredoc body is data, not code, so `mapfile` inside one is a string.
    # An UNQUOTED delimiter still expands, though, so `${v^^}` in that body is
    # a real 3.2 failure and the expansion rules stay live.
    terminator = ""
    expands = False
    for number, raw in enumerate(source.splitlines(), start=1):
        if terminator:
            if raw.strip() == terminator:
                terminator = ""
            elif expands:
                findings.extend(
                    Finding(path, number, rule)
                    for rule in RULES
                    if rule.expansion and rule.pattern.search(raw)
                )
            continue
        code, comment = split_code(raw)
        # The delimiter is matched on the raw line -- `<<'EOF'` quotes it, so
        # the blanked code holds only `<<` -- but only once the code says a
        # redirection is there at all, so a `<<EOF` inside a string or comment
        # cannot swallow the lines that follow it.
        heredoc = HEREDOC.search(raw) if "<<" in code else None
        if heredoc:
            terminator = heredoc.group(3)
            expands = heredoc.group(2) == ""
        if ALLOW.search(comment):
            continue
        # A `quoted` rule matches the line with only its comment removed: a
        # `printf '%(%F)T'` format is inside a string by construction, so the
        # blanked code would never show it.
        uncommented = raw[: len(raw) - len(comment)] if comment else raw
        findings.extend(
            Finding(path, number, rule)
            for rule in RULES
            if rule.pattern.search(uncommented if rule.quoted else code)
        )
    return findings


def macos_runner_jobs(source: str) -> list[tuple[int, int]]:
    """Line ranges of workflow jobs that can execute on a macOS runner.

    Determining that is the "per-job runner analysis" a whole-file scan would
    need, and in this repository it is a literal `os:` key: jobs either name
    their runner or take it from a matrix that lists one per entry.
    """
    lines = source.splitlines()
    jobs: list[tuple[int, int]] = []
    start = None
    in_jobs = False
    for index, line in enumerate(lines):
        if re.match(r"^jobs:\s*$", line):
            in_jobs = True
            continue
        top_level = bool(line) and not line[0].isspace() and not line.startswith("#")
        header = bool(re.match(r"^  [A-Za-z0-9_-]+:\s*$", line))
        if start is not None and (header or top_level):
            jobs.append((start, index))
            start = None
        if top_level:
            in_jobs = False
        if in_jobs and header:
            start = index
    if start is not None:
        jobs.append((start, len(lines)))

    macos: list[tuple[int, int]] = []
    for begin, end in jobs:
        body = "\n".join(lines[begin:end])
        runners = re.findall(r"^\s*(?:- )?(?:runs-on|os):\s*(\S+)", body, re.MULTILINE)
        if any("macos" in runner for runner in runners):
            macos.append((begin, end))
    return macos


def scan_workflow(source: str, path: str) -> list[Finding]:
    """Findings in the `run:` steps of a workflow's macOS-capable jobs.

    A GitHub `run:` step with no `shell:` key runs under `bash -e {0}`, which
    on a `macos-*` image is /bin/bash -- 3.2. That is the interpreter that
    killed the native lane's narrowed run, so these steps are held to the same
    baseline as a checked-in script.
    """
    lines = source.splitlines()
    findings: list[Finding] = []
    for begin, end in macos_runner_jobs(source):
        index = begin
        while index < end:
            match = re.match(r"^(\s+)run:\s*(.*)$", lines[index])
            if not match:
                index += 1
                continue
            indent, inline = match.group(1), match.group(2).strip()
            body: list[tuple[int, str]] = []
            if inline in ("|", "|-", "|+", ">", ">-", ">+"):
                index += 1
                while index < end:
                    line = lines[index]
                    if line.strip() and not line.startswith(indent + " "):
                        break
                    body.append((index + 1, line))
                    index += 1
            else:
                body.append((index + 1, inline))
                index += 1
            step = "\n".join(text for _, text in body)
            for finding in scan(step, path):
                findings.append(Finding(path, body[finding.line - 1][0], finding.rule))
    return findings


def _prune(directory: Path, names: list[str]) -> list[str]:
    """Directory names to descend into.

    Nested checkouts are pruned by their own VCS marker rather than by name:
    the working agreement puts sibling worktrees under `.claude/worktrees/`,
    and reporting `.claude/worktrees/rue-1265/test.sh` as though it were a path
    in THIS tree is worse than not scanning it -- that copy has its own gate.
    """
    kept = []
    for name in sorted(names):
        if name in SKIP_DIRECTORIES:
            continue
        path = directory / name
        if (path / ".git").exists() or (path / ".jj").exists():
            continue
        kept.append(name)
    return kept


def sources(root: Path) -> tuple[list[Path], list[Path]]:
    """Every bash script and workflow file under `root`."""
    scripts: list[Path] = []
    workflows: list[Path] = []
    for directory, names, files in os.walk(root):
        here = Path(directory)
        names[:] = _prune(here, names)
        for name in sorted(files):
            path = here / name
            if path.is_symlink() or not path.is_file():
                continue
            if path.suffix in (".yml", ".yaml") and (
                here.name == "workflows" and here.parent.name == ".github"
            ):
                workflows.append(path)
                continue
            if path.suffix and path.suffix != ".sh":
                continue
            try:
                with path.open("r", encoding="utf-8", errors="strict") as handle:
                    first = handle.readline()
            except (OSError, UnicodeDecodeError):
                continue
            # Unlike the pipefail policy, the shebang is the whole question: it
            # decides which bash runs the file, so a `.sh` without one is not
            # this gate's business.
            if BASH_SHEBANG.match(first):
                scripts.append(path)
    return sorted(scripts), sorted(workflows)


def validate(root: Path) -> tuple[list[Finding], int]:
    findings: list[Finding] = []
    scripts, workflows = sources(root)
    for path in scripts:
        relative = path.relative_to(root).as_posix()
        findings.extend(scan(path.read_text(encoding="utf-8"), relative))
    for path in workflows:
        relative = path.relative_to(root).as_posix()
        findings.extend(scan_workflow(path.read_text(encoding="utf-8"), relative))
    return findings, len(scripts) + len(workflows)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=ROOT)
    args = parser.parse_args()

    findings, scanned = validate(args.root)
    if findings:
        for finding in findings:
            print(f"error: {finding}", file=sys.stderr)
        return 1

    print(f"bash baseline policy valid: {scanned} file(s) scanned")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
