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

/// The canonical address of a zero-sized place (RUE-605).
///
/// A zero-sized place occupies no storage, so the slot model has no cell that
/// names it: an all-ZST root spans zero slots, and a zero-sized field at the
/// end of its root names the root's past-the-end address, which the descending
/// slot numbering cannot express at all (it underflowed `u32`, panicking in
/// debug and wrapping to a wild frame slot in release).
///
/// ADR-0052 ruling 3 settles the semantics: a zero-sized type has size 0 and
/// alignment 1, and a well-aligned dangling pointer to one is a valid,
/// non-dereferenceable address. Address-taking is therefore accepted and every
/// zero-sized place forms this one constant instead of a frame address: it is
/// non-null, well-aligned for alignment 1, frame-independent, and identical on
/// both backends. Nothing can dereference it — zero-sized reads return the
/// canonical zero value and zero-sized writes store no slots.
pub(crate) const ZERO_SIZED_PLACE_ADDR: i64 = 1;

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

    /// Materialize [`ZERO_SIZED_PLACE_ADDR`], the canonical address of a place
    /// that occupies no storage.
    ///
    /// The constant itself is shared; only the immediate-materialization
    /// instruction differs per target, so this leaf carries no policy.
    fn emit_zero_sized_place_addr(&mut self, dst: VReg);

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

/// Slot count of the sub-object the projection chain selects.
///
/// Every projection carries the type it projects *out of*, so the last link
/// alone determines the selected type; an empty chain selects the root. Zero
/// here means the place has no storage, which is what
/// [`ZERO_SIZED_PLACE_ADDR`] answers for.
fn resolved_projected_slot_count<B: PlaceLowerBackend + ?Sized>(
    b: &B,
    place: &ResolvedPlace,
) -> u32 {
    match place.projections.last() {
        None => resolved_root_count(b, place),
        Some(crate::value_plan::ProjectionPlan::Field {
            struct_id,
            field_index,
        }) => {
            let struct_def = b.ctx().type_pool.struct_def(*struct_id);
            b.ctx()
                .type_slot_count(struct_def.fields[*field_index as usize].ty)
        }
        Some(crate::value_plan::ProjectionPlan::Index { array_type, .. }) => {
            b.ctx().array_element_slot_count(*array_type)
        }
    }
}

/// The frame slot holding the low (lowest-addressed) end of a projected place.
///
/// The root occupies slots `base_slot ..= base_slot + root_count - 1` and slot
/// numbers descend in address, so the field at static slot offset `k` starts at
/// `base_slot + root_count - 1 - k`. A place with at least one slot always has
/// `k + slot_count <= root_count`, hence `k <= root_count - 1`, so the
/// subtraction is in range. A zero-sized place is the only shape that can push
/// `k` past the end, and every caller diverts those to
/// [`ZERO_SIZED_PLACE_ADDR`] before reaching here (RUE-605) — so an underflow
/// is a violated invariant, reported as an ICE rather than silently wrapping to
/// a wild slot in release builds.
fn projected_low_slot(base_slot: u32, root_count: u32, static_slot_offset: u32) -> u32 {
    (base_slot + root_count)
        .checked_sub(1 + static_slot_offset)
        .unwrap_or_else(|| {
            panic!(
                "projected place at slot offset {static_slot_offset} of a \
                 {root_count}-slot root has no storage slot; zero-sized places \
                 must be diverted to the canonical zero-sized address"
            )
        })
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
            projected_low_slot(slot, root_count, offsets.static_slot_offset),
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
        } => {
            let base_slot = b.ctx().param_frame_slot(slot);
            frame_access(
                b,
                projected_low_slot(base_slot, root_count, offsets.static_slot_offset),
                dynamic_offset,
            )
        }
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
    // `resolved_offsets` has already emitted the bounds checks, so a zero-sized
    // indexed place keeps its language-level trap edge even though the address
    // itself is a constant. The index math below is deliberately skipped: a
    // zero-sized element has a zero stride, so no index can move the address.
    if resolved_projected_slot_count(b, place) == 0 {
        b.emit_zero_sized_place_addr(dst);
        return;
    }
    let root_count = resolved_root_count(b, place);
    let dynamic = compute_resolved_index_offset(b, &offsets.index_levels);
    match place.base {
        crate::value_plan::PlaceBasePlan::Local(slot) => {
            b.emit_frame_addr(
                dst,
                projected_low_slot(slot, root_count, offsets.static_slot_offset),
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
            let base_slot = b.ctx().param_frame_slot(slot);
            b.emit_frame_addr(
                dst,
                projected_low_slot(base_slot, root_count, offsets.static_slot_offset),
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
    use rue_air::{
        AirEditor, AirPlaceBase, AirProjection, AirValidationContext, FrozenTypeInternPool,
        ParamSlotModes, SourceParamAbi, StructDef, StructField, StructId, Type, TypeInternPool,
    };
    use rue_cfg::{
        BlockId, Cfg, CfgArgMode, CfgBuilder, CfgCallArg, CfgInst, CfgInstData, CfgValue, Place,
        PlaceBase, Projection,
    };
    use rue_span::{FileId, Span};
    use rue_target::Target;

    use super::ZERO_SIZED_PLACE_ADDR;
    use crate::aarch64::{
        Aarch64Inst, CfgLower as Aarch64CfgLower, Operand as Aarch64Operand, Reg as Aarch64Reg,
    };
    use crate::x86_64::{CfgLower as X86CfgLower, X86Inst};

    fn span() -> Span {
        Span::new(0, 0)
    }

    fn value(cfg: &mut Cfg, block: BlockId, data: CfgInstData, ty: Type) -> CfgValue {
        cfg.append_inst(
            block,
            CfgInst {
                data,
                ty,
                span: span(),
            },
        )
    }

    fn konst(cfg: &mut Cfg, block: BlockId, literal: u64, ty: Type) -> CfgValue {
        value(cfg, block, CfgInstData::Const(literal), ty)
    }

    fn storage_live(cfg: &mut Cfg, block: BlockId, slot: u32, local_ty: Type) {
        value(
            cfg,
            block,
            CfgInstData::StorageLive { slot, local_ty },
            Type::UNIT,
        );
    }

    fn storage_dead(cfg: &mut Cfg, block: BlockId, slot: u32, local_ty: Type) {
        value(
            cfg,
            block,
            CfgInstData::StorageDead { slot, local_ty },
            Type::UNIT,
        );
    }

    fn alloc_slot(cfg: &mut Cfg, block: BlockId, slot: u32, init: CfgValue) {
        value(cfg, block, CfgInstData::Alloc { slot, init }, Type::UNIT);
    }

    fn load_slot(cfg: &mut Cfg, block: BlockId, slot: u32, ty: Type) -> CfgValue {
        value(cfg, block, CfgInstData::Load { slot }, ty)
    }

    /// Append a projected `PlaceWrite`. The projection payload is owner-issued,
    /// so the write is appended projection-free and rewritten in place.
    fn place_write(
        cfg: &mut Cfg,
        block: BlockId,
        base: PlaceBase,
        base_type: Type,
        projections: impl IntoIterator<Item = Projection>,
        stored: CfgValue,
    ) {
        let seed = match base {
            PlaceBase::Local(slot) => Place::local(slot, base_type),
            PlaceBase::Param(slot) => Place::param(slot, base_type),
            PlaceBase::Accessor(_) => unreachable!("accessor places are spliced before lowering"),
        };
        let instruction = value(
            cfg,
            block,
            CfgInstData::PlaceWrite {
                place: seed,
                value: stored,
            },
            Type::UNIT,
        );
        cfg.replace_place_write(instruction, base, base_type, projections, stored)
            .unwrap();
    }

    fn register_struct(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        fields: &[(&str, Type)],
    ) -> StructId {
        let (id, _) = pool.register_struct(
            interner.get_or_intern(name),
            StructDef {
                name: name.into(),
                fields: fields
                    .iter()
                    .map(|(field, ty)| StructField {
                        name: (*field).to_string(),
                        ty: *ty,
                    })
                    .collect(),
                is_copy: false,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        id
    }

    /// One direct scalar ABI descriptor per parameter slot.
    fn scalar_param_abi(count: u32) -> Vec<SourceParamAbi> {
        (0..count)
            .map(|slot| SourceParamAbi {
                start_slot: slot,
                slot_count: 1,
                crossing_regs: 1,
                ty: None,
            })
            .collect()
    }

    /// The shared type environment for the nested-projection fixtures:
    /// `struct Grid { pad: i32, cells: [[i32; 2]; 2] }` and the zero-sized
    /// `struct Empty { unit: () }` with its two-element array.
    struct GridFixture {
        pool: FrozenTypeInternPool,
        interner: ThreadedRodeo,
        grid_id: StructId,
        empty_id: StructId,
        grid_ty: Type,
        empty_ty: Type,
        inner_ty: Type,
        outer_ty: Type,
        empty_array_ty: Type,
        ptr_i32_ty: Type,
    }

    fn grid_fixture() -> GridFixture {
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let inner_ty = Type::new_array(pool.intern_array_from_type(Type::I32, 2));
        let outer_ty = Type::new_array(pool.intern_array_from_type(inner_ty, 2));
        let grid_id = register_struct(
            &pool,
            &interner,
            "Grid",
            &[("pad", Type::I32), ("cells", outer_ty)],
        );
        let empty_id = register_struct(&pool, &interner, "Empty", &[("unit", Type::UNIT)]);
        let empty_ty = Type::new_struct(empty_id);
        let empty_array_ty = Type::new_array(pool.intern_array_from_type(empty_ty, 2));
        let ptr_i32_ty = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::I32));
        GridFixture {
            pool: pool.freeze(),
            interner,
            grid_id,
            empty_id,
            grid_ty: Type::new_struct(grid_id),
            empty_ty,
            inner_ty,
            outer_ty,
            empty_array_ty,
            ptr_i32_ty,
        }
    }

    /// The two nested-index projections `grid.cells[i][j]` walk in every
    /// fixture: `.cells`, then the outer index, then the inner index.
    fn cells_projections(
        fixture: &GridFixture,
        outer_index: CfgValue,
        inner_index: CfgValue,
    ) -> [Projection; 3] {
        [
            Projection::Field {
                struct_id: fixture.grid_id,
                field_index: 1,
            },
            Projection::Index {
                array_type: fixture.outer_ty,
                index: outer_index,
            },
            Projection::Index {
                array_type: fixture.inner_ty,
                index: inner_index,
            },
        ]
    }

    /// Build the Grid `main` CFG the pipeline produces for:
    ///
    /// ```text
    /// let mut scalar = 7;                                   // slot 0
    /// let _borrow_read = read_borrow(borrow scalar);        // slot 1
    /// let _scalar_read = read_inout(inout scalar);          // slot 2
    /// let empty = Empty { unit: () };                       // slot 3 (0 slots)
    /// read_unit(empty);
    /// let mut empty_values = [Empty { unit: () }; 2];       // slot 3 (0 slots)
    /// read_unit_index(borrow empty_values, 0);
    /// write_unit_index(inout empty_values, 0);
    /// let mut grid = Grid { pad: 9, cells: [[10,20],[30,40]] }; // slots 3..8
    /// let i: u64 = 1;                                       // slot 8
    /// let j: u64 = 0;                                       // slot 9
    /// grid.cells[i][j] = grid.cells[i][j] + 1;
    /// grid.cells[i][j]
    /// ```
    fn grid_main_cfg(fixture: &GridFixture) -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 10, 0, "main".to_string(), Vec::new());
        let entry = cfg.new_block();
        cfg.entry = entry;

        storage_live(&mut cfg, entry, 0, Type::I32);
        let seven = konst(&mut cfg, entry, 7, Type::I32);
        alloc_slot(&mut cfg, entry, 0, seven);

        for (slot, callee, mode) in [
            (1u32, "read_borrow", CfgArgMode::Borrow),
            (2, "read_inout", CfgArgMode::Inout),
        ] {
            storage_live(&mut cfg, entry, slot, Type::I32);
            let scalar = load_slot(&mut cfg, entry, 0, Type::I32);
            let result = cfg
                .append_call(
                    entry,
                    None,
                    fixture.interner.get_or_intern(callee),
                    [CfgCallArg {
                        value: scalar,
                        mode,
                    }],
                    Type::I32,
                    span(),
                )
                .unwrap();
            alloc_slot(&mut cfg, entry, slot, result);
        }

        storage_live(&mut cfg, entry, 3, fixture.empty_ty);
        let unit = konst(&mut cfg, entry, 0, Type::UNIT);
        let empty = cfg
            .append_struct_init(entry, fixture.empty_id, [unit], fixture.empty_ty, span())
            .unwrap();
        alloc_slot(&mut cfg, entry, 3, empty);
        let empty = load_slot(&mut cfg, entry, 3, fixture.empty_ty);
        cfg.append_call(
            entry,
            None,
            fixture.interner.get_or_intern("read_unit"),
            [CfgCallArg {
                value: empty,
                mode: CfgArgMode::Normal,
            }],
            Type::UNIT,
            span(),
        )
        .unwrap();

        storage_live(&mut cfg, entry, 3, fixture.empty_array_ty);
        let elements: Vec<CfgValue> = (0..2)
            .map(|_| {
                let unit = konst(&mut cfg, entry, 0, Type::UNIT);
                cfg.append_struct_init(entry, fixture.empty_id, [unit], fixture.empty_ty, span())
                    .unwrap()
            })
            .collect();
        let empty_values = cfg
            .append_array_init(entry, elements, fixture.empty_array_ty, span())
            .unwrap();
        alloc_slot(&mut cfg, entry, 3, empty_values);
        for (callee, mode) in [
            ("read_unit_index", CfgArgMode::Borrow),
            ("write_unit_index", CfgArgMode::Inout),
        ] {
            let array = load_slot(&mut cfg, entry, 3, fixture.empty_array_ty);
            let index = konst(&mut cfg, entry, 0, Type::U64);
            cfg.append_call(
                entry,
                None,
                fixture.interner.get_or_intern(callee),
                [
                    CfgCallArg { value: array, mode },
                    CfgCallArg {
                        value: index,
                        mode: CfgArgMode::Normal,
                    },
                ],
                Type::UNIT,
                span(),
            )
            .unwrap();
        }

        storage_live(&mut cfg, entry, 3, fixture.grid_ty);
        let pad = konst(&mut cfg, entry, 9, Type::I32);
        let rows: Vec<CfgValue> = [[10u64, 20], [30, 40]]
            .into_iter()
            .map(|row| {
                let cells: Vec<CfgValue> = row
                    .into_iter()
                    .map(|cell| konst(&mut cfg, entry, cell, Type::I32))
                    .collect();
                cfg.append_array_init(entry, cells, fixture.inner_ty, span())
                    .unwrap()
            })
            .collect();
        let cells = cfg
            .append_array_init(entry, rows, fixture.outer_ty, span())
            .unwrap();
        let grid = cfg
            .append_struct_init(
                entry,
                fixture.grid_id,
                [pad, cells],
                fixture.grid_ty,
                span(),
            )
            .unwrap();
        alloc_slot(&mut cfg, entry, 3, grid);

        storage_live(&mut cfg, entry, 8, Type::U64);
        let one = konst(&mut cfg, entry, 1, Type::U64);
        alloc_slot(&mut cfg, entry, 8, one);
        storage_live(&mut cfg, entry, 9, Type::U64);
        let zero = konst(&mut cfg, entry, 0, Type::U64);
        alloc_slot(&mut cfg, entry, 9, zero);

        let i = load_slot(&mut cfg, entry, 8, Type::U64);
        let j = load_slot(&mut cfg, entry, 9, Type::U64);
        let projections = cells_projections(fixture, i, j);
        let element = cfg
            .append_place_read(
                entry,
                PlaceBase::Local(3),
                fixture.grid_ty,
                projections,
                Type::I32,
                span(),
            )
            .unwrap();
        let one = konst(&mut cfg, entry, 1, Type::I32);
        let bumped = value(&mut cfg, entry, CfgInstData::Add(element, one), Type::I32);
        let i = load_slot(&mut cfg, entry, 8, Type::U64);
        let j = load_slot(&mut cfg, entry, 9, Type::U64);
        let projections = cells_projections(fixture, i, j);
        place_write(
            &mut cfg,
            entry,
            PlaceBase::Local(3),
            fixture.grid_ty,
            projections,
            bumped,
        );
        let i = load_slot(&mut cfg, entry, 8, Type::U64);
        let j = load_slot(&mut cfg, entry, 9, Type::U64);
        let projections = cells_projections(fixture, i, j);
        let result = cfg
            .append_place_read(
                entry,
                PlaceBase::Local(3),
                fixture.grid_ty,
                projections,
                Type::I32,
                span(),
            )
            .unwrap();

        storage_dead(&mut cfg, entry, 9, Type::U64);
        storage_dead(&mut cfg, entry, 8, Type::U64);
        storage_dead(&mut cfg, entry, 3, fixture.grid_ty);
        storage_dead(&mut cfg, entry, 3, fixture.empty_array_ty);
        storage_dead(&mut cfg, entry, 3, fixture.empty_ty);
        storage_dead(&mut cfg, entry, 2, Type::I32);
        storage_dead(&mut cfg, entry, 1, Type::I32);
        storage_dead(&mut cfg, entry, 0, Type::I32);
        cfg.set_return(entry, Some(result));
        cfg
    }

    /// `fn read_borrow(borrow value: i32) -> i32 { value }` (and the `inout`
    /// twin): one by-reference scalar parameter read and returned.
    fn by_ref_scalar_read_cfg(fixture: &GridFixture, name: &str, writable: bool) -> Cfg {
        let mut cfg = Cfg::new(
            Type::I32,
            0,
            1,
            name.to_string(),
            ParamSlotModes::new(vec![true], vec![writable]),
        );
        cfg.set_source_param_abi(scalar_param_abi(1));
        let entry = cfg.new_block();
        cfg.entry = entry;
        let parameter = value(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::I32);
        cfg.set_return(entry, Some(parameter));
        let _ = fixture;
        cfg
    }

    /// `fn read_unit(value: Empty) -> () { value.unit }`: a projected read of
    /// a zero-sized field from a zero-slot by-value parameter. The parameter
    /// occupies zero ABI slots, so the CFG is produced through the AIR
    /// builder, exactly as the pipeline builds it.
    fn read_unit_cfg(fixture: &GridFixture) -> rue_cfg::ValidatedCfg {
        let mut air = AirEditor::new(Type::UNIT);
        let place = air
            .make_place(
                AirPlaceBase::Param(0),
                fixture.empty_ty,
                [AirProjection::Field {
                    struct_id: fixture.empty_id,
                    field_index: 0,
                }],
            )
            .unwrap();
        let read = air.add_place_read(place, Type::UNIT, span());
        air.add_ret(Some(read), Type::UNIT, span());
        let air = air
            .finish(AirValidationContext::Canonical(&fixture.pool))
            .expect("test AIR must validate");
        let output = CfgBuilder::build(
            &air,
            0,
            0,
            "read_unit",
            &fixture.pool,
            vec![],
            &fixture.interner,
            false,
            rue_air::AnalyzedCallableKind::Ordinary,
        );
        output.cfg.expect("test CFG must build")
    }

    /// `fn read_unit_index(borrow arr: [Empty; 2], i: u64) -> () { arr[i].unit }`:
    /// an indexed zero-sized read that must keep its bounds check.
    fn read_unit_index_cfg(fixture: &GridFixture) -> Cfg {
        let mut cfg = Cfg::new(
            Type::UNIT,
            0,
            2,
            "read_unit_index".to_string(),
            ParamSlotModes::new(vec![true, false], vec![false, false]),
        );
        cfg.set_source_param_abi(scalar_param_abi(2));
        let entry = cfg.new_block();
        cfg.entry = entry;
        let index = value(&mut cfg, entry, CfgInstData::Param { index: 1 }, Type::U64);
        cfg.append_place_read(
            entry,
            PlaceBase::Param(0),
            fixture.empty_array_ty,
            [
                Projection::Index {
                    array_type: fixture.empty_array_ty,
                    index,
                },
                Projection::Field {
                    struct_id: fixture.empty_id,
                    field_index: 0,
                },
            ],
            Type::UNIT,
            span(),
        )
        .unwrap();
        cfg.set_return(entry, None);
        cfg
    }

    /// `fn write_unit_index(inout arr: [Empty; 2], i: u64) { arr[i] = Empty { unit: () }; }`:
    /// an indexed zero-sized write that must keep its bounds check.
    fn write_unit_index_cfg(fixture: &GridFixture) -> Cfg {
        let mut cfg = Cfg::new(
            Type::UNIT,
            0,
            2,
            "write_unit_index".to_string(),
            ParamSlotModes::new(vec![true, false], vec![true, false]),
        );
        cfg.set_source_param_abi(scalar_param_abi(2));
        let entry = cfg.new_block();
        cfg.entry = entry;
        let unit = konst(&mut cfg, entry, 0, Type::UNIT);
        let element = cfg
            .append_struct_init(entry, fixture.empty_id, [unit], fixture.empty_ty, span())
            .unwrap();
        let index = value(&mut cfg, entry, CfgInstData::Param { index: 1 }, Type::U64);
        place_write(
            &mut cfg,
            entry,
            PlaceBase::Param(0),
            fixture.empty_array_ty,
            [Projection::Index {
                array_type: fixture.empty_array_ty,
                index,
            }],
            element,
        );
        konst(&mut cfg, entry, 0, Type::UNIT);
        cfg.set_return(entry, None);
        cfg
    }

    /// Exercise all three shared entry points with a field followed by two
    /// index projections. The outer index has a two-slot stride (multiply),
    /// while the inner scalar index has a one-slot stride (shift).
    #[test]
    fn nested_field_index_read_write_and_addr_lower_on_both_backends() {
        let fixture = grid_fixture();
        let cfg = grid_main_cfg(&fixture);

        let x86 = X86CfgLower::new_unchecked(&cfg, &fixture.pool, &fixture.interner)
            .lower()
            .expect("x86 fixture should lower");
        let arm = Aarch64CfgLower::new_unchecked(
            &cfg,
            &fixture.pool,
            &fixture.interner,
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
                name: "IndexedPair".into(),
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
        let pair_x86 =
            X86CfgLower::new_unchecked(&indexed_cfg, &synthetic_types, &synthetic_interner)
                .lower()
                .expect("x86 indexed aggregate read should lower");
        let pair_arm = Aarch64CfgLower::new_unchecked(
            &indexed_cfg,
            &synthetic_types,
            &synthetic_interner,
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
                    X86Inst::CallRel { symbol_id, .. }
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
                    Aarch64Inst::Bl { symbol_id, .. }
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
        for (by_ref_fn, writable) in [("read_borrow", false), ("read_inout", true)] {
            let by_ref_cfg = by_ref_scalar_read_cfg(&fixture, by_ref_fn, writable);
            let by_ref_x86 =
                X86CfgLower::new_unchecked(&by_ref_cfg, &fixture.pool, &fixture.interner)
                    .lower()
                    .expect("x86 by-ref fixture should lower");
            let by_ref_arm = Aarch64CfgLower::new_unchecked(
                &by_ref_cfg,
                &fixture.pool,
                &fixture.interner,
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

        let unit_cfg = read_unit_cfg(&fixture);
        let unit_x86 = X86CfgLower::new_unchecked(&unit_cfg, &fixture.pool, &fixture.interner)
            .lower()
            .expect("x86 ZST fixture should lower");
        let unit_arm = Aarch64CfgLower::new_unchecked(
            &unit_cfg,
            &fixture.pool,
            &fixture.interner,
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
        let unit_index_cfg = read_unit_index_cfg(&fixture);
        let unit_index_x86 =
            X86CfgLower::new_unchecked(&unit_index_cfg, &fixture.pool, &fixture.interner)
                .lower()
                .expect("x86 indexed ZST fixture should lower");
        let unit_index_arm = Aarch64CfgLower::new_unchecked(
            &unit_index_cfg,
            &fixture.pool,
            &fixture.interner,
            Target::Aarch64Linux,
        )
        .lower()
        .expect("AArch64 indexed ZST fixture should lower");
        assert!(unit_index_x86.instructions().iter().any(|inst| {
            matches!(inst, X86Inst::CallRel { symbol_id, .. } if unit_index_x86.get_symbol(*symbol_id) == "__rue_bounds_check")
        }));
        assert!(unit_index_arm.instructions().iter().any(|inst| {
            matches!(inst, Aarch64Inst::Bl { symbol_id, .. } if unit_index_arm.get_symbol(*symbol_id) == "__rue_bounds_check")
        }));

        let unit_write_cfg = write_unit_index_cfg(&fixture);
        let unit_write_x86 =
            X86CfgLower::new_unchecked(&unit_write_cfg, &fixture.pool, &fixture.interner)
                .lower()
                .expect("x86 indexed ZST write fixture should lower");
        let unit_write_arm = Aarch64CfgLower::new_unchecked(
            &unit_write_cfg,
            &fixture.pool,
            &fixture.interner,
            Target::Aarch64Linux,
        )
        .lower()
        .expect("AArch64 indexed ZST write fixture should lower");
        assert!(unit_write_x86.instructions().iter().any(|inst| {
            matches!(inst, X86Inst::CallRel { symbol_id, .. } if unit_write_x86.get_symbol(*symbol_id) == "__rue_bounds_check")
        }));
        assert!(unit_write_arm.instructions().iter().any(|inst| {
            matches!(inst, Aarch64Inst::Bl { symbol_id, .. } if unit_write_arm.get_symbol(*symbol_id) == "__rue_bounds_check")
        }));
    }

    /// Forming the address of a place with no storage yields
    /// [`ZERO_SIZED_PLACE_ADDR`] rather than frame slot arithmetic (RUE-605).
    ///
    /// Both address-forming entry points are exercised: `@raw` through
    /// [`lower_place_addr_plan`], and a `borrow` argument through
    /// [`lower_checked_place_addr_plan`]. Both root shapes are exercised too:
    /// an all-ZST root, which spans zero slots, and a zero-sized field at the
    /// end of a sized root, which names the root's past-the-end address. Before
    /// the fix the latter underflowed the descending slot numbering — an ICE in
    /// debug, a wild frame slot in release.
    #[test]
    fn zero_sized_place_addresses_are_canonical_on_both_backends() {
        // The CFG models:
        //
        //     let empty = Empty { unit: () };                  // slot 0 (0 slots)
        //     let _all_zst: ptr const () = @raw(empty.unit);   // slot 0
        //     let tail = Tail { value: 7, unit: () };          // slot 1
        //     let _tail_zst: ptr const () = @raw(tail.unit);   // slot 2
        //     take_unit(borrow tail.unit)
        //
        // Both `@raw` operands mark their roots address-taken, as the builder
        // does.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let empty_id = register_struct(&pool, &interner, "Empty", &[("unit", Type::UNIT)]);
        let tail_id = register_struct(
            &pool,
            &interner,
            "Tail",
            &[("value", Type::I64), ("unit", Type::UNIT)],
        );
        let empty_ty = Type::new_struct(empty_id);
        let tail_ty = Type::new_struct(tail_id);
        let ptr_unit_ty = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::UNIT));
        let pool = pool.freeze();
        let raw = interner.get_or_intern("raw");

        let mut cfg = Cfg::new(Type::I32, 3, 0, "main".to_string(), Vec::new());
        cfg.mark_address_taken(0);
        cfg.mark_address_taken(1);
        let entry = cfg.new_block();
        cfg.entry = entry;
        storage_live(&mut cfg, entry, 0, empty_ty);
        let unit = konst(&mut cfg, entry, 0, Type::UNIT);
        let empty = cfg
            .append_struct_init(entry, empty_id, [unit], empty_ty, span())
            .unwrap();
        alloc_slot(&mut cfg, entry, 0, empty);
        storage_live(&mut cfg, entry, 0, ptr_unit_ty);
        let empty_unit = cfg
            .append_place_read(
                entry,
                PlaceBase::Local(0),
                empty_ty,
                [Projection::Field {
                    struct_id: empty_id,
                    field_index: 0,
                }],
                Type::UNIT,
                span(),
            )
            .unwrap();
        let all_zst = cfg
            .append_intrinsic(entry, None, raw, [empty_unit], ptr_unit_ty, span())
            .unwrap();
        alloc_slot(&mut cfg, entry, 0, all_zst);
        storage_live(&mut cfg, entry, 1, tail_ty);
        let seven = konst(&mut cfg, entry, 7, Type::I64);
        let unit = konst(&mut cfg, entry, 0, Type::UNIT);
        let tail = cfg
            .append_struct_init(entry, tail_id, [seven, unit], tail_ty, span())
            .unwrap();
        alloc_slot(&mut cfg, entry, 1, tail);
        storage_live(&mut cfg, entry, 2, ptr_unit_ty);
        let tail_unit = cfg
            .append_place_read(
                entry,
                PlaceBase::Local(1),
                tail_ty,
                [Projection::Field {
                    struct_id: tail_id,
                    field_index: 1,
                }],
                Type::UNIT,
                span(),
            )
            .unwrap();
        let tail_zst = cfg
            .append_intrinsic(entry, None, raw, [tail_unit], ptr_unit_ty, span())
            .unwrap();
        alloc_slot(&mut cfg, entry, 2, tail_zst);
        let borrow_operand = cfg
            .append_place_read(
                entry,
                PlaceBase::Local(1),
                tail_ty,
                [Projection::Field {
                    struct_id: tail_id,
                    field_index: 1,
                }],
                Type::UNIT,
                span(),
            )
            .unwrap();
        let result = cfg
            .append_call(
                entry,
                None,
                interner.get_or_intern("take_unit"),
                [CfgCallArg {
                    value: borrow_operand,
                    mode: CfgArgMode::Borrow,
                }],
                Type::I32,
                span(),
            )
            .unwrap();
        storage_dead(&mut cfg, entry, 2, ptr_unit_ty);
        storage_dead(&mut cfg, entry, 1, tail_ty);
        storage_dead(&mut cfg, entry, 0, ptr_unit_ty);
        storage_dead(&mut cfg, entry, 0, empty_ty);
        cfg.set_return(entry, Some(result));

        let x86 = X86CfgLower::new_unchecked(&cfg, &pool, &interner)
            .lower()
            .expect("x86 zero-sized address fixture should lower");
        let arm = Aarch64CfgLower::new_unchecked(&cfg, &pool, &interner, Target::Aarch64Linux)
            .lower()
            .expect("AArch64 zero-sized address fixture should lower");

        // Three zero-sized addresses, one canonical constant each, and no frame
        // address formed for any of them: the only `lea`/`add fp` in the
        // function would come from this lowering, since nothing else here takes
        // an address.
        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(
                    inst,
                    X86Inst::MovRI64 { imm, .. } if *imm == ZERO_SIZED_PLACE_ADDR
                ))
                .count(),
            3
        );
        assert!(
            !x86.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Lea { .. })),
            "a zero-sized place must not form a frame address"
        );
        assert_eq!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(
                    inst,
                    Aarch64Inst::MovImm { imm, .. } if *imm == ZERO_SIZED_PLACE_ADDR
                ))
                .count(),
            3
        );
        assert!(
            !arm.instructions().iter().any(|inst| matches!(
                inst,
                Aarch64Inst::AddImm {
                    src: Aarch64Operand::Physical(Aarch64Reg::Fp),
                    ..
                }
            )),
            "a zero-sized place must not form a frame address"
        );
    }

    /// Taking a raw pointer into an element of a frame-resident non-slot-identical
    /// array (`@raw(grid.cells[i][j])`) is refused loudly on both backends: the
    /// frame stores the array slot-shaped while the raw pointer addresses memory
    /// by its packed compact image, so an `@ptr_offset` walk would stride across
    /// mismatched layouts (the compact-layout M2b contract, RUE-1035 / RUE-987).
    /// The sibling read/write projection test covers the paths that still lower.
    #[test]
    fn raw_pointer_into_frame_nested_array_is_refused_on_both_backends() {
        // The CFG models:
        //
        //     let mut grid = Grid { pad: 9, cells: [[10,20],[30,40]] }; // slots 0..5
        //     let i: u64 = 1;                                          // slot 5
        //     let j: u64 = 0;                                          // slot 6
        //     let _address: ptr const i32 = @raw(grid.cells[i][j]);    // slot 7
        //     grid.cells[i][j]
        //
        // `@raw` marks `grid` address-taken, as the builder does.
        let fixture = grid_fixture();
        let interner = &fixture.interner;
        let ptr_i32_ty = fixture.ptr_i32_ty;

        let mut cfg = Cfg::new(Type::I32, 8, 0, "main".to_string(), Vec::new());
        cfg.mark_address_taken(0);
        let entry = cfg.new_block();
        cfg.entry = entry;
        storage_live(&mut cfg, entry, 0, fixture.grid_ty);
        let pad = konst(&mut cfg, entry, 9, Type::I32);
        let rows: Vec<CfgValue> = [[10u64, 20], [30, 40]]
            .into_iter()
            .map(|row| {
                let cells: Vec<CfgValue> = row
                    .into_iter()
                    .map(|cell| konst(&mut cfg, entry, cell, Type::I32))
                    .collect();
                cfg.append_array_init(entry, cells, fixture.inner_ty, span())
                    .unwrap()
            })
            .collect();
        let cells = cfg
            .append_array_init(entry, rows, fixture.outer_ty, span())
            .unwrap();
        let grid = cfg
            .append_struct_init(
                entry,
                fixture.grid_id,
                [pad, cells],
                fixture.grid_ty,
                span(),
            )
            .unwrap();
        alloc_slot(&mut cfg, entry, 0, grid);
        storage_live(&mut cfg, entry, 5, Type::U64);
        let one = konst(&mut cfg, entry, 1, Type::U64);
        alloc_slot(&mut cfg, entry, 5, one);
        storage_live(&mut cfg, entry, 6, Type::U64);
        let zero = konst(&mut cfg, entry, 0, Type::U64);
        alloc_slot(&mut cfg, entry, 6, zero);
        storage_live(&mut cfg, entry, 7, ptr_i32_ty);
        let i = load_slot(&mut cfg, entry, 5, Type::U64);
        let j = load_slot(&mut cfg, entry, 6, Type::U64);
        let projections = cells_projections(&fixture, i, j);
        let element = cfg
            .append_place_read(
                entry,
                PlaceBase::Local(0),
                fixture.grid_ty,
                projections,
                Type::I32,
                span(),
            )
            .unwrap();
        let address = cfg
            .append_intrinsic(
                entry,
                None,
                interner.get_or_intern("raw"),
                [element],
                ptr_i32_ty,
                span(),
            )
            .unwrap();
        alloc_slot(&mut cfg, entry, 7, address);
        let i = load_slot(&mut cfg, entry, 5, Type::U64);
        let j = load_slot(&mut cfg, entry, 6, Type::U64);
        let projections = cells_projections(&fixture, i, j);
        let result = cfg
            .append_place_read(
                entry,
                PlaceBase::Local(0),
                fixture.grid_ty,
                projections,
                Type::I32,
                span(),
            )
            .unwrap();
        storage_dead(&mut cfg, entry, 7, ptr_i32_ty);
        storage_dead(&mut cfg, entry, 6, Type::U64);
        storage_dead(&mut cfg, entry, 5, Type::U64);
        storage_dead(&mut cfg, entry, 0, fixture.grid_ty);
        cfg.set_return(entry, Some(result));

        let x86_err = X86CfgLower::new_unchecked(&cfg, &fixture.pool, interner)
            .lower()
            .expect_err("x86 must refuse a raw pointer into a frame nested array");
        assert!(
            format!("{x86_err:?}").contains("frame-resident aggregate"),
            "unexpected x86 diagnostic: {x86_err:?}"
        );
        let arm_err =
            Aarch64CfgLower::new_unchecked(&cfg, &fixture.pool, interner, Target::Aarch64Linux)
                .lower()
                .expect_err("AArch64 must refuse a raw pointer into a frame nested array");
        assert!(
            format!("{arm_err:?}").contains("frame-resident aggregate"),
            "unexpected AArch64 diagnostic: {arm_err:?}"
        );
    }
}
