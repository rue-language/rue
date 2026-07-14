//! Runtime symbol constants and compiler-provided enum definitions.
//!
//! Source-defined standard-library types use trusted language-item identity;
//! this crate does not inject or describe nominal struct types.

// ============================================================================
// Free-standing String runtime operations (RUE-17 Phase 1, ADR-0035)
// ============================================================================

// These constants document retained runtime ABI spellings. Canonical StrBuf
// operations are source-defined standard-library methods selected through
// trusted language-item identity; the historical `__rue_String_*` exports stay
// stable for ABI compatibility and are not a semantic method registry.

/// Runtime symbol for `@to_string(n)` on a signed integer: formats an `i64` as
/// its decimal representation into a freshly heap-allocated `String` (full range
/// including `i64::MIN`; negatives are prefixed with `-`). Narrower signed
/// operands (`i8`/`i16`/`i32`) are sign-extended to `i64` before the call. sret
/// ABI: `extern "C" fn __rue_to_string(out: *mut StringResult, n: i64)`.
pub const TO_STRING_RUNTIME_FN: &str = "__rue_to_string";

/// Runtime symbol for `@to_string(n)` on an unsigned integer: formats a `u64` as
/// its decimal representation into a freshly heap-allocated `String` (full
/// range, so a value with the high bit set prints as its unsigned magnitude, not
/// a negative number). Narrower unsigned operands (`u8`/`u16`/`u32`) are
/// zero-extended to `u64` before the call. sret ABI:
/// `extern "C" fn __rue_to_string_unsigned(out: *mut StringResult, n: u64)`.
pub const TO_STRING_UNSIGNED_RUNTIME_FN: &str = "__rue_to_string_unsigned";

/// Runtime symbol for `s1 + s2` on two `String`s: returns a NEW `String` whose
/// bytes are the concatenation of `s1` and `s2`. Both operands are borrowed
/// (neither is consumed). sret ABI: `extern "C" fn __rue_String_concat(out:
/// *mut StringResult, ptr1, len1, cap1, ptr2, len2, cap2)`.
pub const STRING_CONCAT_RUNTIME_FN: &str = "__rue_String_concat";

/// Runtime symbol for `print(s)`: writes the raw bytes of `s` to stdout with no
/// added newline. The `String` is passed by borrow (not consumed), flattened
/// into three ABI slots: `extern "C" fn __rue_print(ptr, len, cap)` (`cap`
/// unused). Returns unit (RUE-1).
pub const PRINT_RUNTIME_FN: &str = "__rue_print";

/// Runtime symbol for `println(s)`: writes the raw bytes of `s` to stdout
/// followed by a single `\n`. Same borrow ABI as [`PRINT_RUNTIME_FN`]:
/// `extern "C" fn __rue_println(ptr, len, cap)` (`cap` unused). Returns unit
/// (RUE-1).
pub const PRINTLN_RUNTIME_FN: &str = "__rue_println";

// ============================================================================
// Built-in Enums (Target Platform)
// ============================================================================

/// Definition of a built-in enum type.
///
/// These are synthetic enums injected by the compiler before processing user code.
/// They are used for compile-time platform detection via intrinsics like
/// `@target_arch()` and `@target_os()`.
#[derive(Debug, Clone)]
pub struct BuiltinEnumDef {
    /// Enum name as it appears in source code (e.g., "Arch")
    pub name: &'static str,
    /// Variant names in order (index matches variant_index in EnumVariant)
    pub variants: &'static [&'static str],
}

/// The built-in Arch enum for CPU architecture detection.
///
/// Variants:
/// - `X86_64` (index 0): x86-64 / AMD64
/// - `Aarch64` (index 1): ARM64 / AArch64
///
/// Used with `@target_arch()` intrinsic for platform-specific code.
pub static ARCH_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "Arch",
    variants: &["X86_64", "Aarch64"],
};

/// The built-in Os enum for operating system detection.
///
/// Variants:
/// - `Linux` (index 0): Linux
/// - `Macos` (index 1): macOS / Darwin
///
/// Used with `@target_os()` intrinsic for platform-specific code.
pub static OS_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "Os",
    variants: &["Linux", "Macos"],
};

/// All built-in enums.
///
/// The compiler iterates over this to inject synthetic enums before
/// processing user code.
pub static BUILTIN_ENUMS: &[&BuiltinEnumDef] = &[&ARCH_ENUM, &OS_ENUM];

/// Look up a built-in enum by name.
pub fn get_builtin_enum(name: &str) -> Option<&'static BuiltinEnumDef> {
    BUILTIN_ENUMS.iter().find(|e| e.name == name).copied()
}

/// Check if a name is reserved for a built-in enum.
pub fn is_reserved_enum_name(name: &str) -> bool {
    BUILTIN_ENUMS.iter().any(|e| e.name == name)
}

/// Runtime symbols exported UNMANGLED outside the `__rue_` prefix, because
/// their exact names are load-bearing (RUE-354):
///
/// - `memcpy`/`memmove`/`memset`/`memcmp`/`bcmp` (`rue-runtime/src/memory.rs`):
///   rustc/LLVM lowers copies and comparisons to calls with exactly these
///   compiler-builtin names, so they cannot be moved under `__rue_`.
/// - `_start` / `_main` (`rue-runtime/src/entry.rs`): the program entry points
///   the linker resolves on Linux and macOS respectively.
///
/// Keep this list in sync with the `#[unsafe(no_mangle)]` items in those two
/// files — a missed name surfaces as a confusing duplicate-symbol link error
/// (E1000) instead of the intended E0435 declaration-time diagnostic.
const RUNTIME_EXPORTED_NAMES: &[&str] = &[
    "memcpy", "memmove", "memset", "memcmp", "bcmp", "_start", "_main",
];

/// Check if a name is reserved for a runtime/codegen helper function, and thus
/// may not be used as a user-defined function name.
///
/// A user function with one of these names would collide with a symbol emitted
/// by the runtime or codegen. Almost every such symbol lives under the reserved
/// `__rue_` prefix — built-in type methods and associated functions
/// (`__rue_String_len`, `__rue_String_new`), allocation/exit/drop glue
/// (`__rue_alloc`, `__rue_exit`, `__rue_drop_String`), and operator helpers
/// (`__rue_str_eq`) — but the runtime also exports a handful of unmangled
/// names whose spellings are fixed by the platform or by rustc/LLVM lowering:
/// see [`RUNTIME_EXPORTED_NAMES`]. Reserving exactly `__rue_*` plus that short
/// fixed list (rather than growing the set for every new builtin) means
/// `<BuiltinType>__<method>` spellings like `String__len` are ordinary, legal
/// user identifiers (RUE-125). Depending on link order a real collision either
/// fails to link or silently binds calls to the wrong definition, so these
/// names are rejected at declaration time with a clear diagnostic.
pub fn is_reserved_function_name(name: &str) -> bool {
    // Runtime internal helpers, built-in methods, drop glue, operators — every
    // compiler/runtime symbol not in RUNTIME_EXPORTED_NAMES is emitted under
    // this prefix.
    if name.starts_with("__rue_") {
        return true;
    }
    // Entry points and compiler-builtin memory routines the runtime exports
    // under their fixed platform names (RUE-354).
    RUNTIME_EXPORTED_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_reserved_function_name() {
        // Runtime helper prefix — allocation, exit, drop glue, and the renamed
        // built-in methods/associated functions all live under `__rue_`.
        assert!(is_reserved_function_name("__rue_alloc"));
        assert!(is_reserved_function_name("__rue_exit"));
        assert!(is_reserved_function_name("__rue_drop_String"));
        assert!(is_reserved_function_name("__rue_String_len"));
        assert!(is_reserved_function_name("__rue_String_new"));
        assert!(is_reserved_function_name("__rue_str_eq"));
        // Entry point.
        assert!(is_reserved_function_name("_start"));
        // Not reserved: ordinary user names. Crucially, the `<Type>__<method>`
        // spelling is now a legal user identifier — runtime symbols moved under
        // `__rue_` so the reserved set no longer grows per builtin (RUE-125).
        assert!(!is_reserved_function_name("String__len"));
        assert!(!is_reserved_function_name("String__new"));
        assert!(!is_reserved_function_name("main"));
        assert!(!is_reserved_function_name("my_len"));
        assert!(!is_reserved_function_name("foo__bar"));
        assert!(!is_reserved_function_name("Vec__push"));
        assert!(!is_reserved_function_name("_start_engine"));
        assert!(!is_reserved_function_name("rue_helper")); // no leading __

        // Unmangled runtime exports outside the __rue_ prefix (RUE-354):
        // compiler-builtin memory routines and the macOS entry point.
        for name in ["memcpy", "memmove", "memset", "memcmp", "bcmp", "_main"] {
            assert!(
                is_reserved_function_name(name),
                "runtime-exported symbol must be reserved: {}",
                name
            );
        }
        assert!(!is_reserved_function_name("memcpy2"));
        assert!(!is_reserved_function_name("my_memcpy"));
        assert!(!is_reserved_function_name("_main_loop"));
    }

    // ========================================================================
    // Built-in Enum Tests
    // ========================================================================

    #[test]
    fn test_arch_enum() {
        assert_eq!(ARCH_ENUM.name, "Arch");
        assert_eq!(ARCH_ENUM.variants.len(), 2);
        assert_eq!(ARCH_ENUM.variants[0], "X86_64");
        assert_eq!(ARCH_ENUM.variants[1], "Aarch64");
    }

    #[test]
    fn test_os_enum() {
        assert_eq!(OS_ENUM.name, "Os");
        assert_eq!(OS_ENUM.variants.len(), 2);
        assert_eq!(OS_ENUM.variants[0], "Linux");
        assert_eq!(OS_ENUM.variants[1], "Macos");
    }

    #[test]
    fn test_get_builtin_enum() {
        assert!(get_builtin_enum("Arch").is_some());
        assert!(get_builtin_enum("Os").is_some());
        assert!(get_builtin_enum("Target").is_none());
    }

    #[test]
    fn test_is_reserved_enum_name() {
        assert!(is_reserved_enum_name("Arch"));
        assert!(is_reserved_enum_name("Os"));
        assert!(!is_reserved_enum_name("MyEnum"));
    }

    #[test]
    fn test_builtin_enums_count() {
        assert_eq!(BUILTIN_ENUMS.len(), 2);
    }
}
