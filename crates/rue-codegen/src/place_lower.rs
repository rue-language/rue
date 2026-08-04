//! Shared lowering for CFG places (RUE-604).
//!
//! A [`Place`] names storage rooted at a local or parameter and refined by a
//! chain of field and array-index projections. The address arithmetic is
//! target-independent: field offsets accumulate statically, every checked
//! index is validated before address formation, and dynamic indices contribute
//! `index * element_size` to the root object's low-end address. Historically
//! x86-64 and AArch64 each carried a roughly 560-line copy of that algorithm.
//!
//! This module owns the algorithm once. Backends retain only the instruction
//! leaves in [`PlaceLowerBackend`], including two intentionally narrow leaves
//! that preserve the exact MIR forms emitted before this extraction.

use crate::agg_slots::{self, SlotBackend};
use crate::allocation::{self, ScalePlan};
use crate::vreg::VReg;
use rue_air::Type;

/// A place whose dynamic indices have already been materialized by the shared
/// value dispatcher. This is the normalized input consumed by the plan-only
/// entry points below.
pub type ResolvedPlace = crate::value_plan::PlacePlan;

/// Per-target instruction leaves used by shared place lowering.
pub(crate) trait PlaceLowerBackend: SlotBackend {
    /// Get or lazily materialize a received by-reference parameter pointer.
    fn ensure_by_ref_param_ptr(&mut self, param_slot: u32) -> VReg;

    /// Emit `dst = address of frame slot`.
    fn emit_frame_addr(&mut self, dst: VReg, slot: u32);

    /// Add an address held in `rhs` to `dst`, modifying `dst` in place.
    fn emit_addr_add(&mut self, dst: VReg, rhs: VReg);

    /// Add a nonzero byte displacement to an address in `dst`.
    fn emit_addr_add_imm(&mut self, dst: VReg, byte_offset: i32);

    /// Scale the copied index in `scaled` by the normalized byte-width plan.
    /// Implementations may allocate target-specific temporary vregs.
    fn emit_scale_index_bytes(&mut self, scaled: VReg, plan: ScalePlan);

    /// Materialize the canonical value for a zero-sized place read.
    ///
    /// This stays distinct from [`SlotBackend::emit_load_zero`] because x86-64
    /// historically used a 64-bit immediate here; retaining that exact MIR is
    /// part of making this extraction mechanically output-neutral.
    fn emit_zero_sized_place(&mut self, dst: VReg);

    /// Load from the address in `ptr` with no encoded displacement.
    ///
    /// AArch64 has separate base-only and base-plus-zero MIR variants. Keeping
    /// this leaf preserves the pre-extraction choice at simple and dynamically
    /// addressed place reads.
    fn emit_load_ptr_base(&mut self, dst: VReg, ptr: VReg);
}

fn resolved_offsets<B: PlaceLowerBackend + ?Sized>(
    b: &mut B,
    place: &ResolvedPlace,
    check_bounds: bool,
) -> ResolvedProjectionOffsets {
    let mut static_slot_offset = 0;
    let mut index_levels = Vec::new();
    for projection in &place.projections {
        match *projection {
            crate::value_plan::ProjectionPlan::Field {
                struct_id,
                field_index,
            } => {
                static_slot_offset += b.ctx().struct_field_slot_offset(struct_id, field_index);
            }
            crate::value_plan::ProjectionPlan::Index { array_type, index } => {
                if check_bounds {
                    allocation::lower_bounds_check(
                        b,
                        allocation::BoundsCheckPlan::new(index, b.ctx().array_length(array_type)),
                    );
                }
                index_levels.push(ResolvedIndexLevel {
                    index,
                    scale: allocation::index_scale_plan(b.ctx().type_pool, array_type),
                });
            }
        }
    }
    ResolvedProjectionOffsets {
        static_slot_offset,
        index_levels,
    }
}

fn resolved_root_count<B: PlaceLowerBackend + ?Sized>(b: &B, place: &ResolvedPlace) -> u32 {
    b.ctx().type_slot_count(place.base_type)
}

fn resolved_access<B: PlaceLowerBackend + ?Sized>(
    b: &mut B,
    place: &ResolvedPlace,
    offsets: ResolvedProjectionOffsets,
) -> ProjectedAccess {
    let root_count = resolved_root_count(b, place);
    let dynamic_offset = compute_resolved_index_offset(b, &offsets.index_levels);
    match place.base {
        crate::value_plan::PlaceBasePlan::Local(slot) => frame_access(
            b,
            slot + root_count.saturating_sub(1) - offsets.static_slot_offset,
            dynamic_offset,
        ),
        crate::value_plan::PlaceBasePlan::Param { slot, by_ref: true } => {
            let ptr = b.ensure_by_ref_param_ptr(slot);
            let byte_offset = offsets.static_slot_offset as i32 * allocation::SLOT_BYTES as i32;
            if let Some(dynamic) = dynamic_offset {
                let addr = b.alloc_vreg();
                b.emit_reg_move(addr, ptr);
                if byte_offset != 0 {
                    b.emit_addr_add_imm(addr, byte_offset);
                }
                b.emit_addr_add(addr, dynamic);
                ProjectedAccess::PointerAddr(addr)
            } else {
                ProjectedAccess::PointerOffset { ptr, byte_offset }
            }
        }
        crate::value_plan::PlaceBasePlan::Param {
            slot,
            by_ref: false,
        } => frame_access(
            b,
            b.ctx().param_frame_slot(slot) + root_count.saturating_sub(1)
                - offsets.static_slot_offset,
            dynamic_offset,
        ),
    }
}

fn compute_resolved_index_offset<B: PlaceLowerBackend + ?Sized>(
    b: &mut B,
    levels: &[ResolvedIndexLevel],
) -> Option<VReg> {
    let mut total = None;
    for level in levels {
        let scaled = b.alloc_vreg();
        b.emit_reg_move(scaled, level.index);
        b.emit_scale_index_bytes(scaled, level.scale);
        if let Some(previous) = total {
            b.emit_addr_add(previous, scaled);
        } else {
            total = Some(scaled);
        }
    }
    total
}

fn frame_access<B: PlaceLowerBackend + ?Sized>(
    b: &mut B,
    low_slot: u32,
    dynamic_offset: Option<VReg>,
) -> ProjectedAccess {
    if let Some(dynamic_offset) = dynamic_offset {
        let addr = b.alloc_vreg();
        b.emit_frame_addr(addr, low_slot);
        b.emit_addr_add(addr, dynamic_offset);
        ProjectedAccess::PointerAddr(addr)
    } else {
        ProjectedAccess::FrameSlot(low_slot)
    }
}

pub(crate) fn lower_place_read_plan<B: PlaceLowerBackend>(
    b: &mut B,
    dst: VReg,
    place: &ResolvedPlace,
    ty: Type,
) {
    if b.ctx().type_slot_count(ty) == 0 {
        resolved_offsets(b, place, true);
        b.emit_zero_sized_place(dst);
        return;
    }
    if place.projections.is_empty() {
        match place.base {
            crate::value_plan::PlaceBasePlan::Local(slot) => b.emit_load_slot(dst, slot),
            crate::value_plan::PlaceBasePlan::Param { slot, by_ref: true } => {
                let ptr = b.ensure_by_ref_param_ptr(slot);
                b.emit_load_ptr_base(dst, ptr);
            }
            crate::value_plan::PlaceBasePlan::Param {
                slot,
                by_ref: false,
            } => b.emit_load_slot(dst, b.ctx().param_frame_slot(slot)),
        }
        return;
    }
    let offsets = resolved_offsets(b, place, true);
    match resolved_access(b, place, offsets) {
        ProjectedAccess::FrameSlot(slot) => b.emit_load_slot(dst, slot),
        ProjectedAccess::PointerAddr(ptr) => b.emit_load_ptr_base(dst, ptr),
        ProjectedAccess::PointerOffset { ptr, byte_offset } => {
            b.emit_load_through_ptr(dst, ptr, byte_offset)
        }
    }
}

pub(crate) fn lower_place_write_plan<B: PlaceLowerBackend>(
    b: &mut B,
    place: &ResolvedPlace,
    vals: &[VReg],
) {
    if vals.is_empty() {
        resolved_offsets(b, place, true);
        return;
    }
    if place.projections.is_empty() {
        match place.base {
            crate::value_plan::PlaceBasePlan::Local(slot) => agg_slots::store_slots(b, vals, slot),
            crate::value_plan::PlaceBasePlan::Param { slot, by_ref: true } => {
                let ptr = b.ensure_by_ref_param_ptr(slot);
                agg_slots::store_slots_through_ptr(b, vals, ptr, 0);
            }
            crate::value_plan::PlaceBasePlan::Param {
                slot,
                by_ref: false,
            } => agg_slots::store_slots(b, vals, b.ctx().param_frame_slot(slot)),
        }
        return;
    }
    let offsets = resolved_offsets(b, place, true);
    match resolved_access(b, place, offsets) {
        ProjectedAccess::FrameSlot(slot) => agg_slots::store_slots_at_low(b, vals, slot),
        ProjectedAccess::PointerAddr(ptr) => agg_slots::store_slots_through_ptr(b, vals, ptr, 0),
        ProjectedAccess::PointerOffset { ptr, byte_offset } => {
            agg_slots::store_slots_through_ptr(b, vals, ptr, byte_offset)
        }
    }
}

fn lower_place_addr_plan_with_bounds<B: PlaceLowerBackend + ?Sized>(
    b: &mut B,
    dst: VReg,
    place: &ResolvedPlace,
    check_bounds: bool,
) {
    let offsets = resolved_offsets(b, place, check_bounds);
    let root_count = resolved_root_count(b, place);
    let dynamic = compute_resolved_index_offset(b, &offsets.index_levels);
    match place.base {
        crate::value_plan::PlaceBasePlan::Local(slot) => {
            b.emit_frame_addr(
                dst,
                slot + root_count.saturating_sub(1) - offsets.static_slot_offset,
            );
            if let Some(dynamic) = dynamic {
                b.emit_addr_add(dst, dynamic);
            }
        }
        crate::value_plan::PlaceBasePlan::Param { slot, by_ref: true } => {
            let ptr = b.ensure_by_ref_param_ptr(slot);
            b.emit_reg_move(dst, ptr);
            let byte_offset = offsets.static_slot_offset as i32 * allocation::SLOT_BYTES as i32;
            if byte_offset != 0 {
                b.emit_addr_add_imm(dst, byte_offset);
            }
            if let Some(dynamic) = dynamic {
                b.emit_addr_add(dst, dynamic);
            }
        }
        crate::value_plan::PlaceBasePlan::Param {
            slot,
            by_ref: false,
        } => {
            b.emit_frame_addr(
                dst,
                b.ctx().param_frame_slot(slot) + root_count.saturating_sub(1)
                    - offsets.static_slot_offset,
            );
            if let Some(dynamic) = dynamic {
                b.emit_addr_add(dst, dynamic);
            }
        }
    }
}

/// Form a place address after applying the shared bounds policy. Aggregate
/// reads use this entry point so their multi-slot load path cannot bypass an
/// indexed-place trap.
pub(crate) fn lower_checked_place_addr_plan<B: PlaceLowerBackend + ?Sized>(
    b: &mut B,
    dst: VReg,
    place: &ResolvedPlace,
) {
    lower_place_addr_plan_with_bounds(b, dst, place, true);
}

/// Form an unchecked place address for target leaves whose caller has already
/// established the policy (for example raw ABI pointer materialization).
pub(crate) fn lower_place_addr_plan<B: PlaceLowerBackend + ?Sized>(
    b: &mut B,
    dst: VReg,
    place: &ResolvedPlace,
) {
    lower_place_addr_plan_with_bounds(b, dst, place, false);
}

struct ResolvedIndexLevel {
    index: VReg,
    scale: ScalePlan,
}

struct ResolvedProjectionOffsets {
    static_slot_offset: u32,
    index_levels: Vec<ResolvedIndexLevel>,
}

enum ProjectedAccess {
    /// A statically known frame slot containing the accessed value's low end.
    FrameSlot(u32),
    /// A fully formed address. Reads use the backend's base-only load form.
    PointerAddr(VReg),
    /// A received by-ref pointer plus a statically encoded displacement.
    PointerOffset { ptr: VReg, byte_offset: i32 },
}

#[cfg(test)]
mod tests {
    use lasso::ThreadedRodeo;
    use rue_air::{Sema, SemaMetadata, StructDef, StructField, Type, TypeInternPool};
    use rue_cfg::{Cfg, CfgBuilder, CfgInst, CfgInstData, PlaceBase, Projection};
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;
    use rue_span::{FileId, Span};
    use rue_target::Target;

    use crate::aarch64::{Aarch64Inst, CfgLower as Aarch64CfgLower};
    use crate::x86_64::{CfgLower as X86CfgLower, X86Inst};

    /// Exercise all three shared entry points with a field followed by two
    /// index projections. The outer index has a two-slot stride (multiply),
    /// while the inner scalar index has a one-slot stride (shift).
    #[test]
    fn nested_field_index_read_write_and_addr_lower_on_both_backends() {
        let source = r#"
            struct Grid { pad: i32, cells: [[i32; 2]; 2] }
            struct Empty { unit: () }
            fn read_borrow(borrow value: i32) -> i32 { value }
            fn read_inout(inout value: i32) -> i32 { value }
            fn read_unit(value: Empty) -> () { value.unit }
            fn read_unit_index(borrow arr: [Empty; 2], i: u64) -> () { arr[i].unit }
            fn write_unit_index(inout arr: [Empty; 2], i: u64) { arr[i] = Empty { unit: () }; }
            fn main() -> i32 {
                let mut scalar = 7;
                let _borrow_read = read_borrow(borrow scalar);
                let _scalar_read = read_inout(inout scalar);
                let empty = Empty { unit: () };
                read_unit(empty);
                let mut empty_values: [Empty; 2] = [Empty { unit: () }, Empty { unit: () }];
                read_unit_index(borrow empty_values, 0);
                write_unit_index(inout empty_values, 0);
                let mut grid = Grid { pad: 9, cells: [[10, 20], [30, 40]] };
                let i: u64 = 1;
                let j: u64 = 0;
                grid.cells[i][j] = grid.cells[i][j] + 1;
                grid.cells[i][j]
            }
        "#;

        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().expect("fixture should lex");
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().expect("fixture should parse");
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let output = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .analyze_all_for_test()
            .expect("fixture should analyze");
        let build_cfg = |name: &str| {
            let symbol = SemaMetadata::synthetic_root_function_symbol(name);
            let function = output
                .functions
                .iter()
                .find(|function| function.name == symbol)
                .expect("fixture function should exist");
            CfgBuilder::build(
                &function.air,
                function.num_locals,
                function.num_param_slots,
                &function.name,
                &output.type_pool,
                function.param_modes.clone(),
                &interner,
                function.allow_unreachable_code,
                function.callable_kind,
            )
            .cfg
            .unwrap()
        };
        let cfg = build_cfg("main");

        let x86 = X86CfgLower::new_unchecked(&cfg, &output.type_pool, &interner)
            .lower()
            .expect("x86 fixture should lower");
        let arm = Aarch64CfgLower::new_unchecked(
            &cfg,
            &output.type_pool,
            &interner,
            Target::Aarch64Linux,
        )
        .lower()
        .expect("AArch64 fixture should lower");

        // RHS read, write destination, and final read each walk both indices.
        // Only the write emits an indexed store; the two ordinary reads emit
        // indexed loads. (Taking a raw pointer into the frame-resident nested
        // array is refused by the compact-layout M2 contract and is covered by
        // `raw_pointer_into_frame_nested_array_is_refused_on_both_backends`.)
        // These counts make projection traversal regressions visible without
        // snapshotting unrelated prologue/ABI details.
        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::ImulRR64 { .. }))
                .count(),
            3
        );
        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::Shl { .. }))
                .count(),
            3
        );
        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::MovRMIndexed { .. }))
                .count(),
            2
        );
        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::MovMRIndexed { .. }))
                .count(),
            1
        );

        assert_eq!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::MulRR { .. }))
                .count(),
            3
        );
        assert_eq!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::LslImm { .. }))
                .count(),
            3
        );
        assert_eq!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::LdrIndexed { .. }))
                .count(),
            2
        );
        assert_eq!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::StrIndexedOffset { .. }))
                .count(),
            1
        );

        // Build a direct CFG fixture for an indexed PlaceRead whose result is
        // a two-slot aggregate. The source language normally passes such a
        // place by reference, so this isolates the returning aggregate shape
        // and makes the shared slot traversal observable on both adapters.
        let synthetic_interner = ThreadedRodeo::new();
        let synthetic_types = TypeInternPool::new();
        let (pair_id, _) = synthetic_types.register_struct(
            synthetic_interner.get_or_intern("IndexedPair"),
            StructDef {
                name: "IndexedPair".to_string(),
                fields: vec![
                    StructField {
                        name: "left".to_string(),
                        ty: Type::I64,
                    },
                    StructField {
                        name: "right".to_string(),
                        ty: Type::I64,
                    },
                ],
                is_copy: true,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let pair_ty = Type::new_struct(pair_id);
        let array_id = synthetic_types.intern_array_from_type(pair_ty, 2);
        let array_ty = Type::new_array(array_id);
        let synthetic_types = synthetic_types.freeze();
        let mut indexed_cfg = Cfg::new(pair_ty, 1, 1, "indexed_pair_read".to_string(), vec![false]);
        let indexed_entry = indexed_cfg.new_block();
        indexed_cfg.entry = indexed_entry;
        let index = indexed_cfg.append_inst(
            indexed_entry,
            CfgInst {
                data: CfgInstData::Param { index: 0 },
                ty: Type::U64,
                span: Span::new(0, 0),
            },
        );
        let read = indexed_cfg
            .append_place_read(
                indexed_entry,
                PlaceBase::Local(0),
                array_ty,
                [Projection::Index {
                    array_type: array_ty,
                    index,
                }],
                pair_ty,
                Span::new(0, 0),
            )
            .unwrap();
        indexed_cfg.set_return(indexed_entry, Some(read));
        let pair_x86 = X86CfgLower::new_unchecked(&indexed_cfg, &synthetic_types, &interner)
            .lower()
            .expect("x86 indexed aggregate read should lower");
        let pair_arm = Aarch64CfgLower::new_unchecked(
            &indexed_cfg,
            &synthetic_types,
            &interner,
            Target::Aarch64Linux,
        )
        .lower()
        .expect("AArch64 indexed aggregate read should lower");
        let x86_bounds = pair_x86
            .instructions()
            .iter()
            .position(|inst| {
                matches!(
                    inst,
                    X86Inst::CallRel { symbol_id }
                        if pair_x86.get_symbol(*symbol_id) == "__rue_bounds_check"
                )
            })
            .expect("indexed aggregate read must retain its bounds trap edge");
        let x86_loads: Vec<(usize, i32)> = pair_x86
            .instructions()
            .iter()
            .enumerate()
            .filter_map(|(index, inst)| match inst {
                X86Inst::MovRMIndexed { offset, .. } => Some((index, *offset)),
                _ => None,
            })
            .collect();
        assert_eq!(
            x86_loads
                .iter()
                .map(|(_, offset)| *offset)
                .collect::<Vec<_>>(),
            vec![0, 8]
        );
        assert!(x86_bounds < x86_loads[0].0);

        let arm_bounds = pair_arm
            .instructions()
            .iter()
            .position(|inst| {
                matches!(
                    inst,
                    Aarch64Inst::Bl { symbol_id }
                        if pair_arm.get_symbol(*symbol_id) == "__rue_bounds_check"
                )
            })
            .expect("indexed aggregate read must retain its bounds trap edge");
        let arm_loads: Vec<(usize, i32)> = pair_arm
            .instructions()
            .iter()
            .enumerate()
            .filter_map(|(index, inst)| match inst {
                Aarch64Inst::LdrIndexedOffset { offset, .. } => Some((index, *offset)),
                _ => None,
            })
            .collect();
        assert_eq!(
            arm_loads
                .iter()
                .map(|(_, offset)| *offset)
                .collect::<Vec<_>>(),
            vec![0, 8]
        );
        assert!(arm_bounds < arm_loads[0].0);

        // Lock down the two deliberately distinct compatibility leaves. A
        // simple borrow and inout reads must retain AArch64's base-only
        // LdrIndexed form,
        // while a projected ZST read must materialize zero without attempting
        // root-origin address arithmetic (and x86 keeps its 64-bit immediate).
        for by_ref_fn in ["read_borrow", "read_inout"] {
            let by_ref_cfg = build_cfg(by_ref_fn);
            let by_ref_x86 = X86CfgLower::new_unchecked(&by_ref_cfg, &output.type_pool, &interner)
                .lower()
                .expect("x86 by-ref fixture should lower");
            let by_ref_arm = Aarch64CfgLower::new_unchecked(
                &by_ref_cfg,
                &output.type_pool,
                &interner,
                Target::Aarch64Linux,
            )
            .lower()
            .expect("AArch64 by-ref fixture should lower");
            assert!(matches!(
                by_ref_x86.instructions(),
                [
                    X86Inst::MovRM { .. },
                    X86Inst::MovRMIndexed { .. },
                    X86Inst::MovRR { .. },
                    X86Inst::Ret
                ]
            ));
            assert!(matches!(
                by_ref_arm.instructions(),
                [
                    Aarch64Inst::Ldr { .. },
                    Aarch64Inst::LdrIndexed { .. },
                    Aarch64Inst::MovRR { .. },
                    Aarch64Inst::Ret
                ]
            ));
        }

        let unit_cfg = build_cfg("read_unit");
        let unit_x86 = X86CfgLower::new_unchecked(&unit_cfg, &output.type_pool, &interner)
            .lower()
            .expect("x86 ZST fixture should lower");
        let unit_arm = Aarch64CfgLower::new_unchecked(
            &unit_cfg,
            &output.type_pool,
            &interner,
            Target::Aarch64Linux,
        )
        .lower()
        .expect("AArch64 ZST fixture should lower");
        assert!(matches!(
            unit_x86.instructions(),
            [X86Inst::MovRI64 { imm: 0, .. }, X86Inst::Ret]
        ));
        assert!(matches!(
            unit_arm.instructions(),
            [Aarch64Inst::MovImm { imm: 0, .. }, Aarch64Inst::Ret]
        ));

        // A zero-sized indexed place has no load/store, but it still has a
        // language-level bounds check. Both the value and address paths must
        // retain the shared trap edge even though they materialize no bytes.
        let unit_index_cfg = build_cfg("read_unit_index");
        let unit_index_x86 =
            X86CfgLower::new_unchecked(&unit_index_cfg, &output.type_pool, &interner)
                .lower()
                .expect("x86 indexed ZST fixture should lower");
        let unit_index_arm = Aarch64CfgLower::new_unchecked(
            &unit_index_cfg,
            &output.type_pool,
            &interner,
            Target::Aarch64Linux,
        )
        .lower()
        .expect("AArch64 indexed ZST fixture should lower");
        assert!(unit_index_x86.instructions().iter().any(|inst| {
            matches!(inst, X86Inst::CallRel { symbol_id } if unit_index_x86.get_symbol(*symbol_id) == "__rue_bounds_check")
        }));
        assert!(unit_index_arm.instructions().iter().any(|inst| {
            matches!(inst, Aarch64Inst::Bl { symbol_id } if unit_index_arm.get_symbol(*symbol_id) == "__rue_bounds_check")
        }));

        let unit_write_cfg = build_cfg("write_unit_index");
        let unit_write_x86 =
            X86CfgLower::new_unchecked(&unit_write_cfg, &output.type_pool, &interner)
                .lower()
                .expect("x86 indexed ZST write fixture should lower");
        let unit_write_arm = Aarch64CfgLower::new_unchecked(
            &unit_write_cfg,
            &output.type_pool,
            &interner,
            Target::Aarch64Linux,
        )
        .lower()
        .expect("AArch64 indexed ZST write fixture should lower");
        assert!(unit_write_x86.instructions().iter().any(|inst| {
            matches!(inst, X86Inst::CallRel { symbol_id } if unit_write_x86.get_symbol(*symbol_id) == "__rue_bounds_check")
        }));
        assert!(unit_write_arm.instructions().iter().any(|inst| {
            matches!(inst, Aarch64Inst::Bl { symbol_id } if unit_write_arm.get_symbol(*symbol_id) == "__rue_bounds_check")
        }));
    }

    /// Taking a raw pointer into an element of a frame-resident non-slot-identical
    /// array (`@raw(grid.cells[i][j])`) is refused loudly on both backends: the
    /// frame stores the array slot-shaped while the raw pointer addresses memory
    /// by its packed compact image, so an `@ptr_offset` walk would stride across
    /// mismatched layouts (the compact-layout M2b contract, RUE-1035 / RUE-987).
    /// The sibling read/write projection test covers the paths that still lower.
    #[test]
    fn raw_pointer_into_frame_nested_array_is_refused_on_both_backends() {
        let source = r#"
            struct Grid { pad: i32, cells: [[i32; 2]; 2] }
            fn main() -> i32 {
                let mut grid = Grid { pad: 9, cells: [[10, 20], [30, 40]] };
                let i: u64 = 1;
                let j: u64 = 0;
                let _address: ptr const i32 = checked { @raw(grid.cells[i][j]) };
                grid.cells[i][j]
            }
        "#;

        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().expect("fixture should lex");
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().expect("fixture should parse");
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let output = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .analyze_all_for_test()
            .expect("fixture should analyze");
        let function = output
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("fixture function should exist");
        let cfg = CfgBuilder::build(
            &function.air,
            function.num_locals,
            function.num_param_slots,
            &function.name,
            &output.type_pool,
            function.param_modes.clone(),
            &interner,
            function.allow_unreachable_code,
            function.callable_kind,
        )
        .cfg
        .unwrap();

        let x86_err = X86CfgLower::new_unchecked(&cfg, &output.type_pool, &interner)
            .lower()
            .expect_err("x86 must refuse a raw pointer into a frame nested array");
        assert!(
            format!("{x86_err:?}").contains("frame-resident aggregate"),
            "unexpected x86 diagnostic: {x86_err:?}"
        );
        let arm_err = Aarch64CfgLower::new_unchecked(
            &cfg,
            &output.type_pool,
            &interner,
            Target::Aarch64Linux,
        )
        .lower()
        .expect_err("AArch64 must refuse a raw pointer into a frame nested array");
        assert!(
            format!("{arm_err:?}").contains("frame-resident aggregate"),
            "unexpected AArch64 diagnostic: {arm_err:?}"
        );
    }
}
