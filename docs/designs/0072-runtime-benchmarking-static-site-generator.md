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
relates: ["RUE-487", "RUE-1045", "RUE-1046", "RUE-1047", "RUE-1049", "RUE-1481", "RUE-1482", "RUE-1483", "RUE-1484", "RUE-1485", "ADR-0006", "ADR-0057", "ADR-0067", "ADR-0068", "ADR-0070", "ADR-0071"]
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

Rue gains a runtime benchmarking system whose anchor workload is **gazette**,
a static site generator written in Rue that builds the live rue-lang.dev corpus
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
  ~96 markdown files and roughly 513 KiB of Markdown (525,479 bytes) once
  `website/build.sh` copies the specification in (25 content pages plus 71
  spec pages), 17 templates, and 1,225 shortcode invocations — 1,224 of them
  `rule`. Zola v0.21.0 is already pinned as a
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

The anchor runtime workload — and the first cross-tool benchmark — is
**gazette**, a static site generator written in
Rue, living at `examples/gazette/` as a maintained example registered through
the ADR-0070 build actions. Gazette walks a content directory, parses TOML
front matter, renders a defined Markdown subset, expands the site's two
shortcodes, applies templates with inheritance, generates section index pages
and an RSS feed, and writes the output tree. It is an ordinary example
program — reviewed, tested, and idiomatic — not a benchmark-only artifact.

Its supporting libraries (front matter, Markdown, templating) are modules
inside the example. Promotion of any of them into `std` is a separate later
decision; nothing in this ADR presumes it.

Gazette anchors the suite but is not its first measured program: the durable
runtime series begins in Phase 1 with a declared existing-example workload
(see Implementation Phases), so measurement infrastructure exists before the
workload that motivates it.

### 2. Build the live corpus and record its identity per observation

The runtime workload input is the live rue-lang.dev content, assembled at
fixture-preparation time exactly as `website/build.sh` assembles it for the
real site (site content plus the copied spec — currently ~96 markdown files,
~513 KiB of Markdown, and growing). The corpus is deliberately **not**
frozen. The
project's question is comparative runtime performance at order-of-magnitude
precision, and the cross-tool comparison is internally valid because every
tool builds the identical corpus within a run. A content change that moves
one tool disproportionately is signal about that tool's behavior on real
input — exactly what this system exists to surface — not noise to be pinned
away. Freezing would also fit this input badly: the site changes with every
blog post, so snapshots would either churn suite revisions constantly or
benchmark a progressively staler site.

Corpus drift is handled by measurement discipline instead of pinning:

- **Recorded identity.** Every observation records the complete
  fixture-input identity: a tree hash over everything the tools consume —
  the Markdown content tree, the static passthrough assets (currently 33
  files, ~1.2 MiB: more bytes than the Markdown itself), and the versioned
  template-port and parity-config revision — plus file count and total
  bytes. Any movement in a series is thereby attributable to compiler,
  workload, or input from the data alone, and no input class can change
  work without changing the recorded identity.
- **Annotated events.** Corpus changes appear as annotated events on the
  published charts, alongside compiler releases and peer tool bumps.
- **Peers as the corpus control.** The peer tools are version-pinned, so a
  pinned peer's series moves only with the corpus and with runner noise.
  Corpus effects shift all three tools; Rue changes shift only gazette. The
  gazette-to-peer ratio is therefore the corpus-normalized longitudinal
  signal, published alongside the raw series — corpus-normalized, not
  noise-free: the denominator carries hosted-runner dispersion like any
  other observation, which is why Decision 9's per-run peer canary exists
  and why ratio flags stay advisory until runtime-specific calibration
  lands (Decision 5).

Template ports for each tool — gazette templates, a Zola template/config set,
and a Hugo template/config set, each idiomatic to its tool — and the parity
configurations live in the repository and are versioned ordinarily.
Deterministic 10x and 100x scale variants, generated by path-prefixed
duplication of the current corpus, are derived at fixture-preparation time,
outside the measured window, so results show a scale curve rather than a
single point. Two honesty notes about that curve: duplication scales
per-page work but not site shape — internal `@/…` links still resolve to
the original pages, so cross-reference resolution stays constant — and the
published charts must label it as a page-count curve, not a site-size
curve. Duplicated sections also tie on weight, title, and date, so the
validation rules assert each tool's ordering tie-break is deterministic
rather than assuming it.

Recording instead of pinning the corpus is a deliberate amendment to
ADR-0067's identity discipline, not a reuse of it. ADR-0067 pins each
workload's own sources precisely because they are not the product, and
fails any run whose pinned components moved. The runtime suite introduces a
third input category the compile-time suites do not have — a **recorded
input**: one that is expected to move, whose identity is captured with
every observation instead of failing validation when it changes. The
runtime record schema names this category explicitly. Nothing about
ADR-0067's rules for the compile-time suites changes.

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
inheritance, conditionals, loops, and the filter and helper set the template
ports actually use (`safe`, `default`, `date`, `upper`, `lower`, `truncate`,
`striptags`, `pluralize`, `sort`, `slice`, `join`; URL and section lookups);
section index generation with weight ordering; RSS feed generation; writing
the output tree.

**Out of scope for v1:** syntax highlighting, search-index generation, HTML
minification, Sass, Tailwind or any CSS building, live reload, image
processing, taxonomies, and pagination. Zola and Hugo are configured with
these features disabled; the parity configurations are versioned in the
repository alongside the template ports. The Tailwind step of the real site
build is outside the measured boundary for every tool.

Two live-site features force explicit carve-outs rather than silent drops.
The site's blog listing uses Zola's `paginator`; pagination is outside the
subset, so the benchmark template ports omit paginated views even though the
production templates have them. The performance dashboard page is excluded
from the benchmark corpus entirely: it loads derived benchmark data at build
time (`load_data`), which would make the benchmark an input to its own
workload. These carve-outs are also why the benchmark uses template ports at
all rather than the production templates directly: each port is the
production template set minus the excluded pages and features, kept as close
to production as the subset allows.

Fairness constrains gazette's implementation, not only the peers'
configuration. The corpus is extremely skewed — 1,224 of its 1,225 shortcode
invocations are `rule` — and a gazette that special-cased that shortcode as
a hardcoded interpolation would beat Zola's general Tera rendering while
passing every output check. Gazette must render shortcodes, filters, and
templates through its general engine paths; corpus-specific fast paths are
out of bounds. Even so, a subset engine is structurally leaner than a
general-purpose one, so every published comparison carries the caption that
this is **a corpus-specialized Rue program against general-purpose tools**,
alongside the disabled-feature list.

Gazette's libraries reject constructs outside their documented subset with an
error rather than rendering them wrong, so new site content that steps
outside the subset fails the next collection run loudly instead of corrupting
output silently. This makes the benchmark a soft consumer of the website: a
content change can break the runtime leg of performance collection, and the
remedy — extending gazette, or amending the content — is intended dogfooding
pressure rather than an accident. Highlighting, search, and
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
- **Semantic oracle, on every page:** outside the timed window, a normalized
  extraction is computed for every emitted page — front-matter metadata,
  heading tree, visible text content, link targets, shortcode expansion
  results, section membership, and feed entry ordering — and compared across
  all three tools. This is the complete work-equivalence check the other
  layers cannot provide: a renderer that consistently drops body content
  from every un-goldened page passes determinism, file-set, and structural
  checks, and fails the oracle.
- **Spot goldens:** a small set of stable pages carries committed expected
  output per tool, changed only deliberately, covering the rendered
  markup-level form that the normalized oracle deliberately ignores.

The file-set criterion carries a documented allowlist for tool-mandated
differences — sitemap emission, feed filename mapping, static passthrough —
rather than an unstated assumption of exact equality. Static passthrough is
measured work for every tool, and its inputs are part of the recorded
fixture identity (Decision 2), so a static-asset change can never alter the
measured job without being visible in the data. The ports strip
build-time-varying output so the determinism check can hold byte-exactly;
Hugo's feed `lastBuildDate` and `generator` elements are the known cases.
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
RSS, and binary size, with median and spread derived downstream.
GitHub-hosted runners do not expose PMU counters, so hardware-counter
collection is gated on a future controlled-hardware epoch rather than
promised from the v1 regime; wall time and RSS are the portable core.

Thread policy is part of the epoch. Rue currently has no concurrency
support, while Zola parallelizes rendering via rayon and Hugo is
goroutine-parallel; on four-vCPU hosted runners the default configurations
would compare a single-threaded program against four-way-parallel peers,
and up to that factor of any published ratio would be thread count rather
than language or codegen quality. The primary published ratio therefore
pins the peers to one worker thread (`RAYON_NUM_THREADS=1`,
`GOMAXPROCS=1`) — the same reasoning as ADR-0071's one-worker primary
target: it exposes per-unit work rather than core count. The peers' default
parallel configuration is measured and published as a clearly labeled
secondary row, so the number users would actually experience is never
hidden, and the comparison is ready for the day Rue gains concurrency. Every observation records the Rue compiler
commit, gazette source identity, corpus identity, and peer tool versions.
In-process
iteration timing for microbenchmarks is explicitly deferred until the
microbenchmark tier needs it; v1 is whole-process only.

Rue builds of runtime workloads are release-quality (`-O3`), matching
ADR-0071's definition of the product. The v1 runtime platform matrix is
exact and deliberately narrow: `x86_64-linux` on the pinned `ubuntu-24.04`
regime. `aarch64-linux` joins once the Phase 2 std work is CI-verified
there; macOS stays deferred behind the known `@syscall` error-detection
gap. Calibration does not transfer from the compiler suites: dispersion is
a property of the workload, so each runtime workload is calibrated per
platform from its own repeated samples, and regression flags on a platform
are advisory until that calibration exists. The order-of-magnitude
comparison claim tolerates hosted-runner dispersion either way.

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
compile-time policy. Adding it is a suite-revision event on the scaling
manifest (currently revision 3), and the scaling workflow's time budget is
re-checked for the added workload — both under ADR-0067's ordinary rules.
One program thereby feeds both series: the compiler suites answer "how fast
does Rue compile a real text-processing program," and the runtime suite
answers "how fast does that program run."

The scaling curve's observations are the **only** recorded compile-time
measurements of gazette. The per-push runtime leg compiles gazette because
it must run it, so the compile cost is shared — but that compile's timing
enters no compile-time series: a one-sample compile from the runtime job
matches no declared epoch in `manifest.toml` or `scaling.toml`, and
ADR-0067's validation would rightly refuse to append it.

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

Runtime measurement rides the existing performance-capture and
website-publish pipeline rather than adding a new schedule:

- **Gazette is measured on every compiler change.** The per-trunk-push
  performance-collection workflow gains a runtime leg: compile gazette
  (release-quality; the compile is a cost the runtime harness pays anyway,
  and it is not recorded as a compile-time observation — see Decision 7),
  then run it against the current corpus. Runtime regressions therefore
  surface attached to the compiler change that caused them, and the website
  rebuild that already follows performance collection publishes them.
- **Peers are re-measured on events, not on a clock.** Pinned Zola and Hugo
  results move only when their inputs move, so the full peer leg runs only
  when the recorded fixture identity changes (content, static assets,
  template ports, parity configs), a peer toolchain is bumped, or the
  runner regime starts a new epoch. The per-push job detects an identity
  change at fixture-preparation time and runs the peer leg in that same
  run. Between events, the derive step joins gazette observations against
  the latest full peer observation with matching fixture identity.
- **A peer canary rides every run.** One single-threaded Zola build of the
  1x corpus runs alongside every gazette observation — cheap on this
  corpus, and enough to give every observation a same-run ratio
  denominator, so no segment's ratio ever leans on a single stale or noisy
  peer sample. The full peer matrix (Hugo, scale variants, the
  default-parallel secondary row) stays event-driven; the canary falls
  under the same cost safety valve as the scale variants.
- **Scale variants have a safety valve.** The 1x corpus runs per push; if
  the 10x/100x variants prove too expensive for per-push collection, they
  move to a scheduled cadence without changing any other policy.

The runtime series is its own record kind and sits deliberately outside
ADR-0067's required-CI gates — including the no-bypass stall gate that
fails every pull request when a headline series stops advancing. That gate
exists for series whose remedy is a small manifest change; a runtime series
stalled by site content outside gazette's subset has a remedy measured in
parser work, and must surface as a dashboard staleness flag and a
maintainer triage item, never a repository-wide block. This is the advisory
posture ADR-0068 already established for the incremental suite.

Regression flagging respects corpus discontinuities. With site content
changing every handful of trunk commits, a trailing window over the raw
per-push series would routinely span a corpus change, so raw medians are
compared only within corpus-identity-matched segments, never across one.
The cross-segment signal is the gazette-to-peer ratio, computed against the
same-run canary denominator so it never inherits the noise of a stale or
singleton peer sample; even so, cross-segment ratio flags are advisory
until the runtime-specific per-platform calibration of Decision 5 exists.
Flagged deltas get maintainer triage and no hard CI gate in
v1: the project's stated goal is order-of-magnitude placement and
trustworthy trend lines, and a ratchet like ADR-0071's should be introduced
only after the runtime series' dispersion is calibrated. Peer toolchain
versions are bumped deliberately, recorded as annotated events, and never
silently.

## Implementation Phases

- [x] **Phase 1: rue-bench runtime measurement mode and the runtime
  manifest, stood up on the declared wordfreq workload** - RUE-1046
- [ ] **Phase 2: Standard-library prerequisites — directory enumeration and
  substring operations** - RUE-1481, RUE-1482
- [ ] **Phase 3: Gazette libraries — TOML front matter, Markdown subset,
  template engine** - RUE-1483
- [ ] **Phase 4: Gazette, live-corpus fixture preparation, output validation,
  and the scaling-curve rung** - RUE-1484
- [ ] **Phase 5: Cross-tool comparison — Hugo pin, parity configs, CI runs** -
  RUE-1485
- [ ] **Phase 6: Website publication — comparison table, time series,
  side-by-side source** - RUE-1049

The harness comes first, against a precisely declared workload that already
exists. Phase 1's initial runtime workload is `wordfreq`, run over a large
deterministic text fixture — generated at fixture-preparation time by a
checked-in seeded generator, with the seed and generator revision pinned in
`performance/runtime.toml` and the generated identity recorded per
observation, sized on the order of tens of MiB so the run measures word
counting and map pressure rather than process startup — with fixed
arguments and a byte-exact golden output as its correctness oracle. That
series is permanent, not scaffolding: it remains in the suite as a
string/map-bound realistic workload after gazette lands, joining RUE-1047's
corpus. `jsonfmt` or `ruelex` may join the same way, each with its own
declared fixture and oracle. No new std work is needed, so the longitudinal
Rue-only series and the publication path produce data immediately, and
gazette later lands into working measurement infrastructure instead of
being a prerequisite for it. This restores RUE-1045's requested harness-first sequencing, which an
earlier draft of this ADR had inverted. Phases 2 and 3 can proceed in
parallel once accepted; Phase 6's longitudinal view can begin as soon as
Phase 1 produces data. RUE-1047 (corpus principles) is partially realized
by Phase 4 — gazette anchors its realistic tier — and otherwise remains
open for the microbenchmark tier. RUE-1048 (peer translations) is untouched
by this ADR and stays in the project as future scope.

## Consequences

### Positive

- One workload advances both performance questions: gazette is simultaneously
  the anchor runtime benchmark and a new compile-time scaling rung.
- Per-push measurement makes a runtime regression attributable to the
  specific compiler change that introduced it, at marginal cost on top of
  the compile the runtime harness performs anyway.
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
  page using constructs outside the equivalence subset breaks the runtime
  leg of performance collection until gazette or the content is extended.
- Template and configuration sets number four, not three: the three
  benchmark ports plus the production site templates the ports derive from.
  Production drift away from the ports quietly erodes the live framing, so
  website-template changes should include a port review.
- Published ratios compare a corpus-specialized Rue program against
  general-purpose tools. The required caption and the general-engine-path
  rule keep that visible and bounded, but the asymmetry is structural and
  cannot be validated away.
- An SSG stresses strings, maps, allocation, and I/O but not numeric or
  bit-manipulation workloads; until the corpus grows more programs, the
  runtime series over-indexes on one workload shape.
- Gazette and peer observations in a given ratio are usually taken at
  different times, since peers re-measure only on events; hosted-runner
  day-to-day variance is absorbed into the order-of-magnitude claim rather
  than controlled by same-run measurement.
- Per-push collection gains a runtime leg, so its wall-clock cost grows;
  the scale-variant safety valve bounds this, but the cost must be watched.

### Neutral

- RUE-1048's translation tracks (agent-default and tuned, Rust and Zig) are
  neither implemented nor retired; they remain the project's second
  comparison model.
- Zola remains the tool that builds the real rue-lang.dev; nothing here
  changes the production website pipeline.
- The microbenchmark tier of RUE-1047 remains open and unscheduled.

## Alternatives Considered

- **Frozen corpus snapshot.** An earlier draft froze the corpus under the
  lattice rules. Rejected in favor of the live corpus with recorded
  identity (Decision 2): a comparative observatory does not need a fixed
  input, the site changes far too often to freeze without revision churn or
  staleness, and the pinned peers already provide a corpus control. Freezing
  returns as a prerequisite if a gated target is ever attached.
- **Gazette-first sequencing.** An earlier draft built the workload before
  the harness, leaving the longitudinal series gated behind three phases of
  language and library work. Inverted: the harness stands up first against
  existing examples, per RUE-1045's original harness-first sequencing.
- **Translation-first comparison.** RUE-1048's model — the same benchmarks
  rewritten in Rust and Zig on agent-default and tuned tracks — answers the
  cross-language question with translated code rather than production
  tools. Deferred, not rejected; it remains the project's second comparison
  model.
- **A separate benchmark repository.** Rejected, answering RUE-1045's
  layout question in-tree: the corpus, the measurement machinery, the
  durable store, and the CI regime all live in this repository, and an
  external repo would reintroduce every synchronization problem the
  existing suites avoid.

## Open Questions

1. Once gazette can build the full corpus within the equivalence subset, does
   rue-lang.dev eventually dogfood it as its production generator? That is a
   separate decision with its own reliability bar, deliberately not made here.
2. Do the 10x/100x scale variants stay in per-push collection, or move to a
   scheduled cadence once their cost is measured?
3. When the corpus gains more programs, do agent-default/tuned translation
   tracks (RUE-1048) also apply to the SSG scenario, or only to smaller
   benchmarks?
4. What regression-flagging threshold is appropriate for
   corpus-identity-matched segments and the ratio series once the runtime
   series' dispersion is calibrated on the pinned regime?
5. The relationship to the Agent Outcomes Gauntlet (RUE-487) — shared
   prompts, shared corpus, or fully independent — is explicitly deferred
   rather than decided: it becomes concrete only when the corpus has
   multiple programs or the RUE-1048 translation tracks activate, and
   nothing in this ADR constrains the answer.

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
