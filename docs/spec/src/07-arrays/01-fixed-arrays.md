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

## Nested Arrays

r[7.1.16#normative]
Arrays may contain other arrays as elements, forming multi-dimensional arrays.

r[7.1.17#normative]
Nested arrays are indexed using chained bracket notation, evaluated left to right.

r[7.1.18]
```rue
fn main() -> i32 {
    let matrix: [[i32; 2]; 2] = [[1, 2], [3, 4]];
    matrix[1][1]  // 4
}
```

## Arrays in Structs

r[7.1.19#normative]
Struct fields may have array types.

r[7.1.20#normative]
Array fields are accessed by combining field access with array indexing.

r[7.1.21]
```rue
struct Container { values: [i32; 3] }

fn main() -> i32 {
    let c = Container { values: [10, 20, 30] };
    c.values[1]  // 20
}
```

## Arrays as Function Parameters

r[7.1.22#normative]
Functions may accept arrays as parameters.

r[7.1.23#normative]
Array parameters are passed by value; the entire array is copied to the function.

r[7.1.24]
```rue
fn sum(arr: [i32; 3]) -> i32 {
    arr[0] + arr[1] + arr[2]
}

fn main() -> i32 {
    let data: [i32; 3] = [10, 20, 12];
    sum(data)  // 42
}
```
