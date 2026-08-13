#!/usr/bin/env python3
"""Differential check of gazette's Markdown rendering against pinned Zola.

ADR-0072 Phase 3 (RUE-1483) delivers gazette's three text-processing libraries.
The correctness bar for the Markdown one is the LIVE rue-lang.dev corpus, not
the CommonMark suite, so the only honest check is to render that corpus with
both tools and compare. This is that check, kept in the repository because the
claim "gazette agrees with Zola on the corpus" is worthless if reproducing it
means rebuilding the apparatus from scratch. ADR-0072 Decision 4 needs the same
comparison for its semantic oracle, so Phase 4 inherits this rather than
starting over.

WHAT IS COMPARED is the rendered Markdown body of every content page — Zola's
`page.content` — and nothing else. Template application, section indexes, and
the feed are RUE-1484's, and the template ports are RUE-1485's. Both tools are
therefore driven with body-only templates.

THE COMPARISON IS BYTE-FOR-BYTE, and that is the point. An earlier structural
comparison normalized HTML entities and collapsed whitespace, which silently
erased two entire classes of real difference. Here every difference must be
either byte-identical or attributable to one NAMED, documented divergence class
below; anything else fails the run. The structural view is still reported, as a
second and weaker layer, never as the headline.

DOCUMENTED DIVERGENCE CLASSES, all three from Zola's side of the comparison:

  zola-code-block-wrapper
      Zola writes every fenced code block through its own writer, even with
      `highlight_code = false`, as `<pre data-lang="X" class="language-X ">
      <code class="language-X" data-lang="X">` — note the duplicated language
      and the trailing space in the `class`. pulldown-cmark, and so gazette,
      emit CommonMark's `<pre><code class="language-X">`. This applies to every
      info string uniformly, `rue` and `rue check` alike.

  zola-code-block-escaping
      Inside those blocks Zola also escapes `"`, `'`, and `/`, so `//` becomes
      `&#x2F;&#x2F;`. Gazette uses pulldown-cmark's body-text set (`&`, `<`,
      `>`), which is what CommonMark specifies.

      Gazette keeps the specified behaviour deliberately. Adopting Zola's
      escape table would close THIS class but not the wrapper class above, so
      byte identity on a page with a code block is unreachable either way
      without emitting Zola's own non-CommonMark markup — and the cost would be
      a Markdown renderer that no longer matches the specification it
      documents. The two agree on visible text once entities are decoded, which
      is what ADR-0072 Decision 4's semantic oracle compares. Decision 4's spot
      goldens are per-tool markup, so this is a disclosed difference rather
      than a hidden one, and it is the reason a code-bearing page is expected
      to land in this class rather than in `byte-identical`.

  summary-span-newline
      Gazette terminates the `<!-- more -->` anchor span with a newline, as it
      terminates every block it emits. Zola emits that newline when the marker
      precedes a paragraph and omits it when the marker precedes a heading — a
      pulldown-cmark writer asymmetry rather than a rule. Cross-tool whitespace
      is a non-goal (ADR-0072 Terminology).

EXCLUDED PAGES are listed in EXCLUSIONS with a reason each. There is exactly
one, and it is excluded because Zola does not render it at all.

Usage:
    scripts/gazette-corpus-diff.py [--gazette PATH] [--keep] [--verbose]
"""

from __future__ import annotations

import argparse
import html.parser
import os
import re
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASE_URL = "https://rue-lang.dev"

# Pages Zola emits no rendered body for. Each entry must name the reason.
EXCLUSIONS = {
    "spec/_index.md": (
        "front matter sets `redirect_to`, so Zola emits a redirect stub instead "
        "of the section body; there is nothing to compare against"
    ),
}


# ---------------------------------------------------------------------------
# Corpus assembly — exactly what website/build.sh does
# ---------------------------------------------------------------------------


def assemble_corpus(dest: str) -> list[str]:
    """Copy site content plus the specification, rewriting spec-internal links."""
    shutil.copytree(os.path.join(REPO, "website", "content"), dest)
    spec_dest = os.path.join(dest, "spec")
    if os.path.exists(spec_dest):
        shutil.rmtree(spec_dest)
    shutil.copytree(os.path.join(REPO, "docs", "spec", "src"), spec_dest)

    pattern = re.compile(r"@/(\d)")
    for dirpath, _dirs, files in os.walk(spec_dest):
        for name in files:
            if not name.endswith(".md"):
                continue
            path = os.path.join(dirpath, name)
            with open(path, encoding="utf-8") as handle:
                body = handle.read()
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(pattern.sub(r"@/spec/\1", body))

    pages = []
    for dirpath, _dirs, files in os.walk(dest):
        for name in sorted(files):
            if name.endswith(".md"):
                rel = os.path.relpath(os.path.join(dirpath, name), dest)
                pages.append(rel.replace(os.sep, "/"))
    return sorted(pages)


def route_of(rel: str) -> str:
    """Zola's route for a content path, which is also the page's permalink."""
    stem = rel[: -len(".md")]
    if stem == "_index":
        return ""
    if stem.endswith("/_index"):
        return stem[: -len("_index")]
    return stem + "/"


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------


def build_gazette() -> str:
    binary = subprocess.run(
        [os.path.join(REPO, "scripts", "rue-bin")],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    out = os.path.join(tempfile.mkdtemp(prefix="gazette-bin-"), "gazette")
    subprocess.run(
        [binary, os.path.join(REPO, "examples", "gazette", "main.rue"), "-o", out],
        check=True, capture_output=True, text=True,
        env={**os.environ, "RUE_STD_PATH": os.path.join(REPO, "std")},
    )
    return out


def render_with_gazette(gazette: str, corpus: str, pages: list[str], dest: str) -> dict:
    shortcodes = os.path.join(REPO, "website", "templates", "shortcodes")
    args = []
    for name in sorted(os.listdir(shortcodes)):
        args += ["-t", "shortcodes/%s=%s" % (name, os.path.join(shortcodes, name))]

    diagnostics = {}
    for rel in pages:
        target = os.path.join(dest, rel[: -len(".md")] + ".html")
        os.makedirs(os.path.dirname(target), exist_ok=True)
        result = subprocess.run(
            [gazette, "render"] + args
            + ["-u", "%s/%s" % (BASE_URL, route_of(rel)), os.path.join(corpus, rel)],
            capture_output=True, text=True,
        )
        with open(target, "w", encoding="utf-8") as handle:
            handle.write(result.stdout)
        if result.returncode != 0:
            diagnostics[rel] = (result.stdout + result.stderr).strip()
    return diagnostics


PARITY_CONFIG = """\
base_url = "%s"
title = "Rue"
description = "corpus differential"
compile_sass = false
minify_html = false
generate_feeds = false
build_search_index = false

[markdown]
highlight_code = false
""" % BASE_URL


def render_with_zola(corpus: str, pages: list[str], root: str) -> str:
    """Build the same corpus with the pinned Zola, emitting bodies only."""
    shutil.copytree(corpus, os.path.join(root, "content"))
    templates = os.path.join(root, "templates")
    os.makedirs(os.path.join(templates, "shortcodes"))
    for name in os.listdir(os.path.join(REPO, "website", "templates", "shortcodes")):
        shutil.copy(
            os.path.join(REPO, "website", "templates", "shortcodes", name),
            os.path.join(templates, "shortcodes", name),
        )
    with open(os.path.join(root, "config.toml"), "w", encoding="utf-8") as handle:
        handle.write(PARITY_CONFIG)

    # Every template a page or section can name, rendering the body and nothing
    # else. `paginate_by` is dropped for the same reason ADR-0072 Decision 3
    # carves pagination out of the equivalence subset.
    wanted = {"page.html", "section.html", "index.html"}
    for rel in pages:
        head = open(os.path.join(corpus, rel), encoding="utf-8").read()
        for key in ("template", "page_template"):
            found = re.search(r'^%s = "([^"]+)"' % key, head, re.M)
            if found:
                wanted.add(found.group(1))
    for name in sorted(wanted):
        path = os.path.join(templates, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        body = "section" if ("section" in name or "index" in name or name in
                             ("blog.html",)) else "page"
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("{{ %s.content | safe }}" % body)
    # A `base.html` that page templates might extend must exist and stay empty.
    for name in sorted({os.path.dirname(n) for n in wanted if os.path.dirname(n)} | {""}):
        path = os.path.join(templates, name, "base.html")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("")

    content = os.path.join(root, "content")
    for rel in pages:
        path = os.path.join(content, rel)
        body = open(path, encoding="utf-8").read()
        body = re.sub(r"^paginate_by = .*\n", "", body, flags=re.M)
        open(path, "w", encoding="utf-8").write(body)

    result = subprocess.run(
        [os.path.join(REPO, "zola"), "build"], cwd=root, capture_output=True, text=True
    )
    if result.returncode != 0:
        sys.exit("zola build failed:\n" + result.stdout + result.stderr)
    return os.path.join(root, "public")


# ---------------------------------------------------------------------------
# Difference classification
# ---------------------------------------------------------------------------

PRE_OPEN = re.compile(
    r'<pre(?: data-lang="[^"]*")?(?: class="language-[^"]*")?>'
    r'<code(?: class="language-([^"]*?)\s*")?(?: data-lang="[^"]*")?>'
)


def normalize_code_wrapper(page: str) -> str:
    return PRE_OPEN.sub(
        lambda m: "<pre><code%s>" % (
            ' class="language-%s"' % m.group(1) if m.group(1) else ""
        ),
        page,
    )


CODE_BLOCK = re.compile(r"(<pre><code[^>]*>)(.*?)(</code></pre>)", re.S)


def normalize_code_escaping(page: str) -> str:
    def relax(match: re.Match) -> str:
        body = match.group(2)
        body = body.replace("&quot;", '"').replace("&#x27;", "'").replace("&#x2F;", "/")
        return match.group(1) + body + match.group(3)

    return CODE_BLOCK.sub(relax, page)


def normalize_summary_newline(page: str) -> str:
    return page.replace('<span id="continue-reading"></span>\n',
                        '<span id="continue-reading"></span>')


NORMALIZERS = [
    ("zola-code-block-wrapper", normalize_code_wrapper),
    ("zola-code-block-escaping", normalize_code_escaping),
    ("summary-span-newline", normalize_summary_newline),
]


class Extract(html.parser.HTMLParser):
    """Element, text, and link-target sequence — the structural layer."""

    VOID = {"br", "hr", "img", "input", "meta", "link"}

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.events: list = []

    def handle_starttag(self, tag, attrs):
        keep = {k: v for k, v in attrs if k in ("href", "src", "alt", "id", "start")}
        self.events.append(("<" + tag, tuple(sorted(keep.items()))))

    def handle_endtag(self, tag):
        if tag not in self.VOID:
            self.events.append((">" + tag, ()))

    def handle_data(self, data):
        squashed = " ".join(data.split())
        if squashed:
            self.events.append(("#text", squashed))


def structure(page: str) -> list:
    parser = Extract()
    parser.feed(page)
    return parser.events


# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gazette", help="prebuilt gazette binary")
    parser.add_argument("--keep", action="store_true", help="keep the work tree")
    parser.add_argument("--verbose", action="store_true", help="print every page")
    options = parser.parse_args()

    work = tempfile.mkdtemp(prefix="gazette-corpus-")
    try:
        corpus = os.path.join(work, "content")
        pages = assemble_corpus(corpus)
        gazette = options.gazette or build_gazette()

        rendered = os.path.join(work, "gazette")
        diagnostics = render_with_gazette(gazette, corpus, pages, rendered)
        public = render_with_zola(corpus, pages, os.path.join(work, "zola"))

        print("corpus: %d markdown files" % len(pages))
        if diagnostics:
            print("\ngazette reported diagnostics on %d page(s):" % len(diagnostics))
            for rel in sorted(diagnostics):
                print("  %s" % rel)
                for line in diagnostics[rel].splitlines():
                    if re.match(r"^(front-matter|markdown|template|shortcode):", line):
                        print("      " + line)

        identical = 0
        classified: dict[str, int] = {}
        unexplained: list[str] = []
        structural = 0
        compared = 0
        missing: list[str] = []

        for rel in pages:
            if rel in EXCLUSIONS:
                continue
            ours_path = os.path.join(rendered, rel[: -len(".md")] + ".html")
            theirs_path = os.path.join(public, route_of(rel), "index.html")
            if not os.path.exists(theirs_path):
                missing.append(rel)
                continue
            compared += 1
            ours = open(ours_path, encoding="utf-8").read()
            theirs = open(theirs_path, encoding="utf-8").read()

            if ours == theirs:
                identical += 1
                applied: list[str] = []
            else:
                applied = []
                a, b = ours, theirs
                for name, normalize in NORMALIZERS:
                    na, nb = normalize(a), normalize(b)
                    if (na, nb) != (a, b):
                        applied.append(name)
                    a, b = na, nb
                if a != b:
                    unexplained.append(rel)
                for name in applied:
                    classified[name] = classified.get(name, 0) + 1
            if structure(ours) == structure(theirs):
                structural += 1
            if options.verbose:
                mark = "identical" if not applied else "+".join(applied)
                print("  %-60s %s" % (rel, mark))

        print("\nexcluded: %d" % len(EXCLUSIONS))
        for rel, why in sorted(EXCLUSIONS.items()):
            print("  %s — %s" % (rel, why))
        if missing:
            print("\nNO ZOLA OUTPUT (not excluded): %d" % len(missing))
            for rel in missing:
                print("  %s" % rel)

        print("\ncompared: %d page(s)" % compared)
        print("  byte-identical:                    %d" % identical)
        for name, _ in NORMALIZERS:
            print("  differ by %-24s %d" % (name + ":", classified.get(name, 0)))
        print("  structurally identical:            %d" % structural)
        print("  UNEXPLAINED differences:           %d" % len(unexplained))

        for rel in unexplained[:5]:
            ours_path = os.path.join(rendered, rel[: -len(".md")] + ".html")
            theirs_path = os.path.join(public, route_of(rel), "index.html")
            a, b = open(ours_path, encoding="utf-8").read(), open(theirs_path, encoding="utf-8").read()
            for _, normalize in NORMALIZERS:
                a, b = normalize(a), normalize(b)
            print("\n=== unexplained: %s" % rel)
            import difflib
            for line in list(difflib.unified_diff(
                b.splitlines(), a.splitlines(), "zola", "gazette", n=0, lineterm=""
            ))[:14]:
                print("  " + line[:200])

        failed = bool(unexplained) or bool(diagnostics) or bool(missing)
        print("\n%s" % ("FAILED" if failed else "OK: every difference is byte-identical "
                        "or a documented divergence"))
        return 1 if failed else 0
    finally:
        if options.keep:
            print("\nwork tree kept at %s" % work)
        else:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
