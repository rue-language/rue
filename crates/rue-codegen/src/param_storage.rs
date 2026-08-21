//! Per-parameter storage planning (RUE-1170).
//!
//! Historically both backends stored every incoming ABI argument into a frame
//! slot at function entry, and the body re-loaded parameters from those homes.
//! Unused and read-only register arguments paid entry stores, frame growth,
//! and per-read reloads for nothing.
//!
//! This module decides, once per function and target, where each parameter
//! ABI slot lives:
//!
//! - **Frame**: the prologue homes the slot into the (compacted) frame
//!   parameter area, exactly as before. Everything that can address memory —
//!   aggregates, address-taken or writable by-value parameters, by-reference
//!   call arguments naming the parameter, stack-passed arguments, and raw CFG
//!   slot references into the parameter range — keeps a home.
//! - **Register-only**: the value (for a by-value scalar) or the pointer (for
//!   a by-reference parameter) arrives in one incoming argument register and
//!   is never stored. Lowering copies it into a virtual register in the entry
//!   preamble; the register allocator gives it a callee-saved register or an
//!   ordinary spill slot. An unused register argument produces no code and no
//!   frame slot at all.
//!
//! The plan is the single authority shared by frame accounting
//! (`codegen_pipeline`), both MIR lowerers, both emitters' prologues, and the
//! `--emit stackframe` reporter, so the frame layout and the code that
//! addresses it cannot drift apart.
//!
//! A CFG without grouped source-parameter descriptors (a directly constructed
//! CFG in synthetic tests) falls back to homing every slot, reproducing the
//! historical prologue exactly.

use rue_air::FrozenTypeInternPool;
use rue_cfg::{Cfg, CfgArgMode, CfgInstData, PlaceBase};

use crate::codegen_pipeline::ParamHoming;

/// Where one parameter ABI slot lives inside the function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamSlotStorage {
    /// Homed: the prologue stores this slot into the frame parameter area at
    /// `area_slot` (0-based within the area, after register-only slots are
    /// compacted away).
    Frame { area_slot: u32 },
    /// Register-only: the slot arrives in incoming ABI argument register
    /// `abi_index` (sret shift included) and has no frame home.
    Register { abi_index: u32 },
}

/// The complete per-function parameter storage decision.
#[derive(Debug, Clone)]
pub(crate) struct ParamStoragePlan {
    /// Per parameter ABI slot.
    slots: Vec<ParamSlotStorage>,
    /// Per parameter ABI slot: whether any `Param` instruction (or raw slot
    /// reference) names it. A register-only slot that is unused needs no
    /// entry copy.
    used: Vec<bool>,
    /// Frame slots the compacted parameter area occupies.
    homed_area_slots: u32,
    /// Prologue homing entries for the homed source parameters.
    homing: Vec<ParamHoming>,
}

impl ParamStoragePlan {
    /// The historical plan: every parameter ABI slot is homed at its own
    /// index and every slot is treated as used. This is both the fallback
    /// for synthetic CFGs without source-parameter descriptors and the
    /// baseline the planned path must shrink from.
    pub(crate) fn all_homed(num_params: u32, has_sret: bool) -> Self {
        let abi_shift = has_sret as u32;
        Self {
            slots: (0..num_params)
                .map(|slot| ParamSlotStorage::Frame { area_slot: slot })
                .collect(),
            used: vec![true; num_params as usize],
            homed_area_slots: num_params,
            homing: (0..num_params)
                .map(|slot| ParamHoming {
                    start_slot: slot,
                    reg_count: 1,
                    abi_start: slot + abi_shift,
                })
                .collect(),
        }
    }

    /// Decide storage for every parameter of `cfg` on a target with
    /// `arg_reg_count` incoming argument registers.
    pub(crate) fn plan(
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        has_sret: bool,
        arg_reg_count: u32,
    ) -> Self {
        let num_params = cfg.num_params();
        let descriptors = cfg.source_param_abi();
        if descriptors.is_empty() {
            return Self::all_homed(num_params, has_sret);
        }
        // Defensive: the descriptors must tile the parameter area exactly;
        // anything else falls back to the historical all-homed plan rather
        // than planning against inconsistent metadata.
        let covered: u32 = descriptors.iter().map(|d| d.slot_count).sum();
        if covered != num_params
            || descriptors
                .iter()
                .zip(descriptors.iter().skip(1))
                .any(|(a, b)| a.start_slot + a.slot_count != b.start_slot)
            || descriptors.first().is_some_and(|d| d.start_slot != 0)
        {
            return Self::all_homed(num_params, has_sret);
        }

        let scan = scan_param_references(cfg, type_pool);
        let mut slots = Vec::with_capacity(num_params as usize);
        let mut homing = Vec::new();
        let mut abi = has_sret as u32;
        let mut area = 0u32;
        for d in descriptors {
            let slot = d.start_slot;
            // A register-only parameter must be a single slot arriving in a
            // single register that actually is a register (not stack-passed).
            // A by-reference parameter's slot holds only the incoming
            // pointer, which nothing may address (every consumer goes through
            // `ensure_by_ref_param_ptr`); a by-value slot must additionally
            // be immutable and never addressed.
            let eligible = d.slot_count == 1
                && d.crossing_regs == 1
                && abi < arg_reg_count
                && !scan.needs_home[slot as usize]
                && (cfg.is_param_by_ref(slot)
                    || (!cfg.is_param_writable(slot) && !cfg.is_param_address_taken(slot)));
            if eligible {
                slots.push(ParamSlotStorage::Register { abi_index: abi });
            } else {
                for j in 0..d.slot_count {
                    slots.push(ParamSlotStorage::Frame {
                        area_slot: area + j,
                    });
                }
                homing.push(ParamHoming {
                    start_slot: area,
                    reg_count: d.crossing_regs,
                    abi_start: abi,
                });
                area += d.slot_count;
            }
            abi += d.crossing_regs;
        }
        Self {
            slots,
            used: scan.used,
            homed_area_slots: area,
            homing,
        }
    }

    /// Storage decision for parameter ABI slot `index`.
    #[cfg(test)]
    pub(crate) fn slot(&self, index: u32) -> ParamSlotStorage {
        self.slots[index as usize]
    }

    /// The compacted parameter-area position of slot `index`, if homed.
    pub(crate) fn area_slot(&self, index: u32) -> Option<u32> {
        match self.slots[index as usize] {
            ParamSlotStorage::Frame { area_slot } => Some(area_slot),
            ParamSlotStorage::Register { .. } => None,
        }
    }

    /// Whether any CFG instruction references parameter ABI slot `index`.
    #[cfg(test)]
    pub(crate) fn is_used(&self, index: u32) -> bool {
        self.used[index as usize]
    }

    /// Frame slots the compacted parameter area occupies.
    pub(crate) fn homed_area_slots(&self) -> u32 {
        self.homed_area_slots
    }

    /// Prologue homing entries for the homed source parameters.
    pub(crate) fn homing(&self) -> &[ParamHoming] {
        &self.homing
    }

    /// Register-only parameter slots needing an entry copy, as
    /// `(param ABI slot, incoming ABI index)`.
    ///
    /// A by-reference pointer is copied whether or not it is used, mirroring
    /// the unconditional by-ref preload of the homed path; a by-value slot is
    /// copied only when a `Param` instruction reads it.
    pub(crate) fn entry_copies<'a>(
        &'a self,
        cfg: &'a Cfg,
    ) -> impl Iterator<Item = (u32, u32)> + 'a {
        self.slots
            .iter()
            .enumerate()
            .filter_map(move |(slot, storage)| {
                let slot = slot as u32;
                match storage {
                    ParamSlotStorage::Register { abi_index }
                        if cfg.is_param_by_ref(slot) || self.used[slot as usize] =>
                    {
                        Some((slot, *abi_index))
                    }
                    _ => None,
                }
            })
    }
}

struct ParamReferenceScan {
    /// Slot requires a frame home for a reason visible only in the
    /// instruction stream (writes, address exposure, raw slot references).
    needs_home: Vec<bool>,
    /// Slot is referenced at all.
    used: Vec<bool>,
}

/// Scan the CFG for the per-slot facts eligibility needs beyond the static
/// parameter metadata: actual writes, address-exposing uses, and raw frame
/// slot references into the parameter range.
///
/// A reference rooted at a parameter's first ABI slot can span the
/// parameter's whole type — a multi-slot `Param` read, a place access, or a
/// by-value `ParamStore` addresses `[index, index + slot_count)` as one
/// contiguous frame region. Under the direct-slot cleanup convention
/// (destructors and drop glue) those slots are described by *separate*
/// single-slot descriptors, so every mark covers the referenced type's full
/// slot span, not just the base slot.
fn scan_param_references(cfg: &Cfg, type_pool: &FrozenTypeInternPool) -> ParamReferenceScan {
    let num_params = cfg.num_params() as usize;
    let num_locals = cfg.num_locals();
    let mut needs_home = vec![false; num_params];
    let mut used = vec![false; num_params];

    let param_of_slot = |slot: u32| -> Option<u32> {
        (slot >= num_locals && slot < num_locals + num_params as u32).then(|| slot - num_locals)
    };
    let mark_span = |v: &mut Vec<bool>, index: u32, span: u32| {
        for offset in 0..span {
            if let Some(flag) = v.get_mut((index + offset) as usize) {
                *flag = true;
            }
        }
    };
    let span_of = |ty: rue_cfg::Type| crate::types::type_slot_span(type_pool, ty);

    for block in cfg.blocks() {
        for &value in &block.insts {
            let inst = cfg.get_inst(value);
            match &inst.data {
                CfgInstData::Param { index } => {
                    let span = if cfg.is_param_by_ref(*index) {
                        1
                    } else {
                        span_of(inst.ty)
                    };
                    mark_span(&mut used, *index, span);
                    // A multi-slot by-value read loads the value out of its
                    // contiguous frame region, so the whole span must be
                    // homed even when the cleanup convention split it into
                    // single-slot descriptors. A scalar read consumes the
                    // register copy and needs no home.
                    if !cfg.is_param_by_ref(*index) && span > 1 {
                        mark_span(&mut needs_home, *index, span);
                    }
                }
                CfgInstData::ParamStore { param_slot, value } => {
                    let span = if cfg.is_param_by_ref(*param_slot) {
                        1
                    } else {
                        span_of(cfg.get_inst(*value).ty)
                    };
                    mark_span(&mut used, *param_slot, span);
                    // A by-ref store goes through the incoming pointer; a
                    // by-value store (a `mut self` receiver) writes the home.
                    if !cfg.is_param_by_ref(*param_slot) {
                        mark_span(&mut needs_home, *param_slot, span);
                    }
                }
                CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
                    match place.base {
                        PlaceBase::Param(slot) => {
                            // Place access to a by-value parameter addresses
                            // its home region (rooted at the base slot and
                            // spanning the base type); by-ref access goes
                            // through the pointer.
                            if cfg.is_param_by_ref(slot) {
                                mark_span(&mut used, slot, 1);
                            } else {
                                let span = span_of(place.base_type);
                                mark_span(&mut used, slot, span);
                                mark_span(&mut needs_home, slot, span);
                            }
                        }
                        PlaceBase::Local(base_slot) => {
                            if let Some(index) = param_of_slot(base_slot) {
                                let span = span_of(place.base_type);
                                mark_span(&mut used, index, span);
                                mark_span(&mut needs_home, index, span);
                            }
                        }
                        PlaceBase::Accessor(_) => {
                            panic!("mandatory-inline accessor place reached codegen")
                        }
                        // The pointer producer is an ordinary SSA value. Any
                        // parameter storage it depends on is recorded by that
                        // producer's own operands; the indirect base itself
                        // names no parameter slot or home region.
                        PlaceBase::Indirect(_) => {}
                    }
                }
                CfgInstData::Alloc { slot, .. }
                | CfgInstData::Load { slot }
                | CfgInstData::StorageLive { slot, .. }
                | CfgInstData::StorageDead { slot, .. } => {
                    if let Some(index) = param_of_slot(*slot) {
                        let span = span_of(inst.ty);
                        mark_span(&mut used, index, span);
                        mark_span(&mut needs_home, index, span);
                    }
                }
                CfgInstData::Store { slot, value } => {
                    if let Some(index) = param_of_slot(*slot) {
                        // A store to a by-ref parameter slot is redirected
                        // through the incoming pointer (`store_destination`);
                        // a by-value one writes the home region.
                        if cfg.is_param_by_ref(index) {
                            mark_span(&mut used, index, 1);
                        } else {
                            let span = span_of(cfg.get_inst(*value).ty);
                            mark_span(&mut used, index, span);
                            mark_span(&mut needs_home, index, span);
                        }
                    }
                }
                data @ CfgInstData::Call { .. } => {
                    // A by-reference call argument forwards the address of
                    // its operand. When that operand is a by-value parameter,
                    // `byref_args` takes the address of its frame home.
                    for arg in cfg.get_call_args(data) {
                        if arg.mode == CfgArgMode::Normal {
                            continue;
                        }
                        if let CfgInstData::Param { index } = &cfg.get_inst(arg.value).data {
                            let arg_inst = cfg.get_inst(arg.value);
                            if cfg.is_param_by_ref(*index) {
                                mark_span(&mut used, *index, 1);
                            } else {
                                let span = span_of(arg_inst.ty);
                                mark_span(&mut used, *index, span);
                                mark_span(&mut needs_home, *index, span);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    ParamReferenceScan { needs_home, used }
}

#[cfg(test)]
mod tests {
    use lasso::Spur;
    use rue_air::{SourceParamAbi, Type, TypeInternPool};
    use rue_cfg::{Cfg, CfgArgMode, CfgCallArg, CfgInst, CfgInstData};
    use rue_span::Span;

    use super::{ParamSlotStorage, ParamStoragePlan};

    fn pool() -> rue_air::FrozenTypeInternPool {
        TypeInternPool::new().freeze()
    }

    fn scalar_descriptors(count: u32) -> Vec<SourceParamAbi> {
        (0..count)
            .map(|slot| SourceParamAbi {
                start_slot: slot,
                slot_count: 1,
                crossing_regs: 1,
                ty: None,
            })
            .collect()
    }

    fn push_param(cfg: &mut Cfg, block: rue_cfg::BlockId, index: u32) -> rue_cfg::CfgValue {
        cfg.append_inst(
            block,
            CfgInst {
                data: CfgInstData::Param { index },
                ty: Type::I64,
                span: Span::default(),
            },
        )
    }

    #[test]
    fn synthetic_cfg_without_descriptors_homes_every_slot() {
        let cfg = Cfg::new(Type::UNIT, 1, 2, "synthetic".into(), vec![false, false]);
        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, 6);
        assert_eq!(plan.homed_area_slots(), 2);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(1), ParamSlotStorage::Frame { area_slot: 1 });
        assert_eq!(plan.homing().len(), 2);
        assert_eq!(plan.homing()[1].abi_start, 1);
    }

    #[test]
    fn unused_and_read_only_register_params_lose_their_homes() {
        let mut cfg = Cfg::new(Type::I64, 0, 2, "reads".into(), vec![false, false]);
        cfg.set_source_param_abi(scalar_descriptors(2));
        let entry = cfg.new_block();
        cfg.entry = entry;
        // Param 0 is read; param 1 is never referenced.
        push_param(&mut cfg, entry, 0);

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Register { abi_index: 0 });
        assert_eq!(plan.slot(1), ParamSlotStorage::Register { abi_index: 1 });
        assert!(plan.is_used(0));
        assert!(!plan.is_used(1));
        assert_eq!(plan.homed_area_slots(), 0);
        assert!(plan.homing().is_empty());
        assert_eq!(plan.entry_copies(&cfg).collect::<Vec<_>>(), [(0, 0)]);
    }

    #[test]
    fn sret_shifts_register_indices() {
        let mut cfg = Cfg::new(Type::I64, 0, 1, "sret".into(), vec![false]);
        cfg.set_source_param_abi(scalar_descriptors(1));
        let entry = cfg.new_block();
        cfg.entry = entry;
        push_param(&mut cfg, entry, 0);

        let plan = ParamStoragePlan::plan(&cfg, &pool(), true, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Register { abi_index: 1 });
    }

    #[test]
    fn stack_passed_params_keep_their_homes() {
        let count = 8u32;
        let mut cfg = Cfg::new(
            Type::I64,
            0,
            count,
            "stack".into(),
            vec![false; count as usize],
        );
        cfg.set_source_param_abi(scalar_descriptors(count));
        let entry = cfg.new_block();
        cfg.entry = entry;
        for index in 0..count {
            push_param(&mut cfg, entry, index);
        }

        // Six argument registers: slots 6 and 7 are stack-passed.
        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, 6);
        for slot in 0..6 {
            assert!(matches!(plan.slot(slot), ParamSlotStorage::Register { .. }));
        }
        assert_eq!(plan.slot(6), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(7), ParamSlotStorage::Frame { area_slot: 1 });
        assert_eq!(plan.homed_area_slots(), 2);
        // The homed entries still name their original incoming ABI indices.
        assert_eq!(plan.homing()[0].abi_start, 6);
        assert_eq!(plan.homing()[1].abi_start, 7);

        // With eight argument registers (AArch64) everything is register-only.
        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, 8);
        assert_eq!(plan.homed_area_slots(), 0);
    }

    #[test]
    fn writes_and_byref_call_arguments_force_homes() {
        let mut cfg = Cfg::new(Type::I64, 0, 3, "writes".into(), vec![false, false, false]);
        cfg.set_source_param_abi(scalar_descriptors(3));
        let entry = cfg.new_block();
        cfg.entry = entry;
        // Param 0: written via ParamStore (a by-value `mut self` shape).
        let value = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I64,
                span: Span::default(),
            },
        );
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::ParamStore {
                    param_slot: 0,
                    value,
                },
                ty: Type::UNIT,
                span: Span::default(),
            },
        );
        // Param 1: forwarded as a by-reference call argument.
        let param1 = push_param(&mut cfg, entry, 1);
        cfg.append_call(
            entry,
            None,
            Spur::default(),
            vec![CfgCallArg {
                value: param1,
                mode: CfgArgMode::Inout,
            }],
            Type::UNIT,
            Span::default(),
        )
        .unwrap();
        // Param 2: plain read.
        push_param(&mut cfg, entry, 2);

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(1), ParamSlotStorage::Frame { area_slot: 1 });
        assert_eq!(plan.slot(2), ParamSlotStorage::Register { abi_index: 2 });
        assert_eq!(plan.homed_area_slots(), 2);
    }

    #[test]
    fn address_taken_and_writable_by_value_params_keep_homes() {
        let mut cfg = rue_cfg::Cfg::new(
            Type::I64,
            0,
            2,
            "flags".into(),
            rue_air::ParamSlotModes::new(vec![false, false], vec![false, true]),
        );
        cfg.set_source_param_abi(scalar_descriptors(2));
        cfg.mark_param_address_taken(0);
        let entry = cfg.new_block();
        cfg.entry = entry;
        push_param(&mut cfg, entry, 0);
        push_param(&mut cfg, entry, 1);

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(1), ParamSlotStorage::Frame { area_slot: 1 });
    }

    #[test]
    fn by_ref_pointer_params_are_register_only_even_when_writable() {
        // An inout parameter: by-ref and writable. The write goes through the
        // pointer, so the pointer itself can stay in a register.
        let mut cfg = rue_cfg::Cfg::new(
            Type::UNIT,
            0,
            1,
            "inout".into(),
            rue_air::ParamSlotModes::new(vec![true], vec![true]),
        );
        cfg.set_source_param_abi(scalar_descriptors(1));
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(7),
                ty: Type::I64,
                span: Span::default(),
            },
        );
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::ParamStore {
                    param_slot: 0,
                    value,
                },
                ty: Type::UNIT,
                span: Span::default(),
            },
        );

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Register { abi_index: 0 });
        // The pointer is copied at entry whether or not a Param inst exists.
        assert_eq!(plan.entry_copies(&cfg).collect::<Vec<_>>(), [(0, 0)]);
    }

    /// The direct-slot cleanup convention (destructors, drop glue) describes
    /// one source aggregate as SEPARATE single-slot descriptors. A place or
    /// multi-slot `Param` reference rooted at the first slot addresses the
    /// whole contiguous region, so the scan must home the full type span —
    /// homing only the base slot leaves the tail slots reading garbage (the
    /// RUE-1170 ArrayBuf destructor regression).
    #[test]
    fn references_home_the_full_type_span_across_split_descriptors() {
        let type_pool = TypeInternPool::new();
        let interner = lasso::ThreadedRodeo::new();
        let (struct_id, _) = type_pool.register_struct(
            interner.get_or_intern("PairLike"),
            rue_air::StructDef {
                name: "PairLike".into(),
                fields: vec![
                    rue_air::StructField {
                        name: "a".to_string(),
                        ty: Type::I64,
                    },
                    rue_air::StructField {
                        name: "b".to_string(),
                        ty: Type::I64,
                    },
                ],
                is_copy: true,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        );
        let pair_ty = Type::new_struct(struct_id);
        let pool = type_pool.freeze();

        let mut cfg = Cfg::new(Type::UNIT, 0, 2, "split".into(), vec![false, false]);
        // Two single-slot descriptors for one two-slot source value, the
        // direct-slot cleanup shape.
        cfg.set_source_param_abi(scalar_descriptors(2));
        let entry = cfg.new_block();
        cfg.entry = entry;
        // One whole-value read of the two-slot aggregate rooted at slot 0.
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Param { index: 0 },
                ty: pair_ty,
                span: Span::default(),
            },
        );

        let plan = ParamStoragePlan::plan(&cfg, &pool, false, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(
            plan.slot(1),
            ParamSlotStorage::Frame { area_slot: 1 },
            "the read's type span must home the tail slot too"
        );
        assert_eq!(plan.homed_area_slots(), 2);
    }

    #[test]
    fn aggregates_and_raw_slot_references_keep_homes() {
        // Param 0 is a two-slot aggregate; param slot 2 is referenced as a
        // raw frame slot (num_locals + 2).
        let mut cfg = Cfg::new(Type::I64, 1, 3, "agg".into(), vec![false, false, false]);
        cfg.set_source_param_abi(vec![
            SourceParamAbi {
                start_slot: 0,
                slot_count: 2,
                crossing_regs: 2,
                ty: None,
            },
            SourceParamAbi {
                start_slot: 2,
                slot_count: 1,
                crossing_regs: 1,
                ty: None,
            },
        ]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Load { slot: 1 + 2 },
                ty: Type::I64,
                span: Span::default(),
            },
        );

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(1), ParamSlotStorage::Frame { area_slot: 1 });
        assert_eq!(plan.slot(2), ParamSlotStorage::Frame { area_slot: 2 });
        assert_eq!(plan.homed_area_slots(), 3);
        assert_eq!(plan.homing().len(), 2);
    }
}
