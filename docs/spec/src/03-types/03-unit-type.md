# Unit Type

r[3.3:1#normative]
The unit type, written `()`, has exactly one value, also written `()`.

r[3.3:2#normative]
Functions without an explicit return type annotation implicitly return `()`.

r[3.3:3#normative]
Expressions that produce side effects but no meaningful value have type `()`.

r[3.3:4#normative]
The unit value `()` occupies zero bytes in memory.

r[3.3:5]
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
