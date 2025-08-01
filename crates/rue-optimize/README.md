# Rue Optimize - Pass Manager Framework

The `rue-optimize` crate provides a comprehensive optimization framework for the Rue compiler's MIR (Mid-level Intermediate Representation). It includes a Pass Manager Framework with fixed-point iteration, configurable optimization levels, and detailed pass statistics.

## Features

- **Pass Manager Framework**: Orchestrates optimization passes with fixed-point iteration
- **Configurable Optimization Levels**: Support for -O0, -O1, and -O2 optimization levels
- **Change Detection**: Passes report whether they made changes to enable early termination
- **Pass Statistics**: Detailed timing and transformation statistics for performance analysis
- **Extensible Design**: Easy to add new optimization passes following the `Pass` trait

## Architecture

### Pass Trait

All optimization passes implement the `Pass` trait:

```rust
pub trait Pass {
    fn name(&self) -> &'static str;
    fn run(&mut self, program: &mut MirProgram) -> PassResult;
    fn description(&self) -> &'static str;
    fn can_repeat(&self) -> bool;
}
```

### Pass Manager

The `PassManager` orchestrates pass execution:

```rust
let mut pass_manager = PassManager::for_optimization_level(OptimizationLevel::Full);
let statistics = pass_manager.run_to_fixpoint(program, &mut passes);
```

### Optimization Levels

- **OptimizationLevel::None (-O0)**: No optimizations
- **OptimizationLevel::Basic (-O1)**: Fast, basic optimizations
- **OptimizationLevel::Full (-O2)**: All optimizations with fixed-point iteration

## Usage Examples

### Basic Usage with Optimization Profiles

The simplest way to use the optimization framework is through the `OptimizationProfileFactory`:

```rust
use rue_optimize::{OptimizationLevel, OptimizationProfileFactory};
use rue_ir::mir::MirProgram;

// Optimize a MIR program with full optimizations
let mut program: MirProgram = /* ... */;
OptimizationProfileFactory::optimize_program(&mut program, OptimizationLevel::Full);
```

### Custom Pass Manager Configuration

For more control, create a custom pass manager:

```rust
use rue_optimize::{PassConfig, PassManager, Pass};
use rue_optimize::passes::{ConstProp, CommonSubexpressionElimination, DeadCodeElimination};

// Create custom configuration
let mut config = PassConfig::default();
config.max_iterations = 5;
config.verbose_logging = true;
config.collect_statistics = true;

let mut pass_manager = PassManager::with_config(config);

// Create passes
let mut const_prop = ConstProp::new();
let mut cse = CommonSubexpressionElimination::new();
let mut dce = DeadCodeElimination::new();

// Run passes to fixed-point
let statistics = pass_manager.run_to_fixpoint(program, &mut [
    &mut const_prop,
    &mut cse, 
    &mut dce
]);

// Access detailed statistics
println!("Total passes executed: {}", statistics.total_passes);
println!("Converged: {}", statistics.converged);
println!("Total duration: {:?}", statistics.total_duration);
```

### Individual Pass Usage

You can also run passes individually:

```rust
use rue_optimize::{Pass, passes::ConstProp};

let mut const_prop = ConstProp::new();
let result = const_prop.run(&mut program);

if result.changed {
    println!("Constant propagation made {} transformations in {:?}", 
             result.transformations, result.duration);
}
```

### Implementing Custom Passes

To implement a custom optimization pass:

```rust
use rue_optimize::{Pass, PassResult};
use rue_ir::mir::MirProgram;
use std::time::Instant;

pub struct MyCustomPass {
    transformations: u32,
}

impl MyCustomPass {
    pub fn new() -> Self {
        Self { transformations: 0 }
    }
}

impl Pass for MyCustomPass {
    fn name(&self) -> &'static str {
        "my_custom_pass"
    }

    fn run(&mut self, program: &mut MirProgram) -> PassResult {
        let start = Instant::now();
        self.transformations = 0;

        // Implement your optimization logic here
        for function in &mut program.functions {
            // Analyze and transform the function
            // Increment self.transformations for each change made
        }

        let duration = start.elapsed();
        if self.transformations > 0 {
            PassResult::changed(duration, self.transformations)
        } else {
            PassResult::no_change(duration)
        }
    }

    fn description(&self) -> &'static str {
        "My custom optimization pass"
    }
}
```

## Available Optimization Passes

### Constant Propagation (`ConstProp`)

Evaluates constant expressions at compile time and replaces them with their computed values. Also optimizes constant branches by converting them to unconditional jumps.

**Features:**
- Constant folding for arithmetic operations
- Constant branch optimization
- Support for all MIR constant types (i32, i64, bool)

### Common Subexpression Elimination (`CommonSubexpressionElimination`)

Identifies duplicate computations and replaces them with uses of previously computed values.

**Features:**
- Local CSE within basic blocks
- Commutative operation normalization
- Constant deduplication

### Dead Code Elimination (`DeadCodeElimination`)

Removes assignments to temporaries that are never used and eliminates unreachable basic blocks.

**Features:**
- Use-def analysis for live variable detection
- Side effect preservation for function calls
- Unreachable block removal

## Logging and Diagnostics

The framework provides comprehensive logging through the `tracing` crate:

```bash
# Enable detailed optimization logging
RUST_LOG=rue::optimize=debug cargo run

# Enable trace-level logging for specific components
RUST_LOG=rue::optimize::stats=trace cargo run

# Use structured logging with tree format
RUST_LOG=rue::optimize=debug cargo run -- --log-format=tree
```

### Log Targets

- `rue::optimize`: General pass manager operations
- `rue::optimize::stats`: Pass statistics and performance data

## Performance Considerations

### Fixed-Point Iteration

The pass manager runs passes until no changes are made (fixed-point) or maximum iterations are reached:

- **Basic optimization (-O1)**: 3 iterations maximum
- **Full optimization (-O2)**: 10 iterations maximum
- **Custom**: Configurable via `PassConfig::max_iterations`

### Pass Ordering

Pass ordering can significantly impact optimization effectiveness:

1. **Constant Propagation** - Run first to enable other optimizations
2. **Common Subexpression Elimination** - Benefits from constant propagation
3. **Dead Code Elimination** - Run last to clean up unused code

### Statistics Collection

Pass statistics collection has minimal overhead but can be disabled for production builds:

```rust
let mut config = PassConfig::default();
config.collect_statistics = false; // Disable for performance
```

## Integration with Compiler Pipeline

The optimization framework is integrated into the Rue compiler pipeline in `/workspace/crates/rue-compiler/src/pipeline.rs`. The `optimize_and_verify_mir` function uses `OptimizationProfileFactory` to apply optimizations based on the compilation flags.

## Testing

The framework includes comprehensive tests covering:

- Individual pass correctness
- Pass manager fixed-point behavior  
- Statistics collection accuracy
- Optimization level configurations

Run tests with:

```bash
cargo test -p rue-optimize
```

## Future Extensions

The framework is designed to be extensible. Potential future enhancements include:

- **Global optimizations**: Cross-function analysis and optimization
- **Loop optimizations**: Specialized passes for loop constructs
- **Alias analysis**: Better understanding of memory dependencies
- **Profile-guided optimization**: Using runtime profiling data
- **Parallel pass execution**: Running independent passes in parallel

## Contributing

When adding new optimization passes:

1. Implement the `Pass` trait
2. Add comprehensive tests
3. Update optimization profiles if appropriate
4. Document the pass behavior and use cases
5. Consider integration with existing passes