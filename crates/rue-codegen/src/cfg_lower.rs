//! Shared types and utilities for CFG lowering across backends.
//!
//! This module contains types and helper functions used by both x86_64 and aarch64
//! backends when lowering CFG to machine IR.
//!
//! ## Architecture
//!
//! The CFG lowering is split into two parts:
//!
//! 1. **Shared context** ([`CfgLowerContext`]): Holds common data and implements
//!    architecture-independent helper methods like type queries and chain tracing.
//!
//! 2. **Backend-specific lowering** (per-backend `CfgLower`): Each backend embeds
//!    a `CfgLowerContext` and implements instruction-specific lowering that produces
//!    its MIR type.
//!
//! This design eliminates significant code duplication while keeping the
//! instruction-specific logic where it belongs.

use std::fmt;

use lasso::{Key, ThreadedRodeo};
use rue_air::{FrozenTypeInternPool, StructId, TypeKind};
use rue_cfg::{BlockId, Cfg, CfgValue, Type};

use crate::types;

/// A single lowering decision: maps one CFG instruction to its MIR expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweringDecision {
    /// The CFG value (instruction) being lowered.
    pub cfg_value: CfgValue,
    /// Human-readable description of the CFG instruction.
    pub cfg_inst_desc: String,
    /// The type of the CFG instruction.
    pub cfg_type: String,
    /// Generated MIR instructions (as human-readable strings).
    pub mir_insts: Vec<String>,
    /// Rationale for the lowering decision (if non-obvious).
    pub rationale: Option<String>,
}

/// A lowering decision for a block terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminatorLoweringDecision {
    /// Human-readable description of the terminator.
    pub terminator_desc: String,
    /// Generated MIR instructions (as human-readable strings).
    pub mir_insts: Vec<String>,
    /// Rationale for the lowering decision.
    pub rationale: Option<String>,
    /// Target-independent topology and policy facts observed by both backend
    /// debug lowerers.  MIR text is intentionally kept separate from this
    /// trace so cross-target tests do not compare architecture spellings.
    pub policy_trace: crate::terminator_plan::TerminatorTrace,
}

/// Debug information for a single basic block's lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockLoweringInfo {
    /// The block ID.
    pub block_id: BlockId,
    /// Lowering decisions for instructions in this block.
    pub instructions: Vec<LoweringDecision>,
    /// Lowering decision for the terminator.
    pub terminator: Option<TerminatorLoweringDecision>,
}

/// Debug information from the CFG-to-MIR lowering pass.
///
/// This captures how each CFG instruction is expanded into MIR instructions,
/// including the rationale for instruction selection decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweringDebugInfo {
    /// Function name.
    pub fn_name: String,
    /// Target architecture (e.g., "x86_64", "aarch64").
    pub target_arch: String,
    /// Per-block lowering information.
    pub blocks: Vec<BlockLoweringInfo>,
}

impl fmt::Display for LoweringDebugInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "=== Instruction Selection ({}) ===", self.fn_name)?;
        writeln!(f)?;

        for block_info in &self.blocks {
            writeln!(f, "{}:", block_info.block_id)?;
            writeln!(f)?;

            for decision in &block_info.instructions {
                writeln!(
                    f,
                    "  CFG: {} = {} : {}",
                    decision.cfg_value, decision.cfg_inst_desc, decision.cfg_type
                )?;

                for mir_inst in &decision.mir_insts {
                    writeln!(f, "    -> {}", mir_inst)?;
                }

                if let Some(ref rationale) = decision.rationale {
                    writeln!(f, "    Decision: {}", rationale)?;
                }
                writeln!(f)?;
            }

            if let Some(ref term) = block_info.terminator {
                writeln!(f, "  Terminator: {}", term.terminator_desc)?;

                for mir_inst in &term.mir_insts {
                    writeln!(f, "    -> {}", mir_inst)?;
                }

                if let Some(ref rationale) = term.rationale {
                    writeln!(f, "    Decision: {}", rationale)?;
                }
                writeln!(f)?;
            }
        }

        Ok(())
    }
}

/// Format CFG instruction data with interned symbols resolved to stable names.
pub(crate) fn format_cfg_inst_data_with_interner(
    cfg: &rue_cfg::Cfg,
    data: &rue_cfg::CfgInstData,
    interner: &ThreadedRodeo,
) -> String {
    format_cfg_inst_data_impl(cfg, data, Some(interner))
}

fn format_cfg_inst_data_impl(
    cfg: &rue_cfg::Cfg,
    data: &rue_cfg::CfgInstData,
    interner: Option<&ThreadedRodeo>,
) -> String {
    use rue_cfg::CfgInstData;

    match data {
        CfgInstData::Const(v) => format!("const {}", v),
        CfgInstData::BoolConst(v) => format!("const {}", v),
        CfgInstData::StringConst(idx) => format!("string_const @{}", idx),
        CfgInstData::Param { index } => format!("param {}", index),
        CfgInstData::BlockParam { index } => format!("block_param {}", index),
        CfgInstData::Add(lhs, rhs) => format!("add {}, {}", lhs, rhs),
        CfgInstData::Sub(lhs, rhs) => format!("sub {}, {}", lhs, rhs),
        CfgInstData::Mul(lhs, rhs) => format!("mul {}, {}", lhs, rhs),
        CfgInstData::WrappingAdd(lhs, rhs) => format!("wrapping_add {}, {}", lhs, rhs),
        CfgInstData::WrappingSub(lhs, rhs) => format!("wrapping_sub {}, {}", lhs, rhs),
        CfgInstData::WrappingMul(lhs, rhs) => format!("wrapping_mul {}, {}", lhs, rhs),
        CfgInstData::Div(lhs, rhs) => format!("div {}, {}", lhs, rhs),
        CfgInstData::Mod(lhs, rhs) => format!("mod {}, {}", lhs, rhs),
        CfgInstData::Eq(lhs, rhs) => format!("eq {}, {}", lhs, rhs),
        CfgInstData::Ne(lhs, rhs) => format!("ne {}, {}", lhs, rhs),
        CfgInstData::Lt(lhs, rhs) => format!("lt {}, {}", lhs, rhs),
        CfgInstData::Gt(lhs, rhs) => format!("gt {}, {}", lhs, rhs),
        CfgInstData::Le(lhs, rhs) => format!("le {}, {}", lhs, rhs),
        CfgInstData::Ge(lhs, rhs) => format!("ge {}, {}", lhs, rhs),
        CfgInstData::BitAnd(lhs, rhs) => format!("bit_and {}, {}", lhs, rhs),
        CfgInstData::BitOr(lhs, rhs) => format!("bit_or {}, {}", lhs, rhs),
        CfgInstData::BitXor(lhs, rhs) => format!("bit_xor {}, {}", lhs, rhs),
        CfgInstData::Shl(lhs, rhs) => format!("shl {}, {}", lhs, rhs),
        CfgInstData::Shr(lhs, rhs) => format!("shr {}, {}", lhs, rhs),
        CfgInstData::Neg(v) => format!("neg {}", v),
        CfgInstData::Not(v) => format!("not {}", v),
        CfgInstData::BitNot(v) => format!("bit_not {}", v),
        CfgInstData::Alloc { slot, init } => format!("alloc ${} = {}", slot, init),
        CfgInstData::Load { slot } => format!("load ${}", slot),
        CfgInstData::Store { slot, value } => format!("store ${} = {}", slot, value),
        CfgInstData::ParamStore { param_slot, value } => {
            format!("param_store %{} = {}", param_slot, value)
        }
        CfgInstData::Call { runtime, name, .. } => {
            let args: Vec<String> = cfg
                .get_call_args(data)
                .iter()
                .map(|a| format!("{}", a.value))
                .collect();
            let name = interner
                .map(|interner| interner.resolve(name).to_string())
                .unwrap_or_else(|| name.into_usize().to_string());
            let name = runtime
                .map(|runtime| runtime.helper().helper().symbol.to_string())
                .unwrap_or(name);
            format!("call @{}({})", name, args.join(", "))
        }
        CfgInstData::Intrinsic {
            operation, name, ..
        } => {
            let args: Vec<String> = cfg
                .get_intrinsic_args(data)
                .iter()
                .map(|v| format!("{}", v))
                .collect();
            let name = interner
                .map(|interner| interner.resolve(name).to_string())
                .unwrap_or_else(|| name.into_usize().to_string());
            format!("intrinsic {operation:?} @{}({})", name, args.join(", "))
        }
        CfgInstData::StructInit { struct_id, .. } => {
            let fields: Vec<String> = cfg
                .get_struct_fields(data)
                .iter()
                .map(|v| format!("{}", v))
                .collect();
            format!("struct_init #{struct_id:?} {{{}}}", fields.join(", "))
        }
        CfgInstData::ArrayInit { .. } => {
            // Note: Can't show elements without Cfg access
            "array_init [...]".to_string()
        }
        CfgInstData::EnumVariant {
            enum_id,
            variant_index,
            ..
        } => {
            format!("enum_variant #{enum_id:?}.{variant_index}")
        }
        CfgInstData::EnumPayloadGet {
            base,
            enum_id,
            variant_index,
            field_index,
        } => {
            format!(
                "enum_payload_get {} #{:?}.{}.{}",
                base, enum_id, variant_index, field_index
            )
        }
        CfgInstData::IntCast { value, from_ty } => {
            format!("int_cast {} : {}", value, from_ty.name())
        }
        CfgInstData::Drop { value } => format!("drop {}", value),
        CfgInstData::StorageLive { slot, .. } => format!("storage_live ${}", slot),
        CfgInstData::StorageDead { slot, .. } => format!("storage_dead ${}", slot),
        CfgInstData::AccessorCall { name, .. } => match interner {
            Some(interner) => format!("accessor_call @{}", interner.resolve(name)),
            None => format!("accessor_call @{}", name.into_usize()),
        },
        // Place operations
        CfgInstData::PlaceRead { place } => {
            format!("place_read {}", cfg.place_to_string(place))
        }
        CfgInstData::PlaceWrite { place, value } => {
            format!("place_write {} = {}", cfg.place_to_string(place), value)
        }
    }
}

// ============================================================================
// Internal calling convention helpers
// ============================================================================

/// Does a by-value return of `ty` use the indirect-result convention
/// (caller-allocated return buffer, pointer in the target row's own
/// indirect-result register) instead of result registers?
///
/// The native convention returns a value in registers whenever its eightbytes
/// fit the native return bank — six general-purpose registers on x86-64 and
/// eight on AArch64, plus eight floating-point ones on each — classified by the
/// compilation target's own C aggregate rule (ADR-0084). Otherwise the callee
/// writes the value's compact image through a caller-provided pointer that
/// travels where the C row puts it: `rdi` with the `rax` echo on SysV AMD64,
/// the dedicated `x8` on AAPCS64.
///
/// The decision itself is [`rue_air::lower_native_return`]'s, reached through
/// [`crate::call_plan::return_plan`]; this is the thin boolean predicate the
/// sret decision sites and both backends consult.
pub fn type_uses_sret_return(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    pairing: rue_target::ConventionSpec,
) -> bool {
    crate::call_plan::return_plan(type_pool, ty, pairing).uses_sret()
}

/// Does this function return its value via the sret convention?
/// See [`type_uses_sret_return`] for the convention.
#[cfg(test)]
pub(crate) fn fn_uses_sret_return(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    pairing: rue_target::ConventionSpec,
) -> bool {
    type_uses_sret_return(type_pool, cfg.return_type(), pairing)
}

// ============================================================================
// Shared CFG Lowering Context
// ============================================================================

/// Shared context for CFG lowering operations.
///
/// This struct holds the common data needed by both x86_64 and aarch64 backends
/// and provides architecture-independent helper methods for:
///
/// - Type queries (slot counts, field offsets, array lengths)
/// - Builtin type detection and operator lookup
/// - Slot offset calculations
///
/// Each backend's `CfgLower` embeds this context and delegates to its methods.
#[derive(Clone, Copy)]
pub(crate) struct CfgLowerContext<'a> {
    /// The CFG being lowered.
    pub(crate) cfg: &'a Cfg,
    /// Type intern pool for struct/enum/array lookups.
    pub(crate) type_pool: &'a FrozenTypeInternPool,
    /// Number of local variable slots.
    pub(crate) num_locals: u32,
    /// Number of parameter ABI slots.
    pub(crate) num_params: u32,
    /// The pipeline's per-parameter storage decision (RUE-1170). `None` — a
    /// directly constructed context in tests — behaves as the historical
    /// all-homed layout: every parameter ABI slot has a frame home at
    /// `num_locals + index`.
    param_storage: Option<&'a crate::param_storage::ParamStoragePlan>,
    /// The pipeline's local frame-slot decision (RUE-768). `None` — a directly
    /// constructed context in tests — behaves as the historical identity
    /// layout: every CFG local slot keeps its own cell at its own index for
    /// the whole function.
    local_storage: Option<&'a crate::local_storage::LocalSlotPlan>,
}

impl<'a> CfgLowerContext<'a> {
    /// Create a new CFG lowering context with the historical all-homed
    /// parameter storage (every parameter ABI slot has a frame home) and the
    /// historical identity local layout. Production lowering supplies the
    /// pipeline's real plans via
    /// [`with_param_storage`](Self::with_param_storage) and
    /// [`with_local_storage`](Self::with_local_storage).
    pub(crate) fn new(cfg: &'a Cfg, type_pool: &'a FrozenTypeInternPool) -> Self {
        Self {
            cfg,
            type_pool,
            num_locals: cfg.num_locals(),
            num_params: cfg.num_params(),
            param_storage: None,
            local_storage: None,
        }
    }

    /// Install the pipeline's per-parameter storage decision (RUE-1170). The
    /// same plan drives the frame slot sums and the emitter's prologue, so
    /// lowering must consume this exact plan rather than re-deriving one.
    pub(crate) fn with_param_storage(
        mut self,
        param_storage: &'a crate::param_storage::ParamStoragePlan,
    ) -> Self {
        self.param_storage = Some(param_storage);
        self
    }

    /// Install the pipeline's local frame-slot decision (RUE-768). The same
    /// plan sets the base of the parameter area, the spill-placement floor,
    /// and the emitted frame size, so lowering must consume this exact plan
    /// rather than re-deriving one.
    pub(crate) fn with_local_storage(
        mut self,
        local_storage: &'a crate::local_storage::LocalSlotPlan,
    ) -> Self {
        self.local_storage = Some(local_storage);
        self
    }

    /// Register-only parameters needing an entry copy, as
    /// `(param ABI slot, incoming ABI index)` (RUE-1170). Empty without a
    /// pipeline plan: the historical layout homes everything.
    pub(crate) fn param_entry_copies(
        &self,
    ) -> Vec<(
        u32,
        crate::abi_slot_class::AbiSlotClass,
        crate::call_plan::AbiSlotLocation,
    )> {
        match self.param_storage {
            None => Vec::new(),
            Some(plan) => plan.entry_copies(self.cfg).collect(),
        }
    }

    /// Frame slots the (compacted) parameter area occupies (RUE-1170).
    pub(crate) fn homed_param_slots(&self) -> u32 {
        self.param_storage
            .map_or(self.num_params, |plan| plan.homed_area_slots())
    }

    /// Frame slots the (shared) local area occupies (RUE-768). This is the
    /// base of the emitted parameter area, which the CFG's own slot numbering
    /// places at `cfg.num_locals()`.
    pub(crate) fn frame_local_slots(&self) -> u32 {
        self.local_storage
            .map_or(self.num_locals, |plan| plan.frame_local_slots())
    }

    // ========================================================================
    // Type helpers
    // ========================================================================

    /// Get the length of an array type.
    pub fn array_length(&self, array_type: Type) -> u64 {
        types::array_length_from_type(self.type_pool, array_type)
    }

    /// Calculate the total number of slots needed to store a type.
    pub fn type_slot_count(&self, ty: Type) -> u32 {
        types::type_slot_count(self.type_pool, ty)
    }

    /// Whether `ty` is a multi-slot aggregate that must be materialized and
    /// stored slot-by-slot (struct, fixed-size array, or a payload-carrying
    /// enum). A discriminant-only (C-like) enum is a 1-slot scalar and is
    /// deliberately excluded so it keeps its existing scalar codegen path
    /// (RUE-221).
    pub fn is_multislot_aggregate(&self, ty: Type) -> bool {
        types::is_multislot_aggregate(self.type_pool, ty)
    }

    /// Calculate the slot count for a single element of an array type.
    pub fn array_element_slot_count(&self, array_type: Type) -> u32 {
        types::array_element_slot_count_from_type(self.type_pool, array_type)
    }

    /// Calculate the slot offset for a field within a struct.
    pub fn struct_field_slot_offset(&self, struct_id: StructId, field_index: u32) -> u32 {
        types::struct_field_slot_offset(self.type_pool, struct_id, field_index)
    }

    // ========================================================================
    // Builtin type helpers
    // ========================================================================

    /// Check if a type is the canonical trusted standard-library StrBuf.
    pub fn is_strbuf(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => self.type_pool.is_strbuf(struct_id),
            _ => false,
        }
    }

    /// Check if a type has string byte-content equality semantics.
    ///
    /// `StrBuf`, `str`, and `Str(N)` use byte-content equality rather than
    /// structural pointer/length equality. Semantic analysis asks the same
    /// question of a *component* of an aggregate — a string leaf compares by
    /// content at any depth (spec 4.3:3, RUE-1992) — so the view spelling is
    /// read through the one shared classifier rather than open-coded here:
    /// two walks with two answers would compare the same field by content on
    /// one and by address on the other.
    pub fn is_string_like_for_equality(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                self.is_strbuf(ty) || rue_air::is_string_view_struct_name(&struct_def.name)
            }
            _ => false,
        }
    }

    /// The frame slot holding the incoming sret pointer, one past the
    /// (compacted, RUE-1170) param area, which itself starts past the (shared,
    /// RUE-768) local area (only meaningful for an sret-returning function).
    /// The prologue stores the hidden first argument here; the return path
    /// loads it back to write the result through. Register-allocator spill
    /// slots start after this slot.
    pub fn sret_ptr_slot(&self) -> u32 {
        self.frame_local_slots() + self.homed_param_slots()
    }

    // ========================================================================
    // Slot helpers
    // ========================================================================

    /// Calculate the stack offset for a local variable slot.
    ///
    /// Local variables are stored at negative offsets from the frame pointer.
    /// The offset is a byte-based product of the frame-layout authority (before
    /// the backend's saved-register adjustment), not a re-derived `* 8`.
    pub fn local_offset(&self, slot: u32) -> i32 {
        crate::frame_layout::slot_offset_pre_saved(slot)
    }

    /// Check if a slot corresponds to a parameter ABI slot.
    ///
    /// Returns `Some(param_index)` if it is a parameter slot, `None` otherwise.
    /// In the CFG's slot numbering, parameter ABI slots follow local slots.
    pub fn slot_to_param_index(&self, slot: u32) -> Option<u32> {
        if slot >= self.num_locals && slot < self.num_locals + self.num_params {
            Some(slot - self.num_locals)
        } else {
            None
        }
    }

    /// Translate a CFG slot number into the emitted frame's slot number.
    ///
    /// The CFG numbers local slots `0..num_locals` assuming every local owns a
    /// cell for the whole function and parameter slots
    /// `num_locals..num_locals + num_params` assuming every parameter is
    /// homed. The emitted frame narrows both: locals with provably disjoint
    /// storage windows share cells (RUE-768) and register-only parameters are
    /// compacted away (RUE-1170).
    ///
    /// This is the single funnel every slot-addressed frame access passes
    /// through. Slots inside one multi-slot local keep their relative order,
    /// so the `frame_slot(base) + k` addressing every aggregate access uses
    /// stays correct. Reaching this with a register-only parameter slot is a
    /// planning bug (the storage plan must home every slot the body
    /// addresses), reported as a panic-backed ICE.
    pub fn frame_slot(&self, slot: u32) -> u32 {
        match self.slot_to_param_index(slot) {
            None => self.local_frame_slot(slot),
            Some(index) => self.param_frame_slot(index),
        }
    }

    /// The emitted frame slot of CFG local slot `slot` (RUE-768).
    fn local_frame_slot(&self, slot: u32) -> u32 {
        match self.local_storage {
            None => slot,
            Some(plan) => plan.frame_slot(slot),
        }
    }

    /// The emitted frame slot of parameter ABI slot `index`, which must be
    /// homed (RUE-1170).
    /// The by-value aggregate parameters whose leaves must be read back out of
    /// their compact image at entry (ADR-0084). Empty when no storage plan was
    /// supplied.
    pub(crate) fn param_unmarshals(&self) -> &[crate::param_storage::ParamUnmarshal] {
        self.param_storage.map_or(&[], |plan| plan.unmarshals())
    }

    pub fn param_frame_slot(&self, index: u32) -> u32 {
        let area_slot = match self.param_storage {
            None => index,
            Some(plan) => plan.area_slot(index).unwrap_or_else(|| {
                panic!(
                    "parameter ABI slot {index} is register-only but the body \
                     addresses its frame home; the storage plan must home every \
                     addressed parameter"
                )
            }),
        };
        self.frame_local_slots() + area_slot
    }

    /// The emitted frame slot holding logical slot 0 — the low end in address —
    /// of the `slot_count`-slot value that begins at parameter ABI slot `index`.
    ///
    /// A homed parameter is one contiguous frame image laid out ascending in
    /// address with its logical slots while frame slot numbers descend
    /// (ADR-0040), so the parameter's own slot 0 is its *last* frame slot and
    /// slot `k` is one slot back from slot `k - 1`. A `Param { index }` naming a
    /// slot inside a wider parameter — the leaf a cleanup body reads out of its
    /// owner (RUE-2074) — is therefore a projection into that image, addressed
    /// exactly as a place projection at the same slot offset would be, not a
    /// parameter of its own.
    ///
    /// With no grouped descriptors (a synthetic CFG) every slot is its own
    /// homed parameter, which is the same answer for `index` naming a whole
    /// parameter.
    pub fn param_value_low_slot(&self, index: u32, slot_count: u32) -> u32 {
        let group = self
            .cfg
            .source_param_abi()
            .iter()
            .find(|descriptor| {
                descriptor.start_slot <= index
                    && index < descriptor.start_slot + descriptor.slot_count
            })
            .map(|descriptor| (descriptor.start_slot, descriptor.slot_count));
        match group {
            Some((start_slot, group_slots)) => {
                let offset = index - start_slot;
                assert!(
                    offset + slot_count <= group_slots,
                    "a {slot_count}-slot value at offset {offset} runs past the \
                     {group_slots}-slot parameter that starts at slot {start_slot}"
                );
                self.param_frame_slot(start_slot) + group_slots - 1 - offset
            }
            None => self.param_frame_slot(index) + slot_count.saturating_sub(1),
        }
    }
}
