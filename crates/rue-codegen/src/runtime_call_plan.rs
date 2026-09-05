//! Manifest-derived planning for compiler calls into the Rue runtime.
//!
//! This is the single logical boundary between language/codegen policy and
//! target call emission. A plan is keyed by [`RuntimeHelperId`], validated
//! against that helper's complete manifest signature, and retains logical
//! argument materialization until a target adapter assigns physical registers.

use rue_runtime_abi::{
    AbiParameter, AbiResult, AbiType, AggregateShapeId, ParameterMode, ReturnBehavior,
    RuntimeHelperId, RuntimeTarget,
};
use rue_target::{CallingConvention, Target};

use crate::allocation::ScalePlan;
use crate::call_plan::{CallArgInput, CallMaterializer, ReturnPlan};
use crate::vreg::VReg;

/// The one correspondence between the compiler's [`Target`] and the runtime
/// manifest's dependency-light [`RuntimeTarget`] mirror.
///
/// `rue-runtime-abi` is a `no_std` leaf that cannot depend on `rue-target`
/// (ADR-0055), so it names targets in its own enum. This pair of renamings is
/// the bridge, and it carries no ABI policy of its own: a manifest-side target
/// reaches the single `"C"` alias table by becoming a [`Target`] first. Both
/// directions are total, and the crate's tests pin them as mutual inverses.
pub const fn runtime_target(target: Target) -> RuntimeTarget {
    match target {
        Target::X86_64Linux => RuntimeTarget::X86_64Linux,
        Target::Aarch64Linux => RuntimeTarget::Aarch64Linux,
        Target::Aarch64Macos => RuntimeTarget::Aarch64Macos,
    }
}

/// The [`Target`] a manifest-side [`RuntimeTarget`] names; the inverse of
/// [`runtime_target`].
pub const fn compiler_target(target: RuntimeTarget) -> Target {
    match target {
        RuntimeTarget::X86_64Linux => Target::X86_64Linux,
        RuntimeTarget::Aarch64Linux => Target::Aarch64Linux,
        RuntimeTarget::Aarch64Macos => Target::Aarch64Macos,
    }
}

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

    /// The convention a runtime-helper call crosses under on `target`.
    ///
    /// The helper boundary is a `"C"` call like any other, so it resolves
    /// through the one alias table rather than carrying a convention of its
    /// own: every compiler-callable helper in the manifest is a C call, and
    /// which C row that is depends only on the compilation target. This is an
    /// associated function, not a method, because the plan itself holds nothing
    /// target-specific.
    pub const fn calling_convention(target: Target) -> CallingConvention {
        CallingConvention::c_for_target(target)
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

    /// Every `RuntimeTarget` the manifest can name. `RuntimeTarget` has no
    /// `ALL`, so this list is exhaustive by construction: adding a variant makes
    /// [`compiler_target`] fail to compile and this list fail its round trip.
    const RUNTIME_TARGETS: [RuntimeTarget; 3] = [
        RuntimeTarget::X86_64Linux,
        RuntimeTarget::Aarch64Linux,
        RuntimeTarget::Aarch64Macos,
    ];

    #[test]
    fn the_target_correspondence_is_a_total_bijection() {
        for target in Target::all() {
            assert_eq!(compiler_target(runtime_target(*target)), *target);
        }
        for target in RUNTIME_TARGETS {
            assert_eq!(runtime_target(compiler_target(target)), target);
        }
        assert_eq!(Target::all().len(), RUNTIME_TARGETS.len());
    }

    #[test]
    fn the_helper_boundary_resolves_through_the_one_c_alias_table() {
        // Total over every `Target`, total over every `RuntimeTarget`, and the
        // two agree: a manifest-side target resolves the alias by becoming a
        // compiler target first, so there is one mapping table and not two.
        for target in Target::all() {
            let convention = RuntimeCallPlan::calling_convention(*target);
            assert_eq!(
                convention,
                CallingConvention::c_for_target(*target),
                "the runtime-helper boundary is the target's `\"C\"` convention"
            );
            assert!(convention.is_c());
            assert_eq!(
                convention,
                CallingConvention::c_for_target(compiler_target(runtime_target(*target))),
                "the `RuntimeTarget` route must reach the same row"
            );
        }
        for target in RUNTIME_TARGETS {
            assert!(CallingConvention::c_for_target(compiler_target(target)).is_c());
        }
        assert_eq!(
            RuntimeCallPlan::calling_convention(Target::Aarch64Macos),
            CallingConvention::Aarch64AapcsDarwin
        );
        assert_eq!(
            RuntimeCallPlan::calling_convention(Target::Aarch64Linux),
            CallingConvention::Aarch64Aapcs
        );
        assert_eq!(
            RuntimeCallPlan::calling_convention(Target::X86_64Linux),
            CallingConvention::X86_64SysV
        );
    }

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
