//! Shared types for CFG lowering across backends.
//!
//! This module contains types used by both x86_64 and aarch64 backends
//! when tracing through field and index chains during CFG lowering.

use std::fmt;

use rue_air::ArrayTypeId;
use rue_cfg::{BlockId, CfgValue};

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
        CfgInstData::Call { name, args } => {
            let args_str = args
                .iter()
                .map(|a| format!("{}", a.value))
                .collect::<Vec<_>>()
                .join(", ");
            format!("call {}({})", name, args_str)
        }
        CfgInstData::Intrinsic { name, args } => {
            let args_str = args
                .iter()
                .map(|a| format!("{}", a))
                .collect::<Vec<_>>()
                .join(", ");
            format!("intrinsic @{}({})", name, args_str)
        }
        CfgInstData::StructInit { struct_id, fields } => {
            let fields_str = fields
                .iter()
                .map(|f| format!("{}", f))
                .collect::<Vec<_>>()
                .join(", ");
            format!("struct_init #{} {{{}}}", struct_id.0, fields_str)
        }
        CfgInstData::FieldGet {
            base,
            struct_id,
            field_index,
        } => format!("field_get {}.#{}.{}", base, struct_id.0, field_index),
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
        CfgInstData::ArrayInit {
            array_type_id,
            elements,
        } => {
            let elems_str = elements
                .iter()
                .map(|e| format!("{}", e))
                .collect::<Vec<_>>()
                .join(", ");
            format!("array_init @{} [{}]", array_type_id.0, elems_str)
        }
        CfgInstData::IndexGet {
            base,
            array_type_id,
            index,
        } => format!("index_get {}[@{}][{}]", base, array_type_id.0, index),
        CfgInstData::IndexSet {
            slot,
            array_type_id,
            index,
            value,
        } => format!(
            "index_set ${}[@{}][{}] = {}",
            slot, array_type_id.0, index, value
        ),
        CfgInstData::ParamIndexSet {
            param_slot,
            array_type_id,
            index,
            value,
        } => format!(
            "param_index_set %{}[@{}][{}] = {}",
            param_slot, array_type_id.0, index, value
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
    }
}

/// Format a CFG terminator as a human-readable string.
pub fn format_terminator(terminator: &rue_cfg::Terminator) -> String {
    use rue_cfg::Terminator;

    match terminator {
        Terminator::Goto { target, args } => {
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
            then_args,
            else_block,
            else_args,
        } => {
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
            cases,
            default,
        } => {
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

/// Result of tracing back through FieldGet chains to find the original source.
pub enum FieldChainBase {
    /// Chain originates from a Load instruction with the given slot.
    Load { slot: u32 },
    /// Chain originates from a Param instruction with the given index.
    Param { index: u32 },
}

/// Result of tracing back through IndexGet chains to find the original source.
#[derive(Clone)]
pub enum IndexChainBase {
    /// Chain originates from a Load instruction with the given slot.
    Load { slot: u32 },
    /// Chain originates from a Param instruction with the given index.
    Param { index: u32 },
    /// Chain originates from a FieldGet (array within a struct).
    FieldGet {
        struct_base_slot: u32,
        field_slot_offset: u32,
    },
}

/// Represents an index operation in a chain: the index value and the stride (slots per element).
#[derive(Clone)]
pub struct IndexLevel {
    pub index: CfgValue,
    pub elem_slot_count: u32,
    pub array_type_id: ArrayTypeId,
}
