# Rue Compiler Benchmarks

This directory contains synthetic compiler phase probes plus one tracked
representative compiler fixture. The phase probes are diagnostic
inputs, not representative Rue applications, and they do not measure the
runtime performance of compiled programs. The representative cold/reused
scenario families are published separately and never enter this aggregate.

## Structure

```
benchmarks/
├── manifest.toml       # Benchmark metadata
├── README.md           # This file
└── stress/             # Stress test programs
    ├── many_functions.rue    # 1,000 functions
    ├── deep_nesting.rue      # 12,198-line nesting stress corpus
    ├── large_structs.rue     # Many struct types
    ├── arithmetic_heavy.rue  # Expression-heavy code
    └── control_flow.rue      # Complex if/while/match
```

## Benchmark Descriptions

The manifest is the canonical classification and dimension source. Its
verified dimensions are checked against the files before every benchmark run.

<!-- BEGIN GENERATED PROBE INVENTORY -->
| Probe | Primary subsystem | Workload family | Verified dimensions |
|---|---|---|---|
| `many_functions` | frontend/declarations | declaration-volume | 2,120 lines; 1,001 functions |
| `deep_nesting` | frontend/parser-nesting | nesting-pathology | 12,198 lines; 151 functions |
| `large_structs` | semantic/types | type-volume | 2,187 lines; 701 functions; 700 structs |
| `arithmetic_heavy` | frontend-expressions/backend | expression-volume | 5,312 lines; 251 functions |
| `control_flow` | cfg | control-flow-volume | 10,678 lines; 391 functions |
| `array_heavy` | semantic/arrays | array-operation-volume | 6,185 lines; 201 functions |
| `register_pressure` | backend/register-allocation | live-value-volume | 6,109 lines; 211 functions; 10 structs |
<!-- END GENERATED PROBE INVENTORY -->

The deep-nesting probe contains 120 nested-control functions (50 block + 50
if + 20 while), each up to 40 levels (`v39`), plus 30 deep-expression helpers.
Those adversarial details are pinned by focused tests in addition to the
generic manifest dimension check.

## Running Benchmarks

### Using the Benchmark Runner (Recommended)

The `bench.sh` script handles the complete benchmark workflow:

```bash
# Run all benchmarks with defaults (5 iterations, append to history)
./bench.sh

# Custom number of iterations for more accuracy
./bench.sh --iterations 10

# Save to a specific file without updating history
./bench.sh --no-history --output results.json

# Show help
./bench.sh --help
```

The benchmark runner:
1. Builds the compiler in release mode
2. Validates and parses the canonical `manifest.toml`
3. Runs each benchmark multiple times
4. Calculates mean and standard deviation
5. Generates five deterministic three-tier scaling families spanning frontend
   declarations, module imports, types, CFG, and backend pressure
6. Publishes latency, peak memory, robust median/scaled-MAD variation,
   source/pass structural counters, adjacent size-normalized growth bounds,
   and evidence classifications as JSON
7. Runs the representative root through the real cold batch driver and a fixed
   edit sequence through canonical `CompilerSession` queries
8. Appends to partitioned website history (unless `--no-history`)

Scaling tiers have bounded per-compile timeouts plus adjacent-tier latency/unit
and memory/unit budgets. A violation requires the conservative lower growth
bound to exceed its budget; bounds crossing the budget and runs with fewer than
three samples remain visibly indeterminate. A range guard also keeps extreme
minority samples from becoming a false `±0` conclusion when MAD is zero.
Both proven violations and indeterminate evidence fail enforcement and are not
publishable. Passing a loose bound is not a claim of linear complexity.
Absolute time is shown but is not the complexity budget. The scaling section
is separate from the static phase-probe aggregate.

`scenarios/representative` is a deterministic multi-module root/import graph
with control flow, typed functions, strings, and an adjacent minimal standard
library. Every session variant contains only its root's actual transitive
closure; the import edit removes one leaf and adds its replacement. Cold
samples require byte-identical raw compiler output both across iterations and
against the exact fresh/reused `CompilerSession` base executable. Reused-session
samples require exact fresh-session artifact/diagnostic/output parity and
publish direct required/reused module, semantic-body, CFG, and semantic-query
counters. Unsupported durable conversions fail closed and are reported as
rebuilds; elapsed time is never used to infer reuse.

### Running Individual Benchmarks

For manual testing or debugging:

```bash
# Run a single benchmark with timing output
./buck2 run //crates/rue:rue -- --time-passes benchmarks/stress/many_functions.rue /tmp/out

# Get JSON timing output
./buck2 run //crates/rue:rue -- --benchmark-json benchmarks/stress/many_functions.rue /tmp/out
```

The quick per-pass corpus (`scripts/rue perf`) also includes the exact
`deep_nesting.rue` file. Each fresh compile of that workload has a fixed
10-second ceiling even when the general harness timeout is larger. Separately,
the depth-60 CLI regression in `crates/rue-cli-tests/cases/deep_nesting.toml`
has its own explicit 10-second timeout; it is the executable complexity gate
that turns renewed superlinear/exponential nesting behavior into a test failure.
For parser-only timing and allocation density, including malformed and
adversarial scaling families, run `scripts/parser-profile.py --release`.

## Adding Benchmarks

1. Add a `.rue` file to `stress/` (or create a new category directory)
2. Add an entry to `manifest.toml`
3. Ensure the program compiles and runs correctly

Each benchmark should:
- Be large enough to produce measurable timing (aim for >1ms compilation)
- Focus on a specific compiler phase or feature
- Declare its diagnostic subsystem, family, interpretation,
  non-representative limitation, and mechanically verified dimensions
- Return a deterministic exit code for verification

## Benchmark History

Results are stored as content-addressed, partitioned history for the performance
dashboard and retained without a fixed run-count cap.

For more details on the performance tracking workflow, see `docs/perf-branch.md`.
