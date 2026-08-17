# ADR-0076 symbol-handle ordered-use audit

Status: implementation audit, 2026-08-17. This is ADR-0076 Phase 1: the
complete inventory of every place a symbol handle's *order* or *numeric value*
can influence something deterministic, the classification of each, and what was
done about it. Phase 2 shares one append-only symbol interner across the bodies
of a revision, at which point a `Spur`'s value stops being a function of the
body that minted it and becomes a function of worker scheduling. Everything
below is what has to be true before that is safe.

The measurement that motivates the sharing is
[per-body-identity-closure-materialization.md](per-body-identity-closure-materialization.md);
the contract is [ADR-0076](../designs/0076-shared-revision-symbol-space.md).

## Result

Three findings, in descending order of how much they change the plan.

1. **Nothing in the compiler orders symbol handles through `Ord`.** The whole
   ordered-use class travels through one explicit conversion,
   `lasso::Key::into_usize`, which makes the audit finite and greppable rather
   than a search over every comparison in the tree.
2. **Exactly one site let a handle's value reach an emitted artifact**, and it
   was latent rather than live: specialization name mangling spelled a
   symbol-valued comptime argument as its interner index, and that mangled name
   is a link-time symbol. Both of its arms are unreachable today, which is why
   the byte-identity gate below stayed green when they were converted to spell
   the symbol's text.
3. **The blocking finding for Phase 2 is not an ordering at all.** A body's
   symbol handles are today *dense and equal to the packed RIR's symbol
   ordinals*, by an invariant the compiler asserts. A revision-shared interner
   breaks that equality by construction, so Phase 2 needs a body-local remap
   that ADR-0076 does not currently mention. This is stated in full under
   [What Phase 2 still owes](#what-phase-2-still-owes).

## How the inventory was taken

Four sweeps, because no single one is complete.

**Trait-level ordering, proven by the compiler.** `PartialOrd, Ord` were
temporarily removed from the vendored `lasso::Spur` derive and `//crates/...`
was built in full, tests included. It compiled. No code in the workspace orders
a symbol handle through the `Ord` trait, directly or through a derive: a scan of
every type that carries a symbol-handle field or payload (84 in the committed
tree) confirms none of them derives `Ord` or `PartialOrd` either, and no `BTreeMap` or `BTreeSet` in the
tree is keyed by a symbol handle. The vendored derive was restored afterwards.

**Hash iteration order, already neutralized.** Symbol handles are heavily used
as `AHashMap`/`AHashSet` keys — `AHashMap<Spur, Type>`, `AHashMap<Spur,
ConstValue>`, `AHashSet<Spur>` and friends, most of the 1,136 `Spur` mentions in
the tree. None of these iteration orders can reach output, and that is a
property the tree already has rather than one this audit established: `ahash` is
built with `runtime-rng` (`third-party/BUCK`), so its seeds come from
`getrandom` once per process, and `std`'s `RandomState` is seeded per process
too. Iteration order of a hash map keyed by anything already varies between
compiler runs. The executables below are byte-identical across four separate
compiler processes, so no hash-map iteration order of any key type escapes into
a deterministic artifact — the whole class is (a) by construction, and sharing
the interner does not change it.

**Value extraction, enumerated.** Every `Key::into_usize` call site: 88 in the
tree, 38 of them the AST's `Display`. Each is classified below.

**Byte identity, measured.** The gate at the end of this note.

## Classification

`(a)` equality or membership only — safe under any handle assignment.
`(b)` ordered — a handle decides a sort, a search, or an iteration order.
`(c)` value-bearing — a handle's number becomes a name, a stored word, or an
index into something.

### (a) Equality and membership

Everything not named below, which is the overwhelming majority: every
`AHashMap`/`AHashSet` keyed by a handle in `sema`, `inference`, `scope`, the
comptime substitution maps, `KnownSymbols`' pre-interned comparison handles, the
endpoint overlay's exact-lookup registries, `SpecializationKey`'s `base_name`,
and the `PartialEq`/`Hash` derives that carry a handle inside a larger key. No
action; these are exactly what an equality-only handle is for.

### (b) Ordered uses

| Site | What it orders | Classification and action |
| --- | --- | --- |
| `crates/rue-compiler/src/durable_cfg.rs:1201` (`CfgDomainProjection::new`) and `:633` (`import_accessor_cfg`) | Sorts `symbols: Vec<(Spur, StableCfgSymbol)>` by `(live.into_usize(), stable)`, then dedups | **Contained.** The order is a private lookup index consumed only by `callable_for_symbol`'s `partition_point` on the same key, so the answer is a function of the queried handle and never of the order handles were issued in. Left in place, with the reason recorded at both ends. Its one escape is fixed below. |
| `crates/rue-compiler/src/durable_cfg.rs:688` (`callable_for_symbol`) | Binary search by `live.into_usize()` | **Contained**, same argument. A structural test forbids replacing it with a linear scan, so the sorted vector stays. |
| `crates/rue-compiler/src/durable_cfg.rs:506` (`stable_debug_snapshot`) | Rendered the projection's stable symbols in live-handle order | **Converted.** The snapshot now sorts the projected `StableCfgSymbol`s. Behavior-preserving because the rendered *set* is unchanged and the new order is a total order on durable content; what changes is that the string no longer describes the interner's issue order. This was the only place `symbols`' vector order was observable. |
| `crates/rue-compiler/src/parsed_modules.rs:508` (`impl Ord for RawWarningCallHead`) | Orders declaration-local warning call heads by their path components' handles, for `sort` + `dedup` | **Contained.** `project_warning_call_heads` resolves every component to its spelling and then re-sorts and re-dedups the projection by that text before publishing it, so the raw order decides only which raw heads are adjacent. Two distinct handles never resolve to the same text within one interner, so both dedups agree on the same set. Left in place with the containment argument recorded at the `impl`. |
| `crates/rue-air/src/sema/analysis/type_inference.rs:294` | `string_literal_types` gathered from `generated_structs().values()` — a handle-keyed map | **Contained.** The gathered types are `sort_unstable_by_key(Type::as_u32)`-ed and deduped before use, so the map's iteration order is discarded. Pool indices are body-private and ADR-0076 does not share them. |
| `crates/rue-air/src/sema/provider_body_host.rs:2428` (`anonymous_key_cmp`) | Orders exported anonymous identities | **(a), listed to close it out.** The comparator reads `IssuedAnonymousNominalKey`, whose components are definition tokens and canonical arguments; no handle participates. Its neighbours at `:2488`/`:2504` sort capture lists by resolved `Arc<str>` name, which is already the text ordering this ADR asks for. |
| `crates/rue-air/src/semantic_import.rs:1172`, `:1355`, `crates/rue-air/src/specialize.rs:310` | Sorts of nominals, callables, and selected specializations | **(a).** All key on durable identities (`NominalInstanceKey`, `FunctionInstanceKey`, `SemanticSpecializationIdentity`), never on a handle. |

### (c) Value-bearing uses

| Site | What the value becomes | Classification and action |
| --- | --- | --- |
| `crates/rue-air/src/specialize.rs` `mangle_const_value` | `vfn{index}` / `vstr{index}` fragments of a specialized function's mangled name — which is interned and becomes a **link-time symbol** | **Converted.** Both arms now spell the symbol's text, and `mangle_specialized_name` takes the interner for that purpose. Behavior-preserving because neither arm is reachable: a callable alias is refused as a comptime argument by `validate_comptime_value_for_type_impl`, and no comptime parameter has a string type (RUE-957), which the AIR encoder also asserts. This is the one site where a handle's number could have reached an emitted artifact, and the conversion removes the possibility rather than the (absent) symptom. |
| `crates/rue-air/src/inst.rs:895` (AIR comptime-argument encoder) | One `u32` word per `ConstValue::Function` argument in the AIR payload | **(c), structural; deferred to Phase 2 with its contract stated in source.** The word is legitimate exactly because the body's interner is dense: it is the body RIR's interner, and `materialize_candidate_rir` builds it by interning the packed symbol section in order. The call now reads `SymbolHandle::body_local_ordinal` and says which dense space it means. A shared interner invalidates the premise; see below. |
| `crates/rue-rir/src/inst.rs:2749`, `:3582`, `:3607`, `:4435`, `:4574`, `:4577`, `:4584`, `:4636`, `:4677`, `:4830` and `crates/rue-rir/src/inst/packed.rs:606`, `:653`, `:746`, `:801` | The packed RIR symbol section: a handle **is** its ordinal in a dense per-body table, written as a `u32` word and validated against `symbol_count` | **(c), structural; the blocking Phase 2 item.** Not convertible — the encoding is the dense space. See below. |
| `crates/rue-compiler/src/canonical_lower.rs:386` | Asserts `symbol.into_usize() == ordinal` while rebuilding a body's interner from the packed symbol section | **(c), structural.** This is the invariant the two rows above depend on, stated as a runtime check. It is also the exact assertion a shared interner fails. |
| `crates/rue-compiler/src/revisioned_query_database.rs:6697`, `:6714`, `:6735`, `:6758`, `:6773`, `:6851` | Indexes a `&[&str]` dense candidate symbol view by handle ordinal | **(c), structural**, and the same dense space: `materialize_semantic_candidate_rir` returns the packed symbol section as a `Vec<&str>` whose positions are the ordinals. Safe as long as the packed space stays body-local. |
| `crates/rue-compiler/src/canonical_lower.rs:517` | `"candidate AST references foreign symbol ordinal {n}"` | **Contained.** Internal-error prose on a path that has already failed; not a published diagnostic surface. |
| `crates/rue-parser/src/ast.rs` (38 sites), `crates/rue-lexer/src/lib.rs:333`–`:335`, `crates/rue-air/src/inst.rs:3777`/`:3790`/`:3808`/`:3843`, `crates/rue-cfg/src/inst.rs:3100`/`:3117`/`:3141`, `crates/rue-rir/src/inst.rs:6481`, `crates/rue-codegen/src/cfg_lower.rs:185`/`:199`/`:243` | `sym:{n}` in a `Debug`/`Display` render | **Contained.** Every one is either a `Debug` impl or the no-interner arm of a render whose production caller always supplies an interner — `format_cfg_inst_data_with_interner` is `cfg_lower`'s only caller and passes `Some`. Left as is: they are the diagnostic of last resort for a handle that cannot be resolved, and giving them a text spelling is not possible by definition. |
| `crates/rue-air/src/inst.rs:351` | `"symbol {n} is outside the interner"` | **Contained**, same reason. |
| `crates/rue-compiler/src/parsed_modules.rs:44`, `:3450`, `crates/rue-cfg/src/opt/forward.rs:357`/`:474`/`:514`, `crates/rue-air/src/inst.rs:4184`–`:4186`, `crates/rue-cfg/src/inst.rs:3479`–`:3480` | Test scaffolding | **Contained.** Test-only; they construct or assert on handles in a single interner they own. |

## The mechanical guard

`rue_rir::SymbolHandle` is the equality-only wrapper ADR-0076 requires. It wraps
a `Spur` and deliberately offers less than one: no `Ord`/`PartialOrd`, so it
cannot be a sort key, a `BTreeMap`/`BTreeSet` key, or an operand of a
comparison; and no `lasso::Key` implementation, so `into_usize()` is not in
scope on it. The only way to a handle's number is
`SymbolHandle::body_local_ordinal`, whose name says what a caller has to be able
to justify and whose doc comment says it.

The migrated surface is `ConstValue::Function` and `ConstValue::String`, the two
symbol payloads that carry a handle out of body analysis into the two places
that must not depend on its value — the specialization mangler and the AIR
comptime-argument encoder. Both were live hazards; both are now expressed
through the wrapper, so the mangler *cannot* reach for an index and the encoder
*must* name the dense space it is speaking about. Everything else in the tree
still uses bare `Spur`.

That is a deliberate stopping point, not an accident of effort. A full migration
would touch 1,136 sites across eight crates, and the trait sweep above showed
what it would buy: nothing, because no site orders a handle through `Ord`. The
guard that matters against reintroduction is the one on the *value*, and the
value is what the two migrated payloads carry.

### What is not migrated

- The RIR/packed layer (`rue-rir`), where handles are dense ordinals on purpose.
  Migrating it would mean spelling `body_local_ordinal` at every encode and
  decode, which is noise around a contract that is already this note's headline
  Phase 2 item.
- The body-analysis maps (`AHashMap<Spur, …>` and friends). They are class (a)
  and the wrapper adds nothing they do not already have.
- `rue-parser` and `rue-lexer`, whose interners are per-file parse interners
  that ADR-0076 does not share.

## What Phase 2 still owes

Flagged for the consensus round: ADR-0076's implementation shape does not
mention any of this, and Phase 2 cannot land without it.

**The body's interner is a dense table and the compiler depends on it.**
`materialize_candidate_rir_internal` builds each body's `ThreadedRodeo` by
interning the packed symbol section in order and *fails the body* unless every
handle's ordinal equals its index in that section
(`crates/rue-compiler/src/canonical_lower.rs:386`). The packed RIR encodes every
symbol as that ordinal in a `u32` word and validates each against
`symbol_count`; the semantic candidate path hands the same section out as a
`Vec<&str>` indexed by ordinal; the AIR comptime-argument encoder writes the same
ordinal. A revision-shared interner assigns a body's symbols sparse handles out
of a revision-wide space, so the invariant fails on the first body and the
encodings become meaningless. This is not an ordering that can be converted to
text — the ordinal *is* the encoding.

The shape of the fix already exists in the tree: `PackedValidatedRir` decodes
through `remap_symbol: impl FnMut(u32) -> Result<Spur, E>` and `AstGen` takes a
`normalize_symbol: Fn(Spur) -> Spur`, so a body-local dense table alongside the
shared interner is a remap away rather than a rewrite. But ADR-0076 currently
reads as though the shared interner replaces the body's interner one-for-one
behind `body_symbol_interner()`, and it cannot: a body needs *both* a shared
equality space and a private dense space, and the ADR should say which artifact
speaks which.

**`require_rir_authority` holds an `Rc`, not an `Arc`.** ADR-0076 §5 says the
assertion "becomes `Arc::ptr_eq` against the revision's interner". Today it is
`std::ptr::eq(rir.rir_interner(), Rc::as_ref(&state_interner))` over
`Rc<ThreadedRodeo>` (`crates/rue-air/src/sema/body_identity.rs:794`), and the
interner is `Rc`-held throughout `body_identity.rs` and `provider_body_host.rs`.
A revision-shared interner has to cross worker threads, so Phase 2 includes an
`Rc` → `Arc` conversion of that whole ownership chain. It is mechanical, but it
is not free and the ADR does not budget for it.

**Two dead mangler arms are load-bearing for the byte-identity gate.** The
specialization mangler's symbol-valued arms are unreachable, which is why
converting them cost nothing. If a later change makes a callable alias or a
string a legal comptime argument, the conversion in this note is what keeps the
resulting link symbol independent of intern order — and the resulting mangled
names will differ from what the pre-conversion code would have produced. That is
the intended behavior, not a regression to investigate.

## Byte identity

Release build (`./buck2 build //crates/rue:rue --target-platforms
//platforms:release`), `RUE_STD_PATH` at the repository `std`, three shapes,
`-j1` and `-j4`, two runs of each tree.

Emitted executables are byte-identical before and after, on all three shapes at
both worker counts:

| shape | executable |
| --- | --- |
| Lattice (`performance/workloads/lattice/main.rue`) | `45784ce7c7cde992d7ea820912ca1692c05dc7582a367210d017b3765a9a89e7` |
| Mosaic (`examples/mosaic/main.rue`) | `4cdead1a98e77fce940b5d6f9b693d8decba26b1c666896bcc78d48a1b79f97b` |
| chain256 (generated, 256 single-call modules) | `32e711f148c77d8dc798b905ae7ea1c80701595527f150540ad781e929a198b6` |

`compiler_work`, `source_metrics`, `emitted_output`, and `compiler_boundary`
compared field by field:

| shape | fields | `-j1` differences | `-j4` differences | `-j4` fields excluded as scheduling-inherent |
| --- | ---: | ---: | ---: | ---: |
| Lattice | 658 | 0 | 0 | 18 |
| Mosaic | 424 | 0 | 0 | 19 |
| chain256 | 946 | 0 | 0 | 17 |

At `-j1` every field is reproducible and every field matches. At `-j4` a small
set of `query_runtime` and `query_engine` scheduling counters (the excluded
count is empirical, and drifts by a few fields between samples) (`reuses`,
`demands`, `endorsement_probes`, `joins`, …) varies between repeated runs of the
*same* tree; those are excluded by comparing two runs of each tree and masking
any field that already disagrees with itself. No field outside that mask
differs. The exclusion is a property of the query engine's work-stealing, not of
this change: the same fields vary run to run on the parent commit.

## Suites

`//crates/rue-air:rue-air-test`, `//crates/rue-rir:rue-rir-test`,
`//crates/rue-compiler:rue-compiler-test`, `//crates/rue:rue-driver-test`,
`scripts/rue quick` (30 targets), `scripts/rue ui` (250 cases), and
`scripts/rue cli` all pass, with `scripts/rue fmt` applied and the clippy gate
clean.
