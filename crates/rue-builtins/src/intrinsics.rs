//! The canonical inventory of compiler intrinsic spellings.
//!
//! Every phase that needs to know an intrinsic by name reads it from here:
//! the pre-interned semantic symbol table, the AIR operation's diagnostic
//! spelling, canonical RIR packing's fallible-intrinsic projection, the
//! toolchain `Option(payload)` demand derived from that projection, and
//! declaration-time comptime classification. Matching an intrinsic spelling
//! against a string literal anywhere else would reintroduce a second table
//! that nothing checks against this one, so [`IntrinsicName::from_spelling`]
//! is the only place a spelling is compared.
//!
//! The crate is a leaf, so both `rue-rir` (which packs the fallible subset
//! into its cache format) and `rue-air` (which owns intrinsic semantics) can
//! consume one table rather than agreeing by convention.

/// The argument grammar an intrinsic's argument positions take.
///
/// Most intrinsics take value expressions. A few take types, and the parser
/// and AstGen must agree with semantic analysis about which, so the fact is
/// stated once here and consumed by every phase that classifies an argument
/// list (RUE-788).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicArgumentGrammar {
    /// Ordinary value-expression arguments.
    Value,
    /// Every argument position is parsed with the canonical type grammar,
    /// as in `@size_of(T)`.
    Type,
    /// A leading type argument followed by an ordinary expression naming a
    /// field, as in `@offset_of(T, field)`.
    TypeThenField,
}

/// The payload of the trusted standard-library `Option(payload)` a fallible
/// intrinsic's result requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntrinsicFalliblePayload {
    I32,
    I64,
    U32,
    U64,
    StrBuf,
}

/// The typed projection of [`IntrinsicName`] onto the intrinsics whose result
/// is a trusted `Option(payload)`.
///
/// A body that states one of these demands the matching `Option` instantiation
/// from the toolchain, so the payload is part of the intrinsic's identity
/// rather than a fact a later phase rediscovers. Because [`Self::payload`] is
/// an exhaustive match, a new fallible intrinsic cannot reach semantic
/// analysis without declaring the `Option` prerequisite its body needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FallibleIntrinsic {
    ParseI32,
    ParseI64,
    ParseU32,
    ParseU64,
    ReadLine,
}

impl FallibleIntrinsic {
    /// Every fallible intrinsic, in the order consumers iterate them.
    pub const ALL: [Self; 5] = [
        Self::ParseI32,
        Self::ParseI64,
        Self::ParseU32,
        Self::ParseU64,
        Self::ReadLine,
    ];

    /// The spelling-table row this projection selects.
    pub const fn name(self) -> IntrinsicName {
        match self {
            Self::ParseI32 => IntrinsicName::ParseI32,
            Self::ParseI64 => IntrinsicName::ParseI64,
            Self::ParseU32 => IntrinsicName::ParseU32,
            Self::ParseU64 => IntrinsicName::ParseU64,
            Self::ReadLine => IntrinsicName::ReadLine,
        }
    }

    /// The `Option` payload this intrinsic's result carries.
    pub const fn payload(self) -> IntrinsicFalliblePayload {
        match self {
            Self::ParseI32 => IntrinsicFalliblePayload::I32,
            Self::ParseI64 => IntrinsicFalliblePayload::I64,
            Self::ParseU32 => IntrinsicFalliblePayload::U32,
            Self::ParseU64 => IntrinsicFalliblePayload::U64,
            Self::ReadLine => IntrinsicFalliblePayload::StrBuf,
        }
    }

    /// The canonical source spelling, taken from the one spelling table.
    pub const fn spelling(self) -> &'static str {
        self.name().spelling()
    }
}

/// One closed row of the spelling inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IntrinsicRow {
    name: IntrinsicName,
    spelling: &'static str,
    grammar: IntrinsicArgumentGrammar,
    fallible: Option<FallibleIntrinsic>,
}

macro_rules! intrinsic_inventory {
    (
        $(
            $variant:ident => {
                spelling: $spelling:literal,
                grammar: $grammar:ident,
                fallible: $fallible:expr
            }
        ),+ $(,)?
    ) => {
        /// The exhaustive typed identity of an intrinsic spelling.
        ///
        /// Consumers match on this rather than on a string, so adding a row
        /// is a non-exhaustive-match error in every phase that classifies
        /// intrinsics instead of a silent omission.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u8)]
        pub enum IntrinsicName {
            $($variant),+
        }

        impl IntrinsicName {
            /// Every intrinsic, in inventory order.
            pub const ALL: [Self; intrinsic_inventory!(@count $($variant),+)] = [
                $(Self::$variant),+
            ];

            const fn row(self) -> &'static IntrinsicRow {
                &INTRINSIC_ROWS[self as usize]
            }

            /// The canonical source spelling, without its `@` sigil.
            pub const fn spelling(self) -> &'static str {
                self.row().spelling
            }

            /// The argument grammar this intrinsic's arguments are parsed with.
            pub const fn argument_grammar(self) -> IntrinsicArgumentGrammar {
                self.row().grammar
            }

            /// The fallible projection of this row, when its result is a
            /// trusted `Option(payload)`.
            pub const fn fallible(self) -> Option<FallibleIntrinsic> {
                self.row().fallible
            }

            /// Classify a source spelling. This is the only place in the
            /// compiler where an intrinsic name is compared against text.
            pub fn from_spelling(spelling: &str) -> Option<Self> {
                Some(match spelling {
                    $($spelling => Self::$variant,)+
                    _ => return None,
                })
            }
        }

        const INTRINSIC_ROWS:
            [IntrinsicRow; intrinsic_inventory!(@count $($variant),+)] = [
                $(
                    IntrinsicRow {
                        name: IntrinsicName::$variant,
                        spelling: $spelling,
                        grammar: IntrinsicArgumentGrammar::$grammar,
                        fallible: $fallible,
                    }
                ),+
            ];
    };
    (@count $($variant:ident),+) => {
        <[()]>::len(&[$(intrinsic_inventory!(@replace $variant ())),+])
    };
    (@replace $_variant:ident $value:expr) => {
        $value
    };
}

intrinsic_inventory! {
    Dbg => { spelling: "dbg", grammar: Value, fallible: None },
    Drop => { spelling: "drop", grammar: Value, fallible: None },
    IntCast => { spelling: "intCast", grammar: Value, fallible: None },
    BitCast => { spelling: "bitCast", grammar: Value, fallible: None },
    Cast => { spelling: "cast", grammar: Value, fallible: None },
    IntToFloat => { spelling: "int_to_float", grammar: Value, fallible: None },
    FloatToInt => { spelling: "float_to_int", grammar: Value, fallible: None },
    FloatCast => { spelling: "float_cast", grammar: Value, fallible: None },
    TotalCmp => { spelling: "total_cmp", grammar: Value, fallible: None },
    Sqrt => { spelling: "sqrt", grammar: Value, fallible: None },
    Floor => { spelling: "floor", grammar: Value, fallible: None },
    Ceil => { spelling: "ceil", grammar: Value, fallible: None },
    Trunc => { spelling: "trunc", grammar: Value, fallible: None },
    Round => { spelling: "round", grammar: Value, fallible: None },
    Panic => { spelling: "panic", grammar: Value, fallible: None },
    Assert => { spelling: "assert", grammar: Value, fallible: None },
    AssertEq => { spelling: "assert_eq", grammar: Value, fallible: None },
    AssertNe => { spelling: "assert_ne", grammar: Value, fallible: None },
    ReadLine => {
        spelling: "read_line",
        grammar: Value,
        fallible: Some(FallibleIntrinsic::ReadLine)
    },
    ToString => { spelling: "to_string", grammar: Value, fallible: None },
    ParseI32 => {
        spelling: "parse_i32",
        grammar: Value,
        fallible: Some(FallibleIntrinsic::ParseI32)
    },
    ParseI64 => {
        spelling: "parse_i64",
        grammar: Value,
        fallible: Some(FallibleIntrinsic::ParseI64)
    },
    ParseU32 => {
        spelling: "parse_u32",
        grammar: Value,
        fallible: Some(FallibleIntrinsic::ParseU32)
    },
    ParseU64 => {
        spelling: "parse_u64",
        grammar: Value,
        fallible: Some(FallibleIntrinsic::ParseU64)
    },
    TestPreviewGate => { spelling: "test_preview_gate", grammar: Value, fallible: None },
    Import => { spelling: "import", grammar: Value, fallible: None },
    RandomU32 => { spelling: "random_u32", grammar: Value, fallible: None },
    RandomU64 => { spelling: "random_u64", grammar: Value, fallible: None },
    ArgCount => { spelling: "arg_count", grammar: Value, fallible: None },
    ArgPtr => { spelling: "arg_ptr", grammar: Value, fallible: None },
    ArgLen => { spelling: "arg_len", grammar: Value, fallible: None },
    EnvCount => { spelling: "env_count", grammar: Value, fallible: None },
    EnvPtr => { spelling: "env_ptr", grammar: Value, fallible: None },
    EnvLen => { spelling: "env_len", grammar: Value, fallible: None },
    WrappingAdd => { spelling: "wrapping_add", grammar: Value, fallible: None },
    WrappingSub => { spelling: "wrapping_sub", grammar: Value, fallible: None },
    WrappingMul => { spelling: "wrapping_mul", grammar: Value, fallible: None },
    PtrRead => { spelling: "ptr_read", grammar: Value, fallible: None },
    PtrWrite => { spelling: "ptr_write", grammar: Value, fallible: None },
    PtrReadUnaligned => { spelling: "ptr_read_unaligned", grammar: Value, fallible: None },
    PtrWriteUnaligned => { spelling: "ptr_write_unaligned", grammar: Value, fallible: None },
    PtrOffset => { spelling: "ptr_offset", grammar: Value, fallible: None },
    PtrToInt => { spelling: "ptr_to_int", grammar: Value, fallible: None },
    IntToPtr => { spelling: "int_to_ptr", grammar: Value, fallible: None },
    Raw => { spelling: "raw", grammar: Value, fallible: None },
    RawMut => { spelling: "raw_mut", grammar: Value, fallible: None },
    FieldPtr => { spelling: "field_ptr", grammar: Value, fallible: None },
    Place => { spelling: "place", grammar: Value, fallible: None },
    Syscall => { spelling: "syscall", grammar: Value, fallible: None },
    Alloc => { spelling: "alloc", grammar: Value, fallible: None },
    AllocZeroed => { spelling: "alloc_zeroed", grammar: Value, fallible: None },
    Free => { spelling: "free", grammar: Value, fallible: None },
    Realloc => { spelling: "realloc", grammar: Value, fallible: None },
    Resize => { spelling: "resize", grammar: Value, fallible: None },
    ByteCopy => { spelling: "byte_copy", grammar: Value, fallible: None },
    ByteMove => { spelling: "byte_move", grammar: Value, fallible: None },
    ByteSet => { spelling: "byte_set", grammar: Value, fallible: None },
    TargetArch => { spelling: "target_arch", grammar: Value, fallible: None },
    TargetOs => { spelling: "target_os", grammar: Value, fallible: None },
    TargetDataModel => { spelling: "target_data_model", grammar: Value, fallible: None },
    SizeOf => { spelling: "size_of", grammar: Type, fallible: None },
    AlignOf => { spelling: "align_of", grammar: Type, fallible: None },
    RequireDroppable => { spelling: "require_droppable", grammar: Type, fallible: None },
    RequireTriviallyDroppable => {
        spelling: "require_trivially_droppable",
        grammar: Type,
        fallible: None
    },
    IntMax => { spelling: "int_max", grammar: Type, fallible: None },
    IntMin => { spelling: "int_min", grammar: Type, fallible: None },
    OffsetOf => { spelling: "offset_of", grammar: TypeThenField, fallible: None },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn rows_are_indexed_by_their_own_variant() {
        for (index, name) in IntrinsicName::ALL.into_iter().enumerate() {
            assert_eq!(name as usize, index, "{name:?} is out of inventory order");
            assert_eq!(name.row().name, name, "{name:?} reads a foreign row");
        }
    }

    #[test]
    fn spellings_are_unique_and_round_trip() {
        let mut seen = BTreeSet::new();
        for name in IntrinsicName::ALL {
            let spelling = name.spelling();
            assert!(seen.insert(spelling), "duplicate spelling `{spelling}`");
            assert_eq!(IntrinsicName::from_spelling(spelling), Some(name));
        }
        assert_eq!(IntrinsicName::from_spelling("not_an_intrinsic"), None);
        assert_eq!(IntrinsicName::from_spelling(""), None);
    }

    #[test]
    fn fallible_projection_is_a_bijection_onto_its_rows() {
        let declared: BTreeSet<_> = IntrinsicName::ALL
            .into_iter()
            .filter_map(IntrinsicName::fallible)
            .collect();
        assert_eq!(
            declared,
            FallibleIntrinsic::ALL.into_iter().collect::<BTreeSet<_>>(),
            "every fallible intrinsic must select exactly one inventory row"
        );
        let mut payloads = BTreeSet::new();
        for fallible in FallibleIntrinsic::ALL {
            assert_eq!(fallible.name().fallible(), Some(fallible));
            assert_eq!(fallible.spelling(), fallible.name().spelling());
            assert!(
                payloads.insert(fallible.payload()),
                "{fallible:?} shares a payload with another fallible intrinsic"
            );
        }
    }

    #[test]
    fn type_grammar_rows_are_the_type_position_intrinsics() {
        let type_grammar: BTreeSet<_> = IntrinsicName::ALL
            .into_iter()
            .filter(|name| name.argument_grammar() == IntrinsicArgumentGrammar::Type)
            .map(IntrinsicName::spelling)
            .collect();
        assert_eq!(
            type_grammar,
            BTreeSet::from([
                "size_of",
                "align_of",
                "require_droppable",
                "require_trivially_droppable",
                "int_max",
                "int_min",
            ])
        );
        let mixed: Vec<_> = IntrinsicName::ALL
            .into_iter()
            .filter(|name| name.argument_grammar() == IntrinsicArgumentGrammar::TypeThenField)
            .collect();
        assert_eq!(mixed, vec![IntrinsicName::OffsetOf]);
    }
}
