# The Spec Prose Rubric (RUE-202)

How to rewrite a prose-spec chapter against the formal core. Following this
rubric, a chapter rewrite is a **fill-in-the-checklist task, not a design
task**. If a step forces a choice the rubric doesn't cover, stop and escalate
(see "What escalates" at the end) — that is the signal you have left rubric
territory.

Template PRs to copy the shape of: the ch-4.5/4.6/4.9/4.10 pass (this rubric's
companion PR), the ch-5 statements pass (#1153), and the ch-7 arrays pass
(#1164). The register to imitate: `3.4:3–4` (never coercion), `5.2:11`,
`3.8:76` — each states one claim, names the core rule that governs it, and
stops.

## The one law: IDs are permanent

A paragraph ID (`X.Y:Z`) is an **explicit string key**, not a position. The
traceability checker (`crates/rue-spec/src/traceability.rs`) keys every
paragraph by the literal `id="X.Y:Z"` in its `{{ rule(...) }}` marker and
fails the build on: a test referencing a nonexistent ID (orphan), a normative
paragraph with no test, or a duplicated ID.

Therefore:

- **Never renumber, reuse, or delete an ID.** Rewording under a stable ID is
  always safe; renumbering breaks every test that cites the old ID *and*
  strands the new one uncovered.
- **Reordering paragraphs is free.** Physical order in the file is
  meaningless to the checker (chapter 3.8 already runs :1, :2, :3, :4, :76,
  :14, …). Put paragraphs in the best reading order; the IDs stay.
- **New claims get fresh IDs** — the next integer above the section's current
  maximum (check the whole file; numbering is not contiguous).
- **Never emit the same ID twice**, even transiently (last-writer-wins would
  silently erase the earlier rule's coverage obligation).

## Procedure

1. **Inventory.** List the chapter's paragraph IDs, categories, and per-ID
   test counts (`grep -rn '"X.Y:' crates/rue-spec/cases/`). High-count IDs are
   load-bearing: reword freely, but their *claim* must keep its meaning.
2. **Classify each paragraph's defects** (any of):
   - *folk-term* — see the banned-terms table below;
   - *multi-claim* — two independently-testable claims under one ID;
   - *miscategorized* — e.g. an EBNF block as `normative` (should be
     `syntax`), a runtime-behavior rule as `normative` (should be
     `dynamic-semantics`), rationale inside a normative rule;
   - *uncited* — a value/ownership/evaluation claim with no core-calculus
     citation;
   - *missing denotation* — the construct's rules give its type and legality
     but never say **what value it evaluates to**;
   - *restated ownership* — the paragraph re-asserts a rule owned by 3.8/3.9/
     6.1 instead of cross-referencing it.
3. **Rewrite,** applying the register rules below.
4. **Split multi-claim paragraphs.** The old ID keeps the claim its existing
   tests actually exercise (read the tests to decide); each split-out claim
   gets a fresh ID.
5. **Wire coverage.** Every new ID in a gated category (`normative`,
   `legality-rule`, `dynamic-semantics`, `syntax`, `undefined-behavior`)
   needs ≥1 test reference **in the same PR**. Prefer adding the new ID to an
   existing case that already exercises the claim; write a new case only if
   none does. Category changes *within* the gated set are coverage-neutral.
6. **Verify.** Every claim you sharpened must be checked against the
   compiler by **executing a program** (`scripts/rue exec`), not by reading
   code. Then run the gates:
   ```bash
   ./buck2 run //crates/rue-spec:rue-spec -- --traceability
   ./buck2 run //crates/rue-spec:rue-spec -- "<chapter keyword>"
   ```
   and `./test.sh` before the PR.
7. **PR** with a per-ID change table (ID → what changed → why), and
   `Part of RUE-202` in the body.

## Register rules

- **One claim per gated ID.** A paragraph in a gated category asserts exactly
  one independently-testable thing. Clarifying cross-references in
  parentheses are not claims and are welcome. Rationale is a claim about *why*
  — it moves to its own `cat="informative"` paragraph.
- **Cite the core.** Every claim about value denotation, evaluation order,
  ownership effects, or drops cites the governing rule with the established
  convention:
  `(core calculus \`docs/formal/01-core-calculus.md\` §X.Y, rule \`(Name)\`)`.
  If no core rule covers your claim, the chapter is blocked on a core gap:
  **file the gap, do not invent the rule.**
- **Categories.** `syntax` for grammar blocks; `legality-rule` for
  compile-time rejections (use **MUST**); `dynamic-semantics` for what
  happens at run time; `normative` for definitional/static claims that are
  neither; `informative` for rationale, navigation, and restatements;
  `example` for code. A gated claim must never sit in a no-`cat` paragraph
  (no-`cat` defaults to informative and silently loses its coverage
  obligation).
- **Cross-reference, don't restate.** Ownership rules belong to 3.8/3.9,
  parameter modes to 6.1, coercion to 3.4. An expression chapter states its
  own value/type/legality rules and points elsewhere in one sentence
  (`cat="informative"`) for the interactions. A restatement drifts; a
  reference cannot.
- **State the denotation.** Every expression form's chapter must contain a
  `dynamic-semantics` (or `normative`) paragraph saying what the form
  **evaluates to**, cited to the core. "Its type is T" is not a denotation.

## Banned folk-terms

| Folk phrasing | Replace with |
|---|---|
| "implicit return", "implicitly returns" | the body is an expression; a sequence evaluates to its tail, a call to its body's value (core §4.3, §6.7, §6.9) |
| "the implicit `()`" | "the `()` of the omitted form (4.9:3)" — name the desugaring rule |
| "destroyed", "cleaned up", "freed" (of a binding) | "dropped" + cite 3.9 and core §6.7/§6.11 |
| "goes out of scope" (as the operative event) | "leaves scope; its drop obligation is specified by 3.9:18" |
| "compatible with" (of types) | "**identical to**, up to the one never-type coercion (3.4:3)" |
| "consumed", "used up" (undefined) | cite the definition of *use* (3.8:76; core §4.2) |
| "executed" (of expressions) | "evaluated" (statements execute; expressions evaluate to values) |
| bare "evaluates to" with no rule | keep the phrase, add the denotation content and the core citation — the phrase is fine; a missing rule is not |

## What escalates to a maintainer (file an issue, stop)

- Any suspected **behavior difference** between the prose claim and the
  compiler (verify by execution first; file with the repro, framed as a
  hypothesis).
- Any **core gap** — a claim the core has no rule for.
- Any proposal to change what a rule *means* (this rubric covers register,
  not semantics).
- Reclassifying a **tested** paragraph out of the gated set (dropping a
  coverage obligation needs judgment).
- New `undefined-behavior`/`unspecified` classifications (ADR-0036 owns that
  decision).

## Worked example (from the template PR)

Before (4.9:4, `legality-rule`):

> The expression following `return` (or the implicit `()`) **MUST** have a
> type compatible with the function's declared return type.

Defects: folk-term ("the implicit `()`"), vague "compatible with" (Rue has
exactly one coercion), no citation.

After:

> The operand of `return` — the written expression, or the `()` of the
> omitted form (4.9:3) — **MUST** have the function's declared return type.
> As everywhere, the one admitted coercion is from the never type (3.4:3); no
> other type difference is accepted.

Same ID, same tests, same meaning — now stated so a second implementation
could not misread it.

---

*Marker syntax note: the live paragraph-marker format is the Zola shortcode
`{{ rule(id="X.Y:Z", cat="category") }}`. The `r[X.Y:Z#category]` form that
appears in some older process docs is stale — do not use it.*
