//! Shared backend pass sequencing and frame accounting (RUE-607).
//!
//! Concrete MIR, register allocation, instruction selection, scheduling facts,
//! verification rules, and emission remain target-specific. The order in which
//! those passes run — and the distinction between spill-placement slots and
//! emitted-frame locals — is common to every machine-code emission entry point.

use rue_air::{ArgClass, ArgConvention, FrozenTypeInternPool, NativeCallAbi, ReturnClass};
use rue_cfg::{Cfg, CfgArgMode, CfgInstData, CfgValue};
use rue_error::{CompileError, CompileResult, ErrorKind};

use crate::frame_layout::{FrameLayout, SavedRegScheme};

/// How the callee prologue homes one source parameter's incoming argument
/// registers into its frame parameter slots (ADR-0052 phase 5.8, RUE-1005).
///
/// The historical prologue assumed one incoming register per parameter slot. A
/// by-value indirect compact aggregate breaks that: it arrives as one pointer
/// register (`reg_count == 1`) yet reserves `slot_count` frame slots, so
/// subsequent parameters' incoming registers shift. Each entry homes `reg_count`
/// consecutive incoming registers starting at the running argument index into
/// frame parameter slots `[start_slot, start_slot + reg_count)`; the parameter's
/// remaining reserved slots (for an indirect aggregate) are filled lazily by the
/// body from the homed pointer. With the gate off every parameter is direct and
/// `reg_count == slot_count`, so the emitted prologue is byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParamHoming {
    pub(crate) start_slot: u32,
    pub(crate) reg_count: u32,
}

/// Build the callee prologue's per-source-parameter homing plan from the CFG's
/// grouped descriptors, each carrying its precomputed incoming-register crossing
/// width (RUE-1005). Falls back to one register per parameter slot when the CFG
/// carries no grouped source-parameter layout (a directly constructed CFG in
/// synthetic tests), reproducing the historical prologue exactly.
pub(crate) fn param_homing_plan(cfg: &Cfg) -> Vec<ParamHoming> {
    let source_params = cfg.source_param_abi();
    if source_params.is_empty() {
        return (0..cfg.num_params())
            .map(|slot| ParamHoming {
                start_slot: slot,
                reg_count: 1,
            })
            .collect();
    }
    source_params
        .iter()
        .map(|param| ParamHoming {
            start_slot: param.start_slot,
            reg_count: param.crossing_regs,
        })
        .collect()
}

/// A target's MIR after allocation, peephole optimization, scheduling, and
/// stack verification, together with the frame metadata its emitter needs.
pub(crate) struct PreparedMir<M, R> {
    pub(crate) mir: M,
    pub(crate) total_locals: u32,
    pub(crate) num_locals_original: u32,
    pub(crate) num_params: u32,
    pub(crate) has_sret: bool,
    pub(crate) used_callee_saved: Vec<R>,
    /// Per-source-parameter prologue homing plan (RUE-1005).
    pub(crate) param_homing: Vec<ParamHoming>,
    /// Validated final layout consumed by emission and presentation paths.
    pub(crate) frame_layout: FrameLayout,
}

pub(crate) fn frame_budget_error(cfg: &Cfg, value: Option<CfgValue>) -> CompileError {
    let kind = ErrorKind::FunctionFrameTooLarge {
        max_bytes: rue_air::layout::MAX_FUNCTION_FRAME_BYTES,
    };
    value
        .map(|value| CompileError::new(kind.clone(), cfg.get_inst(value).span))
        .unwrap_or_else(|| CompileError::without_span(kind))
}

pub(crate) fn checked_slot_sum(parts: impl IntoIterator<Item = u32>) -> Option<u32> {
    parts.into_iter().try_fold(0_u32, u32::checked_add)
}

/// Reject every base-frame and outgoing-call calculation before lowering can
/// narrow it to a backend immediate or allocate proportional metadata.
pub(crate) fn validate_pre_lowering_budget(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    arg_reg_count: u32,
    return_reg_count: u32,
    scheme: SavedRegScheme,
) -> CompileResult<bool> {
    let return_class =
        NativeCallAbi::new(type_pool, return_reg_count).classify_return(cfg.return_type());
    let has_sret = return_class.uses_sret();
    let base_slots = checked_slot_sum([cfg.num_locals(), cfg.num_params(), u32::from(has_sret)])
        .ok_or_else(|| frame_budget_error(cfg, None))?;
    FrameLayout::try_new(scheme, 0, base_slots).map_err(|_| frame_budget_error(cfg, None))?;

    let argument_abi = NativeCallAbi::for_arguments(type_pool);
    for raw in 0..cfg.value_count() {
        let value = CfgValue::from_raw(raw as u32);
        let inst = cfg.get_inst(value);
        let CfgInstData::Call { .. } = &inst.data else {
            continue;
        };
        let return_class = NativeCallAbi::new(type_pool, return_reg_count).classify_return(inst.ty);
        let (mut abi_slots, sret_bytes) = match return_class {
            ReturnClass::Indirect { slot_count } => {
                let bytes = u64::from(slot_count)
                    .checked_mul(rue_air::layout::SLOT_BYTES)
                    .ok_or_else(|| frame_budget_error(cfg, Some(value)))?;
                let bytes = crate::frame_layout::checked_aligned_region_bytes(bytes)
                    .map_err(|_| frame_budget_error(cfg, Some(value)))?;
                (1_u32, u64::from(bytes))
            }
            _ => (0_u32, 0),
        };
        let mut indirect_bytes = 0_u64;
        for arg in cfg.get_call_args(&inst.data) {
            let ty = cfg.get_inst(arg.value).ty;
            let convention = match arg.mode {
                CfgArgMode::Normal => ArgConvention::ByValue,
                CfgArgMode::Inout | CfgArgMode::Borrow => ArgConvention::ByReference,
            };
            let class = argument_abi.classify_arg(ty, convention);
            abi_slots = abi_slots
                .checked_add(class.crossing_slots())
                .ok_or_else(|| frame_budget_error(cfg, Some(value)))?;
            if arg.mode == CfgArgMode::Normal && class == ArgClass::Indirect {
                let bytes =
                    crate::frame_layout::checked_aligned_region_bytes(type_pool.layout(ty).size)
                        .map_err(|_| frame_budget_error(cfg, Some(value)))?;
                indirect_bytes = indirect_bytes
                    .checked_add(u64::from(bytes))
                    .ok_or_else(|| frame_budget_error(cfg, Some(value)))?;
            }
        }
        let stack_slots = u64::from(abi_slots.saturating_sub(arg_reg_count));
        crate::frame_layout::checked_call_area_bytes(stack_slots, sret_bytes, indirect_bytes)
            .map_err(|_| frame_budget_error(cfg, Some(value)))?;
    }
    Ok(has_sret)
}

/// Run the target-independent backend pipeline around concrete pass hooks.
///
/// The closures monomorphize for each backend; there is no dynamic dispatch or
/// universal backend trait. Keeping the two slot formulas here is deliberate:
///
/// - `existing_slots` includes locals, parameters, and the optional incoming
///   sret pointer so register-allocation spills cannot overlap any of them.
/// - `total_locals` includes only original locals and new spill slots because
///   emitters account for parameters and sret separately.
///
/// Run the canonical backend pipeline while carrying optional diagnostic
/// observations alongside the same lowering and allocation execution.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_mir_with_artifacts<M, R, D, Lower, Allocate, Peephole, Schedule, Verify>(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    arg_reg_count: u32,
    return_reg_count: u32,
    scheme: SavedRegScheme,
    lower: Lower,
    allocate: Allocate,
    peephole: Peephole,
    schedule: Schedule,
    verify: Verify,
) -> CompileResult<(PreparedMir<M, R>, D)>
where
    Lower: FnOnce() -> CompileResult<(M, D)>,
    Allocate: FnOnce(M, u32, &mut D) -> CompileResult<(M, u32, Vec<R>)>,
    Peephole: FnOnce(&mut M),
    Schedule: FnOnce(&mut M),
    Verify: FnOnce(&M) -> CompileResult<()>,
{
    let num_locals_original = cfg.num_locals();
    let num_params = cfg.num_params();
    let has_sret =
        validate_pre_lowering_budget(cfg, type_pool, arg_reg_count, return_reg_count, scheme)?;
    let param_homing = param_homing_plan(cfg);

    let (mir, mut artifacts) = lower()?;
    let existing_slots = checked_slot_sum([num_locals_original, num_params, u32::from(has_sret)])
        .ok_or_else(|| frame_budget_error(cfg, None))?;
    let (mut mir, num_spills, used_callee_saved) = allocate(mir, existing_slots, &mut artifacts)?;

    peephole(&mut mir);
    schedule(&mut mir);
    verify(&mir)?;

    let total_locals = num_locals_original
        .checked_add(num_spills)
        .ok_or_else(|| frame_budget_error(cfg, None))?;
    let total_slots = checked_slot_sum([total_locals, num_params, u32::from(has_sret)])
        .ok_or_else(|| frame_budget_error(cfg, None))?;
    let frame_layout = FrameLayout::try_new(scheme, used_callee_saved.len(), total_slots)
        .map_err(|_| frame_budget_error(cfg, None))?;

    Ok((
        PreparedMir {
            mir,
            total_locals,
            num_locals_original,
            num_params,
            has_sret,
            used_callee_saved,
            param_homing,
            frame_layout,
        },
        artifacts,
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use lasso::Spur;
    use rue_air::{Type, TypeInternPool};
    use rue_cfg::{Cfg, CfgArgMode, CfgCallArg, CfgInst, CfgInstData};
    use rue_span::Span;

    use super::{SavedRegScheme, prepare_mir_with_artifacts, validate_pre_lowering_budget};

    #[test]
    fn pass_order_and_frame_slot_formulas_are_single_source() {
        let type_pool = TypeInternPool::new();
        let array_id = type_pool.intern_array_from_type(Type::I32, 7);
        let type_pool = type_pool.freeze();
        let cfg = Cfg::new(
            Type::new_array(array_id),
            3,
            2,
            "pipeline_test".to_owned(),
            vec![false, false],
        );
        let events = RefCell::new(Vec::new());

        // A seven-slot return exceeds the six-register budget, so spill
        // placement sees 3 locals + 2 params + 1 sret-pointer slot. Four
        // spills then produce 3 + 4 emitted locals (not 3 + 2 + 1 + 4).
        let (prepared, ()) = prepare_mir_with_artifacts(
            &cfg,
            &type_pool,
            6,
            6,
            SavedRegScheme::X86_64,
            || {
                events.borrow_mut().push("lower");
                Ok((10_u32, ()))
            },
            |mir, existing_slots, _artifacts| {
                events.borrow_mut().push("allocate");
                assert_eq!(existing_slots, 6);
                Ok((mir + 1, 4, vec![5_u8]))
            },
            |mir| {
                events.borrow_mut().push("peephole");
                *mir += 2;
            },
            |mir| {
                events.borrow_mut().push("schedule");
                *mir += 3;
            },
            |mir| {
                events.borrow_mut().push("verify");
                assert_eq!(*mir, 16);
                Ok(())
            },
        )
        .expect("synthetic pipeline should succeed");

        assert_eq!(
            events.into_inner(),
            ["lower", "allocate", "peephole", "schedule", "verify"]
        );
        assert_eq!(prepared.mir, 16);
        assert_eq!(prepared.total_locals, 7);
        assert_eq!(prepared.num_locals_original, 3);
        assert_eq!(prepared.num_params, 2);
        assert!(prepared.has_sret);
        assert_eq!(prepared.used_callee_saved, [5]);
    }

    #[test]
    fn target_frame_boundaries_are_checked_before_lowering() {
        let type_pool = TypeInternPool::new().freeze();
        let max_slots = rue_air::layout::MAX_FUNCTION_FRAME_SLOTS as u32;
        let cfg = Cfg::new(Type::UNIT, max_slots, 0, "boundary".into(), vec![]);

        assert!(
            validate_pre_lowering_budget(&cfg, &type_pool, 6, 6, SavedRegScheme::X86_64,).is_ok()
        );
        assert!(
            validate_pre_lowering_budget(&cfg, &type_pool, 8, 8, SavedRegScheme::Aarch64,).is_err(),
            "AArch64's mandatory FP/LR save must count against the same budget"
        );

        let param_boundary = Cfg::new(Type::UNIT, 0, max_slots, "param_boundary".into(), vec![]);
        assert!(
            validate_pre_lowering_budget(
                &param_boundary,
                &type_pool,
                6,
                6,
                SavedRegScheme::X86_64,
            )
            .is_ok()
        );
    }

    #[test]
    fn aggregate_argument_buffers_and_sret_share_one_call_area_budget() {
        let type_pool = TypeInternPool::new();
        let array_id = type_pool.intern_array_from_type(Type::I32, 134_217_728);
        let huge = Type::new_array(array_id);
        let type_pool = type_pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "call_budget".into(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let args = (0..4)
            .map(|_| {
                let value = cfg.append_inst(
                    entry,
                    CfgInst {
                        data: CfgInstData::Const(0),
                        ty: huge,
                        span: Span::default(),
                    },
                );
                CfgCallArg {
                    value,
                    mode: CfgArgMode::Normal,
                }
            })
            .collect::<Vec<_>>();
        cfg.append_call(entry, None, Spur::default(), args, huge, Span::default())
            .unwrap();

        for (arg_regs, ret_regs, scheme) in [
            (6, 6, SavedRegScheme::X86_64),
            (8, 8, SavedRegScheme::Aarch64),
        ] {
            let error = validate_pre_lowering_budget(&cfg, &type_pool, arg_regs, ret_regs, scheme)
                .unwrap_err();
            assert!(matches!(
                error.kind,
                rue_error::ErrorKind::FunctionFrameTooLarge { .. }
            ));
        }
    }
}
