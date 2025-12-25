+++
title = "Undefined Behavior and Runtime Panics"
weight = 2
template = "spec/page.html"
+++

# Appendix B: Undefined Behavior and Runtime Panics

{{ rule(id="B.1:1") }}

Rue currently has no undefined behavior. All operations in Rue have defined semantics: they either complete successfully, fail to compile, or cause a runtime panic.

{{ rule(id="B.1:2") }}

This is a deliberate design choice. Where other systems languages define certain conditions as undefined behavior (allowing implementations to assume they never occur), Rue instead detects these conditions and responds with a defined runtime panic.

{{ rule(id="B.1:3") }}

Future versions of Rue may introduce undefined behavior for specific low-level operations (such as unchecked arithmetic or raw pointer manipulation), but these will be explicitly marked as such and will require opt-in syntax.

## Runtime Panics

{{ rule(id="B.2:1") }}

Rue detects certain error conditions at runtime and responds with a panic, terminating the program with a specific exit code.

## Integer Overflow

{{ rule(id="B.2:2", cat="dynamic-semantics") }}

Signed or unsigned integer arithmetic that overflows the representable range **MUST** cause a runtime panic.

**Operations affected:**
- Addition (`+`)
- Subtraction (`-`)
- Multiplication (`*`)
- Unary negation (`-`)

**Runtime behavior:** Panic with exit code 101.

## Division by Zero

{{ rule(id="B.2:3", cat="dynamic-semantics") }}

Division or remainder with a divisor of zero **MUST** cause a runtime panic.

**Operations affected:**
- Division (`/`)
- Remainder (`%`)

**Runtime behavior:** Panic with exit code 101.

## Array Bounds Violation

{{ rule(id="B.2:4", cat="dynamic-semantics") }}

Accessing an array element with an index outside the valid range `[0, length)` **MUST** cause a runtime panic.

**Operations affected:**
- Array indexing (`arr[i]`)
- Array element assignment (`arr[i] = v`)

**Runtime behavior:** Panic with exit code 101.

## Exit Codes

| Condition | Exit Code |
|-----------|-----------|
| Integer overflow | 101 |
| Division by zero | 101 |
| Array out of bounds | 101 |

{{ rule(id="B.2:5") }}

All runtime panics produce exit code 101, matching Rust's convention for unwinding panics.
