# Integer Overflow

r[8.1:1#normative]
Integer overflow during arithmetic operations causes a runtime panic.

r[8.1:2#normative]
On overflow, the program terminates with exit code 101 and prints an error message.

r[8.1:3#normative]
The following operations can overflow:
- Addition (`+`)
- Subtraction (`-`)
- Multiplication (`*`)
- Negation (`-` unary)

r[8.1:4]
```rue
fn main() -> i32 {
    2147483647 + 1  // Runtime error: integer overflow
}
```

r[8.1:5]
```rue
fn main() -> i32 {
    -2147483648 - 1  // Runtime error: integer overflow
}
```

r[8.1:6]
Future versions of Rue may provide wrapping arithmetic operations that do not panic on overflow.
