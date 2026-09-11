---
id: 0091
title: "Struct patterns: exhaustive field destructuring with no rest form"
status: accepted
tags: [language, syntax, semantics, patterns, ownership]
feature-flag: struct_patterns
created: 2026-09-11
accepted: 2026-09-10
implemented:
spec-sections: ["5.1:2", "5.1:18", "5.1:19", "5.1:20", "5.1:21"]
superseded-by:
relates: ["RUE-1884", "RUE-2175", "RUE-613", "RUE-246", "ADR-0005", "ADR-0037", "ADR-0038"]
---

# ADR-0091: Struct patterns: exhaustive field destructuring with no rest form

## Status

Accepted on 2026-09-10 by Steve (the RUE-1884 ruling). Phase 1, `let`
patterns, is implemented behind the `struct_patterns` preview feature.

## Summary

A struct pattern binds every field of a struct value by name:
`let Point { x, y } = p;`, with renames (`y: py`), per-binding mutability
(`mut x`), and discards (`z: _`). A pattern has no rest form, so it must name
every field the struct declares. Adding a field to a struct is therefore a
compile-time error at every pattern over that struct until the pattern names
the field, which is the property the feature exists for.

## Context

Hand-written per-type code (a serializer, a hasher, an equality function, a
debug renderer) must name every field. Nothing tells its author when the
struct gains a field and one of those functions is not updated: the program
compiles and is wrong, and for agent-written code that is the worst class of
failure, because no compiler signal starts the correction loop. Rust's idiom of
writing a rest-less struct pattern precisely to get "new field breaks the
build" is the interim answer to `derive` until reflection over concrete types
is designed (RUE-246).

Rue had field-access destructuring of declared-linear values (3.8:33) and
enum payload patterns (4.7:30), but no struct pattern in `let` or `match`.

## Decision

1. **Surface.** `let_pattern` gains `struct_pattern = type "{" [ field_patterns ] "}"`.
   The head is written with the type grammar, so it takes every form a `let`
   annotation does: a struct name, a module-qualified name, or a
   type-constructor call. A field pattern is `f: b`, the shorthand `f`
   (mirroring field-init shorthand, RUE-613), `f: mut b` or `mut f`, or the
   discard `f: _`. `mut` belongs to a binding and is written inside the
   pattern; `let mut T { ... }` is a syntax error that says so.
2. **No rest form.** The pattern must name every declared field exactly once.
   A missing field is E0400 naming every missing field, an unknown field is
   E0401, a repeated field is E0402. Whether a later `..` is ever admitted is
   deliberately left open; admitting it would give up the property above.
3. **Semantics by desugaring.** A struct pattern is the sequence of let
   statements it stands for: the initializer is bound once to an unnameable
   temporary, then each field is `let b = t.f;` (or `let _ = t.f;`) in source
   order. Every rule those statements already have applies unchanged:
   shadowing and scope (5.1:10, 5.1:12), the wildcard discard (5.1:16), Copy
   versus move of a field read, the linear must-consume obligation (E0478 on a
   discarded linear field), and the destructor-prefix rule (E0456 on a move
   field of a struct with a destructor). No new ownership rule is introduced.
4. **Head checks.** The head must name a struct type (E0213, a new code) and
   the initializer must have exactly that type (E0206).
5. **Preview gate.** The feature ships behind `--preview struct_patterns`
   until its second phase lands.

### Implementation shape

The parser produces `LetPattern::Struct`. RIR lowering emits the temporary's
`Alloc`, one new `StructPattern` instruction naming the temporary, the head
type and the field list (a `pattern fields` payload family), and one `Alloc`
of a `FieldGet` per field. Semantic analysis checks the `StructPattern`
instruction — preview gate, head resolution, type agreement, and the
exhaustive field list — before the projections are analyzed, and emits no
value for it. Nothing downstream of AIR changes.

## Implementation Phases

- [x] **Phase 1: `let` struct patterns** - RUE-1884 (this ADR's initial
  implementation).
- [ ] **Phase 2: struct patterns in `match` arms** - RUE-2175: a top-level
  struct pattern over a struct scrutinee and a struct pattern in an enum
  payload position (`R.Ok(Point { x, y })`). This needs a `RirPattern`
  variant and its packed-payload encoding, the pattern matrix treating the
  irrefutable struct row as a binder that owns nested projections, and the
  same exhaustive field check.
- [ ] **Stabilization**: drop the preview gate once phase 2 lands and the
  spec cases run without it.

## Consequences

### Positive

- A field added to a struct fails every pattern that does not bind it, at the
  use site, with the missing fields named.
- No new ownership rules: the desugaring inherits the let statement's.
- Renames and per-binding `mut` cover the usual reasons to write a pattern
  instead of a sequence of field reads.

### Negative

- A pattern over a wide struct is verbose, and there is no `..` to shorten
  it; that verbosity is the point.
- A struct with a destructor cannot be destructured by move: its move fields
  are E0456 exactly as a field move is. Copy fields still bind.
- The lowering introduces an unnameable temporary per pattern; `--emit rir`
  shows it as `_@rue:destructure:N`.

## Open Questions

- Whether a rest form `..` is ever admitted (the ruling asked this to be
  recorded; the answer is deliberately deferred).
- Nested struct patterns inside a field position (`Line { a: Point { x, y }, b }`)
  are not part of phase 1.

## References

- [ADR-0005: Preview Features](0005-preview-features.md)
- [ADR-0037: Exclusivity model: access-point based](0037-exclusivity-model-access-point-based.md)
- [ADR-0038: Error handling: sum types, Result/Option, and must-check via linearity](0038-error-handling-sum-types-result-must-check.md)
- [Let Statements, §5.1](../spec/src/05-statements/01-let-statements.md)
- RUE-1884, RUE-613, RUE-246
