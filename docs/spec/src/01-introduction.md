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
- Types (integers, booleans, arrays, structs, enums, strings, move semantics, destructors)
- Expressions and operators (including compile-time expressions)
- Statements
- Items (functions, structs, enums, constants)
- Arrays
- Runtime behavior (overflow, bounds, panics)
- Unchecked code and raw pointers
- Modules and program composition

{{ rule(id="1.2:2") }}

This specification does not cover:

- The standard library (when one exists)
- Compiler implementation details
- Platform-specific behavior beyond what is explicitly documented

## Conformance

{{ rule(id="1.3:1", cat="normative") }}

A conforming implementation **MUST** implement all normative requirements of this specification.

{{ rule(id="1.3:1a", cat="informative") }}

This specification has a companion **formal core** (`docs/formal/`) that states the ownership and type judgments it covers as precise inference and reduction rules. The prose, the formal core, and a conforming compiler are three views of one language and **MUST agree where they overlap**; a genuine disagreement is a defect in the specification, reconciled by fixing whichever of the three is wrong — no artifact wins by precedence. The formal core is the more precise statement of the judgments within its scope and may *sharpen* imprecise prose (which is not a disagreement); where the formal core is **silent**, the prose governs.

{{ rule(id="1.3:2") }}

A paragraph's `cat=` marker classifies it. A paragraph carrying **no** `cat=`
marker is **informative**: normativity is opted into explicitly, never inherited
by default. B.1:5 states the same default, and the traceability tooling
(`crates/rue-spec/src/traceability.rs`) applies it when computing normative
coverage.

It follows that a paragraph stating a requirement **MUST** carry an explicit
normative category. An uncategorised paragraph cannot be relied on by a
normative rule elsewhere: it imposes no requirement and the coverage gate does
not track it. Where a normative rule needs to ground itself on such a
paragraph, the paragraph is given its own category rather than borrowing
normativity from the rule that cites it.

The following categories are used:

| Category | Description | Normative? |
|----------|-------------|------------|
| `normative` | A general requirement on a conforming implementation | Yes |
| `legality-rule` | Compile-time requirements that must be enforced | Yes |
| `syntax` | Grammar rules defining valid program structure | Yes |
| `dynamic-semantics` | Runtime behavior requirements | Yes |
| `undefined-behavior` | A condition whose behavior is undefined (imposes no requirement, but identifies the hazard) | Yes |
| `informative` | Explanatory text that is not normative | No |
| `example` | Code examples that are not normative | No |

## Behavior Categories

{{ rule(id="1.3:3", cat="informative") }}

Beyond the paragraph categories above, this specification classifies the
*behavior* of a program into four categories. These categories describe what a
conforming implementation is required to guarantee when a program exhibits the
behavior. ADR-0036 records the design rationale for assigning behavior to these
categories; this section defines the terminology used below.

{{ rule(id="1.3:4", cat="informative") }}

**Undefined behavior** imposes no requirements on a conforming implementation: a program that exhibits undefined behavior is invalid, and the implementation may do anything. In Rue, undefined behavior is confined to `unchecked` code as specified by this specification; safe Rue programs do not exhibit undefined behavior.

{{ rule(id="1.3:5", cat="informative") }}

**Unspecified behavior** is behavior for which this specification permits a set of possible results and does not require a conforming implementation to choose or document any particular result.

{{ rule(id="1.3:6", cat="informative") }}

**Implementation-defined behavior** is behavior for which this specification permits a set of possible results and requires a conforming implementation to choose and **document** the result it provides.

{{ rule(id="1.3:7", cat="informative") }}

**Erroneous behavior** is behavior that is well-defined but constitutes a program error a conforming implementation is encouraged to diagnose.

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
if_expr     = "if" expression "{" block "}" [ else_clause ] ;
else_clause = "else" ( "{" block "}" | if_expr ) ;
```

## Organization

{{ rule(id="1.6:1") }}

This specification is organized as follows:

- **Chapter 2: Lexical Structure** - Tokens, comments, whitespace, keywords
- **Chapter 3: Types** - Integer types, booleans, unit, never, arrays, structs, enums, strings, move semantics, destructors
- **Chapter 4: Expressions** - Operators, control flow, function calls, compile-time expressions
- **Chapter 5: Statements** - Variable bindings, assignment
- **Chapter 6: Items** - Functions, structs, enums, constants
- **Chapter 7: Arrays** - Fixed-size array behavior
- **Chapter 8: Runtime Behavior** - Overflow, bounds checking, panics
- **Chapter 9: Unchecked Code** - Raw pointers and unchecked intrinsics
- **Chapter 10: Modules** - Module forms, import resolution, visibility, program composition
- **Appendix A: Grammar** - Complete EBNF grammar
- **Appendix B: Runtime Panics** - Summary of panic conditions
- **Appendix C: Implementation Limits** - Language limits, this implementation's capacity ceilings, and the requirement to diagnose rather than wrap when one is exceeded

## Version

{{ rule(id="1.7:1") }}

This specification corresponds to version 0.1.0 of the Rue language.
