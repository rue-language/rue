# Rue Language Server (LSP)

A Language Server Protocol implementation for the Rue programming language.

## Features

- **Syntax Error Diagnostics**: Real-time syntax error reporting with accurate line/column positions
- **Type Error Diagnostics**: Full semantic analysis including type checking and inference
- **Semantic Token Highlighting**: Proper highlighting for all language constructs including comments
- **Multi-Error Reporting**: Shows all syntax and type errors simultaneously
- **Complete LSP Lifecycle**: Initialize, shutdown, and document management
- **File Watching**: Responds to document open, change, and close events

## Usage

### Starting the Server

```bash
# With Cargo
cargo run -p rue-lsp --bin rue-lsp

# The server communicates via stdin/stdout using JSON-RPC
```

### Editor Integration

The LSP server can be integrated with any editor that supports LSP, such as:

- **VS Code**: Create a language extension with the language server path
- **Neovim**: Configure with `nvim-lspconfig`
- **Emacs**: Use `lsp-mode` configuration
- **Other editors**: Any editor with LSP support

### VS Code Integration

A complete VS Code extension is provided in `vscode-rue-extension/`. To set it up:

1. **Install dependencies**:
   ```bash
   cd vscode-rue-extension
   npm install
   npm run compile
   ```

2. **Install the extension** (choose one method):

   **Method A: Development mode (recommended for testing)**
   - Open the `vscode-rue-extension` folder in VS Code
   - Press `F5` to launch a new Extension Development Host window
   - The extension will be active in the new window
   
   **Method B: Install from folder**
   - Open VS Code Command Palette (`Ctrl+Shift+P`)
   - Type "Developer: Install Extension from Location..."
   - Select the `vscode-rue-extension` folder

3. **Test the integration**:
   - Open the rue project folder in VS Code
   - Create a file with `.rue` extension
   - Try valid syntax: `fn main() { 42 }`
   - Try invalid syntax: `fn main( { 42 }` (missing closing paren)
   - You should see real-time syntax error highlighting!

### Features in VS Code
- **Syntax highlighting** for keywords, numbers, operators, comments, and all language constructs
- **Real-time error detection** with red squiggly underlines showing exact error locations
- **Type error reporting** with helpful error messages
- **Semantic token highlighting** for enhanced code coloring
- **Auto-completion** for brackets and quotes
- **Automatic language server startup** when opening .rue files

## Implementation Details

- Built on `tower-lsp` for robust LSP protocol handling
- Uses Rue's complete compiler pipeline including lexer, parser, and semantic analyzer
- Provides both syntax and semantic error diagnostics
- Accurate line/column position reporting with proper UTF-16 handling
- Semantic token support for enhanced syntax highlighting
- Async/await architecture with Tokio runtime
- Maintains document state in memory for fast responses

## Current Limitations

- No code completion beyond auto-closing brackets
- No hover information or documentation
- No go-to-definition or find-references yet
- No code formatting or refactoring support

## Future Enhancements

- Code completion with type-aware suggestions
- Hover information showing types and documentation
- Go-to-definition and find-references
- Code formatting (rue fmt integration)
- Refactoring support (rename, extract function)
- Inlay hints for type annotations
- Code actions and quick fixes