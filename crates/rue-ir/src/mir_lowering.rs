//! HIR to MIR lowering
//!
//! This module converts HIR (High-level IR) to MIR (Mid-level IR) in SSA form.
//! The lowering process:
//! - Converts control flow to basic blocks
//! - Introduces temporaries for all intermediate values
//! - Uses block parameters instead of phi nodes for control flow joins

use crate::hir::{
    BinOp as HirBinOp, HirBlock, HirExpr, HirFunction, HirLiteral, HirProgram, HirStatement,
    UnaryOp as HirUnaryOp,
};
use crate::mir::{
    BasicBlock, BlockId, MirBinOp, MirConst, MirFunction, MirProgram, MirStatement, MirTerminator,
    MirUnaryOp, MirValue, Temp,
};
use crate::types::RueType;
use std::collections::HashMap;

/// Builder for constructing MIR from HIR
pub struct MirBuilder {
    /// Counter for generating unique temporaries
    temp_counter: u32,
    /// Counter for generating unique blocks
    block_counter: u32,
    /// Current basic block being built
    current_block: Option<BasicBlock>,
    /// All completed blocks
    blocks: Vec<BasicBlock>,
    /// Mapping from variable names to their current temporary
    variables: HashMap<String, Temp>,
    /// Type information for temporaries
    temp_types: HashMap<Temp, RueType>,
}

impl Default for MirBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MirBuilder {
    /// Create a new MIR builder
    pub fn new() -> Self {
        Self {
            temp_counter: 0,
            block_counter: 0,
            current_block: None,
            blocks: Vec::new(),
            variables: HashMap::new(),
            temp_types: HashMap::new(),
        }
    }

    /// Generate a fresh temporary
    fn fresh_temp(&mut self, ty: RueType) -> Temp {
        let temp = Temp(self.temp_counter);
        self.temp_counter += 1;
        self.temp_types.insert(temp, ty);
        temp
    }

    /// Generate a fresh block ID
    fn fresh_block(&mut self) -> BlockId {
        let id = BlockId(self.block_counter);
        self.block_counter += 1;
        id
    }

    /// Start a new basic block
    fn start_block(&mut self, id: BlockId, params: Vec<(Temp, RueType)>) {
        if let Some(block) = self.current_block.take() {
            self.blocks.push(block);
        }
        self.current_block = Some(BasicBlock {
            id,
            params,
            statements: Vec::new(),
            terminator: MirTerminator::Return { value: None }, // Placeholder
        });
    }

    /// Add a statement to the current block
    fn add_statement(&mut self, stmt: MirStatement) {
        if let Some(block) = &mut self.current_block {
            block.statements.push(stmt);
        }
    }

    /// Set the terminator for the current block
    fn set_terminator(&mut self, term: MirTerminator) {
        if let Some(block) = &mut self.current_block {
            block.terminator = term;
        }
    }

    /// Finish the current block
    fn finish_block(&mut self) {
        if let Some(block) = self.current_block.take() {
            self.blocks.push(block);
        }
    }

    /// Lower a HIR program to MIR
    pub fn lower_program(hir: &HirProgram) -> MirProgram {
        let mut functions = Vec::new();

        for hir_func in &hir.functions {
            let mir_func = Self::lower_function(hir_func);
            functions.push(mir_func);
        }

        MirProgram { functions }
    }

    /// Lower a HIR function to MIR
    pub fn lower_function(func: &HirFunction) -> MirFunction {
        let mut builder = MirBuilder::new();

        // Create entry block with function parameters as block parameters
        let entry_block = builder.fresh_block();
        let mut block_params = Vec::new();

        // Map function parameters to temporaries
        for (name, ty) in &func.params {
            let temp = builder.fresh_temp(ty.clone());
            builder.variables.insert(name.clone(), temp);
            block_params.push((temp, ty.clone()));
        }

        builder.start_block(entry_block, block_params);

        // Lower the function body
        let result = builder.lower_block(&func.body);

        // Set return terminator
        builder.set_terminator(MirTerminator::Return { value: result });
        builder.finish_block();

        MirFunction {
            name: func.name.clone(),
            params: func.params.clone(),
            return_type: func.return_type.clone(),
            blocks: builder.blocks,
            entry_block,
            span: func.span,
        }
    }

    /// Lower a HIR block
    fn lower_block(&mut self, block: &HirBlock) -> Option<Temp> {
        // Lower all statements
        for stmt in &block.statements {
            self.lower_statement(stmt);
        }

        // Lower the final expression if present
        block.expr.as_ref().map(|expr| self.lower_expr(expr))
    }

    /// Lower a HIR statement
    fn lower_statement(&mut self, stmt: &HirStatement) {
        match stmt {
            HirStatement::Let {
                name,
                ty: _,
                init,
                span: _,
            } => {
                let init_temp = self.lower_expr(init);
                self.variables.insert(name.clone(), init_temp);
            }
            HirStatement::Assign {
                name,
                value,
                span: _,
            } => {
                let value_temp = self.lower_expr(value);
                // In SSA form, we create a new temp for the assignment
                self.variables.insert(name.clone(), value_temp);
            }
            HirStatement::Expr(expr) => {
                // Expression statement - evaluate for side effects
                self.lower_expr(expr);
            }
        }
    }

    /// Lower a HIR expression to MIR, returning the temporary holding the result
    fn lower_expr(&mut self, expr: &HirExpr) -> Temp {
        match expr {
            HirExpr::Literal { lit, span } => {
                let const_val = match lit {
                    HirLiteral::Int32(n) => MirConst::Int32(*n),
                    HirLiteral::Int64(n) => MirConst::Int64(*n),
                    HirLiteral::Bool(b) => MirConst::Bool(*b),
                    HirLiteral::Unit => MirConst::Unit,
                };
                let ty = const_val.ty();
                let temp = self.fresh_temp(ty);
                self.add_statement(MirStatement::Assign {
                    dest: temp,
                    value: MirValue::Const(const_val),
                    span: Some(*span),
                });
                temp
            }
            HirExpr::Var {
                name,
                ty: _,
                span: _,
            } => {
                // Look up the current temporary for this variable
                self.variables
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| panic!("Undefined variable: {name}"))
            }
            HirExpr::Binary {
                op,
                lhs,
                rhs,
                ty,
                span,
            } => {
                let lhs_temp = self.lower_expr(lhs);
                let rhs_temp = self.lower_expr(rhs);
                let result_temp = self.fresh_temp(ty.clone());

                let mir_op = match op {
                    HirBinOp::Add => MirBinOp::Add,
                    HirBinOp::Sub => MirBinOp::Sub,
                    HirBinOp::Mul => MirBinOp::Mul,
                    HirBinOp::Div => MirBinOp::Div,
                    HirBinOp::Mod => MirBinOp::Mod,
                    HirBinOp::Lt => MirBinOp::Lt,
                    HirBinOp::Le => MirBinOp::Le,
                    HirBinOp::Gt => MirBinOp::Gt,
                    HirBinOp::Ge => MirBinOp::Ge,
                    HirBinOp::Eq => MirBinOp::Eq,
                    HirBinOp::Ne => MirBinOp::Ne,
                };

                self.add_statement(MirStatement::Assign {
                    dest: result_temp,
                    value: MirValue::BinaryOp {
                        op: mir_op,
                        lhs: lhs_temp,
                        rhs: rhs_temp,
                    },
                    span: Some(*span),
                });
                result_temp
            }
            HirExpr::Unary { op, expr, ty, span } => {
                let operand_temp = self.lower_expr(expr);
                let result_temp = self.fresh_temp(ty.clone());

                let mir_op = match op {
                    HirUnaryOp::Neg => MirUnaryOp::Neg,
                };

                self.add_statement(MirStatement::Assign {
                    dest: result_temp,
                    value: MirValue::UnaryOp {
                        op: mir_op,
                        operand: operand_temp,
                    },
                    span: Some(*span),
                });
                result_temp
            }
            HirExpr::Call {
                func,
                args,
                ty,
                span,
            } => {
                let arg_temps: Vec<Temp> = args.iter().map(|arg| self.lower_expr(arg)).collect();
                let result_temp = self.fresh_temp(ty.clone());

                self.add_statement(MirStatement::Assign {
                    dest: result_temp,
                    value: MirValue::Call {
                        func: func.clone(),
                        args: arg_temps,
                        return_type: ty.clone(),
                    },
                    span: Some(*span),
                });
                result_temp
            }
            HirExpr::If {
                cond,
                then_block,
                else_block,
                ty,
                span: _,
            } => self.lower_if_expr(cond, then_block, else_block.as_ref(), ty),
            HirExpr::While { cond, body, span } => {
                self.lower_while_expr(cond, body);
                // While always returns unit
                let unit_temp = self.fresh_temp(RueType::Unit);
                self.add_statement(MirStatement::Assign {
                    dest: unit_temp,
                    value: MirValue::Const(MirConst::Unit),
                    span: Some(*span),
                });
                unit_temp
            }
        }
    }

    /// Lower an if expression to MIR
    fn lower_if_expr(
        &mut self,
        cond: &HirExpr,
        then_block: &HirBlock,
        else_block: Option<&HirBlock>,
        result_ty: &RueType,
    ) -> Temp {
        // Evaluate condition
        let cond_temp = self.lower_expr(cond);

        // Save current variable state before branching
        // Sort variable names to ensure deterministic parameter ordering
        let mut var_names: Vec<String> = self.variables.keys().cloned().collect();
        var_names.sort_unstable();

        let vars_before_branch: Vec<(String, Temp)> = var_names
            .iter()
            .map(|name| (name.clone(), self.variables[name]))
            .collect();

        // Create blocks
        let then_block_id = self.fresh_block();
        let else_block_id = self.fresh_block();
        let join_block_id = self.fresh_block();

        // Create result temporary for join block
        let result_temp = self.fresh_temp(result_ty.clone());

        // Branch to then/else blocks
        self.set_terminator(MirTerminator::Branch {
            condition: cond_temp,
            then_block: then_block_id,
            then_args: vec![],
            else_block: else_block_id,
            else_args: vec![],
        });
        self.finish_block();

        // Lower then block
        self.start_block(then_block_id, vec![]);
        let then_value = self.lower_block(then_block);
        let then_result = then_value.unwrap_or_else(|| {
            // If no expression, use unit
            let temp = self.fresh_temp(RueType::Unit);
            self.add_statement(MirStatement::Assign {
                dest: temp,
                value: MirValue::Const(MirConst::Unit),
                span: None,
            });
            temp
        });

        // Collect variables after then block
        let vars_after_then = self.variables.clone();

        // Prepare arguments for join block from then branch
        let mut then_join_args = vec![then_result];
        for (name, _) in &vars_before_branch {
            if let Some(&temp) = vars_after_then.get(name) {
                then_join_args.push(temp);
            }
        }

        self.set_terminator(MirTerminator::Goto {
            target: join_block_id,
            args: then_join_args,
        });
        self.finish_block();

        // Reset variables to state before branching for else block
        self.variables = vars_before_branch.iter().cloned().collect();

        // Lower else block
        self.start_block(else_block_id, vec![]);
        let else_result = if let Some(else_blk) = else_block {
            let else_value = self.lower_block(else_blk);
            else_value.unwrap_or_else(|| {
                let temp = self.fresh_temp(RueType::Unit);
                self.add_statement(MirStatement::Assign {
                    dest: temp,
                    value: MirValue::Const(MirConst::Unit),
                    span: None,
                });
                temp
            })
        } else {
            // No else block - return unit
            let temp = self.fresh_temp(RueType::Unit);
            self.add_statement(MirStatement::Assign {
                dest: temp,
                value: MirValue::Const(MirConst::Unit),
                span: None,
            });
            temp
        };

        // Collect variables after else block
        let vars_after_else = self.variables.clone();

        // Prepare arguments for join block from else branch
        let mut else_join_args = vec![else_result];
        for (name, _) in &vars_before_branch {
            if let Some(&temp) = vars_after_else.get(name) {
                else_join_args.push(temp);
            }
        }

        self.set_terminator(MirTerminator::Goto {
            target: join_block_id,
            args: else_join_args,
        });
        self.finish_block();

        // Join block receives the result and all potentially modified variables as parameters
        // IMPORTANT: The result is always the first parameter. This ordering is critical
        // because we might have a variable with the same name as the result, and we need
        // to ensure the result temp is not shadowed by variable temps.
        let mut join_params = vec![(result_temp, result_ty.clone())];

        // Create a new temp for each variable that might have been modified
        let mut join_param_vars = Vec::new();
        for (name, original_temp) in &vars_before_branch {
            // Get the type from any of the branches (they should be the same)
            let ty = self
                .temp_types
                .get(original_temp)
                .cloned()
                .expect("type for temp not registered in temp_types");
            let join_temp = self.fresh_temp(ty.clone());
            join_params.push((join_temp, ty));
            join_param_vars.push((name.clone(), join_temp));
        }

        self.start_block(join_block_id, join_params);

        // Update variable mappings to use the join block parameters
        // The first parameter is the result, subsequent parameters are the variables
        for (name, param_temp) in join_param_vars {
            self.variables.insert(name, param_temp);
        }

        result_temp
    }

    /// Lower a while expression to MIR
    fn lower_while_expr(&mut self, cond: &HirExpr, body: &HirBlock) {
        // Save variable state before loop
        // Sort variable names to ensure deterministic parameter ordering
        let mut var_names: Vec<String> = self.variables.keys().cloned().collect();
        var_names.sort_unstable();

        let vars_before_loop: Vec<(String, Temp)> = var_names
            .iter()
            .map(|name| (name.clone(), self.variables[name]))
            .collect();

        // Create blocks
        let loop_header = self.fresh_block();
        let loop_body = self.fresh_block();
        let loop_exit = self.fresh_block();

        // Prepare initial arguments for loop header (current variable values)
        let mut initial_args = Vec::new();
        for (_, temp) in &vars_before_loop {
            initial_args.push(*temp);
        }

        // Jump to loop header with current variables
        self.set_terminator(MirTerminator::Goto {
            target: loop_header,
            args: initial_args,
        });
        self.finish_block();

        // Create loop header parameters for all variables
        let mut loop_header_params = Vec::new();
        let mut loop_vars = HashMap::new();

        for (name, temp) in &vars_before_loop {
            let temp_type = self
                .temp_types
                .get(temp)
                .cloned()
                .expect("type for temp not registered in temp_types");
            let header_temp = self.fresh_temp(temp_type.clone());
            loop_header_params.push((header_temp, temp_type));
            loop_vars.insert(name.clone(), header_temp);
        }

        // Loop header - evaluate condition with loop-carried variables
        self.start_block(loop_header, loop_header_params);

        // Update variables to use loop header parameters
        self.variables = loop_vars.clone();

        let cond_temp = self.lower_expr(cond);

        // Prepare arguments for both branches (pass current variables through)
        let mut branch_args = Vec::new();
        for (name, _) in &vars_before_loop {
            if let Some(&temp) = self.variables.get(name) {
                branch_args.push(temp);
            }
        }

        self.set_terminator(MirTerminator::Branch {
            condition: cond_temp,
            then_block: loop_body,
            then_args: vec![],
            else_block: loop_exit,
            else_args: branch_args.clone(),
        });
        self.finish_block();

        // Loop body
        self.start_block(loop_body, vec![]);
        self.lower_block(body);

        // Collect variables after loop body to pass back to header
        let mut loop_back_args = Vec::new();
        for (name, _) in &vars_before_loop {
            if let Some(&temp) = self.variables.get(name) {
                loop_back_args.push(temp);
            }
        }

        self.set_terminator(MirTerminator::Goto {
            target: loop_header,
            args: loop_back_args,
        });
        self.finish_block();

        // Loop exit - receives variables from loop header
        let mut exit_params = Vec::new();
        let mut exit_vars = HashMap::new();

        for (name, temp) in &vars_before_loop {
            let temp_type = self
                .temp_types
                .get(temp)
                .cloned()
                .expect("type for temp not registered in temp_types");
            let exit_temp = self.fresh_temp(temp_type.clone());
            exit_params.push((exit_temp, temp_type));
            exit_vars.insert(name.clone(), exit_temp);
        }

        self.start_block(loop_exit, exit_params);

        // Update variables to use exit block parameters
        self.variables = exit_vars;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::*;
    use rue_lexer::Span;

    #[test]
    fn test_lower_simple_function() {
        // fn add(a: i32, b: i32) -> i32 { a + b }
        let hir_func = HirFunction {
            name: "add".to_string(),
            params: vec![
                ("a".to_string(), RueType::I32),
                ("b".to_string(), RueType::I32),
            ],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(HirExpr::Var {
                        name: "a".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    rhs: Box::new(HirExpr::Var {
                        name: "b".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    ty: RueType::I32,
                    span: Span::dummy(),
                })),
            },
            span: Span::dummy(),
        };

        let mir_func = MirBuilder::lower_function(&hir_func);

        // Check structure
        assert_eq!(mir_func.name, "add");
        assert_eq!(mir_func.params.len(), 2);
        assert_eq!(mir_func.return_type, RueType::I32);
        assert_eq!(mir_func.blocks.len(), 1);

        // Check entry block
        let entry_block = &mir_func.blocks[0];
        assert_eq!(entry_block.params.len(), 2);
        assert_eq!(entry_block.statements.len(), 1);

        // Check addition
        match &entry_block.statements[0] {
            MirStatement::Assign {
                value: MirValue::BinaryOp { op, .. },
                ..
            } => {
                assert_eq!(*op, MirBinOp::Add);
            }
            _ => panic!("Expected binary operation"),
        }
    }

    #[test]
    fn test_lower_if_expression() {
        // if x > 0 { 1 } else { 0 }
        let hir_expr = HirExpr::If {
            cond: Box::new(HirExpr::Binary {
                op: BinOp::Gt,
                lhs: Box::new(HirExpr::Var {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    span: Span::dummy(),
                }),
                rhs: Box::new(HirExpr::Literal {
                    lit: HirLiteral::Int32(0),
                    span: Span::dummy(),
                }),
                ty: RueType::Bool,
                span: Span::dummy(),
            }),
            then_block: HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Literal {
                    lit: HirLiteral::Int32(1),
                    span: Span::dummy(),
                })),
            },
            else_block: Some(HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Literal {
                    lit: HirLiteral::Int32(0),
                    span: Span::dummy(),
                })),
            }),
            ty: RueType::I32,
            span: Span::dummy(),
        };

        let hir_func = HirFunction {
            name: "test".to_string(),
            params: vec![("x".to_string(), RueType::I32)],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![],
                expr: Some(Box::new(hir_expr)),
            },
            span: Span::dummy(),
        };

        let mir_func = MirBuilder::lower_function(&hir_func);

        // Should have 4 blocks: entry, then, else, join
        assert_eq!(mir_func.blocks.len(), 4);
    }
}
