---
id: 0023
title: Multi-File Compilation
status: proposal
tags: [architecture, compiler, scalability]
feature-flag: multi_file
created: 2025-12-31
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0023: Multi-File Compilation

## Status

Proposal

## Summary

Enable the Rue compiler to accept multiple source files and compile them into a single executable. This is a foundational capability that unblocks real-world programs that don't fit in a single file, and lays groundwork for the module system (ADR-0022).

## Context

### The Problem: Single-File Limitation

Today, the Rue compiler accepts exactly one source file:

```bash
rue source.rue output
```

This works for learning and small programs, but becomes limiting quickly:
- Large programs become unwieldy in a single file
- No way to share code between programs (copy-paste only)
- Can't incrementally build real projects
- Blocks progress toward a module system and standard library

### What We Need

A minimal multi-file compilation model that:
1. Accepts multiple `.rue` files on the command line
2. Compiles each file independently
3. Links them together into a single executable
4. Handles cross-file function calls

### What We Explicitly Defer

This ADR does **not** address:
- **Module syntax** (`mod`, `use`, `pub`) — that's ADR-0022
- **Visibility/privacy** — all symbols are public for now
- **Namespacing** — all symbols share a flat global namespace
- **Incremental compilation** — we rebuild everything each time
- **Build system integration** — no `rue.toml` or package manifest

### Why Flat Namespace First?

A flat namespace (all functions globally visible, no `mod`/`use`) is simpler to implement and provides immediate value:

1. **Low implementation cost** — no parser changes, minimal sema changes
2. **Immediately useful** — users can split large programs today
3. **Tests the linker** — exercises cross-file symbol resolution
4. **Foundation for modules** — the plumbing we build here (multi-file parsing, symbol merging, cross-file linking) is reused when modules land

The UX is admittedly awkward (`rue a.rue b.rue c.rue -o out`), but this is a stepping stone, not the final design.

## Decision

### CLI Interface

```bash
# Single file (unchanged)
rue source.rue output

# Multiple files (new)
rue main.rue utils.rue math.rue -o program

# Glob patterns via shell expansion
rue src/*.rue -o program

# Explicit output required with multiple inputs
rue a.rue b.rue              # Error: multiple inputs require -o
rue a.rue b.rue -o out       # OK
```

The `-o` flag becomes required when multiple source files are provided, to avoid ambiguity about which positional argument is the output.

### Compilation Model

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  main.rue   │     │  utils.rue  │     │  math.rue   │
└──────┬──────┘     └──────┬──────┘     └──────┬──────┘
       │                   │                   │
       ▼                   ▼                   ▼
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│    Lexer    │     │    Lexer    │     │    Lexer    │
│    Parser   │     │    Parser   │     │    Parser   │
│    AstGen   │     │    AstGen   │     │    AstGen   │
└──────┬──────┘     └──────┬──────┘     └──────┬──────┘
       │                   │                   │
       └───────────────────┼───────────────────┘
                           │
                           ▼
                 ┌─────────────────────┐
                 │   Symbol Merging    │
                 │   (global scope)    │
                 └──────────┬──────────┘
                            │
                            ▼
                 ┌─────────────────────┐
                 │   Sema (all files)  │
                 │   Cross-file calls  │
                 └──────────┬──────────┘
                            │
                            ▼
                 ┌─────────────────────┐
                 │   CFG Construction  │
                 │   (parallel)        │
                 └──────────┬──────────┘
                            │
                            ▼
                 ┌─────────────────────┐
                 │   Codegen + Link    │
                 └──────────┬──────────┘
                            │
                            ▼
                      ┌───────────┐
                      │ Executable│
                      └───────────┘
```

**Key insight**: Parse files independently (parallelizable), then merge symbols into a unified scope for semantic analysis.

### Implementation Details

#### 1. CLI Changes (`rue/src/main.rs`)

```rust
struct Options {
    source_paths: Vec<String>,  // Changed from single source_path
    output_path: String,
    // ... rest unchanged
}
```

Argument parsing:
- Collect all non-option arguments as potential source files
- If multiple sources and no `-o`, error
- If single source and no `-o`, use `a.out` (unchanged behavior)

#### 2. CompileOptions Changes (`rue-compiler/src/lib.rs`)

```rust
/// Input to the compiler - either single source or multiple files.
pub enum CompileInput<'a> {
    /// Single source string (for backwards compatibility and tests).
    Single(&'a str),
    /// Multiple source files with their paths.
    Multiple(Vec<SourceFile<'a>>),
}

pub struct SourceFile<'a> {
    pub path: &'a str,
    pub source: &'a str,
}
```

#### 3. Frontend Changes

**Parallel parsing** (one thread per file):
```rust
fn parse_all_files(inputs: &[SourceFile]) -> MultiErrorResult<Vec<ParsedFile>> {
    inputs.par_iter().map(|file| {
        let lexer = Lexer::new(file.source);
        let (tokens, interner) = lexer.tokenize()?;
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse()?;
        Ok(ParsedFile { path: file.path, ast, interner })
    }).collect()
}
```

**Symbol merging** after parsing:
```rust
fn merge_symbols(parsed_files: Vec<ParsedFile>) -> MergedProgram {
    let mut global_interner = ThreadedRodeo::new();
    let mut all_functions = Vec::new();
    let mut all_structs = Vec::new();
    let mut all_enums = Vec::new();

    for file in parsed_files {
        // Merge interner entries
        // Collect functions, structs, enums
        // Check for duplicates (error on collision)
    }

    MergedProgram {
        functions: all_functions,
        structs: all_structs,
        enums: all_enums,
        interner: global_interner,
    }
}
```

**Duplicate detection**:
- Same function name in two files → error with both locations
- Same struct name in two files → error with both locations
- Same enum name in two files → error with both locations

#### 4. Sema Changes

Currently, `Sema::new()` takes a single `&Rir`. We need to support merged RIR from multiple files:

```rust
impl Sema {
    /// Create sema from merged program (multiple files).
    pub fn from_merged(
        merged: &MergedProgram,
        preview_features: PreviewFeatures,
    ) -> Self {
        // Build scope with all functions, structs, enums visible
        // Cross-file references resolve naturally since everything is in scope
    }
}
```

#### 5. Error Reporting

Errors must include the source file path:

```
error[E0001]: type mismatch
  --> utils.rue:15:12
   |
15 |     return "hello";
   |            ^^^^^^^ expected i32, found String

error[E0002]: duplicate function definition
  --> math.rue:5:1
   |
 5 | fn add(a: i32, b: i32) -> i32 { a + b }
   | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
note: first defined here
  --> utils.rue:10:1
   |
10 | fn add(x: i32, y: i32) -> i32 { x + y }
   | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

The `DiagnosticFormatter` already supports source file names; we need to ensure each error carries the correct file context.

#### 6. Linking

The linker already supports multiple object files — one per function. Multi-file compilation just means more functions come from different source files, which the linker handles transparently.

### Backwards Compatibility

Single-file compilation remains unchanged:

```bash
rue source.rue output    # Still works exactly as before
```

The new multi-file mode is additive.

### Entry Point

The `main()` function must exist in exactly one of the input files:
- No `main()` in any file → error: "no main function found"
- `main()` in multiple files → error: "duplicate function definition: main"

## Implementation Phases

### Phase 1: CLI and Input Handling

**Goal**: Accept multiple source files, read them all, but still process only the first one.

**Tasks**:
- Update `Options` to hold `Vec<String>` for source paths
- Add `-o` flag requirement for multiple inputs
- Update argument parsing tests
- Read all source files into memory

**Verification**: `rue a.rue b.rue -o out` reads both files but only compiles `a.rue`.

### Phase 2: Parallel Parsing

**Goal**: Parse all files in parallel, producing separate ASTs.

**Tasks**:
- Add `SourceFile` and `ParsedFile` types
- Implement `parse_all_files()` with Rayon
- Merge string interners from all files
- Error if any file fails to parse

**Verification**: Parsing errors from any file are reported with correct file paths.

### Phase 3: Symbol Merging

**Goal**: Merge declarations from all files into a unified global scope.

**Tasks**:
- Implement `merge_symbols()` function
- Detect and report duplicate definitions
- Build merged RIR for semantic analysis
- Update error messages to show both locations for duplicates

**Verification**: Duplicate function names produce clear errors with both file locations.

### Phase 4: Cross-File Semantic Analysis

**Goal**: Functions in one file can call functions in another.

**Tasks**:
- Update `Sema` to work with merged program
- Ensure cross-file function calls resolve correctly
- Struct and enum types visible across files
- Update tests

**Verification**: `main.rue` can call `helper()` defined in `utils.rue`.

### Phase 5: Documentation and Polish

**Goal**: Document the feature and ensure good UX.

**Tasks**:
- Update CLAUDE.md with multi-file examples
- Add `--help` text for multiple inputs
- Add user-facing documentation
- Performance testing with many files

## Consequences

### Positive

- **Real programs possible**: Users can organize code across files
- **Foundation for modules**: Parsing, merging, linking all exercised
- **Parallel parsing**: Multiple files parse simultaneously
- **Incremental progress**: Ship value before full module system

### Negative

- **Flat namespace**: All symbols globally visible (no privacy)
- **Manual file listing**: Users must list all files explicitly
- **No incremental builds**: Recompile everything on each change
- **Symbol collisions**: Easy to accidentally have name conflicts

### Neutral

- **Stepping stone**: This is explicitly a transitional design
- **UX will improve**: Module system (ADR-0022) will provide better ergonomics
- **Tests as validation**: Spec tests can use multiple files once modules land

## Design Decisions

### 1. Why require `-o` for multiple files?

Without it, `rue a.rue b.rue` is ambiguous — is `b.rue` the output or a second source file? Requiring `-o` makes intent explicit.

### 2. Why not auto-discover files?

Some languages (Go) discover files automatically from a directory. We chose explicit listing because:
- Simpler implementation
- No need for "which files are part of this project?" logic
- Build systems can generate file lists
- Aligns with how C/Rust compilers work

### 3. Why merge at RIR level?

We could merge at AST level or later. RIR is the right boundary because:
- ASTs are per-file naturally (parser doesn't need changes)
- RIR represents "program items" that can be combined
- Sema expects a program-level view, not file-level

### 4. How do we handle the string interner?

Each file gets its own interner during parsing (since `ThreadedRodeo` is thread-safe for insertion but we want parallel parsing without contention). After parsing, we merge into a single interner that's used for sema and codegen.

### 5. What about `--emit` modes?

The `--emit` modes (tokens, ast, air, etc.) work per-file in multi-file mode:

```bash
rue --emit ast a.rue b.rue -o out
# Outputs:
# === AST (a.rue) ===
# ...
# === AST (b.rue) ===
# ...
```

This is useful for debugging which file contributed what.

## Open Questions

None at this time.

## Future Work

- **Module system** (ADR-0022): Adds `mod`, `use`, `pub` syntax
- **Visibility**: Private-by-default, explicit `pub` for exports
- **Incremental compilation**: Rebuild only changed files
- **Build system**: `rue.toml` or similar for project definition
- **Parallel sema**: Currently sema is single-threaded; could parallelize per-function

## References

- [ADR-0022: Module System](0022-modules.md) — Upcoming design for modules and visibility
- [ADR-0020: Built-in Types as Synthetic Structs](0020-builtin-types-as-structs.md) — Related type system work
- Current CLI implementation: `crates/rue/src/main.rs`
- Current compiler driver: `crates/rue-compiler/src/lib.rs`
