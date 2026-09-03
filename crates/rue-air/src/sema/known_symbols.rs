//! Pre-interned known symbols for fast comparison.
//!
//! This module provides `KnownSymbols`, a struct that holds pre-interned `Spur`
//! values for commonly compared strings like intrinsic names. By interning these
//! strings once at initialization, we can compare symbols directly (integer
//! comparison) instead of resolving to strings and doing string comparison.
//!
//! # Performance
//!
//! Each `interner.resolve()` call involves a hash table lookup. While individual
//! lookups are fast, the cumulative cost across many intrinsic dispatches can be
//! significant. Pre-interning known symbols reduces intrinsic dispatch from
//! O(string_length) to O(1).
//!
//! # Usage
//!
//! ```ignore
//! let known = KnownSymbols::new(interner);
//!
//! // Fast symbol comparison instead of string comparison
//! if name == known.dbg {
//!     // Handle @dbg intrinsic
//! } else if name == known.cast {
//!     // Handle @cast intrinsic
//! }
//! ```

use lasso::Spur;
#[cfg(test)]
use lasso::ThreadedRodeo;
use rue_rir::SharedSymbolSpace;

/// Pre-interned symbols for known strings.
///
/// This struct is created once during semantic-analysis setup and provides fast
/// symbol comparison for intrinsic dispatch and other common lookups.
#[derive(Debug, Clone, Copy)]
pub struct KnownSymbols {
    // Intrinsic names
    /// The `dbg` intrinsic symbol.
    pub dbg: Spur,
    /// The `drop` intrinsic symbol - the intentional-destroy escape hatch
    /// (RUE-187, ADR-0039). Runs a value's drop glue and discharges its
    /// consumption obligation (linear/affine).
    pub drop: Spur,
    /// The `intCast` intrinsic symbol (deprecated, use `cast`).
    pub int_cast: Spur,
    /// The `bitCast` intrinsic symbol — same-width two's-complement
    /// reinterpretation between integer types (RUE-952, spec 4.13:118). Unlike
    /// `intCast` it preserves the bits rather than the value, and never traps.
    pub bit_cast: Spur,
    pub int_to_float: Spur,
    pub float_to_int: Spur,
    pub float_cast: Spur,
    pub total_cmp: Spur,
    /// The `cast` intrinsic symbol.
    pub cast: Spur,
    /// The `panic` intrinsic symbol.
    pub panic: Spur,
    /// The `assert` intrinsic symbol.
    pub assert: Spur,
    /// The `read_line` intrinsic symbol.
    pub read_line: Spur,
    /// The `to_string` intrinsic symbol - formats an i64 as a decimal String.
    pub to_string: Spur,
    /// The `print` builtin free function - writes a String to stdout (RUE-1).
    pub print: Spur,
    /// The `println` builtin free function - writes a String plus a newline to
    /// stdout (RUE-1).
    pub println: Spur,
    /// The `parse_i32` intrinsic symbol.
    pub parse_i32: Spur,
    /// The `parse_i64` intrinsic symbol.
    pub parse_i64: Spur,
    /// The `parse_u32` intrinsic symbol.
    pub parse_u32: Spur,
    /// The `parse_u64` intrinsic symbol.
    pub parse_u64: Spur,
    /// The `test_preview_gate` intrinsic symbol.
    pub test_preview_gate: Spur,
    /// The `import` builtin symbol.
    pub import: Spur,
    /// The `random_u32` intrinsic symbol.
    pub random_u32: Spur,
    /// The `random_u64` intrinsic symbol.
    pub random_u64: Spur,
    /// The `arg_count` intrinsic symbol — number of command-line arguments,
    /// including `argv[0]` (RUE-935). Nullary, returns `u64`.
    pub arg_count: Spur,
    /// The `arg_ptr` intrinsic symbol — raw pointer to command-line argument
    /// `i`'s bytes, or null when `i >= @arg_count()` (RUE-935). Returns
    /// `ptr mut u8`; requires a `checked` block like the other raw-pointer
    /// intrinsics.
    pub arg_ptr: Spur,
    /// The `arg_len` intrinsic symbol — byte length of command-line argument
    /// `i`, or 0 when out of range (RUE-935). Takes `u64`, returns `u64`.
    pub arg_len: Spur,
    /// The `env_count` intrinsic symbol — number of environment entries
    /// (RUE-935). Nullary, returns `u64`.
    pub env_count: Spur,
    /// The `env_ptr` intrinsic symbol — raw pointer to environment entry `i`'s
    /// `KEY=VALUE` bytes, or null when out of range (RUE-935). Returns
    /// `ptr mut u8`; requires a `checked` block.
    pub env_ptr: Spur,
    /// The `env_len` intrinsic symbol — byte length of environment entry `i`,
    /// or 0 when out of range (RUE-935). Takes `u64`, returns `u64`.
    pub env_len: Spur,
    /// The `wrapping_add` intrinsic symbol — two's-complement addition mod 2^N
    /// (RUE-647).
    pub wrapping_add: Spur,
    /// The `wrapping_sub` intrinsic symbol — two's-complement subtraction mod
    /// 2^N (RUE-647).
    pub wrapping_sub: Spur,
    /// The `wrapping_mul` intrinsic symbol — two's-complement multiplication mod
    /// 2^N (RUE-647).
    pub wrapping_mul: Spur,

    // Type intrinsics

    // Pointer intrinsics (require unchecked block)
    /// The `ptr_read` intrinsic symbol - reads value through pointer.
    pub ptr_read: Spur,
    /// The `ptr_write` intrinsic symbol - writes value through pointer.
    pub ptr_write: Spur,
    /// The `ptr_read_unaligned` / `ptr_write_unaligned` intrinsics: the
    /// explicit unaligned scalar access pair for packed/parsed data
    /// (ADR-0059 Phase 4, RUE-978/RUE-962).
    pub ptr_read_unaligned: Spur,
    pub ptr_write_unaligned: Spur,
    /// The `ptr_offset` intrinsic symbol - pointer arithmetic.
    pub ptr_offset: Spur,
    /// The `ptr_to_int` intrinsic symbol - converts pointer to usize.
    pub ptr_to_int: Spur,
    /// The `int_to_ptr` intrinsic symbol - converts usize to pointer.
    pub int_to_ptr: Spur,
    /// The `raw` intrinsic symbol - takes address of lvalue.
    pub raw: Spur,
    /// The `raw_mut` intrinsic symbol - takes mutable address of lvalue.
    pub raw_mut: Spur,
    /// The `field_ptr` intrinsic symbol - raw `ptr mut` to a struct field place
    /// without forming a reference (RUE-301), the `&raw mut (*p).field` analog.
    pub field_ptr: Spur,
    /// The trusted checked pointer-to-place bridge used by std accessors.
    pub place: Spur,
    /// The `syscall` intrinsic symbol - direct OS syscall.
    pub syscall: Spur,
    /// The unified byte-and-alignment allocation family (ADR-0059 Phase 3,
    /// RUE-961). Every operand is a physical byte count and every pointer is
    /// `ptr mut u8`; typed allocation is source-computed sugar over
    /// `@size_of`/`@align_of`.
    ///
    /// `alloc` is `@alloc(size, align) -> ptr mut u8`, `free` is
    /// `@free(p, size, align)`, and `realloc` is
    /// `@realloc(p, old_size, align, new_size) -> ptr mut u8`. The sizeless
    /// allocator ABI means `@free`/`@realloc` hand the layout back rather than
    /// making the runtime keep a per-block header.
    pub alloc: Spur,
    /// The `free` intrinsic symbol - free a block previously `@alloc`'d.
    pub free: Spur,
    /// The `realloc` intrinsic symbol - grow/shrink an `@alloc`'d block.
    pub realloc: Spur,
    /// The `alloc_zeroed` intrinsic symbol — `@alloc` whose storage is
    /// guaranteed all-zero bytes (ADR-0059 Future Work, RUE-968).
    pub alloc_zeroed: Spur,
    /// The `resize` intrinsic symbol — `@resize(p, old_size, align, new_size)`,
    /// the in-place-only grow/shrink that never moves the block and reports
    /// success as a `bool` (Zig's `Allocator.resize`, RUE-968).
    pub resize: Spur,
    /// Bulk byte primitives `@byte_copy` (memcpy), `@byte_move` (memmove), and
    /// `@byte_set` (memset). ADR-0059 Phase 1 (RUE-937) plus the overlapping
    /// sibling `@byte_move` (RUE-964).
    pub byte_copy: Spur,
    pub byte_move: Spur,
    pub byte_set: Spur,

    // Target platform intrinsics
    /// The `target_arch` intrinsic symbol - returns target CPU architecture.
    pub target_arch: Spur,
    /// The `target_os` intrinsic symbol - returns target operating system.
    pub target_os: Spur,
    /// The `target_data_model` intrinsic symbol - returns the target C data
    /// model (ADR-0064 Amendment 1).
    pub target_data_model: Spur,
    // Builtin type names

    // Special function names
}

impl KnownSymbols {
    /// Create a new `KnownSymbols` by interning all known strings.
    ///
    /// This should be called once during semantic-analysis setup.
    #[cfg(test)]
    pub fn new(interner: &ThreadedRodeo) -> Self {
        Self::with_intern(|text| {
            Ok(interner
                .try_get_or_intern_static(text)
                .expect("test known-symbol fixture must fit its private interner"))
        })
        .expect("test known-symbol fixture must fit its private interner")
    }

    /// Build the known-symbol table through the revision-owned interner
    /// policy. Exhaustion is latched by the shared space and reported at the
    /// provider query boundary; no normal-build mutable override is involved.
    pub fn new_in_space(space: &SharedSymbolSpace) -> Result<Self, lasso::LassoErrorKind> {
        Self::with_intern(|text| space.try_intern(text))
    }

    fn with_intern(
        mut intern: impl FnMut(&'static str) -> Result<Spur, lasso::LassoErrorKind>,
    ) -> Result<Self, lasso::LassoErrorKind> {
        Ok(Self {
            // Intrinsic names
            dbg: intern("dbg")?,
            drop: intern("drop")?,
            int_cast: intern("intCast")?,
            bit_cast: intern("bitCast")?,
            int_to_float: intern("int_to_float")?,
            float_to_int: intern("float_to_int")?,
            float_cast: intern("float_cast")?,
            total_cmp: intern("total_cmp")?,
            cast: intern("cast")?,
            panic: intern("panic")?,
            assert: intern("assert")?,
            read_line: intern("read_line")?,
            to_string: intern("to_string")?,
            print: intern("print")?,
            println: intern("println")?,
            parse_i32: intern("parse_i32")?,
            parse_i64: intern("parse_i64")?,
            parse_u32: intern("parse_u32")?,
            parse_u64: intern("parse_u64")?,
            test_preview_gate: intern("test_preview_gate")?,
            import: intern("import")?,
            random_u32: intern("random_u32")?,
            random_u64: intern("random_u64")?,
            arg_count: intern("arg_count")?,
            arg_ptr: intern("arg_ptr")?,
            arg_len: intern("arg_len")?,
            env_count: intern("env_count")?,
            env_ptr: intern("env_ptr")?,
            env_len: intern("env_len")?,
            wrapping_add: intern("wrapping_add")?,
            wrapping_sub: intern("wrapping_sub")?,
            wrapping_mul: intern("wrapping_mul")?,

            // Type intrinsics

            // Pointer intrinsics
            ptr_read: intern("ptr_read")?,
            ptr_write: intern("ptr_write")?,
            ptr_read_unaligned: intern("ptr_read_unaligned")?,
            ptr_write_unaligned: intern("ptr_write_unaligned")?,
            ptr_offset: intern("ptr_offset")?,
            ptr_to_int: intern("ptr_to_int")?,
            int_to_ptr: intern("int_to_ptr")?,
            raw: intern("raw")?,
            raw_mut: intern("raw_mut")?,
            field_ptr: intern("field_ptr")?,
            place: intern("place")?,
            syscall: intern("syscall")?,
            alloc: intern("alloc")?,
            free: intern("free")?,
            realloc: intern("realloc")?,
            alloc_zeroed: intern("alloc_zeroed")?,
            resize: intern("resize")?,
            byte_copy: intern("byte_copy")?,
            byte_move: intern("byte_move")?,
            byte_set: intern("byte_set")?,

            // Target platform intrinsics
            target_arch: intern("target_arch")?,
            target_os: intern("target_os")?,
            target_data_model: intern("target_data_model")?,
            // Builtin type names

            // Special function names
        })
    }

    /// Return the semantic operation selected by a parse intrinsic symbol.
    /// Diagnostic spelling is derived from the resulting typed operation.
    pub fn get_parse_intrinsic_operation(&self, sym: Spur) -> Option<crate::IntrinsicOperation> {
        if sym == self.parse_i32 {
            Some(crate::IntrinsicOperation::ParseI32)
        } else if sym == self.parse_i64 {
            Some(crate::IntrinsicOperation::ParseI64)
        } else if sym == self.parse_u32 {
            Some(crate::IntrinsicOperation::ParseU32)
        } else if sym == self.parse_u64 {
            Some(crate::IntrinsicOperation::ParseU64)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_symbols_creation() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        // Verify symbols can be resolved back to their expected strings
        assert_eq!(interner.resolve(&known.dbg), "dbg");
        assert_eq!(interner.resolve(&known.drop), "drop");
        assert_eq!(interner.resolve(&known.int_cast), "intCast");
        assert_eq!(interner.resolve(&known.bit_cast), "bitCast");
        assert_eq!(interner.resolve(&known.cast), "cast");
        assert_eq!(interner.resolve(&known.panic), "panic");
        assert_eq!(interner.resolve(&known.assert), "assert");
        assert_eq!(interner.resolve(&known.read_line), "read_line");
        assert_eq!(interner.resolve(&known.print), "print");
        assert_eq!(interner.resolve(&known.println), "println");
        assert_eq!(interner.resolve(&known.parse_i32), "parse_i32");
        assert_eq!(interner.resolve(&known.parse_i64), "parse_i64");
        assert_eq!(interner.resolve(&known.parse_u32), "parse_u32");
        assert_eq!(interner.resolve(&known.parse_u64), "parse_u64");
        assert_eq!(
            interner.resolve(&known.test_preview_gate),
            "test_preview_gate"
        );
        assert_eq!(interner.resolve(&known.import), "import");
        assert_eq!(interner.resolve(&known.random_u32), "random_u32");
        assert_eq!(interner.resolve(&known.random_u64), "random_u64");
        assert_eq!(interner.resolve(&known.arg_count), "arg_count");
        assert_eq!(interner.resolve(&known.arg_ptr), "arg_ptr");
        assert_eq!(interner.resolve(&known.arg_len), "arg_len");
        assert_eq!(interner.resolve(&known.env_count), "env_count");
        assert_eq!(interner.resolve(&known.env_ptr), "env_ptr");
        assert_eq!(interner.resolve(&known.env_len), "env_len");
        assert_eq!(interner.resolve(&known.ptr_read), "ptr_read");
        assert_eq!(interner.resolve(&known.ptr_write), "ptr_write");
        assert_eq!(
            interner.resolve(&known.ptr_read_unaligned),
            "ptr_read_unaligned"
        );
        assert_eq!(
            interner.resolve(&known.ptr_write_unaligned),
            "ptr_write_unaligned"
        );
        assert_eq!(interner.resolve(&known.ptr_offset), "ptr_offset");
        assert_eq!(interner.resolve(&known.ptr_to_int), "ptr_to_int");
        assert_eq!(interner.resolve(&known.int_to_ptr), "int_to_ptr");
        assert_eq!(interner.resolve(&known.raw), "raw");
        assert_eq!(interner.resolve(&known.raw_mut), "raw_mut");
        assert_eq!(interner.resolve(&known.syscall), "syscall");
        assert_eq!(interner.resolve(&known.alloc), "alloc");
        assert_eq!(interner.resolve(&known.free), "free");
        assert_eq!(interner.resolve(&known.realloc), "realloc");
        assert_eq!(interner.resolve(&known.alloc_zeroed), "alloc_zeroed");
        assert_eq!(interner.resolve(&known.resize), "resize");
        assert_eq!(interner.resolve(&known.byte_copy), "byte_copy");
        assert_eq!(interner.resolve(&known.byte_move), "byte_move");
        assert_eq!(interner.resolve(&known.byte_set), "byte_set");
        assert_eq!(interner.resolve(&known.target_arch), "target_arch");
        assert_eq!(interner.resolve(&known.target_os), "target_os");
        assert_eq!(
            interner.resolve(&known.target_data_model),
            "target_data_model"
        );
    }

    #[test]
    fn known_symbols_comparison() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        // Interning the same string should return the same Spur
        let dbg_sym = interner.get_or_intern("dbg");
        assert_eq!(dbg_sym, known.dbg);
    }

    #[test]
    fn parse_intrinsic_classifier_has_exact_spelling_to_operation_table() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        let exact = [
            (
                "parse_i32",
                known.parse_i32,
                crate::IntrinsicOperation::ParseI32,
            ),
            (
                "parse_i64",
                known.parse_i64,
                crate::IntrinsicOperation::ParseI64,
            ),
            (
                "parse_u32",
                known.parse_u32,
                crate::IntrinsicOperation::ParseU32,
            ),
            (
                "parse_u64",
                known.parse_u64,
                crate::IntrinsicOperation::ParseU64,
            ),
        ];
        for (spelling, symbol, operation) in exact {
            assert_eq!(interner.resolve(&symbol), spelling);
            assert_eq!(known.get_parse_intrinsic_operation(symbol), Some(operation));
            assert_eq!(operation.expected_spelling(), spelling);
        }
        assert_eq!(known.get_parse_intrinsic_operation(known.dbg), None);
    }

    #[test]
    fn known_symbols_is_copy() {
        // KnownSymbols should be Copy since it only contains Spur values
        fn assert_copy<T: Copy>() {}
        assert_copy::<KnownSymbols>();
    }
}
