---
id: 0018
title: Tracing Infrastructure
status: proposal
tags: [infrastructure, tooling]
feature-flag: n/a
created: 2025-12-27
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0018: Tracing Infrastructure

## Status

Proposal

## Summary

Add the `tracing` crate to Rue for structured debug logging, following the "wide events" philosophy from loggingsucks.com. This provides rich, context-aware logging per compilation pass rather than scattered debug statements, and also implements the `--time-passes` feature (rue-uxgx).

## Context

Currently Rue has no formal logging infrastructure. Debug output uses ad-hoc `println!`/`eprintln!` calls scattered throughout the codebase. This makes debugging compiler issues difficult and provides no structured way to analyze compilation performance.

The "wide events" philosophy (from loggingsucks.com) advocates for:
1. **Canonical log lines** - One rich, structured log per operation containing all debugging context
2. **Structured format** - Key-value pairs (JSON) instead of plain strings for queryability
3. **High-cardinality data** - Include contextual data like function names, file sizes, instruction counts

For a compiler, this translates to emitting one structured span per compilation pass with rich context (timing, counts, outcomes).

Additionally, there's an open feature request (rue-uxgx) for `--time-passes` to show compilation timing. The `tracing` crate provides timing spans naturally, so both needs are addressed by the same infrastructure.

## Decision

Add `tracing` and `tracing-subscriber` as dependencies and instrument the compiler pipeline.

### CLI Interface

```bash
# Normal compilation (no logging by default)
rue source.rue output

# Show timing per pass
rue --time-passes source.rue output

# Enable debug logging
rue --log-level=debug source.rue output
RUST_LOG=debug rue source.rue output

# JSON format for tooling integration
rue --log-format=json --log-level=debug source.rue output

# Filter to specific module
RUST_LOG=rue_compiler::sema=trace rue source.rue output
```

### --time-passes Output

```
=== Compilation Timing ===

  Lexer:              0.2ms (  1%)
  Parser:             1.1ms (  5%)
  AST generation:     0.8ms (  4%)
  Semantic analysis:  3.2ms ( 15%)
  CFG construction:   1.5ms (  7%)
  CFG lowering:       2.1ms ( 10%)
  Register alloc:     8.4ms ( 40%)
  Emission:           2.8ms ( 13%)
  Linking:            1.0ms (  5%)
  --------------------------------
  Total:             21.1ms (100%)
```

### Instrumentation Approach

Each compilation pass gets a tracing span:

```rust
use tracing::{info_span, info};

pub fn compile_frontend_with_options(...) -> CompileResult<...> {
    let _span = info_span!("compile", file = %source_path, size = source.len()).entered();

    let tokens = {
        let _span = info_span!("lexer").entered();
        lex(&source)?
    };
    info!(token_count = tokens.len(), "lexing complete");

    let ast = {
        let _span = info_span!("parser").entered();
        parse(&tokens)?
    };
    info!(function_count = ast.functions.len(), "parsing complete");

    // ... etc
}
```

### Logging Level Guidelines

| Level | Use for |
|-------|---------|
| `error` | Compilation failures, internal compiler errors |
| `warn` | Suspicious patterns (already surfaced via diagnostics) |
| `info` | Per-pass completion with summary metrics |
| `debug` | Decision points, intermediate state |
| `trace` | Detailed internal state, individual instructions |

## Implementation Phases

- [x] **Phase 1: Add dependencies** - rue-irz1.1
  - Update `third-party/Cargo.toml` with tracing, tracing-subscriber
  - Run `reindeer buckify`
  - Update crate BUCK files

- [x] **Phase 2: CLI and subscriber** - rue-irz1.2
  - Initialize tracing-subscriber in main.rs
  - Add `--log-level` flag (off/error/warn/info/debug/trace)
  - Add `--log-format` flag (text/json)
  - Support `RUST_LOG` environment variable

- [x] **Phase 3: --time-passes** - rue-irz1.3
  - Implement `--time-passes` using tracing spans
  - Collect timing from spans and format output
  - Closes rue-uxgx

- [x] **Phase 4: Instrument compiler** - rue-irz1.4
  - Add spans to `compile_frontend_with_options()`
  - Add spans to backend functions
  - Include context: file size, token/instruction counts

- [ ] **Phase 5: Documentation** - rue-irz1.5
  - Add logging guidelines to CLAUDE.md
  - Document the "wide events" philosophy
  - Provide good/bad examples

## Consequences

### Positive

- **Structured debugging** - Filter and query logs instead of grep
- **Performance visibility** - `--time-passes` shows where time goes
- **Consistent approach** - Single logging framework across codebase
- **Tooling integration** - JSON output enables external analysis
- **Future-proof** - Easy to add more instrumentation

### Negative

- **New dependency** - tracing + tracing-subscriber add to compile time
- **Learning curve** - Contributors need to understand tracing idioms
- **Overhead when enabled** - Some cost when logging is active (negligible when off)

## Open Questions

None - resolved during planning:
- JSON output: Yes, via `--log-format=json`
- PassTimer integration: Deleted, using tracing entirely
- --time-passes: Yes, implemented via tracing spans

## Future Work

- Memory usage tracking (`--stats` flag)
- Tracing to file for long compilations
- Integration with external observability tools

## References

- https://loggingsucks.com/ - Wide events philosophy
- https://docs.rs/tracing - Tracing crate documentation
- rue-uxgx - Original --time-passes feature request
