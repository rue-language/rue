# Fixed-Size Arrays

## Array Literals

r[7.1.1#normative]
```ebnf
array_literal = "[" [ expression { "," expression } ] "]" ;
```

r[7.1.2#normative]
An array literal creates an array with the given elements.

r[7.1.3#normative]
All elements must have the same type.

r[7.1.4#normative]
The number of elements must match the declared array size.

r[7.1.5]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 12];
    arr[0] + arr[1] + arr[2]  // 42
}
```

## Array Indexing

r[7.1.6#normative]
Array elements are accessed using bracket notation `arr[index]`.

r[7.1.7#normative]
The index must be an integer type.

r[7.1.8]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [100, 42, 200];
    arr[1]  // 42
}
```

## Bounds Checking

r[7.1.9#normative]
For constant indices, bounds are checked at compile time.

r[7.1.10#normative]
For variable indices, bounds are checked at runtime.

r[7.1.11#normative]
Out-of-bounds access causes a runtime panic.

## Mutable Arrays

r[7.1.12#normative]
Mutable arrays allow element assignment.

r[7.1.13]
```rue
fn main() -> i32 {
    let mut arr: [i32; 2] = [0, 0];
    arr[0] = 20;
    arr[1] = 22;
    arr[0] + arr[1]  // 42
}
```

## Array Type Syntax

r[7.1.14#normative]
```ebnf
array_type = "[" type ";" INTEGER "]" ;
```

r[7.1.15#normative]
The size must be a non-negative integer literal.
