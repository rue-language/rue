//! Constant propagation pass for MIR
//!
//! This optimization pass evaluates constant expressions at compile time
//! and replaces them with their computed values.

use crate::pass_manager::{Pass, PassResult};
use crate::visitor::{MirMutVisitor, VisitorControl};
use rue_ir::mir::{
    MirBinOp, MirConst, MirFunction, MirProgram, MirStatement, MirTerminator, MirUnaryOp, MirValue,
    Temp,
};
use std::collections::HashMap;
use std::time::Instant;

/// Constant propagation pass
pub struct ConstProp {
    /// Known constant values for temporaries
    constants: HashMap<Temp, MirConst>,
    /// Number of transformations made in the current run
    transformations: u32,
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
            transformations: 0,
        }
    }

    /// Run constant propagation on a MIR program (legacy interface)
    pub fn run(&mut self, program: &mut MirProgram) {
        self.transformations = 0;
        for func in &mut program.functions {
            self.optimize_function(func);
        }
    }

    /// Optimize a single function
    fn optimize_function(&mut self, func: &mut MirFunction) {
        // The visitor pattern requires starting from the top
        // visit_function will handle clearing state and traversing the function
        let _ = self.visit_function(func);
    }

    /// Propagate constants in a value (replace temp uses with known constants)
    fn propagate_constants_in_value(&self, value: &mut MirValue) {
        match value {
            MirValue::Use(temp) => {
                if let Some(const_val) = self.constants.get(temp) {
                    *value = MirValue::Const(const_val.clone());
                }
            }
            MirValue::BinaryOp { .. } => {
                // No need to modify lhs/rhs as they are temps, not values
                // The evaluation will handle looking them up
            }
            MirValue::UnaryOp { .. } => {
                // Same as above
            }
            _ => {}
        }
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

                // Fold the binary operation
                Self::fold_binop(*op, lhs_const, rhs_const)
            }
            MirValue::UnaryOp { op, operand } => {
                // Get constant value for operand
                let operand_const = self.constants.get(operand)?;

                // Fold the unary operation
                Self::fold_unop(*op, operand_const)
            }
            MirValue::Call { .. } => None, // Calls cannot be constant-folded
            // Aggregate operations cannot be constant-folded yet
            MirValue::ConstructAggregate { .. } => None,
            MirValue::GetField { .. } => None,
            MirValue::SetField { .. } => None,
            MirValue::StructUpdate { .. } => None,
            MirValue::DynamicArrayAccess { .. } => None, // Dynamic array access cannot be constant-folded
        }
    }

    /// Optimize a terminator
    fn optimize_terminator(&mut self, term: &mut MirTerminator) {
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
                self.transformations += 1;
            }
        }
    }

    /// Fold a binary operation on constants, returning None if the operation
    /// cannot be folded (e.g., division by zero, type mismatch)
    fn fold_binop(op: MirBinOp, lhs: &MirConst, rhs: &MirConst) -> Option<MirConst> {
        use MirBinOp::*;
        use MirConst::*;

        match (op, lhs, rhs) {
            // i32 arithmetic - uses wrapping semantics as per Rue spec
            (Add, Int32(a), Int32(b)) => Some(Int32(a.wrapping_add(*b))),
            (Sub, Int32(a), Int32(b)) => Some(Int32(a.wrapping_sub(*b))),
            (Mul, Int32(a), Int32(b)) => Some(Int32(a.wrapping_mul(*b))),
            (Div, Int32(a), Int32(b)) if *b != 0 => Some(Int32(a.wrapping_div(*b))),
            (Mod, Int32(a), Int32(b)) if *b != 0 => Some(Int32(a.wrapping_rem(*b))),

            // i64 arithmetic - uses wrapping semantics as per Rue spec
            (Add, Int64(a), Int64(b)) => Some(Int64(a.wrapping_add(*b))),
            (Sub, Int64(a), Int64(b)) => Some(Int64(a.wrapping_sub(*b))),
            (Mul, Int64(a), Int64(b)) => Some(Int64(a.wrapping_mul(*b))),
            (Div, Int64(a), Int64(b)) if *b != 0 => Some(Int64(a.wrapping_div(*b))),
            (Mod, Int64(a), Int64(b)) if *b != 0 => Some(Int64(a.wrapping_rem(*b))),

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

            // Type mismatches or unsupported operations return None
            _ => None,
        }
    }

    /// Fold a unary operation on a constant, returning None if the operation
    /// cannot be folded (e.g., type mismatch)
    fn fold_unop(op: MirUnaryOp, operand: &MirConst) -> Option<MirConst> {
        use MirConst::*;
        use MirUnaryOp::*;

        match (op, operand) {
            (Neg, Int32(n)) => Some(Int32(-n)),
            (Neg, Int64(n)) => Some(Int64(-n)),
            // Negation on bool or unit is not supported
            _ => None,
        }
    }
}

// Visitor implementation for constant propagation
impl MirMutVisitor for ConstProp {
    fn visit_function(&mut self, func: &mut MirFunction) -> VisitorControl {
        // Clear constants for new function
        self.constants.clear();

        // Let the default implementation handle traversal
        // by not calling this method from the trait
        crate::visitor::walk_function_mut(self, func)
    }

    fn visit_statement(&mut self, stmt: &mut MirStatement) -> VisitorControl {
        match stmt {
            MirStatement::Assign { dest, value, .. } => {
                // First, propagate constants in the value
                self.propagate_constants_in_value(value);

                // Then try to evaluate the value as a constant
                let new_value = self.evaluate_value(value);

                if let Some(const_val) = new_value {
                    // Record this temp as a constant
                    self.constants.insert(*dest, const_val.clone());

                    // Replace with constant value
                    *value = MirValue::Const(const_val);
                    self.transformations += 1;
                } else if let MirValue::Const(c) = value {
                    // If the value is already a constant, record it
                    self.constants.insert(*dest, c.clone());
                }
            }
            MirStatement::Return { value, .. } => {
                // Propagate constants in return value if present
                if let Some(temp) = value
                    && let Some(_const_val) = self.constants.get(temp)
                {
                    // Could optimize by replacing return temp with constant,
                    // but this doesn't affect correctness, just efficiency
                }
            }
        }
        VisitorControl::Continue
    }

    fn visit_value(&mut self, value: &mut MirValue) -> VisitorControl {
        self.propagate_constants_in_value(value);
        VisitorControl::Continue
    }

    fn visit_terminator(&mut self, term: &mut MirTerminator) -> VisitorControl {
        // First propagate constants in the terminator
        if let MirTerminator::Branch { condition, .. } = term
            && let Some(_const_val) = self.constants.get(condition)
        {
            // We know the condition value, but we need to let optimize_terminator handle the transformation
        }
        self.optimize_terminator(term);
        VisitorControl::Continue
    }
}

impl Pass for ConstProp {
    fn name(&self) -> &'static str {
        "const_prop"
    }

    fn run(&mut self, program: &mut MirProgram) -> PassResult {
        let start = Instant::now();
        self.transformations = 0;

        for func in &mut program.functions {
            self.optimize_function(func);
        }

        let duration = start.elapsed();
        let changed = self.transformations > 0;

        if changed {
            PassResult::changed(duration, self.transformations)
        } else {
            PassResult::no_change(duration)
        }
    }

    fn description(&self) -> &'static str {
        "Evaluates constant expressions at compile time and replaces them with computed values"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_ir::mir::{BasicBlock, BlockId, MirBinOp, MirStatement, Temp};
    use rue_ir::types::RueType;
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
                    ],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(2)),
                        span: None,
                    },
                }],
            }],
            function_signatures: HashMap::new(),
            type_context: rue_ir::types::TypeContext::new(),
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
                temp_types: HashMap::new(),
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
                        terminator: MirTerminator::Return {
                            value: None,
                            span: None,
                        },
                    },
                    BasicBlock {
                        id: BlockId(2),
                        params: vec![],
                        statements: vec![],
                        terminator: MirTerminator::Return {
                            value: None,
                            span: None,
                        },
                    },
                ],
            }],
            function_signatures: HashMap::new(),
            type_context: rue_ir::types::TypeContext::new(),
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
