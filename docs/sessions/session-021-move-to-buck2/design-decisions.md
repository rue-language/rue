# Design Decisions: Buck2-Only Migration

This document captures the key design decisions, trade-offs, and rationale for migrating Rue to a Buck2-only build system.

## Key Decisions

### 1. Use Dotslash for Buck2 Bootstrapping

**Decision**: Use dotslash to bootstrap Buck2 and related tools instead of requiring manual installation.

**Rationale**:
- Zero-friction onboarding - developers just run `./buck2` 
- Version consistency - everyone uses the same Buck2 version
- CI simplification - no need to install Buck2 in GitHub Actions
- Small text files in version control instead of binaries
- Cross-platform support built-in

**Alternatives Considered**:
- Manual Buck2 installation: Rejected due to version inconsistency and setup friction
- Docker containers: Rejected due to performance overhead and complexity
- Nix/Bazel: Rejected as they introduce additional toolchain dependencies

**Trade-offs**:
- Requires dotslash installation (but it's a single static binary)
- Additional layer of indirection for Buck2 execution
- Depends on external hosting for Buck2 binaries

### 2. Use rust-project Tool for Rust-Analyzer Support

**Decision**: Use the rust-project tool (from Buck2 integrations) to generate rust-project.json for IDE support.

**Rationale**:
- Maintains full IDE functionality without Cargo
- Official Buck2 solution for Rust IDE integration
- Actively maintained by Meta/Facebook
- Works with any rust-analyzer compatible editor

**Alternatives Considered**:
- Keep minimal Cargo.toml files: Rejected as it defeats the purpose of Buck2-only
- Custom rust-project.json generator: Rejected due to maintenance burden
- No IDE support: Rejected as it severely impacts developer experience

**Trade-offs**:
- Requires regenerating rust-project.json after dependency changes
- Additional tool to bootstrap and maintain
- Some IDE features might work differently than with Cargo

### 3. Keep Reindeer for Dependency Management

**Decision**: Continue using reindeer to manage third-party Rust dependencies.

**Rationale**:
- Proven solution for Cargo → Buck2 dependency conversion
- Already integrated and working in the project
- Maintains compatibility with crates.io ecosystem
- Handles complex dependency resolution

**Alternatives Considered**:
- Manual BUCK file writing: Rejected due to maintenance burden
- Vendor all dependencies: Rejected due to repository bloat
- Custom dependency tool: Rejected due to development effort

**Trade-offs**:
- Still requires a Cargo.toml in third-party/rust/
- Additional tool in the toolchain
- Learning curve for fixups configuration

### 4. Remove All Cargo Infrastructure

**Decision**: Completely remove Cargo build files after migration is complete.

**Rationale**:
- Single source of truth for build configuration
- Eliminates confusion about which build system to use
- Reduces maintenance burden
- Forces commitment to Buck2

**Alternatives Considered**:
- Maintain both systems: Rejected due to maintenance overhead
- Keep Cargo as fallback: Rejected as it undermines Buck2 adoption
- Gradual deprecation: Rejected in favor of clean cut-over

**Trade-offs**:
- No fallback if Buck2 has issues
- Can't use Cargo-only tools directly
- Harder to contribute for developers only familiar with Cargo

### 5. Bootstrap Tools in buck/bin/ Directory

**Decision**: Place all dotslash bootstrap scripts in a `buck/bin/` directory.

**Rationale**:
- Clear organization of build tools
- Follows thoughtpolice/a repository pattern
- Separates bootstrap tools from source code
- Easy to find and update tool versions

**Alternatives Considered**:
- Root directory placement: Rejected due to clutter
- Hidden directory (.buck/): Rejected as it's less discoverable
- tools/ directory: Rejected to clearly associate with Buck2

**Trade-offs**:
- Additional directory structure
- Slightly longer paths to tools

### 6. Migrate CI/CD Completely to Buck2

**Decision**: Update all CI/CD pipelines to use Buck2 exclusively.

**Rationale**:
- Ensures Buck2 is production-ready
- Leverages Buck2's superior caching
- Faster CI builds with better parallelization
- Consistent with local development

**Alternatives Considered**:
- Keep Cargo in CI: Rejected as it doubles maintenance
- Hybrid approach: Rejected due to complexity
- External CI service: Rejected due to vendor lock-in

**Trade-offs**:
- Initial migration effort
- Need to recreate quality checks (fmt, clippy)
- Potential CI downtime during migration

## Implementation Strategy

### Phased Rollout
1. **Phase 1**: Bootstrap and tool setup (low risk)
2. **Phase 2**: IDE integration (developer-facing)
3. **Phase 3**: Feature parity verification (validation)
4. **Phase 4**: CI/CD migration (high visibility)
5. **Phase 5**: Documentation (communication)
6. **Phase 6**: Cargo removal (point of no return)
7. **Phase 7**: Enhancements (optimization)

### Rollback Strategy
- Keep Cargo files in a separate branch until migration is proven
- Document any Buck2-specific changes that would need reverting
- Maintain ability to regenerate Cargo.toml from Buck files if needed

### Success Metrics
- Build time improvement (target: 30% faster with caching)
- Developer onboarding time (target: < 5 minutes)
- CI pipeline duration (target: 25% reduction)
- Zero loss of IDE functionality
- All tests passing with Buck2

## Risks and Mitigations

### Risk: Buck2 Stability
**Mitigation**: Pin specific Buck2 version in dotslash config, test thoroughly before updating.

### Risk: Developer Resistance
**Mitigation**: Comprehensive documentation, migration guide, and support period.

### Risk: Missing Cargo Features
**Mitigation**: Identify critical features early, implement Buck2 equivalents or workarounds.

### Risk: Third-party Tool Incompatibility
**Mitigation**: Survey tools in use, provide Buck2 alternatives or adapters.

### Risk: Performance Regression
**Mitigation**: Benchmark before and after, optimize Buck2 configuration as needed.

## Long-term Vision

This migration positions Rue to:
- Scale to larger codebases efficiently
- Leverage advanced Buck2 features (remote execution, distributed caching)
- Integrate with other Buck2-based projects
- Benefit from Meta's continued investment in Buck2
- Achieve faster incremental builds and better parallelization

## References

- [thoughtpolice/a repository](https://github.com/thoughtpolice/a) - Example of Buck2 with dotslash
- [dtolnay/buck2-rustc-bootstrap](https://github.com/dtolnay/buck2-rustc-bootstrap) - Rust compiler built with Buck2
- [facebook/buck2](https://github.com/facebook/buck2) - Official Buck2 repository
- [facebook/dotslash](https://github.com/facebook/dotslash) - Dotslash tool
- [Buck2 Rust integration](https://github.com/facebook/buck2/tree/main/integrations/rust-project) - rust-project tool source