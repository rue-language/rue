---
id: 0026
title: Module System
status: stable
tags: [architecture, compiler, modules, scalability]
created: 2026-01-01
accepted: 2026-01-04
implemented: 2026-01-04
stabilized: 2026-01-04
spec-sections: []
superseded-by:
amended-by: [0078]
---

# ADR-0026: Module System

## Status

Stable (no longer requires `--preview module_types`)

## Summary

Introduce a module system for Rue that prioritizes fast compilation through lazy semantic analysis, uses a simple "file = struct" model inspired by Zig, and provides straightforward pub/private visibility. This design supersedes the flat namespace from ADR-0023 (multi-file compilation) while preserving forward compatibility with future package management.

## Context

### Current State

ADR-0023 introduced multi-file compilation with a flat global namespace—all functions, structs, and enums are globally visible across files. This was explicitly a stepping stone:

> "The UX is admittedly awkward (`rue a.rue b.rue c.rue -o out`), but this is a stepping stone, not the final design."

We now need a proper module system that provides:
1. **Namespacing** — Avoid symbol collisions as codebases grow
2. **Encapsulation** — Hide implementation details
3. **Fast compilation** — The primary design constraint

### Design Goals

1. **Compilation speed is paramount** — Inspired by Zig's lazy analysis approach
2. **Simplicity over flexibility** — Avoid Rust's module system complexity
3. **Files are the unit of organization** — No separate "module declaration" concept
4. **Forward-compatible with packages** — Don't box out future package management

### Research Summary

We analyzed module systems from several languages:

| Language | Key Insight |
|----------|-------------|
| **Zig** | Files are structs; lazy analysis skips unreferenced code; simple pub/private |
| **Rust** | Explicit `mod`/`use` creates cognitive overhead; fine-grained visibility rarely needed |
| **Hylo** | Intra-module visibility is automatic; `pub` only affects cross-module |
| **Swift** | Multiple visibility levels add complexity without proportional benefit |
| **Go** | Directory = package; implicit file discovery; simple and fast |

**Key takeaways:**
- Zig's lazy analysis enables dramatically faster builds by only analyzing referenced code
- Rust's module system is the #2 complaint after borrow checking—too many concepts (`mod`, `use`, `pub use`, `extern crate`, visibility modifiers)
- Simple pub/private (Zig) or pub/internal/private (Hylo) covers 99% of use cases

## Decision

### Core Principle: Files Are Structs

Every `.rue` file is implicitly a struct. Importing a file returns a struct type containing all `pub` declarations from that file.

```rue
// math.rue
pub fn add(a: i32, b: i32) -> i32 { a + b }
pub fn sub(a: i32, b: i32) -> i32 { a - b }
fn helper() -> i32 { 42 }  // private, not exported

// main.rue
const math = @import("math.rue");

fn main() -> i32 {
    math.add(1, 2)  // OK
    // math.helper()  // Error: `helper` is private
}
```

### Import Syntax

Following Zig's model, `@import` is a builtin that returns a struct type containing all `pub` declarations from the imported file:

```rue
// @import returns a struct type
const math = @import("math.rue");
math.add(1, 2)

// You can alias to any name
const m = @import("math.rue");
m.add(1, 2)

// Access nested items directly
const add = @import("math.rue").add;
add(1, 2)
```

> **Implementation status:** chained member access works, including through
> nested directory modules (`std.math.abs(-7)` resolves member-by-member,
> RUE-136/RUE-122). Member access is also const-evaluable for modules and
> value constants (RUE-160): `const math = std.math;` aliases a nested
> module, and `const X = m.ANSWER;` / `m.ANSWER` at a use site resolve a
> member value constant. Binding a *function* member (`const add =
> @import("math.rue").add`) is supported as a callable alias (RUE-173). These
> aliases are not first-class runtime values: `add(1, 2)` is accepted, but
> using `add` as an ordinary expression is still rejected as a function
> reference in const/runtime value context.

The key insight: `@import("foo.rue")` is equivalent to a struct containing the file's contents:

```rue
// If math.rue contains:
pub fn add(a: i32, b: i32) -> i32 { a + b }
pub fn sub(a: i32, b: i32) -> i32 { a - b }
fn helper() -> i32 { 42 }  // private

// Then @import("math.rue") returns something like:
// struct {
//     pub fn add(a: i32, b: i32) -> i32 { ... }
//     pub fn sub(a: i32, b: i32) -> i32 { ... }
//     // helper is not visible - it's private
// }
```

**Resolution order for `@import("foo")`:**
1. Local file `foo.rue` (simple file module)
2. Directory `foo/` containing the facade file `foo/_foo.rue` (directory module)
3. (Future) Dependency named `foo` in `rue.toml`

Having BOTH `foo.rue` and `foo/_foo.rue` was a compile error (E0708, ambiguous;
retired by ADR-0078 — the extensionless path now names the facade alone
module) — mirroring Rust's E0761 rather than silently preferring one form.

### Directory Modules

A directory becomes a module when it contains a `_` prefixed facade file with
the same name as the directory — the facade lives **inside** the directory,
exactly where Rust's `mod.rs` would (this is matklad's actual proposal: the
underscore file is the `mod.rs` replacement, renamed so it sorts first and is
fuzzy-findable):

```
src/
  main.rue
  math.rue           # const math = @import("math.rue");
  utils/
    _utils.rue       # facade — the module root for utils/
    strings.rue      # Submodule
    internal.rue     # Submodule
```

> Decision history: an earlier revision of this ADR placed `_utils.rue` as a
> sibling of `utils/`, misreading the cited article (and contradicting the
> shipped `std/_std.rue`). RUE-137 ratified the in-directory placement.

The `utils/_utils.rue` file controls what the directory exports using Zig's
`pub const` pattern for re-exports:

```rue
// utils/_utils.rue — the module root for utils/

// Re-export the entire strings module
pub const strings = @import("utils/strings.rue");

// Re-export a specific function from internal
pub const helper = @import("utils/internal.rue").helper;

// internal module itself is not re-exported, so users can't access
// @import("utils").internal — they'd have to import it directly
```

Usage:

```rue
// main.rue
const utils = @import("utils");

// Access re-exported submodule
utils.strings.format("hello")

// Access re-exported function directly
utils.helper()
```

**Why `_` prefix?**
1. **Sorts first** — Within the directory's listing, `_utils.rue` appears above the submodule files, making the module entry point the first thing you see when you open the directory
2. **Unambiguous** — `foo.rue` is always a standalone file module; a directory `foo/` containing `_foo.rue` is always a directory module
3. **Self-contained modules** — unlike Rust 2018's `foo.rs` + `foo/` split, everything about the module lives in one directory: moving, vendoring, or deleting a module is a single filesystem operation

This follows matklad's suggestion from "Notes on Module System" for improving discoverability.

**No `mod` declarations needed.** Unlike Rust, there's no need to write `mod strings;` to declare that `strings.rue` exists. The filesystem is the source of truth — if the file exists, it can be imported. Re-exports are explicit `pub const` bindings, following Zig's pattern.

### Visibility: Simple pub/private

Only two visibility levels:

| Modifier | Meaning |
|----------|---------|
| `pub` | Visible to all importers |
| (none) | Private to this module (directory) |

**Rationale:** Rust's `pub(crate)`, `pub(super)`, `pub(in path)` are rarely used and add cognitive overhead. If we later need package-level visibility, we can add `pub(pkg)` without breaking existing code.

**Intra-module visibility:** Files within the same directory can access each other's non-pub items. This matches Hylo's model where `pub` only affects cross-module (cross-directory) visibility.

```rue
// utils/strings.rue
fn internal_helper() { ... }  // Private

// utils/parser.rue
const strings = @import("strings.rue");
fn parse() {
    strings.internal_helper()  // OK - same directory
}

// main.rue
const utils = @import("utils");
fn main() {
    // utils.strings.internal_helper()  // Error - different directory
}
```

### Lazy Semantic Analysis

**The key to fast compilation.** The compiler only analyzes code that is actually referenced from entry points.

```rue
// broken.rue
pub fn works() -> i32 { 42 }
pub fn broken() -> i32 { "not an int" }  // Type error!

// main.rue
const broken = @import("broken.rue");
fn main() -> i32 {
    broken.works()  // Only this is analyzed
    // broken.broken() is never called, so its error is NOT reported
}
```

**Trade-off:** Errors in unreferenced code are silently ignored. This is the same trade-off Zig makes, and it's deliberate:
- Faster builds (don't analyze unused code)
- Smaller binaries (unused code isn't codegen'd)
- IDE tooling may need separate "check all" mode

**Implementation:** Semantic analysis starts at `main()` and follows references. Each declaration is analyzed at most once, with results cached.

### Entry Points

- A program must have exactly one `pub fn main()`
- The file containing `main()` is the entry point for analysis
- Libraries (future) will have different entry point rules

### Standard Library

The standard library is **not** implicitly imported (unlike Hylo). Users must explicitly import what they need:

```rue
const io = @import("std").io;
const Vec = @import("std").collections.Vec;
```

**Rationale:** Explicit imports make dependencies clear and don't pollute the namespace. This also makes the prelude smaller and compilation faster.

### Circular Imports

Circular imports between files are **allowed** at the type level but not at the value level:

```rue
// a.rue
const b = @import("b.rue");
pub struct Foo { b: b.Bar }  // OK - type reference

// b.rue
const a = @import("a.rue");
pub struct Bar { a: a.Foo }  // OK - type reference
```

```rue
// a.rue
const b = @import("b.rue");
pub const X: i32 = b.Y + 1;  // Error - circular value dependency

// b.rue
const a = @import("a.rue");
pub const Y: i32 = a.X + 1;  // Error - circular value dependency
```

The compiler detects cycles during lazy analysis and reports clear errors.

### Relationship to Multi-File Compilation (ADR-0023)

This ADR **supersedes** the flat namespace from ADR-0023. The compilation model remains similar:

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  main.rue   │     │  math.rue   │     │  utils/     │
└──────┬──────┘     └──────┬──────┘     └──────┬──────┘
       │                   │                   │
       ▼                   ▼                   ▼
┌─────────────────────────────────────────────────────┐
│              Lazy Semantic Analysis                  │
│  (starts at main, follows imports on demand)        │
└─────────────────────────────────────────────────────┘
                           │
                           ▼
                    ┌─────────────┐
                    │   Codegen   │
                    └─────────────┘
```

**Key change:** Instead of merging all symbols into a flat namespace, we now:
1. Start analysis at `main()`
2. When encountering `@import("foo.rue")`, lazily analyze `foo.rue`
3. Only analyze declarations that are actually referenced

### Future: Package Management

This module system is designed to be forward-compatible with packages. A future `rue.toml` would map package names to sources:

```toml
# rue.toml (future)
[dependencies]
http = { url = "https://...", hash = "sha256:..." }
json = { path = "../json-lib" }
```

The resolution order becomes:
1. Local file `foo.rue` (simple file module)
2. Directory `foo/` containing the facade `foo/_foo.rue` (directory module)
3. Dependency `foo` from `rue.toml`

The import syntax (`@import("foo")`) remains unchanged—the package manager just adds another resolution step.

## Implementation Phases

All phases are complete. 40 spec tests pass.

### Phase 1: Basic Module Imports ✓

**Goal:** `@import("foo.rue")` imports `foo.rue` from the same directory.

- [x] `@import` parses as `IntrinsicCall`
- [x] Resolve relative file paths from importing file
- [x] Load and parse imported files on demand
- [x] Create Module type for imported file
- [x] Stabilized (preview gate removed)

### Phase 2: Module Member Access ✓

**Goal:** Access module members via `module.symbol` qualified syntax.

- [x] Handle FieldGet on Module types → lookup in module's exports
- [x] Visibility checking (only `pub` declarations accessible)
- [x] Type checking for module member access
- [x] Codegen for qualified function calls

### Phase 3: Directory Modules ✓

**Goal:** `@import("foo")` can import the facade `foo/_foo.rue`, which re-exports submodules from `foo/`.

- [x] Directory module resolution (`foo/_foo.rue` facade pattern)
- [x] Re-exports via `pub const`
- [x] Intra-directory visibility rules

### Phase 4: Lazy Analysis ✓

**Goal:** Only analyze referenced code.

- [x] Sema starts from entry point (main)
- [x] On-demand declaration analysis
- [x] Caching for analyzed declarations
- [x] Referenced function tracking

### Phase 5: Standard Library Structure ✓

**Goal:** Organize std as proper modules.

- [x] `std/` directory with `_std.rue` root
- [x] `@import("std")` resolution
- [x] `std.math` submodule with abs, min, max, clamp

## Consequences

### Positive

- **Fast compilation** — Lazy analysis skips unreferenced code
- **Simple mental model** — Files are structs, `pub` or private
- **No boilerplate** — No `mod` declarations, no `extern crate`
- **Forward-compatible** — Package system can layer on top

### Negative

- **Silent errors in unused code** — Trade-off for speed
- **Less encapsulation than Rust** — No `pub(crate)` equivalent (yet)
- **Different from Rust** — Users familiar with Rust need to unlearn some habits

### Neutral

- **Directory structure matters** — File layout determines module structure
- **No implicit prelude** — More explicit, but more typing for common imports

## Open Questions

1. **IDE support for lazy analysis** — Should we provide a `rue check --all` mode that analyzes everything regardless of reachability? (Not needed immediately, but maybe someday.)

## Future Work

- **Package management** — `rue.toml`, dependency resolution, content-addressed packages
- **Package visibility** — `pub(pkg)` for package-internal APIs if needed
- **Incremental compilation** — Build on lazy analysis for file-level caching
- **Conditional compilation** — `#[cfg(...)]` attributes for platform-specific code

## References

- [ADR-0023: Multi-File Compilation](0023-multi-file-compilation.md) — Superseded flat namespace
- [ADR-0025: Comptime](0025-comptime.md) — Related compile-time evaluation
- [Notes on Module System](https://matklad.github.io/2021/11/27/notes-on-module-system.html) — matklad's analysis of module system design principles (key inspiration for filesystem-based modules, no `mod` declarations)
- [Zig Module System](https://ziglang.org/learn/build-system/) — "Files are structs" inspiration
- [Zig Sema: Lazy Analysis](https://mitchellh.com/zig/sema) — Mitchell Hashimoto's explanation
- [Zig and Rust Comparison](https://matklad.github.io/2023/03/26/zig-and-rust.html) — matklad's analysis
- [Rust Module System Criticism](https://without.boats/blog/the-rust-module-system-is-too-confusing/) — What to avoid
- [Hylo Modules](https://docs.hylo-lang.org/language-tour/modules) — Intra-module visibility inspiration
`
