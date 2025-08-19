//! Compiler intrinsics stubs for the Rue runtime
//!
//! This module provides stub implementations for floating point and other
//! compiler intrinsics that may be referenced by the Rust core library.
//! Since Rue only supports integers, these panic if called.

/// Panic handler for unsupported floating point operations
fn unsupported_float_op() -> ! {
    panic!("Floating point operations are not supported in Rue runtime");
}

/// Stub for floating point addition
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __adddf3(_a: f64, _b: f64) -> f64 {
    unsupported_float_op();
}

/// Stub for floating point subtraction
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __subdf3(_a: f64, _b: f64) -> f64 {
    unsupported_float_op();
}

/// Stub for floating point multiplication
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __muldf3(_a: f64, _b: f64) -> f64 {
    unsupported_float_op();
}

/// Stub for floating point division
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __divdf3(_a: f64, _b: f64) -> f64 {
    unsupported_float_op();
}

/// Stub for single precision floating point addition
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __addsf3(_a: f32, _b: f32) -> f32 {
    unsupported_float_op();
}

/// Stub for single precision floating point subtraction
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __subsf3(_a: f32, _b: f32) -> f32 {
    unsupported_float_op();
}

/// Stub for single precision floating point multiplication
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __mulsf3(_a: f32, _b: f32) -> f32 {
    unsupported_float_op();
}

/// Stub for single precision floating point division
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __divsf3(_a: f32, _b: f32) -> f32 {
    unsupported_float_op();
}

/// Stub for float to double conversion
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __extendsfdf2(_a: f32) -> f64 {
    unsupported_float_op();
}

/// Stub for double to float conversion
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __truncdfsf2(_a: f64) -> f32 {
    unsupported_float_op();
}

/// Stub for integer to float conversion
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __floatsidf(_a: i32) -> f64 {
    unsupported_float_op();
}

/// Stub for integer to double conversion
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __floatdidf(_a: i64) -> f64 {
    unsupported_float_op();
}

/// Stub for float to integer conversion
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __fixdfsi(_a: f64) -> i32 {
    unsupported_float_op();
}

/// Stub for double to integer conversion
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __fixdfdi(_a: f64) -> i64 {
    unsupported_float_op();
}
