---
id: 0049
title: Function Inlining
status: accepted
tags: [compiler, codegen, optimization]
feature-flag: none
created: 2026-07-15
accepted:
implemented:
spec-sections: []
superseded-by:
---

<!-- Note: Inlining is a compiler-internal optimization, not a language feature.
     It never changes what a program means, only how the emitted code is shaped
     (see ADR-0044's observable-behavior invariant). No preview gate applies. -->

# ADR-0049: Function Inlining

## Status

Accepted on 2026-07-16, closing the RUE-915 design issue and filing the phase
issues below. Phase 1 shipped in July 2026: the CFG splice primitive landed
with RUE-929 (`crates/rue-cfg/src/inline.rs`) and is consumed today by the
compiler's mandatory accessor splices
(`crates/rue-compiler/src/cfg_query.rs`). RUE-930's conservative whole-program
`-O2`/`-O3` free-function inlining batch is implemented in
`crates/rue-compiler/src/cfg_query.rs`; this implementation also completes the
accepted Phase 5 reachability step in that same batch. Phase 3/4 and the
method/destructor extension remain tracked separately.

This note analyzes the architecture of function inlining and records the
accepted direction. ADR-0044 already fixed the *level*
placement — conservative small/leaf-function inlining at `-O2`, aggressive
thresholds at `-O3` — so this note answers the *architecture* questions that
sit under that placement: where inlining runs, what it does mechanically to the
CFG IR, how it integrates with the **already-landed durable CFG cache**, how it
handles **parameter ownership and drop**, and how it stays inside the
observable-behavior invariant. Since the durable CFG cache (`DurableCfgArtifact`)
and the parameter-mode physical ABI both landed since the first draft, **durable
caching and parameter ownership are core design constraints here, not deferred
implementation details**. Tracks RUE-915.

## Summary

Inlining replaces a `Call` to a suitable callee with a copy of the callee's body
spliced into the caller, renumbered onto the caller's frame and value space, so
that intra-procedural passes (constant folding/propagation, peephole, CFG
simplification, value forwarding, CSE, DCE, and the future loop passes) can then
see across the former call boundary. The central tension
is that Rue's optimizer is today strictly **per-function**: `build_functions_and_cfgs`
builds and optimizes each function's CFG independently and in parallel
(`crates/rue-compiler/src/queries.rs:198`, `into_par_iter().map(...)`), whereas
inlining is inherently **cross-function** — it needs the callee's body while
optimizing the caller. This note recommends running inlining as a **dedicated
inter-procedural stage orchestrated by `rue-compiler`, operating on already-built
callee CFGs**, sequenced between CFG construction and the existing per-function
`opt::optimize` pass. That choice keeps `rue-cfg`'s passes purely local and
reuses the CFG the builder already produces.

Two consequences are load-bearing and are given their own decision sections:

- **Durable cache integration is not optional and cannot be deferred.** Rue
  already retains, imports, and exports optimized `DurableCfgArtifact`s keyed by
  optimization level and target (`crates/rue-compiler/src/queries.rs:137`,
  `217-232`, `330-343`). Inlining changes *what* a cached caller artifact
  contains (foreign callee domains) and *what invalidates it* (callee body
  edits). The ADR decides now: cached CFGs are **final optimized, post-inlining**
  CFGs; caller cache keys must incorporate **callee body fingerprints**; and the
  caller-only domain projection (`CfgDomainProjection::from_body`) must be
  extended to carry **foreign callee domains** (§4).
- **The inliner's call-site graph is a separate structure from the semantic
  dependency manifest** (§5). Call-site multiplicity, leafness, candidate
  lookup, and recursion SCCs are answered by **scanning CFG `Call`
  instructions**; the deduplicated `body_dependencies` manifest
  (`crates/rue-compiler/src/session.rs:596`, `619`) is used for invalidation and
  durable reuse. They answer different questions and must not be conflated.

## Context

### The optimizer is per-function today (ground truth)

The optimizer lives entirely in `crates/rue-cfg/src/opt/` and runs as CFG → CFG
transforms on **one function at a time**. `opt::optimize(cfg, level, type_pool)`
(`crates/rue-cfg/src/opt/mod.rs:142`) takes a single `&mut Cfg` and has no access
to any other function. A `Cfg` is one function: it owns that function's
`blocks`, `values`, `extra`, `call_args`, `num_locals`, `num_params`, `fn_name`,
`param_modes`, and `address_taken_slots` (`crates/rue-cfg/src/inst.rs:536`).

The production build seam is `build_functions_and_cfgs`
(`crates/rue-compiler/src/queries.rs:156`). It:

1. synthesizes drop-glue functions (`drop_glue::synthesize_drop_glue`, line 174);
2. concatenates user functions + glue and **sorts them by machine symbol name**
   (`all_functions.sort_by(|left, right| left.name.cmp(&right.name))`, line 194) —
   the machine symbol is the stable semantic identity shared by user, specialized,
   destructor, and glue functions;
3. maps each function through an **import-or-build-then-optimize** step in a
   Rayon parallel map (`into_par_iter().map(...)`, lines 198–360): it tries to
   reuse a cached `DurableCfgArtifact` (below), else runs `CfgBuilder::build`
   (line 255) then `opt::optimize` (line 282), then exports the optimized CFG as
   a fresh artifact (line 330). **No cross-function state is threaded through the
   map** — each function is built and optimized in isolation.

A `Call` names its callee purely by interned symbol (`CfgInstData::Call { name:
Spur, .. }`, `crates/rue-cfg/src/inst.rs:351`; lowered at
`crates/rue-cfg/src/build.rs:875`). That symbol is exactly the key the function
list is sorted by, so "find the callee body for this call" is a symbol lookup
over the already-built function set — cheap to express, but it requires a stage
that can see the *whole set*, which the current per-function parallel map
deliberately cannot.

### The durable CFG cache has landed (ground truth — supersedes the first draft)

The first draft asserted "there is no CFG cache to invalidate today — every build
rebuilds every CFG from AIR." **That is no longer true.** A durable, fail-closed
CFG cache is now in production:

- **Retention.** `CfgFrontendOutput` carries
  `durable_cfgs: Arc<[DurableCfgArtifact]>` (`crates/rue-compiler/src/queries.rs:129`),
  retained across semantic requests on the session as `last_successful_cfg_cache`
  / `successful_cfg_cache` (`crates/rue-compiler/src/session.rs:1676`, `2243`) and
  threaded back in as `durable_cfg_candidates`
  (`crates/rue-compiler/src/canonical_semantic.rs:613`, `688`, `857`).
- **Import (reuse).** `build_functions_and_cfgs` takes
  `durable_candidates: &[DurableCfgArtifact]` and
  `stable_inputs: &[StableCfgInput]` (`queries.rs:161-162`). Per function it
  binary-searches a candidate by machine symbol (`queries.rs:210`) and reuses it
  only when **every** key component matches:
  `candidate.schema_version == DURABLE_CFG_SCHEMA_VERSION && candidate.opt_level
  == opt_level && candidate.target == target` and the stable input agrees
  (`input.body == candidate.input.body && input.specialization ==
  candidate.input.specialization && input.type_inputs ==
  candidate.input.type_inputs`), after which the stored CFG is re-projected into
  the current domains via `CfgDomainProjection::import_cfg` (`queries.rs:217-232`).
  Any mismatch falls back to a full build.
- **Export.** After `opt::optimize` runs (`queries.rs:282`), the **optimized**
  CFG is exported as a new `DurableCfgArtifact` — gated on the function producing
  no warnings, no incomplete implicit-destructor edges, and a successful
  `domains.validate_cfg(&cfg, input.body_span)` round-trip (`queries.rs:330-343`).
- **Cache key contents.** `DurableCfgArtifact` (`queries.rs:137`) is
  `{schema_version, input: StableCfgInput, opt_level, target, cfg, domains}`. The
  key material is `schema_version`, `opt_level`, `target`, and `StableCfgInput`
  = `{identity, body_span, body: DurableOrdinaryBodyPayload, specialization,
  type_inputs}` (`crates/rue-compiler/src/durable_cfg.rs:11-17`). **Crucially,
  `StableCfgInput` fingerprints only the caller's *own* body** — it has no field
  for the bodies of callees the caller calls.

The consequence for inlining is the opposite of the first draft's: the cache is
real, the exported CFGs are **already the final optimized CFGs**, and inlining
must integrate with retention/import/export from day one (§4). A caller that
inlines callee `f` (a) contains `f`'s domains inside its CFG, which the current
caller-only projection cannot express, and (b) must be invalidated when `f`'s
**body** changes, which the current caller-only key cannot detect.

### `CfgDomainProjection::from_body` is caller-only by contract (ground truth)

`CfgDomainProjection::from_body` (`crates/rue-compiler/src/durable_cfg.rs:223`)
builds the type/string/symbol/span remapping tables that make a cached CFG
portable across compilation sessions. It derives that mapping **solely from the
caller's AIR body**: it zips `function.air.iter()` against
`input.body.instructions` one-for-one (`durable_cfg.rs:235`), and it **rejects
any instruction whose span falls outside the caller body's span range** —
`current.span.file_id != input.body_span.file_id || current.span.start <
input.body_span.start || current.span.end > input.body_span.end` returns
`CfgDomainFailure::Unsupported` (`durable_cfg.rs:237-242`). The symbol table maps
only `Call`/`Intrinsic` names that appear in the caller's own AIR
(`durable_cfg.rs:268-278`); the string table maps only string constants in the
caller's AIR (`durable_cfg.rs:251-260`).

An inlined callee **introduces foreign domains by definition**: types, string
constants, callee `Call`/`Intrinsic` symbols, and — fatally for the span check —
instruction spans that point into the *callee's* source range, not the caller's.
The existing export boundary therefore cannot treat an inlined CFG as an ordinary
caller artifact: `from_body` would reject it on the span range check, so today
inlining would simply make every caller *uncacheable*. §4 designs the fix.

### The semantic dependency manifest (ADR-0050) — what it is and is not

> **Historical architecture:** ADR-0063 superseded ADR-0050 and removed the
> whole-program manifest and `semantic_invalidation_plan`. An inlining
> implementation must express these dependencies through the canonical per-key
> body-reference/query graph. The discussion below explains the distinction
> that motivated this design; its named APIs and source locations are retired.

The dependency-derived invalidation machinery is **ADR-0050 (Stable semantic
dependency manifests)**, with session support in
`crates/rue-compiler/src/session.rs`:

- `CompilerSession::semantic_dependency_inputs`
  (`crates/rue-compiler/src/session.rs:5337`) publishes an ordered definition
  universe whose per-owner body dependencies are
  `StableBodyDependencyInputRecord`s (`session.rs:596`). Each record's
  `direct_dependency_inputs: Arc<[StableDefinitionInputFingerprint]>`
  (`session.rs:601`, `619`) is the owner's dependency set — **sorted and
  deduplicated** (`direct_dependencies.sort(); direct_dependencies.dedup();`,
  `session.rs:6018-6019`) — merging call targets, named-method/destructor edges,
  and declaration-type edges into one flat set of *definition* fingerprints.
- `CompilerSession::semantic_invalidation_plan` (`session.rs:6373`) memoizes a
  comparison of two manifests and computes a deterministic **reverse-dependency
  closure** over those direct edges (`collect_reverse_dependencies`,
  `session.rs:6561`; `reverse_closure_nodes_visited`, `session.rs:1455`), failing
  closed (`IncompleteDependencyGraph`) when any endpoint is missing.

The manifest is a **deduplicated definition-dependency graph**, not a call-site
multigraph. It records *that* a caller depends on a callee's definition; it
**cannot distinguish one call from ten**, and it folds calls together with type
and destructor dependencies. This shape is exactly right for invalidation and
durable reuse — a callee body edit must invalidate every caller that depends on
it, regardless of call count — and exactly wrong for the inliner's structural
questions (call multiplicity, leafness, recursion cycles). §5 makes the
separation an explicit decision.

The body-fingerprint partition ADR-0050 already draws (declaration/signature
fingerprints vs body fingerprints) is what inlining's cache key needs: a caller
that inlines callee `f` must be rebuilt when `f`'s **body** fingerprint changes,
not merely its signature (§4b).

### Runtime traps carry no source location today (ground truth)

The brief asks how spans flow to runtime errors so an inlined callee's trap
still reports sensibly. The verified answer is that **Rue's runtime traps carry
no source location at all**. Overflow, divide-by-zero, and bounds traps lower to
calls to fixed runtime helpers whose symbols are the constants
`__rue_overflow` / `__rue_bounds_check` (and `__rue_intcast_overflow`) held in
`RUNTIME_TRAP_SYMBOLS` (`crates/rue-codegen/src/allocation.rs:168-172`), passed
with no span/line/column argument (`emit_overflow_check` /
`emit_signed_div_overflow_check` at `crates/rue-codegen/src/x86_64/cfg_lower.rs:244`,
`emit_bounds_trap` at line 2838, each taking only the trap symbol string). The
runtime prints a fixed message ("integer overflow", "division by zero", "index
out of bounds") and exits 101; tests assert only the message string
(`crates/rue-cli-tests/cases/opt.toml`, `runtime_error_contains`). Spans
(`CfgInst.span`, `crates/rue-cfg/src/inst.rs:215`) drive **compile-time
diagnostics**, which run during semantic analysis and CFG construction — *before*
any inlining would run. This reframes the trap-span obligation (see Decision §7).

## Decision

### 1. Where inlining runs — a dedicated inter-procedural stage in `rue-compiler`

Three options were considered.

**(a) AIR-level inlining, before CFG construction.** Splice callee AIR into
caller AIR, then build one CFG. *Cost:* AIR is the typed, structured,
pre-control-flow IR; inlining there means re-implementing parameter binding,
scope/drop insertion, and move analysis at the AIR level, duplicating what
`CfgBuilder` already does (drop elaboration, `StorageLive`/`StorageDead`
pairing, move pre-scan — `crates/rue-cfg/src/build.rs:292, 1124, 2495`). It also
fights the durable-cache grain: `StableCfgInput` fingerprints an AIR body and
`from_body` anchors its projection at AIR body boundaries; splicing AIR bodies
would disturb those anchors and the cache key. Rejected as the most invasive and
the worst fit for the incremental model.

**(b) CFG-level inlining as a `rue-cfg` pass with access to other CFGs.** Add a
pass inside `opt::optimize` that reaches other functions' CFGs. *Cost:* this
breaks the defining property of `rue-cfg`'s optimizer — that a pass sees exactly
one function — and forces `opt::optimize`'s signature to take the whole function
set, which then has to be threaded through the Rayon per-function map in
`queries.rs:198`. It puts cross-function orchestration (callee lookup, recursion
refusal, threshold policy, cache-key composition) inside `rue-cfg`, which has no
view of the session, the manifest, or symbol resolution. Rejected:
it pushes orchestration to the wrong layer.

**(c) A dedicated inter-procedural stage orchestrated by `rue-compiler`,
operating on built CFGs — RECOMMENDED.** Keep `CfgBuilder::build` producing
per-function CFGs exactly as today. Restructure the map in
`build_functions_and_cfgs` (`crates/rue-compiler/src/queries.rs`) so that CFG
**construction** (import-or-build) runs first for all functions, then a new
inliner stage runs with the full `{symbol → Cfg}` map, then the existing
per-function `opt::optimize` + export runs. The inliner itself is a self-contained
CFG→CFG *splice* primitive that can live in `rue-cfg` (it is pure CFG surgery and
belongs with the IR it edits), but it is *driven* by `rue-compiler`, which owns
the callee map, the call-site graph, and the manifest.

Reasoning:

- It preserves the invariant that every `rue-cfg` *optimization* pass is
  strictly local; the only cross-function code is the orchestration, which lives
  where the whole-program view already lives (the session).
- It reuses the CFGs the builder already produces — no second lowering, no
  AIR-level re-implementation.
- The one real cost it pays honestly: the per-function step can no longer *also*
  optimize in the same parallel pass, because inlining needs all callee bodies to
  exist before any caller is rewritten. Construction and optimization become two
  phases (build/import-all, then inline+optimize+export) rather than one fused
  parallel map. Optimization remains embarrassingly parallel *after* the inliner
  has run (the splice is a batch step; per-caller `optimize` is still
  independent). At `-O0`/`-O1` the inliner is skipped and the fused single-phase
  map is retained.
- Interaction with the cache import path: today import, build, optimize, and
  export all happen inside the one per-function closure (`queries.rs:206-343`).
  Two-phasing means **import/build** populates the callee map first; the inliner
  rewrites callers; then **optimize+export** runs. A caller reused verbatim from
  the cache (import success) still enters the inliner stage, because its cached
  form is *already post-inlining* (§4a) — so a reused caller needs no
  re-splice, and the inliner skips it. Whether that reuse is still valid is a
  pure cache-key question (§4b).

### 2. What inlining a call means mechanically in this IR

Inlining one `Call` site in caller `C` to callee `E` (`E`'s `Cfg` already built)
is a splice with the following moving parts. All of them are index-space
rebasing, because the CFG uses compact index references throughout (AGENTS.md:
"IR instructions and entities use compact index-based references. Preserve index
validity across transforms").

- **Value-id offsetting.** `E`'s `values: Vec<CfgInst>` are appended to `C`'s
  `values`; every `CfgValue` operand inside the copied instructions is shifted by
  `C.values.len()` before the append. `CfgValue` is a newtype index
  (`crates/rue-cfg/src/inst.rs`), so this is a uniform add.
- **Block-id offsetting.** `E`'s `blocks` are appended to `C`'s `blocks`; every
  `BlockId` in copied terminators (and in `BlockParam` references) is shifted by
  `C.blocks.len()`. The `extra`, `call_args`, `switch_cases`, and `projections`
  side-arrays each get their contents appended and every `(start, len)` index in
  the copied instructions/terminators re-based onto the new lengths.
- **Slot renumbering.** `E`'s local slots (`0..E.num_locals`) must be rebased
  onto `C`'s frame: add `C.num_locals`, then `C.num_locals += E.num_locals`.
  Every `Alloc/Load/Store { slot }`, `StorageLive/StorageDead { slot }`, and
  `Place` root that names a local slot is shifted. `E`'s `address_taken_slots`
  are unioned (shifted) into `C`'s — losing this reintroduces the RUE-521 O1+
  segfault the field exists to prevent (`crates/rue-cfg/src/inst.rs:576` doc). A
  `Cfg` now *also* tracks address-taken **parameters** separately from locals
  (`address_taken_params`, `crates/rue-cfg/src/inst.rs:589`, set via
  `mark_param_address_taken`, `crates/rue-cfg/src/inst.rs:691`, queried by
  `is_param_address_taken`, `:697`): a by-value parameter whose address escapes
  through `@raw`/`@raw_mut`/`@field_ptr` is not purity-safe. When a by-value
  argument is materialized into a fresh caller local (below), any such escape
  fact on `E`'s parameter must carry onto that new slot's `address_taken_slots`
  entry, so the splice must consult `is_param_address_taken` while lowering each
  parameter.
- **Parameter passing — consult the physical ABI, not the logical mode.** The
  splice must redirect `E`'s parameter accesses to the caller's arguments, and
  the correct discriminator is `E`'s **physical** parameter convention, queried
  by `Cfg::is_param_by_ref` / `is_param_writable`
  (`crates/rue-cfg/src/inst.rs:763, 773`), **not** the logical
  `Normal`/`Inout`/`Borrow` source mode. The two can diverge: a slice `borrow
  s: [T]` is a two-word fat pointer passed **by value** through the multi-slot
  aggregate ABI, so `is_param_by_ref` is *false* for it even though the source
  mode is `Borrow` (`is_slice_by_value` in
  `crates/rue-air/src/sema/ordinary_engine.rs`). Splicing off the logical mode
  would misclassify these.
  - *Physically by-value (`is_param_by_ref == false`):* materialize each argument
    into a fresh caller local (`Alloc` + `StorageLive`), sized at the parameter's
    **full ABI slot width** — an aggregate/slice occupies
    `require_layout_slots` slots (`crates/rue-air/src/sema/typeck.rs`), and the materialization
    must reserve every slot, not one. Rewrite `E`'s `Param { index }` reads to a
    `Load` of that slot. Ownership/drop of these materialized slots is §3.
  - *Physically by-reference (`is_param_by_ref == true`):* the call argument is a
    **place** — non-place by-ref arguments are rejected before codegen (RUE-760,
    `crates/rue-air/src/sema/analysis.rs:2219`; address formation in
    `crates/rue-codegen/src/byref_args.rs:21`). The splice redirects `E`'s by-ref
    parameter accesses (and its `ParamStore { param_slot, value }` write-backs,
    lowered at `crates/rue-cfg/src/build.rs:832`) to that caller place, using
    `is_param_writable` to decide read-only vs writable. **Restriction (see §3
    point 4): the initial inliner accepts only by-ref arguments whose place is a
    simple local or parameter root, and excludes projected by-ref arguments** —
    direct substitution of a projected place needs a proof that address-formation
    timing and trap behavior are preserved, which is deferred.
- **Return-value wiring via block params.** `E`'s `Return { value }` terminators
  (`crates/rue-cfg/src/inst.rs:498`) cannot remain returns inside `C`. The call
  site's block is split: instructions after the call move to a fresh
  continuation block that takes a single `BlockParam` of the call's result type;
  each copied `Return { value: Some(v) }` becomes `Goto(continuation, [v])`, and
  `Return { value: None }` becomes `Goto(continuation, [])`. This reuses the
  explicit block-parameter mechanism the CFG already uses for phi-like joins
  (`BlockParam`, `crates/rue-cfg/src/inst.rs:278`), so no new IR concept is
  needed. `Unreachable`/`-> !` callees (the RUE-347 path,
  `crates/rue-cfg/src/build.rs:883`) inline with no continuation edge.

### 3. Parameter ownership and drop — a core section

Drop glue is synthesized as *separate functions* (`synthesize_drop_glue`,
`crates/rue-compiler/src/drop_glue.rs:94`) and invoked from the CFG builder's
scope-exit elaboration, which emits `Drop` then `StorageDead` per slot in
reverse order (`crates/rue-cfg/src/build.rs:1124, 2495`). Because drop
elaboration already ran when `E`'s CFG was built, `E`'s body **already contains**
its `StorageLive`/`StorageDead`/`Drop` instructions for its own locals. Inlining
therefore must **not** re-elaborate drops; it copies `E`'s already-elaborated
drop instructions verbatim (slots rebased). Inlining is strictly *after* drop
elaboration and strictly *before* MIR lowering, so it never re-runs elaboration
and never sees un-elaborated drops.

The subtle part is the **materialized by-value parameter**. The callee owns its
by-value (`Normal`) parameters and drops them at exit unless they are moved out
(RUE-61); `param_drops` records this obligation (set in
`crates/rue-air/src/sema/ordinary_engine.rs`, consulted by the CFG builder
in `crates/rue-cfg/src/build.rs`). When the splice materializes a by-value
argument into a fresh caller slot, that slot **is** the callee's parameter
storage for the inlined body's lifetime. The correct rule is therefore **not**
ordinary caller-scope drop treatment (the first draft's error), but:

1. Emit an explicit **callee-lifetime `StorageLive`** for the materialized slot
   immediately before the spliced body and an explicit **`StorageDead`** after
   the continuation join, so the slot's live range is exactly the inlined body's
   extent — matching the storage lifetime the callee's own frame would have given
   the parameter.
2. Reproduce the callee's **copied parameter-drop behavior**: the drop
   obligation the callee had for that parameter (its `param_drops` entry) is
   discharged by `E`'s already-elaborated `Drop`/`StorageDead` on the parameter
   slot — which the splice copies — retargeted to the materialized slot. The
   splice must **not** additionally synthesize a caller-scope drop for the same
   slot, or the value is dropped twice; and it must **not** drop the caller's
   original argument value again, because move semantics already transferred
   ownership into the materialized slot.

The net rule: **one materialized slot, one live range bounded by the inlined
body, one drop discharged by the callee's own copied drop instructions.** This
matrix (moved-out parameter, dropped-at-exit parameter, `Copy` parameter needing
no drop) is the highest-risk correctness surface and needs a worked elaboration
example plus differential coverage before Phase 1.

### 4. Durable CFG cache integration — a core section

Inlining touches all three cache operations. The decisions:

**(a) Cached CFGs represent FINAL optimized, post-inlining CFGs.** The export
point is already *after* `opt::optimize` (`queries.rs:282, 330`), so cached
artifacts are final optimized CFGs today. With inlining sequenced *before*
`opt::optimize` (§1c), the exported CFG is naturally **post-inlining and
post-optimization** — the single fully-transformed artifact. This is the right
choice: it keeps one artifact per function with no "half-optimized, pre-inline"
intermediate to store, version, or invalidate separately, and it means an import
hit reproduces the complete transform for free. The cost is the invalidation
obligation in (b): a post-inlining artifact depends on its inlined callees'
bodies, so its key must say so. *Rejected alternative:* caching pre-inlining CFGs
and re-inlining on every build would make a caller's cache hit useless (the
expensive cross-function work reruns every time) and would still need the
callee-body key for the inline decision itself — strictly worse.

**(b) Caller cache keys incorporate callee body fingerprints and inlining
policy.** `StableCfgInput` (`durable_cfg.rs:11`) today fingerprints only the
caller's own `body`, `specialization`, and `type_inputs`, and the reuse check
(`queries.rs:220-224`) compares exactly those. A post-inlining caller artifact is
invalidated by two more things:
1. **Every inlined callee's body fingerprint.** The key must gain a set of
   `{callee identity → StableDefinitionInputFingerprint}` for each callee the
   caller actually inlined, drawn from the ADR-0050 body-fingerprint partition
   (`session.rs`, `StableDefinitionInputFingerprint`). Editing an inlined
   callee's body changes its body fingerprint, which changes the caller's key,
   which forces a rebuild — exactly the reverse-dependency invalidation
   `semantic_invalidation_plan` already computes (`session.rs:6373`), now keyed on
   *body* rather than *signature*.
2. **The inlining policy revision.** A change to the threshold table or inlining
   algorithm must invalidate every inlined caller. Bump a coarse
   `inlining_policy_version` (companion to `DURABLE_CFG_SCHEMA_VERSION`,
   `queries.rs:132`) into the key; the simplest correct form folds it into the
   schema version so a policy change invalidates the whole CFG cache. A finer
   per-caller policy fingerprint is a later refinement.

Only callees actually inlined into *this* caller enter its key — not the caller's
whole dependency set — so a caller that inlined nothing keeps today's key
unchanged and its cache behavior is untouched.

**(c) Durable domain projection must carry foreign callee domains.** This is the
sharpest constraint. `from_body`'s caller-only contract (Context) rejects any
CFG containing spans, symbols, strings, or types that do not originate in the
caller's AIR body — which every inlined CFG does. The design:

- During the splice, the inliner already walks each copied callee instruction to
  rebase indices. At the same time it **accumulates a foreign-domain remapping
  table**: for each callee type, string constant, `Call`/`Intrinsic` symbol, and
  span that lands in the caller CFG, record the callee-side stable identity the
  callee's *own* `CfgDomainProjection` already holds (each callee has one built
  by `from_body` from its own body). The callee's projection is the authoritative
  source of stable identities for its domains.
- Extend `CfgDomainProjection` from a single caller-body projection to a
  **caller projection unioned with the projections of every inlined callee**,
  with callee spans anchored relative to the *callee's* `body_span` (the
  `DurableBodyAnchor` mechanism `from_body` already uses,
  `durable_cfg.rs:243-249`) plus a callee-identity tag so import can resolve each
  foreign span against the right body. Concretely, the span check at
  `durable_cfg.rs:237-242` must change from "reject spans outside the caller
  body" to "resolve each span against the caller body *or* a recorded inlined
  callee body," and `import_cfg`'s span/symbol/string/type remappers
  (`durable_cfg.rs:351-406`, driving `Cfg::try_remap_domains`,
  `crates/rue-cfg/src/inst.rs:597`) must consult the unioned table.
- `import_cfg` already remaps whole-CFG domains through `try_remap_domains`
  (type/struct/enum/symbol/string/span), so the *mechanism* to re-project a
  multi-body CFG exists; what must change is the *table construction*
  (`from_body`) and the *span-range contract*. Until that lands, the safe
  fallback is explicit and cheap: a caller that inlined anything **fails the
  export gate** (`from_body`/`validate_cfg` returns `Unsupported`) and is simply
  not cached — correct, just slower, and identical to today's behavior for any
  function that fails validation. Phase 4 turns caching on for inlined callers.

**Sequencing note:** because import happens per-function (`queries.rs:214-249`), a
cached post-inlining caller can be imported *without* rebuilding its callees — the
stored CFG already contains the inlined bodies. The callee-body fingerprints in
the key (4b) are what make that reuse sound; the multi-body projection (4c) is
what makes it *expressible*.

### 5. Call-site graph vs semantic dependency manifest — an explicit separation

The inliner needs a **call-site graph** and must build it by **scanning the CFG's
`Call` instructions**, not by reading the ADR-0050 `body_dependencies` manifest.
These are different structures answering different questions:

- **Call-site multiplicity** ("is this callee called once or ten times?"): count
  `CfgInstData::Call` instructions naming a symbol across all caller CFGs. The
  manifest **cannot** answer this — `direct_dependency_inputs` is deduplicated
  (`session.rs:6018-6019`), so ten calls to `f` and one call to `f` produce the
  identical single edge.
- **Leafness** ("does this callee contain any call?"): scan the callee's
  `values` for `CfgInstData::Call`. The manifest folds calls together with type
  and destructor dependencies, so it cannot cleanly report "contains a call."
- **Candidate lookup** ("what is the callee body for this call?"): the `Call`'s
  `name: Spur` is the symbol key into the `{symbol → Cfg}` map. This is
  CFG-native.
- **Recursion / SCC refusal**: the recursion cycle must be computed over the
  **actual call-site graph**, not the dependency manifest. Using the whole
  deduplicated dependency graph for recursion analysis **can produce false
  cycles**: because it merges calls with type and destructor edges, a
  non-recursive function that merely *mentions a type* whose destructor
  transitively references the function's own definition would appear in a cycle
  and be needlessly refused. The call-site graph contains only real call edges,
  so its SCCs are exactly the real recursion cycles.

Conversely, the manifest — not the CFG scan — remains authoritative for
**invalidation and durable reuse**: the reverse-dependency closure
(`semantic_invalidation_plan`, `session.rs:6373`) and the caller cache key (§4b)
consume `body_dependencies`. **Scanning the CFG is not a competing invalidation
path**; it answers the inliner's structural questions, while the manifest answers
"what must rebuild when a definition changes." Both exist; neither substitutes
for the other.

### 6. Threshold and candidate policy

Initial, deliberately conservative criteria (concrete numbers are the maintainer
knob to set; these are the proposed starting points):

- **Callee instruction count** below a small budget. The counter is exact and
  free: `callee.values.len()`. Proposed initial cap: small (≈ a few dozen
  instructions) at `-O2`; larger at `-O3` per ADR-0044.
- **Leaf-ness** from a CFG scan (§5): a callee containing no `CfgInstData::Call`
  is a leaf and the safest first target. `-O2` = leaf callees under the small
  cap; `-O3` relaxes to non-leaf and larger caps.
- **Single-call-site** callees (call multiplicity 1, from the §5 call-site graph)
  are the highest-value case: inlining them removes a call with no code-size cost
  at the inlined site. **But the callee is not deleted by the inliner.** Whether a
  now-uncalled function can be removed is a **whole-program dead-function-
  elimination decision** made with full reachability knowledge (address-taken
  functions, exported symbols, call sites in other translation units), not a
  local inliner side effect. Deleting a function because *this* inliner consumed
  its only visible call site would be unsound in the presence of any other
  reference. The inliner inlines; DFE (a separate, later pass over the whole
  function set) decides removal.
- **Bounded code growth.** Inlining and `-O3` unrolling (ADR-0054) share one
  per-function code-growth budget debited by whichever runs, so a function does
  not both inline heavily and unroll into a size explosion. The budget source is
  the same `values`-based instruction count.
- **Counters' provenance.** Instruction counts and call-site multiplicity and
  leafness all come from CFG scans (§5); no new counting infrastructure and no
  manifest read are required for the inline *decision*.

**Recursion — must refuse.** Direct recursion is refused by checking callee
symbol == caller symbol. Mutual recursion is refused by detecting a
strongly-connected component of size > 1 in the **call-site graph** (§5).
Refusing on the SCC is strictly sound and avoids unbounded expansion; a later
relaxation (bounded-depth recursive inlining under a size budget) is out of
scope.

**Exclusions.** `@intrinsic` calls (`CfgInstData::Intrinsic`,
`crates/rue-cfg/src/inst.rs:362`) are never inlining candidates — they have no
Rue-level body. Runtime-helper calls (`__rue_overflow`, `__rue_bounds_check`,
and friends, emitted only in codegen) never appear as `CfgInstData::Call` in the
CFG, so they are excluded by construction. Drop-glue functions *are* ordinary
functions and *could* be inlined, but are excluded initially: they are
generated, often recursive over aggregate structure, and their call sites are
already synthesized.

### 7. Safety and semantics

- **The observable-behavior invariant (ADR-0044).** `-O2` already carries
  content distinct from `-O1` (value forwarding and CSE, RUE-914/913), so the
  invariant is already exercised beyond `-O0` vs `-O1`; inlining is the first
  *cross-function* transform to do so. Per ADR-0044, every pass that populates
  `-O2`/`-O3` must (a) place itself at the assigned level, (b) add multi-case
  differential CLI coverage for the shapes it rewrites, and (c) keep the full
  differential set green (`differential_opt = true`, `run_case_differential`,
  RUE-236, `crates/rue-cli-tests/src/main.rs:802`). Inlining's differential
  obligations specifically include: callees that trap (overflow / div-by-zero /
  bounds) — the trap must still fire post-inline exactly as pre-inline; callees
  with inout/borrow parameters and write-backs; **slice-borrow parameters passed
  by value**; callees with aggregate by-value parameters (multi-slot); callees
  that move a by-value parameter and drop it; callees with early `return`; and
  `-> !` callees. The existing `opt.toml` trap-survival cases are the template.
- **Trap spans.** As established in Context, runtime traps carry **no** source
  location today — they are fixed-message aborts. Therefore inlining **cannot
  regress runtime trap location fidelity, because there is none to regress**: an
  overflow inside an inlined callee prints the same "integer overflow" and exits
  101 whether or not it was inlined, which is exactly what the invariant
  requires. The forward-looking obligation is that the splice must **faithfully
  copy each callee `CfgInst.span`** onto the moved instruction (which the
  durable-projection design in §4c already requires, since those spans become
  foreign-domain entries), so any future location-carrying trap mechanism
  inherits the callee's real source position. Rue's compile-time diagnostics run
  *before* inlining, so inlining changes no diagnostic a user sees today. This is
  a genuine discrepancy with the brief's premise that spans flow to runtime
  errors; the code says they do not, and the design follows the code.

## Implementation Phases

- [x] **Phase 1: Splice primitive** — CFG→CFG inline of one call given caller +
  callee `Cfg`, with value/block/slot rebasing, physical-ABI parameter lowering
  (by-value materialization at full slot width; by-ref place redirection limited
  to simple local/parameter roots), callee-lifetime storage + copied
  parameter-drop handling (§3), and return→block-param wiring. Unit-tested in
  `rue-cfg`. (landed, RUE-929, July 2026)
- [x] **Phase 2: Free-function driver at `-O2`** — two-phase construction; build
  the call-site graph by CFG scan (§5); conservative leaf/small thresholds;
  single-call-site inlining *without* callee removal; recursion refusal via
  call-site SCC; differential CLI coverage. Inlined callers are **not cached
  yet** (fail the export gate, §4c). (file RUE-930)
- [ ] **Phase 3: `-O3` thresholds** — larger caps, non-leaf callees, shared
  code-growth budget with unrolling (ADR-0054). (file RUE-931)
- [ ] **Phase 4: Durable cache integration for inlined callers** — multi-body
  `CfgDomainProjection` (§4c), callee-body-fingerprint + policy-version cache key
  (§4b); turn caching on for inlined callers. (file RUE-932)
- [x] **Phase 5: Whole-program dead-function elimination** — the canonical
  deterministic reachability step in the whole-program batch removes
  now-unreachable functions (including single-call-site callees the inliner
  consumed) while retaining entry points, exports, calls, cleanup edges, and
  conservatively all units when dependency completeness is unknown. (file
  RUE-933)
- [ ] **Phase 6: Methods/destructors** — extend as the ADR-0050 method/destructor
  caller surfaces complete. (file RUE-NNN — awaits the ADR-0050 method/destructor
  caller surfaces before it can be scoped and filed.)

## Consequences

### Positive

- `-O2`/`-O3` gain their first cross-function content, and downstream passes
  (CSE, copy-prop, loop opts) become far more effective across former call
  boundaries.
- The call-site graph (§5) gives the inliner exact multiplicity/leafness/recursion
  facts the deduplicated manifest cannot, while the manifest keeps one canonical
  invalidation path.
- Caching integration is designed up front (§4), so inlined callers become
  cacheable rather than silently defeating the durable cache.

### Negative

- CFG construction and optimization can no longer be one fused parallel map when
  inlining is on; they split into build/import-all then inline+optimize+export.
- The multi-body durable projection (§4c) is real work that must land before
  inlined callers cache; until Phase 4 they are recomputed every build.
- The by-ref place-redirection, materialized-parameter drop rules (§3), and the
  foreign-domain projection are subtle and need a focused correctness matrix.
- Every inlining PR must grow the differential set (deliberate, per ADR-0044).

### Neutral

- No language-semantics change, no spec sections, no preview gate.
- `-O0`/`-O1` behavior is unchanged; the inliner is skipped there.

## Open Questions

- **Policy-version granularity** — fold `inlining_policy_version` into
  `DURABLE_CFG_SCHEMA_VERSION` (coarse, invalidates the whole CFG cache on any
  policy change) or key it per-caller (finer, more bookkeeping)? Decide when
  Phase 4 lands; the deciding criterion is whether policy changes are frequent
  enough that whole-cache invalidation hurts incremental builds.
- **Materialized-parameter drop elaboration** — the exact instruction sequence
  for §3's one-slot/one-live-range/one-drop rule needs a worked example and test
  before Phase 1; deciding criterion is the differential trap/drop-order suite
  staying green across moved-out, dropped-at-exit, and `Copy` parameters.
- **Projected by-ref arguments** — excluded initially; the deciding criterion for
  admitting them is a proof that direct place substitution preserves
  address-formation timing and trap behavior.
- **Recursion relaxation** — bounded-depth recursive inlining is possible later;
  the initial rule refuses all call-site-SCC-participating calls.

## Future Work

- Broader whole-program reachability roots and method/destructor inlining remain
  future work as described by Phases 3, 4, and 6.
- Bounded recursive inlining under a size budget.
- Profile-guided inlining once any profiling exists.

## References

- [ADR-0044: Optimization Levels](0044-optimization-levels.md) — fixes the level
  placement and the observable-behavior invariant this note operates under.
- [ADR-0050: Stable semantic dependency manifests](0050-semantic-dependency-manifest.md)
  — the body-fingerprint partition and reverse-dependency closure inlining's
  cache key consumes (but whose deduplicated graph is *not* the call-site graph).
- [ADR-0053: Typed compiler query state](0053-typed-compiler-query-state.md) — the
  durable CFG cache (`DurableCfgArtifact`) inlining must integrate with.
- [ADR-0012: Compiler Optimization Passes](0012-optimization-passes.md) — the CFG
  opt framework.
- [ADR-0010: Destructors](0010-destructors.md), [ADR-0013: Borrowing Modes](0013-borrowing-modes.md),
  [ADR-0043: Collection/string type trio](0043-collection-string-type-trio.md)
  — drop, by-ref, and slice-by-value semantics the splice must preserve.
- RUE-915 (this design), RUE-236 (differential-opt harness), RUE-57 (trap-survival
  bug class), RUE-521 (address-taken-slot segfault), RUE-322/RUE-385 (slice/`str`
  parameter ABI), RUE-760 (non-place by-ref rejection), RUE-347 (`-> !` call
  divergence).
</content>
</invoke>
