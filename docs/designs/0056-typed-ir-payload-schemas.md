---
id: 0056
title: Typed IR payload schemas
status: implemented
tags: [architecture, compiler, ir, performance, validation]
feature-flag: null
created: 2026-07-16
accepted: 2026-07-16
implemented: 2026-07-17
spec-sections: []
superseded-by:
relates: ["RUE-790", "RUE-839", "RUE-840", "RUE-841", "RUE-842", "RUE-843", "RUE-844"]
---

# ADR-0056: Typed IR payload schemas

## Status

Accepted under RUE-839 on 2026-07-16 after independent adversarial review. The
review required enforceable artifact-instance provenance, separate logical
family and physical-store markers, owning validation typestate with canonical
external contexts, and explicit M5/performance/guardrail gates. This is an
internal compiler architecture decision. It does not change Rue language
semantics, the specification, or a preview feature. The production M6
migrations were gated on RUE-838 and are complete as of 2026-07-17 under
RUE-844. RIR, AIR, and CFG now own typed payload schemas, validated
owner/editor boundaries, malformed-data and fuzz coverage, and source/API
inventories that prevent raw storage paths from returning.

The maintainer approved one measured performance-gate amendment on 2026-07-17
under RUE-843. An AIR owner-local incremental rebuild may regress by at most 4%
when its rebuilt action count is exactly unchanged and its peak RSS satisfies
the ordinary noisy-metric gate. All other performance gates remain unchanged.

## Summary

RIR, AIR, and CFG each own phase-local typed schemas for their variable-length
instruction payloads. Instruction and terminator fields carry family-specific
range types rather than interchangeable raw `u32` starts and lengths. Each
schema is the sole implementation of its record layout, checked decoder,
validation, and storage-owned builder; producers and consumers cannot repeat
positional arithmetic.

The initial implementation uses ordinary handwritten Rust in each owning IR
crate. It does not add a shared crate, erased word-store interface, declarative
schema language, derive, or procedural macro. The phases share a strict schema
contract, not a storage representation: RIR and part of AIR retain compact
`u32` words, while AIR projections and CFG payload families retain their
phase-specific element vectors. Reads are borrowing views or zero-allocation
iterators. Builders preflight and commit atomically, and checked validators
support malformed-data tests without `unsafe` or a second decoder.

## Context

Rue's compact side tables are valuable. Before this decision was implemented,
their type and layout contracts were spread between instruction fields,
producers, and consumers.

In the pre-migration RIR, `Rir` owned `extra: Vec<u32>`. `InstData` variants
stored raw `(start, len)` fields, and the module manually kept layout
constants such as `CALL_ARG_SIZE`, `PARAM_SIZE`, and the pattern sizes in sync
with `add_*` and `get_*` methods. `get_inst_refs`, `get_symbols`,
`get_call_args`, `get_params`, `get_match_arms`, `get_field_inits`,
`get_field_decls`, and `get_directives` allocate decoded `Vec`s. RUE-790
specifically requires those reads to become zero-allocation. Variable-width
path patterns and directives also perform positional arithmetic directly, so a
producer and a consumer can disagree about an offset or encoded count.

In the pre-migration AIR, `Air` owned `extra: Vec<u32>` and a separate
`Vec<AirProjection>`. Some reads, including `get_air_refs`, `get_call_args`,
and `get_match_arms`, already return iterators, and projection reads already
borrow a slice. Nevertheless, instruction data and `AirPlace` still expose raw
start/length pairs. Encoding, tag decoding, direct extra slicing, and range
arithmetic remain distributed between `AirPattern`, `Air`, semantic producers,
and downstream consumers.

In the pre-migration CFG, `Cfg` deliberately used several element stores:
`Vec<CfgValue>`, `Vec<CfgCallArg>`, `Vec<(i64, BlockId)>`, and
`Vec<Projection>`. Instructions, places, and terminators refer to them with raw
`u32` start/length pairs. The similar arithmetic hides the fact that the
element types and invariants are different. A common design must preserve this
phase-specific representation rather than forcing all CFG payloads through a
`u32` serialization layer.

Those raw APIs escaped their owning modules. The implemented architecture now
keeps construction and mutation in the owner, while semantic analysis,
optimizers, codegen, display, and artifact projection consume typed borrowing
views. Public machine-code generation requires a validated CFG; the read-only
`Sema::new(&Rir)` boundary cannot access payload positions or storage.

The failure modes are architectural, not merely ergonomic:

1. a width, tag, or variable-record offset can be updated in a writer but not
   every reader;
2. unchecked `usize`/`u32` arithmetic can truncate, overflow, or slice beyond
   the owning table;
3. one table's raw range can be accidentally used against another table;
4. malformed tags, booleans, IDs, or counts can panic far from their boundary
   or silently decode as a different operation; and
5. an apparently convenient decoded `Vec` makes hot traversal allocate and
   obscures the compact representation's actual cost.

## Decision

### Phase-local schemas, one common discipline

The owning crate defines a private or crate-visible `payload` module:

- `rue-rir` owns every RIR word encoding and RIR payload family;
- `rue-air` owns every AIR word encoding, projection family, and place payload
  family; and
- `rue-cfg` owns every CFG value, call-argument, switch-case, and projection
  family.

Schemas are handwritten ordinary Rust. The initial implementation adds no
shared crate, common erased store trait, declarative macro, derive, procedural
macro, build script, or generated Rust. The three crates form a dependency
chain and their element representations differ. A shared abstraction now would
either erase useful types or generalize over layout and validation policies
before the implementations demonstrate a genuinely identical primitive. A
small shared leaf primitive may be proposed later only if the completed phase
implementations prove the same code and semantics; it may not move a phase's
record declarations or validators out of their owner.

Each phase implements the following common contract:

1. every payload family has a distinct range type;
2. its storage is private and mutation occurs only through storage-owned
   builders;
3. one schema implementation owns width, tag, count, and offset arithmetic for
   both writes and reads;
4. checked decoding and validation are production-safe and contain no
   `unsafe`;
5. normal traversal borrows or decodes lazily without allocating; and
6. raw positions never leave the owning schema module as construction,
   traversal, serialization, or debugging APIs. Schema-owned diagnostics may
   format a typed range's position without returning its raw parts.

The common contract is an architectural invariant enforced by RUE-844's API
and source inventory. Similar spelling between phases is not itself an API.

### Typed ranges, indices, stores, and provenance

Conceptually, each payload module uses a zero-sized family marker and a private
generic core like this:

```rust
#[repr(C)]
struct PayloadRange<Family> {
    start: u32,
    extent: u32,
    family: PhantomData<fn() -> Family>,
}

#[repr(transparent)]
struct PayloadIndex<Family> {
    position: u32,
    family: PhantomData<fn() -> Family>,
}

struct PayloadStore<Store, Element> {
    elements: Vec<Element>,
    store: PhantomData<fn() -> Store>,
}
```

The `Family` marker names a logical schema. The separate `Store` marker names a
physical vector such as `RirWordStore`, `AirWordStore`, `CfgValueStore`, or
`CfgCallArgStore`.
Each family declares exactly one associated physical store, so several distinct
logical families can share one word or value vector without pretending that
the vector belongs to only one family.

The exact names may differ. Stored fields inside the owning artifact module use
family newtypes such as `RirCallArgs`, `AirMatchArms`, `CfgIntrinsicArgs`,
`CfgStructFields`, `CfgBranchArgs`, `CfgCallArgs`, `CfgSwitchCases`, and
`CfgProjections`, not a publicly generic `PayloadRange<F>`. Semantically
distinct roles receive distinct range families even when they share
`PayloadStore<CfgValueStore, CfgValue>`. The range types and their raw fields
are private to that module and are never returned as owned values. A
`CfgCallArgs` value therefore cannot be passed to an accessor for
`CfgStructFields`, and an AIR projection range cannot index AIR words.
Conversions between family ranges do not exist.

The marker is zero-sized. Every stored range remains exactly two `u32`s with
the same alignment as the current pair; implementations add compile-time size
and alignment assertions. `start` is a position in the family's physical
store. `extent` is the number of physical storage elements covered: words for
a word schema, `CfgCallArg`s for the CFG call store, and so on. The schema, not
the consumer, interprets that extent as records.

The type system proves physical-store and family provenance. It cannot prove
artifact-instance provenance: a two-word range carries no runtime owner stamp,
and a coincidentally in-bounds range copied from another artifact is
observationally identical. Validators therefore do not claim to detect erased
foreign provenance.

Instead, artifact-instance provenance is enforced by construction. Family
ranges do not implement public `Copy` or `Clone`. Payload-bearing instruction,
place, and terminator construction and replacement are owner-mediated, and
consumers cannot clone, remove, or insert a detached payload-bearing node.
Whole-owner cloning remains legal through an owning implementation that copies
the stores and nodes together. An editor may duplicate a range only inside the
same owner through a schema method. Import, selective clone/remap, transform,
and cross-artifact transfer decode a source view and build a new destination
range; they never copy a range-bearing node. RUE-840 through RUE-842 must close
the current public detached insertion and rewrite surfaces as part of each
phase migration rather than waiting for the final inventory.

Multiple logical families may retain one compact physical vector where the
element type and locality justify it, as with RIR word payloads and CFG value
lists. Only a family schema whose associated store matches can address that
vector. Conversely, CFG call arguments, switch cases, and projections remain
separate typed element stores. The discipline does not require word
serialization.

### Fixed-width and variable-width records

A fixed-width schema owns its physical width and its complete encode/decode
logic. Conceptually:

```rust
trait FixedRecordSchema {
    type Input;
    type View<'a> where Self: 'a;
    const WIDTH: u32;

    fn encode(input: &Self::Input, staging: &mut Vec<u32>)
        -> Result<(), PayloadBuildError>;
    fn decode<'a>(words: &'a [u32])
        -> Result<Self::View<'a>, PayloadError>;
}
```

The trait is illustrative, not a requirement to expose one generic trait.
Each implementation also has one ordinary Rust layout descriptor: named field
descriptors carry offsets and widths for a fixed record, while a tagged
variable record maps its tag to a plan containing the fixed prefix,
count-controlled segment, and trailing fields. The encoder and decoder consume
that same descriptor. They do not each maintain numeric offsets. The descriptor
is data or `const` values in the schema module, not generated code or a second
runtime format.

For a fixed-width range, the schema validates that `extent` is divisible by
`WIDTH`; its iterator length is `extent / WIDTH`. Call-argument modes, spans,
symbols, booleans, and IDs are decoded in that implementation. Consumers never
multiply a caller-provided count by a width or name a field offset.

Variable-width schemas own an envelope and record cursor. A nonempty envelope
contains its record count followed by the records; an empty range has zero
extent and no header. The range's extent bounds the entire envelope. The
schema's checked cursor decodes a record, computes its next position with
checked arithmetic, and proves that the final record ends exactly at the
range's end. For example, the RIR match-arm schema exclusively owns the tag,
span words, optional IDs, binding count, bindings, and body position. Neither
its builder nor a consumer repeats those offsets.

After a complete range scan proves the envelope count and terminal extent,
variable-record validated views retain that count, so their iterators implement
`ExactSizeIterator` even though physical widths differ. The fallible cursor
used to perform that scan does not. Fixed-record and direct-element validated
views implement `ExactSizeIterator` as well. A family whose format cannot
provide an exact remaining logical count must document that exception in its
schema and still provide a zero-allocation iterator; the initial RIR, AIR, and
CFG families are expected to provide exact validated views.

The current raw pair usually carries a start and logical record count. This
decision instead makes the second range word the bounded physical extent, then
stores the logical count in a nonempty variable envelope. The cost is exactly
one additional `u32`, or 4 bytes, per nonempty variable payload instance, not
per record. A three-word range would add 4 bytes to the instruction-side range
and abandon the two-word layout; deriving the count with a pre-scan would make
iterator construction O(records) and repeat decoding before traversal. The
envelope localizes the extra word in the side table while providing both an
exact physical bound and an O(1) `ExactSizeIterator` count. RUE-843 must measure
that tradeoff per family, including total physical side-table bytes as well as
RSS and timing; it is not hidden in the claim that stored range fields remain
two words.

Views borrow where the representation permits it. A direct typed-element store
returns `&[CfgCallArg]`, `&[(i64, BlockId)]`, or an equivalent typed slice. A
word encoding returns lightweight decoded values or views from an iterator.
`to_vec`, `collect`, or an explicit `into_owned` is allowed only where a caller
must outlive the artifact, reorder independently, or cross an ownership
boundary. There is no default owned getter.

### One owner for positional arithmetic

The schema declaration and implementation are the only production source of:

- fixed record widths;
- tag and discriminant values;
- field offsets and optional-value sentinels;
- variable-length counts, headers, and cursor advancement;
- conversion between logical records and physical extents; and
- the terminal condition for a range.

The storage-owned editor invokes that schema's encoder. A producer supplies
typed logical input to one atomic owner operation such as `add_call(args)` or
`replace_match_arms(inst, arms)`; that operation appends the payload and
installs or replaces the range-bearing node together. It never returns an
owned family range. Intra-owner duplication accepts an owner-local instruction
or entity reference and performs the copy internally.

Borrowing access and artifact validation invoke the schema's decoder. Consumers
request a semantic borrowing view from the owner by instruction, place, block,
or terminator reference; they never receive a range value. Tests may construct
malformed physical fixtures through a test-only corruption facility, but
production code cannot append raw words, extract ranges, or interpret raw
positions.

Adding a variable field therefore changes one schema implementation. It cannot
require a production writer and reader to update independent offset lists. A
schema may factor common field decoding into private helpers, but there is one
call graph for checked decoding, not a copied validation decoder.

### Checked construction and atomic mutation

All sizes and positions are bounded by the two-`u32` range representation.
Builders perform, in order:

1. checked conversion of logical counts and physical positions from `usize` to
   `u32`;
2. checked multiplication for fixed record widths;
3. checked addition for headers, variable fields, the new store length, and
   `start + extent`;
4. validation of every enum, boolean, raw ID, span component, optional
   sentinel, and referenced value representable at construction time; and
5. reservation or staging before mutating the owner.

No conversion uses `as u32` when truncation is possible. `saturating_add` is
not an overflow proof. The maximum physical store length, range start, range
extent, and logical count is `u32::MAX`, subject to the schema's width and
header. A fixed-width family therefore admits at most
`floor(u32::MAX / WIDTH)` records in a single range and may admit fewer when
the current store position leaves less capacity. Variable schemas derive and
expose the corresponding checked maximum; they do not guess it at call sites.

Every empty input returns the one canonical raw encoding `(start = 0,
extent = 0)` and appends nothing, regardless of the store's current length.
Every empty-range accessor yields an empty exact iterator or slice without
reading a header. Checked validation rejects every other zero-extent encoding.

Fallible or variable encoding uses private staging storage. A direct
typed-element builder either accepts a fully validated slice/exact-count input
or stages an arbitrary iterator completely; `size_hint` alone is not a
preflight proof. The builder validates every element and obtains capacity with
`try_reserve` before the commit. The commit phase performs only infallible
element moves except for a process abort. If any check, reservation, iterator,
or encode step returns an error, the physical table and all previously returned
ranges remain unchanged. An out-of-memory abort is outside Rust's recoverable
allocation model, but a reported builder error never leaves a partial append.

Builder and decoder failures are distinct types rather than one catch-all:

- `ResourceLimitExceeded` means a documented source-driven IR size limit was
  exceeded and becomes an ordinary user-facing resource-limit diagnostic;
- `CapacityFailure` records a recoverable allocation/reservation failure and
  follows the compiler's resource-exhaustion path;
- `InvalidBuilderInput` is a same-process producer invariant failure and is
  never blamed on source text; and
- `PayloadCorruption` is malformed reconstructed or stored data handled by the
  artifact policy below.

The exact enum spelling may differ, but these categories cannot be collapsed by
a broad conversion at the phase boundary.

### Checked decoding and malformed internal data

Malformed side-table data is not a source-language error. It indicates a
compiler invariant failure, a producer/consumer version mismatch, or a corrupt
reconstructed artifact. Checked APIs return a structured `PayloadError` with
the phase, family, range, record position, and reason. Artifact/import
boundaries attach the owning instruction or terminator when available.

Checked decoding rejects at least:

- a start, extent, multiplication, addition, or slice end outside its store;
- a fixed range whose extent is not a multiple of its schema width;
- a missing or inconsistent variable-record count or trailing words;
- an unknown enum/tag/discriminant, including reserved values;
- any boolean encoding other than `0` or `1`;
- an invalid raw symbol/ID conversion, forbidden sentinel, malformed span, or
  out-of-range instruction/value/block reference when contextual artifact
  validation has the corresponding authority.

There is no silent enum fallback, `unwrap`-based raw-ID conversion, unchecked
slicing, wrong-family alias, or undefined behavior. Wrong-family/store use is
a compile-time property covered by compile-fail/API-surface tests, not a
runtime corruption category. Artifact-instance provenance is enforced by the
opaque owner-mediated APIs described above and cannot be reconstructed from
raw bits. Malformed-fixture and fuzz tests use a narrow `cfg(test)` or
fuzz-support constructor that accepts raw storage and a selected family range,
then call production checked validation. They do not use pointer fabrication,
transmute, out-of-bounds writes, or another test-only decoder.

A top-level compiler path converts a `PayloadError` from a reconstructed or
cache artifact into the existing artifact rejection/recomputation behavior.
If data built in the same compiler process fails validation, the compiler
reports a deterministic internal invariant failure with context. It must not
blame the user's Rue program for malformed compiler-owned data.

### Builder, validator, and consumer lifecycle

Builders establish local schema validity at construction: representable
lengths, complete records, valid tags and scalars, and ranges into the correct
store. They cannot necessarily establish graph facts that refer forward to
instructions, blocks, or types not yet complete.

The design uses owning typestate, not a detachable validation token. Mutable
construction and rewrite types are `RirEditor`, `AirEditor`, and `CfgEditor`
(exact spelling may differ). `finish(context)` consumes an editor, validates
the complete artifact, and returns `Result<ValidatedRir, _>`,
`Result<ValidatedAir, _>`, or `Result<ValidatedCfg, _>`. A validated owner is
immutable. Any later optimization or transform that needs mutation consumes it
with `into_editor()`, performs owner-mediated rewrites, and must call `finish`
again. There is no free token that can outlive a mutation and no mutation API
on a validated owner.

Validation has two layers with explicit external authority:

- schema-local validation checks physical ranges, envelopes, tags, scalar
  encodings, and record termination using only the owner and family schema;
- contextual graph validation receives phase-specific borrowed context. RIR
  context supplies the canonical interner and source/file metadata needed for
  symbols and spans. AIR context supplies its canonical interner/source
  metadata and authoritative M5 type pool. CFG context supplies the frozen
  type pool and any other canonical phase metadata already required by CFG
  verification.

Payload schemas do not invent a second symbol, span, or type-validity model.
They call the owning interner/source/type APIs; in particular, live `Type`
validation uses ADR-0024's canonical encoding and owner-provided pool checks.

Every publication boundary runs both layers. This includes semantic imports,
durable artifact reconstruction, transforms that rebuild or rewrite payloads,
CFG rewrites, and artifacts returned by the canonical `CompilerSession` query
graph. `CompilerSession` stores and publishes `Arc<ValidatedRir>`,
`Arc<ValidatedAir>`, and `Arc<ValidatedCfg>` or enclosing artifacts that own
exactly those validated values. Clone/remap operations preserve ranges only
through a whole-owner clone; selective transfer rebuilds them through
destination editors.

Unvalidated data exposes a fallible cursor only. For a variable envelope,
`validate_range` scans to the declared physical end, validates the count and
terminal condition, and returns a `ValidatedView`. Only
`ValidatedView::iter()` implements `ExactSizeIterator`; a corrupt header can
therefore never make a fallible cursor claim an untrue remaining length. Normal
downstream consumers receive validated owners and use infallible convenience
accessors. Iterator steps call the same checked record decoder and convert an
impossible `Err` into a contextual invariant panic; they are not a second
unchecked or trusted decoder.

Validation is once per artifact publication or mutation boundary, not a full
artifact scan on every traversal. Individual iterator steps still use the one
safe schema decoder and ordinary bounds-checked Rust access. If profiling shows
those local checks matter, the implementation may cache safe derived view
metadata inside the validated owner, but it may not introduce `unsafe`, a peer
decoder, or mutable validation state.

### Ownership boundaries

| Concern | Authority |
| --- | --- |
| RIR word layouts, RIR family ranges, RIR payload validation | `rue-rir` phase-local payload schemas |
| AIR word/projection layouts, AIR family ranges, AIR payload validation | `rue-air` phase-local payload schemas |
| CFG value/call/case/projection stores, CFG family ranges, CFG validation | `rue-cfg` phase-local payload schemas |
| Physical vector mutation and range creation | The owning artifact's storage-owned builders |
| Positional arithmetic, tags, widths, counts, encode/decode | Exactly one family schema implementation |
| Cross-artifact or cross-phase transfer | Source borrowing view plus destination builder |
| Symbol, span, type, and graph-reference validation | The owning phase validator plus its canonical interner/source/type context |
| Canonical orchestration and artifact publication | `rue-compiler::CompilerSession` |
| Debug/error formatting | Schema-owned formatting that does not expose raw accessors |

No consumer reproduces another row for convenience, performance, migration,
or presentation.

## Required invariants

The implementation is complete only while all of these hold:

1. Every side-table field in a production instruction, place, or terminator is
   a family-specific typed range or index, never a public raw start/length pair.
2. A range occupies two `u32`s unless a later ADR supplies measured evidence
   and explicitly changes the compact representation.
3. A family range never leaves its owning artifact module as an owned value and
   can only be consumed by its associated physical store and schema.
   Artifact-instance provenance is preserved by atomic owner operations and
   semantic borrowing views, not inferred later from range bits.
4. The physical stores and raw range constructors are private to their owning
   artifact/schema modules.
5. One schema implementation owns every width, tag, field offset, count,
   extent calculation, encoder, checked decoder, and terminal condition.
6. Builders use checked `usize`/`u32`, multiplication, and addition; a reported
   failure performs no partial append.
7. Empty ranges use only `(0, 0)`, append nothing, and traverse without reading
   storage; every other zero-extent encoding is malformed.
8. Checked validation rejects every malformed tag, scalar, ID, width, count,
   extent, reference, and trailing record without panic or undefined behavior.
9. The infallible consumer adapter invokes the checked decoder; there is no
   peer trusted decoder or unchecked fast path.
10. Routine traversal allocates zero heap objects per read. Owned conversion is
    explicit at a genuine ownership boundary.
11. Only a fully validated view implements `ExactSizeIterator`; fallible raw
    cursors do not. All initial migrated families produce exact validated
    views.
12. RIR may retain compact words, AIR may combine words with typed projection
    storage, and CFG may retain phase-specific element vectors. Common schema
    rules do not erase those representations.
13. Editors are the only mutable artifact state. `finish(context)` consumes an
    editor and returns an immutable validated owner after schema-local and
    contextual validation; `CompilerSession` publishes only validated owners.
14. Imports, transforms, rewrites, and canonical artifact publication use the
    same typed schema validators as malformed-data tests.
15. Successful artifacts contain no placeholder/unvalidated range. Editors
    never return ranges; detached payload-bearing nodes cannot be cloned or
    inserted into another owner; selective transfer always rebuilds the
    payload.

## Implementation sequence

The M6 work proceeds through these issue-owned boundaries:

1. **RUE-839 — accept the schema contract.** Adversarially review this proposal,
   settle the phase-local API and performance gates, accept the ADR, and record
   the dependency graph. This issue does not migrate production IR.
2. **Finish M5 before production migration.** RUE-839 may be accepted while M5
   is active, but RUE-790 and RUE-840 through RUE-842 wait for RUE-838. That
   gate lets RUE-836 stabilize AIR/CFG/codegen type-bearing APIs and RUE-838
   remove compatibility surfaces before M6 edits the same consumers. Linear
   records RUE-838 as a blocker of the first independently runnable M6
   migration issues.
3. **RUE-790 — make existing RIR reads borrowing.** Replace the allocating RIR
   getters with iterator/view forms and remove consumer assumptions that a read
   returns an owned `Vec`. This establishes the zero-allocation consumer shape
   before RIR's schema surface changes.
4. **RUE-840, RUE-841, and RUE-842 — migrate the owning phases.** RUE-840 moves
   CFG to typed ranges over its phase-specific stores. RUE-841 moves RIR to the
   accepted typed word schemas after RUE-790. RUE-842 moves AIR word and
   projection payloads. After the M5 gate, RUE-840 and RUE-842 may proceed in
   parallel with RUE-790; RUE-841 is blocked by RUE-790. Each issue updates all
   of its phase's producers and consumers rather than preserving public raw
   compatibility constructors.
5. **RUE-843 — verify schemas and performance.** After RUE-840, RUE-841, and
   RUE-842, add property, corruption, malformed-fixture, cross-family,
   boundary, iterator-allocation, layout, and performance coverage. Artifact
   validators and fuzz targets must use production decoders.
6. **RUE-844 — remove escape hatches and enforce the architecture.** After
   RUE-843, inventory production APIs and sources, delete remaining public raw
   constructors and start/length fields, direct side-table indexing, raw enum
   casts, allocating default getters, and mutable raw side-table access. Add a
   guard that fails when those surfaces return. Update this ADR and its index
   to implemented only after the inventory and required suites pass.

RUE-840, RUE-841, and RUE-842 own all necessary consumer edits in `rue-air`,
`rue-cfg`, `rue-codegen`, and `rue-compiler`. A temporary adapter may exist
inside the owning payload module during one issue's migration, but it is not
public, cannot accept arbitrary raw ranges, and is removed before that issue
lands. RUE-844 is cleanup and enforcement, not permission to leave dual public
paths after a phase migration.

RUE-844 installs a layered guard suite rather than claiming a grep proves the
whole architecture:

- compile-fail/API tests prove family/store mismatch, raw construction,
  extraction or movement of a payload range between two editors, and detached
  payload-bearing node insertion are unavailable to consumers;
- a tightly scoped, reviewed source inventory rejects raw start/length fields,
  enum casts, side-table indexing, and mutable storage outside the three
  allowlisted payload modules;
- public-API inventory rejects raw range/store accessors and compatibility
  constructors;
- property, layout, allocation, and malformed-data tests prove the behavioral
  invariants; and
- schema-owned `Debug`/error formatting is tested as output, never as a raw
  accessor.

The source inventory cannot prove that one descriptor feeds encoder and
decoder; focused schema review and property/corruption tests establish that
fact. Fuzz-only raw fixture construction is separately named, feature-gated,
and absent from production artifacts.

## Performance evidence and gates

This proposal supplies structural evidence, not invented runtime measurements:

- the typed range uses the same two `u32` fields as current instruction data;
- marker types add no stored bytes;
- direct CFG stores retain their current element types;
- borrowing slices and lazy decoded iterators require no per-read collection;
  and
- schemas centralize checks without requiring serialization of rich phase
  elements; however, each nonempty variable-width word envelope adds one
  explicit count word, which must be included in the measurements.

RUE-840 through RUE-843 must turn that argument into measured evidence. The
benchmark record includes the exact baseline and candidate revisions, host and
OS, target, Rust/Buck2 versions, build profile, allocator-counting method,
workload hashes or generators, raw samples, medians, and median absolute
deviations. Baseline and candidate runs alternate on the same otherwise-idle
host after one warmup. Use at least seven measured samples for compiler
workloads and at least five for compiler build timings.

The reproducible matrix is:

| Workload | Property | Required measurements |
| --- | --- | --- |
| Many small functions/calls | Dense fixed-width call/parameter traversal | Compiler wall time, peak RSS, allocation count, phase timing, per-family logical/capacity bytes, total side-table bytes, peak staging bytes |
| Match-heavy functions, including variable path bindings | Variable-width tag/count/cursor traversal | Compiler wall time, peak RSS, allocation count, phase timing, nonempty-envelope count, per-family logical/capacity bytes, total side-table bytes, peak staging bytes |
| Generic/comptime specialization workload | Repeated AIR/CFG construction and traversal | Compiler wall time, peak RSS, allocation count, phase timing, per-family logical/capacity bytes, total side-table bytes, peak staging bytes |
| Focused iterator/builder microbench for every migrated family | Hot read and atomic-build behavior independent of parsing/codegen | Elements/second, allocations per complete traversal/build, logical and capacity bytes, and peak staging bytes for fixed logical inputs |
| Compiler clean build | Added crate/macro/code-generation and type-checking cost | Wall time and peak RSS for `./buck2 build //crates/rue:rue` from an equivalent clean state |
| Compiler incremental rebuild after a deterministic edit in each owner crate | Locality and downstream rebuild cost | Wall time and rebuilt action count for RIR, AIR, and CFG edits |

The first three workloads used the now-retired external data-collection
infrastructure together with the compiler's preserved `--benchmark-json`
instrumentation. Deterministic generators added for this work were checked in
or recorded verbatim. Peak RSS used the platform's standard process accounting.
Allocation counts used a benchmark-only counting allocator
around the compiler phase or focused traversal, with counting disabled outside
the measured interval. The benchmark must account for iterator construction
and full consumption, not merely creation.

Structural gates are exact:

- `size_of` and `align_of` assertions prove every typed stored range equals
  the replaced two-`u32` pair;
- each focused read traversal performs zero heap allocations;
- no default accessor returns an owned collection; and
- CFG element stores do not become word-encoded or boxed;
- fixed-width and direct-element families add no physical payload bytes for an
  identical logical input; and
- a variable-width family's physical-byte increase is at most 4 bytes times
  its number of nonempty payload instances, exactly accounting for the count
  envelopes. Any other increase blocks the issue or requires an ADR amendment.

Every memory record distinguishes logical bytes (`len * size_of::<Element>()`)
from allocated-capacity bytes and transient staging bytes. Atomic construction
must not hide retained over-allocation or a peak scratch copy behind unchanged
logical lengths.

For noisy metrics, a candidate is non-regressing when its median wall time,
peak RSS, and compiler clean/incremental build time do not exceed the baseline
by more than the larger of 2% of the baseline median or three times the larger
of the baseline and candidate MADs. A threshold crossing is rerun once as a
complete alternating series; a second crossing blocks the issue or requires
explicit measured justification in an ADR amendment.
As the sole amended exception, an AIR owner-local incremental rebuild may use
a 4% wall-time limit when its rebuilt action count is exactly unchanged and
its peak RSS passes the preceding ordinary gate. RUE-843 measured 5.99 seconds
to 6.22 seconds (+3.84%) with 12 actions on both revisions and effectively
unchanged RSS. This exception does not apply to clean builds, RIR or CFG
incremental builds, runtime workloads, semantic-query timings, allocation
counts, or any correctness requirement.
Whole-workload allocation counts may decrease or remain equal and must not
increase; focused payload traversals must remain exactly zero. Correctness is
never traded for passing a performance threshold.

## Alternatives considered

### One shared erased word-store abstraction

Rejected. It would fit RIR but force `AirProjection`, `CfgCallArg`, switch
cases, and CFG projections into words or erase their element types behind a
generic serialization interface. It would hide conversion, validation, and
layout costs that this decision requires each phase to expose.

### A dependency-light shared schema crate

Rejected for the initial migration. The range wrapper is small, while schema
ownership, IDs, and element types are phase-local. A new leaf crate would add a
build and API boundary without centralizing the actual layouts. It can be
reconsidered only from demonstrated identical implementations, and it may not
become the owner of phase records.

### Per-phase ad hoc helper methods

Rejected. Renaming `add_*`/`get_*` while retaining raw fields, raw table access,
and independent writer/reader arithmetic leaves the current failure mode in
place. The shared discipline, typed family provenance, validator lifecycle,
and removal endpoint are the decision.

### Owned decoded `Vec`s

Rejected as the default read representation. They simplify some callers but
make every traversal allocate and copy, conceal lifetime boundaries, and fail
RUE-790. Callers that truly require ownership opt in explicitly.

### Procedural macros, derives, or schema generation

Rejected initially. They could reduce repeated declarations, but they also
hide width, allocation, compile-time, and diagnostic behavior behind generated
code. The present number of families is reviewable as ordinary Rust. A later
proposal needs measured compile-time benefit or a demonstrated class of drift
that handwritten single-owner schemas cannot prevent.

### Validate the entire payload on every traversal

Rejected. Publication/mutation boundaries validate graph-wide state once.
Traversal continues to use the same safe checked element decoder, but does not
rescan unrelated ranges. This preserves deterministic failure and one decoder
without multiplying full validation cost by consumer count.

### Unsafe unchecked fast paths after validation

Rejected. They would create a second decoder whose offsets and tags can drift,
turn an invariant failure into undefined behavior, and prevent straightforward
malformed-data fuzzing. If safe checked access is measurably expensive, improve
the schema/view representation under the performance protocol rather than
bypassing it.

### Widen indices and ranges to `usize` or `u64`

Rejected. It increases instruction and terminator size, makes artifacts
host-width-sensitive if `usize` is used, and weakens current cache locality.
Checked `u32` limits are ample for a single IR artifact; exceeding them yields
a resource-limit diagnostic rather than truncation. Widening requires a later
ADR with representative evidence.

## Non-goals

This decision does not:

- change Rue syntax, semantics, diagnostics for valid programs, or the
  language specification;
- prescribe one physical element type for all compiler phases;
- serialize live RIR, AIR, or CFG ranges as a stable durable format;
- make a range transferable between artifact instances or compiler epochs;
- redesign instruction arenas, type identity, CFG algorithms, or codegen;
- require a public generic schema API; or
- use M6 to add a peer compiler pipeline or artifact authority outside
  `CompilerSession`.

## Consequences

### Benefits

- Instruction fields express which payload family they address.
- Producers, readers, validators, and fuzz tests share one layout definition.
- Variable-width evolution cannot leave a production consumer on stale
  positional arithmetic.
- RIR reads become zero-allocation while compact word storage is preserved.
- AIR and CFG retain representations appropriate to their phases.
- Wrong-family/store use becomes unrepresentable, while overflow and corruption
  failures become deterministic and contextual rather than panics from
  arbitrary indexing.
- Guardrails have a precise final API and source inventory to enforce.

### Costs and risks

- The migration touches producers and consumers across semantic analysis, CFG
  optimization, codegen, compiler artifacts, printers, and tests.
- Handwritten phase-local primitives repeat a small amount of range boilerplate.
- Safe checked decoding retains some branches and bounds checks on hot paths;
  the benchmark gates determine whether view specialization is needed.
- Two-`u32` ranges cannot encode an artifact-instance stamp, so owner
  provenance depends on opaque payload-bearing nodes, non-detachable ranges,
  and owner-mediated construction rather than runtime detection.
- Bounded variable traversal adds a 4-byte logical-count header to every
  nonempty variable payload instance. This is chosen over wider instruction
  ranges or an O(records) iterator pre-scan and is subject to the explicit
  per-family physical-byte and performance gates.
- Atomic staging can allocate while building some variable payloads even
  though reads allocate nothing. Implementations should reuse scratch storage
  when measurements justify it without exposing partial mutation.

## Completion criteria

ADR-0056 becomes implemented only when:

- [x] RUE-790 and RUE-839 through RUE-844 are merged in their dependency order.
- [x] RUE-838 is merged before the production M6 migrations begin, and Linear
      records the external M5 gate on the first runnable M6 issues.
- [x] RIR, AIR, and CFG each own handwritten phase-local schema modules under
      the common contract, with no shared erased store or schema generator.
- [x] All production instruction, place, and terminator payload fields use
      family-specific typed ranges or indices.
- [x] Compile-time assertions prove stored ranges remain two `u32`s with the
      expected alignment.
- [x] Raw range construction, physical store mutation, and range fields are
      private to owning schema/artifact modules.
- [x] Payload-bearing nodes cannot be cloned or inserted detached from their
      owner; whole-owner clone is coherent and selective transfer rebuilds.
- [x] Each family has one authoritative width/tag/count/offset implementation
      used by its builder, checked decoder, and artifact validator.
- [x] Builders check every conversion and arithmetic operation, append
      atomically, and cover empty and maximum-size behavior.
- [x] Checked malformed fixtures cover truncation, overflow metadata, unknown
      tags, noncanonical booleans, bad IDs/references, trailing words, and
      bad same-family ranges without `unsafe` or undefined behavior;
      compile-fail tests cover family/store mismatch, raw construction, range
      extraction or cross-editor movement, and detached node insertion.
- [x] RIR getters named in the context traverse without allocating, and all
      migrated family iterators implement `ExactSizeIterator`.
- [x] AIR and CFG borrowing consumers retain their phase-specific typed element
      stores rather than being flattened into words.
- [x] Editors are the only mutable form; consuming validation with canonical
      interner/source/type contexts produces immutable validated owners, and
      `CompilerSession` publishes only those owners.
- [x] Import, whole-owner clone, selective remap, transform, rewrite, fuzz, and
      canonical publication paths use the production schema validators and
      preserve or rebuild owner provenance.
- [x] Infallible validated-artifact access delegates to the checked decoder;
      no unsafe or duplicate trusted decoder exists.
- [x] The benchmark matrix records paired before/after wall time, RSS,
      allocations, phase timing, compiler clean build, and incremental rebuild
      evidence, including logical/capacity/staging bytes, and all structural
      and non-regression gates pass.
- [x] RUE-844's inventory proves public raw constructors, direct side-table
      indexing, raw enum casts, allocating default getters, mutable raw side
      tables, and compatibility payload paths are absent from production code.
- [x] Focused tests, `scripts/rue quick`, relevant compiler/CFG/codegen suites,
      and CI pass.
- [x] RUE-844 updates this ADR's context, checklist, status, and generated index
      to describe the verified final architecture.

## Open questions

None. Any change to the chosen ownership, representation, validation, or
performance contracts requires an ADR amendment rather than an implementation
issue making a local exception.

## References

- [ADR-0024: Canonical Type Handle and Intern Pool](0024-type-intern-pool.md)
- [ADR-0053: Typed CompilerSession query state](0053-typed-compiler-query-state.md)
