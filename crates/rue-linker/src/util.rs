//! Small shared helpers used across the linker's emitters.

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
