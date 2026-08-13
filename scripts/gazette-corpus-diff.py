#!/usr/bin/env python3
"""Corpus-driven checks of gazette against the LIVE rue-lang.dev corpus.

Two modes, one apparatus. Both assemble the corpus exactly as
`website/build.sh` does — site content plus the copied specification, with the
spec's internal links rewritten — because ADR-0072 Decision 2 measures the site
as it actually exists rather than a frozen snapshot.

    scripts/gazette-corpus-diff.py body
        RUE-1483's Markdown differential. Renders every content page's BODY
        with gazette and with the pinned Zola and compares byte for byte. The
        correctness bar for the Markdown library is this corpus, not the
        CommonMark suite, so the only honest check is to render it with both
        tools and compare.

    scripts/gazette-corpus-diff.py site [--scale N]
        RUE-1484's whole-site validation. Prepares a fixture (corpus, static
        assets, template port, and the deterministic scale variants), records
        the fixture identity, builds it with gazette several times, and judges
        the result: determinism, file set, the semantic oracle on every emitted
        page, spot goldens, and the wall-clock/RSS/binary-size metrics
        ADR-0072 Decision 5 records per sample.

    scripts/gazette-corpus-diff.py golden [--bless]
        The markup-level spot goldens, over the committed miniature site in
        `performance/fixtures/gazette/`.

WHAT `body` COMPARES is the rendered Markdown body of every content page —
Zola's `page.content` — and nothing else. THE COMPARISON IS BYTE-FOR-BYTE, and
that is the point. An earlier structural comparison normalized HTML entities
and collapsed whitespace, which silently erased two entire classes of real
difference. Here every difference must be either byte-identical or attributable
to one NAMED, documented divergence class below; anything else fails the run.

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

None of the three survives the structural extraction the `site` oracle uses:
entity references are decoded, whitespace is collapsed, and `class`/`data-lang`
attributes are not kept. That is why the oracle needs no normalizer table of
its own — it compares what the two tools genuinely have to agree on.
"""

from __future__ import annotations

import argparse
import hashlib
import html.parser
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASE_URL = "https://rue-lang.dev"

# The gazette half of ADR-0072 Decision 2's "template ports and parity
# configurations". The DIGEST below is the authoritative identity — it is
# computed from the bytes and cannot drift — and this integer is the
# human-legible label a maintainer bumps when changing the port deliberately,
# so a reader of two observations can say "the port moved" without diffing two
# hashes. Both ride every recorded identity.
PORT_REVISION = 1
PORT_ROOTS = [
    "examples/gazette/config.toml",
    "examples/gazette/templates",
]

# The production template set the port derives from, as it stood when
# PORT_REVISION was last set. ADR-0072's Consequences name the risk this
# discharges: "Production drift away from the ports quietly erodes the live
# framing, so website-template changes should include a port review." A comment
# asking for a review is not a review, so the digest is pinned and the `golden`
# mode fails when it moves.
#
# When it fires, the fix is to look at what changed in `website/templates/`,
# decide whether the port should follow, change the port if it should, then
# advance PORT_REVISION and paste the new digest here. Advancing the revision
# is not busywork: it is what moves the recorded fixture identity, which is
# what starts a new comparable segment in the series rather than shifting an
# existing one under a reader.
PRODUCTION_TEMPLATE_ROOT = "website/templates"
PRODUCTION_TEMPLATE_DIGEST = (
    "47d52cd447662e02d6f7a76b7d14eafffc1b4602788b8e9547a303be528d87ec"
)

# Pages Zola emits no rendered body for, so `body` mode has nothing to compare
# against. Each entry must name the reason.
EXCLUSIONS = {
    "spec/_index.md": (
        "front matter sets `redirect_to`, so Zola emits a redirect stub instead "
        "of the section body; there is nothing to compare against"
    ),
}

# Content EXCLUDED FROM THE BENCHMARK CORPUS, visibly rather than silently
# (ADR-0072 Decision 3). Both entries are the same carve-out: a page whose
# template reads derived benchmark data at build time would make the benchmark
# an input to its own workload. `performance.md` is the carve-out the ADR names;
# `runtime.md` is RUE-1049's page and is the same page in every respect that
# matters here.
#
# The exclusion is applied at fixture-preparation time and the excluded set is
# part of the recorded identity, so a page joining or leaving this table moves
# the fixture identity and starts a new comparable segment.
CORPUS_EXCLUSIONS = {
    "performance.md": (
        "the performance dashboard renders derived benchmark data, so building "
        "it would make the benchmark an input to its own workload (ADR-0072 "
        "Decision 3)"
    ),
    "runtime.md": (
        "the runtime dashboard is the same carve-out as performance.md: it "
        "renders the very series this workload produces"
    ),
}

# Internal links the LIVE CONTENT gets wrong, which Zola resolves exactly the
# same way and emits exactly as dead. Gazette's job is to reproduce the routing,
# not to repair the corpus, so each one is allowed here with its reason rather
# than either failing the benchmark or being swept under a permissive rule.
#
# Each entry declares HOW MANY TIMES the dead route is expected to appear per
# copy of the corpus, and the count is asserted. AGENTS.md sets the convention
# for `known_bug` markers — when the bug is fixed, find its cases and remove
# the marker — and an allowlist that could only ever grow would be the opposite
# convention. So the check fails two ways: an entry that stops appearing is
# stale and must go, and an entry that starts appearing MORE is new breakage
# hiding behind an old excuse.
KNOWN_BROKEN_LINKS = {
    "std/math/": (
        1,
        "the standard-library index links `math/` relative to `/std/`, but the "
        "page is `01-math.md` and so routes to `/std/01-math/`",
    ),
}

# Links per page into the two excluded pages: the desktop navigation bar names
# both, and the mobile menu names both again. Asserted rather than printed, for
# the reason the count exists at all — an exclusion is only "visible rather
# than silent" if a change in its blast radius is visible too.
EXCLUDED_LINKS_PER_PAGE = 4


# ---------------------------------------------------------------------------
# Corpus assembly — exactly what website/build.sh does
# ---------------------------------------------------------------------------


def assemble_corpus(dest: str, exclude: dict | None = None) -> list[str]:
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

    for rel in exclude or {}:
        victim = os.path.join(dest, rel)
        if os.path.exists(victim):
            os.remove(victim)

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


def body_mode(options) -> int:
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


# ===========================================================================
# site mode — ADR-0072 Phase 4 (RUE-1484)
# ===========================================================================

# ---------------------------------------------------------------------------
# An independent model of the corpus
# ---------------------------------------------------------------------------
#
# Everything below re-derives, from the source tree, what the emitted site
# ought to contain. It is deliberately a SECOND implementation of gazette's
# routing, section, and ordering rules rather than a reading of gazette's
# output: an oracle that asked the program under test what the answer was would
# check nothing. It covers exactly the front-matter subset gazette documents,
# and only the keys the model needs.

FRONT_MATTER = re.compile(r"\A\+\+\+\r?\n(.*?)^\+\+\+\r?\n", re.S | re.M)
SCALAR = re.compile(r'^([A-Za-z0-9_-]+)\s*=\s*(.+?)\s*$', re.M)


def read_front_matter(path: str) -> dict:
    text = open(path, encoding="utf-8").read()
    found = FRONT_MATTER.match(text)
    if not found:
        raise SystemExit("%s does not open with a `+++` fence" % path)
    fields: dict = {}
    table = None
    for line in found.group(1).splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped.startswith("[") and stripped.endswith("]"):
            table = stripped[1:-1]
            fields[table] = {}
            continue
        pair = SCALAR.match(stripped)
        if not pair:
            continue
        key, raw = pair.group(1), pair.group(2)
        # Strip a trailing comment outside quotes, as gazette's reader does.
        if '"' not in raw:
            raw = raw.split("#", 1)[0].strip()
        if raw.startswith('"""'):
            value = "<multiline>"
        elif raw.startswith('"'):
            value = raw[1:].split('"')[0]
        elif raw in ("true", "false"):
            value = raw == "true"
        elif raw.isdigit():
            value = int(raw)
        elif raw.startswith("["):
            value = [p.strip().strip('"') for p in raw[1:-1].split(",") if p.strip()]
        else:
            value = raw
        if table is None:
            fields[key] = value
        else:
            fields[table][key] = value
    return fields


class Model:
    """Routes, sections, orderings, and feeds, computed from the source tree."""

    def __init__(self, content: str) -> None:
        self.content = content
        self.pages: dict[str, dict] = {}
        for dirpath, _dirs, files in os.walk(content):
            for name in sorted(files):
                if not name.endswith(".md"):
                    continue
                rel = os.path.relpath(os.path.join(dirpath, name), content)
                rel = rel.replace(os.sep, "/")
                self.pages[rel] = read_front_matter(os.path.join(content, rel))

        self.sections = {rel: front for rel, front in self.pages.items()
                         if rel.endswith("_index.md")}
        self.members: dict[str, list[str]] = {key: [] for key in self.sections}
        for rel in self.pages:
            if rel.endswith("_index.md"):
                continue
            directory = rel.rsplit("/", 1)[0] + "/" if "/" in rel else ""
            key = directory + "_index.md"
            if key not in self.members:
                raise SystemExit("no `_index.md` for %s" % rel)
            self.members[key].append(rel)
        for key, listed in self.members.items():
            self.members[key] = self.order(key, listed)

        self.subsections: dict[str, list[str]] = {key: [] for key in self.sections}
        for key in self.sections:
            directory = key[: -len("_index.md")]
            if directory == "":
                continue
            above = directory[:-1].rsplit("/", 1)[0] + "/" if "/" in directory[:-1] else ""
            self.subsections[above + "_index.md"].append(key)
        for key in self.subsections:
            self.subsections[key].sort()

    def order(self, key: str, listed: list[str]) -> list[str]:
        """The declared ordering, tie-broken by content path.

        The tie-break is the point. ADR-0072 Decision 2's scale variants
        duplicate the corpus under path prefixes, so every duplicated page ties
        with its original on weight, on title, and on date; a rule that stopped
        at those fields would leave the order up to discovery, and this oracle
        would then be asserting whatever gazette happened to do. Ending in the
        path makes the order total, and comparing against it is what turns
        "the tools' tie-breaks are deterministic" from an assumption into a
        check.
        """
        sort_by = self.sections[key].get("sort_by")
        if sort_by == "weight":
            # A missing weight sorts last, never as a zero.
            return sorted(listed, key=lambda r: (self.pages[r].get("weight", 1 << 62), r))
        if sort_by == "date":
            return sorted(listed, key=lambda r: (self._inverse_date(r), r))
        return sorted(listed)

    def _inverse_date(self, rel: str) -> str:
        date = str(self.pages[rel].get("date", ""))
        # Newest first: invert each byte so an ascending sort is descending by
        # date, and an absent date (the empty string) still lands last.
        return "".join(chr(255 - ord(c)) for c in date) if date else chr(0)

    def routes(self) -> dict[str, str]:
        return {rel: route_of(rel) for rel in self.pages}

    def feeds(self) -> list[str]:
        return sorted(key for key, front in self.sections.items()
                      if front.get("generate_feeds") is True)

    def redirects(self) -> list[str]:
        return sorted(key for key, front in self.sections.items()
                      if front.get("redirect_to"))

    def expected_files(self, static_files: list[str]) -> set[str]:
        expected = {route_of(rel) + "index.html" for rel in self.pages}
        for key in self.feeds():
            expected.add(route_of(key) + "rss.xml")
        return expected | set(static_files)


# ---------------------------------------------------------------------------
# Fixture preparation and identity
# ---------------------------------------------------------------------------


def walk_files(root: str) -> list[str]:
    found = []
    for dirpath, _dirs, files in os.walk(root):
        for name in files:
            rel = os.path.relpath(os.path.join(dirpath, name), root)
            found.append(rel.replace(os.sep, "/"))
    return sorted(found)


def tree_digest(root: str, files: list[str] | None = None) -> tuple[str, int, int]:
    """A hash over paths, sizes, and contents — the recorded input identity.

    Paths are included, so a file that only moved changes the digest; sizes are
    included, so no content can be silently truncated to another file's bytes.
    """
    listed = walk_files(root) if files is None else files
    digest = hashlib.sha256()
    total = 0
    for rel in listed:
        path = os.path.join(root, rel)
        blob = open(path, "rb").read()
        total += len(blob)
        digest.update(rel.encode("utf-8"))
        digest.update(b"\0%d\0" % len(blob))
        digest.update(blob)
    return digest.hexdigest(), len(listed), total


def duplicate_corpus(content: str, scale: int) -> None:
    """The deterministic path-prefixed duplication of ADR-0072 Decision 2.

    The originals stay in place and `scale - 1` copies join them under `xN-KK/`
    prefixes, so the result is exactly `scale` times the page count.

    TWO HONESTY NOTES, both of which the published charts must carry.

    First, this scales PER-PAGE WORK BUT NOT SITE SHAPE. Internal `@/…` links
    and every `get_section(path=…)` in the templates still name the ORIGINAL
    content, because the copies are byte-identical to the originals and a copy
    has no way to refer to itself. Cross-reference resolution therefore stays
    constant while page count grows. It is a page-count curve, not a
    site-size curve. `redirect_to` behaves the same way: a duplicated
    `spec/_index.md` still redirects to the ORIGINAL introduction page, because
    its front matter names an absolute content path and a copy cannot rewrite
    itself.

    Second, and following from the first: the specification sidebar COLLAPSES on
    a duplicated page, and the magnitude is worth stating rather than gesturing
    at. Its recursion and its `active` class are both gated on the current
    page's permalink prefixing a subsection's, and a copy's permalink prefixes
    nothing in the original tree that `get_section` returns, so BOTH arms of the
    gate go cold. Measured over the 10x fixture's spec pages: the 70 originals
    render navigations of 2,540 to 6,205 bytes, mean 4,446, with 1.84 `active`
    markers each; all 630 duplicates render a single byte-identical 2,534-byte
    navigation with ZERO `active` markers. That is 57% of the original mean, on
    the majority page class, and at 100x it is 99% of all spec pages.

    So the fixture reproduces, at scale, exactly the page-invariant sidebar
    ADR-0072 Decision 3 forbids a gazette implementation from producing. The
    RULE still holds — the program evaluates the gate per page and the data
    says "no match" — and the cross-tool comparison is unaffected, since every
    tool builds the identical tree and sees the identical collapse. What it
    means is that the 10x and 100x points understate per-page templating work
    relative to 1x, so the curve cannot be read as "what a 1x page costs, times
    N", and the fairness evidence that the navigation is page-dependent is 1x
    evidence.
    """
    if scale <= 1:
        return
    original = tempfile.mkdtemp(prefix="gazette-1x-")
    shutil.rmtree(original)
    shutil.copytree(content, original)
    try:
        for index in range(scale - 1):
            shutil.copytree(original, os.path.join(content, "x%d-%02d" % (scale, index)))
    finally:
        shutil.rmtree(original, ignore_errors=True)


def prepare_fixture(root: str, scale: int) -> dict:
    """Assemble a complete gazette site and record its input identity.

    Everything here happens OUTSIDE the measured window (ADR-0071's boundary
    discipline): the corpus assembly, the scale duplication, and the hashing
    are fixture preparation, and only the build that follows is timed.
    """
    content = os.path.join(root, "content")
    assemble_corpus(content, CORPUS_EXCLUSIONS)
    content_digest, content_files, content_bytes = tree_digest(content)
    duplicate_corpus(content, scale)

    shutil.copytree(os.path.join(REPO, "examples", "gazette", "templates"),
                    os.path.join(root, "templates"))
    shutil.copy(os.path.join(REPO, "examples", "gazette", "config.toml"),
                os.path.join(root, "config.toml"))

    static_source = os.path.join(REPO, "website", "static")
    static_files = [rel for rel in walk_files(static_source)
                    # Derived at site-build time and never committed; including
                    # it would make the fixture identity move with every
                    # collection run rather than with the site.
                    if rel != "performance-data.json"]
    static_digest, static_count, static_bytes = tree_digest(static_source, static_files)
    shutil.copytree(static_source, os.path.join(root, "static"),
                    ignore=shutil.ignore_patterns("performance-data.json"))

    port_digest = hashlib.sha256()
    port_files = 0
    port_bytes = 0
    for entry in PORT_ROOTS:
        path = os.path.join(REPO, entry)
        if os.path.isdir(path):
            digest, count, size = tree_digest(path)
        else:
            digest, count, size = tree_digest(os.path.dirname(path),
                                              [os.path.basename(path)])
        port_digest.update(entry.encode("utf-8"))
        port_digest.update(digest.encode("ascii"))
        port_files += count
        port_bytes += size

    identity = {
        "scale": scale,
        "content_digest": content_digest,
        "content_files": content_files,
        "content_bytes": content_bytes,
        "static_digest": static_digest,
        "static_files": static_count,
        "static_bytes": static_bytes,
        "port_revision": PORT_REVISION,
        "port_digest": port_digest.hexdigest(),
        "port_files": port_files,
        "port_bytes": port_bytes,
        "excluded": sorted(CORPUS_EXCLUSIONS),
    }
    # One digest over all of it, so an observation can be matched to a segment
    # with a single comparison. Every input class is inside it: no static asset,
    # template, or configuration change can alter the measured job without
    # changing this value.
    fingerprint = hashlib.sha256()
    for key in sorted(identity):
        fingerprint.update(("%s=%s;" % (key, identity[key])).encode("utf-8"))
    identity["fixture_digest"] = fingerprint.hexdigest()
    return identity


# ---------------------------------------------------------------------------
# Measurement
# ---------------------------------------------------------------------------


def rss_bytes(raw: int) -> int:
    """`ru_maxrss` in bytes. Kilobytes on Linux, bytes on Darwin."""
    return raw if platform.system() == "Darwin" else raw * 1024


def run_build(gazette: str, root: str, out: str) -> dict:
    """One measured build: wall clock and THIS CHILD's peak RSS, spawn to exit.

    The rusage is reaped from the child itself with `wait4`, and that detail is
    the whole of the measurement's honesty. `getrusage(RUSAGE_CHILDREN)` is a
    cumulative high-water mark over every child the process has ever reaped, so
    reading it around a spawn reports the largest child so far rather than this
    one — and this script's default path has just run the Rue compiler at
    roughly 210 MiB to produce the binary it is about to measure. Every sample
    at every scale then reported the compiler's footprint: 210.9, 210.1, and
    206.9 MiB where gazette's true peaks are 4.4, 11.3, and 104.9. The reported
    figure was not merely wrong, it was FLAT and slightly decreasing across a
    range over which the real one grows 24x, which is exactly the shape a
    reader would conclude something from.

    Two properties follow from `wait4` that the old reading could not have.
    The number is this process's alone, so it is unaffected by anything built
    or measured before it; and it is per sample rather than a running maximum,
    which is what ADR-0072 Decision 5 records.
    """
    start = time.perf_counter()
    process = subprocess.Popen([gazette, "build", root, "-o", out],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True)
    # Drain before reaping: `wait4` does not consume the pipes, and a child
    # that filled one would block forever waiting for a reader.
    stdout = process.stdout.read()
    stderr = process.stderr.read()
    _pid, status, usage = os.wait4(process.pid, 0)
    wall = time.perf_counter() - start
    # The child is already reaped, so `Popen` must be told rather than left to
    # call `waitpid` on a pid that no longer exists.
    process.returncode = os.waitstatus_to_exitcode(status)
    process.stdout.close()
    process.stderr.close()
    return {
        "wall_seconds": wall,
        "peak_rss_bytes": rss_bytes(usage.ru_maxrss),
        "exit_code": process.returncode,
        "stdout": stdout,
        "stderr": stderr,
    }


def tree_hash(root: str) -> str:
    digest, _count, _bytes = tree_digest(root)
    return digest


# ---------------------------------------------------------------------------
# The semantic oracle
# ---------------------------------------------------------------------------

SEP = "\x1e"


def event_text(events: list) -> str:
    return SEP + SEP.join("%s\x1f%s" % (tag, extra) for tag, extra in events) + SEP


def contains_run(page_events: list, body_events: list) -> bool:
    """Whether the body's event stream appears CONTIGUOUSLY inside the page's.

    This is the load-bearing half of ADR-0072 Decision 4's semantic oracle, and
    it is one comparison rather than six because the event stream already IS
    the normalized extraction: heading tree, visible text, link targets, image
    targets, and shortcode expansion results, in document order, with entities
    decoded, whitespace collapsed, and presentational attributes dropped.

    Contiguity is what makes it strong. A renderer that consistently dropped
    body content from every page would pass determinism, the file set, and
    every structural count, and would fail here on the first page — and so
    would one that dropped a single paragraph, reordered two headings, or
    resolved one link differently.
    """
    if not body_events:
        return True
    return event_text(page_events).find(event_text(body_events)) >= 0


def diagnose(page_events: list, body_events: list) -> str:
    """Which facet diverged, for a page whose body run was not found."""
    def headings(events):
        out, capture, level = [], None, None
        for tag, extra in events:
            if capture is not None:
                if tag == ">" + level:
                    out.append((level, capture))
                    capture, level = None, None
                elif tag == "#text":
                    capture = (capture + " " + extra).strip()
            elif tag in ("<h1", "<h2", "<h3", "<h4", "<h5", "<h6"):
                level, capture = tag[1:], ""
        return out

    def links(events):
        return [dict(extra).get("href") for tag, extra in events
                if tag == "<a" and dict(extra).get("href")]

    def words(events):
        return [extra for tag, extra in events if tag == "#text"]

    faults = []
    for name, extract in (("headings", headings), ("links", links), ("text", words)):
        want, have = extract(body_events), extract(page_events)
        missing = [item for item in want if item not in have]
        if missing:
            faults.append("%s: %d of %d missing, first %r"
                          % (name, len(missing), len(want), missing[0]))
    return "; ".join(faults) or "body events present but not contiguous"


def check_semantics(out: str, corpus: str, model: Model, gazette: str,
                    work: str, verbose: bool) -> tuple[int, list[str]]:
    """Compare every emitted page's body semantics against Zola's rendering.

    The anchor is deliberately an OTHER TOOL. Phase 5 (RUE-1485) compares the
    full page across three tools; what is available here is Zola rendering the
    same corpus through body-only templates, which is exactly the independent
    authority this check needs and is already built for `body` mode. It runs at
    1x only: the scale variants' pages are the same documents at different
    permalinks, so a second and third Zola build of 960 and 9,600 copies would
    buy nothing the 1x run has not already established, and the peer leg is
    Phase 5's to schedule.
    """
    pages = sorted(model.pages)
    public = render_with_zola(corpus, pages, os.path.join(work, "zola-oracle"))
    redirects = set(model.redirects())
    checked = 0
    failures = []
    for rel in pages:
        if rel in redirects:
            # Zola emits its redirect stub here rather than a body, so there is
            # no body semantics to compare. `check_redirects` asserts the thing
            # that actually matters about the page — its target — and the spot
            # goldens cover its markup.
            continue
        route = route_of(rel)
        emitted = os.path.join(out, route, "index.html")
        reference = os.path.join(public, route, "index.html")
        if not os.path.exists(reference):
            # Zola emits its own redirect stub here and no body; gazette emits
            # its port's stub. The file set check covers the page's existence,
            # and the redirect target is checked separately.
            continue
        if not os.path.exists(emitted):
            failures.append("%s: gazette emitted no page" % rel)
            continue
        page_events = structure(open(emitted, encoding="utf-8").read())
        body_events = structure(open(reference, encoding="utf-8").read())
        checked += 1
        if not contains_run(page_events, body_events):
            failures.append("%s: %s" % (rel, diagnose(page_events, body_events)))
        elif verbose:
            print("  oracle ok  %s (%d body events)" % (rel, len(body_events)))
    return checked, failures


def check_metadata(out: str, model: Model) -> list[str]:
    """Front-matter metadata reaches the page it describes.

    Weaker than the body oracle by construction: WHICH metadata a page shows is
    a property of the template port rather than of the corpus, so this asserts
    only what every port template provably surfaces — the title as visible text,
    and the ISO date on a dated page. The full normalized metadata rides the
    recorded oracle digest, so a change in it is visible in the data even where
    it is not asserted here.
    """
    faults = []
    for rel, front in sorted(model.pages.items()):
        if rel in model.redirects():
            continue
        path = os.path.join(out, route_of(rel), "index.html")
        if not os.path.exists(path):
            continue
        page = open(path, encoding="utf-8").read()
        text = " ".join(word for tag, word in structure(page) if tag == "#text")
        title = str(front.get("title", ""))
        if title and title not in text and html_unescape(title) not in text:
            faults.append("%s: title %r absent from the emitted page" % (rel, title))
        date = str(front.get("date", ""))
        if date and date not in page:
            faults.append("%s: date %r absent from the emitted page" % (rel, date))
    return faults


def html_unescape(value: str) -> str:
    import html as html_module
    return html_module.unescape(value)


def check_membership(out: str, model: Model) -> list[str]:
    """Every section index links its own pages, in the declared order.

    The emitted page carries the sidebar's copy of the list as well as the
    index's, so the assertion is that the model's ordered routes appear as a
    CONTIGUOUS RUN among the links into that section, and that no link into the
    section names a page the model does not place there. That catches a page
    filed under the wrong section, a page dropped from an index, and a
    reordering, without depending on how many times a template chooses to
    render the list.
    """
    faults = []
    redirects = set(model.redirects())
    for key, listed in sorted(model.members.items()):
        if not listed or key in redirects:
            # A redirect section emits a stub in place of an index, so it lists
            # nothing to check. `check_redirects` covers it.
            continue
        path = os.path.join(out, route_of(key), "index.html")
        if not os.path.exists(path):
            faults.append("%s: no emitted section index" % key)
            continue
        wanted = [route_of(rel) for rel in listed]
        owned = set(wanted)
        # Only links INTO this section are collected. A link out of it —
        # including a relative one the content itself got wrong — is the
        # dangling-link check's business, not this one's.
        seen = []
        for tag, extra in structure(open(path, encoding="utf-8").read()):
            if tag != "<a":
                continue
            href = dict(extra).get("href")
            if not href:
                continue
            route = href_route(href)
            if route is None:
                continue
            if route in owned:
                seen.append(route)
        if not run_within(seen, wanted):
            faults.append("%s: expected order %s, emitted %s"
                          % (key, wanted[:4], seen[:8]))
    return faults


def run_within(haystack: list[str], needle: list[str]) -> bool:
    if not needle:
        return True
    for start in range(len(haystack) - len(needle) + 1):
        if haystack[start:start + len(needle)] == needle:
            return True
    return False


def href_route(href: str) -> str | None:
    """The site route an href names, or None when it names something else."""
    if href.startswith(BASE_URL):
        rest = href[len(BASE_URL):]
    elif href.startswith("/") and not href.startswith("//"):
        rest = href
    else:
        return None
    rest = rest.split("#", 1)[0].split("?", 1)[0]
    return rest.lstrip("/")


SCALE_PREFIX = re.compile(r"^x\d+-\d\d/")


def unprefixed(route: str) -> str:
    """A route with any scale-variant path prefix removed.

    The copies are byte-identical to the originals, so anything true of an
    original route is true of every copy of it, and a table keyed on original
    routes should not need one entry per duplication factor.
    """
    return SCALE_PREFIX.sub("", route)


def route_exists(route: str, emitted: set, directories: set) -> bool:
    """Whether a route names something the site emitted.

    A route is accepted with or without its trailing slash. `get_url(path=
    'tutorial')` produces an unslashed URL in Zola too, and the real site
    serves it by redirect, so treating the two forms as different would report
    every navigation link in the corpus as broken.
    """
    key = route.rstrip("/")
    if key == "":
        return "index.html" in emitted
    return (key in emitted
            or key + "/index.html" in emitted
            or key + "/" in directories)


def check_links(out: str, model: Model, scale: int) -> tuple[int, int, int, list[str]]:
    """No internal link points at a route the site does not emit.

    Two categories are tolerated, and BOTH are asserted rather than merely
    counted, because a tolerated category whose size nobody checks is how an
    allowlist turns into a blindfold.

    Links into EXCLUDED content: the two carve-outs are named by the navigation
    bar and again by the mobile menu, so dropping them from the corpus
    necessarily leaves four dead links on every page that has a navigation. The
    expected total is therefore a product of the page count, not a magic
    number, which is what lets it survive the corpus growing while still
    failing if a template stops linking them or starts linking them twice.

    Links in KNOWN_BROKEN_LINKS: each entry declares its occurrences per corpus
    copy, and a count that has fallen to zero means the content was fixed and
    the entry is stale.

    KNOWN LIMIT: only hrefs beginning with the site's base URL are resolved.
    Root-relative ones are skipped, which today is right — the corpus's are
    `/spec/` and the `preview_feature` shortcode's `/designs/…`, the latter
    naming a route no tool emits — but a template that started emitting
    relative links would lose coverage silently rather than loudly. The
    `checked` count printed alongside is what would show it.
    """
    emitted = set(walk_files(out))
    directories = {rel.rsplit("/", 1)[0] + "/" for rel in emitted if "/" in rel}
    excluded_routes = {route_of(rel).rstrip("/") for rel in CORPUS_EXCLUSIONS}
    faults = []
    checked = 0
    into_excluded = 0
    known_broken = {route: 0 for route in KNOWN_BROKEN_LINKS}
    with_navigation = 0
    redirects = set(model.redirects())
    for rel in sorted(model.pages):
        path = os.path.join(out, route_of(rel), "index.html")
        if not os.path.exists(path):
            continue
        # A redirect stub carries no navigation, so it is not one of the pages
        # the excluded-link arithmetic below counts over.
        if rel not in redirects:
            with_navigation += 1
        for tag, extra in structure(open(path, encoding="utf-8").read()):
            if tag not in ("<a", "<link", "<img", "<script"):
                continue
            fields = dict(extra)
            href = fields.get("href") or fields.get("src")
            if not href or not href.startswith(BASE_URL):
                continue
            checked += 1
            route = href_route(href)
            if route is not None and route_exists(route, emitted, directories):
                continue
            if route is not None and route.rstrip("/") in excluded_routes:
                into_excluded += 1
                continue
            if route is not None and unprefixed(route) in known_broken:
                known_broken[unprefixed(route)] += 1
                continue
            faults.append("%s: dangling internal link %s" % (rel, href))

    wanted_excluded = with_navigation * EXCLUDED_LINKS_PER_PAGE
    if into_excluded != wanted_excluded:
        faults.append(
            "links into excluded content: %d, expected %d (%d pages with a "
            "navigation x %d links each). Either a template changed how it "
            "links the excluded pages, or EXCLUDED_LINKS_PER_PAGE is stale."
            % (into_excluded, wanted_excluded, with_navigation,
               EXCLUDED_LINKS_PER_PAGE))
    for route, (per_copy, why) in sorted(KNOWN_BROKEN_LINKS.items()):
        found = known_broken[route]
        wanted = per_copy * scale
        if found == wanted:
            continue
        if found == 0:
            faults.append(
                "KNOWN_BROKEN_LINKS entry %s is stale: it no longer appears, so "
                "the content was fixed and the entry should be removed (%s)"
                % (route, why))
        else:
            faults.append(
                "KNOWN_BROKEN_LINKS entry %s appears %d times, expected %d "
                "(%d per corpus copy x %d): new breakage is hiding behind an "
                "old allowance" % (route, found, wanted, per_copy, scale))
    return checked, into_excluded, sum(known_broken.values()), faults


def check_feeds(out: str, model: Model) -> list[str]:
    """Feed entry order equals the section's declared order, exactly."""
    faults = []
    for key in model.feeds():
        path = os.path.join(out, route_of(key), "rss.xml")
        if not os.path.exists(path):
            faults.append("%s: declares generate_feeds but no rss.xml was emitted" % key)
            continue
        feed = open(path, encoding="utf-8").read()
        emitted = re.findall(r"<link>([^<]*)</link>", feed)[1:]
        wanted = ["%s/%s" % (BASE_URL, route_of(rel)) for rel in model.members[key]]
        if emitted != wanted:
            faults.append("%s: feed order %s, expected %s" % (key, emitted[:3], wanted[:3]))
    return faults


def check_redirects(out: str, model: Model) -> list[str]:
    faults = []
    for key in model.redirects():
        path = os.path.join(out, route_of(key), "index.html")
        if not os.path.exists(path):
            faults.append("%s: sets redirect_to but no page was emitted" % key)
            continue
        page = open(path, encoding="utf-8").read()
        target = "%s/%s/" % (BASE_URL, str(model.sections[key]["redirect_to"]).strip("/"))
        if target not in page:
            faults.append("%s: redirect stub does not point at %s" % (key, target))
    return faults


# ---------------------------------------------------------------------------
# Spot goldens
# ---------------------------------------------------------------------------

GOLDEN_SITE = os.path.join(REPO, "performance", "fixtures", "gazette", "site")
GOLDEN_EXPECTED = os.path.join(REPO, "performance", "fixtures", "gazette", "expected")


def check_port_sync() -> list[str]:
    """The production template set has not moved without a port review.

    This is the only automated guard on ADR-0072's stated risk that production
    drift erodes the ports. It cannot tell a cosmetic change from a structural
    one — nothing can — so it does the one thing a machine can do honestly:
    refuse to let the question go unasked.
    """
    digest, count, size = tree_digest(os.path.join(REPO, PRODUCTION_TEMPLATE_ROOT))
    if digest == PRODUCTION_TEMPLATE_DIGEST:
        return []
    return ["%s changed (now %d files, %d bytes, digest %s) without a port "
            "review: check whether examples/gazette/templates/ should follow, "
            "then advance PORT_REVISION and PRODUCTION_TEMPLATE_DIGEST in "
            "scripts/gazette-corpus-diff.py"
            % (PRODUCTION_TEMPLATE_ROOT, count, size, digest)]


def port_sync_report() -> str:
    digest, count, size = tree_digest(os.path.join(REPO, PRODUCTION_TEMPLATE_ROOT))
    return ("production templates: %d files, %d bytes, digest %s%s"
            % (count, size, digest,
               "" if digest == PRODUCTION_TEMPLATE_DIGEST else "  (PINNED DIGEST IS STALE)"))


def golden_mode(options) -> int:
    """The markup-level check ADR-0072 Decision 4 asks spot goldens for.

    The goldens sit on a committed MINIATURE site rather than on live corpus
    pages, and the reason is Decision 2: the corpus is deliberately not frozen,
    so a golden over a live page would fail on every content change and be
    re-blessed rather than read — which is the failure mode a golden exists to
    avoid. The miniature site uses gazette's real template port and real
    configuration, so the goldens are sensitive to exactly what they are for:
    the rendered markup-level form the normalized oracle deliberately ignores,
    including the recursive navigation's active branch, the feed, and the
    redirect stub.
    """
    gazette = options.gazette or build_gazette()
    work = tempfile.mkdtemp(prefix="gazette-golden-")
    try:
        root = os.path.join(work, "site")
        shutil.copytree(GOLDEN_SITE, root)
        shutil.copytree(os.path.join(REPO, "examples", "gazette", "templates"),
                        os.path.join(root, "templates"))
        shutil.copy(os.path.join(REPO, "examples", "gazette", "config.toml"),
                    os.path.join(root, "config.toml"))
        out = os.path.join(work, "public")
        result = run_build(gazette, root, out)
        if result["exit_code"] != 0:
            print(result["stdout"] + result["stderr"])
            return 1

        emitted = walk_files(out)
        if options.bless:
            print(port_sync_report())
            if os.path.exists(GOLDEN_EXPECTED):
                shutil.rmtree(GOLDEN_EXPECTED)
            shutil.copytree(out, GOLDEN_EXPECTED)
            print("blessed %d file(s) into %s"
                  % (len(emitted), os.path.relpath(GOLDEN_EXPECTED, REPO)))
            return 0

        expected = walk_files(GOLDEN_EXPECTED)
        failures = []
        failures += check_port_sync()
        if emitted != expected:
            failures.append("file set differs: only emitted %s; only expected %s"
                            % (sorted(set(emitted) - set(expected)),
                               sorted(set(expected) - set(emitted))))
        for rel in sorted(set(emitted) & set(expected)):
            ours = open(os.path.join(out, rel), "rb").read()
            theirs = open(os.path.join(GOLDEN_EXPECTED, rel), "rb").read()
            if ours != theirs:
                failures.append("%s differs from its golden" % rel)
        print("goldens: %d file(s) compared" % len(set(emitted) & set(expected)))
        for fault in failures:
            print("  FAIL %s" % fault)
        print("\n%s" % ("FAILED" if failures else "OK"))
        return 1 if failures else 0
    finally:
        if options.keep:
            print("\nwork tree kept at %s" % work)
        else:
            shutil.rmtree(work, ignore_errors=True)


# ---------------------------------------------------------------------------


def site_mode(options) -> int:
    gazette = options.gazette or build_gazette()
    work = tempfile.mkdtemp(prefix="gazette-site-")
    try:
        root = os.path.join(work, "site")
        os.makedirs(root)
        identity = prepare_fixture(root, options.scale)
        model = Model(os.path.join(root, "content"))

        print("fixture identity (recorded with every observation, ADR-0072 Decision 2)")
        for key in sorted(identity):
            print("  %-16s %s" % (key, identity[key]))
        print("  %-16s %d" % ("pages", len(model.pages)))
        print("  %-16s %d" % ("sections", len(model.sections)))
        print("excluded from the corpus:")
        for rel, why in sorted(CORPUS_EXCLUSIONS.items()):
            print("  %s — %s" % (rel, why))

        # --- determinism, and the metrics ---------------------------------
        samples = []
        hashes = []
        outputs = []
        for index in range(options.samples):
            out = os.path.join(work, "public-%d" % index)
            outputs.append(out)
            sample = run_build(gazette, root, out)
            if sample["exit_code"] != 0:
                print("\ngazette build failed (exit %d):" % sample["exit_code"])
                print(sample["stdout"] + sample["stderr"])
                return 1
            samples.append(sample)
            hashes.append(tree_hash(out))

        print("\nmeasurement (fixture preparation and judging are outside the window)")
        for index, sample in enumerate(samples):
            print("  sample %d  %.3fs  peak RSS %.1f MiB"
                  % (index, sample["wall_seconds"],
                     sample["peak_rss_bytes"] / (1024 * 1024)))
        print("  binary size %d bytes" % os.path.getsize(gazette))
        print("  %-16s %s" % ("output_digest", hashes[0]))

        failures = []
        if len(set(hashes)) != 1:
            failures.append("samples are not byte-identical: %s" % hashes)

        out = outputs[0]
        emitted = set(walk_files(out))
        expected = model.expected_files(
            [rel for rel in walk_files(os.path.join(root, "static"))])
        if emitted != expected:
            extra = sorted(emitted - expected)[:6]
            absent = sorted(expected - emitted)[:6]
            failures.append("file set differs: %d extra %s, %d missing %s"
                            % (len(emitted - expected), extra,
                               len(expected - emitted), absent))

        failures += check_membership(out, model)
        failures += check_feeds(out, model)
        failures += check_redirects(out, model)
        failures += check_metadata(out, model)
        link_count, into_excluded, known_broken, link_faults = check_links(
            out, model, options.scale)
        failures += link_faults

        oracle_checked = 0
        if options.zola:
            oracle_checked, oracle_faults = check_semantics(
                out, os.path.join(root, "content"), model, gazette, work,
                options.verbose)
            failures += oracle_faults

        print("\nvalidation")
        print("  determinism:        %d sample(s), %d distinct output tree(s)"
              % (len(hashes), len(set(hashes))))
        print("  file set:           %d emitted, %d expected" % (len(emitted), len(expected)))
        print("  section membership: %d section(s)" % len(model.members))
        print("  feed ordering:      %d feed(s)" % len(model.feeds()))
        print("  redirects:          %d" % len(model.redirects()))
        print("  internal links:     %d checked, %d into excluded content, "
              "%d known-broken in the corpus" % (link_count, into_excluded, known_broken))
        for route, (per_copy, why) in sorted(KNOWN_BROKEN_LINKS.items()):
            print("      %s (%d per corpus copy) — %s" % (route, per_copy, why))
        if options.zola:
            print("  semantic oracle:    %d page(s) compared against Zola" % oracle_checked)
        else:
            print("  semantic oracle:    SKIPPED (--no-zola)")

        for fault in failures[:20]:
            print("  FAIL %s" % fault)
        if len(failures) > 20:
            print("  ... and %d more" % (len(failures) - 20))
        print("\n%s" % ("FAILED" if failures else "OK"))
        if options.identity_out:
            identity["output_digest"] = hashes[0]
            identity["pages"] = len(model.pages)
            with open(options.identity_out, "w", encoding="utf-8") as handle:
                json.dump(identity, handle, indent=2, sort_keys=True)
        return 1 if failures else 0
    finally:
        if options.keep:
            print("\nwork tree kept at %s" % work)
        else:
            shutil.rmtree(work, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--gazette", help="prebuilt gazette binary")
    parser.add_argument("--keep", action="store_true", help="keep the work tree")
    parser.add_argument("--verbose", action="store_true", help="print every page")
    modes = parser.add_subparsers(dest="mode", required=True)

    modes.add_parser("body", help="RUE-1483's Markdown differential against Zola")

    site = modes.add_parser("site", help="RUE-1484's whole-site validation")
    site.add_argument("--scale", type=int, default=1, choices=(1, 10, 100),
                      help="corpus scale variant (ADR-0072 Decision 2)")
    site.add_argument("--samples", type=int, default=3,
                      help="builds per run; determinism needs at least two")
    site.add_argument("--no-zola", dest="zola", action="store_false",
                      help="skip the cross-tool semantic oracle")
    site.add_argument("--identity-out", help="write the recorded identity as JSON")

    golden = modes.add_parser("golden", help="markup-level spot goldens")
    golden.add_argument("--bless", action="store_true",
                        help="rewrite the committed goldens from this build")

    options = parser.parse_args()
    if options.mode == "body":
        return body_mode(options)
    if options.mode == "golden":
        return golden_mode(options)
    return site_mode(options)


if __name__ == "__main__":
    sys.exit(main())
