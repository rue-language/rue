//! Unicode's table of well-formed UTF-8 byte sequences.
//!
//! Table 3-7 of the Unicode standard is the whole of UTF-8 well-formedness. A
//! lead byte fixes two things: how many bytes the sequence it begins occupies,
//! and the range its *first* continuation byte may take. That first-byte range
//! is what rejects an overlong encoding (`0xE0` needs `>= 0xA0`, `0xF0` needs
//! `>= 0x90`), a surrogate (`0xED` needs `<= 0x9F`), and a scalar above
//! `U+10FFFF` (`0xF4` needs `<= 0x8F`). Every later byte of the sequence is an
//! unconstrained continuation, so a full-width sequence that clears the table
//! is always a valid scalar, and nothing outside the table ever is.
//!
//! The table is spelled out here rather than delegated to
//! `core::str::from_utf8` so the freestanding build's emitted code stays a
//! handful of comparisons with no libcall in it. Spelling it out *once* is why
//! this module exists: the JSON escaper in [`crate::test_channel`] and the
//! lossy decoder in [`crate::string`] both need the same rows, and two
//! hand-copied tables in one crate are two things to keep in step.

/// One lead byte's row of the table.
///
/// A row describes a multi-byte sequence only; see [`lead`] for why ASCII has
/// no row.
pub(crate) struct Utf8Lead {
    /// Total bytes in the sequence, counting the lead byte itself.
    ///
    /// **Always at least 2.** The table has no single-byte row (see [`lead`]),
    /// and both consumers depend on that: they read the first continuation
    /// byte without a separate emptiness check once they know the buffer holds
    /// `width` bytes, and [`lead`] derives [`Self::scalar_prefix`] with a mask
    /// that is only correct for widths 2, 3 and 4. Adding a width-1 row would
    /// make an in-bounds read out of bounds and the prefix mask wrong; add an
    /// ASCII path to the caller instead.
    pub(crate) width: usize,
    /// Inclusive lower bound on the first continuation byte.
    pub(crate) first_continuation_min: u8,
    /// Inclusive upper bound on the first continuation byte.
    pub(crate) first_continuation_max: u8,
    /// The scalar bits the lead byte itself contributes, already masked off:
    /// the high bits of a decoded scalar, before any continuation byte shifts
    /// in its own six.
    ///
    /// Only meaningful for a multi-byte row; see [`Self::width`].
    pub(crate) scalar_prefix: u32,
}

/// The table row for `byte`, or `None` when `byte` cannot begin a multi-byte
/// sequence: a continuation byte (`0x80..=0xBF`), an always-overlong lead
/// (`0xC0`, `0xC1`), or a lead past the end of the Unicode range
/// (`0xF5..=0xFF`).
///
/// ASCII has no row on purpose — `byte < 0x80` returns `None` as well. A
/// single byte is not a sequence with a continuation range to check, and every
/// caller takes an ASCII fast path before it reaches the table, so a row for
/// ASCII would only be an arm no caller can reach. That every row is therefore
/// at least two bytes wide is load-bearing rather than incidental; see
/// [`Utf8Lead::width`] before adding one.
#[inline]
pub(crate) fn lead(byte: u8) -> Option<Utf8Lead> {
    let (width, first_continuation_min, first_continuation_max) = match byte {
        0xC2..=0xDF => (2usize, 0x80u8, 0xBFu8),
        0xE0 => (3, 0xA0, 0xBF),
        0xE1..=0xEC => (3, 0x80, 0xBF),
        0xED => (3, 0x80, 0x9F),
        0xEE..=0xEF => (3, 0x80, 0xBF),
        0xF0 => (4, 0x90, 0xBF),
        0xF1..=0xF3 => (4, 0x80, 0xBF),
        0xF4 => (4, 0x80, 0x8F),
        _ => return None,
    };
    // A width-`n` lead is `n` leading ones, a zero, then its payload bits:
    // `110xxxxx`, `1110xxxx`, `11110xxx`. That leaves `7 - width` payload bits,
    // which is exactly the mask `0x7F >> width`. This relies on every row above
    // having `width >= 2` (see `Utf8Lead::width`): a width-1 row would mask
    // with `0x3F` and drop two bits of an ASCII byte.
    let scalar_prefix = (byte & (0x7F >> width)) as u32;
    Some(Utf8Lead {
        width,
        first_continuation_min,
        first_continuation_max,
        scalar_prefix,
    })
}

impl Utf8Lead {
    /// Whether `byte` is an acceptable *first* continuation for this lead.
    #[inline]
    pub(crate) fn accepts_first_continuation(&self, byte: u8) -> bool {
        (self.first_continuation_min..=self.first_continuation_max).contains(&byte)
    }
}

/// Whether `byte` is a continuation byte, `0b10xxxxxx`.
///
/// This is the check for every byte of a sequence *after* the first
/// continuation, whose range the lead byte narrows instead.
#[inline]
pub(crate) fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    /// Assemble the shortest byte sequence that exercises lead `b0` with first
    /// continuation `b1`, padding any remaining positions with a continuation
    /// byte that the table always accepts.
    fn candidate(b0: u8, b1: u8, width: usize) -> std::vec::Vec<u8> {
        let mut bytes = std::vec![0x80u8; width];
        bytes[0] = b0;
        if width > 1 {
            bytes[1] = b1;
        }
        bytes
    }

    /// The table is Unicode's, so it must agree with the standard library's
    /// validator on the whole lead-by-first-continuation cross product — the
    /// entire content of the rows. `core::str::from_utf8` is the oracle here
    /// only; the freestanding build never calls it.
    #[test]
    fn the_table_agrees_with_the_standard_validator_on_every_lead_and_first_byte() {
        for b0 in 0u8..=0xFF {
            for b1 in 0u8..=0xFF {
                let row = lead(b0);
                let accepted = row
                    .as_ref()
                    .is_some_and(|row| row.accepts_first_continuation(b1));
                // Width 4 is the widest row; a shorter row ignores the tail.
                let width = row.as_ref().map_or(1, |row| row.width);
                let bytes = candidate(b0, b1, width);
                let valid = b0 >= 0x80 && core::str::from_utf8(&bytes).is_ok();
                assert_eq!(
                    accepted, valid,
                    "lead {b0:#04x}, first continuation {b1:#04x}: table says \
                     {accepted}, validator says {valid}"
                );
            }
        }
    }

    /// The prefix a row reports has to be the payload bits of the lead byte,
    /// so a decoder that shifts continuations into it lands on the scalar the
    /// standard library decodes.
    #[test]
    fn a_rows_scalar_prefix_reconstructs_the_scalar() {
        for b0 in 0u8..=0xFF {
            let Some(row) = lead(b0) else { continue };
            for b1 in row.first_continuation_min..=row.first_continuation_max {
                let bytes = candidate(b0, b1, row.width);
                let text = core::str::from_utf8(&bytes).expect("row accepts this sequence");
                let expected = text.chars().next().expect("one scalar").len_utf8();
                assert_eq!(expected, row.width, "{bytes:?}");

                let mut scalar = row.scalar_prefix;
                for byte in &bytes[1..] {
                    assert!(is_continuation(*byte), "{bytes:?}");
                    scalar = (scalar << 6) | (u32::from(*byte) & 0x3F);
                }
                assert_eq!(
                    scalar,
                    text.chars().next().expect("one scalar") as u32,
                    "{bytes:?}"
                );
            }
        }
    }

    /// The bytes that can never begin a sequence are exactly the ones with no
    /// row: continuations, the two always-overlong leads, and everything past
    /// the end of the Unicode range.
    #[test]
    fn only_the_bytes_that_can_begin_a_sequence_have_a_row() {
        for byte in 0u8..=0xFF {
            let expected = matches!(byte, 0xC2..=0xF4);
            assert_eq!(lead(byte).is_some(), expected, "{byte:#04x}");
        }
    }

    /// Every row is at least two bytes wide. Both consumers read the first
    /// continuation byte off the strength of this, and `lead`'s prefix mask is
    /// only correct above width 1, so pin it rather than leaving it to prose.
    #[test]
    fn every_row_is_at_least_two_bytes_wide() {
        for byte in 0u8..=0xFF {
            let Some(row) = lead(byte) else { continue };
            assert!(row.width >= 2, "{byte:#04x} has width {}", row.width);
            assert!(row.width <= 4, "{byte:#04x} has width {}", row.width);
        }
    }

    /// A continuation byte is the low quarter of the high half, and nothing
    /// else.
    #[test]
    fn continuation_bytes_are_exactly_0x80_through_0xbf() {
        for byte in 0u8..=0xFF {
            assert_eq!(
                is_continuation(byte),
                (0x80..=0xBF).contains(&byte),
                "{byte:#04x}"
            );
        }
    }
}
