# Buck2 IDE Setup Guide

This guide explains how to set up your development environment for the Rue compiler with Buck2 and Rust-Analyzer integration.

## Overview

The Rue project uses Buck2 as the primary build system, but IDE support requires generating a `rust-project.json` file for Rust-Analyzer to understand the project structure. This guide walks you through the complete setup process.

## Prerequisites

- **Linux x86-64**: Buck2 binaries are currently only available for Linux
- **VS Code**: This guide focuses on VS Code integration, but the rust-project.json works with any Rust-Analyzer-compatible editor
- **Rust toolchain**: Install via [rustup.rs](https://rustup.rs/)

## Step 1: Install Dotslash

Buck2 binaries in this project use [dotslash](https://github.com/facebook/dotslash) for cross-platform binary distribution. You need to install dotslash first.

### Option A: Install via Cargo (Recommended)

```bash
cargo install dotslash
```

### Option B: Download Binary Directly

```bash
# Download for Linux x86-64
curl -L -o dotslash https://github.com/facebook/dotslash/releases/latest/download/dotslash-linux-x86_64
chmod +x dotslash
sudo mv dotslash /usr/local/bin/
```

### Verify Installation

```bash
dotslash --version
```

You should see output like: `dotslash 0.x.x`

## Step 2: Generate rust-project.json

Once dotslash is installed, you can generate the `rust-project.json` file that Rust-Analyzer needs.

### Using the Provided Script

```bash
# From the project root
./scripts/update-rust-project.sh
```

This script will:
- Check that Buck2 tools are available
- Run the rust-project command
- Validate the generated JSON
- Provide helpful error messages if something goes wrong

### Manual Generation

If you prefer to run the command manually:

```bash
# From the project root
./buck/bin/rust-project develop //crates/... --out rust-project.json
```

### Expected Output

When successful, you should see:
```
🔧 Updating rust-project.json for Rust-Analyzer...
📁 Working directory: /workspace
🔨 Running: /workspace/buck/bin/rust-project develop //crates/... --out rust-project.json
✅ Successfully generated rust-project.json
📄 File location: /workspace/rust-project.json
📊 File size: XXXX bytes
🦀 Found XX crates in rust-project.json
🎉 Rust-Analyzer should now have updated project information!
💡 Restart VS Code or reload the Rust-Analyzer extension to pick up changes.
```

## Step 3: Configure VS Code

The project includes pre-configured VS Code settings in `.vscode/settings.json` that:
- Tell Rust-Analyzer to use `rust-project.json` instead of `Cargo.toml`
- Exclude Buck2 build artifacts from search and file explorer
- Set up file associations for `.rue` files
- Configure formatting and editor settings

### Key Settings

```json
{
    "rust-analyzer.linkedProjects": [
        "./rust-project.json"
    ],
    "rust-analyzer.cargo.buildScripts.enable": false,
    "search.exclude": {
        "**/buck-out/**": true,
        "rust-project.json": true
    }
}
```

### Available VS Code Tasks

Use `Ctrl+Shift+P` → "Tasks: Run Task" to access these tasks:

- **Update Rust Project (Buck2)**: Regenerate rust-project.json
- **Buck2: Build All Crates**: Build using Buck2
- **Buck2: Test All Crates**: Run tests using Buck2 (default test task)
- **Buck2: Clean**: Clean build artifacts
- **Cargo: Build All**: Fallback build using Cargo
- **Cargo: Test All**: Fallback test using Cargo

## Step 4: Restart VS Code

After generating `rust-project.json`, restart VS Code or reload the Rust-Analyzer extension:
- `Ctrl+Shift+P` → "Developer: Reload Window", or
- `Ctrl+Shift+P` → "Rust Analyzer: Restart Server"

## Troubleshooting

### "dotslash: command not found"

**Problem**: The dotslash binary is not in your PATH.

**Solution**: 
1. Make sure dotslash is installed (see Step 1)
2. Verify it's in your PATH: `which dotslash`
3. If installed via cargo, ensure `~/.cargo/bin` is in your PATH

### "Failed to generate rust-project.json"

**Problem**: The Buck2 rust-project command failed.

**Common causes and solutions**:

1. **Dotslash not installed**: Install dotslash (see Step 1)
2. **Network issues**: Dotslash needs to download Buck2 binaries on first run
3. **Permission issues**: Make sure `buck/bin/rust-project` is executable
4. **Build configuration issues**: Try running `buck2 build //crates/...` first

### "Rust-Analyzer shows many errors"

**Problem**: Rust-Analyzer is not picking up the Buck2 project structure.

**Solution**:
1. Verify `rust-project.json` exists in the project root
2. Check VS Code settings include `"rust-analyzer.linkedProjects": ["./rust-project.json"]`
3. Restart Rust-Analyzer: `Ctrl+Shift+P` → "Rust Analyzer: Restart Server"
4. Check the Output panel for Rust-Analyzer logs

### "Buck2 commands not found"

**Problem**: Buck2 is not available in the shell.

**Solution**:
The project uses dotslash-managed Buck2 binaries, not system-installed Buck2. Use:
- `./buck/bin/buck2` instead of `buck2`
- Or use the provided VS Code tasks

### Performance Issues

**Problem**: Rust-Analyzer is slow or using too much memory.

**Solutions**:
1. **Disable unused features**:
   ```json
   "rust-analyzer.cargo.buildScripts.enable": false,
   "rust-analyzer.diagnostics.disabled": ["unresolved-proc-macro"]
   ```

2. **Limit analysis scope** (if needed):
   ```json
   "rust-analyzer.files.excludeDirs": ["buck-out", "target", "third-party"]
   ```

3. **Increase memory limits**:
   ```json
   "rust-analyzer.server.extraEnv": {
       "RA_LOG": "info"
   }
   ```

## Development Workflow

### Regular Development

1. **Code editing**: Edit Rust files normally - Rust-Analyzer will provide completions, diagnostics, etc.
2. **Building**: Use `buck2 build //crates/...` or the VS Code build task
3. **Testing**: Use `buck2 test //crates/...` or the VS Code test task (Ctrl+Shift+P → "Tasks: Run Test Task")

### When Buck2 Configuration Changes

After modifying `BUCK` files or adding new crates:

1. Regenerate rust-project.json: `./scripts/update-rust-project.sh`
2. Restart Rust-Analyzer: `Ctrl+Shift+P` → "Rust Analyzer: Restart Server"

### Troubleshooting Build Issues

If Buck2 integration isn't working:

```bash
# Clean build cache
./buck2 clean

# Regenerate rust-project.json
./buck2 run support/buck2:rust-project

# Restart rust-analyzer
# In VS Code: Ctrl+Shift+P → "Rust Analyzer: Restart Server"
```

## Advanced Configuration

### Custom Rust-Analyzer Settings

You can add project-specific Rust-Analyzer settings to `.vscode/settings.json`:

```json
{
    "rust-analyzer.linkedProjects": ["./rust-project.json"],
    
    // Enable more detailed logging
    "rust-analyzer.server.extraEnv": {
        "RA_LOG": "rust_analyzer::project_model=debug"
    },
    
    // Customize feature flags
    "rust-analyzer.cargo.features": "all",
    
    // Customize diagnostics
    "rust-analyzer.diagnostics.enable": true,
    "rust-analyzer.diagnostics.experimental.enable": true
}
```

### Automated rust-project.json Updates

For automated workflows, you can set up a git hook or CI job to regenerate `rust-project.json` when `BUCK` files change:

```bash
#!/bin/bash
# .git/hooks/post-merge (make executable)
if git diff HEAD@{1} --name-only | grep -q "BUCK\|buck2$\|Cargo.toml"; then
    echo "Buck2 configuration changed, updating rust-project.json..."
    ./scripts/update-rust-project.sh
fi
```

## Alternative Editors

While this guide focuses on VS Code, the generated `rust-project.json` works with any editor that supports Rust-Analyzer:

### Neovim with rust-analyzer

```lua
-- In your Neovim config
require('lspconfig').rust_analyzer.setup({
    settings = {
        ["rust-analyzer"] = {
            linkedProjects = {"./rust-project.json"}
        }
    }
})
```

### Emacs with lsp-mode

```elisp
;; In your Emacs config
(with-eval-after-load 'lsp-rust
  (setq lsp-rust-analyzer-server-command '("rust-analyzer"))
  (setq lsp-rust-analyzer-linked-projects ["./rust-project.json"]))
```

## References

- [Buck2 Documentation](https://buck2.build/)
- [Dotslash Project](https://github.com/facebook/dotslash)
- [Rust-Analyzer Manual](https://rust-analyzer.github.io/manual.html)
- [Buck2 Rust Integration](https://buck2.build/docs/users/build_file/rust/)

## Getting Help

If you encounter issues not covered in this guide:

1. Check the Buck2 logs: `./buck/bin/buck2 log show`
2. Check Rust-Analyzer logs in VS Code: View → Output → "Rust Analyzer Language Server"
3. File an issue in the project repository with:
   - Your operating system and version
   - Output of `dotslash --version`
   - Output of `./scripts/update-rust-project.sh`
   - Any error messages from VS Code or Rust-Analyzer