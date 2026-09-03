//! Compiler-owned descriptions of semantic operands at the runtime ABI.

use rue_runtime_abi::{
    AbiParameter, AbiResult, AbiType, AggregateShapeId, ParameterMode, ReturnBehavior,
    RuntimeHelperId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeOperandOrigin {
    OutResult(AggregateShapeId),
    ValueArgument {
        index: u8,
        ty: AbiType,
    },
    SignExtendedArgument(u8),
    ZeroExtendedArgument(u8),
    BoolWordArgument(u8),
    AbiExtendedIntegerArgument(u8),
    MutablePointerArgument {
        index: u8,
        source: RuntimeAirType,
    },
    /// A `ptr const u8` / `ptr mut u8` argument passed straight through to a
    /// `const u8*` runtime parameter (the `@byte_copy`/`@byte_move` source,
    /// ADR-0059).
    BytePointerArgument(u8),
    TextPointer(u8),
    TextLength(u8),
    ProjectedTextPointer(u8),
    ProjectedTextLength(u8),
    OptionDiscriminant(OptionVariant),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptionVariant {
    Some,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeCallActivation {
    Always,
    WhenArgumentIsFalse(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeAirType {
    Unit,
    U8,
    I64,
    U64,
    U32,
    Bool,
    SignedInteger,
    UnsignedInteger,
    Integer,
    Text,
    BytePointer,
    ConstBytePointer,
    MutPointer,
    MutBytePointer,
    StrBuf,
    OptionStrBuf,
    OptionI32,
    OptionI64,
    OptionU32,
    OptionU64,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeAirArgument {
    pub ty: RuntimeAirType,
    pub mode: crate::AirArgMode,
}

impl RuntimeOperandOrigin {
    fn accepts(self, parameter: AbiParameter) -> bool {
        match self {
            Self::OutResult(shape) => parameter.mode == ParameterMode::OutPointer(shape),
            Self::TextPointer(_) | Self::ProjectedTextPointer(_) | Self::BytePointerArgument(_) => {
                parameter.ty == AbiType::Byte && parameter.mode == ParameterMode::ConstPointer
            }
            Self::TextLength(_) | Self::ProjectedTextLength(_) | Self::OptionDiscriminant(_) => {
                parameter.ty == AbiType::U64 && parameter.mode == ParameterMode::Value
            }
            Self::ValueArgument { ty, .. } => {
                parameter.ty == ty && parameter.mode == ParameterMode::Value
            }
            Self::SignExtendedArgument(_) => {
                parameter.ty == AbiType::I64 && parameter.mode == ParameterMode::Value
            }
            Self::ZeroExtendedArgument(_) | Self::AbiExtendedIntegerArgument(_) => {
                parameter.ty == AbiType::U64 && parameter.mode == ParameterMode::Value
            }
            Self::BoolWordArgument(_) => {
                parameter.ty == AbiType::BoolWordI64 && parameter.mode == ParameterMode::Value
            }
            Self::MutablePointerArgument { .. } => {
                parameter.ty == AbiType::Byte && parameter.mode == ParameterMode::MutPointer
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeCallKind {
    StrByteAt,
    StrCharScalar,
    StrCharNext,
    StrCharScalarLossy,
    StrCharNextLossy,
    ToString,
    ToStringUnsigned,
    StrPrintAggregate,
    StrPrintProjected,
    StrPrintlnAggregate,
    StrPrintlnProjected,
    DebugI64,
    DebugU64,
    DebugBool,
    DebugStr,
    Panic,
    PanicNoMessage,
    AssertFailed,
    AssertWithMessage,
    /// Typed identity for compiler-inserted slice bounds traps. This shares
    /// the `assert` intrinsic's conditional shape while selecting the
    /// dedicated `__rue_bounds_check` runtime helper in codegen.
    BoundsCheck,
    ReadLine,
    ParseI32,
    ParseI64,
    ParseU32,
    ParseU64,
    RandomU32,
    RandomU64,
    Alloc,
    AllocZeroed,
    Free,
    Realloc,
    Resize,
    ArgCount,
    ArgPtr,
    ArgLen,
    EnvCount,
    EnvPtr,
    EnvLen,
    ByteCopy,
    ByteMove,
    ByteSet,
    /// Present the pinned test-visible process inventory (ADR-0083 §3).
    TestNormalizeProcess,
    /// Write the dispatcher's terminal completion frame (ADR-0083 §3).
    TestComplete,
    /// Stage the source location the next failure record carries.
    TestFailureSite,
    /// Report a structured failure on the §5.1 channel, then abort.
    TestFail,
    /// Write the pinned malformed-selector diagnostic and return.
    TestUsageError,
}

const STR_BYTE_AT: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::TextPointer(0),
    RuntimeOperandOrigin::TextLength(0),
    RuntimeOperandOrigin::AbiExtendedIntegerArgument(1),
];
const CHAR_INDEX: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::ProjectedTextPointer(0),
    RuntimeOperandOrigin::ProjectedTextLength(1),
    RuntimeOperandOrigin::ValueArgument {
        index: 2,
        ty: AbiType::U64,
    },
];
const FORMAT_SIGNED: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::OutResult(AggregateShapeId::StrBufResult),
    RuntimeOperandOrigin::ValueArgument {
        index: 0,
        ty: AbiType::I64,
    },
];
const FORMAT_UNSIGNED: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::OutResult(AggregateShapeId::StrBufResult),
    RuntimeOperandOrigin::ValueArgument {
        index: 0,
        ty: AbiType::U64,
    },
];
const TEXT: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::TextPointer(0),
    RuntimeOperandOrigin::TextLength(0),
];
const PROJECTED_TEXT: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::ProjectedTextPointer(0),
    RuntimeOperandOrigin::ProjectedTextLength(1),
];
const SIGNED_SCALAR: &[RuntimeOperandOrigin] = &[RuntimeOperandOrigin::SignExtendedArgument(0)];
const UNSIGNED_SCALAR: &[RuntimeOperandOrigin] = &[RuntimeOperandOrigin::ZeroExtendedArgument(0)];
const BOOL_SCALAR: &[RuntimeOperandOrigin] = &[RuntimeOperandOrigin::BoolWordArgument(0)];
const NONE: &[RuntimeOperandOrigin] = &[];
const READ_LINE: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::OutResult(AggregateShapeId::OptionStrBufResult),
    RuntimeOperandOrigin::OptionDiscriminant(OptionVariant::Some),
    RuntimeOperandOrigin::OptionDiscriminant(OptionVariant::None),
];
const PARSE: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::OutResult(AggregateShapeId::OptionIntResult),
    RuntimeOperandOrigin::TextPointer(0),
    RuntimeOperandOrigin::TextLength(0),
    RuntimeOperandOrigin::OptionDiscriminant(OptionVariant::Some),
    RuntimeOperandOrigin::OptionDiscriminant(OptionVariant::None),
];
// The unified allocation family is byte-shaped end to end (ADR-0059 Phase 3,
// RUE-961): `@alloc(size, align)` and `@alloc_zeroed(size, align)` hand their
// two `u64` operands straight to the runtime helper, so no operand is derived
// from a pointee type. Typed allocation is source-computed sugar over
// `@size_of`/`@align_of` and never reaches this table.
const ALLOC: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::ValueArgument {
        index: 0,
        ty: AbiType::U64,
    },
    RuntimeOperandOrigin::ValueArgument {
        index: 1,
        ty: AbiType::U64,
    },
];
// The indexed process-inventory accessors (`@arg_ptr`/`@arg_len`/`@env_ptr`/
// `@env_len`, RUE-935) all pass a single `u64` index straight through to the
// runtime helper.
const PROCESS_INDEX: &[RuntimeOperandOrigin] = &[RuntimeOperandOrigin::ValueArgument {
    index: 0,
    ty: AbiType::U64,
}];
// `@free(p, size, align)` — the sizeless-allocator ABI hands the block's layout
// back to the runtime (ADR-0059 Phase 3, RUE-961).
const FREE: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::MutablePointerArgument {
        index: 0,
        source: RuntimeAirType::MutBytePointer,
    },
    RuntimeOperandOrigin::ValueArgument {
        index: 1,
        ty: AbiType::U64,
    },
    RuntimeOperandOrigin::ValueArgument {
        index: 2,
        ty: AbiType::U64,
    },
];
// `@realloc(p, old_size, align, new_size)` and `@resize(p, old_size, align,
// new_size)` map onto the runtime helper signatures `__rue_realloc(ptr,
// old_size, new_size, align)` and `__rue_resize(ptr, old_size, new_size,
// align)`, so the alignment operand (AIR argument 2) is passed last while
// `new_size` (AIR argument 3) precedes it. Keeping the intrinsic's `align`
// ahead of `new_size` keeps `(p, old_size, align, ...)` identical across
// `@free`, `@realloc`, and `@resize` at the call site.
const RESIZE_LAYOUT: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::MutablePointerArgument {
        index: 0,
        source: RuntimeAirType::MutBytePointer,
    },
    RuntimeOperandOrigin::ValueArgument {
        index: 1,
        ty: AbiType::U64,
    },
    RuntimeOperandOrigin::ValueArgument {
        index: 3,
        ty: AbiType::U64,
    },
    RuntimeOperandOrigin::ValueArgument {
        index: 2,
        ty: AbiType::U64,
    },
];
// `@byte_copy` and `@byte_move` share one operand plan and differ only in the
// helper they select (memcpy vs memmove, RUE-937 / RUE-964).
const BYTE_COPY: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::MutablePointerArgument {
        index: 0,
        source: RuntimeAirType::MutBytePointer,
    },
    RuntimeOperandOrigin::BytePointerArgument(1),
    RuntimeOperandOrigin::ValueArgument {
        index: 2,
        ty: AbiType::U64,
    },
];
const BYTE_SET: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::MutablePointerArgument {
        index: 0,
        source: RuntimeAirType::MutBytePointer,
    },
    RuntimeOperandOrigin::ZeroExtendedArgument(1),
    RuntimeOperandOrigin::ValueArgument {
        index: 2,
        ty: AbiType::U64,
    },
];

// The ADR-0083 §5.1 failure record, in the two calls a register-only helper
// budget affords: `__rue_test_failure_site(file, line, column)` stages the
// location, then `__rue_test_fail(kind, message, payload)` emits the record and
// aborts. `payload` is the open, versioned field an assertion library fills in.
const TEST_FAILURE_SITE: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::TextPointer(0),
    RuntimeOperandOrigin::TextLength(0),
    RuntimeOperandOrigin::ValueArgument {
        index: 1,
        ty: AbiType::U32,
    },
    RuntimeOperandOrigin::ValueArgument {
        index: 2,
        ty: AbiType::U32,
    },
];
const TEST_FAIL: &[RuntimeOperandOrigin] = &[
    RuntimeOperandOrigin::TextPointer(0),
    RuntimeOperandOrigin::TextLength(0),
    RuntimeOperandOrigin::TextPointer(1),
    RuntimeOperandOrigin::TextLength(1),
    RuntimeOperandOrigin::TextPointer(2),
    RuntimeOperandOrigin::TextLength(2),
];

impl RuntimeCallKind {
    pub const ALL: [Self; 46] = [
        Self::StrByteAt,
        Self::StrCharScalar,
        Self::StrCharNext,
        Self::StrCharScalarLossy,
        Self::StrCharNextLossy,
        Self::ToString,
        Self::ToStringUnsigned,
        Self::StrPrintAggregate,
        Self::StrPrintProjected,
        Self::StrPrintlnAggregate,
        Self::StrPrintlnProjected,
        Self::DebugI64,
        Self::DebugU64,
        Self::DebugBool,
        Self::DebugStr,
        Self::Panic,
        Self::PanicNoMessage,
        Self::AssertFailed,
        Self::AssertWithMessage,
        Self::BoundsCheck,
        Self::ReadLine,
        Self::ParseI32,
        Self::ParseI64,
        Self::ParseU32,
        Self::ParseU64,
        Self::RandomU32,
        Self::RandomU64,
        Self::Alloc,
        Self::AllocZeroed,
        Self::Free,
        Self::Realloc,
        Self::Resize,
        Self::ArgCount,
        Self::ArgPtr,
        Self::ArgLen,
        Self::EnvCount,
        Self::EnvPtr,
        Self::EnvLen,
        Self::ByteCopy,
        Self::ByteMove,
        Self::ByteSet,
        Self::TestNormalizeProcess,
        Self::TestComplete,
        Self::TestFailureSite,
        Self::TestFail,
        Self::TestUsageError,
    ];

    pub const fn helper(self) -> RuntimeHelperId {
        match self {
            Self::StrByteAt => RuntimeHelperId::StrByteAt,
            Self::StrCharScalar => RuntimeHelperId::StrCharScalar,
            Self::StrCharNext => RuntimeHelperId::StrCharNext,
            Self::StrCharScalarLossy => RuntimeHelperId::StrCharScalarLossy,
            Self::StrCharNextLossy => RuntimeHelperId::StrCharNextLossy,
            Self::ToString => RuntimeHelperId::ToString,
            Self::ToStringUnsigned => RuntimeHelperId::ToStringUnsigned,
            Self::StrPrintAggregate | Self::StrPrintProjected => RuntimeHelperId::StrPrint,
            Self::StrPrintlnAggregate | Self::StrPrintlnProjected => RuntimeHelperId::StrPrintln,
            Self::DebugI64 => RuntimeHelperId::DebugI64,
            Self::DebugU64 => RuntimeHelperId::DebugU64,
            Self::DebugBool => RuntimeHelperId::DebugBool,
            Self::DebugStr => RuntimeHelperId::DebugStr,
            Self::Panic => RuntimeHelperId::Panic,
            Self::PanicNoMessage => RuntimeHelperId::PanicNoMessage,
            Self::AssertFailed => RuntimeHelperId::AssertFailed,
            Self::AssertWithMessage => RuntimeHelperId::Panic,
            Self::BoundsCheck => RuntimeHelperId::BoundsCheck,
            Self::ReadLine => RuntimeHelperId::ReadLine,
            Self::ParseI32 => RuntimeHelperId::ParseI32,
            Self::ParseI64 => RuntimeHelperId::ParseI64,
            Self::ParseU32 => RuntimeHelperId::ParseU32,
            Self::ParseU64 => RuntimeHelperId::ParseU64,
            Self::RandomU32 => RuntimeHelperId::RandomU32,
            Self::RandomU64 => RuntimeHelperId::RandomU64,
            Self::Alloc => RuntimeHelperId::Alloc,
            Self::AllocZeroed => RuntimeHelperId::AllocZeroed,
            Self::Free => RuntimeHelperId::Free,
            Self::Realloc => RuntimeHelperId::Realloc,
            Self::Resize => RuntimeHelperId::Resize,
            Self::ArgCount => RuntimeHelperId::ArgCount,
            Self::ArgPtr => RuntimeHelperId::ArgPtr,
            Self::ArgLen => RuntimeHelperId::ArgLen,
            Self::EnvCount => RuntimeHelperId::EnvCount,
            Self::EnvPtr => RuntimeHelperId::EnvPtr,
            Self::EnvLen => RuntimeHelperId::EnvLen,
            Self::ByteCopy => RuntimeHelperId::ByteCopy,
            Self::ByteMove => RuntimeHelperId::ByteMove,
            Self::ByteSet => RuntimeHelperId::ByteSet,
            Self::TestNormalizeProcess => RuntimeHelperId::TestNormalizeProcess,
            Self::TestComplete => RuntimeHelperId::TestComplete,
            Self::TestFailureSite => RuntimeHelperId::TestFailureSite,
            Self::TestFail => RuntimeHelperId::TestFail,
            Self::TestUsageError => RuntimeHelperId::TestUsageError,
        }
    }

    pub const fn operands(self) -> &'static [RuntimeOperandOrigin] {
        match self {
            Self::StrByteAt => STR_BYTE_AT,
            Self::StrCharScalar
            | Self::StrCharNext
            | Self::StrCharScalarLossy
            | Self::StrCharNextLossy => CHAR_INDEX,
            Self::ToString => FORMAT_SIGNED,
            Self::ToStringUnsigned => FORMAT_UNSIGNED,
            Self::StrPrintAggregate | Self::StrPrintlnAggregate | Self::DebugStr | Self::Panic => {
                TEXT
            }
            Self::StrPrintProjected | Self::StrPrintlnProjected => PROJECTED_TEXT,
            Self::AssertWithMessage => &[
                RuntimeOperandOrigin::TextPointer(1),
                RuntimeOperandOrigin::TextLength(1),
            ],
            Self::DebugI64 => SIGNED_SCALAR,
            Self::DebugU64 => UNSIGNED_SCALAR,
            Self::DebugBool => BOOL_SCALAR,
            Self::PanicNoMessage
            | Self::AssertFailed
            | Self::BoundsCheck
            | Self::RandomU32
            | Self::RandomU64
            | Self::ArgCount
            | Self::EnvCount
            | Self::TestNormalizeProcess
            | Self::TestComplete
            | Self::TestUsageError => NONE,
            Self::ArgPtr | Self::ArgLen | Self::EnvPtr | Self::EnvLen => PROCESS_INDEX,
            Self::ReadLine => READ_LINE,
            Self::ParseI32 | Self::ParseI64 | Self::ParseU32 | Self::ParseU64 => PARSE,
            Self::Alloc | Self::AllocZeroed => ALLOC,
            Self::Free => FREE,
            Self::Realloc | Self::Resize => RESIZE_LAYOUT,
            Self::ByteCopy | Self::ByteMove => BYTE_COPY,
            Self::ByteSet => BYTE_SET,
            Self::TestFailureSite => TEST_FAILURE_SITE,
            Self::TestFail => TEST_FAIL,
        }
    }

    pub const fn activation(self) -> RuntimeCallActivation {
        match self {
            Self::AssertFailed | Self::AssertWithMessage | Self::BoundsCheck => {
                RuntimeCallActivation::WhenArgumentIsFalse(0)
            }
            _ => RuntimeCallActivation::Always,
        }
    }

    pub fn validate(self) -> bool {
        let parameters = self.helper().helper().parameters;
        parameters.len() == self.operands().len()
            && self
                .operands()
                .iter()
                .zip(parameters)
                .all(|(o, p)| o.accepts(*p))
    }

    pub fn validate_air_arguments(self, arguments: &[RuntimeAirArgument]) -> bool {
        Self::validate_air_arguments_for_plan(self.operands(), self.activation(), arguments)
    }

    pub fn validate_air_call(
        self,
        arguments: &[RuntimeAirArgument],
        result: RuntimeAirType,
    ) -> bool {
        self.validate_air_arguments(arguments) && self.validate_air_result(result)
    }

    pub fn validate_air_result(self, result: RuntimeAirType) -> bool {
        let helper = self.helper().helper();
        self.validate_air_result_for_manifest(helper.result, helper.return_behavior, result)
    }

    fn validate_air_result_for_manifest(
        self,
        helper_result: AbiResult,
        return_behavior: ReturnBehavior,
        result: RuntimeAirType,
    ) -> bool {
        if return_behavior == ReturnBehavior::Never && helper_result != AbiResult::Void {
            return false;
        }
        let expected = match self.activation() {
            RuntimeCallActivation::WhenArgumentIsFalse(_) => {
                if return_behavior != ReturnBehavior::Never {
                    return false;
                }
                RuntimeAirType::Unit
            }
            RuntimeCallActivation::Always => {
                if return_behavior == ReturnBehavior::Never {
                    RuntimeAirType::Never
                } else if let Some(shape) = self.out_result_shape() {
                    if helper_result != AbiResult::Void {
                        return false;
                    }
                    match shape {
                        AggregateShapeId::StrBufResult => RuntimeAirType::StrBuf,
                        AggregateShapeId::OptionStrBufResult => RuntimeAirType::OptionStrBuf,
                        AggregateShapeId::OptionIntResult => match self {
                            Self::ParseI32 => RuntimeAirType::OptionI32,
                            Self::ParseI64 => RuntimeAirType::OptionI64,
                            Self::ParseU32 => RuntimeAirType::OptionU32,
                            Self::ParseU64 => RuntimeAirType::OptionU64,
                            _ => return false,
                        },
                    }
                } else if helper_result == AbiResult::Scalar(AbiType::MutBytePointer) {
                    self.pointer_result_type()
                } else {
                    self.logical_scalar_result()
                        .unwrap_or_else(|| Self::air_type_for_abi_result(helper_result))
                }
            }
        };
        if result == RuntimeAirType::Never {
            expected == RuntimeAirType::Never
        } else {
            Self::air_type_accepts(expected, result)
        }
    }

    fn out_result_shape(self) -> Option<AggregateShapeId> {
        let mut shape = None;
        for operand in self.operands() {
            if let RuntimeOperandOrigin::OutResult(candidate) = operand {
                if shape.replace(*candidate).is_some() {
                    return None;
                }
            }
        }
        shape
    }

    fn pointer_result_type(self) -> RuntimeAirType {
        self.operands()
            .iter()
            .find_map(|operand| match operand {
                RuntimeOperandOrigin::MutablePointerArgument { source, .. } => Some(*source),
                _ => None,
            })
            .unwrap_or(RuntimeAirType::MutBytePointer)
    }

    fn logical_scalar_result(self) -> Option<RuntimeAirType> {
        match self {
            Self::StrByteAt => Some(RuntimeAirType::U8),
            Self::StrCharScalar | Self::StrCharScalarLossy => Some(RuntimeAirType::U32),
            _ => None,
        }
    }

    fn validate_air_arguments_for_plan(
        operands: &[RuntimeOperandOrigin],
        activation: RuntimeCallActivation,
        arguments: &[RuntimeAirArgument],
    ) -> bool {
        let mut requirements = vec![None; arguments.len()];
        let mut require = |index: u8, ty: RuntimeAirType| {
            let Some(slot) = requirements.get_mut(index as usize) else {
                return false;
            };
            match *slot {
                Some(existing) => existing == ty,
                None => {
                    *slot = Some(ty);
                    true
                }
            }
        };
        for operand in operands {
            let valid = match *operand {
                RuntimeOperandOrigin::OutResult(_)
                | RuntimeOperandOrigin::OptionDiscriminant(_) => true,
                RuntimeOperandOrigin::ValueArgument { index, ty } => {
                    require(index, Self::air_type_for_abi_value(ty))
                }
                RuntimeOperandOrigin::SignExtendedArgument(index) => {
                    require(index, RuntimeAirType::SignedInteger)
                }
                RuntimeOperandOrigin::ZeroExtendedArgument(index) => {
                    require(index, RuntimeAirType::UnsignedInteger)
                }
                RuntimeOperandOrigin::BoolWordArgument(index) => {
                    require(index, RuntimeAirType::Bool)
                }
                RuntimeOperandOrigin::AbiExtendedIntegerArgument(index) => {
                    require(index, RuntimeAirType::Integer)
                }
                RuntimeOperandOrigin::MutablePointerArgument { index, source } => {
                    matches!(
                        source,
                        RuntimeAirType::MutPointer | RuntimeAirType::MutBytePointer
                    ) && require(index, source)
                }
                RuntimeOperandOrigin::TextPointer(index)
                | RuntimeOperandOrigin::TextLength(index) => require(index, RuntimeAirType::Text),
                RuntimeOperandOrigin::BytePointerArgument(index) => {
                    require(index, RuntimeAirType::BytePointer)
                }
                RuntimeOperandOrigin::ProjectedTextPointer(index) => {
                    require(index, RuntimeAirType::BytePointer)
                }
                RuntimeOperandOrigin::ProjectedTextLength(index) => {
                    require(index, RuntimeAirType::U64)
                }
            };
            if !valid {
                return false;
            }
        }
        if let RuntimeCallActivation::WhenArgumentIsFalse(index) = activation
            && !require(index, RuntimeAirType::Bool)
        {
            return false;
        }
        requirements
            .iter()
            .zip(arguments)
            .all(|(expected, actual)| {
                expected.is_some_and(|expected| Self::air_type_accepts(expected, actual.ty))
                    && actual.mode == crate::AirArgMode::Normal
            })
    }

    fn air_type_for_abi_value(ty: AbiType) -> RuntimeAirType {
        match ty {
            AbiType::I32 => RuntimeAirType::SignedInteger,
            AbiType::I64 => RuntimeAirType::I64,
            AbiType::U32 => RuntimeAirType::U32,
            AbiType::U64 | AbiType::Usize => RuntimeAirType::U64,
            AbiType::BoolWordI64 => RuntimeAirType::Bool,
            AbiType::Byte => RuntimeAirType::UnsignedInteger,
            AbiType::MutBytePointer => RuntimeAirType::MutBytePointer,
        }
    }

    fn air_type_for_abi_result(result: AbiResult) -> RuntimeAirType {
        match result {
            AbiResult::Void => RuntimeAirType::Unit,
            AbiResult::Scalar(AbiType::Byte) => RuntimeAirType::U8,
            AbiResult::Scalar(ty) => Self::air_type_for_abi_value(ty),
        }
    }

    fn air_type_accepts(expected: RuntimeAirType, actual: RuntimeAirType) -> bool {
        use RuntimeAirType as T;
        if actual == T::Never {
            return true;
        }
        match expected {
            T::SignedInteger => matches!(actual, T::SignedInteger | T::I64),
            T::UnsignedInteger => {
                matches!(actual, T::UnsignedInteger | T::U64 | T::U32)
            }
            T::Integer => matches!(
                actual,
                T::Integer | T::SignedInteger | T::UnsignedInteger | T::I64 | T::U64 | T::U32
            ),
            T::BytePointer => matches!(
                actual,
                T::BytePointer | T::ConstBytePointer | T::MutBytePointer
            ),
            // No runtime helper takes a non-`u8` pointer since the allocation
            // family became byte-shaped (ADR-0059 Phase 3), but the AIR
            // classification still distinguishes the two, and a `ptr mut u8`
            // satisfies a plan written for any mutable pointer.
            T::MutPointer => matches!(actual, T::MutPointer | T::MutBytePointer),
            _ => expected == actual,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_runtime_call_plan_matches_the_manifest() {
        for kind in RuntimeCallKind::ALL {
            assert!(kind.validate(), "{kind:?}");
        }
        assert_eq!(
            RuntimeCallKind::AssertWithMessage.activation(),
            RuntimeCallActivation::WhenArgumentIsFalse(0)
        );
        assert_eq!(
            RuntimeCallKind::Panic.activation(),
            RuntimeCallActivation::Always
        );
        assert_eq!(
            RuntimeCallKind::BoundsCheck.helper(),
            RuntimeHelperId::BoundsCheck
        );
        assert_eq!(
            RuntimeCallKind::BoundsCheck.activation(),
            RuntimeCallActivation::WhenArgumentIsFalse(0)
        );
    }

    #[test]
    fn air_argument_validation_rejects_arity_type_and_mode_drift() {
        let normal = |ty| RuntimeAirArgument {
            ty,
            mode: crate::AirArgMode::Normal,
        };
        assert!(RuntimeCallKind::ToString.validate_air_arguments(&[normal(RuntimeAirType::I64)]));
        assert!(!RuntimeCallKind::ToString.validate_air_arguments(&[normal(RuntimeAirType::U64)]));
        assert!(
            !RuntimeCallKind::Alloc
                .validate_air_arguments(&[normal(RuntimeAirType::SignedInteger)])
        );
        assert!(
            !RuntimeCallKind::StrCharNext.validate_air_arguments(&[
                normal(RuntimeAirType::Text),
                normal(RuntimeAirType::U64),
            ])
        );
        assert!(
            !RuntimeCallKind::AssertFailed.validate_air_arguments(&[RuntimeAirArgument {
                ty: RuntimeAirType::Bool,
                mode: crate::AirArgMode::Borrow,
            }])
        );
    }

    #[test]
    fn operand_origin_validation_rejects_out_of_range_and_wrong_source_types() {
        let normal = |ty| RuntimeAirArgument {
            ty,
            mode: crate::AirArgMode::Normal,
        };
        assert!(!RuntimeCallKind::validate_air_arguments_for_plan(
            &[RuntimeOperandOrigin::ValueArgument {
                index: 1,
                ty: AbiType::U64,
            }],
            RuntimeCallActivation::Always,
            &[normal(RuntimeAirType::U64)],
        ));
        assert!(!RuntimeCallKind::validate_air_arguments_for_plan(
            &[RuntimeOperandOrigin::TextPointer(0)],
            RuntimeCallActivation::Always,
            &[normal(RuntimeAirType::U64)],
        ));
        assert!(!RuntimeCallKind::validate_air_arguments_for_plan(
            &[RuntimeOperandOrigin::MutablePointerArgument {
                index: 0,
                source: RuntimeAirType::MutBytePointer,
            }],
            RuntimeCallActivation::Always,
            &[normal(RuntimeAirType::MutPointer)],
        ));
    }

    #[test]
    fn layout_returning_plans_require_a_byte_pointer_and_u64_sizes() {
        let normal = |ty| RuntimeAirArgument {
            ty,
            mode: crate::AirArgMode::Normal,
        };
        // `@realloc`/`@resize`: (ptr mut u8, old_size, align, new_size).
        let layout = [
            normal(RuntimeAirType::MutBytePointer),
            normal(RuntimeAirType::U64),
            normal(RuntimeAirType::U64),
            normal(RuntimeAirType::U64),
        ];
        assert!(RuntimeCallKind::Realloc.validate_air_arguments(&layout));
        assert!(RuntimeCallKind::Resize.validate_air_arguments(&layout));
        // The pointer operand is not interchangeable with a size operand.
        assert!(!RuntimeCallKind::Realloc.validate_air_arguments(&[
            normal(RuntimeAirType::U64),
            normal(RuntimeAirType::U64),
            normal(RuntimeAirType::U64),
            normal(RuntimeAirType::MutBytePointer),
        ]));
        // `@free`: (ptr mut u8, size, align) — one operand shorter.
        assert!(!RuntimeCallKind::Free.validate_air_arguments(&layout));
        assert!(RuntimeCallKind::Free.validate_air_arguments(&layout[..3]));
    }

    #[test]
    fn result_validation_rejects_aggregate_pointer_and_control_flow_drift() {
        assert!(RuntimeCallKind::ToString.validate_air_result(RuntimeAirType::StrBuf));
        assert!(!RuntimeCallKind::ToString.validate_air_result(RuntimeAirType::Text));
        assert!(RuntimeCallKind::ReadLine.validate_air_result(RuntimeAirType::OptionStrBuf));
        assert!(!RuntimeCallKind::ReadLine.validate_air_result(RuntimeAirType::StrBuf));
        for (kind, expected, wrong_width) in [
            (
                RuntimeCallKind::ParseI32,
                RuntimeAirType::OptionI32,
                RuntimeAirType::OptionI64,
            ),
            (
                RuntimeCallKind::ParseI64,
                RuntimeAirType::OptionI64,
                RuntimeAirType::OptionI32,
            ),
            (
                RuntimeCallKind::ParseU32,
                RuntimeAirType::OptionU32,
                RuntimeAirType::OptionU64,
            ),
            (
                RuntimeCallKind::ParseU64,
                RuntimeAirType::OptionU64,
                RuntimeAirType::OptionU32,
            ),
        ] {
            assert!(kind.validate_air_result(expected));
            assert!(!kind.validate_air_result(wrong_width));
            assert!(!kind.validate_air_result(RuntimeAirType::OptionStrBuf));
        }
        for kind in [RuntimeCallKind::Alloc, RuntimeCallKind::AllocZeroed] {
            assert!(kind.validate_air_result(RuntimeAirType::MutBytePointer));
            assert!(!kind.validate_air_result(RuntimeAirType::U64));
        }
        // `@resize` reports success as a `bool`, not as a pointer.
        assert!(RuntimeCallKind::Resize.validate_air_result(RuntimeAirType::Bool));
        assert!(!RuntimeCallKind::Resize.validate_air_result(RuntimeAirType::MutBytePointer));
        assert!(RuntimeCallKind::Realloc.validate_air_result(RuntimeAirType::MutBytePointer));
        assert!(!RuntimeCallKind::Realloc.validate_air_result(RuntimeAirType::Bool));
        // The overlapping and non-overlapping bulk moves are both statements.
        assert!(RuntimeCallKind::ByteMove.validate_air_result(RuntimeAirType::Unit));
        assert!(!RuntimeCallKind::ByteMove.validate_air_result(RuntimeAirType::U64));
        assert!(RuntimeCallKind::Panic.validate_air_result(RuntimeAirType::Never));
        assert!(!RuntimeCallKind::Panic.validate_air_result(RuntimeAirType::Unit));
        assert!(RuntimeCallKind::AssertFailed.validate_air_result(RuntimeAirType::Unit));
        assert!(!RuntimeCallKind::AssertFailed.validate_air_result(RuntimeAirType::Never));
        assert!(RuntimeCallKind::RandomU32.validate_air_result(RuntimeAirType::U32));
        assert!(!RuntimeCallKind::RandomU32.validate_air_result(RuntimeAirType::U64));
        assert!(RuntimeCallKind::RandomU64.validate_air_result(RuntimeAirType::U64));
        assert!(!RuntimeCallKind::RandomU64.validate_air_result(RuntimeAirType::U32));
        assert!(RuntimeCallKind::StrByteAt.validate_air_result(RuntimeAirType::U8));
        assert!(!RuntimeCallKind::StrByteAt.validate_air_result(RuntimeAirType::U32));
        assert!(
            !RuntimeCallKind::AssertFailed.validate_air_result_for_manifest(
                AbiResult::Scalar(AbiType::U64),
                ReturnBehavior::Never,
                RuntimeAirType::Unit,
            )
        );
    }

    #[test]
    fn abi_scalar_result_mapping_preserves_exact_widths_and_bool() {
        assert_eq!(
            RuntimeCallKind::air_type_for_abi_result(AbiResult::Scalar(AbiType::I64)),
            RuntimeAirType::I64
        );
        assert_eq!(
            RuntimeCallKind::air_type_for_abi_result(AbiResult::Scalar(AbiType::U32)),
            RuntimeAirType::U32
        );
        assert_eq!(
            RuntimeCallKind::air_type_for_abi_result(AbiResult::Scalar(AbiType::U64)),
            RuntimeAirType::U64
        );
        assert_eq!(
            RuntimeCallKind::air_type_for_abi_result(AbiResult::Scalar(AbiType::BoolWordI64)),
            RuntimeAirType::Bool
        );
        assert_eq!(
            RuntimeCallKind::air_type_for_abi_result(AbiResult::Void),
            RuntimeAirType::Unit
        );
    }
}
