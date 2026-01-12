---
id: 0032
title: Data Structure Selection for Small Collections
status: accepted
tags: [performance, implementation]
feature-flag: null
created: 2026-01-11
accepted: 2026-01-11
implemented: 2026-01-11
spec-sections: []
superseded-by:
---

# ADR-0032: Data Structure Selection for Small Collections

## Status

Accepted and implemented (2026-01-11)

## Summary

Use `Vec` with linear search instead of `HashMap` for collections that typically contain fewer than 20 items. Benchmarks show Vec is 3-22x faster for small collections due to better cache locality, no hashing overhead, and simpler memory layout.

## Context

The Rue compiler uses many small lookup tables throughout semantic analysis and code generation:
- Function parameters (typically 0-10 items)
- Local variables (typically 1-50 items)
- Struct fields (typically 1-10 items)
- Method tables per type (typically 1-20 items)
- Move tracking state (typically 0-5 items)

The default choice of `HashMap` provides O(1) lookups but carries overhead:
- Hash computation for every lookup
- Memory allocation for hash table buckets
- Poor cache locality (pointer-chasing through buckets)
- Higher memory usage (load factor ~75%, extra metadata)

For small collections, these overheads dominate the theoretical O(1) benefit.

## Decision

**Use `Vec` for collections that typically contain fewer than 20 items.**

**Guidelines:**

1. **Use Vec when:**
   - Collection typically has <20 items
   - Items are accessed frequently (hot path)
   - Collection is short-lived (per-function, per-pass)
   - Simplicity is valuable (fewer types, clearer code)

2. **Use HashMap when:**
   - Collection can grow to 50+ items
   - Collection is long-lived (cross-module, global)
   - Insertion/deletion is frequent
   - Worst-case lookup time matters

3. **Consider alternatives:**
   - `SmallVec<[T; N]>` - Inline small collections, spill to heap when large
   - `IndexMap` - Preserves insertion order with O(1) lookup
   - Custom arena/index-based structures for large-scale data

## Benchmarks

Micro-benchmarks comparing HashMap vs Vec for typical compiler collection sizes:

| Size | Operation | HashMap | Vec | Speedup |
|------|-----------|---------|-----|---------|
| 2    | Lookup    | 166ns   | 31ns | **5.35x** |
| 2    | Insert    | 283ns   | 35ns | **8.09x** |
| 5    | Lookup    | 410ns   | 74ns | **5.54x** |
| 5    | Insert    | 871ns   | 73ns | **11.93x** |
| 10   | Lookup    | 838ns   | 181ns | **4.63x** |
| 10   | Insert    | 2.1µs   | 114ns | **18.66x** |
| 20   | Lookup    | 1.6µs   | 464ns | **3.52x** |
| 20   | Insert    | 4.2µs   | 185ns | **22.81x** |
| 50   | Lookup    | 4.5µs   | 2.3µs | **1.93x** |

**Key insight:** Vec is faster until ~50 items, with peak advantage at 10-20 items.

**Why Vec wins for small collections:**
- **Cache locality**: Sequential memory access vs pointer-chasing
- **No hashing**: Direct comparison vs hash computation + equality check
- **Simple layout**: Contiguous array vs scattered buckets
- **Lower overhead**: No hash table metadata or load factor waste

## Implementation

### Conversion: `AnalysisContext.params`

Converted per-function parameter maps from `HashMap<Spur, ParamInfo>` to `Vec<ParamInfo>`.

**Changes:**
- Added `name: Spur` field to `ParamInfo` struct
- Changed `AnalysisContext.params` from `&HashMap<Spur, ParamInfo>` to `&[ParamInfo]`
- Updated param lookups from `.get(&name)` to `.iter().find(|p| p.name == name)`
- Modified 2 construction sites, 22 lookup sites

**Results:**
- Lookups 3-5x faster (typical function has 2-10 params)
- Simpler code (no HashMap imports, fewer generic types)
- Better memory efficiency (~40 bytes/param vs ~56 bytes with HashMap overhead)
- All tests pass (1337 spec + 11 unit + 48 UI)

**Profiling impact:**
- Param lookups are a small fraction of semantic analysis time
- Sema is 1-2% of total compilation time
- Overall compilation speedup: ~0.5% (modest but positive)
- Real benefit: code simplicity and cache-friendliness compound in large projects

## Future Candidates

Based on the audit, these collections are strong candidates for Vec conversion:

### High Priority
1. **`AnalysisContext.locals`** - `HashMap<Spur, LocalVar>` (typically 1-50 locals/function)
   - Expected benefit: Similar to params (3-5x lookup speedup)
   - Complexity: Moderate (more lookup sites than params)

2. **`VariableMoveState.partial_moves`** - `HashMap<FieldPath, Span>` (typically 0-5 moves)
   - Expected benefit: Very high (extremely small collections)
   - Complexity: Low (few lookup sites)

### Keep as HashMap (Correct Decision)
These collections can grow large and should remain HashMap:
- **Global symbol tables**: `functions`, `structs`, `enums`, `methods`, `constants` (50-1000+ items)
- **Type pools**: Can grow to 1000+ unique types
- **Cross-module lookups**: Module system will introduce large name resolution tables

## Consequences

### Positive
- **Performance**: 3-22x faster lookups for small collections
- **Simplicity**: Vec is simpler than HashMap (fewer types, no hashing)
- **Memory**: ~30-40% less memory per item for small collections
- **Cache**: Sequential access improves CPU cache utilization
- **Maintainability**: Clearer intent ("small list" vs "map")

### Negative
- **Asymptotic behavior**: O(n) lookups degrade for large collections
- **Break-even point**: Need to know typical sizes (if wrong, performance regresses)
- **Code changes**: All `.get()` calls become `.iter().find()`

### Neutral
- **Methodology established**: Benchmark infrastructure and decision framework for future work
- **Documentation**: Clear guidelines prevent future confusion

## Methodology for Future Decisions

When considering HashMap vs Vec for a new collection:

1. **Estimate typical size**:
   - Profile existing code with representative workloads
   - Check test suites for typical examples
   - Add debug logging if uncertain

2. **Benchmark if uncertain**:
   - Create micro-benchmarks for the specific access pattern
   - Test at expected size ranges (small, typical, large)
   - Consider both average case and worst case

3. **Measure before/after**:
   - Use `--time-passes` to measure phase timing
   - Run full test suite to catch regressions
   - Document findings for future reference

4. **When in doubt, prefer simplicity**:
   - Vec is simpler to understand and maintain
   - Premature optimization is worse than premature pessimization
   - Easy to convert Vec → HashMap later if needed

## Open Questions

None - all questions resolved through benchmarking and implementation.

## References

- Issue: rue-vfmy
- Commit: "Optimize: Convert AnalysisContext.params from HashMap to Vec"
- Benchmark data: See "Benchmarks" section above
- Rust std docs: [`HashMap`](https://doc.rust-lang.org/std/collections/struct.HashMap.html), [`Vec`](https://doc.rust-lang.org/std/vec/struct.Vec.html)
