# Linting and Formatting

This document describes how to run clippy (linting) and rustfmt (formatting) checks in the Rue compiler project.

## Quick Start

### Prerequisites

Install the required Rust components:

```bash
rustup component add clippy rustfmt
```

### Running Checks

**Format checking:**
```bash
./buck2 test //tools/rustfmt:fmt_check_test # Check formatting without changes
./buck2 run //tools/rustfmt:fmt_fix        # Apply formatting fixes to all files
```

**Linting with Clippy:**
```bash
./buck2 bxl //tools/bxl:clippy.bxl:all     # Run clippy on all crates
./buck2 bxl //tools/bxl:clippy.bxl:check -- --targets //crates/rue-lexer:clippy  # Check specific target
```

## How It Works

### Rustfmt Integration

The Buck2 rustfmt targets in `//tools/rustfmt:` provide:

1. **fmt_check_test**: Runs rustfmt in check mode to verify formatting (test target)
2. **fmt_fix**: Applies formatting fixes to all Rust files

These targets use Buck2's native integration to run rustfmt on all Rust source files in the project.

### Clippy Integration  

The BXL scripts in `//tools/bxl:clippy.bxl` provide:

1. **all**: Runs clippy on all crates in the project
2. **check**: Checks specific targets provided via command line
3. **fix**: Provides instructions for fixing clippy issues

Each crate has a `:clippy` target that can be built to run clippy checks on that specific crate.

## CI Integration

In GitHub Actions or other CI systems, use these commands:

```yaml
- name: Check formatting
  run: ./buck2 test //tools/rustfmt:fmt_check_test
  
- name: Run clippy
  run: ./buck2 bxl //tools/bxl:clippy.bxl:all
```

Both commands will exit with non-zero status if issues are found.

## Advanced Usage

### BXL Scripts

Buck2 Extension Language (BXL) scripts provide advanced integration:

**Formatting BXL (provides instructions):**
```bash
# Get instructions for checking formatting
./buck2 bxl //tools/bxl:fmt.bxl:check

# Get instructions for specific package
./buck2 bxl //tools/bxl:fmt.bxl:check -- --scope="//crates/rue-lexer"

# Get instructions for fixing formatting
./buck2 bxl //tools/bxl:fmt.bxl:fix
```

Note: The fmt.bxl script provides instructions rather than directly running rustfmt due to BXL limitations.

**Clippy BXL:**
```bash
# Run on all crates
./buck2 bxl //tools/bxl:clippy.bxl:all

# Run with pedantic mode
./buck2 bxl //tools/bxl:clippy.bxl:all -- --pedantic=true

# Check specific targets
./buck2 bxl //tools/bxl:clippy.bxl:check -- --targets //crates/rue:clippy //crates/rue-parser:clippy
```

### Clippy Configuration

The project includes a `.clippy.toml` file that configures clippy for compiler development:

```toml
# Allows higher complexity for compiler code
cognitive-complexity-threshold = 100
too-many-arguments-threshold = 10

# Compiler-specific allows
allow = [
    "clippy::module_name_repetitions",  # Common in compilers
    "clippy::missing_panics_doc",       # Internal compiler functions
    "clippy::must_use_candidate",       # Too noisy for compilers
]
```

### Custom Clippy Rules

To use clippy wrapper macros in BUCK files:

```starlark
load("//tools/rust:defs.bzl", "clippy_rust_library")

clippy_rust_library(
    name = "clippy",
    srcs = glob(["src/**/*.rs"]),
    crate_root = "src/lib.rs",
    edition = "2024",
    deps = [...],
)
```

This creates a clippy target that can be built to run clippy checks.

## Project Structure

The linting and formatting setup is organized as:

```
tools/
├── rustfmt/          # Rustfmt Buck2 targets
│   ├── BUCK
│   ├── fmt_check.sh
│   └── fmt_fix.sh
├── bxl/              # BXL scripts for advanced integration
│   ├── clippy.bxl    # Clippy integration
│   └── fmt.bxl       # Formatting integration (provides instructions)
└── clippy/           # Clippy configuration
    └── auto.bzl      # Auto-discovery of clippy targets
```

## Troubleshooting

### "clippy-driver not found"

Install clippy:
```bash
rustup component add clippy
```

### "rustfmt not found"

Install rustfmt:
```bash
rustup component add rustfmt
```

### No clippy warnings shown

Buck2 may be caching the build. Clean and rebuild:
```bash
./buck2 clean
./buck2 bxl //tools/bxl:clippy.bxl:all
```

### Formatting changes not applied

Use the fmt_fix target:
```bash
./buck2 run //tools/rustfmt:fmt_fix
```

## Architecture

The linting and formatting setup leverages Buck2's build system:

1. **Buck2 Targets**: Native Buck2 targets in `//tools/rustfmt:` for formatting
2. **BXL Scripts**: Advanced Buck2 integration for both clippy and rustfmt
3. **Per-Crate Targets**: Each crate has a `:clippy` target for targeted linting
4. **Auto-Discovery**: The `clippy_all_group` macro automatically finds all clippy targets

This approach ensures reliable linting and formatting while fully integrating with Buck2's build system.