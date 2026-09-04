#![no_std]

//! Canonical typed description of the Rue compiler/runtime ABI.
//!
//! This crate deliberately has no dependencies. It describes logical C-boundary
//! facts; target register assignment and compiler semantic types belong to
//! their respective owners.

use core::fmt;

/// Invoke a callback with the canonical runtime ABI version and marker
/// identifier.
///
/// Both metadata and runtime emission consume this declaration, so an ABI bump
/// cannot leave behind an independently spelled marker.
#[macro_export]
macro_rules! runtime_abi_version {
    ($callback:ident) => {
        $callback!(5, __rue_runtime_abi_v5);
    };
}

macro_rules! define_runtime_abi_version {
    ($version:literal, $marker:ident) => {
        /// Current lockstep compiler/runtime ABI version.
        pub const RUNTIME_ABI_VERSION: u32 = $version;

        /// Retained data symbol exported by each runtime archive for this ABI version.
        pub const RUNTIME_ABI_VERSION_SYMBOL: &str = stringify!($marker);
    };
}

runtime_abi_version!(define_runtime_abi_version);

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

/// Width discriminants accepted by the float-consuming helpers,
/// `__rue_to_string_float` and `__rue_dbg_float`.
///
/// The value travels as a `u32`, not as a Rust or C enum, so an invalid caller
/// value cannot create undefined behavior at the boundary. The runtime traps
/// on any value other than these two constants. For [`FLOAT_WIDTH_F32`], only
/// the low 32 bits of the adjacent `u64` bit-pattern parameter may be set.
pub const FLOAT_WIDTH_F32: u32 = 32;
pub const FLOAT_WIDTH_F64: u32 = 64;

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

impl RuntimeTarget {
    /// Direct `write(2)` syscall number for this supported runtime target.
    ///
    /// This is a shared ABI fact consumed by both the no-std runtime wrappers
    /// and the reference oracle; target-specific register placement remains in
    /// each runtime module.
    pub const fn write_syscall_number(self) -> u64 {
        match self {
            Self::X86_64Linux => 1,
            Self::Aarch64Linux => 64,
            Self::Aarch64Macos => 4,
        }
    }
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

    /// Whether caller-owned pointer validity requires an unsafe Rust boundary.
    pub const fn requires_unsafe_rust_boundary(self) -> bool {
        self.contains(Self::READABLE_BYTES)
            || self.contains(Self::WRITABLE_RESULT)
            || self.contains(Self::VALID_ALLOCATION)
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

// Slot order mirrors `StrBuf`'s flattened `{buf, cap, len}` layout (RUE-1066):
// the `RawBuf(u8)` core contributes `{buf, cap}`, then the length.
const STR_BUF_SLOTS: &[AggregateSlot] = &[
    AggregateSlot {
        name: "ptr",
        ty: AbiType::MutBytePointer,
    },
    AggregateSlot {
        name: "cap",
        ty: AbiType::U64,
    },
    AggregateSlot {
        name: "len",
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
        name: "cap",
        ty: AbiType::U64,
    },
    AggregateSlot {
        name: "len",
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

/// The three-word growable-string result written by formatting helpers. The
/// field order mirrors `StrBuf`'s flattened slot layout `{buf, cap, len}` — the
/// buffer and capacity live in the shared `RawBuf(u8)` core, then the length
/// (RUE-1066) — so a `StrBuf`-shaped value is copied slot-for-slot from this
/// result. Nothing outside the compiler/runtime depends on this order.
#[repr(C)]
pub struct StrBufResult {
    pub ptr: *mut u8,
    pub cap: u64,
    pub len: u64,
}

/// The tagged growable-string option written by `__rue_read_line`.
#[repr(C)]
pub struct OptionStrBufResult {
    pub disc: u64,
    pub ptr: *mut u8,
    pub cap: u64,
    pub len: u64,
}

/// The tagged integer option written by the parsing helpers.
#[repr(C)]
pub struct OptionIntResult {
    pub disc: u64,
    pub value: u64,
}

const _: () = {
    assert!(core::mem::size_of::<StrBufResult>() == 24);
    assert!(core::mem::align_of::<StrBufResult>() == 8);
    assert!(core::mem::offset_of!(StrBufResult, ptr) == 0);
    assert!(core::mem::offset_of!(StrBufResult, cap) == 8);
    assert!(core::mem::offset_of!(StrBufResult, len) == 16);
    assert!(core::mem::size_of::<OptionStrBufResult>() == 32);
    assert!(core::mem::align_of::<OptionStrBufResult>() == 8);
    assert!(core::mem::offset_of!(OptionStrBufResult, disc) == 0);
    assert!(core::mem::offset_of!(OptionStrBufResult, ptr) == 8);
    assert!(core::mem::offset_of!(OptionStrBufResult, cap) == 16);
    assert!(core::mem::offset_of!(OptionStrBufResult, len) == 24);
    assert!(core::mem::size_of::<OptionIntResult>() == 16);
    assert!(core::mem::align_of::<OptionIntResult>() == 8);
    assert!(core::mem::offset_of!(OptionIntResult, disc) == 0);
    assert!(core::mem::offset_of!(OptionIntResult, value) == 8);
};

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
const U32_VALUE: AbiParameter = AbiParameter::value(AbiType::U32);
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
/// A `bool`-valued helper result: the C-ABI word carrying 0 or 1, the same
/// representation `AbiType::BoolWordI64` gives a `bool` argument.
const BOOL_WORD_RESULT: AbiResult = AbiResult::Scalar(AbiType::BoolWordI64);
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
            $variant:ident => $safety_kind:ident $function:ident(
                $($argument:ident : $rust_type:ty),* $(,)?
            ) $(-> $rust_result:ty)? {
                symbol: $symbol:literal,
                parameters: $parameters:expr,
                result: $result:expr,
                safety: $safety:expr,
                returns: $returns:expr
            }
        ),+ $(,)?
    ) => {
        /// Exhaustive typed identity for compiler-callable runtime helpers.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

        const _: () = {
            $(
                assert!(string_eq(stringify!($function), $symbol));
                assert!(
                    $safety.requires_unsafe_rust_boundary()
                        == runtime_helpers!(@is_unsafe $safety_kind)
                );
            )+
        };
    };
    (@count $($variant:ident),+) => {
        <[()]>::len(&[$(runtime_helpers!(@replace $variant ())),+])
    };
    (@replace $_variant:ident $value:expr) => {
        $value
    };
    (@is_unsafe safe) => {
        false
    };
    (@is_unsafe unsafe) => {
        true
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

/// Invoke a callback with the canonical Rust declaration and logical contract
/// for every compiler-callable runtime helper.
///
/// The runtime uses the Rust declarations to generate its exported wrappers,
/// while this crate uses the same rows to build [`RUNTIME_HELPERS`]. Keeping
/// both consumers on this macro prevents a peer Rust signature table from
/// drifting away from the manifest.
#[macro_export]
macro_rules! for_each_runtime_helper {
    ($callback:ident) => {
        $callback! {
        Exit => safe __rue_exit(status: i32) -> ! {
            symbol: "__rue_exit",
            parameters: params![I32_VALUE],
            result: VOID,
            safety: TERMINATES,
            returns: NEVER
        },
        Alloc => safe __rue_alloc(size: u64, align: u64) -> *mut u8 {
            symbol: "__rue_alloc",
            parameters: params![U64_VALUE, U64_VALUE],
            result: MUT_POINTER_RESULT,
            safety: ALLOC_LAYOUT,
            returns: RETURNS
        },
        AllocZeroed => safe __rue_alloc_zeroed(size: u64, align: u64) -> *mut u8 {
            symbol: "__rue_alloc_zeroed",
            parameters: params![U64_VALUE, U64_VALUE],
            result: MUT_POINTER_RESULT,
            safety: ALLOC_LAYOUT,
            returns: RETURNS
        },
        Free => unsafe __rue_free(ptr: *mut u8, size: u64, align: u64) {
            symbol: "__rue_free",
            parameters: params![MUT_BYTE_POINTER, U64_VALUE, U64_VALUE],
            result: VOID,
            safety: VALID_ALLOC_LAYOUT,
            returns: RETURNS
        },
        Realloc => unsafe __rue_realloc(
            ptr: *mut u8,
            old_size: u64,
            new_size: u64,
            align: u64,
        ) -> *mut u8 {
            symbol: "__rue_realloc",
            parameters: params![MUT_BYTE_POINTER, U64_VALUE, U64_VALUE, U64_VALUE],
            result: MUT_POINTER_RESULT,
            safety: VALID_ALLOC_LAYOUT,
            returns: RETURNS
        },
        Resize => unsafe __rue_resize(
            ptr: *mut u8,
            old_size: u64,
            new_size: u64,
            align: u64,
        ) -> i64 {
            symbol: "__rue_resize",
            parameters: params![MUT_BYTE_POINTER, U64_VALUE, U64_VALUE, U64_VALUE],
            result: BOOL_WORD_RESULT,
            safety: VALID_ALLOC_LAYOUT,
            returns: RETURNS
        },
        DivByZero => safe __rue_div_by_zero() -> ! {
            symbol: "__rue_div_by_zero",
            parameters: params![],
            result: VOID,
            safety: TERMINATES,
            returns: NEVER
        },
        Overflow => safe __rue_overflow() -> ! {
            symbol: "__rue_overflow",
            parameters: params![],
            result: VOID,
            safety: TERMINATES,
            returns: NEVER
        },
        IntcastOverflow => safe __rue_intcast_overflow() -> ! {
            symbol: "__rue_intcast_overflow",
            parameters: params![],
            result: VOID,
            safety: TERMINATES,
            returns: NEVER
        },
        BoundsCheck => safe __rue_bounds_check() -> ! {
            symbol: "__rue_bounds_check",
            parameters: params![],
            result: VOID,
            safety: TERMINATES,
            returns: NEVER
        },
        Panic => unsafe __rue_panic(ptr: *const u8, len: u64) -> ! {
            symbol: "__rue_panic",
            parameters: params![BYTE_VIEW, U64_VALUE],
            result: VOID,
            safety: READABLE.union(TERMINATES),
            returns: NEVER
        },
        PanicNoMessage => safe __rue_panic_no_msg() -> ! {
            symbol: "__rue_panic_no_msg",
            parameters: params![],
            result: VOID,
            safety: TERMINATES,
            returns: NEVER
        },
        AssertFailed => safe __rue_assert_failed() -> ! {
            symbol: "__rue_assert_failed",
            parameters: params![],
            result: VOID,
            safety: TERMINATES,
            returns: NEVER
        },
        DebugI64 => safe __rue_dbg_i64(value: i64) {
            symbol: "__rue_dbg_i64",
            parameters: params![I64_VALUE],
            result: VOID,
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        DebugU64 => safe __rue_dbg_u64(value: u64) {
            symbol: "__rue_dbg_u64",
            parameters: params![U64_VALUE],
            result: VOID,
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        DebugBool => safe __rue_dbg_bool(value: i64) {
            symbol: "__rue_dbg_bool",
            parameters: params![BOOL_WORD_VALUE],
            result: VOID,
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        DebugFloat => safe __rue_dbg_float(bits: u64, width: u32) {
            symbol: "__rue_dbg_float",
            parameters: params![U64_VALUE, U32_VALUE],
            result: VOID,
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        DebugStr => unsafe __rue_dbg_str(ptr: *const u8, len: u64) {
            symbol: "__rue_dbg_str",
            parameters: params![BYTE_VIEW, U64_VALUE],
            result: VOID,
            safety: READABLE,
            returns: RETURNS
        },
        StrEq => unsafe __rue_str_eq(
            ptr1: *const u8,
            len1: u64,
            ptr2: *const u8,
            len2: u64,
        ) -> u64 {
            symbol: "__rue_str_eq",
            parameters: params![BYTE_VIEW, U64_VALUE, BYTE_VIEW, U64_VALUE],
            result: U64_RESULT,
            safety: READABLE,
            returns: RETURNS
        },
        StrByteAt => unsafe __rue_str_byte_at(ptr: *const u8, len: u64, index: u64) -> u64 {
            symbol: "__rue_str_byte_at",
            parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
            result: U64_RESULT,
            safety: READABLE,
            returns: RETURNS
        },
        StrCharScalar => unsafe __rue_str_char_scalar(
            ptr: *const u8,
            len: u64,
            byte_offset: u64,
        ) -> u64 {
            symbol: "__rue_str_char_scalar",
            parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
            result: U64_RESULT,
            safety: READABLE,
            returns: RETURNS
        },
        StrCharNext => unsafe __rue_str_char_next(
            ptr: *const u8,
            len: u64,
            byte_offset: u64,
        ) -> u64 {
            symbol: "__rue_str_char_next",
            parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
            result: U64_RESULT,
            safety: READABLE,
            returns: RETURNS
        },
        StrCharScalarLossy => unsafe __rue_str_char_scalar_lossy(
            ptr: *const u8,
            len: u64,
            byte_offset: u64,
        ) -> u64 {
            symbol: "__rue_str_char_scalar_lossy",
            parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
            result: U64_RESULT,
            safety: READABLE,
            returns: RETURNS
        },
        StrCharNextLossy => unsafe __rue_str_char_next_lossy(
            ptr: *const u8,
            len: u64,
            byte_offset: u64,
        ) -> u64 {
            symbol: "__rue_str_char_next_lossy",
            parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
            result: U64_RESULT,
            safety: READABLE,
            returns: RETURNS
        },
        ToString => unsafe __rue_to_string(out: *mut $crate::StrBufResult, n: i64) {
            symbol: "__rue_to_string",
            parameters: params![STR_BUF_OUT, I64_VALUE],
            result: VOID,
            safety: WRITABLE,
            returns: RETURNS
        },
        ToStringUnsigned => unsafe __rue_to_string_unsigned(
            out: *mut $crate::StrBufResult,
            n: u64,
        ) {
            symbol: "__rue_to_string_unsigned",
            parameters: params![STR_BUF_OUT, U64_VALUE],
            result: VOID,
            safety: WRITABLE,
            returns: RETURNS
        },
        ToStringFloat => unsafe __rue_to_string_float(
            out: *mut $crate::StrBufResult,
            bits: u64,
            width: u32,
        ) {
            symbol: "__rue_to_string_float",
            parameters: params![STR_BUF_OUT, U64_VALUE, U32_VALUE],
            result: VOID,
            safety: WRITABLE,
            returns: RETURNS
        },
        Print => unsafe __rue_print(ptr: *const u8, len: u64, cap: u64) {
            symbol: "__rue_print",
            parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
            result: VOID,
            safety: READABLE,
            returns: RETURNS
        },
        Println => unsafe __rue_println(ptr: *const u8, len: u64, cap: u64) {
            symbol: "__rue_println",
            parameters: params![BYTE_VIEW, U64_VALUE, U64_VALUE],
            result: VOID,
            safety: READABLE,
            returns: RETURNS
        },
        StrPrint => unsafe __rue_str_print(ptr: *const u8, len: u64) {
            symbol: "__rue_str_print",
            parameters: params![BYTE_VIEW, U64_VALUE],
            result: VOID,
            safety: READABLE,
            returns: RETURNS
        },
        StrPrintln => unsafe __rue_str_println(ptr: *const u8, len: u64) {
            symbol: "__rue_str_println",
            parameters: params![BYTE_VIEW, U64_VALUE],
            result: VOID,
            safety: READABLE,
            returns: RETURNS
        },
        ReadLine => unsafe __rue_read_line(
            out: *mut $crate::OptionStrBufResult,
            some_disc: u64,
            none_disc: u64,
        ) {
            symbol: "__rue_read_line",
            parameters: params![OPTION_STR_BUF_OUT, U64_VALUE, U64_VALUE],
            result: VOID,
            safety: WRITABLE_DISCRIMINANTS,
            returns: RETURNS
        },
        ParseI32 => unsafe __rue_parse_i32(
            out: *mut $crate::OptionIntResult,
            ptr: *const u8,
            len: u64,
            some_disc: u64,
            none_disc: u64,
        ) {
            symbol: "__rue_parse_i32",
            parameters: params![OPTION_INT_OUT, BYTE_VIEW, U64_VALUE, U64_VALUE, U64_VALUE],
            result: VOID,
            safety: READABLE_WRITABLE_DISCRIMINANTS,
            returns: RETURNS
        },
        ParseI64 => unsafe __rue_parse_i64(
            out: *mut $crate::OptionIntResult,
            ptr: *const u8,
            len: u64,
            some_disc: u64,
            none_disc: u64,
        ) {
            symbol: "__rue_parse_i64",
            parameters: params![OPTION_INT_OUT, BYTE_VIEW, U64_VALUE, U64_VALUE, U64_VALUE],
            result: VOID,
            safety: READABLE_WRITABLE_DISCRIMINANTS,
            returns: RETURNS
        },
        ParseU32 => unsafe __rue_parse_u32(
            out: *mut $crate::OptionIntResult,
            ptr: *const u8,
            len: u64,
            some_disc: u64,
            none_disc: u64,
        ) {
            symbol: "__rue_parse_u32",
            parameters: params![OPTION_INT_OUT, BYTE_VIEW, U64_VALUE, U64_VALUE, U64_VALUE],
            result: VOID,
            safety: READABLE_WRITABLE_DISCRIMINANTS,
            returns: RETURNS
        },
        ParseU64 => unsafe __rue_parse_u64(
            out: *mut $crate::OptionIntResult,
            ptr: *const u8,
            len: u64,
            some_disc: u64,
            none_disc: u64,
        ) {
            symbol: "__rue_parse_u64",
            parameters: params![OPTION_INT_OUT, BYTE_VIEW, U64_VALUE, U64_VALUE, U64_VALUE],
            result: VOID,
            safety: READABLE_WRITABLE_DISCRIMINANTS,
            returns: RETURNS
        },
        RandomU32 => safe __rue_random_u32() -> u32 {
            symbol: "__rue_random_u32",
            parameters: params![],
            result: U32_RESULT,
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        RandomU64 => safe __rue_random_u64() -> u64 {
            symbol: "__rue_random_u64",
            parameters: params![],
            result: U64_RESULT,
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        InvalidUtf8 => safe __rue_invalid_utf8() -> ! {
            symbol: "__rue_invalid_utf8",
            parameters: params![],
            result: VOID,
            safety: TERMINATES,
            returns: NEVER
        },
        ArgCount => safe __rue_arg_count() -> u64 {
            symbol: "__rue_arg_count",
            parameters: params![],
            result: U64_RESULT,
            // The runtime captured argc at process entry; reading a stored count
            // imposes no caller obligation.
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        ArgPtr => safe __rue_arg_ptr(index: u64) -> *mut u8 {
            symbol: "__rue_arg_ptr",
            parameters: params![U64_VALUE],
            result: MUT_POINTER_RESULT,
            // The runtime bounds-checks `index` against the captured count and
            // returns null when out of range, so the caller owes nothing.
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        ArgLen => safe __rue_arg_len(index: u64) -> u64 {
            symbol: "__rue_arg_len",
            parameters: params![U64_VALUE],
            result: U64_RESULT,
            // NUL-terminated scan of a captured argv entry; out-of-range yields 0.
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        EnvCount => safe __rue_env_count() -> u64 {
            symbol: "__rue_env_count",
            parameters: params![],
            result: U64_RESULT,
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        EnvPtr => safe __rue_env_ptr(index: u64) -> *mut u8 {
            symbol: "__rue_env_ptr",
            parameters: params![U64_VALUE],
            result: MUT_POINTER_RESULT,
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        EnvLen => safe __rue_env_len(index: u64) -> u64 {
            symbol: "__rue_env_len",
            parameters: params![U64_VALUE],
            result: U64_RESULT,
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        ByteCopy => unsafe __rue_byte_copy(dst: *mut u8, src: *const u8, size: u64) {
            symbol: "__rue_byte_copy",
            parameters: params![MUT_BYTE_POINTER, BYTE_VIEW, U64_VALUE],
            result: VOID,
            // Copies `size` bytes from `src` into the non-overlapping region at
            // `dst`; both pointer/length ranges must describe accessible bytes.
            safety: READABLE.union(WRITABLE),
            returns: RETURNS
        },
        ByteMove => unsafe __rue_byte_move(dst: *mut u8, src: *const u8, size: u64) {
            symbol: "__rue_byte_move",
            parameters: params![MUT_BYTE_POINTER, BYTE_VIEW, U64_VALUE],
            result: VOID,
            // Copies `size` bytes from `src` into `dst` as if through a
            // temporary buffer, so the two ranges may overlap; both must
            // describe accessible bytes.
            safety: READABLE.union(WRITABLE),
            returns: RETURNS
        },
        ByteSet => unsafe __rue_byte_set(dst: *mut u8, value: u64, size: u64) {
            symbol: "__rue_byte_set",
            parameters: params![MUT_BYTE_POINTER, U64_VALUE, U64_VALUE],
            result: VOID,
            // Writes the low byte of `value` to each of `size` bytes at `dst`,
            // which must denote that many writable bytes.
            safety: WRITABLE,
            returns: RETURNS
        },
        // The ADR-0083 test channel (§3, §5.1). The dispatcher's own three are
        // runner plumbing no source spelling selects; the reporting pair and
        // its comparison form are what `@assert_eq`/`@assert_ne` and a test
        // body's `?` lower to, in a test image and in an ordinary build alike.
        TestNormalizeProcess => safe __rue_test_normalize_process() {
            symbol: "__rue_test_normalize_process",
            parameters: params![],
            result: VOID,
            // Rewrites the runtime's own captured argument count; the caller
            // supplies nothing and owes nothing.
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        TestComplete => safe __rue_test_complete() {
            symbol: "__rue_test_complete",
            parameters: params![],
            result: VOID,
            // Writes the terminal completion frame to the inherited failure
            // channel; the write is best-effort and takes no caller pointer.
            safety: SafetyContract::NONE,
            returns: RETURNS
        },
        // A failure record carries more than a register-only call can take:
        // three byte views plus a file, a line, and a column is ten arguments,
        // and every runtime helper is register-only (six on x86-64). The record
        // is therefore assembled by two calls. This one stages the failing
        // source location, and the terminal call below emits the record and
        // aborts. Nothing may run between them.
        TestFailureSite => unsafe __rue_test_failure_site(
            file_ptr: *const u8,
            file_len: u64,
            line: u32,
            column: u32,
        ) {
            symbol: "__rue_test_failure_site",
            parameters: params![BYTE_VIEW, U64_VALUE, U32_VALUE, U32_VALUE],
            result: VOID,
            // The file bytes are borrowed, not copied: they must stay readable
            // until the `__rue_test_fail` that consumes this site has written
            // its record.
            safety: READABLE,
            returns: RETURNS
        },
        TestFail => unsafe __rue_test_fail(
            kind_ptr: *const u8,
            kind_len: u64,
            message_ptr: *const u8,
            message_len: u64,
            payload_ptr: *const u8,
            payload_len: u64,
        ) -> ! {
            symbol: "__rue_test_fail",
            parameters: params![
                BYTE_VIEW,
                U64_VALUE,
                BYTE_VIEW,
                U64_VALUE,
                BYTE_VIEW,
                U64_VALUE,
            ],
            result: VOID,
            safety: READABLE.union(TERMINATES),
            returns: NEVER
        },
        // The comparison form of the same terminal call (ADR-0083 Phase 2.5).
        // It carries the two rendered operands where `__rue_test_fail` carries
        // one message and one open payload, so a consumer reads `left` and
        // `right` as separate fields instead of parsing them back out of one
        // string. The message is pinned by the kind rather than passed, which
        // is what keeps this to the same six registers.
        TestFailComparison => unsafe __rue_test_fail_comparison(
            kind_ptr: *const u8,
            kind_len: u64,
            left_ptr: *const u8,
            left_len: u64,
            right_ptr: *const u8,
            right_len: u64,
        ) -> ! {
            symbol: "__rue_test_fail_comparison",
            parameters: params![
                BYTE_VIEW,
                U64_VALUE,
                BYTE_VIEW,
                U64_VALUE,
                BYTE_VIEW,
                U64_VALUE,
            ],
            result: VOID,
            safety: READABLE.union(TERMINATES),
            returns: NEVER
        },
        // The `@assert` form of the same terminal call (spec 4.13:5d). Its
        // two stderr messages are two different pinned strings rather than one
        // string built from the kind, so the form travels as a parameter:
        // `with_message` selects the user's text and `@panic`'s `panic: `
        // prefix over the bare `assertion failed` that is both the frame's
        // message and the whole stderr line. Neither form carries a `payload`;
        // a bare assertion has nothing structured to put in one.
        TestFailAssert => unsafe __rue_test_fail_assert(
            message_ptr: *const u8,
            message_len: u64,
            with_message: u32,
        ) -> ! {
            symbol: "__rue_test_fail_assert",
            parameters: params![BYTE_VIEW, U64_VALUE, U32_VALUE],
            result: VOID,
            safety: READABLE.union(TERMINATES),
            returns: NEVER
        },
        TestUsageError => safe __rue_test_usage_error() {
            symbol: "__rue_test_usage_error",
            parameters: params![],
            result: VOID,
            // Writes one pinned diagnostic to stderr and returns so the
            // dispatcher can choose its own exit status.
            safety: SafetyContract::NONE,
            returns: RETURNS
        }
            }
    };
}

for_each_runtime_helper!(runtime_helpers);

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

/// Invoke a callback with every separately classified runtime export.
///
/// The callback receives the exact Rust declaration, applicability, and
/// manifest metadata. Runtime declarations and the typed reserved-export table
/// are generated from these same rows.
#[macro_export]
macro_rules! for_each_reserved_runtime_export {
    ($callback:ident) => {
        $callback! {
            (Memcpy => function all unsafe memcpy(
                dst: *mut u8,
                src: *const u8,
                count: usize,
            ) -> *mut u8 {
                class: ReservedExportClass::CompilerBuiltMemory,
                signature: COPY_FUNCTION
            }),
            (Memmove => function all unsafe memmove(
                dst: *mut u8,
                src: *const u8,
                count: usize,
            ) -> *mut u8 {
                class: ReservedExportClass::CompilerBuiltMemory,
                signature: COPY_FUNCTION
            }),
            (Memset => function all unsafe memset(
                dst: *mut u8,
                value: i32,
                count: usize,
            ) -> *mut u8 {
                class: ReservedExportClass::CompilerBuiltMemory,
                signature: MEMSET_FUNCTION
            }),
            (Memcmp => function all unsafe memcmp(
                left: *const u8,
                right: *const u8,
                count: usize,
            ) -> i32 {
                class: ReservedExportClass::CompilerBuiltMemory,
                signature: COMPARE_FUNCTION
            }),
            (Bcmp => function all unsafe bcmp(
                left: *const u8,
                right: *const u8,
                count: usize,
            ) -> i32 {
                class: ReservedExportClass::CompilerBuiltMemory,
                signature: COMPARE_FUNCTION
            }),
            (LinuxStart => linux_entry linux unsafe _start() -> ! {
                class: ReservedExportClass::ProgramEntry,
                signature: NEVER_FUNCTION
            }),
            (MacosMain => function aarch64_macos unsafe _main(
                argc: i32,
                argv: *const *const u8,
                envp: *const *const u8,
            ) -> ! {
                class: ReservedExportClass::ProgramEntry,
                signature: NEVER_FUNCTION
            }),
            (X86_64LinuxStart => function x86_64_linux safe __rue_x86_64_linux_start(
                stack: *const usize,
            ) -> ! {
                class: ReservedExportClass::PlatformShim,
                signature: NEVER_FUNCTION
            }),
            (Aarch64LinuxStart => function aarch64_linux safe __rue_aarch64_linux_start(
                stack: *const usize,
            ) -> ! {
                class: ReservedExportClass::PlatformShim,
                signature: NEVER_FUNCTION
            }),
            (RtSigreturn => assembly x86_64_linux unsafe __rue_rt_sigreturn() {
                class: ReservedExportClass::PlatformShim,
                signature: VOID_FUNCTION
            }),
            (RuntimeAbiVersion => marker all {
                class: ReservedExportClass::AbiVersionMarker,
                size: 1
            })
        }
    };
}

macro_rules! define_reserved_export_ids {
    (
        $(
            ($variant:ident => $kind:ident $($declaration:tt)*)
        ),+ $(,)?
    ) => {
        /// Exhaustive identity for intentionally visible, non-helper runtime exports.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u8)]
        pub enum ReservedExportId {
            $($variant),+
        }

        impl ReservedExportId {
            pub const ALL: [Self; <[()]>::len(&[$(define_reserved_export_ids!(@unit $variant)),+])] = [
                $(Self::$variant),+
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
    };
    (@unit $_variant:ident) => { () };
}

for_each_reserved_runtime_export!(define_reserved_export_ids);

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

macro_rules! define_reserved_exports {
    (
        $(
            ($variant:ident => $($row:tt)*)
        ),+ $(,)?
    ) => {
        pub const RESERVED_EXPORTS: [ReservedExport; ReservedExportId::ALL.len()] = [
            $(
                define_reserved_exports!(@row $variant => $($row)*),
            )+
        ];
    };
    (
        @row $variant:ident => function $target:ident $safety:ident $function:ident(
            $($argument:ident : $rust_type:ty),* $(,)?
        ) $(-> $rust_result:ty)? {
            class: $class:path,
            signature: $signature:ident
        }
    ) => {
        ReservedExport {
            id: ReservedExportId::$variant,
            symbol: stringify!($function),
            class: $class,
            kind: ReservedExportKind::Function($signature),
            availability: define_reserved_exports!(@targets $target),
        }
    };
    (
        @row $variant:ident => linux_entry $target:ident $safety:ident $function:ident()
            -> $rust_result:ty {
                class: $class:path,
                signature: $signature:ident
            }
    ) => {
        ReservedExport {
            id: ReservedExportId::$variant,
            symbol: stringify!($function),
            class: $class,
            kind: ReservedExportKind::Function($signature),
            availability: define_reserved_exports!(@targets $target),
        }
    };
    (
        @row $variant:ident => assembly $target:ident $safety:ident $function:ident()
            $(-> $rust_result:ty)? {
                class: $class:path,
                signature: $signature:ident
            }
    ) => {
        ReservedExport {
            id: ReservedExportId::$variant,
            symbol: stringify!($function),
            class: $class,
            kind: ReservedExportKind::Function($signature),
            availability: define_reserved_exports!(@targets $target),
        }
    };
    (
        @row RuntimeAbiVersion => marker $target:ident {
            class: $class:path,
            size: $size:literal
        }
    ) => {
        ReservedExport {
            id: ReservedExportId::RuntimeAbiVersion,
            symbol: RUNTIME_ABI_VERSION_SYMBOL,
            class: $class,
            kind: ReservedExportKind::ReadOnlyData { size: $size },
            availability: define_reserved_exports!(@targets $target),
        }
    };
    (@targets all) => { TargetSet::ALL };
    (@targets linux) => { TargetSet::X86_64_LINUX.union(TargetSet::AARCH64_LINUX) };
    (@targets aarch64_macos) => { TargetSet::AARCH64_MACOS };
    (@targets x86_64_linux) => { TargetSet::X86_64_LINUX };
    (@targets aarch64_linux) => { TargetSet::AARCH64_LINUX };
}

for_each_reserved_runtime_export!(define_reserved_exports);

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
    // Version 3 added the ADR-0083 §5.1 comparison failure channel; version 4
    // added width-explicit float formatting; version 5 added the channel's
    // `@assert` form, `__rue_test_fail_assert`. This explicit check makes an
    // ABI bump update both metadata values rather than silently retaining a
    // stale symbol.
    version == 5 && string_eq(symbol, "__rue_runtime_abi_v5")
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
        assert_eq!(RuntimeHelperId::ALL.len(), 55);
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
            "__rue_alloc_zeroed",
            "__rue_free",
            "__rue_realloc",
            "__rue_resize",
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
            "__rue_dbg_float",
            "__rue_dbg_str",
            "__rue_str_eq",
            "__rue_str_byte_at",
            "__rue_str_char_scalar",
            "__rue_str_char_next",
            "__rue_str_char_scalar_lossy",
            "__rue_str_char_next_lossy",
            "__rue_to_string",
            "__rue_to_string_unsigned",
            "__rue_to_string_float",
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
            "__rue_arg_count",
            "__rue_arg_ptr",
            "__rue_arg_len",
            "__rue_env_count",
            "__rue_env_ptr",
            "__rue_env_len",
            "__rue_byte_copy",
            "__rue_byte_move",
            "__rue_byte_set",
            "__rue_test_normalize_process",
            "__rue_test_complete",
            "__rue_test_failure_site",
            "__rue_test_fail",
            "__rue_test_fail_comparison",
            "__rue_test_fail_assert",
            "__rue_test_usage_error",
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
            visited: &mut [bool; 55],
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

        let mut visited = [false; 55];
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
            &[RuntimeHelperId::Alloc, RuntimeHelperId::AllocZeroed],
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
            VALID_ALLOC_LAYOUT,
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
            &[RuntimeHelperId::Resize],
            &[MUT_BYTE_POINTER, U64_VALUE, U64_VALUE, U64_VALUE],
            BOOL_WORD_RESULT,
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
            &[RuntimeHelperId::DebugFloat],
            &[U64_VALUE, U32_VALUE],
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
            &[RuntimeHelperId::ToStringFloat],
            &[STR_BUF_OUT, U64_VALUE, U32_VALUE],
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
        check(
            &mut visited,
            &[RuntimeHelperId::ArgCount, RuntimeHelperId::EnvCount],
            &[],
            U64_RESULT,
            SafetyContract::NONE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::ArgPtr, RuntimeHelperId::EnvPtr],
            &[U64_VALUE],
            MUT_POINTER_RESULT,
            SafetyContract::NONE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::ArgLen, RuntimeHelperId::EnvLen],
            &[U64_VALUE],
            U64_RESULT,
            SafetyContract::NONE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::ByteCopy, RuntimeHelperId::ByteMove],
            &[MUT_BYTE_POINTER, BYTE_VIEW, U64_VALUE],
            VOID,
            READABLE.union(WRITABLE),
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::ByteSet],
            &[MUT_BYTE_POINTER, U64_VALUE, U64_VALUE],
            VOID,
            WRITABLE,
            RETURNS,
        );
        check(
            &mut visited,
            &[
                RuntimeHelperId::TestNormalizeProcess,
                RuntimeHelperId::TestComplete,
                RuntimeHelperId::TestUsageError,
            ],
            &[],
            VOID,
            SafetyContract::NONE,
            RETURNS,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::TestFailureSite],
            &[BYTE_VIEW, U64_VALUE, U32_VALUE, U32_VALUE],
            VOID,
            READABLE,
            RETURNS,
        );
        check(
            &mut visited,
            &[
                RuntimeHelperId::TestFail,
                RuntimeHelperId::TestFailComparison,
            ],
            &[
                BYTE_VIEW, U64_VALUE, BYTE_VIEW, U64_VALUE, BYTE_VIEW, U64_VALUE,
            ],
            VOID,
            READABLE.union(TERMINATES),
            NEVER,
        );
        check(
            &mut visited,
            &[RuntimeHelperId::TestFailAssert],
            &[BYTE_VIEW, U64_VALUE, U32_VALUE],
            VOID,
            READABLE.union(TERMINATES),
            NEVER,
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
                    name: "cap",
                    ty: AbiType::U64
                },
                AggregateSlot {
                    name: "len",
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
                    name: "cap",
                    ty: AbiType::U64
                },
                AggregateSlot {
                    name: "len",
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
            RuntimeHelperId::ToStringFloat.helper().parameters,
            [STR_BUF_OUT, U64_VALUE, U32_VALUE],
            "float bits and width must cross the ABI explicitly"
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

        let free = RuntimeHelperId::Free.helper().safety;
        assert!(free.contains(SafetyContract::VALID_ALLOCATION));
        assert!(free.contains(SafetyContract::ALLOCATION_LAYOUT));
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
    fn write_syscall_numbers_are_centralized_per_supported_target() {
        assert_eq!(RuntimeTarget::X86_64Linux.write_syscall_number(), 1);
        assert_eq!(RuntimeTarget::Aarch64Linux.write_syscall_number(), 64);
        assert_eq!(RuntimeTarget::Aarch64Macos.write_syscall_number(), 4);
    }

    #[test]
    fn reserved_exports_are_separate_and_targeted() {
        assert_eq!(classify_export("rue_darwin_sigtramp"), None);
        assert_eq!(ReservedExportId::from_symbol("rue_darwin_sigtramp"), None);
        assert_eq!(RuntimeHelperId::from_symbol("rue_darwin_sigtramp"), None);
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
        assert_eq!(RUNTIME_ABI_VERSION, 5);
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
