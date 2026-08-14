#!/usr/bin/env python3
"""Tests for the runtime page's source-excerpt extractor (RUE-1495).

The property under test is the one the mechanism was chosen for: an excerpt
declaration that has stopped describing its source file must FAIL, not render
something else. A wrong excerpt on the runtime page is worse than no excerpt,
because it looks exactly like a right one.

The last two tests run against the real repository, so the declarations shipped
in the extractor are checked against the ports as they stand rather than only
against fixtures.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "extract_source_excerpts",
    Path(__file__).resolve().parent / "extract-source-excerpts.py",
)
ex = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ex)

REPO = Path(__file__).resolve().parent.parent

SOURCE = "\n".join([
    "{# a comment above the block #}",
    "{% macro render_nav(section, current_path) %}",
    "<ul>",
    "    {% if current_path | starts_with(prefix=sub.permalink) %}",
    "    {{ self::render_nav(section=sub) }}",
    "    {% endif %}",
    "</ul>",
    "{% endmacro %}",
    "",
    "{# trailing content #}",
])

DECLARATION = {
    "id": "sidebar",
    "tool": "gazette",
    "language": "Tera subset",
    "file": "templates/spec/base.html",
    "start": "{% macro render_nav(",
    "end": "{% endmacro %}",
    "must_contain": ["starts_with(prefix=", "self::render_nav("],
}


def declaration(**overrides) -> dict:
    merged = dict(DECLARATION)
    merged.update(overrides)
    return merged


def fails(source: str, decl: dict) -> str:
    try:
        ex.extract(source, decl, decl["file"])
    except ex.ExcerptError as failure:
        return str(failure)
    raise AssertionError("expected an ExcerptError, got an excerpt")


# ---- the happy path --------------------------------------------------------

def test_the_block_between_the_anchors_is_extracted_inclusively() -> None:
    excerpt = ex.extract(SOURCE, DECLARATION, DECLARATION["file"])
    assert excerpt["text"].startswith("{% macro render_nav(")
    assert excerpt["text"].endswith("{% endmacro %}")
    assert "{# a comment above the block #}" not in excerpt["text"]
    assert "{# trailing content #}" not in excerpt["text"]


def test_the_cited_line_numbers_are_resolved_not_declared() -> None:
    # The page cites where the excerpt lives. Citing a declared range would be
    # the line-range manifest this mechanism exists to avoid, so the numbers
    # come from where the anchors were actually found.
    excerpt = ex.extract(SOURCE, DECLARATION, DECLARATION["file"])
    assert excerpt["first_line"] == 2
    assert excerpt["last_line"] == 8
    assert excerpt["lines"] == 7

    shifted = "\n".join(["{# one more line #}"] * 5 + [SOURCE])
    moved = ex.extract(shifted, DECLARATION, DECLARATION["file"])
    assert moved["first_line"] == 7
    assert moved["text"] == excerpt["text"]


def test_insertions_above_the_block_do_not_move_the_excerpt() -> None:
    # The whole reason a line-range manifest was rejected.
    shifted = "{# inserted #}\n{# inserted #}\n" + SOURCE
    assert (ex.extract(shifted, DECLARATION, DECLARATION["file"])["text"]
            == ex.extract(SOURCE, DECLARATION, DECLARATION["file"])["text"])


def test_shared_indentation_is_dropped() -> None:
    nested = "\n".join([
        "fn eval_filter() {",
        '    // Tera spells this as the test',
        '    if equals_literal(name, "starts_with") {',
        "        return true;",
        "    }",
        '    fail(inout eng, line, "unknown filter", name);',
        "}",
    ])
    excerpt = ex.extract(nested, declaration(
        start="// Tera spells this as the test",
        end='fail(inout eng, line, "unknown filter", name);',
        must_contain=["unknown filter"],
    ), "tmpl_eval.rue")
    assert excerpt["text"].startswith("// Tera spells")
    # The block's own shared indent goes; the nesting inside it stays.
    assert '\nif equals_literal(name, "starts_with") {\n' in excerpt["text"]
    assert "\n    return true;\n" in excerpt["text"]


# ---- staleness fails -------------------------------------------------------

def test_a_renamed_block_fails() -> None:
    renamed = SOURCE.replace("{% macro render_nav(", "{% macro render_sidebar(")
    assert "no line contains the start anchor" in fails(renamed, DECLARATION)


def test_an_ambiguous_anchor_fails() -> None:
    # Two matches is a failure rather than a first-match win: the anchor has
    # stopped naming one block, and silently choosing one would publish
    # whichever happened to come first.
    twice = SOURCE + "\n" + SOURCE
    assert "matches 2 lines" in fails(twice, DECLARATION)


def test_a_reordered_block_fails() -> None:
    reordered = "\n".join([
        "{% endmacro %}",
        "{% macro render_nav(section, current_path) %}",
    ])
    assert "above the start anchor" in fails(reordered, DECLARATION)


def test_losing_a_claimed_substring_fails() -> None:
    # The guard anchors cannot provide: the block keeps its name and loses the
    # thing the page says about it. Precomputing the prefix test outside the
    # template is exactly the fairness break ADR-0072 Decision 3 forbids, and
    # it would leave the excerpt extracting perfectly.
    flattened = SOURCE.replace(
        "{% if current_path | starts_with(prefix=sub.permalink) %}",
        "{% if sub.active %}")
    failure = fails(flattened, DECLARATION)
    assert "no longer contains" in failure
    assert "starts_with(prefix=" in failure


def test_a_missing_file_fails() -> None:
    try:
        ex.build(str(REPO), [declaration(file="performance/ports/nowhere.html")])
    except ex.ExcerptError as failure:
        assert "does not exist" in str(failure)
    else:
        raise AssertionError("expected an ExcerptError for a missing file")


# ---- the shipped declarations, against the real ports ----------------------

def test_the_shipped_declarations_still_describe_the_real_sources() -> None:
    payload = ex.build(str(REPO), ex.EXCERPTS)
    assert set(payload["excerpts"]) == {
        "sidebar-gazette", "sidebar-zola", "sidebar-hugo", "engine-starts-with"}
    for excerpt in payload["excerpts"].values():
        assert excerpt["lines"] > 0
        assert excerpt["text"].strip()


def test_the_three_sidebar_ports_are_three_different_texts() -> None:
    # A panel whose columns agree would be making no point. Gazette's template
    # dialect is a Tera subset, so this is a real risk: `blog-page.html` — the
    # byline template — IS byte-identical between the gazette and Zola ports,
    # which is why the sidebar was chosen instead.
    #
    # Distinctness alone is too weak a guard: three texts differing by one
    # character would satisfy it while the panel silently stopped making its
    # point. What the page claims is that the three say the prefix test three
    # ways, so that is what is asserted.
    payload = ex.build(str(REPO), ex.EXCERPTS)["excerpts"]
    texts = [payload[name]["text"]
             for name in ("sidebar-gazette", "sidebar-zola", "sidebar-hugo")]
    assert len(set(texts)) == 3
    for name, spelling in (
        ("sidebar-gazette", "starts_with(prefix="),
        ("sidebar-zola", "is starting_with("),
        ("sidebar-hugo", "strings.HasPrefix"),
    ):
        assert spelling in payload[name]["text"], (
            f"{name} no longer spells the prefix test as {spelling!r}; the "
            "panel's claim that the three ports say it three ways is stale"
        )


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_")]
    for test in tests:
        test()
    print(f"ok: {len(tests)} test(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
