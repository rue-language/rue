# Boolean Type

r[3.2:1#normative]
The type `bool` represents boolean values.

r[3.2:2#normative]
The only values of type `bool` are `true` and `false`.

r[3.2:3#normative]
In memory, `bool` values are represented as a single byte: `false` is 0, `true` is 1.

r[3.2:4#normative]
Boolean values support equality comparison (`==`, `!=`) but not ordering comparison (`<`, `>`, `<=`, `>=`).

r[3.2:5]
```rue
fn main() -> i32 {
    let a = true;
    let b = false;
    let c = a == b;  // false
    if c { 1 } else { 0 }
}
```
