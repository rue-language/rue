# Per-body identity closure materialization

Status: measurement and design note, 2026-08-17, extended 2026-08-18. It answers
one question the
[post-ADR-0063 cold audit](post-adr-0063-cold-compiler-architecture-audit.md)
and the [ADR-0071 re-audit](adr-0071-horizontal-vertical-ownership-reaudit.md)
left open: semantic analysis is roughly 45 percent of a cold Lattice build, and
per-body semantic materialization is the suspected cause. Current source and
the measurements below are authoritative; the suspicion that motivated the
investigation is not.

Read in two parts. Everything through "Rejected on the measurements" is the
original per-body minting measurement and the sharing designs it does and does
not support; the interning half of that diagnosis shipped as ADR-0076 and the
spelling memos. "The other half: durable-source provider queries" re-measures
the provider half on later trunk, with its own totals — the cold builds moved
underneath the earlier numbers, so the two parts' absolute instruction counts
are not comparable and each part states its own baseline.

## Result

The suspicion is half right, and the half that is wrong is the half that was
being optimized.

- **Right**: every function body re-materializes the same whole-program
  identity closure. Cold Lattice mints 6,502 named nominals for 61 distinct
  ones, 13,823 anonymous nominals for 42 distinct ones, and installs 80,306
  anonymous-method endpoints for 394 distinct ones. The whole cluster is
  828,934,152 retired instructions, 11.43 percent of the cold build.
- **Wrong**: the intern pool is not where that cost lands. Registering the pool
  entries themselves — `declare_struct`, `complete_declared_struct`,
  `register_enum`, pointer interning — is 41.9 million instructions, 0.58 percent
  of the build. `finalize_containment_metadata`, `derive_overlay`, and
  `flatten` together cost 9.5 million, 0.13 percent. The `derive_overlay` O(1)
  fast path *is* defeated by body-local lazy minting exactly as suspected (1,087
  slow flattens against 176 fast shares on Lattice), and fixing that would
  recover approximately one eighth of one percent of the build.

The cost is the *identity, name, and signature work around* each mint: string
interning, durable-source fact queries, parameter-arena allocation, and key
cloning and hashing. That work is redundant across bodies for the same
structural reason the pool work is — every body owns a fresh `TypeInternPool`
*and* a fresh `ThreadedRodeo` — but no change confined to the pool reaches it.

One bounded, byte-identical subset was extracted and is implemented below
(-0.5682 percent Lattice instructions). Everything else requires an ADR,
because the shared thing that would have to be shared is an index space that
compiled artifacts are keyed on.

## What was measured

Fixed one-worker cold release builds, `-O3 -j1`, `RUE_STD_PATH` at the
repository `std`. Three shapes:

| shape | bodies | total ms | semantic ns | semantic share |
| --- | ---: | ---: | ---: | ---: |
| Lattice (`performance/workloads/lattice/main.rue`) | 1,263 | 2,701.33 | 1,290,235,844 | 47.8% |
| Mosaic (`examples/mosaic/main.rue`) | 630 | 985.41 | 432,529,736 | 43.9% |
| chain256 (generated, 256 single-call modules) | 257 | 332.19 | 150,342,980 | 45.3% |

The three shapes bracket the question: Lattice and Mosaic have a large shared
nominal closure, chain256 has none, and all three spend roughly the same share
of the build in semantic analysis. Absolute milliseconds move between runs on
this host by tens of percent; the shares are stable and the instruction counts
below are exact.

Instruction attribution is `valgrind --tool=callgrind` on the release binary,
which is deterministic and therefore the only usable comparison metric on this
host: sixteen balanced alternating Lattice clock pairs had a 28.69 percent
paired MAD, so wall clock says nothing at this magnitude and is not quoted as
evidence anywhere below.

Structural counts come from temporary process-global counters in
`TypeInternPool`, `BodyIdentityPool`, and `ProviderBodyHost`, removed before
commit. They are reported here rather than published, because publishing them
would add counters to the 136-counter `compiler_work` contract for a diagnosis
that is now complete.

## The per-body identity closure

`BodyRirBundle::provider_body_state` builds one `ProviderBodyAnalysisState` per
body, and `BodyIdentityPool::new` gives it a brand-new `TypeInternPool` seeded
with the builtin enums and `str`. The body's `ThreadedRodeo` is likewise fresh:
`canonical_lower` constructs each body's symbol table alongside its RIR. Both
are index spaces — a `Type` is a pool index, a `Spur` is an interner index — and
both are private to one body, so a nominal, an anonymous shape, or a callable
signature that ten bodies mention is materialized ten times from scratch.

Cold Lattice, exact counts:

| operation | performed | distinct identities | redundancy |
| --- | ---: | ---: | ---: |
| named nominal mints | 6,502 | 61 | 106.6x |
| anonymous nominal mints | 13,823 | 42 | 329.1x |
| anonymous-method endpoint installations (owners) | 4,316 | 25 | 172.6x |
| anonymous-method endpoint signatures | 80,306 | 394 | 203.8x |
| pool entries pushed | 31,923 | — | — |
| pools created | 1,263 | — | 1 per body |

The whole program's nominal closure is 103 types. The compiler materializes it
20,325 times. `nominal_materializations = 6502` in the published
`semantic_provider` counters is the same number arrived at independently, which
is the cross-check that the temporary counters were measuring the real path.

Per body the closure is small — a median of 34 pool entries, 3 of them local to
the body's own overlay and 31 read from its sealed base, with a p90 of 37 and a
maximum of 62. That is why "O(bodies x closure size)" understates the problem in
one direction and overstates it in another: the pool is tiny, and the work per
minted entry is not.

## Where the 828 million instructions go

Total cold Lattice is 7,249,286,766 instructions. The semantic body closure
query (`RevisionedQueryDatabase::body_closure`) is 2,636,241,835 of them
(36.37 percent). Under it, `analyze_provider_ordinary_body` is 1,405,617,290
(19.39 percent) across 1,053 ordinary bodies, with the remaining 210 of the
1,263 transactions split between `analyze_provider_anonymous_body` (168 bodies,
88,021,849) and `analyze_provider_specialized_body` (42 bodies, 23,867,894).

Entering the identity cluster — `register_import_nominal_identities`,
`register_provider_anonymous_method_endpoints`, `find_or_create_anon`,
`resolve_provider_type`, `mint_named`, `resolve_anonymous_shape_type`,
`resolve_callable_type`, `build_function_signature`, `intern_params`,
`resolve_function`, `resolve_method` — costs 828,934,152 instructions,
11.43 percent of the build and roughly 28 percent of semantic analysis. Only
53,437,256 of that (6.4 percent of the cluster) is the cluster's own code. The
rest is leaves:

| leaf | instructions | share of build | calls |
| --- | ---: | ---: | ---: |
| `ThreadedRodeo::try_get_or_intern` | 213,748,722 | 2.95% | 186,840 |
| durable-source provider queries (`CompilerBodyDurableSource`) | 197,686,859 | 2.73% | 35,120 |
| `HashMap::insert` | 57,931,149 | 0.80% | 215,260 |
| `ParamArena::alloc` | 48,179,775 | 0.66% | 84,386 |
| `AnonymousNominalKey` clone / map / hash / eq | 67,349,618 | 0.93% | 193,091 |
| `TypeInternPoolInner::struct_symbol_name` | 26,732,482 | 0.37% | 80,313 |
| `append_member_callable_name` | 25,751,628 | 0.36% | 80,306 |
| `complete_declared_struct` + `declare_struct` + `register_enum` + pointer interning | 41,867,758 | 0.58% | 42,689 |
| `format_inner` + `Formatter::pad` | 30,649,010 | 0.42% | 83,815 |

The pool line is the second-smallest entry in that table. Every hypothesis in
the brief that targeted the pool — a shared frozen closure pool with genuinely
empty overlays, memoized closures keyed by an identity epoch, pre-minting the
closure per module — would, if it changed only the pool, be competing for
0.58 percent of the build while leaving the 2.95 percent of string interning and
the 2.73 percent of durable-fact querying exactly where they are.

### The interner is the larger half of the same fact

186,840 interning calls at an average 1,144 instructions each is insertion cost,
not lookup cost: a member callable name is roughly fifty characters
(`__anon_struct_<32 hex digits>.<member>`), there are 394 distinct ones in the
whole program, and each is inserted afresh into every interner whose body
mentions its owner. `Spur` values are interner-relative, so this is the
same sharing problem as the pool wearing different clothes, and any design that
shares the type closure without also sharing the symbol space keeps this term.

## Falsifier: chain256

The generated chain shape isolates the fixed per-body cost. 256 modules, each
importing the next and calling one function through it, plus a root. It mints
nothing at all: zero named mints, zero anonymous mints, zero endpoint
installations, five pool entries per body (the builtins), and `derive_overlay`
takes its O(1) shared-base path on all 257 bodies.

The identity cluster on chain256 is 2,432,182 instructions, 0.42 percent of that
build — and semantic analysis is still 45.3 percent of its wall time. Its
instruction profile is dominated by import discovery, source-snapshot recording,
path comparison, and query bookkeeping, none of which the minting work touches.

This falsifies the general claim. Per-body identity materialization is a
Lattice-shaped (and Mosaic-shaped) cost that scales with the size of a program's
shared nominal closure, not a universal per-body semantic tax. Any design note
that predicts a semantic speedup on all workloads is wrong; the honest
prediction is a speedup proportional to closure reuse.

## Candidate designs

Each is stated with the deterministic artifacts it changes, the falsifier that
would kill it, and the saving the measurements above actually support — not the
saving the shape of the idea suggests.

### A. One frozen closure pool shared across bodies, with copy-on-write overlays

Mint the whole-program nominal closure once per revision into a frozen
`TypeInternPool`, derive every body's pool from it, and keep body overlays
genuinely empty for closure types.

*Reachable saving*: the pool-registration term and the containment term —
41.9 million plus 9.5 million instructions, about 0.71 percent of the build. It
does **not** reach the interning, durable-query, param-arena, or key-cloning
terms unless the same change also shares the symbol space, because those are
performed by the caller before and around the pool call, not by the pool.

*Deterministic artifacts that change*: pool indices, which are not merely
internal. `ConstraintGenerator::expr_types` is ordered by them (the ahash sweep
noted this), `CfgDomainProjection` reads them, and a shared base assigns them in
whole-program mint order rather than per-body first-touch order. Every body's
indices shift. Byte-identity of emitted executables is not preserved by
construction; it has to be re-established or the counters rebaselined.

*Falsifier*: build the shared pool, keep everything else, and check that
Lattice's executable digest survives. If it does not, this is an ADR-scale
contract change, not an optimization. The measurements predict it survives only
if every index consumer is order-independent, which the `expr_types` ordering
note says it is not.

*Risk*: high. It converts a per-body private index space into a shared one
across a boundary the query graph currently uses to keep bodies independent, and
the revision-scoped invalidation story for the frozen base is unwritten.

### B. Memoize minted closures across bodies, keyed by the closure's identity epoch

Cache `(durable key, epoch) -> minted result` so the second body reuses the
first body's work.

*Reachable saving*: nothing, as stated. The cached value is a `Type`, which
means a pool index in *the first body's* pool, and a `Spur` in the first body's
interner. Neither is meaningful in the second body. A memo that has to be
re-relocated per body is the mint it was replacing. This candidate collapses
into A plus a shared interner, and should not be pursued separately.

*Falsifier*: none needed; it is refuted by the index-space argument, and the
measurement that 99 percent of mints are redundant does not rescue it.

### C. Pre-mint the closure once per module or revision

The same as A with a different scope key. Per-module scoping is strictly worse
than per-revision here: Lattice's 61 distinct named nominals are already shared
across all 1,263 bodies, so module scoping keeps most of the redundancy while
paying all of A's index-space cost.

*Reachable saving*: bounded above by A, and lower in proportion to how many
modules touch each nominal.

*Risk*: same as A, minus the invalidation advantage of a revision-scoped base.

### D. Share the body symbol space, not the type space

The measurement says the largest single term is interning, so the design that
follows the numbers shares the `ThreadedRodeo` across the bodies of one revision
instead of (or before) sharing the pool.

*Reachable saving*: up to 213 million instructions of interning plus the
formatting that feeds it — roughly 2.9 to 3.3 percent of the build, five times
what candidate A can reach.

*Deterministic artifacts that change*: `Spur` values, which are used as map keys
and in AIR. Whether any of them is a *sort* key is the first thing to establish;
if one is, ordering changes and output is not byte-identical.

*Risk*: high, and it interacts with body-local RIR ownership
(`require_rir_authority` asserts the analysis state's interner is pointer-equal
to the body RIR's). This is the candidate the measurements favor, and it is the
candidate that most clearly needs an ADR.

### E. Bounded subset: render each anonymous owner's symbol once per installation

Not a sharing change. `register_provider_anonymous_method_endpoints_inner` and
`install_provider_anonymous_methods_with_issued` spelled a member callable name
per method by re-reading the owner's symbol name from the pool, then appending
onto an exactly-sized `String` — a pool read lock plus a reallocation for each
of the 80,306 member spellings that 4,316 installations produce, when the owner
is constant across the loop and a pool entry's name never changes after
registration.

*Reachable saving*: the `struct_symbol_name` and `append_member_callable_name`
terms above, 52.5 million instructions, minus the cost of building each name
once with its exact final capacity.

*Deterministic artifacts that change*: none. The rendered string is identical,
so every interned symbol, every map keyed by one, and every emitted byte is
identical.

*This is implemented.* Its result is below.

## Implemented: E

`member_callable_name_for_owner` replaces `append_member_callable_name` as the
single spelling authority (the RUE-1236 one-renderer rule is preserved: there is
still exactly one function that joins an owner, a separator, and a member name).
It takes the owner by reference and reserves the exact final capacity, so
joining never reallocates. `member_callable_owner` renders the owner component,
and both anonymous-method installation loops hoist it out of their per-method
body.

Cold Lattice `struct_symbol_name` calls from the endpoint installer fall from
80,306 to 4,316, and `append_member_callable_name`'s reallocating append is
gone. Retired instructions, measured by callgrind on the release binary:

| shape | parent | current | delta |
| --- | ---: | ---: | ---: |
| Lattice | 7,249,597,095 | 7,208,401,429 | -41,195,666 (-0.5682%) |
| Mosaic | 3,001,117,064 | 2,988,168,092 | -12,948,972 (-0.4315%) |
| chain256 | 585,064,585 | 585,053,899 | -10,686 (-0.0018%) |

chain256 has no anonymous methods, which is the expected shape of the result and
a check that the change touches only the path it claims to.

Emitted executables are byte-identical on all three shapes:
Lattice `b893e76cfabed737b149d0e8c4d8527077dedd17da78418db20a28a7d30885e5`,
Mosaic `be93e5264ca189db2ba5ce43a9a343488e6361194c041e6933afe9f7bed3a208`,
chain256 `4d669c38e603d4278cb0fb3ce0d005011e851a26cc149164c46573639aea76e7`.
All 136 `compiler_work` counters, plus `source_metrics`, `emitted_output`, and
`compiler_boundary`, are identical on all three (658, 424, and 946 compared
fields, zero differences). Peak RSS is neutral: a -0.1672 percent paired median
with 0.2842 percent paired MAD over sixteen balanced Lattice pairs. Compiler
clock is not reported, for the reason given at the top.

## Implemented: the rendering that fed the lookups

Sharing the symbol space (ADR-0076) turned each body's interner insertion into a
lookup, but a lookup still needs the rendered string, so the *rendering* was
still performed once per body. Measured on trunk after ADR-0076, cold Lattice at
6,771,047,268 instructions:

| site | instructions | share | what it renders |
| --- | ---: | ---: | --- |
| `member_callable_name_for_owner` | 13,355,977 | 0.197% | `Owner.method` joins, ~80,300 per build |
| `format_inner` under `find_or_create_anon` | 19,576,691 | 0.289% | `__anon_struct_<digest>` and its `.__drop` |
| `try_get_or_intern` from the endpoint installer | 63,715,889 | 0.941% | hashing those rendered strings, 160,404 calls |

Both families are a total function of data that does not vary across the
bodies of one revision — a member name of the owner and member spellings and
the separator; an anonymous nominal's name of its producer digest — so the
generation memoizes the spelling-to-handle association in `SharedSymbolSpace`
(`derived_symbol`, `keyed_symbol_spelling`, `keyed_name`). Only the first body
to need a spelling renders it; the rest take a handle-keyed map lookup instead
of a fifty-character render, hash, and compare. The memos live inside the
generation, so ADR-0076's retirement semantics govern them unchanged, and they
never intern a string the unrendered path would not have interned — the
retention counters are read from the interner's length and byte count.

Cold Lattice `member_callable_name_for_owner` falls to 471,179 instructions,
`format_inner` to 84,228,020, and `try_get_or_intern` to 197,729,360, against a
new `derived_symbol` term of 8,983,621. Retired instructions:

| shape | before | after | delta |
| --- | ---: | ---: | ---: |
| Lattice | 6,771,047,268 | 6,709,961,467 | -61,085,801 (-0.9022%) |
| Mosaic | 2,787,590,395 | 2,770,003,393 | -17,587,002 (-0.6309%) |
| chain256 | 553,664,114 | 554,249,181 | +585,067 (+0.1057%) |

chain256 mints no anonymous nominals and installs no anonymous methods, so it
consults neither memo; its per-generation construction cost measures 5,194
instructions there and the rest of its movement is inlining reshuffling in
untouched code (`SourceMetadata::extend_with_appended` -3.1M against
`Iterator::eq_by` +1.8M and `physical_path` +1.6M).

Executables are byte-identical on all three shapes at `-j1` and `-j4`
(Lattice `45784ce7c7cde992d7ea820912ca1692c05dc7582a367210d017b3765a9a89e7`,
Mosaic `4cdead1a98e77fce940b5d6f9b693d8decba26b1c666896bcc78d48a1b79f97b`,
chain256 `45f01130cbbae57a2fe22f95e23cb34902a8c28040cc677dce56ba84050c0b1a`), as
are `compiler_work`, `source_metrics`, `emitted_output`, and
`compiler_boundary` — 615, 381, and 903 compared fields, zero differences. The
one exclusion is the `compiler_work.query_runtime` scheduling counters at `-j4`,
which a baseline-versus-baseline pair of the *same* binary moves by the same
magnitudes; they are identical at `-j1`. Peak RSS over twelve interleaved pairs:
Lattice -0.22% (MAD 0.34%), Mosaic -0.03% (MAD 0.13%), chain256 +0.04%
(MAD 0.06%).

## What needs an ADR

Candidates A, C, and D all move a body-private index space into a shared one.
That is an ADR-0063 boundary question — bodies are independent because their
artifacts are request-independent, and stable identities rather than compact
indices are what cross query boundaries — not an optimization that a
byte-identity gate can decide. Any of them should be proposed as an ADR that
states, at minimum:

1. which index space becomes shared, and what its revision scope is;
2. whether emitted output remains byte-identical, and if not, what the
   rebaseline procedure for the executable digests and the 136 counters is;
3. what invalidates the shared base, and what happens to bodies analyzed against
   a superseded one;
4. whether `require_rir_authority`'s pointer-equality contract between a body's
   RIR interner and its analysis state survives, and what replaces it if not.

The measurements say D has roughly five times the headroom of A and should be
evaluated first, which inverts the ordering the pool-shaped framing suggests.

## Rejected on the measurements

- **Fixing the `derive_overlay` fast path alone.** Body-local lazy minting does
  defeat it — 1,087 flattening derivations against 176 sharing ones on Lattice,
  copying 27,145 local and 5,435 base entries in total — but the entire
  containment and overlay term reached from body analysis is 9,519,746
  instructions, 0.13 percent of the build. The diagnosis was correct and the
  target is not worth an implementation.
- **Caching the formatted member callable name across bodies.** It removes the
  formatting but not the interning, because the body's interner is fresh and the
  insertion is the expensive half. Subsumed by D — *while the interner was
  body-private*. D landed as ADR-0076 and made the interner revision-shared,
  which turned this from subsumed into the remaining half: a shared-space lookup
  still needs the rendered string, and a rendered-name-to-handle association is
  now stable across the revision's bodies. Implemented above.
- **Making anonymous-method endpoint installation lazy.** 76 endpoints are
  installed per body and it is not established how many are used. This changes
  work counters by construction, so it is a contract change, not a bounded
  subset, and it should not be attempted before D settles whether the
  installation is expensive at all.

## The other half: durable-source provider queries

Status: re-measurement and verdict, 2026-08-18 (RUE-1580). The table above named
durable-source provider queries as the second-largest leaf of the identity
cluster, at 197,686,859 instructions and 2.73 percent of cold Lattice, and only
the interning half was picked up — ADR-0076, then the spelling memos. This
section re-measures the provider half on current trunk, establishes how much of
it is redundant across bodies, and records why that redundancy is not reachable
without an ADR.

Same protocol as above: `valgrind --tool=callgrind` on a release build
(`--target-platforms //platforms:release`), `RUE_STD_PATH` at the repository
`std`, `-j1`, three shapes. Cold totals moved with trunk — Lattice
6,889,260,664, Mosaic 2,779,780,516, chain256 510,415,731.

### The boundary is larger than the earlier figure, and shape-independent

The 2.73 percent counted only the durable-source calls reached from inside the
identity cluster. Every call crossing from body analysis into
`CompilerBodyDurableSource` costs:

| shape | boundary calls | instructions | share of build |
| --- | ---: | ---: | ---: |
| Lattice | 102,848 | 361,075,319 | 5.241% |
| Mosaic | 39,681 | 131,022,177 | 4.713% |
| chain256 | 6,668 | 24,322,801 | 4.765% |

chain256 is the result that reframes the problem. It mints no nominal at all and
asks 26 questions per body against Lattice's 81, yet the boundary is the *same
share* of its build. The durable-source boundary is therefore not a
closure-shaped cost the way the minting cluster is. What sets its share is the
price of one question, and every body pays that price for questions whose
answers it could have been handed.

### Question kinds, asks, and distinct answers

Asks and instructions are the callgrind attribution. The distinct counts come
from temporary process-global counters keyed on each question's exact argument,
removed before commit. `per body` counts asks that are the first of their key
*within one body*, which is the ceiling of what a body-local memo could reach;
`program` counts distinct keys over the whole build, the ceiling of what a
revision-scoped memo could reach.

Cold Lattice:

| question | asks | distinct per body | distinct in program | instructions | share | Ir/ask |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `DurableAnonymousSource::anonymous_shape_and_digest` | 13,823 | 13,823 | 42 | 91,955,776 | 1.335% | 6,652 |
| `DurableNominalSource::nominal` | 10,159 | 6,502 | 61 | 61,787,716 | 0.897% | 6,082 |
| `DurableBodyLookupSource::value_const` | 15,697 | 2,845 | 1,521 | 33,258,020 | 0.483% | 2,118 |
| `DurableCallableSource::function` | 5,013 | 3,769 | 996 | 32,410,168 | 0.470% | 6,465 |
| `DurableAnonymousSource::anonymous_methods` | 4,546 | 4,317 | 25 | 30,818,769 | 0.447% | 6,779 |
| `DurableBodyLookupSource::qualified_free_function` | 1,841 | 1,841 | 254 | 16,334,368 | 0.237% | 8,872 |
| `DurableBodyLookupSource::free_function` | 919 | 898 | 758 | 10,250,789 | 0.149% | 11,154 |
| `DurableBodyLookupSource::nominal` | 12,132 | 1,358 | 595 | 9,429,536 | 0.137% | 777 |
| `DurableBodyLookupSource::module_source` | 4,927 | — | — | 9,018,201 | 0.131% | 1,830 |
| `DurableNominalSource::nominal_file_id` | 6,502 | — | — | 8,530,283 | 0.124% | 1,311 |
| `DurableBodyLookupSource::language_item_nominal` | 456 | 456 | 100 | 8,494,677 | 0.123% | 18,628 |
| `DurableBodyLookupSource::qualified_value_const` | 2,927 | — | — | 5,706,097 | 0.083% | 1,949 |
| `DurableBodyLookupSource::named_member` | 250 | 250 | 61 | 3,613,587 | 0.052% | 14,454 |
| `DurableCallableSource::method` | 311 | 311 | 61 | 3,269,785 | 0.047% | 10,513 |
| `DurableConstSource::constant` | 306 | 153 | 91 | 2,529,269 | 0.037% | 8,265 |

The redundancy the diagnosis predicted is there, and it is enormous: 13,823
anonymous-shape consults for 42 answers, 10,159 nominal consults for 61, 4,546
anonymous-method consults for 25. The answers are revision-stable — each is a
total function of its key within a revision, which is why the query engine
memoizes them at all.

The column that decides the question is `distinct per body`. It equals or nearly
equals `asks` for every expensive question: 13,823 of 13,823 for anonymous
shapes, 1,841 of 1,841 for qualified free functions, 456 of 456 for language
items. The per-provider `nucleus_cache` and `lookup_name_cache` and the per-body
`BodyDurablePayloadCache` have already taken everything a body-local memo can
take; they are exactly the gap between asks and `distinct per body` wherever one
exists (10,159 against 6,502, 5,013 against 3,769). **All the remaining
redundancy is across bodies, and none of it is within a body.**

### Where an ask's instructions go

Cold Lattice, everything the boundary's code calls out to, plus its own code:

| leaf | instructions | share of build | calls |
| --- | ---: | ---: | ---: |
| `QueryContext::query_registered` | 104,673,896 | 1.519% | 63,852 |
| the boundary's own code | 37,348,593 | 0.542% | — |
| `definition_hash_accelerator` | 28,490,340 | 0.414% | 3,933 |
| `BTreeMap` search (canonical anonymous registry) | 28,138,840 | 0.408% | 31,104 |
| `CanonicalAnonymousNominalRegistry::extend` | 23,663,969 | 0.343% | 19,296 |
| `Map::fold` (anonymous shape and method projections) | 23,401,991 | 0.340% | 7,742 |
| `BTreeSet::insert` (the body's positive-reference set) | 23,323,316 | 0.339% | 37,859 |
| `ObservedLookupRoot::record` | 15,978,274 | 0.232% | 20,117 |
| `Rc::drop_slow` (registry entries displaced by re-merges) | 14,609,914 | 0.212% | 11,638 |
| `HashMap::insert` (per-body payload caches) | 10,428,190 | 0.151% | 24,003 |

The `query_registered` term is inclusive, so it carries both the
once-per-revision evaluation of each distinct question and the per-body cost of
asking a question that is already answered. Attribution alone does not separate
them, and it is the term a cross-body memo would be trying to remove.

### Why the redundancy is not reachable

Three walls, and a candidate has to clear all three.

**A memo hit must not drop an edge.** Every redundant ask reaches its answer
through `QueryContext::query_registered`, and that call is what records the
asking body's dependency edge on the answering node. A revision-scoped memo
answering from another body's result would leave this body with no edge, so a
later edit to the answering declaration would not invalidate it. Replaying the
edge costs the round trip the memo was removing. This is not a new rule: the
existing `nucleus_cache` and `lookup_name_cache` are legal precisely because
they serve a repeat *within the one query task that already recorded the edge*,
which their own comments state, and the `distinct per body` column above is the
measurement that they have already consumed that allowance completely.

**The ask counts are published.** `name_lookups` (50,864), `identity_facts`
(11,349), `signature_facts` (10,582), `const_facts` (13,419), `producer_facts`
(13,647), and the four `*_materializations` (10,888 combined) are metered at the
ask site. Answering a question without asking it moves every one of them. Under
a bar of byte identity plus unchanged deterministic counters that is a contract
change, not a bounded subset. The existing `nominal_materialization_reuses` and
`function_materialization_reuses` split is the shape an honest cross-body tier
would have to take, and adding that tier is a `compiler_work` revision.

**The residue needs shared state whose visibility is an ADR-0063 question.**
Subtract the query round trips and the metered asks, and what is left is the
per-body rebuild of derived state: the canonical anonymous registry — search,
merge, and displaced-`Rc` churn together 66,412,723 instructions, 0.96 percent
of the build — the anonymous shape and method projections (23,401,991, 0.34
percent), and the stable-key hash accelerator (28,490,340 inside the boundary,
0.41 percent). All three are total functions of revision-stable inputs, and each
has its own obstruction:

- The registry's merge rule is monotone: a methods-bearing entry never degrades,
  so a revision-shared registry converges to the same value whatever order
  bodies merge in. But a body reading a shared registry can see an entry that
  *another* body upgraded, which is a richer answer than that body would have
  derived from its own consults. That is a change in what one body observes
  about another — the ADR-0063 body-independence boundary — not an optimization
  a byte-identity gate can decide.
- The projections are pure functions of a registry entry, so they ride on
  whatever the registry decides and need no separate ruling.
- `definition_hash_accelerator` is the surprise. Its docstring calls it a bucket
  selector that "is recomputed at issuance and is not a durable or serialized
  identity", and it is indeed reachable only through `Hash for
  StableDefinitionKey`. But `BodyQueryKey::stable_hash` absorbs
  `FunctionInstanceKey::hash`, and with it those accelerator bytes, into
  `rue_query::stable_key_hash` — the content-derived, process-independent node
  digest whose collision witness also orders colliding keys. The accelerator is
  therefore part of a published deterministic identity, so making the digest
  *cheaper* changes published values. Memoizing it does not. At 65,494,916
  instructions build-wide over 8,921 issuances — 0.95 percent of cold Lattice,
  7,341 instructions of SHA-256 per key — that is the largest single candidate
  this measurement found. It needs a revision-scoped store reachable from
  `StableDefinitionKey::from_stable_parts`, which today is a free constructor
  with no revision in scope. *Taken up while this note was being written:*
  RUE-1587 hashes durable anonymous nominals by their cached digest, which is
  the memoization this paragraph asks for; the numbers above are the
  pre-RUE-1587 attribution that motivated it.

### Attempted and rejected on the measurements

Two bounded subsets were implemented and measured before this verdict, on the
theory that a body-local memo or a cheaper container could take the residue
without touching an edge or a counter. Both were reverted.

- **Remembering, per body, which producers' anonymous projections have already
  been merged.** Sound: the merge is idempotent under the monotone rule, and the
  producer fact is still requested every time, so no edge and no counter moves.
  It reaches nothing. `CanonicalAnonymousNominalRegistry::extend` runs 19,296
  times before and 19,296 times after, because the 13,436 producer consults on
  that path are 13,436 *distinct* `(body, producer)` pairs. The repeats the
  registry does see return early on a methods-bearing entry and never reach the
  merge at all. Cost of the memo that never fires: 13,436 hash-set insertions
  and key clones, +8,980,385 instructions.
- **Keying the canonical anonymous registry by hash instead of by order.** The
  registry has exactly two operations and nothing iterates it, so this is
  byte-identical and counter-neutral by construction. Registry lookup falls from
  28,138,840 to 20,035,197 instructions and merging rises from 23,663,969 to
  26,054,746: a net -5,712,866, 0.083 percent of the build. That is below the bar
  this note already set when it declined the `derive_overlay` fast path at 0.13
  percent, and it spends a documented ordered container to get there.

Measured together the pair is +3,893,137 (+0.0565 percent) on Lattice — the
arithmetic of a memo that never fires, paid for out of a container change that
barely does. Executables stayed byte-identical on all three shapes at `-j1` with
both applied, which is the evidence that the two subsets were sound and merely
worthless.

### Input for RUE-1576

The largest single term this measurement crossed is not a semantic-provider
leaf. `Task::commit_handoffs` — the lookup-lease and retained-cone transport
that publishes a body transaction's observed roots — costs 140,200,300
instructions on cold Lattice, 2.035 percent of the build, over 1,270 calls.
Inside it, `PublishedRootLookupLease::record_incarnation` spends 59,822,892
instructions in 76,268 `BTreeMap` insertions and 24,417,121 in 35,974 removals.
The two handoff commits, `PublishedBodyClosureLookupHandoff` and
`PublishedLookupRootHandoff`, are 46,190,040 and 43,101,609 of the total over
the same 19,067 lease records each, which is the shape of two transports
carrying one observation set.

The same term is 47,209,936 instructions on Mosaic (1.698 percent) and 6,426,955
on chain256 (1.259 percent), so it scales with bodies rather than with closure
size. `ObservedLookupRoot::record`, the provider-side half that pins each
observed terminal while the request lease still protects it, adds 15,978,274
(0.232 percent) inside the durable-source boundary.

This is proof-lease transport, which is RUE-1576's scope rather than this one's.
It is recorded here rather than acted on.

### Verdict

The provider half of the identity-closure cost is real, is 5.2 percent of cold
Lattice, and is up to 329 times redundant across bodies. It is not the same kind
of problem as the interning half ADR-0076 solved. The interner was a
body-private *index space*, and sharing it needed a determinism audit; these are
*questions asked of the query graph*, and the asking is the dependency record.
Nothing here is reachable by a change confined to the semantic provider.

What the measurements support is one candidate worth an ADR on its own — a
revision-scoped memo for the stable-key hash accelerator, 0.95 percent of the
build for a value that is a pure function of the key's fields and identical
whoever computes it — and one that needs the ADR-0063 body-independence ruling
before it can be designed at all: a revision-shared canonical anonymous
registry, 1.3 percent of the build with its projections, whose merge converges
but whose visibility does not stay per-body.
