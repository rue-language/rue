#!/usr/bin/env python3
"""Corpus-driven checks of gazette against the LIVE rue-lang.dev corpus.

Several modes, one apparatus. All of them assemble the corpus exactly as
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

    scripts/gazette-corpus-diff.py peers [--scale N]
        RUE-1485's cross-tool comparison. Builds the identical corpus with
        gazette, the pinned Zola, and the pinned Hugo — each through its own
        native template port and parity configuration — under the thread parity
        ADR-0072 Decision 5 requires, and judges work equivalence across all
        three before reporting a single number.

    scripts/gazette-corpus-diff.py prepare --root DIR --out JSON [--peers]
    scripts/gazette-corpus-diff.py judge --root DIR --gazette-out DIR [...]
        The two halves `rue-bench runtime` calls, either side of the measured
        window. `prepare` assembles every tool's fixture and records the
        identity; `judge` runs the whole validation stack over emitted trees.
        Measurement itself belongs to `rue-bench`, so every tool's time comes
        from one clock and every tool's memory from one reaping.

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
with open(os.path.join(REPO, "website", "spec-route-root.txt"), encoding="ascii") as handle:
    SPEC_ROUTE_ROOT = handle.read().strip()
if not re.fullmatch(r"[a-z0-9-]+(?:/[a-z0-9-]+)*", SPEC_ROUTE_ROOT):
    raise SystemExit(
        "website/spec-route-root.txt must contain a non-empty lower-case ASCII route"
    )

# The peer half of the comparison, in its own file so that its digest rides the
# COMPARISON identity and not the workload one (RUE-1493). `sys.path` already
# has this directory when the script is run directly; naming it explicitly
# keeps a `-P`-style invocation from turning a design decision into an
# ImportError.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gazette_peer_ports as peer_ports  # noqa: E402

# ADR-0072 Decision 2's "template ports and parity configurations", split in
# two because they answer two different questions (RUE-1493).
#
# THE PARTITION IS THE POINT. Gazette's own port is an input to the program
# under measurement: change a gazette template and gazette does different work,
# so the change must open a new segment in Rue's longitudinal series. The Zola
# and Hugo ports are not inputs to gazette at all — no gazette build reads them
# — so a Hugo layout edit that cannot change what gazette does must NOT segment
# gazette's series. Revision 2 rode all three on one digest, which was wrong in
# both directions at once: too aggressive for Rue's series, and (since peer
# VERSIONS were outside the identity entirely) too weak for the peer join.
#
# The split is a FILE split and not only a digest one, because this file's own
# digest is inside the workload identity: peer pins that lived here would move
# that identity every time a maintainer bumped one. The peer half is
# `gazette_peer_ports.py`, whose digest rides the comparison identity alone.
#
# The DIGESTS are authoritative, computed from the bytes and unable to drift;
# the revisions are the human-legible labels a maintainer bumps when changing a
# port deliberately, so a reader of two observations can say "the port moved"
# without diffing two hashes.
#
# The revision stays at 2 through RUE-1493: no gazette template changed here,
# and a label that advanced anyway would tell a reader diffing two observations
# that the port moved when it did not. What changed is the COMPOSITION of the
# identities, which `PREPARER_REVISION` records.
GAZETTE_PORT_REVISION = 2

# The revision of the FIXTURE ASSEMBLY this script implements, which
# `performance/runtime.toml` pins for the gazette workloads. It is not a port
# revision: a port is what a tool renders, and this is how the tree they render
# is built — the corpus assembly, the exclusions, the normalizations, the scale
# duplication, and the composition of the identities recorded with every
# observation. Bump it whenever any of those changes what a tool consumes or
# what an observation records, so a manifest pinning an assembly this script no
# longer implements is refused rather than silently measuring a different job.
# The seeded generator's `generator_revision` is checked the same way and for
# the same reason.
#
# REVISION 2 (RUE-1493) splits the recorded identity in two: `fixture_digest`
# now covers only what gazette consumes, and `comparison_digest` covers the
# peer ports and peer versions beside it. A harness pinning revision 1 would
# get no comparison identity at all, so it is refused here rather than left to
# produce records that cannot join.
#
# REVISION 3 (RUE-1496) drops the lower-casing pass from `assemble_corpus`.
# Gazette slugifies a content path now, so the three tools agree on the three
# upper-case file names without the input being rewritten — which means the
# tools consume a different tree than they did under revision 2, however
# slightly, and the WORKLOAD identity has to move for it. That is the correct
# outcome rather than a cost: a new comparable segment is exactly what a change
# to what every tool consumes should open.
#
# REVISION 4 (RUE-1503) derives the static passthrough from the committed
# tree: every file git ignores under `website/static` is excluded from both
# the copy and the digest. Until now the exclusion was one hardcoded name,
# and `website/build.sh` generates TWO files there — `status.json` slipped
# through, so a machine that had ever built the website recorded a different
# `static_digest` than a clean checkout. The identity is supposed to be a
# property of the corpus, not of the working tree's build history. Worse, two
# of `status.json`'s rows are derived from the performance store, which is
# the same benchmark-feeds-itself loop the `performance-data.json` exclusion
# and ADR-0072 Decision 3's page carve-outs exist to prevent.
PREPARER_REVISION = 4

# What gazette itself renders. Inside the workload identity. The peers' roots
# are `peer_ports.PEER_PORT_ROOTS`, deliberately in the other file.
GAZETTE_PORT_ROOTS = [
    "examples/gazette/config.toml",
    "examples/gazette/templates",
]

# The corpus rules the peer half borrows: the base URL it strips from a link
# target, the front-matter fence it re-fences a respelled page with, the
# scale-prefix rule, the file walk, and the route rule. Passed rather than
# duplicated — two spellings of `route_of` is exactly the silent divergence
# Decision 2 exists to prevent — and passed rather than imported back, so the
# dependency runs one way and the peer file can be hashed on its own.
def corpus_rules() -> peer_ports.CorpusRules:
    return peer_ports.CorpusRules(
        base_url=BASE_URL,
        front_matter=FRONT_MATTER,
        unprefixed=unprefixed,
        walk_files=walk_files,
        route_of=route_of,
    )

# The production template set the ports derive from, as it stood at the last
# review. ADR-0072's Consequences name the risk this discharges: "Production
# drift away from the ports quietly erodes the live framing, so website-template
# changes should include a port review." A comment asking for a review is not a
# review, so the digest is pinned and the `golden` mode fails when it moves.
#
# IT STAYS ON THE WORKLOAD SIDE of the RUE-1493 split, in this file, even though
# the review it forces covers all three ports. Two reasons. It is not an input
# to either recorded identity — no observation carries it, and it changes no
# measurement — so it is a review gate rather than an identity, and putting a
# gate in the comparison file would make a production-template edit move the
# comparison digest and re-run the peer leg for a change no tool consumed. And
# `website/templates` is GAZETTE's production source: the peer ports are ports
# OF gazette's port, so the review starts here and reaches them through it.
#
# When it fires, the fix is to look at what changed in `website/templates/`,
# decide whether each port should follow, change the ports that should, then
# advance the revision of each port that moved and paste the new digest here.
# Advancing a revision is not busywork, and it is now two separate decisions:
# GAZETTE_PORT_REVISION is what moves the WORKLOAD identity, PEER_PORT_REVISION
# (in `gazette_peer_ports.py`) is what moves the COMPARISON identity, and a
# moved identity starts a new comparable segment in that series rather than
# shifting an existing one under a reader. A re-pin on its own advances
# neither: the digest tracks the tree it was measured in, and the revisions
# track the ports.
#
# THE LEDGER OF REVIEWS, newest last. Each entry records what moved and what
# followed, because the conclusion "no port follows" is the part a later reader
# needs and the part a bare digest cannot carry.
#
#   RUE-1485. `runtime.html` moved: the cross-tool table it renders gained data
#   and the captions for the scale axis. It is the template of a page EXCLUDED
#   from the benchmark corpus, so no port template derives from it and none
#   followed. The port revision advanced anyway, for an unrelated reason in the
#   same change — the peer ports joined the fixture identity.
#
#   RUE-1495. `runtime.html` again, for the side-by-side source panel ADR-0072
#   Decision 8 asks for. Same conclusion, no port followed, and the revision
#   deliberately did NOT advance: advancing it would have opened a new
#   comparable segment to record a presentation change no port followed, which
#   is exactly the cost the paragraph above says a revision exists to buy
#   deliberately.
#
#   RUE-1493. `runtime.html` again, and the same conclusion for the third time:
#   an excluded page's template, no port follows. It moved because the empty
#   state now says what a newest run that publishes no denominator means, and
#   because a joined row is now published against the peer configuration as well
#   as the corpus. Neither port revision advanced FOR this: no port's bytes
#   moved in this change, and a revision that advanced anyway would tell a
#   reader diffing two observations that a port moved when it did not. What did
#   change is the COMPOSITION of the identities, which PREPARER_REVISION
#   records.
#
#   RUE-1496. `runtime.html` a fourth time, and the fourth identical conclusion:
#   an excluded page's template, no port follows. It moved because the
#   disclosure it renders said three file names are lower-cased before any tool
#   sees the corpus, which stopped being true when gazette learned to slugify a
#   content path. GAZETTE_PORT_REVISION did not advance — no gazette template's
#   bytes moved — and PEER_PORT_REVISION did not either, for a reason argued
#   beside it in `gazette_peer_ports.py`: both peer configurations lost the same
#   stale comment, and neither peer's rendering changed. What did change is an
#   assembly rule, which PREPARER_REVISION records.
#
#   RUE-1503. `runtime.html` a fifth time, and the fifth identical conclusion:
#   an excluded page's template, no port follows. It moved because the
#   disclosure now says the static passthrough is the committed tree — the
#   site build derives `status.json` and `performance-data.json` into
#   `website/static`, and only the latter was excluded, so the recorded
#   identity depended on whether the website was ever built. No port's bytes
#   moved. What did change is an assembly rule, which PREPARER_REVISION
#   records.
PRODUCTION_TEMPLATE_ROOT = "website/templates"
PRODUCTION_TEMPLATE_DIGEST = (
    "12e9ab194652e71ff147598103aed456d63206e2a8ca53d8461aad6b145fff9d"
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
#
# The table is empty, and staying empty is the point rather than an accident of
# there being nothing to say. Its one entry was `std/math/` — the standard-
# library index linked `math/` relative to `/std/`, but the page is
# `01-math.md` and routes to `/std/01-math/` — and RUE-1492 fixed the content
# instead, so the entry went stale by its own rule and was removed with the fix.
KNOWN_BROKEN_LINKS = {}

# Links per page into the two excluded pages: the desktop navigation bar names
# both, and the mobile menu names both again. Asserted rather than printed, for
# the reason the count exists at all — an exclusion is only "visible rather
# than silent" if a change in its blast radius is visible too.
EXCLUDED_LINKS_PER_PAGE = 4


# ---------------------------------------------------------------------------
# Corpus assembly — exactly what website/build.sh does
# ---------------------------------------------------------------------------

# The one excluded feature the corpus itself turns on. See `assemble_corpus`.
PAGINATE_BY = re.compile(r"^paginate_by = .*\n", re.M)


def assemble_corpus(dest: str, exclude: dict | None = None) -> list[str]:
    """Copy site content plus the specification, rewriting spec-internal links.

    ONE normalization is applied to the assembled tree, outside any measured
    window and applied to EVERY tool's copy of it, because ADR-0072 Decision 4's
    cross-tool criterion is that the tools consume the identical corpus and emit
    the identical file set.

    One key is stripped from the assembled front matter: `paginate_by`.
    Pagination is outside the equivalence subset for every tool (ADR-0072
    Decision 3), and it is the one excluded feature the CORPUS turns on rather
    than a configuration file — so leaving it in place would have Zola emitting
    `blog/page/1/index.html` for a paginated view gazette and the Hugo port
    never build. Stripping it here rather than in one tool's fixture is what
    keeps the sentence "every tool builds the identical corpus within a run"
    true.

    THERE WERE TWO UNTIL RUE-1496, and what the second one was is worth
    recording because it is now a closed gap rather than a live one. Every
    content file name was lower-cased here, for the three files in the corpus
    that have an upper-case one (`spec/appendices/A-grammar.md` and its two
    siblings). Zola slugifies a content path's file stem into its route and Hugo
    lower-cases it into its page identity, so both served
    `/spec/appendices/a-grammar/` — what the production site serves — while
    gazette, which did not slugify at all, would have emitted
    `/spec/appendices/A-grammar/`. That was a Rue capability gap suppressed by
    rewriting the input for all three tools, and the exact file-set check it
    bought was exact only because the input had been changed to make it so.
    Gazette slugifies now (`content_route` in `examples/gazette/tmpl_eval.rue`),
    so the three tools agree on those routes with the corpus as it stands, and
    `route_of` below is the independent model of the same rule.

    THE NORMALIZATION IS NOT VISIBLE IN THE CONTENT DIGEST, and an earlier
    version of this docstring wrongly claimed otherwise. The digest is taken
    over the tree this rule has already rewritten, so a new section adopting
    `paginate_by` produces byte-identical assembled content and an unchanged
    digest. What records it is `prepare_fixture`'s `preparer_revision` and
    `preparer_digest`, which are inside the fixture identity for exactly this
    reason: the rules are as much an input to the measured job as the bytes are.
    """
    shutil.copytree(os.path.join(REPO, "website", "content"), dest)
    spec_dest = os.path.join(dest, *SPEC_ROUTE_ROOT.split("/"))
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
                handle.write(pattern.sub("@/%s/\\1" % SPEC_ROUTE_ROOT, body))

    for rel in exclude or {}:
        victim = os.path.join(dest, rel)
        if os.path.exists(victim):
            os.remove(victim)

    for dirpath, _dirs, files in os.walk(dest):
        for name in files:
            if not name.endswith(".md"):
                continue
            path = os.path.join(dirpath, name)
            with open(path, encoding="utf-8") as handle:
                body = handle.read()
            stripped = PAGINATE_BY.sub("", body)
            if stripped != body:
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write(stripped)

    pages = []
    for dirpath, _dirs, files in os.walk(dest):
        for name in sorted(files):
            if name.endswith(".md"):
                rel = os.path.relpath(os.path.join(dirpath, name), dest)
                pages.append(rel.replace(os.sep, "/"))
    return sorted(pages)


# The route subset, as the independent model of it. THE WHOLE ROUTE is checked,
# directories included, and not only the file stem that folds: every component
# must be ASCII lower-case letters, digits, and `-` once a page's stem has been
# case-folded. `Docs/page.md` is refused rather than routed.
#
# THIS IS THE SECOND IMPLEMENTATION OF ONE RULE, deliberately (ADR-0072
# Decision 4): gazette computes a route in `content_route`, and the validation
# recomputes it here from the source tree and compares. So the subset has to be
# the same subset — a model that quietly slugified more than gazette does would
# turn a real gap into a passing check, which is the failure RUE-1496 closed.
ROUTE_SUBSET = re.compile(r"^[a-z0-9/-]*$")


def route_of(rel: str) -> str:
    """Zola's route for a content path, which is also the page's permalink.

    A page's FILE STEM folds case; DIRECTORY COMPONENTS are copied as authored
    and must already be lower-case, and a section's route is its directories, so
    nothing folds there. `spec/appendices/A-grammar.md` routes to
    `spec/appendices/a-grammar/`; `Docs/page.md` is refused.

    THE SUBSET IS EXACTLY WHERE THE TWO PEERS AGREE, measured against both
    pinned binaries rather than read off Zola's documentation:

        content path           zola 0.21.0          hugo 0.152.2
        plain/A-Grammar.md     /plain/a-grammar/    /plain/a-grammar/    agree
        plain/Under_Score.md   /plain/under-score/  /plain/under_score/  differ
        plain/Dots.v2.md       /plain/dots-v2/      /plain/dots.v2/      differ
        UPPER/lower.md         /UPPER/lower/        /upper/lower/        differ
        Mixed/Sub/_index.md    /Mixed/Sub/          /mixed/sub/          differ
        Dir_Under/p.md         /Dir_Under/p/        /dir_under/p/        differ

    Zola slugifies the stem with the `slug` crate and copies directories
    verbatim; Hugo lower-cases the whole path and touches nothing else. They
    agree on one thing: an ASCII letter's case in a file stem. Everywhere else
    no single rule can satisfy both, so Decision 4's file-set equality could not
    hold whichever side gazette picked, and refusing is the only answer that
    does not pick one silently.
    """
    stem = rel[: -len(".md")]
    if stem == "_index":
        return ""
    if stem.endswith("/_index"):
        return checked_route(stem[: -len("_index")], rel)
    head, sep, name = stem.rpartition("/")
    return checked_route(head + sep + name.lower(), rel) + "/"


def checked_route(route: str, rel: str) -> str:
    if not ROUTE_SUBSET.match(route):
        raise SystemExit(
            "%s routes to /%s, which is outside the slug subset gazette "
            "implements: every component must be lower-case ASCII letters, "
            "digits, and `-`, and only a page's file stem folds case. That is "
            "where the pinned Zola and Hugo agree, and they route everything "
            "else differently — so widen it only when they stop disagreeing, "
            "in `content_route`, here, and in ADR-0072 Decision 3, or rename "
            "the file. A preparer that modelled more than gazette implements "
            "would hide the difference rather than surface it" % (rel, route))
    return route


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------


def build_gazette() -> str:
    """Compile gazette at RELEASE QUALITY, as `performance/runtime.toml` does.

    `-O3` is not decoration here. ADR-0072 Decision 5 defines the measured
    product as the release build and the manifest pins `-O3` for every runtime
    epoch, while the compiler's own default is `-O0`. A script reporting numbers
    taken at `-O0` would be measuring something the contract does not describe,
    whatever the measured difference happened to be on the day — and this script
    is what the CI work-equivalence lane runs.
    """
    binary = subprocess.run(
        [os.path.join(REPO, "scripts", "rue-bin")],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    out = os.path.join(tempfile.mkdtemp(prefix="gazette-bin-"), "gazette")
    subprocess.run(
        [binary, os.path.join(REPO, "examples", "gazette", "main.rue"), "-O3", "-o", out],
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

    # `paginate_by` is already stripped by `assemble_corpus`, for every tool at
    # once rather than for this renderer alone.
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
        # date. An absent date sorts LAST, matching the `weight` path above and
        # Zola's treatment of an absent key: `chr(255)` is above every inverted
        # date byte, since inverting a digit or a dash cannot exceed 210.
        #
        # This returned `chr(0)` until RUE-1485, which sorted an undated page
        # FIRST and contradicted both this method's docstring and ADR-0072
        # Decision 2. It was latent only because every post in the corpus is
        # dated: the first undated one would have failed `check_membership` and
        # stalled collection over an oracle bug rather than a tool bug.
        return "".join(chr(255 - ord(c)) for c in date) if date else chr(255)

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


# The scale variants this script can assemble.
#
# 1, 10, and 100 are ADR-0072 Decision 2's page-count rungs; 1 and 10 are the
# collected suite and 100 is the safety valve held in reserve. 2 IS NOT A RUNG
# OF THE PUBLISHED CURVE and never enters `performance/runtime.toml` — it is
# the smallest fixture that is DUPLICATED, and duplication is the property
# required CI needs to exercise (RUE-1493).
#
# The failure class it exists for is real and was found the expensive way:
# Hugo picks a page's layout from its PATH, so every copy under an `xN-KK/`
# prefix fell out of the specification layout and rendered with no sidebar,
# while the emitted file set and the page count stayed exactly right at every
# scale. Nothing at 1x can see that, because at 1x nothing is duplicated. A 2x
# fixture asks the same question as a 10x one — is a copy translated into each
# tool's dialect the way its original is — for a fifth of the corpus work,
# which is what keeps it inside a required lane's budget (Decision 9's cost
# valve).
#
# ITS RESIDUAL, stated rather than left to be discovered: one copy answers "is a
# copy treated like its original", and nothing else. A defect that needs SEVERAL
# copies — a collision between two copies' routes, a cache keyed on something
# only many copies collide on, a threshold an aggregation crosses at some page
# count — is invisible here exactly as it was at 1x. The 10x rung still runs on
# the collection regime and the 100x variant is still available from this
# script, which is where a defect of that shape would surface.
SCALES = (1, 2, 10, 100)


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


def static_passthrough_files(static_source: str) -> list[str]:
    """The static files a fixture carries: the tree minus git-ignored output.

    `website/build.sh` generates derived files INTO `website/static`
    (`status.json`, `performance-data.json`), so what that directory contains
    depends on whether the site was ever built in this working tree — and the
    fixture identity must not (RUE-1503). The registry of "generated, never
    committed" is the `.gitignore` entries that already have to exist for that
    output, so the exclusion is derived from git rather than from a second
    hand-maintained name list here, which is how `status.json` got past the
    `performance-data.json` entry.

    Refusal was considered and rejected for the ignored class: refusing would
    make the preparer unrunnable on any machine that ever ran `build.sh`,
    for files the repository has already declared to be build output. An
    untracked file that is NOT ignored still rides along, exactly like an
    uncommitted content edit: the digest moves and the record says so.

    No git, no answer: without the ignore rules there is no way to tell site
    content from build output, and preparing a fixture whose identity depends
    on the working tree's build history is the defect this function exists to
    prevent, so that case refuses rather than degrades.
    """
    try:
        listing = subprocess.run(
            ["git", "-C", REPO, "ls-files", "--others", "--ignored",
             "--exclude-standard", "-z", "--", "website/static"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except OSError as error:
        raise SystemExit(
            "gazette-corpus-diff: cannot run git to list ignored files under "
            "website/static (%s); without the ignore rules, site content "
            "cannot be told apart from generated build output, and a fixture "
            "identity that depends on whether the website was ever built is "
            "the defect this check prevents (RUE-1503)" % error)
    if listing.returncode != 0:
        raise SystemExit(
            "gazette-corpus-diff: cannot list git-ignored files under "
            "website/static (git exited %d: %s); without the ignore rules, "
            "site content cannot be told apart from generated build output, "
            "and a fixture identity that depends on whether the website was "
            "ever built is the defect this check prevents (RUE-1503)"
            % (listing.returncode, listing.stderr.decode("utf-8", "replace").strip()))
    prefix = "website/static/"
    ignored = {entry[len(prefix):] for entry in
               listing.stdout.decode("utf-8").split("\0")
               if entry.startswith(prefix)}
    return [rel for rel in walk_files(static_source) if rel not in ignored]


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
    # The corpus AS BUILT, after duplication. The digest above identifies the
    # corpus and is deliberately taken before the copies — a scale variant of an
    # unchanged corpus should not look like a content change — but a record that
    # reported 95 files for a 950-page build would be describing the wrong job
    # to anyone reading the store, so both are carried.
    scaled_files = 0
    scaled_bytes = 0
    for rel in walk_files(content):
        scaled_files += 1
        scaled_bytes += os.path.getsize(os.path.join(content, rel))

    shutil.copytree(os.path.join(REPO, "examples", "gazette", "templates"),
                    os.path.join(root, "templates"))
    shutil.copy(os.path.join(REPO, "examples", "gazette", "config.toml"),
                os.path.join(root, "config.toml"))

    # ONE list drives both the digest and the copy, so the fixture cannot
    # carry a file the recorded identity does not cover — the split where a
    # hardcoded exclusion in one of two call sites could quietly disagree
    # with the other.
    static_source = os.path.join(REPO, "website", "static")
    static_files = static_passthrough_files(static_source)
    static_digest, static_count, static_bytes = tree_digest(static_source, static_files)
    os.makedirs(os.path.join(root, "static"), exist_ok=True)
    for rel in static_files:
        dest = os.path.join(root, "static", rel.replace("/", os.sep))
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        shutil.copy2(os.path.join(static_source, rel), dest)

    port_digest, port_files, port_bytes = port_identity(GAZETTE_PORT_ROOTS)

    identity = {
        "scale": scale,
        # The ASSEMBLY RULES, not just the assembled bytes. `assemble_corpus`
        # excludes two pages, strips `paginate_by`, and rewrites the
        # specification's internal links — decisions that change what all three
        # tools consume and that no other field here can see, because the
        # digests below are taken over the tree AFTER they have been applied.
        # (It lower-cased three file names too, until gazette learned to
        # slugify and the rule stopped being needed; that removal is the reason
        # the revision is 3.) Without these two entries, editing an
        # assembly rule would change the measured job while leaving the
        # recorded identity untouched, and Decision 2's whole mechanism —
        # a moved identity opens a new comparable segment — would not fire.
        #
        # The revision is the human-legible label; the digest is the
        # authoritative one, since it cannot be forgotten. The digest is
        # deliberately over-sensitive — editing a comment in this file opens a
        # new segment — because the failure it prevents is silent and the
        # failure it causes is a visible discontinuity in a series read for
        # order of magnitude.
        "preparer_revision": PREPARER_REVISION,
        "preparer_digest": tree_digest(
            os.path.dirname(os.path.abspath(__file__)),
            [os.path.basename(os.path.abspath(__file__))],
        )[0],
        "content_digest": content_digest,
        "content_files": content_files,
        "content_bytes": content_bytes,
        "scaled_content_files": scaled_files,
        "scaled_content_bytes": scaled_bytes,
        "static_digest": static_digest,
        "static_files": static_count,
        "static_bytes": static_bytes,
        # GAZETTE's port alone. The peer ports are not inputs to gazette — no
        # gazette build reads them — so including them here would open a new
        # segment in Rue's own wall-clock series for a Hugo template edit that
        # cannot change what gazette does. They ride the comparison identity
        # instead (ADR-0072 Decision 2, RUE-1493).
        "gazette_port_revision": GAZETTE_PORT_REVISION,
        "gazette_port_digest": port_digest,
        "gazette_port_files": port_files,
        "gazette_port_bytes": port_bytes,
        "excluded": sorted(CORPUS_EXCLUSIONS),
    }
    # One digest over all of it, so an observation can be matched to a segment
    # with a single comparison. Every input class GAZETTE consumes is inside it:
    # no content, static asset, gazette template, configuration, or assembly-rule
    # change can alter the measured job without changing this value.
    identity["fixture_digest"] = compose_digest(identity)
    return identity


def port_identity(roots: list[str]) -> tuple[str, int, int]:
    """One digest, file count, and byte count over a set of port roots."""
    digest = hashlib.sha256()
    files = 0
    total = 0
    for entry in roots:
        path = os.path.join(REPO, entry)
        if os.path.isdir(path):
            entry_digest, count, size = tree_digest(path)
        else:
            entry_digest, count, size = tree_digest(os.path.dirname(path),
                                                    [os.path.basename(path)])
        digest.update(entry.encode("utf-8"))
        digest.update(entry_digest.encode("ascii"))
        files += count
        total += size
    return digest.hexdigest(), files, total


def compose_digest(fields: dict) -> str:
    """One digest over a set of identity fields, in sorted key order.

    Deliberately over-sensitive — editing a comment in this file moves
    `preparer_digest` and so moves the workload identity — because the failure
    it prevents is silent and the failure it causes is a visible discontinuity
    in a series read for order of magnitude.
    """
    fingerprint = hashlib.sha256()
    for key in sorted(fields):
        fingerprint.update(("%s=%s;" % (key, fields[key])).encode("utf-8"))
    return fingerprint.hexdigest()


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
            "then advance GAZETTE_PORT_REVISION and PRODUCTION_TEMPLATE_DIGEST "
            "in scripts/gazette-corpus-diff.py"
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

# ===========================================================================
# The cross-tool leg — ADR-0072 Phase 5 (RUE-1485), Phase 5's identity split
# (RUE-1493)
# ===========================================================================
#
# The peers' half of it lives in `gazette_peer_ports.py` and not here, because
# this file's digest is inside the WORKLOAD identity. Everything about what the
# peers do — the pinned tools, their ports, Hugo's dialect of the corpus, the
# invocations, the thread parity, and the cross-tool oracle — belongs to the
# COMPARISON identity instead, and sharing a file would have meant sharing a
# digest: bumping a peer port revision, or fixing a Hugo respelling defect,
# would open a new segment in Rue's own wall-clock series for a change no
# gazette build can observe (ADR-0072 Decision 2).
#
# What stays here is what measures ALL THREE tools with one clock and one
# reaping, because a comparison whose tools were timed differently measures the
# harness.

def run_process(argv: list[str], environment: dict) -> dict:
    """One measured process, spawn to exit, with THIS child's peak RSS.

    The same `wait4` reaping `run_build` documents, and the same function for
    every tool on purpose: a comparison in which one tool's time came from a
    different clock, or one tool's memory from a cumulative high-water mark,
    would be measuring the harness rather than the tools.
    """
    start = time.perf_counter()
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, env={**os.environ, **environment})
    stdout = process.stdout.read()
    stderr = process.stderr.read()
    _pid, status, usage = os.wait4(process.pid, 0)
    wall = time.perf_counter() - start
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



# ---------------------------------------------------------------------------
# Fixture preparation for the whole comparison
# ---------------------------------------------------------------------------


def prepare_comparison(root: str, scale: int, peers: bool, epoch: str = "") -> dict:
    """Assemble every tool's site from one corpus and record both identities.

    All of it is outside the measured window: assembly, duplication, the
    peers' respelling, and the hashing. Only the builds that follow are timed.

    TWO IDENTITIES, not one (ADR-0072 Decision 2, as amended by RUE-1493):

      * The WORKLOAD identity — `identity["fixture_digest"]` — is everything
        gazette consumes: the corpus, the static passthrough, the assembly
        rules, and gazette's own port. It segments Rue's longitudinal series,
        and it must not move when a peer port does.
      * The COMPARISON identity — `identity["comparison_digest"]` — is the
        workload identity plus every peer port, the peer preparer's own digest
        and revision, every peer version, and the runner epoch. It is what the
        derive step joins a canary-only run to an earlier full peer leg on, and
        it is what the peer-leg event question below is asked against.

    The peer VERSIONS are inside the comparison identity rather than beside
    it, which is the half revision 2 was missing: a Hugo shim bump moved no
    recorded identity at all, so a failed full leg left the next canary-only
    run joining a Hugo row measured under a version the project no longer
    pins — self-labelled, and therefore not a misattributed number, but a row
    published against a pin that had never successfully run.
    """
    identity = prepare_fixture(root, scale)
    description = {
        "identity": identity,
        "scale": scale,
        "roots": {"gazette": root},
        "peer_versions": {},
        "epoch": epoch,
        "thread_policies": list(peer_ports.THREAD_POLICIES),
    }
    if not peers:
        return description

    corpus = os.path.join(root, "content")
    static = os.path.join(root, "static")
    for tool in peer_ports.PEERS:
        description["roots"][tool] = peer_ports.prepare_peer_site(
            tool, root, corpus, static, corpus_rules())
        description["peer_versions"][tool] = peer_ports.peer_version(tool)

    peer_digest, peer_files, peer_bytes = port_identity(
        peer_ports.PEER_PORT_ROOTS)
    identity["peer_port_revision"] = peer_ports.PEER_PORT_REVISION
    identity["peer_port_digest"] = peer_digest
    identity["peer_port_files"] = peer_files
    identity["peer_port_bytes"] = peer_bytes
    # The peer PREPARER's own digest, the mirror of `preparer_digest` in the
    # workload identity and the reason the two files exist. What decides what
    # the peers DO — the respelling, the invocations, the thread parity, the
    # cross-tool oracle, and the port pins above — is in that file, so this
    # digest moves when any of them does and the workload identity does not.
    # Over-sensitive for the same reason its counterpart is: editing a comment
    # there opens a new comparable segment for the COMPARISON, and the failure
    # it prevents is a joined row published against rules that changed under it.
    #
    # ONE PIECE STAYED BEHIND, and the claim above is bounded by it. `peer_event`
    # decides whether the peers run at all, and it reads the identity composed
    # here, so moving it would invert the one-way dependency the split bought.
    # Editing it therefore still moves `preparer_digest` and still segments
    # Rue's series for a peer-side change — the residual of finding 3 rather
    # than an oversight, named here so a later reader weighs it rather than
    # rediscovers it.
    identity["peer_preparer"] = os.path.relpath(peer_ports.__file__, REPO)
    identity["peer_preparer_digest"] = tree_digest(
        os.path.dirname(os.path.abspath(peer_ports.__file__)),
        [os.path.basename(peer_ports.__file__)],
    )[0]
    identity["peer_versions"] = json.dumps(description["peer_versions"],
                                           sort_keys=True)
    identity["comparison_epoch"] = epoch
    identity["comparison_digest"] = compose_digest({
        "fixture_digest": identity["fixture_digest"],
        "peer_port_revision": peer_ports.PEER_PORT_REVISION,
        "peer_port_digest": peer_digest,
        "peer_preparer_digest": identity["peer_preparer_digest"],
        "peer_versions": identity["peer_versions"],
        "epoch": epoch,
    })
    return description


# ---------------------------------------------------------------------------
# The per-run peer canary and the event-driven full peer leg (Decision 9)
# ---------------------------------------------------------------------------
#
# The canary is one single-threaded Zola build of the 1x corpus, run beside
# every gazette observation. It is cheap on this corpus and it is what gives
# every observation a SAME-RUN ratio denominator, so no segment's ratio ever
# leans on a stale or singleton peer sample.
#
# The full peer leg — Hugo, the scale variants, the default-parallel secondary
# row — runs only on an event: the recorded fixture identity moved, a peer
# version moved, or the runner epoch changed. `peer_event` is what the
# collection job asks at fixture-preparation time, in the same run.

CANARY_TOOL = "zola"
CANARY_SCALE = 1


def peer_event(description: dict, epoch: str, state_path: str | None) -> dict:
    """Whether the full peer leg is due, and why.

    ONE QUESTION, asked of the comparison identity: did anything the joined
    rows depend on move? That identity is composed of exactly what a joined row
    depends on — the workload identity, the peer ports and their revision, the
    peer preparer's own digest, the peer versions, and the epoch — so the
    verdict is a single digest comparison and cannot fall out of step with what
    the derive step joins on. The components are still compared individually,
    but only to say WHICH one moved; the digest decides, and a move it sees
    that no component explains still runs the leg.

    THIS FUNCTION STAYS ON THE WORKLOAD SIDE, and it is the one thing about the
    peers that does. It reads the composed comparison identity, so moving it
    into `gazette_peer_ports.py` would invert the one-way dependency the file
    split bought. The residual is real and worth naming: editing the Decision 9
    event logic here moves `preparer_digest`, and so opens a new segment in
    Rue's own series for a change that only decides whether the peers run. It
    is bounded by being one function that changes rarely, where the peer
    respelling and the ports it was separated from change with every parity
    expansion.
    """
    identity = description["identity"]
    current = {
        "comparison_digest": identity.get("comparison_digest", ""),
        "fixture_digest": identity["fixture_digest"],
        "peer_port_digest": identity.get("peer_port_digest", ""),
        "peer_preparer_digest": identity.get("peer_preparer_digest", ""),
        "peer_versions": description["peer_versions"],
        "epoch": epoch,
    }
    previous = None
    unreadable = ""
    if state_path and os.path.exists(state_path):
        try:
            with open(state_path, encoding="utf-8") as handle:
                previous = json.load(handle)
        except (OSError, ValueError) as error:
            # A half-restored Actions cache is a MISSING state, not a fatal
            # one. Falling through costs one redundant peer leg; propagating
            # would abort a whole collection run over a file whose only job is
            # to save a minute.
            previous = None
            unreadable = " (%s could not be read: %s)" % (state_path, error)
    if previous is None:
        return {"due": True,
                "reason": "no previous peer observation is recorded" + unreadable,
                "state": current}
    reasons = []
    if previous.get("fixture_digest") != current["fixture_digest"]:
        reasons.append("the recorded workload identity moved")
    if previous.get("peer_port_digest") != current["peer_port_digest"]:
        reasons.append("a peer template port moved")
    if previous.get("peer_preparer_digest") != current["peer_preparer_digest"]:
        reasons.append("the peer preparer moved")
    if previous.get("peer_versions") != current["peer_versions"]:
        reasons.append("a peer toolchain version moved (%s -> %s)"
                       % (previous.get("peer_versions"), current["peer_versions"]))
    if previous.get("epoch") != current["epoch"]:
        reasons.append("the runner epoch changed (%s -> %s)"
                       % (previous.get("epoch"), current["epoch"]))
    moved = previous.get("comparison_digest") != current["comparison_digest"]
    if moved and not reasons:
        # The digest is authoritative; a move it sees and the components above
        # do not means the identity's own composition changed, which is a
        # preparer change and just as much an event.
        reasons.append("the comparison identity moved (%s -> %s)"
                       % (str(previous.get("comparison_digest"))[:12],
                          current["comparison_digest"][:12]))
    # `or reasons` runs the leg when a component moved and the digest did not,
    # which can only be a composition defect. The conservative direction costs
    # a redundant peer run; the other costs a segment of joined ratios.
    return {"due": moved or bool(reasons),
            "reason": "; ".join(reasons) or
                      "workload identity, peer ports, peer preparer, peer "
                      "versions, and epoch are unchanged",
            "state": current}


# ---------------------------------------------------------------------------


def peers_mode(options) -> int:
    """Build the identical corpus with all three tools and judge the result."""
    gazette = options.gazette or build_gazette()
    work = tempfile.mkdtemp(prefix="gazette-peers-")
    try:
        root = os.path.join(work, "site")
        os.makedirs(root)
        # This mode runs wherever it was invoked rather than on a pinned
        # regime, so it has no runner epoch to name. The comparison identity it
        # prints is therefore the epoch-less one and will not equal a
        # collection run's; the workload identity above it is the same value
        # collection records, since the epoch is not one of its inputs.
        description = prepare_comparison(root, options.scale, peers=True,
                                         epoch=options.epoch)
        identity = description["identity"]
        model = Model(os.path.join(root, "content"))

        print("recorded identities (both ride every observation, ADR-0072 "
              "Decision 2)")
        for key in sorted(identity):
            print("  %-22s %s" % (key, identity[key]))
        print("peers (pinned; every observation records both versions)")
        for tool in peer_ports.PEERS:
            print("  %-16s %s" % (tool, description["peer_versions"][tool]))

        measurements: dict = {}
        digests: dict = {}
        faults: list[str] = []

        def measure(label, argv, environment, out_prefix):
            samples = []
            trees = []
            for index in range(options.samples):
                out = os.path.join(work, "%s-%d" % (out_prefix, index))
                sample = run_process(argv(out), environment)
                if sample["exit_code"] != 0:
                    faults.append("%s build failed (exit %d): %s"
                                  % (label, sample["exit_code"],
                                     (sample["stdout"] + sample["stderr"])[-800:]))
                    return None, None
                samples.append(sample)
                trees.append((out, tree_hash(out)))
            hashes = {digest for _out, digest in trees}
            if len(hashes) != 1:
                faults.append(
                    "%s is not deterministic: %d distinct output trees across %d "
                    "samples. A port that emits a build timestamp — Hugo's feed "
                    "`lastBuildDate` and `generator` are the known cases — fails "
                    "exactly here." % (label, len(hashes), len(samples)))
            measurements[label] = samples
            digests[label] = sorted(hashes)[0]
            return trees[0][0], samples

        gazette_out, _ = measure(
            "gazette",
            lambda out: [gazette, "build", root, "-o", out],
            {},
            "gazette-out")

        peer_outs = {}
        for tool in peer_ports.PEERS:
            for policy in peer_ports.THREAD_POLICIES:
                if policy != peer_ports.THREAD_POLICIES[0] and not options.secondary:
                    continue
                label = "%s/%s" % (tool, policy)
                out, _ = measure(
                    label,
                    lambda out, tool=tool: peer_ports.peer_command(
                        tool, description["roots"][tool], out, work),
                    peer_ports.PEER_THREAD_ENV[tool][policy],
                    label.replace("/", "-") + "-out")
                if policy == peer_ports.THREAD_POLICIES[0] and out is not None:
                    peer_outs[tool] = out

        # DIAGNOSTIC TIMINGS, never published. The published series is taken by
        # `rue-bench runtime` on the pinned regime; these run wherever this
        # script was invoked, usually beside a compiler build. Every sample is
        # printed rather than only the median, because the first build of a run
        # is cold — the binary was just compiled and the fixture just written —
        # and a reader who cannot see that cannot discount it.
        print("\nmeasurement (diagnostic only; fixture preparation and judging "
              "are outside the window)")
        for label in sorted(measurements):
            samples = measurements[label]
            times = [sample["wall_seconds"] for sample in samples]
            rss = max(sample["peak_rss_bytes"] for sample in samples)
            print("  %-32s median %.3fs  peak RSS %.1f MiB  samples %s"
                  % (label, median(times), rss / (1024 * 1024),
                     " ".join("%.3f" % value for value in times)))

        if gazette_out is not None and len(peer_outs) == len(peer_ports.PEERS):
            report, cross_faults = peer_ports.cross_tool_check(
                gazette_out, peer_outs, model, options.scale, corpus_rules())
            faults += cross_faults
            print("\nwork equivalence (ADR-0072 Decision 4)")
            print("  page count:         %s" % report["pages"])
            print("  semantic oracle:    %d page(s) compared across three tools"
                  % report["oracle_pages"])
            print("  file-set allowlist: %s"
                  % ({tool: sorted(entries) for tool, entries in
                      peer_ports.FILE_SET_ALLOWLIST.items() if entries}))
        else:
            faults.append("not every tool produced a build, so work equivalence "
                          "could not be judged")

        # The primary ratio, and the secondary row beside it rather than
        # instead of it.
        if "gazette" in measurements:
            base = median([sample["wall_seconds"] for sample in measurements["gazette"]])
            print("\nratios at scale %dx (gazette = 1.00) — DIAGNOSTIC, not the "
                  "published comparison, which is derived from records taken on "
                  "the pinned regime" % options.scale)
            for label in sorted(measurements):
                if label == "gazette":
                    continue
                other = median([sample["wall_seconds"] for sample in measurements[label]])
                marker = "" if label.endswith(peer_ports.THREAD_POLICIES[0]) else "   [secondary]"
                print("  %-32s %.2fx%s" % (label, other / base, marker))

        for fault in faults[:20]:
            print("  FAIL %s" % fault)
        if len(faults) > 20:
            print("  ... and %d more" % (len(faults) - 20))
        print("\n%s" % ("FAILED" if faults else "OK"))

        if options.out:
            payload = {
                "identity": identity,
                "peer_versions": description["peer_versions"],
                "scale": options.scale,
                "output_digests": digests,
                "measurements": {
                    label: [{"wall_seconds": sample["wall_seconds"],
                             "peak_rss_bytes": sample["peak_rss_bytes"]}
                            for sample in samples]
                    for label, samples in measurements.items()
                },
                "faults": faults,
            }
            with open(options.out, "w", encoding="utf-8") as handle:
                json.dump(payload, handle, indent=2, sort_keys=True)
        return 1 if faults else 0
    finally:
        if options.keep:
            print("\nwork tree kept at %s" % work)
        else:
            shutil.rmtree(work, ignore_errors=True)


def median(values: list[float]) -> float:
    """The median, averaging the two middles — `rue_perf_schema::median`'s rule.

    This took the UPPER of two middles until RUE-1485's review, and the
    difference was not academic. Combined with the first sample of a run being
    cold — the binary was just compiled and the fixture just written, so the
    first build pays page-cache costs the rest do not — a two-sample run
    reported the cold sample as its median. That is where this script's quoted
    gazette figure of 0.44s came from against the harness's 0.246s on the same
    machine: not `-O0`, not host speed, but a warm-up sample promoted to the
    headline by a median that rounded the wrong way.

    Two defences now: the convention matches the harness, and `peers` prints
    every sample so a cold first one is visible rather than averaged into a
    number nobody can question.
    """
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2 == 1:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


# ---------------------------------------------------------------------------
# The harness interface: prepare, judge, and the peer-event question
# ---------------------------------------------------------------------------
#
# `rue-bench runtime` owns measurement — it starts the clock, reaps the child,
# and writes the record — and calls these two modes for everything either side
# of the measured window. ADR-0072 names this script the reference
# implementation the harness mode should ADOPT rather than reinvent, and two
# implementations of corpus assembly, routing, and validation would be two
# implementations to disagree.


def prepare_mode(options) -> int:
    os.makedirs(options.root, exist_ok=True)
    description = prepare_comparison(options.root, options.scale, options.peers,
                                     options.epoch)
    description["commands"] = {
        "gazette": {"argv": ["{program}", "build", options.root, "-o", "{out}"],
                    "env": {}},
    }
    for tool in peer_ports.PEERS if options.peers else ():
        for policy in peer_ports.THREAD_POLICIES:
            description["commands"]["%s/%s" % (tool, policy)] = {
                "argv": peer_ports.peer_command(tool, description["roots"][tool],
                                     "{out}", options.root),
                "env": peer_ports.PEER_THREAD_ENV[tool][policy],
            }
    description["canary"] = {"tool": CANARY_TOOL, "scale": CANARY_SCALE}
    description["preparer_revision"] = PREPARER_REVISION
    model = Model(os.path.join(options.root, "content"))
    description["pages"] = len(model.pages)
    if options.peer_state or options.peer_event_out:
        event = peer_event(description, options.epoch, options.peer_state)
        description["peer_event"] = event
        if options.peer_event_out:
            with open(options.peer_event_out, "w", encoding="utf-8") as handle:
                json.dump(event, handle, indent=2, sort_keys=True)
    with open(options.out, "w", encoding="utf-8") as handle:
        json.dump(description, handle, indent=2, sort_keys=True)
    print("prepared %dx fixture in %s (%d pages, fixture digest %s)"
          % (options.scale, options.root, description["pages"],
             description["identity"]["fixture_digest"][:12]))
    return 0


def judge_mode(options) -> int:
    """Judge emitted output trees. Never times anything; never builds anything."""
    root = options.root
    model = Model(os.path.join(root, "content"))
    failures: list[str] = []
    out = options.gazette_out

    emitted = set(walk_files(out))
    expected = model.expected_files(walk_files(os.path.join(root, "static")))
    if emitted != expected:
        failures.append("file set differs: %d extra %s, %d missing %s"
                        % (len(emitted - expected), sorted(emitted - expected)[:6],
                           len(expected - emitted), sorted(expected - emitted)[:6]))
    failures += check_membership(out, model)
    failures += check_feeds(out, model)
    failures += check_redirects(out, model)
    failures += check_metadata(out, model)
    _links, _excluded, _known, link_faults = check_links(out, model, options.scale)
    failures += link_faults

    peer_outs = {}
    for tool in peer_ports.PEERS:
        directory = getattr(options, "%s_out" % tool)
        if directory:
            peer_outs[tool] = directory
    report = {}
    if peer_outs:
        report, cross_faults = peer_ports.cross_tool_check(
            out, peer_outs, model, options.scale, corpus_rules())
        failures += cross_faults

    verdict = {
        "verdict": "match" if not failures else "mismatch",
        "detail": "; ".join(failures[:5]),
        "failures": failures,
        "output_digest": tree_hash(out),
        "pages": len(model.pages),
        "peers_judged": sorted(peer_outs),
        "cross_tool": report,
    }
    if options.out:
        with open(options.out, "w", encoding="utf-8") as handle:
            json.dump(verdict, handle, indent=2, sort_keys=True)
    print("%s: %d check failure(s)" % (verdict["verdict"], len(failures)))
    for failure in failures[:20]:
        print("  FAIL %s" % failure)
    return 1 if failures else 0


# ---------------------------------------------------------------------------


def site_mode(options) -> int:
    gazette = options.gazette or build_gazette()
    work = tempfile.mkdtemp(prefix="gazette-site-")
    try:
        root = os.path.join(work, "site")
        os.makedirs(root)
        identity = prepare_fixture(root, options.scale)
        model = Model(os.path.join(root, "content"))

        print("workload identity (recorded with every observation, ADR-0072 "
              "Decision 2)")
        for key in sorted(identity):
            print("  %-22s %s" % (key, identity[key]))
        print("  %-22s %d" % ("pages", len(model.pages)))
        print("  %-22s %d" % ("sections", len(model.sections)))
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
    site.add_argument("--scale", type=int, default=1, choices=SCALES,
                      help="corpus scale variant (ADR-0072 Decision 2)")
    site.add_argument("--samples", type=int, default=3,
                      help="builds per run; determinism needs at least two")
    site.add_argument("--no-zola", dest="zola", action="store_false",
                      help="skip the cross-tool semantic oracle")
    site.add_argument("--identity-out", help="write the recorded identity as JSON")

    golden = modes.add_parser("golden", help="markup-level spot goldens")
    golden.add_argument("--bless", action="store_true",
                        help="rewrite the committed goldens from this build")

    peers = modes.add_parser(
        "peers", help="RUE-1485's cross-tool comparison against Zola and Hugo")
    peers.add_argument("--scale", type=int, default=1, choices=SCALES,
                       help="corpus scale variant (ADR-0072 Decision 2)")
    peers.add_argument("--samples", type=int, default=3,
                       help="builds per tool; determinism needs at least two")
    peers.add_argument("--no-secondary", dest="secondary", action="store_false",
                       help="skip the peers' default-parallel secondary row")
    peers.add_argument("--epoch", default="",
                       help="the runner epoch the comparison identity is scoped "
                            "to; this mode measures nothing publishable, so it "
                            "defaults to none")
    peers.add_argument("--out", help="write the comparison as JSON")

    prepare = modes.add_parser(
        "prepare", help="assemble every tool's fixture and print its identity")
    prepare.add_argument("--root", required=True, help="directory to build the fixture in")
    prepare.add_argument("--scale", type=int, default=1, choices=SCALES)
    prepare.add_argument("--out", required=True, help="where to write the description")
    prepare.add_argument("--peers", action="store_true",
                         help="also lay out the Zola and Hugo sites")
    prepare.add_argument("--epoch", default="",
                         help="the runner epoch, for the peer-event question")
    prepare.add_argument("--peer-state",
                         help="the previous peer run's recorded state, as JSON")
    prepare.add_argument("--peer-event-out",
                         help="write the peer-event answer as JSON")

    judge = modes.add_parser(
        "judge", help="judge emitted output trees; times nothing, builds nothing")
    judge.add_argument("--root", required=True, help="the prepared fixture root")
    judge.add_argument("--gazette-out", required=True, help="gazette's emitted tree")
    judge.add_argument("--zola-out", help="Zola's emitted tree, when the peer leg ran")
    judge.add_argument("--hugo-out", help="Hugo's emitted tree, when the peer leg ran")
    judge.add_argument("--scale", type=int, default=1, choices=SCALES)
    judge.add_argument("--out", help="write the verdict as JSON")

    options = parser.parse_args()
    if options.mode == "body":
        return body_mode(options)
    if options.mode == "golden":
        return golden_mode(options)
    if options.mode == "peers":
        return peers_mode(options)
    if options.mode == "prepare":
        return prepare_mode(options)
    if options.mode == "judge":
        return judge_mode(options)
    return site_mode(options)


if __name__ == "__main__":
    sys.exit(main())
