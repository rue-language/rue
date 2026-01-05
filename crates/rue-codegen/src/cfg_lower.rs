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

use lasso::Key;
use rue_air::{StructId, TypeInternPool, TypeKind};
use rue_builtins::{BinOp, get_builtin_type};
use rue_cfg::{BlockId, Cfg, CfgInstData, CfgValue, Type};

use crate::types;

/// Represents an index operation: the index value and the stride (slots per element).
/// Used for lowering Place projections that include array indexing.
#[derive(Clone)]
pub struct IndexLevel {
    pub index: CfgValue,
    pub elem_slot_count: u32,
    /// The array type (Type::Array(...)) for bounds checking.
    pub array_type: Type,
}

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

/// Format a CFG instruction data as a human-readable string.
pub fn format_cfg_inst_data(data: &rue_cfg::CfgInstData) -> String {
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
        CfgInstData::Call { name, .. } => {
            // Note: Can't show args without Cfg access; just show name
            // Display symbol as @{id} since we don't have interner access
            format!("call @{}(...)", name.into_usize())
        }
        CfgInstData::Intrinsic { name, .. } => {
            // Note: Can't show args without Cfg access; just show name
            // Display symbol as @{id} since we don't have interner access
            format!("intrinsic @{}(...)", name.into_usize())
        }
        CfgInstData::StructInit { struct_id, .. } => {
            // Note: Can't show fields without Cfg access; just show struct_id
            format!("struct_init #{} {{...}}", struct_id.0)
        }
        CfgInstData::FieldSet {
            slot,
            struct_id,
            field_index,
            value,
        } => format!(
            "field_set ${}.#{}.{} = {}",
            slot, struct_id.0, field_index, value
        ),
        CfgInstData::ParamFieldSet {
            param_slot,
            inner_offset,
            struct_id,
            field_index,
            value,
        } => format!(
            "param_field_set %{}+{}.#{}.{} = {}",
            param_slot, inner_offset, struct_id.0, field_index, value
        ),
        CfgInstData::ArrayInit { .. } => {
            // Note: Can't show elements without Cfg access
            "array_init [...]".to_string()
        }
        CfgInstData::IndexSet {
            slot,
            array_type,
            index,
            value,
        } => format!(
            "index_set ${}[{}][{}] = {}",
            slot,
            array_type.name(),
            index,
            value
        ),
        CfgInstData::ParamIndexSet {
            param_slot,
            array_type,
            index,
            value,
        } => format!(
            "param_index_set %{}[{}][{}] = {}",
            param_slot,
            array_type.name(),
            index,
            value
        ),
        CfgInstData::EnumVariant {
            enum_id,
            variant_index,
        } => {
            format!("enum_variant #{}.{}", enum_id.0, variant_index)
        }
        CfgInstData::IntCast { value, from_ty } => {
            format!("int_cast {} : {}", value, from_ty.name())
        }
        CfgInstData::Drop { value } => format!("drop {}", value),
        CfgInstData::StorageLive { slot } => format!("storage_live ${}", slot),
        CfgInstData::StorageDead { slot } => format!("storage_dead ${}", slot),
        // Place operations (ADR-0030)
        CfgInstData::PlaceRead { place } => {
            format!("place_read {}", place)
        }
        CfgInstData::PlaceWrite { place, value } => {
            format!("place_write {} = {}", place, value)
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

    /// Check if a type is the builtin String struct.
    ///
    /// Returns true if the type is a struct that is marked as builtin with name "String".
    pub fn is_builtin_string(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def.is_builtin && struct_def.name == "String"
            }
            _ => false,
        }
    }

    /// Get the builtin operator runtime function for a type and operation.
    ///
    /// Returns `Some((runtime_fn, invert_result))` if the type has a builtin
    /// operator implementation, `None` otherwise.
    pub fn get_builtin_operator(&self, ty: Type, op: BinOp) -> Option<(&'static str, bool)> {
        let builtin = match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                if struct_def.is_builtin {
                    get_builtin_type(&struct_def.name)?
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        builtin
            .find_operator(op)
            .map(|op_def| (op_def.runtime_fn, op_def.invert_result))
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

    /// Check if a slot corresponds to an inout parameter.
    ///
    /// Returns `Some(param_index)` if it's an inout param slot, `None` otherwise.
    /// Inout parameter slots are stored after local variable slots.
    pub fn slot_to_inout_param_index(&self, slot: u32) -> Option<u32> {
        if slot >= self.num_locals && slot < self.num_locals + self.num_params {
            Some(slot - self.num_locals)
        } else {
            None
        }
    }
}
