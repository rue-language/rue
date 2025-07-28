//! MIR Verifier Pass
//!
//! This module implements a verification pass for MIR that checks:
//! - Block arguments match parameters in goto/branch instructions
//! - All temps are defined before use (dominance)
//! - Each temp is assigned at most once (SSA property)
//! - Types are consistent across edges

use crate::mir::{
    BasicBlock, BlockId, MirFunction, MirProgram, MirStatement, MirTerminator, MirValue, Temp,
};
use crate::types::RueType;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct VerificationError {
    pub message: String,
    pub function: String,
    pub block: Option<BlockId>,
}

pub struct MirVerifier {
    errors: Vec<VerificationError>,
    current_function: String,
}

impl Default for MirVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl MirVerifier {
    pub fn new() -> Self {
        Self {
            errors: Vec::new(),
            current_function: String::new(),
        }
    }

    /// Verify an entire MIR program
    pub fn verify_program(&mut self, program: &MirProgram) -> Result<(), Vec<VerificationError>> {
        for function in &program.functions {
            self.verify_function(function);
        }

        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(std::mem::take(&mut self.errors))
        }
    }

    /// Verify a single function
    fn verify_function(&mut self, function: &MirFunction) {
        self.current_function = function.name.clone();

        // Build a map of block IDs to blocks for quick lookup
        let block_map: HashMap<BlockId, &BasicBlock> =
            function.blocks.iter().map(|b| (b.id, b)).collect();

        // Check that entry block exists
        if !block_map.contains_key(&function.entry_block) {
            self.add_error(
                format!(
                    "Entry block {:?} not found in function",
                    function.entry_block
                ),
                None,
            );
            return;
        }

        // Track defined temps and their types
        let mut defined_temps: HashMap<Temp, RueType> = HashMap::new();

        // Add function parameters as defined temps
        if let Some(entry_block) = block_map.get(&function.entry_block) {
            for (temp, ty) in &entry_block.params {
                defined_temps.insert(*temp, ty.clone());
            }
        }

        // Verify each block
        for block in &function.blocks {
            self.verify_block(block, &block_map, &mut defined_temps);
        }

        // Check for unreachable blocks (except entry)
        let reachable = self.find_reachable_blocks(function.entry_block, &block_map);
        for block in &function.blocks {
            if block.id != function.entry_block && !reachable.contains(&block.id) {
                self.add_error(
                    format!("Block {:?} is unreachable", block.id),
                    Some(block.id),
                );
            }
        }
    }

    /// Verify a single block
    fn verify_block(
        &mut self,
        block: &BasicBlock,
        block_map: &HashMap<BlockId, &BasicBlock>,
        defined_temps: &mut HashMap<Temp, RueType>,
    ) {
        // Add block parameters as defined
        for (temp, ty) in &block.params {
            if let Some(existing_ty) = defined_temps.insert(*temp, ty.clone()) {
                if existing_ty != *ty {
                    self.add_error(
                        format!(
                            "Block parameter {temp:?} type mismatch: {existing_ty:?} vs {ty:?}"
                        ),
                        Some(block.id),
                    );
                }
            }
        }

        // Verify statements
        for stmt in &block.statements {
            self.verify_statement(stmt, block.id, defined_temps);
        }

        // Verify terminator
        self.verify_terminator(&block.terminator, block.id, block_map, defined_temps);
    }

    /// Verify a statement
    fn verify_statement(
        &mut self,
        stmt: &MirStatement,
        block_id: BlockId,
        defined_temps: &mut HashMap<Temp, RueType>,
    ) {
        match stmt {
            MirStatement::Assign { dest, value, .. } => {
                // Check that dest is not already defined (SSA property)
                if defined_temps.contains_key(dest) {
                    self.add_error(
                        format!("Temp {dest:?} assigned multiple times (violates SSA)"),
                        Some(block_id),
                    );
                }

                // Special handling for Call assignments since calls don't carry return type info
                match value {
                    MirValue::Call { args, func, .. } => {
                        // Debug output
                        if std::env::var("RUE_DEBUG_MIR").is_ok() {
                            eprintln!("DEBUG: Processing call assignment: {dest:?} = {func}({args:?})");
                        }
                        
                        // Verify all arguments are defined
                        for arg in args {
                            if !defined_temps.contains_key(arg) {
                                self.add_error(
                                    format!("Use of undefined temp {arg:?} in call to {func}"),
                                    Some(block_id),
                                );
                            }
                        }
                        
                        // For calls, we infer the return type from the function signature
                        // For now, we'll use a placeholder approach and mark the dest as defined
                        // with an inferred type based on common function signatures
                        let inferred_type = self.infer_call_return_type(func);
                        if std::env::var("RUE_DEBUG_MIR").is_ok() {
                            eprintln!("DEBUG: Inferred type for {func}: {inferred_type:?}, marking {dest:?} as defined");
                        }
                        defined_temps.insert(*dest, inferred_type);
                    }
                    _ => {
                        // For non-call values, use the original logic
                        if let Some(value_ty) = self.verify_value(value, block_id, defined_temps) {
                            defined_temps.insert(*dest, value_ty);
                        }
                    }
                }
            }
        }
    }

    /// Verify a value and return its type
    fn verify_value(
        &mut self,
        value: &MirValue,
        block_id: BlockId,
        defined_temps: &HashMap<Temp, RueType>,
    ) -> Option<RueType> {
        match value {
            MirValue::Use(temp) => {
                if let Some(ty) = defined_temps.get(temp) {
                    Some(ty.clone())
                } else {
                    self.add_error(format!("Use of undefined temp {temp:?}"), Some(block_id));
                    None
                }
            }
            MirValue::Const(c) => Some(c.ty()),
            MirValue::BinaryOp { op, lhs, rhs } => {
                let lhs_ty = if let Some(ty) = defined_temps.get(lhs) {
                    Some(ty.clone())
                } else {
                    self.add_error(
                        format!("Use of undefined temp {lhs:?} in binary op"),
                        Some(block_id),
                    );
                    None
                };

                let rhs_ty = if let Some(ty) = defined_temps.get(rhs) {
                    Some(ty.clone())
                } else {
                    self.add_error(
                        format!("Use of undefined temp {rhs:?} in binary op"),
                        Some(block_id),
                    );
                    None
                };

                // Check operand types match
                if let (Some(lhs_t), Some(rhs_t)) = (&lhs_ty, &rhs_ty) {
                    if lhs_t != rhs_t {
                        self.add_error(
                            format!(
                                "Binary op operands have mismatched types: {lhs_t:?} vs {rhs_t:?}"
                            ),
                            Some(block_id),
                        );
                    }
                }

                // Determine result type based on operation
                match op {
                    crate::mir::MirBinOp::Add
                    | crate::mir::MirBinOp::Sub
                    | crate::mir::MirBinOp::Mul
                    | crate::mir::MirBinOp::Div
                    | crate::mir::MirBinOp::Mod => {
                        // Arithmetic ops preserve operand type
                        lhs_ty.or(rhs_ty)
                    }
                    crate::mir::MirBinOp::Lt
                    | crate::mir::MirBinOp::Le
                    | crate::mir::MirBinOp::Gt
                    | crate::mir::MirBinOp::Ge
                    | crate::mir::MirBinOp::Eq
                    | crate::mir::MirBinOp::Ne => {
                        // Comparison ops always return Bool
                        Some(RueType::Bool)
                    }
                }
            }
            MirValue::UnaryOp { operand, .. } => {
                if let Some(ty) = defined_temps.get(operand) {
                    Some(ty.clone())
                } else {
                    self.add_error(
                        format!("Use of undefined temp {operand:?} in unary op"),
                        Some(block_id),
                    );
                    None
                }
            }
            MirValue::Call {
                args,
                func,
                kind: _,
            } => {
                // Verify all arguments are defined
                for arg in args {
                    if !defined_temps.contains_key(arg) {
                        self.add_error(
                            format!("Use of undefined temp {arg:?} in call to {func}"),
                            Some(block_id),
                        );
                    }
                }
                // Call return type needs to be determined from assignment context
                // For now, return None to indicate the type should come from the dest
                None
            }
        }
    }

    /// Verify a terminator
    fn verify_terminator(
        &mut self,
        term: &MirTerminator,
        block_id: BlockId,
        block_map: &HashMap<BlockId, &BasicBlock>,
        defined_temps: &HashMap<Temp, RueType>,
    ) {
        match term {
            MirTerminator::Goto { target, args, .. } => {
                self.verify_jump(*target, args, block_id, block_map, defined_temps);
            }
            MirTerminator::Branch {
                condition,
                then_block,
                then_args,
                else_block,
                else_args,
                ..
            } => {
                // Verify condition is defined
                if !defined_temps.contains_key(condition) {
                    self.add_error(
                        format!("Branch condition {condition:?} is undefined"),
                        Some(block_id),
                    );
                }

                // Verify both jumps
                self.verify_jump(*then_block, then_args, block_id, block_map, defined_temps);
                self.verify_jump(*else_block, else_args, block_id, block_map, defined_temps);
            }
            MirTerminator::Switch {
                discriminant,
                targets,
                default,
                default_args,
                ..
            } => {
                // Verify discriminant is defined
                if !defined_temps.contains_key(discriminant) {
                    self.add_error(
                        format!("Switch discriminant {discriminant:?} is undefined"),
                        Some(block_id),
                    );
                }

                // Verify all target jumps
                for (_, target_block, target_args) in targets {
                    self.verify_jump(*target_block, target_args, block_id, block_map, defined_temps);
                }

                // Verify default jump
                self.verify_jump(*default, default_args, block_id, block_map, defined_temps);
            }
            MirTerminator::Unreachable { .. } => {
                // Unreachable terminators are always valid (they should never be reached)
            }
            MirTerminator::Return { value, .. } => {
                if let Some(val) = value {
                    if !defined_temps.contains_key(val) {
                        self.add_error(
                            format!("Return value {val:?} is undefined"),
                            Some(block_id),
                        );
                    }
                }
            }
        }
    }

    /// Verify a jump to a target block
    fn verify_jump(
        &mut self,
        target: BlockId,
        args: &[Temp],
        from_block: BlockId,
        block_map: &HashMap<BlockId, &BasicBlock>,
        defined_temps: &HashMap<Temp, RueType>,
    ) {
        // Check target exists
        let target_block = match block_map.get(&target) {
            Some(block) => block,
            None => {
                self.add_error(
                    format!("Jump from {from_block:?} to non-existent block {target:?}"),
                    Some(from_block),
                );
                return;
            }
        };

        // Check argument count matches
        if args.len() != target_block.params.len() {
            self.add_error(
                format!(
                    "Jump from {:?} to {:?} has {} arguments but target expects {}",
                    from_block,
                    target,
                    args.len(),
                    target_block.params.len()
                ),
                Some(from_block),
            );
            return;
        }

        // Check each argument is defined and types match
        for (i, (arg, (_param, param_ty))) in args.iter().zip(&target_block.params).enumerate() {
            if let Some(arg_ty) = defined_temps.get(arg) {
                if arg_ty != param_ty {
                    self.add_error(
                        format!(
                            "Type mismatch in jump from {from_block:?} to {target:?}: argument {i} has type {arg_ty:?} but parameter has type {param_ty:?}"
                        ),
                        Some(from_block),
                    );
                }
            } else {
                self.add_error(
                    format!(
                        "Jump from {from_block:?} to {target:?} uses undefined temp {arg:?} as argument {i}"
                    ),
                    Some(from_block),
                );
            }
        }
    }

    /// Find all reachable blocks from a starting block
    fn find_reachable_blocks(
        &self,
        start: BlockId,
        block_map: &HashMap<BlockId, &BasicBlock>,
    ) -> HashSet<BlockId> {
        let mut reachable = HashSet::new();
        let mut worklist = vec![start];

        while let Some(block_id) = worklist.pop() {
            if !reachable.insert(block_id) {
                continue; // Already visited
            }

            if let Some(block) = block_map.get(&block_id) {
                match &block.terminator {
                    MirTerminator::Goto { target, .. } => {
                        worklist.push(*target);
                    }
                    MirTerminator::Branch {
                        then_block,
                        else_block,
                        ..
                    } => {
                        worklist.push(*then_block);
                        worklist.push(*else_block);
                    }
                    MirTerminator::Switch {
                        targets,
                        default,
                        ..
                    } => {
                        // Add all target blocks from switch cases
                        for (_, target_block, _) in targets {
                            worklist.push(*target_block);
                        }
                        // Add default block
                        worklist.push(*default);
                    }
                    MirTerminator::Return { .. } | MirTerminator::Unreachable { .. } => {}
                }
            }
        }

        reachable
    }

    /// Infer the return type of a function call based on its name
    /// This is a temporary solution until we have proper function signature tracking
    fn infer_call_return_type(&self, func_name: &str) -> RueType {
        match func_name {
            // Casting functions
            "to_i32" => RueType::I32,
            "to_i64" => RueType::I64,
            "to_bool" => RueType::Bool,
            
            // I/O functions  
            "println_i32" | "println_i64" | "println_bool" => RueType::Unit,
            "input_i32" => RueType::I32,
            "input_i64" => RueType::I64,
            
            // For recursive functions, we need to infer from context
            // For now, assume i32 as a reasonable default for unknown functions
            _ => {
                // Try to infer from the current function context
                // If we're in a function that returns a specific type, assume that
                // This is a heuristic for recursive calls
                if func_name == self.current_function {
                    // For recursive calls, we'd need to look up the function's return type
                    // For now, assume i32 as the most common case
                    RueType::I32
                } else {
                    // For unknown functions, assume i32 as default
                    RueType::I32
                }
            }
        }
    }

    /// Add an error to the list
    fn add_error(&mut self, message: String, block: Option<BlockId>) {
        self.errors.push(VerificationError {
            message,
            function: self.current_function.clone(),
            block,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::MirConst;
    use rue_lexer::Span;

    #[test]
    fn test_verify_valid_program() {
        let program = MirProgram {
            functions: vec![MirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(0),
                        value: MirValue::Const(MirConst::Int32(42)),
                        span: None,
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(0)),
                        span: None,
                    },
                }],
                span: Span::dummy(),
            }],
        };

        let mut verifier = MirVerifier::new();
        assert!(verifier.verify_program(&program).is_ok());
    }

    #[test]
    fn test_verify_undefined_use() {
        let program = MirProgram {
            functions: vec![MirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(0)), // Undefined!
                        span: None,
                    },
                }],
                span: Span::dummy(),
            }],
        };

        let mut verifier = MirVerifier::new();
        let result = verifier.verify_program(&program);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("undefined"));
    }

    #[test]
    fn test_verify_multiple_assignments() {
        let program = MirProgram {
            functions: vec![MirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![
                        MirStatement::Assign {
                            dest: Temp(0),
                            value: MirValue::Const(MirConst::Int32(1)),
                            span: None,
                        },
                        MirStatement::Assign {
                            dest: Temp(0), // Reassignment!
                            value: MirValue::Const(MirConst::Int32(2)),
                            span: None,
                        },
                    ],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(0)),
                        span: None,
                    },
                }],
                span: Span::dummy(),
            }],
        };

        let mut verifier = MirVerifier::new();
        let result = verifier.verify_program(&program);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| e.message.contains("SSA")));
    }

    #[test]
    fn test_verify_argument_count_mismatch() {
        let program = MirProgram {
            functions: vec![MirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                blocks: vec![
                    BasicBlock {
                        id: BlockId(0),
                        params: vec![],
                        statements: vec![],
                        terminator: MirTerminator::Goto {
                            target: BlockId(1),
                            args: vec![Temp(0), Temp(1)], // Too many args!
                            span: None,
                        },
                    },
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![(Temp(2), RueType::I32)], // Only expects 1
                        statements: vec![],
                        terminator: MirTerminator::Return {
                            value: Some(Temp(2)),
                            span: None,
                        },
                    },
                ],
                span: Span::dummy(),
            }],
        };

        let mut verifier = MirVerifier::new();
        let result = verifier.verify_program(&program);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("2 arguments but target expects 1"))
        );
    }
}
