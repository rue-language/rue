# Array Types

r[3.5:1#normative]
An array type, written `[T; N]`, represents a fixed-size sequence of `N` elements of type `T`.

r[3.5:2#normative]
The length `N` must be a non-negative integer literal known at compile time.

r[3.5:3#normative]
All elements of an array must have the same type `T`.

r[3.5:4#normative]
Arrays are stored contiguously in memory. The size of `[T; N]` is `N * size_of(T)`.

r[3.5:5]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 12];
    arr[0] + arr[1] + arr[2]  // 42
}
```

r[3.5:6#normative]
Array elements are accessed using index syntax `arr[index]`.

r[3.5:7#normative]
For constant indices, bounds checking is performed at compile time.

r[3.5:8#normative]
For variable indices, bounds checking is performed at runtime.

r[3.5:9#normative]
Accessing an array with an out-of-bounds index causes a runtime panic.

r[3.5:10]
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    let idx = 5;
    arr[idx]  // Runtime error: index out of bounds
}
```
