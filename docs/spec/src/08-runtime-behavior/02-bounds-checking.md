# Bounds Checking

r[8.2.1#normative]
Array access with an out-of-bounds index causes a runtime panic.

r[8.2.2#normative]
On out-of-bounds access, the program terminates with exit code 101 and prints an error message.

r[8.2.3#normative]
For constant indices, bounds checking is performed at compile time.

r[8.2.4#normative]
For variable indices, bounds checking is performed at runtime before the access.

r[8.2.5]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    let idx = 10;
    arr[idx]  // Runtime error: index out of bounds
}
```

r[8.2.6]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    arr[5]  // Compile-time error: index out of bounds
}
```

r[8.2.7#normative]
Negative indices, when used with signed integer types, result in out-of-bounds errors because they represent large unsigned values when converted.
