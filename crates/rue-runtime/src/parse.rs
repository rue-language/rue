//! Integer parsing intrinsics
//!
//! This module implements the `@parse_i32`, `@parse_i64`, `@parse_u32`, and
//! `@parse_u64` intrinsics for parsing strings into integers.
//!
//! See ADR-0022 for the design rationale and ADR-0038 / RUE-6 for the error
//! model: a parse that fails (empty string, invalid character, overflow, or a
//! negative value for an unsigned type) yields `None` rather than trapping, so
//! callers recover instead of aborting.

/// Result of an `@parse_*` intrinsic: a tagged-union `Option(int)` written via
/// sret. The compiler lays a payload-carrying enum out as slot 0 =
/// discriminant followed by the payload slots (RUE-221), so the in-memory
/// layout of `Option(iN)` is `[disc, value]`. This struct mirrors that layout
/// exactly (RUE-6).
#[repr(C)]
pub struct OptionIntResult {
    /// Discriminant slot: `some_disc` on success, `none_disc` on failure.
    pub disc: u64,
    /// Payload slot: the parsed integer's bit pattern (only meaningful when
    /// `disc == some_disc`). Narrower types occupy the low bits.
    pub value: u64,
}

/// Parse decimal ASCII into an `i64`, returning `None` on empty input, an
/// invalid character, or overflow. Shared core for the signed parsers.
fn parse_i64_checked(ptr: *const u8, len: u64) -> Option<i64> {
    if len == 0 {
        return None;
    }

    // SAFETY: the caller (an `@parse_*` intrinsic) passes the ptr/len of a
    // live borrowed String; we only read `len` bytes.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    let mut idx = 0;
    let is_negative = bytes[0] == b'-';

    if is_negative {
        idx = 1;
        if idx >= bytes.len() {
            return None;
        }
    }

    let mut result: i64 = 0;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if !byte.is_ascii_digit() {
            return None;
        }

        let digit = (byte - b'0') as i64;

        if is_negative {
            // For negative numbers, check against i64::MIN
            if result < i64::MIN / 10 {
                return None;
            }
            result *= 10;
            if result < i64::MIN + digit {
                return None;
            }
            result -= digit;
        } else {
            // For positive numbers, check against i64::MAX
            if result > i64::MAX / 10 {
                return None;
            }
            result *= 10;
            if result > i64::MAX - digit {
                return None;
            }
            result += digit;
        }

        idx += 1;
    }

    Some(result)
}

/// Parse decimal ASCII into a `u64`, returning `None` on empty input, a leading
/// minus, an invalid character, or overflow. Shared core for the unsigned
/// parsers.
fn parse_u64_checked(ptr: *const u8, len: u64) -> Option<u64> {
    if len == 0 {
        return None;
    }

    // SAFETY: see `parse_i64_checked`.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };

    // A leading minus is invalid for unsigned types.
    if bytes[0] == b'-' {
        return None;
    }

    let mut result: u64 = 0;
    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }

        let digit = (byte - b'0') as u64;

        if result > u64::MAX / 10 {
            return None;
        }
        result *= 10;
        if result > u64::MAX - digit {
            return None;
        }
        result += digit;
    }

    Some(result)
}

/// Parse decimal ASCII into an `i32` (range-checked), `None` on failure.
fn parse_i32_checked(ptr: *const u8, len: u64) -> Option<i32> {
    let v = parse_i64_checked(ptr, len)?;
    if v < i32::MIN as i64 || v > i32::MAX as i64 {
        return None;
    }
    Some(v as i32)
}

/// Parse decimal ASCII into a `u32` (range-checked), `None` on failure.
fn parse_u32_checked(ptr: *const u8, len: u64) -> Option<u32> {
    let v = parse_u64_checked(ptr, len)?;
    if v > u32::MAX as u64 {
        return None;
    }
    Some(v as u32)
}

/// Write an `Option(int)` result: `Some(value)` or `None`, per the caller's
/// discriminant assignment.
///
/// # Safety
///
/// `out` must point to caller-allocated space for an `OptionIntResult`.
#[inline]
unsafe fn write_parse_result(
    out: *mut OptionIntResult,
    parsed: Option<u64>,
    some_disc: u64,
    none_disc: u64,
) {
    // SAFETY: `out` points to caller-allocated space for the Option(int)
    // result (2 slots). On `None` we zero the payload; a `None` never has its
    // payload read, so the zero is inert.
    unsafe {
        match parsed {
            Some(v) => {
                (*out).disc = some_disc;
                (*out).value = v;
            }
            None => {
                (*out).disc = none_disc;
                (*out).value = 0;
            }
        }
    }
}

crate::define_for_all_platforms! {
    /// extern "C" fn __rue_parse_i32(out, ptr, len, some_disc, none_disc)
    pub extern "C" fn __rue_parse_i32(out: *mut OptionIntResult, ptr: *const u8, len: u64, some_disc: u64, none_disc: u64) {
        let parsed = parse_i32_checked(ptr, len).map(|v| v as u64);
        // SAFETY: `out` is the caller-allocated sret buffer for the result.
        unsafe { write_parse_result(out, parsed, some_disc, none_disc) };
    }
}

define_for_all_platforms! {
    /// extern "C" fn __rue_parse_i64(out, ptr, len, some_disc, none_disc)
    pub extern "C" fn __rue_parse_i64(out: *mut OptionIntResult, ptr: *const u8, len: u64, some_disc: u64, none_disc: u64) {
        let parsed = parse_i64_checked(ptr, len).map(|v| v as u64);
        // SAFETY: `out` is the caller-allocated sret buffer for the result.
        unsafe { write_parse_result(out, parsed, some_disc, none_disc) };
    }
}

define_for_all_platforms! {
    /// extern "C" fn __rue_parse_u32(out, ptr, len, some_disc, none_disc)
    pub extern "C" fn __rue_parse_u32(out: *mut OptionIntResult, ptr: *const u8, len: u64, some_disc: u64, none_disc: u64) {
        let parsed = parse_u32_checked(ptr, len).map(|v| v as u64);
        // SAFETY: `out` is the caller-allocated sret buffer for the result.
        unsafe { write_parse_result(out, parsed, some_disc, none_disc) };
    }
}

define_for_all_platforms! {
    /// extern "C" fn __rue_parse_u64(out, ptr, len, some_disc, none_disc)
    pub extern "C" fn __rue_parse_u64(out: *mut OptionIntResult, ptr: *const u8, len: u64, some_disc: u64, none_disc: u64) {
        let parsed = parse_u64_checked(ptr, len);
        // SAFETY: `out` is the caller-allocated sret buffer for the result.
        unsafe { write_parse_result(out, parsed, some_disc, none_disc) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The parsing core is exercised directly through the `_checked` helpers;
    // the extern wrappers just marshal the result into the sret Option layout.

    #[test]
    fn test_parse_i32_basic() {
        assert_eq!(parse_i32_checked(b"42".as_ptr(), 2), Some(42));
        assert_eq!(parse_i32_checked(b"0".as_ptr(), 1), Some(0));
        assert_eq!(
            parse_i32_checked(b"2147483647".as_ptr(), 10),
            Some(2147483647)
        );
    }

    #[test]
    fn test_parse_i32_negative() {
        assert_eq!(parse_i32_checked(b"-17".as_ptr(), 3), Some(-17));
        assert_eq!(
            parse_i32_checked(b"-2147483648".as_ptr(), 11),
            Some(-2147483648)
        );
    }

    #[test]
    fn test_parse_i64_basic() {
        assert_eq!(parse_i64_checked(b"42".as_ptr(), 2), Some(42));
        assert_eq!(
            parse_i64_checked(b"9223372036854775807".as_ptr(), 19),
            Some(9223372036854775807)
        );
    }

    #[test]
    fn test_parse_u32_basic() {
        assert_eq!(parse_u32_checked(b"42".as_ptr(), 2), Some(42));
        assert_eq!(
            parse_u32_checked(b"4294967295".as_ptr(), 10),
            Some(4294967295)
        );
    }

    #[test]
    fn test_parse_u64_basic() {
        assert_eq!(parse_u64_checked(b"42".as_ptr(), 2), Some(42));
        assert_eq!(
            parse_u64_checked(b"18446744073709551615".as_ptr(), 20),
            Some(18446744073709551615)
        );
    }

    #[test]
    fn test_parse_failure_returns_none() {
        // Empty, invalid character, and lone-minus all fail to `None`.
        assert_eq!(parse_i64_checked(b"".as_ptr(), 0), None);
        assert_eq!(parse_i64_checked(b"12abc".as_ptr(), 5), None);
        assert_eq!(parse_i64_checked(b"-".as_ptr(), 1), None);
        // Overflow past i64 range.
        assert_eq!(parse_i64_checked(b"9223372036854775808".as_ptr(), 19), None);
        // i32 range check.
        assert_eq!(parse_i32_checked(b"2147483648".as_ptr(), 10), None);
    }

    #[test]
    fn test_parse_unsigned_rejects_negative() {
        assert_eq!(parse_u32_checked(b"-17".as_ptr(), 3), None);
        assert_eq!(parse_u64_checked(b"-1".as_ptr(), 2), None);
        // u32 range check.
        assert_eq!(parse_u32_checked(b"4294967296".as_ptr(), 10), None);
    }
}
