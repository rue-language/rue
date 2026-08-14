#!/usr/bin/env python3
"""Extract the runtime page's side-by-side source excerpts (ADR-0072 Decision 8).

The runtime dashboard publishes a ratio between gazette and two general-purpose
site generators, and Decision 8 asks it to show "side-by-side source and
template excerpts" so that "a corpus-specialized Rue program against
general-purpose tools" is something a reader can look at rather than only read
in a caption. This script produces those excerpts, and `website/build.sh` runs
it before Zola so the page renders them server-side.

WHY EXTRACTION RATHER THAN A PASTE
----------------------------------
The ports are the benchmark's actual input. Every other claim on the page is
guarded against drifting from them — the fixture identity digests the port
trees, the spot goldens digest the production templates they derive from, and
the cross-tool lane rebuilds the corpus with all three tools. A hand-pasted
excerpt would be the one thing on that page with no such guard, and the way it
would fail is the worst kind: it would keep rendering, keep looking right, and
describe a port that no longer exists.

WHY ANCHORS RATHER THAN MARKERS OR LINE RANGES
----------------------------------------------
Three mechanisms were available.

A LINE-RANGE MANIFEST is rejected outright: inserting a line above a range
silently slides it, so the page renders the wrong lines and nothing fails. That
is exactly the failure this script exists to prevent.

MARKER COMMENTS in the source files are robust, but they cannot be paid for
here. `examples/gazette/templates`, `performance/ports/zola/templates`, and
`performance/ports/hugo/layouts` are all inside `PORT_ROOTS` in
`scripts/gazette-corpus-diff.py`, and that digest is part of the recorded
fixture identity. Its comment is explicit that the sensitivity is deliberate:
"editing a comment in this file opens a new segment". So adding a marker comment
to a port template would end the current comparable stretch of the published
series — a discontinuity in the measurements, bought with a presentation
change. No excerpt is worth that.

CONTENT ANCHORS are what is used. Each excerpt names a start and an end
substring, each of which must match EXACTLY ONE line of the file. The excerpt is
the lines between them, inclusive. Nothing is written into the sources, so no
digest moves; and because the excerpt is always recomputed from the file's
current bytes, the page cannot show stale text. What it can show is text whose
framing moved, and that is what `must_contain` is for.

HOW STALENESS FAILS
-------------------
Every one of these is a non-zero exit, and `website/build.sh` runs under
`set -e`, so each one fails the site build rather than publishing a wrong or
partial panel:

  * a declared file does not exist;
  * an anchor matches no line — the block was renamed, moved to another file,
    or deleted;
  * an anchor matches more than one line — it stopped being an anchor, so
    which block it names is now ambiguous;
  * the end anchor matches above the start anchor;
  * a `must_contain` string is gone — the excerpt still extracts, but a claim
    the page makes about it no longer holds. This is the guard against the one
    failure anchors alone cannot catch: the block keeps its name and loses its
    substance. Every entry below is a sentence the page actually says.

The page reads the result through Zola's `load_data`, which is itself a hard
error when the file is missing, so a site built without this step fails too.
"""

from __future__ import annotations

import argparse
import json
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The excerpts the runtime page publishes.
#
# CHOICE OF SLICE. All four show one job: the recursive specification sidebar.
# It is the right slice for three reasons, and the alternatives were weighed
# against them. It is where the work is — roughly 700 of the corpus's ~800
# pages are specification pages, and the sidebar is re-rendered for every one of
# them, because both the `active` class and the recursion are gated on a prefix
# test against the CURRENT page's permalink. It is the one template all three
# ports had to solve rather than transcribe, so the three genuinely correspond
# and differ exactly where the languages do: a Tera macro recursing through
# `self::`, a Go partial recursing through `partial`, and gazette's port of the
# first. And it is where the subset shows: gazette has no `is` tests and no
# arithmetic, so the same test is spelled as a filter and the macro's unused
# `depth` parameter is gone.
#
# The blog byline was the other candidate and was rejected: gazette's dialect is
# close enough to Tera that its listing template is byte-identical to the Zola
# port's, so two of the three columns would have shown the same text.
EXCERPTS = [
    {
        "id": "sidebar-gazette",
        "tool": "gazette",
        "language": "Tera subset",
        "file": "examples/gazette/templates/spec/base.html",
        "start": "{% macro render_nav(",
        "end": "{% endmacro %}",
        # The page says the prefix test is evaluated by the engine and that the
        # recursion is gated on it. Both are the fairness constraint ADR-0072
        # Decision 3 puts on this template, and both are visible right here.
        "must_contain": [
            "starts_with(prefix=",
            "self::render_nav(",
            "section.subsections | sort",
        ],
    },
    {
        "id": "sidebar-zola",
        "tool": "Zola",
        "language": "Tera",
        "file": "performance/ports/zola/templates/spec/base.html",
        "start": "{% macro render_nav(",
        "end": "{% endmacro %}",
        # `is starting_with` rather than the filter: the Zola port is the
        # production template unchanged, which is the whole point of the leg.
        "must_contain": [
            "is starting_with(",
            "self::render_nav(",
            "section.subsections | sort",
        ],
    },
    {
        "id": "sidebar-hugo",
        "tool": "Hugo",
        "language": "Go text/template",
        "file": "performance/ports/hugo/layouts/_partials/spec-nav.html",
        "start": "$current := .current",
        "end": "</ul>",
        "must_contain": [
            "strings.HasPrefix",
            'partial "spec-nav.html"',
            'sort .section.Sections "Path"',
        ],
    },
    {
        # The fourth excerpt is not a fourth port. It is the answer to the
        # question the first three raise — the peers bring a general-purpose
        # template engine, so what does gazette bring? — and it is the
        # concrete form of "corpus-specialized": the filter chain ends in a
        # diagnostic rather than a pass-through, so the engine implements the
        # filters this corpus uses and refuses every other one.
        "id": "engine-starts-with",
        "tool": "gazette",
        "language": "Rue",
        "file": "examples/gazette/tmpl_eval.rue",
        "start": "// Tera spells this as the test",
        "end": 'fail(inout eng, line, "unknown filter", name);',
        "must_contain": [
            'equals_literal(name, "starts_with")',
            "unknown filter",
        ],
    },
]


class ExcerptError(Exception):
    """A declaration that no longer describes its source file."""


def sole_line(lines: list[str], needle: str, role: str, path: str) -> int:
    """The index of the one line containing `needle`, or an error.

    Both "no match" and "several matches" are failures, and the second is the
    less obvious one: an anchor that matches twice has stopped identifying a
    block, and picking the first match would quietly publish whichever one
    happened to sort earlier.
    """
    hits = [i for i, line in enumerate(lines) if needle in line]
    if not hits:
        raise ExcerptError(
            "%s: no line contains the %s anchor %r. The block was renamed, "
            "moved, or deleted; update the declaration in "
            "scripts/extract-source-excerpts.py to name the block as it is now."
            % (path, role, needle))
    if len(hits) > 1:
        raise ExcerptError(
            "%s: the %s anchor %r matches %d lines (%s), so it no longer "
            "identifies one block. Choose a substring unique to the block."
            % (path, role, needle, len(hits),
               ", ".join(str(i + 1) for i in hits)))
    return hits[0]


def dedent(lines: list[str]) -> list[str]:
    """Drop the indentation the whole block shares.

    A block lifted out of a function would otherwise render with its enclosing
    indentation, which reads as a formatting mistake in a narrow column.
    """
    widths = [len(line) - len(line.lstrip()) for line in lines if line.strip()]
    common = min(widths) if widths else 0
    return [line[common:] if line.strip() else "" for line in lines]


def extract(text: str, declaration: dict, path: str) -> dict:
    lines = text.splitlines()
    start = sole_line(lines, declaration["start"], "start", path)
    end = sole_line(lines, declaration["end"], "end", path)
    if end < start:
        raise ExcerptError(
            "%s: the end anchor %r is on line %d, above the start anchor %r on "
            "line %d. The block was reordered; the declaration no longer "
            "describes it."
            % (path, declaration["end"], end + 1, declaration["start"], start + 1))

    body = lines[start:end + 1]
    while body and not body[-1].strip():
        body.pop()
    excerpt = "\n".join(dedent(body))

    missing = [needle for needle in declaration.get("must_contain", [])
               if needle not in excerpt]
    if missing:
        raise ExcerptError(
            "%s: the excerpt %r no longer contains %s. The page states this as "
            "a fact about the port; either the port changed and the runtime "
            "page's caption must follow, or the declaration is naming the "
            "wrong block."
            % (path, declaration["id"],
               ", ".join(repr(needle) for needle in missing)))

    return {
        "tool": declaration["tool"],
        "language": declaration["language"],
        "path": declaration["file"],
        # Resolved rather than declared, so citing them on the page cannot go
        # stale: they are wherever the anchors were found in this build.
        "first_line": start + 1,
        "last_line": end + 1,
        "lines": len(body),
        "text": excerpt,
    }


def build(repo: str, declarations: list[dict]) -> dict:
    excerpts = {}
    for declaration in declarations:
        path = os.path.join(repo, declaration["file"])
        if not os.path.exists(path):
            raise ExcerptError(
                "%s does not exist, but the runtime page publishes an excerpt "
                "from it. Update scripts/extract-source-excerpts.py."
                % declaration["file"])
        with open(path, "r", encoding="utf-8") as handle:
            text = handle.read()
        excerpts[declaration["id"]] = extract(text, declaration, declaration["file"])
    return {
        "generator": "scripts/extract-source-excerpts.py",
        "excerpts": excerpts,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=REPO,
                        help="repository root the excerpt paths resolve against")
    parser.add_argument("--out", required=True,
                        help="JSON file the runtime template reads with load_data")
    options = parser.parse_args()

    try:
        payload = build(options.repo, EXCERPTS)
    except ExcerptError as failure:
        sys.stderr.write("source excerpts: %s\n" % failure)
        return 1

    with open(options.out, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print("  extracted %d source excerpt(s)" % len(payload["excerpts"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
