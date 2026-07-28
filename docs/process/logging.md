# Logging Guidelines

Moved from CLAUDE.md (2026-06-11) to keep the always-loaded context lean.

Rue uses the `tracing` crate for structured logging, following the **"wide events"** philosophy from [loggingsucks.com](https://loggingsucks.com/). This means:

1. **Canonical log lines** - One rich, structured log per operation containing all debugging context
2. **Structured format** - Key-value pairs instead of plain strings for queryability
3. **High-cardinality data** - Include contextual data like function names, counts, sizes

### Using the Logging

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

`--time-passes` and `--benchmark-json` use inclusive spans: a parent's time
contains its children. Their total is therefore the duration of root compiler
spans, not the sum of every row. Machine-readable schema v2 includes invocation,
root, and leaf counts so consumers can distinguish aggregate spans from phase
rows. Distinct parallel leaf spans may overlap and should be interpreted as
summed work rather than wall time. Logging filters such as `RUST_LOG=error`
affect log output only; they do not disable timing collection.

Parser timing keeps `parse_file -> parser` as the canonical aggregate. The
handwritten parser reports nesting scan, parser-state setup, grammar execution,
and directive validation as direct leaves. Parsing runs on the caller thread;
there is no parser graph construction, token adaptation, or worker lifecycle.

A span may declare `driver_phase = true` to mark host work that runs outside the
compiler's timing root, such as writing the linked executable. Those spans (and
their children) are reported in `driver_phases` instead of `passes`; they break
down process-minus-root overhead and never contribute to the compiler total, so
`compile` stays the sole timing root. Reserve this for driver work that is
genuinely outside `compile` — instrument compiler work with an ordinary span.

Work that runs on Rayon workers needs its parent passed explicitly: workers do
not inherit the caller's current span, so a nested span created there would be
reported as its own timing root. Hold the parent span by value and re-enter it
as the first statement inside the closure, as `codegen` does in
`crates/rue-compiler/src/backend.rs`.

The current span tree is exposed by the driver's `--time-passes` and
`--benchmark-json` modes.

### Adding Instrumentation

Each compilation pass should have a tracing span wrapping the work:

```rust
use tracing::{info_span, info};

pub fn my_pass(input: &Input) -> Result<Output> {
    // Create a span for the pass - includes timing automatically
    let _span = info_span!("my_pass").entered();

    // Do the work...
    let result = process(input)?;

    // Log completion with useful metrics
    info!(
        item_count = result.items.len(),
        "pass complete"
    );

    Ok(result)
}
```

### Logging Levels

| Level | Use for | Example |
|-------|---------|---------|
| `error` | Compilation failures, internal compiler errors | ICE, unrecoverable errors |
| `warn` | Suspicious patterns (surfaced via diagnostics) | Deprecated feature usage |
| `info` | Per-pass completion with summary metrics | "lexing complete", token counts |
| `debug` | Decision points, intermediate state | "resolving symbol X to Y" |
| `trace` | Detailed internal state, individual instructions | Instruction-by-instruction output |

### Good vs Bad Examples

**Good: Wide event with context**
```rust
let _span = info_span!(
    "codegen",
    arch = "x86_64",
    function_count = functions.len()
).entered();

// ... do code generation ...

info!(
    code_bytes = total_bytes,
    "code generation complete"
);
```

**Bad: Scattered debug statements**
```rust
println!("Starting codegen...");
for func in functions {
    println!("Generating function: {:?}", func.name);
    // ...
}
println!("Done!");
```

**Good: Structured key-value data**
```rust
info!(
    token_count = tokens.len(),
    source_bytes = source.len(),
    "lexing complete"
);
```

**Bad: String interpolation**
```rust
println!("Lexed {} tokens from {} bytes", tokens.len(), source.len());
```

### Key Principles

1. **Spans for timing**: Wrap passes in `info_span!()` - this enables `--time-passes`
2. **Events for outcomes**: Use `info!()` after completing work with metrics
3. **Context in spans**: Include high-level context (file, function count) in span fields
4. **Metrics in events**: Include computed metrics (instruction counts, sizes) in events
5. **Zero-cost when off**: Tracing has no overhead when no subscriber is active
