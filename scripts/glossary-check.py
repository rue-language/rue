#!/usr/bin/env python3
"""Check that docs/formal/GLOSSARY.md lists every term and symbol the formal docs use (RUE-2461).

The formal core's reader-facing documents, and the doc-comments of the
mechanization's definition layers, may use a term or a symbol only when the
glossary has a row for it (its meaning, its upstream source and its class), so
no term enters the formal core untraced. The gate reads:

* the Markdown documents ``MARKDOWN_DOCS`` (those present on the branch);
* the doc-comments (``/-- … -/``, ``/-! … -/``) of every module the layer table
  in ``docs/formal/lean/RueCore/LayersMain.lean`` puts in ``DEFINITION_LAYERS``,
  and the names those modules declare.

It extracts three kinds of item and fails on any the glossary does not cover:

1. **Marked terms.** The documents reserve no marker for a defining
   occurrence: bold marks a definition ("A **use** of a place …") and also
   plain emphasis ("does **not**"), and italics likewise. So every bold span
   (``**…**``) and italic span (``*…*``, ``_…_``) outside fenced code blocks is
   extracted, and each must be a spelling in a term row or listed in the
   glossary's "Emphasis, not terms" section. Skipped mechanically: a span of
   more than ``MAX_TERM_WORDS`` words (a sentence or a lemma's statement); one
   ending in ``.``, ``:`` or ``?`` (a run-in heading); one opening with ``[``,
   ``(``, ``,`` or ``;`` or containing ``§`` (a status tag, a rule label, a
   citation); a single character, a number or an issue id; and one that is all
   code (an identifier, which rule 3 covers when it is a definition-layer name).
2. **Symbols.** Every non-ASCII character outside ``TYPOGRAPHY``, in the
   documents (code blocks included) and in the doc-comments. Each must appear
   in the first column of the symbols table.
3. **Definition-layer names.** Every ``def``, ``abbrev``, ``inductive`` and
   ``structure`` the definition-layer modules declare, qualified below
   ``RueCore``. Each must have a row in the Lean-names table.

Matching is on a normal form: lower case, backticks and asterisks removed,
whitespace collapsed, a leading article and trailing punctuation dropped. A
term cell lists its spellings separated by ``;``; a spelling's trailing
parenthesized qualifier (``residue (of a destructure)``) is not matched, so two
senses of one word can have two rows.

It also checks the rows themselves: each term or Lean-name row has one of the
classes in ``CLASSES`` (a Lean name may also be ``helper``); a *standard* term
links a FIELD.md section and a *Rue-specific* one cites a spec paragraph; every
Lean name a term row cites is declared in the package. The ``First use``
columns are generated: ``--write`` fills each from the documents (the section of
the first occurrence of any of the row's spellings in each document; for a Lean
name, the first code span that starts with it), and the check fails when one is
stale, so the column cannot rot.
"""

from __future__ import annotations

import argparse
import re
import sys
import unicodedata
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Sequence, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

ROOT = Path(__file__).resolve().parent.parent
FORMAL = ROOT / "docs" / "formal"

# The documents the glossary covers, with the short name the First-use column
# uses. A document not yet on the branch is skipped.
MARKDOWN_DOCS: List[Tuple[str, str]] = [
    ("01-core-calculus.md", "01"),
    ("03-metatheory.md", "03"),
    ("README.md", "README"),
    ("WHAT-IT-MEANS.md", "WHAT-IT-MEANS"),
    ("REDTEAM.md", "REDTEAM"),
    ("lean/README.md", "lean/README"),
    ("lean/GUIDE.md", "GUIDE"),
    ("lean/BRIDGE-SENSITIVITY.md", "BRIDGE-SENSITIVITY"),
]

# The layers whose doc-comments and declared names the glossary covers: L0
# syntax and L1 definitions. When the statement/proof split (RUE-2460) adds a
# statements layer, add its number here.
DEFINITION_LAYERS = (0, 1)

DECL_KINDS = ("def", "abbrev", "inductive", "structure")

MAX_TERM_WORDS = 4

# Non-ASCII characters that are typography, not mathematics: dashes, ellipses,
# quotation marks (Lean's «» quote a name), the section sign, the branches of a
# drawn tree, a drawn diagram's arrowhead, and spaces. The horizontal rule `─`
# is not here: it draws an inference rule's bar.
TYPOGRAPHY = set("–—‑…‘’“”«»§│└├┌┐┘┬┴┼▶") | {"\u00a0", "\u2009", "\u202f", "\u200b"}

BOLD = re.compile(r"\*\*(?=\S)(.+?)(?<=\S)\*\*")
ITALIC_STAR = re.compile(r"(?<![\w*\\])\*(?=[^\s*])([^*\n]+?)(?<=[^\s*])\*(?![\w*])")
ITALIC_UNDERSCORE = re.compile(r"(?<![\w\\])_(?=[^\s_])([^_\n]+?)(?<=[^\s_])_(?!\w)")
CODE_SPAN = re.compile(r"(`+)(.+?)\1")
FENCE = re.compile(r"^\s*(```|~~~)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
DOC_COMMENT = re.compile(r"/-[-!](.*?)-/", re.S)
LAYER_ENTRY = re.compile(r"\(`RueCore\.([\w.]+),\s*(\d+)\)")

EMPHASIS_HEADING = "Emphasis, not terms"

# The classes a term row may have, and the evidence its Source cell must carry:
# a standard term links a FIELD.md section (`[FIELD §n][Fn]`), a Rue-specific one
# cites a paragraph of the prose specification (`[spec X.Y:Z][s…]`).
CLASSES = ("standard", "Rue-specific, grounded", "ours, pending audit")
FIELD_LINK = re.compile(r"\[FIELD §\d+\]\[F\d+\]")
SPEC_LINK = re.compile(r"\[(?:spec )?\d+\.\d+:\d+[a-z]?\]\[s[\d.]+\]")


def normalize(text: str) -> str:
    text = unicodedata.normalize("NFC", text)
    text = text.replace("`", "").replace("*", "")
    text = re.sub(r"\s+", " ", text).strip().lower()
    text = re.sub(r"^(?:a|an|the) ", "", text)
    return text.strip(" ,;:.")


# --- sources -----------------------------------------------------------------


@dataclass
class Source:
    """One document: its short name and its lines, each with its section label."""

    short: str
    lines: List[Tuple[str, str]] = field(default_factory=list)  # (section, text)
    prose: List[str] = field(default_factory=list)  # text outside fenced code blocks


def section_label(heading: str) -> str:
    heading = heading.replace("`", "").replace("*", "")
    match = re.match(r"^(\d+(?:\.\d+)*)\.?\s", heading)
    if match:
        return "§" + match.group(1)
    heading = re.sub(r"\s*\(.*?\)\s*", " ", heading).strip()
    words = heading.split()
    label = " ".join(words[:5]) + (" …" if len(words) > 5 else "")
    return f"“{label}”"


def read_markdown(path: Path, short: str) -> Source:
    source = Source(short)
    section = "intro"
    in_fence = False
    for raw in path.read_text(encoding="utf-8").splitlines():
        if FENCE.match(raw):
            in_fence = not in_fence
            source.lines.append((section, raw))
            continue
        if not in_fence:
            heading = HEADING.match(raw)
            if heading and len(heading.group(1)) > 1:
                section = section_label(heading.group(2))
            source.prose.append(raw)
        source.lines.append((section, raw))
    return source


def definition_modules(lean_dir: Path) -> List[str]:
    table = (lean_dir / "RueCore" / "LayersMain.lean").read_text(encoding="utf-8")
    return [name for name, layer in LAYER_ENTRY.findall(table) if int(layer) in DEFINITION_LAYERS]


def read_lean_docs(lean_dir: Path, module: str) -> Source:
    path = lean_dir / "RueCore" / Path(*module.split(".")).with_suffix(".lean")
    text = path.read_text(encoding="utf-8")
    source = Source(f"`{module}`")
    for comment in DOC_COMMENT.findall(text):
        for line in comment.splitlines():
            source.lines.append((f"`{module}`", line))
            source.prose.append(line)
    return source


def declared_names(lean_dir: Path, modules: Sequence[str]) -> Dict[str, str]:
    """Every def/abbrev/inductive/structure name each module declares, qualified below RueCore."""
    xref = load_script("validate-lean-xref-index.py", __file__)
    names: Dict[str, str] = {}
    for module in modules:
        path = lean_dir / "RueCore" / Path(*module.split(".")).with_suffix(".lean")
        parsed = xref.parse_lean(path, lean_dir)
        for decl in parsed.declarations:
            if decl.kind not in DECL_KINDS:
                continue
            name = decl.name
            if name.startswith("RueCore."):
                name = name[len("RueCore."):]
            names.setdefault(name, module)
    return names


def all_declared(lean_dir: Path) -> set:
    """Every declaration and constructor of the package, qualified below RueCore."""
    xref = load_script("validate-lean-xref-index.py", __file__)
    names = set()
    for path in sorted((lean_dir / "RueCore").rglob("*.lean")):
        for decl in xref.parse_lean(path, lean_dir).declarations:
            name = decl.name
            names.add(name[len("RueCore."):] if name.startswith("RueCore.") else name)
    return names


# --- extraction --------------------------------------------------------------


def marked_spans(text: str) -> List[str]:
    """The bold and italic spans of one line of Markdown, per the module docstring's rule 1."""
    spans: List[str] = []
    for match in BOLD.finditer(text):
        spans.append(match.group(1))
    rest = BOLD.sub(" ", text)
    rest_no_code = CODE_SPAN.sub(lambda m: "\x00" * len(m.group(0)), rest)
    for pattern in (ITALIC_STAR, ITALIC_UNDERSCORE):
        for match in pattern.finditer(rest_no_code):
            spans.append(rest[match.start(1):match.end(1)])
    kept: List[str] = []
    for span in spans:
        stripped = span.strip()
        if not stripped or stripped[0] in "[(,;" or "**" in stripped or "§" in stripped:
            continue  # a status tag, a rule label or citation, a mis-paired marker
        if re.fullmatch(r"[\w]|\d+|RUE-\d+", stripped):
            continue  # a list marker, a number, an issue id
        if stripped[-1] in ".:?":
            continue
        if not CODE_SPAN.sub("", stripped).strip(" ,;'’s"):
            continue  # all code
        if len(stripped.split()) > MAX_TERM_WORDS:
            continue
        kept.append(stripped)
    return kept


def symbols(text: str) -> Iterable[str]:
    for ch in unicodedata.normalize("NFC", text):
        if ord(ch) > 127 and ch not in TYPOGRAPHY:
            yield ch


@dataclass
class Extracted:
    terms: Dict[str, Tuple[str, str]] = field(default_factory=dict)  # normal form -> (spelling, where)
    symbols: Dict[str, str] = field(default_factory=dict)  # char -> where
    names: Dict[str, str] = field(default_factory=dict)  # Lean name -> module


def extract(sources: Sequence[Source], names: Dict[str, str]) -> Extracted:
    out = Extracted(names=dict(names))
    for source in sources:
        for line in source.prose:
            for span in marked_spans(line):
                out.terms.setdefault(normalize(span), (span, source.short))
        for _, line in source.lines:
            for ch in symbols(line):
                out.symbols.setdefault(ch, source.short)
    return out


# --- the glossary --------------------------------------------------------------


@dataclass
class Table:
    kind: str  # "term" | "symbol" | "name"
    header: List[str]
    header_line: int
    rows: List[Tuple[int, List[str]]]  # (line index, cells)


def split_row(line: str) -> List[str]:
    body = line.strip()
    if body.startswith("|"):
        body = body[1:]
    if body.endswith("|") and not body.endswith("\\|"):
        body = body[:-1]
    cells = re.split(r"(?<!\\)\|", body)
    return [cell.strip() for cell in cells]


def table_kind(header: List[str]) -> Optional[str]:
    first = header[0].lower() if header else ""
    if first == "term":
        return "term"
    if first == "symbol":
        return "symbol"
    if first == "lean name":
        return "name"
    return None


def parse_glossary(lines: List[str]) -> Tuple[List[Table], List[str]]:
    tables: List[Table] = []
    emphasis_text: List[str] = []
    i = 0
    in_emphasis = False
    while i < len(lines):
        line = lines[i]
        heading = HEADING.match(line)
        if heading:
            in_emphasis = heading.group(2).strip() == EMPHASIS_HEADING
        if in_emphasis and (line.startswith("- ") or (line.startswith("  ") and line.strip())):
            # A list item, or a continuation line of one; items are `;`-separated.
            emphasis_text.append((";" if line.startswith("- ") else "") + line[2:])
        if line.startswith("|") and i + 1 < len(lines) and re.match(r"^\|\s*:?-{3,}", lines[i + 1]):
            header = split_row(line)
            kind = table_kind(header)
            j = i + 2
            rows: List[Tuple[int, List[str]]] = []
            while j < len(lines) and lines[j].startswith("|"):
                rows.append((j, split_row(lines[j])))
                j += 1
            if kind is not None:
                tables.append(Table(kind, header, i, rows))
            i = j
            continue
        i += 1
    emphasis = [part.strip() for part in " ".join(emphasis_text).split(";") if part.strip()]
    return tables, emphasis


def spellings(cell: str) -> List[str]:
    """A term cell's spellings, each without a trailing parenthesized qualifier.

    `residue (of a destructure)` and `residue (of a partial move)` are two rows
    for two senses of one word; both are spelled `residue` in the documents.
    """
    parts = [part.strip() for part in re.split(r"(?<!\\);", cell) if part.strip()]
    return [re.sub(r"\s*\([^()]*\)$", "", part) or part for part in parts]


def code_names(cell: str) -> List[str]:
    return [m.group(2).strip() for m in CODE_SPAN.finditer(cell)]


# --- first uses ------------------------------------------------------------------


def first_use(patterns: List[re.Pattern], sources: Sequence[Source]) -> str:
    found: List[str] = []
    for source in sources:
        for section, text in source.lines:
            flat = text.replace("`", "").replace("*", "")
            if any(p.search(flat) or p.search(text) for p in patterns):
                label = section if source.short.startswith("`") else f"{source.short} {section}"
                if source.short.startswith("`"):
                    # One entry for the definition layers: the first module that uses it.
                    if any(f.startswith("`") for f in found):
                        break
                found.append(label)
                break
    return "; ".join(found) if found else "—"


def word_pattern(spelling: str) -> Optional[re.Pattern]:
    text = spelling.replace("`", "").replace("*", "").strip()
    if not text:
        return None
    escaped = re.escape(text)
    left = r"(?<![\w])" if text[0].isalnum() or text[0] == "_" else ""
    right = r"(?![\w])" if text[-1].isalnum() or text[-1] == "_" else ""
    return re.compile(left + escaped + right, re.IGNORECASE if text[0].isalpha() else 0)


def row_patterns(kind: str, first_cell: str) -> List[re.Pattern]:
    if kind == "name":
        items = code_names(first_cell) or spellings(first_cell)
        # A Lean name counts as used where a code span starts with it, bare or
        # qualified (`Name`, `RueCore.Name`, `Name args`).
        return [re.compile(r"`(?:RueCore\.)?" + re.escape(item) + r"(?![\w.'])") for item in items]
    if kind == "symbol":
        items = code_names(first_cell) or spellings(first_cell)
        return [re.compile(re.escape(item)) for item in items if item]
    pats = []
    for item in spellings(first_cell):
        pattern = word_pattern(item)
        if pattern is not None:
            pats.append(pattern)
    return pats


# --- the check ---------------------------------------------------------------------


def load_sources(formal: Path, lean_dir: Path) -> Tuple[List[Source], Dict[str, str], List[str]]:
    sources: List[Source] = []
    for relative, short in MARKDOWN_DOCS:
        path = formal / relative
        if path.is_file():
            sources.append(read_markdown(path, short))
    modules = definition_modules(lean_dir)
    for module in modules:
        sources.append(read_lean_docs(lean_dir, module))
    names = declared_names(lean_dir, modules)
    return sources, names, modules


def run(formal: Path, lean_dir: Path, glossary: Path, write: bool, list_missing: bool) -> int:
    sources, names, modules = load_sources(formal, lean_dir)
    extracted = extract(sources, names)
    lines = glossary.read_text(encoding="utf-8").splitlines() if glossary.is_file() else []
    tables, emphasis = parse_glossary(lines)

    covered_terms = {normalize(e) for e in emphasis}
    covered_symbols: set = set()
    covered_names: set = set()
    for table in tables:
        for _, cells in table.rows:
            if not cells:
                continue
            if table.kind == "term":
                covered_terms.update(normalize(s) for s in spellings(cells[0]))
            elif table.kind == "symbol":
                covered_symbols.update(symbols(cells[0]))
            else:
                covered_names.update(code_names(cells[0]) or spellings(cells[0]))

    errors: List[str] = []
    errors.extend(check_rows(tables, lean_dir))
    for norm, (spelling, where) in sorted(extracted.terms.items()):
        if norm not in covered_terms:
            errors.append(f"term {spelling!r} ({where}): not in GLOSSARY.md (a term row, or the '{EMPHASIS_HEADING}' list)")
    for ch, where in sorted(extracted.symbols.items()):
        if ch not in covered_symbols:
            errors.append(f"symbol {ch!r} U+{ord(ch):04X} {unicodedata.name(ch, '?')} ({where}): not in a GLOSSARY.md symbols table")
    for name, module in sorted(extracted.names.items()):
        if name not in covered_names:
            errors.append(f"Lean name `{name}` ({module}): not in the GLOSSARY.md Lean-names table")

    # First-use columns.
    stale = 0
    new_lines = list(lines)
    for table in tables:
        try:
            col = [h.lower() for h in table.header].index("first use")
        except ValueError:
            errors.append(f"GLOSSARY.md line {table.header_line + 1}: a {table.kind} table without a 'First use' column")
            continue
        for index, cells in table.rows:
            want = first_use(row_patterns(table.kind, cells[0]), sources)
            if col >= len(cells):
                errors.append(f"GLOSSARY.md line {index + 1}: row has {len(cells)} cells, the header {len(table.header)}")
                continue
            if cells[col] != want:
                stale += 1
                cells = list(cells)
                cells[col] = want
                new_lines[index] = "| " + " | ".join(cells) + " |"
    if write:
        if new_lines != lines:
            glossary.write_text("\n".join(new_lines) + "\n", encoding="utf-8")
        print(f"glossary-check: wrote the First use column ({stale} cells changed)")
        stale = 0
    elif stale:
        errors.append(f"GLOSSARY.md: {stale} First use cell(s) are stale; run scripts/glossary-check.py --write")

    if list_missing:
        for error in errors:
            print(error)
        return 0
    if errors:
        for error in errors:
            print(f"glossary-check: {error}", file=sys.stderr)
        print(f"glossary-check: {len(errors)} problem(s)", file=sys.stderr)
        return 1
    counts = class_counts(tables)
    print(
        f"glossary-check: {len(extracted.terms)} marked spans, {len(extracted.symbols)} symbols, "
        f"{len(extracted.names)} definition-layer names ({len(modules)} modules), all in GLOSSARY.md; "
        + "; ".join(f"{kind} rows: " + ", ".join(f"{n} {c}" for c, n in sorted(per.items())) for kind, per in counts.items())
    )
    return 0


def column(table: Table, name: str) -> Optional[int]:
    lowered = [h.lower() for h in table.header]
    return lowered.index(name) if name in lowered else None


def check_rows(tables: Sequence[Table], lean_dir: Path) -> List[str]:
    """Each row's class, the evidence its class needs, and the Lean names it cites."""
    errors: List[str] = []
    declared = all_declared(lean_dir)
    for table in tables:
        if table.kind == "symbol":
            continue
        cls, src, lean = column(table, "class"), column(table, "source"), column(table, "lean")
        if cls is None:
            errors.append(f"GLOSSARY.md line {table.header_line + 1}: a {table.kind} table without a 'Class' column")
            continue
        for index, cells in table.rows:
            where = f"GLOSSARY.md line {index + 1}"
            if len(cells) != len(table.header):
                errors.append(f"{where}: {len(cells)} cells, the header has {len(table.header)}")
                continue
            value = cells[cls]
            allowed = CLASSES + (("helper",) if table.kind == "name" else ())
            chosen = next((c for c in allowed if value == c or value.startswith(c + " (")), None)
            if chosen is None:
                errors.append(f"{where}: class {value!r} is not one of {', '.join(allowed)}")
                continue
            if table.kind == "term" and src is not None:
                if chosen == "standard" and not FIELD_LINK.search(cells[src]):
                    errors.append(f"{where}: a standard term's Source must link a FIELD.md section")
                if chosen == "Rue-specific, grounded" and not SPEC_LINK.search(cells[src]):
                    errors.append(f"{where}: a Rue-specific term's Source must cite a spec paragraph")
            if lean is not None:
                for name in code_names(cells[lean]):
                    if name not in declared:
                        errors.append(f"{where}: Lean name `{name}` is not declared in RueCore")
    return errors


def class_counts(tables: Sequence[Table]) -> Dict[str, Dict[str, int]]:
    counts: Dict[str, Dict[str, int]] = {}
    for table in tables:
        cls = column(table, "class")
        if cls is None:
            continue
        per = counts.setdefault(table.kind, {})
        for _, cells in table.rows:
            value = cells[cls] if cls < len(cells) else ""
            key = next((c for c in CLASSES + ("helper",) if value == c or value.startswith(c + " (")), value)
            per[key] = per.get(key, 0) + 1
    return counts


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--formal", type=Path, default=FORMAL, help="docs/formal directory")
    parser.add_argument("--glossary", type=Path, default=None, help="the glossary (default: <formal>/GLOSSARY.md)")
    parser.add_argument("--write", action="store_true", help="refresh the glossary's First use columns")
    parser.add_argument("--list", action="store_true", help="print every problem and exit 0 (for drafting)")
    args = parser.parse_args(argv)
    formal = args.formal
    glossary = args.glossary or formal / "GLOSSARY.md"
    return run(formal, formal / "lean", glossary, args.write, args.list)


if __name__ == "__main__":
    sys.exit(main())
