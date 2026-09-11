---
id: 0092
title: "No configuration declarations: target behavior through comptime"
status: accepted
tags: [language, semantics, comptime, principle]
feature-flag: null
created: 2026-09-10
accepted: 2026-09-10
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-1900"]
---

# ADR-0092: No configuration declarations: target behavior through comptime

## Status

Accepted on 2026-09-10 by Steve.

## Decision

Rue refuses `cfg` attributes, package feature flags, and build-provided symbol
sets that add or remove source declarations or make declaration resolution vary.
Target queries such as `@target_os`, `@target_arch`, and
`@target_data_model` supply comptime values; existing comptime control-flow and
lazy semantic-analysis rules select and analyze reached bodies, while loaded
modules are parsed and syntactic import discovery remains the source-loader's
policy. Optional behavior uses ordinary declarations and explicitly imported
modules and functions. This principle does not remove compiler target
selection, optimization or debug options, or ADR-0005 preview gates: preview
gates control availability of unfinished language facilities, while this ADR
defines the source declaration boundary.

## References

- [ADR-0005: Preview Features](0005-preview-features.md)
- [ADR-0034: Per-Target Runtime Archives for Cross-Compilation](0034-cross-target-runtime.md)
- [ADR-0045: Lazy semantic analysis](0045-lazy-semantic-analysis.md)
- [ADR-0047: Root-module compilation units and build-system inputs](0047-root-module-build-inputs.md)
- [ADR-0063: Parallel demand-driven incremental compilation](0063-parallel-demand-driven-incremental-compilation.md)
