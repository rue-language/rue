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
use rue_air::{StructId, TypeInternPool, TypeKind};
use rue_cfg::{BlockId, Cfg, CfgValue, Type};

use crate::types;

/// A single lowering decision: maps one CFG instruction to its MIR expansion.
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct TerminatorLoweringDecision {
    /// Human-readable description of the terminator.
    pub terminator_desc: String,
    /// Generated MIR instructions (as human-readable strings).
    pub mir_insts: Vec<String>,
    /// Rationale for the lowering decision.
    pub rationale: Option<String>,
}

/// Debug information for a single basic block's lowering.
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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

/// Format a CFG instruction data as a human-readable string. Takes the `Cfg`
/// so places render with their projections resolved (`$0.#2.1`), not as raw
/// projection-arena index ranges (`$0[3..5]`).
pub fn format_cfg_inst_data(cfg: &rue_cfg::Cfg, data: &rue_cfg::CfgInstData) -> String {
    format_cfg_inst_data_impl(cfg, data, None)
}

/// Format CFG instruction data with interned symbols resolved to stable names.
pub fn format_cfg_inst_data_with_interner(
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
        CfgInstData::Call {
            name,
            args_start,
            args_len,
        } => {
            let args: Vec<String> = cfg
                .get_call_args(*args_start, *args_len)
                .iter()
                .map(|a| format!("{}", a.value))
                .collect();
            let name = interner
                .map(|interner| interner.resolve(name).to_string())
                .unwrap_or_else(|| name.into_usize().to_string());
            format!("call @{}({})", name, args.join(", "))
        }
        CfgInstData::Intrinsic {
            name,
            args_start,
            args_len,
        } => {
            let args: Vec<String> = cfg
                .get_extra(*args_start, *args_len)
                .iter()
                .map(|v| format!("{}", v))
                .collect();
            let name = interner
                .map(|interner| interner.resolve(name).to_string())
                .unwrap_or_else(|| name.into_usize().to_string());
            format!("intrinsic @{}({})", name, args.join(", "))
        }
        CfgInstData::StructInit {
            struct_id,
            fields_start,
            fields_len,
        } => {
            let fields: Vec<String> = cfg
                .get_extra(*fields_start, *fields_len)
                .iter()
                .map(|v| format!("{}", v))
                .collect();
            format!("struct_init #{} {{{}}}", struct_id.0, fields.join(", "))
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
            format!("enum_variant #{}.{}", enum_id.0, variant_index)
        }
        CfgInstData::EnumPayloadGet {
            base,
            enum_id,
            variant_index,
            field_index,
        } => {
            format!(
                "enum_payload_get {} #{}.{}.{}",
                base, enum_id.0, variant_index, field_index
            )
        }
        CfgInstData::IntCast { value, from_ty } => {
            format!("int_cast {} : {}", value, from_ty.name())
        }
        CfgInstData::Drop { value } => format!("drop {}", value),
        CfgInstData::StorageLive { slot, .. } => format!("storage_live ${}", slot),
        CfgInstData::StorageDead { slot, .. } => format!("storage_dead ${}", slot),
        // Place operations
        CfgInstData::PlaceRead { place } => {
            format!("place_read {}", cfg.place_to_string(place))
        }
        CfgInstData::PlaceWrite { place, value } => {
            format!("place_write {} = {}", cfg.place_to_string(place), value)
        }
    }
}

/// Format a CFG terminator as a human-readable string.
pub fn format_terminator(cfg: &rue_cfg::Cfg, terminator: &rue_cfg::Terminator) -> String {
    use rue_cfg::Terminator;

    match terminator {
        Terminator::Goto {
            target,
            args_start,
            args_len,
        } => {
            let args = cfg.get_extra(*args_start, *args_len);
            if args.is_empty() {
                format!("goto {}", target)
            } else {
                let args_str = args
                    .iter()
                    .map(|a| format!("{}", a))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("goto {}({})", target, args_str)
            }
        }
        Terminator::Branch {
            cond,
            then_block,
            then_args_start,
            then_args_len,
            else_block,
            else_args_start,
            else_args_len,
        } => {
            let then_args = cfg.get_extra(*then_args_start, *then_args_len);
            let then_str = if then_args.is_empty() {
                format!("{}", then_block)
            } else {
                let args_str = then_args
                    .iter()
                    .map(|a| format!("{}", a))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}({})", then_block, args_str)
            };
            let else_args = cfg.get_extra(*else_args_start, *else_args_len);
            let else_str = if else_args.is_empty() {
                format!("{}", else_block)
            } else {
                let args_str = else_args
                    .iter()
                    .map(|a| format!("{}", a))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}({})", else_block, args_str)
            };
            format!("branch {}, {}, {}", cond, then_str, else_str)
        }
        Terminator::Switch {
            scrutinee,
            cases_start,
            cases_len,
            default,
        } => {
            let cases = cfg.get_switch_cases(*cases_start, *cases_len);
            let cases_str = cases
                .iter()
                .map(|(val, target)| format!("{} => {}", val, target))
                .collect::<Vec<_>>()
                .join(", ");
            format!("switch {} [{}], default: {}", scrutinee, cases_str, default)
        }
        Terminator::Return { value } => {
            if let Some(val) = value {
                format!("return {}", val)
            } else {
                "return".to_string()
            }
        }
        Terminator::Unreachable => "unreachable".to_string(),
        Terminator::None => "<no terminator>".to_string(),
    }
}

// ============================================================================
// Internal calling convention helpers
// ============================================================================

/// Does a by-value return of `ty` use the sret convention (caller-allocated
/// return buffer, pointer passed as a hidden first argument) instead of
/// return registers?
///
/// The internal Rue calling convention for aggregate returns (RUE-106):
///
/// - Aggregates whose flattened slot count fits in the backend's return
///   registers (`ret_reg_budget`: 6 on x86-64, 8 on aarch64) are returned
///   with one slot per return register.
/// - Builtin `String` *always* returns via sret, regardless of fitting in
///   registers: the runtime implementations (`__rue_String_clone`,
///   `__rue_read_line`, ...) are `extern "C"` functions taking an
///   `out: *mut StringResult` first parameter, so the convention is fixed
///   by the runtime ABI. (RUE-92)
/// - Any other aggregate with more slots than `ret_reg_budget` also returns
///   via sret: the caller allocates `slot_count * 8` bytes (16-aligned) on
///   its stack and passes the buffer address as a hidden first argument
///   (shifting all user arguments by one ABI slot); the callee stores every
///   slot through that pointer before returning. (RUE-13/78/91)
///
/// Scalars and unit never use sret.
pub fn type_uses_sret_return(type_pool: &TypeInternPool, ty: Type, ret_reg_budget: u32) -> bool {
    match ty.kind() {
        TypeKind::Struct(struct_id) => {
            if type_pool.is_strbuf(struct_id) {
                return true;
            }
            types::type_slot_count(type_pool, ty) > ret_reg_budget
        }
        TypeKind::Array(_) => types::type_slot_count(type_pool, ty) > ret_reg_budget,
        _ => false,
    }
}

/// Does this function return its value via the sret convention?
/// See [`type_uses_sret_return`] for the convention.
pub fn fn_uses_sret_return(cfg: &Cfg, type_pool: &TypeInternPool, ret_reg_budget: u32) -> bool {
    type_uses_sret_return(type_pool, cfg.return_type(), ret_reg_budget)
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
pub struct CfgLowerContext<'a> {
    /// The CFG being lowered.
    pub cfg: &'a Cfg,
    /// Type intern pool for struct/enum/array lookups.
    pub type_pool: &'a TypeInternPool,
    /// Number of local variable slots.
    pub num_locals: u32,
    /// Number of parameter slots.
    pub num_params: u32,
}

impl<'a> CfgLowerContext<'a> {
    /// Create a new CFG lowering context.
    pub fn new(cfg: &'a Cfg, type_pool: &'a TypeInternPool) -> Self {
        Self {
            cfg,
            type_pool,
            num_locals: cfg.num_locals(),
            num_params: cfg.num_params(),
        }
    }

    // ========================================================================
    // Type helpers
    // ========================================================================

    /// Get the length of an array type.
    pub fn array_length(&self, array_type: Type) -> u64 {
        types::array_length_from_type(self.type_pool, array_type)
    }

    /// Get the array type definition.
    ///
    /// Returns `Some((element_type, length))` for array types, `None` otherwise.
    pub fn array_type_def(&self, array_type: Type) -> Option<(Type, u64)> {
        types::array_type_def_from_type(self.type_pool, array_type)
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
        matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_))
            || (ty.is_enum() && self.type_slot_count(ty) > 1)
    }

    /// Calculate the slot count for a single element of an array type.
    pub fn array_element_slot_count(&self, array_type: Type) -> u32 {
        types::array_element_slot_count_from_type(self.type_pool, array_type)
    }

    /// Calculate the slot offset for a field within a struct.
    pub fn struct_field_slot_offset(&self, struct_id: StructId, field_index: u32) -> u32 {
        types::struct_field_slot_offset(self.type_pool, struct_id, field_index)
    }

    /// Total slot count of a struct (sum of its fields' slot counts). Used as
    /// the root-object origin shift for ascending place addressing
    /// (ADR-0040 / RUE-311).
    pub fn struct_total_slot_count(&self, struct_id: StructId) -> u32 {
        types::struct_slot_count(self.type_pool, struct_id)
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
    /// structural pointer/length equality.
    pub fn is_string_like_for_equality(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                self.is_strbuf(ty)
                    || struct_def.name == "str"
                    || (struct_def.name.starts_with("Str(") && struct_def.name.ends_with(')'))
            }
            _ => false,
        }
    }

    /// Does a by-value return of `ty` use the sret convention for the
    /// backend's return-register budget? See [`type_uses_sret_return`].
    pub fn type_uses_sret(&self, ty: Type, ret_reg_budget: u32) -> bool {
        type_uses_sret_return(self.type_pool, ty, ret_reg_budget)
    }

    /// Does the function being lowered return via the sret convention?
    pub fn uses_sret_return(&self, ret_reg_budget: u32) -> bool {
        self.type_uses_sret(self.cfg.return_type(), ret_reg_budget)
    }

    /// The frame slot holding the incoming sret pointer, one past the param
    /// area (only meaningful when [`Self::uses_sret_return`] is true). The
    /// prologue stores the hidden first argument here; the return path loads
    /// it back to write the result through. Register-allocator spill slots
    /// start after this slot.
    pub fn sret_ptr_slot(&self) -> u32 {
        self.num_locals + self.num_params
    }

    // ========================================================================
    // Slot helpers
    // ========================================================================

    /// Calculate the stack offset for a local variable slot.
    ///
    /// Local variables are stored at negative offsets from the frame pointer.
    pub fn local_offset(&self, slot: u32) -> i32 {
        -((slot as i32 + 1) * 8)
    }

    /// Check if a slot corresponds to a parameter ABI slot.
    ///
    /// Returns `Some(param_index)` if it is a parameter slot, `None` otherwise.
    /// Parameter ABI slots are stored after local variable slots.
    pub fn slot_to_param_index(&self, slot: u32) -> Option<u32> {
        if slot >= self.num_locals && slot < self.num_locals + self.num_params {
            Some(slot - self.num_locals)
        } else {
            None
        }
    }
}
