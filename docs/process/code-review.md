# Code Review

This document describes how we review code before committing.

## When to Review

Review before every commit. The review catches issues that are easier to fix now than after merging.

## What to Review

### 1. Correctness

- Does the code do what it's supposed to?
- Are there any bugs?
- Are edge cases handled?

### 2. Performance

Consider both:
- **Compiler performance**: Will this slow down compilation?
- **Generated code performance**: Will the compiled programs be slower?

Minor regressions may be acceptable with justification.

### 3. Style

- Follows Rust idioms
- Consistent with project conventions
- Clear variable and function names
- Appropriate comments (not too many, not too few)

### 4. Error Handling

- Appropriate error types used
- Error messages are clear and actionable
- Spans point to the right source locations

### 5. Tests

Are changes adequately tested?

| Change Type | Required Tests |
|-------------|----------------|
| Language semantics | Spec tests with `spec = [...]` references |
| Warnings/diagnostics | UI tests |
| Internal implementation | Unit tests (when behavior isn't covered by above) |

### 6. Specification

If the change affects language semantics:
- Is `docs/spec/src/` updated?
- Do spec paragraphs have proper ID markers (`{{ rule(id="X.Y:Z", cat="category") }}`)? Does the prose follow [the spec prose rubric](spec-prose-rubric.md)?
- Do spec tests reference the new paragraphs?
- Will traceability check pass (100% coverage required)?

## Rue-Specific Checks

### Index-Based References

We use u32 indices instead of pointers for cache-friendly, lifetime-free data structures. Check:
- No dangling indices (referencing removed items)
- Indices used with correct arena/vector

### IR Transformations

Ensure transformations preserve semantics:
- Types are correctly propagated
- Control flow is maintained
- Values aren't lost or duplicated incorrectly

### Span Tracking

Source locations must be maintained for error reporting:
- New IR nodes have appropriate spans
- Errors point to meaningful source locations

### Multi-Backend Consistency

If changes touch `rue-codegen`, verify equivalent changes in ALL backends:

| File | x86_64 | aarch64 |
|------|--------|---------|
| MIR definitions | `x86_64/mir.rs` | `aarch64/mir.rs` |
| Instruction emission | `x86_64/emit.rs` | `aarch64/emit.rs` |
| Register allocation | `x86_64/regalloc.rs` | `aarch64/regalloc.rs` |
| Liveness analysis | `x86_64/liveness.rs` | `aarch64/liveness.rs` |
| CFG lowering | `x86_64/cfg_lower.rs` | `aarch64/cfg_lower.rs` |

For CFG/codegen changes, apply the ADR-0048 boundary checklist as well:

- Confirm the shared `value_plan::lower_value` is the only exhaustive
  `CfgInstData` dispatcher and that the shared CFG/terminator walk is used.
- Confirm calls, intrinsics, checked arithmetic, traps, and residual values
  enter their separate normalized domain hooks. Plans must contain decided
  vregs/slot vectors and ABI facts, never raw `CfgValue` handles.
- Confirm the adapters do not recompute aggregate slot counts, block-parameter
  routing, by-reference classification/preloads, bounds ordering, or sret
  selection. Those decisions belong to the shared core.
- Inspect every same-purpose function pair in both CFG lowerers. Keep a pair
  only when it is justified by a concrete target fact such as a physical
  register, ABI location, flag/NZCV sequence, immediate encoding, MIR form,
  scheduler fact, peephole, encoder, or native-runtime entry.
- Reproduce the non-test inventory before making that comparison:
  `extract_cfg_fns() { awk '/^#\[cfg\(test\)\]/{exit} /^    (pub )?fn
  /{sub(/^    (pub )?fn /, ""); sub(/\(.*/, ""); print}' "$1" | sort -u;
  }; x86=$(mktemp); arm=$(mktemp); trap 'rm -f "$x86" "$arm"' EXIT;
  extract_cfg_fns crates/rue-codegen/src/x86_64/cfg_lower.rs >"$x86";
  extract_cfg_fns crates/rue-codegen/src/aarch64/cfg_lower.rs >"$arm";
  comm -12 "$x86" "$arm"; comm -3 "$x86" "$arm"`.
  The first output is the complete paired-name list and the second is the
  target-specific remainder. Do not count test helpers, and list shared policy
  helpers such as width and shift-mask selection separately from the
  target-fact pairs. When the two backends split one same-purpose target leaf
  under different names, add an explicit named group for it (as ADR-0048 does
  for the checked-arithmetic overflow family) rather than treating the name
  difference as proof that the semantics differ.
- Add or update a shared-plan test that covers the new semantic case and
  cross-target tests that prove both adapters consume that plan with their
  explicit target facts. Do not accept a renamed backend dispatcher or a
  generic catch-all hook as centralization.

## Review Output

Provide specific, actionable feedback:

**Blocking issues**: Must be fixed before commit
- Reference specific file:line locations
- Explain what's wrong and how to fix it

**Non-blocking improvements**: Can be addressed later
- File as Linear issues (`save_issue`, `bug` label, priority 4)
- Note in review that it's non-blocking

## Example Review

```
## Review of: Add modulo operator

### Blocking Issues

1. **Missing aarch64 implementation** - x86_64/emit.rs:234
   The modulo instruction is only implemented for x86_64. Need equivalent
   in aarch64/emit.rs.

2. **Wrong span on error** - sema.rs:567
   Division-by-zero error points to the whole expression, should point
   to the divisor operand.

### Non-Blocking (filed as issues)

- RUE-45: Consider optimizing modulo by power of 2 to bitwise AND

### Looks Good

- Spec tests cover all cases
- Type checking is correct
- x86_64 codegen is correct
```

## After Review

1. Fix all blocking issues
2. Re-run tests: `./test.sh`
3. Proceed to commit (see [committing.md](committing.md))
