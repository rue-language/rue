//! Type checker that builds HIR during semantic analysis
//!
//! This module provides a unified type checker that performs semantic analysis
//! and HIR construction in a single pass, ensuring that all type information
//! is accurately preserved in the HIR.

use crate::{FunctionSignature, Scope, SemanticError};
use rue_ast::{
    ArrayAccessNode, ArrayLiteralNode, CstRoot, ExpressionNode, FieldAccessNode, FieldKindNode,
    FunctionNode, StatementNode, StructLiteralNode, TupleLiteralNode,
};
use rue_ir::hir::{
    BinOp, HirBlock, HirExpr, HirFunction, HirLiteral, HirProgram, HirStatement, UnaryOp,
};
use rue_ir::types::{FieldId, RueType};
use rue_lexer::{Span, TokenKind};
use std::collections::HashMap;

/// Simple scope for variable type tracking
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

    /// Check if an expression is a numeric literal that can be contextually inferred
    fn is_numeric_literal(&self, expr: &ExpressionNode) -> bool {
        match expr {
            ExpressionNode::Literal(lit) => matches!(lit.kind, TokenKind::Integer(_)),
            _ => false,
        }
    }

    /// Type check the entire program and build HIR
    pub fn check_program(&mut self, ast: &CstRoot) -> Result<HirProgram, SemanticError> {
        let mut functions = Vec::new();

        for item in &ast.items {
            match item {
                rue_ast::CstNode::Function(func) => {
                    let hir_func = self.check_function(func)?;
                    functions.push(hir_func);
                }
                rue_ast::CstNode::StructDefinition(_) => {
                    // Struct definitions are already processed in the first pass
                    // by analyze_cst, so we skip them here
                }
                rue_ast::CstNode::Statement(_) => {
                    return Err(SemanticError {
                        message: "Top-level statements are not supported".to_string(),
                        span: Span { start: 0, end: 0 },
                    });
                }
                rue_ast::CstNode::Expression(_)
                | rue_ast::CstNode::Token(_)
                | rue_ast::CstNode::Error(_) => {
                    return Err(SemanticError {
                        message: "Unexpected top-level node type".to_string(),
                        span: Span { start: 0, end: 0 },
                    });
                }
            }
        }

        Ok(HirProgram { functions })
    }

    /// Type check a function and build HIR
    fn check_function(&mut self, func: &FunctionNode) -> Result<HirFunction, SemanticError> {
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

        // Create new scope for function parameters
        self.push_scope();

        // Add parameters to current scope
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
            self.current_scope_mut().insert(param_name, param_type);
        }

        // Type check function body
        let body = self.check_block(&func.body, Some(&signature.return_type))?;

        // Pop function scope
        self.pop_scope();

        Ok(HirFunction {
            name,
            params,
            return_type: signature.return_type,
            body,
            span: func.name.span,
        })
    }

    /// Type check a block and build HIR
    fn check_block(
        &mut self,
        block: &rue_ast::BlockNode,
        expected_return_type: Option<&RueType>,
    ) -> Result<HirBlock, SemanticError> {
        // Create new scope for the block
        self.push_scope();

        let mut statements = Vec::new();
        let mut expr = None;

        // Process all statements
        for stmt in &block.statements {
            let hir_stmt = self.check_statement(stmt)?;
            statements.push(hir_stmt);
        }

        // Process final expression if present
        if let Some(final_expression) = &block.final_expr {
            let hir_expr = if let Some(expected_type) = expected_return_type {
                // Use expected return type as hint for inference
                self.check_expression_with_hint(final_expression, Some(expected_type))?
            } else {
                self.check_expression(final_expression)?
            };
            expr = Some(Box::new(hir_expr));
        }

        // Validate return type if expected
        if let Some(expected_type) = expected_return_type {
            let block_type = if let Some(ref final_expr) = expr {
                final_expr.ty()
            } else {
                &RueType::Unit
            };

            if block_type != expected_type {
                return Err(SemanticError {
                    message: format!(
                        "Type mismatch: Expected return type {expected_type}, found {block_type}"
                    ),
                    span: block.open_brace.span,
                });
            }
        }

        // Pop block scope
        self.pop_scope();

        Ok(HirBlock { statements, expr })
    }

    /// Type check a statement and build HIR
    fn check_statement(&mut self, stmt: &StatementNode) -> Result<HirStatement, SemanticError> {
        match stmt {
            StatementNode::Let(let_stmt) => {
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
                    crate::convert_type_node_with_scope(&type_ann.ty, &self.global_scope)?
                } else {
                    // For now, we'll check expression first, then infer
                    let temp_expr = self.check_expression(&let_stmt.value)?;
                    temp_expr.ty().clone()
                };

                // Type check initialization expression with type hint
                let init_expr = if let_stmt.type_annotation.is_some() {
                    // Use declared type as hint for inference
                    self.check_expression_with_hint(&let_stmt.value, Some(&declared_type))?
                } else {
                    // No type annotation, infer from expression
                    self.check_expression(&let_stmt.value)?
                };

                // Validate type compatibility
                if init_expr.ty() != &declared_type {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch: expected {declared_type}, found {}",
                            init_expr.ty()
                        ),
                        span: let_stmt.value.span(),
                    });
                }

                // Add variable to current scope
                self.current_scope_mut()
                    .insert(var_name.clone(), declared_type.clone());

                Ok(HirStatement::Let {
                    name: var_name,
                    ty: declared_type,
                    init: init_expr,
                    span: let_stmt.let_token.span,
                })
            }
            StatementNode::Assign(assign_stmt) => {
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
                let value_expr =
                    self.check_expression_with_hint(&assign_stmt.value, Some(&var_type))?;

                // Validate type compatibility
                if value_expr.ty() != &var_type {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch in assignment: expected {var_type}, found {}",
                            value_expr.ty()
                        ),
                        span: assign_stmt.value.span(),
                    });
                }

                Ok(HirStatement::Assign {
                    name: var_name,
                    value: value_expr,
                    span: assign_stmt.name.span,
                })
            }
            StatementNode::Expression(expr_stmt) => {
                let hir_expr = self.check_expression(&expr_stmt.expression)?;
                Ok(HirStatement::Expr(hir_expr))
            }
        }
    }

    /// Type check an expression and build HIR
    fn check_expression(&mut self, expr: &ExpressionNode) -> Result<HirExpr, SemanticError> {
        self.check_expression_with_hint(expr, None)
    }

    /// Type check an expression with an optional type hint for inference
    fn check_expression_with_hint(
        &mut self,
        expr: &ExpressionNode,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        match expr {
            ExpressionNode::Literal(lit) => self.check_literal_with_hint(lit, type_hint),
            ExpressionNode::Identifier(ident) => self.check_identifier(ident),
            ExpressionNode::Binary(bin_expr) => self.check_binary_expression(bin_expr),
            ExpressionNode::Unary(unary_expr) => self.check_unary_expression(unary_expr),
            ExpressionNode::Call(call) => self.check_call_expression(call),
            ExpressionNode::If(if_expr) => self.check_if_expression(if_expr),
            ExpressionNode::While(while_expr) => self.check_while_expression(while_expr),
            ExpressionNode::StructLiteral(struct_lit) => self.check_struct_literal(struct_lit),
            ExpressionNode::FieldAccess(field_access) => self.check_field_access(field_access),
            ExpressionNode::TupleLiteral(tuple_lit) => self.check_tuple_literal(tuple_lit),
            ExpressionNode::ArrayLiteral(array_lit) => self.check_array_literal(array_lit),
            ExpressionNode::ArrayAccess(array_access) => self.check_array_access(array_access),
        }
    }

    fn check_literal_with_hint(
        &mut self,
        lit: &rue_lexer::Token,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        let hir_lit = match &lit.kind {
            TokenKind::Integer(n) => {
                // Use type hint for inference, default to i32 if no hint
                match type_hint {
                    Some(RueType::I64) => HirLiteral::Int64(*n),
                    Some(RueType::I32) | None => HirLiteral::Int32(*n as i32),
                    Some(other_type) => {
                        return Err(SemanticError {
                            message: format!(
                                "Cannot use integer literal in context expecting {other_type}"
                            ),
                            span: lit.span,
                        });
                    }
                }
            }
            TokenKind::True => HirLiteral::Bool(true),
            TokenKind::False => HirLiteral::Bool(false),
            TokenKind::Unit => HirLiteral::Unit,
            _ => {
                return Err(SemanticError {
                    message: format!("Invalid literal: {:?}", lit.kind),
                    span: lit.span,
                });
            }
        };

        Ok(HirExpr::Literal {
            lit: hir_lit,
            span: lit.span,
        })
    }

    fn check_identifier(&mut self, ident: &rue_lexer::Token) -> Result<HirExpr, SemanticError> {
        let name = match &ident.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => {
                return Err(SemanticError {
                    message: "Expected identifier".to_string(),
                    span: ident.span,
                });
            }
        };

        let var_type = self
            .lookup_variable(&name)
            .cloned()
            .ok_or_else(|| SemanticError {
                message: format!("Undefined variable: {name}"),
                span: ident.span,
            })?;

        Ok(HirExpr::Var {
            name,
            ty: var_type,
            span: ident.span,
        })
    }

    fn check_binary_expression(
        &mut self,
        bin_expr: &rue_ast::BinaryExprNode,
    ) -> Result<HirExpr, SemanticError> {
        // First try with no hints to see what we get
        let lhs = self.check_expression(&bin_expr.left)?;
        let rhs = self.check_expression(&bin_expr.right)?;

        // If operands have different types, try contextual inference for numeric literals
        let (lhs, rhs) = if lhs.ty() != rhs.ty() {
            // Check if we can apply contextual inference
            if self.is_numeric_literal(&bin_expr.left)
                && matches!(rhs.ty(), RueType::I32 | RueType::I64)
            {
                // Re-check lhs with rhs type as hint
                let lhs_with_hint =
                    self.check_expression_with_hint(&bin_expr.left, Some(rhs.ty()))?;
                (lhs_with_hint, rhs)
            } else if self.is_numeric_literal(&bin_expr.right)
                && matches!(lhs.ty(), RueType::I32 | RueType::I64)
            {
                // Re-check rhs with lhs type as hint
                let rhs_with_hint =
                    self.check_expression_with_hint(&bin_expr.right, Some(lhs.ty()))?;
                (lhs, rhs_with_hint)
            } else {
                (lhs, rhs)
            }
        } else {
            (lhs, rhs)
        };

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

        // Type checking for binary operations
        let result_type = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                // Arithmetic operations: operands must be same numeric type
                if lhs.ty() != rhs.ty() {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch in binary operation: {} {} {}",
                            lhs.ty(),
                            op,
                            rhs.ty()
                        ),
                        span: bin_expr.operator.span,
                    });
                }
                match lhs.ty() {
                    RueType::I32 | RueType::I64 => lhs.ty().clone(),
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
                if lhs.ty() != rhs.ty() {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch in comparison: {} {} {}",
                            lhs.ty(),
                            op,
                            rhs.ty()
                        ),
                        span: bin_expr.operator.span,
                    });
                }
                RueType::Bool
            }
        };

        Ok(HirExpr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            ty: result_type,
            span: bin_expr.operator.span,
        })
    }

    fn check_unary_expression(
        &mut self,
        unary_expr: &rue_ast::UnaryExprNode,
    ) -> Result<HirExpr, SemanticError> {
        let expr = self.check_expression(&unary_expr.operand)?;

        let op = match &unary_expr.operator.kind {
            TokenKind::Minus => UnaryOp::Neg,
            _ => {
                return Err(SemanticError {
                    message: format!("Invalid unary operator: {:?}", unary_expr.operator.kind),
                    span: unary_expr.operator.span,
                });
            }
        };

        // Type checking for unary operations
        let result_type = match op {
            UnaryOp::Neg => match expr.ty() {
                RueType::I32 | RueType::I64 => expr.ty().clone(),
                _ => {
                    return Err(SemanticError {
                        message: format!("Unary negation not supported for type: {}", expr.ty()),
                        span: unary_expr.operator.span,
                    });
                }
            },
        };

        Ok(HirExpr::Unary {
            op,
            expr: Box::new(expr),
            ty: result_type,
            span: unary_expr.operator.span,
        })
    }

    fn check_call_expression(
        &mut self,
        call: &rue_ast::CallExprNode,
    ) -> Result<HirExpr, SemanticError> {
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
        let mut args = Vec::new();
        for (i, arg_expr) in call.args.iter().enumerate() {
            let expected_type = &signature.param_types[i];
            let arg = self.check_expression_with_hint(arg_expr, Some(expected_type))?;

            if arg.ty() != expected_type {
                return Err(SemanticError {
                    message: format!(
                        "Type mismatch: Argument {} of function {}: expected {expected_type}, found {}",
                        i + 1,
                        func_name,
                        arg.ty()
                    ),
                    span: arg_expr.span(),
                });
            }

            args.push(arg);
        }

        Ok(HirExpr::Call {
            func: func_name,
            args,
            ty: signature.return_type,
            span: call.open_paren.span,
        })
    }

    fn check_if_expression(
        &mut self,
        if_expr: &rue_ast::IfStatementNode,
    ) -> Result<HirExpr, SemanticError> {
        // Type check condition
        let cond = self.check_expression(&if_expr.condition)?;
        if cond.ty() != &RueType::Bool {
            return Err(SemanticError {
                message: format!("If condition must be bool, found {}", cond.ty()),
                span: if_expr.condition.span(),
            });
        }

        // Type check then block
        let then_block = self.check_block(&if_expr.then_block, None)?;

        // Type check else block if present
        let (else_block, result_type) = if let Some(else_clause) = &if_expr.else_clause {
            let else_block_node = match &else_clause.body {
                rue_ast::ElseBodyNode::Block(block) => block.as_ref(),
                rue_ast::ElseBodyNode::If(_) => {
                    return Err(SemanticError {
                        message: "Else-if chains not yet supported in expressions".to_string(),
                        span: if_expr.if_token.span,
                    });
                }
            };
            let else_block = self.check_block(else_block_node, None)?;

            // Determine result type by combining both branches
            let then_type = if let Some(ref then_expr) = then_block.expr {
                then_expr.ty()
            } else {
                &RueType::Unit
            };

            let else_type = if let Some(ref else_expr) = else_block.expr {
                else_expr.ty()
            } else {
                &RueType::Unit
            };

            if then_type != else_type {
                return Err(SemanticError {
                    message: "If branches have incompatible types".to_string(),
                    span: if_expr.if_token.span,
                });
            }

            (Some(else_block), then_type.clone())
        } else {
            // No else block means result is unit type
            (None, RueType::Unit)
        };

        Ok(HirExpr::If {
            cond: Box::new(cond),
            then_block,
            else_block,
            ty: result_type,
            span: if_expr.if_token.span,
        })
    }

    fn check_while_expression(
        &mut self,
        while_expr: &rue_ast::WhileStatementNode,
    ) -> Result<HirExpr, SemanticError> {
        // Type check condition
        let cond = self.check_expression(&while_expr.condition)?;
        if cond.ty() != &RueType::Bool {
            return Err(SemanticError {
                message: format!("While condition must be bool, found {}", cond.ty()),
                span: while_expr.condition.span(),
            });
        }

        // Type check body
        let body = self.check_block(&while_expr.body, None)?;

        Ok(HirExpr::While {
            cond: Box::new(cond),
            body,
            span: while_expr.while_token.span,
        })
    }

    fn check_struct_literal(
        &mut self,
        struct_lit: &StructLiteralNode,
    ) -> Result<HirExpr, SemanticError> {
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
        let mut hir_fields = Vec::new();
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
            let field_value = self.check_expression_with_hint(&field.value, Some(expected_type))?;
            if field_value.ty() != expected_type {
                return Err(SemanticError {
                    message: format!(
                        "Type mismatch: Field {} expected {expected_type}, found {}",
                        field_name,
                        field_value.ty()
                    ),
                    span: field.value.span(),
                });
            }

            provided_fields.insert(field_name.clone());
            hir_fields.push((field_name, field_value));
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

        Ok(HirExpr::StructLiteral {
            struct_name,
            fields: hir_fields,
            ty: RueType::Struct(struct_def.id),
            span: struct_lit.name.span,
        })
    }

    fn check_field_access(
        &mut self,
        field_access: &FieldAccessNode,
    ) -> Result<HirExpr, SemanticError> {
        let base = self.check_expression(&field_access.base)?;

        match base.ty() {
            RueType::Struct(struct_id) => {
                // Look up struct definition
                let struct_def =
                    self.global_scope
                        .get_struct_by_id(*struct_id)
                        .ok_or_else(|| SemanticError {
                            message: format!("Unknown struct with ID: {struct_id:?}"),
                            span: field_access.dot.span,
                        })?;

                let field_name = match &field_access.field {
                    FieldKindNode::Named(name_token) => {
                        if let TokenKind::Ident(name) = &name_token.kind {
                            name.clone()
                        } else {
                            return Err(SemanticError {
                                message: "Expected field name".to_string(),
                                span: name_token.span,
                            });
                        }
                    }
                    FieldKindNode::Positional(_) => {
                        return Err(SemanticError {
                            message: "Structs must use field names, not numeric indices"
                                .to_string(),
                            span: match &field_access.field {
                                FieldKindNode::Named(token) | FieldKindNode::Positional(token) => {
                                    token.span
                                }
                            },
                        });
                    }
                };

                // Find field in struct definition
                let field_type = struct_def
                    .fields
                    .iter()
                    .find(|(name, _)| name == &field_name)
                    .map(|(_, ty)| ty.clone())
                    .ok_or_else(|| SemanticError {
                        message: format!(
                            "Struct {} has no field named {}",
                            struct_def.name, field_name
                        ),
                        span: match &field_access.field {
                            FieldKindNode::Named(token) | FieldKindNode::Positional(token) => {
                                token.span
                            }
                        },
                    })?;

                Ok(HirExpr::FieldAccess {
                    base: Box::new(base),
                    field: FieldId::Named(field_name),
                    ty: field_type,
                    span: field_access.dot.span,
                })
            }
            RueType::Tuple(element_types) => {
                let field_index = match &field_access.field {
                    FieldKindNode::Positional(index_token) => {
                        if let TokenKind::Integer(index) = index_token.kind {
                            index as usize
                        } else {
                            return Err(SemanticError {
                                message: "Expected numeric index for tuple".to_string(),
                                span: index_token.span,
                            });
                        }
                    }
                    FieldKindNode::Named(_) => {
                        return Err(SemanticError {
                            message: "Tuples must use integer literal indices, not field names"
                                .to_string(),
                            span: match &field_access.field {
                                FieldKindNode::Named(token) | FieldKindNode::Positional(token) => {
                                    token.span
                                }
                            },
                        });
                    }
                };

                if field_index >= element_types.len() {
                    return Err(SemanticError {
                        message: format!(
                            "Tuple index {} out of bounds (tuple has {} elements)",
                            field_index,
                            element_types.len()
                        ),
                        span: match &field_access.field {
                            FieldKindNode::Named(token) | FieldKindNode::Positional(token) => {
                                token.span
                            }
                        },
                    });
                }

                let field_type = element_types[field_index].clone();

                Ok(HirExpr::FieldAccess {
                    base: Box::new(base),
                    field: FieldId::Index(field_index),
                    ty: field_type,
                    span: field_access.dot.span,
                })
            }
            _ => Err(SemanticError {
                message: format!(
                    "Cannot access field on type {}, expected struct or tuple",
                    base.ty()
                ),
                span: field_access.base.span(),
            }),
        }
    }

    fn check_tuple_literal(
        &mut self,
        tuple_lit: &TupleLiteralNode,
    ) -> Result<HirExpr, SemanticError> {
        let mut elements = Vec::new();
        let mut element_types = Vec::new();

        for element_expr in &tuple_lit.elements {
            let element = self.check_expression(element_expr)?;
            element_types.push(element.ty().clone());
            elements.push(element);
        }

        Ok(HirExpr::TupleLiteral {
            elements,
            ty: RueType::Tuple(element_types),
            span: tuple_lit.open_paren.span,
        })
    }

    fn check_array_literal(
        &mut self,
        array_lit: &ArrayLiteralNode,
    ) -> Result<HirExpr, SemanticError> {
        if array_lit.elements.is_empty() {
            return Err(SemanticError {
                message: "Cannot infer type of empty array literal".to_string(),
                span: array_lit.open_bracket.span,
            });
        }

        let mut elements = Vec::new();
        let mut element_type: Option<RueType> = None;

        for element_expr in &array_lit.elements {
            let element = self.check_expression(element_expr)?;

            // Check that all elements have the same type
            if let Some(ref expected_type) = element_type {
                if element.ty() != expected_type {
                    return Err(SemanticError {
                        message: format!(
                            "Array elements must have the same type: expected {expected_type}, found {}",
                            element.ty()
                        ),
                        span: element_expr.span(),
                    });
                }
            } else {
                element_type = Some(element.ty().clone());
            }

            elements.push(element);
        }

        let array_type = RueType::Array(Box::new(element_type.unwrap()), array_lit.elements.len());

        Ok(HirExpr::ArrayLiteral {
            elements,
            ty: array_type,
            span: array_lit.open_bracket.span,
        })
    }

    fn check_array_access(
        &mut self,
        array_access: &ArrayAccessNode,
    ) -> Result<HirExpr, SemanticError> {
        let base = self.check_expression(&array_access.base)?;
        let index = self.check_expression(&array_access.index)?;

        // Validate index type
        match index.ty() {
            RueType::I32 | RueType::I64 => {}
            _ => {
                return Err(SemanticError {
                    message: "Array index must be integer type".to_string(),
                    span: array_access.index.span(),
                });
            }
        }

        // Validate base type and extract element type
        let element_type = match base.ty() {
            RueType::Array(element_type, _) => (**element_type).clone(),
            _ => {
                return Err(SemanticError {
                    message: format!("Cannot index into type {}, expected array", base.ty()),
                    span: array_access.base.span(),
                });
            }
        };

        Ok(HirExpr::ArrayAccess {
            base: Box::new(base),
            index: Box::new(index),
            ty: element_type,
            span: array_access.open_bracket.span,
        })
    }
}
