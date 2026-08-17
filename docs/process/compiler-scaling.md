# Compiler scaling reports

The weekly compiler-scaling workflow measures the complete ADR-0071
release-quality boundary as maintained Rue programs grow from Ruelex through
Mosaic, Gazette, and Harbor to Lattice. Every workload is compiled at one, two,
four, eight, and automatic workers.

The ladder is ascending by source size, and the manifest's workload order is
the report order, so a rung belongs where its size puts it: Ruelex 2,222 lines,
Mosaic 6,108, Gazette 6,524, Harbor 9,000, Lattice 13,030. Gazette joined at
manifest revision 4 (ADR-0072 Decision 7); it is a static site generator, so it
is the rung whose shape is parsing and string work rather than a broad domain
model. Adding a workload is a suite-revision event, and revisions are not
continuous with each other. It complements the fast compiler-performance
headline; it does not contribute to that index.

## Measurement regime

Each sample launches the canonical compiler binary in a new process and asks
it for `--benchmark-json`. The manifest, external runner, and compiler must
independently agree that this is `fresh_source_to_native_v1`: Rue `-O3`, the
internal linker, one published native executable, the canonical rooted query
graph, and the declared worker row. There is no retained compiler session,
daemon, persistent query cache, precompiled package/program/standard artifact,
or undeclared external link input. Filesystem and operating-system page-cache
state are not reset, so the reports call this a **fresh-process compile**, not
a cold compile.

The compiler and measurement runner are built with the release target platform
(`-Copt-level=3 -Clto=thin`). The compiler reports that profile in its benchmark
metadata, and the scaling runner rejects a binary that does not match the
manifest's declared profile.

The runner executes three timing samples sequentially for every workload and
worker row. JSON keeps every raw observation in integer nanoseconds. The
Markdown view reports the median and median absolute deviation (MAD), along
with the machine and hosted runner fingerprint. The measured one-worker
samples are also the deterministic structural probes: their compiler-work
counters must agree exactly. Parallel rows retain every raw work record because
join, reuse, and validation outcomes can legitimately depend on scheduling. A
one-worker counter disagreement rejects the workload instead of averaging
structurally different compilations. Across all rows, different emitted bytes,
fixture shape, compiler target, or automatic-worker resolution reject the run.

Every process proof includes the compiler-binary digest; fresh state and output
directories; independently rehashed source inputs; the selected embedded
runtime; completed source-through-publication stages; successful exit; and an
independently verified native output digest. The report records:

- files, modules, source bytes, lines, lexer tokens, and functions considered;
- externally observed fresh-process wall time and peak resident memory;
- the compiler's additive, mutually exclusive phase accounting;
- semantic-provider name/import lookups, method/operator candidates, exact
  declaration fact-family reads, durable materializations, and
  anonymous/producer/toolchain facts;
- imported-type nominal registration requests, type-node and edge visits,
  complete/cycle cache hits, fresh named registrations, and anonymous
  registrations inside body-local identity domains;
- semantic-reachability scans, scheduled keys, and fixed frontier-width
  buckets;
- request-local CFG-materialization index builds, declaration/anonymous/type
  scans, exact body-local fact selections, and selected epoch-input
  cardinalities;
- logical CFG retained-charge walks over body-local symbol-table entries and
  UTF-8 bytes;
- query validation, endorsement, lease, demand, and retention-scan work;
- query claims, terminal reuses, joins, body completions, publication
  outcomes, cancellations, and cycles;
- display-only query identities materialized for memo nodes, structured batch
  cycle rendering, and abort fallbacks, with exact counts and formatted key
  bytes;
- query-worker active time and peak workers;
- dependency-ready item count, summed queue delay, and maximum queue delay;
- longest query/dependency ancestry;
- bounded request-wide declaration-graph, body-closure, and rooted-body-graph
  projection histograms, with nested declaration-nucleus, semantic-prerequisite,
  body-input lowering, provider analysis, semantic-analysis, CFG-construction,
  and CFG-optimization distributions;
- the inclusive reached-toolchain acquisition envelope (which contains the
  rooted semantic attempt), joins, declined joins, and permit donations;
- output binary size.

The Markdown work table includes validation nodes per token. That ratio is a
clock-independent amplification signal: source growth should not silently turn
into many repeated validation visits merely because elapsed time still looks
plausible on one host.

The worker-scaling table derives utilization from summed active query-worker
time divided by compiler-root time and resolved worker capacity. It reports
ready-item mean and maximum wait separately because the total is additive over
items rather than elapsed wall time. Body timing distributions use 64 bounded
log2 buckets, exact count/total/maximum, thread-local accumulation, and bounded
worker-completion merges; they never retain one event per body.

Semantic prerequisite and analysis durations are adjacent, non-overlapping
intervals inside each reached body transaction. The prerequisite interval owns
registered toolchain-demand, anonymous-nominal, and well-known-type queries;
the analysis interval owns body-input lowering, semantic-provider execution,
stable-key projection, and body-transaction publication.

After a semantic request selects its root inputs, the semantic-detail table
separates three adjacent, non-overlapping intervals: declaration-graph
collection, rooted body-closure collection, and rooted-body-graph projection.
Declaration occurrence-index and nucleus timings are nested inside the
declaration interval; body prerequisite and analysis timings are nested inside
the closure interval. Nested values explain their owner and are never added to
the enclosing duration.

The semantic-leaf table further attributes exact body-fragment lowering plus
provider-backed analysis inside body analysis. This separates the cost of
materializing a body's request-local input from the semantic engine that then
runs over it, without changing the canonical computation path. Since the
canonical-plan cutover that lowering interval contains no syntax preparation:
assembly/snapshot, lex/parse, and body-local RIR lowering are emitted as
literal zeros, and what it measures is packed decode, current-span projection,
temporary base-symbol reconstruction, and validation. Nested values explain
their owner rather than adding to it.

It attributed exact signature-fragment parsing as well until RUE-1510 projected
signatures from the canonical parsed declaration. There is no signature parse
left to time, so the column is gone rather than reporting a structural zero;
the boundary evidence keeps the field and now proves the deletion, refusing a
process that performs one.

The provider-detail table then separates host setup, canonical expression
analysis, specialization selection, durable body export, and final result
projection. Those intervals are measured inside the single provider-backed
body analyzer; they neither introduce nor select a peer semantic path.

The reached-toolchain value is an inclusive host-operation envelope, not an
exclusive phase. The operation runs the rooted semantic attempt to discover
whether a trusted module is absent, so its duration overlaps semantic work and
must never be added to the phase partition or interpreted as filesystem time.

The semantic-reachability table reports average scheduled keys per non-empty
dependency-ready logical frontier, fixed width buckets, and body transactions
consumed from bounded ready-frontier prefetch windows versus demanded on the
fallback coordinator path. Multi-permit runtimes execute each window as a
structured batch; a single-permit runtime executes it inline so query-proof
state stays in the coordinator task. These counters
distinguish dependency graphs that expose little parallel work from wide
frontiers whose work remains slow because of allocation, hashing, query-runtime,
or synchronization overhead. They describe deterministic graph shape and
scheduler submissions; they are not elapsed-time or worker-utilization
estimates.

The semantic-provider tables snapshot counters already incremented by the
production body-fact provider. The scaling probe performs no extra lookup or
materialization to collect them. Keeping lookup demand, exact fact-family
reads, and durable body-local materializations separate lets a follow-up tell
whether body analysis is asking for the same fact too often or merely paying
too much to represent each necessary answer. The materialization total is
exactly partitioned into constant, named-nominal, free-function, and method
events so representation changes can target the declaration kind which owns
the measured traffic. It is also exactly partitioned by provider-boundary
representation: shared payloads reuse canonical immutable storage, while owned
payloads rebuild an equivalent body-local transport value.
Named nominal payload reuses count repeated type-pool, endpoint, and export
consumers satisfied from the exact body transaction's first materialization.
Free-function payload reuses count the equivalent repeated callable imports.

The nominal-registration table measures the work after a durable type fact has
crossed that boundary. A request installs the named and anonymous identities its
type closure needs into one body-local identity universe. Named probes are
partitioned into complete-cache hits, in-progress cycle breaks, and fresh
registrations; type visits and traversed edges expose repeated closure walking
even when each individual provider fact is already shared.

The CFG-materialization table separates immutable lookup preparation from the
exact fact-closure selections that remain body-local. Its selections-per-build
ratio makes accidental per-body rebuilding of request-wide declaration,
anonymous-nominal, destructor, and slice-source indexes directly visible. The
selected-input table then counts the declaration, anonymous-nominal, callable,
nominal-metadata, module, builtin, and required-type roots copied into those
closures, exposing the mean size of the fresh semantic epoch each selection
constructs.

The CFG retained-charge table counts the body-local symbol-table entries and
UTF-8 bytes visited by memory-policy bookkeeping at publication. Keeping this
separate from materialization shows when accounting work repeats over an
unchanged artifact, even when host timing noise hides its cost.

The display-identity table similarly reports formatted key bytes per token.
Memo-node identities and abort fallbacks label diagnostics. Every count here
is a rendered name rather than a node: since
[ADR-0074](../designs/0074-structural-node-identity.md) a memo node's identity
is its family plus a content-derived 128-bit digest of the typed key, so the
two readers that used to need every node's text no longer do. The
retained-terminal charge is denominated in a fixed per-identity charge, and a
completed frame publishes its dependency array in canonical
`(family, stable_hash, incarnation)` order. A memo-node identity is therefore
formatted only when something asks what a node is called — a diagnostic, a
rendered cycle, an abort, or a debug dump — and a clean build formats
approximately none. The earlier measurement that rejected deferring the format
under the old text-ordered contract is in the
[cold-compiler architecture audit](../notes/post-adr-0063-cold-compiler-architecture-audit.md).
Structured-wait identities are materialized only when a detected wait cycle
must be rendered; registering an acyclic edge formats no key text. Typed query
keys, not display strings, remain authoritative for memo lookup. Family names
are shared and therefore excluded from the byte totals.

The compiled examples are never executed. Runtime tests and heavyweight
example scenarios remain in their existing suites, so a slow example runtime
cannot be mistaken for a compiler regression.

## Running locally

Build the compiler and the existing ADR-0067 runner, then run its scaling mode:

```bash
./buck2 build //crates/rue:rue //crates/rue-bench:rue-bench --target-platforms //platforms:release
RUE="$(scripts/rue-bin --target-platforms //platforms:release)"
BENCH="$(./buck2 build //crates/rue-bench:rue-bench --target-platforms //platforms:release --show-simple-output 2>/dev/null | tail -1)"
"$BENCH" scaling \
  --manifest performance/scaling.toml \
  --compiler "$RUE" \
  --commit <40-character-revision> \
  --repo-root . \
  --out /tmp/rue-scaling.json
```

This writes `/tmp/rue-scaling.json` and `/tmp/rue-scaling.md`. It performs 60
fresh compiler processes with the current four-workload, five-worker-row,
three-sample manifest. Supplying a
`--workdir` keeps the generated executables for inspection; without it the
runner uses a temporary directory.

## Comparing history

The scheduled workflow stores both report forms as a 90-day workflow artifact.
Compare rows only within the same target, worker setting, and manifest revision, and treat a
runner fingerprint change as advisory because GitHub-hosted hardware is not a
controlled machine.

The accepted ADR-0071 Phase 1 reference observation and interpretation live in
[`docs/notes/adr-0071-phase-1-reference-baseline.md`](../notes/adr-0071-phase-1-reference-baseline.md).
It records the first protocol-v2 hosted baseline, the fixed non-regression
ratchet, and the five-worker critical-path result that selects the next
semantic-to-CFG ownership audit.

Every workload row includes a shape id derived from its root path and the
compiler-produced file/module/function and source-size counts. A changed shape
id marks the timing comparison as advisory even if someone forgot to advance
the fixture revision. Deliberate changes to workload membership or intent must
increment `revision` in `performance/scaling.toml`; the old workflow artifact
remains the boundary record. Raw JSON is the authority, while the Markdown
table is a derived view that can be regenerated from those samples.

The suite is intentionally lower frequency. Lattice also participates in the
ADR-0071 absolute-latency series in `performance/manifest.toml`, but the other
maintained programs and the full worker matrix belong here; adding them to the
per-trunk headline would make collection expensive and silently change that
series' meaning.
