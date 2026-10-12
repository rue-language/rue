# Language server

`rue-lsp` is a Language Server Protocol server for editors. Like the
[MCP server](mcp.md), it is a thin adapter over compiler-owned artifacts: it
adds no parser, name resolver, or diagnostic renderer of its own.

Build it and point an editor at the binary, or run it through Buck so the
repository's standard library is supplied as `RUE_STD_PATH`:

```console
./buck2 build //crates/rue-lsp:rue-lsp --show-full-output
./buck2 run //crates/rue-lsp:server
```

The server speaks LSP over stdio (`--stdio` is accepted and ignored) and
reports UTF-16 positions.

## What it offers

| Request | Source of the answer |
|---|---|
| Diagnostics (errors and warnings) | The whole program, checked on open and on save |
| Syntax errors while typing | The live buffer, reparsed on every change |
| Document symbols (outline) | The live buffer's syntax view |
| Go to definition, references, document highlight | Syntax views of open buffers and of every file in a checked program |
| Hover | The declaration's signature and its `///` comment |
| Completion | Binders in scope, the buffer's declarations, built-in types, keywords |
| Workspace symbols | Declarations of open buffers and checked programs |
| Semantic tokens | The live buffer's token view |
| Quick fixes | The compiler's own suggestions on a diagnostic |

## How it works

**Whole-program checks.** On open and on save, the server checks the program
the file belongs to through a retained `FilesystemCompilerHost`, the same host
the CLI, `--watch`, and the compiler service use. A check reobserves the
root's import closure, acquires reached toolchain modules, and stops at
ADR-0068's codegen-ready endpoint, so it reports every diagnostic a build
would without emitting objects or linking. Each root keeps its host, so later
checks reuse whatever the edit did not invalidate. Diagnostics are the
compiler's `--error-format json` records ([diagnostics.md](diagnostics.md))
mapped to LSP coordinates: notes and helps follow the message, secondary spans
become related information, and suggestions become quick fixes.

Analysis is rooted, as in a build: a function that nothing reachable from the
root calls is not analyzed, and so reports no semantic diagnostics.

The program a file belongs to is, in order:

1. the `root` initialization option, if set;
2. the file itself, if it declares `fn main`;
3. a program the server already checked whose closure contains the file;
4. the nearest `main.rue` in the file's directory or an enclosing one, up to
   the workspace folder;
5. otherwise the file alone, rooted at its `test` items if it declares any.

**Live buffers.** Each open buffer has its own `CompilerSession` over a
one-file snapshot of the unsaved text. It parses only; it never reads imports
or analyzes semantics. While a buffer does not parse, its syntax errors
replace the saved-state diagnostics for that file, and navigation keeps using
the last revision that parsed.

**Navigation** is syntactic, over the compiler's canonical syntax view.
Locals resolve by lexical scope (parameters, earlier `let`s in enclosing
blocks, `for` binders, and match-arm bindings, with shadowing).
`module.item` through `const module = @import("file.rue")` resolves into the
imported file. Other member accesses (`value.field`, `value.method()`) are
resolved by name across the known types, because the receiver's type is not
known without semantic queries; they can return several candidates.

## Configuration

`initializationOptions`, all optional:

| Option | Meaning |
|---|---|
| `root` | The program root to check every file against, relative to the workspace folder |
| `stdPath` | The standard library root; defaults to `RUE_STD_PATH` |
| `preview` | Preview feature names to enable, as for `--preview` |

## Limits

- Whole-program diagnostics reflect saved files. Unsaved edits get syntax
  errors only, and saved-state positions do not move with unsaved edits.
- Hover shows declarations, not inferred types, and navigation does not use
  semantic resolution.
- The server is single-threaded: a check runs to completion before the next
  message is read.
- Rue's Rust toolchain builds with `panic = "abort"`, so an internal
  compiler error ends the server process; the editor restarts it.
- Up to four programs are retained at once, each under the compiler service's
  retention budget; the least recently checked is dropped first.
