//! Type checker that builds HIR during semantic analysis
//!
//! This module provides a type checker that performs semantic analysis
//! and HIR instruction construction in a single pass, emitting a flat
//! sequence of instructions instead of building tree structures.

use crate::{FunctionSignature, Scope, SemanticError, convert_type_node_with_scope};
use rue_ast::{
    ArrayAccessNode, ArrayLiteralNode, CstRoot, ExpressionNode, FieldAccessNode, FieldKindNode,
    FunctionNode, StatementNode, StructLiteralNode, TupleLiteralNode,
};
use rue_ir::hir::{BinOp, BlockParam, BlockParamIndex, UnaryOp, ValueRef};
use rue_ir::hir_builder::HirBuilder;
use rue_ir::types::{BindingId, RueType};
use rue_lexer::{Span, TokenKind};
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// Trait for AST nodes that can compute write sets
trait ComputeWrites {
    fn compute_writes(&self, builder: &HirBuilder) -> HashSet<BindingId>;
}

/// Trait for AST nodes that can compute read sets
trait ComputeReads {
    fn compute_reads(&self, builder: &HirBuilder) -> HashSet<BindingId>;
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

/// Type checker that builds HIR during analysis
pub struct TypeChecker {
    /// Stack of variable scopes for type lookup
    variable_scopes: Vec<VariableScope>,
    /// Global function signatures for call validation
    function_signatures: HashMap<String, FunctionSignature>,
    /// Global scope containing struct definitions
    global_scope: Scope,
}

impl TypeChecker {
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

    /// Replace the current (top) variable scope with whatever the HirBuilder
    /// says is visible *right now*. This keeps names->types aligned.
    fn sync_scope_from_builder(&mut self, builder: &HirBuilder) {
        // Build a fresh scope with just names -> types (TypeChecker only needs types)
        let mut synced = VariableScope::new();
        // Only variables visible in the builder's current block will appear here
        let captured = builder.capture_environment();
        debug!(
            "Syncing scope from builder, captured {} variables",
            captured.len()
        );
        for (_bid, name, _val, ty) in captured {
            debug!("  Adding variable {} with type {:?}", name, ty);
            synced.insert(name, ty);
        }
        // Replace the top frame; do NOT push/pop (preserves nesting)
        *self.current_scope_mut() = synced;
        debug!(
            "Scope sync complete, current scope has {} variables",
            self.current_scope_mut().variables.len()
        );
    }

    #[cfg(debug_assertions)]
    fn debug_assert_scopes_match(&self, builder: &HirBuilder) {
        use std::collections::HashSet;
        let tc_names: HashSet<_> = self
            .variable_scopes
            .last()
            .expect("variable_scopes should have at least one scope")
            .variables
            .keys()
            .cloned()
            .collect();
        let b_names: HashSet<_> = builder
            .capture_environment()
            .into_iter()
            .map(|(_, n, _, _)| n)
            .collect();
        // The two sets don't have to be identical if you allow extra helper params,
        // but in typical Rue scoping they should match.
        assert_eq!(
            tc_names, b_names,
            "TypeChecker and HirBuilder scopes diverged"
        );
    }

    /// Type check the entire program and build HIR
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
                    // by analyze_cst, so we skip them here
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

    /// Type check a function and build HIR instructions
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

        // Create new scope for function parameters
        self.push_scope();

        // Collect parameters and add to scope
        let mut params = Vec::new();
        let mut block_params = Vec::new();
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

            // Create block parameter for the function body
            block_params.push(rue_ir::hir::BlockParam {
                name: builder.intern_string(&param_name),
                ty: param_type.clone(),
            });

            // Add to semantic analyzer scope
            self.current_scope_mut()
                .insert(param_name.clone(), param_type.clone());
        }

        // Start function body block with parameters as the entry point
        let entry_block_id = builder.start_block_with_params(block_params);

        // Add parameters to HirBuilder scope as block parameters
        for (i, (param_name, param_type)) in params.iter().enumerate() {
            let param_ref = rue_ir::hir::ValueRef::from_block_param_with_id(
                entry_block_id,
                rue_ir::hir::BlockParamIndex::new(i as u32),
            );
            // Allocate a BindingId for this function parameter
            let binding_id = builder.alloc_binding_id();
            builder.current_scope_mut().declare_var(
                param_name.clone(),
                binding_id,
                param_ref,
                param_type.clone(),
            );
        }

        // Type check function body
        self.check_block(&func.body, builder, Some(&signature.return_type))?;

        // Pop function scope
        self.pop_scope();

        // End the current block (might be different from entry if body is an if-expression)
        builder.end_block();

        // Create the function using the entry block as the body
        // This ensures the function starts at the condition evaluation, not the merge block
        builder
            .create_function(&name, params, signature.return_type, entry_block_id)
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
        self.check_block_internal(block, builder, expected_return_type, true)
    }

    /// Internal block checking with control over return emission
    fn check_block_internal(
        &mut self,
        block: &rue_ast::BlockNode,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
        emit_return: bool,
    ) -> Result<(), SemanticError> {
        // Create new scope for the block
        self.push_scope();

        // Process all statements
        for stmt in &block.statements {
            self.check_statement(stmt, builder, type_hint)?;
        }

        // Process final expression if present
        if let Some(final_expression) = &block.final_expr {
            let expr_inst = if let Some(hint_type) = type_hint {
                // Use type hint for inference
                self.check_expression_with_hint(final_expression, builder, Some(hint_type))?
            } else {
                self.check_expression_with_hint(final_expression, builder, None)?
            };

            // Validate the final expression type matches the expected type (for function returns)
            if emit_return && let Some(expected_type) = type_hint {
                let expr_type = builder.get_value_type(expr_inst).unwrap_or(RueType::Unit);

                if expr_type != *expected_type {
                    return Err(SemanticError {
                        message: format!(
                            "Return type mismatch: expected {expected_type}, found {expr_type}"
                        ),
                        span: final_expression.span(),
                    });
                }

                builder.set_return_terminator(Some(expr_inst));
            }
        } else {
            // No final expression means the block returns Unit
            // UNLESS the block has already been terminated (e.g., by a return statement)

            // Check if the block has already been explicitly terminated
            if !builder.current_block_has_terminator() {
                // Block hasn't been terminated, so it returns Unit
                if emit_return && let Some(expected_type) = type_hint {
                    if *expected_type != RueType::Unit {
                        return Err(SemanticError {
                            message: format!(
                                "Type mismatch: function expects to return {expected_type}, but block returns () (unit)"
                            ),
                            span: block.close_brace.span,
                        });
                    }
                    // Set empty return for Unit-returning functions
                    builder.set_return_terminator(None);
                }
            }
            // If the block has been terminated (e.g., by return statement), we don't need to check
            // or add another terminator
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

                // Get declared type and initialization instruction
                let (declared_type, init_inst) = if let Some(type_ann) = &let_stmt.type_annotation {
                    let declared_type =
                        convert_type_node_with_scope(&type_ann.ty, &self.global_scope)?;
                    // Use declared type as hint for inference
                    let init_inst = self.check_expression_with_hint(
                        &let_stmt.value,
                        builder,
                        Some(&declared_type),
                    )?;
                    (declared_type, init_inst)
                } else {
                    // For let statements without type annotations, infer from initializer
                    let init_inst =
                        self.check_expression_with_hint(&let_stmt.value, builder, None)?;
                    let inferred_type = builder.get_value_type(init_inst).unwrap_or(RueType::Unit);
                    (inferred_type, init_inst)
                };

                // Validate type compatibility
                let init_type = builder.get_value_type(init_inst).unwrap_or(RueType::Unit);
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
                let value_type = builder.get_value_type(value_inst).unwrap_or(RueType::Unit);
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
                        let expr_type = builder.get_value_type(expr_inst).unwrap_or(RueType::Unit);
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

                builder.set_return_terminator(return_inst);
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
    ) -> Result<ValueRef, SemanticError> {
        debug!(
            "check_expression_with_hint: processing {:?} with hint {:?}",
            std::mem::discriminant(expr),
            type_hint
        );
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
    ) -> Result<ValueRef, SemanticError> {
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
    ) -> Result<ValueRef, SemanticError> {
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
    ) -> Result<ValueRef, SemanticError> {
        builder.set_span(bin_expr.operator.span);

        // Convert operator
        let op = match &bin_expr.operator.kind {
            TokenKind::Plus => BinOp::Add,
            TokenKind::Minus => BinOp::Sub,
            TokenKind::Star => BinOp::Mul,
            TokenKind::Slash => BinOp::Div,
            TokenKind::Percent => BinOp::Mod,
            TokenKind::LessEqual => BinOp::Le,
            TokenKind::Less => BinOp::Lt,
            TokenKind::GreaterEqual => BinOp::Ge,
            TokenKind::Greater => BinOp::Gt,
            TokenKind::Equal => BinOp::Eq,
            TokenKind::NotEqual => BinOp::Ne,
            _ => {
                return Err(SemanticError {
                    message: format!("Invalid binary operator: {:?}", bin_expr.operator.kind),
                    span: bin_expr.operator.span,
                });
            }
        };

        // Check operands with improved type inference for arithmetic operations
        let (lhs_inst, rhs_inst) = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                // For arithmetic operations, use two-pass type inference:
                // 1. Check left operand first with any available type hint
                let lhs_inst =
                    self.check_expression_with_hint(&bin_expr.left, builder, type_hint)?;

                // 2. Get left operand's type to guide right operand inference
                let lhs_type = builder.get_value_type(lhs_inst).unwrap_or(RueType::Unit);

                // 3. Use left operand's type as hint for right operand, but only for numeric types
                let right_hint = match lhs_type {
                    RueType::I32 | RueType::I64 => Some(&lhs_type),
                    _ => type_hint,
                };

                let rhs_inst =
                    self.check_expression_with_hint(&bin_expr.right, builder, right_hint)?;

                (lhs_inst, rhs_inst)
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                // For comparison operations, don't pass type hints to operands
                let lhs_inst = self.check_expression_with_hint(&bin_expr.left, builder, None)?;
                let rhs_inst = self.check_expression_with_hint(&bin_expr.right, builder, None)?;
                (lhs_inst, rhs_inst)
            }
        };

        // Get operand types
        let lhs_ty = builder.get_value_type(lhs_inst).unwrap_or(RueType::Unit);
        let rhs_ty = builder.get_value_type(rhs_inst).unwrap_or(RueType::Unit);

        // Type checking for binary operations
        let result_type = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                // Arithmetic operations: operands must be same numeric type
                if lhs_ty != rhs_ty {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch in binary operation: {lhs_ty} {:?} {rhs_ty}",
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
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                // Comparison operations: operands must be same type, result is bool
                if lhs_ty != rhs_ty {
                    return Err(SemanticError {
                        message: format!("Type mismatch in comparison: {lhs_ty} {:?} {rhs_ty}", op),
                        span: bin_expr.operator.span,
                    });
                }
                RueType::Bool
            }
        };

        // Emit binary instruction
        Ok(builder.emit_binary(lhs_inst, rhs_inst, op, result_type))
    }

    fn check_unary_expression_with_hint(
        &mut self,
        unary_expr: &rue_ast::UnaryExprNode,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
    ) -> Result<ValueRef, SemanticError> {
        builder.set_span(unary_expr.operator.span);

        // For unary operations, we can pass the type hint to the operand
        let operand_inst =
            self.check_expression_with_hint(&unary_expr.operand, builder, type_hint)?;

        let op = match &unary_expr.operator.kind {
            TokenKind::Minus => UnaryOp::Neg,
            _ => {
                return Err(SemanticError {
                    message: format!("Invalid unary operator: {:?}", unary_expr.operator.kind),
                    span: unary_expr.operator.span,
                });
            }
        };

        // Get operand type
        let operand_ty = builder
            .get_value_type(operand_inst)
            .unwrap_or(RueType::Unit);

        // Type checking for unary operations
        let result_type = match op {
            UnaryOp::Neg => match operand_ty {
                RueType::I32 | RueType::I64 => operand_ty,
                _ => {
                    return Err(SemanticError {
                        message: format!("Unary negation not supported for type: {operand_ty}"),
                        span: unary_expr.operator.span,
                    });
                }
            },
        };

        // Emit unary instruction
        Ok(builder.emit_unary(operand_inst, op, result_type))
    }

    fn check_call_expression(
        &mut self,
        call: &rue_ast::CallExprNode,
        builder: &mut HirBuilder,
    ) -> Result<ValueRef, SemanticError> {
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

            let arg_type = builder.get_value_type(arg_inst).unwrap_or(RueType::Unit);
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
    ) -> Result<ValueRef, SemanticError> {
        debug!(
            "check_if_expression_with_hint called with type_hint: {:?}",
            type_hint
        );
        builder.set_span(if_expr.if_token.span);

        // STEP 1: Evaluate condition in current block
        debug!("Type checking if condition");
        let cond_inst = self.check_expression_with_hint(&if_expr.condition, builder, None)?;
        debug!("Condition instruction: {:?}", cond_inst);
        let cond_type = builder.get_value_type(cond_inst).unwrap_or(RueType::Unit);
        debug!("Condition type: {:?}", cond_type);
        if cond_type != RueType::Bool {
            debug!(
                "Condition type check failed - expected Bool, got {:?}",
                cond_type
            );
            return Err(SemanticError {
                message: format!("If condition must be bool, found {cond_type}"),
                span: if_expr.condition.span(),
            });
        }
        debug!("Condition type check passed");

        // STEP 2: Capture incoming environment after checking condition
        let incoming = builder.capture_environment(); // Vec<(BindingId, String, ValueRef, RueType)>
        let order: Vec<BindingId> = incoming.iter().map(|(b, _, _, _)| *b).collect();
        debug!(
            "Captured incoming environment with {} variables",
            incoming.len()
        );

        // STEP 3: Create then/else blocks with explicit parameters from captured environment
        let params: Vec<BlockParam> = incoming
            .iter()
            .map(|(_, n, _, ty)| BlockParam {
                name: builder.intern_string(n),
                ty: ty.clone(),
            })
            .collect();

        let then_block_id = builder.start_block_with_params(params.clone());
        builder.set_block_binding_order(then_block_id, order.clone());
        builder.end_block();

        let else_block_id = builder.start_block_with_params(params.clone());
        builder.set_block_binding_order(else_block_id, order.clone());
        builder.end_block();

        // STEP 3: We'll create the merge block after processing branches to get correct type

        // STEP 4: Pass environment args explicitly to set_if_terminator
        let env_args: Vec<ValueRef> = incoming.iter().map(|(_, _, v, _)| *v).collect();
        builder.set_if_terminator(
            cond_inst,
            then_block_id,
            env_args.clone(),
            else_block_id,
            env_args,
        );

        // STEP 5: Process then branch and track updates
        debug!("Processing then branch");
        builder.set_insertion_point(then_block_id);
        self.push_scope();

        // Track updates to incoming bindings during this branch (TODO: implement assignment tracking)
        use std::collections::HashMap;
        let _updates_then: HashMap<BindingId, ValueRef> = HashMap::new();

        for stmt in &if_expr.then_block.statements {
            self.check_statement(stmt, builder, None)?;
            // TODO: Track assignments to incoming variables here
            // For now, we'll capture the updated environment at the end
        }

        let then_value = if let Some(then_expr) = &if_expr.then_block.final_expr {
            self.check_expression_with_hint(then_expr, builder, type_hint)?
        } else {
            builder.emit_literal(0, RueType::Unit)
        };

        let then_type = builder.get_value_type(then_value).unwrap_or(RueType::Unit);
        self.pop_scope();

        // IMPORTANT: Capture where the then branch actually ended
        // After processing nested expressions, we might be in a different block
        let then_end_block = builder
            .get_current_block_id()
            .expect("Should have a current block after processing then branch");

        // STEP 5: Process else branch and track updates
        debug!("Processing else branch");
        builder.set_insertion_point(else_block_id);

        let (else_value, else_type, else_end_block, _updates_else) =
            if let Some(else_clause) = &if_expr.else_clause {
                let else_block_node = match &else_clause.body {
                    rue_ast::ElseBodyNode::Block(block) => block.as_ref(),
                    rue_ast::ElseBodyNode::If(_) => {
                        return Err(SemanticError {
                            message: "Else-if chains not yet supported in expressions".to_string(),
                            span: if_expr.if_token.span,
                        });
                    }
                };

                self.push_scope();

                // Track updates to incoming bindings during this branch (TODO: implement assignment tracking)
                let updates_else: HashMap<BindingId, ValueRef> = HashMap::new();

                for stmt in &else_block_node.statements {
                    self.check_statement(stmt, builder, None)?;
                    // TODO: Track assignments to incoming variables here
                    // For now, we'll capture the updated environment at the end
                }

                let else_val = if let Some(else_expr) = &else_block_node.final_expr {
                    self.check_expression_with_hint(else_expr, builder, type_hint)?
                } else {
                    builder.emit_literal(0, RueType::Unit)
                };

                let else_ty = builder.get_value_type(else_val).unwrap_or(RueType::Unit);
                self.pop_scope();

                // Capture where the else branch actually ended
                let else_end = builder
                    .get_current_block_id()
                    .expect("Should have a current block after processing else branch");

                (else_val, else_ty, else_end, updates_else)
            } else {
                // No else clause - use unit value and current block, no updates
                let unit_val = builder.emit_literal(0, RueType::Unit);
                let current_block = builder
                    .get_current_block_id()
                    .expect("Should have a current block");
                let _no_updates: HashMap<BindingId, ValueRef> = HashMap::new();
                (unit_val, RueType::Unit, current_block, _no_updates)
            };

        // STEP 6: Unify result types and validate
        let result_type = if then_type == else_type {
            then_type
        } else {
            return Err(SemanticError {
                message: format!(
                    "If expression branches have incompatible types: {} vs {}",
                    then_type, else_type
                ),
                span: if_expr.if_token.span,
            });
        };

        // STEP 6: Determine which variables are live in both branches
        // Only variables that are live (not shadowed) in ALL branches will be passed to merge

        builder.set_insertion_point(then_end_block);
        let then_env = builder.capture_environment();

        builder.set_insertion_point(else_end_block);
        let else_env = builder.capture_environment();

        // Find variables that are live in both branches
        let then_bindings: std::collections::HashSet<BindingId> =
            then_env.iter().map(|(bid, _, _, _)| *bid).collect();
        let else_bindings: std::collections::HashSet<BindingId> =
            else_env.iter().map(|(bid, _, _, _)| *bid).collect();

        let live_in_both: Vec<BindingId> = order
            .iter()
            .filter(|bid| then_bindings.contains(bid) && else_bindings.contains(bid))
            .cloned()
            .collect();

        debug!("Original variables: {:?}", order);
        debug!("Live in then: {:?}", then_bindings);
        debug!("Live in else: {:?}", else_bindings);
        debug!("Live in both: {:?}", live_in_both);

        // STEP 7: Create merge block with only live-in-both variables + result parameters
        let mut merge_params = Vec::new();
        for bid in &live_in_both {
            // Find the parameter info from the original environment
            let (_, name, _, ty) = incoming
                .iter()
                .find(|(orig_bid, _, _, _)| orig_bid == bid)
                .expect("Live variable should be in original environment");
            merge_params.push(BlockParam {
                name: builder.intern_string(name),
                ty: ty.clone(),
            });
        }
        merge_params.push(BlockParam {
            name: builder.intern_string("__if_result"),
            ty: result_type.clone(),
        });

        let merge_block_id = builder.start_block_with_params(merge_params);
        builder.set_block_binding_order(merge_block_id, live_in_both.clone());
        builder.end_block();

        // STEP 8: Set goto terminators from the ACTUAL END blocks to merge
        // Only pass variables that are live in both branches

        // For then branch: only pass live-in-both variables
        builder.set_insertion_point(then_end_block);
        let mut then_args = Vec::new();

        for bid in &live_in_both {
            let val = then_env
                .iter()
                .find(|(env_bid, _, _, _)| env_bid == bid)
                .map(|(_, _, val, _)| *val)
                .expect("Variable should be live in then branch");
            then_args.push(val);
        }
        then_args.push(then_value);
        builder.set_goto_terminator(merge_block_id, then_args);

        // For else branch: only pass live-in-both variables
        builder.set_insertion_point(else_end_block);
        let mut else_args = Vec::new();

        for bid in &live_in_both {
            let val = else_env
                .iter()
                .find(|(env_bid, _, _, _)| env_bid == bid)
                .map(|(_, _, val, _)| *val)
                .expect("Variable should be live in else branch");
            else_args.push(val);
        }
        else_args.push(else_value);
        builder.set_goto_terminator(merge_block_id, else_args);

        // STEP 9: Continue in merge block and return the parameter
        builder.set_insertion_point(merge_block_id);

        // STEP 10: Sync TypeChecker scope with HIR builder scope
        // The HIR builder has captured the environment, but our TypeChecker scope is out of sync
        // We need to rebuild our scope to match what's available in the merge block
        self.sync_scope_from_builder(builder);

        #[cfg(debug_assertions)]
        self.debug_assert_scopes_match(builder);

        // Return the merge block's __if_result parameter (it's the last parameter)
        let result_param_index = live_in_both.len() as u32;
        Ok(ValueRef::BlockParam {
            block_id: merge_block_id,
            index: BlockParamIndex::new(result_param_index),
        })
    }

    /// Compute the set of BindingIds that are written to in an AST node
    fn compute_writes(
        &self,
        node: &impl ComputeWrites,
        builder: &HirBuilder,
    ) -> HashSet<BindingId> {
        node.compute_writes(builder)
    }

    /// Compute the set of BindingIds that are read from in an AST node
    fn compute_reads(&self, node: &impl ComputeReads, builder: &HirBuilder) -> HashSet<BindingId> {
        node.compute_reads(builder)
    }

    fn check_while_expression(
        &mut self,
        while_expr: &rue_ast::WhileStatementNode,
        builder: &mut HirBuilder,
    ) -> Result<ValueRef, SemanticError> {
        builder.set_span(while_expr.while_token.span);
        debug!("Starting while loop semantic analysis");

        // First, compute WRITES and READS to determine loop-carried variables
        let writes = self.compute_writes(&while_expr.body, builder);
        let reads_cond = self.compute_reads(&while_expr.condition, builder);
        let reads_body = self.compute_reads(&while_expr.body, builder);

        // LC = WRITES ∪ READS (from both condition and body)
        let mut lc_set: HashSet<BindingId> = writes.clone();
        lc_set.extend(reads_cond);
        lc_set.extend(reads_body);

        debug!(
            "Loop analysis: {} writes, {} total loop-carried variables",
            writes.len(),
            lc_set.len()
        );

        // Get current environment and filter to only loop-carried variables
        let all_env = builder.capture_environment();

        // Filter to only loop-carried variables and maintain stable order
        let loop_carried_env: Vec<(BindingId, String, ValueRef, RueType)> = all_env
            .iter()
            .filter(|(bid, _, _, _)| lc_set.contains(bid))
            .cloned()
            .collect();

        // Keep track of the BindingIds in order for later use
        let loop_carried_bindings: Vec<BindingId> =
            loop_carried_env.iter().map(|(id, _, _, _)| *id).collect();
        let loop_carried_vars: Vec<String> = loop_carried_env
            .iter()
            .map(|(_, name, _, _)| name.clone())
            .collect();
        let _loop_carried_types: Vec<RueType> = loop_carried_env
            .iter()
            .map(|(_, _, _, ty)| ty.clone())
            .collect();

        debug!(
            "Captured {} variables for while loop",
            loop_carried_env.len()
        );

        // Pass the full carried environment with BindingIds to preserve identity
        let ctx = builder.while_begin(loop_carried_env.clone());
        debug!(
            "Created while loop context with {} captured variables",
            loop_carried_vars.len()
        );

        // Set up condition evaluation in header block
        // First, switch to header and set up scope
        builder.set_insertion_point(ctx.header_block_id);

        // Use the HIR builder method to set up the loop condition and branching
        let _header_values = builder.while_set_cond(&ctx, |builder, _header_vals| {
            // The header block has parameters for loop-carried variables
            // They're automatically in scope via set_insertion_point
            self.check_expression_with_hint(&while_expr.condition, builder, Some(&RueType::Bool))
                .expect("while loop condition should type check successfully")
        });

        // No need to prepare exit from header - args are passed explicitly in while_set_cond
        debug!("Set while loop condition and prepared exit");

        // Process the body block
        // Switch to body block
        builder.set_insertion_point(ctx.body_block_id);

        // Get body parameter values - these are the loop-carried values passed from the header
        // We need these to pass through unmodified variables
        let body_param_values: Vec<ValueRef> = (0..loop_carried_vars.len())
            .map(|i| {
                ValueRef::from_block_param_with_id(
                    ctx.body_block_id,
                    BlockParamIndex::new(i as u32),
                )
            })
            .collect();

        // Track variable updates in the loop body
        // Don't create a new scope - the loop body shares the function's scope
        let updated_values =
            self.check_block_with_loop_updates(&while_expr.body, builder, &loop_carried_bindings)?;

        // Build backedge args in the SAME order as loop_carried_bindings, using updates when present.
        let backedge_values: Vec<ValueRef> = loop_carried_bindings
            .iter()
            .enumerate()
            .map(|(i, binding_id)| {
                if let Some(&updated_value) = updated_values.get(binding_id) {
                    debug!(
                        "Using UPDATED value for loop var {:?} at idx {}: {:?}",
                        binding_id, i, updated_value
                    );
                    updated_value
                } else {
                    debug!(
                        "Passing through body param for UNMODIFIED loop var {:?} at idx {}",
                        binding_id, i
                    );
                    body_param_values[i]
                }
            })
            .collect();

        // Optional: quick arity/type check against body param types
        debug_assert_eq!(
            backedge_values.len(),
            body_param_values.len(),
            "backedge arity mismatch"
        );
        for (i, arg) in backedge_values.iter().enumerate() {
            let got = builder.get_value_type(*arg).unwrap_or(RueType::Unit);
            let expect = builder
                .get_value_type(body_param_values[i])
                .unwrap_or(RueType::Unit);
            debug_assert_eq!(
                got, expect,
                "backedge type mismatch at index {}: got {:?}, expected {:?}",
                i, got, expect
            );
        }

        // If there were any writes in the loop body, at least one carried binding must be updated
        if !loop_carried_bindings.is_empty() {
            let _any_update = loop_carried_bindings
                .iter()
                .any(|b| updated_values.contains_key(b));
            debug!(
                "Loop has {} carried vars, {} have updates",
                loop_carried_bindings.len(),
                updated_values.len()
            );
        }

        // Ensure at least one arg differs if an update was recorded for that binding
        for (i, binding_id) in loop_carried_bindings.iter().enumerate() {
            if updated_values.contains_key(binding_id) {
                debug_assert_ne!(
                    backedge_values[i], body_param_values[i],
                    "Updated value not used on backedge at idx {} for binding {:?}",
                    i, binding_id
                );
            }
        }

        // Complete the loop with the backedge
        builder.while_backedge(&ctx, backedge_values);
        debug!(
            "Created loop backedge with {} values",
            loop_carried_vars.len()
        );

        // Switch to exit block to continue
        builder.set_insertion_point(ctx.exit_block_id);
        debug!("Completed while loop, continuing at exit block");

        // Sync TypeChecker scope with HIR builder scope after exiting the while loop
        // The HIR builder has captured the environment and created exit block parameters
        self.sync_scope_from_builder(builder);

        #[cfg(debug_assertions)]
        self.debug_assert_scopes_match(builder);

        // While loops evaluate to unit
        Ok(builder.emit_literal(0, RueType::Unit))
    }

    /// Process a block's statements and track variable updates for loop-carried dependencies
    fn check_block_with_loop_updates(
        &mut self,
        block: &rue_ast::BlockNode,
        builder: &mut HirBuilder,
        loop_carried: &[BindingId],
    ) -> Result<HashMap<BindingId, ValueRef>, SemanticError> {
        let mut updates = HashMap::new();

        // Process statements
        for stmt in &block.statements {
            match stmt {
                StatementNode::Let(_) => {
                    // Let statements create new bindings (shadowing)
                    // They should NOT be treated as updates to loop-carried variables
                    // Process normally to create the new binding
                    self.check_statement(stmt, builder, None)?;
                    debug!("Let statement creates new binding, not a loop update");
                }
                StatementNode::Assign(assign_stmt) => {
                    // Get the variable name
                    let var_name = match &assign_stmt.name.kind {
                        TokenKind::Ident(name) => name.clone(),
                        _ => {
                            return Err(SemanticError {
                                message: "Expected variable name".to_string(),
                                span: assign_stmt.name.span,
                            });
                        }
                    };

                    // Look up the BindingId for this variable
                    let (binding_id, var_type) =
                        if let Some((bid, _, ty)) = builder.lookup_variable(&var_name) {
                            (bid, ty)
                        } else {
                            // Fall back to semantic scope lookup
                            let _var_type =
                                self.lookup_variable(&var_name).cloned().ok_or_else(|| {
                                    SemanticError {
                                        message: format!("Undefined variable: {var_name}"),
                                        span: assign_stmt.name.span,
                                    }
                                })?;
                            // We don't have the binding ID from the semantic scope,
                            // so we can't track this update properly
                            // This shouldn't happen if builder scope is properly maintained
                            return Err(SemanticError {
                                message: format!(
                                    "Internal error: variable {} not found in builder scope",
                                    var_name
                                ),
                                span: assign_stmt.name.span,
                            });
                        };

                    // Type check the value
                    let value_inst = self.check_expression_with_hint(
                        &assign_stmt.value,
                        builder,
                        Some(&var_type),
                    )?;

                    // Validate type compatibility
                    let value_type = builder.get_value_type(value_inst).unwrap_or(RueType::Unit);
                    if value_type != var_type {
                        return Err(SemanticError {
                            message: format!(
                                "Type mismatch in assignment: expected {var_type}, found {value_type}"
                            ),
                            span: assign_stmt.value.span(),
                        });
                    }

                    // Always emit assign instruction so the variable is updated in the current iteration
                    builder
                        .emit_assign(&var_name, value_inst)
                        .map_err(|e| SemanticError {
                            message: e,
                            span: assign_stmt.name.span,
                        })?;

                    // Check if this is a loop-carried variable that we're writing to
                    debug!(
                        "Assignment to {} (BindingId: {:?}), checking if in loop-carried set",
                        var_name, binding_id
                    );
                    debug!("Loop-carried set contains: {:?}", loop_carried);
                    if loop_carried.contains(&binding_id) {
                        // Track the updated value for backedge
                        updates.insert(binding_id, value_inst);
                        debug!(
                            "Tracking update to loop-carried variable: {} (BindingId: {:?})",
                            var_name, binding_id
                        );
                    } else {
                        debug!(
                            "Assignment to non-carried binding {:?} ({}), not threading through backedge",
                            binding_id, var_name
                        );
                    }
                }
                _ => {
                    // Handle other statements normally
                    self.check_statement(stmt, builder, None)?;
                }
            }
        }

        // Process final expression if present
        if let Some(final_expr) = &block.final_expr {
            self.check_expression_with_hint(final_expr, builder, None)?;
        }

        Ok(updates)
    }

    fn check_struct_literal(
        &mut self,
        struct_lit: &StructLiteralNode,
        builder: &mut HirBuilder,
    ) -> Result<ValueRef, SemanticError> {
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

        // Type check each field and collect them in a map
        let mut field_insts_map = HashMap::new();
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
            let field_type = builder.get_value_type(field_inst).unwrap_or(RueType::Unit);
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
            field_insts_map.insert(field_name, field_inst);
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

        // Collect field values in the correct order (as defined in the struct)
        let mut field_values = Vec::new();
        for (field_name, _) in &struct_def.fields {
            let field_inst = field_insts_map
                .get(field_name)
                .expect("Field should exist, we already validated all fields are present");
            field_values.push(*field_inst);
        }

        // Create the struct type and emit the struct literal instruction
        let struct_type = RueType::Struct(struct_def.id);
        Ok(builder.build_struct_literal(struct_def.id, field_values, struct_type))
    }

    fn check_field_access(
        &mut self,
        field_access: &FieldAccessNode,
        builder: &mut HirBuilder,
    ) -> Result<ValueRef, SemanticError> {
        builder.set_span(field_access.dot.span);

        let base_inst = self.check_expression_with_hint(&field_access.base, builder, None)?;
        let base_type = builder.get_value_type(base_inst).unwrap_or(RueType::Unit);

        match base_type {
            RueType::Struct(struct_id) => {
                // Handle struct field access (e.g., struct.field_name)
                match &field_access.field {
                    FieldKindNode::Named(token) => {
                        // Extract field name from the token
                        let field_name = match &token.kind {
                            rue_lexer::TokenKind::Ident(name) => name.clone(),
                            _ => {
                                return Err(SemanticError {
                                    message: "Expected field name for struct field access"
                                        .to_string(),
                                    span: token.span,
                                });
                            }
                        };

                        // Look up the struct definition
                        let struct_def =
                            self.global_scope
                                .get_struct_by_id(struct_id)
                                .ok_or_else(|| SemanticError {
                                    message: format!(
                                        "Internal error: struct with ID {} not found",
                                        struct_id
                                    ),
                                    span: field_access.dot.span,
                                })?;

                        // Find the field index and type
                        let (field_index, field_type) = struct_def
                            .fields
                            .iter()
                            .enumerate()
                            .find(|(_, (name, _))| name == &field_name)
                            .map(|(idx, (_, ty))| (idx, ty.clone()))
                            .ok_or_else(|| SemanticError {
                                message: format!(
                                    "Field '{}' not found in struct '{}'",
                                    field_name, struct_def.name
                                ),
                                span: token.span,
                            })?;

                        // Create the FieldAccess instruction
                        Ok(builder.emit_field_access(base_inst, field_index as u32, field_type))
                    }
                    FieldKindNode::Positional(_) => Err(SemanticError {
                        message: "Positional field access is not supported for structs".to_string(),
                        span: field_access.dot.span,
                    }),
                }
            }
            RueType::Tuple(element_types) => {
                // Handle tuple field access (e.g., tuple.0, tuple.1)
                match &field_access.field {
                    FieldKindNode::Positional(token) => {
                        // Extract field index from the token
                        let field_index = match &token.kind {
                            rue_lexer::TokenKind::Integer(n) => *n as usize,
                            _ => {
                                return Err(SemanticError {
                                    message: "Expected integer literal for tuple field access"
                                        .to_string(),
                                    span: token.span,
                                });
                            }
                        };

                        // Check if the field index is valid
                        if field_index >= element_types.len() {
                            return Err(SemanticError {
                                message: format!(
                                    "Tuple field index out of bounds: tuple has {} fields, but tried to access field {}",
                                    element_types.len(),
                                    field_index
                                ),
                                span: token.span,
                            });
                        }

                        // Get the type of the accessed field
                        let field_type = element_types[field_index].clone();

                        // Create the FieldAccess instruction
                        Ok(builder.emit_field_access(base_inst, field_index as u32, field_type))
                    }
                    FieldKindNode::Named(_) => Err(SemanticError {
                        message: "Named field access is not supported for tuples".to_string(),
                        span: field_access.dot.span,
                    }),
                }
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
    ) -> Result<ValueRef, SemanticError> {
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
                .get_value_type(element_inst)
                .unwrap_or(RueType::Unit);
            element_types.push(element_type);
            element_insts.push(element_inst);
        }

        // Create the tuple type and build the literal
        let tuple_type = RueType::Tuple(element_types);

        // Validate against type hint if provided
        if let Some(expected_type) = type_hint
            && tuple_type != *expected_type
        {
            return Err(SemanticError {
                message: format!(
                    "Tuple literal type mismatch: expected {}, found {}",
                    expected_type, tuple_type
                ),
                span: tuple_lit.open_paren.span,
            });
        }

        Ok(builder.build_tuple_literal(element_insts, tuple_type))
    }

    fn check_array_literal_with_hint(
        &mut self,
        array_lit: &ArrayLiteralNode,
        builder: &mut HirBuilder,
        type_hint: Option<&RueType>,
    ) -> Result<ValueRef, SemanticError> {
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

                // Create empty array literal with the correct type
                let array_type = type_hint.expect("type_hint should be Some(Array) in this branch").clone();
                return Ok(builder.build_array_literal(vec![], array_type));
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
                .get_value_type(element_inst)
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

        // Create the array type and build the literal
        let element_type = element_type.expect("element_type should be Some after validating elements");
        let array_size = element_insts.len();
        let array_type = RueType::Array(Box::new(element_type), array_size);

        // Validate against type hint if provided
        if let Some(expected_type) = type_hint
            && array_type != *expected_type
        {
            return Err(SemanticError {
                message: format!(
                    "Array literal type mismatch: expected {}, found {}",
                    expected_type, array_type
                ),
                span: array_lit.open_bracket.span,
            });
        }

        Ok(builder.build_array_literal(element_insts, array_type))
    }

    fn check_array_access(
        &mut self,
        array_access: &ArrayAccessNode,
        builder: &mut HirBuilder,
    ) -> Result<ValueRef, SemanticError> {
        builder.set_span(array_access.open_bracket.span);

        let base_inst = self.check_expression_with_hint(&array_access.base, builder, None)?;
        // Check array index without forcing a specific type, allow both i32 and i64
        let index_inst = self.check_expression_with_hint(&array_access.index, builder, None)?;

        // Validate index type - must be an integer type
        let index_type = builder.get_value_type(index_inst).unwrap_or(RueType::Unit);
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
        let base_type = builder.get_value_type(base_inst).unwrap_or(RueType::Unit);
        let element_type = match base_type {
            RueType::Array(element_type, _) => (*element_type).clone(),
            _ => {
                return Err(SemanticError {
                    message: format!("Cannot index into type {base_type}, expected array"),
                    span: array_access.base.span(),
                });
            }
        };

        // Create the IndexAccess instruction using HIRBuilder
        Ok(builder.emit_index_access(base_inst, index_inst, element_type))
    }
}
// Implementations of ComputeWrites trait

impl ComputeWrites for rue_ast::BlockNode {
    fn compute_writes(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        let mut writes = HashSet::new();
        for stmt in &self.statements {
            writes.extend(stmt.compute_writes(builder));
        }
        if let Some(expr) = &self.final_expr {
            writes.extend(expr.compute_writes(builder));
        }
        writes
    }
}

impl ComputeWrites for StatementNode {
    fn compute_writes(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        match self {
            StatementNode::Assign(assign) => {
                // Assignment writes to a variable
                if let TokenKind::Ident(name) = &assign.name.kind {
                    // Look up the BindingId for this variable name
                    if let Some((binding_id, _, _)) = builder.lookup_variable(name) {
                        let mut writes = HashSet::new();
                        writes.insert(binding_id);
                        // Also check for writes in the RHS expression
                        writes.extend(assign.value.compute_writes(builder));
                        return writes;
                    }
                }
                // If we can't find the binding, just check the RHS
                assign.value.compute_writes(builder)
            }
            StatementNode::Let(let_stmt) => {
                // Let creates a new binding, not a write to existing
                // But check the initializer for writes
                let_stmt.value.compute_writes(builder)
            }
            StatementNode::Expression(expr_stmt) => expr_stmt.expression.compute_writes(builder),
            StatementNode::Return(ret_stmt) => {
                if let Some(expr) = &ret_stmt.expression {
                    expr.compute_writes(builder)
                } else {
                    HashSet::new()
                }
            }
        }
    }
}

impl ComputeWrites for ExpressionNode {
    fn compute_writes(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        match self {
            ExpressionNode::Binary(bin) => {
                let mut writes = bin.left.compute_writes(builder);
                writes.extend(bin.right.compute_writes(builder));
                writes
            }
            ExpressionNode::Unary(un) => un.operand.compute_writes(builder),
            ExpressionNode::Call(call) => {
                let mut writes = call.function.compute_writes(builder);
                for arg in &call.args {
                    writes.extend(arg.compute_writes(builder));
                }
                writes
            }
            ExpressionNode::If(if_expr) => {
                let mut writes = if_expr.condition.compute_writes(builder);
                writes.extend(if_expr.then_block.compute_writes(builder));
                if let Some(else_clause) = &if_expr.else_clause {
                    // else_clause is ElseClauseNode which contains a BlockNode
                    writes.extend(else_clause.body.compute_writes(builder));
                }
                writes
            }
            ExpressionNode::While(while_expr) => {
                let mut writes = while_expr.condition.compute_writes(builder);
                writes.extend(while_expr.body.compute_writes(builder));
                writes
            }
            ExpressionNode::StructLiteral(struct_lit) => {
                let mut writes = HashSet::new();
                for field in &struct_lit.fields {
                    writes.extend(field.value.compute_writes(builder));
                }
                writes
            }
            ExpressionNode::FieldAccess(field_acc) => field_acc.base.compute_writes(builder),
            ExpressionNode::ArrayAccess(array_acc) => {
                let mut writes = array_acc.base.compute_writes(builder);
                writes.extend(array_acc.index.compute_writes(builder));
                writes
            }
            ExpressionNode::TupleLiteral(tuple_lit) => {
                let mut writes = HashSet::new();
                for elem in &tuple_lit.elements {
                    writes.extend(elem.compute_writes(builder));
                }
                writes
            }
            ExpressionNode::ArrayLiteral(array_lit) => {
                let mut writes = HashSet::new();
                for elem in &array_lit.elements {
                    writes.extend(elem.compute_writes(builder));
                }
                writes
            }
            // Literals and identifiers don't write
            ExpressionNode::Literal(_) | ExpressionNode::Identifier(_) => HashSet::new(),
        }
    }
}

// Implementations of ComputeReads trait

impl ComputeReads for rue_ast::BlockNode {
    fn compute_reads(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        let mut reads = HashSet::new();
        for stmt in &self.statements {
            reads.extend(stmt.compute_reads(builder));
        }
        if let Some(expr) = &self.final_expr {
            reads.extend(expr.compute_reads(builder));
        }
        reads
    }
}

impl ComputeReads for StatementNode {
    fn compute_reads(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        match self {
            StatementNode::Assign(assign) => {
                // Assignment reads from the RHS
                assign.value.compute_reads(builder)
            }
            StatementNode::Let(let_stmt) => {
                // Let reads from the initializer
                let_stmt.value.compute_reads(builder)
            }
            StatementNode::Expression(expr_stmt) => expr_stmt.expression.compute_reads(builder),
            StatementNode::Return(ret_stmt) => {
                if let Some(expr) = &ret_stmt.expression {
                    expr.compute_reads(builder)
                } else {
                    HashSet::new()
                }
            }
        }
    }
}

impl ComputeReads for ExpressionNode {
    fn compute_reads(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        match self {
            ExpressionNode::Identifier(ident) => {
                // Identifier is a read
                if let TokenKind::Ident(name) = &ident.kind {
                    // Look up in builder's scope
                    if let Some((binding_id, _, _)) = builder.lookup_variable(name) {
                        let mut reads = HashSet::new();
                        reads.insert(binding_id);
                        return reads;
                    }
                }
                HashSet::new()
            }
            ExpressionNode::Binary(bin) => {
                let mut reads = bin.left.compute_reads(builder);
                reads.extend(bin.right.compute_reads(builder));
                reads
            }
            ExpressionNode::Unary(un) => un.operand.compute_reads(builder),
            ExpressionNode::Call(call) => {
                let mut reads = call.function.compute_reads(builder);
                for arg in &call.args {
                    reads.extend(arg.compute_reads(builder));
                }
                reads
            }
            ExpressionNode::If(if_expr) => {
                let mut reads = if_expr.condition.compute_reads(builder);
                reads.extend(if_expr.then_block.compute_reads(builder));
                if let Some(else_clause) = &if_expr.else_clause {
                    reads.extend(else_clause.body.compute_reads(builder));
                }
                reads
            }
            ExpressionNode::While(while_expr) => {
                let mut reads = while_expr.condition.compute_reads(builder);
                reads.extend(while_expr.body.compute_reads(builder));
                reads
            }
            ExpressionNode::StructLiteral(struct_lit) => {
                let mut reads = HashSet::new();
                for field in &struct_lit.fields {
                    reads.extend(field.value.compute_reads(builder));
                }
                reads
            }
            ExpressionNode::FieldAccess(field_acc) => field_acc.base.compute_reads(builder),
            ExpressionNode::ArrayAccess(array_acc) => {
                let mut reads = array_acc.base.compute_reads(builder);
                reads.extend(array_acc.index.compute_reads(builder));
                reads
            }
            ExpressionNode::TupleLiteral(tuple_lit) => {
                let mut reads = HashSet::new();
                for elem in &tuple_lit.elements {
                    reads.extend(elem.compute_reads(builder));
                }
                reads
            }
            ExpressionNode::ArrayLiteral(array_lit) => {
                let mut reads = HashSet::new();
                for elem in &array_lit.elements {
                    reads.extend(elem.compute_reads(builder));
                }
                reads
            }
            // Literals don't read
            ExpressionNode::Literal(_) => HashSet::new(),
        }
    }
}

impl ComputeWrites for rue_ast::ElseBodyNode {
    fn compute_writes(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        match self {
            rue_ast::ElseBodyNode::Block(block) => block.compute_writes(builder),
            rue_ast::ElseBodyNode::If(if_stmt) => if_stmt.compute_writes(builder),
        }
    }
}

impl ComputeReads for rue_ast::ElseBodyNode {
    fn compute_reads(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        match self {
            rue_ast::ElseBodyNode::Block(block) => block.compute_reads(builder),
            rue_ast::ElseBodyNode::If(if_stmt) => if_stmt.compute_reads(builder),
        }
    }
}

impl ComputeWrites for rue_ast::IfStatementNode {
    fn compute_writes(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        let mut writes = self.then_block.compute_writes(builder);
        if let Some(ref else_clause) = self.else_clause {
            writes.extend(else_clause.body.compute_writes(builder));
        }
        writes
    }
}

impl ComputeReads for rue_ast::IfStatementNode {
    fn compute_reads(&self, builder: &HirBuilder) -> HashSet<BindingId> {
        let mut reads = self.condition.compute_reads(builder);
        reads.extend(self.then_block.compute_reads(builder));
        if let Some(ref else_clause) = self.else_clause {
            reads.extend(else_clause.body.compute_reads(builder));
        }
        reads
    }
}
