# Buck2 Build System Guide

This guide explains how to use Buck2, the exclusive build system for the Rue programming language compiler.

## Overview

Rue uses Buck2 as its build system. Buck2 provides faster incremental builds, better dependency management, and improved CI/CD performance compared to traditional build systems. The project has fully migrated from Cargo and no longer supports Cargo builds.

## Quick Command Reference

### Essential Commands

| Task | Buck2 Command |
|------|--------------|
| Build compiler | `./buck2 build //crates/rue:rue` |
| Build all crates | `./buck2 build //crates/...` |
| Run all tests | `./buck2 test //crates/...` |
| Compile a program | `./buck2 run //crates/rue:rue file.rue` |
| Start LSP server | `./buck2 run //crates/rue-lsp` |

### Testing Commands

| Test Type | Buck2 Command |
|-----------|---------------|
| Lexer tests | `./buck2 test //crates/rue-lexer:test` |
| Parser tests | `./buck2 test //crates/rue-parser:test` |
| Semantic tests | `./buck2 test //crates/rue-semantic:test` |
| Codegen tests | `./buck2 test //crates/rue-codegen:test` |
| Compiler tests | `./buck2 test //crates/rue-compiler:test` |
| Runtime tests | `./buck2 test //crates/rue-runtime:test` |
| Integration tests | `./buck2 test //crates/rue:` |
| Corpus tests | `./buck2 test //crates/rue:corpus_tests` |
| Type system tests | `./buck2 test //crates/rue:type_system_tests` |

## Setup Instructions

### Prerequisites

1. **Install dotslash** (required for Buck2):
   ```bash
   curl -L https://github.com/facebook/dotslash/releases/latest/download/dotslash-linux.tar.xz | tar -xJ
   sudo install dotslash /usr/local/bin/
   ```

2. **Verify Buck2 installation**:
   ```bash
   ./buck2 --version
   ```

### IDE Setup

For IDE support with Buck2, you'll need to generate a `rust-project.json` file for rust-analyzer:

1. **Generate rust-project.json**:
   ```bash
   ./buck2 run support/buck2:rust-project
   ```

2. **Configure your editor**:
   - **VS Code**: Install the rust-analyzer extension. It should automatically detect the `rust-project.json` file.
   - **Other editors**: Configure rust-analyzer to use the generated `rust-project.json` file.

3. **Keep rust-project.json updated**:
   - Re-run the command after adding new dependencies or crates
   - Consider setting up a git hook to regenerate it automatically

For detailed IDE setup instructions, see [buck2-ide-setup.md](buck2-ide-setup.md).

## Development Workflow

### Daily Development

1. **Building and testing**:
   ```bash
   # Build the compiler
   ./buck2 build //crates/rue:rue
   
   # Run all tests
   ./buck2 test //crates/...
   
   # Compile a Rue program
   ./buck2 run //crates/rue:rue examples/basic/simple.rue
   ./examples/basic/simple; echo $?
   ```

2. **Debugging builds**:
   ```bash
   # Build with detailed output
   ./buck2 build //crates/rue:rue --verbose
   
   # Run with Rust backtrace
   RUST_BACKTRACE=1 ./buck2 run //crates/rue:rue examples/basic/simple.rue
   ```

### Adding Dependencies

When adding new Rust dependencies:

1. **Add to third-party/rust/Cargo.toml**:
   ```toml
   [dependencies]
   new-crate = "1.0.0"
   ```

2. **Add to individual crate's BUCK file**:
   ```python
   rust_library(
       name = "my-crate",
       srcs = glob(["src/**/*.rs"]),
       deps = [
           "//third-party/rust:new-crate",
           # ... other deps
       ],
   )
   ```

3. **Regenerate Buck2 build files**:
   ```bash
   # Get update instructions
   ./buck2 bxl //tools/bxl:deps.bxl:update
   
   # Then run reindeer to regenerate
   cd third-party/rust && ../../reindeer buckify
   ```

4. **Test the changes**:
   ```bash
   ./buck2 test //crates/...
   ```

For dependencies that need special configuration, create fixup files in `third-party/rust/fixups/<crate>/fixups.toml`.

### CI/CD Integration

Buck2 provides several advantages for CI/CD:

- **Faster builds**: Better caching and parallelization
- **Precise dependencies**: Only rebuilds what's necessary
- **Consistent environments**: Hermetic builds reduce "works on my machine" issues

Example CI commands:
```bash
# Build all targets
./buck2 build //crates/...

# Run all tests with parallelization
./buck2 test //crates/... --jobs 4

# Generate and check rust-project.json
./buck2 run support/buck2:rust-project
git diff --exit-code rust-project.json
```

## Benefits of Buck2

### Performance

- **Incremental builds**: Only rebuilds changed components
- **Better parallelization**: More efficient use of multiple CPU cores
- **Intelligent caching**: Shares build artifacts across similar configurations

### Developer Experience

- **Precise dependencies**: Clear dependency graph prevents issues
- **IDE integration**: rust-analyzer works well with generated rust-project.json
- **Consistent builds**: Hermetic builds reduce environmental issues

### Maintenance

- **Dependency management**: Cleaner handling of third-party dependencies
- **Build reproducibility**: Same inputs always produce same outputs
- **Scalability**: Designed for large monorepos

## Troubleshooting

### Common Issues

1. **"dotslash not found"**:
   - Install dotslash using the instructions above
   - Make sure `/usr/local/bin` is in your PATH

2. **"Buck2 command not found"**:
   - Use `./buck2` (with the dot-slash prefix)
   - Ensure you're in the project root directory

3. **IDE not recognizing code**:
   - Regenerate rust-project.json: `./buck2 run support/buck2:rust-project`
   - Restart your IDE/editor
   - Check that rust-analyzer is properly installed

4. **Dependency issues**:
   - Run `./buck2 run support/buck2:sync-cargo-deps` to sync dependencies
   - Check for required fixups in `third-party/rust/fixups/`
   - Look at existing fixups for examples

5. **Build failures**:
   - Clean build cache: `./buck2 clean`
   - Check for conflicting Cargo.lock files
   - Verify all dependencies are properly synced

### Getting Help

- Check the [Buck2 documentation](https://buck2.build/)
- Look at existing BUCK files in the repository for patterns
- Examine `third-party/rust/fixups/` for dependency configuration examples
- Refer to [CONTRIBUTING.md](../CONTRIBUTING.md) for development guidance

## Project Status

✅ **Completed:**
- Full Buck2 integration
- All crates building with Buck2
- Complete test suite execution
- Dependency management via Reindeer
- IDE support with rust-analyzer
- CI/CD using Buck2 exclusively
- BXL scripts for common tasks
- Removal of Cargo support

📋 **Ongoing Improvements:**
- Performance optimization
- Custom build rules for Rue-specific tasks
- Enhanced IDE integration features

---

For questions about Buck2 usage, see the development documentation in [CONTRIBUTING.md](../CONTRIBUTING.md). For historical context about the migration from Cargo, see [docs/sessions/session-021-move-to-buck2/](sessions/session-021-move-to-buck2/).