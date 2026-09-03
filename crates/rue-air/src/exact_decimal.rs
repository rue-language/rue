//! Canonical exact identity for decimal floating-point literals.

use crate::Type;

/// Convert an exact decimal spelling to the correctly-rounded IEEE bit pattern.
///
/// This is the sole exact-decimal-to-machine boundary. Rust's decimal parser
/// supplies correctly rounded ties-to-even conversion; Rue additionally
/// rejects values whose rounded result is infinite.
#[must_use]
pub fn finite_float_literal_bits(text: &str, ty: Type) -> Option<u64> {
    match ty {
        Type::F32 => {
            let value = text.parse::<f32>().ok()?;
            value.is_finite().then(|| u64::from(value.to_bits()))
        }
        Type::F64 => {
            let value = text.parse::<f64>().ok()?;
            value.is_finite().then(|| value.to_bits())
        }
        _ => None,
    }
}

/// Convert a lexer-owned unsigned literal while applying a source unary sign.
#[must_use]
pub fn finite_float_literal_bits_with_sign(text: &str, ty: Type, negative: bool) -> Option<u64> {
    let bits = finite_float_literal_bits(text, ty)?;
    Some(if negative {
        bits ^ if ty == Type::F32 { 1 << 31 } else { 1 << 63 }
    } else {
        bits
    })
}

/// Canonicalize a lexer-validated, non-negative decimal float literal.
///
/// The result is `<significand>e<decimal exponent>`. Both components remain
/// decimal strings, so neither precision nor exponent range is machine-bound.
#[must_use]
pub fn canonical_decimal_literal(text: &str) -> Option<String> {
    let exponent_at = text.find(['e', 'E']);
    let (mantissa, exponent) = exponent_at.map_or((text, "0"), |at| (&text[..at], &text[at + 1..]));
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let mut joined = String::with_capacity(whole.len() + fraction.len());
    joined.push_str(whole);
    joined.push_str(fraction);
    let digits = joined.trim_start_matches('0');
    if digits.is_empty() {
        return Some("0e0".to_owned());
    }
    let trailing = digits
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'0')
        .count();
    let significand = &digits[..digits.len() - trailing];

    let mut exponent = SignedDecimal::parse(exponent)?;
    exponent.add_signed_count(false, fraction.len());
    exponent.add_signed_count(true, trailing);
    Some(format!("{significand}e{}", exponent.render()))
}

#[derive(Clone)]
struct SignedDecimal {
    negative: bool,
    magnitude: String,
}

impl SignedDecimal {
    fn parse(text: &str) -> Option<Self> {
        let (negative, magnitude) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        if magnitude.is_empty() || !magnitude.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let magnitude = magnitude.trim_start_matches('0');
        Some(Self {
            negative: negative && !magnitude.is_empty(),
            magnitude: if magnitude.is_empty() { "0" } else { magnitude }.to_owned(),
        })
    }

    fn add_signed_count(&mut self, positive: bool, count: usize) {
        if count == 0 {
            return;
        }
        let rhs = count.to_string();
        if self.negative != positive {
            self.magnitude = add_magnitudes(&self.magnitude, &rhs);
            return;
        }
        match compare_magnitudes(&self.magnitude, &rhs) {
            std::cmp::Ordering::Greater => {
                self.magnitude = subtract_magnitudes(&self.magnitude, &rhs);
            }
            std::cmp::Ordering::Equal => {
                self.negative = false;
                self.magnitude = "0".to_owned();
            }
            std::cmp::Ordering::Less => {
                self.negative = !self.negative;
                self.magnitude = subtract_magnitudes(&rhs, &self.magnitude);
            }
        }
    }

    fn render(&self) -> String {
        if self.negative {
            format!("-{}", self.magnitude)
        } else {
            self.magnitude.clone()
        }
    }
}

fn compare_magnitudes(lhs: &str, rhs: &str) -> std::cmp::Ordering {
    lhs.len().cmp(&rhs.len()).then_with(|| lhs.cmp(rhs))
}

fn add_magnitudes(lhs: &str, rhs: &str) -> String {
    let mut lhs = lhs.bytes().rev();
    let mut rhs = rhs.bytes().rev();
    let mut carry = 0;
    let mut result = Vec::new();
    loop {
        let l = lhs.next().map(|byte| byte - b'0');
        let r = rhs.next().map(|byte| byte - b'0');
        if l.is_none() && r.is_none() && carry == 0 {
            break;
        }
        let sum = l.unwrap_or(0) + r.unwrap_or(0) + carry;
        result.push(b'0' + sum % 10);
        carry = sum / 10;
    }
    result.reverse();
    String::from_utf8(result).expect("decimal arithmetic emits ASCII")
}

/// Subtract `rhs` from `lhs`; callers guarantee `lhs >= rhs`.
fn subtract_magnitudes(lhs: &str, rhs: &str) -> String {
    let mut result = Vec::with_capacity(lhs.len());
    let mut rhs = rhs.bytes().rev();
    let mut borrow = 0_i16;
    for l in lhs.bytes().rev() {
        let mut digit = i16::from(l - b'0') - borrow;
        let r = i16::from(rhs.next().map_or(0, |byte| byte - b'0'));
        if digit < r {
            digit += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        result.push(b'0' + u8::try_from(digit - r).expect("decimal digit"));
    }
    while result.len() > 1 && result.last() == Some(&b'0') {
        result.pop();
    }
    result.reverse();
    String::from_utf8(result).expect("decimal arithmetic emits ASCII")
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_decimal_literal, finite_float_literal_bits, finite_float_literal_bits_with_sign,
    };
    use crate::Type;

    #[test]
    fn equivalent_spellings_share_exact_identity() {
        for spelling in ["1.0", "1e0", "01.000e+000", "1000e-3"] {
            assert_eq!(canonical_decimal_literal(spelling).as_deref(), Some("1e0"));
        }
        assert_eq!(
            canonical_decimal_literal("0.000e999999999999999999999").as_deref(),
            Some("0e0")
        );
        assert_eq!(
            canonical_decimal_literal("123.4500e-999999999999999999999").as_deref(),
            Some("12345e-1000000000000000000001")
        );
    }

    #[test]
    fn contextual_machine_rounding_and_range_are_centralized() {
        let lower = canonical_decimal_literal("1.000000059604644775390625").unwrap();
        let upper = canonical_decimal_literal("1.000000178813934326171875").unwrap();
        assert_eq!(
            finite_float_literal_bits(&lower, Type::F32),
            Some(u64::from(1.0_f32.to_bits()))
        );
        assert_eq!(
            finite_float_literal_bits(&upper, Type::F32),
            Some(u64::from(1.0_f32.to_bits() + 2))
        );
        let f64_lower = "1.00000000000000011102230246251565404236316680908203125";
        let f64_upper = "1.00000000000000033306690738754696212708950042724609375";
        assert_eq!(
            finite_float_literal_bits(f64_lower, Type::F64),
            Some(1.0_f64.to_bits())
        );
        assert_eq!(
            finite_float_literal_bits(f64_upper, Type::F64),
            Some(1.0_f64.to_bits() + 2)
        );
        assert_eq!(
            finite_float_literal_bits("-1e-9999", Type::F64),
            Some((-0.0_f64).to_bits())
        );
        assert_eq!(finite_float_literal_bits("1e-9999", Type::F32), Some(0));
        assert!(
            finite_float_literal_bits("340282346638528859811704183484516925440", Type::F32,)
                .is_some()
        );
        // One decimal unit above the exact midpoint between max-finite and
        // infinity must overflow; the midpoint itself is the ties-to-even
        // boundary for this last binade.
        assert!(
            finite_float_literal_bits("340282356779733661637539395458142568449", Type::F32,)
                .is_none()
        );
        assert!(finite_float_literal_bits("1.7976931348623157e308", Type::F64).is_some());
        assert!(finite_float_literal_bits("1.7976931348623159e308", Type::F64).is_none());
        assert_eq!(
            finite_float_literal_bits_with_sign("0.0", Type::F32, true),
            Some(u64::from((-0.0_f32).to_bits()))
        );
        assert_eq!(
            finite_float_literal_bits_with_sign("0.0", Type::F64, true),
            Some((-0.0_f64).to_bits())
        );
    }
}
