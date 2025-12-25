+++
title = "Builtins"
weight = 5
template = "spec/page.html"
+++

# Builtins

{{ rule(id="2.5:1", cat="normative") }}

A builtin is a compiler-provided construct prefixed with `@`.

{{ rule(id="2.5:2", cat="normative") }}

```ebnf
builtin = "@" IDENT "(" [ builtin_args ] ")" ;
builtin_args = builtin_arg { "," builtin_arg } ;
builtin_arg = IDENT | expression | type ;
```

{{ rule(id="2.5:3", cat="normative") }}

The `@` prefix distinguishes builtins from user-defined constructs.

{{ rule(id="2.5:4", cat="legality-rule") }}

Using an unknown builtin name is a compile-time error.

## Builtin Kinds

{{ rule(id="2.5:5", cat="normative") }}

There are two kinds of builtins, distinguished by their syntactic position:

| Kind | Position | Purpose | Examples |
|------|----------|---------|----------|
| Intrinsic | Expression | Produces a value | `@dbg`, `@size_of`, `@align_of` |
| Directive | Before item/statement | Modifies compiler behavior | `@allow`, `@copy` |

{{ rule(id="2.5:6", cat="normative") }}

An intrinsic builtin appears where an expression is expected and evaluates to a value. See [Intrinsic Expressions](@/04-expressions/13-intrinsics.md) for details.

{{ rule(id="2.5:7", cat="normative") }}

A directive builtin appears before an item or statement and modifies how the compiler processes that construct. See [Directives](#directives) for details.

# Directives

{{ rule(id="2.5:8", cat="normative") }}

A directive is a builtin that modifies the behavior of the immediately following item or statement.

{{ rule(id="2.5:9", cat="normative") }}

```ebnf
directive = "@" IDENT "(" [ directive_args ] ")" ;
directive_args = directive_arg { "," directive_arg } ;
directive_arg = IDENT ;
```

{{ rule(id="2.5:10", cat="legality-rule") }}

A directive **MUST** be immediately followed by an item or statement. A directive at the end of a file or block without a following construct is a compile-time error.

## `@allow`

{{ rule(id="2.5:11", cat="normative") }}

The `@allow` directive suppresses specific compiler warnings for the following item or statement.

{{ rule(id="2.5:12", cat="normative") }}

`@allow` accepts one or more warning names as arguments.

{{ rule(id="2.5:13", cat="normative") }}

The following warning names are recognized:

| Warning Name | Description |
|--------------|-------------|
| `unused_variable` | Variable is declared but never used |
| `unused_function` | Function is declared but never called |
| `unreachable_code` | Code that can never be executed |
| `unreachable_pattern` | Match arm pattern that can never match |

{{ rule(id="2.5:14", cat="legality-rule") }}

Using an unrecognized warning name in `@allow` is a compile-time error.

### Suppressing Unused Variable Warnings

{{ rule(id="2.5:15", cat="normative") }}

When `@allow(unused_variable)` precedes a let statement, no unused variable warning is emitted for that binding.

{{ rule(id="2.5:16") }}

```rue
fn main() -> i32 {
    @allow(unused_variable)
    let x = 42;  // no warning, even though x is unused
    0
}
```

{{ rule(id="2.5:17", cat="normative") }}

When `@allow(unused_variable)` precedes a function definition, no unused variable warnings are emitted for any bindings within that function.

{{ rule(id="2.5:18") }}

```rue
@allow(unused_variable)
fn example() -> i32 {
    let a = 1;  // no warning
    let b = 2;  // no warning
    0
}
```

### Suppressing Unused Function Warnings

{{ rule(id="2.5:19", cat="normative") }}

When `@allow(unused_function)` precedes a function definition, no unused function warning is emitted for that function.

{{ rule(id="2.5:20") }}

```rue
@allow(unused_function)
fn helper() {
    // This function is never called, but no warning is emitted
}

fn main() -> i32 {
    0
}
```

### Suppressing Unreachable Code Warnings

{{ rule(id="2.5:21", cat="normative") }}

When `@allow(unreachable_code)` precedes a function definition, no unreachable code warnings are emitted for code within that function.

{{ rule(id="2.5:22") }}

```rue
@allow(unreachable_code)
fn example() -> i32 {
    return 0;
    let x = 42;  // unreachable, but no warning
    x
}
```

### Multiple Warnings

{{ rule(id="2.5:23", cat="normative") }}

Multiple warning names may be specified in a single `@allow` directive, separated by commas.

{{ rule(id="2.5:24") }}

```rue
@allow(unused_variable, unreachable_code)
fn example() -> i32 {
    let x = 1;
    return 0;
    let y = 2;
    0
}
```

### Relationship to Underscore Prefix

{{ rule(id="2.5:25", cat="normative") }}

The underscore prefix convention (e.g., `_unused`) and `@allow(unused_variable)` are both valid ways to suppress unused variable warnings. The underscore prefix is more concise for individual variables; `@allow` is useful when suppressing warnings for an entire function or when the variable name should not have an underscore prefix.

{{ rule(id="2.5:26") }}

```rue
fn main() -> i32 {
    let _x = 42;                    // underscore prefix suppresses warning

    @allow(unused_variable)
    let important_name = 42;        // @allow preserves the meaningful name

    0
}
```

## `@copy`

{{ rule(id="2.5:27", cat="normative") }}

The `@copy` directive marks a struct type as a Copy type.

{{ rule(id="2.5:28", cat="normative") }}

`@copy` must appear immediately before a struct definition.

{{ rule(id="2.5:29", cat="normative") }}

`@copy` takes no arguments.

{{ rule(id="2.5:30") }}

```rue
@copy
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;  // p is copied, not moved
    p.x + q.x   // both are valid
}
```

{{ rule(id="2.5:31", cat="informative") }}

See [Move Semantics](@/03-types/08-move-semantics.md#the-copy-directive) for the full semantics of `@copy` structs.
