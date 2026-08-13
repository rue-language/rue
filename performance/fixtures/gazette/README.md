# gazette spot goldens

ADR-0072 Decision 4 asks for "a small set of stable pages carrying committed
expected output", covering the rendered markup-level form that the normalized
semantic oracle deliberately ignores. This directory is that check.

`site/` is a miniature Zola-shaped site — thirteen content files — and
`expected/` is gazette's byte-exact output for it, built with gazette's real
template port (`examples/gazette/templates/`) and real configuration
(`examples/gazette/config.toml`).

    scripts/gazette-corpus-diff.py golden            # compare
    scripts/gazette-corpus-diff.py golden --bless    # rewrite after a
                                                     # deliberate port change

## Why a miniature site and not live corpus pages

Decision 2 keeps the benchmark corpus deliberately unfrozen: it is the live
rue-lang.dev content, and it changes with every blog post. A golden over a
live page would therefore fail on every content change and be re-blessed
rather than read, which is precisely the failure mode a golden exists to
prevent — a check that is always red teaches its reader to bless it unseen.

The miniature site is stable by construction and exercises the same templates,
so the goldens stay sensitive to what they are actually for: a change in the
template port, the escaping, the routing, or the feed shape. The live corpus is
covered by the layers that tolerate its motion — determinism, the file set, the
independently modelled section and feed ordering, and the semantic oracle
against Zola, all in `scripts/gazette-corpus-diff.py site`.

## What the miniature site is chosen to exercise

Every template in the port, and every emitted file kind:

- the recursive specification navigation **with an active branch**, which is
  the fairness-critical construct: `spec/02-things/` is a subsection with two
  pages, so a page inside it expands the sidebar that a page outside it leaves
  collapsed. A nav that had been flattened host-side would render identically
  on both, and this is where that shows up;
- `redirect_to` on `spec/_index.md`, so the redirect stub is goldened;
- `generate_feeds` on a `sort_by = "date"` section, so the feed and its
  newest-first ordering are goldened;
- `sort_by = "weight"` sections, prev/next neighbours, both shortcodes, a
  `<!-- more -->` summary, a pipe table with an escaped cell, a multi-line
  `prompt`, an `[extra]` author list, and a static asset.
