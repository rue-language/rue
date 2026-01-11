//! Integer parsing intrinsics
//!
//! This module implements the `@parse_i32`, `@parse_i64`, `@parse_u32`, and
//! `@parse_u64` intrinsics for parsing strings into integers.
//!
//! See ADR-0022 for the design rationale.

use crate::platform;

/// Parse a string as an i32.
///
/// # Behavior
///
/// - Accepts optional leading `-`
/// - Parses ASCII decimal digits
/// - Panics on empty string, invalid characters, overflow, or other errors
///
/// # Calling Convention
///
/// - `ptr`: Pointer to the string bytes
/// - `len`: Length of the string in bytes
/// - Returns: Parsed i32 value
crate::define_for_all_platforms! {
    /// extern "C" fn __rue_parse_i32(ptr: *const u8, len: u64) -> i32
    pub extern "C" fn __rue_parse_i32(ptr: *const u8, len: u64) -> i32 {
        // Delegate to i64 parser, then check range
        let result = __rue_parse_i64(ptr, len);
        if result < i32::MIN as i64 || result > i32::MAX as i64 {
            parse_error_overflow();
        }
        result as i32
    }
}

define_for_all_platforms! {
    /// extern "C" fn __rue_parse_i64(ptr: *const u8, len: u64) -> i64
    pub extern "C" fn __rue_parse_i64(ptr: *const u8, len: u64) -> i64 {
        if len == 0 {
            parse_error_empty();
        }

        unsafe {
            let bytes = core::slice::from_raw_parts(ptr, len as usize);
            let mut idx = 0;
            let is_negative = bytes[0] == b'-';

            if is_negative {
                idx = 1;
                if idx >= bytes.len() {
                    parse_error_char();
                }
            }

            let mut result: i64 = 0;
            while idx < bytes.len() {
                let byte = bytes[idx];
                if byte < b'0' || byte > b'9' {
                    parse_error_char();
                }

                let digit = (byte - b'0') as i64;

                // Check for overflow before multiplying
                if is_negative {
                    // For negative numbers, check against i64::MIN
                    if result < i64::MIN / 10 {
                        parse_error_overflow();
                    }
                    result = result * 10;
                    if result < i64::MIN + digit {
                        parse_error_overflow();
                    }
                    result = result - digit;
                } else {
                    // For positive numbers, check against i64::MAX
                    if result > i64::MAX / 10 {
                        parse_error_overflow();
                    }
                    result = result * 10;
                    if result > i64::MAX - digit {
                        parse_error_overflow();
                    }
                    result = result + digit;
                }

                idx += 1;
            }

            result
        }
    }
}

define_for_all_platforms! {
    /// extern "C" fn __rue_parse_u32(ptr: *const u8, len: u64) -> u32
    pub extern "C" fn __rue_parse_u32(ptr: *const u8, len: u64) -> u32 {
        // Delegate to u64 parser, then check range
        let result = __rue_parse_u64(ptr, len);
        if result > u32::MAX as u64 {
            parse_error_overflow();
        }
        result as u32
    }
}

define_for_all_platforms! {
    /// extern "C" fn __rue_parse_u64(ptr: *const u8, len: u64) -> u64
    pub extern "C" fn __rue_parse_u64(ptr: *const u8, len: u64) -> u64 {
        if len == 0 {
            parse_error_empty();
        }

        unsafe {
            let bytes = core::slice::from_raw_parts(ptr, len as usize);

            // Check for negative sign (invalid for unsigned)
            if bytes[0] == b'-' {
                parse_error_negative();
            }

            let mut result: u64 = 0;
            for &byte in bytes {
                if byte < b'0' || byte > b'9' {
                    parse_error_char();
                }

                let digit = (byte - b'0') as u64;

                // Check for overflow before multiplying
                if result > u64::MAX / 10 {
                    parse_error_overflow();
                }
                result = result * 10;
                if result > u64::MAX - digit {
                    parse_error_overflow();
                }
                result = result + digit;
            }

            result
        }
    }
}

/// Print "parse error: empty string\n" and exit.
fn parse_error_empty() -> ! {
    // Build error message byte-by-byte to avoid macOS linker bug with byte strings
    let mut msg = [0u8; 28];
    msg[0] = b'p';
    msg[1] = b'a';
    msg[2] = b'r';
    msg[3] = b's';
    msg[4] = b'e';
    msg[5] = b' ';
    msg[6] = b'e';
    msg[7] = b'r';
    msg[8] = b'r';
    msg[9] = b'o';
    msg[10] = b'r';
    msg[11] = b':';
    msg[12] = b' ';
    msg[13] = b'e';
    msg[14] = b'm';
    msg[15] = b'p';
    msg[16] = b't';
    msg[17] = b'y';
    msg[18] = b' ';
    msg[19] = b's';
    msg[20] = b't';
    msg[21] = b'r';
    msg[22] = b'i';
    msg[23] = b'n';
    msg[24] = b'g';
    msg[25] = b'\n';
    platform::write_stderr(&msg[0..26]);
    platform::exit(101);
}

/// Print "parse error: invalid character\n" and exit.
fn parse_error_char() -> ! {
    // Build error message byte-by-byte to avoid macOS linker bug with byte strings
    let mut msg = [0u8; 32];
    msg[0] = b'p';
    msg[1] = b'a';
    msg[2] = b'r';
    msg[3] = b's';
    msg[4] = b'e';
    msg[5] = b' ';
    msg[6] = b'e';
    msg[7] = b'r';
    msg[8] = b'r';
    msg[9] = b'o';
    msg[10] = b'r';
    msg[11] = b':';
    msg[12] = b' ';
    msg[13] = b'i';
    msg[14] = b'n';
    msg[15] = b'v';
    msg[16] = b'a';
    msg[17] = b'l';
    msg[18] = b'i';
    msg[19] = b'd';
    msg[20] = b' ';
    msg[21] = b'c';
    msg[22] = b'h';
    msg[23] = b'a';
    msg[24] = b'r';
    msg[25] = b'a';
    msg[26] = b'c';
    msg[27] = b't';
    msg[28] = b'e';
    msg[29] = b'r';
    msg[30] = b'\n';
    platform::write_stderr(&msg[0..31]);
    platform::exit(101);
}

/// Print "parse error: integer overflow\n" and exit.
fn parse_error_overflow() -> ! {
    // Build error message byte-by-byte to avoid macOS linker bug with byte strings
    let mut msg = [0u8; 32];
    msg[0] = b'p';
    msg[1] = b'a';
    msg[2] = b'r';
    msg[3] = b's';
    msg[4] = b'e';
    msg[5] = b' ';
    msg[6] = b'e';
    msg[7] = b'r';
    msg[8] = b'r';
    msg[9] = b'o';
    msg[10] = b'r';
    msg[11] = b':';
    msg[12] = b' ';
    msg[13] = b'i';
    msg[14] = b'n';
    msg[15] = b't';
    msg[16] = b'e';
    msg[17] = b'g';
    msg[18] = b'e';
    msg[19] = b'r';
    msg[20] = b' ';
    msg[21] = b'o';
    msg[22] = b'v';
    msg[23] = b'e';
    msg[24] = b'r';
    msg[25] = b'f';
    msg[26] = b'l';
    msg[27] = b'o';
    msg[28] = b'w';
    msg[29] = b'\n';
    platform::write_stderr(&msg[0..30]);
    platform::exit(101);
}

/// Print "parse error: negative value for unsigned type\n" and exit.
fn parse_error_negative() -> ! {
    // Build error message byte-by-byte to avoid macOS linker bug with byte strings
    let mut msg = [0u8; 48];
    msg[0] = b'p';
    msg[1] = b'a';
    msg[2] = b'r';
    msg[3] = b's';
    msg[4] = b'e';
    msg[5] = b' ';
    msg[6] = b'e';
    msg[7] = b'r';
    msg[8] = b'r';
    msg[9] = b'o';
    msg[10] = b'r';
    msg[11] = b':';
    msg[12] = b' ';
    msg[13] = b'n';
    msg[14] = b'e';
    msg[15] = b'g';
    msg[16] = b'a';
    msg[17] = b't';
    msg[18] = b'i';
    msg[19] = b'v';
    msg[20] = b'e';
    msg[21] = b' ';
    msg[22] = b'v';
    msg[23] = b'a';
    msg[24] = b'l';
    msg[25] = b'u';
    msg[26] = b'e';
    msg[27] = b' ';
    msg[28] = b'f';
    msg[29] = b'o';
    msg[30] = b'r';
    msg[31] = b' ';
    msg[32] = b'u';
    msg[33] = b'n';
    msg[34] = b's';
    msg[35] = b'i';
    msg[36] = b'g';
    msg[37] = b'n';
    msg[38] = b'e';
    msg[39] = b'd';
    msg[40] = b' ';
    msg[41] = b't';
    msg[42] = b'y';
    msg[43] = b'p';
    msg[44] = b'e';
    msg[45] = b'\n';
    platform::write_stderr(&msg[0..46]);
    platform::exit(101);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_i32_basic() {
        assert_eq!(__rue_parse_i32(b"42".as_ptr(), 2), 42);
        assert_eq!(__rue_parse_i32(b"0".as_ptr(), 1), 0);
        assert_eq!(__rue_parse_i32(b"2147483647".as_ptr(), 10), 2147483647);
    }

    #[test]
    fn test_parse_i32_negative() {
        assert_eq!(__rue_parse_i32(b"-17".as_ptr(), 3), -17);
        assert_eq!(__rue_parse_i32(b"-2147483648".as_ptr(), 11), -2147483648);
    }

    #[test]
    fn test_parse_i64_basic() {
        assert_eq!(__rue_parse_i64(b"42".as_ptr(), 2), 42);
        assert_eq!(
            __rue_parse_i64(b"9223372036854775807".as_ptr(), 19),
            9223372036854775807
        );
    }

    #[test]
    fn test_parse_u32_basic() {
        assert_eq!(__rue_parse_u32(b"42".as_ptr(), 2), 42);
        assert_eq!(__rue_parse_u32(b"4294967295".as_ptr(), 10), 4294967295);
    }

    #[test]
    fn test_parse_u64_basic() {
        assert_eq!(__rue_parse_u64(b"42".as_ptr(), 2), 42);
        assert_eq!(
            __rue_parse_u64(b"18446744073709551615".as_ptr(), 20),
            18446744073709551615
        );
    }
}
