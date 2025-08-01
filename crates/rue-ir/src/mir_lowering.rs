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
    BasicBlock, BlockId, FunctionSignature, MirBinOp, MirConst, MirFunction, MirProgram,
    MirStatement, MirTerminator, MirUnaryOp, MirValue, Temp,
};
use crate::types::RueType;
use std::collections::HashMap;
use tracing::{debug, trace};

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
    /// Whether we're lowering the top-level expression of a function
    /// (used to generate direct returns instead of join blocks)
    is_function_body: bool,
    /// Track variables that were assigned (not just declared) in the current scope
    /// This helps distinguish between assignment (x = value) and shadowing (let x = value)
    assigned_variables: std::collections::HashSet<String>,
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
            is_function_body: false,
            assigned_variables: std::collections::HashSet::new(),
        }
    }

    /// Generate a fresh temporary with type registration
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

    /// Create a new temporary and assign a constant value to it
    fn emit_const(&mut self, constant: MirConst, span: Option<rue_lexer::Span>) -> Temp {
        let ty = constant.ty();
        let temp = self.fresh_temp(ty);
        self.add_statement(MirStatement::Assign {
            dest: temp,
            value: MirValue::Const(constant),
            span,
        });
        temp
    }

    /// Create a new temporary and assign a binary operation to it
    fn emit_binop(
        &mut self,
        op: MirBinOp,
        lhs: Temp,
        rhs: Temp,
        result_ty: RueType,
        span: Option<rue_lexer::Span>,
    ) -> Temp {
        let temp = self.fresh_temp(result_ty);
        self.add_statement(MirStatement::Assign {
            dest: temp,
            value: MirValue::BinaryOp { op, lhs, rhs },
            span,
        });
        temp
    }

    /// Create a new temporary and assign a unary operation to it
    fn emit_unop(
        &mut self,
        op: MirUnaryOp,
        operand: Temp,
        result_ty: RueType,
        span: Option<rue_lexer::Span>,
    ) -> Temp {
        let temp = self.fresh_temp(result_ty);
        self.add_statement(MirStatement::Assign {
            dest: temp,
            value: MirValue::UnaryOp { op, operand },
            span,
        });
        temp
    }

    /// Create a new temporary and assign a function call to it
    fn emit_call(
        &mut self,
        func: String,
        args: Vec<Temp>,
        kind: crate::mir::CallKind,
        return_ty: RueType,
        span: Option<rue_lexer::Span>,
    ) -> Temp {
        let temp = self.fresh_temp(return_ty);
        self.add_statement(MirStatement::Assign {
            dest: temp,
            value: MirValue::Call { func, args, kind },
            span,
        });
        temp
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
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            }, // Placeholder
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
        let mut function_signatures = HashMap::new();

        // First pass: collect all function signatures (both user-defined and builtin)
        // Add builtin function signatures
        Self::add_builtin_function_signatures(&mut function_signatures);

        // Add user-defined function signatures
        for hir_func in &hir.functions {
            let sig = FunctionSignature {
                param_types: hir_func.params.iter().map(|(_, ty)| ty.clone()).collect(),
                return_type: hir_func.return_type.clone(),
            };
            function_signatures.insert(hir_func.name.clone(), sig);
        }

        // Second pass: lower functions to MIR
        for hir_func in &hir.functions {
            let mir_func = Self::lower_function(hir_func);
            functions.push(mir_func);
        }

        MirProgram {
            functions,
            function_signatures,
        }
    }

    /// Add builtin function signatures to the function registry
    fn add_builtin_function_signatures(signatures: &mut HashMap<String, FunctionSignature>) {
        // I/O functions
        signatures.insert(
            "println_i32".to_string(),
            FunctionSignature {
                param_types: vec![RueType::I32],
                return_type: RueType::Unit,
            },
        );
        signatures.insert(
            "println_i64".to_string(),
            FunctionSignature {
                param_types: vec![RueType::I64],
                return_type: RueType::Unit,
            },
        );
        signatures.insert(
            "println_bool".to_string(),
            FunctionSignature {
                param_types: vec![RueType::Bool],
                return_type: RueType::Unit,
            },
        );
        signatures.insert(
            "println_unit".to_string(),
            FunctionSignature {
                param_types: vec![RueType::Unit],
                return_type: RueType::Unit,
            },
        );
        signatures.insert(
            "input".to_string(),
            FunctionSignature {
                param_types: vec![],
                return_type: RueType::I64,
            },
        );
        signatures.insert(
            "input_i32".to_string(),
            FunctionSignature {
                param_types: vec![],
                return_type: RueType::I32,
            },
        );
        signatures.insert(
            "input_i64".to_string(),
            FunctionSignature {
                param_types: vec![],
                return_type: RueType::I64,
            },
        );

        // Type conversion functions
        signatures.insert(
            "to_i32".to_string(),
            FunctionSignature {
                param_types: vec![RueType::I64],
                return_type: RueType::I32,
            },
        );
        signatures.insert(
            "to_i64".to_string(),
            FunctionSignature {
                param_types: vec![RueType::I32],
                return_type: RueType::I64,
            },
        );
        signatures.insert(
            "to_bool".to_string(),
            FunctionSignature {
                param_types: vec![RueType::I32],
                return_type: RueType::Bool,
            },
        );

        // System functions
        signatures.insert(
            "exit".to_string(),
            FunctionSignature {
                param_types: vec![RueType::I64],
                return_type: RueType::Unit,
            },
        );
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

        // Lower the function body with is_function_body flag set
        builder.is_function_body = true;
        let result = builder.lower_block(&func.body);
        builder.is_function_body = false;

        debug!(target: "rue::mir", function = %func.name, ?result, "Function body lowering result");

        // Set return terminator if we haven't already returned
        if builder.current_block.is_some() {
            builder.set_terminator(MirTerminator::Return {
                value: result,
                span: None,
            });
            builder.finish_block();
        }

        let mir_func = MirFunction {
            name: func.name.clone(),
            params: func.params.clone(),
            return_type: func.return_type.clone(),
            blocks: builder.blocks,
            entry_block,
            temp_types: builder.temp_types,
            span: func.span,
        };

        // Debug: print MIR if enabled
        trace!(target: "rue::mir", function = %mir_func.name, mir = %mir_func, "Generated MIR for function");

        mir_func
    }

    /// Lower a HIR block
    fn lower_block(&mut self, block: &HirBlock) -> Option<Temp> {
        // Save the is_function_body flag
        let saved_is_function_body = self.is_function_body;

        // Lower all statements with is_function_body = false
        // Statements should never generate direct returns, only the final expression
        self.is_function_body = false;
        for stmt in &block.statements {
            self.lower_statement(stmt);
        }

        // Restore is_function_body for the final expression only
        self.is_function_body = saved_is_function_body;

        // Lower the final expression if present
        let result = block.expr.as_ref().map(|expr| self.lower_expr(expr));

        debug!(
            target: "rue::mir::blocks",
            ?result,
            has_expr = block.expr.is_some(),
            "Block lowering result"
        );

        result
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
                // Let statements create new bindings (shadowing) - don't mark as assigned
            }
            HirStatement::Assign {
                name,
                value,
                span: _,
            } => {
                let value_temp = self.lower_expr(value);
                // In SSA form, we create a new temp for the assignment
                self.variables.insert(name.clone(), value_temp);
                // Mark this variable as having been assigned (not just shadowed)
                self.assigned_variables.insert(name.clone());
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
                self.emit_const(const_val, Some(*span))
            }
            HirExpr::Var {
                name,
                ty: _,
                span: _,
            } => {
                // Look up the current temporary for this variable
                let temp = self
                    .variables
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| panic!("Undefined variable: {name}"));
                trace!(
                    target: "rue::mir::vars",
                    variable = %name,
                    ?temp,
                    ?self.variables,
                    "Variable lookup"
                );
                temp
            }
            HirExpr::Binary {
                op,
                lhs,
                rhs,
                ty,
                span,
            } => {
                // Save and clear is_function_body for sub-expressions
                let saved_is_function_body = self.is_function_body;
                self.is_function_body = false;

                let lhs_temp = self.lower_expr(lhs);
                let rhs_temp = self.lower_expr(rhs);

                // Restore is_function_body
                self.is_function_body = saved_is_function_body;

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

                self.emit_binop(mir_op, lhs_temp, rhs_temp, ty.clone(), Some(*span))
            }
            HirExpr::Unary { op, expr, ty, span } => {
                // Save and clear is_function_body for sub-expressions
                let saved_is_function_body = self.is_function_body;
                self.is_function_body = false;

                let operand_temp = self.lower_expr(expr);

                // Restore is_function_body
                self.is_function_body = saved_is_function_body;

                let mir_op = match op {
                    HirUnaryOp::Neg => MirUnaryOp::Neg,
                };

                self.emit_unop(mir_op, operand_temp, ty.clone(), Some(*span))
            }
            HirExpr::Call {
                func,
                args,
                ty,
                span,
            } => {
                // Save and clear is_function_body for sub-expressions
                let saved_is_function_body = self.is_function_body;
                self.is_function_body = false;

                let arg_temps: Vec<Temp> = args.iter().map(|arg| self.lower_expr(arg)).collect();

                // Restore is_function_body
                self.is_function_body = saved_is_function_body;

                // For now, assume all function calls are impure (conservative approach)
                // This can be refined later with function signature analysis or annotations

                self.emit_call(
                    func.clone(),
                    arg_temps,
                    crate::mir::CallKind::Impure,
                    ty.clone(),
                    Some(*span),
                )
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
                let unit_temp = self.emit_const(MirConst::Unit, Some(*span));
                debug!(target: "rue::mir", ?unit_temp, "While expression returns unit temp");
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
        // Evaluate condition (save is_function_body state to restore later)
        let saved_is_function_body = self.is_function_body;
        self.is_function_body = false; // Nested expressions shouldn't generate direct returns
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

        // Check if we should generate direct returns
        // Re-enable direct returns now that we've improved block parameter handling
        let generate_direct_returns = saved_is_function_body;

        // Only create join block if not generating direct returns
        let join_block_id = if generate_direct_returns {
            BlockId(u32::MAX) // Dummy value, won't be used
        } else {
            self.fresh_block()
        };

        // Create result temporary for join block (only used if not direct returns)
        let result_temp = self.fresh_temp(result_ty.clone());

        // When using direct returns, we need to pass variables as block parameters
        let (then_block_params, else_block_params, branch_args) = if generate_direct_returns {
            // Create block parameters for all variables
            let mut params = Vec::new();
            let mut args = Vec::new();

            for (_var_name, temp) in &vars_before_branch {
                let ty = self
                    .temp_types
                    .get(temp)
                    .cloned()
                    .expect("type for temp not registered in temp_types");
                let param_temp = self.fresh_temp(ty.clone());
                params.push((param_temp, ty));
                args.push(*temp);
            }

            (params.clone(), params, args)
        } else {
            (vec![], vec![], vec![])
        };

        // Branch to then/else blocks
        self.set_terminator(MirTerminator::Branch {
            condition: cond_temp,
            then_block: then_block_id,
            then_args: branch_args.clone(),
            else_block: else_block_id,
            else_args: branch_args,
            span: None,
        });
        self.finish_block();

        // Lower then block (restore is_function_body for block lowering)
        self.start_block(then_block_id, then_block_params.clone());

        // Update variable mappings if using direct returns
        if generate_direct_returns {
            for ((name, _), (param_temp, _)) in
                vars_before_branch.iter().zip(then_block_params.iter())
            {
                self.variables.insert(name.clone(), *param_temp);
            }
        }

        // Save and clear assigned variables tracking for then block scope
        let saved_assigned_vars = self.assigned_variables.clone();
        self.assigned_variables.clear();

        self.is_function_body = saved_is_function_body;
        let then_value = self.lower_block(then_block);
        self.is_function_body = false;
        let then_result = then_value.unwrap_or_else(|| {
            // If no expression, use unit
            self.emit_const(MirConst::Unit, None)
        });

        // Collect variables after then block
        let vars_after_then = self.variables.clone();

        // Prepare arguments for join block from then branch
        // CRITICAL: Handle variable shadowing vs assignment correctly for if-expressions
        let mut then_join_args = vec![then_result];
        for (name, original_temp) in &vars_before_branch {
            if let Some(&current_temp) = vars_after_then.get(name) {
                if self.assigned_variables.contains(name) {
                    // Variable was assigned in then block - pass the new value
                    then_join_args.push(current_temp);
                } else {
                    // Variable was only shadowed - pass the original value
                    then_join_args.push(*original_temp);
                }
            } else {
                // Variable not found, pass original
                then_join_args.push(*original_temp);
            }
        }

        // Store then block's assigned variables for later restoration
        let then_assigned_vars = self.assigned_variables.clone();
        // Restore assigned variables tracking
        self.assigned_variables = saved_assigned_vars.clone();

        if generate_direct_returns {
            // Direct return from then block
            self.set_terminator(MirTerminator::Return {
                value: Some(then_result),
                span: None,
            });
            self.finish_block();
        } else {
            // Jump to join block
            self.set_terminator(MirTerminator::Goto {
                target: join_block_id,
                args: then_join_args,
                span: None,
            });
            self.finish_block();
        }

        // Reset variables to state before branching for else block
        self.variables = vars_before_branch.iter().cloned().collect();

        // Lower else block
        self.start_block(else_block_id, else_block_params.clone());

        // Update variable mappings if using direct returns
        if generate_direct_returns {
            for ((name, _), (param_temp, _)) in
                vars_before_branch.iter().zip(else_block_params.iter())
            {
                self.variables.insert(name.clone(), *param_temp);
            }
        }

        // Clear assigned variables tracking for else block scope
        self.assigned_variables.clear();

        self.is_function_body = saved_is_function_body;
        let else_result = if let Some(else_blk) = else_block {
            let else_value = self.lower_block(else_blk);
            else_value.unwrap_or_else(|| self.emit_const(MirConst::Unit, None))
        } else {
            // No else block - return unit
            self.emit_const(MirConst::Unit, None)
        };
        self.is_function_body = false;

        // Collect variables after else block
        let vars_after_else = self.variables.clone();

        // Prepare arguments for join block from else branch
        // CRITICAL: Same logic as then branch - handle shadowing vs assignment
        let mut else_join_args = vec![else_result];
        for (name, original_temp) in &vars_before_branch {
            if let Some(&current_temp) = vars_after_else.get(name) {
                if self.assigned_variables.contains(name) {
                    // Variable was assigned in else block - pass the new value
                    else_join_args.push(current_temp);
                } else {
                    // Variable was only shadowed - pass the original value
                    else_join_args.push(*original_temp);
                }
            } else {
                // Variable not found, pass original
                else_join_args.push(*original_temp);
            }
        }

        // Merge assignment tracking from both branches
        let else_assigned_vars = self.assigned_variables.clone();
        // Restore original assignment tracking and merge both branches
        self.assigned_variables = saved_assigned_vars;
        self.assigned_variables.extend(then_assigned_vars);
        self.assigned_variables.extend(else_assigned_vars);

        if generate_direct_returns {
            // Direct return from else block
            self.set_terminator(MirTerminator::Return {
                value: Some(else_result),
                span: None,
            });
            self.finish_block();
            // Clear current block since all paths have returned
            self.current_block = None;
            // Return dummy temp since we won't reach here
            result_temp
        } else {
            // Jump to join block
            self.set_terminator(MirTerminator::Goto {
                target: join_block_id,
                args: else_join_args,
                span: None,
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

            self.start_block(join_block_id, join_params.clone());

            // Update variable mappings to use the join block parameters
            // The first parameter is the result, subsequent parameters are the variables
            for (name, param_temp) in join_param_vars {
                self.variables.insert(name, param_temp);
            }

            // Return the join block's first parameter (the result), not the original temp
            join_params[0].0
        }
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
            span: None,
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
            then_args: branch_args.clone(),
            else_block: loop_exit,
            else_args: branch_args.clone(),
            span: None,
        });
        self.finish_block();

        // Loop body - create block parameters for all variables
        let mut loop_body_params = Vec::new();
        let mut body_vars = HashMap::new();

        for (name, _) in &vars_before_loop {
            let temp_type = self
                .temp_types
                .get(&loop_vars[name])
                .cloned()
                .expect("type for temp not registered in temp_types");
            let body_temp = self.fresh_temp(temp_type.clone());
            loop_body_params.push((body_temp, temp_type));
            body_vars.insert(name.clone(), body_temp);
        }

        // Save the loop body parameters before passing them to start_block
        let saved_loop_body_params = loop_body_params.clone();

        self.start_block(loop_body, loop_body_params);

        // Update variables to use loop body parameters
        self.variables = body_vars;

        // Clear assigned variables tracking for the loop body scope
        let saved_assigned_vars = self.assigned_variables.clone();
        self.assigned_variables.clear();

        self.lower_block(body);

        // Collect variables after loop body to pass back to header
        // CRITICAL: We need to handle variable shadowing vs assignment correctly
        // - Variables that are assigned (x = value) should pass their new values
        // - Variables that are shadowed (let x = value) should pass their original values
        let mut loop_back_args = Vec::new();

        // Get the variable state when we entered the loop body (should match loop header params)
        let loop_body_entry_vars: HashMap<String, Temp> = vars_before_loop
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                // The loop body parameter for this variable
                let body_temp = saved_loop_body_params[i].0;
                (name.clone(), body_temp)
            })
            .collect();

        // CRITICAL: Handle variable shadowing vs assignment correctly:
        // - Variables that were assigned (x = value) should pass their new values
        // - Variables that were only shadowed (let x = value) should pass loop-carried values
        for (name, _original_temp) in &vars_before_loop {
            if let Some(&current_temp) = self.variables.get(name) {
                // Get the loop-carried value (what this variable had when entering the loop body)
                let loop_entry_temp = loop_body_entry_vars[name];

                if self.assigned_variables.contains(name) {
                    // Variable was assigned in the loop body - pass the new value
                    debug!(
                        target: "rue::mir::vars",
                        variable = %name,
                        ?current_temp,
                        "While loop: variable was assigned, passing new value"
                    );
                    loop_back_args.push(current_temp);
                } else {
                    // Variable was not assigned (only shadowed) - pass the loop-carried value
                    debug!(
                        target: "rue::mir::vars",
                        variable = %name,
                        ?loop_entry_temp,
                        "While loop: variable was not assigned, passing loop-carried value"
                    );
                    loop_back_args.push(loop_entry_temp);
                }
            } else {
                // This shouldn't happen in well-formed code
                debug!(
                    target: "rue::mir::vars",
                    variable = %name,
                    "While loop variable not found in current scope"
                );
            }
        }

        // Restore the assigned variables set
        self.assigned_variables = saved_assigned_vars;

        self.set_terminator(MirTerminator::Goto {
            target: loop_header,
            args: loop_back_args,
            span: None,
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
    fn debug_if_mult_mir() {
        // Create HIR for: if x == 1 { 1 } else { x * 2 }
        let hir_expr = HirExpr::If {
            cond: Box::new(HirExpr::Binary {
                op: BinOp::Eq,
                lhs: Box::new(HirExpr::Var {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    span: Span::dummy(),
                }),
                rhs: Box::new(HirExpr::Literal {
                    lit: HirLiteral::Int32(1),
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
                expr: Some(Box::new(HirExpr::Binary {
                    op: BinOp::Mul,
                    lhs: Box::new(HirExpr::Var {
                        name: "x".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    rhs: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(2),
                        span: Span::dummy(),
                    }),
                    ty: RueType::I32,
                    span: Span::dummy(),
                })),
            }),
            ty: RueType::I32,
            span: Span::dummy(),
        };

        let hir_func = HirFunction {
            name: "test_if".to_string(),
            params: vec![("x".to_string(), RueType::I32)],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![],
                expr: Some(Box::new(hir_expr)),
            },
            span: Span::dummy(),
        };

        let mir_func = MirBuilder::lower_function(&hir_func);

        // Print the MIR for debugging
        println!("\nMIR for test_if:");
        println!("Parameters: {:?}", mir_func.params);
        for block in &mir_func.blocks {
            println!("\nBlock {}:", block.id.0);
            println!("  Parameters: {:?}", block.params);
            for stmt in &block.statements {
                println!("  {stmt:?}");
            }
            println!("  Terminator: {:?}", block.terminator);
        }

        // The issue: in the else block, when we compute x * 2,
        // which temp is used for x?
        // It should be the block parameter, not the original function parameter
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

        // Should have 3 blocks: entry, then, else (join block optimized away since both branches return)
        assert_eq!(mir_func.blocks.len(), 3);
    }
}
