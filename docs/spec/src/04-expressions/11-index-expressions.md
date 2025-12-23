# Index Expressions

r[4.11:1#normative]
An index expression accesses an element of an array.

r[4.11:2#normative]
```ebnf
index_expr = expression "[" expression "]" ;
```

r[4.11:3#normative]
The expression before the brackets must have an array type `[T; N]`.

r[4.11:4#normative]
The index expression must have an integer type.

r[4.11:5#normative]
The type of an index expression is the element type `T`.

r[4.11:6]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 42, 100];
    arr[1]  // 42
}
```

## Bounds Checking

r[4.11:7#normative]
For constant indices, bounds checking is performed at compile time.

r[4.11:8#normative]
For variable indices, bounds checking is performed at runtime.

r[4.11:9#normative]
An out-of-bounds access causes a runtime panic.

r[4.11:10]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    arr[5]  // Compile-time error: index out of bounds
}
```

r[4.11:11]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    let idx = 5;
    arr[idx]  // Runtime error: index out of bounds
}
```

## Index Assignment

r[4.11:12#normative]
For mutable arrays, elements can be assigned using index expressions.

r[4.11:13]
```rue
fn main() -> i32 {
    let mut arr: [i32; 2] = [0, 0];
    arr[0] = 20;
    arr[1] = 22;
    arr[0] + arr[1]  // 42
}
```
