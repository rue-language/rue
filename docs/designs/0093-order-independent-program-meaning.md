---
id: 0093
title: "Order-independent program meaning"
status: accepted
tags: [language, semantics, modules, principle]
feature-flag: null
created: 2026-09-10
accepted: 2026-09-10
implemented:
spec-sections: ["10.5:5"]
superseded-by:
relates: ["ADR-0045", "ADR-0063", "ADR-0066", "RUE-1100", "RUE-1903"]
---

# ADR-0093: Order-independent program meaning

## Status

Accepted on 2026-09-10 by Steve. This ratifies semantic behavior already
provided by the compiler and adds no implementation phase or preview feature.

## Decision

For a selected target, a program's semantic meaning is determined by its root
module and the rooted set of declarations reached through each module's
explicit imports, independent of top-level declaration order, import order,
which importer or analyzer runs first, lazy body-demand order, or source-
manifest entry order. Relocating a module-level constant import within its file
preserves its file-scope binding; local `let` scope remains lexical. This is a
semantic-meaning guarantee and does not promise byte-identical binaries or
unchanged source positions after edits. This rule does not change the existing
ordering semantics of statements, parameters, fields, enum variants, or
effectful calls. The rule follows the rooted, demand-driven and stable identity
boundaries of ADR-0045, ADR-0063, and ADR-0066; RUE-1100's explicit module-
binding syntax remains the authority for import dependencies.

## References

- [Program Composition](../spec/src/10-modules/05-program-composition.md)
- [ADR-0045: Lazy semantic analysis](0045-lazy-semantic-analysis.md)
- [ADR-0063: Parallel demand-driven incremental compilation](0063-parallel-demand-driven-incremental-compilation.md)
- [ADR-0066: Producer-nominal anonymous types and incremental locality](0066-producer-nominal-anonymous-types-and-incremental-locality.md)
- RUE-1100: explicit module-binding syntax and import discovery authority
