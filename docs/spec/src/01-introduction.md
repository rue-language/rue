+++
title = "Introduction"
weight = 1
template = "spec/page.html"
+++

# Introduction

{{ rule(id="1.1:1") }}

This document is the Rue Language Specification. It defines the syntax and semantics of the Rue programming language.

## Scope

{{ rule(id="1.2:1") }}

This specification describes the Rue programming language as implemented by the reference compiler. It covers:

- Lexical structure (tokens, comments, whitespace)
- Types (integers, booleans, arrays, structs)
- Expressions and operators
- Statements
- Items (functions, struct definitions)
- Runtime behavior

{{ rule(id="1.2:2") }}

This specification does not cover:

- The standard library (when one exists)
- Compiler implementation details
- Platform-specific behavior beyond what is explicitly documented

## Conformance

{{ rule(id="1.3:1", cat="normative") }}

A conforming implementation must implement all normative requirements of this specification.

{{ rule(id="1.3:2") }}

Paragraphs marked with rule categories are normative unless explicitly marked as informative. The following categories are used:

| Category | Description |
|----------|-------------|
| `legality-rule` | Compile-time requirements that must be enforced |
| `syntax` | Grammar rules defining valid program structure |
| `dynamic-semantics` | Runtime behavior requirements |
| `informative` | Explanatory text that is not normative |
| `example` | Code examples that are not normative |

## Normative Language

{{ rule(id="1.4:1") }}

This specification uses terminology from [RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119) to indicate requirement levels. The key words are interpreted as follows:

{{ rule(id="1.4:2", cat="informative") }}

**MUST** and **SHALL**: An absolute requirement. A conforming implementation is required to satisfy this.

{{ rule(id="1.4:3", cat="informative") }}

**MUST NOT** and **SHALL NOT**: An absolute prohibition. A conforming implementation is required not to do this.

{{ rule(id="1.4:4", cat="informative") }}

**SHOULD** and **RECOMMENDED**: There may be valid reasons to ignore this requirement, but the implications must be understood.

{{ rule(id="1.4:5", cat="informative") }}

**SHOULD NOT** and **NOT RECOMMENDED**: There may be valid reasons to accept this behavior, but the implications must be understood.

{{ rule(id="1.4:6", cat="informative") }}

**MAY** and **OPTIONAL**: An item is truly optional. Implementations may or may not include it.

{{ rule(id="1.4:7") }}

These keywords appear in **bold** throughout this specification to distinguish normative requirements from descriptive text.

## Definitions

{{ rule(id="1.4:8") }}

The following terms are used throughout this specification:

{{ rule(id="1.4:9") }}

**Expression**: A syntactic construct that evaluates to a value.

{{ rule(id="1.4:10") }}

**Statement**: A syntactic construct that performs an action but does not produce a value.

{{ rule(id="1.4:11") }}

**Item**: A top-level definition in a program, such as a function or struct.

{{ rule(id="1.4:12") }}

**Type**: A classification that determines what values an expression can produce and what operations are valid on those values.

{{ rule(id="1.4:13") }}

**Normative**: Content that defines required behavior for conforming implementations.

{{ rule(id="1.4:14") }}

**Informative**: Content that provides explanation or context but does not define required behavior.

{{ rule(id="1.4:15") }}

**Value**: An instance of a type. Expressions evaluate to values.

{{ rule(id="1.4:16") }}

**Coercion**: An implicit type conversion that occurs automatically during type checking. See section 3.4 for the complete set of coercions in Rue.

{{ rule(id="1.4:17") }}

**Compatible type**: A type is compatible with another type if they are the same type, or if the first type can be coerced to the second type.

{{ rule(id="1.4:18") }}

**Panic**: A runtime error condition that terminates program execution with a specific exit code. See Appendix B for the complete list of panic conditions.

## Notation

{{ rule(id="1.5:1") }}

Spec paragraph identifiers follow the format `{chapter}.{section}:{paragraph}`. For example, `3.1:5` refers to Chapter 3, Section 1, Paragraph 5.

{{ rule(id="1.5:2") }}

Grammar rules use Extended Backus-Naur Form (EBNF) notation:

- `=` defines a production
- `|` separates alternatives
- `{ }` indicates zero or more repetitions
- `[ ]` indicates optional elements
- `" "` indicates literal text
- `UPPERCASE` indicates terminal symbols (tokens)

{{ rule(id="1.5:3") }}

```ebnf
if_expr = "if" expression "{" block "}" [ "else" "{" block "}" ] ;
```

## Organization

{{ rule(id="1.6:1") }}

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

{{ rule(id="1.7:1") }}

This specification corresponds to version 0.1.0 of the Rue language.
