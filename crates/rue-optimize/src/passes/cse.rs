//! Common Subexpression Elimination (CSE) pass for MIR
//!
//! This optimization pass identifies duplicate computations and replaces them
//! with uses of previously computed values. This reduces redundant calculations
//! and can improve performance.

use rue_ir::mir::{
    BasicBlock, MirBinOp, MirConst, MirFunction, MirProgram, MirStatement, MirUnaryOp, MirValue,
    Temp,
};
use std::collections::HashMap;

/// Represents an expression that can be compared for equality
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Expression {
    Const(MirConst),
    BinaryOp { op: MirBinOp, lhs: Temp, rhs: Temp },
    UnaryOp { op: MirUnaryOp, operand: Temp },
}

/// Common Subexpression Elimination pass
pub struct CommonSubexpressionElimination {
    /// Map from expressions to the temporaries that compute them
    available_exprs: HashMap<Expression, Temp>,
    /// Map from temporaries to the expressions they compute
    temp_to_expr: HashMap<Temp, Expression>,
}

impl Default for CommonSubexpressionElimination {
    fn default() -> Self {
        Self::new()
    }
}

impl CommonSubexpressionElimination {
    pub fn new() -> Self {
        Self {
            available_exprs: HashMap::new(),
            temp_to_expr: HashMap::new(),
        }
    }

    /// Run CSE on a MIR program
    pub fn run(&mut self, program: &mut MirProgram) {
        for func in &mut program.functions {
            self.optimize_function(func);
        }
    }

    /// Optimize a single function
    fn optimize_function(&mut self, func: &mut MirFunction) {
        // Process each block independently (local CSE)
        // A more sophisticated implementation would do global CSE
        for block in &mut func.blocks {
            self.optimize_block(block);
        }
    }

    /// Optimize a single block
    fn optimize_block(&mut self, block: &mut BasicBlock) {
        // Clear state for new block
        self.available_exprs.clear();
        self.temp_to_expr.clear();

        let mut new_statements = Vec::new();

        for stmt in &block.statements {
            match stmt {
                MirStatement::Assign { dest, value, span } => {
                    // Try to convert the value to an expression
                    if let Some(expr) = self.value_to_expression(value) {
                        // Check if we've already computed this expression
                        if let Some(&existing_temp) = self.available_exprs.get(&expr) {
                            // Replace with use of existing temp
                            new_statements.push(MirStatement::Assign {
                                dest: *dest,
                                value: MirValue::Use(existing_temp),
                                span: *span,
                            });
                        } else {
                            // This is a new expression, record it
                            self.available_exprs.insert(expr.clone(), *dest);
                            self.temp_to_expr.insert(*dest, expr);
                            new_statements.push(stmt.clone());
                        }
                    } else {
                        // Not an expression we can eliminate (e.g., function call)
                        new_statements.push(stmt.clone());

                        // If this assigns to a temp that was computing an expression,
                        // that expression is no longer available
                        if let Some(old_expr) = self.temp_to_expr.remove(dest) {
                            self.available_exprs.remove(&old_expr);
                        }
                    }
                }
            }
        }

        block.statements = new_statements;
    }

    /// Convert a MIR value to an expression if possible
    fn value_to_expression(&self, value: &MirValue) -> Option<Expression> {
        match value {
            MirValue::Const(c) => Some(Expression::Const(*c)),
            MirValue::BinaryOp { op, lhs, rhs } => {
                // For commutative operations, normalize the order
                let (lhs, rhs) = if self.is_commutative(*op) && lhs > rhs {
                    (*rhs, *lhs)
                } else {
                    (*lhs, *rhs)
                };

                Some(Expression::BinaryOp { op: *op, lhs, rhs })
            }
            MirValue::UnaryOp { op, operand } => Some(Expression::UnaryOp {
                op: *op,
                operand: *operand,
            }),
            MirValue::Use(_) => None, // Simple uses aren't expressions we eliminate
            MirValue::Call { .. } => None, // Function calls have side effects
        }
    }

    /// Check if a binary operation is commutative
    fn is_commutative(&self, op: MirBinOp) -> bool {
        use MirBinOp::*;
        matches!(op, Add | Mul | Eq | Ne)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_ir::mir::{BlockId, MirStatement, MirTerminator, Temp};
    use rue_ir::types::RueType;
    use rue_lexer::Span;

    #[test]
    fn test_cse_basic() {
        // Create a MIR program with duplicate computations:
        // t0 = 10
        // t1 = 20
        // t2 = t0 + t1
        // t3 = t0 + t1  // Should be replaced with t3 = t2
        // return t3

        let mut program = MirProgram {
            functions: vec![MirFunction {
                name: "test".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                span: Span::dummy(),
                temp_types: HashMap::new(),
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
                        MirStatement::Assign {
                            dest: Temp(3),
                            value: MirValue::BinaryOp {
                                op: MirBinOp::Add,
                                lhs: Temp(0),
                                rhs: Temp(1),
                            },
                            span: None,
                        },
                    ],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(3)),
                        span: None,
                    },
                }],
            }],
        };

        // Run CSE
        let mut cse = CommonSubexpressionElimination::new();
        cse.run(&mut program);

        // Check that the duplicate computation was eliminated
        let block = &program.functions[0].blocks[0];
        match &block.statements[3] {
            MirStatement::Assign {
                value: MirValue::Use(Temp(2)),
                ..
            } => {} // Success! t3 = t2
            _ => panic!("Expected t3 to be assigned t2"),
        }
    }

    #[test]
    fn test_cse_commutative() {
        // Test that commutative operations are recognized as equivalent:
        // t0 = 10
        // t1 = 20
        // t2 = t0 + t1
        // t3 = t1 + t0  // Should be replaced with t3 = t2 (commutative)

        let mut program = MirProgram {
            functions: vec![MirFunction {
                name: "test".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                span: Span::dummy(),
                temp_types: HashMap::new(),
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
                        MirStatement::Assign {
                            dest: Temp(3),
                            value: MirValue::BinaryOp {
                                op: MirBinOp::Add,
                                lhs: Temp(1), // Swapped order
                                rhs: Temp(0),
                            },
                            span: None,
                        },
                    ],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(3)),
                        span: None,
                    },
                }],
            }],
        };

        // Run CSE
        let mut cse = CommonSubexpressionElimination::new();
        cse.run(&mut program);

        // Check that the commutative operation was recognized
        let block = &program.functions[0].blocks[0];
        match &block.statements[3] {
            MirStatement::Assign {
                value: MirValue::Use(Temp(2)),
                ..
            } => {} // Success! t3 = t2
            _ => panic!("Expected t3 to be assigned t2"),
        }
    }

    #[test]
    fn test_cse_const_dedup() {
        // Test that duplicate constants are eliminated:
        // t0 = 42
        // t1 = 42  // Should be replaced with t1 = t0

        let mut program = MirProgram {
            functions: vec![MirFunction {
                name: "test".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                span: Span::dummy(),
                temp_types: HashMap::new(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![
                        MirStatement::Assign {
                            dest: Temp(0),
                            value: MirValue::Const(MirConst::Int32(42)),
                            span: None,
                        },
                        MirStatement::Assign {
                            dest: Temp(1),
                            value: MirValue::Const(MirConst::Int32(42)),
                            span: None,
                        },
                    ],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(1)),
                        span: None,
                    },
                }],
            }],
        };

        // Run CSE
        let mut cse = CommonSubexpressionElimination::new();
        cse.run(&mut program);

        // Check that the duplicate constant was eliminated
        let block = &program.functions[0].blocks[0];
        match &block.statements[1] {
            MirStatement::Assign {
                value: MirValue::Use(Temp(0)),
                ..
            } => {} // Success! t1 = t0
            _ => panic!("Expected t1 to be assigned t0"),
        }
    }
}
