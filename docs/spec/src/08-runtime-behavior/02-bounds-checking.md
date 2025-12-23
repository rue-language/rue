# Bounds Checking

r[8.2:1#normative]
Array access with an out-of-bounds index causes a runtime panic.

r[8.2:2#normative]
On out-of-bounds access, the program terminates with exit code 101 and prints an error message.

r[8.2:3#normative]
For constant indices, bounds checking is performed at compile time.

r[8.2:4#normative]
A constant index is an expression that can be fully evaluated at compile time. This includes integer literals, arithmetic operations on constants, comparison operations on constants, and parenthesized constant expressions.

r[8.2:5#normative]
For variable indices, bounds checking is performed at runtime before the access.

r[8.2:6]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    let idx = 10;
    arr[idx]  // Runtime error: index out of bounds
}
```

r[8.2:7]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    arr[5]  // Compile-time error: index out of bounds
}
```

r[8.2:8]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    arr[1 + 5]  // Compile-time error: index out of bounds
}
```

r[8.2:9#normative]
Negative indices, when used with signed integer types, result in out-of-bounds errors because they represent large unsigned values when converted.
