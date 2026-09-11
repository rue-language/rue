---
id: 0087
title: "No function overloading: one name, one signature per scope"
status: accepted
tags: [language, semantics, principle]
feature-flag: null
created: 2026-09-10
accepted: 2026-09-10
implemented:
spec-sections: ["6.1:44"]
superseded-by:
relates: ["RUE-1894"]
---

# ADR-0087: No function overloading: one name, one signature per scope

## Status

Accepted on 2026-09-10 by Steve. This record formalizes the current duplicate
callable-name behavior as a permanent language principle and does not add a
preview feature or change the implementation.

## Decision

Each callable name denotes one signature within its module or type scope:
functions, methods, and associated functions cannot be overloaded by parameter
type, parameter count, return type, or parameter mode, so same-scope duplicate
callable names are rejected regardless of how their signatures differ. This
keeps name resolution and tooling from searching overload candidates; APIs
that need different behavior use distinct names or explicit value/type
dispatch. The same spelling remains valid in distinct module or type scopes.
Comptime specialization creates instances of one generic declaration, and
fixed numeric operator intrinsics are language primitives, not user-defined
overloads. Future interface primitive tiers are a separate design direction,
not a claim of shipped functionality.

## References

- [Specification rule 6.1:44](../spec/src/06-items/01-functions.md)
- [ADR-0025: Compile-Time Execution (comptime)](0025-comptime.md)
- [ADR-0066: Producer-nominal anonymous types and incremental locality](0066-producer-nominal-anonymous-types-and-incremental-locality.md)
