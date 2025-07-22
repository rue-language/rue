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

                // Verify the value and get its type
                if let Some(value_ty) = self.verify_value(value, block_id, defined_temps) {
                    defined_temps.insert(*dest, value_ty);
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
            MirValue::BinaryOp { lhs, rhs, .. } => {
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

                // For now, assume binary ops preserve the type of operands
                lhs_ty.or(rhs_ty)
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
                return_type,
                args,
                func,
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
                Some(return_type.clone())
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
            MirTerminator::Goto { target, args } => {
                self.verify_jump(*target, args, block_id, block_map, defined_temps);
            }
            MirTerminator::Branch {
                condition,
                then_block,
                then_args,
                else_block,
                else_args,
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
            MirTerminator::Return { value } => {
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
                    MirTerminator::Return { .. } => {}
                }
            }
        }

        reachable
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
                        },
                    },
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![(Temp(2), RueType::I32)], // Only expects 1
                        statements: vec![],
                        terminator: MirTerminator::Return {
                            value: Some(Temp(2)),
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
