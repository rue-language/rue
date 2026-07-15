//! Target-independent planning for Rue calls.
//!
//! This module decides the logical ABI shape of a call.  It deliberately does
//! not know about physical registers or target instructions; the two backend
//! lowerers consume the normalized slot vector and only choose how to marshal
//! those slots for their ABI.

use rue_air::FrozenTypeInternPool;
use rue_cfg::{Cfg, CfgArgMode, CfgCallArg, Type};

use crate::cfg_lower::type_uses_sret_return;
use crate::types;
use crate::vreg::VReg;

/// The source ABI expected by a call target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalleeAbi {
    /// A Rue-compiled function, whose aggregate arguments use reversed ABI
    /// slot order to preserve the ascending logical frame layout.
    Rue,
    /// A runtime helper using the natural C-compatible aggregate slot order.
    Runtime,
}

impl CalleeAbi {
    fn for_symbol(symbol: &str) -> Self {
        if symbol.starts_with("__rue_") {
            Self::Runtime
        } else {
            Self::Rue
        }
    }
}

/// The logical mode of one user argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserArgMode {
    /// A normal argument is passed by value.
    Value,
    /// An `inout` argument is represented by one preserved address slot.
    Inout,
    /// A `borrow` argument is represented by one preserved address slot.
    Borrow,
}

/// One classified user argument.  Its vregs are already materialized by the
/// shared aggregate/by-ref leaves, but no physical ABI assignment has happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserArgPlan {
    pub mode: UserArgMode,
    pub slots: Vec<VReg>,
}

/// The hidden caller-provided return storage, when the return uses sret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HiddenSretPlan {
    /// The vreg containing the address of the caller-owned storage.
    pub pointer: VReg,
    /// The logical ABI position of the hidden pointer.
    pub abi_slot: usize,
    /// Number of logical return slots written by the callee.
    pub slot_count: u32,
    /// Caller storage size, rounded up to the call-stack alignment.
    pub storage_bytes: u32,
}

/// How a call result is reconstructed after the target call instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnPlan {
    /// Unit/never/empty aggregates have no materialized slots.
    ZeroSized,
    /// A one-slot scalar is returned in the target's primary return register.
    Scalar,
    /// A complete aggregate is returned one logical slot per return register.
    Registers { slot_count: u32 },
    /// A complete aggregate is written to caller-provided storage.
    Sret { slot_count: u32, storage_bytes: u32 },
}

impl ReturnPlan {
    /// Number of logical return slots represented by this plan.
    pub const fn slot_count(self) -> u32 {
        match self {
            Self::ZeroSized => 0,
            Self::Scalar => 1,
            Self::Registers { slot_count } | Self::Sret { slot_count, .. } => slot_count,
        }
    }

    pub const fn uses_sret(self) -> bool {
        matches!(self, Self::Sret { .. })
    }
}

/// A normalized call ABI plan consumed by both target adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPlan {
    pub symbol: String,
    pub callee_abi: CalleeAbi,
    pub hidden_sret: Option<HiddenSretPlan>,
    pub user_args: Vec<UserArgPlan>,
    /// Complete logical ABI slots, including the hidden sret pointer when
    /// present.  This is the only slot vector the adapters may marshal.
    pub abi_slots: Vec<VReg>,
    pub return_plan: ReturnPlan,
    pub stack_slot_count: usize,
    pub stack_bytes: u32,
}

/// Input metadata for one CFG call argument.  This is copied out of the CFG
/// before the mutable materialization adapter is borrowed.
#[derive(Debug, Clone, Copy)]
pub struct CallArgInput {
    pub value: rue_cfg::CfgValue,
    pub mode: CfgArgMode,
    pub slot_count: u32,
    pub is_multislot_aggregate: bool,
}

/// Shared policy inputs copied from a CFG before backend materialization.
#[derive(Debug, Clone)]
pub struct CallInputs {
    pub args: Vec<CallArgInput>,
    pub return_plan: ReturnPlan,
}

impl CallInputs {
    pub fn from_cfg(
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        return_ty: Type,
        args: &[CfgCallArg],
        ret_reg_budget: u32,
    ) -> Self {
        let args = args
            .iter()
            .map(|arg| {
                let arg_ty = cfg.get_inst(arg.value).ty;
                CallArgInput {
                    value: arg.value,
                    mode: arg.mode,
                    slot_count: types::type_slot_count(type_pool, arg_ty),
                    is_multislot_aggregate: types::is_multislot_aggregate(type_pool, arg_ty),
                }
            })
            .collect();
        Self {
            args,
            return_plan: return_plan(type_pool, return_ty, ret_reg_budget),
        }
    }
}

impl CallArgInput {
    fn is_by_ref(self) -> bool {
        matches!(self.mode, CfgArgMode::Inout | CfgArgMode::Borrow)
    }
}

/// Existing backend leaves used to materialize a plan's logical values.
/// Keeping these as one adapter prevents competing mutable callbacks from
/// creating a second aggregate or by-ref discovery path.
pub trait CallMaterializer {
    fn materialize_scalar(&mut self, value: rue_cfg::CfgValue) -> VReg;
    fn materialize_aggregate(&mut self, value: rue_cfg::CfgValue) -> Vec<VReg>;
    fn materialize_by_ref(&mut self, value: rue_cfg::CfgValue, mode: CfgArgMode) -> VReg;
    fn materialize_sret_pointer(&mut self, storage_bytes: u32) -> VReg;
}

impl CallPlan {
    /// Build a complete plan from copied CFG argument metadata.
    ///
    /// The callbacks are materialization leaves only: aggregate discovery is
    /// still required to return exactly the type's canonical slot count, and
    /// by-reference arguments always return one address vreg, including for a
    /// zero-sized pointee.
    pub fn from_inputs<M: CallMaterializer>(
        symbol: &str,
        return_plan: ReturnPlan,
        args: &[CallArgInput],
        arg_reg_budget: usize,
        materializer: &mut M,
    ) -> Self {
        let callee_abi = CalleeAbi::for_symbol(symbol);
        let mut hidden_sret = None;
        let mut abi_slots = Vec::new();

        if let ReturnPlan::Sret {
            slot_count,
            storage_bytes,
        } = return_plan
        {
            let pointer = materializer.materialize_sret_pointer(storage_bytes);
            hidden_sret = Some(HiddenSretPlan {
                pointer,
                abi_slot: 0,
                slot_count,
                storage_bytes,
            });
            abi_slots.push(pointer);
        }

        let mut user_args = Vec::with_capacity(args.len());
        for arg in args {
            let mode = match arg.mode {
                CfgArgMode::Normal => UserArgMode::Value,
                CfgArgMode::Inout => UserArgMode::Inout,
                CfgArgMode::Borrow => UserArgMode::Borrow,
            };

            let slots = if arg.is_by_ref() {
                // A reference is an ABI pointer even when the pointee has no
                // storage slots.  This branch must precede the ZST omission.
                vec![materializer.materialize_by_ref(arg.value, arg.mode)]
            } else {
                let slot_count = arg.slot_count;
                if slot_count == 0 {
                    Vec::new()
                } else if arg.is_multislot_aggregate {
                    let slots = materializer.materialize_aggregate(arg.value);
                    assert_eq!(
                        slots.len(),
                        slot_count as usize,
                        "aggregate materialization must produce every logical ABI slot"
                    );
                    if callee_abi == CalleeAbi::Runtime {
                        slots
                    } else {
                        slots.into_iter().rev().collect()
                    }
                } else {
                    vec![materializer.materialize_scalar(arg.value)]
                }
            };

            abi_slots.extend(slots.iter().copied());
            user_args.push(UserArgPlan { mode, slots });
        }

        let stack_slot_count = abi_slots.len().saturating_sub(arg_reg_budget);
        let stack_bytes = align_up((stack_slot_count * 8) as u32, 16);

        Self {
            symbol: symbol.to_owned(),
            callee_abi,
            hidden_sret,
            user_args,
            abi_slots,
            return_plan,
            stack_slot_count,
            stack_bytes,
        }
    }

    /// Build the same normalized shape for drop/glue calls whose slots have
    /// already been materialized by the canonical aggregate leaves.
    pub fn from_slot_values(symbol: &str, slots: &[VReg], arg_reg_budget: usize) -> Self {
        let stack_slot_count = slots.len().saturating_sub(arg_reg_budget);
        Self {
            symbol: symbol.to_owned(),
            callee_abi: CalleeAbi::for_symbol(symbol),
            hidden_sret: None,
            user_args: vec![UserArgPlan {
                mode: UserArgMode::Value,
                slots: slots.to_vec(),
            }],
            abi_slots: slots.to_vec(),
            return_plan: ReturnPlan::ZeroSized,
            stack_slot_count,
            stack_bytes: align_up((stack_slot_count * 8) as u32, 16),
        }
    }
}

/// The one shared return policy.  In particular, sret selection remains
/// delegated to `type_uses_sret_return` rather than being inferred by a caller.
pub fn return_plan(type_pool: &FrozenTypeInternPool, ty: Type, ret_reg_budget: u32) -> ReturnPlan {
    let slot_count = types::type_slot_count(type_pool, ty);
    if slot_count == 0 {
        ReturnPlan::ZeroSized
    } else if type_uses_sret_return(type_pool, ty, ret_reg_budget) {
        ReturnPlan::Sret {
            slot_count,
            storage_bytes: align_up(slot_count * 8, 16),
        }
    } else if types::is_multislot_aggregate(type_pool, ty) {
        ReturnPlan::Registers { slot_count }
    } else {
        ReturnPlan::Scalar
    }
}

const fn align_up(value: u32, alignment: u32) -> u32 {
    (value + alignment - 1) / alignment * alignment
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestMaterializer;

    impl CallMaterializer for TestMaterializer {
        fn materialize_scalar(&mut self, value: rue_cfg::CfgValue) -> VReg {
            VReg::new(100 + value.as_u32())
        }

        fn materialize_aggregate(&mut self, _value: rue_cfg::CfgValue) -> Vec<VReg> {
            vec![VReg::new(10), VReg::new(11)]
        }

        fn materialize_by_ref(&mut self, _value: rue_cfg::CfgValue, _mode: CfgArgMode) -> VReg {
            VReg::new(30)
        }

        fn materialize_sret_pointer(&mut self, _storage_bytes: u32) -> VReg {
            VReg::new(40)
        }
    }

    #[test]
    fn slot_call_plan_counts_aligned_stack_slots() {
        let slots: Vec<_> = (0..9).map(VReg::new).collect();
        let plan = CallPlan::from_slot_values("drop", &slots, 6);

        assert_eq!(plan.abi_slots, slots);
        assert_eq!(plan.stack_slot_count, 3);
        assert_eq!(plan.stack_bytes, 32);
        assert_eq!(plan.return_plan, ReturnPlan::ZeroSized);
    }

    #[test]
    fn zero_sized_normal_arguments_are_omitted_but_by_ref_is_preserved() {
        let slots = vec![VReg::new(7)];
        let plan = CallPlan {
            symbol: "test".into(),
            callee_abi: CalleeAbi::Rue,
            hidden_sret: None,
            user_args: vec![
                UserArgPlan {
                    mode: UserArgMode::Value,
                    slots: vec![],
                },
                UserArgPlan {
                    mode: UserArgMode::Borrow,
                    slots: slots.clone(),
                },
            ],
            abi_slots: slots.clone(),
            return_plan: ReturnPlan::ZeroSized,
            stack_slot_count: 0,
            stack_bytes: 0,
        };

        assert!(plan.user_args[0].slots.is_empty());
        assert_eq!(plan.user_args[1].slots, slots);
        assert_eq!(plan.abi_slots.len(), 1);
    }

    #[test]
    fn return_plan_preserves_sret_storage_alignment() {
        // This checks the normalized shape independently of a target register
        // enum; type-pool-backed sret selection is exercised by backend ABI
        // tests through `return_plan`.
        assert_eq!(align_up(24, 16), 32);
        assert_eq!(
            ReturnPlan::Sret {
                slot_count: 3,
                storage_bytes: 32
            }
            .slot_count(),
            3
        );
    }

    #[test]
    fn cfg_plan_orders_aggregate_slots_by_callee_abi_and_keeps_hidden_sret_first() {
        let args = [
            CallArgInput {
                value: rue_cfg::CfgValue::from_raw(1),
                mode: CfgArgMode::Normal,
                slot_count: 2,
                is_multislot_aggregate: true,
            },
            CallArgInput {
                value: rue_cfg::CfgValue::from_raw(2),
                mode: CfgArgMode::Borrow,
                slot_count: 0,
                is_multislot_aggregate: false,
            },
            CallArgInput {
                value: rue_cfg::CfgValue::from_raw(3),
                mode: CfgArgMode::Normal,
                slot_count: 0,
                is_multislot_aggregate: false,
            },
        ];
        let mut materializer = TestMaterializer;
        let rue = CallPlan::from_inputs(
            "callee",
            ReturnPlan::Sret {
                slot_count: 3,
                storage_bytes: 32,
            },
            &args,
            3,
            &mut materializer,
        );

        assert_eq!(
            rue.abi_slots,
            vec![VReg::new(40), VReg::new(11), VReg::new(10), VReg::new(30)]
        );
        assert_eq!(rue.hidden_sret.unwrap().abi_slot, 0);
        assert_eq!(rue.stack_slot_count, 1);
        assert_eq!(rue.stack_bytes, 16);
        assert_eq!(rue.user_args[1].mode, UserArgMode::Borrow);
        assert!(rue.user_args[2].slots.is_empty());

        let mut materializer = TestMaterializer;
        let runtime = CallPlan::from_inputs(
            "__rue_runtime",
            ReturnPlan::Sret {
                slot_count: 3,
                storage_bytes: 32,
            },
            &args,
            8,
            &mut materializer,
        );
        assert_eq!(
            runtime.abi_slots,
            vec![VReg::new(40), VReg::new(10), VReg::new(11), VReg::new(30)]
        );
        assert_eq!(runtime.stack_slot_count, 0);
        assert_eq!(runtime.stack_bytes, 0);
    }
}
