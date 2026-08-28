//! Shared backend pass sequencing and frame accounting (RUE-607).
//!
//! Concrete MIR, register allocation, instruction selection, scheduling facts,
//! verification rules, and emission remain target-specific. The order in which
//! those passes run — and the distinction between spill-placement slots and
//! emitted-frame locals — is common to every machine-code emission entry point.

use rue_air::{ArgClass, ArgConvention, FrozenTypeInternPool, NativeCallAbi, ReturnClass};
use rue_cfg::{Cfg, CfgArgMode, CfgInstData, CfgValue};
use rue_error::{CompileError, CompileResult, ErrorKind};
use tracing::info_span;

use crate::frame_layout::{FrameLayout, FramePointer, SavedRegScheme};

/// How the callee prologue homes one source parameter's incoming argument
/// registers into its frame parameter slots (ADR-0052 phase 5.8, RUE-1005).
///
/// The historical prologue assumed one incoming register per parameter slot. A
/// by-value indirect compact aggregate breaks that: it arrives as one pointer
/// register (`reg_count == 1`) yet reserves `slot_count` frame slots, so
/// subsequent parameters' incoming registers shift. Each entry homes `reg_count`
/// consecutive incoming registers starting at ABI argument index `abi_start`
/// (sret shift included, RUE-1170) into frame parameter slots
/// `[start_slot, start_slot + reg_count)`; the parameter's remaining reserved
/// slots (for an indirect aggregate) are filled lazily by the body from the
/// homed pointer. `start_slot` is the parameter's position within the
/// *compacted* frame parameter area: register-only parameters (RUE-1170) get
/// no entry at all and reserve no slots, so homed parameters pack together
/// while their `abi_start` still names the original incoming register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParamHoming {
    pub(crate) start_slot: u32,
    pub(crate) reg_count: u32,
    pub(crate) abi_start: u32,
}

/// Decide whether the final frame needs a frame pointer and a slot region, or
/// whether the function is an eligible frameless leaf (RUE-1171).
///
/// Two facts make a frame unavoidable:
///
/// - **Any frame slot.** A local, a register-allocator spill, a homed
///   parameter, or the incoming sret pointer cell is addressed FP-relative, so
///   the frame pointer and the slot region both have to exist. Conversely
///   `total_slots == 0` means nothing in the body can name a frame location:
///   every FP-based address this backend emits — lowering's local, parameter,
///   and sret addressing and allocation's spill reloads — is derived from a
///   slot index. The emitters assert this rather than assume it, and raise an
///   internal error if an FP-relative operand reaches a frameless function.
/// - **Any call.** A call needs the call-boundary stack alignment the slot
///   region's rounding establishes, and on AArch64 it clobbers the link
///   register that the FP/LR save preserves. Only leaves are eligible.
pub(crate) fn plan_frame_pointer(total_slots: u32, is_leaf: bool) -> FramePointer {
    if total_slots == 0 && is_leaf {
        FramePointer::Omitted
    } else {
        FramePointer::Established
    }
}

/// A target's MIR after allocation, peephole optimization, scheduling, and
/// stack verification, together with the frame metadata its emitter needs.
pub(crate) struct PreparedMir<M, R> {
    pub(crate) mir: M,
    pub(crate) total_locals: u32,
    /// Frame slots the local area occupies after marker-driven slot sharing
    /// (RUE-768), and therefore the base of the emitted parameter area. This
    /// is at most `cfg.num_locals()`; spill slots are counted separately in
    /// `total_locals`.
    pub(crate) frame_local_slots: u32,
    pub(crate) has_sret: bool,
    pub(crate) used_callee_saved: Vec<R>,
    /// Per-source-parameter prologue homing plan (RUE-1005), covering only
    /// the homed parameters (RUE-1170).
    pub(crate) param_homing: Vec<ParamHoming>,
    /// Per-parameter storage decision shared by lowering, emission, and the
    /// stack-frame reporter (RUE-1170).
    pub(crate) param_storage: crate::param_storage::ParamStoragePlan,
    /// Per-local frame-cell decision (RUE-768), carried for the same reason:
    /// the stack-frame reporter names which CFG locals share a cell.
    pub(crate) local_storage: crate::local_storage::LocalSlotPlan,
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
#[cfg(test)]
pub(crate) fn validate_pre_lowering_budget(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    arg_reg_count: u32,
    return_reg_count: u32,
    scheme: SavedRegScheme,
) -> CompileResult<bool> {
    validate_pre_lowering_budget_for_target(
        cfg,
        type_pool,
        arg_reg_count,
        return_reg_count,
        scheme,
        rue_air::TargetCAbiFlavor::SysVAmd64,
        &|_| false,
    )
}

pub(crate) fn validate_pre_lowering_budget_for_target(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    arg_reg_count: u32,
    return_reg_count: u32,
    scheme: SavedRegScheme,
    target_c_flavor: rue_air::TargetCAbiFlavor,
    is_foreign_symbol: &dyn Fn(lasso::Spur) -> bool,
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
        let CfgInstData::Call { name, .. } = &inst.data else {
            continue;
        };
        let call_args = cfg.get_call_args(&inst.data);
        if is_foreign_symbol(*name)
            && crate::foreign_call::ForeignCallInputs::call_touches_aggregate(
                cfg, inst.ty, call_args,
            )
        {
            crate::foreign_call::ForeignCallInputs::checked_call_area_bytes(
                cfg,
                type_pool,
                inst.ty,
                call_args,
                target_c_flavor,
            )
            .map_err(|_| frame_budget_error(cfg, Some(value)))?;
            continue;
        }
        let return_class = NativeCallAbi::new(type_pool, return_reg_count).classify_return(inst.ty);
        let (mut abi_slots, sret_bytes) = match return_class {
            ReturnClass::Indirect { slot_count } => {
                let bytes =
                    crate::frame_layout::checked_aligned_cell_region_bytes(u64::from(slot_count))
                        .map_err(|_| frame_budget_error(cfg, Some(value)))?;
                (1_u32, u64::from(bytes))
            }
            _ => (0_u32, 0),
        };
        let mut indirect_bytes = 0_u64;
        for arg in call_args {
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
/// - `existing_slots` includes the shared local area (RUE-768), the *homed*
///   parameters (RUE-1170), and the optional incoming sret pointer so
///   register-allocation spills cannot overlap any of them.
/// - `total_locals` includes only the shared local area and new spill slots
///   because emitters account for parameters and sret separately.
///
/// Run the canonical backend pipeline while carrying optional diagnostic
/// observations alongside the same lowering and allocation execution.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_mir_with_artifacts<
    M,
    R,
    D,
    Lower,
    Allocate,
    Peephole,
    Schedule,
    Verify,
    IsLeaf,
>(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    interner: &lasso::ThreadedRodeo,
    arg_reg_count: u32,
    return_reg_count: u32,
    scheme: SavedRegScheme,
    target_c_flavor: rue_air::TargetCAbiFlavor,
    is_foreign_symbol: &dyn Fn(lasso::Spur) -> bool,
    lower: Lower,
    allocate: Allocate,
    peephole: Peephole,
    schedule: Schedule,
    verify: Verify,
    is_leaf: IsLeaf,
) -> CompileResult<(PreparedMir<M, R>, D)>
where
    Lower: FnOnce(
        &crate::param_storage::ParamStoragePlan,
        &crate::local_storage::LocalSlotPlan,
    ) -> CompileResult<(M, D)>,
    Allocate: FnOnce(M, u32, &mut D) -> CompileResult<(M, u32, Vec<R>)>,
    Peephole: FnOnce(&mut M),
    Schedule: FnOnce(&mut M),
    Verify: FnOnce(&M) -> CompileResult<()>,
    IsLeaf: FnOnce(&M) -> bool,
{
    let has_sret = validate_pre_lowering_budget_for_target(
        cfg,
        type_pool,
        arg_reg_count,
        return_reg_count,
        scheme,
        target_c_flavor,
        is_foreign_symbol,
    )?;
    // The per-parameter storage decision (RUE-1170) is computed once here and
    // shared by lowering (body addressing and entry copies), the frame slot
    // sums below, and the emitter's prologue homing, so they cannot disagree
    // about which parameters have frame homes.
    let param_storage =
        crate::param_storage::ParamStoragePlan::plan(cfg, type_pool, has_sret, arg_reg_count);
    let param_homing = param_storage.homing().to_vec();
    let homed_param_slots = param_storage.homed_area_slots();
    // The local frame-slot decision (RUE-768) is target-independent and is
    // likewise computed once: lowering addresses through it, the sums below
    // place the parameter area and the spill floor above it, and the emitters
    // size the frame from it.
    let local_storage = crate::local_storage::LocalSlotPlan::plan(cfg, type_pool, interner);
    let frame_local_slots = local_storage.frame_local_slots();

    let (mir, mut artifacts) = {
        let _span = info_span!("mir_lowering").entered();
        lower(&param_storage, &local_storage)?
    };
    let existing_slots =
        checked_slot_sum([frame_local_slots, homed_param_slots, u32::from(has_sret)])
            .ok_or_else(|| frame_budget_error(cfg, None))?;
    let (mut mir, num_spills, used_callee_saved) = {
        let _span = info_span!("register_allocation").entered();
        allocate(mir, existing_slots, &mut artifacts)?
    };

    {
        let _span = info_span!("mir_peephole").entered();
        peephole(&mut mir);
    }
    {
        let _span = info_span!("mir_scheduling").entered();
        schedule(&mut mir);
    }
    {
        let _span = info_span!("mir_verification").entered();
        verify(&mir)?;
    }

    let total_locals = frame_local_slots
        .checked_add(num_spills)
        .ok_or_else(|| frame_budget_error(cfg, None))?;
    let total_slots = checked_slot_sum([total_locals, homed_param_slots, u32::from(has_sret)])
        .ok_or_else(|| frame_budget_error(cfg, None))?;
    // Frame planning is the last thing the pipeline decides: the eligible-leaf
    // question can only be answered once allocation, peephole, and scheduling
    // have settled the final instruction stream and the final spill count.
    let frame_layout = match plan_frame_pointer(total_slots, is_leaf(&mir)) {
        FramePointer::Omitted => FrameLayout::frameless(scheme, used_callee_saved.len()),
        FramePointer::Established => {
            FrameLayout::try_new(scheme, used_callee_saved.len(), total_slots)
                .map_err(|_| frame_budget_error(cfg, None))?
        }
    };

    Ok((
        PreparedMir {
            mir,
            total_locals,
            frame_local_slots,
            has_sret,
            used_callee_saved,
            param_homing,
            param_storage,
            local_storage,
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

    use super::{
        FramePointer, SavedRegScheme, plan_frame_pointer, prepare_mir_with_artifacts,
        validate_pre_lowering_budget,
    };

    #[test]
    fn only_a_slotless_leaf_omits_the_frame_pointer() {
        assert_eq!(plan_frame_pointer(0, true), FramePointer::Omitted);
        // One real frame consumer — a local, a spill, a homed parameter, or the
        // sret pointer cell — restores the frame.
        assert_eq!(plan_frame_pointer(1, true), FramePointer::Established);
        // A call needs the call-boundary alignment and, on AArch64, the FP/LR
        // save, even with nothing to store in the frame.
        assert_eq!(plan_frame_pointer(0, false), FramePointer::Established);
        assert_eq!(plan_frame_pointer(3, false), FramePointer::Established);
    }

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
            &lasso::ThreadedRodeo::new(),
            6,
            6,
            SavedRegScheme::X86_64,
            rue_air::TargetCAbiFlavor::SysVAmd64,
            &|_| false,
            |_param_storage, local_storage| {
                events.borrow_mut().push("lower");
                // No storage markers, so the local layout is the identity.
                assert_eq!(local_storage.frame_local_slots(), 3);
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
            |mir| {
                events.borrow_mut().push("is_leaf");
                assert_eq!(*mir, 16, "frame planning sees the final instruction stream");
                true
            },
        )
        .expect("synthetic pipeline should succeed");

        assert_eq!(
            events.into_inner(),
            [
                "lower", "allocate", "peephole", "schedule", "verify", "is_leaf"
            ]
        );
        assert_eq!(prepared.mir, 16);
        assert_eq!(prepared.total_locals, 7);
        assert_eq!(prepared.frame_local_slots, 3);
        assert_eq!(prepared.param_storage.homed_area_slots(), 2);
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

        let over_boundary = Cfg::new(Type::UNIT, max_slots + 1, 0, "over_boundary".into(), vec![]);
        assert!(
            validate_pre_lowering_budget(&over_boundary, &type_pool, 6, 6, SavedRegScheme::X86_64,)
                .is_err(),
            "an oversized production CFG must be rejected before backend lowering"
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
    fn foreign_argument_buffers_and_sret_share_one_call_area_budget() {
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

        for (arg_regs, ret_regs, scheme, flavor) in [
            (
                6,
                6,
                SavedRegScheme::X86_64,
                rue_air::TargetCAbiFlavor::SysVAmd64,
            ),
            (
                8,
                8,
                SavedRegScheme::Aarch64,
                rue_air::TargetCAbiFlavor::Aapcs64,
            ),
        ] {
            let error = super::validate_pre_lowering_budget_for_target(
                &cfg,
                &type_pool,
                arg_regs,
                ret_regs,
                scheme,
                flavor,
                &|_| true,
            )
            .unwrap_err();
            assert!(matches!(
                error.kind,
                rue_error::ErrorKind::FunctionFrameTooLarge { .. }
            ));
        }

        // Keep the native planner's cumulative indirect+sret guard covered as
        // well; the target-C cases above exercise the distinct foreign shapes.
        let error = validate_pre_lowering_budget(&cfg, &type_pool, 6, 6, SavedRegScheme::X86_64)
            .unwrap_err();
        assert!(matches!(
            error.kind,
            rue_error::ErrorKind::FunctionFrameTooLarge { .. }
        ));
    }

    #[test]
    fn foreign_image_scratch_shares_the_live_sret_budget() {
        let type_pool = TypeInternPool::new();
        let max_array = type_pool
            .intern_array_from_type(Type::I32, rue_air::layout::MAX_FUNCTION_FRAME_BYTES / 4);
        let small_array = type_pool.intern_array_from_type(Type::I32, 4);
        let huge = Type::new_array(max_array);
        let small = Type::new_array(small_array);
        let type_pool = type_pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "scratch_budget".into(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let arg = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: small,
                span: Span::default(),
            },
        );
        cfg.append_call(
            entry,
            None,
            Spur::default(),
            [CfgCallArg {
                value: arg,
                mode: CfgArgMode::Normal,
            }],
            huge,
            Span::default(),
        )
        .unwrap();

        // The final call area contains only the max-sized sret buffer, but the
        // aggregate argument's 16-byte image scratch is live alongside it
        // during lowering. Both target-C lowerers must reject that peak before
        // their stack adjustments are emitted.
        for (arg_regs, ret_regs, scheme, flavor) in [
            (
                6,
                6,
                SavedRegScheme::X86_64,
                rue_air::TargetCAbiFlavor::SysVAmd64,
            ),
            (
                8,
                8,
                SavedRegScheme::Aarch64,
                rue_air::TargetCAbiFlavor::Aapcs64,
            ),
        ] {
            assert!(
                super::validate_pre_lowering_budget_for_target(
                    &cfg,
                    &type_pool,
                    arg_regs,
                    ret_regs,
                    scheme,
                    flavor,
                    &|_| true,
                )
                .is_err(),
                "foreign scratch must count against the {flavor:?} live sret area"
            );
        }
    }
}
