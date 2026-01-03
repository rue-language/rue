+++
title = "Standard Library"
sort_by = "weight"
template = "std/section.html"
page_template = "std/page.html"
+++

The Rue standard library provides common utilities for Rue programs. It is organized into modules that can be imported using `@import("std")`.

## Importing the Standard Library

The standard library is **not** implicitly imported. You must explicitly import what you need:

```rue
const std = @import("std");
const math = std.math;

fn main() -> i32 {
    math.abs(-42)
}
```

Or import specific modules directly:

```rue
const math = @import("std").math;

fn main() -> i32 {
    math.max(10, 20)
}
```

## Module Structure

The standard library is organized as a module tree:

```
std/
  _std.rue      # Module root - re-exports submodules
  math.rue      # Mathematical utilities
```

When you write `@import("std")`, the compiler resolves this to the `_std.rue` file in the standard library directory. This file re-exports the submodules:

```rue
// std/_std.rue
pub const math = @import("math.rue");
```

## Available Modules

| Module | Description |
|--------|-------------|
| [`math`](math/) | Mathematical functions: `abs`, `min`, `max`, `clamp` |

## Future Modules

As Rue matures, the standard library will grow to include:

- **io** - Input/output operations
- **collections** - Data structures like `Vec`, `HashMap`
- **strings** - String manipulation utilities
- **mem** - Memory utilities

## Design Philosophy

The Rue standard library follows several design principles:

1. **Explicit imports** - No implicit prelude; all dependencies are visible
2. **Lazy analysis** - Only imported code is analyzed, enabling fast compilation
3. **File = module** - Each `.rue` file is a module; the filesystem is the source of truth
4. **Simple visibility** - Just `pub` (public) or nothing (private)

For more details on the module system, see [ADR-0026: Module System](https://github.com/rue-language/rue/blob/main/docs/designs/0026-module-system.md).
