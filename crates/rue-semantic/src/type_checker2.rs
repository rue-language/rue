//! Type checker that builds HIR2 during semantic analysis
//!
//! This module provides a type checker that performs semantic analysis
//! and HIR2 instruction construction in a single pass, emitting a flat
//! sequence of instructions instead of building tree structures.

use crate::{FunctionSignature, Scope, SemanticError, convert_type_node_with_scope};
use rue_ast::{
    ArrayAccessNode, ArrayLiteralNode, CstRoot, ExpressionNode, FieldAccessNode, FunctionNode,
    StatementNode, StructLiteralNode, TupleLiteralNode,
};
use rue_ir::hir::{BinOp as HirBinOp, UnaryOp as HirUnaryOp};
use rue_ir::hir2::{BinOp as Hir2BinOp, Hir, InstIndex, UnaryOp as Hir2UnaryOp};
use rue_ir::hir2_builder::HirBuilder;
use rue_ir::types::RueType;
use rue_lexer::{Span, TokenKind};
use std::collections::HashMap;

/// Convert HIR BinOp to HIR2 BinOp
fn convert_binop(op: HirBinOp) -> Hir2BinOp {
    match op {
        HirBinOp::Add => Hir2BinOp::Add,
        HirBinOp::Sub => Hir2BinOp::Sub,
        HirBinOp::Mul => Hir2BinOp::Mul,
        HirBinOp::Div => Hir2BinOp::Div,
        HirBinOp::Mod => Hir2BinOp::Mod,
        HirBinOp::Lt => Hir2BinOp::Lt,
        HirBinOp::Le => Hir2BinOp::Le,
        HirBinOp::Gt => Hir2BinOp::Gt,
        HirBinOp::Ge => Hir2BinOp::Ge,
        HirBinOp::Eq => Hir2BinOp::Eq,
        HirBinOp::Ne => Hir2BinOp::Ne,
    }
}

/// Convert HIR UnaryOp to HIR2 UnaryOp
fn convert_unaryop(op: HirUnaryOp) -> Hir2UnaryOp {
    match op {
        HirUnaryOp::Neg => Hir2UnaryOp::Neg,
    }
}

/// Simple scope for variable type tracking (compatible with HirBuilder)
#[derive(Debug, Clone)]
struct VariableScope {
    variables: HashMap<String, RueType>,
}

impl VariableScope {
    fn new() -> Self {
        Self {
            variables: HashMap::new(),
        }
    }

    fn insert(&mut self, name: String, ty: RueType) {
        self.variables.insert(name, ty);
    }

    fn get(&self, name: &str) -> Option<&RueType> {
        self.variables.get(name)
    }
}

/// Type checker that builds HIR2 during analysis
pub struct TypeChecker2 {
    /// Stack of variable scopes for type lookup
    variable_scopes: Vec<VariableScope>,
    /// Global function signatures for call validation
    function_signatures: HashMap<String, FunctionSignature>,
    /// Global scope containing struct definitions
    global_scope: Scope,
}

impl TypeChecker2 {
    /// Create a new type checker with the given function signatures
    pub fn new(global_scope: Scope) -> Self {
        let function_signatures = global_scope.functions.clone();
        Self {
            variable_scopes: vec![VariableScope::new()],
            function_signatures,
            global_scope,
        }
    }

    /// Push a new variable scope onto the stack
    fn push_scope(&mut self) {
        self.variable_scopes.push(VariableScope::new());
    }

    /// Pop the current variable scope from the stack
    fn pop_scope(&mut self) {
        if self.variable_scopes.len() > 1 {
            self.variable_scopes.pop();
        }
    }

    /// Get the current variable scope (mutable)
    fn current_scope_mut(&mut self) -> &mut VariableScope {
        self.variable_scopes
            .last_mut()
            .expect("Should always have at least one scope")
    }

    /// Look up a variable in the scope stack
    fn lookup_variable(&self, name: &str) -> Option<&RueType> {
        // Search from innermost to outermost scope
        for scope in self.variable_scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty);
            }
        }
        None
    }

    /// Type check the entire program and build HIR2
    pub fn check_program(
        &mut self,
        ast: &CstRoot,
        builder: &mut HirBuilder,
    ) -> Result<(), SemanticError> {
        for item in &ast.items {
            match item {
                rue_ast::CstNode::Function(func) => {
                    self.check_function(func, builder)?;
                }
                rue_ast::CstNode::StructDefinition(_) => {
                    // Struct definitions are already processed in the first pass
                    // by analyze_cst_v2, so we skip them here
                }
                rue_ast::CstNode::Statement(_) => {
                    return Err(crate::create_semantic_error(
                        "Top-level statements are not supported",
                        Span { start: 0, end: 0 },
                    ));
                }
                rue_ast::CstNode::Expression(_)
                | rue_ast::CstNode::Token(_)
                | rue_ast::CstNode::Error(_) => {
                    return Err(crate::create_semantic_error(
                        "Unexpected top-level node type",
                        Span { start: 0, end: 0 },
                    ));
                }
            }
        }

        Ok(())
    }

    /// Type check a function and build HIR2 instructions
    fn check_function(
        &mut self,
        func: &FunctionNode,
        builder: &mut HirBuilder,
    ) -> Result<(), SemanticError> {
        // Extract function name
        let name = match &func.name.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => {
                return Err(SemanticError {
                    message: "Expected function name".to_string(),
                    span: func.name.span,
                });
            }
        };

        // Get function signature from pre-collected signatures
        let signature = self
            .function_signatures
            .get(&name)
            .ok_or_else(|| SemanticError {
                message: format!("Function signature not found: {name}"),
                span: func.name.span,
            })?
            .clone();

        // Set source span for error reporting
        builder.set_span(func.name.span);

        // Start function body block
        builder.start_block();

        // Create new scope for function parameters
        self.push_scope();

        // Add parameters to current scope and create HirBuilder params
        let mut params = Vec::new();
        for (i, param) in func.param_list.params.iter().enumerate() {
            let param_name = match &param.name.kind {
                TokenKind::Ident(name) => name.clone(),
                _ => {
                    return Err(SemanticError {
                        message: "Expected parameter name".to_string(),
                        span: param.name.span,
                    });
                }
            };

            let param_type = signature.param_types[i].clone();
            params.push((param_name.clone(), param_type.clone()));

            // Add to both our scope and the HirBuilder scope
            self.current_scope_mut()
                .insert(param_name.clone(), param_type.clone());

            // Emit a let instruction to make the parameter available in HIR2
            let param_inst = builder.emit_literal(0, param_type.clone()); // Placeholder value for parameter
            builder.emit_let(&param_name, param_inst, param_type);
        }

        // Type check function body
        self.check_block(&func.body, builder, Some(&signature.return_type))?;

        // Pop function scope
        self.pop_scope();

        // End function body block and get the real block index
        let body_block = builder.end_block();

        // Create the function
        builder
            .create_function(&name, params, signature.return_type, body_block)
            .map_err(|e| SemanticError {
                message: e,
                span: func.name.span,
            })?;

        Ok(())
    }

    /// Type check a block and emit instructions
    fn check_block(
        &mut self,
        block: &rue_ast::BlockNode,
        builder: &mut HirBuilder,
        expected_return_type: Option<&RueType>,
    ) -> Result<(), SemanticError> {
        // Create new scope for the block
        self.push_scope();

        // Process all statements
        for stmt in &block.statements {
            self.check_statement(stmt, builder, expected_return_type)?;
        }

        // Process final expression if present
        if let Some(final_expression) = &block.final_expr {
            let _expr_inst = if let Some(expected_type) = expected_return_type {
                // Use expected return type as hint for inference
                self.check_expression_with_hint(final_expression, builder, Some(expected_type))?
            } else {
                self.check_expression_with_hint(final_expression, builder, None)?
            };

            // Note: In HIR2, the final expression result is implicit through the block structure
            // The HirBuilder manages this automatically
        }

        // Pop block scope
        self.pop_scope();

        Ok(())
    }

    /// Type check a statement and emit instructions
    fn check_statement(
        &mut self,
        stmt: &StatementNode,
        builder: &mut HirBuilder,
        expected_return_type: Option<&RueType>,
    ) -> Result<(), SemanticError> {
        match stmt {
            StatementNode::Let(let_stmt) => {
                builder.set_span(let_stmt.let_token.span);

                // Extract variable name
                let var_name = match &let_stmt.name.kind {
                    TokenKind::Ident(name) => name.clone(),
                    _ => {
                        return Err(SemanticError {
                            message: "Expected variable name".to_string(),
                            span: let_stmt.name.span,
                        });
                    }
                };

                // Get declared type from type annotation first
                let declared_type = if let Some(type_ann) = &let_stmt.type_annotation {
                    convert_type_node_with_scope(&type_ann.ty, &self.global_scope)?
                } else {
                    // For let statements without type annotations, infer from initializer
                    let temp_expr =
                        self.check_expression_with_hint(&let_stmt.value, builder, None)?;
                    builder
                        .get_instruction_type(temp_expr)
                        .cloned()
                        .unwrap_or(RueType::Unit)
                };

                // Type check initialization expression with type hint
                let init_inst = if let_stmt.type_annotation.is_some() {
                    // Use declared type as hint for inference
                    self.check_expression_with_hint(&let_stmt.value, builder, Some(&declared_type))?
                } else {
                    // Re-check with inferred type
                    self.check_expression_with_hint(&let_stmt.value, builder, Some(&declared_type))?
                };

                // Validate type compatibility
                let init_type = builder
                    .get_instruction_type(init_inst)
                    .cloned()
                    .unwrap_or(RueType::Unit);
                if init_type != declared_type {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch: expected {declared_type}, found {init_type}"
                        ),
                        span: let_stmt.value.span(),
                    });
                }

                // Add variable to current scope
                self.current_scope_mut()
                    .insert(var_name.clone(), declared_type.clone());

                // Emit let instruction
                builder.emit_let(&var_name, init_inst, declared_type);

                Ok(())
            }
            StatementNode::Assign(assign_stmt) => {
                builder.set_span(assign_stmt.name.span);

                // Extract variable name
                let var_name = match &assign_stmt.name.kind {
                    TokenKind::Ident(name) => name.clone(),
                    _ => {
                        return Err(SemanticError {
                            message: "Expected variable name".to_string(),
                            span: assign_stmt.name.span,
                        });
                    }
                };

                // Look up variable type
                let var_type =
                    self.lookup_variable(&var_name)
                        .cloned()
                        .ok_or_else(|| SemanticError {
                            message: format!("Undefined variable: {var_name}"),
                            span: assign_stmt.name.span,
                        })?;

                // Type check value expression with type hint
                let value_inst =
                    self.check_expression_with_hint(&assign_stmt.value, builder, Some(&var_type))?;

                // Validate type compatibility
                let value_type = builder
                    .get_instruction_type(value_inst)
                    .cloned()
                    .unwrap_or(RueType::Unit);
                if value_type != var_type {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch in assignment: expected {var_type}, found {value_type}"
                        ),
                        span: assign_stmt.value.span(),
                    });
                }

                // Emit assign instruction
                builder
                    .emit_assign(&var_name, value_inst)
                    .map_err(|e| SemanticError {
                        message: e,
                        span: assign_stmt.name.span,
                    })?;

                Ok(())
            }
            StatementNode::Expression(expr_stmt) => {
                let _expr_inst =
                    self.check_expression_with_hint(&expr_stmt.expression, builder, None)?;
                Ok(())
            }
            StatementNode::Return(return_stmt) => {
                builder.set_span(return_stmt.return_token.span);

                let return_inst = if let Some(expr) = &return_stmt.expression {
                    // Type check the return expression with the expected return type as hint
                    let expr_inst = if let Some(expected_type) = expected_return_type {
                        self.check_expression_with_hint(expr, builder, Some(expected_type))?
                    } else {
                        self.check_expression_with_hint(expr, builder, None)?
                    };

                    // Validate that the return type matches the function's return type
                    if let Some(expected_type) = expected_return_type {
                        let expr_type = builder
                            .get_instruction_type(expr_inst)
                            .cloned()
                            .unwrap_or(RueType::Unit);
                        if expr_type != *expected_type {
                            return Err(SemanticError {
                                message: format!(
                                    "Return type mismatch: expected {expected_type}, found {expr_type}"
                                ),
                                span: expr.span(),
                            });
                        }
                    }

                    Some(expr_inst)
                } else {
                    // Bare return; - should be unit type
                    if let Some(expected_type) = expected_return_type
                        && *expected_type != RueType::Unit
                    {
                        return Err(SemanticError {
                            message: format!(
                                "Return type mismatch: expected {expected_type}, found unit (bare return)"
                            ),
                            span: return_stmt.return_token.span,
                        });
                    }
                    None
                };

                builder.emit_return(return_inst);
                Ok(())
            }
        }
    }

    /// Type check an expression with an optional type hint and emit instructions
    fn check_expression_with_hint(
        &mut self,
        expr: &ExpressionNode,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
    ) -> Result<InstIndex, SemanticError> {
        match expr {
            ExpressionNode::Literal(lit) => self.check_literal_with_hint(lit, builder, type_hint),
            ExpressionNode::Identifier(ident) => self.check_identifier(ident, builder),
            ExpressionNode::Binary(bin_expr) => {
                self.check_binary_expression_with_hint(bin_expr, builder, type_hint)
            }
            ExpressionNode::Unary(unary_expr) => {
                self.check_unary_expression_with_hint(unary_expr, builder, type_hint)
            }
            ExpressionNode::Call(call) => self.check_call_expression(call, builder),
            ExpressionNode::If(if_expr) => {
                self.check_if_expression_with_hint(if_expr, builder, type_hint)
            }
            ExpressionNode::While(while_expr) => self.check_while_expression(while_expr, builder),
            ExpressionNode::StructLiteral(struct_lit) => {
                self.check_struct_literal(struct_lit, builder)
            }
            ExpressionNode::FieldAccess(field_access) => {
                self.check_field_access(field_access, builder)
            }
            ExpressionNode::TupleLiteral(tuple_lit) => {
                self.check_tuple_literal_with_hint(tuple_lit, builder, type_hint)
            }
            ExpressionNode::ArrayLiteral(array_lit) => {
                self.check_array_literal_with_hint(array_lit, builder, type_hint)
            }
            ExpressionNode::ArrayAccess(array_access) => {
                self.check_array_access(array_access, builder)
            }
        }
    }

    fn check_literal_with_hint(
        &mut self,
        lit: &rue_lexer::Token,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(lit.span);

        match &lit.kind {
            TokenKind::Integer(n) => {
                // Use type hint or default to i32
                let inferred_type = type_hint.cloned().unwrap_or(RueType::I32);

                match inferred_type {
                    RueType::I32 => {
                        // Check if value fits in i32
                        if *n >= i32::MIN as i64 && *n <= i32::MAX as i64 {
                            Ok(builder.emit_literal(*n as u64, RueType::I32))
                        } else {
                            Err(SemanticError {
                                message: format!(
                                    "literal out of range for `i32`\n\
                                     help: the literal `{n}` does not fit into the type `i32` whose range is `-2147483648..=2147483647`"
                                ),
                                span: lit.span,
                            })
                        }
                    }
                    RueType::I64 => Ok(builder.emit_literal(*n as u64, RueType::I64)),
                    _ => Err(SemanticError {
                        message: format!(
                            "Cannot use integer literal in context expecting {inferred_type}"
                        ),
                        span: lit.span,
                    }),
                }
            }
            TokenKind::True => Ok(builder.emit_literal(1, RueType::Bool)),
            TokenKind::False => Ok(builder.emit_literal(0, RueType::Bool)),
            TokenKind::Unit => Ok(builder.emit_literal(0, RueType::Unit)),
            _ => Err(SemanticError {
                message: format!("Invalid literal: {:?}", lit.kind),
                span: lit.span,
            }),
        }
    }

    fn check_identifier(
        &mut self,
        ident: &rue_lexer::Token,
        builder: &mut HirBuilder,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(ident.span);

        let name = match &ident.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => {
                return Err(SemanticError {
                    message: "Expected identifier".to_string(),
                    span: ident.span,
                });
            }
        };

        // Look up variable in scope and emit load instruction
        builder.emit_load(&name).map_err(|e| SemanticError {
            message: e,
            span: ident.span,
        })
    }

    fn check_binary_expression_with_hint(
        &mut self,
        bin_expr: &rue_ast::BinaryExprNode,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(bin_expr.operator.span);

        // Convert operator
        let op = match &bin_expr.operator.kind {
            TokenKind::Plus => HirBinOp::Add,
            TokenKind::Minus => HirBinOp::Sub,
            TokenKind::Star => HirBinOp::Mul,
            TokenKind::Slash => HirBinOp::Div,
            TokenKind::Percent => HirBinOp::Mod,
            TokenKind::LessEqual => HirBinOp::Le,
            TokenKind::Less => HirBinOp::Lt,
            TokenKind::GreaterEqual => HirBinOp::Ge,
            TokenKind::Greater => HirBinOp::Gt,
            TokenKind::Equal => HirBinOp::Eq,
            TokenKind::NotEqual => HirBinOp::Ne,
            _ => {
                return Err(SemanticError {
                    message: format!("Invalid binary operator: {:?}", bin_expr.operator.kind),
                    span: bin_expr.operator.span,
                });
            }
        };

        // For arithmetic operations, pass the type hint to operands if available
        let operand_hint = match op {
            HirBinOp::Add | HirBinOp::Sub | HirBinOp::Mul | HirBinOp::Div | HirBinOp::Mod => {
                type_hint
            }
            HirBinOp::Lt
            | HirBinOp::Le
            | HirBinOp::Gt
            | HirBinOp::Ge
            | HirBinOp::Eq
            | HirBinOp::Ne => None,
        };

        // Check operands
        let lhs_inst = self.check_expression_with_hint(&bin_expr.left, builder, operand_hint)?;
        let rhs_inst = self.check_expression_with_hint(&bin_expr.right, builder, operand_hint)?;

        // Get operand types
        let lhs_ty = builder
            .get_instruction_type(lhs_inst)
            .cloned()
            .unwrap_or(RueType::Unit);
        let rhs_ty = builder
            .get_instruction_type(rhs_inst)
            .cloned()
            .unwrap_or(RueType::Unit);

        // Type checking for binary operations
        let result_type = match op {
            HirBinOp::Add | HirBinOp::Sub | HirBinOp::Mul | HirBinOp::Div | HirBinOp::Mod => {
                // Arithmetic operations: operands must be same numeric type
                if lhs_ty != rhs_ty {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch in binary operation: {lhs_ty} {} {rhs_ty}",
                            op
                        ),
                        span: bin_expr.operator.span,
                    });
                }
                match lhs_ty {
                    RueType::I32 | RueType::I64 => lhs_ty,
                    _ => {
                        return Err(SemanticError {
                            message: "Arithmetic operators require numeric types".to_string(),
                            span: bin_expr.operator.span,
                        });
                    }
                }
            }
            HirBinOp::Lt
            | HirBinOp::Le
            | HirBinOp::Gt
            | HirBinOp::Ge
            | HirBinOp::Eq
            | HirBinOp::Ne => {
                // Comparison operations: operands must be same type, result is bool
                if lhs_ty != rhs_ty {
                    return Err(SemanticError {
                        message: format!("Type mismatch in comparison: {lhs_ty} {} {rhs_ty}", op),
                        span: bin_expr.operator.span,
                    });
                }
                RueType::Bool
            }
        };

        // Convert HIR op to HIR2 op and emit instruction
        let hir2_op = convert_binop(op);
        Ok(builder.emit_binary(lhs_inst, rhs_inst, hir2_op, result_type))
    }

    fn check_unary_expression_with_hint(
        &mut self,
        unary_expr: &rue_ast::UnaryExprNode,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(unary_expr.operator.span);

        // For unary operations, we can pass the type hint to the operand
        let operand_inst =
            self.check_expression_with_hint(&unary_expr.operand, builder, type_hint)?;

        let op = match &unary_expr.operator.kind {
            TokenKind::Minus => HirUnaryOp::Neg,
            _ => {
                return Err(SemanticError {
                    message: format!("Invalid unary operator: {:?}", unary_expr.operator.kind),
                    span: unary_expr.operator.span,
                });
            }
        };

        // Get operand type
        let operand_ty = builder
            .get_instruction_type(operand_inst)
            .cloned()
            .unwrap_or(RueType::Unit);

        // Type checking for unary operations
        let result_type = match op {
            HirUnaryOp::Neg => match operand_ty {
                RueType::I32 | RueType::I64 => operand_ty,
                _ => {
                    return Err(SemanticError {
                        message: format!("Unary negation not supported for type: {operand_ty}"),
                        span: unary_expr.operator.span,
                    });
                }
            },
        };

        // Convert HIR op to HIR2 op and emit instruction
        let hir2_op = convert_unaryop(op);
        Ok(builder.emit_unary(operand_inst, hir2_op, result_type))
    }

    fn check_call_expression(
        &mut self,
        call: &rue_ast::CallExprNode,
        builder: &mut HirBuilder,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(call.open_paren.span);

        // Extract function name
        let func_name = match call.function.as_ref() {
            ExpressionNode::Identifier(token) => match &token.kind {
                TokenKind::Ident(name) => name.clone(),
                _ => {
                    return Err(SemanticError {
                        message: "Expected function name".to_string(),
                        span: token.span,
                    });
                }
            },
            _ => {
                return Err(SemanticError {
                    message: "Function calls must use simple identifiers".to_string(),
                    span: call.function.span(),
                });
            }
        };

        // Look up function signature
        let signature = self
            .function_signatures
            .get(&func_name)
            .ok_or_else(|| SemanticError {
                message: format!("Undefined function: {func_name}"),
                span: match call.function.as_ref() {
                    ExpressionNode::Identifier(token) => token.span,
                    _ => call.open_paren.span,
                },
            })?
            .clone();

        // Check argument count
        if call.args.len() != signature.param_types.len() {
            return Err(SemanticError {
                message: format!(
                    "Function '{}' expects {} arguments, but {} were provided",
                    func_name,
                    signature.param_types.len(),
                    call.args.len()
                ),
                span: call.open_paren.span,
            });
        }

        // Type check arguments with type hints for inference
        let mut arg_insts = Vec::new();
        for (i, arg_expr) in call.args.iter().enumerate() {
            let expected_type = &signature.param_types[i];
            let arg_inst =
                self.check_expression_with_hint(arg_expr, builder, Some(expected_type))?;

            let arg_type = builder
                .get_instruction_type(arg_inst)
                .cloned()
                .unwrap_or(RueType::Unit);
            if arg_type != *expected_type {
                return Err(SemanticError {
                    message: format!(
                        "Type mismatch: Argument {} of function {}: expected {expected_type}, found {arg_type}",
                        i + 1,
                        func_name,
                    ),
                    span: arg_expr.span(),
                });
            }

            arg_insts.push(arg_inst);
        }

        // Emit call instruction
        Ok(builder.emit_call(&func_name, arg_insts, signature.return_type))
    }

    fn check_if_expression_with_hint(
        &mut self,
        if_expr: &rue_ast::IfStatementNode,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(if_expr.if_token.span);

        // Type check condition
        let cond_inst = self.check_expression_with_hint(&if_expr.condition, builder, None)?;
        let cond_type = builder
            .get_instruction_type(cond_inst)
            .cloned()
            .unwrap_or(RueType::Unit);
        if cond_type != RueType::Bool {
            return Err(SemanticError {
                message: format!("If condition must be bool, found {cond_type}"),
                span: if_expr.condition.span(),
            });
        }

        // Type check then block with type hint
        let then_block = builder.start_block();
        self.check_block(&if_expr.then_block, builder, type_hint)?;
        builder.end_block();

        // Type check else block if present
        let else_block = if let Some(else_clause) = &if_expr.else_clause {
            let else_block_node = match &else_clause.body {
                rue_ast::ElseBodyNode::Block(block) => block.as_ref(),
                rue_ast::ElseBodyNode::If(_) => {
                    return Err(SemanticError {
                        message: "Else-if chains not yet supported in expressions".to_string(),
                        span: if_expr.if_token.span,
                    });
                }
            };

            let else_blk = builder.start_block();
            self.check_block(else_block_node, builder, type_hint)?;
            builder.end_block();
            Some(else_blk)
        } else {
            None
        };

        // Determine result type - for now, use type hint or Unit
        let result_type = type_hint.cloned().unwrap_or(RueType::Unit);

        // Emit if instruction
        Ok(builder.emit_if(cond_inst, then_block, else_block, Some(result_type)))
    }

    fn check_while_expression(
        &mut self,
        while_expr: &rue_ast::WhileStatementNode,
        builder: &mut HirBuilder,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(while_expr.while_token.span);

        // Type check condition
        let cond_inst = self.check_expression_with_hint(&while_expr.condition, builder, None)?;
        let cond_type = builder
            .get_instruction_type(cond_inst)
            .cloned()
            .unwrap_or(RueType::Unit);
        if cond_type != RueType::Bool {
            return Err(SemanticError {
                message: format!("While condition must be bool, found {cond_type}"),
                span: while_expr.condition.span(),
            });
        }

        // Type check body
        let body_block = builder.start_block();
        self.check_block(&while_expr.body, builder, None)?;
        builder.end_block();

        // Emit while instruction
        Ok(builder.emit_while(cond_inst, body_block))
    }

    fn check_struct_literal(
        &mut self,
        struct_lit: &StructLiteralNode,
        builder: &mut HirBuilder,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(struct_lit.name.span);

        let struct_name = match &struct_lit.name.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => {
                return Err(SemanticError {
                    message: "Expected struct name".to_string(),
                    span: struct_lit.name.span,
                });
            }
        };

        // Look up struct definition
        let struct_def = self
            .global_scope
            .get_struct(&struct_name)
            .ok_or_else(|| SemanticError {
                message: format!("Undefined struct: {struct_name}"),
                span: struct_lit.name.span,
            })?
            .clone();

        // Create a map of expected fields for validation
        let expected_fields: HashMap<String, RueType> = struct_def
            .fields
            .iter()
            .map(|(name, ty)| (name.clone(), ty.clone()))
            .collect();

        // Type check each field and collect them
        let mut field_insts = Vec::new();
        let mut provided_fields = std::collections::HashSet::new();

        for field in &struct_lit.fields {
            let field_name = match &field.name.kind {
                TokenKind::Ident(name) => name.clone(),
                _ => {
                    return Err(SemanticError {
                        message: "Expected field name".to_string(),
                        span: field.name.span,
                    });
                }
            };

            // Check if field exists in struct definition
            let expected_type = expected_fields
                .get(&field_name)
                .ok_or_else(|| SemanticError {
                    message: format!("Unknown field {field_name}"),
                    span: field.name.span,
                })?;

            // Type check field value with expected type as hint
            let field_inst =
                self.check_expression_with_hint(&field.value, builder, Some(expected_type))?;
            let field_type = builder
                .get_instruction_type(field_inst)
                .cloned()
                .unwrap_or(RueType::Unit);
            if field_type != *expected_type {
                return Err(SemanticError {
                    message: format!(
                        "Type mismatch: Field {} expected {expected_type}, found {field_type}",
                        field_name,
                    ),
                    span: field.value.span(),
                });
            }

            provided_fields.insert(field_name.clone());
            field_insts.push(field_inst);
        }

        // Check that all expected fields are provided
        let missing_fields: Vec<_> = expected_fields
            .keys()
            .filter(|field| !provided_fields.contains(*field))
            .cloned()
            .collect();

        if !missing_fields.is_empty() {
            return Err(SemanticError {
                message: format!("Missing fields: {}", missing_fields.join(", ")),
                span: struct_lit.name.span,
            });
        }

        // For HIR2, we'd need to implement struct literal instructions
        // For now, return a placeholder (this would need to be implemented in HirBuilder)
        Err(SemanticError {
            message: "Struct literals not yet implemented in HIR2".to_string(),
            span: struct_lit.name.span,
        })
    }

    fn check_field_access(
        &mut self,
        field_access: &FieldAccessNode,
        builder: &mut HirBuilder,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(field_access.dot.span);

        let base_inst = self.check_expression_with_hint(&field_access.base, builder, None)?;
        let base_type = builder
            .get_instruction_type(base_inst)
            .cloned()
            .unwrap_or(RueType::Unit);

        match base_type {
            RueType::Struct(_struct_id) => {
                // For HIR2, we'd need to implement field access instructions
                // For now, return error (this would need to be implemented in HirBuilder)
                Err(SemanticError {
                    message: "Struct field access not yet implemented in HIR2".to_string(),
                    span: field_access.dot.span,
                })
            }
            RueType::Tuple(_element_types) => {
                // For HIR2, we'd need to implement tuple field access instructions
                // For now, return error (this would need to be implemented in HirBuilder)
                Err(SemanticError {
                    message: "Tuple field access not yet implemented in HIR2".to_string(),
                    span: field_access.dot.span,
                })
            }
            _ => Err(SemanticError {
                message: format!(
                    "Cannot access field on type {base_type}, expected struct or tuple"
                ),
                span: field_access.base.span(),
            }),
        }
    }

    fn check_tuple_literal_with_hint(
        &mut self,
        tuple_lit: &TupleLiteralNode,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(tuple_lit.open_paren.span);

        // Extract element type hints from the tuple type hint
        let element_hints: Vec<Option<&RueType>> =
            if let Some(RueType::Tuple(expected_types)) = type_hint {
                expected_types.iter().map(Some).collect()
            } else {
                vec![None; tuple_lit.elements.len()]
            };

        let mut element_insts = Vec::new();
        let mut element_types = Vec::new();

        for (i, element_expr) in tuple_lit.elements.iter().enumerate() {
            let element_hint = element_hints.get(i).and_then(|h| *h);
            let element_inst =
                self.check_expression_with_hint(element_expr, builder, element_hint)?;
            let element_type = builder
                .get_instruction_type(element_inst)
                .cloned()
                .unwrap_or(RueType::Unit);
            element_types.push(element_type);
            element_insts.push(element_inst);
        }

        // For HIR2, we'd need to implement tuple literal instructions
        // For now, return error (this would need to be implemented in HirBuilder)
        Err(SemanticError {
            message: "Tuple literals not yet implemented in HIR2".to_string(),
            span: tuple_lit.open_paren.span,
        })
    }

    fn check_array_literal_with_hint(
        &mut self,
        array_lit: &ArrayLiteralNode,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(array_lit.open_bracket.span);

        // Handle empty array literals with type inference
        if array_lit.elements.is_empty() {
            // For empty arrays, we need a type hint to infer the element type
            if let Some(RueType::Array(_element_type, size)) = type_hint {
                // Verify that the size matches (should be 0 for empty array)
                if *size != 0 {
                    return Err(SemanticError {
                        message: format!("Array size mismatch: expected {size} elements, found 0"),
                        span: array_lit.open_bracket.span,
                    });
                }

                // For HIR2, we'd need to implement array literal instructions
                // For now, return error (this would need to be implemented in HirBuilder)
                return Err(SemanticError {
                    message: "Array literals not yet implemented in HIR2".to_string(),
                    span: array_lit.open_bracket.span,
                });
            } else {
                return Err(SemanticError {
                    message: "Cannot infer type of empty array literal without type annotation"
                        .to_string(),
                    span: array_lit.open_bracket.span,
                });
            }
        }

        let mut element_insts = Vec::new();
        let mut element_type: Option<RueType> = None;

        // Extract element type hint from the array type hint
        let element_hint = if let Some(RueType::Array(element_type, _)) = type_hint {
            Some(element_type.as_ref())
        } else {
            None
        };

        for element_expr in &array_lit.elements {
            let element_inst =
                self.check_expression_with_hint(element_expr, builder, element_hint)?;
            let elem_type = builder
                .get_instruction_type(element_inst)
                .cloned()
                .unwrap_or(RueType::Unit);

            // Check that all elements have the same type
            if let Some(ref expected_type) = element_type {
                if elem_type != *expected_type {
                    return Err(SemanticError {
                        message: format!(
                            "Array elements must have the same type: expected {expected_type}, found {elem_type}"
                        ),
                        span: element_expr.span(),
                    });
                }
            } else {
                element_type = Some(elem_type);
            }

            element_insts.push(element_inst);
        }

        // For HIR2, we'd need to implement array literal instructions
        // For now, return error (this would need to be implemented in HirBuilder)
        Err(SemanticError {
            message: "Array literals not yet implemented in HIR2".to_string(),
            span: array_lit.open_bracket.span,
        })
    }

    fn check_array_access(
        &mut self,
        array_access: &ArrayAccessNode,
        builder: &mut HirBuilder,
    ) -> Result<InstIndex, SemanticError> {
        builder.set_span(array_access.open_bracket.span);

        let base_inst = self.check_expression_with_hint(&array_access.base, builder, None)?;
        // Check array index without forcing a specific type, allow both i32 and i64
        let index_inst = self.check_expression_with_hint(&array_access.index, builder, None)?;

        // Validate index type - must be an integer type
        let index_type = builder
            .get_instruction_type(index_inst)
            .cloned()
            .unwrap_or(RueType::Unit);
        match index_type {
            RueType::I32 | RueType::I64 => {}
            _ => {
                return Err(SemanticError {
                    message: "Array index must be an integer type (i32 or i64)".to_string(),
                    span: array_access.index.span(),
                });
            }
        }

        // Validate base type and extract element type
        let base_type = builder
            .get_instruction_type(base_inst)
            .cloned()
            .unwrap_or(RueType::Unit);
        let _element_type = match base_type {
            RueType::Array(element_type, _) => (*element_type).clone(),
            _ => {
                return Err(SemanticError {
                    message: format!("Cannot index into type {base_type}, expected array"),
                    span: array_access.base.span(),
                });
            }
        };

        // For HIR2, we'd need to implement array access instructions
        // For now, return error (this would need to be implemented in HirBuilder)
        Err(SemanticError {
            message: "Array access not yet implemented in HIR2".to_string(),
            span: array_access.open_bracket.span,
        })
    }
}

/// Analyze a CST using the new HIR2 TypeChecker (HIR2 pipeline)
///
/// This function uses the TypeChecker2 directly on the CST and outputs HIR2
/// instead of the tree-based HIR. This is the new "HIR2 pipeline" for
/// instruction-based intermediate representation.
pub fn analyze_cst_v2(cst: &CstRoot) -> Result<Hir, SemanticError> {
    // Create global scope and add built-in functions
    let mut global_scope = Scope::default();
    crate::add_builtin_functions(&mut global_scope);

    // Phase 1: Process struct definitions from CST first
    for item in &cst.items {
        if let rue_ast::CstNode::StructDefinition(struct_def) = item {
            crate::analyze_struct_definition(&mut global_scope, struct_def)?;
        }
    }

    // Phase 2: Collect function signatures from CST
    for item in &cst.items {
        if let rue_ast::CstNode::Function(func) = item {
            // Extract function name
            let func_name = match &func.name.kind {
                rue_lexer::TokenKind::Ident(name) => name.clone(),
                _ => {
                    return Err(SemanticError {
                        message: "Expected function name".to_string(),
                        span: func.name.span,
                    });
                }
            };

            // Extract parameter types
            let mut param_types = Vec::new();
            for param in &func.param_list.params {
                if let Some(ref type_annotation) = param.type_annotation {
                    let param_type =
                        convert_type_node_with_scope(&type_annotation.ty, &global_scope)?;
                    param_types.push(param_type);
                } else {
                    return Err(SemanticError {
                        message: "Function parameters must have type annotations".to_string(),
                        span: param.name.span,
                    });
                }
            }

            // Extract return type
            let return_type = if let Some(ref return_type_annotation) = func.return_type {
                convert_type_node_with_scope(&return_type_annotation.ty, &global_scope)?
            } else {
                RueType::Unit
            };

            // Register function signature
            global_scope.functions.insert(
                func_name,
                FunctionSignature {
                    param_types,
                    return_type,
                },
            );
        }
    }

    // Phase 3: Create TypeChecker2 and HirBuilder
    let mut type_checker = TypeChecker2::new(global_scope.clone());
    let mut builder = HirBuilder::new();

    // Use TypeChecker2 to analyze the CST and produce HIR2
    type_checker.check_program(cst, &mut builder)?;

    // Phase 4: Finish building and return HIR2
    Ok(builder.finish())
}
