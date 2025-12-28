# Rue Compiler Benchmarks

This directory contains benchmark programs for measuring Rue compiler performance.

## Structure

```
benchmarks/
├── manifest.toml       # Benchmark metadata
├── README.md           # This file
└── stress/             # Stress test programs
    ├── many_functions.rue    # 100+ functions
    ├── deep_nesting.rue      # Deeply nested blocks
    ├── large_structs.rue     # Many struct types
    ├── arithmetic_heavy.rue  # Expression-heavy code
    └── control_flow.rue      # Complex if/while/match
```

## Benchmark Descriptions

| Benchmark | What it tests | Size |
|-----------|--------------|------|
| `many_functions` | Function handling, symbol resolution | 100 functions |
| `deep_nesting` | Scope handling, block nesting | 10 nesting levels |
| `large_structs` | Type definitions, field access | 50 struct types |
| `arithmetic_heavy` | Expression parsing, codegen | ~100 expressions |
| `control_flow` | CFG construction | if/while/match mix |

## Running Benchmarks

Benchmarks are run via the `--benchmark-json` flag (once implemented):

```bash
# Run all benchmarks
./buck2 run //crates/rue:rue -- --benchmark-json results.json benchmarks/

# Run with timing report
./buck2 run //crates/rue:rue -- --time-passes benchmarks/stress/many_functions.rue /tmp/out
```

## Adding Benchmarks

1. Add a `.rue` file to `stress/` (or create a new category directory)
2. Add an entry to `manifest.toml`
3. Ensure the program compiles and runs correctly

Each benchmark should:
- Be large enough to produce measurable timing (aim for >1ms compilation)
- Focus on a specific compiler phase or feature
- Return a deterministic exit code for verification
