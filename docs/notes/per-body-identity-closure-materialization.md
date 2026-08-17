# Per-body identity closure materialization

Status: measurement and design note, 2026-08-17. It answers one question the
[post-ADR-0063 cold audit](post-adr-0063-cold-compiler-architecture-audit.md)
and the [ADR-0071 re-audit](adr-0071-horizontal-vertical-ownership-reaudit.md)
left open: semantic analysis is roughly 45 percent of a cold Lattice build, and
per-body semantic materialization is the suspected cause. Current source and
the measurements below are authoritative; the suspicion that motivated the
investigation is not.

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
  insertion is the expensive half. Subsumed by D.
- **Making anonymous-method endpoint installation lazy.** 76 endpoints are
  installed per body and it is not established how many are used. This changes
  work counters by construction, so it is a contract change, not a bounded
  subset, and it should not be attempted before D settles whether the
  installation is expensive at all.
