//! Target-independent CFG lowering: context, drivers, and debug reporting.
//!
//! ## Architecture
//!
//! CFG lowering is split three ways:
//!
//! 1. **Shared context** ([`CfgLowerContext`]): the CFG, the type pool, and the
//!    frame-slot translation every addressed access passes through.
//!
//! 2. **Shared drivers** ([`LoweringDriverBackend`] and the functions below):
//!    the parts that decide *what* a lowering does — how a terminator's edges
//!    are ordered, where a parameter is read from, what the entry preamble
//!    copies, which cleanup calls a drop plan makes. These read the shared
//!    plans (`terminator_plan`, `value_plan`, `call_plan`) and reach the
//!    machine through per-target leaves.
//!
//! 3. **Backend-specific lowering** (per-backend `CfgLower`): the instruction
//!    spellings, the register-class-specific sequences, and the arms that are
//!    genuinely one target's (`@syscall`, division and overflow sequences).
//!
//! A driver written once cannot drift between the two backends. A driver
//! written twice can, silently: the multi-eightbyte return order did exactly
//! that, and the reason for it now travels with the plan rather than with one
//! backend's comment (see [`crate::call_plan::return_register_write_order`]).

use std::fmt;

use lasso::{Key, ThreadedRodeo};
use rue_air::{FrozenTypeInternPool, StructId, TypeKind};
use rue_cfg::{BlockId, Cfg, CfgValue, Type};

use crate::types;
use crate::vreg::{LabelId, VReg};

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
        CfgInstData::ArrayInit { shape, .. } => match shape {
            // Note: Can't show elements without Cfg access
            rue_air::ArrayInitShape::Elementwise => "array_init [...]".to_string(),
            rue_air::ArrayInitShape::Repeat => "array_repeat [...]".to_string(),
        },
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
                self.is_strbuf(ty)
                    || matches!(
                        self.type_pool.text_view_kind(struct_id),
                        Some(rue_air::TextViewKind::Str | rue_air::TextViewKind::StrFixed(_))
                    )
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

/// The canonical-form extension a foreign scalar return needs (ADR-0064 P2).
///
/// The narrow value occupies the low bits of the target row's result register
/// with unspecified high bits, so unlike a Rue-internal 32-bit unsigned value —
/// which every 32-bit machine operation already leaves canonical — a foreign
/// `unsigned int` return has to be zero-extended explicitly. Each backend
/// spells the named extension through its one extension primitive.
pub(crate) fn c_return_extension(
    ext: rue_air::ScalarAbiExtension,
) -> crate::value_plan::IntegerExtension {
    use crate::value_plan::IntegerExtension;
    use rue_air::ScalarAbiExtension;
    match ext {
        ScalarAbiExtension::None => IntegerExtension::None,
        ScalarAbiExtension::Signed { from_bits: 8 } => IntegerExtension::Sign8,
        ScalarAbiExtension::Signed { from_bits: 16 } => IntegerExtension::Sign16,
        ScalarAbiExtension::Signed { from_bits: 32 } => IntegerExtension::Sign32,
        ScalarAbiExtension::Unsigned { from_bits: 8 } => IntegerExtension::Zero8,
        ScalarAbiExtension::Unsigned { from_bits: 16 } => IntegerExtension::Zero16,
        ScalarAbiExtension::Unsigned { from_bits: 32 } => IntegerExtension::Zero32,
        ScalarAbiExtension::Signed { from_bits } | ScalarAbiExtension::Unsigned { from_bits } => {
            panic!("unexpected target-C scalar extension width {from_bits}")
        }
    }
}

// ============================================================================
// Target-independent lowering drivers
// ============================================================================

/// The lowerer state and per-target instruction leaves the drivers below reach.
///
/// The drivers are the parts of CFG lowering that decide *what* happens rather
/// than how it is spelled — cache a value's vreg, copy each register-only
/// parameter out of its argument register, emit one move per edge slot, run a
/// drop plan's cleanup calls. They were hand-mirrored between the two backends
/// for as long as there were two of them; they live here once, and each
/// backend's method delegates. Only operations whose spelling *is* an
/// instruction, and the lowerer's own caches, are trait items.
///
/// The lifetime parameter carries the lowering context's own borrow of the CFG
/// and type pool, which outlives any borrow of the lowerer, so a driver can
/// hold the context while it emits.
pub(crate) trait LoweringDriverBackend<'a>: crate::place_lower::PlaceLowerBackend {
    /// The shared lowering context, by value.
    fn lowering_context(&self) -> CfgLowerContext<'a>;

    /// The primary vreg cache: one entry per lowered CFG value.
    fn value_map(&mut self) -> &mut ahash::AHashMap<CfgValue, VReg>;

    /// The vreg carrying each block parameter, by `(block, parameter index)`.
    fn block_param_vregs(&mut self) -> &mut ahash::AHashMap<(BlockId, u32), VReg>;

    /// The received by-reference parameter pointers, by parameter ABI slot.
    fn by_ref_param_ptrs(&mut self) -> &mut ahash::AHashMap<u32, VReg>;

    /// The vregs holding register-only parameters (RUE-1170), by ABI slot.
    fn param_reg_vregs(&mut self) -> &mut ahash::AHashMap<u32, VReg>;

    /// Emit a floating-point register-to-register move at `width`.
    fn emit_float_reg_move(&mut self, dst: VReg, src: VReg, width: crate::value_plan::FloatWidth);

    /// Copy incoming general-purpose argument register `index` into `dst`.
    fn emit_gp_arg_register_copy(&mut self, dst: VReg, index: usize);

    /// Copy incoming floating-point argument register `index` into `dst`.
    fn emit_fp_arg_register_copy(
        &mut self,
        dst: VReg,
        index: usize,
        width: crate::value_plan::FloatWidth,
    );

    /// Lower one planned call and materialize its result.
    fn lower_call_plan(
        &mut self,
        plan: crate::call_plan::CallPlan,
    ) -> crate::value_plan::MaterializedValue;

    /// Jump to `target`'s block label.
    fn emit_jump_to_block(&mut self, target: BlockId);

    /// Branch to `label` when `condition` is nonzero, comparing against zero
    /// first on a target whose branches read a flags register.
    fn emit_branch_if_nonzero(&mut self, condition: VReg, label: LabelId);

    /// Branch to `label` when `condition` is zero, comparing against zero
    /// first on a target whose branches read a flags register.
    fn emit_branch_if_zero(&mut self, condition: VReg, label: LabelId);

    /// Compare `scrutinee` against one switch case value at `width`.
    fn emit_switch_case_compare(
        &mut self,
        scrutinee: VReg,
        value: i64,
        width: crate::value_plan::IntegerWidth,
    );

    /// Branch to `target`'s block label when the last comparison was equal.
    fn emit_branch_if_equal(&mut self, target: BlockId);

    /// Move a scalar return value into the result register of its own bank.
    fn emit_scalar_return_move(
        &mut self,
        value: VReg,
        float_width: Option<crate::value_plan::FloatWidth>,
    );

    /// Return from the function.
    fn emit_return(&mut self);

    /// Trap: control reached a terminator the CFG proved unreachable.
    fn emit_unreachable(&mut self);

    /// Write one register-returned value into the result registers the shared
    /// lowering named for it, in
    /// [`ReturnRegisters::write_order`](crate::call_plan::ReturnRegisters::write_order).
    fn write_return_registers(
        &mut self,
        registers: &crate::call_plan::ReturnRegisters,
        slots: &[VReg],
    );

    /// Lower one planned runtime-helper call.
    fn lower_runtime_call(
        &mut self,
        plan: crate::runtime_call_plan::RuntimeCallPlan,
    ) -> crate::value_plan::MaterializedValue;
}

/// The vreg carrying `value`, lowering the value first if it has not been
/// reached yet.
pub(crate) fn get_vreg<'a, B: LoweringDriverBackend<'a>>(b: &mut B, value: CfgValue) -> VReg {
    if let Some(&vreg) = b.value_map().get(&value) {
        return vreg;
    }

    // Not yet lowered - lower it now
    let ctx = b.lowering_context();
    crate::value_plan::lower_value(&ctx, b, value);

    b.value_map()
        .get(&value)
        .copied()
        .expect("value should have been lowered")
}

/// The vregs a CFG edge's argument arrives in, which
/// [`prepare_block_param`] reserved before any block was lowered.
pub(crate) fn materialize_block_param<'a, B: LoweringDriverBackend<'a>>(
    b: &mut B,
    target: BlockId,
    param_index: u32,
    value: CfgValue,
    plan: crate::value_plan::ValuePlan,
) -> crate::value_plan::MaterializedValue {
    let primary = b.block_param_vregs()[&(target, param_index)];
    let slots = if plan.shape.requires_complete_slots() {
        let slots = b
            .slot_cache()
            .get(&value)
            .cloned()
            .expect("aggregate block parameter slots should be preallocated");
        plan.assert_complete_slots(slots.len());
        slots
    } else {
        Vec::new()
    };
    crate::value_plan::MaterializedValue { primary, slots }
}

/// Reserve the vregs one block parameter arrives in, in the register class its
/// leaf names, before any block is lowered.
pub(crate) fn prepare_block_param<'a, B: LoweringDriverBackend<'a>>(
    b: &mut B,
    block: BlockId,
    index: u32,
    value: CfgValue,
    ty: Type,
) {
    let primary_ty = crate::types::aggregate_leaf_types(b.ctx().type_pool, ty)
        .first()
        .copied()
        .unwrap_or(ty);
    let vreg = if crate::value_plan::float_width(primary_ty).is_some() {
        b.alloc_float_vreg()
    } else {
        b.alloc_vreg()
    };
    b.block_param_vregs().insert((block, index), vreg);
    b.value_map().insert(value, vreg);
    crate::agg_slots::preallocate_block_param_slots(b, value, ty, vreg);
}

/// The whole entry preamble: register-only parameter copies, by-reference
/// parameter pointers, and the by-value aggregate parameters read back out of
/// their compact images.
pub(crate) fn preload_by_ref_params<'a, B: LoweringDriverBackend<'a>>(b: &mut B) {
    preload_by_ref_param_ptrs(b);
    // Read each by-value aggregate parameter whose leaves are not its
    // eightbytes back out of the compact image the prologue laid down, so
    // field projection and whole-value reads see the correct decomposition
    // (ADR-0084).
    let unmarshals = crate::value_plan::param_image_unmarshals(b.ctx());
    for (base_slot, image_slot_offset, through_pointer, image) in unmarshals {
        crate::agg_slots::unmarshal_param_image(
            b,
            base_slot,
            image_slot_offset,
            through_pointer,
            &image,
        );
    }
}

/// Materialize every by-reference parameter pointer before CFG control flow
/// begins, so the function-wide cache only contains definitions that dominate
/// every block which may reuse them.
pub(crate) fn preload_by_ref_param_ptrs<'a, B: LoweringDriverBackend<'a>>(b: &mut B) {
    materialize_register_params(b);
    let by_ref = crate::value_plan::by_ref_param_slots(b.ctx());
    for param_slot in by_ref {
        ensure_by_ref_param_ptr(b, param_slot);
    }
}

/// Copy every register-only parameter (RUE-1170) out of its incoming argument
/// register into a virtual register, before CFG control flow begins: the
/// argument registers are caller-saved, so the copies must precede every call
/// and dominate every use (including loop back-edges into the entry block). A
/// register-only by-ref pointer seeds the by-ref cache directly.
pub(crate) fn materialize_register_params<'a, B: LoweringDriverBackend<'a>>(b: &mut B) {
    let entry_copies = b.ctx().param_entry_copies();
    for (param_slot, class, location) in entry_copies {
        let vreg = match (class, location) {
            (
                crate::abi_slot_class::AbiSlotClass::Gp,
                crate::call_plan::AbiSlotLocation::GpReg(class_index),
            ) => {
                let vreg = b.alloc_vreg();
                b.emit_gp_arg_register_copy(vreg, class_index);
                vreg
            }
            (
                crate::abi_slot_class::AbiSlotClass::Fp(width),
                crate::call_plan::AbiSlotLocation::FpReg(class_index),
            ) => {
                let vreg = b.alloc_float_vreg();
                b.emit_fp_arg_register_copy(vreg, class_index, width);
                vreg
            }
            _ => unreachable!("register-only parameter class and ABI bank must agree"),
        };
        if b.ctx().cfg.is_param_by_ref(param_slot) {
            b.by_ref_param_ptrs().insert(param_slot, vreg);
        } else {
            b.param_reg_vregs().insert(param_slot, vreg);
        }
    }
}

/// The pointer a by-reference parameter was received through, loaded from its
/// frame home on first use and cached for the rest of the function.
pub(crate) fn ensure_by_ref_param_ptr<'a, B: LoweringDriverBackend<'a>>(
    b: &mut B,
    param_slot: u32,
) -> VReg {
    if let Some(ptr_vreg) = b.by_ref_param_ptrs().get(&param_slot).copied() {
        return ptr_vreg;
    }

    // Load the pointer from the param's frame home. A register-only
    // by-ref pointer (RUE-1170) never reaches this load: the entry
    // preamble copies it out of its argument register into the cache
    // before any block is lowered, so the memoized hit above serves it.
    // Stack-passed pointers stay homed by the prologue, so this load is
    // uniform regardless of param count.
    let ptr_vreg = b.alloc_vreg();
    let slot = b.ctx().param_frame_slot(param_slot);
    b.emit_load_slot(ptr_vreg, slot);

    // Cache it for future use
    b.by_ref_param_ptrs().insert(param_slot, ptr_vreg);
    ptr_vreg
}

/// Emit one move per logical slot an edge carries into its target's block
/// parameters.
pub(crate) fn emit_edge_moves<'a, B: LoweringDriverBackend<'a>>(
    b: &mut B,
    edge: &crate::terminator_plan::EdgePlan,
) {
    for movement in &edge.moves {
        match movement.float_width {
            Some(width) => b.emit_float_reg_move(movement.destination, movement.source, width),
            None => b.emit_reg_move(movement.destination, movement.source),
        }
    }
}

/// The vregs one `Param` value's slots arrive in.
///
/// A parameter reaches its reader in one of four shapes: through the pointer a
/// by-reference parameter was received as, out of the frame image a homed
/// parameter occupies, straight from the vreg the entry preamble copied a
/// register-only parameter into (RUE-1170), or — for a zero-slot value — not
/// at all.
pub(crate) fn lower_param_value<'a, B: LoweringDriverBackend<'a>>(
    b: &mut B,
    index: u32,
    ty: Type,
    policy: crate::value_plan::ValuePlan,
) -> (VReg, Vec<VReg>) {
    // A slot's register class follows its LEAF, not the type wrapped around
    // it (RUE-2001): a `struct Q { f: f64 }` and a bare `f64` hold the same
    // thing in the same one slot, so reading the width off the parameter's
    // own type would load a float-carrying wrapper with an integer load into
    // a general-purpose register and hand it to a float-typed consumer.
    let leaf_types = crate::types::aggregate_leaf_types(b.ctx().type_pool, ty);
    let float_width = crate::value_plan::primary_slot_float_width(&leaf_types);
    let dst = if float_width.is_some() {
        b.alloc_float_vreg()
    } else {
        b.alloc_vreg()
    };
    let count = policy.shape.slot_count();
    if count == 0 {
        // No slot to load: `dst` stays the never-read placeholder.
        return (dst, Vec::new());
    }
    if let crate::value_plan::StoragePolicy::ParameterSlot { by_ref: true, .. } = policy.storage {
        let ptr = ensure_by_ref_param_ptr(b, index);
        if count > 1 {
            let slots: Vec<_> = (0..count)
                .map(|slot| {
                    let v = b.alloc_vreg();
                    b.emit_load_through_ptr(
                        v,
                        ptr,
                        crate::frame_layout::slot_byte_offset(slot as usize),
                    );
                    v
                })
                .collect();
            return (slots[0], slots);
        }
        b.emit_load_ptr_base(dst, ptr, float_width);
    } else if count > 1 {
        let slots: Vec<_> = (0..count)
            .map(|slot| {
                let width = crate::value_plan::float_width(leaf_types[slot as usize]);
                let v = if width.is_some() {
                    b.alloc_float_vreg()
                } else {
                    b.alloc_vreg()
                };
                let frame_slot = b.ctx().param_value_low_slot(index, count) - slot;
                match width {
                    Some(width) => b.emit_float_load_slot(v, frame_slot, width),
                    None => b.emit_load_slot(v, frame_slot),
                }
                v
            })
            .collect();
        return (slots[0], slots);
    } else if let Some(&vreg) = b.param_reg_vregs().get(&index) {
        // Register-only scalar (RUE-1170): the entry preamble copied the
        // argument register into one read-only vreg shared by every read.
        return (vreg, Vec::new());
    } else {
        let frame_slot = b.ctx().param_value_low_slot(index, 1);
        match float_width {
            Some(width) => b.emit_float_load_slot(dst, frame_slot, width),
            None => b.emit_load_slot(dst, frame_slot),
        }
    }
    (dst, Vec::new())
}

/// Emit one block terminator from its target-neutral plan.
///
/// Every branch topology decision — which edge falls through, which one gets
/// the setup label, which comparison the switch spends — is made here, once;
/// the backends supply only the instruction spelling for each step.
pub(crate) fn emit_terminator_plan<'a, B: LoweringDriverBackend<'a>>(
    b: &mut B,
    plan: crate::terminator_plan::TerminatorPlan,
) {
    use crate::terminator_plan::{ReturnMode, ReturnValuePlan, TerminatorPlan};

    match plan {
        TerminatorPlan::Goto { edge } => {
            emit_edge_moves(b, &edge);
            if !edge.fallthrough {
                b.emit_jump_to_block(edge.target);
            }
        }
        TerminatorPlan::Branch {
            condition,
            then_edge,
            else_edge,
        } => {
            // The edge that falls through is emitted last, so the branch is
            // spelled against the other one and the fall-through edge's moves
            // land immediately before its target block.
            if then_edge.fallthrough {
                let then_setup_label = b.alloc_lowering_label();
                b.emit_branch_if_nonzero(condition, then_setup_label);
                emit_edge_moves(b, &else_edge);
                if !else_edge.fallthrough {
                    b.emit_jump_to_block(else_edge.target);
                }
                b.emit_lowering_label(then_setup_label);
                emit_edge_moves(b, &then_edge);
            } else {
                let else_setup_label = b.alloc_lowering_label();
                b.emit_branch_if_zero(condition, else_setup_label);
                emit_edge_moves(b, &then_edge);
                if !then_edge.fallthrough {
                    b.emit_jump_to_block(then_edge.target);
                }
                b.emit_lowering_label(else_setup_label);
                emit_edge_moves(b, &else_edge);
                if !else_edge.fallthrough {
                    b.emit_jump_to_block(else_edge.target);
                }
            }
        }
        TerminatorPlan::Switch {
            scrutinee,
            width,
            cases,
            default,
        } => {
            for case in cases {
                b.emit_switch_case_compare(scrutinee, case.value, width);
                b.emit_branch_if_equal(case.target);
            }
            b.emit_jump_to_block(default);
        }
        TerminatorPlan::Return { mode } => match mode {
            ReturnMode::Exit { call } => {
                let _ = b.lower_runtime_call(call);
            }
            ReturnMode::Function { value } => match value {
                ReturnValuePlan::ZeroSized => b.emit_return(),
                ReturnValuePlan::Scalar { value, float_width } => {
                    b.emit_scalar_return_move(value, float_width);
                    b.emit_return();
                }
                ReturnValuePlan::Aggregate {
                    slots,
                    return_plan,
                    registers,
                } => {
                    if let crate::call_plan::ReturnPlan::Sret { echoed, .. } = return_plan {
                        let return_ty = b.ctx().cfg.return_type();
                        let slot_map =
                            crate::types::aggregate_physical_slot_map(b.ctx().type_pool, return_ty);
                        match slot_map {
                            Some(map) => {
                                // The sret image is written compact; its padding
                                // is zeroed first (ADR-0052 ruling 5).
                                let padding =
                                    b.ctx().type_pool.compact_image_padding_ranges(return_ty);
                                crate::agg_slots::store_slots_to_sret_compact(
                                    b, &slots, &map, &padding,
                                )
                            }
                            None => {
                                let dispatch = crate::types::aggregate_dispatch_image(
                                    b.ctx().type_pool,
                                    return_ty,
                                );
                                match dispatch {
                                    // Heterogeneous compact aggregate return (RUE-1037):
                                    // write the sret image with a per-variant tag dispatch.
                                    Some(image) => crate::agg_slots::store_dispatch_image_to_sret(
                                        b, &slots, &image,
                                    ),
                                    None => crate::agg_slots::store_slots_to_sret(b, &slots),
                                }
                            }
                        }
                        // SysV AMD64 requires the callee to leave the
                        // indirect-result pointer in `rax` on return; AAPCS64's
                        // dedicated `x8` is not echoed. The row's own field is
                        // read rather than assumed, so the two stay one rule.
                        if echoed {
                            crate::agg_slots::SlotBackend::emit_sret_pointer_echo(b);
                        }
                    } else {
                        match registers.as_ref() {
                            Some(registers) => b.write_return_registers(registers, &slots),
                            // A zero-sized aggregate names no result
                            // register because it has no bytes to carry.
                            None => assert!(
                                slots.is_empty(),
                                "only a zero-sized aggregate return names no \
                                 result register"
                            ),
                        }
                    }
                    b.emit_return();
                }
            },
        },
        TerminatorPlan::Unreachable => b.emit_unreachable(),
    }
}

/// Emit a drop plan's cleanup calls.
pub(crate) fn lower_drop_plan<'a, B: LoweringDriverBackend<'a>>(
    b: &mut B,
    actions: Vec<crate::value_plan::DropAction>,
) -> crate::value_plan::ValueResult {
    for action in actions {
        // One cleanup call at a time: building the plan emits the
        // argument's marshaling, and a caller-owned indirect copy must stay
        // live until the call it belongs to has returned.
        let plan = crate::call_plan::CallPlan::from_inputs(
            crate::call_plan::CallTarget::rue(action.symbol),
            crate::call_plan::ReturnPlan::ZeroSized,
            std::slice::from_ref(&action.argument),
            std::slice::from_ref(&action.native),
            b,
        );
        let _ = b.lower_call_plan(plan);
    }

    crate::value_plan::ValueResult::SideEffect
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value_plan::IntegerExtension;
    use rue_air::ScalarAbiExtension;

    #[test]
    fn a_foreign_narrow_return_names_the_extension_its_signedness_asks_for() {
        assert_eq!(
            c_return_extension(ScalarAbiExtension::None),
            IntegerExtension::None
        );
        for (bits, signed, expected) in [
            (8, true, IntegerExtension::Sign8),
            (16, true, IntegerExtension::Sign16),
            (32, true, IntegerExtension::Sign32),
            (8, false, IntegerExtension::Zero8),
            (16, false, IntegerExtension::Zero16),
            // A foreign `unsigned int` leaves bits 32-63 unspecified, unlike
            // every Rue-internal 32-bit result, so this one is explicit.
            (32, false, IntegerExtension::Zero32),
        ] {
            let ext = if signed {
                ScalarAbiExtension::Signed { from_bits: bits }
            } else {
                ScalarAbiExtension::Unsigned { from_bits: bits }
            };
            assert_eq!(c_return_extension(ext), expected, "{ext:?}");
        }
    }

    #[test]
    #[should_panic(expected = "unexpected target-C scalar extension width")]
    fn a_foreign_return_of_an_unclassified_width_is_rejected() {
        c_return_extension(ScalarAbiExtension::Unsigned { from_bits: 7 });
    }
}
