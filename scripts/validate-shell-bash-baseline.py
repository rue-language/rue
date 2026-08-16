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

TWO CHECKS (RUE-1512), and neither implies the other's coverage.

The table above names constructs. That is its strength -- it can say which
version introduced a thing and how to spell it on 3.2 -- and it is also its
ceiling: it cannot flag a file that does not PARSE, because a syntax error is
not a construct and there is nothing to point at. RUE-1511 shipped one into a
branch, in this shape:

    if ! covered="$("$buck2" uquery \\
        "attrfilter(labels, a, //crates/...) except (attrfilter(...))" \\
        2>/dev/null); then

The closing `"` that should follow the `)` is missing, so the double quote
opened after `covered=` never closes. Bash reads to end of file looking for it
and reports:

    line 9: unexpected EOF while looking for matching `"'
    line 12: syntax error: unexpected end of file

Neither line is the mistake -- they are where the PARSER gave up, several lines
past it and usually at the end of the file, which is why the error reads as a
puzzle. Nothing on the table is used, so the gate answered `bash baseline
policy valid: 53 file(s) scanned` and the break was found by running the suite
by hand.

So the second check is `bash -n`: parse the script, execute nothing. It needs
no name for what it finds, which is exactly the class the table cannot
enumerate. It does not replace the table either -- `mapfile` and `${v:1:-1}`
parse perfectly on 3.2 and then exit 127 or answer wrongly -- so a file can be
flawlessly parseable and still dead on a Mac. Run both.

WHICH BASH, and why it matters more than it looks. Measured, not assumed:

  both 3.2 and 5.2 reject   an unbalanced quote, an unterminated `if`, an
                            unclosed heredoc -- the RUE-1511 class. ANY bash
                            catches these, so this half runs wherever a bash
                            exists, including Linux CI.
  only 3.2 rejects          `;;&`, `;&`, `coproc`. Bash 5 parses all three
                            happily, so a `bash -n` on Linux is genuinely
                            weaker and must not be reported as though it were
                            not.
  neither rejects           `mapfile`, `declare -A`, `declare -g`, `${v^^}` --
                            syntax on every version, wrong behaviour or a
                            runtime error on 3.2. The table's territory, not
                            the parser's.
  both reject WRONGLY       `@(a|b)` after a runtime `shopt -s extglob`. See
                            the annotation below; this is the one direction in
                            which the parse check over-catches.

Note what this does NOT say: the RUE-1511 construct is not a Bash 4 feature and
3.2 is not what broke on it. The comment left at the repaired site said 3.2
"cannot parse a `$(...)` whose body spans a line continuation and contains a
quoted argument with parentheses"; it can, and so can 5.2, once the quote is
balanced. The bug was ordinary, which is the point -- an ordinary syntax error
is invisible to a table of exotic constructs.

So the parse check runs with the strictest bash the host has, prefers a 3.2
when one exists, and says which it used. On ci.yml's macos-15 leg -- the only
runner whose /bin/bash IS a 3.2 -- it runs with `--require-baseline-bash`, so a
bash that moved or aged into 5.x fails the build instead of quietly downgrading
the check. A check that cannot fail where it runs is worse than no check,
because it is believed (RUE-1506).

The parse check has its own annotation, `# bash-parse-ok: <reason>`, and it is
file-level because a parse failure has no line worth attaching to. It exists
for one real case: `bash -n` parses without executing, so it never runs a
`shopt`, and

    shopt -s extglob
    case "$x" in @(abc|def)) echo yes ;; esac

RUNS correctly on 3.2.57 and is rejected by `bash -n` on that same 3.2.57.
There the file is right and the check is wrong. Keep the bar there: a genuine
syntax error must be fixed, not annotated, and the reason is reviewed here the
way `# bash-baseline-ok:` is. Every exemption is printed and counted, because
the annotation stops checking the whole file.

Workflow `run:` steps get the table only. A step's text is not final Bash: the
runner substitutes its `${{ }}` expressions first, and a block scalar's uniform
indentation is not part of the script (an indented heredoc terminator would
read as an unterminated heredoc). Parsing one as written can therefore differ
from what the runner parses in both directions, and a gate with false positives
is a gate that gets switched off. `actionlint` shellchecks those blocks in its
own job. Extending the parse check there needs the reconstruction done
properly, and is a separate decision.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple

sys.path.insert(0, str(Path(__file__).resolve().parent))
# SKIP_DIRECTORIES is re-exported: the tool tests read the prune policy
# through this module as the gate's own surface.
from gatelib import SKIP_DIRECTORIES, walk_files


ROOT = Path(__file__).resolve().parent.parent

# The interpreter this policy is written against. macOS has shipped GNU Bash
# 3.2.57 as `/bin/bash` since 2007 and will not ship a GPLv3 one, so the floor
# is a concrete binary at a concrete path rather than a version to hunt for.
BASELINE_BASH = "/bin/bash"
BASELINE_MAJOR = 3

# Names the one interpreter the parse check may use -- a hand-built 3.2 on a
# Linux host, or a deliberately wrong one in a test. When it is set, nothing
# else is tried: a fallback would make the override advisory, and an override
# that can be silently ignored is worse than none. It is checked for version,
# not for sanity: pointing it at macOS's /bin/sh passes the 3.x test, because
# that IS Bash 3.2, and then parses everything in POSIX mode. Point it at a
# bash.
BASELINE_ENV = "RUE_BASELINE_BASH"

# `#!/usr/bin/env bash`, `#!/bin/bash`, `#!/usr/bin/env -S bash -eu`.
BASH_SHEBANG = re.compile(r"^#!\s*\S*(?:\s+-\S+)*\s*(?:\S*/)?bash\b|^#!\S*/bash\b")

# Any shell. Only the PARSE check uses this: a `#!/bin/sh` script has a POSIX
# problem rather than a Bash-version one, so the construct table rightly
# ignores it -- but an unbalanced quote in it is the same syntax error, and
# reaches trunk green if nothing parses the file. Deliberately the pipefail
# gate's spelling, so the two gates cover the same set of files.
SHELL_SHEBANG = re.compile(r"^#!.*\b(?:ba|da|k|z)?sh\b")

ALLOW = re.compile(r"#\s*bash-baseline-ok:\s*(?P<reason>\S.*?)\s*$")

# The parse check's own escape, and it needs a separate one: a parse failure
# has no reliable line to annotate, since bash reports where it gave up rather
# than where the mistake is. This is file-level and silences the whole file.
PARSE_ALLOW = re.compile(r"#\s*bash-parse-ok:\s*(?P<reason>\S.*?)\s*$")

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


class Interpreter(NamedTuple):
    path: str
    version: str
    major: int

    @property
    def at_baseline(self) -> bool:
        """Whether this interpreter can reject what only Bash 3.2 rejects.

        A Bash 5 accepts `;;&`, `;&`, and `coproc`, so it is a real but weaker
        parser for this policy. The distinction is carried in the type rather
        than recomputed at each use, because every message that reports a clean
        parse has to say which of the two it was.
        """
        return self.major == BASELINE_MAJOR


def probe(candidate: str) -> Interpreter | None:
    """Identify `candidate` as a bash, by asking it rather than by its path."""
    try:
        result = subprocess.run(
            [candidate, "-c", 'echo "${BASH_VERSINFO[0]:-} ${BASH_VERSION:-}"'],
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    fields = result.stdout.split()
    # A non-bash interpreter leaves both variables unset, or answers with
    # something that is not a version at all.
    if len(fields) != 2 or not fields[0].isdigit():
        return None
    return Interpreter(candidate, fields[1], int(fields[0]))


def parse_interpreter() -> Interpreter | None:
    """A 3.2 if this host has one, else the first other bash it finds.

    Preferring rather than requiring, because the two tiers catch different
    things and the weaker one is worth having. An unbalanced quote -- the
    RUE-1511 break -- is a syntax error on every bash, so a Linux runner's 5.x
    catches it on every pull request; `;;&` is a syntax error only on 3.2, so
    that half waits for the macos-15 leg. Requiring 3.2 outright would trade
    the first for nothing, and reporting a 5.x as though it were the baseline
    would claim the second without having it.
    """
    override = os.environ.get(BASELINE_ENV)
    if override:
        return probe(override)
    candidates = [BASELINE_BASH]
    on_path = shutil.which("bash")
    if on_path and on_path not in candidates:
        candidates.append(on_path)
    # `/bin/bash` first, because that is the one macOS pins at 3.2 while a
    # developer's PATH may well put a Homebrew 5.x ahead of it -- and this
    # check wants the stricter parser, not the newer one.
    fallback = None
    for candidate in candidates:
        interpreter = probe(candidate)
        if interpreter is None:
            continue
        if interpreter.at_baseline:
            return interpreter
        fallback = fallback or interpreter
    return fallback


class ParseFailure(NamedTuple):
    path: str
    version: str
    detail: str

    def __str__(self) -> str:
        return (
            f"{self.path}: Bash {self.version} cannot parse this file -- "
            f"{self.detail}. The line named is where the PARSER gave up, not "
            "necessarily the mistake: look upward from it for an unbalanced "
            "quote or bracket, an unclosed heredoc, or a missing `fi`/`done`. "
            "If the file is correct and this is `bash -n` refusing syntax that "
            "a runtime `shopt -s extglob` would enable -- it parses without "
            "executing, so it never sees the `shopt` -- annotate the file with "
            "`# bash-parse-ok: <reason>`"
        )


class ParseExemption(NamedTuple):
    path: str
    reason: str

    def __str__(self) -> str:
        return f"{self.path}: not parsed (# bash-parse-ok: {self.reason})"


def parse_annotation(source: str) -> str | None:
    """The reviewed reason this file is exempt from `bash -n`, if any.

    File-level rather than per-line, because a parse failure has no trustworthy
    line to attach to. It has to sit in a real comment, so a file quoting this
    policy's own prose does not exempt itself.
    """
    for raw in source.splitlines():
        _, comment = split_code(raw)
        match = PARSE_ALLOW.search(comment)
        if match:
            return match.group("reason")
    return None


def parse_check(
    scripts: list[Path], root: Path, interpreter: Interpreter
) -> tuple[list[ParseFailure], list[ParseExemption]]:
    """Parse each script with `interpreter`, executing nothing.

    `bash -n` reads the file, builds the parse tree, and stops, so this costs a
    process spawn per script and cannot run any of their side effects. That
    matters here: these scripts drive builds, and a gate that ran them would be
    a gate nobody could afford to run on every commit.

    Not executing is also this check's one real blind spot in reverse. `shopt`
    takes effect when it RUNS, so a file that enables `extglob` and then uses
    `@(a|b)` executes correctly on 3.2 and is rejected by `bash -n` on the same
    interpreter. That file is right and the check is wrong, which is what
    `# bash-parse-ok:` is for.
    """
    failures: list[ParseFailure] = []
    exemptions: list[ParseExemption] = []
    for path in scripts:
        relative = path.relative_to(root).as_posix()
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        reason = parse_annotation(source)
        if reason is not None:
            exemptions.append(ParseExemption(relative, reason))
            continue
        try:
            result = subprocess.run(
                # `--` because a repository-root file named `-x.sh` would
                # otherwise be read as options, and the usage dump reported as
                # that file's syntax error.
                [interpreter.path, "-n", "--", relative],
                cwd=root,
                capture_output=True,
                text=True,
                check=False,
                # Parsing is linear and these are small files, so a hang means
                # something pathological. Fail it rather than sit until the CI
                # job's own timeout, where the cause is far less obvious.
                timeout=60,
            )
        except subprocess.TimeoutExpired:
            failures.append(
                ParseFailure(relative, interpreter.version, "`bash -n` timed out")
            )
            continue
        if result.returncode == 0:
            continue
        # Bash prefixes every diagnostic with the script name it was given.
        # Repeating it in a message that already opens with the path is noise.
        lines = []
        for line in result.stderr.splitlines():
            line = line.strip()
            if line.startswith(f"{relative}: "):
                line = line[len(relative) + 2 :]
            if line:
                lines.append(line)
        detail = "; ".join(lines) or f"`bash -n` exited {result.returncode}"
        failures.append(ParseFailure(relative, interpreter.version, detail))
    return failures, exemptions


class Sources(NamedTuple):
    # Bash-shebang scripts: the construct table's set, and parsed as well.
    bash: list[Path]
    # Other shell scripts -- a `#!/bin/sh` shebang, or a bare `.sh`. The table
    # skips these on purpose (a POSIX policy is not a Bash-version policy), but
    # a syntax error in one is the same bug, so the parse check takes them.
    shell: list[Path]
    workflows: list[Path]

    @property
    def parseable(self) -> list[Path]:
        return sorted(self.bash + self.shell)


def sources(root: Path) -> Sources:
    """Every shell script and workflow file under `root`."""
    scripts: list[Path] = []
    shell: list[Path] = []
    workflows: list[Path] = []
    for path in walk_files(root):
        here = path.parent
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
        # For the TABLE the shebang is the whole question: it decides which
        # bash runs the file, so a `.sh` without one is not that policy's
        # business. The parse check has no such dependence -- a syntax
        # error is one in any shell -- so it takes the wider set.
        if BASH_SHEBANG.match(first):
            scripts.append(path)
        elif path.suffix == ".sh" or SHELL_SHEBANG.match(first):
            shell.append(path)
    return Sources(sorted(scripts), sorted(shell), sorted(workflows))


class Report(NamedTuple):
    findings: list[Finding]
    failures: list[ParseFailure]
    exemptions: list[ParseExemption]
    # Files the table scanned: bash scripts plus macOS-capable workflows.
    scanned: int
    # Scripts the parse check is responsible for, parsed or not. Reported even
    # when it did not run, because "44 unparsed" is the honest summary of a
    # host with no bash and "53 scanned" would read like full coverage.
    scripts: int


def validate(root: Path, interpreter: Interpreter | None = None) -> Report:
    """Both checks over one discovery pass.

    Discovery is shared deliberately: the parse check's claim is that it covers
    at least the files the table does, so a second, subtly different walk would
    be a way for a script to fall between them.
    """
    findings: list[Finding] = []
    found = sources(root)
    for path in found.bash:
        relative = path.relative_to(root).as_posix()
        findings.extend(scan(path.read_text(encoding="utf-8"), relative))
    for path in found.workflows:
        relative = path.relative_to(root).as_posix()
        findings.extend(scan_workflow(path.read_text(encoding="utf-8"), relative))
    parseable = found.parseable
    failures, exemptions = (
        parse_check(parseable, root, interpreter) if interpreter else ([], [])
    )
    return Report(
        findings,
        failures,
        exemptions,
        len(found.bash) + len(found.workflows),
        len(parseable),
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=ROOT)
    parser.add_argument(
        "--require-baseline-bash",
        action="store_true",
        help=(
            f"fail unless the parse check ran on a Bash {BASELINE_MAJOR}.x, "
            "rather than accepting a newer one or none at all. Pass it "
            "wherever the host is supposed to have one -- ci.yml's macos-15 "
            "leg -- so an interpreter that moved or aged into 5.x fails the "
            "build instead of quietly downgrading the check."
        ),
    )
    args = parser.parse_args()

    interpreter = parse_interpreter()
    report = validate(args.root, interpreter)

    for finding in report.findings:
        print(f"error: {finding}", file=sys.stderr)
    for failure in report.failures:
        print(f"error: {failure}", file=sys.stderr)
    # An exemption is a reviewed decision, so it passes -- but it is never
    # silent, because the whole file stops being checked.
    for exemption in report.exemptions:
        print(f"note: {exemption}", file=sys.stderr)
    failed = bool(report.findings or report.failures)

    # An empty discovery set is a broken walk, never a clean tree: the gate
    # would report "0 file(s) scanned" and pass, which is the fail-open both
    # halves exist to prevent, and `--require-baseline-bash` would certify the
    # interpreter while certifying no coverage at all. ci.yml makes the same
    # call sixteen lines above this gate's own step, for the same reason
    # (RUE-1152). The library entry point `validate()` has no such floor, so a
    # test may still scan a deliberately empty tree.
    if report.scripts == 0:
        print(
            f"error: no shell scripts found under {args.root}. This gate is "
            "run against a repository checkout, where that is a broken "
            "discovery walk rather than a clean tree -- check the root "
            "argument and SKIP_DIRECTORIES.",
            file=sys.stderr,
        )
        failed = True

    # The parse check's strength varies by host, so every run states which of
    # the three it got. Silence would let "policy valid" mean any of them.
    if interpreter is None:
        looked = os.environ.get(BASELINE_ENV) or BASELINE_BASH
        shortfall = (
            f"no bash at {looked}, so {report.scripts} script(s) went "
            "unparsed. The construct table names constructs and cannot see a "
            "file that simply does not parse (RUE-1511)"
        )
    elif not interpreter.at_baseline:
        shortfall = (
            f"the parse check ran on Bash {interpreter.version}, above the "
            "3.2 baseline. It catches what every bash rejects "
            "-- unbalanced quotes, unterminated blocks -- but `;;&`, `;&`, and "
            "`coproc` parse here and are a syntax error on macOS, so that half "
            "belongs to ci.yml's macos-15 leg"
        )
    else:
        shortfall = ""

    if shortfall:
        if args.require_baseline_bash:
            print(f"error: {shortfall}", file=sys.stderr)
            failed = True
        else:
            print(f"note: {shortfall}", file=sys.stderr)

    if failed:
        return 1

    summary = f"bash baseline policy valid: {report.scanned} file(s) scanned"
    if interpreter is None:
        summary += f"; {report.scripts} script(s) NOT parsed -- no bash here"
    else:
        where = "at the baseline" if interpreter.at_baseline else "above the baseline"
        parsed = report.scripts - len(report.exemptions)
        summary += (
            f", {parsed} script(s) parsed by {interpreter.path} "
            f"(Bash {interpreter.version}, {where})"
        )
        if report.exemptions:
            summary += f", {len(report.exemptions)} annotated bash-parse-ok"
    print(summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
