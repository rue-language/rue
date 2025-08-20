# Dependency Management

This guide explains how to manage Rust dependencies in the Rue project using Buck2 and Reindeer.

## Overview

Rue uses Reindeer to convert Cargo.toml dependencies into Buck2 build files. Dependencies are managed in `third-party/rust/` and are automatically converted to Buck2 targets.

## Quick Reference

| Task | Command |
|------|---------|
| Update all dependencies | `./buck2 bxl //tools/bxl:deps.bxl:update` |
| Add a new dependency | `./buck2 bxl //tools/bxl:deps.bxl:add -- --crate=<name> --version=<version>` |
| Check dependency status | `./buck2 bxl //tools/bxl:deps.bxl:check` |
| Create a fixup for a crate | `./buck2 bxl //tools/bxl:deps.bxl:fixup -- --crate=<name>` |

## Adding a New Dependency

1. **Get instructions for adding the dependency:**
   ```bash
   ./buck2 bxl //tools/bxl:deps.bxl:add -- --crate=serde --version=1.0
   ```

2. **Edit `third-party/rust/Cargo.toml`:**
   Add your dependency to the `[dependencies]` section:
   ```toml
   [dependencies]
   serde = "1.0"
   ```

3. **Update your crate's BUCK file:**
   Add the dependency to the `deps` array:
   ```python
   rust_library(
       name = "my-crate",
       deps = [
           "//third-party/rust:serde",
           # other deps...
       ],
   )
   ```
   Note: Replace hyphens with underscores in the dependency name.

4. **Regenerate Buck2 build files:**
   ```bash
   ./buck2 bxl //tools/bxl:deps.bxl:update
   ```
   Then follow the instructions to run reindeer.

5. **Test the changes:**
   ```bash
   ./buck2 build //...
   ./buck2 test //crates/...
   ```

## Updating Dependencies

To update all dependencies to their latest compatible versions:

1. **Run the update command:**
   ```bash
   ./buck2 bxl //tools/bxl:deps.bxl:update
   ```

2. **Follow the instructions** to:
   - Change to `third-party/rust/`
   - Run `cargo update` to update Cargo.lock
   - Run `../../reindeer buckify` to regenerate Buck2 files
   - Test the changes

## Creating Fixups

Some crates require special configuration (fixups) to work with Buck2. Common reasons include:
- Build scripts that need environment variables
- Platform-specific dependencies
- Source files that should be excluded

### When to Create a Fixup

You'll need a fixup if you see warnings like:
- "Build script not supported"
- "Platform-specific dependency not handled"
- "Source file not found"

### Creating a Fixup

1. **Get fixup template:**
   ```bash
   ./buck2 bxl //tools/bxl:deps.bxl:fixup -- --crate=problematic-crate
   ```

2. **Create the fixup file:**
   ```bash
   mkdir -p third-party/rust/fixups/problematic-crate
   ```

3. **Edit `third-party/rust/fixups/problematic-crate/fixups.toml`:**
   ```toml
   # Example: Set environment variables for build script
   [[buildscript]]
   [buildscript.rustc_env]
   SOME_VAR = "value"

   # Example: Add platform-specific dependencies
   [platform_fixup."cfg(target_os = \"linux\")"]
   extra_deps = ["//third-party/rust:linux-specific-dep"]

   # Example: Exclude problematic source files
   omit_srcs = ["src/problematic.rs"]
   ```

4. **Regenerate Buck2 files:**
   ```bash
   cd third-party/rust && ../../reindeer buckify
   ```

### Common Fixup Patterns

Look at existing fixups for examples:
- `third-party/rust/fixups/libc/` - Platform-specific configuration
- `third-party/rust/fixups/proc-macro2/` - Build script environment
- `third-party/rust/fixups/serde/` - Feature flags

## Checking Dependency Status

To check if dependencies are up to date:

```bash
./buck2 bxl //tools/bxl:deps.bxl:check
```

This will provide instructions for:
- Checking for outdated dependencies with `cargo outdated`
- Verifying Buck2 files are in sync with Cargo.toml

## Reindeer Tool

The project includes Reindeer via dotslash at `./reindeer`. This tool converts Cargo dependencies to Buck2 build files.

### Manual Reindeer Commands

While the BXL scripts handle most tasks, you can run reindeer directly:

```bash
# Regenerate Buck2 files from Cargo.toml
cd third-party/rust && ../../reindeer buckify

# Verify Buck2 files are in sync
cd third-party/rust && ../../reindeer verify

# Vendor dependencies (if needed)
cd third-party/rust && ../../reindeer vendor
```

## Troubleshooting

### "Reindeer not found"
Make sure you have dotslash installed:
```bash
curl -L https://github.com/facebook/dotslash/releases/latest/download/dotslash-linux.tar.xz | tar -xJ
sudo install dotslash /usr/local/bin/
```

### "Build script not supported"
Create a fixup file for the problematic crate. See "Creating Fixups" above.

### "Dependency not found in Buck2"
1. Ensure the dependency is in `third-party/rust/Cargo.toml`
2. Run `./buck2 bxl //tools/bxl:deps.bxl:update` to regenerate Buck2 files
3. Use underscores instead of hyphens in Buck2 target names

### "Version conflict"
1. Check `third-party/rust/Cargo.lock` for the resolved versions
2. Update `third-party/rust/Cargo.toml` with specific versions if needed
3. Run `cargo update` in `third-party/rust/` to resolve conflicts

## Best Practices

1. **Test after updates**: Always run tests after updating dependencies
2. **Use exact versions**: Specify exact versions in Cargo.toml for reproducibility
3. **Document fixups**: Add comments in fixup files explaining why they're needed
4. **Check CI**: Ensure CI passes before merging dependency updates
5. **Update incrementally**: Update one major dependency at a time to isolate issues