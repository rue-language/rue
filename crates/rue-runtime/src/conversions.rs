//! Type conversion functions for the Rue runtime
//!
//! This module provides functions for converting between different types,
//! particularly for integer type conversions and string/numeric conversions.

/// Convert i64 to i32 (truncate)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_to_i32(value: i64) -> i32 {
    value as i32
}

/// Convert i32 to i64 (sign extend)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_to_i64(value: i32) -> i64 {
    value as i64
}

/// Convert integer to string representation
///
/// This function converts an i64 to its decimal string representation
/// and stores it in the provided buffer.
///
/// Returns the number of characters written
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_itoa(value: i64, buffer: *mut u8) -> usize {
    if buffer.is_null() {
        return 0;
    }

    let mut num = value;
    let mut is_negative = false;
    let mut digits = 0;

    // Handle negative numbers
    if num < 0 {
        is_negative = true;
        num = -num;
    }

    // Handle zero case
    if num == 0 {
        unsafe {
            *buffer = b'0';
        }
        return 1;
    }

    // Convert digits (in reverse order)
    let mut temp_buffer = [0u8; 21]; // Enough for i64::MIN
    let mut temp_pos = 0;

    while num > 0 {
        temp_buffer[temp_pos] = (b'0' + (num % 10) as u8);
        num /= 10;
        temp_pos += 1;
    }

    // Add negative sign if needed
    if is_negative {
        unsafe {
            *buffer = b'-';
        }
        digits = 1;
    }

    // Copy digits in correct order
    for i in 0..temp_pos {
        unsafe {
            *buffer.add(digits + i) = temp_buffer[temp_pos - 1 - i];
        }
    }

    digits + temp_pos
}

/// Convert string to integer
///
/// This function parses a decimal string representation to an i64
///
/// Returns the parsed integer, or 0 if parsing fails
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_atoi(str: *const u8, len: usize) -> i64 {
    if str.is_null() || len == 0 {
        return 0;
    }

    let mut result: i64 = 0;
    let mut is_negative = false;
    let mut start_pos = 0;

    // Check for negative sign
    unsafe {
        if *str == b'-' {
            is_negative = true;
            start_pos = 1;
        }
    }

    // Parse digits
    for i in start_pos..len {
        unsafe {
            let byte = *str.add(i);
            if byte >= b'0' && byte <= b'9' {
                let digit = (byte - b'0') as i64;
                result = result * 10 + digit;
            } else {
                break; // Stop at first non-digit
            }
        }
    }

    if is_negative { -result } else { result }
}
