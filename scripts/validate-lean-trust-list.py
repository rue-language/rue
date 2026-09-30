#!/usr/bin/env python3
"""Hold BUCK's `lean-ruecore` trust list to the theorems the package declares.

`root//:lean-ruecore` prints `#print axioms` for each name in its
`lean_package(trust = [...])` list, and fails when one of them is not a
constant. The target is deliberately not a test target (RUE-2241), so no CI
lane builds it, and a renamed theorem once left it broken on trunk for four
days (RUE-2525). This gate runs where CI does, and fails when:

* a listed name is not a theorem in `TRUST.md`, the generated and gated trust
  report of every theorem the package declares; or
* a spine theorem (`RueCore/Spec.lean`'s `spine`) is missing from the list,
  since the list exists to report what the claim rests on.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BUCK = ROOT / "BUCK"
DEFAULT_LEAN_DIR = ROOT / "docs" / "formal" / "lean"

PACKAGE = "lean-ruecore"
NAMESPACE = "RueCore."

# The `lean_package(name = "lean-ruecore", …)` call, up to its closing paren
# at column 0, and within it the `trust = [ … ]` list.
PACKAGE_CALL = re.compile(r'^lean_package\(\n(?P<body>.*?)^\)', re.M | re.S)
TRUST_LIST = re.compile(r'^\s*trust\s*=\s*\[(?P<items>.*?)\]', re.M | re.S)
QUOTED = re.compile(r'"([^"]+)"')
# A `TRUST.md` row: | `name` | `Module` | axioms |
TRUST_ROW = re.compile(r'^\|\s*`(?P<name>[^`]+)`\s*\|\s*`RueCore[^`]*`\s*\|', re.M)
# The `spine` list in Spec.lean: (`RueCore.name, ``name_stmt)
SPINE_DEF = re.compile(r'^def spine\b[^\n]*\n(?P<body>.*?)^\]', re.M | re.S)
SPINE_ENTRY = re.compile(r'\(`(?P<name>RueCore\.[\w.\']+)\s*,')


def trust_list(buck: Path) -> list[str]:
    for call in PACKAGE_CALL.finditer(buck.read_text()):
        body = call.group("body")
        if f'name = "{PACKAGE}"' not in body:
            continue
        match = TRUST_LIST.search(body)
        if match is None:
            raise ValueError(f"{buck}: `{PACKAGE}` has no `trust = [...]` list")
        return QUOTED.findall(match.group("items"))
    raise ValueError(f"{buck}: no `lean_package(name = \"{PACKAGE}\", ...)` call")


def declared_theorems(trust_md: Path) -> set[str]:
    names = {NAMESPACE + m.group("name") for m in TRUST_ROW.finditer(trust_md.read_text())}
    if not names:
        raise ValueError(f"{trust_md}: found no theorem rows; is this the generated trust report?")
    return names


def spine_theorems(spec: Path) -> list[str]:
    match = SPINE_DEF.search(spec.read_text())
    if match is None:
        raise ValueError(f"{spec}: no `def spine` list")
    names = SPINE_ENTRY.findall(match.group("body"))
    if not names:
        raise ValueError(f"{spec}: `spine` names no theorem")
    return names


def errors(buck: Path, lean_dir: Path) -> list[str]:
    try:
        listed = trust_list(buck)
        declared = declared_theorems(lean_dir / "TRUST.md")
        spine = spine_theorems(lean_dir / "RueCore" / "Spec.lean")
    except (OSError, ValueError) as error:
        return [str(error)]
    problems: list[str] = []
    seen: set[str] = set()
    for name in listed:
        if name in seen:
            problems.append(f"{buck}: `{PACKAGE}`'s trust list names `{name}` twice")
        seen.add(name)
        if name not in declared:
            problems.append(
                f"{buck}: `{PACKAGE}`'s trust list names `{name}`, which is not a theorem "
                f"the package declares (TRUST.md); was it renamed or removed?"
            )
    for name in spine:
        if name not in seen:
            problems.append(
                f"{buck}: spine theorem `{name}` (RueCore/Spec.lean) is missing from "
                f"`{PACKAGE}`'s trust list"
            )
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--buck", type=Path, default=DEFAULT_BUCK, help="the root BUCK file")
    parser.add_argument("--lean-dir", type=Path, default=DEFAULT_LEAN_DIR, help="the Lake package directory")
    args = parser.parse_args(argv)
    problems = errors(args.buck, args.lean_dir)
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    if not problems:
        count = len(trust_list(args.buck))
        print(f"Lean trust list current: {count} names, every one declared, every spine theorem listed")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
