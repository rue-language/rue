//! The language-level semantics of Rue's fixed-width integer types.
//!
//! This module deliberately knows nothing about [`Type`] or any IR.  It is the
//! one place where width, signedness, canonical representation, and the
//! checked/wrapping arithmetic rules are defined.  AIR, CFG folding, and
//! machine-code planning are adapters over this value-independent kernel.

use std::cmp::Ordering;

/// The raw mathematical result and the result accepted by a fixed-width
/// integer operation.  `raw` remains available when it is outside the target
/// type, so diagnostics can explain the value that overflowed; `checked` is
/// the authoritative typed result consumed by evaluators and adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckedIntegerResult {
    raw: Option<i128>,
    checked: Option<i128>,
}

impl CheckedIntegerResult {
    pub const fn raw(self) -> Option<i128> {
        self.raw
    }

    pub const fn checked(self) -> Option<i128> {
        self.checked
    }

    pub const fn from_raw(raw: Option<i128>) -> Self {
        Self { raw, checked: raw }
    }
}

/// A compact description of one of Rue's eight integer types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntegerType {
    bits: u8,
    signed: bool,
}

impl IntegerType {
    /// Construct an integer description.  Only the language's supported
    /// widths are accepted.
    pub const fn new(bits: u8, signed: bool) -> Option<Self> {
        match bits {
            8 | 16 | 32 | 64 => Some(Self { bits, signed }),
            _ => None,
        }
    }

    pub const fn bits(self) -> u32 {
        self.bits as u32
    }

    pub const fn is_signed(self) -> bool {
        self.signed
    }

    pub const fn is_unsigned(self) -> bool {
        !self.signed
    }

    pub const fn min_i128(self) -> i128 {
        if self.signed {
            -(1_i128 << (self.bits - 1))
        } else {
            0
        }
    }

    pub const fn max_i128(self) -> i128 {
        if self.signed {
            (1_i128 << (self.bits - 1)) - 1
        } else {
            (1_i128 << self.bits) - 1
        }
    }

    pub const fn mask_u128(self) -> u128 {
        (1_u128 << self.bits) - 1
    }

    /// Return whether a mathematical integer is representable in this type.
    pub fn fits_i128(self, value: i128) -> bool {
        (self.min_i128()..=self.max_i128()).contains(&value)
    }

    /// Canonicalize a mathematical integer to the signed/unsigned value
    /// represented by this type.
    pub fn canonicalize_i128(self, value: i128) -> i128 {
        let masked = (value as u128 & self.mask_u128()) as i128;
        if self.signed && masked >= (1_i128 << (self.bits - 1)) {
            masked - (1_i128 << self.bits)
        } else {
            masked
        }
    }

    /// Keep only the low bits of a register image.
    pub fn mask_u64(self, value: u64) -> u64 {
        value & self.mask_u128() as u64
    }

    /// Convert a register image to Rue's canonical 64-bit representation.
    pub fn canonicalize_u64(self, value: u64) -> u64 {
        self.canonicalize_i128(value as i128) as u64
    }

    pub fn sign_extend_u64(self, value: u64) -> u64 {
        self.canonicalize_u64(value)
    }

    /// Rue masks shift amounts by the operand width minus one.
    pub const fn shift_count_mask(self) -> u64 {
        self.bits as u64 - 1
    }

    pub fn shift_count_i128(self, value: i128) -> u32 {
        (value as u128 & u128::from(self.shift_count_mask())) as u32
    }

    pub fn shift_count_u64(self, value: u64) -> u32 {
        (value & self.shift_count_mask()) as u32
    }

    pub fn checked_add_i128(self, lhs: i128, rhs: i128) -> Option<i128> {
        self.checked_add_report_i128(lhs, rhs).checked()
    }

    pub fn checked_add_report_i128(self, lhs: i128, rhs: i128) -> CheckedIntegerResult {
        self.checked_binary_report_i128(lhs, rhs, i128::checked_add)
    }

    pub fn checked_sub_i128(self, lhs: i128, rhs: i128) -> Option<i128> {
        self.checked_sub_report_i128(lhs, rhs).checked()
    }

    pub fn checked_sub_report_i128(self, lhs: i128, rhs: i128) -> CheckedIntegerResult {
        self.checked_binary_report_i128(lhs, rhs, i128::checked_sub)
    }

    pub fn checked_mul_i128(self, lhs: i128, rhs: i128) -> Option<i128> {
        self.checked_mul_report_i128(lhs, rhs).checked()
    }

    pub fn checked_mul_report_i128(self, lhs: i128, rhs: i128) -> CheckedIntegerResult {
        self.checked_binary_report_i128(lhs, rhs, i128::checked_mul)
    }

    pub fn checked_div_i128(self, lhs: i128, rhs: i128) -> Option<i128> {
        self.checked_div_report_i128(lhs, rhs).checked()
    }

    pub fn checked_div_report_i128(self, lhs: i128, rhs: i128) -> CheckedIntegerResult {
        let lhs = self.canonicalize_i128(lhs);
        let rhs = self.canonicalize_i128(rhs);
        if self.is_signed() && lhs == self.min_i128() && rhs == -1 {
            return CheckedIntegerResult::from_raw(None);
        }
        self.checked_binary_report_i128(lhs, rhs, i128::checked_div)
    }

    pub fn checked_rem_i128(self, lhs: i128, rhs: i128) -> Option<i128> {
        self.checked_rem_report_i128(lhs, rhs).checked()
    }

    pub fn checked_rem_report_i128(self, lhs: i128, rhs: i128) -> CheckedIntegerResult {
        let lhs = self.canonicalize_i128(lhs);
        let rhs = self.canonicalize_i128(rhs);
        let report = self.checked_binary_report_i128(lhs, rhs, i128::checked_rem);
        if self.is_signed() && lhs == self.min_i128() && rhs == -1 {
            CheckedIntegerResult::from_raw(None)
        } else {
            report
        }
    }

    pub fn checked_neg_i128(self, value: i128) -> Option<i128> {
        self.checked_neg_report_i128(value).checked()
    }

    pub fn checked_neg_report_i128(self, value: i128) -> CheckedIntegerResult {
        let value = self.canonicalize_i128(value);
        if self.is_unsigned() {
            let raw = (value == 0).then_some(0);
            CheckedIntegerResult::from_raw(raw)
        } else {
            self.report_raw(value.checked_neg())
        }
    }

    /// Negate an integer literal's mathematical magnitude.  Literal syntax
    /// deliberately keeps the magnitude outside the target type's positive
    /// range so that `-128` can inhabit `i8`; unlike a runtime value, it must
    /// not be canonicalized before the checked operation.
    pub fn checked_neg_literal_i128(self, magnitude: i128) -> Option<i128> {
        self.checked_neg_literal_report_i128(magnitude).checked()
    }

    pub fn checked_neg_literal_report_i128(self, magnitude: i128) -> CheckedIntegerResult {
        if self.is_unsigned() || magnitude < 0 {
            return CheckedIntegerResult::from_raw(None);
        }
        self.report_raw(magnitude.checked_neg())
    }

    pub fn wrapping_add_i128(self, lhs: i128, rhs: i128) -> i128 {
        self.canonicalize_i128(lhs.wrapping_add(rhs))
    }

    pub fn wrapping_sub_i128(self, lhs: i128, rhs: i128) -> i128 {
        self.canonicalize_i128(lhs.wrapping_sub(rhs))
    }

    pub fn wrapping_mul_i128(self, lhs: i128, rhs: i128) -> i128 {
        self.canonicalize_i128(lhs.wrapping_mul(rhs))
    }

    pub fn shift_i128(self, value: i128, amount: i128, left: bool) -> i128 {
        let amount = self.shift_count_i128(amount);
        let value = self.canonicalize_i128(value);
        let shifted = if left {
            value.wrapping_shl(amount)
        } else {
            value >> amount
        };
        self.canonicalize_i128(shifted)
    }

    pub fn bitnot_i128(self, value: i128) -> i128 {
        self.canonicalize_i128(!value)
    }

    pub fn compare_i128(self, lhs: i128, rhs: i128) -> Ordering {
        self.canonicalize_i128(lhs)
            .cmp(&self.canonicalize_i128(rhs))
    }

    pub fn checked_add_u64(self, lhs: u64, rhs: u64) -> Option<u64> {
        self.map_u64(self.checked_add_i128(self.to_i128(lhs), self.to_i128(rhs))?)
    }

    pub fn checked_sub_u64(self, lhs: u64, rhs: u64) -> Option<u64> {
        self.map_u64(self.checked_sub_i128(self.to_i128(lhs), self.to_i128(rhs))?)
    }

    pub fn checked_mul_u64(self, lhs: u64, rhs: u64) -> Option<u64> {
        self.map_u64(self.checked_mul_i128(self.to_i128(lhs), self.to_i128(rhs))?)
    }

    pub fn checked_div_u64(self, lhs: u64, rhs: u64) -> Option<u64> {
        self.map_u64(self.checked_div_i128(self.to_i128(lhs), self.to_i128(rhs))?)
    }

    pub fn checked_rem_u64(self, lhs: u64, rhs: u64) -> Option<u64> {
        self.map_u64(self.checked_rem_i128(self.to_i128(lhs), self.to_i128(rhs))?)
    }

    pub fn checked_neg_u64(self, value: u64) -> Option<u64> {
        self.map_u64(self.checked_neg_i128(self.to_i128(value))?)
    }

    pub fn wrapping_add_u64(self, lhs: u64, rhs: u64) -> u64 {
        self.canonicalize_u64(lhs.wrapping_add(rhs))
    }

    pub fn wrapping_sub_u64(self, lhs: u64, rhs: u64) -> u64 {
        self.canonicalize_u64(lhs.wrapping_sub(rhs))
    }

    pub fn wrapping_mul_u64(self, lhs: u64, rhs: u64) -> u64 {
        self.canonicalize_u64(lhs.wrapping_mul(rhs))
    }

    pub fn shift_u64(self, value: u64, amount: u64, left: bool) -> u64 {
        self.shift_i128(self.to_i128(value), amount as i128, left) as u64
    }

    pub fn bitnot_u64(self, value: u64) -> u64 {
        self.canonicalize_u64(!value)
    }

    pub fn compare_u64(self, lhs: u64, rhs: u64) -> Ordering {
        self.to_i128(lhs).cmp(&self.to_i128(rhs))
    }

    fn checked_binary_report_i128(
        self,
        lhs: i128,
        rhs: i128,
        op: impl FnOnce(i128, i128) -> Option<i128>,
    ) -> CheckedIntegerResult {
        self.report_raw(op(self.canonicalize_i128(lhs), self.canonicalize_i128(rhs)))
    }

    fn report_raw(self, raw: Option<i128>) -> CheckedIntegerResult {
        CheckedIntegerResult {
            raw,
            checked: raw.filter(|&result| self.fits_i128(result)),
        }
    }

    fn to_i128(self, value: u64) -> i128 {
        if self.signed {
            self.canonicalize_u64(value) as i64 as i128
        } else {
            self.mask_u64(value) as i128
        }
    }

    fn map_u64(self, value: i128) -> Option<u64> {
        self.fits_i128(value)
            .then_some(self.canonicalize_i128(value) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::IntegerType;

    const ALL: [IntegerType; 8] = [
        IntegerType::new(8, true).unwrap(),
        IntegerType::new(16, true).unwrap(),
        IntegerType::new(32, true).unwrap(),
        IntegerType::new(64, true).unwrap(),
        IntegerType::new(8, false).unwrap(),
        IntegerType::new(16, false).unwrap(),
        IntegerType::new(32, false).unwrap(),
        IntegerType::new(64, false).unwrap(),
    ];

    #[test]
    fn bounds_and_canonicalization_cover_every_integer_type() {
        for ty in ALL {
            let min = ty.min_i128();
            let max = ty.max_i128();
            assert!(ty.fits_i128(min));
            assert!(ty.fits_i128(max));
            assert!(!ty.fits_i128(min - 1));
            assert!(!ty.fits_i128(max + 1));
            assert_eq!(ty.canonicalize_i128(max + 1), min);
            assert_eq!(ty.canonicalize_i128(min - 1), max);
        }
    }

    #[test]
    fn checked_and_wrapping_boundaries_cover_every_integer_type() {
        for ty in ALL {
            let min = ty.min_i128();
            let max = ty.max_i128();
            assert_eq!(ty.checked_add_i128(max, 1), None);
            assert_eq!(ty.checked_sub_i128(min, 1), None);
            assert_eq!(ty.checked_mul_i128(max, 2), None);
            assert_eq!(ty.wrapping_add_i128(max, 1), min);
            assert_eq!(ty.wrapping_sub_i128(min, 1), max);
            let expected_neg = if ty.is_signed() { None } else { Some(0) };
            assert_eq!(ty.checked_neg_i128(min), expected_neg);
            assert_eq!(ty.checked_div_i128(max, 1), Some(max));
            assert_eq!(ty.checked_rem_i128(max, 1), Some(0));
            if ty.is_signed() {
                assert_eq!(ty.checked_div_i128(min, -1), None);
                assert_eq!(ty.checked_rem_i128(min, -1), None);
            }
            assert_eq!(ty.checked_div_i128(0, 0), None);
            assert_eq!(ty.checked_rem_i128(0, 0), None);
            assert_eq!(ty.compare_i128(min, max), std::cmp::Ordering::Less);
            assert_eq!(ty.compare_i128(max, min), std::cmp::Ordering::Greater);
        }
    }

    #[test]
    fn checked_reports_preserve_overflowing_mathematical_results() {
        let i32 = IntegerType::new(32, true).unwrap();
        let report = i32.checked_add_report_i128(2_000_000_000, 2_000_000_000);
        assert_eq!(report.raw(), Some(4_000_000_000));
        assert_eq!(report.checked(), None);

        for report in [
            i32.checked_div_report_i128(i32.min_i128(), -1),
            i32.checked_rem_report_i128(i32.min_i128(), -1),
        ] {
            assert_eq!(report.raw(), None);
            assert_eq!(report.checked(), None);
        }

        let unrepresentable = i32.report_raw(i128::MAX.checked_add(1));
        assert_eq!(unrepresentable.raw(), None);
        assert_eq!(unrepresentable.checked(), None);
    }

    #[test]
    fn shifts_and_bitnot_use_width_and_signedness() {
        for ty in ALL {
            let max = ty.max_i128();
            let mask = ty.shift_count_mask();
            assert_eq!(ty.shift_count_i128(ty.bits() as i128), 0);
            assert_eq!(ty.shift_count_i128(-1), mask as u32);
            assert_eq!(ty.shift_i128(1, ty.bits() as i128, true), 1);
            assert_eq!(
                ty.shift_i128(1, -1, true),
                ty.shift_i128(1, mask as i128, true)
            );
            let expected_not = if ty.is_signed() { -1 } else { max };
            assert_eq!(ty.bitnot_i128(0), expected_not);
            if ty.is_signed() {
                assert_eq!(ty.shift_i128(-1, 1, false), -1);
                assert_eq!(ty.checked_neg_i128(max), Some(-max));
            } else {
                assert_eq!(ty.shift_i128(max, 1, false), max >> 1);
                assert_eq!(ty.checked_neg_i128(max), None);
            }
        }
    }

    #[test]
    fn checked_neg_canonicalizes_noncanonical_operands() {
        let u8 = IntegerType::new(8, false).unwrap();
        let i8 = IntegerType::new(8, true).unwrap();
        assert_eq!(u8.checked_neg_i128(256), Some(0));
        assert_eq!(i8.checked_neg_i128(128), None);
    }

    #[test]
    fn checked_neg_literal_accepts_signed_minimum_magnitudes() {
        for ty in ALL.iter().copied().filter(|ty| ty.is_signed()) {
            let minimum = ty.min_i128();
            let magnitude = -minimum;
            assert_eq!(ty.checked_neg_literal_i128(magnitude), Some(minimum));
            assert_eq!(ty.checked_neg_literal_i128(magnitude + 1), None);
            assert_eq!(ty.checked_neg_i128(minimum), None);
        }
    }

    #[test]
    fn u64_adapters_preserve_canonical_register_images() {
        for ty in ALL {
            let max = ty.max_i128() as u64;
            assert_eq!(ty.canonicalize_u64(max), max);
            let expected_not = if ty.is_signed() { u64::MAX } else { max };
            assert_eq!(ty.bitnot_u64(0), expected_not);
            assert_eq!(ty.shift_u64(1, ty.bits() as u64, true), 1);
            assert_eq!(ty.shift_count_u64(u64::MAX), ty.shift_count_mask() as u32);
            assert_eq!(ty.compare_u64(0, max), std::cmp::Ordering::Less);
            assert_eq!(ty.checked_div_u64(max, 1), Some(max));
            assert_eq!(ty.checked_rem_u64(max, 1), Some(0));
        }
    }
}
