# mdbook-spec

An mdbook preprocessor for the Rue Language Specification.

## Attribution

This tool is inspired by and derived from the [mdbook-spec](https://github.com/rust-lang/reference/tree/master/tools/mdbook-spec)
tool used by the Rust Reference. We thank the Rust project for making their tooling
available under permissive licenses.

The original Rust Reference mdbook-spec is licensed under Apache-2.0 OR MIT.

## Features

- **Rule definitions**: Use `r[rule.id]` syntax to define linkable rule anchors
- **Rule references**: Link to rules from anywhere using `[link text]: rule.id`
- **Automatic anchors**: Rules get `#r-rule.id` anchors for deep linking

## Usage

Rules are defined using a simple bracket syntax:

```markdown
r[types.integer.signed]

A signed integer type is one of: `i8`, `i16`, `i32`, or `i64`.
```

This generates an anchor that can be linked to with `#r-types.integer.signed`.

Reference rules from other pages:

```markdown
See [integer types] for more information.

[integer types]: types.integer.signed
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
