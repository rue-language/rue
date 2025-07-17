# Design Decisions: Comments

## Overview
This document records the design decisions made for adding comment support to Rue.

## Comment Syntax

### Single-line Comments
- **Decision**: Use `//` for single-line comments (Rust-style)
- **Rationale**: Following Rust as our primary language influence, familiar to modern programmers
- **Alternative considered**: `#` (Python/Ruby style) - rejected for consistency with Rust

### Multi-line Comments
- **Decision**: Use `/* */` for multi-line comments with nesting support (Rust-style)
- **Rationale**: Rust's approach to nested comments is superior to C's, allows commenting out code blocks that contain comments
- **Alternative considered**: `(* *)` (ML-style) - rejected for consistency with Rust

## Implementation Approach

### Lexer Integration
- **Decision**: Handle comments entirely in the lexer, don't pass them to parser
- **Rationale**: Comments are not part of the AST, simpler implementation
- **Trade-off**: Cannot preserve comments for documentation generation later

### Nested Comments
- **Decision**: Support nested multi-line comments
- **Rationale**: More flexible, allows commenting out code that already contains comments
- **Implementation**: Track nesting depth when lexing `/*` and `*/` tokens

## Edge Cases

### Comment Placement
- Comments can appear anywhere whitespace is allowed
- Comments at end of file don't require trailing newline
- Empty comments are valid (`//` with nothing after, `/**/`)