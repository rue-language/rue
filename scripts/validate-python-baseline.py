#!/usr/bin/env python3
"""Hold repository Python to the interpreter floor AGENTS.md documents.

Every gate, generator, and test runner under `scripts/` is a standalone
`#!/usr/bin/env python3` program, so the interpreter is whatever the host puts
first on PATH. CI's is generous -- `ubuntu-latest` provides 3.12.3 and
`macos-15` provides 3.14.6 -- and a developer's is not: macOS ships
`/usr/bin/python3` as 3.9.6. A construct newer than the host interpreter is
therefore not a portability nicety, it is a traceback before the program does
any of its work, on a machine CI cannot see. That is how `tomllib`, stdlib
only from 3.11, killed the premerge suite on every stock Mac while CI stayed
green (RUE-1509); the Bash analogue is `mapfile` in test.sh (RUE-1506).

The floor is a uniform 3.9 -- exactly what a stock Mac provides, so meeting it
costs a contributor nothing. It was briefly 3.11 for the `tomllib` consumers,
with version-guard machinery holding everything else to 3.9; RUE-1524 pointed
those consumers at a Buck-materialized JSON twin of the TOML instead, so no
file needs more than 3.9 and the guard machinery is retired. A construct newer
than the floor is simply an error. The only escape is a reviewed
`# python-baseline-ok: <reason>` annotation -- it has to sit in a real comment,
and it silences its whole line -- or raising the floor in AGENTS.md and here,
deliberately.

WHAT IS SCANNED. Every `.py` file, plus every extensionless file carrying a
python shebang -- `scripts/ci-test-failure-report` is a 126-line program CI runs
twice, and the interpreter is chosen by its shebang, not by its name. Vendored
and build directories and nested checkouts are excluded.

WHAT THIS SCAN CANNOT SEE, stated plainly so the gate is not read as a proof:

  - It is only as strict as the interpreter running it. Syntax newer than the
    scanning interpreter is a parse error, and BELOW the floor a parse error
    cannot be told apart from syntax the floor legitimately allows -- so such a
    file is reported as unscanned rather than guessed at, and the authoritative
    run is the `fmt` job's CI step, which is at or above the floor.
  - Symmetrically, ABOVE the floor it is too permissive: a scanner on 3.14
    parses a PEP 701 f-string that 3.9 would reject and calls the file clean.
    The Bash gate closes its version of that gap by parsing with a real 3.2 on
    ci.yml's macos-15 leg (RUE-1512). The Python counterpart is
    `ast.parse(source, feature_version=(3, 9))`, which runs on whatever
    interpreter is here -- it has existed since 3.8. It is deliberately NOT
    used, and the reason is coverage rather than availability: `feature_version`
    is documented best-effort, it gates only what CPython's own parser gates by
    version, and it cannot manufacture grammar the RUNNING interpreter lacks --
    so it would still pass PEP 701 f-strings and every method call while adding
    a second, weaker parser's opinion. Parsing with a real 3.9 in CI is the
    change that would make a sound check possible; until then this is stated
    rather than approximated. What the gate CAN see without any of that --
    version-gated imports, attributes, and grammar with a distinct AST node --
    it sees from any host, which is why the table is not redundant either.
  - Grammar with no distinct AST node is invisible: PEP 701 f-strings (3.12)
    and PEP 696 TypeVar defaults (3.13) parse into the same nodes as their
    older spellings on an interpreter new enough to accept them.
  - The API table covers module attributes and imports, not method calls.
    `Path.walk()`, `Exception.add_note()`, `int.bit_count()`,
    `glob(root_dir=...)` and their kind need a type to resolve the receiver,
    which this scanner deliberately does not have -- a name-based rule would
    fire on every method that happened to share the name.
  - `importlib.import_module("tomllib")` and `__import__` evade it, as any
    static scan of a dynamic import must.
  - An annotation silences its whole line, so a construct inside a multi-line
    call is annotated at the line the scanner reports, not at the argument.

The table is curated, not exhaustive: only constructs whose introducing version
is unambiguous. A rule that had to hedge would be answered with an annotation
rather than a fix, which teaches reviewers to ignore the gate.

Annotate a reviewed exception with `# python-baseline-ok: <reason>`. It has to
sit in a real comment and it silences its whole line.
"""

from __future__ import annotations

import argparse
import ast
import re
import sys
from pathlib import Path
from typing import NamedTuple

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import walk_files


ROOT = Path(__file__).resolve().parent.parent

# The one floor: what the repository requires, which is also what a stock
# Mac's `/usr/bin/python3` (3.9.6) provides. AGENTS.md states this in prose and
# `check_docs` holds the two copies together, so the gate cannot outlive the
# documentation that tells a contributor what runs the tooling. RUE-1524
# retired the short-lived 3.11 floor and the version-guard machinery that a
# split floor required.
FLOOR = (3, 9)

SCANNER = (sys.version_info[0], sys.version_info[1])

ALLOW = re.compile(r"#\s*python-baseline-ok:\s*(?P<reason>\S.*?)\s*$")

# `#!/usr/bin/env python3`, `#!/usr/bin/python3`, `#!/usr/bin/env -S python3 -u`.
PYTHON_SHEBANG = re.compile(r"^#!\s*\S*(?:\s+-\S+)*\s*(?:\S*/)?python[\d.]*\b")

# The AGENTS.md section that owns the floor, and the sentence within it. Anchored
# to the section so an unrelated line elsewhere in a long file cannot satisfy the
# check by accident.
FLOOR_SECTION = "## Repository tooling baseline"
DOCUMENTED_FLOOR = re.compile(r"requires? Python (\d+)\.(\d+) or newer")

# Other documents are checked for a CONFLICTING copy of the same sentence.
# `docs/process/build-cache.md` states a different Python number on purpose --
# the remote worker image's, for the Buck prelude's rustc wrapper -- and says so;
# it does not use this sentence, and must not start to.
OTHER_DOCS = ("CONTRIBUTING.md", "README.md", "docs")


# Stdlib modules by the version that introduced them. An import of one of these
# is the failure that started this: it raises ModuleNotFoundError at import
# time, before any of the program's own error handling exists.
MODULE_SINCE = {
    "graphlib": (3, 9),
    "zoneinfo": (3, 9),
    "tomllib": (3, 11),
    "wsgiref.types": (3, 11),
    "dbm.sqlite3": (3, 13),
    "annotationlib": (3, 14),
    "compression": (3, 14),
    "concurrent.interpreters": (3, 14),
}

# Names within an existing module, which fail later and less clearly than a
# missing module: an AttributeError somewhere down a call path.
API_SINCE = {
    ("dataclasses", "KW_ONLY"): (3, 10),
    ("itertools", "pairwise"): (3, 10),
    ("typing", "ParamSpec"): (3, 10),
    ("typing", "TypeAlias"): (3, 10),
    ("typing", "TypeGuard"): (3, 10),
    ("asyncio", "TaskGroup"): (3, 11),
    ("asyncio", "timeout"): (3, 11),
    ("contextlib", "chdir"): (3, 11),
    ("datetime", "UTC"): (3, 11),
    ("enum", "StrEnum"): (3, 11),
    ("hashlib", "file_digest"): (3, 11),
    ("math", "cbrt"): (3, 11),
    ("math", "exp2"): (3, 11),
    ("operator", "call"): (3, 11),
    ("typing", "LiteralString"): (3, 11),
    ("typing", "Never"): (3, 11),
    ("typing", "NotRequired"): (3, 11),
    ("typing", "Required"): (3, 11),
    ("typing", "Self"): (3, 11),
    ("typing", "assert_never"): (3, 11),
    ("typing", "assert_type"): (3, 11),
    ("itertools", "batched"): (3, 12),
    ("random", "binomialvariate"): (3, 12),
    ("sys", "monitoring"): (3, 12),
    ("typing", "override"): (3, 12),
    ("base64", "z85encode"): (3, 13),
    ("copy", "replace"): (3, 13),
    ("os", "process_cpu_count"): (3, 13),
    ("typing", "ReadOnly"): (3, 13),
    ("typing", "TypeIs"): (3, 13),
    ("warnings", "deprecated"): (3, 13),
}

# Builtins. Only the unambiguous ones: a rule keyed on a bare lowercase name
# would fire on any local variable that happened to share it.
BUILTIN_SINCE = {
    "ExceptionGroup": (3, 11),
    "BaseExceptionGroup": (3, 11),
}

# Keyword arguments added to a call that has existed forever. `zip(strict=True)`
# on 3.9 is a TypeError at the call site, which reads like a bug in the caller.
# The flag says whether the callee must be a bare name: `zip` must, because
# `anything.zip(a, strict=True)` is somebody else's method, while `dataclass`
# is as legitimate spelled `dataclasses.dataclass`.
KEYWORD_SINCE = {
    ("zip", "strict"): ((3, 10), True),
    ("dataclass", "slots"): ((3, 10), False),
    ("dataclass", "kw_only"): ((3, 10), False),
    ("dataclass", "match_args"): ((3, 10), False),
}


class Construct(NamedTuple):
    description: str
    since: tuple[int, int]
    fix: str


class Finding(NamedTuple):
    path: str
    line: int
    construct: Construct

    def __str__(self) -> str:
        needs = version_text(self.construct.since)
        return (
            f"{self.path}:{self.line}: {self.construct.description} needs "
            f"Python {needs}, above the repository floor of "
            f"{version_text(FLOOR)}. Instead, {self.construct.fix} -- annotate "
            "a reviewed exception with `# python-baseline-ok: <reason>`, or "
            "raise the floor in AGENTS.md and here, deliberately."
        )


class Unscanned(NamedTuple):
    """A file this interpreter could not parse, and could not judge.

    Below the floor, a parse error is ambiguous: PEP 614's relaxed decorator
    grammar needs 3.9, which the floor allows, so a 3.8 host cannot tell a
    policy violation from a file it is simply too old to read. Guessing would
    produce a finding that is wrong about the version, wrong about the floor,
    and backwards about the fix -- the very CI-lenient/developer-strict split
    this policy exists to close, inverted. So it says what it could not do and
    leaves the verdict to the CI step.
    """

    path: str
    line: int
    reason: str

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: not scanned -- Python "
            f"{version_text(SCANNER)} cannot parse it ({self.reason}), and this "
            f"host is below the floor of {version_text(FLOOR)}, so a parse error "
            "here does not distinguish a violation from syntax the floor allows. "
            "The `fmt` job's CI step runs at or above the floor and is "
            "authoritative."
        )


def version_text(version: tuple[int, int]) -> str:
    return f"{version[0]}.{version[1]}"


def module_aliases(tree: ast.Module) -> dict[str, str]:
    """Local name -> module it refers to, for `import x` and `import x as y`.

    `import os.path` binds `os`, not `os.path`, so the binding name and the
    module it resolves to are not the same string and cannot be conflated.
    """
    aliases: dict[str, str] = {}
    for node in ast.walk(tree):
        if not isinstance(node, ast.Import):
            continue
        for alias in node.names:
            if alias.asname:
                aliases[alias.asname] = alias.name
            else:
                top = alias.name.split(".")[0]
                aliases[top] = top
    return aliases


def constructs(tree: ast.Module) -> list[tuple[int, Construct]]:
    """Every version-gated construct in one parsed module, with its line."""
    found: list[tuple[int, Construct]] = []
    aliases = module_aliases(tree)

    def record(line: int, description: str, since: tuple[int, int], fix: str) -> None:
        found.append((line, Construct(description, since, fix)))

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                since = MODULE_SINCE.get(alias.name)
                if since:
                    record(
                        node.lineno,
                        f"the `{alias.name}` module",
                        since,
                        "read the data with a module that predates the floor",
                    )
        elif isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
            since = MODULE_SINCE.get(node.module)
            if since:
                record(
                    node.lineno,
                    f"the `{node.module}` module",
                    since,
                    "import a module that predates the floor",
                )
            for alias in node.names:
                dotted = f"{node.module}.{alias.name}"
                since = MODULE_SINCE.get(dotted)
                if since:
                    record(
                        node.lineno,
                        f"the `{dotted}` module",
                        since,
                        "import a module that predates the floor",
                    )
                since = API_SINCE.get((node.module, alias.name))
                if since:
                    record(
                        node.lineno,
                        f"`{dotted}`",
                        since,
                        "use the spelling available at the floor",
                    )
        elif isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
            module = aliases.get(node.value.id)
            since = API_SINCE.get((module, node.attr)) if module else None
            if since:
                record(
                    node.lineno,
                    f"`{module}.{node.attr}`",
                    since,
                    "use the spelling available at the floor",
                )
        elif isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load):
            since = BUILTIN_SINCE.get(node.id)
            if since:
                record(
                    node.lineno,
                    f"the `{node.id}` builtin",
                    since,
                    "raise and catch an ordinary exception",
                )
        elif isinstance(node, ast.Call):
            bare = isinstance(node.func, ast.Name)
            if bare:
                name = node.func.id
            elif isinstance(node.func, ast.Attribute):
                name = node.func.attr
            else:
                name = None
            for keyword in node.keywords:
                rule = KEYWORD_SINCE.get((name, keyword.arg)) if name else None
                if rule is None:
                    continue
                since, bare_only = rule
                if bare_only and not bare:
                    continue
                record(
                    node.lineno,
                    f"the `{name}({keyword.arg}=...)` argument",
                    since,
                    "drop the argument and do the work explicitly",
                )

    found.extend(syntax_constructs(tree))
    return found


def syntax_constructs(tree: ast.Module) -> list[tuple[int, Construct]]:
    """Grammar that a newer interpreter added.

    The node classes are fetched with `getattr` because they do not exist on
    every interpreter this scanner must itself run on: `ast.Match` arrived in
    3.10 and `ast.TypeAlias` in 3.12, so naming them directly would make the
    gate crash on exactly the old hosts it exists to protect.
    """
    rules = (
        (getattr(ast, "Match", ()), "a `match` statement", (3, 10), "use `if`/`elif`"),
        (
            getattr(ast, "TryStar", ()),
            "an `except*` group",
            (3, 11),
            "catch the exceptions individually",
        ),
        (
            getattr(ast, "TypeAlias", ()),
            "a PEP 695 `type` alias",
            (3, 12),
            "assign the alias as an ordinary name",
        ),
    )
    found: list[tuple[int, Construct]] = []
    for node in ast.walk(tree):
        for node_type, description, since, fix in rules:
            if node_type and isinstance(node, node_type):
                found.append((node.lineno, Construct(description, since, fix)))
        # PEP 695 parameter lists on a def or class. The attribute is absent
        # before 3.12, where such a file would not have parsed in the first
        # place.
        if getattr(node, "type_params", None):
            found.append(
                (
                    node.lineno,
                    Construct(
                        "PEP 695 type parameters",
                        (3, 12),
                        "declare a module-level `TypeVar`",
                    ),
                )
            )
    return found


def scan(source: str, path: str) -> tuple[list[Finding], list[Unscanned]]:
    """Findings in one Python file, and whether it could be judged at all."""
    try:
        tree = ast.parse(source)
    except SyntaxError as error:
        line = error.lineno or 1
        reason = error.msg
        if SCANNER < FLOOR:
            # Ambiguous from here: the floor may allow grammar this interpreter
            # cannot read.
            return [], [Unscanned(path, line, reason)]
        # At or above the floor, unparseable means newer than the floor, or
        # malformed. Either is a finding, and neither claims a version the
        # scanner cannot know.
        return (
            [
                Finding(
                    path,
                    line,
                    Construct(
                        f"syntax Python {version_text(SCANNER)} cannot parse "
                        f"({reason})",
                        FLOOR,
                        "use syntax the floor accepts, or fix the file",
                    ),
                )
            ],
            [],
        )

    lines = source.splitlines()
    findings: dict[tuple[int, str], Finding] = {}
    for line, construct in constructs(tree):
        if construct.since <= FLOOR:
            continue
        raw = lines[line - 1] if 0 < line <= len(lines) else ""
        _, comment = split_comment(raw)
        if ALLOW.search(comment):
            continue
        findings[(line, construct.description)] = Finding(path, line, construct)
    return [findings[key] for key in sorted(findings)], []


def split_comment(line: str) -> tuple[str, str]:
    """Split a source line into code and its trailing comment.

    A `#` inside a string literal does not open a comment, so an annotation is
    only an annotation outside quotes -- otherwise any file quoting this
    policy's own prose would silence itself.
    """
    quote = ""
    index = 0
    while index < len(line):
        character = line[index]
        if quote:
            if character == "\\":
                index += 2
                continue
            if character == quote:
                quote = ""
        elif character in "'\"":
            quote = character
        elif character == "#":
            return line[:index], line[index:]
        index += 1
    return line, ""


def sources(root: Path) -> list[Path]:
    """Every Python file under `root`, by extension or by shebang.

    The shebang half is not decoration. `scripts/ci-test-failure-report` has no
    suffix and is run twice by ci.yml, and the shebang is what decides which
    interpreter reads it -- the same reason the Bash baseline policy discovers
    its scripts that way. A name is not a contract; `#!` is.
    """
    found: list[Path] = []
    for path in walk_files(root):
        if path.is_symlink() or not path.is_file():
            continue
        if path.suffix == ".py":
            found.append(path)
            continue
        if path.suffix:
            continue
        try:
            with path.open("r", encoding="utf-8", errors="strict") as handle:
                first = handle.readline()
        except (OSError, UnicodeDecodeError):
            continue
        if PYTHON_SHEBANG.match(first):
            found.append(path)
    return sorted(found)


def strip_fences(text: str) -> str:
    """Blank out fenced code blocks.

    A floor quoted inside an example is an example, not a claim, and the
    section may quote spellings in fenced blocks.
    """
    kept = []
    fenced = False
    for line in text.splitlines():
        if line.lstrip().startswith("```"):
            fenced = not fenced
            kept.append("")
            continue
        kept.append("" if fenced else line)
    return "\n".join(kept)


def floor_section(text: str) -> str | None:
    """The body of AGENTS.md's tooling-baseline section, or None."""
    lines = text.splitlines()
    try:
        start = lines.index(FLOOR_SECTION)
    except ValueError:
        return None
    for offset, line in enumerate(lines[start + 1 :], start=start + 1):
        if line.startswith("## "):
            return "\n".join(lines[start + 1 : offset])
    return "\n".join(lines[start + 1 :])


def check_docs(root: Path) -> list[str]:
    """Hold the documented floor and this gate's FLOOR together.

    A floor nobody wrote down cannot be acted on, and a floor the gate no longer
    agrees with is worse than none: the contributor installs one version and the
    gate demands another. The claim is anchored to its own section and read
    outside code fences, so an unrelated sentence or a quoted example cannot
    satisfy it. Other documents are checked for a conflicting copy -- the
    failure mode is not an absent floor but two of them.
    """
    agents = root / "AGENTS.md"
    try:
        text = agents.read_text(encoding="utf-8")
    except OSError:
        return [f"{agents}: unreadable, so the documented Python floor cannot be read"]

    section = floor_section(text)
    if section is None:
        return [
            f'AGENTS.md: no "{FLOOR_SECTION}" section. The floor must live in '
            "one named place a contributor can be sent to."
        ]
    match = DOCUMENTED_FLOOR.search(strip_fences(section))
    if not match:
        return [
            f'AGENTS.md, "{FLOOR_SECTION}": no documented Python floor. It must '
            f'state "requires Python {version_text(FLOOR)} or newer" outside a '
            "code fence, so a contributor hitting this gate knows what to install."
        ]
    documented = (int(match.group(1)), int(match.group(2)))
    problems = []
    if documented != FLOOR:
        problems.append(
            f"AGENTS.md documents a Python floor of {version_text(documented)} "
            f"but this gate enforces {version_text(FLOOR)}. Move both together."
        )
    problems.extend(conflicting_docs(root, documented))
    return problems


def conflicting_docs(root: Path, documented: tuple[int, int]) -> list[str]:
    """Any other document stating a different floor with the same sentence."""
    candidates: list[Path] = []
    for name in OTHER_DOCS:
        path = root / name
        if path.is_dir():
            candidates.extend(sorted(path.rglob("*.md")))
        elif path.is_file():
            candidates.append(path)
    problems = []
    for path in candidates:
        try:
            body = strip_fences(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError):
            continue
        for match in DOCUMENTED_FLOOR.finditer(body):
            stated = (int(match.group(1)), int(match.group(2)))
            if stated != documented:
                problems.append(
                    f"{path.relative_to(root).as_posix()}: states a Python floor "
                    f"of {version_text(stated)}, but AGENTS.md documents "
                    f"{version_text(documented)}. One floor, one place."
                )
    return problems


def validate(root: Path) -> tuple[list[Finding], list[Unscanned], list[str], int]:
    findings: list[Finding] = []
    unscanned: list[Unscanned] = []
    files = sources(root)
    for path in files:
        relative = path.relative_to(root).as_posix()
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        file_findings, file_unscanned = scan(source, relative)
        findings.extend(file_findings)
        unscanned.extend(file_unscanned)
    return findings, unscanned, check_docs(root), len(files)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=ROOT)
    args = parser.parse_args()

    findings, unscanned, documentation, scanned = validate(args.root)
    for note in unscanned:
        print(f"note: {note}", file=sys.stderr)
    for problem in documentation:
        print(f"error: {problem}", file=sys.stderr)
    for finding in findings:
        print(f"error: {finding}", file=sys.stderr)
    if findings or documentation:
        return 1

    summary = (
        f"python baseline policy valid: {scanned} file(s) scanned against "
        f"Python {version_text(FLOOR)}"
    )
    if unscanned:
        summary += (
            f"; {len(unscanned)} unparseable here and left to the CI step, which "
            f"runs at or above the floor (this is Python {version_text(SCANNER)})"
        )
    print(summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
