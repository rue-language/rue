# Rue Language Support for VS Code

A VS Code extension that provides language support for Rue through the Rue Language Server.

## Setup Instructions

1. **Install dependencies**:
   ```bash
   cd vscode-rue-extension
   npm install
   ```

2. **Compile the extension**:
   ```bash
   npm run compile
   ```

3. **Install the extension** (choose one method):

   **Method A: Install from folder**
   - Open VS Code
   - Press `Ctrl+Shift+P` (or `Cmd+Shift+P` on Mac)
   - Type "Extensions: Install from VSIX..."
   - But first, package it: `vsce package` (install vsce with `npm install -g vsce`)
   
   **Method B: Development mode (easier for testing)**
   - Open VS Code
   - Press `Ctrl+Shift+P` (or `Cmd+Shift+P` on Mac)  
   - Type "Developer: Install Extension from Location..."
   - Select the `vscode-rue-extension` folder

   **Method C: From VS Code dev instance**
   - Open the `vscode-rue-extension` folder in VS Code
   - Press `F5` to launch a new Extension Development Host window
   - The extension will be active in the new window

## Testing the Extension

1. **Open the rue project** in VS Code (the folder containing the rue compiler)
2. **Create a test file** with a `.rue` extension
3. **Try valid syntax**:
   ```rue
   // A simple Rue program with comments
   fn main() -> i32 {
       let x: i32 = 42;
       /* This is a multi-line comment
          that can span multiple lines */
       x
   }
   ```
4. **Try various features**:
   ```rue
   fn factorial(n: i64) -> i64 {
       let result: i64 = 1;
       let i: i64 = 1;
       while i <= n {
           result = result * i;
           i = i + 1;
       }
       result
   }
   
   fn main() -> i32 {
       if factorial(5) == 120 {
           1  // Success!
       } else {
           0  // Failed
       }
   }
   ```
5. **Try invalid syntax** to see error reporting:
   ```rue
   fn main( {  // Missing closing paren
       let x = 42;  // Missing type annotation
       y + 1  // Undefined variable
   }
   ```

## Features

- **Syntax highlighting** for Rue keywords, numbers, operators, types, and comments
- **Real-time error detection** with both syntax and type error reporting
- **Semantic token highlighting** for enhanced code coloring
- **Comment support** with proper highlighting for single-line (`//`) and multi-line (`/* */`) comments
- **Auto-completion** for brackets, quotes, built-in functions, and keywords
- **Code completion** for all built-in functions (exit, println_i64, println_i32, println_bool, println_unit, input)
- **Hover information** showing function signatures and documentation
- **Automatic language server startup** when opening .rue files
- **Accurate error positioning** with line/column information

## Configuration

The extension can be configured in VS Code settings:

- `rue.languageServer.path`: Path to cargo (default: "cargo")
- `rue.languageServer.args`: Arguments for running rue-lsp (default: ["run", "-p", "rue-lsp", "--bin", "rue-lsp"])

## Troubleshooting

- **Language server not starting**: Check that you have the rue project open and cargo is in your PATH
- **No syntax highlighting**: Make sure the file has a `.rue` extension
- **No error detection**: Check the Output panel (View → Output) and select "Rue Language Server" to see logs