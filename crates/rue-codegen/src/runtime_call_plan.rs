//! Manifest-derived planning for compiler calls into the Rue runtime.
//!
//! This is the single logical boundary between language/codegen policy and
//! target call emission. A plan is keyed by [`RuntimeHelperId`], validated
//! against that helper's complete manifest signature, and retains logical
//! argument materialization until a target adapter assigns physical registers.

use rue_runtime_abi::{
    AbiParameter, AbiResult, AbiType, AggregateShapeId, CallingConvention, ParameterMode,
    ReturnBehavior, RuntimeHelperId,
};

use crate::allocation::ScalePlan;
use crate::call_plan::{CallArgInput, CallMaterializer, ReturnPlan};
use crate::vreg::VReg;

/// One canonical logical runtime argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCallArg {
    Slot {
        value: VReg,
        parameter: AbiParameter,
    },
    Immediate {
        value: u64,
        parameter: AbiParameter,
    },
    Scaled {
        value: VReg,
        scale: ScalePlan,
        parameter: AbiParameter,
    },
    Extended {
        value: VReg,
        extension: crate::value_plan::IntegerExtension,
        parameter: AbiParameter,
    },
    OutPointer {
        shape: AggregateShapeId,
    },
}

impl RuntimeCallArg {
    pub const fn value(value: VReg, ty: AbiType) -> Self {
        Self::Slot {
            value,
            parameter: AbiParameter::value(ty),
        }
    }

    pub const fn const_pointer(value: VReg, ty: AbiType) -> Self {
        Self::Slot {
            value,
            parameter: AbiParameter::const_pointer(ty),
        }
    }

    pub const fn mut_pointer(value: VReg, ty: AbiType) -> Self {
        Self::Slot {
            value,
            parameter: AbiParameter::mut_pointer(ty),
        }
    }

    pub const fn immediate(value: u64, ty: AbiType) -> Self {
        Self::Immediate {
            value,
            parameter: AbiParameter::value(ty),
        }
    }

    pub const fn scaled(value: VReg, scale: ScalePlan, ty: AbiType) -> Self {
        Self::Scaled {
            value,
            scale,
            parameter: AbiParameter::value(ty),
        }
    }

    pub const fn extended(
        value: VReg,
        extension: crate::value_plan::IntegerExtension,
        ty: AbiType,
    ) -> Self {
        Self::Extended {
            value,
            extension,
            parameter: AbiParameter::value(ty),
        }
    }

    pub const fn out_pointer(shape: AggregateShapeId) -> Self {
        Self::OutPointer { shape }
    }

    pub(crate) const fn from_parameter(value: VReg, parameter: AbiParameter) -> Self {
        Self::Slot { value, parameter }
    }

    pub const fn parameter(self) -> AbiParameter {
        match self {
            Self::Slot { parameter, .. }
            | Self::Immediate { parameter, .. }
            | Self::Scaled { parameter, .. }
            | Self::Extended { parameter, .. } => parameter,
            Self::OutPointer { shape } => AbiParameter::out_pointer(shape),
        }
    }
}

/// Manifest-derived result materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCallResult {
    Void,
    Scalar(AbiType),
    OutPointer(AggregateShapeId),
}

/// A complete normalized runtime call consumed identically by both backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCallPlan {
    helper: RuntimeHelperId,
    args: Vec<RuntimeCallArg>,
    result: RuntimeCallResult,
    return_behavior: ReturnBehavior,
    calling_convention: CallingConvention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCallPlanError {
    ParameterCount {
        expected: usize,
        actual: usize,
    },
    Parameter {
        index: usize,
        expected: AbiParameter,
        actual: AbiParameter,
    },
    MultipleOutPointers,
    OutPointerWithDirectResult,
    SemanticReturn {
        expected: RuntimeCallResult,
        actual: ReturnPlan,
    },
    FlattenedSlotCount {
        expected: usize,
        actual: usize,
    },
}

impl RuntimeCallPlan {
    pub fn new(
        helper: RuntimeHelperId,
        args: impl IntoIterator<Item = RuntimeCallArg>,
    ) -> Result<Self, RuntimeCallPlanError> {
        let manifest = helper.helper();
        let args = args.into_iter().collect::<Vec<_>>();
        if args.len() != manifest.parameters.len() {
            return Err(RuntimeCallPlanError::ParameterCount {
                expected: manifest.parameters.len(),
                actual: args.len(),
            });
        }

        let mut out_shape = None;
        for (index, (arg, expected)) in args.iter().zip(manifest.parameters).enumerate() {
            let actual = arg.parameter();
            if actual != *expected {
                return Err(RuntimeCallPlanError::Parameter {
                    index,
                    expected: *expected,
                    actual,
                });
            }
            if let ParameterMode::OutPointer(shape) = actual.mode {
                if out_shape.replace(shape).is_some() {
                    return Err(RuntimeCallPlanError::MultipleOutPointers);
                }
            }
        }

        let result = match (out_shape, manifest.result) {
            (Some(_), AbiResult::Scalar(_)) => {
                return Err(RuntimeCallPlanError::OutPointerWithDirectResult);
            }
            (Some(shape), AbiResult::Void) => RuntimeCallResult::OutPointer(shape),
            (None, AbiResult::Void) => RuntimeCallResult::Void,
            (None, AbiResult::Scalar(ty)) => RuntimeCallResult::Scalar(ty),
        };

        Ok(Self {
            helper,
            args,
            result,
            return_behavior: manifest.return_behavior,
            calling_convention: manifest.calling_convention,
        })
    }

    pub fn expect_manifest(
        helper: RuntimeHelperId,
        args: impl IntoIterator<Item = RuntimeCallArg>,
    ) -> Self {
        Self::new(helper, args)
            .unwrap_or_else(|error| panic!("invalid runtime call plan for {helper:?}: {error:?}"))
    }

    pub fn no_args(helper: RuntimeHelperId) -> Self {
        Self::expect_manifest(helper, [])
    }

    /// Transitional exact bridge for CFG calls that still carry only an
    /// interned symbol. The caller must first resolve that symbol to an exact
    /// manifest helper identity; no prefix or naming convention is accepted.
    pub(crate) fn from_cfg_inputs<M: CallMaterializer>(
        helper: RuntimeHelperId,
        return_plan: ReturnPlan,
        args: &[CallArgInput],
        materializer: &mut M,
    ) -> Result<Self, RuntimeCallPlanError> {
        let mut slots = Vec::new();
        for arg in args {
            match arg {
                CallArgInput::ByRef { address, .. } => {
                    slots.push(materializer.materialize_by_ref(address));
                }
                // A by-value indirect compact aggregate (RUE-1005) only arises
                // for a Rue-target call under `aggregate_layout`; the register-
                // only TargetC memory builtins take scalars and pointers, never a
                // by-value compact aggregate, so it cannot reach a runtime call.
                CallArgInput::IndirectValue { .. } | CallArgInput::IndirectValueDispatch { .. } => {
                    unreachable!(
                        "a compiler-built memory routine cannot take a by-value indirect \
                         compact aggregate argument"
                    )
                }
                CallArgInput::Value {
                    value,
                    slot_count,
                    is_multislot_aggregate,
                    ..
                } => {
                    if *slot_count == 0 {
                        continue;
                    }
                    if *is_multislot_aggregate {
                        let materialized = materializer.materialize_aggregate(*value);
                        assert_eq!(
                            materialized.len(),
                            *slot_count as usize,
                            "aggregate materialization must produce every logical ABI slot"
                        );
                        slots.extend(materialized);
                    } else {
                        slots.push(materializer.materialize_scalar(*value));
                    }
                }
            }
        }

        let manifest = helper.helper();
        let expected_slots = manifest
            .parameters
            .iter()
            .filter(|parameter| !matches!(parameter.mode, ParameterMode::OutPointer(_)))
            .count();
        if slots.len() != expected_slots {
            return Err(RuntimeCallPlanError::FlattenedSlotCount {
                expected: expected_slots,
                actual: slots.len(),
            });
        }

        let mut slots = slots.into_iter();
        let call_args = manifest
            .parameters
            .iter()
            .map(|parameter| match parameter.mode {
                ParameterMode::OutPointer(shape) => RuntimeCallArg::out_pointer(shape),
                ParameterMode::Value | ParameterMode::ConstPointer | ParameterMode::MutPointer => {
                    RuntimeCallArg::from_parameter(
                        slots
                            .next()
                            .expect("validated flattened runtime slot count"),
                        *parameter,
                    )
                }
            });
        let plan = Self::new(helper, call_args)?;
        let return_matches = match (plan.result, return_plan) {
            (RuntimeCallResult::Void, ReturnPlan::ZeroSized)
            | (RuntimeCallResult::Scalar(_), ReturnPlan::Scalar) => true,
            (
                RuntimeCallResult::OutPointer(shape),
                ReturnPlan::Registers { slot_count } | ReturnPlan::Sret { slot_count, .. },
            ) => shape.shape().slots.len() == slot_count as usize,
            _ => false,
        };
        if !return_matches {
            return Err(RuntimeCallPlanError::SemanticReturn {
                expected: plan.result,
                actual: return_plan,
            });
        }
        Ok(plan)
    }

    pub const fn helper(&self) -> RuntimeHelperId {
        self.helper
    }

    pub fn args(&self) -> &[RuntimeCallArg] {
        &self.args
    }

    pub const fn result(&self) -> RuntimeCallResult {
        self.result
    }

    pub const fn return_behavior(&self) -> ReturnBehavior {
        self.return_behavior
    }

    pub const fn calling_convention(&self) -> CallingConvention {
        self.calling_convention
    }

    pub const fn symbol(&self) -> &'static str {
        self.helper.symbol()
    }

    pub fn out_shape(&self) -> Option<AggregateShapeId> {
        match self.result() {
            RuntimeCallResult::OutPointer(shape) => Some(shape),
            RuntimeCallResult::Void | RuntimeCallResult::Scalar(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_rejects_arbitrary_arguments_and_sret() {
        assert_eq!(
            RuntimeCallPlan::new(RuntimeHelperId::Panic, []),
            Err(RuntimeCallPlanError::ParameterCount {
                expected: 2,
                actual: 0,
            })
        );
        assert!(matches!(
            RuntimeCallPlan::new(
                RuntimeHelperId::Panic,
                [
                    RuntimeCallArg::out_pointer(AggregateShapeId::StrBufResult),
                    RuntimeCallArg::immediate(0, AbiType::U64),
                ],
            ),
            Err(RuntimeCallPlanError::Parameter { index: 0, .. })
        ));
    }

    #[test]
    fn manifest_derives_results_and_control_contracts() {
        let random = RuntimeCallPlan::no_args(RuntimeHelperId::RandomU64);
        assert_eq!(random.result, RuntimeCallResult::Scalar(AbiType::U64));
        assert_eq!(random.return_behavior, ReturnBehavior::Returns);
        assert_eq!(random.calling_convention, CallingConvention::TargetC);

        let read = RuntimeCallPlan::expect_manifest(
            RuntimeHelperId::ReadLine,
            [
                RuntimeCallArg::out_pointer(AggregateShapeId::OptionStrBufResult),
                RuntimeCallArg::immediate(1, AbiType::U64),
                RuntimeCallArg::immediate(0, AbiType::U64),
            ],
        );
        assert_eq!(
            read.result,
            RuntimeCallResult::OutPointer(AggregateShapeId::OptionStrBufResult)
        );
    }
}
