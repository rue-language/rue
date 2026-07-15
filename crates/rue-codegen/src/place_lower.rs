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

use rue_air::Type;
use rue_cfg::{CfgValue, Place, PlaceBase, Projection};

use crate::agg_slots::{self, SlotBackend};
use crate::allocation::{self, ScalePlan};
use crate::vreg::VReg;

/// Per-target instruction leaves used by shared place lowering.
pub trait PlaceLowerBackend: SlotBackend {
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

#[derive(Clone, Copy)]
struct IndexLevel {
    index: CfgValue,
    scale: ScalePlan,
}

struct ProjectionOffsets {
    static_slot_offset: u32,
    index_levels: Vec<IndexLevel>,
}

enum ProjectedAccess {
    /// A statically known frame slot containing the accessed value's low end.
    FrameSlot(u32),
    /// A fully formed address. Reads use the backend's base-only load form.
    PointerAddr(VReg),
    /// A received by-ref pointer plus a statically encoded displacement.
    PointerOffset { ptr: VReg, byte_offset: i32 },
}

/// Lower a scalar [`Place`] read, including projection bounds checks and
/// address formation.
pub fn lower_place_read<B: PlaceLowerBackend>(b: &mut B, dst: VReg, place: &Place, ty: Type) {
    // Even a zero-sized indexed place has a valid language-level access that
    // must reject an out-of-range index. There is no address to form or load
    // for the zero-sized value, but the bounds edge still has to be emitted.
    let projections = b.ctx().cfg.get_place_projections(place).to_vec();
    // This guard must precede root-origin arithmetic: a zero-slot root would
    // underflow at `root_count - 1`, and there are no bytes to load (RUE-577).
    if b.ctx().type_slot_count(ty) == 0 {
        if !projections.is_empty() {
            analyze_projections(b, &projections, true);
        }
        b.emit_zero_sized_place(dst);
        return;
    }

    // Clone the compact projection slice so generic emission can mutably
    // borrow the backend without retaining a borrow through its CFG context.
    if projections.is_empty() {
        match place.base {
            PlaceBase::Local(slot) => b.emit_load_slot(dst, slot),
            PlaceBase::Param(param_slot) => {
                if b.ctx().cfg.is_param_by_ref(param_slot) {
                    let ptr = b.ensure_by_ref_param_ptr(param_slot);
                    b.emit_load_ptr_base(dst, ptr);
                } else {
                    let slot = b.ctx().num_locals + param_slot;
                    b.emit_load_slot(dst, slot);
                }
            }
        }
        return;
    }

    let offsets = analyze_projections(b, &projections, true);
    match projected_access(b, place, offsets) {
        ProjectedAccess::FrameSlot(slot) => b.emit_load_slot(dst, slot),
        ProjectedAccess::PointerAddr(ptr) => b.emit_load_ptr_base(dst, ptr),
        ProjectedAccess::PointerOffset { ptr, byte_offset } => {
            b.emit_load_through_ptr(dst, ptr, byte_offset);
        }
    }
}

/// Lower a [`Place`] write. `vals` contains one vreg per logical value slot,
/// with slot zero first.
pub fn lower_place_write<B: PlaceLowerBackend>(b: &mut B, place: &Place, vals: &[VReg]) {
    let projections = b.ctx().cfg.get_place_projections(place).to_vec();
    // A zero-sized value has no storage to update. Current CFG callers filter
    // these writes before reaching codegen, but the index still needs its
    // language-level bounds edge. Keeping this boundary total avoids forming
    // an address for a value with no storage.
    if vals.is_empty() {
        if !projections.is_empty() {
            analyze_projections(b, &projections, true);
        }
        return;
    }

    if projections.is_empty() {
        match place.base {
            PlaceBase::Local(slot) => agg_slots::store_slots(b, vals, slot),
            PlaceBase::Param(param_slot) => {
                if b.ctx().cfg.is_param_by_ref(param_slot) {
                    let ptr = b.ensure_by_ref_param_ptr(param_slot);
                    agg_slots::store_slots_through_ptr(b, vals, ptr, 0);
                } else {
                    let slot = b.ctx().num_locals + param_slot;
                    agg_slots::store_slots(b, vals, slot);
                }
            }
        }
        return;
    }

    let offsets = analyze_projections(b, &projections, true);
    match projected_access(b, place, offsets) {
        ProjectedAccess::FrameSlot(low_slot) => {
            agg_slots::store_slots_at_low(b, vals, low_slot);
        }
        ProjectedAccess::PointerAddr(ptr) => {
            agg_slots::store_slots_through_ptr(b, vals, ptr, 0);
        }
        ProjectedAccess::PointerOffset { ptr, byte_offset } => {
            agg_slots::store_slots_through_ptr(b, vals, ptr, byte_offset);
        }
    }
}

/// Check every index projection in source order. Callers that form an address
/// directly (by-ref arguments and aggregate materialization) use this helper
/// so they cannot accidentally bypass the shared trap edge.
pub fn lower_place_bounds<B: PlaceLowerBackend + ?Sized>(b: &mut B, place: &Place) {
    let projections = b.ctx().cfg.get_place_projections(place).to_vec();
    for projection in projections {
        if let Projection::Index { array_type, index } = projection {
            let index_vreg = b.get_vreg(index);
            allocation::lower_bounds_check(
                b,
                allocation::BoundsCheckPlan::new(index_vreg, b.ctx().array_length(array_type)),
            );
        }
    }
}

/// Emit the low-end address of `place` into `dst`.
///
/// Index projections are intentionally unchecked here: `@raw` is unchecked,
/// while by-ref argument lowering performs its bounds checks before calling
/// through [`SlotBackend::emit_place_addr`].
pub fn lower_place_addr<B: PlaceLowerBackend>(b: &mut B, dst: VReg, place: &Place) {
    let projections = b.ctx().cfg.get_place_projections(place).to_vec();
    let offsets = analyze_projections(b, &projections, false);
    let root_count = agg_slots::root_slot_count(b, place);
    let dynamic_offset = compute_index_offset(b, &offsets.index_levels);

    match place.base {
        PlaceBase::Local(slot) => {
            let low_slot = slot + (root_count - 1) - offsets.static_slot_offset;
            b.emit_frame_addr(dst, low_slot);
            if let Some(dynamic_offset) = dynamic_offset {
                b.emit_addr_add(dst, dynamic_offset);
            }
        }
        PlaceBase::Param(param_slot) => {
            if b.ctx().cfg.is_param_by_ref(param_slot) {
                // A received by-ref pointer already denotes the caller place's
                // low end. Both field and index offsets therefore add.
                let ptr = b.ensure_by_ref_param_ptr(param_slot);
                b.emit_reg_move(dst, ptr);
                let static_byte_offset = offsets.static_slot_offset as i32 * 8;
                if static_byte_offset != 0 {
                    b.emit_addr_add_imm(dst, static_byte_offset);
                }
                if let Some(dynamic_offset) = dynamic_offset {
                    b.emit_addr_add(dst, dynamic_offset);
                }
            } else {
                let low_slot =
                    b.ctx().num_locals + param_slot + (root_count - 1) - offsets.static_slot_offset;
                b.emit_frame_addr(dst, low_slot);
                if let Some(dynamic_offset) = dynamic_offset {
                    b.emit_addr_add(dst, dynamic_offset);
                }
            }
        }
    }
}

fn analyze_projections<B: PlaceLowerBackend>(
    b: &mut B,
    projections: &[Projection],
    check_bounds: bool,
) -> ProjectionOffsets {
    let mut static_slot_offset = 0;
    let mut index_levels = Vec::new();

    for projection in projections {
        match projection {
            Projection::Field {
                struct_id,
                field_index,
            } => {
                static_slot_offset += b.ctx().struct_field_slot_offset(*struct_id, *field_index);
            }
            Projection::Index { array_type, index } => {
                if check_bounds {
                    // Preserve the historical ordering: every bounds check is
                    // emitted while walking the chain, before any address math.
                    let index_vreg = b.get_vreg(*index);
                    let length = b.ctx().array_length(*array_type);
                    allocation::lower_bounds_check(
                        b,
                        allocation::BoundsCheckPlan::new(index_vreg, length),
                    );
                }
                index_levels.push(IndexLevel {
                    index: *index,
                    scale: allocation::index_scale_plan(b.ctx().type_pool, *array_type),
                });
            }
        }
    }

    ProjectionOffsets {
        static_slot_offset,
        index_levels,
    }
}

fn projected_access<B: PlaceLowerBackend>(
    b: &mut B,
    place: &Place,
    offsets: ProjectionOffsets,
) -> ProjectedAccess {
    // Frame slots descend in address, so the root object's low-address end is
    // its highest-numbered slot. Once anchored there, logical aggregate slots,
    // field offsets, and scaled indices all ascend in address (ADR-0040).
    let root_count = agg_slots::root_slot_count(b, place);
    let dynamic_offset = compute_index_offset(b, &offsets.index_levels);

    match place.base {
        PlaceBase::Local(slot) => {
            let low_slot = slot + (root_count - 1) - offsets.static_slot_offset;
            frame_access(b, low_slot, dynamic_offset)
        }
        PlaceBase::Param(param_slot) => {
            if b.ctx().cfg.is_param_by_ref(param_slot) {
                let ptr = b.ensure_by_ref_param_ptr(param_slot);
                let static_byte_offset = offsets.static_slot_offset as i32 * 8;
                if let Some(dynamic_offset) = dynamic_offset {
                    let addr = b.alloc_vreg();
                    b.emit_reg_move(addr, ptr);
                    if static_byte_offset != 0 {
                        b.emit_addr_add_imm(addr, static_byte_offset);
                    }
                    b.emit_addr_add(addr, dynamic_offset);
                    ProjectedAccess::PointerAddr(addr)
                } else {
                    ProjectedAccess::PointerOffset {
                        ptr,
                        byte_offset: static_byte_offset,
                    }
                }
            } else {
                let low_slot =
                    b.ctx().num_locals + param_slot + (root_count - 1) - offsets.static_slot_offset;
                frame_access(b, low_slot, dynamic_offset)
            }
        }
    }
}

fn frame_access<B: PlaceLowerBackend>(
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

fn compute_index_offset<B: PlaceLowerBackend>(b: &mut B, levels: &[IndexLevel]) -> Option<VReg> {
    let mut total = None;

    for level in levels {
        let index = b.get_vreg(level.index);
        let scaled = b.alloc_vreg();
        b.emit_reg_move(scaled, index);
        b.emit_scale_index_bytes(scaled, level.scale);

        if let Some(previous) = total {
            b.emit_addr_add(previous, scaled);
        } else {
            total = Some(scaled);
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use rue_air::Sema;
    use rue_cfg::CfgBuilder;
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;
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
        let output = Sema::new(&rir, &mut interner, PreviewFeatures::new())
            .analyze_all()
            .expect("fixture should analyze");
        let build_cfg = |name: &str| {
            let function = output
                .functions
                .iter()
                .find(|function| function.name == name)
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
            )
            .cfg
        };
        let cfg = build_cfg("main");

        let x86 = X86CfgLower::new(&cfg, &output.type_pool, &interner)
            .lower()
            .expect("x86 fixture should lower");
        let arm = Aarch64CfgLower::new(&cfg, &output.type_pool, &interner, Target::Aarch64Linux)
            .lower()
            .expect("AArch64 fixture should lower");

        // RHS read, write destination, @raw's argument read/address pair, and
        // final read each walk both indices. Only the write emits an indexed
        // store; the three ordinary reads emit indexed loads. These counts
        // make projection traversal regressions visible without snapshotting
        // unrelated prologue/ABI details.
        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::ImulRR64 { .. }))
                .count(),
            5
        );
        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::Shl { .. }))
                .count(),
            5
        );
        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::MovRMIndexed { .. }))
                .count(),
            3
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
            5
        );
        assert_eq!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::LslImm { .. }))
                .count(),
            5
        );
        assert_eq!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::LdrIndexed { .. }))
                .count(),
            3
        );
        assert_eq!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::StrIndexedOffset { .. }))
                .count(),
            1
        );

        // Lock down the two deliberately distinct compatibility leaves. A
        // simple borrow and inout reads must retain AArch64's base-only
        // LdrIndexed form,
        // while a projected ZST read must materialize zero without attempting
        // root-origin address arithmetic (and x86 keeps its 64-bit immediate).
        for by_ref_fn in ["read_borrow", "read_inout"] {
            let by_ref_cfg = build_cfg(by_ref_fn);
            let by_ref_x86 = X86CfgLower::new(&by_ref_cfg, &output.type_pool, &interner)
                .lower()
                .expect("x86 by-ref fixture should lower");
            let by_ref_arm = Aarch64CfgLower::new(
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
        let unit_x86 = X86CfgLower::new(&unit_cfg, &output.type_pool, &interner)
            .lower()
            .expect("x86 ZST fixture should lower");
        let unit_arm = Aarch64CfgLower::new(
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
        let unit_index_x86 = X86CfgLower::new(&unit_index_cfg, &output.type_pool, &interner)
            .lower()
            .expect("x86 indexed ZST fixture should lower");
        let unit_index_arm = Aarch64CfgLower::new(
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
        let unit_write_x86 = X86CfgLower::new(&unit_write_cfg, &output.type_pool, &interner)
            .lower()
            .expect("x86 indexed ZST write fixture should lower");
        let unit_write_arm = Aarch64CfgLower::new(
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
}
