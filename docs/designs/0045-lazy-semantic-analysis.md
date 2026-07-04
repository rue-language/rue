---
id: 0045
title: Lazy semantic analysis (compile-on-reference)
status: accepted
tags: [compiler, semantics, comptime, modules, stdlib, language-shape]
feature-flag: lazy-analysis
created: 2026-07-03
accepted: 2026-07-03
implemented:
spec-sections: []
relates: ["ADR-0025", "ADR-0026", "ADR-0042", "RUE-315", "RUE-328"]
---

# ADR-0045: Lazy semantic analysis (compile-on-reference)

## Status

**Accepted — ratified by Steve, 2026-07-03 (RUE-328, out of the ADR-0042
discussion).** The *design* is decided (Zig-style lazy analysis, rolled out
incrementally); the implementation is tracked separately and broken into Linear
tasks after this ADR lands. This ADR documents the decided model, not a menu of
options.

## Summary

Rue analyzes **only the declarations that are actually referenced**, transitively
from a program's entry point (`main`, the *reference root*). A declaration that
nothing reaches — on the current target, with the current comptime configuration
— is never type-checked, lowered, or codegen'd. This is the Zig model, chosen
deliberately over "check everything by default." Its headline consequence is
**conditional compilation for free**: a `@target_os()`/`@target_arch()` comptime
branch that is false on the current target selects code that is never analyzed,
so platform-specific declarations simply don't need to compile off-target — no
`#ifdef`, no `cfg`, no preprocessor. This falls out of comptime (ADR-0025) plus
lazy analysis, and it is the primary motivation, not merely a compile-time
optimization. The accepted tradeoff is the Zig bet: a type error in an
*unreferenced* declaration stays hidden until something references it. A future
opt-in `check-all` mode may force full analysis for CI / library validation, but
the model **must not depend on it**.

## Context

### The pressure: a growing `std` and off-target code

ADR-0042 (std availability, Option C) commits Rue to an explicit
`@import("std")` bundle backed by a comptime-source standard library — generic
type functions (`Option(T)`, `Vec(T)`, `StrBuf`) instantiated on use. Two forces
make **eager, whole-program** semantic analysis untenable as that library grows:

1. **Cost.** Eagerly analyzing every declaration in `std` (and every
   `@import`ed module) on every compile scales with the size of the library, not
   the size of the program. A hello-world that imports `std` should not pay to
   type-check `Vec`, `HashMap`, and the parser combinators nobody called.
2. **Correctness across targets.** More fundamentally, eager analysis *forces
   off-target code to compile*. A function that calls a Windows-only syscall
   wrapper cannot be type-checked on Linux if the wrapper's types don't exist
   there. Under eager analysis you need a preprocessor (`#ifdef`/`cfg`) to
   physically remove that code before the type-checker sees it. Rue does not want
   a preprocessor — it wants comptime to be the *only* metaprogramming layer.

Rue already does on-demand work in one place: **comptime monomorphization**
(ADR-0025). A generic function `fn id(comptime T: type, x: T) T` is not analyzed
as a template; it is analyzed *per instantiation*, when a concrete `T` is
supplied at a call site. Lazy semantic analysis is the generalization of that
principle from "generic instantiations" to "all declarations": nothing is a
template the compiler eagerly walks; everything is analyzed the first time it is
reached from the reference root.

### Why the pure Zig model (not check-all-by-default)

The considered alternative was a **hybrid**: analyze lazily for speed, but still
walk every declaration once ("check-all by default") so that dead code is still
type-checked. Steve chose the **pure** model instead, because the conditional-
compilation payoff *requires* it: if off-target declarations are still checked,
`@target_os` branches don't give you free conditional compilation — the
Windows-only code must still type-check on Linux, which is exactly the problem.
You cannot have both "dead code is always checked" and "off-target code need not
compile." Rue picks the second. Check-all becomes an **opt-in mode** (see below),
never the default, and the language semantics are defined so that programs never
*rely* on unreferenced code being checked.

## Decision

### 1. The reference root and the analysis frontier

Analysis is a **reachability walk** over declarations, seeded from a set of
**reference roots** and driven by references discovered during analysis.

- **Reference root (the seed).** For an executable, the root is the program's
  `main`. Analysis begins at `main`, and a declaration is analyzed the first time
  it is referenced from an already-analyzed declaration — a called function, a
  named type, a read constant, an instantiated generic. The transitive closure of
  references from the roots is the **analyzed set**; everything else is the
  **unanalyzed remainder** and is, by design, unchecked.
- **References that pull a declaration in** include: a call or method resolution,
  a type appearing in a signature/annotation/struct field that must be laid out,
  a `const` whose value is read, a comptime evaluation that touches a declaration,
  and a generic instantiation (which pulls the *specific monomorphization*, per
  ADR-0025, not the generic body abstractly).
- **The frontier is comptime-configured.** Reachability is computed *after* the
  comptime facts of the current build are known — target OS/arch, comptime
  constants, `@import` results. A branch that comptime-evaluates to false
  contributes no references, so its body never enters the analyzed set. This is
  the mechanism behind conditional compilation (§4).

### 2. Eager → on-demand across the pipeline

The compiler pipeline (Lexer → Parser → AstGen → Sema → CfgBuilder → Lower →
RegAlloc → Emit → Link) is today driven eagerly: each pass processes *every*
item before the next pass runs. Lazy analysis reorganizes the front half of the
pipeline to be **demand-driven per declaration**, converging on the model
comptime monomorphization already uses:

| Stage | Today (eager) | Under lazy analysis |
|-------|---------------|---------------------|
| Lexer | whole file | whole file (unchanged — lexing is cheap, and a file is the unit of parse) |
| Parser → AST | whole file | whole file, but a **module is not parsed until referenced** (§3) |
| AstGen → RIR | every item | still builds RIR for parsed items (untyped, cheap); this is the "declaration index" the frontier walks |
| **Sema → AIR** | every item | **on demand**: a declaration is type-checked the first time the frontier reaches it; results are memoized so a declaration is analyzed at most once (or once per monomorphization) |
| CfgBuilder → CFG | every function | only for functions in the analyzed set |
| Lower → MIR | every function | only for functions in the analyzed set |
| RegAlloc / Emit / Link | every function | only for functions in the analyzed set |

The key move is at **Sema**: it becomes a memoized, reentrant query
("analyze declaration D") rather than a pass over a list. Lowering and codegen
then naturally process only what Sema produced. RIR/AstGen stays eager *per
parsed file* because it is cheap and gives the frontier a name-indexed table of
declarations to resolve references against; what lazy analysis avoids is the
expensive part — type-checking, layout, CFG, lowering — for unreferenced decls.

Parsing granularity is a **file** (a module is parsed whole or not at all);
analysis granularity is a **declaration**. That split is deliberate: it makes the
first rollout (lazy *module* loading) a coarse, low-risk cut, with per-declaration
laziness layered on later (§6).

### 3. Lazy module / `@import` loading

The coarse-grained first increment: an `@import`ed module is **not read, parsed,
or analyzed until a name from it is referenced**. `@import("std")` binds a
namespace handle; touching `std.Vec` is what forces `std`'s `Vec` submodule to be
located, parsed, and analyzed. Consequences:

- **`std` is structured as fine-grained submodules** so that importing the bundle
  pulls in only the pieces actually used. A program that uses `std.Option` does
  not parse `std.Hash`. This is where most of the compile-time win lands with the
  least machinery, and it is why the rollout starts here (RUE-315 provides the
  `std` bundle + namespacing this rides on).
- Module resolution and parse become **demand-driven queries** keyed by the
  imported path, memoized so each module is parsed at most once.
- A module that is imported but never dereferenced is never even parsed — so a
  syntax error inside an unreferenced submodule is, like a type error, not
  surfaced until something reaches it. Same Zig bet, one stage earlier.

### 4. Conditional compilation for free (the headline)

Because the reachability frontier is computed against comptime facts, a comptime
`if` on the target selects which declarations are analyzed:

```rue
fn open_console() {
    if @target_os() == .windows {
        win_alloc_console();   // analyzed ONLY when target_os == windows
    } else {
        posix_isatty();        // analyzed ONLY otherwise
    }
}
```

On Linux, `@target_os() == .windows` comptime-evaluates to false; the `then`
branch contributes no references; `win_alloc_console` never enters the analyzed
set and is **never type-checked or lowered**. It may reference Windows-only types
that don't exist on this target — that is fine, because the compiler never looks
at it. On Windows the branches swap. No preprocessor, no `cfg` attribute, no
separate build-config language: conditional compilation is just comptime plus
lazy analysis.

This pattern (`@target_os()` / `@target_arch()` comptime branches guarding
platform-specific declarations) is a **supported, tested** idiom, not an
accident. The implementation work includes CLI cases that compile the *same*
source for two targets and assert each target analyzes (and rejects errors in)
only its own branch — the differential oracle for this feature is "an error
planted in the off-target branch does not fail the on-target build, and vice
versa."

### 5. The accepted tradeoff and where `check-all` hooks in

**The Zig bet.** A type error (or syntax error) in a declaration that nothing
references is **not reported**. A library can ship a broken, unused `fn` and any
program that doesn't call it compiles clean. This is the deliberate cost of the
conditional-compilation payoff — the two are the same coin. Rue accepts it.

**`check-all` (future, opt-in, non-load-bearing).** A later mode may force
analysis of a broader set than the reference closure — e.g. "every `pub` item in
the crate being built" — so that CI and library authors can validate code no test
happens to reach. Its natural hook is **seeding the frontier with extra roots**:
`check-all` adds all `pub` declarations (and, for a fully exhaustive mode, all
declarations) to the root set before the reachability walk, then runs the exact
same on-demand Sema. It changes *what is rooted*, not *how analysis works*.
Crucially, the language is specified so that **no program's meaning depends on
`check-all`**: it can only turn currently-hidden errors into reported ones; it can
never change which code runs. That is what "the model must not depend on it"
means, and it is why `check-all` can be added later without disturbing this ADR.

### 6. Rollout (incremental)

1. **Lazy module loading** (first): don't parse/analyze an `@import`ed module
   until a name from it is referenced; ship `std` as fine-grained submodules
   (rides on RUE-315). Most of the compile-time win, coarse granularity, lowest
   risk.
2. **Per-declaration laziness** (later, if profiling justifies it): make Sema a
   memoized per-declaration query so unreferenced declarations *within an analyzed
   module* are also skipped, and so intra-file `@target_os` branches get the same
   free-conditional-compilation treatment as cross-module code.
3. **`check-all` mode** (later, optional): opt-in extra roots for CI / library
   validation, as in §5.

Each step is independently shippable and independently valuable; the model is
defined by the end state, but the language semantics (only referenced code is
guaranteed checked) hold from step 1.

### Scope boundary

This ADR is **within-compilation laziness only**: within a single `rue` invocation,
analyze only what the reference root reaches. **Cross-compilation incremental
caching** — persisting analysis results between separate compiles, invalidation,
a query database on disk — is a *separate, later* concern and explicitly out of
scope here. The two are often conflated ("lazy/incremental"); this ADR is the
first, and does not presuppose the second.

## Implementation Phases

Phases are recorded here for context; Steve breaks them into Linear tasks under
RUE-328 after this ADR lands.

- [ ] **Phase 1: Lazy module loading** — parse/analyze `@import`ed modules
  on first reference; memoized module-resolution query. (rides on RUE-315)
- [ ] **Phase 2: `std` as fine-grained submodules** — restructure the bundle so
  import pulls only referenced pieces. (RUE-315)
- [ ] **Phase 3: On-demand Sema** — memoized per-declaration `analyze(D)` query
  seeded from `main`; CFG/Lower/codegen consume only the analyzed set.
- [ ] **Phase 4: Conditional-compilation idiom + oracle** — `@target_os`/
  `@target_arch` comptime branches as a supported pattern; differential CLI cases
  compiling one source for two targets, asserting per-target branch selection and
  that off-target errors don't fail the on-target build.
- [ ] **Phase 5 (optional, later): `check-all` mode** — opt-in extra roots (all
  `pub`, or all decls) for CI / library validation.

## Consequences

### Positive

- **Conditional compilation with no preprocessor** — the headline. `@target_os`/
  `@target_arch` comptime branches give platform-conditional code that falls out
  of comptime + laziness, keeping comptime the single metaprogramming layer.
- **Compile time scales with the program, not the library.** Importing `std`
  costs only the parts used; a growing standard library stays cheap. This is what
  makes ADR-0042's explicit-`std`-bundle model viable long-term.
- **Consistent with comptime.** Generalizes the on-demand analysis comptime
  monomorphization already does (ADR-0025) from generic instantiations to all
  declarations — one mental model, not two.
- **Off-target code need not compile on the host** — no `cfg`/`#ifdef` gymnastics,
  no "stub the other platform's types" boilerplate.

### Negative

- **Unreferenced errors hide (the Zig bet).** Type/syntax errors in code no
  reference reaches are not reported until reached. A library's untested, unused
  function can be broken and still ship green. Mitigation is opt-in `check-all`
  (later) plus the testing story below — but the model deliberately does not
  *depend* on either.
- **Sema becomes reentrant and memoized**, not a straight pass — more machinery,
  and cycle handling (a declaration referencing itself / mutual reference) must be
  explicit. This mirrors work comptime already required.
- **Error ordering / determinism.** Which errors surface, and in what order,
  depends on the reachability walk rather than source order; diagnostics must be
  made deterministic (stable frontier ordering) so builds are reproducible.

### Neutral

- Parsing stays whole-file; only *which files/modules* are parsed becomes lazy at
  first. Per-declaration parse laziness is not pursued (declarations are cheap to
  keep as an untyped RIR index; the expensive stages are the ones made lazy).
- Cross-compilation incremental caching is left for a future ADR; nothing here
  forecloses it (the memoized-query shape is, in fact, a natural foundation for
  it).

## Testing and spec-traceability implications

Lazy analysis interacts with Rue's two verification nets in a way that is
**self-consistent rather than a problem**, and this is the reasoning that makes
"unreferenced code is unchecked" acceptable in practice:

- **Tests are reference roots.** A spec/CLI/UI test that exercises a feature
  *references* the code implementing it, so that code is pulled into the analyzed
  set and fully checked. **Covered code is force-analyzed by the very test that
  covers it.** There is no "tested but unchecked" gap: coverage *is* analysis.
- **`pub` library items are exercised by their tests, not by mere existence.** A
  `pub fn` in `std` is checked when a test (or a program) references it — which,
  under the traceability regime, is exactly when it has spec coverage. An
  unreferenced *and* untested `pub` item is unchecked **by design**; that is the
  Zig bet stated in coverage terms.
- **Differential oracle.** The conditional-compilation idiom is validated by
  compiling one source for multiple targets and asserting each target analyzes
  only its branch (an error planted off-target does not fail the on-target build).
  Any new codegen/ABI surface a target-specific branch introduces carries its own
  multi-case CLI coverage in the same change, per the project's
  coverage-follows-capability rule.
- **`check-all` and CI.** If/when a `check-all` mode lands, CI may run it to close
  the untested-`pub` gap for library crates — but because no program's meaning
  depends on it, it is a *belt-and-suspenders* validation layer, not part of the
  language definition.

Net: the traceability check ("every normative paragraph has a test") continues to
mean what it meant — a covered paragraph's implementation is analyzed because the
test references it. What lazy analysis adds is that *un*covered, *un*referenced
code is unanalyzed, which is the intended semantics, not a regression in the net.

## Open questions

Deferred to the implementation tasks; none block accepting the model:

1. **Cycle handling** — the exact protocol for mutually-referential declarations
   in the memoized Sema query (in-progress markers, error recovery).
2. **Diagnostic determinism** — the canonical frontier-ordering that makes error
   output reproducible independent of walk scheduling.
3. **`check-all` root set** — when it lands, does "all decls" or "all `pub`
   decls" become the default exhaustive set, and is it per-crate or whole-program?
4. **Interaction with future incremental caching** — confirming the memoized-query
   shape chosen here is the one a later on-disk cache wants.

## Future Work

- **Cross-compilation incremental caching** — persisting and invalidating
  analysis results across compiles (separate ADR; out of scope here).
- **`check-all` mode** — opt-in full analysis for CI / library validation (§5).

## References

- RUE-328 — Lazy semantic analysis (compile-on-reference); the decision recorded
  here.
- [ADR-0042](0042-std-availability-model.md) — std availability (Option C,
  explicit `@import("std")` bundle); the pressure that motivates lazy analysis,
  and which this ADR backs.
- [ADR-0025](0025-comptime.md) — comptime; monomorphization is the existing
  on-demand analysis this ADR generalizes, and the mechanism behind free
  conditional compilation.
- [ADR-0026](0026-module-system.md) — module system / `@import`; the loading path
  made lazy in Phase 1.
- RUE-315 — std bundle + `@import("std")` namespacing; Phases 1–2 ride on it.
- Zig's lazy compilation / comptime conditional compilation — the model Rue adopts.
