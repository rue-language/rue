---
id: 0089
title: "No mutable globals: immutable module-level values"
status: accepted
tags: [language, semantics, ownership, principle]
feature-flag: null
created: 2026-09-10
accepted: 2026-09-10
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-1896"]
---

# ADR-0089: No mutable globals: immutable module-level values

## Status

Accepted on 2026-09-10 by Steve. This record ratifies the current module-level
value-binding policy and does not add a language feature or preview gate.

## Decision

Module-level value bindings are `const` only, so they are immutable
compile-time values; Rue does not provide ordinary mutable globals or a
mutable `static` item. Implicit access to mutable global state would hide
aliases and effects from the call signatures through which ADR-0037's
fully static exclusivity model expresses access. Future shared runtime state
must instead use immutable bindings with an explicitly designed
interior-mutability abstraction, following Rust's general approach. That
abstraction remains undesigned; this decision introduces no runtime static
item, interior-mutability API, or change to ADR-0037's enforcement model.

## References

- [Specification note: Module-level Value Bindings](../spec/src/06-items/_index.md#module-level-value-bindings)
- [ADR-0037: Exclusivity model — access-point-based, statically enforced](0037-exclusivity-model-access-point-based.md)
