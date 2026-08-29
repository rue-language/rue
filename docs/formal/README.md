# The Rue Formal Semantics

This directory is the **formal core** of Rue: a precise, mechanizable definition
of the language's static and dynamic semantics. It exists to serve two purposes
the prose specification (`docs/spec/`) cannot:

1. **Implementation completeness.** Enough detail that a second, independent
   compiler could be built to be *behaviorally compatible* with the reference
   compiler — not by reading our source, but by reading this.
2. **Verifiability.** Written as judgments and reduction rules, so that
   properties — above all Rue's central claim, *memory safety without a garbage
   collector* — can be stated precisely and, in time, mechanically proven.

The prose spec answers "what does this feature do, for a human learning the
language." The formal core answers "what is the *exact* meaning, for a compiler
author or a proof." The compiler is a third view — the running realization. All
three are views of one language and must agree where they overlap; a genuine
disagreement is a bug in one of them, reconciled by fixing whichever is wrong
rather than by precedence (RUE-305), and surfaced mechanically by the
differential oracle. Where the core is silent, the prose governs.

> **Status: foundation in progress.** This is being built keystone-first. The
> core calculus, the definition of *use*, and the shape of the ownership and
> dynamic semantics come first (`01-core-calculus.md`); breadth — every surface
> construct's rules — is filled in against the rubric below. Sections marked
> *(sketch)* state the intended shape but are not yet complete.

---

## The architecture: surface → elaboration → core

Rue is not formalized as one monolith. It is split into three layers, and only
the middle artifact — the **core** — carries the load-bearing semantics:

```
   surface Rue  ──[ elaboration ]──▶  core Rue  ──[ operational semantics ]──▶  behavior
   (what you    (desugaring +          (a small,     (a small-step machine;
    write)       comptime +             orthogonal    the definition of
                 monomorphization)      calculus)     "run this program")
```

- **Surface Rue** is the full language the programmer writes: blocks, `if`/`else
  if`, `while`, method-call sugar, `&&`/`||`, `inout`/`borrow` call-site marks,
  `comptime`, generics via comptime `type` parameters, and so on.
- **Elaboration** maps the surface to the core. It performs desugaring (a block
  becomes a `let`-sequence; `a && b` becomes an `if`; a method call becomes a
  function call with an explicit `self`) **and** it runs comptime: comptime
  blocks are evaluated away, and comptime-parameterized functions are
  monomorphized into ordinary specialized functions. **After elaboration there is
  no `comptime` and no generics** — the core is a first-order, fully-monomorphic
  language.
- **Core Rue** is the small calculus in `01-core-calculus.md`. Its abstract
  syntax has a handful of forms. Everything hard about Rue's *runtime* meaning —
  ownership, moves, borrows, drops, linear consumption, overflow, panics — lives
  here and only here.

### Why comptime is elaboration, not core

This is the single most important scoping decision, and it is deliberate.
Comptime is a *staging* construct: it runs during compilation and produces a
residual program. Formalizing staging (a two-level, quote/splice or partial-
evaluation semantics) is a large, separable project. By treating comptime as an
elaboration pass that produces core programs, we get to formalize the runtime
language — where the memory-safety claim lives — without first solving staging.

The obligation this creates is explicit and tracked: **elaboration must be
specified too** (what comptime evaluates, how monomorphization assigns identity
to specializations), but as its own layer, later, once the core is solid. The
core is what an alternate compiler's *back half* targets; elaboration is its
*front half*. (Deferring the staging semantics is not deferring correctness: the
core's soundness holds for any well-formed core program, however it was
elaborated.)

### Why not just formalize the surface directly

Because the surface is large and redundant, and every redundant form is a place
two implementations can drift. The core is *orthogonal*: one way to express each
idea. Ten surface constructs collapse to three core ones, and a property proven
once about the core holds for all ten. This is also what makes the language
extensible by less-capable authors (see the rubric): adding a surface construct
is "give its desugaring to existing core," which is mechanical, versus "invent
new semantics," which is not.

---

## Relationship to the prose spec (`docs/spec/`)

The prose spec remains the human-facing document and the home of the
paragraph-level traceability tests (`crates/rue-spec`). The formal core does not
replace it; it *grounds* it. Concretely:

- Each core rule cites the prose-spec paragraph(s) it formalizes (e.g. a core
  move rule cites `3.8:5`, `3.8:7`, `3.8:22`). This is the same traceability
  discipline the test suite already enforces, run in the other direction.
- Where the formal core reveals that a prose rule is imprecise, incomplete, or a
  folk-term ("implicit return", "when used"), the prose rule is **rewritten to
  match the core** — sharpening underspecified prose is the core doing its job,
  not a disagreement. But a genuine *contradiction* between the prose, the core,
  and the compiler is a **defect in one of the three**, reconciled by fixing
  whichever is wrong — no artifact is presumed authoritative, and none wins by
  precedence (RUE-305). Several such rewrites are expected; each is a normal spec
  change, pre-1.0.
- The formalization is *allowed to remove or roll back* language features it
  shows to be ill-defined or not worth their complexity. Pre-1.0, simplification
  is a feature. Such proposals are filed to Backlog for a maintainer decision,
  never enacted unilaterally.

---

## The executable oracle

A semantics written only on paper can silently disagree with the compiler. The
core's operational semantics is therefore built to be **executable**: a
reference interpreter that runs any core program and produces its result, its
panics, and its drop trace.

This one artifact does three jobs at once:

- it *is* the formal dynamic semantics (purpose 2);
- it is the behavioral reference an alternate compiler is checked against
  (purpose 1);
- it is the **differential-testing oracle** of RUE-50: run a random program
  through both the interpreter and the compiler; any disagreement is a bug in one
  of them. A planted-defect study measured this claim against historical Rue
  compiler failures rather than relying on a counterfactual: the corpus oracle
  caught RUE-348 at O1--O3 and RUE-914/RUE-1758 at O2--O3, while a bounded
  generated-fuzz window missed all three. See the
  [RUE-1816 coverage ledger](../notes/rue-1816-planted-miscompile-coverage.md)
  for the reproducible matrix and its accepted gaps.

The interpreter is validated *against* the compiler and the compiler against
*it*: neither is presumed correct; disagreement is the signal. (See RUE-50.)

---

## The extension rubric

This is the part that matters for handoff. Once the framework exists, adding or
changing a construct is a **fill-in-the-template** task, not a design task. To
add a construct to the core (or to add a surface construct that desugars to the
core), provide, in order:

1. **Abstract syntax.** The new form, added to the grammar in `01-core-calculus.md §2`.
   (Surface-only construct? Give its *desugaring* to existing core forms instead,
   and stop — you are done; the core rules already cover it.)
2. **Static rule(s).** Its typing judgment `Γ; Σ ⊢ e ⇒ T ⊣ Σ'` (`§5`): what it
   requires of its subexpressions, what type it has, and how it threads the
   ownership state Σ (does it *use* — hence move/copy — any place? does it
   reinitialize one?).
3. **Dynamic rule(s).** Its small-step reduction over the machine configuration
   (`§6`): how it steps, and every drop it performs.
4. **Prose-spec citation.** The `X.Y:Z` paragraph(s) it corresponds to, added as
   a comment on the rule. If none exists, the prose spec needs a paragraph too.
5. **Oracle case + differential test.** Extend the reference interpreter, and add
   a test program exercising the construct that must agree between interpreter
   and compiler.

A change that cannot be expressed by touching exactly these five things is a
change to the *framework*, not the language — and that is the kind of thing to
escalate to a maintainer, not to do mechanically.

The worked constructs in `01-core-calculus.md` are the templates: copy each one's
shape.

---

## Contents

- **`01-core-calculus.md`** — the keystone. Abstract syntax; the multiplicity
  lattice (Copy / Affine / Linear); **the definition of *use* (place vs. value
  context)**; the ownership-threading type judgment — including the leaf,
  operator, aggregate, and call statics (§5.8) that complete every §2 expression
  form; the **complete small-step dynamic semantics (§6)** — a reduction rule for
  every §2 form, grounded function-for-function in the `rue-oracle` interpreter
  (RUE-50) — and drop; the **allocation store and the library container
  defining equations (§6.13)** — the ratified RUE-390 modeling decision, which
  brings `ArrayBuf`/`StrBuf` buffers inside the proved perimeter under stated
  RustBelt-style library obligations; and the soundness theorems (memory
  safety), stated precisely.
- *(planned)* `02-elaboration.md` — surface→core desugaring and the comptime /
  monomorphization semantics.
- *(planned)* `03-metatheory.md` — proofs (progress, preservation, and the
  memory-safety corollaries) as they are discharged.
