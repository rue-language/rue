# Implementation Plan: Migrate to Buck2-Only Build System

This document outlines the step-by-step plan to migrate Rue from a dual Cargo/Buck2 build system to Buck2-only, with proper bootstrapping and developer experience.

## Implementation Checklist

### Phase 1: Buck2 & Tool Bootstrapping
- [x] **1.1a** Create `buck/bin/` directory structure
- [x] **1.1b** Write dotslash config for buck2 binary
- [x] **1.1c** Write dotslash config for rust-project tool
- [x] **1.1d** Test buck2 bootstrap on Linux x86_64
- [x] **1.1e** Test rust-project bootstrap on Linux x86_64
- [ ] **1.1f** Add platform support for macOS/Windows if needed
- [x] **1.1g** Create `./buck2` symlink to `buck/bin/buck2` for convenience
- [x] **1.1h** Document dotslash installation requirements

### Phase 2: Rust-Analyzer Integration
- [x] **2.1a** Test rust-project tool with `./buck/bin/rust-project develop //crates/...`
- [ ] **2.1b** Verify generated rust-project.json works with rust-analyzer
- [x] **2.1c** Create script to regenerate rust-project.json
- [x] **2.1d** Add VS Code task for regenerating rust-project.json
- [x] **2.1e** Update VS Code settings.json to use rust-project.json
- [ ] **2.1f** Test that all IDE features work (completion, go-to-def, etc.)
- [x] **2.1g** Document rust-analyzer setup process

### Phase 3: Feature Parity Verification
- [ ] **3.1a** Verify `buck2 build //crates/rue:rue` works
- [ ] **3.1b** Verify `buck2 test //crates/...` runs all tests
- [ ] **3.1c** Check all individual test targets work
- [ ] **3.1d** Verify release builds with optimizations
- [ ] **3.1e** Test `buck2 run //crates/rue:rue -- samples/simple.rue`
- [ ] **3.1f** Ensure all sample programs compile and run
- [ ] **3.1g** Verify LSP server can be built (if it works with Buck2)
- [ ] **3.1h** Test benchmark targets if applicable

### Phase 4: CI/CD Migration
- [x] **4.1a** Update GitHub Actions to install dotslash
- [x] **4.1b** Replace `cargo build` with `./buck2 build //crates/rue:rue`
- [x] **4.1c** Replace `cargo test` with `./buck2 test //crates/...`
- [x] **4.1d** Update caching to use buck-out/ instead of target/
- [ ] **4.1e** Remove Cargo-specific steps (fmt, clippy, audit)
- [ ] **4.1f** Create Buck2 formatting check (BXL or action)
- [ ] **4.1g** Create Buck2 linting check
- [ ] **4.1h** Test CI changes in a pull request
- [ ] **4.1i** Ensure CI passes on all platforms

### Phase 5: Documentation Updates
- [x] **5.1a** Update README.md build instructions
- [x] **5.1b** Update README.md to show Buck2 as primary build system
- [x] **5.1c** Update CONTRIBUTING.md with Buck2-only commands
- [x] **5.1d** Remove all Cargo references from CONTRIBUTING.md
- [x] **5.1e** Update CLAUDE.md with Buck2 workflows
- [x] **5.1f** Document reindeer workflow for dependencies
- [x] **5.1g** Create quick-start guide for new developers
- [x] **5.1h** Update debugging instructions for Buck2 builds

### Phase 6: Cargo Infrastructure Removal
- [ ] **6.1a** Remove root Cargo.toml
- [ ] **6.1b** Remove all crate-level Cargo.toml files
- [ ] **6.1c** Keep third-party/rust/Cargo.toml for reindeer
- [ ] **6.1d** Remove Cargo.lock
- [ ] **6.1e** Update .gitignore to remove Cargo artifacts
- [ ] **6.1f** Remove cargo-specific CI caching configurations
- [ ] **6.1g** Clean up any remaining Cargo references

### Phase 7: Developer Experience Enhancements
- [x] **7.1a** Create shell aliases for common Buck2 commands
- [ ] **7.1b** Add shell completion for Buck2 targets
- [x] **7.1c** Create development scripts (build.sh, test.sh, etc.)
- [ ] **7.1d** Set up Buck2 remote caching if applicable
- [ ] **7.1e** Optimize Buck2 configuration for faster builds
- [ ] **7.1f** Create troubleshooting guide for common issues
- [ ] **7.1g** Add pre-commit hooks for Buck2 checks

## Bootstrap File Examples

### buck/bin/buck2
```json
#!/usr/bin/env dotslash

{
  "name": "buck2",
  "platforms": {
    "linux-x86_64": {
      "size": <size>,
      "hash": "sha256",
      "digest": "<sha256>",
      "format": "zst",
      "path": "buck2",
      "providers": [
        {
          "url": "https://github.com/facebook/buck2/releases/download/<version>/buck2-<platform>.zst"
        }
      ]
    }
  }
}
```

### buck/bin/rust-project
```json
#!/usr/bin/env dotslash

{
  "name": "rust-project",
  "platforms": {
    "linux-x86_64": {
      "size": <size>,
      "hash": "sha256", 
      "digest": "<sha256>",
      "format": "zst",
      "path": "rust-project",
      "providers": [
        {
          "url": "https://github.com/facebook/buck2/releases/download/<version>/rust-project-<platform>.zst"
        }
      ]
    }
  }
}
```

## Commands Reference

### Before (Cargo)
```bash
cargo build -p rue
cargo test
cargo run -p rue samples/simple.rue
cargo fmt --all -- --check
cargo clippy --all-targets
```

### After (Buck2)
```bash
./buck2 build //crates/rue:rue
./buck2 test //crates/...
./buck2 run //crates/rue:rue -- samples/simple.rue
./buck2 bxl //tools:fmt.bxl -- --check
./buck2 bxl //tools:clippy.bxl
```

## Rust-Analyzer Setup

1. Generate rust-project.json:
```bash
./buck/bin/rust-project develop //crates/... --out rust-project.json
```

2. Configure VS Code settings.json:
```json
{
  "rust-analyzer.linkedProjects": ["./rust-project.json"]
}
```

3. Regenerate after dependency changes:
```bash
./scripts/update-rust-project.sh
```

## Risk Mitigation

1. **Rollback Plan**: Keep Cargo files in a separate branch initially
2. **Testing**: Thoroughly test in CI before merging
3. **Documentation**: Ensure all workflows are documented
4. **Team Communication**: Announce changes and provide migration guide
5. **Support Period**: Maintain both systems briefly if needed

## Success Criteria

- [ ] All developers can build and test with Buck2 only
- [ ] CI/CD runs faster with Buck2 caching
- [ ] Rust-analyzer works seamlessly
- [ ] Documentation is complete and accurate
- [ ] No loss of functionality from Cargo removal
- [ ] Onboarding is simpler with dotslash bootstrap