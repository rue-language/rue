---
id: 0090
title: "No implicit conversions: typed values cross boundaries explicitly"
status: accepted
tags: [language, types, semantics, principle]
feature-flag: null
created: 2026-09-10
accepted: 2026-09-10
implemented:
spec-sections: ["3.11"]
superseded-by:
relates: ["RUE-1895"]
---

# ADR-0090: No implicit conversions: typed values cross boundaries explicitly

## Status

Accepted on 2026-09-10 by Steve. This record ratifies the existing type-boundary
policy and adds no conversion behavior or preview feature.

## Decision

Rue does not implicitly convert an already-typed value between distinct concrete
types: integer widths and signedness domains, floating-point widths, integer and
floating-point domains, string representations, option wrapping, and
user-defined types require an explicit conversion intrinsic or constructor. The
never type remains the sole general inference coercion, while contextual typing
of untyped literals is inference; explicit `borrow`/`inout` view materialization
for strings and slices remains a parameter-mode operation under the rules cited
in 3.11 and is not a first-class value conversion. This keeps typed boundaries
self-describing and prevents hidden loss, allocation, ownership, or runtime
work; comptime specialization and fixed numeric operator intrinsics do not
change that rule.

## References

- [Type Inference](../spec/src/03-types/11-type-inference.md)
- [ADR-0007: Hindley–Milner type inference](0007-hindley-milner-inference.md)
- [ADR-0043: The collection/string type trio](0043-collection-string-type-trio.md)
