"""The peer half of ADR-0072's cross-tool comparison (RUE-1493).

A SEPARATE FILE BECAUSE IT IS A SEPARATE IDENTITY. `gazette-corpus-diff.py`
composes the WORKLOAD identity, and a digest of that whole file is inside it —
deliberately over-sensitive, because a rule change that decides what gazette
consumes must never be able to move the measured job without moving the
recorded identity.

That is exactly why the peer half cannot live there. Everything in this module
decides what the PEERS do and nothing about what gazette does: the pinned
tools and their versions, the peer template ports, Hugo's dialect of the
corpus, the peers' invocations and thread parity, and the cross-tool oracle
that judges all three. Sharing a file with the workload rules meant sharing a
digest with them, so bumping `PEER_PORT_REVISION` — which the port-review
comment instructs a maintainer to do whenever a peer port moves — opened a new
segment in Rue's own wall-clock series. So did any edit to `hugoize`, which is
where ALL THREE Hugo port defects RUE-1485 found were fixed: the page-type
translation, the duplicated landing page, and the prepend-not-append front
matter. Those are the very changes ADR-0072 Decision 2 now cites as the reason
the identity was split, and under one file they would still discard Rue's
history.

This module's digest therefore rides the COMPARISON identity alone
(`comparison_digest`), beside the peer ports and the peer versions, and the
workload identity is blind to it — as it should be, since no gazette build
reads a line of it.

The dependency runs one way. This module imports nothing from the workload
preparer; the few corpus rules it needs — the base URL, the front-matter
fence, the scale-prefix rule, the file walk, and the route rule — arrive as an
explicit [`CorpusRules`] from the caller. Copying them here would be worse than
passing them: two spellings of `route_of` is precisely the silent divergence
Decision 2 exists to prevent, and it would show up as a cross-tool fault
nobody could attribute.
"""

from __future__ import annotations

import collections
import html.parser
import os
import re
import shutil
import subprocess

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The corpus rules this module borrows from the workload preparer, passed in
# rather than duplicated. Every one of them is a property of the CORPUS, so the
# workload preparer owns it and this module is a consumer.
CorpusRules = collections.namedtuple(
    "CorpusRules",
    ["base_url", "front_matter", "unprefixed", "walk_files", "route_of"])

# The revision of the PEER PREPARATION this file implements — the ports, the
# respelling, the invocations, and the cross-tool oracle. It is the
# human-legible label beside this file's digest in the comparison identity, the
# way the workload preparer's revision sits beside its own; the digest is
# authoritative because it cannot be forgotten.
#
# It stays at 2 through RUE-1493: the identity split moved this code between
# files without changing a rule of it, and neither peer port's bytes moved. The
# composition change is recorded by the workload preparer's revision, which is
# what the manifest pins.
#
# It stays at 2 through RUE-1496 as well, and that case is the harder one to
# state. Both peer configurations changed bytes — each carried a comment
# explaining that the corpus was lower-cased for gazette's benefit, and the
# corpus is not lower-cased any more — so `peer_port_digest` moves. Nothing
# either peer RENDERS moved: Zola still slugifies, Hugo still lower-cases, and
# both route the three appendices exactly where they did. The revision is the
# label for a deliberate change to what a port renders, so advancing it here
# would tell a reader diffing two observations that a peer's output changed
# when only a comment did. The digest is the authoritative half and it records
# the edit either way.
PEER_PORT_REVISION = 2

# What the PEERS render, and nothing gazette reads. Inside the comparison
# identity alone.
PEER_PORT_ROOTS = [
    "performance/ports/zola/config.toml",
    "performance/ports/zola/templates",
    "performance/ports/hugo/hugo.toml",
    "performance/ports/hugo/layouts",
]


PEERS = ("zola", "hugo")

# Thread policies a peer is measured under. The FIRST is the primary published
# ratio's denominator; the second is the secondary row.
#
# `RAYON_NUM_THREADS` is Zola's rendering pool; `GOMAXPROCS` is Hugo's
# scheduler. Neither is a fairness fudge in gazette's favour — pinning the
# peers to one thread makes the comparison per unit of work rather than per
# core, and the four-way-parallel numbers are published beside it so the figure
# a user would actually experience is never hidden.
THREAD_POLICIES = ("pinned_single_thread", "tool_default_parallel")

PEER_THREAD_ENV = {
    "zola": {"pinned_single_thread": {"RAYON_NUM_THREADS": "1"},
             "tool_default_parallel": {}},
    "hugo": {"pinned_single_thread": {"GOMAXPROCS": "1"},
             "tool_default_parallel": {}},
}

# The documented allowlist ADR-0072 Decision 4 requires for tool-mandated
# differences in the emitted file set. It is deliberately as small as the tools
# allow, because every entry is a place a tool could skip a page and hide
# behind an excuse — so a difference that CAN be configured away is configured
# away in the parity configs rather than allowed here. Three candidates were
# removed that way and are named so the absence is legible rather than
# accidental: Hugo's sitemap and robots.txt (`disableKinds`), Hugo's per-section
# and site-wide feeds (`[outputs]`), and the feed FILENAME mapping the ADR
# anticipated — `[outputFormats.rss] baseName` puts Hugo's feed at the same
# route Zola's and gazette's occupy, so the routes themselves are compared.
#
# Static passthrough is the third difference the ADR names and needs no entry
# either: all three tools copy the same `website/static` tree, and its bytes
# are part of the recorded fixture identity.
FILE_SET_ALLOWLIST = {
    "zola": {
        "404.html": (
            "Zola always writes a 404 page, from a built-in template when the "
            "site has none, and offers no switch to suppress it. Hugo emits "
            "one only when a layout exists, and the Hugo port deliberately has "
            "none, so this is the one difference no configuration can remove"
        ),
    },
    "hugo": {},
}


def peer_binary(tool: str) -> str:
    return os.path.join(REPO, tool)


def peer_version(tool: str) -> str:
    """The pinned peer's own version string, recorded with every observation."""
    argv = [peer_binary(tool), "--version" if tool == "zola" else "version"]
    result = subprocess.run(argv, capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit("could not read %s's version:\n%s" % (tool, result.stderr))
    text = result.stdout.strip().splitlines()[0]
    if tool == "zola":
        return text.split()[-1]
    # `hugo v0.152.2-6abdac… darwin/arm64 BuildDate=…`
    return text.split()[1].split("-")[0].lstrip("v")


# ---------------------------------------------------------------------------
# Hugo's dialect of the same corpus
# ---------------------------------------------------------------------------
#
# The corpus is written in Zola's dialect, so the Hugo leg needs it respelled:
# Hugo cannot read `{{ rule(id="…") }}` or `@/path.md` any more than Zola could
# read `{{< rule id="…" >}}`. The translation is mechanical, deterministic, and
# happens at fixture-preparation time, outside every measured window.
#
# WHAT IT MAY NOT DO is the part worth stating. It respells calls; it never
# performs them. `@/spec/03-types.md` becomes `{{< relref "/spec/03-types.md" >}}`
# rather than a resolved URL, so Hugo still resolves the link itself; shortcode
# invocations keep their arguments and are still expanded by Hugo's shortcode
# machinery. Resolving either host-side would move measured work out of the
# measured program, and no output check would notice, because the resolved page
# looks the same either way.

SHORTCODE_CALL = re.compile(r"\{\{\s*([a-z_]+)\((.*?)\)\s*\}\}", re.S)
SHORTCODE_ARG = re.compile(r'([a-z_]+)\s*=\s*("[^"]*")')
INTERNAL_LINK = re.compile(r"\]\(@/([^)\s]+)\)")


def hugoize(corpus: str, rules: CorpusRules) -> None:
    """Respell the corpus in Hugo's dialect, in place."""
    for dirpath, _dirs, files in os.walk(corpus):
        for name in sorted(files):
            if not name.endswith(".md"):
                continue
            path = os.path.join(dirpath, name)
            with open(path, encoding="utf-8") as handle:
                text = handle.read()
            found = rules.front_matter.match(text)
            if not found:
                raise SystemExit("%s does not open with a `+++` fence" % path)
            front, body = found.group(1), text[found.end():]

            added = []
            # WHICH TEMPLATE RENDERS THIS PAGE, in Hugo's spelling. The corpus
            # already tells Zola, in `template` and `page_template`; Hugo's
            # equivalent is the page's type, and translating one into the other
            # is the same kind of respelling as the shortcode syntax below.
            #
            # It is also load-bearing, which is why it is not left to Hugo's
            # default. Hugo derives an untyped page's layout from its PATH, and
            # ADR-0072 Decision 2's scale variants live under `xN-KK/` prefixes
            # — so at 10x every copy of a specification page fell out of
            # `layouts/spec/` and rendered through the plain page layout, with
            # no sidebar at all: 17,940 bytes where gazette and Zola emit
            # 22,443 and 22,630. Nine tenths of the corpus, with the most
            # expensive template in the port skipped, while the file set and
            # the page count stayed exactly right. Setting the type from the
            # UNPREFIXED path makes a copy render as its original does.
            section = rules.unprefixed(
                os.path.relpath(path, corpus).replace(os.sep, "/"))
            if "/" in section:
                added.append('type = "%s"' % section.split("/", 1)[0])
            # Zola asks for a section feed with `generate_feeds`; Hugo asks for
            # one by naming the section's output formats. Same feed, same
            # route, each tool's own spelling.
            if re.search(r"^generate_feeds = true", front, re.M):
                added.append('outputs = ["html", "rss"]')
            # Zola acts on `redirect_to` natively; Hugo selects a layout by
            # name. The TARGET is still resolved by Hugo's template.
            if re.search(r"^redirect_to = ", front, re.M):
                added.append('layout = "redirect"')
            # The corpus's root `_index.md` names the landing-page template
            # explicitly, and Zola and gazette honour that key wherever the page
            # sits. Hugo reserves its home layout for the site root, so a
            # DUPLICATED root — `x10-00/_index.md`, one per copy at scale — is
            # an ordinary section to it and rendered as one, dropping the hero,
            # the specimen, and the recent-notes list the other two tools build.
            # Naming the layout is the same translation as `redirect_to` above.
            elif re.search(r'^template = "index\.html"', front, re.M):
                added.append('layout = "landing"')

            body = SHORTCODE_CALL.sub(
                lambda match: "{{< %s %s >}}" % (
                    match.group(1),
                    " ".join("%s=%s" % pair
                             for pair in SHORTCODE_ARG.findall(match.group(2)))),
                body)
            body = INTERNAL_LINK.sub(
                lambda match: '](%s)' % ('{{< relref "/%s" >}}' % match.group(1)),
                body)
            # Zola's summary marker spelled Hugo's way.
            body = body.replace("<!-- more -->", "<!--more-->")

            # PREPENDED, not appended, and the difference is not cosmetic: TOML
            # binds a bare key to the last table header above it, and the blog
            # posts' front matter ends with an `[extra]` table. Appending put
            # `type` inside it, where Hugo never looks, so every duplicated blog
            # post silently rendered through the generic page layout — no
            # byline, no title block. Top-level keys go above the first table.
            trailer = "\n".join(added) + "\n" if added else ""
            with open(path, "w", encoding="utf-8") as handle:
                handle.write("+++\n" + trailer + front + "+++\n" + body)


# ---------------------------------------------------------------------------
# Preparing all three sites from one corpus
# ---------------------------------------------------------------------------


def prepare_peer_site(tool: str, root: str, corpus: str, static: str,
                      rules: CorpusRules) -> str:
    """Lay out one peer's site root beside gazette's, from the same corpus."""
    site = os.path.join(root, tool)
    os.makedirs(site)
    shutil.copytree(corpus, os.path.join(site, "content"))
    if tool == "zola":
        shutil.copytree(os.path.join(REPO, "performance/ports/zola/templates"),
                        os.path.join(site, "templates"))
        shutil.copy(os.path.join(REPO, "performance/ports/zola/config.toml"),
                    os.path.join(site, "config.toml"))
    else:
        shutil.copytree(os.path.join(REPO, "performance/ports/hugo/layouts"),
                        os.path.join(site, "layouts"))
        shutil.copy(os.path.join(REPO, "performance/ports/hugo/hugo.toml"),
                    os.path.join(site, "hugo.toml"))
        hugoize(os.path.join(site, "content"), rules)
    shutil.copytree(static, os.path.join(site, "static"))
    return site


def peer_command(tool: str, site: str, out: str, work: str) -> list[str]:
    """The measured invocation, identical in shape for both peers.

    Hugo's cache is pointed at a path inside this run's own work directory, so
    nothing survives from a previous run or from a developer's home directory —
    which is what would otherwise make the first sample of a run do work the
    rest do not. It is shared BETWEEN this run's samples, and that is fine here
    because it stays empty: nothing in the equivalence subset populates it, and
    the emitted trees are byte-identical across samples, which is the property
    the determinism check asserts. `--noBuildLock` keeps a lock left by a killed
    sample from stalling the next one.
    """
    if tool == "zola":
        # `--root` is Zola's global flag and has to precede the subcommand.
        return [peer_binary("zola"), "--root", site, "build", "--output-dir", out]
    return [peer_binary("hugo"),
            "--source", site,
            "--destination", out,
            "--cacheDir", os.path.join(work, "hugo-cache"),
            "--noBuildLock"]


# ---------------------------------------------------------------------------
# The cross-tool semantic oracle
# ---------------------------------------------------------------------------

SKIP_TEXT_IN = {"script", "style"}


class Facets(html.parser.HTMLParser):
    """The normalized extraction ADR-0072 Decision 4 compares across tools.

    Exactly the facets the Decision names — the heading tree, visible text, and
    link and image targets, in document order — and nothing else. What it
    deliberately drops is what three different renderers and three different
    template languages are entitled to disagree about: element structure, class
    and id attributes, whitespace, and entity spelling.

    That is a weaker comparison than the gazette-vs-Zola body oracle, which
    compares the full event stream contiguously, and it is weaker for a stated
    reason rather than a convenient one: Goldmark and pulldown-cmark do not
    emit the same markup for the same Markdown, and requiring them to would
    make the check fail on every page for a difference ADR-0072's Terminology
    already calls a non-goal. What it still catches is every way a tool can do
    LESS WORK: a dropped paragraph, a missing heading, an unexpanded shortcode,
    an unresolved link, a page whose sidebar was not rendered for that page.
    """

    def __init__(self, rules: CorpusRules, route: str = "") -> None:
        super().__init__(convert_charrefs=True)
        self.rules = rules
        self.route = route
        self.headings: list = []
        self.text: list = []
        self.links: list = []
        self._skip = 0
        self._heading = None

    def handle_starttag(self, tag, attrs):
        fields = dict(attrs)
        if tag in SKIP_TEXT_IN:
            self._skip += 1
        if tag in ("h1", "h2", "h3", "h4", "h5", "h6"):
            self._heading = [tag, ""]
        if tag == "a" and fields.get("href"):
            self.links.append(
                normalize_target(self.rules, fields["href"], self.route))
        if tag == "img" and fields.get("src"):
            self.links.append(
                normalize_target(self.rules, fields["src"], self.route))

    def handle_endtag(self, tag):
        if tag in SKIP_TEXT_IN and self._skip:
            self._skip -= 1
        if tag in ("h1", "h2", "h3", "h4", "h5", "h6") and self._heading:
            self.headings.append((self._heading[0], self._heading[1].strip()))
            self._heading = None

    def handle_data(self, data):
        if self._skip:
            return
        squashed = " ".join(data.split())
        if not squashed:
            return
        self.text.append(squashed)
        if self._heading is not None:
            self._heading[1] += " " + squashed


def normalize_target(rules: CorpusRules, href: str, route: str = "") -> str:
    """A link target in the one spelling every tool can be held to.

    THE SAME DESTINATION, WRITTEN THREE WAYS, is not three destinations, and
    the three tools genuinely write it three ways:

      * Absolute or root-relative. Hugo's `absURL` and Zola's `get_url` do not
        always agree about which to emit, and a browser cannot tell them apart.

      * A bare fragment. The corpus writes `[Directives](#directives)`; Zola
        resolves it against the page's own permalink and Hugo leaves it as
        CommonMark wrote it. Both land on the same place.

      * Relative. `[math](01-math/)` on the standard-library index resolves
        against the page in every browser, and Zola resolves it at build time
        while Hugo does not. It is the corpus's ONLY page-relative destination
        — the `../…` ones are parent-relative and every tool leaves those as
        authored — so it is the single case that makes this bullet load-
        bearing rather than hypothetical. Until RUE-1492 it read `math/` and
        named a route no tool emits, and what the comparison showed then was
        that all three agreed on the dead route; what it shows now is that all
        three agree on the live one. Either way the agreement only exists once
        both spellings are resolved, which is this function's job.

    Resolving here rather than allowing three spellings is what lets the
    comparison stay a strict equality: a link that genuinely moved still fails.
    """
    target = href.strip()
    if target.startswith(rules.base_url):
        target = target[len(rules.base_url):] or "/"
    if "://" in target.split("?", 1)[0].split("#", 1)[0] or target.startswith("mailto:"):
        return target
    if target.startswith("//"):
        return target
    if target.startswith("#"):
        target = "/" + route + target
    elif not target.startswith("/"):
        target = "/" + route + target
    fragment = ""
    if "#" in target:
        target, fragment = target.split("#", 1)
        fragment = "#" + fragment
    return target.rstrip("/") + "/" + fragment if target != "/" else "/" + fragment


def facets(rules: CorpusRules, page: str, route: str = "") -> dict:
    parser = Facets(rules, route)
    parser.feed(page)
    return {
        "headings": parser.headings,
        "text": parser.text,
        "links": parser.links,
    }


def compare_facets(reference: dict, other: dict, name: str, peer: str) -> list[str]:
    faults = []
    for facet in ("headings", "text", "links"):
        want, have = reference[facet], other[facet]
        if want == have:
            continue
        missing = [item for item in want if item not in have]
        extra = [item for item in have if item not in want]
        faults.append(
            "%s: %s differs from gazette's (%d vs %d; %d missing, %d extra%s)"
            % (name, facet, len(want), len(have), len(missing), len(extra),
               "; first missing %r" % (missing[0],) if missing else ""))
    return ["%s %s" % (peer, fault) for fault in faults]


def cross_tool_check(gazette_out: str, peer_outs: dict, model,
                     scale: int, rules: CorpusRules) -> tuple[dict, list[str]]:
    """Work equivalence across the three tools (ADR-0072 Decision 4)."""
    faults: list[str] = []
    report: dict = {}

    emitted = {"gazette": set(rules.walk_files(gazette_out))}
    for tool, out in sorted(peer_outs.items()):
        emitted[tool] = set(rules.walk_files(out))

    # 1. The emitted file sets, with the documented allowlist.
    for tool, files in sorted(emitted.items()):
        if tool == "gazette":
            continue
        allowed = FILE_SET_ALLOWLIST.get(tool, {})
        extra = sorted(files - emitted["gazette"] - set(allowed))
        absent = sorted(emitted["gazette"] - files)
        unused = sorted(set(allowed) - files)
        if extra:
            faults.append("%s emits %d file(s) gazette does not, none of them "
                          "allowlisted: %s" % (tool, len(extra), extra[:6]))
        if absent:
            faults.append("%s emits %d fewer file(s) than gazette: %s"
                          % (tool, len(absent), absent[:6]))
        for rel in unused:
            faults.append(
                "the file-set allowlist entry %s/%s no longer applies: the tool "
                "stopped emitting it, so the entry is stale and must go" % (tool, rel))

    # 2. Page counts and non-emptiness. A tool cannot win by writing stubs.
    #
    # The redirect stub is the one page that IS a stub in all three tools, by
    # design and byte for byte, so it is named here rather than raising the
    # threshold until nothing trips it.
    stubs = {rules.route_of(key) + "index.html" for key in model.redirects()}
    pages = {}
    for tool, files in sorted(emitted.items()):
        out = gazette_out if tool == "gazette" else peer_outs[tool]
        html_files = sorted(rel for rel in files if rel.endswith("index.html"))
        pages[tool] = len(html_files)
        empty = [rel for rel in html_files
                 if rel not in stubs
                 and os.path.getsize(os.path.join(out, rel)) < 512]
        if empty:
            faults.append("%s emitted %d page(s) under 512 bytes: %s"
                          % (tool, len(empty), empty[:6]))
    wanted_pages = len(model.pages) * 1
    for tool, count in sorted(pages.items()):
        if count != wanted_pages:
            faults.append("%s emitted %d page(s); the corpus has %d"
                          % (tool, count, wanted_pages))
    report["pages"] = pages

    # 3. The semantic oracle, across all three tools: every original page, plus
    #    ONE COPY OF EACH at scale.
    #
    #    Comparing all 9,600 copies at 100x would buy nothing — they are the
    #    same documents at other permalinks — but comparing NONE of them was a
    #    blind spot with a defect already sitting in it. Hugo picks a page's
    #    layout from its path unless told otherwise, so every duplicate fell
    #    out of the specification layout and rendered without a sidebar: nine
    #    tenths of the corpus, with the port's most expensive template skipped,
    #    while the emitted file set and the page count stayed exactly right.
    #    One copy of each page costs a second and closes the hole, and the
    #    copies are byte-identical to each other so one is as good as all.
    prefix = "" if scale == 1 else "x%d-00/" % scale
    compared = 0
    redirects = set(model.redirects())
    for pass_prefix in ("",) if scale == 1 else ("", prefix):
        for rel in sorted(model.pages):
            if not rel.startswith(pass_prefix) or (
                pass_prefix == "" and rules.unprefixed(rel) != rel
            ):
                continue
            if rel in redirects:
                # A stub carries no page semantics; `check_redirects` asserts
                # the thing that matters about it, which is its target.
                continue
            route = rules.route_of(rel)
            reference_path = os.path.join(gazette_out, route, "index.html")
            if not os.path.exists(reference_path):
                continue
            reference = facets(
                rules, open(reference_path, encoding="utf-8").read(), route)
            for tool, out in sorted(peer_outs.items()):
                path = os.path.join(out, route, "index.html")
                if not os.path.exists(path):
                    faults.append("%s emitted no page for %s" % (tool, rel))
                    continue
                faults += compare_facets(
                    reference,
                    facets(rules, open(path, encoding="utf-8").read(), route),
                    rel, tool)
            compared += 1
    report["oracle_pages"] = compared

    # 4. The feed, across tools: same entries, same order, and the same fields
    #    carried per entry.
    #
    #    The last of those is not fussiness. Zola's BUILT-IN feed — which it
    #    falls back to when a site ships no `rss.xml` template, as this port
    #    once did — renders every post's full rendered body into its
    #    `<description>`, which is Markdown rendering and escaping that the
    #    other two tools do not perform. Comparing `<link>` order alone said
    #    the feeds agreed while one tool was doing twice the work, growing with
    #    every post. So the emptiness PATTERN of the descriptions is compared
    #    too: which entries carry one is a property of the corpus, and any tool
    #    disagreeing about it is doing a different job.
    for key in model.feeds():
        route = rules.route_of(key)
        wanted = ["%s/%s" % (rules.base_url, rules.route_of(rel))
                  for rel in model.members[key]]
        shapes = {}
        for tool, out in sorted({**peer_outs, "gazette": gazette_out}.items()):
            path = os.path.join(out, route, "rss.xml")
            if not os.path.exists(path):
                faults.append("%s emitted no feed at %srss.xml" % (tool, route))
                continue
            feed = open(path, encoding="utf-8").read()
            entries = [normalize_target(rules, link)
                       for link in re.findall(r"<link>([^<]*)</link>", feed)[1:]]
            if entries != [normalize_target(rules, link) for link in wanted]:
                faults.append("%s feed order %s, expected %s"
                              % (tool, entries[:3], wanted[:3]))
            shapes[tool] = [bool(body.strip()) for body in
                            re.findall(r"<description>(.*?)</description>", feed, re.S)[1:]]
        if "gazette" in shapes:
            for tool, shape in sorted(shapes.items()):
                if tool != "gazette" and shape != shapes["gazette"]:
                    faults.append(
                        "%s's feed carries descriptions on %d of %d entries where "
                        "gazette carries them on %d: one tool is rendering post "
                        "bodies into the feed that the others are not"
                        % (tool, sum(shape), len(shape), sum(shapes["gazette"])))
    return report, faults
