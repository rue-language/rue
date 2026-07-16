#![no_std]

//! Canonical typed description of the Rue compiler/runtime ABI.
//!
//! This crate deliberately has no dependencies. It describes logical C-boundary
//! facts; target register assignment and compiler semantic types belong to
//! their respective owners.

use core::fmt;

/// Current lockstep compiler/runtime ABI version.
pub const RUNTIME_ABI_VERSION: u32 = 1;

/// Retained data symbol exported by each runtime archive for this ABI version.
pub const RUNTIME_ABI_VERSION_SYMBOL: &str = "__rue_runtime_abi_v1";

/// A physical scalar type at the C boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AbiType {
    I32,
    I64,
    U32,
    U64,
    /// The current ABI's canonical boolean representation.
    BoolWordI64,
    /// A single opaque byte, used as a pointer pointee.
    Byte,
    /// A mutable opaque byte pointer returned in a scalar result or aggregate slot.
    MutBytePointer,
    /// The target C ABI's `usize`, used only by compiler-built memory routines.
    Usize,
}

/// How a parameter crosses the C boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParameterMode {
    Value,
    ConstPointer,
    MutPointer,
    OutPointer(AggregateShapeId),
}

/// One ordered function parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AbiParameter {
    pub ty: AbiType,
    pub mode: ParameterMode,
}

impl AbiParameter {
    pub const fn value(ty: AbiType) -> Self {
        Self {
            ty,
            mode: ParameterMode::Value,
        }
    }

    pub const fn const_pointer(ty: AbiType) -> Self {
        Self {
            ty,
            mode: ParameterMode::ConstPointer,
        }
    }

    pub const fn mut_pointer(ty: AbiType) -> Self {
        Self {
            ty,
            mode: ParameterMode::MutPointer,
        }
    }

    pub const fn out_pointer(shape: AggregateShapeId) -> Self {
        Self {
            ty: AbiType::Byte,
            mode: ParameterMode::OutPointer(shape),
        }
    }
}

/// A function's direct C result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AbiResult {
    Void,
    Scalar(AbiType),
}

/// Whether control can continue after a helper call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReturnBehavior {
    Returns,
    Never,
}

/// Calling-convention family. Physical register assignment remains target-owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallingConvention {
    TargetC,
}

/// Supported runtime targets, represented without depending on `rue-target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeTarget {
    X86_64Linux,
    Aarch64Linux,
    Aarch64Macos,
}

/// A composable set of runtime targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetSet(u8);

impl TargetSet {
    pub const X86_64_LINUX: Self = Self(1 << 0);
    pub const AARCH64_LINUX: Self = Self(1 << 1);
    pub const AARCH64_MACOS: Self = Self(1 << 2);
    pub const ALL: Self =
        Self(Self::X86_64_LINUX.0 | Self::AARCH64_LINUX.0 | Self::AARCH64_MACOS.0);

    pub const fn contains(self, target: RuntimeTarget) -> bool {
        let bit = match target {
            RuntimeTarget::X86_64Linux => Self::X86_64_LINUX.0,
            RuntimeTarget::Aarch64Linux => Self::AARCH64_LINUX.0,
            RuntimeTarget::Aarch64Macos => Self::AARCH64_MACOS.0,
        };
        self.0 & bit != 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Composable caller obligations and control-flow properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SafetyContract(u16);

impl SafetyContract {
    pub const NONE: Self = Self(0);
    /// A pointer/length pair must describe readable bytes.
    pub const READABLE_BYTES: Self = Self(1 << 0);
    /// An out pointer must address aligned, writable aggregate storage.
    pub const WRITABLE_RESULT: Self = Self(1 << 1);
    /// Byte counts and alignment must describe a supported allocation layout.
    pub const ALLOCATION_LAYOUT: Self = Self(1 << 2);
    /// A mutable pointer must denote the allocation described by the other arguments.
    pub const VALID_ALLOCATION: Self = Self(1 << 3);
    /// Caller-supplied enum discriminants must be concrete and distinct.
    pub const CONCRETE_DISCRIMINANTS: Self = Self(1 << 4);
    /// The operation unconditionally traps or terminates the process.
    pub const TERMINATES: Self = Self(1 << 5);

    pub const fn contains(self, requirement: Self) -> bool {
        self.0 & requirement.0 == requirement.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Named aggregate layouts written through explicit out pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AggregateShapeId {
    StrBufResult,
    OptionStrBufResult,
    OptionIntResult,
}

impl AggregateShapeId {
    pub const ALL: [Self; 3] = [
        Self::StrBufResult,
        Self::OptionStrBufResult,
        Self::OptionIntResult,
    ];

    pub const fn shape(self) -> &'static AggregateShape {
        &AGGREGATE_SHAPES[self as usize]
    }
}

/// One aggregate slot in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AggregateSlot {
    pub name: &'static str,
    pub ty: AbiType,
}

/// A named C-boundary aggregate shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AggregateShape {
    pub id: AggregateShapeId,
    pub name: &'static str,
    pub slots: &'static [AggregateSlot],
}

const STR_BUF_SLOTS: &[AggregateSlot] = &[
    AggregateSlot {
        name: "ptr",
        ty: AbiType::MutBytePointer,
    },
    AggregateSlot {
        name: "len",
        ty: AbiType::U64,
    },
    AggregateSlot {
        name: "cap",
        ty: AbiType::U64,
    },
];

const OPTION_STR_BUF_SLOTS: &[AggregateSlot] = &[
    AggregateSlot {
        name: "discriminant",
        ty: AbiType::U64,
    },
    AggregateSlot {
        name: "ptr",
        ty: AbiType::MutBytePointer,
    },
    AggregateSlot {
        name: "len",
        ty: AbiType::U64,
    },
    AggregateSlot {
        name: "cap",
        ty: AbiType::U64,
    },
];

const OPTION_INT_SLOTS: &[AggregateSlot] = &[
    AggregateSlot {
        name: "discriminant",
        ty: AbiType::U64,
    },
    AggregateSlot {
        name: "value",
        ty: AbiType::U64,
    },
];

pub const AGGREGATE_SHAPES: [AggregateShape; 3] = [
    AggregateShape {
        id: AggregateShapeId::StrBufResult,
        name: "StrBufResult",
        slots: STR_BUF_SLOTS,
    },
    AggregateShape {
        id: AggregateShapeId::OptionStrBufResult,
        name: "OptionStrBufResult",
        slots: OPTION_STR_BUF_SLOTS,
    },
    AggregateShape {
        id: AggregateShapeId::OptionIntResult,
        name: "OptionIntResult",
        slots: OPTION_INT_SLOTS,
    },
];

macro_rules! params {
    () => {
        &[]
    };
    ($($parameter:expr),+ $(,)?) => {
        &[$($parameter),+]
    };
}

const I32_VALUE: AbiParameter = AbiParameter::value(AbiType::I32);
const I64_VALUE: AbiParameter = AbiParameter::value(AbiType::I64);
const U64_VALUE: AbiParameter = AbiParameter::value(AbiType::U64);
const BOOL_WORD_VALUE: AbiParameter = AbiParameter::value(AbiType::BoolWordI64);
const BYTE_VIEW: AbiParameter = AbiParameter::const_pointer(AbiType::Byte);
const MUT_BYTE_POINTER: AbiParameter = AbiParameter::mut_pointer(AbiType::Byte);
const STR_BUF_OUT: AbiParameter = AbiParameter::out_pointer(AggregateShapeId::StrBufResult);
const OPTION_STR_BUF_OUT: AbiParameter =
    AbiParameter::out_pointer(AggregateShapeId::OptionStrBufResult);
const OPTION_INT_OUT: AbiParameter = AbiParameter::out_pointer(AggregateShapeId::OptionIntResult);

const RETURNS: ReturnBehavior = ReturnBehavior::Returns;
const NEVER: ReturnBehavior = ReturnBehavior::Never;
const VOID: AbiResult = AbiResult::Void;
const U32_RESULT: AbiResult = AbiResult::Scalar(AbiType::U32);
const U64_RESULT: AbiResult = AbiResult::Scalar(AbiType::U64);
const MUT_POINTER_RESULT: AbiResult = AbiResult::Scalar(AbiType::MutBytePointer);
const TARGET_C: CallingConvention = CallingConvention::TargetC;
const ALL_TARGETS: TargetSet = TargetSet::ALL;

const READABLE: SafetyContract = SafetyContract::READABLE_BYTES;
const WRITABLE: SafetyContract = SafetyContract::WRITABLE_RESULT;
const TERMINATES: SafetyContract = SafetyContract::TERMINATES;
const ALLOC_LAYOUT: SafetyContract = SafetyContract::ALLOCATION_LAYOUT;
const VALID_ALLOC_LAYOUT: SafetyContract =
    SafetyContract::VALID_ALLOCATION.union(SafetyContract::ALLOCATION_LAYOUT);
const WRITABLE_DISCRIMINANTS: SafetyContract =
    SafetyContract::WRITABLE_RESULT.union(SafetyContract::CONCRETE_DISCRIMINANTS);
const READABLE_WRITABLE_DISCRIMINANTS: SafetyContract =
    SafetyContract::READABLE_BYTES.union(WRITABLE_DISCRIMINANTS);

macro_rules! runtime_helpers {
    (
        $(
            $variant:ident => {
                symbol: $symbol:literal,
                parameters: $parameters:expr,
                result: $result:expr,
                safety: $safety:expr,
                returns: $returns:expr
            }
        ),+ $(,)?
    ) => {
        /// Exhaustive typed identity for compiler-callable runtime helpers.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum RuntimeHelperId {
            $($variant),+
        }

        impl RuntimeHelperId {
            pub const ALL: [Self; runtime_helpers!(@count $($variant),+)] = [
                $(Self::$variant),+
            ];

            pub const fn helper(self) -> &'static RuntimeHelper {
                &RUNTIME_HELPERS[self as usize]
            }

            pub const fn symbol(self) -> &'static str {
                self.helper().symbol
            }

            pub fn from_symbol(symbol: &str) -> Option<Self> {
                let mut index = 0;
                while index < Self::ALL.len() {
                    let id = Self::ALL[index];
                    if string_eq(id.symbol(), symbol) {
                        return Some(id);
                    }
                    index += 1;
                }
                None
            }
        }

        impl fmt::Display for RuntimeHelperId {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.symbol())
            }
        }

        pub const RUNTIME_HELPERS: [RuntimeHelper; runtime_helpers!(@count $($variant),+)] = [
            $(
                RuntimeHelper {
                    id: RuntimeHelperId::$variant,
                    symbol: $symbol,
                    parameters: $parameters,
                    result: $result,
                    safety: $safety,
                    return_behavior: $returns,
                    availability: ALL_TARGETS,
                    calling_convention: TARGET_C,
                }
            ),+
        ];
    };
    (@count $($variant:ident),+) => {
        <[()]>::len(&[$(runtime_helpers!(@replace $variant ())),+])
    };
    (@replace $_variant:ident $value:expr) => {
        $value
    };
}

/// Complete logical signature and contract for one runtime helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuntimeHelper {
    pub id: RuntimeHelperId,
    pub symbol: &'static str,
    pub parameters: &'static [AbiParameter],
    pub result: AbiResult,
    pub safety: SafetyContract,
    pub return_behavior: ReturnBehavior,
    pub availability: TargetSet,
    pub calling_convention: CallingConvention,
}

runtime_helpers! {
    Exit => {
        symbol: "__rue_exit",
        parameters: params![I32_VALUE],
        result: VOID,
        safety: TERMINATES,
        returns: NEVER
    },
    Alloc => {
        symbol: "__rue_alloc",
        parameters: params![U64_VALUE, U64_VALUE],
        result: MUT_POINTER_RESULT,
        safety: ALLOC_LAYOUT,
        returns: RETURNS
    },
    Free => {
        symbol: "__rue_free",
        parameters: params![MUT_BYTE_POINTER, U64_VALUE, U64_VALUE],
        result: VOID,
        // The current bump allocator's free operation is a no-op and imposes
        // no pointer or layout precondition.
        safety: SafetyContract::NONE,
        returns: RETURNS
    },
    Realloc => {
        symbol: "__rue_realloc",
        parameters: params![MUT_BYTE_POINTER, U64_VALUE, U64_VALUE, U64_VALUE],
        result: MUT_POINTER_RESULT,
        safety: VALID_ALLOC_LAYOUT,
        returns: RETURNS
    },
    DivByZero => {
        symbol: "__rue_div_by_zero",
        parameters: params![],
        result: VOID,
        safety: TERMINATES,
        returns: NEVER
    },
    Overflow => {
        symbol: "__rue_overflow",
        parameters: params![],
        result: VOID,
        safety: TERMINATES,
        returns: NEVER
    },
    IntcastOverflow => {
        symbol: "__rue_intcast_overflow",
        parameters: params![],
        result: VOID,
        safety: TERMINATES,
        returns: NEVER
    },
    BoundsCheck => {
        symbol: "__rue_bounds_check",
        parameters: params![],
        result: VOID,
        safety: TERMINATES,
        returns: NEVER
    },
    Panic => {
        symbol: "__rue_panic",
        parameters: params![BYTE_VIEW, U64_VALUE],
        result: VOID,
        safety: READABLE.union(TERMINATES),
        returns: NEVER
    },
    PanicNoMessage => {
        symbol: "__rue_panic_no_msg",
        parameters: params![],
        result: VOID,
        safety: TERMINATES,
        returns: NEVER
    },
    AssertFailed => {
        symbol: "__rue_assert_failed",
        parameters: params![],
        result: VOID,
        safety: TERMINATES,
        returns: NEVER
    },
    DebugI64 => {
        symbol: "__rue_dbg_i64",
        parameters: params![I64_VALUE],
        result: VOID,
        safety: SafetyContract::NONE,
        returns: RETURNS
    },
    DebugU64 => {
        symbol: "__rue_dbg_u64",
        parameters: params![U64_VALUE],
        result: VOID,
        safety: SafetyContract::NONE,
        returns: RETURNS
    },
    DebugBool => {
        symbol: "__rue_dbg_bool",
        parameters: params![BOOL_WORD_VALUE],
        result: VOID,
        safety: SafetyContract::NONE,
        returns: RETURNS
    },
    DebugStr => {
        symbol: "__rue_dbg_str",
        parameters: params![BYTE_VIEW, U64_VALUE],
        result: VOID,
        safety: READABLE,
        returns: RETURNS
    },
    StrEq => {
        symbol: "__rue_str_eq",
        parameters: params![BYTE_VIEW, U64_VALUE, BYTE_VIEW, U64_VALUE],
        result: U64_RESULT,
        safety: READABLE,
        returns: RETURNS
    },
    StrByteAt => {
        symbol: "__rue_str_byte_at",
        parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
        result: U64_RESULT,
        safety: READABLE,
        returns: RETURNS
    },
    StrCharScalar => {
        symbol: "__rue_str_char_scalar",
        parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
        result: U64_RESULT,
        safety: READABLE,
        returns: RETURNS
    },
    StrCharNext => {
        symbol: "__rue_str_char_next",
        parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
        result: U64_RESULT,
        safety: READABLE,
        returns: RETURNS
    },
    StrCharScalarLossy => {
        symbol: "__rue_str_char_scalar_lossy",
        parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
        result: U64_RESULT,
        safety: READABLE,
        returns: RETURNS
    },
    StrCharNextLossy => {
        symbol: "__rue_str_char_next_lossy",
        parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
        result: U64_RESULT,
        safety: READABLE,
        returns: RETURNS
    },
    ToString => {
        symbol: "__rue_to_string",
        parameters: params![STR_BUF_OUT, I64_VALUE],
        result: VOID,
        safety: WRITABLE,
        returns: RETURNS
    },
    ToStringUnsigned => {
        symbol: "__rue_to_string_unsigned",
        parameters: params![STR_BUF_OUT, U64_VALUE],
        result: VOID,
        safety: WRITABLE,
        returns: RETURNS
    },
    Print => {
        symbol: "__rue_print",
        parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
        result: VOID,
        safety: READABLE,
        returns: RETURNS
    },
    Println => {
        symbol: "__rue_println",
        parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
        result: VOID,
        safety: READABLE,
        returns: RETURNS
    },
    StrPrint => {
        symbol: "__rue_str_print",
        parameters: params![BYTE_VIEW, U64_VALUE],
        result: VOID,
        safety: READABLE,
        returns: RETURNS
    },
    StrPrintln => {
        symbol: "__rue_str_println",
        parameters: params![BYTE_VIEW, U64_VALUE],
        result: VOID,
        safety: READABLE,
        returns: RETURNS
    },
    ReadLine => {
        symbol: "__rue_read_line",
        parameters: params![OPTION_STR_BUF_OUT, U64_VALUE, U64_VALUE],
        result: VOID,
        safety: WRITABLE_DISCRIMINANTS,
        returns: RETURNS
    },
    ParseI32 => {
        symbol: "__rue_parse_i32",
        parameters: params![OPTION_INT_OUT, BYTE_VIEW, U64_VALUE, U64_VALUE, U64_VALUE],
        result: VOID,
        safety: READABLE_WRITABLE_DISCRIMINANTS,
        returns: RETURNS
    },
    ParseI64 => {
        symbol: "__rue_parse_i64",
        parameters: params![OPTION_INT_OUT, BYTE_VIEW, U64_VALUE, U64_VALUE, U64_VALUE],
        result: VOID,
        safety: READABLE_WRITABLE_DISCRIMINANTS,
        returns: RETURNS
    },
    ParseU32 => {
        symbol: "__rue_parse_u32",
        parameters: params![OPTION_INT_OUT, BYTE_VIEW, U64_VALUE, U64_VALUE, U64_VALUE],
        result: VOID,
        safety: READABLE_WRITABLE_DISCRIMINANTS,
        returns: RETURNS
    },
    ParseU64 => {
        symbol: "__rue_parse_u64",
        parameters: params![OPTION_INT_OUT, BYTE_VIEW, U64_VALUE, U64_VALUE, U64_VALUE],
        result: VOID,
        safety: READABLE_WRITABLE_DISCRIMINANTS,
        returns: RETURNS
    },
    RandomU32 => {
        symbol: "__rue_random_u32",
        parameters: params![],
        result: U32_RESULT,
        safety: SafetyContract::NONE,
        returns: RETURNS
    },
    RandomU64 => {
        symbol: "__rue_random_u64",
        parameters: params![],
        result: U64_RESULT,
        safety: SafetyContract::NONE,
        returns: RETURNS
    },
    InvalidUtf8 => {
        symbol: "__rue_invalid_utf8",
        parameters: params![],
        result: VOID,
        safety: TERMINATES,
        returns: NEVER
    }
}

/// Logical function signature for separately classified non-helper exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AbiSignature {
    pub parameters: &'static [AbiParameter],
    pub result: AbiResult,
    pub return_behavior: ReturnBehavior,
    pub calling_convention: CallingConvention,
}

/// Why a reserved runtime symbol is externally visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReservedExportClass {
    CompilerBuiltMemory,
    ProgramEntry,
    PlatformShim,
    AbiVersionMarker,
}

/// Whether a separately classified export is callable code or retained data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReservedExportKind {
    Function(AbiSignature),
    ReadOnlyData { size: u8 },
}

/// Exhaustive identity for intentionally visible, non-helper runtime exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ReservedExportId {
    Memcpy,
    Memmove,
    Memset,
    Memcmp,
    Bcmp,
    LinuxStart,
    MacosMain,
    X86_64LinuxStart,
    RtSigreturn,
    RuntimeAbiVersion,
}

impl ReservedExportId {
    pub const ALL: [Self; 10] = [
        Self::Memcpy,
        Self::Memmove,
        Self::Memset,
        Self::Memcmp,
        Self::Bcmp,
        Self::LinuxStart,
        Self::MacosMain,
        Self::X86_64LinuxStart,
        Self::RtSigreturn,
        Self::RuntimeAbiVersion,
    ];

    pub const fn export(self) -> &'static ReservedExport {
        &RESERVED_EXPORTS[self as usize]
    }

    pub const fn symbol(self) -> &'static str {
        self.export().symbol
    }

    pub fn from_symbol(symbol: &str) -> Option<Self> {
        let mut index = 0;
        while index < Self::ALL.len() {
            let id = Self::ALL[index];
            if string_eq(id.symbol(), symbol) {
                return Some(id);
            }
            index += 1;
        }
        None
    }
}

/// A separately classified, intentionally visible runtime export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReservedExport {
    pub id: ReservedExportId,
    pub symbol: &'static str,
    pub class: ReservedExportClass,
    pub kind: ReservedExportKind,
    pub availability: TargetSet,
}

const VOID_FUNCTION: AbiSignature = AbiSignature {
    parameters: &[],
    result: VOID,
    return_behavior: RETURNS,
    calling_convention: TARGET_C,
};
const NEVER_FUNCTION: AbiSignature = AbiSignature {
    parameters: &[],
    result: VOID,
    return_behavior: NEVER,
    calling_convention: TARGET_C,
};
const COPY_FUNCTION: AbiSignature = AbiSignature {
    parameters: &[
        AbiParameter::mut_pointer(AbiType::Byte),
        AbiParameter::const_pointer(AbiType::Byte),
        AbiParameter::value(AbiType::Usize),
    ],
    result: MUT_POINTER_RESULT,
    return_behavior: RETURNS,
    calling_convention: TARGET_C,
};
const MEMSET_FUNCTION: AbiSignature = AbiSignature {
    parameters: &[
        AbiParameter::mut_pointer(AbiType::Byte),
        AbiParameter::value(AbiType::I32),
        AbiParameter::value(AbiType::Usize),
    ],
    result: MUT_POINTER_RESULT,
    return_behavior: RETURNS,
    calling_convention: TARGET_C,
};
const COMPARE_FUNCTION: AbiSignature = AbiSignature {
    parameters: &[
        AbiParameter::const_pointer(AbiType::Byte),
        AbiParameter::const_pointer(AbiType::Byte),
        AbiParameter::value(AbiType::Usize),
    ],
    result: AbiResult::Scalar(AbiType::I32),
    return_behavior: RETURNS,
    calling_convention: TARGET_C,
};

pub const RESERVED_EXPORTS: [ReservedExport; 10] = [
    ReservedExport {
        id: ReservedExportId::Memcpy,
        symbol: "memcpy",
        class: ReservedExportClass::CompilerBuiltMemory,
        kind: ReservedExportKind::Function(COPY_FUNCTION),
        availability: ALL_TARGETS,
    },
    ReservedExport {
        id: ReservedExportId::Memmove,
        symbol: "memmove",
        class: ReservedExportClass::CompilerBuiltMemory,
        kind: ReservedExportKind::Function(COPY_FUNCTION),
        availability: ALL_TARGETS,
    },
    ReservedExport {
        id: ReservedExportId::Memset,
        symbol: "memset",
        class: ReservedExportClass::CompilerBuiltMemory,
        kind: ReservedExportKind::Function(MEMSET_FUNCTION),
        availability: ALL_TARGETS,
    },
    ReservedExport {
        id: ReservedExportId::Memcmp,
        symbol: "memcmp",
        class: ReservedExportClass::CompilerBuiltMemory,
        kind: ReservedExportKind::Function(COMPARE_FUNCTION),
        availability: ALL_TARGETS,
    },
    ReservedExport {
        id: ReservedExportId::Bcmp,
        symbol: "bcmp",
        class: ReservedExportClass::CompilerBuiltMemory,
        kind: ReservedExportKind::Function(COMPARE_FUNCTION),
        availability: ALL_TARGETS,
    },
    ReservedExport {
        id: ReservedExportId::LinuxStart,
        symbol: "_start",
        class: ReservedExportClass::ProgramEntry,
        kind: ReservedExportKind::Function(NEVER_FUNCTION),
        availability: TargetSet::X86_64_LINUX.union(TargetSet::AARCH64_LINUX),
    },
    ReservedExport {
        id: ReservedExportId::MacosMain,
        symbol: "_main",
        class: ReservedExportClass::ProgramEntry,
        kind: ReservedExportKind::Function(NEVER_FUNCTION),
        availability: TargetSet::AARCH64_MACOS,
    },
    ReservedExport {
        id: ReservedExportId::X86_64LinuxStart,
        symbol: "__rue_x86_64_linux_start",
        class: ReservedExportClass::PlatformShim,
        kind: ReservedExportKind::Function(NEVER_FUNCTION),
        availability: TargetSet::X86_64_LINUX,
    },
    ReservedExport {
        id: ReservedExportId::RtSigreturn,
        symbol: "__rue_rt_sigreturn",
        class: ReservedExportClass::PlatformShim,
        kind: ReservedExportKind::Function(VOID_FUNCTION),
        availability: TargetSet::X86_64_LINUX,
    },
    ReservedExport {
        id: ReservedExportId::RuntimeAbiVersion,
        symbol: RUNTIME_ABI_VERSION_SYMBOL,
        class: ReservedExportClass::AbiVersionMarker,
        kind: ReservedExportKind::ReadOnlyData { size: 1 },
        availability: ALL_TARGETS,
    },
];

/// Classification of a known externally visible runtime name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeExport {
    Helper(RuntimeHelperId),
    Reserved(ReservedExportId),
}

pub fn classify_export(symbol: &str) -> Option<RuntimeExport> {
    RuntimeHelperId::from_symbol(symbol)
        .map(RuntimeExport::Helper)
        .or_else(|| ReservedExportId::from_symbol(symbol).map(RuntimeExport::Reserved))
}

/// Manifest invariant violated by [`validate_manifest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManifestError {
    HelperIdOrder,
    DuplicateHelperSymbol,
    InvalidHelperSymbol,
    AggregateIdOrder,
    EmptyAggregate,
    InvalidAggregateSlot,
    ReservedExportIdOrder,
    DuplicateReservedSymbol,
    HelperReservedSymbolCollision,
    InvalidVersionMarker,
}

const fn string_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

const fn starts_with(value: &str, prefix: &str) -> bool {
    let value = value.as_bytes();
    let prefix = prefix.as_bytes();
    if value.len() < prefix.len() {
        return false;
    }
    let mut index = 0;
    while index < prefix.len() {
        if value[index] != prefix[index] {
            return false;
        }
        index += 1;
    }
    true
}

const fn ends_with_decimal_version(symbol: &str, version: u32) -> bool {
    // Version 1 is the initial ABI. This explicit check makes an ABI bump update
    // both metadata values rather than silently retaining a stale symbol.
    version == 1 && string_eq(symbol, "__rue_runtime_abi_v1")
}

/// Validate all table ordering, uniqueness, classification, and layout invariants.
pub const fn validate_manifest() -> Result<(), ManifestError> {
    let mut index = 0;
    while index < RUNTIME_HELPERS.len() {
        let helper = &RUNTIME_HELPERS[index];
        if helper.id as usize != index || RuntimeHelperId::ALL[index] as usize != index {
            return Err(ManifestError::HelperIdOrder);
        }
        if !starts_with(helper.symbol, "__rue_") || helper.symbol.is_empty() {
            return Err(ManifestError::InvalidHelperSymbol);
        }
        let mut other = index + 1;
        while other < RUNTIME_HELPERS.len() {
            if string_eq(helper.symbol, RUNTIME_HELPERS[other].symbol) {
                return Err(ManifestError::DuplicateHelperSymbol);
            }
            other += 1;
        }
        index += 1;
    }

    index = 0;
    while index < AGGREGATE_SHAPES.len() {
        let aggregate = &AGGREGATE_SHAPES[index];
        if aggregate.id as usize != index || AggregateShapeId::ALL[index] as usize != index {
            return Err(ManifestError::AggregateIdOrder);
        }
        if aggregate.slots.is_empty() {
            return Err(ManifestError::EmptyAggregate);
        }
        let mut slot = 0;
        while slot < aggregate.slots.len() {
            if aggregate.slots[slot].name.is_empty() {
                return Err(ManifestError::InvalidAggregateSlot);
            }
            slot += 1;
        }
        index += 1;
    }

    index = 0;
    while index < RESERVED_EXPORTS.len() {
        let export = &RESERVED_EXPORTS[index];
        if export.id as usize != index || ReservedExportId::ALL[index] as usize != index {
            return Err(ManifestError::ReservedExportIdOrder);
        }
        let mut helper = 0;
        while helper < RUNTIME_HELPERS.len() {
            if string_eq(export.symbol, RUNTIME_HELPERS[helper].symbol) {
                return Err(ManifestError::HelperReservedSymbolCollision);
            }
            helper += 1;
        }
        let mut other = index + 1;
        while other < RESERVED_EXPORTS.len() {
            if string_eq(export.symbol, RESERVED_EXPORTS[other].symbol) {
                return Err(ManifestError::DuplicateReservedSymbol);
            }
            other += 1;
        }
        index += 1;
    }

    if !ends_with_decimal_version(RUNTIME_ABI_VERSION_SYMBOL, RUNTIME_ABI_VERSION) {
        return Err(ManifestError::InvalidVersionMarker);
    }
    Ok(())
}

const _: () = assert!(validate_manifest().is_ok());

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::format;
    use std::string::ToString;
    use std::vec::Vec;

    #[test]
    fn manifest_is_const_valid_and_exhaustive() {
        assert_eq!(validate_manifest(), Ok(()));
        assert_eq!(RuntimeHelperId::ALL.len(), 35);
        assert_eq!(RuntimeHelperId::ALL.len(), RUNTIME_HELPERS.len());
        for (index, id) in RuntimeHelperId::ALL.iter().copied().enumerate() {
            assert_eq!(id as usize, index);
            assert_eq!(id.helper().id, id);
            assert_eq!(RuntimeHelperId::from_symbol(id.symbol()), Some(id));
            assert_eq!(id.to_string(), id.symbol());
        }
        assert_eq!(RuntimeHelperId::from_symbol("__rue_not_a_helper"), None);
    }

    #[test]
    fn helper_symbols_are_unique() {
        let symbols: Vec<_> = RUNTIME_HELPERS.iter().map(|helper| helper.symbol).collect();
        for (index, symbol) in symbols.iter().enumerate() {
            assert!(!symbols[index + 1..].contains(symbol), "duplicate {symbol}");
        }
    }

    #[test]
    fn helper_inventory_has_the_exact_accepted_symbols_in_stable_order() {
        let expected = [
            "__rue_exit",
            "__rue_alloc",
            "__rue_free",
            "__rue_realloc",
            "__rue_div_by_zero",
            "__rue_overflow",
            "__rue_intcast_overflow",
            "__rue_bounds_check",
            "__rue_panic",
            "__rue_panic_no_msg",
            "__rue_assert_failed",
            "__rue_dbg_i64",
            "__rue_dbg_u64",
            "__rue_dbg_bool",
            "__rue_dbg_str",
            "__rue_str_eq",
            "__rue_str_byte_at",
            "__rue_str_char_scalar",
            "__rue_str_char_next",
            "__rue_str_char_scalar_lossy",
            "__rue_str_char_next_lossy",
            "__rue_to_string",
            "__rue_to_string_unsigned",
            "__rue_print",
            "__rue_println",
            "__rue_str_print",
            "__rue_str_println",
            "__rue_read_line",
            "__rue_parse_i32",
            "__rue_parse_i64",
            "__rue_parse_u32",
            "__rue_parse_u64",
            "__rue_random_u32",
            "__rue_random_u64",
            "__rue_invalid_utf8",
        ];
        assert_eq!(
            RUNTIME_HELPERS.map(|helper| helper.symbol),
            expected,
            "changing a helper symbol is an ABI change"
        );
    }

    #[test]
    fn every_helper_has_the_exact_accepted_signature_and_contract() {
        fn check(
            visited: &mut [bool; 35],
            ids: &[RuntimeHelperId],
            parameters: &[AbiParameter],
            result: AbiResult,
            safety: SafetyContract,
            return_behavior: ReturnBehavior,
        ) {
            for id in ids {
                let helper = id.helper();
                assert!(!visited[*id as usize], "{id:?} checked twice");
                visited[*id as usize] = true;
                assert_eq!(helper.parameters, parameters, "{id:?} parameters");
                assert_eq!(helper.result, result, "{id:?} result");
                assert_eq!(helper.safety, safety, "{id:?} safety");
                assert_eq!(
                    helper.return_behavior, return_behavior,
                    "{id:?} return behavior"
                );
            }
        }

        let mut visited = [false; 35];
        check(
            &mut visited,
            &[RuntimeHelperId::Exit],
            &[I32_VALUE],
            VOID,
            TERMINATES,
            NEVER,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::Alloc],
            &[U64_VALUE, U64_VALUE],
            MUT_POINTER_RESULT,
            ALLOC_LAYOUT,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::Free],
            &[MUT_BYTE_POINTER, U64_VALUE, U64_VALUE],
            VOID,
            SafetyContract::NONE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::Realloc],
            &[MUT_BYTE_POINTER, U64_VALUE, U64_VALUE, U64_VALUE],
            MUT_POINTER_RESULT,
            VALID_ALLOC_LAYOUT,
            RETURNS,
        );
        check(
            &mut visited,
            &[
                RuntimeHelperId::DivByZero,
                RuntimeHelperId::Overflow,
                RuntimeHelperId::IntcastOverflow,
                RuntimeHelperId::BoundsCheck,
                RuntimeHelperId::PanicNoMessage,
                RuntimeHelperId::AssertFailed,
                RuntimeHelperId::InvalidUtf8,
            ],
            &[],
            VOID,
            TERMINATES,
            NEVER,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::Panic],
            &[BYTE_VIEW, U64_VALUE],
            VOID,
            READABLE.union(TERMINATES),
            NEVER,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::DebugI64],
            &[I64_VALUE],
            VOID,
            SafetyContract::NONE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::DebugU64],
            &[U64_VALUE],
            VOID,
            SafetyContract::NONE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::DebugBool],
            &[BOOL_WORD_VALUE],
            VOID,
            SafetyContract::NONE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::DebugStr],
            &[BYTE_VIEW, U64_VALUE],
            VOID,
            READABLE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::StrEq],
            &[BYTE_VIEW, U64_VALUE, BYTE_VIEW, U64_VALUE],
            U64_RESULT,
            READABLE,
            RETURNS,
        );
        check(
            &mut visited,
            &[
                RuntimeHelperId::StrByteAt,
                RuntimeHelperId::StrCharScalar,
                RuntimeHelperId::StrCharNext,
                RuntimeHelperId::StrCharScalarLossy,
                RuntimeHelperId::StrCharNextLossy,
            ],
            &[BYTE_VIEW, U64_VALUE, U64_VALUE],
            U64_RESULT,
            READABLE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::ToString],
            &[STR_BUF_OUT, I64_VALUE],
            VOID,
            WRITABLE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::ToStringUnsigned],
            &[STR_BUF_OUT, U64_VALUE],
            VOID,
            WRITABLE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::Print, RuntimeHelperId::Println],
            &[BYTE_VIEW, U64_VALUE, U64_VALUE],
            VOID,
            READABLE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::StrPrint, RuntimeHelperId::StrPrintln],
            &[BYTE_VIEW, U64_VALUE],
            VOID,
            READABLE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::ReadLine],
            &[OPTION_STR_BUF_OUT, U64_VALUE, U64_VALUE],
            VOID,
            WRITABLE_DISCRIMINANTS,
            RETURNS,
        );
        check(
            &mut visited,
            &[
                RuntimeHelperId::ParseI32,
                RuntimeHelperId::ParseI64,
                RuntimeHelperId::ParseU32,
                RuntimeHelperId::ParseU64,
            ],
            &[OPTION_INT_OUT, BYTE_VIEW, U64_VALUE, U64_VALUE, U64_VALUE],
            VOID,
            READABLE_WRITABLE_DISCRIMINANTS,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::RandomU32],
            &[],
            U32_RESULT,
            SafetyContract::NONE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::RandomU64],
            &[],
            U64_RESULT,
            SafetyContract::NONE,
            RETURNS,
        );
        assert!(visited.into_iter().all(|was_visited| was_visited));
    }

    #[test]
    fn aggregate_shapes_have_exact_slots() {
        assert_eq!(
            AggregateShapeId::StrBufResult.shape().slots,
            [
                AggregateSlot {
                    name: "ptr",
                    ty: AbiType::MutBytePointer
                },
                AggregateSlot {
                    name: "len",
                    ty: AbiType::U64
                },
                AggregateSlot {
                    name: "cap",
                    ty: AbiType::U64
                },
            ]
        );
        assert_eq!(
            AggregateShapeId::OptionStrBufResult.shape().slots,
            [
                AggregateSlot {
                    name: "discriminant",
                    ty: AbiType::U64
                },
                AggregateSlot {
                    name: "ptr",
                    ty: AbiType::MutBytePointer
                },
                AggregateSlot {
                    name: "len",
                    ty: AbiType::U64
                },
                AggregateSlot {
                    name: "cap",
                    ty: AbiType::U64
                },
            ]
        );
        assert_eq!(
            AggregateShapeId::OptionIntResult.shape().slots,
            [
                AggregateSlot {
                    name: "discriminant",
                    ty: AbiType::U64
                },
                AggregateSlot {
                    name: "value",
                    ty: AbiType::U64
                },
            ]
        );
    }

    #[test]
    fn every_out_pointer_selects_a_declared_shape() {
        for helper in RUNTIME_HELPERS {
            for parameter in helper.parameters {
                if let ParameterMode::OutPointer(id) = parameter.mode {
                    assert_eq!(id.shape().id, id);
                    assert!(!id.shape().slots.is_empty());
                }
            }
        }
    }

    #[test]
    fn signatures_preserve_widths_and_modes_that_must_not_be_inferred() {
        assert_eq!(
            RuntimeHelperId::DebugBool.helper().parameters,
            [AbiParameter::value(AbiType::BoolWordI64)]
        );
        assert_eq!(
            RuntimeHelperId::StrEq.helper().result,
            AbiResult::Scalar(AbiType::U64)
        );
        assert_eq!(
            RuntimeHelperId::StrCharScalar.helper().result,
            AbiResult::Scalar(AbiType::U64)
        );
        assert_eq!(
            RuntimeHelperId::StrCharScalarLossy.helper().result,
            AbiResult::Scalar(AbiType::U64)
        );
        assert_eq!(
            RuntimeHelperId::RandomU32.helper().result,
            AbiResult::Scalar(AbiType::U32)
        );
        assert_eq!(
            RuntimeHelperId::ToString.helper().parameters[0].mode,
            ParameterMode::OutPointer(AggregateShapeId::StrBufResult)
        );
        assert_eq!(
            RuntimeHelperId::ParseI64.helper().parameters[0].mode,
            ParameterMode::OutPointer(AggregateShapeId::OptionIntResult)
        );
    }

    #[test]
    fn safety_requirements_are_composable() {
        let parse = RuntimeHelperId::ParseI32.helper().safety;
        assert!(parse.contains(SafetyContract::READABLE_BYTES));
        assert!(parse.contains(SafetyContract::WRITABLE_RESULT));
        assert!(parse.contains(SafetyContract::CONCRETE_DISCRIMINANTS));
        assert!(!parse.contains(SafetyContract::VALID_ALLOCATION));

        let realloc = RuntimeHelperId::Realloc.helper().safety;
        assert!(realloc.contains(SafetyContract::VALID_ALLOCATION));
        assert!(realloc.contains(SafetyContract::ALLOCATION_LAYOUT));
    }

    #[test]
    fn all_helpers_are_available_on_every_runtime_target_and_use_target_c() {
        for helper in RUNTIME_HELPERS {
            for target in [
                RuntimeTarget::X86_64Linux,
                RuntimeTarget::Aarch64Linux,
                RuntimeTarget::Aarch64Macos,
            ] {
                assert!(helper.availability.contains(target));
            }
            assert_eq!(helper.calling_convention, CallingConvention::TargetC);
        }
    }

    #[test]
    fn reserved_exports_are_separate_and_targeted() {
        assert_eq!(ReservedExportId::ALL.len(), RESERVED_EXPORTS.len());
        for (index, id) in ReservedExportId::ALL.iter().copied().enumerate() {
            assert_eq!(id as usize, index);
            assert_eq!(id.export().id, id);
            assert_eq!(ReservedExportId::from_symbol(id.symbol()), Some(id));
            assert!(RuntimeHelperId::from_symbol(id.symbol()).is_none());
            assert_eq!(
                classify_export(id.symbol()),
                Some(RuntimeExport::Reserved(id))
            );
        }
        assert!(
            ReservedExportId::LinuxStart
                .export()
                .availability
                .contains(RuntimeTarget::X86_64Linux)
        );
        assert!(
            !ReservedExportId::LinuxStart
                .export()
                .availability
                .contains(RuntimeTarget::Aarch64Macos)
        );
        assert!(
            ReservedExportId::MacosMain
                .export()
                .availability
                .contains(RuntimeTarget::Aarch64Macos)
        );
        assert!(
            !ReservedExportId::RtSigreturn
                .export()
                .availability
                .contains(RuntimeTarget::Aarch64Linux)
        );
    }

    #[test]
    fn abi_version_metadata_is_a_one_byte_data_export() {
        assert_eq!(RUNTIME_ABI_VERSION, 1);
        assert_eq!(
            RUNTIME_ABI_VERSION_SYMBOL,
            format!("__rue_runtime_abi_v{RUNTIME_ABI_VERSION}")
        );
        assert_eq!(
            ReservedExportId::RuntimeAbiVersion.export().kind,
            ReservedExportKind::ReadOnlyData { size: 1 }
        );
    }
}
