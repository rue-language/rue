# ADR-020: Preview Features

## Status

Proposed

## Context

Large features in Rue often require multiple implementation phases spanning several commits or development sessions. Examples include:

- **Mutable strings** (ADR-019): Runtime allocator, 3-tuple representation, clone semantics, drop insertion, concatenation
- **Inout parameters** (future): Parser changes, exclusivity analysis, codegen calling convention
- **Enums** (future): New type kind, pattern matching, memory layout

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
rue --preview mutable_strings source.rue output
```

Code using preview features without the flag produces a clear error:

```
error: string concatenation requires preview feature `mutable_strings`
  --> source.rue:3:13
   |
 3 |     let c = "a" + "b";
   |             ^^^^^^^^^
   |
   = help: compile with `--preview mutable_strings` to enable
   = note: preview features may change or be removed; see ADR-019
```

### Feature Registry

```rust
// rue-compiler/src/features.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PreviewFeature {
    MutableStrings,
}

impl PreviewFeature {
    pub fn name(&self) -> &'static str {
        match self {
            PreviewFeature::MutableStrings => "mutable_strings",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "mutable_strings" => Some(PreviewFeature::MutableStrings),
            _ => None,
        }
    }

    pub fn adr(&self) -> &'static str {
        match self {
            PreviewFeature::MutableStrings => "ADR-019",
        }
    }
}
```

### Compile Options

```rust
pub struct CompileOptions {
    pub target: Target,
    pub linker: LinkerMode,
    pub preview_features: HashSet<PreviewFeature>,
}
```

### Semantic Analysis Gates

When analyzing code that requires a preview feature:

```rust
// In sema.rs
fn analyze_binary_op(&mut self, op: BinOp, lhs: AirRef, rhs: AirRef, span: Span) -> Result<...> {
    if op == BinOp::Add && lhs_type == Type::String && rhs_type == Type::String {
        self.require_preview(PreviewFeature::MutableStrings, "string concatenation", span)?;
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
name = "string_concat_basic"
preview = "mutable_strings"
source = """
fn main() -> i32 {
    let c = "hello" + " world";
    if c == "hello world" { 1 } else { 0 }
}
"""
exit_code = 1
```

The test runner:
1. **Always runs preview tests** (compiling with the appropriate `--preview` flag)
2. **Allows preview tests to fail** without failing the overall suite
3. **Reports progress** on each preview feature

### Test Runner Output

```
Running spec tests...

=== Stable Tests ===
expressions.arithmetic::add_basic                    ... ok
expressions.arithmetic::sub_basic                    ... ok
types.strings::string_literal_eq                     ... ok
...
Stable: 750 passed, 0 failed

=== Preview: mutable_strings ===
types.strings::string_concat_basic                   ... ok
types.strings::string_concat_chained                 ... FAILED
types.strings::string_copy_semantics                 ... FAILED
...
mutable_strings: 5/18 passed (27%)

=== Summary ===
Stable: 750/750 PASSED
Preview: mutable_strings 5/18 (27%)

Result: PASS (all stable tests passed)
```

### Stabilization

When all tests for a preview feature pass:

1. Remove `preview = "..."` from spec tests
2. Remove the `require_preview()` gate from sema
3. Remove the feature from the `PreviewFeature` enum
4. Update the ADR status to "Stable"

The feature is now part of stable Rue.

### Verifying Gates Work

For each preview feature, add a stable test that verifies the gate:

```toml
[[case]]
name = "string_concat_requires_preview"
source = """
fn main() -> i32 {
    let c = "a" + "b";
    0
}
"""
compile_fail = true
error_contains = "requires preview feature"
```

This test runs without any preview flag and ensures the error message is correct.

## Feature Size Guidelines

Not every feature needs preview gating. Use this heuristic:

### Small Features (no preview needed)

- Touches 1-3 files
- Single concept (new operator, syntax sugar)
- No new runtime functions
- No new IR instructions
- Completable in one session/commit

Examples: `%` operator, `else if` syntax, unary `+`

### Large Features (preview required)

- Touches many files across multiple crates
- Multiple implementation phases
- New runtime support functions
- New IR instruction kinds
- New type system concepts
- Multi-session implementation

Examples: mutable strings, inout parameters, enums, traits

### Decision Process

When starting a feature, ask:
1. Does this need an ADR? → Probably needs preview
2. Does this add runtime functions? → Probably needs preview
3. Does this change the type system? → Probably needs preview
4. Can I finish this in one commit with confidence? → Probably doesn't need preview

## Implementation Plan

### Phase 1: Infrastructure

1. Add `PreviewFeature` enum to `rue-compiler`
2. Add `preview_features` to `CompileOptions`
3. Add `--preview` flag to CLI argument parsing
4. Add `PreviewFeatureRequired` error kind

### Phase 2: Test Runner

1. Add `preview` field to spec test case schema
2. Update test runner to pass `--preview` when running preview tests
3. Update test runner to allow preview test failures
4. Add progress reporting for preview features

### Phase 3: Migration

1. Add `preview = "mutable_strings"` to existing mutable string tests
2. Add gates to sema for mutable string operations
3. Verify stable tests still pass

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

### Mitigations

- Keep the feature registry simple (just an enum)
- Stabilization checklist in ADRs
- Test runner makes the distinction clear

## Alternatives Considered

### Expected Failures Only

Mark tests as `expected_fail = true` without compiler gates.

Rejected because:
- Can't actually use the feature (no `--preview` flag)
- Can't ship incremental working functionality
- Just tracks "this is broken" not "this is in progress"

### Nightly/Stable Channels

Separate compiler builds like Rust.

Rejected because:
- Too much infrastructure for a small project
- "Nightly" implies daily releases, which isn't our cadence
- Preview features are simpler and sufficient

### No Gating

Just implement features and fix bugs.

Rejected because:
- This is what we tried with mutable strings
- 18 failing tests with no isolation of what's broken
- Can't ship partial progress

## Related

- ADR-019: Mutable Strings (first feature to use preview gating)
- `.claude/commands/feature.md` (workflow for implementing features)
