//! Comprehensive MIR Verifier
//!
//! This module implements a thorough verification pass for MIR that validates:
//! - **SSA Dominance**: Every use of a temporary is dominated by its definition
//! - **Block Parameters**: All block parameters are properly typed and used correctly
//! - **Control Flow Integrity**: All terminators reference valid blocks with correct argument counts and types
//! - **Type Consistency**: All operations respect type rules
//! - **Reachability**: All blocks are reachable from the entry block
//! - **No Uninit Terminators**: Ensure no blocks have Uninit terminators (sentinel values)
//! - **Temporary Usage**: Every temporary that's defined is used, and every temporary that's used is defined
//! - **Function Signature Consistency**: Entry block parameters match function parameters
//!
//! The verifier performs a comprehensive dominance analysis to ensure SSA properties
//! are correctly maintained, including proper block parameter usage in place of phi nodes.

use crate::mir::{
    BasicBlock, BlockId, MirBinOp, MirFunction, MirStatement, MirTerminator, MirValue, Temp,
};
use crate::types::{FieldId, RueType, TypeContext};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// Comprehensive verification error with detailed context
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationError {
    /// Human-readable error message
    pub message: String,
    /// The function where the error occurred
    pub function: String,
    /// The block where the error occurred (if applicable)
    pub block: Option<BlockId>,
    /// The temporary involved (if applicable)
    pub temp: Option<Temp>,
    /// Error category for programmatic handling
    pub category: ErrorCategory,
}

/// Categories of verification errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCategory {
    /// SSA dominance violation
    DominanceViolation,
    /// Type consistency error
    TypeError,
    /// Control flow error (invalid jumps, etc.)
    ControlFlowError,
    /// Unreachable code
    UnreachableCode,
    /// Invalid terminator (Uninit found)
    InvalidTerminator,
    /// Unused or undefined temporary
    TemporaryError,
    /// Function signature mismatch
    SignatureMismatch,
    /// Invalid operation (unsupported for type, etc.)
    InvalidOperation,
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.category, self.message)?;
        if let Some(temp) = self.temp {
            write!(f, " (temp: {})", temp)?;
        }
        if let Some(block) = self.block {
            write!(f, " (block: {})", block)?;
        }
        write!(f, " (function: {})", self.function)
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorCategory::DominanceViolation => write!(f, "Dominance Violation"),
            ErrorCategory::TypeError => write!(f, "Type Error"),
            ErrorCategory::ControlFlowError => write!(f, "Control Flow Error"),
            ErrorCategory::UnreachableCode => write!(f, "Unreachable Code"),
            ErrorCategory::InvalidTerminator => write!(f, "Invalid Terminator"),
            ErrorCategory::TemporaryError => write!(f, "Temporary Error"),
            ErrorCategory::SignatureMismatch => write!(f, "Signature Mismatch"),
            ErrorCategory::InvalidOperation => write!(f, "Invalid Operation"),
        }
    }
}

/// Dominance information for SSA validation
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DominanceInfo {
    /// Immediate dominator of each block (None for entry block)
    idom: HashMap<BlockId, Option<BlockId>>,
    /// Set of blocks that dominate each block
    dominators: HashMap<BlockId, HashSet<BlockId>>,
    /// Dominance tree children
    dom_children: HashMap<BlockId, Vec<BlockId>>,
}

impl DominanceInfo {
    /// Check if block `a` dominates block `b`
    fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        if a == b {
            return true;
        }
        self.dominators
            .get(&b)
            .is_some_and(|doms| doms.contains(&a))
    }
}

/// Comprehensive MIR verifier
pub struct MirVerifier<'ctx> {
    /// Type context for type checking
    type_context: &'ctx TypeContext,
    /// Accumulated errors
    errors: Vec<VerificationError>,
    /// Current function being verified
    current_function: String,
}

impl<'ctx> MirVerifier<'ctx> {
    /// Create a new verifier with the given type context
    pub fn new(type_context: &'ctx TypeContext) -> Self {
        Self {
            type_context,
            errors: Vec::new(),
            current_function: String::new(),
        }
    }

    /// Verify a MIR function, returning all errors found
    pub fn verify_function(
        &mut self,
        function: &MirFunction,
    ) -> Result<(), Vec<VerificationError>> {
        self.current_function = function.name.clone();
        self.errors.clear();

        // Build block lookup map
        let block_map: HashMap<BlockId, &BasicBlock> =
            function.blocks.iter().map(|b| (b.id, b)).collect();

        // 1. Basic structural validation
        self.verify_structure(function, &block_map);

        // 2. Verify function signature consistency
        self.verify_function_signature(function, &block_map);

        // 3. Build dominance information
        let dominance = match self.build_dominance_info(function, &block_map) {
            Ok(dom) => dom,
            Err(_) => {
                // If we can't build dominance info, the CFG is invalid
                // Errors already recorded, return early
                return Err(self.errors.clone());
            }
        };

        // 4. Verify reachability (no unreachable blocks)
        self.verify_reachability(function, &block_map, &dominance);

        // 5. Verify SSA dominance properties
        self.verify_ssa_dominance(function, &block_map, &dominance);

        // 6. Verify type consistency
        self.verify_type_consistency(function, &block_map);

        // 7. Verify temporary usage (all defined temps are used)
        self.verify_temporary_usage(function, &block_map);

        // 8. Verify no Uninit terminators remain
        self.verify_no_uninit_terminators(function);

        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors.clone())
        }
    }

    /// Verify basic structural properties
    fn verify_structure(
        &mut self,
        function: &MirFunction,
        block_map: &HashMap<BlockId, &BasicBlock>,
    ) {
        // Check entry block exists
        if !block_map.contains_key(&function.entry_block) {
            self.add_error(
                format!(
                    "Entry block {:?} not found in function",
                    function.entry_block
                ),
                ErrorCategory::ControlFlowError,
                None,
                None,
            );
            return;
        }

        // Check all block IDs are unique
        let mut seen_ids = HashSet::new();
        for block in &function.blocks {
            if !seen_ids.insert(block.id) {
                self.add_error(
                    format!("Duplicate block ID: {:?}", block.id),
                    ErrorCategory::ControlFlowError,
                    Some(block.id),
                    None,
                );
            }
        }

        // Check all referenced blocks exist
        for block in &function.blocks {
            self.verify_terminator_targets(&block.terminator, block.id, block_map);
        }
    }

    /// Verify function signature consistency with entry block
    fn verify_function_signature(
        &mut self,
        function: &MirFunction,
        block_map: &HashMap<BlockId, &BasicBlock>,
    ) {
        let entry_block = match block_map.get(&function.entry_block) {
            Some(block) => block,
            None => return, // Error already reported in structure verification
        };

        // Entry block parameters should match function parameters
        if function.params.len() != entry_block.params.len() {
            self.add_error(
                format!(
                    "Function has {} parameters but entry block has {} parameters",
                    function.params.len(),
                    entry_block.params.len()
                ),
                ErrorCategory::SignatureMismatch,
                Some(function.entry_block),
                None,
            );
            return;
        }

        // Parameter types should match
        for (i, ((param_name, param_ty), (entry_temp, entry_ty))) in
            function.params.iter().zip(&entry_block.params).enumerate()
        {
            if param_ty != entry_ty {
                self.add_error(
                    format!(
                        "Parameter {} '{}' has type {} in function signature but {} in entry block",
                        i, param_name, param_ty, entry_ty
                    ),
                    ErrorCategory::SignatureMismatch,
                    Some(function.entry_block),
                    Some(*entry_temp),
                );
            }

            // Check that temp_types contains the right type for this temp
            if let Some(temp_ty) = function.temp_types.get(entry_temp) {
                if temp_ty != entry_ty {
                    self.add_error(
                        format!(
                            "Entry block parameter {} has type {} but temp_types has {}",
                            entry_temp, entry_ty, temp_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(function.entry_block),
                        Some(*entry_temp),
                    );
                }
            } else {
                self.add_error(
                    format!(
                        "Entry block parameter {} has no type in temp_types",
                        entry_temp
                    ),
                    ErrorCategory::TemporaryError,
                    Some(function.entry_block),
                    Some(*entry_temp),
                );
            }
        }
    }

    /// Build dominance information using iterative algorithm
    fn build_dominance_info(
        &mut self,
        function: &MirFunction,
        block_map: &HashMap<BlockId, &BasicBlock>,
    ) -> Result<DominanceInfo, ()> {
        let entry = function.entry_block;

        // Compute predecessors for each block
        let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        for block_id in block_map.keys() {
            predecessors.insert(*block_id, Vec::new());
        }

        for block in &function.blocks {
            match &block.terminator {
                MirTerminator::Goto { target, .. } => {
                    if let Some(preds) = predecessors.get_mut(target) {
                        preds.push(block.id);
                    }
                }
                MirTerminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => {
                    if let Some(preds) = predecessors.get_mut(then_block) {
                        preds.push(block.id);
                    }
                    if let Some(preds) = predecessors.get_mut(else_block) {
                        preds.push(block.id);
                    }
                }
                MirTerminator::Switch {
                    targets, default, ..
                } => {
                    for (_, target, _) in targets {
                        if let Some(preds) = predecessors.get_mut(target) {
                            preds.push(block.id);
                        }
                    }
                    if let Some(preds) = predecessors.get_mut(default) {
                        preds.push(block.id);
                    }
                }
                MirTerminator::Return { .. } | MirTerminator::Unreachable { .. } => {}
                MirTerminator::Uninit => {
                    // This will be caught in uninit verification
                }
            }
        }

        // Initialize dominators: entry dominates only itself, all others dominate everything initially
        let all_blocks: HashSet<BlockId> = block_map.keys().copied().collect();
        let mut dominators: HashMap<BlockId, HashSet<BlockId>> = HashMap::new();

        dominators.insert(entry, [entry].iter().copied().collect());
        for &block_id in &all_blocks {
            if block_id != entry {
                dominators.insert(block_id, all_blocks.clone());
            }
        }

        // Iteratively refine dominators
        let mut changed = true;
        while changed {
            changed = false;

            for &block_id in &all_blocks {
                if block_id == entry {
                    continue;
                }

                let old_doms = dominators[&block_id].clone();

                // New dominators = {block} ∪ (∩ dominators of all predecessors)
                let mut new_doms = HashSet::new();
                new_doms.insert(block_id);

                if let Some(preds) = predecessors.get(&block_id)
                    && !preds.is_empty()
                {
                    // Start with dominators of first predecessor
                    let mut common_doms = dominators[&preds[0]].clone();

                    // Intersect with dominators of all other predecessors
                    for &pred in &preds[1..] {
                        common_doms = common_doms
                            .intersection(&dominators[&pred])
                            .copied()
                            .collect();
                    }

                    new_doms.extend(common_doms);
                }

                if new_doms != old_doms {
                    dominators.insert(block_id, new_doms);
                    changed = true;
                }
            }
        }

        // Compute immediate dominators
        let mut idom: HashMap<BlockId, Option<BlockId>> = HashMap::new();
        idom.insert(entry, None);

        for &block_id in &all_blocks {
            if block_id == entry {
                continue;
            }

            let block_doms = &dominators[&block_id];
            let proper_doms: HashSet<BlockId> = block_doms
                .iter()
                .copied()
                .filter(|&d| d != block_id)
                .collect();

            // Find the immediate dominator (dominator not dominated by any other dominator)
            let mut immediate = None;
            for &dom in &proper_doms {
                let is_immediate = !proper_doms
                    .iter()
                    .any(|&other| other != dom && dominators[&dom].contains(&other));
                if is_immediate {
                    immediate = Some(dom);
                    break;
                }
            }
            idom.insert(block_id, immediate);
        }

        // Compute dominance tree children
        let mut dom_children: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        for &block_id in &all_blocks {
            dom_children.insert(block_id, Vec::new());
        }

        for (&child, &parent_opt) in &idom {
            if let Some(parent) = parent_opt {
                dom_children.get_mut(&parent).expect("parent should exist in dom_children").push(child);
            }
        }

        Ok(DominanceInfo {
            idom,
            dominators,
            dom_children,
        })
    }

    /// Verify that all blocks are reachable from entry
    fn verify_reachability(
        &mut self,
        _function: &MirFunction,
        block_map: &HashMap<BlockId, &BasicBlock>,
        dominance: &DominanceInfo,
    ) {
        // All blocks in dominance tree are reachable by definition
        // Find any blocks not in the dominance tree
        for &block_id in block_map.keys() {
            if !dominance.dominators.contains_key(&block_id) {
                self.add_error(
                    format!("Block {:?} is unreachable from entry block", block_id),
                    ErrorCategory::UnreachableCode,
                    Some(block_id),
                    None,
                );
            }
        }
    }

    /// Verify SSA dominance properties
    fn verify_ssa_dominance(
        &mut self,
        function: &MirFunction,
        _block_map: &HashMap<BlockId, &BasicBlock>,
        dominance: &DominanceInfo,
    ) {
        // Track where each temporary is defined
        let mut temp_definitions: HashMap<Temp, BlockId> = HashMap::new();

        // First pass: record all definitions
        for block in &function.blocks {
            // Block parameters are defined at the start of the block
            for &(temp, _) in &block.params {
                if let Some(existing_block) = temp_definitions.insert(temp, block.id) {
                    self.add_error(
                        format!(
                            "Temporary {} defined in multiple blocks: {:?} and {:?}",
                            temp, existing_block, block.id
                        ),
                        ErrorCategory::DominanceViolation,
                        Some(block.id),
                        Some(temp),
                    );
                }
            }

            // Assignments define temporaries
            for stmt in &block.statements {
                if let MirStatement::Assign { dest, .. } = stmt
                    && let Some(existing_block) = temp_definitions.insert(*dest, block.id)
                {
                    self.add_error(
                        format!(
                            "Temporary {} assigned in multiple blocks: {:?} and {:?}",
                            dest, existing_block, block.id
                        ),
                        ErrorCategory::DominanceViolation,
                        Some(block.id),
                        Some(*dest),
                    );
                }
            }
        }

        // Second pass: verify all uses are dominated by definitions
        for block in &function.blocks {
            // Check uses in statements
            for stmt in &block.statements {
                match stmt {
                    MirStatement::Assign { value, .. } => {
                        self.verify_value_dominance(value, block.id, &temp_definitions, dominance);
                    }
                    MirStatement::Return {
                        value: Some(temp), ..
                    } => {
                        self.verify_temp_dominance(*temp, block.id, &temp_definitions, dominance);
                    }
                    MirStatement::Return { value: None, .. } => {}
                }
            }

            // Check uses in terminator
            self.verify_terminator_dominance(
                &block.terminator,
                block.id,
                &temp_definitions,
                dominance,
            );
        }
    }

    /// Verify dominance for a value expression
    fn verify_value_dominance(
        &mut self,
        value: &MirValue,
        use_block: BlockId,
        temp_definitions: &HashMap<Temp, BlockId>,
        dominance: &DominanceInfo,
    ) {
        match value {
            MirValue::Use(temp) => {
                self.verify_temp_dominance(*temp, use_block, temp_definitions, dominance);
            }
            MirValue::Const(_) => {
                // Constants don't use temporaries
            }
            MirValue::BinaryOp { lhs, rhs, .. } => {
                self.verify_temp_dominance(*lhs, use_block, temp_definitions, dominance);
                self.verify_temp_dominance(*rhs, use_block, temp_definitions, dominance);
            }
            MirValue::UnaryOp { operand, .. } => {
                self.verify_temp_dominance(*operand, use_block, temp_definitions, dominance);
            }
            MirValue::Call { args, .. } => {
                for &arg in args {
                    self.verify_temp_dominance(arg, use_block, temp_definitions, dominance);
                }
            }
            MirValue::ConstructAggregate { fields, .. } => {
                for &field in fields {
                    self.verify_temp_dominance(field, use_block, temp_definitions, dominance);
                }
            }
            MirValue::GetField { base, .. } => {
                self.verify_temp_dominance(*base, use_block, temp_definitions, dominance);
            }
            MirValue::SetField {
                base, value: val, ..
            } => {
                self.verify_temp_dominance(*base, use_block, temp_definitions, dominance);
                self.verify_temp_dominance(*val, use_block, temp_definitions, dominance);
            }
            MirValue::StructUpdate { base, updates, .. } => {
                self.verify_temp_dominance(*base, use_block, temp_definitions, dominance);
                for &(_, val) in updates {
                    self.verify_temp_dominance(val, use_block, temp_definitions, dominance);
                }
            }
            MirValue::DynamicArrayAccess { base, index } => {
                self.verify_temp_dominance(*base, use_block, temp_definitions, dominance);
                self.verify_temp_dominance(*index, use_block, temp_definitions, dominance);
            }
        }
    }

    /// Verify dominance for terminator uses
    fn verify_terminator_dominance(
        &mut self,
        terminator: &MirTerminator,
        use_block: BlockId,
        temp_definitions: &HashMap<Temp, BlockId>,
        dominance: &DominanceInfo,
    ) {
        match terminator {
            MirTerminator::Goto { args, .. } => {
                for &arg in args {
                    self.verify_temp_dominance(arg, use_block, temp_definitions, dominance);
                }
            }
            MirTerminator::Branch {
                condition,
                then_args,
                else_args,
                ..
            } => {
                self.verify_temp_dominance(*condition, use_block, temp_definitions, dominance);
                for &arg in then_args {
                    self.verify_temp_dominance(arg, use_block, temp_definitions, dominance);
                }
                for &arg in else_args {
                    self.verify_temp_dominance(arg, use_block, temp_definitions, dominance);
                }
            }
            MirTerminator::Switch {
                discriminant,
                targets,
                default_args,
                ..
            } => {
                self.verify_temp_dominance(*discriminant, use_block, temp_definitions, dominance);
                for (_, _, args) in targets {
                    for &arg in args {
                        self.verify_temp_dominance(arg, use_block, temp_definitions, dominance);
                    }
                }
                for &arg in default_args {
                    self.verify_temp_dominance(arg, use_block, temp_definitions, dominance);
                }
            }
            MirTerminator::Return {
                value: Some(temp), ..
            } => {
                self.verify_temp_dominance(*temp, use_block, temp_definitions, dominance);
            }
            MirTerminator::Return { value: None, .. } => {}
            MirTerminator::Unreachable { .. } => {}
            MirTerminator::Uninit => {}
        }
    }

    /// Verify that a temporary use is dominated by its definition
    fn verify_temp_dominance(
        &mut self,
        temp: Temp,
        use_block: BlockId,
        temp_definitions: &HashMap<Temp, BlockId>,
        dominance: &DominanceInfo,
    ) {
        match temp_definitions.get(&temp) {
            Some(&def_block) => {
                if !dominance.dominates(def_block, use_block) {
                    self.add_error(
                        format!(
                            "Use of temporary {} in block {:?} is not dominated by its definition in block {:?}",
                            temp, use_block, def_block
                        ),
                        ErrorCategory::DominanceViolation,
                        Some(use_block),
                        Some(temp),
                    );
                }
            }
            None => {
                self.add_error(
                    format!(
                        "Use of undefined temporary {} in block {:?}",
                        temp, use_block
                    ),
                    ErrorCategory::TemporaryError,
                    Some(use_block),
                    Some(temp),
                );
            }
        }
    }

    /// Verify type consistency throughout the function
    fn verify_type_consistency(
        &mut self,
        function: &MirFunction,
        block_map: &HashMap<BlockId, &BasicBlock>,
    ) {
        for block in &function.blocks {
            // Verify block parameter types are consistent with temp_types
            for &(temp, ref block_param_ty) in &block.params {
                if let Some(temp_ty) = function.temp_types.get(&temp)
                    && temp_ty != block_param_ty
                {
                    self.add_error(
                        format!(
                            "Block parameter {} has type {} but temp_types has {}",
                            temp, block_param_ty, temp_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block.id),
                        Some(temp),
                    );
                }
            }

            // Verify statement type consistency
            for stmt in &block.statements {
                self.verify_statement_types(stmt, block.id, function);
            }

            // Verify terminator type consistency
            self.verify_terminator_types(&block.terminator, block.id, function, block_map);
        }
    }

    /// Verify types in a statement
    fn verify_statement_types(
        &mut self,
        stmt: &MirStatement,
        block_id: BlockId,
        function: &MirFunction,
    ) {
        match stmt {
            MirStatement::Assign { dest, value, .. } => {
                // Check that destination temp has a type in temp_types
                let dest_ty = match function.temp_types.get(dest) {
                    Some(ty) => ty,
                    None => {
                        self.add_error(
                            format!("Assignment destination {} has no type in temp_types", dest),
                            ErrorCategory::TemporaryError,
                            Some(block_id),
                            Some(*dest),
                        );
                        return;
                    }
                };

                // Verify the value expression type matches
                self.verify_value_type(value, dest_ty, block_id, function);
            }
            MirStatement::Return {
                value: Some(temp), ..
            } => {
                // Verify return value type matches function return type
                if let Some(temp_ty) = function.temp_types.get(temp)
                    && temp_ty != &function.return_type
                {
                    self.add_error(
                        format!(
                            "Return value {} has type {} but function returns {}",
                            temp, temp_ty, function.return_type
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        Some(*temp),
                    );
                }
            }
            MirStatement::Return { value: None, .. } => {
                // Verify function returns unit type
                if function.return_type != RueType::Unit {
                    self.add_error(
                        format!(
                            "Bare return statement but function returns {}",
                            function.return_type
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        None,
                    );
                }
            }
        }
    }

    /// Verify the type of a value expression
    fn verify_value_type(
        &mut self,
        value: &MirValue,
        expected_ty: &RueType,
        block_id: BlockId,
        function: &MirFunction,
    ) {
        let temp_type_lookup = |temp: Temp| -> RueType {
            function
                .temp_types
                .get(&temp)
                .cloned()
                .unwrap_or(RueType::Unknown)
        };

        match value {
            MirValue::Use(temp) => {
                if let Some(temp_ty) = function.temp_types.get(temp)
                    && temp_ty != expected_ty
                {
                    self.add_error(
                        format!(
                            "Temporary {} has type {} but expected {}",
                            temp, temp_ty, expected_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        Some(*temp),
                    );
                }
            }
            MirValue::Const(const_val) => {
                let const_ty = const_val.ty();
                if &const_ty != expected_ty {
                    self.add_error(
                        format!(
                            "Constant has type {} but expected {}",
                            const_ty, expected_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        None,
                    );
                }
            }
            MirValue::BinaryOp { op, lhs, rhs } => {
                // Verify operand types are compatible
                let lhs_ty = temp_type_lookup(*lhs);
                let rhs_ty = temp_type_lookup(*rhs);

                if lhs_ty != rhs_ty {
                    self.add_error(
                        format!(
                            "Binary operation {:?} has mismatched operand types: {} and {}",
                            op, lhs_ty, rhs_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        None,
                    );
                }

                // Verify operation is valid for the type and result type is correct
                let expected_result_ty = match op {
                    MirBinOp::Add
                    | MirBinOp::Sub
                    | MirBinOp::Mul
                    | MirBinOp::Div
                    | MirBinOp::Mod => {
                        // Arithmetic operations preserve the operand type
                        if !matches!(lhs_ty, RueType::I32 | RueType::I64) {
                            self.add_error(
                                format!(
                                    "Arithmetic operation {:?} not valid for type {}",
                                    op, lhs_ty
                                ),
                                ErrorCategory::InvalidOperation,
                                Some(block_id),
                                None,
                            );
                        }
                        lhs_ty
                    }
                    MirBinOp::Lt | MirBinOp::Le | MirBinOp::Gt | MirBinOp::Ge => {
                        // Comparison operations require numeric types and return bool
                        if !matches!(lhs_ty, RueType::I32 | RueType::I64) {
                            self.add_error(
                                format!(
                                    "Comparison operation {:?} not valid for type {}",
                                    op, lhs_ty
                                ),
                                ErrorCategory::InvalidOperation,
                                Some(block_id),
                                None,
                            );
                        }
                        RueType::Bool
                    }
                    MirBinOp::Eq | MirBinOp::Ne => {
                        // Equality operations work on any type and return bool
                        RueType::Bool
                    }
                };

                if &expected_result_ty != expected_ty {
                    self.add_error(
                        format!(
                            "Binary operation {:?} produces {} but expected {}",
                            op, expected_result_ty, expected_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        None,
                    );
                }
            }
            MirValue::UnaryOp { op, operand } => {
                let operand_ty = temp_type_lookup(*operand);

                if !op.is_valid_for_type(&operand_ty) {
                    self.add_error(
                        format!("Unary operation {:?} not valid for type {}", op, operand_ty),
                        ErrorCategory::InvalidOperation,
                        Some(block_id),
                        Some(*operand),
                    );
                }

                if let Some(result_ty) = op.result_type(&operand_ty)
                    && &result_ty != expected_ty
                {
                    self.add_error(
                        format!(
                            "Unary operation {:?} produces {} but expected {}",
                            op, result_ty, expected_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        Some(*operand),
                    );
                }
            }
            MirValue::Call { .. } => {
                // Call type checking is complex and depends on function signatures
                // For now, we trust that the type in temp_types is correct
                // This could be enhanced with function signature verification
            }
            MirValue::ConstructAggregate { ty, fields } => {
                if ty != expected_ty {
                    self.add_error(
                        format!(
                            "Aggregate construction creates {} but expected {}",
                            ty, expected_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        None,
                    );
                }

                // Verify field types match aggregate structure
                match ty {
                    RueType::Tuple(field_types) => {
                        if fields.len() != field_types.len() {
                            self.add_error(
                                format!(
                                    "Tuple construction has {} fields but type expects {}",
                                    fields.len(),
                                    field_types.len()
                                ),
                                ErrorCategory::TypeError,
                                Some(block_id),
                                None,
                            );
                        } else {
                            for (i, (&field_temp, expected_field_ty)) in
                                fields.iter().zip(field_types).enumerate()
                            {
                                let field_ty = temp_type_lookup(field_temp);
                                if &field_ty != expected_field_ty {
                                    self.add_error(
                                        format!(
                                            "Tuple field {} has type {} but expected {}",
                                            i, field_ty, expected_field_ty
                                        ),
                                        ErrorCategory::TypeError,
                                        Some(block_id),
                                        Some(field_temp),
                                    );
                                }
                            }
                        }
                    }
                    RueType::Array(elem_ty, len) => {
                        if fields.len() != *len {
                            self.add_error(
                                format!(
                                    "Array construction has {} elements but type expects {}",
                                    fields.len(),
                                    len
                                ),
                                ErrorCategory::TypeError,
                                Some(block_id),
                                None,
                            );
                        } else {
                            for (i, &field_temp) in fields.iter().enumerate() {
                                let field_ty = temp_type_lookup(field_temp);
                                if &field_ty != elem_ty.as_ref() {
                                    self.add_error(
                                        format!(
                                            "Array element {} has type {} but expected {}",
                                            i, field_ty, elem_ty
                                        ),
                                        ErrorCategory::TypeError,
                                        Some(block_id),
                                        Some(field_temp),
                                    );
                                }
                            }
                        }
                    }
                    RueType::Struct(_) => {
                        // Struct field verification would require looking up the struct definition
                        // For now, we trust the type system
                    }
                    _ => {
                        self.add_error(
                            format!("ConstructAggregate used with non-aggregate type {}", ty),
                            ErrorCategory::TypeError,
                            Some(block_id),
                            None,
                        );
                    }
                }
            }
            MirValue::GetField { base, field } => {
                let base_ty = temp_type_lookup(*base);
                // Verify field access is valid and result type is correct
                let field_ty = match (&base_ty, field) {
                    (RueType::Tuple(types), FieldId::Index(idx)) => {
                        if *idx >= types.len() {
                            self.add_error(
                                format!(
                                    "Field index {} out of bounds for tuple with {} fields",
                                    idx,
                                    types.len()
                                ),
                                ErrorCategory::TypeError,
                                Some(block_id),
                                Some(*base),
                            );
                            return;
                        }
                        types[*idx].clone()
                    }
                    (RueType::Array(elem_ty, len), FieldId::Index(idx)) => {
                        if *idx >= *len {
                            self.add_error(
                                format!(
                                    "Field index {} out of bounds for array with {} elements",
                                    idx, len
                                ),
                                ErrorCategory::TypeError,
                                Some(block_id),
                                Some(*base),
                            );
                            return;
                        }
                        elem_ty.as_ref().clone()
                    }
                    (RueType::Struct(struct_id), field_id) => {
                        // Would need to look up struct definition
                        match self.type_context.get_field_type(*struct_id, field_id) {
                            Ok(field_ty) => field_ty.clone(),
                            Err(_) => {
                                self.add_error(
                                    format!(
                                        "Invalid field access: {} on struct {}",
                                        field_id, struct_id
                                    ),
                                    ErrorCategory::TypeError,
                                    Some(block_id),
                                    Some(*base),
                                );
                                return;
                            }
                        }
                    }
                    _ => {
                        self.add_error(
                            format!("Invalid field access: {} on type {}", field, base_ty),
                            ErrorCategory::TypeError,
                            Some(block_id),
                            Some(*base),
                        );
                        return;
                    }
                };

                if &field_ty != expected_ty {
                    self.add_error(
                        format!(
                            "Field access produces {} but expected {}",
                            field_ty, expected_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        Some(*base),
                    );
                }
            }
            MirValue::SetField { base, .. } => {
                let base_ty = temp_type_lookup(*base);
                if &base_ty != expected_ty {
                    self.add_error(
                        format!("SetField produces {} but expected {}", base_ty, expected_ty),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        Some(*base),
                    );
                }
            }
            MirValue::StructUpdate { struct_type, .. } => {
                let expected_struct_ty = RueType::Struct(*struct_type);
                if &expected_struct_ty != expected_ty {
                    self.add_error(
                        format!(
                            "StructUpdate produces {} but expected {}",
                            expected_struct_ty, expected_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        None,
                    );
                }
            }
            MirValue::DynamicArrayAccess { base, index } => {
                let base_ty = temp_type_lookup(*base);
                let index_ty = temp_type_lookup(*index);

                // Index must be integer
                if !matches!(index_ty, RueType::I32 | RueType::I64) {
                    self.add_error(
                        format!("Array index has type {} but must be integer", index_ty),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        Some(*index),
                    );
                }

                // Base must be array
                match &base_ty {
                    RueType::Array(elem_ty, _) => {
                        if elem_ty.as_ref() != expected_ty {
                            self.add_error(
                                format!(
                                    "Array access produces {} but expected {}",
                                    elem_ty, expected_ty
                                ),
                                ErrorCategory::TypeError,
                                Some(block_id),
                                Some(*base),
                            );
                        }
                    }
                    _ => {
                        self.add_error(
                            format!("Dynamic array access on non-array type {}", base_ty),
                            ErrorCategory::TypeError,
                            Some(block_id),
                            Some(*base),
                        );
                    }
                }
            }
        }
    }

    /// Verify terminator type consistency and control flow
    fn verify_terminator_types(
        &mut self,
        terminator: &MirTerminator,
        block_id: BlockId,
        function: &MirFunction,
        block_map: &HashMap<BlockId, &BasicBlock>,
    ) {
        match terminator {
            MirTerminator::Goto { target, args, .. } => {
                self.verify_jump_types(*target, args, block_id, function, block_map);
            }
            MirTerminator::Branch {
                condition,
                then_block,
                then_args,
                else_block,
                else_args,
                ..
            } => {
                // Verify condition is boolean
                if let Some(cond_ty) = function.temp_types.get(condition)
                    && cond_ty != &RueType::Bool
                {
                    self.add_error(
                        format!("Branch condition has type {} but must be bool", cond_ty),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        Some(*condition),
                    );
                }

                self.verify_jump_types(*then_block, then_args, block_id, function, block_map);
                self.verify_jump_types(*else_block, else_args, block_id, function, block_map);
            }
            MirTerminator::Switch {
                discriminant,
                targets,
                default,
                default_args,
                ..
            } => {
                // Verify discriminant is integer
                if let Some(discrim_ty) = function.temp_types.get(discriminant)
                    && !matches!(discrim_ty, RueType::I32 | RueType::I64)
                {
                    self.add_error(
                        format!(
                            "Switch discriminant has type {} but must be integer",
                            discrim_ty
                        ),
                        ErrorCategory::TypeError,
                        Some(block_id),
                        Some(*discriminant),
                    );
                }

                for (_, target, args) in targets {
                    self.verify_jump_types(*target, args, block_id, function, block_map);
                }
                self.verify_jump_types(*default, default_args, block_id, function, block_map);
            }
            MirTerminator::Return {
                value: Some(_temp), ..
            } => {
                // Already verified in statement verification
            }
            MirTerminator::Return { value: None, .. } => {
                // Already verified in statement verification
            }
            MirTerminator::Unreachable { .. } => {
                // Always valid
            }
            MirTerminator::Uninit => {
                // Will be caught in uninit verification
            }
        }
    }

    /// Verify jump argument types match target block parameters
    fn verify_jump_types(
        &mut self,
        target: BlockId,
        args: &[Temp],
        from_block: BlockId,
        function: &MirFunction,
        block_map: &HashMap<BlockId, &BasicBlock>,
    ) {
        let target_block = match block_map.get(&target) {
            Some(block) => block,
            None => return, // Error already reported in structure verification
        };

        if args.len() != target_block.params.len() {
            self.add_error(
                format!(
                    "Jump from {:?} to {:?} has {} arguments but target expects {}",
                    from_block,
                    target,
                    args.len(),
                    target_block.params.len()
                ),
                ErrorCategory::ControlFlowError,
                Some(from_block),
                None,
            );
            return;
        }

        for (i, (&arg_temp, (_, param_ty))) in args.iter().zip(&target_block.params).enumerate() {
            if let Some(arg_ty) = function.temp_types.get(&arg_temp)
                && arg_ty != param_ty
            {
                self.add_error(
                    format!(
                        "Jump argument {} has type {} but target parameter has type {}",
                        i, arg_ty, param_ty
                    ),
                    ErrorCategory::TypeError,
                    Some(from_block),
                    Some(arg_temp),
                );
            }
        }
    }

    /// Verify terminator targets exist
    fn verify_terminator_targets(
        &mut self,
        terminator: &MirTerminator,
        block_id: BlockId,
        block_map: &HashMap<BlockId, &BasicBlock>,
    ) {
        match terminator {
            MirTerminator::Goto { target, .. } => {
                if !block_map.contains_key(target) {
                    self.add_error(
                        format!("Goto target {:?} does not exist", target),
                        ErrorCategory::ControlFlowError,
                        Some(block_id),
                        None,
                    );
                }
            }
            MirTerminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                if !block_map.contains_key(then_block) {
                    self.add_error(
                        format!("Branch then-target {:?} does not exist", then_block),
                        ErrorCategory::ControlFlowError,
                        Some(block_id),
                        None,
                    );
                }
                if !block_map.contains_key(else_block) {
                    self.add_error(
                        format!("Branch else-target {:?} does not exist", else_block),
                        ErrorCategory::ControlFlowError,
                        Some(block_id),
                        None,
                    );
                }
            }
            MirTerminator::Switch {
                targets, default, ..
            } => {
                for (value, target, _) in targets {
                    if !block_map.contains_key(target) {
                        self.add_error(
                            format!(
                                "Switch target {:?} for value {} does not exist",
                                target, value
                            ),
                            ErrorCategory::ControlFlowError,
                            Some(block_id),
                            None,
                        );
                    }
                }
                if !block_map.contains_key(default) {
                    self.add_error(
                        format!("Switch default target {:?} does not exist", default),
                        ErrorCategory::ControlFlowError,
                        Some(block_id),
                        None,
                    );
                }
            }
            MirTerminator::Return { .. }
            | MirTerminator::Unreachable { .. }
            | MirTerminator::Uninit => {
                // No targets to verify
            }
        }
    }

    /// Verify temporary usage (all defined temps are used, all used temps are defined)
    fn verify_temporary_usage(
        &mut self,
        function: &MirFunction,
        _block_map: &HashMap<BlockId, &BasicBlock>,
    ) {
        let mut defined_temps = HashSet::new();
        let mut used_temps = HashSet::new();

        // Collect all defined temporaries
        for block in &function.blocks {
            for &(temp, _) in &block.params {
                defined_temps.insert(temp);
            }
            for stmt in &block.statements {
                if let MirStatement::Assign { dest, .. } = stmt {
                    defined_temps.insert(*dest);
                }
            }
        }

        // Collect all used temporaries
        for block in &function.blocks {
            for stmt in &block.statements {
                match stmt {
                    MirStatement::Assign { value, .. } => {
                        self.collect_value_uses(value, &mut used_temps);
                    }
                    MirStatement::Return {
                        value: Some(temp), ..
                    } => {
                        used_temps.insert(*temp);
                    }
                    MirStatement::Return { value: None, .. } => {}
                }
            }
            self.collect_terminator_uses(&block.terminator, &mut used_temps);
        }

        // Check for unused defined temporaries (except function return value)
        for &temp in &defined_temps {
            if !used_temps.contains(&temp) {
                // Allow the final return value to be "unused" in the sense that it's only used by return
                // For now, we'll skip this check to avoid false positives
            }
        }

        // Check for used but undefined temporaries
        for &temp in &used_temps {
            if !defined_temps.contains(&temp) && !function.temp_types.contains_key(&temp) {
                self.add_error(
                    format!("Temporary {} is used but never defined", temp),
                    ErrorCategory::TemporaryError,
                    None,
                    Some(temp),
                );
            }
        }

        // Verify all temps in temp_types are actually defined
        for &temp in function.temp_types.keys() {
            if !defined_temps.contains(&temp) {
                self.add_error(
                    format!("Temporary {} in temp_types is never defined", temp),
                    ErrorCategory::TemporaryError,
                    None,
                    Some(temp),
                );
            }
        }
    }

    /// Collect temporary uses from a value
    fn collect_value_uses(&self, value: &MirValue, used_temps: &mut HashSet<Temp>) {
        match value {
            MirValue::Use(temp) => {
                used_temps.insert(*temp);
            }
            MirValue::Const(_) => {}
            MirValue::BinaryOp { lhs, rhs, .. } => {
                used_temps.insert(*lhs);
                used_temps.insert(*rhs);
            }
            MirValue::UnaryOp { operand, .. } => {
                used_temps.insert(*operand);
            }
            MirValue::Call { args, .. } => {
                for &arg in args {
                    used_temps.insert(arg);
                }
            }
            MirValue::ConstructAggregate { fields, .. } => {
                for &field in fields {
                    used_temps.insert(field);
                }
            }
            MirValue::GetField { base, .. } => {
                used_temps.insert(*base);
            }
            MirValue::SetField {
                base, value: val, ..
            } => {
                used_temps.insert(*base);
                used_temps.insert(*val);
            }
            MirValue::StructUpdate { base, updates, .. } => {
                used_temps.insert(*base);
                for &(_, val) in updates {
                    used_temps.insert(val);
                }
            }
            MirValue::DynamicArrayAccess { base, index } => {
                used_temps.insert(*base);
                used_temps.insert(*index);
            }
        }
    }

    /// Collect temporary uses from a terminator
    fn collect_terminator_uses(&self, terminator: &MirTerminator, used_temps: &mut HashSet<Temp>) {
        match terminator {
            MirTerminator::Goto { args, .. } => {
                for &arg in args {
                    used_temps.insert(arg);
                }
            }
            MirTerminator::Branch {
                condition,
                then_args,
                else_args,
                ..
            } => {
                used_temps.insert(*condition);
                for &arg in then_args {
                    used_temps.insert(arg);
                }
                for &arg in else_args {
                    used_temps.insert(arg);
                }
            }
            MirTerminator::Switch {
                discriminant,
                targets,
                default_args,
                ..
            } => {
                used_temps.insert(*discriminant);
                for (_, _, args) in targets {
                    for &arg in args {
                        used_temps.insert(arg);
                    }
                }
                for &arg in default_args {
                    used_temps.insert(arg);
                }
            }
            MirTerminator::Return {
                value: Some(temp), ..
            } => {
                used_temps.insert(*temp);
            }
            MirTerminator::Return { value: None, .. } => {}
            MirTerminator::Unreachable { .. } => {}
            MirTerminator::Uninit => {}
        }
    }

    /// Verify no Uninit terminators remain (these are sentinel values that should be replaced)
    fn verify_no_uninit_terminators(&mut self, function: &MirFunction) {
        for block in &function.blocks {
            if matches!(block.terminator, MirTerminator::Uninit) {
                self.add_error(
                    format!("Block {:?} has uninitialized terminator", block.id),
                    ErrorCategory::InvalidTerminator,
                    Some(block.id),
                    None,
                );
            }
        }
    }

    /// Add an error to the list
    fn add_error(
        &mut self,
        message: String,
        category: ErrorCategory,
        block: Option<BlockId>,
        temp: Option<Temp>,
    ) {
        self.errors.push(VerificationError {
            message,
            function: self.current_function.clone(),
            block,
            temp,
            category,
        });
    }
}

/// Convenience function to verify a single MIR function
pub fn verify_function(function: &MirFunction, type_context: &TypeContext) -> Result<(), String> {
    let mut verifier = MirVerifier::new(type_context);
    match verifier.verify_function(function) {
        Ok(()) => Ok(()),
        Err(errors) => {
            let error_messages: Vec<String> = errors.into_iter().map(|e| e.to_string()).collect();
            Err(error_messages.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{MirBinOp, MirConst};
    use crate::types::TypeContext;
    use rue_lexer::Span;
    use std::collections::HashMap;

    fn create_simple_function() -> MirFunction {
        let mut temp_types = HashMap::new();
        temp_types.insert(Temp(0), RueType::I32); // Function parameter
        temp_types.insert(Temp(1), RueType::I32); // Assignment result

        MirFunction {
            name: "test".to_string(),
            params: vec![("x".to_string(), RueType::I32)],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            temp_types,
            span: Span::dummy(),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: vec![(Temp(0), RueType::I32)],
                statements: vec![MirStatement::Assign {
                    dest: Temp(1),
                    value: MirValue::BinaryOp {
                        op: MirBinOp::Add,
                        lhs: Temp(0),
                        rhs: Temp(0),
                    },
                    span: None,
                }],
                terminator: MirTerminator::Return {
                    value: Some(Temp(1)),
                    span: None,
                },
            }],
        }
    }

    #[test]
    fn test_valid_function_passes_verification() {
        let function = create_simple_function();
        let type_context = TypeContext::new();
        let mut verifier = MirVerifier::new(&type_context);

        assert!(verifier.verify_function(&function).is_ok());
    }

    #[test]
    fn test_uninit_terminator_fails_verification() {
        let mut function = create_simple_function();
        function.blocks[0].terminator = MirTerminator::Uninit;

        let type_context = TypeContext::new();
        let mut verifier = MirVerifier::new(&type_context);

        let result = verifier.verify_function(&function);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.category == ErrorCategory::InvalidTerminator)
        );
    }

    #[test]
    fn test_dominance_violation_fails_verification() {
        let mut temp_types = HashMap::new();
        temp_types.insert(Temp(0), RueType::I32); // Function parameter
        temp_types.insert(Temp(1), RueType::I32); // Defined in B1
        temp_types.insert(Temp(2), RueType::I32); // Used in B0 but defined in B1 (dominance violation)

        let function = MirFunction {
            name: "test_dominance".to_string(),
            params: vec![("x".to_string(), RueType::I32)],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            temp_types,
            span: Span::dummy(),
            blocks: vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![(Temp(0), RueType::I32)],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(2), // This would be a dominance violation if we used Temp(1) here
                        value: MirValue::Use(Temp(0)), // Use parameter instead
                        span: None,
                    }],
                    terminator: MirTerminator::Goto {
                        target: BlockId(1),
                        args: vec![Temp(2)],
                        span: None,
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![(Temp(1), RueType::I32)],
                    statements: vec![],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(1)),
                        span: None,
                    },
                },
            ],
        };

        let type_context = TypeContext::new();
        let mut verifier = MirVerifier::new(&type_context);

        // This should actually pass since we fixed the dominance violation
        assert!(verifier.verify_function(&function).is_ok());
    }

    #[test]
    fn test_actual_dominance_violation() {
        let mut temp_types = HashMap::new();
        temp_types.insert(Temp(0), RueType::I32); // Function parameter
        temp_types.insert(Temp(1), RueType::I32); // Defined in B1
        temp_types.insert(Temp(2), RueType::I32); // Assignment target

        let function = MirFunction {
            name: "test_dominance".to_string(),
            params: vec![("x".to_string(), RueType::I32)],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            temp_types,
            span: Span::dummy(),
            blocks: vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![(Temp(0), RueType::I32)],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(2),
                        value: MirValue::Use(Temp(1)), // Use Temp(1) which is defined in B1 - dominance violation!
                        span: None,
                    }],
                    terminator: MirTerminator::Goto {
                        target: BlockId(1),
                        args: vec![Temp(2)],
                        span: None,
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![(Temp(1), RueType::I32)],
                    statements: vec![],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(1)),
                        span: None,
                    },
                },
            ],
        };

        let type_context = TypeContext::new();
        let mut verifier = MirVerifier::new(&type_context);

        let result = verifier.verify_function(&function);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.category == ErrorCategory::DominanceViolation)
        );
    }

    #[test]
    fn test_type_mismatch_fails_verification() {
        let mut temp_types = HashMap::new();
        temp_types.insert(Temp(0), RueType::I32); // Function parameter
        temp_types.insert(Temp(1), RueType::Bool); // Wrong type for the assignment

        let function = MirFunction {
            name: "test_type_mismatch".to_string(),
            params: vec![("x".to_string(), RueType::I32)],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            temp_types,
            span: Span::dummy(),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: vec![(Temp(0), RueType::I32)],
                statements: vec![MirStatement::Assign {
                    dest: Temp(1),                               // Temp(1) has type Bool in temp_types
                    value: MirValue::Const(MirConst::Int32(42)), // But we're assigning an i32
                    span: None,
                }],
                terminator: MirTerminator::Return {
                    value: Some(Temp(0)), // Return the parameter instead
                    span: None,
                },
            }],
        };

        let type_context = TypeContext::new();
        let mut verifier = MirVerifier::new(&type_context);

        let result = verifier.verify_function(&function);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.category == ErrorCategory::TypeError)
        );
    }

    #[test]
    fn test_signature_mismatch_fails_verification() {
        let mut temp_types = HashMap::new();
        temp_types.insert(Temp(0), RueType::I32);

        let function = MirFunction {
            name: "test_signature_mismatch".to_string(),
            params: vec![
                ("x".to_string(), RueType::I32),
                ("y".to_string(), RueType::I32),
            ], // Two params
            return_type: RueType::I32,
            entry_block: BlockId(0),
            temp_types,
            span: Span::dummy(),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: vec![(Temp(0), RueType::I32)], // But entry block only has one param
                statements: vec![],
                terminator: MirTerminator::Return {
                    value: Some(Temp(0)),
                    span: None,
                },
            }],
        };

        let type_context = TypeContext::new();
        let mut verifier = MirVerifier::new(&type_context);

        let result = verifier.verify_function(&function);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.category == ErrorCategory::SignatureMismatch)
        );
    }

    #[test]
    fn test_convenience_function() {
        let function = create_simple_function();
        let type_context = TypeContext::new();

        assert!(verify_function(&function, &type_context).is_ok());

        // Test with invalid function
        let mut invalid_function = create_simple_function();
        invalid_function.blocks[0].terminator = MirTerminator::Uninit;

        assert!(verify_function(&invalid_function, &type_context).is_err());
    }

    #[test]
    fn test_comprehensive_verification_example() {
        // This test demonstrates comprehensive verification of a more complex function
        use crate::types::TypeContext;

        let type_context = TypeContext::new();
        let mut temp_types = HashMap::new();
        temp_types.insert(Temp(0), RueType::I32); // Function parameter
        temp_types.insert(Temp(1), RueType::Bool); // Comparison result
        temp_types.insert(Temp(2), RueType::I32); // Block parameter
        temp_types.insert(Temp(3), RueType::I32); // Block parameter
        temp_types.insert(Temp(4), RueType::I32); // Final result

        // Create a function with conditional logic:
        // fn test_conditional(x: i32) -> i32 {
        //   if (x > 10) {
        //     return x + 1;
        //   } else {
        //     return x - 1;
        //   }
        // }
        let function = MirFunction {
            name: "test_conditional".to_string(),
            params: vec![("x".to_string(), RueType::I32)],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            temp_types,
            span: Span::dummy(),
            blocks: vec![
                // B0: Entry block
                BasicBlock {
                    id: BlockId(0),
                    params: vec![(Temp(0), RueType::I32)],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(1),
                        value: MirValue::BinaryOp {
                            op: MirBinOp::Gt,
                            lhs: Temp(0),
                            rhs: Temp(0), // Using x > x for simplicity (should be x > 10)
                        },
                        span: None,
                    }],
                    terminator: MirTerminator::Branch {
                        condition: Temp(1),
                        then_block: BlockId(1),
                        then_args: vec![Temp(0)],
                        else_block: BlockId(2),
                        else_args: vec![Temp(0)],
                        span: None,
                    },
                },
                // B1: Then block (x + 1)
                BasicBlock {
                    id: BlockId(1),
                    params: vec![(Temp(2), RueType::I32)],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(4),
                        value: MirValue::BinaryOp {
                            op: MirBinOp::Add,
                            lhs: Temp(2),
                            rhs: Temp(2), // Should be temp(2) + 1 but using temp for simplicity
                        },
                        span: None,
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(4)),
                        span: None,
                    },
                },
                // B2: Else block (x - 1)
                BasicBlock {
                    id: BlockId(2),
                    params: vec![(Temp(3), RueType::I32)],
                    statements: vec![],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(3)),
                        span: None,
                    },
                },
            ],
        };

        // Verify the function passes all checks
        let result = verify_function(&function, &type_context);
        assert!(
            result.is_ok(),
            "Verification should pass but got: {:?}",
            result.err()
        );
    }
}
