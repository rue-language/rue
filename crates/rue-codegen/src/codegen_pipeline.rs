//! Shared backend pass sequencing and frame accounting (RUE-607).
//!
//! Concrete MIR, register allocation, instruction selection, scheduling facts,
//! verification rules, and emission remain target-specific. The order in which
//! those passes run — and the distinction between spill-placement slots and
//! emitted-frame locals — is common to every machine-code emission entry point.

use rue_air::FrozenTypeInternPool;
use rue_cfg::Cfg;
use rue_error::CompileResult;

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
pub(crate) fn prepare_mir<M, R, Lower, Allocate, Peephole, Schedule, Verify>(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    return_reg_count: u32,
    lower: Lower,
    allocate: Allocate,
    peephole: Peephole,
    schedule: Schedule,
    verify: Verify,
) -> CompileResult<PreparedMir<M, R>>
where
    Lower: FnOnce() -> CompileResult<M>,
    Allocate: FnOnce(M, u32) -> CompileResult<(M, u32, Vec<R>)>,
    Peephole: FnOnce(&mut M),
    Schedule: FnOnce(&mut M),
    Verify: FnOnce(&M) -> CompileResult<()>,
{
    let num_locals_original = cfg.num_locals();
    let num_params = cfg.num_params();
    let has_sret = crate::cfg_lower::fn_uses_sret_return(cfg, type_pool, return_reg_count);
    let param_homing = param_homing_plan(cfg);

    let mir = lower()?;
    let existing_slots = num_locals_original + num_params + u32::from(has_sret);
    let (mut mir, num_spills, used_callee_saved) = allocate(mir, existing_slots)?;

    peephole(&mut mir);
    schedule(&mut mir);
    verify(&mir)?;

    Ok(PreparedMir {
        mir,
        total_locals: num_locals_original + num_spills,
        num_locals_original,
        num_params,
        has_sret,
        used_callee_saved,
        param_homing,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use rue_air::{Type, TypeInternPool};
    use rue_cfg::Cfg;

    use super::prepare_mir;

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
        let prepared = prepare_mir(
            &cfg,
            &type_pool,
            6,
            || {
                events.borrow_mut().push("lower");
                Ok(10_u32)
            },
            |mir, existing_slots| {
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
}
