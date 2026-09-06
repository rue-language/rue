//! Small shared helpers used across the linker's emitters.

use crate::linker::LinkError;

/// Bytes copied or filled between two cancellation checkpoints.
///
/// Every buffer-growing helper below walks its work in chunks of this size so a
/// caller's cancellation flag is observed within a bounded amount of copying,
/// however large the merged image is.
const CANCELLATION_CHUNK: usize = 64 * 1024;

/// A fill of plain zero bytes, for gaps that are data rather than instructions.
pub(crate) const ZERO_FILL: &[u8] = &[0];

/// Return `Err(LinkError::Canceled)` if the caller has requested cancellation.
///
/// One error type for every cancellable step of a link: the section merge, the
/// relocation loop, and the Mach-O and ELF serializers all report a canceled
/// link the same way, so no caller has to translate between an `Option` and a
/// `Result` at a module boundary.
pub(crate) fn check_cancellation(cancellation: &mut impl FnMut() -> bool) -> Result<(), LinkError> {
    if cancellation() {
        Err(LinkError::Canceled)
    } else {
        Ok(())
    }
}

/// Append `bytes` to `output`, checking for cancellation every chunk.
pub(crate) fn extend_bytes_with_cancellation(
    output: &mut Vec<u8>,
    bytes: &[u8],
    cancellation: &mut impl FnMut() -> bool,
) -> Result<(), LinkError> {
    for chunk in bytes.chunks(CANCELLATION_CHUNK) {
        check_cancellation(cancellation)?;
        output.extend_from_slice(chunk);
    }
    Ok(())
}

/// Grow `output` to `new_len` by repeating `fill`, checking for cancellation
/// every chunk. Shrinking is not possible: an `output` already at or past
/// `new_len` is left alone.
///
/// The fill is *phased by the buffer's own offsets* — byte `n` of the output is
/// `fill[n % fill.len()]` — so a multi-byte pattern lands on the lattice the
/// pattern describes. That is what makes a 4-byte gap in an aligned AArch64
/// text buffer come out as a whole `BRK #0` instruction rather than a rotation
/// of one.
pub(crate) fn pad_to_with_cancellation(
    output: &mut Vec<u8>,
    new_len: usize,
    fill: &[u8],
    cancellation: &mut impl FnMut() -> bool,
) -> Result<(), LinkError> {
    // Always-on: the linker crate keeps no debug-only barriers between a
    // malformed request and its output, and one check per padding call is free.
    assert!(!fill.is_empty(), "a fill pattern needs at least one byte");
    while output.len() < new_len {
        check_cancellation(cancellation)?;
        let chunk_end = new_len.min(output.len().saturating_add(CANCELLATION_CHUNK));
        if let [byte] = fill {
            output.resize(chunk_end, *byte);
        } else {
            while output.len() < chunk_end {
                output.push(fill[output.len() % fill.len()]);
            }
        }
    }
    Ok(())
}

/// Align a value up to the given alignment.
///
/// `align` must be a power of two.
pub(crate) fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

/// Prepend the single leading underscore that Mach-O uses for every symbol.
///
/// Mach-O emission adds EXACTLY ONE `_` to every symbol name, uniformly, with
/// no special-casing for names that already start with underscores. Keeping
/// this in one function guarantees the emit side and [`strip_macho_underscore`]
/// stay exact inverses (RUE-919):
/// - `"main"`  -> `"_main"`
/// - `"_foo"`  -> `"__foo"`
/// - `"__foo"` -> `"___foo"`
pub(crate) fn add_macho_underscore(name: &str) -> String {
    format!("_{name}")
}

/// Inverse of [`add_macho_underscore`]: strip the single leading underscore that
/// Mach-O emission prepended.
///
/// Because emission adds EXACTLY ONE underscore, the exact inverse removes
/// EXACTLY ONE, which round-trips for any number of leading underscores. This is
/// what lets `_foo` and `__foo` remain distinct symbols instead of both
/// collapsing onto `__foo` (RUE-919). A name with no leading underscore is
/// returned unchanged.
pub(crate) fn strip_macho_underscore(name: &str) -> &str {
    name.strip_prefix('_').unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 16), 0);
        assert_eq!(align_up(1, 16), 16);
        assert_eq!(align_up(16, 16), 16);
        assert_eq!(align_up(17, 16), 32);
        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(7, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
    }

    /// A multi-byte fill is phased by the buffer's own offsets, so an aligned
    /// gap comes out as whole copies of the pattern rather than a rotation of
    /// one. That is what makes a padded AArch64 text gap decode as `BRK #0`.
    #[test]
    fn pad_to_phases_a_multi_byte_fill_by_buffer_offset() {
        let trap = [0x00, 0x00, 0x20, 0xD4];
        let mut aligned = vec![0xAA; 4];
        pad_to_with_cancellation(&mut aligned, 12, &trap, &mut || false).unwrap();
        assert_eq!(&aligned[4..], &trap.repeat(2));

        let mut misaligned = vec![0xAA; 3];
        pad_to_with_cancellation(&mut misaligned, 4, &trap, &mut || false).unwrap();
        assert_eq!(
            misaligned[3], trap[3],
            "byte 3 of the output is byte 3 of the pattern"
        );
    }

    /// Padding only ever grows a buffer: a target length at or below the
    /// current one leaves the bytes alone.
    #[test]
    fn pad_to_never_shrinks() {
        let mut buffer = vec![1, 2, 3, 4];
        pad_to_with_cancellation(&mut buffer, 2, ZERO_FILL, &mut || false).unwrap();
        assert_eq!(buffer, vec![1, 2, 3, 4]);
    }

    /// Both bounded helpers report a canceled link through the one link error
    /// type, so no caller has to translate at a module boundary.
    #[test]
    fn bounded_helpers_report_cancellation_as_a_link_error() {
        let mut buffer = Vec::new();
        assert!(matches!(
            pad_to_with_cancellation(&mut buffer, 8, ZERO_FILL, &mut || true),
            Err(LinkError::Canceled)
        ));
        assert!(matches!(
            extend_bytes_with_cancellation(&mut buffer, &[1, 2, 3], &mut || true),
            Err(LinkError::Canceled)
        ));
        assert!(buffer.is_empty(), "a canceled fill writes nothing");
    }

    /// Mach-O add/strip must be exact inverses for any number of leading
    /// underscores, so distinct source identifiers stay distinct symbols
    /// (RUE-919). The old strip was off by one and collapsed `_foo`/`__foo`.
    #[test]
    fn test_macho_underscore_round_trip() {
        for name in [
            "foo",
            "_foo",
            "__foo",
            "___foo",
            "main",
            ".rodata.str0",
            "____many",
        ] {
            let emitted = add_macho_underscore(name);
            assert_eq!(emitted, format!("_{name}"), "emission adds exactly one _");
            assert_eq!(
                strip_macho_underscore(&emitted),
                name,
                "strip must recover the original name for {name:?}",
            );
        }
    }

    /// The key regression: `_foo` and `__foo` are distinct identifiers and must
    /// survive the emit/strip round-trip as distinct symbols.
    #[test]
    fn test_macho_underscore_keeps_distinct_symbols() {
        let a = add_macho_underscore("_foo");
        let b = add_macho_underscore("__foo");
        assert_ne!(a, b, "distinct names emit to distinct symbols");
        assert_ne!(
            strip_macho_underscore(&a),
            strip_macho_underscore(&b),
            "distinct names must not collapse on strip",
        );
        assert_eq!(strip_macho_underscore(&a), "_foo");
        assert_eq!(strip_macho_underscore(&b), "__foo");
    }

    /// A name without a leading underscore is returned unchanged (e.g. a
    /// synthetic symbol that did not pass through Mach-O emission).
    #[test]
    fn test_strip_macho_underscore_no_prefix() {
        assert_eq!(strip_macho_underscore("foo"), "foo");
        assert_eq!(strip_macho_underscore(""), "");
    }
}
