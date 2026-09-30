# Verso Blueprint as the proof's published form: spike and evaluation

**Status:** spike complete (2026-09-30, RUE-2472). This note is the
evaluation and a recommendation. The decision belongs to the maintainers: a
new build dependency is an architecture change.

[Verso Blueprint](https://github.com/leanprover/verso-blueprint) is the Lean
community's tool for a *blueprint*: an informal account of a proof in which
each labeled node links to its Lean declarations. It renders the chapters, an
interactive dependency graph, and a progress summary. The question was whether
it should replace or complement what the formal core already has: the
generated proof map (`docs/formal/lean/MAP.md`), the generated statement list
(`SPINE.md`), and the hand-written guide (`GUIDE.md`).

## 1. What was built

The spike is a separate Lake package outside the repository. It requires
`RueCore` by path and `verso-blueprint` from git. Nothing under
`docs/formal/lean` changed. The spike only builds and reads `RueCore` in
place, and its build output is ignored.

**Version.** The default branch (`v4.34.0`, at `4848fcae`) requires Lean
v4.34.1. The `v4.33.0` maintenance branch pins Lean v4.33.1, the release the
formal core is pinned to, so the spike uses that branch's head, commit
`e0d667222e8222ac4bfd74780f69a2e89cd4655f` (2026-09-29). No toolchain override
was needed. The project publishes no tags: its template requires a branch
name, and the README describes the `v4.33.0` branch as retained for backports
only.

**Content.** The blueprint has 110 nodes and 442 edges:

- **41 spine theorems**, one per entry of `RueCore.Spec.spine`, in `SPINE.md`'s
  five sections. Each theorem is linked with `(lean := "…")` to both its proof
  and its `_stmt` statement. Its informal text is the statement's doc-comment:
  the English reading, and the calculus paragraph it realizes.
- **25 milestone lemmas**, one per entry of `RueCore.Map.milestones`. Each
  one's text is the reason given in that list.
- **44 definitions**: the definitions the spine statements name (`SPINE.md`'s
  "Names" lists) that have a `GLOSSARY.md` row. Each one's text is the
  glossary term and its meaning.
- **97 proof edges**: exactly the spine diagram's edges in `MAP.md`, written as
  `{uses …}` in each node's proof block.
- **345 statement edges**: each statement to the glossary definitions it names.

No `@[blueprint]` attribute was added to any RueCore source. A 227-line script
generates the chapters from `Spec/*.lean`, `Spec.lean`, `Map.lean`, `SPINE.md`,
`MAP.md` and `GLOSSARY.md`. All 110 Lean names resolve.

**Rendering.** `lake exe vbp build` writes a 17-page multi-page HTML site
(30 MB, with the GUIDE chapter of section 4, whose node makes 111). It has one page per chapter, a dependency-graph page and a summary
page:

- **Chapter pages.** Each node shows its informal text, `uses 12` and
  `used by 3` chips, and a collapsible panel. The panel holds the Lean
  signature, the declaration's own doc-comment, its source file and a
  per-declaration `complete` badge.
- **Summary page.** It reports "Total entries 110, completed 110, sorries 0",
  with collapsible sections: entry index, dependency insights, metadata, and
  structure and coverage.
- **Graph page.** Graphviz lays the graph out in the browser (d3-graphviz,
  fetched from a CDN at view time), with every node filled green. At the
  default fit, the full graph is a flat band about twenty times wider than it
  is tall, and node labels are unreadable. The cause is that nodes are
  clustered by chapter. The spine and milestones alone (66 nodes, the same
  set as MAP's spine diagram) are no better. The page is usable only through
  its interactive zoom, the per-group views in its `View` selector, and
  click-to-preview.

## 2. Measurements

The measurements come from one Apple-silicon laptop, with wall-clock times
from `/usr/bin/time -p`.

| Step | Wall | Notes |
| --- | --- | --- |
| `RueCore` alone, cold (for scale) | 60.6 s | 146 s CPU |
| Dependency download (`lake update`) | 19.9 s | 8 git packages: Verso, SubVerso, verso-slides, ProofWidgets, Illuminate, Plausible, MD4Lean, VersoBlueprint; 89 MB of sources |
| Dependency build, cold | 108.4 s | 648 jobs, 501 s CPU; `.lake/packages` grows to 1.1 GB |
| Blueprint modules cold, plus render | 21.8 s | with the dependencies and `RueCore` already built |
| Render, nothing changed | 14.9 s | the render step itself dominates; `lake build` alone is a 1.5 s no-op |
| Render after editing one chapter | 17.6 s | |

A cold start from nothing costs about 3.5 minutes and 1.1 GB, against about
1 minute for the formal core alone. That is roughly three and a half times
the formal core's own build, spent on documentation.

**Blueprint source per spine node:** 12.2 lines on average, 8 to 22 lines,
including its statement-edge list and its proof block. The generated
chapters total 1,003 lines. Written by hand instead of generated, this is
roughly the same amount of prose that `Spec/*.lean` already carries, in a
second place.

## 3. Against the proof map's Mermaid diagrams

**What the blueprint shows that `MAP.md` does not:**

- **The informal statement beside each node.** Clicking a graph node, or
  reading its chapter, shows the reading and the calculus paragraph next to the
  Lean signature and the declaration's own doc-comment. `MAP.md` names nodes
  and links nothing.
- **Two dependency axes.** Statement edges (the definitions a statement names)
  and proof edges (what its proof rests on) are kept apart in the data and in
  the graph. `MAP.md` has proof edges in its spine diagram. It has the
  statement-to-definition closure only in its per-theorem diagrams and
  definitions table, never in one graph with the proof edges.
- **Interactive reading.** It offers zoom, grouped subviews, previews on
  click, a search box, and a machine-readable manifest
  (`-verso-data/blueprint-manifest.json`, which includes the Graphviz source
  of the graph).
- **Formalization status.** This carries no information for this project:
  every node is complete, and the formal core's own gates already fail on any
  `sorry` or non-standard axiom (below).

**What `MAP.md` shows that the blueprint does not:**

- **Edges computed from the proofs.** `Map.walk` derives every edge from the
  compiled proof terms, through unmarked helpers, equation-compiler auxiliaries
  and Prop-valued definitions. The blueprint's edges are whatever its source
  declares. Its own inference mode (`autoDeps`) was tried on the same 66
  nodes and is not a substitute: it produced 41 edges among them against
  MAP's 97, and only 19 were MAP's. It missed 78 of MAP's edges and added 22
  that MAP does not have. So the blueprint's
  edges have to be generated from `Map.walk`, as this spike did.
- **The per-theorem diagrams**, with their calculus citations per definition.
  **Size statistics**: proof lines, and unmarked helpers under each node.
  **The assurance-chain diagram.**
- **Rendering where the repository is read.** GitHub renders `MAP.md` inline,
  with no build and no network. The blueprint needs a Lean build, a render
  step, a place to publish the site, and a CDN at view time for its graph.
- **A readable whole-graph view.** MAP's Mermaid spine diagram, grouped by
  module, is legible on the page. The blueprint's full graph is not (see
  section 1).

**Status is weaker than the existing gates.** The blueprint's status is local
to each declaration. A probe declaration whose proof reaches `sorry` only
through an untagged helper was reported **complete**. Only a directly
`sorry`-proved declaration was counted as a sorry. The package's
trusted-base lint, and its `#print axioms` listing for every trusted theorem,
already catch both cases.

## 4. Against GUIDE.md

Section 5's worked examples fit as chapters. Example 3 (`panic_after_drop`,
81 lines of GUIDE) was converted mechanically into a 153-line Verso chapter,
then given one hand-written node. That conversion needed three things
Markdown does not:

- `**bold**` becomes `*bold*`, and `_` outside inline code must be escaped
  (a bare `𝔽_w` fails to parse).
- Pipe tables become Verso's nested-list `:::table` blocks, one cell per line
  (hence the doubled length). They must be rectangular: GUIDE's short rows,
  which Markdown pads, are an error.
- A Lean code block is **elaborated**. GUIDE's snippets are expressions, not
  commands (`letIn false (resA (lit 7)) …`), so they cannot be elaborated as
  written and were converted to plain blocks.

That last point is also the one real gain. The hand-written node restates
GUIDE's snippet as `example : panicAfterDrop = letIn false (resA (lit 7)) … :=
rfl`, and the tabulated run as `example : run demoOps (prog tI64
panicAfterDrop) demoFuel = .panic .user [.drop 1 (cA 0 7), .dtor sAffine (cA 0
7)] := by rfl`. Both are checked when the blueprint builds: changing the
literal to 8 fails the build with a type mismatch. GUIDE's prose can drift
from the source today, and in a blueprint its snippets could not. The
summary counts the node as complete, and lists it only under "missing informal
coverage" because it has no informal proof block.

Section headings split into separate pages (`html-multi`), which suits
GUIDE's size. A title containing `_` must quote it as code, or it renders as
emphasis. `vbp build` also leaves pages from earlier renders in its output
directory, so a publish step has to clean it first.

## 5. Cost of keeping it

- **The pin.** A Lake workspace builds with one toolchain, so the blueprint
  package and `RueCore` must name the same Lean release, and so must
  verso-blueprint's line. Today they match only on a branch that receives
  backports and nothing else. Its new work is on the v4.34 line. Every Rue
  toolchain bump would wait for a verso-blueprint line on that release. The
  alternative is to stay on a maintenance branch that stops advancing. The
  Buck pin (`toolchains/lean/defs.bzl`) and the pin validation between it and
  `lean-toolchain` would carry the same coupling.
- **Where it can live.** Not inside `RueCore`: the layering audit
  (`ruecore-layers`) fails the build on any module in the roots' import
  closure that is neither the package's own nor the toolchain's, and
  `RueCore` has no dependencies by design. It must be a separate package, as
  in the spike. The hermetic `lean_package` build takes a source tree and
  fetches nothing, so a Buck target would need the eight dependencies
  vendored (89 MB of sources) or network access at build time.
- **What would drift.** Everything the blueprint says already exists in
  `Spec/*.lean` doc-comments, `Map.lean`, `MAP.md` and `GLOSSARY.md`.
  - If the blueprint is generated from them, as in the spike, it drifts only
    when not regenerated. A gate is the same one `SPINE.md` has: regenerate,
    compare with the committed copy, fail on a difference.
  - If it is written by hand, it is a second copy of the spine's readings,
    and no gate can check that the prose still says what the doc-comment
    says.
  - Either way, a renamed declaration is only a **warning** at build time
    ("could not be resolved … keeping parsed name"), and the summary then
    lists it as a missing declaration. A gate would have to treat that warning
    as an error, or query `vbp query` for missing declarations.
- **Per slice.** Each new spine statement or milestone needs one generator
  run, since the node, its edges and its text all come from existing sources.
  Each new GUIDE example needs a conversion by hand, and its tables need
  rework by hand.

## 6. Recommendation

**Decline adopting it as the published form for now.**

- Generated from the formal core's own sources, the blueprint adds a rendered
  site and a two-axis graph. It adds no information: every statement, edge and
  term already exists in a checked or generated file.
- Its graph is less legible than MAP's Mermaid diagrams at this size.
- Its status page is weaker than the existing lint.
- It costs about 3.5 times the formal core's build, 1.1 GB, and a toolchain
  coupling to a backports-only branch.

**Keep from it:**

- **The two dependency axes.** Let `MAP.md` distinguish statement edges
  (definitions a statement names) from proof edges in one diagram, or at least
  in its edge data, using the blueprint's *uses* vocabulary for both.
- **Its node kinds and labels as the proof map's marking convention.** The
  kinds are definition, lemma and theorem, and each node has a stable label.
  `Map.milestones` and `Spec.spine` already play the role of `@[blueprint]`
  labels without touching proof modules. That remains the right placement,
  and the spike shows it is enough to drive the tool if it is adopted later.
- **Checked snippets for GUIDE.** Its worked examples could cite, for each
  Lean snippet, the `Examples.lean` declaration or `example` it restates, and a
  check could then confirm each snippet appears there. That buys the
  blueprint's one real gain without the dependency.

**If adopted later,** the ADR would say:

- The blueprint is a separate Lake package (`docs/formal/blueprint`) that
  requires `RueCore` by path and verso-blueprint pinned by commit on the
  release line matching the Lean pin. Nothing is added to `RueCore`, and no
  attributes go in its sources.
- Its chapters are generated by a `ruecore-digest` mode from the same sources
  `SPINE.md` and `MAP.md` come from. Hand-written chapters are allowed only
  for GUIDE material, with their Lean blocks elaborated.
- The chain regenerates and compares the chapters, builds with unresolved
  declarations as errors, and renders.
- A toolchain bump waits for a matching verso-blueprint line.
- The rendered site is published with the other documentation, and the Mermaid
  map stays as the in-repository view.

The trigger to revisit is a goal of publishing the proof as a website, or a
verso-blueprint release line that tracks the Lean pin without lag.
