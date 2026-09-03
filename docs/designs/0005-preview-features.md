---
id: 0005
title: Preview Features
status: implemented
tags: [process]
feature-flag: preview-features
created: 2025-01-01
accepted: 2025-01-01
implemented: 2025-12-27
spec-sections: ["6.3:21", "6.3:22"]
superseded-by:
---

<!-- Note: This is the ADR that introduces the preview feature system itself.
     Ironically, it has a placeholder feature-flag to satisfy its own schema. -->

# ADR-0005: Preview Features

## Status

Implemented

## Summary

Introduce a preview feature gating mechanism that allows in-progress language features to be merged to main behind flags, enabling incremental development and test-driven implementation.

## Context

Large features in Rue often require multiple implementation phases spanning several commits or development sessions. Examples include:

- **Inout parameters** (future): Parser changes, exclusivity analysis, codegen calling convention
- **Traits** (future): Trait definitions, impl blocks, method resolution

When implementing these features in a single commit, several problems arise:

1. **All-or-nothing testing**: Tests only exist for the complete feature, so partial implementations can't be validated
2. **Hard to debug**: When tests fail, it's unclear which phase introduced the bug
3. **Can't ship incrementally**: Work-in-progress can't be merged without breaking stable functionality
4. **Context loss**: If development pauses, the incomplete state is hard to resume

We need a way to:
- Write tests for a feature before it's complete
- **Merge partial implementations to main** without breaking stable Rue
- Track which features are in-progress vs stable
- Clearly communicate to users what's experimental

The key goal is **continuous integration of incomplete work**. Each commit—even for a partially-implemented feature—should be mergeable to main. This allows:
- Progress to be preserved across development sessions
- Multiple contributors to collaborate on large features
- Bisection to work for debugging regressions
- No long-lived feature branches that diverge from main

## Decision

Introduce **preview features** - a gating mechanism for in-progress language features.

### Compiler Flag

```bash
rue --preview inout_params source.rue output
```

Code using preview features without the flag produces a clear error:

```
error: inout parameters require preview feature `inout_params`
  --> source.rue:1:12
   |
 1 | fn foo(inout x: i32) { }
   |        ^^^^^
   |
   = help: compile with `--preview inout_params` to enable
   = note: preview features may change or be removed
```

### Feature Registry

```rust
// rue-compiler/src/features.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PreviewFeature {
    // Add preview features here as needed
    // Example: InoutParams,
}

impl PreviewFeature {
    pub fn name(&self) -> &'static str {
        match *self {
            // Example: PreviewFeature::InoutParams => "inout_params",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            // Example: "inout_params" => Some(PreviewFeature::InoutParams),
            _ => None,
        }
    }

    pub fn adr(&self) -> Option<&'static str> {
        match *self {
            // Example: PreviewFeature::InoutParams => Some("ADR-0006"),
        }
    }
}
```

### Semantic Analysis Gates

When analyzing code that requires a preview feature:

```rust
// In sema.rs - example for a hypothetical feature
fn analyze_param(&mut self, param: &Param, span: Span) -> Result<...> {
    if param.is_inout {
        self.require_preview(PreviewFeature::InoutParams, "inout parameters", span)?;
        // ... proceed with implementation
    }
}

fn require_preview(&self, feature: PreviewFeature, what: &str, span: Span) -> Result<(), CompileError> {
    if !self.options.preview_features.contains(&feature) {
        return Err(CompileError::new(
            ErrorKind::PreviewFeatureRequired { feature, what: what.to_string() },
            span,
        ));
    }
    Ok(())
}
```

### Spec Test Integration

Tests can declare which preview feature they require:

```toml
[[case]]
name = "inout_basic"
preview = "inout_params"
source = """
fn increment(inout x: i32) {
    x = x + 1;
}
fn main() -> i32 {
    let mut n = 41;
    increment(inout n);
    n
}
"""
exit_code = 42
```

The test runner:
1. **Always runs preview tests** (compiling with the appropriate `--preview` flag)
2. **Treats ordinary assertion failures as expected** until marked `preview_should_pass`
3. **Fails on fatal subprocess errors and unexpected passes** so infrastructure failures and stale xfail metadata cannot hide
4. **Reports progress** on each preview feature

### Stabilization

When all tests for a preview feature pass:

1. Remove `preview = "..."` from spec tests
2. Remove the `require_preview()` gate from sema
3. Remove the feature from the `PreviewFeature` enum
4. Update the ADR status to "Implemented"

The feature is now part of stable Rue.

### Stabilized features

The following preview features completed this process and are now stable (their
`--preview` gates and `PreviewFeature` variants were removed):

- `array_repeat` — the `[value; count]` array-repeat literal (RUE-235), gated
  under this ADR; stabilized 2026-07-03.
- `for_loops` and `method_receivers` — see ADR-0037; stabilized 2026-07-03.
- `enum_payloads` — see ADR-0038; stabilized 2026-07-03.
- `string_trio` — see ADR-0043; stabilized by RUE-876 on 2026-07-14.
- `slices` — see ADR-0043; stabilized by RUE-936 on 2026-08-20.
- `borrow_accessors` — see ADR-0062; stabilized by RUE-1018 on 2026-08-21.

`test_infra` remains permanently unstable (it exists only to exercise the gating
mechanism), so the `PreviewFeature` enum is not empty.

### Active preview features

- `non_exhaustive_enums` — `@non_exhaustive` public enums (RUE-1104). The
  directive is an opt-in source/API compatibility promise: matches from an
  importing module must include a wildcard, while matches in the defining
  module remain exhaustively checked against the variants known there. Adding
  variants can still change layout, ABI, or runtime behavior, so the promise
  does not provide a binary compatibility guarantee.
- `test_declarations` — the `test "name" { ... }` language item (RUE-1618). See
  ADR-0083, which owns the feature. The gate covers a grammar change, so any
  request whose module closure contains a test declaration needs the flag,
  executable builds included; `rue test` will not enable it implicitly. It
  gates semantic analysis and rooting only — pre-semantic requests
  (`--emit tokens|ast|rir|deps`) present test items without the flag, exactly
  as float literals do.

## Implementation Phases

- [x] **Phase 1: Infrastructure** - Add PreviewFeature enum, CompileOptions, CLI flag, error kind
- [x] **Phase 2: Test runner** - Add preview field to test schema, update runner to handle preview tests
- [x] **Phase 3: First feature** - Gate a new feature with the system (test_infra, see ADR-0016)

## Consequences

### Positive

- **Incremental progress**: Ship partial implementations behind gates
- **Test-driven**: Write tests before implementation is complete
- **Clear status**: Users know what's stable vs experimental
- **Resumable**: Can pause and resume feature work across sessions
- **Debuggable**: Smaller commits with focused changes

### Negative

- **Infrastructure overhead**: More code to maintain
- **Gate maintenance**: Must remember to remove gates on stabilization
- **Two test modes**: Mental overhead of stable vs preview

## Open Questions

- Should we support multiple `--preview` flags, or a comma-separated list?

## Future Work

- Preview feature progress dashboard
- Automatic tracking of preview feature test pass rates

## References

- Rust's feature gates and editions
- TC39 proposal stages
