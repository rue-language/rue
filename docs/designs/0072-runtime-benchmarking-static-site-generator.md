---
id: 0072
title: "Runtime performance benchmarking anchored by a Rue static site generator"
status: proposal
tags: [performance, benchmarking, process, examples, stdlib]
feature-flag: null
created: 2026-08-13
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-1045", "RUE-1046", "RUE-1047", "RUE-1049", "RUE-1481", "RUE-1482", "RUE-1483", "RUE-1484", "RUE-1485", "ADR-0006", "ADR-0057", "ADR-0067", "ADR-0070", "ADR-0071"]
---

# ADR-0072: Runtime performance benchmarking anchored by a Rue static site generator

## Status

Proposal. This ADR is the design document requested by RUE-1045 and defines the
Runtime performance benchmarking project as a whole. It deliberately narrows
that project's first concrete deliverable to one realistic workload — a static
site generator written in Rue — compared tool-vs-tool against Zola and Hugo.
Other comparison programs, the microbenchmark tier, and per-benchmark peer
translations (RUE-1048) remain in the project's future scope and are not
retired by this ADR.

## Summary

Rue gains a runtime benchmarking system whose first workload is **gazette**, a
static site generator written in Rue that builds the live rue-lang.dev corpus
as the real site build assembles it. Gazette is measured whole-process by
`rue-bench` on the pinned CI runner regime, validated for deterministic,
work-equivalent output, and compared against pinned Zola (Rust) and Hugo (Go)
building the identical corpus under a feature-parity configuration. The comparison question is order of
magnitude — is Rue as fast as Zola, twice as slow, one hundred times slower —
not microsecond precision. Results are appended to the existing
`performance-data-v1` durable store and published on the website as comparison
tables and time series. The same program becomes a new maintained rung on the
compiler-performance scaling curve, so one workload advances both of Rue's
performance questions: how fast Rue compiles, and how fast compiled Rue runs.
The standard-library gaps the program exposes — directory enumeration and
substring operations — are closed on the canonical std path as part of this
work, not with benchmark-private shims.

## Context

### The project this ADR defines

The Runtime performance benchmarking project exists to answer four questions:
how Rue programs perform at runtime; how idiomatic Rue compares side by side
with peer languages; how compiled-program performance changes as the compiler
evolves; and whether we have a trusted system to iterate against while
improving performance. RUE-1045 asked for a design-first ADR covering the
harness, metrics, corpus principles, peer policy, storage, and publication.

The compiler-performance side of this story is already built: ADR-0067's
versioned measurement protocol, three scheduled suites driven by
`crates/rue-bench`, raw observations appended to the `performance-data-v1`
orphan branch by a single serialized writer, derived statistics recomputed at
website build time, and ADR-0071's pinned GitHub Actions reference regime with
a ratcheting non-regression gate. This ADR reuses that machinery rather than
inventing a parallel system: the runtime suite is a new workload class inside
the existing measurement, storage, and publication pipeline.

### Why a static site generator

A static site generator is an unusually good first realistic workload:

- **It is a real program class with strong peer exemplars.** Hugo (Go) and
  Zola (Rust) are mature, widely used, performance-proud tools. Comparing
  against them answers the peer-language question with production software
  rather than with translated microbenchmarks.
- **The corpus is already in this repository.** rue-lang.dev is a Zola site:
  ~96 markdown files and roughly 780 KB once `website/build.sh` copies the
  specification in (25 content pages plus 71 spec pages), 17 templates, and
  about 1,200 shortcode invocations. Zola v0.21.0 is already pinned as a
  dotslash shim at the repository root and runs in CI today. The comparison
  target and the input corpus require no new external dependencies beyond a
  Hugo pin.
- **The workload fits the language.** An SSG is file-, string-, map-, and
  allocation-bound. Rue has no floating-point types (ADR-0065 is accepted but
  unimplemented), which rules out classic FP suites; an SSG needs none.
- **It forces the right standard-library work.** `std/fs.rue` (ADR-0057)
  explicitly defers directory enumeration; `std/strings.rue` has no substring
  find, split, replace, or join. These are not benchmark-shaped gaps — they
  are the next things any real Rue program needs, and this workload makes
  them unavoidable in an honest way.
- **It has a north star.** A Rue SSG that can build the real site creates the
  long-term option of rue-lang.dev generating itself with a program written
  in Rue. That option is explicitly not decided here, but the trajectory is
  motivating.

The largest existing examples (lattice, harbor, meridian, caldera) are
deliberate maturity rungs, but none has an external yardstick: there is no
production lattice to compare against. Gazette is the first example whose
performance can be stated relative to software people already use.

### What does not exist yet

Rue currently cannot list a directory, find a substring, split a string, or
parse Markdown, TOML, or templates. Slices remain preview-gated, which makes
text processing more manual than in peer languages. Full Zola feature parity —
syntax highlighting via Sublime syntaxes, an elasticlunr search index, HTML
minification — is far out of reach for a v1 and is not required for an
order-of-magnitude comparison, provided the peers are configured to skip the
same work. The comparison is honest only if all three tools do the same job;
defining that job precisely is a core decision of this ADR.

### Terminology

**Fresh-process build** follows ADR-0071: every sample launches a new process
with no retained state; operating-system caches are not reset. For gazette
this means one complete site build — process spawn, content discovery,
parsing, rendering, templating, output writing, exit.

**Work equivalence** is the fairness standard for cross-tool comparison. The
three tools are equivalent when they consume the identical corpus within a
run, are configured to the same feature subset, and emit the same set of
output files with per-tool validated content. Byte-identical HTML across
tools is a non-goal: Zola, Hugo, and gazette use different Markdown renderers
and will disagree on whitespace and markup details. Within one tool, repeated
samples must produce byte-identical output, so wrong-but-fast runs fail
loudly.

**Tool-vs-tool** distinguishes this comparison model from the project's
translation model (RUE-1048), in which the same small program is rewritten in
each language. Both models are valid; this ADR sequences tool-vs-tool first
because it answers the headline question with production peers.

## Decision

### 1. Anchor the runtime suite with one realistic workload: gazette

The first runtime benchmark is **gazette**, a static site generator written in
Rue, living at `examples/gazette/` as a maintained example registered through
the ADR-0070 build actions. Gazette walks a content directory, parses TOML
front matter, renders a defined Markdown subset, expands the site's two
shortcodes, applies templates with inheritance, generates section index pages
and an RSS feed, and writes the output tree. It is an ordinary example
program — reviewed, tested, and idiomatic — not a benchmark-only artifact.

Its supporting libraries (front matter, Markdown, templating) are modules
inside the example. Promotion of any of them into `std` is a separate later
decision; nothing in this ADR presumes it.

### 2. Build the live corpus and record its identity per observation

The runtime workload input is the live rue-lang.dev content, assembled at
fixture-preparation time exactly as `website/build.sh` assembles it for the
real site (site content plus the copied spec — currently ~96 markdown files,
~780 KB, and growing). The corpus is deliberately **not** frozen. The
project's question is comparative runtime performance at order-of-magnitude
precision, and the cross-tool comparison is internally valid because every
tool builds the identical corpus within a run. A content change that moves
one tool disproportionately is signal about that tool's behavior on real
input — exactly what this system exists to surface — not noise to be pinned
away. Freezing would also fit this input badly: the site changes with every
blog post, so snapshots would either churn suite revisions constantly or
benchmark a progressively staler site.

Corpus drift is handled by measurement discipline instead of pinning:

- **Recorded identity.** Every observation records the corpus identity —
  content tree hash, file count, and total bytes — so any movement in a
  series is attributable to compiler, workload, or corpus from the data
  alone.
- **Annotated events.** Corpus changes appear as annotated events on the
  published charts, alongside compiler releases and peer tool bumps.
- **Peers as the drift control.** The peer tools are version-pinned, so a
  pinned peer's series moves only when the corpus moves. Corpus effects
  shift all three tools; Rue changes shift only gazette. The gazette-to-peer
  ratio is therefore the drift-immune longitudinal signal, published
  alongside the raw series.

Template ports for each tool — gazette templates, a Zola template/config set,
and a Hugo template/config set, each idiomatic to its tool — and the parity
configurations live in the repository and are versioned ordinarily.
Deterministic 10x and 100x scale variants, generated by path-prefixed
duplication of the current corpus, are derived at fixture-preparation time,
outside the measured window, so results show a scale curve rather than a
single point.

If a later decision attaches a regression ratchet or an absolute target to
the runtime series, freezing the corpus for that gated series becomes a
prerequisite at that point, exactly as ADR-0071 froze lattice: a contract
needs a fixed input, but a comparative observatory does not.

### 3. Define the v1 equivalence subset

Version 1 measures exactly this job, for all three tools:

**In scope:** content-tree discovery; TOML front matter; the Markdown subset
actually used by the corpus (headings, paragraphs, emphasis and strong,
inline code, fenced code blocks emitted as escaped `pre`/`code` without
highlighting, links, images, block quotes, lists, thematic breaks); the
`rule` and `preview_feature` shortcodes; template application with
inheritance, conditionals, and loops; section index generation with weight
ordering; RSS feed generation; writing the output tree.

**Out of scope for v1:** syntax highlighting, search-index generation, HTML
minification, Sass, Tailwind or any CSS building, live reload, image
processing, taxonomies, and pagination. Zola and Hugo are configured with
these features disabled; the parity configurations are versioned in the
repository alongside the template ports. The Tailwind step of the real site
build is outside the measured boundary for every tool.

Gazette's libraries reject constructs outside their documented subset with an
error rather than rendering them wrong, so new site content that steps
outside the subset fails the next scheduled run loudly instead of corrupting
output silently. This makes the benchmark a soft consumer of the website: a
content change can break a weekly run, and the remedy — extending gazette, or
amending the content — is intended dogfooding pressure rather than an
accident. Highlighting, search, and
minification are the intended first parity expansions (Future Work); each
widens the equivalence subset for all tools at once under a new suite
revision.

### 4. Validate work equivalence, not byte equality across tools

Every measured run is validated; a run with wrong output fails regardless of
speed:

- **Determinism within a tool:** all samples in a run must produce
  byte-identical output trees; the output hash is recorded with the
  observation.
- **Across tools:** the emitted file sets (relative paths) must be identical,
  and structural checks (page count, non-empty pages, feed presence) must
  pass, so no tool can win by skipping work.
- **Spot goldens:** a small set of stable pages carries committed expected
  output per tool, changed only deliberately, guarding against slow,
  consistent output corruption that determinism and structural checks cannot
  see.

Cross-tool HTML byte equality is explicitly a non-goal, per Terminology.

### 5. Measure with rue-bench under the existing evidence discipline

`crates/rue-bench` gains a runtime-measurement mode driven by a new
`performance/runtime.toml` manifest using ADR-0067's suite/epoch model:
suite revisions pin the workload contract (tool set, template-port and
parity-config identity, scale-variant policy, validation rules); corpus
identity is recorded per observation rather than pinned, per Decision 2;
epochs pin per-platform invocation, sampling policy, and environment. This
resolves RUE-1046's open CLI question:
runtime benchmarking is a `rue-bench` subcommand, not a separate tool.

Per sample, the harness records whole-process wall time (fixture preparation
excluded, spawn-to-exit measured, per ADR-0071's boundary discipline), peak
RSS, and binary size, with median and spread derived downstream; hardware
counters join where the pinned runners support them, as portable extensions
rather than v1 requirements. Every observation records the Rue compiler
commit, gazette source identity, corpus identity, and peer tool versions.
In-process
iteration timing for microbenchmarks is explicitly deferred until the
microbenchmark tier needs it; v1 is whole-process only.

Rue builds of gazette are release-quality (`-O3`), matching ADR-0071's
definition of the product. Runs execute on the same pinned GitHub Actions
regime as the compiler suites; noise mitigation inherits that regime's
calibration, and the order-of-magnitude goal means hosted-runner dispersion
is acceptable for the comparison claim.

### 6. Close standard-library gaps on the canonical path

The two prerequisite capabilities are built in `std`, not in the benchmark:

- **Directory enumeration** (RUE-1481): `read_dir` plus a deterministic
  recursive walk, built on `getdents64` through `@syscall`, pure Rue like the
  rest of ADR-0057's fs surface. Linux first; the known macOS syscall
  error-detection gap is tracked separately and does not block this project.
- **Substring operations** (RUE-1482): index-returning substring find,
  split, replace, join, and a lines helper, with byte-string semantics
  consistent with ADR-0035.

These land as ordinary reviewed std work with tests and documentation. The
benchmark must never depend on a private fork of std behavior.

### 7. Gazette is also a compiler-performance workload

Gazette joins the maintained scaling curve in `performance/scaling.toml`
alongside ruelex, mosaic, harbor, and lattice. Compile-time measurement of
gazette follows ADR-0067/0071 unchanged; this ADR adds the workload, not new
compile-time policy. One program thereby feeds both series: the compiler
suites answer "how fast does Rue compile a real text-processing program," and
the runtime suite answers "how fast does that program run."

### 8. Publish from the durable store

Runtime observations append to the `performance-data-v1` orphan branch as a
new record kind through the existing single-writer workflow. Nothing derived
is stored: medians, spreads, comparison ratios, and chart series are computed
at website build time by the existing derive step, and the raw data remains
public for reproducibility. The website gains a runtime page fed the same way
as the current performance dashboard: a current comparison table (gazette vs
Zola vs Hugo at each corpus scale), longitudinal time series for gazette
across compiler versions — both raw and as the gazette-to-peer ratio, per
Decision 2 — annotated events (compiler releases, peer tool bumps, corpus
changes, suite revisions), and side-by-side source and template excerpts. The
longitudinal Rue-only view ships as soon as the harness produces data and
does not wait for the cross-tool comparison.

### 9. Cadence and regression posture

The runtime suite starts scheduled weekly plus manual dispatch, matching the
scaling suite's pattern; the Rue-only series may move to per-merge once its
cost is known. Regressions are surfaced visibly on the dashboard (flagged
deltas against the previous suite-revision median), with maintainer triage
and no hard CI gate in v1: the project's stated goal is order-of-magnitude
placement and trustworthy trend lines, and a ratchet like ADR-0071's should
be introduced only after the runtime series' dispersion is calibrated. Peer
toolchain versions are bumped deliberately, recorded as annotated events, and
never silently.

## Implementation Phases

- [ ] **Phase 1: Standard-library prerequisites — directory enumeration and
  substring operations** - RUE-1481, RUE-1482
- [ ] **Phase 2: Gazette libraries — TOML front matter, Markdown subset,
  template engine** - RUE-1483
- [ ] **Phase 3: Gazette, live-corpus fixture preparation, output validation,
  and the scaling-curve rung** - RUE-1484
- [ ] **Phase 4: rue-bench runtime measurement mode and the runtime manifest**
  - RUE-1046
- [ ] **Phase 5: Cross-tool comparison — Hugo pin, parity configs, CI runs** -
  RUE-1485
- [ ] **Phase 6: Website publication — comparison table, time series,
  side-by-side source** - RUE-1049

Phases 1 and 2 can proceed in parallel once accepted; Phase 4 depends only on
Phase 3's workload existing in runnable, validated form and can overlap its
later stages.
RUE-1047 (corpus principles) is partially realized by Phase 3 — gazette
anchors its realistic tier — and otherwise remains open for the microbenchmark
tier. RUE-1048 (peer translations) is untouched by this ADR and stays in the
project as future scope.

## Consequences

### Positive

- One workload advances both performance questions: gazette is simultaneously
  the first runtime benchmark and a new compile-time scaling rung.
- The comparison is externally meaningful: results place Rue relative to
  production tools people already use, on a real corpus, not synthetic code.
- Validation is honest: determinism checks, file-set equality, and spot
  goldens make wrong-but-fast impossible to publish, and parity configs make
  the peers' job the same job.
- The std work is durable product improvement — directory enumeration and
  substring operations are needed by essentially every future Rue program.
- Infrastructure reuse: the suite inherits the measurement protocol, durable
  store, runner regime, and website pipeline that already exist, so the new
  surface area is the workload and one harness mode, not a second system.
- The corpus is self-renewing: the benchmark always measures the site as it
  actually exists, and a content change that moves one tool disproportionately
  surfaces as signal instead of being hidden behind a stale snapshot.

### Negative

- A subset comparison invites "unfair to Zola/Hugo" critique; disabled
  features and full configurations must be published prominently with the
  results, and the subset must widen over time to keep the claim meaningful.
- The raw longitudinal series is not directly comparable across corpus
  changes; readers must lean on the chart annotations and the peer-ratio
  series, and the publication layer must make that easy rather than optional.
- The live corpus makes the benchmark a soft consumer of website content: a
  page using constructs outside the equivalence subset breaks the next
  scheduled run until gazette or the content is extended.
- Template ports are maintenance surface: three template sets drift as the
  tools evolve.
- An SSG stresses strings, maps, allocation, and I/O but not numeric or
  bit-manipulation workloads; until the corpus grows more programs, the
  runtime series over-indexes on one workload shape.
- Weekly cadence means regressions can hide for days; acceptable for v1's
  goals, revisited once cost data exists.

### Neutral

- RUE-1048's translation tracks (agent-default and tuned, Rust and Zig) are
  neither implemented nor retired; they remain the project's second
  comparison model.
- Zola remains the tool that builds the real rue-lang.dev; nothing here
  changes the production website pipeline.
- The microbenchmark tier of RUE-1047 remains open and unscheduled.

## Open Questions

1. Once gazette can build the full corpus within the equivalence subset, does
   rue-lang.dev eventually dogfood it as its production generator? That is a
   separate decision with its own reliability bar, deliberately not made here.
2. Does the Rue-only runtime series move to per-merge once its CI cost is
   measured, or does weekly remain the steady state?
3. When the corpus gains more programs, do agent-default/tuned translation
   tracks (RUE-1048) also apply to the SSG scenario, or only to smaller
   benchmarks?
4. What regression-flagging threshold is appropriate once the runtime
   series' dispersion is calibrated on the pinned regime?

## Future Work

- **Parity expansions**, in intended order: syntax highlighting, search-index
  generation, HTML minification — each widening the equivalence subset for
  all tools under a new suite revision.
- Additional comparison programs beyond the SSG, growing the realistic tier.
- The microbenchmark tier and the RUE-1048 translation tracks.
- Incremental-rebuild benchmarking (change one page, rebuild) — the runtime
  counterpart of ADR-0068, and a scenario where Hugo and Zola both invest
  heavily.
- macOS runtime measurement, once the `@syscall` error-detection gap closes.
- A regression ratchet for the runtime series, modeled on ADR-0071, once
  dispersion is understood — this is the point at which a frozen corpus for
  the gated series becomes a prerequisite (Decision 2).

## References

- [ADR-0006: Zola unified website](0006-zola-unified-website.md) — the site
  this benchmark's corpus comes from.
- [ADR-0057: File I/O v0](0057-file-io-v0.md)
- [ADR-0067: Compiler performance measurement, epochs, and dashboard](0067-compiler-performance-measurement.md)
- [ADR-0068: Incremental edit-scenario performance measurement](0068-incremental-edit-performance-measurement.md)
- [ADR-0070: Rue program build actions](0070-rue-program-build-actions.md)
- [ADR-0071: Release-quality compiler performance contract](0071-release-quality-compiler-performance-contract.md)
- Linear: Runtime performance benchmarking project — RUE-1045 (this ADR),
  RUE-1046, RUE-1047, RUE-1048, RUE-1049, RUE-1481, RUE-1482, RUE-1483,
  RUE-1484, RUE-1485.
- Prior art: the Computer Language Benchmarks Game (comparison layout,
  tuned-track ethos); perf.rust-lang.org (longitudinal dashboard).
