---
id: 0081
title: "Symbol-granular incremental linking contract"
status: proposal
tags: [architecture, compiler, incremental, linker, performance]
feature-flag: null
created: 2026-08-27
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0055", "ADR-0063", "ADR-0067", "ADR-0068", "RUE-1096", "RUE-1210", "RUE-1554", "RUE-1668"]
---

# ADR-0081: Symbol-granular incremental linking contract

## Status

Proposal, awaiting the RUE-1096 maintainer ruling. Nothing here is accepted.
This is the follow-up linker ADR that ADR-0063 §12 requires before any
stateful-linking implementation ("The incremental linker requires a follow-up
ADR before implementation"). It defines the contract — stable keys,
invalidation, determinism, fallback, persistence, and measurement — and
recommends a phase split. It deliberately does not commit to an implementation
schedule; implementation is RUE-1554, and the open questions below are the
decisions this ADR asks Steve to rule on rather than resolving silently.

## Summary

ADR-0063 made everything up to code generation a retained, demand-driven query
graph and stopped on purpose at a fresh whole-program link: "code generation
becomes a retained query terminal while linking remains fresh." That boundary
makes the final link the one term in the warm edit loop whose cost scales with
whole-program size regardless of query reuse quality — the incremental-latency
floor as programs grow.

This ADR fixes the contract for crossing that boundary. The stable key of a
linked fragment is the identity the `ProgramImagePlan` already carries: the
encoded stable callable identity per unit, plus the plan's context fields
(target, object format, entry point, runtime ABI identity, required runtime
symbols) and per-input content digests. Invalidation is the existing
`ProgramImagePlanDelta` taxonomy: exact added/changed/removed units and export
thunks, with any link-context change invalidating retained link state
wholesale. Determinism is a hard gate: incremental and fresh links of the same
plan must produce byte-identical executables on every supported target, in
every phase — which forces the corollary that output placement must be a
function of the current plan, never of the edit history. Every unsupported or
unmodeled situation falls back to the deterministic fresh link, and the
fallback is semantically invisible.

The recommended shape is two phases: Phase 1 is relink-with-reuse — activate
the retained plan delta, reuse link-derived facts the delta proves unchanged,
and land the byte-parity oracle and work counters — with true placement
retention (in-place image patching) as an explicit, separately gated Phase 2.

## Context

### What is fresh today, exactly

Every rooted executable request re-links the whole program.
`compile_rooted_with_session` (`crates/rue-compiler/src/queries.rs:333-362`)
takes the rooted codegen output — retained `CodegenUnit` terminals — and hands
it to `linking::link_internal_structured_units_with_warnings`, which
constructs a new `Linker`, admits every unit, and performs a complete link.

RUE-1668 already removed the encode/parse churn from that seam:
`Linker::add_structured_object` (`crates/rue-linker/src/linker.rs:1152`)
admits a compiler-owned `StructuredObject` whose section atoms remain shared
`Arc` byte containers (`crates/rue-linker/src/elf.rs:168-191`), so retained
objects are neither re-serialized nor re-parsed, and their bytes are copied
exactly once into the merged image. What remains fresh, per request, is the
whole-program remainder:

- global symbol table construction and weak/strong resolution
  (`add_link_object`, `linker.rs:1114-1146`);
- archive member selection — the first-eligible fixed point over the runtime
  archive and any user archives (`add_archive`, `linker.rs:1211`, RUE-848
  ordering semantics);
- user `--link-archive` files re-read from the filesystem on every link
  (`crates/rue-compiler/src/linking.rs:523-538`); the runtime archive's ABI
  validation is memoized per process (`linking.rs:461-479`);
- section classification and merge of every atom into the text/rodata/data/bss
  output buffers (`classify_section`, `linker.rs:56-97`);
- every relocation patched against its merged buffer (`PatchHome`,
  `linker.rs:26-33`), including instruction-rewriting relaxations; and
- ELF program-header or Mach-O load-command emission
  (`crates/rue-linker/src/elf.rs`, `macho.rs`).

### What it costs

Two measurements bracket the floor. On the cold end, the maintained Lattice
workload links roughly 1,280 single-function objects (~3.0 MB of object
bytes); its `linking` phase band is 20–50 ms, at or below the host noise floor
(`docs/notes/compiler-worker-scaling.md`, per-phase table). Cold linking is
not today's cold bottleneck.

On the warm end the picture inverts. The RUE-1033 acceptance ledger
(`docs/notes/rue-1033-acceptance-ledger.md`) records the warm single-function
edit baseline on the two-function fixture: 300 µs median edit-to-CodegenUnit
versus 1,618 µs median edit-to-runnable. The deliberately fresh image plan and
internal link are ~80 percent of the warm loop at minimal program size, and
they are the only remaining term that is O(program) rather than O(edit). Query
reuse cannot shrink it; program growth necessarily grows it. That asymmetry — small
absolute cost today, guaranteed dominance eventually — is what shapes the
phase recommendation below: define the contract now, take the cheap reuse and
the oracle now, and gate placement retention on measured evidence.

### What already exists for the seam

Phase 11 of ADR-0063 built the handoff and left it dormant:

- `ProgramImagePlan` (`crates/rue-compiler/src/program_image_plan.rs:74-84`)
  is the deterministic, diagnostics-free description of the link inputs:
  sorted units, sorted export thunks, target, object format, entry point,
  runtime ABI version and symbol, runtime archive identity, and the sorted
  required-runtime-symbol set.
- `ProgramImagePlanDelta` (`program_image_plan.rs:92-166`) computes exact
  added/changed/removed units and thunks between two validated plans, plus a
  `link_context_changed` bit; it is `#[allow(dead_code)]`, annotated as the
  Phase-11 handoff retained for this ADR (RUE-1096, RUE-1242 epic).
- Every object projection owns a lazily materialized, domain-separated SHA-256
  content digest (`ObjectProjection::content_digest`,
  `crates/rue-compiler/src/object_query.rs:141-150`, domain
  `rue.program-image.object\0v1\0`), computed only when plans are compared
  (RUE-1465), and the runtime archive has the analogous durable digest
  (`RuntimeArchiveIdentity::content_digest`, `program_image_plan.rs:679`).

### The determinism posture this ADR inherits

Rue already treats the executable's bytes as a contract surface:

- the `reproducible-programs` suite compiles the same logical project under
  relocated roots, perturbed mtimes, reversed manifest order, and different
  output basenames, and compares the complete unstripped native artifacts with
  `cmp` (`scripts/test-reproducible-output.sh`, RUE-616/RUE-624/RUE-1083,
  fixture under `reproducibility/`);
- warm/fresh parity tests assert `warm.elf == fresh.elf` after edits
  (`crates/rue-compiler/src/pipeline_tests.rs:262-340` among others); and
- worker-count independence is asserted through linked bytes
  (`platform_native_one_and_many_query_workers_produce_identical_linked_executables`,
  `pipeline_tests.rs:1249-1276`).

An incremental linker that could not meet byte equivalence would be the first
component allowed to make output depend on session history. This ADR proposes
not to allow that.

## Decision

The decision is a contract in six parts plus a phase recommendation. The
contract binds any implementation of RUE-1554; the phase split is the
recommended order.

### 1. Stable identity: the key of every linked fragment

The linked image is keyed by the `ProgramImagePlan`, refined to fragment
granularity. All identities are the request-independent stable identities
ADR-0063 §5 already requires; none embed request-local state, allocation
order, or source positions.

| Fragment | Logical key | Content identity |
| --- | --- | --- |
| Function unit (definitions, methods, concrete specializations) | `stable_function_identity`: `StableSymbolEncoder` over `StableCallableId::Function(FunctionInstanceKey)` (`program_image_plan.rs:611-615`) | durable SHA-256 `ObjectProjection` digest; in-memory `CodegenUnit::content_fingerprint` as red/green accelerator only |
| Synthesized drop glue | same encoder over `FunctionInstanceKey::DropGlue(TypeInstanceKey)` — glue is an ordinary unit, not a special case | same |
| Text/rodata atoms and unit-local string data | `(owning unit identity, section kind, atom index)`; ADR-0063 §11 normalizes function-local strings/constants to stable local atoms inside the unit (`CodegenSection.atoms`, `crates/rue-compiler/src/codegen_query.rs:291-301`) | covered by the owning unit's digest |
| Relocations | `NormalizedRelocation { offset, symbol, kind, addend }` within the owning unit (`codegen_query.rs:303-309`) | covered by the owning unit's digest |
| C-ABI export thunks | exported C symbol name | `bytes_digest("rue.program-image.export-thunk\0v1\0", …)` (`program_image_plan.rs:361`) |
| Entry point | plan constant: `_start` (ELF) / `__main` (Mach-O) | plan field |
| Target runtime | `RuntimeArchiveIdentity` — target names the embedded archive within one process; the domain-separated SHA-256 archive digest is the identity that survives a process boundary (`program_image_plan.rs:666-694`) | archive content digest |
| Runtime ABI | `runtime_abi_version` + `RUNTIME_ABI_VERSION_SYMBOL` per ADR-0055's typed manifest | plan fields |
| Runtime-requirement set | sorted `required_runtime_symbols`: entry point, ABI marker, and every unit relocation symbol classified by `rue_runtime_abi::classify_export` (`program_image_plan.rs:442-452`) | plan field (set equality) |
| Archive resolution | which members were extracted and the winning weak/strong bindings — first-eligible member order is output-affecting (RUE-848), so retained resolution state must key on the ordered archive identity list plus the required-symbol set | derived; recomputed or validated, never assumed |

Two rules govern the split between logical key and content identity, both
inherited from ADR-0063 §3:

- **Fingerprints describe deltas; they never prove reuse alone.** The 64-bit
  `content_fingerprint` participates in red/green equality only alongside full
  typed comparison (`codegen_unit_value_equal`, `codegen_query.rs:421-433`).
  Any identity that outlives the process — the eventual on-disk cache — uses
  the domain-separated SHA-256 digests exclusively.
- **Epochs version the machinery.** `backend_epoch`, `abi_layout_epoch`, and
  `object_writer_epoch` already key codegen and object projections. Retained
  link state adds a `linker_epoch` covering placement and emission policy. Any
  epoch change makes all retained link state ineligible — fresh link, no
  migration.

### 2. Change taxonomy and invalidation

`ProgramImagePlanDelta` is the invalidation vocabulary:

- **Added / removed units and thunks** — exact identity-keyed sets.
- **Changed units** — same identity, different content digest. Phase 1 does
  not subdivide "changed"; Phase 2 must refine it into placement classes:
  same-size in-place replaceable, resized within reserved capacity, resized
  beyond capacity, and alignment-changed (alignment is part of the unit's
  section content, so an alignment change is a digest change; the refinement
  only decides *placement* consequences).
- **`link_context_changed`** — target, object format, entry point, runtime
  ABI version or symbol, runtime archive identity, or required-runtime-symbol
  set (`program_image_plan.rs:158-164`). Any of these invalidates retained
  link state wholesale. These are exactly the inputs that select layout
  policy, archive selection, and the loader-visible container, so no per-unit
  mapping is meaningful.
- **User archives** — `--link-archive` inputs (ADR-0064 C FFI) live outside
  the plan and the query graph; the fresh path reads them from disk at link
  time. Under this contract, retained link state records the ordered archive
  path list and a content digest per archive; any difference is treated like
  `link_context_changed` in Phase 1 (wholesale fallback). Whether archives
  should instead become content-digested input leaves of the query graph is
  Open Question 3.

ABI and layout changes need no linker-specific rule: they reach the linker as
changed unit digests, because `abi_layout_epoch` and the ABI/layout facts are
already codegen inputs (ADR-0063 §11). The linker never re-derives semantic
facts; it trusts the delta.

### 3. Determinism: byte equivalence is required

**Fresh-versus-incremental byte equivalence is a hard, permanent gate.** For
every plan, on every supported target, in every phase of this project, the
incremental path must publish an executable byte-identical to what the
deterministic fresh link of that same plan produces. This is the same
discipline the repository already applies everywhere else: the warm/fresh
parity oracle already compares linked bytes, the reproducibility suite already
compares complete native artifacts, and ADR-0079 §2 made byte identity the
per-vertical gate for the semantic universe. An incremental linker is not the
place to introduce the first history-dependent output.

The load-bearing corollary: **output placement must be a pure function of the
current plan (and the linker epoch), never of the edit path that reached it.**
Phase 1 satisfies this trivially — it re-runs the deterministic layout every
time. Phase 2's placement retention must be designed so that the fresh linker,
given the same plan, computes the same layout the incremental path maintains
— for example, reserved capacity derived deterministically from unit content
(capacity classes), with a full deterministic relayout when a unit outgrows
its class. History-dependent slack, tombstones whose position depends on
deletion order, or compaction thresholds keyed to session age are rejected;
they would make `fresh(plan) != incremental(history ending at plan)` and gut
the oracle.

Testing extends the existing oracles rather than inventing new ones:

- the warm/fresh parity oracle gains an incremental-link leg: after each
  scripted edit sequence, incremental output bytes equal a fresh session's
  bytes for the final revision (the assertions at `pipeline_tests.rs:329`,
  `:980`, `:1096` already have the shape; they gain edit-sequence depth);
- one-worker/many-worker executable parity
  (`pipeline_tests.rs:1249-1276`) runs unchanged over the incremental path;
- the `reproducible-programs` suite stays byte-exact with no carve-outs; and
- exact add/change/remove plan tests, stable-address and growth cases,
  reverse-relocation patching, runtime archive changes, deterministic
  fallback, and atomic publication — the test list ADR-0063's acceptance
  section already requires for this project — are the functional matrix.

### 4. Fallback: bail to fresh, invisibly

The fresh internal link remains permanently reliable and is the answer to
every situation the incremental path does not model. Deterministic fallback
triggers, at minimum:

- `link_context_changed` (including runtime archive and
  required-runtime-symbol changes);
- user archive set or content change (Phase 1 policy; see Open Question 3);
- an unsupported or unknown relocation type
  (`RelocationType::Unknown`, and any relocation whose patch the incremental
  path cannot re-derive);
- relocation range overflow after placement — PC-relative i32 ranges,
  ±128 MiB `Jump26`/`Call26` branch ranges, ADRP ±4 GiB page ranges
  (`linker.rs:380-434`, `:502-673` overflow checks) — growth that breaks a
  range forces relayout, and relayout is a fresh link in Phase 1;
- Phase 2 only: a unit exceeding its reserved capacity class or fragmentation
  crossing the deterministic compaction threshold;
- retained link state failing validation (epoch mismatch, digest mismatch,
  corruption); and
- system-linker mode: `LinkerMode::System`
  (`crates/rue-compiler/src/linking.rs:672-776`) is an escape hatch that
  shells out to an external tool over temp files. It is never incremental and
  is not expected to meet warm-link latency targets, exactly as ADR-0063 §12
  states. A target used through the system linker always takes the fresh
  system path.

Fallback is **semantically invisible**: byte-identical output (it is the fresh
link), identical diagnostics and warnings, identical exit behavior. The only
legitimate observation channel is measurement — a deterministic work counter
(`link.fallback.<reason>`) and the ADR-0067 phase bands. A user can tell the
compiler fell back only by reading the performance report, never by diffing
the executable or the diagnostics.

Two mechanisms deserve explicit mention because they constrain Phase 2 and
motivate the fallback-first posture:

- **Patching is not slot-writing.** The internal linker's GOT relaxations
  rewrite instruction bytes conditional on opcode context — x86-64
  `mov`→`lea` and indirect-`call`/`jmp`→`addr32`-direct rewrites
  (`linker.rs:390-473`), and the Mach-O `GOT_LOAD_PAGEOFF12` `ldr`→`add`
  rewrite (`linker.rs:648-672`). In-place re-patching must reverse a prior
  relaxation before applying a new one, or must always re-copy the affected
  atom from its retained pristine bytes and re-patch. The contract requires
  the latter: patch sites are always re-derived from retained unit atoms,
  never edited in the output image in place, so a patch is idempotent and
  fresh-equivalent by construction.
- **Mach-O output is externally signed.** The driver runs `codesign`
  ad-hoc over the final bytes on macOS (`crates/rue/src/platform_signing.rs`;
  the linker leaves 256 bytes of padding for `LC_CODE_SIGNATURE`,
  `crates/rue-linker/src/macho.rs:839-841`, and the `build_dynamic` layout
  couples text file offset to header size, `macho.rs:896+`). Any incremental
  byte change therefore already requires a whole-image external re-sign, and
  Mach-O layout is more header-coupled than ELF. Phase 2 may legitimately
  land ELF-first with Mach-O on fallback (Open Question 4).

### 5. Persistence: in-memory session reuse first

Retained link state follows the same lifecycle as every other retained
terminal:

- **Phase 1/2 retain in-process only.** The previous `ProgramImagePlan` and
  any link-derived retained state (resolution tables, Phase 2 placement) are
  session-owned, charged through `RetainedCharge` against the RUE-1210
  runtime-wide budgets (8 GiB retained-byte / 4M observation soft budgets,
  ADR-0063 §14), and evictable under the same deterministic round-robin
  policy. Eviction of link state is never an error: the next link is fresh,
  not wrong. Link state is protected only while a rooted request is actively
  consuming it.
- **On-disk cache is a separate later phase** under ADR-0063 §14's
  persistence rules: only canonical owned forms are serialized — plans,
  digests, placement tables — never live linker handles or Rust memory
  layout. The cache key includes the compiler build identity, the schema and
  linker epochs, the target, and the runtime archive *content digest* (which
  is exactly why `RuntimeArchiveIdentity::content_digest` exists: within a
  process the target names the embedded archive; across processes only the
  bytes do). Toolchain upgrade therefore misses cleanly.
- **Corruption is a cache miss, never an error and never trusted.** Any parse
  failure, digest mismatch, truncation, or version skew discards the entry
  and falls back fresh. A corrupted cache must be unable to influence output
  bytes — the byte-equivalence gate plus digest validation of every consumed
  artifact enforces this fail-closed.
- **Bounded retention applies on disk as in memory**: an explicit size
  budget, deterministic eviction, and no unbounded growth across sessions.

### 6. Measurement and the RUE-1554 acceptance gate

ADR-0067's phase taxonomy already publishes `linking` and `object_generation`
as required wall-clock stages (`crates/rue-perf-schema/src/validate.rs:1955`),
and ADR-0068's retained-edit suite carries a `linking` `PhaseWork` field per
scenario (`crates/rue-perf-schema/src/incremental.rs:516-517`) with a Lattice
workload whose stated question is the fresh-link boundary
(`performance/incremental.toml`). The dashboard obligations are:

1. **Attribution stays exhaustive.** Incremental-link work lands in the
   `linking` band; the phase-sum invariant is untouched.
2. **Deterministic link work counters**, worker-count-independent like all
   ADR-0063 §13 metrics: units admitted, units reused in place (Phase 2),
   relocations patched, bytes copied into the image, fallbacks by reason.
   A no-edit warm build must show admission work proportional to the delta
   (zero changed units), not to program size, in whatever phase claims that
   property.
3. **The floor curve is part of acceptance.** Cold-link cost today is at the
   noise floor on Lattice (20–50 ms) while dominating the tiny-fixture warm
   loop (~1.3 ms of 1.6 ms); neither number characterizes the scaling that
   motivates this ADR. RUE-1554 Phase 1 must record fresh-link latency and
   deterministic link work versus unit count on the `scale_functions`
   workloads and the maintained programs, establishing the measured entry
   evidence for Phase 2.

The RUE-1554 acceptance gate, per its issue and this contract: stable
identities and fingerprints defined for every row of the §1 table (they are;
the table cites the code); a demonstrated no-edit warm build that reuses all
unchanged units; a demonstrated one-function edit that updates only the
invalidated image cone; fresh/incremental byte equivalence proven on all
supported targets through the extended parity oracle; and the fallback,
persistence, and measurement behavior above, each witnessed by a
deterministic counter or test rather than a claim.

### 7. Phase recommendation: relink-with-reuse first, placement retention second

**Recommendation: Phase 1 is relink-with-reuse. True in-place image patching
is an explicit Phase 2 with its own measured entry gate.**

Phase 1 regenerates the image from retained, fingerprinted inputs on every
request, but drives the regeneration from the activated plan delta:

- retain the previous plan; compute `ProgramImagePlanDelta` per request;
- reuse link-derived facts the delta proves unchanged — archive member
  selection and symbol resolution when the required-symbol set and archive
  identities are unchanged, skipping the per-link selection fixed point;
- keep the O(program) merge-and-patch (the memcpy floor and relocation walk),
  re-run deterministically as today; and
- land the parity oracle extension, the link work counters, and the floor
  curve.

Phase 2 retains placement — per-atom output addresses with reserved growth
capacity, reverse relocation indexes from symbol to patch sites, and atomic
republication of a patched image — under the §3 placement-purity rule and the
§4 pristine-atom re-patch rule.

The argument for this order:

- **Measured cost.** The fresh link is the *eventual* floor, not the current
  bottleneck: 20–50 ms cold on the largest maintained workload, ~1.3 ms on
  the warm fixture. Phase 1's reuse plus the existing RUE-1668 structured
  path capture the cheap constant-factor wins; nothing measured yet justifies
  the state Phase 2 carries. Phase 1's floor curve is precisely the evidence
  a Phase 2 go/no-go needs.
- **Complexity.** Phase 2 adds persistent mutable placement state, capacity
  policy, reverse indexes, atomic publication, and (on macOS) interaction
  with header-coupled layout and external re-signing — ADR-0063's own
  "Negative" consequences list warned that the incremental linker "adds
  persistent mutable placement state and must retain a reliable full-link
  fallback." Phase 1 needs none of it and still activates the entire
  contract surface: keys, delta, determinism oracle, fallback, counters.
- **Determinism risk.** The §3 placement-purity corollary is easy to state
  and nontrivial to design (capacity classes, deterministic compaction). It
  deserves its own reviewed design against Phase 1's oracle, not a bundled
  first landing. Phase 1 has zero placement-policy risk because it re-runs
  the deterministic layout.

The alternative order — placement retention first — is rejected below.

## Implementation Phases

Implementation is RUE-1554 (and successor issues under it); this ADR gates it
but sets no schedule.

- [ ] **Phase 1: Relink-with-reuse** — activate `ProgramImagePlanDelta`,
  retain the previous plan in-session under RUE-1210 budgets, reuse
  delta-proven link facts, land fallback plumbing with reason counters, the
  incremental parity-oracle leg, and the fresh-link floor curve on
  `scale_functions` and maintained workloads. — RUE-1554
- [ ] **Phase 2: Placement retention** — plan-pure placement with reserved
  capacity classes, pristine-atom re-patching via reverse relocation
  indexes, atomic image republication, deterministic compaction, ELF first
  with Mach-O explicitly gated. Entry requires Phase 1's measured curve to
  show the O(program) link term exceeding an agreed share of warm
  edit-to-runnable on a maintained workload (threshold: Open Question 2).
  — follow-up issue under RUE-1554
- [ ] **Phase 3: Cross-process persistence** — on-disk cache of plans and
  link state per §5, keyed by compiler build, epochs, and content digests,
  fail-closed on corruption, with an explicit disk budget. — follow-up issue

## Consequences

### Positive

- The warm edit loop's last whole-program term gets a defined, measurable
  path to O(edit), judged against a stated contract instead of an
  unspecified "someday" boundary (RUE-1096's motivation).
- Byte equivalence as a permanent gate keeps the repository's
  reproducibility posture intact — no output ever depends on session
  history — and every phase remains testable by extending oracles that
  already exist.
- Frontend, semantic, CFG, and codegen query identity are untouched; the
  entire design lives behind the `ProgramImagePlan` seam ADR-0063 §12
  preserved for it.
- Language-design questions about specialization, layout, and reachability
  can now be judged against a known eventual linking model.

### Negative

- The placement-purity rule constrains Phase 2 to layout policies a fresh
  link can reproduce, ruling out some classically cheap incremental-linker
  tricks (history-dependent slack, append-only tombstoning).
- Retained plans and link state add retained bytes under the shared budget;
  a session near budget may thrash between link-state eviction and fresh
  links (mitigated by fallback being correct, merely slower).
- Phase 1 keeps the O(program) copy-and-patch term; anyone expecting
  tens-of-milliseconds warm links on very large programs from Phase 1 alone
  will be disappointed — that is Phase 2's job, deliberately.
- The fallback matrix (archives, relocation ranges, Mach-O, corruption) is
  a real testing surface and must stay exercised, or the fresh path rots
  precisely when it is most needed.

## Alternatives considered

### Placement retention first (in-place patching as Phase 1)

Rejected as first phase. It front-loads the highest-risk state (placement,
capacity, compaction, atomic publication, macOS re-signing) before the parity
oracle, counters, and floor curve exist to validate it, and current
measurements do not show the O(program) term dominating any maintained
workload yet. Nothing in this ADR forecloses it as Phase 2; the contract is
written so Phase 2 changes no key, no invalidation rule, and no oracle.

### Execution-equivalent (not byte-identical) incremental output

Rejected. Allowing the incremental image to differ from the fresh image while
"behaving the same" would end byte-level reproducibility as a property of the
compiler's primary output, fork the `reproducible-programs` contract, and
replace an exact oracle with a behavioral one. The repository's existing
discipline (warm/fresh `.elf` equality, one-vs-many-worker equality,
ADR-0079's byte gates) all point the other way.

### Serialized object files as the incremental identity

Already rejected by ADR-0063 ("Use serialized object files as the incremental
linker identity"): object containers obscure stable atom identity and add
encode/parse work. RUE-1668's structured admission path confirmed the typed
route; object bytes remain a compatibility projection for system linking and
object presentation.

### Making the system-linker path incremental

Rejected. `LinkerMode::System` shells out over temp files to an external
tool; incremental behavior there would depend on foreign toolchain state.
ADR-0063 §12 already exempts it from warm-link latency targets. It remains
the fresh escape hatch, including for any target the internal linker cannot
yet serve.

### A linker-owned second dependency graph

Rejected on ADR-0063 §15 grounds (one compiler graph). The incremental linker
consumes `ProgramImagePlan`/`ProgramImagePlanDelta` values produced by the
canonical query graph; it does not observe frontend artifacts, maintain its
own reachability, or become a peer state machine deciding what is in the
program. Membership is reachability's job; the linker trusts the plan.

## Open Questions

These are the questions the RUE-1096 ruling should answer; recommendations
are stated where the analysis above supports one.

1. **Is byte equivalence permanently binding on Phase 2?** Recommended: yes,
   including the placement-purity corollary (§3). The alternative — a
   documented reproducibility carve-out for incrementally patched images —
   is coherent but forks the repository's output contract, and should be
   ruled out (or in) explicitly now, because Phase 2's placement design
   depends entirely on which regime holds.
2. **What is Phase 2's entry threshold?** Phase 1 produces the fresh-link
   floor curve; the ruling should fix the gate — e.g., the `linking` band
   exceeding a stated share of warm edit-to-runnable (or a stated absolute
   latency) on a maintained workload — so Phase 2 starts on evidence, not
   appetite.
3. **Do user archives join the query graph?** Phase 1 treats any
   `--link-archive` change as wholesale fallback. The eventual alternative is
   content-digested archive input leaves with ordinary invalidation, which
   would also let the accepted-read/observation machinery govern them. The
   ruling sets the direction; Phase 1 is correct either way.
4. **Mach-O scope for Phase 2.** Recommended: Phase 2 lands ELF-first, with
   Mach-O targets on deterministic fallback until the header-coupled layout
   and the external `codesign` re-sign step are separately designed. Confirm
   or reject ELF-first.
5. **Where does retained link state live?** Session-owned state behind the
   plan seam (recommended for Phase 1 — it is a cache of the previous
   request, exactly like the retained plan), versus a `rue-query` terminal
   family (attractive once placement is a pure function of the plan, since a
   plan-keyed layout is then an ordinary memoizable computation). The
   answer may legitimately differ between Phase 1 and Phase 2.

## Future Work

- Phase 2 placement-policy design note: capacity classes, deterministic
  compaction, reverse-relocation index representation — written against
  Phase 1's landed oracle, before Phase 2 code.
- On-disk cache codec and namespace policy (Phase 3), shared with ADR-0063
  §14's broader persistent-cache future work.
- Daemon/watch-mode integration: the incremental link is the natural terminal
  of a future watch loop; nothing here designs that loop.
- A global merged-string/rodata deduplication pass, if measurement ever
  justifies one, needs its own stable-key design; §1 deliberately keys string
  data unit-locally because that is what the codegen normalization produces.

## References

- [ADR-0055: Typed compiler-runtime ABI manifest](0055-typed-runtime-abi-manifest.md)
- [ADR-0063: Parallel demand-driven incremental compilation](0063-parallel-demand-driven-incremental-compilation.md) — §11 `CodegenUnit`, §12 the linking seam, §14 retention
- [ADR-0067: Compiler performance measurement](0067-compiler-performance-measurement.md) — `linking` phase attribution
- [ADR-0068: Incremental edit performance measurement](0068-incremental-edit-performance-measurement.md) — retained-edit suite and its fresh-link boundary question
- [RUE-1033 Phase 12 acceptance ledger](../notes/rue-1033-acceptance-ledger.md) — warm-edit baseline and the "Remaining linker delta" list
- [Post-ADR-0063 cold architecture audit](../notes/post-adr-0063-cold-compiler-architecture-audit.md) — fresh-link seam status, RUE-1459/RUE-1465 digest history, decision boundary naming this ADR
- [Compiler worker scaling note](../notes/compiler-worker-scaling.md) — per-phase linking band measurements
- Linear: RUE-1096 (this decision), RUE-1554 (implementation), RUE-1668
  (structured link admission), RUE-1210 (retention budgets), RUE-848
  (archive extraction order), RUE-1242 (epic)
