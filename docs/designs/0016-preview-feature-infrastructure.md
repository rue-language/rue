---
id: 0016
title: Preview Feature Infrastructure
status: implemented
tags: [infrastructure, process]
feature-flag: preview-features
created: 2025-12-27
accepted: 2025-12-27
implemented: 2025-12-27
spec-sections: []
superseded-by:
---

# ADR-0016: Preview Feature Infrastructure

## Status

Implemented

## Summary

Complete the preview feature infrastructure by threading `PreviewFeatures` through the compiler pipeline to `Sema`, implementing the `require_preview()` gating method, and introducing a permanently-unstable `test_infra` feature as both a testing fixture and usage example.

## Context

ADR-0005 introduced the preview feature system design, but the implementation was incomplete:

1. **Missing pipeline threading**: The `--preview` flag is parsed at the CLI and stored in `CompileOptions`, but `preview_features` never reaches `Sema` where feature gates are checked.

2. **No gating mechanism**: `Sema` lacks a `require_preview()` method, so there's no way to actually gate feature usage.

3. **Empty enum instability**: When `MutableStrings` was stabilized (making `PreviewFeature` empty), various places broke because `match` on an empty enum has edge cases. Having at least one permanent variant solves this.

4. **No example usage**: New contributors have no working example of how to gate a feature at each compiler stage.

The current data flow is:
```
CLI (--preview flag) → CompileOptions.preview_features → compile_with_options() → [dead end]
```

What we need:
```
CLI → CompileOptions → compile_frontend_*() → Sema::new(preview_features) → require_preview() checks
```

### Why Sema is Sufficient

Later stages (CfgBuilder, codegen, regalloc, emit, link) do **not** need preview feature access because:

1. **Sema is the decision point**: It decides what semantics to apply and encodes those decisions in the typed AIR.

2. **Later stages are implementation points**: They process the AIR that Sema already approved. They don't make policy decisions.

3. **Feature semantics become data**: For example, inout parameters are represented as `param_modes: Vec<bool>` in AIR/CFG. Codegen doesn't ask "is inout enabled?" - it sees `param_modes[i] = true` and generates appropriate code.

4. **Rejected code never reaches codegen**: If Sema errors on ungated feature usage, later stages never see it.

This architecture is intentional and matches how Zig and Rust handle feature gating.

## Decision

### 1. Thread `PreviewFeatures` to Sema

Add `preview_features` to the compilation pipeline:

```rust
// rue-compiler/src/lib.rs
pub fn compile_frontend_with_options(
    source: &str,
    opt_level: OptLevel,
    preview_features: &PreviewFeatures,  // NEW
) -> CompileResult<CompileState>

pub fn compile_frontend_from_ast_with_options(
    ast: Ast,
    opt_level: OptLevel,
    preview_features: &PreviewFeatures,  // NEW
) -> CompileResult<CompileState>
```

Update the call in `compile_with_options()` to pass the features through.

### 2. Add `preview_features` to Sema

```rust
// rue-air/src/sema.rs
pub struct Sema<'a> {
    // ... existing fields ...
    preview_features: PreviewFeatures,
}

impl<'a> Sema<'a> {
    pub fn new(
        rir: &'a Rir,
        interner: &'a mut Interner,
        preview_features: PreviewFeatures,  // NEW
    ) -> Self {
        Self {
            // ... existing fields ...
            preview_features,
        }
    }

    /// Check that a preview feature is enabled, returning an error if not.
    pub fn require_preview(
        &self,
        feature: PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        if !self.preview_features.contains(&feature) {
            return Err(CompileError::new(
                ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_string(),
                },
                span,
            ).with_help(format!(
                "compile with `--preview {}` to enable this feature",
                feature.name()
            )));
        }
        Ok(())
    }
}
```

### 3. Introduce `test_infra` permanently-unstable feature

Add a new preview feature that is never stabilized:

```rust
// rue-error/src/lib.rs
pub enum PreviewFeature {
    /// **Permanently unstable** - used only for testing the preview feature
    /// infrastructure. This feature should never be stabilized.
    ///
    /// When enabled, the intrinsic `@test_preview_gate()` becomes available.
    /// This intrinsic does nothing but serves to verify the gating mechanism works.
    TestInfra,
}
```

This feature:
- Provides a permanent variant so the enum is never empty
- Serves as documentation/example for how to implement feature gates
- Can be used in CI to verify the infrastructure works
- Gates a trivial no-op intrinsic (`@test_preview_gate()`) that can be used in tests

### 4. Add `@test_preview_gate()` intrinsic

A minimal gated intrinsic that:
- Requires `--preview test_infra` to use
- Takes no arguments, returns `()`
- Does nothing at runtime
- Exercises the full pipeline from lexer through codegen

```rue
// Only compiles with --preview test_infra
fn main() -> i32 {
    @test_preview_gate();
    0
}
```

### 5. Update test helpers

Add preview feature support to the in-process test helpers:

```rust
// rue-compiler/src/lib.rs (test module)
pub fn compile_to_air(source: &str) -> CompileResult<SemaOutput> { ... }
pub fn compile_to_air_with_preview(
    source: &str,
    features: PreviewFeatures,
) -> CompileResult<SemaOutput> { ... }
```

## Implementation Phases

- [x] **Phase 1: Pipeline threading** - rue-0slf.1
- [x] **Phase 2: Gating method** - rue-0slf.2
- [x] **Phase 3: test_infra feature** - rue-0slf.3
- [x] **Phase 4: Cleanup** - rue-0slf.4

## Consequences

### Positive

- **Complete infrastructure**: Preview features now actually work end-to-end
- **Stable enum**: `PreviewFeature` always has at least one variant
- **Living documentation**: `test_infra` shows exactly how to gate a feature
- **CI validation**: Infrastructure can be tested without depending on real features
- **Simpler stabilization**: Future features can be stabilized without worrying about empty enum edge cases

### Negative

- **Extra variant**: One permanently-unstable feature exists just for testing
- **Trivial intrinsic**: `@test_preview_gate()` has no real use beyond testing

### Neutral

- This is purely infrastructure; no user-facing language changes

## Open Questions

None - this is a straightforward infrastructure completion.

## Future Work

- Consider adding `--preview all` for convenience during development
- Add a CI job that verifies preview feature gating works correctly

## References

- ADR-0005: Preview Features (original design)
- ADR-0014: Mutable Strings (first feature to use the system, now stabilized)
