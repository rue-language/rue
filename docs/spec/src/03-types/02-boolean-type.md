+++
title = "Boolean Type"
weight = 2
template = "spec/page.html"
+++

# Boolean Type

{{ rule(id="3.2:1", cat="normative") }}

The type `bool` represents boolean values.

{{ rule(id="3.2:2", cat="normative") }}

The only values of type `bool` are `true` and `false`.

{{ rule(id="3.2:3", cat="normative") }}

In memory, `bool` values are represented as a single byte: `false` is 0, `true` is 1.

{{ rule(id="3.2:4", cat="normative") }}

Boolean values support equality comparison (`==`, `!=`) but not ordering comparison (`<`, `>`, `<=`, `>=`).

{{ rule(id="3.2:5") }}

```rue
fn main() -> i32 {
    let a = true;
    let b = false;
    let c = a == b;  // false
    if c { 1 } else { 0 }
}
```
