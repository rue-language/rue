# Introduction

r[1.1:1]
This document is the Rue Language Specification. It defines the syntax and semantics of the Rue programming language.

## Scope

r[1.2:1]
This specification describes the Rue programming language as implemented by the reference compiler. It covers:

- Lexical structure (tokens, comments, whitespace)
- Types (integers, booleans, arrays, structs)
- Expressions and operators
- Statements
- Items (functions, struct definitions)
- Runtime behavior

r[1.2:2]
This specification does not cover:

- The standard library (when one exists)
- Compiler implementation details
- Platform-specific behavior beyond what is explicitly documented

## Conformance

r[1.3:1#normative]
A conforming implementation must implement all normative requirements of this specification.

r[1.3:2]
Paragraphs marked with rule categories are normative unless explicitly marked as informative. The following categories are used:

| Category | Description |
|----------|-------------|
| `legality-rule` | Compile-time requirements that must be enforced |
| `syntax` | Grammar rules defining valid program structure |
| `dynamic-semantics` | Runtime behavior requirements |
| `informative` | Explanatory text that is not normative |
| `example` | Code examples that are not normative |

## Definitions

r[1.4:1]
The following terms are used throughout this specification:

r[1.4:2]
**Expression**: A syntactic construct that evaluates to a value.

r[1.4:3]
**Statement**: A syntactic construct that performs an action but does not produce a value.

r[1.4:4]
**Item**: A top-level definition in a program, such as a function or struct.

r[1.4:5]
**Type**: A classification that determines what values an expression can produce and what operations are valid on those values.

r[1.4:6]
**Normative**: Content that defines required behavior for conforming implementations.

r[1.4:7]
**Informative**: Content that provides explanation or context but does not define required behavior.

## Notation

r[1.5:1]
Spec paragraph identifiers follow the format `{chapter}.{section}:{paragraph}`. For example, `3.1:5` refers to Chapter 3, Section 1, Paragraph 5.

r[1.5:2]
Grammar rules use Extended Backus-Naur Form (EBNF) notation:

- `=` defines a production
- `|` separates alternatives
- `{ }` indicates zero or more repetitions
- `[ ]` indicates optional elements
- `" "` indicates literal text
- `UPPERCASE` indicates terminal symbols (tokens)

r[1.5:3]
```ebnf
if_expr = "if" expression "{" block "}" [ "else" "{" block "}" ] ;
```

## Organization

r[1.6:1]
This specification is organized as follows:

- **Chapter 2: Lexical Structure** - Tokens, comments, whitespace, keywords
- **Chapter 3: Types** - Integer types, booleans, unit, never, arrays, structs
- **Chapter 4: Expressions** - Operators, control flow, function calls
- **Chapter 5: Statements** - Variable bindings, assignment
- **Chapter 6: Items** - Functions, struct definitions
- **Chapter 7: Arrays** - Fixed-size array behavior
- **Chapter 8: Runtime Behavior** - Overflow, bounds checking, panics
- **Appendix A: Grammar** - Complete EBNF grammar
- **Appendix B: Runtime Panics** - Summary of panic conditions

## Version

r[1.7:1]
This specification corresponds to version 0.1.0 of the Rue language.
