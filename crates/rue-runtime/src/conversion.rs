//! String conversion functions
//!
//! Provides integer-to-string and string-to-integer conversion functions
//! following Rue's runtime ABI.

use crate::abi::*;

/// Convert an integer to ASCII representation
///
/// Writes the ASCII representation of `value` to `buf` and returns the number
/// of bytes written. The buffer must have at least 20 bytes available (enough
/// for i64::MIN with sign).
///
/// # Safety
/// - `buf` must point to a valid buffer of at least 20 bytes
/// - Returns the number of bytes written to the buffer
///
/// # Algorithm
/// 1. Handle negative numbers by writing '-' and negating
/// 2. Special case for i64::MIN (can't negate it)
/// 3. Extract digits by repeatedly dividing by 10 (reverse order)
/// 4. Reverse the digits in place
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_itoa(value: i64, buf: *mut u8) -> usize {
    if buf.is_null() {
        return 0;
    }

    let buf_slice = unsafe { core::slice::from_raw_parts_mut(buf, ITOA_BUFFER_SIZE) };
    let mut pos = 0;

    // Handle negative numbers
    let mut num = value;
    if value < 0 {
        buf_slice[pos] = b'-';
        pos += 1;

        // Special case for i64::MIN (-9223372036854775808)
        // Can't negate it (would overflow), so handle it directly
        if value == i64::MIN {
            let min_str = b"9223372036854775808";
            buf_slice[pos..pos + min_str.len()].copy_from_slice(min_str);
            return pos + min_str.len();
        }

        num = -value;
    }

    // Special case for 0
    if num == 0 {
        buf_slice[pos] = b'0';
        return pos + 1;
    }

    // Extract digits in reverse order
    let digit_start = pos;
    while num > 0 {
        let digit = (num % 10) as u8;
        buf_slice[pos] = b'0' + digit;
        pos += 1;
        num /= 10;
    }

    // Reverse the digits in place
    let digit_end = pos - 1;
    let mut left = digit_start;
    let mut right = digit_end;
    while left < right {
        buf_slice.swap(left, right);
        left += 1;
        right -= 1;
    }

    pos
}

/// Parse an integer from an ASCII string
///
/// Parses the first integer found in the buffer, skipping leading whitespace.
/// Handles negative numbers starting with '-'. Returns 0 if no valid integer
/// is found or on overflow.
///
/// # Safety
/// - `buf` must point to a valid buffer of `len` bytes
/// - Returns the parsed integer value
///
/// # Algorithm
/// 1. Skip leading whitespace
/// 2. Check for negative sign
/// 3. Parse digits, accumulating result
/// 4. Apply sign and return
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_atoi(buf: *const u8, len: usize) -> i64 {
    if buf.is_null() || len == 0 {
        return 0;
    }

    let buf_slice = unsafe { core::slice::from_raw_parts(buf, len) };
    let mut pos = 0;

    // Skip leading whitespace
    while pos < len
        && (buf_slice[pos] == b' '
            || buf_slice[pos] == b'\t'
            || buf_slice[pos] == b'\n'
            || buf_slice[pos] == b'\r')
    {
        pos += 1;
    }

    if pos >= len {
        return 0;
    }

    // Check for negative sign
    let negative = buf_slice[pos] == b'-';
    if negative || buf_slice[pos] == b'+' {
        pos += 1;
    }

    // Parse digits
    let mut result: i64 = 0;
    let mut found_digit = false;

    while pos < len {
        let ch = buf_slice[pos];
        if ch >= b'0' && ch <= b'9' {
            let digit = (ch - b'0') as i64;

            // Check for overflow (simplified - just detect if we'd overflow)
            if result > (i64::MAX - digit) / 10 {
                return 0; // Overflow
            }

            result = result * 10 + digit;
            found_digit = true;
        } else {
            // Stop at first non-digit
            break;
        }
        pos += 1;
    }

    if !found_digit {
        return 0;
    }

    if negative { -result } else { result }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_itoa_positive() {
        let mut buf = [0u8; 20];
        let len = unsafe { __rue_itoa(42, buf.as_mut_ptr()) };
        assert_eq!(len, 2);
        assert_eq!(&buf[..len], b"42");
    }

    #[test]
    fn test_itoa_negative() {
        let mut buf = [0u8; 20];
        let len = unsafe { __rue_itoa(-42, buf.as_mut_ptr()) };
        assert_eq!(len, 3);
        assert_eq!(&buf[..len], b"-42");
    }

    #[test]
    fn test_itoa_zero() {
        let mut buf = [0u8; 20];
        let len = unsafe { __rue_itoa(0, buf.as_mut_ptr()) };
        assert_eq!(len, 1);
        assert_eq!(&buf[..len], b"0");
    }

    #[test]
    fn test_itoa_min() {
        let mut buf = [0u8; 20];
        let len = unsafe { __rue_itoa(i64::MIN, buf.as_mut_ptr()) };
        assert_eq!(len, 20); // "-9223372036854775808"
        assert_eq!(&buf[..len], b"-9223372036854775808");
    }

    #[test]
    fn test_itoa_max() {
        let mut buf = [0u8; 20];
        let len = unsafe { __rue_itoa(i64::MAX, buf.as_mut_ptr()) };
        assert_eq!(len, 19); // "9223372036854775807"
        assert_eq!(&buf[..len], b"9223372036854775807");
    }

    #[test]
    fn test_atoi_positive() {
        let result = unsafe { __rue_atoi(b"42".as_ptr(), 2) };
        assert_eq!(result, 42);
    }

    #[test]
    fn test_atoi_negative() {
        let result = unsafe { __rue_atoi(b"-42".as_ptr(), 3) };
        assert_eq!(result, -42);
    }

    #[test]
    fn test_atoi_zero() {
        let result = unsafe { __rue_atoi(b"0".as_ptr(), 1) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_atoi_with_whitespace() {
        let result = unsafe { __rue_atoi(b"  42".as_ptr(), 4) };
        assert_eq!(result, 42);
    }

    #[test]
    fn test_atoi_with_trailing_junk() {
        let result = unsafe { __rue_atoi(b"42abc".as_ptr(), 5) };
        assert_eq!(result, 42);
    }

    #[test]
    fn test_atoi_no_digits() {
        let result = unsafe { __rue_atoi(b"abc".as_ptr(), 3) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_atoi_empty() {
        let result = unsafe { __rue_atoi(b"".as_ptr(), 0) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_atoi_plus_sign() {
        let result = unsafe { __rue_atoi(b"+42".as_ptr(), 3) };
        assert_eq!(result, 42);
    }
}
