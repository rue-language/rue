//! Constant propagation pass for MIR
//!
//! This optimization pass evaluates constant expressions at compile time
//! and replaces them with their computed values.

use crate::mir::{
    BasicBlock, MirBinOp, MirConst, MirFunction, MirProgram, MirStatement, MirTerminator, MirValue,
    Temp,
};
use std::collections::HashMap;

/// Constant propagation pass
pub struct ConstProp {
    /// Known constant values for temporaries
    constants: HashMap<Temp, MirConst>,
}

impl Default for ConstProp {
    fn default() -> Self {
        Self::new()
    }
}

impl ConstProp {
    pub fn new() -> Self {
        Self {
            constants: HashMap::new(),
        }
    }

    /// Run constant propagation on a MIR program
    pub fn run(&mut self, program: &mut MirProgram) {
        for func in &mut program.functions {
            self.optimize_function(func);
        }
    }

    /// Optimize a single function
    fn optimize_function(&mut self, func: &mut MirFunction) {
        // Process each block
        for block in &mut func.blocks {
            self.optimize_block(block);
        }
    }

    /// Optimize a single block
    fn optimize_block(&mut self, block: &mut BasicBlock) {
        // Clear constants for new block (simplified - real impl would handle control flow)
        self.constants.clear();

        // Process statements
        let mut new_statements = Vec::new();

        for stmt in &block.statements {
            match stmt {
                MirStatement::Assign { dest, value, span } => {
                    // Try to evaluate the value as a constant
                    if let Some(const_val) = self.evaluate_value(value) {
                        // Record this temp as a constant
                        self.constants.insert(*dest, const_val.clone());

                        // Replace with constant assignment
                        new_statements.push(MirStatement::Assign {
                            dest: *dest,
                            value: MirValue::Const(const_val),
                            span: *span,
                        });
                    } else {
                        // Keep original statement
                        new_statements.push(stmt.clone());
                    }
                }
            }
        }

        block.statements = new_statements;

        // Optimize terminator
        self.optimize_terminator(&mut block.terminator);
    }

    /// Try to evaluate a value as a constant
    fn evaluate_value(&self, value: &MirValue) -> Option<MirConst> {
        match value {
            MirValue::Const(c) => Some(c.clone()),
            MirValue::Use(temp) => self.constants.get(temp).cloned(),
            MirValue::BinaryOp { op, lhs, rhs } => {
                // Get constant values for both operands
                let lhs_const = self.constants.get(lhs)?;
                let rhs_const = self.constants.get(rhs)?;

                // Evaluate the operation
                self.evaluate_binop(*op, lhs_const, rhs_const)
            }
            MirValue::UnaryOp { op, operand } => {
                let operand_const = self.constants.get(operand)?;

                match op {
                    crate::mir::MirUnaryOp::Neg => match operand_const {
                        MirConst::Int32(n) => Some(MirConst::Int32(-n)),
                        MirConst::Int64(n) => Some(MirConst::Int64(-n)),
                        _ => None,
                    },
                }
            }
            MirValue::Call { .. } => None, // Can't constant-fold function calls
        }
    }

    /// Evaluate a binary operation on constants
    fn evaluate_binop(&self, op: MirBinOp, lhs: &MirConst, rhs: &MirConst) -> Option<MirConst> {
        use MirBinOp::*;
        use MirConst::*;

        match (op, lhs, rhs) {
            // i32 arithmetic
            (Add, Int32(a), Int32(b)) => Some(Int32(a.wrapping_add(*b))),
            (Sub, Int32(a), Int32(b)) => Some(Int32(a.wrapping_sub(*b))),
            (Mul, Int32(a), Int32(b)) => Some(Int32(a.wrapping_mul(*b))),
            (Div, Int32(a), Int32(b)) if *b != 0 => Some(Int32(a / b)),
            (Mod, Int32(a), Int32(b)) if *b != 0 => Some(Int32(a % b)),

            // i64 arithmetic
            (Add, Int64(a), Int64(b)) => Some(Int64(a.wrapping_add(*b))),
            (Sub, Int64(a), Int64(b)) => Some(Int64(a.wrapping_sub(*b))),
            (Mul, Int64(a), Int64(b)) => Some(Int64(a.wrapping_mul(*b))),
            (Div, Int64(a), Int64(b)) if *b != 0 => Some(Int64(a / b)),
            (Mod, Int64(a), Int64(b)) if *b != 0 => Some(Int64(a % b)),

            // i32 comparisons
            (Lt, Int32(a), Int32(b)) => Some(Bool(a < b)),
            (Le, Int32(a), Int32(b)) => Some(Bool(a <= b)),
            (Gt, Int32(a), Int32(b)) => Some(Bool(a > b)),
            (Ge, Int32(a), Int32(b)) => Some(Bool(a >= b)),
            (Eq, Int32(a), Int32(b)) => Some(Bool(a == b)),
            (Ne, Int32(a), Int32(b)) => Some(Bool(a != b)),

            // i64 comparisons
            (Lt, Int64(a), Int64(b)) => Some(Bool(a < b)),
            (Le, Int64(a), Int64(b)) => Some(Bool(a <= b)),
            (Gt, Int64(a), Int64(b)) => Some(Bool(a > b)),
            (Ge, Int64(a), Int64(b)) => Some(Bool(a >= b)),
            (Eq, Int64(a), Int64(b)) => Some(Bool(a == b)),
            (Ne, Int64(a), Int64(b)) => Some(Bool(a != b)),

            // Bool comparisons
            (Eq, Bool(a), Bool(b)) => Some(Bool(a == b)),
            (Ne, Bool(a), Bool(b)) => Some(Bool(a != b)),

            _ => None, // Type mismatch or unsupported operation
        }
    }

    /// Optimize a terminator
    fn optimize_terminator(&self, term: &mut MirTerminator) {
        if let MirTerminator::Branch {
            condition,
            then_block,
            then_args,
            else_block,
            else_args,
            ..
        } = term
        {
            // If condition is a known constant, replace with goto
            if let Some(MirConst::Bool(val)) = self.constants.get(condition) {
                *term = if *val {
                    MirTerminator::Goto {
                        target: *then_block,
                        args: then_args.clone(),
                        span: None,
                    }
                } else {
                    MirTerminator::Goto {
                        target: *else_block,
                        args: else_args.clone(),
                        span: None,
                    }
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{BlockId, MirStatement, Temp};
    use crate::types::RueType;
    use rue_lexer::Span;

    #[test]
    fn test_const_prop_arithmetic() {
        // Create a simple MIR program:
        // t0 = 10
        // t1 = 20
        // t2 = t0 + t1  // Should be optimized to t2 = 30

        let mut program = MirProgram {
            functions: vec![MirFunction {
                name: "test".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                span: Span::dummy(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![
                        MirStatement::Assign {
                            dest: Temp(0),
                            value: MirValue::Const(MirConst::Int32(10)),
                            span: None,
                        },
                        MirStatement::Assign {
                            dest: Temp(1),
                            value: MirValue::Const(MirConst::Int32(20)),
                            span: None,
                        },
                        MirStatement::Assign {
                            dest: Temp(2),
                            value: MirValue::BinaryOp {
                                op: MirBinOp::Add,
                                lhs: Temp(0),
                                rhs: Temp(1),
                            },
                            span: None,
                        },
                    ],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(2)),
                        span: None,
                    },
                }],
            }],
        };

        // Run constant propagation
        let mut const_prop = ConstProp::new();
        const_prop.run(&mut program);

        // Check that the addition was optimized
        let block = &program.functions[0].blocks[0];
        match &block.statements[2] {
            MirStatement::Assign {
                value: MirValue::Const(MirConst::Int32(30)),
                ..
            } => {} // Success!
            _ => panic!("Expected constant 30"),
        }
    }

    #[test]
    fn test_const_prop_branch() {
        // Create MIR with constant branch:
        // t0 = true
        // branch t0, B1, B2  // Should be optimized to goto B1

        let mut program = MirProgram {
            functions: vec![MirFunction {
                name: "test".to_string(),
                params: vec![],
                return_type: RueType::Unit,
                entry_block: BlockId(0),
                span: Span::dummy(),
                blocks: vec![
                    BasicBlock {
                        id: BlockId(0),
                        params: vec![],
                        statements: vec![MirStatement::Assign {
                            dest: Temp(0),
                            value: MirValue::Const(MirConst::Bool(true)),
                            span: None,
                        }],
                        terminator: MirTerminator::Branch {
                            condition: Temp(0),
                            then_block: BlockId(1),
                            then_args: vec![],
                            else_block: BlockId(2),
                            else_args: vec![],
                            span: None,
                        },
                    },
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![],
                        statements: vec![],
                        terminator: MirTerminator::Return { value: None, span: None },
                    },
                    BasicBlock {
                        id: BlockId(2),
                        params: vec![],
                        statements: vec![],
                        terminator: MirTerminator::Return { value: None, span: None },
                    },
                ],
            }],
        };

        // Run constant propagation
        let mut const_prop = ConstProp::new();
        const_prop.run(&mut program);

        // Check that branch was optimized to goto
        let block = &program.functions[0].blocks[0];
        match &block.terminator {
            MirTerminator::Goto { target, .. } if *target == BlockId(1) => {} // Success!
            _ => panic!("Expected goto B1"),
        }
    }
}
