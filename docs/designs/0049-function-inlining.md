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
accepted Phase 5 reachability step in that same batch. Phase 3 is complete:
O3 uses a 96-value callee cap, admits bounded non-leaf callees, and shares a
256-value per-function growth budget with unrolling. Phase 4 and the
method/destructor extension remain tracked separately.

This note analyzes the architecture of function inlining and records the
accepted direction. ADR-0044 already fixed the *level*
placement — conservative small/leaf-function inlining at `-O2`, aggressive
thresholds at `-O3` — so this note answers the *architecture* questions that
sit under that placement: where inlining runs, what it does mechanically to the
CFG IR, how it integrates with the **revisioned optimized-CFG query**, how it
handles **parameter ownership and drop**, and how it stays inside the
observable-behavior invariant. The query's terminal/cone retention boundary and
the parameter-mode physical ABI are core design constraints here. Tracks
RUE-915.

## Summary

Inlining replaces a `Call` to a suitable callee with a copy of the callee's body
spliced into the caller, renumbered onto the caller's frame and value space, so
that intra-procedural passes (constant folding/propagation, peephole, CFG
simplification, value forwarding, CSE, DCE, and the future loop passes) can then
see across the former call boundary. The central tension is that Rue's local
optimizer remains strictly **per-function** while general inlining is
inherently **cross-function**. Each optimized-CFG query builds (or imports) one
function, runs its local `rue_cfg::opt::optimize_with_budget`, and publishes that optimized
record. The compiler then runs one deterministic whole-program batch over those
records: it discovers call sites, applies the inline/DFE policy, and
re-optimizes only changed callers with the carried O3 growth budget. The CFG
splice primitive remains in `rue-cfg`; `rue-compiler` owns the callee map,
policy, metadata, cache/query boundaries, and batch orchestration. This keeps
local passes single-function while giving inlining the whole-program view it
requires.

Two consequences are load-bearing and are given their own decision sections:

- **Durable reuse has an explicit current boundary and an accepted Phase 4
  design.** The optimized-CFG query retains, imports, and publishes reusable
  terminals for callers that have not been spliced. A caller changed by general
  inlining is published with `durable_reuse_allowed: false`; it is recomputed on
  the next request. Phase 4 will add callee-body fingerprints, policy versioning,
  and multi-body domain projection before enabling reuse for those records (§4).
- **The inliner's call-site graph is separate from the per-key body-reference
  and query dependency graph** (§5). Call-site multiplicity, leafness, candidate
  lookup, and recursion SCCs are answered by **scanning CFG `Call`
  instructions**; revisioned query dependencies track which retained terminals
  must be invalidated when their stable inputs change. They answer different
  questions and must not be conflated.

## Context

### The optimizer is per-function today (ground truth)

The optimizer lives entirely in `crates/rue-cfg/src/opt/` and runs as CFG → CFG
transforms on **one function at a time**. `opt::optimize_with_budget` takes one
CFG and has no access
to any other function. A `Cfg` is one function: it owns that function's
`blocks`, `values`, `extra`, `call_args`, `num_locals`, `num_params`, `fn_name`,
`param_modes`, and `address_taken_slots` (`crates/rue-cfg/src/inst.rs:536`).

The production build seam is the optimized-CFG query and its compiler-owned
whole-program batch. It:

1. synthesizes drop-glue functions (`drop_glue::synthesize_drop_glue`, line 174);
2. concatenates user functions + glue and sorts them by machine symbol name —
   the machine symbol is the stable semantic identity shared by user, specialized,
   destructor, and glue functions;
3. the optimized-CFG query performs import-or-build and local
   `opt::optimize_with_budget`
   for one function, then publishes that optimized query terminal (or reuses a
   matching retained terminal);
4. the compiler batch scans the complete optimized record set, performs
   deterministic general inlining and DFE, and re-optimizes changed callers
   before publishing the final batch results. **No cross-function state is
   threaded through the local query**; the batch is the sole cross-function
   orchestration boundary.

A `Call` names its callee purely by interned symbol (`CfgInstData::Call { name:
Spur, .. }`, `crates/rue-cfg/src/inst.rs:351`; lowered at
`crates/rue-cfg/src/build.rs:875`). That symbol is exactly the key the function
list is sorted by, so "find the callee body for this call" is a symbol lookup
over the already-built function set — cheap to express, but it requires a stage
that can see the *whole set*, which the current per-function parallel map
deliberately cannot.

### Revisioned optimized-CFG query retention (current architecture)

The revisioned query database retains successful optimized-CFG query terminals
and their observed dependency cones across revisions. A terminal is reusable
only when its query key, target, optimization level, and stable function inputs
match; otherwise the query rebuilds the function. The optimized-CFG query
publishes one-function, locally optimized records, while the compiler-owned
whole-program batch retains its observed terminals and applies general
inlining/DFE to the batch result.

General inlining marks every changed caller
`durable_reuse_allowed: false`. Such a caller remains available in the current
batch and codegen request, but is excluded from durable terminal/cone retention;
the next request recomputes it and reruns the batch. This is the current cache
boundary. Durable reuse of post-inline callers is specified only in the
accepted RUE-932 Phase 4 design below.

### Domain projection is caller-only by contract (ground truth)

`CfgDomainProjection::from_local_body` builds the type/string/symbol/span
remapping tables that make a function-local optimized-CFG terminal portable
across revisions. It derives that mapping solely from the function's own AIR
body and body span. `import_cfg` and `import_accessor_cfg` remap a retained or
imported one-function CFG into the current live domain, while rejecting missing
stable types, symbols, strings, spans, or incomplete domains.

An inlined callee introduces foreign domains by definition: types, string
constants, callee `Call`/`Intrinsic` symbols, and instruction spans that point
into the callee's source range rather than the caller's. The current batch
therefore marks every changed caller `durable_reuse_allowed: false` and excludes
it from terminal/cone retention. Section 4 records the accepted Phase 4 design
for making those post-inline records reusable.

### Per-key body references and query dependencies (current architecture)

Semantic body analysis publishes a canonical, sorted, duplicate-free
`BodyReferences` value for each `BodyQueryKey`. A reference identifies the exact
callable instance, stable definition, nominal type, or drop-glue type selected
while analyzing that body. The body-closure scheduler consumes those typed
references to demand callable bodies, type facts, and drop glue; it does not
infer them from a second CFG scan.

The revisioned query database records the exact query terminals read while each
body, CFG, and downstream artifact is evaluated. Those per-key dependency edges
are the invalidation authority: a changed source, stable body input, target, or
configuration invalidates the affected terminals and their dependents, while
unchanged terminals remain reusable across revisions. Missing endpoints or
incomplete observations fail closed. The optimized-CFG query and compiler batch
therefore use this graph for retention correctness, while the inliner's CFG
scan remains responsible only for structural call-site decisions (§5).

The stable body-fingerprint partition is what inlining's future cache key needs:
a caller that inlines callee `f` must be rebuilt when `f`'s **body** fingerprint
changes, not merely its signature (§4b).

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
fights the durable-query grain: stable query inputs fingerprint an AIR body and
`from_local_body` anchors its projection at AIR body boundaries; splicing AIR bodies
would disturb those anchors and the cache key. Rejected as the most invasive and
the worst fit for the incremental model.

**(b) CFG-level inlining as a `rue-cfg` pass with access to other CFGs.** Add a
pass inside `opt::optimize_with_budget` that reaches other functions' CFGs. *Cost:* this
breaks the defining property of `rue-cfg`'s optimizer — that a pass sees exactly
one function — and forces `opt::optimize_with_budget`'s signature to take the whole function
set, which then has to be threaded through the Rayon per-function map in
the optimized-CFG batch. It puts cross-function orchestration (callee lookup, recursion
refusal, threshold policy, cache-key composition) inside `rue-cfg`, which has no
  view of the session, the query dependency graph, or symbol resolution. Rejected:
it pushes orchestration to the wrong layer.

**(c) A dedicated inter-procedural stage orchestrated by `rue-compiler`,
operating on optimized CFG records — RECOMMENDED.** Keep `CfgBuilder::build`
and the local `rue_cfg::opt::optimize_with_budget` query single-function. Once the complete
optimized record set is available, the compiler batch runs with the full
`{FunctionInstanceKey → CfgRecord}` map, applies the call-site/recursion/size
policy, splices accepted calls with the `rue-cfg` primitive, performs DFE, and
re-optimizes changed callers with the carried budget. Unchanged records are
retained. The inliner is self-contained CFG→CFG surgery in `rue-cfg`, but it is
driven by `rue-compiler`, which owns the callee map, call-site graph, metadata,
cache/query boundaries, and query dependency integration.

Reasoning:

- It preserves the invariant that every `rue-cfg` *optimization* pass is
  strictly local; the only cross-function code is the orchestration, which lives
  where the whole-program view already lives (the session).
- It reuses the CFGs the builder already produces — no second lowering, no
  AIR-level re-implementation.
- The batch runs after local optimization, so changed callers pay one additional
  local reoptimization while unchanged callers retain their query result. This
  makes the shared budget's pass order explicit: local O3 unrolling charges
  first, then accepted inlining and changed-caller reoptimization consume the
  remaining budget. At `-O0`/`-O1` the batch is skipped.
- Durable reuse applies only to the one-function optimized-CFG terminal today;
  post-inline reuse is a Phase 4 design question (§4).

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

### 4. Durable CFG cache integration — accepted Phase 4 design

Inlining touches all three cache operations. The decisions:

**(a) Future cached CFGs represent FINAL optimized, post-inlining CFGs.** The
current retention boundary is the unspliced optimized-CFG query terminal; changed
callers are explicitly excluded with `durable_reuse_allowed: false`. Phase 4
will retain the post-inline, post-reoptimization CFG only after its callee-body
dependency set and foreign-domain projection are durable. This keeps one
artifact per function and avoids silently reusing a caller whose inlined body is
stale.

**(b) Caller cache keys incorporate callee body fingerprints and inlining
policy.** `StableCfgInput` (`durable_cfg.rs:11`) today fingerprints only the
caller's own `body`, `specialization`, and `type_inputs`, and the reuse check
compares exactly those. A post-inlining caller artifact is
invalidated by two more things:
1. **Every inlined callee's body fingerprint.** The key must gain a set of
   `{callee identity → StableDefinitionInputFingerprint}` for each callee the
   caller actually inlined, drawn from the stable body-fingerprint partition
   (`session.rs`, `StableDefinitionInputFingerprint`). Editing an inlined
   callee's body changes its body fingerprint, which changes the caller's key,
   which forces a rebuild — exactly the per-key query dependency invalidation
   required for a callee *body* change rather than merely a signature change.
2. **The inlining policy revision.** A change to the threshold table or inlining
   algorithm must invalidate every inlined caller. Bump a coarse
   `inlining_policy_version` (companion to `DURABLE_CFG_SCHEMA_VERSION`,
   into the key; the simplest correct form folds it into the
   schema version so a policy change invalidates the whole CFG cache. A finer
   per-caller policy fingerprint is a later refinement.

Only callees actually inlined into *this* caller enter its key — not the caller's
whole dependency set — so a caller that inlined nothing keeps today's key
unchanged and its cache behavior is untouched.

**(c) Durable domain projection must carry foreign callee domains.** This is the
sharpest constraint. `from_local_body`'s caller-only contract rejects any
CFG containing spans, symbols, strings, or types that do not originate in the
caller's AIR body — which every inlined CFG does. The design:

- During the splice, the inliner already walks each copied callee instruction to
  rebase indices. At the same time it **accumulates a foreign-domain remapping
  table**: for each callee type, string constant, `Call`/`Intrinsic` symbol, and
  span that lands in the caller CFG, record the callee-side stable identity the
  callee's *own* `CfgDomainProjection` already holds (each callee has one built
  by `from_local_body` from its own body). The callee's projection is the
  authoritative
  source of stable identities for its domains.
- Extend `CfgDomainProjection` from a single caller-body projection to a
  **caller projection unioned with the projections of every inlined callee**,
  with callee spans anchored relative to the *callee's* `body_span` (the
  existing stable span-anchor mechanism) plus a callee-identity tag so import can
  resolve each foreign span against the right body. Concretely, current span
  validation must change from "reject spans outside the caller
  body" to "resolve each span against the caller body *or* a recorded inlined
  callee body," and `import_cfg`'s span/symbol/string/type remappers
  (driving `Cfg::try_remap_domains`) must consult the unioned table.
- `import_cfg` already remaps whole-CFG domains through `try_remap_domains`
  (type/struct/enum/symbol/string/span), so the *mechanism* to re-project a
  multi-body CFG exists; what must change is the *table construction*
  (`from_local_body`) and the *span-range contract*. Until that lands, the safe
  fallback is explicit and cheap: a caller that inlined anything **fails the
  export gate** (`from_local_body`/`validate_cfg` returns `Unsupported`) and is simply
  not cached — correct, just slower, and identical to the current explicit
  `durable_reuse_allowed: false` policy. Phase 4 turns caching on for inlined
  callers after the key and projection changes above.

**Sequencing note:** Phase 4 may import a post-inlining caller without rebuilding
its callees once the callee-body fingerprints in the key (4b) and multi-body
projection (4c) make that reuse sound and expressible.

### 5. Call-site graph and body-reference query roles

The inliner needs a **call-site graph** and builds it by **scanning the CFG's
`Call` instructions**. This graph and the per-key body-reference/query graph
answer different questions:

- **Call-site multiplicity** ("is this callee called once or ten times?"): count
  `CfgInstData::Call` instructions naming a symbol across all caller CFGs. Body
  references are duplicate-free and cannot answer this, so ten calls to `f` and
  one call to `f` produce the same callable reference.
- **Leafness** ("does this callee contain any call?"): scan the callee's
  `values` for `CfgInstData::Call`. Body references also include type and
  drop-glue demands, so they cannot cleanly report "contains a call."
- **Candidate lookup** ("what is the callee body for this call?"): the `Call`'s
  `name: Spur` is the symbol key into the `{symbol → Cfg}` map. This is
  CFG-native.
- **Recursion / SCC refusal**: the recursion cycle must be computed over the
  **actual call-site graph**, not all query dependency edges. Using the whole
  dependency graph for recursion analysis **can produce false cycles**: because
  it includes type and destructor demands, a
  non-recursive function that merely *mentions a type* whose destructor
  transitively references the function's own definition would appear in a cycle
  and be needlessly refused. The call-site graph contains only real call edges,
  so its SCCs are exactly the real recursion cycles.

Conversely, the revisioned query dependency graph — not the CFG scan — remains
authoritative for **invalidation and durable reuse**. Query keys and their
recorded terminal reads determine what must rebuild when a stable input changes.
**Scanning the CFG is not a competing invalidation path**; it answers the
inliner's structural questions, while query dependencies answer retention
correctness.

### 6. Threshold and candidate policy

The current criteria are deliberately conservative:

- **Callee instruction count** below a small budget. The counter is exact and
  free: `callee.values.len()`. O2 keeps its 32-value cap; O3 uses a 96-value
  cap. The reproducible measurement is an O0 CFG emission with the checked-in
  compiler, followed by counting each `vN` line between `cfg` headers (for
  example: `RUE_STD_PATH=$PWD/std $(scripts/rue path) --emit cfg -O0
  performance/workloads/lattice/main.rue`). The standalone checked-in
  programs `examples/life.rue`, `examples/collatz.rue`, and
  `examples/quicksort.rue` produced per-function ranges 23–75 (5 functions),
  13–43 (3), and 17–61 (4), respectively. The larger checked-in Lattice
  workload produced 1,282 reachable CFGs ranging from 1–736 values, with
  median 31, p90 56, p95 120, p99 272, and 1,199/1,282 (93.5%) at or below
  96. Percentiles use the nearest-rank convention (the smallest sorted value
  whose one-based rank is at least `ceil(p × 1,282)`); the exact count is
  retained alongside the rounded percentile labels. Thus 96 is the next
  conservative 32-value boundary above every small
  standalone body while retaining the large workload's short-function majority;
  128 would admit an additional long-tail band without a measurement-backed
  need.
- **Leaf-ness** from a CFG scan (§5): a callee containing no `CfgInstData::Call`
  is a leaf and the safest first target. O2 remains leaf-only under its small
  cap; O3 admits non-leaf callees under the larger cap, while refusing every
  recursive SCC. The batch expands only the deterministic original call-site
  set once, so copied non-leaf calls cannot recursively trigger more inlining.
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
  per-function `CodeGrowthBudget` capped at 256 values and 256 cloned blocks,
  debited by whichever runs. Unrolling's charge includes cloned block
  parameters and instructions; inlining's charge is the checked value-arena and
  basic-block delta from the actual splice. Ordinary never-returning bodies get
  a minimum one-value policy charge even when their exact value delta is zero;
  general inlining excludes accessor calls. The block ceiling bounds block-heavy
  zero-value site work, with checked addition and refusal before publication.
- **Counters' provenance.** Instruction counts and call-site multiplicity and
  leafness all come from CFG scans (§5); query dependency records are not read
  for the inline *decision*.

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
  RUE-236, the differential CLI harness). Inlining's differential
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
- [x] **Phase 3: `-O3` thresholds** — a 96-value callee cap admits the measured
  checked-in program bodies, non-leaf callees are admitted, and inlining shares
  one 256-value/256-block per-function code-growth budget with unrolling
  (ADR-0054).
  (file RUE-931)
- [ ] **Phase 4: Durable cache integration for inlined callers** — multi-body
  `CfgDomainProjection` (§4c), callee-body-fingerprint + policy-version cache key
  (§4b); turn caching on for inlined callers. (file RUE-932)
- [x] **Phase 5: Whole-program dead-function elimination** — the canonical
  deterministic reachability step in the whole-program batch removes
  now-unreachable functions (including single-call-site callees the inliner
  consumed) while retaining entry points, exports, calls, cleanup edges, and
  conservatively all units when dependency completeness is unknown. (file
  RUE-933)
- [ ] **Phase 6: Methods/destructors** — extend when the stable body-reference
  and query surfaces for methods/destructors are complete. (file RUE-NNN)

## Consequences

### Positive

- `-O2`/`-O3` gain their first cross-function content, and downstream passes
  (CSE, copy-prop, loop opts) become far more effective across former call
  boundaries.
- The call-site graph (§5) gives the inliner exact multiplicity/leafness/recursion
  facts that typed body references cannot, while the revisioned query graph keeps
  one canonical invalidation path.
- Caching integration has an accepted Phase 4 design (§4); current inlined
  callers are explicitly uncached rather than silently reused.

### Negative

- The whole-program batch adds a deterministic inline/DFE step after local CFG
  optimization and re-optimizes only changed callers.
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
- **Phase 4 cache policy** — choose the granularity of the policy revision in the
  durable key when foreign-domain projection and callee-body dependencies land.
- **Projected by-ref arguments** — excluded initially; the deciding criterion for
  admitting them is a proof that direct place substitution preserves
  address-formation timing and trap behavior.
- **Recursion relaxation** — bounded-depth recursive inlining is possible later;
  the initial rule refuses all call-site-SCC-participating calls.

## Future Work

- Broader whole-program reachability roots and method/destructor inlining remain
  future work as described by Phases 4 and 6.
- Bounded recursive inlining under a size budget.
- Profile-guided inlining once any profiling exists.

## References

- [ADR-0044: Optimization Levels](0044-optimization-levels.md) — fixes the level
  placement and the observable-behavior invariant this note operates under.
- [ADR-0063: Parallel demand-driven incremental compilation](0063-parallel-demand-driven-incremental-compilation.md)
  — the per-key body-reference and revisioned query dependency graph inlining's
  cache boundary consumes (but which is not the call-site graph).
- [ADR-0053: Typed compiler query state](0053-typed-compiler-query-state.md) — the
  revisioned query terminals and retention cones inlining must integrate with.
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
