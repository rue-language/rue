---
id: 0095
title: "Checked block reasons: every checked site states the invariant it relies on"
status: accepted
tags: [language, syntax, semantics, unchecked, tooling]
feature-flag: checked_reasons
created: 2026-09-11
accepted: 2026-09-10
implemented:
spec-sections: ["9.1:5", "9.1:14", "9.1:15"]
superseded-by:
relates: ["RUE-1887", "RUE-195", "RUE-1912", "ADR-0005", "ADR-0028"]
---

# ADR-0095: Checked block reasons: every checked site states the invariant it relies on

## Status

Accepted on 2026-09-10 by Steve (the RUE-1887 ruling). Phase 1, the reason
clause on `checked` blocks, is implemented behind the `checked_reasons`
preview feature.

## Summary

A `checked` block carries the reason it is sound, as a string literal between
the keyword and the block:

```rue
checked "index < len was established by the guard above" {
    @ptr_read(@ptr_offset(base, index))
}
```

The reason is part of the syntax tree, so it survives formatting, reaches the
RIR and the `--emit` views, and cannot drift away from the block it justifies.
Under the preview the reason is required and must not be empty; there is no
allow-list escape. Once the feature stabilizes, every `checked` block in every
Rue program states its invariant.

## Context

A `checked` block is where the programmer takes over an obligation the
compiler cannot discharge (ADR-0028). The block itself is not the risk. The
risk is that the invariant it relies on is never written down: a reviewer has
to reconstruct it, and an agent that wrote the block never had to articulate
it. Rust learned this the hard way and bolted `undocumented_unsafe_blocks` on
as an opt-in lint; Rue can make it grammar. For agent-written code the cost is
nothing and the forcing function is the benefit: the one part of the language
the compiler cannot check is the part where a stated reason earns reviewer
trust.

A string literal was chosen over a recognized comment form because a literal
is a node of the tree. It survives a formatter, it is visible in every emitted
view of the program, and it cannot be separated from the block by an edit
that moves one line.

## Decision

- `checked_expr = "checked" [ STRING ] "{" block "}"`. The optional string is
  the block's reason.
- With `--preview checked_reasons`, a `checked` block without a reason, or
  with an empty one, is a compile error (E1301) at the block. Without the
  preview, a reason is refused by the ordinary preview gate (E1100), so
  today's programs are unchanged.
- The reason has no effect on the block's value, type, or evaluation
  (9.1:6). It is carried on the RIR `Checked` instruction and shown by the
  RIR printer and the syntax and RIR artifact views, which is the surface a
  listing of every checked site with its invariant will be built on.

## Implementation Phases

- [x] **Phase 1: reason clause on `checked` blocks, behind the preview** -
  RUE-1887
- [ ] **Phase 2: the same clause on `unchecked fn`**, stating the
  precondition the caller upholds. The issue asks for it; the spelling is
  not yet ruled on. The candidate consistent with phase 1 is
  `unchecked "precondition" fn name(...)`: the reason follows the keyword it
  justifies.
- [ ] **Phase 3: a query that lists every checked site with its reason**,
  the audit surface for a reviewer of agent-written code.
- [ ] **Phase 4: stabilization**: the reason becomes mandatory everywhere,
  the standard library, tests, examples, tutorial, and corpus are ported,
  and the preview gate is removed. RUE-195 (the keyword names) should be
  settled first, since the clause attaches to whichever keyword survives.

## Consequences

### Positive

- Every unchecked site states what it relies on, in a place tooling can read.
- No new semantics: the reason is inert at run time and in comptime.

### Negative

- Ceremony on every `checked` block once stabilized. The standard library has
  many; the port is mechanical and the reasons already exist as comments in
  most of them.

## References

- [Specification 9.1](../spec/src/09-unchecked-code/01-syntax.md)
- [ADR-0028: Unsafe code and raw pointers](0028-unsafe-and-raw-pointers.md)
- [ADR-0005: Preview Features](0005-preview-features.md)
