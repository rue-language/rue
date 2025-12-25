+++
title = "Division by Zero"
weight = 3
template = "spec/page.html"
+++

# Division by Zero

{{ rule(id="8.3:1", cat="dynamic-semantics") }}

Division or remainder by zero **MUST** cause a runtime panic.

{{ rule(id="8.3:2", cat="dynamic-semantics") }}

On division by zero, the program **MUST** terminate with exit code 101 and print an error message.

{{ rule(id="8.3:3", cat="normative") }}

Both the division operator (`/`) and remainder operator (`%`) **MAY** cause division-by-zero errors.

{{ rule(id="8.3:4") }}

```rue
fn main() -> i32 {
    10 / 0  // Runtime error: division by zero
}
```

{{ rule(id="8.3:5") }}

```rue
fn main() -> i32 {
    10 % 0  // Runtime error: division by zero
}
```

{{ rule(id="8.3:6") }}

```rue
fn main() -> i32 {
    let divisor = 5 - 5;
    10 / divisor  // Runtime error: division by zero
}
```
