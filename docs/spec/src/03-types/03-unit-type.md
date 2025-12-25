+++
title = "Unit Type"
weight = 3
template = "spec/page.html"
+++

# Unit Type

{{ rule(id="3.3:1", cat="normative") }}

The unit type, written `()`, has exactly one value, also written `()`.

{{ rule(id="3.3:2", cat="normative") }}

Functions without an explicit return type annotation implicitly return `()`.

{{ rule(id="3.3:3", cat="normative") }}

Expressions that produce side effects but no meaningful value have type `()`.

{{ rule(id="3.3:4", cat="normative") }}

The unit type is a zero-sized type. See [Zero-Sized Types](../#zero-sized-types) for the general definition.

{{ rule(id="3.3:5") }}

```rue
fn do_nothing() {
    // Implicitly returns ()
}

fn explicit_unit() -> () {
    // Explicitly returns ()
}

fn main() -> i32 {
    do_nothing();
    explicit_unit();
    0
}
```
