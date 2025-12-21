# Division by Zero

r[8.3.1#normative]
Division or remainder by zero causes a runtime panic.

r[8.3.2#normative]
On division by zero, the program terminates with exit code 101 and prints an error message.

r[8.3.3#normative]
Both the division operator (`/`) and remainder operator (`%`) can cause division-by-zero errors.

r[8.3.4]
```rue
fn main() -> i32 {
    10 / 0  // Runtime error: division by zero
}
```

r[8.3.5]
```rue
fn main() -> i32 {
    10 % 0  // Runtime error: division by zero
}
```

r[8.3.6]
```rue
fn main() -> i32 {
    let divisor = 5 - 5;
    10 / divisor  // Runtime error: division by zero
}
```
