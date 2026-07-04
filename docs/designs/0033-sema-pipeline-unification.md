---
id: 0033
title: Sema Pipeline Unification
status: rejected
tags: [compiler, semantics, architecture]
created: 2026-06-11
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0033: Sema Pipeline Unification

## Status

Rejected as moot. The investigation recorded in RUE-174 established that the
supposed parallel semantic pipelines were dead copies rather than competing
production implementations; they were deleted in PRs #952 and #953. The
remaining live-pipeline duplication is tracked independently.

**Tracking:** RUE-134, RUE-120, RUE-141

## Problem

Semantic analysis exists as **three** parallel implementations in
`crates/rue-air/src/sema/`:

1. **Eager** — `Sema` methods (`analyze_*` on `&mut self`). Drives
   single-file compilation (no imports).
2. **Lazy** — free functions (`analyze_*_ctx` over `&SemaContext` +
   `&mut AnalysisContext`). Drives every compilation with `@import`
   (per ADR-0026's lazy analysis). ~427 `_ctx` references in
   `analysis.rs` alone.
3. **Parallel** — `analyze_all_function_bodies_parallel`, also
   `_ctx`-based, marked `#[allow(dead_code)]`. **Never called.**

The eager and lazy families are hand-mirrored copies of the same logic
and have repeatedly drifted. Confirmed drift instances, all found in a
single working day:

- RUE-97: the unreachable-duplicate-arm fix existed in one match
  analyzer and not the other.
- RUE-120: the inference-context builder was duplicated; a bug fix was
  first applied to the *dead* copy and had no effect.
- The module member-call pair (deduped in #943): the eager copy had
  silently lost the exclusive-access check.
- RUE-141 (fixed in #946): the lazy exclusive-access check tracked only
  inout duplicates while the eager one also tracked borrow duplicates
  and lvalue-ness — multi-file programs got weaker checking than
  single-file programs *for the same source code*.
- Every recent sema fix (loop re-moves, MarkMoved export, by-ref rules,
  Copy-through-moved) had to be written **twice**, doubling effort and
  review surface.

The pattern is structural, not accidental: nothing forces the families
back together, and the test suite exercises whichever pipeline its
fixture shape happens to take (single-file tests → eager; anything with
imports → lazy).

## Options

### A. Eager delegates to lazy (`SemaContext` becomes the only engine)

Build one `SemaContext` up front in the eager path (the constructor
already exists: `build_sema_context`) and run the `_ctx` family for
everything. Delete the `Sema`-method bodies.

- Pro: keeps the immutable-view architecture that a future parallel
  analyzer needs.
- Con: `build_sema_context` clones the struct/enum maps, the type pool,
  and builds the inference context — acceptable once per compile, but
  it also constructs a **fresh empty `ModuleRegistry`** (more dual
  state, must be fixed first). The `_ctx` family is currently the
  *less* checked of the two in at least one known case (RUE-141 class),
  so each migrated function needs a drift audit, not a blind cutover.

### B. Lazy delegates to eager (`&mut Sema` becomes the only engine)

The lazy driver is **sequential** — its use of the immutable
`SemaContext` view only exists to serve the dead parallel pipeline.
Make `analyze_function_bodies_lazy` drive the `Sema` methods directly
and delete the `_ctx` family plus the dead parallel function.

- Pro: deletes the most code (~5,000+ lines); the surviving family is
  the one that has historically carried the fixes first; no
  per-function migration — the lazy driver's *scheduling* (reachable
  functions only) is separable from the *analysis* functions it calls.
- Con: forecloses cheap resurrection of parallel analysis. (Assessment:
  today's parallel pipeline is dead, drifted, and would need its own
  audit before ever being enabled — it is not an asset worth preserving
  in its current form. If parallel analysis is wanted later, the right
  move is to reintroduce an immutable view over the *unified* family,
  not to keep today's copy alive.)

### C. Keep both, share bodies piecemeal

The #943/#946 pattern (extract shared body, thin per-pipeline wrappers)
applied function by function, ~50 more times.

- Pro: lowest per-step risk; proven twice.
- Con: never finishes; the wrappers and the two drivers remain; every
  *new* feature still lands in two entry points.

## Decision (proposed)

**Option B**, executed in four phases, each independently shippable and
suite-verified:

1. **Delete the dead parallel pipeline** (`analyze_all_function_bodies_parallel`
   and anything reachable only from it). Pure dead-code removal;
   `#[allow(dead_code)]` markers come off whatever survives.
2. **Differential audit**: for each `_ctx` function with a `Sema`-method
   twin, diff the bodies and record semantic differences (the RUE-141
   exercise, systematized). Every difference becomes either a bug fix
   (landed separately, with a test pinning the corrected behavior) or a
   documented intentional divergence. *This phase produces fixes, not
   refactors.*
3. **Re-point the lazy driver** at the `Sema` methods, one analysis
   entry point at a time (calls, methods, field ops, match, loops…),
   deleting each `_ctx` function as its callers disappear. The full
   suite (now ~540 normative spec paragraphs at 100% traceability plus
   ~230 CLI cases) is the behavior harness; multi-file CLI cases
   specifically exercise the lazy driver.
4. **Collapse the leftovers**: `SemaContext` shrinks to whatever the
   lazy *scheduler* still needs (reachability tracking); the duplicate
   inference-context builders (RUE-120) merge as a side effect.

### Verification

- Full `./test.sh` after every phase-3 step (the multi-file CLI suite is
  the lazy pipeline's harness).
- A one-off differential run before phase 3: compile every spec/CLI
  fixture through BOTH drivers (forcing the lazy path with a trivial
  import where needed) and diff diagnostics + binaries. Differences
  found become phase-2 entries.
- Error-code and diagnostic-wording changes are forbidden during
  phase 3 (they belong in phase 2 or separate PRs).

### Sizing

Phase 1 is an afternoon. Phase 2 is the real work (estimate: the audit
table itself is a day; fixes are bounded by what it finds). Phase 3 is
mechanical but wide — several PRs, each deleting one functional cluster.
Phase 4 is small. The payoff: every future sema change is written once,
and the RUE-141 class of "same program, different verdict depending on
imports" becomes unrepresentable.
