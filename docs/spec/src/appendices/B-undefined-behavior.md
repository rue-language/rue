# Appendix B: Runtime Panics

This appendix summarizes all conditions that cause runtime panics in Rue.

r[B.1:1]
Rue detects certain error conditions at runtime and responds with a panic, terminating the program with a specific exit code.

## Integer Overflow

r[B.1:2]
Signed or unsigned integer arithmetic that overflows the representable range causes a runtime panic.

**Operations affected:**
- Addition (`+`)
- Subtraction (`-`)
- Multiplication (`*`)
- Unary negation (`-`)

**Runtime behavior:** Panic with exit code 101.

## Division by Zero

r[B.1:3]
Division or remainder with a divisor of zero causes a runtime panic.

**Operations affected:**
- Division (`/`)
- Remainder (`%`)

**Runtime behavior:** Panic with exit code 101.

## Array Bounds Violation

r[B.1:4]
Accessing an array element with an index outside the valid range `[0, length)` causes a runtime panic.

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

r[B.1:5]
All runtime panics produce exit code 101, matching Rust's convention for unwinding panics.
