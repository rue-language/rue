//! Typed builtin/runtime mappings and compiler-provided enum definitions.
//!
//! Source-defined standard-library types use trusted language-item identity;
//! this crate does not inject or describe nominal struct types.

use rue_runtime_abi::{RuntimeHelper, RuntimeHelperId};

// ============================================================================
// Runtime operations that return or consume text values
// ============================================================================

/// The part of the text surface to which a builtin operation belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TextBuiltinCategory {
    Construction,
    Capacity,
    Mutation,
    Indexing,
    Iteration,
    Formatting,
    Io,
}

/// Where the canonical implementation of a text builtin lives.
///
/// Source-owned operations are ordinary methods in `std.strbuf` or functions
/// in `std.fmt`. Direct semantic operations construct AIR without crossing the
/// runtime ABI. Runtime operations carry the exhaustive typed helper identity;
/// their symbol and logical signature are obtained from `rue-runtime-abi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TextBuiltinDisposition {
    OwnedBySource(&'static str),
    DirectSema,
    RuntimeHelper(RuntimeHelperId),
}

impl TextBuiltinDisposition {
    /// Return the canonical runtime ABI entry, if this operation crosses the
    /// compiler/runtime boundary.
    const fn runtime_helper(self) -> Option<&'static RuntimeHelper> {
        match self {
            Self::RuntimeHelper(id) => Some(id.helper()),
            Self::OwnedBySource(_) | Self::DirectSema => None,
        }
    }
}

/// One closed inventory row for the currently supported `StrBuf` surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TextBuiltinMapping {
    operation: TextBuiltinOperation,
    category: TextBuiltinCategory,
    /// Source spelling or a short compiler-owned description. Runtime symbol
    /// names deliberately do not live here.
    surface: &'static str,
    disposition: TextBuiltinDisposition,
}

impl TextBuiltinMapping {
    /// Return the canonical helper and signature for a runtime-backed mapping.
    const fn runtime_helper(self) -> Option<&'static RuntimeHelper> {
        self.disposition.runtime_helper()
    }
}

macro_rules! text_builtin_inventory {
    (
        $(
            $variant:ident => {
                category: $category:ident,
                surface: $surface:literal,
                disposition: $disposition:expr
            }
        ),+ $(,)?
    ) => {
        /// Exhaustive typed identity for the compiler-owned `StrBuf` builtin
        /// surface. Additions must declare their ownership in this macro, so
        /// the inventory cannot acquire an unclassified row.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum TextBuiltinOperation {
            $($variant),+
        }

        impl TextBuiltinOperation {
            #[cfg(test)]
            const ALL: [Self; text_builtin_inventory!(@count $($variant),+)] = [
                $(Self::$variant),+
            ];

            const fn mapping(self) -> &'static TextBuiltinMapping {
                &TEXT_BUILTIN_MAPPINGS[self as usize]
            }

            /// Return this operation's canonical runtime helper and logical ABI
            /// signature, or `None` when source or semantic analysis owns it.
            pub const fn runtime_helper(self) -> Option<&'static RuntimeHelper> {
                self.mapping().runtime_helper()
            }
        }

        const TEXT_BUILTIN_MAPPINGS:
            [TextBuiltinMapping; text_builtin_inventory!(@count $($variant),+)] = [
                $(
                    TextBuiltinMapping {
                        operation: TextBuiltinOperation::$variant,
                        category: TextBuiltinCategory::$category,
                        surface: $surface,
                        disposition: $disposition,
                    }
                ),+
            ];
    };
    (@count $($variant:ident),+) => {
        <[()]>::len(&[$(text_builtin_inventory!(@replace $variant ())),+])
    };
    (@replace $_variant:ident $value:expr) => {
        $value
    };
}

// Current-source inventory for the String/StrBuf paths owned by builtin
// resolution and its directly lowered exceptions. Source-defined entries name
// their canonical source callable. Direct-sema entries have no external ABI.
// Runtime entries name only a RuntimeHelperId; symbol and signature facts stay
// canonical in rue-runtime-abi.
text_builtin_inventory! {
    New => {
        category: Construction,
        surface: "StrBuf.new",
        disposition: TextBuiltinDisposition::OwnedBySource("new")
    },
    WithCapacity => {
        category: Construction,
        surface: "StrBuf.with_capacity",
        disposition: TextBuiltinDisposition::OwnedBySource("with_capacity")
    },
    Allocate => {
        category: Construction,
        surface: "StrBuf.allocate",
        disposition: TextBuiltinDisposition::OwnedBySource("allocate")
    },
    Clone => {
        category: Construction,
        surface: "StrBuf.clone",
        disposition: TextBuiltinDisposition::OwnedBySource("clone")
    },
    Copy => {
        category: Construction,
        surface: "StrBuf.copy",
        disposition: TextBuiltinDisposition::OwnedBySource("copy")
    },
    Substring => {
        category: Construction,
        surface: "StrBuf.substring",
        disposition: TextBuiltinDisposition::OwnedBySource("substring")
    },
    ConcatMethod => {
        category: Construction,
        surface: "StrBuf.concat",
        disposition: TextBuiltinDisposition::OwnedBySource("concat")
    },
    ConcatOperator => {
        category: Construction,
        surface: "StrBuf + StrBuf",
        disposition: TextBuiltinDisposition::OwnedBySource("concat_borrowed")
    },
    Len => {
        category: Capacity,
        surface: "StrBuf.len",
        disposition: TextBuiltinDisposition::OwnedBySource("len")
    },
    Capacity => {
        category: Capacity,
        surface: "StrBuf.capacity",
        disposition: TextBuiltinDisposition::OwnedBySource("capacity")
    },
    IsEmpty => {
        category: Capacity,
        surface: "StrBuf.is_empty",
        disposition: TextBuiltinDisposition::OwnedBySource("is_empty")
    },
    InitialCapacity => {
        category: Capacity,
        surface: "StrBuf.initial_capacity",
        disposition: TextBuiltinDisposition::OwnedBySource("initial_capacity")
    },
    GrowthCapacity => {
        category: Capacity,
        surface: "StrBuf.growth_capacity",
        disposition: TextBuiltinDisposition::OwnedBySource("growth_capacity")
    },
    Reserve => {
        category: Capacity,
        surface: "StrBuf.reserve",
        disposition: TextBuiltinDisposition::OwnedBySource("reserve")
    },
    ReserveSource => {
        category: Capacity,
        surface: "StrBuf.reserve_source",
        disposition: TextBuiltinDisposition::OwnedBySource("reserve_source")
    },
    Grow => {
        category: Capacity,
        surface: "StrBuf.grow",
        disposition: TextBuiltinDisposition::OwnedBySource("grow")
    },
    Push => {
        category: Mutation,
        surface: "StrBuf.push",
        disposition: TextBuiltinDisposition::OwnedBySource("push")
    },
    AppendByte => {
        category: Mutation,
        surface: "StrBuf.append_byte",
        disposition: TextBuiltinDisposition::OwnedBySource("append_byte")
    },
    PushStr => {
        category: Mutation,
        surface: "StrBuf.push_str",
        disposition: TextBuiltinDisposition::OwnedBySource("push_str")
    },
    AppendOwned => {
        category: Mutation,
        surface: "StrBuf.append_owned",
        disposition: TextBuiltinDisposition::OwnedBySource("append_owned")
    },
    Clear => {
        category: Mutation,
        surface: "StrBuf.clear",
        disposition: TextBuiltinDisposition::OwnedBySource("clear")
    },
    Duplicate => {
        category: Mutation,
        surface: "StrBuf.duplicate",
        disposition: TextBuiltinDisposition::OwnedBySource("duplicate")
    },
    Free => {
        category: Mutation,
        surface: "StrBuf.free",
        disposition: TextBuiltinDisposition::OwnedBySource("free")
    },
    Drop => {
        category: Mutation,
        surface: "drop StrBuf",
        disposition: TextBuiltinDisposition::OwnedBySource("drop fn StrBuf")
    },
    ByteIndex => {
        category: Indexing,
        surface: "StrBuf[index]",
        disposition: TextBuiltinDisposition::OwnedBySource("byte_at_borrowed")
    },
    ByteRange => {
        category: Indexing,
        surface: "StrBuf.substring",
        disposition: TextBuiltinDisposition::OwnedBySource("byte_range")
    },
    Equality => {
        category: Indexing,
        surface: "StrBuf == StrBuf",
        disposition: TextBuiltinDisposition::OwnedBySource("equals_borrowed")
    },
    StartsWith => {
        category: Indexing,
        surface: "StrBuf.starts_with",
        disposition: TextBuiltinDisposition::OwnedBySource("starts_with")
    },
    Contains => {
        category: Indexing,
        surface: "StrBuf.contains",
        disposition: TextBuiltinDisposition::OwnedBySource("contains")
    },
    TextViewByteIndex => {
        category: Indexing,
        surface: "str/Str(N)[index]",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::StrByteAt)
    },
    TextViewEquality => {
        category: Indexing,
        surface: "str/Str(N) equality",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::StrEq)
    },
    ByteIteration => {
        category: Iteration,
        surface: "for byte in StrBuf",
        disposition: TextBuiltinDisposition::DirectSema
    },
    CharScalar => {
        category: Iteration,
        surface: "StrBuf.chars scalar",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::StrCharScalar)
    },
    CharNext => {
        category: Iteration,
        surface: "StrBuf.chars next",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::StrCharNext)
    },
    CharScalarLossy => {
        category: Iteration,
        surface: "StrBuf.chars_lossy scalar",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::StrCharScalarLossy)
    },
    CharNextLossy => {
        category: Iteration,
        surface: "StrBuf.chars_lossy next",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::StrCharNextLossy)
    },
    ToStringSigned => {
        category: Formatting,
        surface: "@to_string(signed)",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::ToString)
    },
    ToStringUnsigned => {
        category: Formatting,
        surface: "@to_string(unsigned)",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::ToStringUnsigned)
    },
    ToStringFloat => {
        category: Formatting,
        surface: "@to_string(float)",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::ToStringFloat)
    },

    IntToString => {
        category: Formatting,
        surface: "std.fmt.int_to_string",
        disposition: TextBuiltinDisposition::OwnedBySource("int_to_string")
    },
    BoolToString => {
        category: Formatting,
        surface: "std.fmt.bool_to_string",
        disposition: TextBuiltinDisposition::OwnedBySource("bool_to_string")
    },
    ToRadix => {
        category: Formatting,
        surface: "std.fmt.to_radix",
        disposition: TextBuiltinDisposition::OwnedBySource("to_radix")
    },
    ToHex => {
        category: Formatting,
        surface: "std.fmt.to_hex",
        disposition: TextBuiltinDisposition::OwnedBySource("to_hex")
    },
    ToBinary => {
        category: Formatting,
        surface: "std.fmt.to_binary",
        disposition: TextBuiltinDisposition::OwnedBySource("to_binary")
    },
    PrintView => {
        category: Io,
        surface: "print(StrBuf/str/Str(N))",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::StrPrint)
    },
    PrintlnView => {
        category: Io,
        surface: "println(StrBuf/str/Str(N))",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::StrPrintln)
    },
    DebugText => {
        category: Io,
        surface: "@dbg(text)",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::DebugStr)
    },
    ReadLine => {
        category: Io,
        surface: "@read_line",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::ReadLine)
    },
    ParseI32 => {
        category: Io,
        surface: "@parse_i32",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::ParseI32)
    },
    ParseI64 => {
        category: Io,
        surface: "@parse_i64",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::ParseI64)
    },
    ParseU32 => {
        category: Io,
        surface: "@parse_u32",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::ParseU32)
    },
    ParseU64 => {
        category: Io,
        surface: "@parse_u64",
        disposition: TextBuiltinDisposition::RuntimeHelper(RuntimeHelperId::ParseU64)
    }
}

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

/// The built-in DataModel enum for C data-model detection (ADR-0064
/// Amendment 1).
///
/// The data model names the widths a platform psABI assigns to C `int`,
/// `long`, and pointers. Architecture alone is insufficient (AAPCS64 is the
/// worked example), so it is a distinct target fact.
///
/// Variants (order fixed by `rue_target::DataModel`):
/// - `Ilp32` (index 0): `int`/`long`/pointer = 32
/// - `Lp64` (index 1): `int` = 32, `long`/pointer = 64 (all supported targets)
/// - `Llp64` (index 2): `int`/`long` = 32, `long long`/pointer = 64
///
/// Used with the `@target_data_model()` intrinsic for platform-specific code.
pub static DATA_MODEL_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "DataModel",
    variants: &["Ilp32", "Lp64", "Llp64"],
};

/// All built-in enums.
///
/// The compiler iterates over this to inject synthetic enums before
/// processing user code.
pub static BUILTIN_ENUMS: &[&BuiltinEnumDef] = &[&ARCH_ENUM, &OS_ENUM, &DATA_MODEL_ENUM];

/// Look up a built-in enum by name.
pub fn get_builtin_enum(name: &str) -> Option<&'static BuiltinEnumDef> {
    BUILTIN_ENUMS.iter().find(|e| e.name == name).copied()
}

/// Check if a name is reserved for a built-in enum.
pub fn is_reserved_enum_name(name: &str) -> bool {
    BUILTIN_ENUMS.iter().any(|e| e.name == name)
}

/// Check if a name is reserved for a runtime/codegen helper function, and thus
/// may not be used as a user-defined function name.
///
/// A user function with one of these names would collide with a symbol emitted
/// by the runtime or codegen. The reserved namespace covers runtime helpers,
/// compiler-generated functions, and target shims. The typed ABI manifest
/// separately classifies entry points and compiler-built memory routines whose
/// externally fixed names do not use that namespace. This means
/// `<BuiltinType>__<method>` spellings like `String__len` are ordinary, legal
/// user identifiers (RUE-125). Depending on link order a real collision either
/// fails to link or silently binds calls to the wrong definition, so these
/// names are rejected at declaration time with a clear diagnostic.
pub fn is_reserved_function_name(name: &str) -> bool {
    if name.starts_with("__rue_") {
        return true;
    }
    rue_runtime_abi::ReservedExportId::ALL
        .iter()
        .copied()
        .any(|id| {
            use rue_runtime_abi::ReservedExportClass;

            let export = id.export();
            matches!(
                export.class,
                ReservedExportClass::CompilerBuiltMemory | ReservedExportClass::ProgramEntry
            ) && export.symbol == name
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_runtime_abi::{AbiResult, AbiType, AggregateShapeId, ParameterMode};

    #[test]
    fn test_is_reserved_function_name() {
        // Runtime helper prefix.
        assert!(is_reserved_function_name("__rue_alloc"));
        assert!(is_reserved_function_name("__rue_exit"));
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
        assert!(!is_reserved_function_name("rue_darwin_sigtramp"));
    }

    #[test]
    fn text_builtin_inventory_is_exhaustive_and_index_stable() {
        assert_eq!(TextBuiltinOperation::ALL.len(), TEXT_BUILTIN_MAPPINGS.len());
        for (index, operation) in TextBuiltinOperation::ALL.iter().copied().enumerate() {
            assert_eq!(operation as usize, index);
            assert_eq!(operation.mapping().operation, operation);
            assert!(!operation.mapping().surface.is_empty());
        }
    }

    #[test]
    fn text_builtin_inventory_covers_every_required_area() {
        for category in [
            TextBuiltinCategory::Construction,
            TextBuiltinCategory::Capacity,
            TextBuiltinCategory::Mutation,
            TextBuiltinCategory::Indexing,
            TextBuiltinCategory::Iteration,
            TextBuiltinCategory::Formatting,
            TextBuiltinCategory::Io,
        ] {
            assert!(
                TEXT_BUILTIN_MAPPINGS
                    .iter()
                    .any(|mapping| mapping.category == category),
                "missing text builtin category: {category:?}"
            );
        }
        assert!(TEXT_BUILTIN_MAPPINGS.iter().any(|mapping| matches!(
            mapping.disposition,
            TextBuiltinDisposition::OwnedBySource(_)
        )));
        assert!(
            TEXT_BUILTIN_MAPPINGS
                .iter()
                .any(|mapping| matches!(mapping.disposition, TextBuiltinDisposition::DirectSema))
        );
        assert!(TEXT_BUILTIN_MAPPINGS.iter().any(|mapping| matches!(
            mapping.disposition,
            TextBuiltinDisposition::RuntimeHelper(_)
        )));
    }

    #[test]
    fn runtime_backed_text_builtins_obtain_identity_and_signature_from_manifest() {
        for mapping in TEXT_BUILTIN_MAPPINGS {
            let TextBuiltinDisposition::RuntimeHelper(id) = mapping.disposition else {
                assert!(mapping.runtime_helper().is_none());
                continue;
            };
            let helper = mapping
                .runtime_helper()
                .expect("runtime-backed builtin must resolve a manifest entry");
            assert_eq!(helper.id, id);
            assert_eq!(RuntimeHelperId::from_symbol(helper.symbol), Some(id));
        }

        let signed = TextBuiltinOperation::ToStringSigned
            .runtime_helper()
            .unwrap();
        assert_eq!(
            signed.parameters[0].mode,
            ParameterMode::OutPointer(AggregateShapeId::StrBufResult)
        );
        assert_eq!(signed.parameters[1].ty, AbiType::I64);
        assert_eq!(signed.result, AbiResult::Void);

        let unsigned = TextBuiltinOperation::ToStringUnsigned
            .runtime_helper()
            .unwrap();
        assert_eq!(
            unsigned.parameters[0].mode,
            ParameterMode::OutPointer(AggregateShapeId::StrBufResult)
        );
        assert_eq!(unsigned.parameters[1].ty, AbiType::U64);

        for operation in [
            TextBuiltinOperation::PrintView,
            TextBuiltinOperation::PrintlnView,
        ] {
            let helper = operation.runtime_helper().unwrap();
            assert_eq!(helper.parameters.len(), 2);
            assert_eq!(helper.result, AbiResult::Void);
        }
        assert_eq!(
            TextBuiltinOperation::PrintView.runtime_helper().unwrap().id,
            RuntimeHelperId::StrPrint
        );
        assert_eq!(
            TextBuiltinOperation::PrintlnView
                .runtime_helper()
                .unwrap()
                .id,
            RuntimeHelperId::StrPrintln
        );
        assert!(!TEXT_BUILTIN_MAPPINGS.iter().any(|mapping| matches!(
            mapping.disposition,
            TextBuiltinDisposition::RuntimeHelper(
                RuntimeHelperId::Print | RuntimeHelperId::Println
            )
        )));

        let byte_at = TextBuiltinOperation::TextViewByteIndex
            .runtime_helper()
            .unwrap();
        assert_eq!(byte_at.id, RuntimeHelperId::StrByteAt);
        assert_eq!(byte_at.parameters.len(), 3);
        assert_eq!(byte_at.parameters[0].mode, ParameterMode::ConstPointer);
        assert_eq!(byte_at.parameters[1].ty, AbiType::U64);
        assert_eq!(byte_at.parameters[2].ty, AbiType::U64);
        assert_eq!(byte_at.result, AbiResult::Scalar(AbiType::U64));

        let equality = TextBuiltinOperation::TextViewEquality
            .runtime_helper()
            .unwrap();
        assert_eq!(equality.id, RuntimeHelperId::StrEq);
        assert_eq!(equality.parameters.len(), 4);
        assert_eq!(equality.parameters[0].mode, ParameterMode::ConstPointer);
        assert_eq!(equality.parameters[1].ty, AbiType::U64);
        assert_eq!(equality.parameters[2].mode, ParameterMode::ConstPointer);
        assert_eq!(equality.parameters[3].ty, AbiType::U64);
        assert_eq!(equality.result, AbiResult::Scalar(AbiType::U64));
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
    fn test_data_model_enum() {
        assert_eq!(DATA_MODEL_ENUM.name, "DataModel");
        assert_eq!(DATA_MODEL_ENUM.variants.len(), 3);
        assert_eq!(DATA_MODEL_ENUM.variants[0], "Ilp32");
        assert_eq!(DATA_MODEL_ENUM.variants[1], "Lp64");
        assert_eq!(DATA_MODEL_ENUM.variants[2], "Llp64");
    }

    #[test]
    fn test_get_builtin_enum() {
        assert!(get_builtin_enum("Arch").is_some());
        assert!(get_builtin_enum("Os").is_some());
        assert!(get_builtin_enum("DataModel").is_some());
        assert!(get_builtin_enum("Target").is_none());
    }

    #[test]
    fn test_is_reserved_enum_name() {
        assert!(is_reserved_enum_name("Arch"));
        assert!(is_reserved_enum_name("Os"));
        assert!(is_reserved_enum_name("DataModel"));
        assert!(!is_reserved_enum_name("MyEnum"));
    }

    #[test]
    fn test_builtin_enums_count() {
        assert_eq!(BUILTIN_ENUMS.len(), 3);
    }
}
