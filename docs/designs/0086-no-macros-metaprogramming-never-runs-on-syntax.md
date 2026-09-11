---
id: 0086
title: "No macros: metaprogramming never runs on syntax and never introduces names"
status: accepted
tags: [language, syntax, semantics, comptime, tooling, principle]
feature-flag: null
created: 2026-09-10
accepted: 2026-09-10
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-1893", "ADR-0025", "ADR-0066", "RUE-1550", "RUE-246", "RUE-350", "RUE-1757"]
---

# ADR-0086: No macros: metaprogramming never runs on syntax and never introduces names

## Status

Accepted on 2026-09-10 by Steve. This is an approved language-design
principle and admission criterion for future metaprogramming work. It documents
the boundary for the comptime model in [ADR-0025](0025-comptime.md), subject to
the producer identity and locality constraints in
[ADR-0066](0066-producer-nominal-anonymous-types-and-incremental-locality.md).
It is policy documentation only: there is no implementation phase, preview
feature, or claim that every permitted direction below is shipped.

## Summary

Rue has no macro system. Names are written by the programmer, name resolution
depends on syntax alone, and metaprogramming computes bodies or values and
types. Comptime runs after names have been resolved and each operation is
bounded by the declarations it touches. A proposed operation that needs to
inspect or transform compiler syntax, introduce identifiers, search for
declarations, or run before name resolution is refused as a macro.

## Context

Syntax generation makes name resolution depend on expansion. That couples
otherwise independent declarations, makes IDE name information unavailable
until execution, and damages incremental locality. It also makes the method set
and identity of a type depend on executing a program that the tooling cannot
understand from declarations.

Comptime therefore operates on values and types rather than syntax. The
anonymous-type identity and dependency boundaries in ADR-0066 provide the
locality constraint: a type value can be produced and consumed without making
unrelated producers or declarations part of its identity.

## Decision

### 1. Names are always written by the programmer

Every user-visible declaration or binding name is written in source by the
programmer. Metaprogramming may produce a body, a value, or a type value; it
never produces an identifier or edits the declaration namespace.

A comptime type function may return an anonymous type. The programmer binds
that returned value to a name before using it as a named type. The function
does not choose or mint the binding name.

Future derive-style work follows the same rule. A derive operation may produce
exactly the method bodies for methods declared by a named interface, so tooling
knows the method set without executing the derive operation to discover it. It
does not add undeclared methods or names.

The following constructs are refused:

- a Builder generator that mints `PointBuilder` from `Point`;
- an operation that generates declaration or binding names from fields,
  strings, or other computed data; and
- any syntax macro whose expansion introduces declarations or bindings.

### 2. The order is fixed

The design order is:

1. parse the source;
2. resolve names from syntax alone;
3. run comptime operations; and
4. check the resulting bodies.

An operation that must run before name resolution is a macro and is refused.
This ordering is a design admission criterion for prospective features.

### 3. Metaprogramming is linear and memoizable, never search

Each metaprogramming operation is linear in the declarations it touches and
may be memoized as a fact about those declarations. It does not search the
program for declarations or repeatedly expand an unbounded space.

In particular:

- bounded generics create no instances;
- derive work occurs once per concrete type/interface pair and produces only
  the method bodies that the named interface declares; and
- enumerating the fields of a concrete type is linear in that type's fields.

Blanket conformance or implementation search, and unbounded comptime loops,
are refused. The general comptime quota and restriction design remains tracked
separately by RUE-1550; this ADR does not invent an exact quota.

## Permitted design directions

These are directions that satisfy the decision, not shipped functionality.

- Future derive-style reflection may inspect a concrete type at its
  declaration and return the declared method bodies for a named interface. It
  never reflects over a bounded type parameter and never creates names. The
  interface method-set work is ongoing under RUE-246.
- A comptime function may accept a concrete string literal for a format string
  or small DSL and return a value or a type. The result does not contain names;
  string formatting remains tracked by RUE-350, and accepting string-valued
  comptime parameters by RUE-1757.

## Implementation Phases

None. This ADR records an accepted policy; future feature ADRs must demonstrate
compliance with its three rules and own their implementation phases separately.

## Consequences

### Positive

- Name resolution and IDE name information remain available from source
  declarations without executing metaprogramming.
- Incremental invalidation stays local to the declarations and bodies an
  operation observes.
- Tooling can know a derived method set from the named interface declaration.
- Comptime remains a computation over values and types, with a bounded and
  memoizable dependency surface.

### Negative

- Repetitive declaration generation requires explicit source declarations or
  ordinary comptime-produced bodies and values.
- Libraries cannot offer Builder-style name generation, blanket implementation
  search, or syntax-driven declaration generation.
- Future reflection and DSL facilities must expose their concrete inputs and
  bounded outputs rather than accepting arbitrary syntax.

## Rejected alternatives

- **Syntax macros.** Expansion before name resolution makes the namespace and
  name lookup depend on execution, which breaks locality and tooling.
- **Identifier-producing comptime.** Returning names from strings or computed
  data creates declarations that source did not write and requires namespace
  mutation.
- **Search-based metaprogramming.** Blanket conformance search and unbounded
  loops make work depend on the size and shape of unrelated declarations and
  defeat predictable memoization.

## References

- [ADR-0025: Compile-Time Execution (comptime)](0025-comptime.md)
- [ADR-0066: Producer-nominal anonymous types and incremental locality](0066-producer-nominal-anonymous-types-and-incremental-locality.md)
- RUE-1550: general comptime quota and restrictions
- RUE-246: interface method-set work
- RUE-350: string formatting
- RUE-1757: string-valued comptime parameters
