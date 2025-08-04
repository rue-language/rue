//! Type checker that builds HIR during semantic analysis
//!
//! This module provides a unified type checker that performs semantic analysis
//! and HIR construction in a single pass, ensuring that all type information
//! is accurately preserved in the HIR.

use crate::{FunctionSignature, SemanticError};
use rue_ast::{CstRoot, ExpressionNode, FunctionNode, StatementNode};
use rue_ir::hir::{
    BinOp, HirBlock, HirExpr, HirFunction, HirLiteral, HirProgram, HirStatement, UnaryOp,
};
use rue_ir::types::RueType;
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
    var_scopes: Vec<VariableScope>,
    /// Function signatures for type lookup
    functions: HashMap<String, FunctionSignature>,
    /// HIR functions being built
    hir_functions: Vec<HirFunction>,
}

impl TypeChecker {
    /// Create a new type checker with built-in functions
    pub fn new(functions: HashMap<String, FunctionSignature>) -> Self {
        Self {
            var_scopes: vec![VariableScope::new()],
            functions,
            hir_functions: Vec::new(),
        }
    }

    /// Push a new variable scope
    fn push_scope(&mut self) {
        self.var_scopes.push(VariableScope::new());
    }

    /// Pop the current variable scope
    fn pop_scope(&mut self) {
        if self.var_scopes.len() > 1 {
            self.var_scopes.pop();
        }
    }

    /// Add a variable to the current scope
    fn add_variable(&mut self, name: String, ty: RueType) {
        if let Some(current_scope) = self.var_scopes.last_mut() {
            current_scope.insert(name, ty);
        }
    }

    /// Look up a variable type in the scope stack
    fn lookup_variable(&self, name: &str) -> Option<&RueType> {
        // Search from innermost to outermost scope
        for scope in self.var_scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty);
            }
        }
        None
    }

    /// Type check and build HIR for a complete program
    pub fn check_program(&mut self, ast: &CstRoot) -> Result<HirProgram, SemanticError> {
        // First pass: collect all function signatures
        for item in &ast.items {
            if let rue_ast::CstNode::Function(func) = item {
                self.collect_function_signature(func)?;
            }
        }

        // Second pass: type check function bodies and build HIR
        for item in &ast.items {
            match item {
                rue_ast::CstNode::Function(func) => {
                    let hir_func = self.check_function(func)?;
                    self.hir_functions.push(hir_func);
                }
                rue_ast::CstNode::Statement(stmt) => {
                    // Top-level statements not supported in HIR yet
                    self.check_statement(stmt)?;
                }
                _ => {} // Skip other node types
            }
        }

        Ok(HirProgram {
            functions: std::mem::take(&mut self.hir_functions),
        })
    }

    /// Collect function signature without checking body
    fn collect_function_signature(&mut self, func: &FunctionNode) -> Result<(), SemanticError> {
        let func_name = match &func.name.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => {
                return Err(SemanticError {
                    message: "Expected function name".to_string(),
                    span: func.name.span,
                });
            }
        };

        let mut param_types = Vec::new();
        for param in &func.param_list.params {
            let param_type = if let Some(type_ann) = &param.type_annotation {
                crate::convert_type_node(&type_ann.ty)
            } else {
                RueType::I32 // Default to i32
            };
            param_types.push(param_type);
        }

        let return_type = if let Some(return_type_node) = &func.return_type {
            crate::convert_type_node(&return_type_node.ty)
        } else {
            RueType::Unit
        };

        self.functions.insert(
            func_name,
            FunctionSignature {
                param_types,
                return_type,
            },
        );

        Ok(())
    }

    /// Type check a function and build HIR
    fn check_function(&mut self, func: &FunctionNode) -> Result<HirFunction, SemanticError> {
        // Reset scope for new function
        self.var_scopes = vec![VariableScope::new()];

        let name = match &func.name.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => {
                return Err(SemanticError {
                    message: "Expected function name".to_string(),
                    span: func.name.span,
                });
            }
        };

        // Get function signature
        let sig = self
            .functions
            .get(&name)
            .cloned()
            .ok_or_else(|| SemanticError {
                message: format!("Function '{name}' not found"),
                span: func.name.span,
            })?;

        // Process parameters
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

            let param_type = sig.param_types[i].clone();
            params.push((param_name.clone(), param_type.clone()));
            self.add_variable(param_name, param_type);
        }

        // Type check and build body
        let body = self.check_block(&func.body, &sig.return_type)?;

        // Verify return type
        let actual_return_type = if let Some(expr) = &body.expr {
            expr.ty().clone()
        } else {
            RueType::Unit
        };

        if actual_return_type != sig.return_type {
            return Err(SemanticError {
                message: format!(
                    "Type mismatch: function '{name}' is declared to return '{}' but returns '{}'",
                    sig.return_type, actual_return_type
                ),
                span: func.body.close_brace.span,
            });
        }

        Ok(HirFunction {
            name,
            params,
            return_type: sig.return_type,
            body,
            span: func.name.span,
        })
    }

    /// Type check a block without expected type
    fn check_block_no_expected(
        &mut self,
        block: &rue_ast::BlockNode,
    ) -> Result<HirBlock, SemanticError> {
        self.push_scope();

        let mut statements = Vec::new();

        // Check statements
        for stmt in &block.statements {
            let hir_stmt = self.check_statement(stmt)?;
            if let Some(hir_stmt) = hir_stmt {
                statements.push(hir_stmt);
            }
        }

        // Check final expression without expected type
        let expr = if let Some(final_expr) = &block.final_expr {
            Some(Box::new(self.check_expression(final_expr)?))
        } else {
            None
        };

        self.pop_scope();

        Ok(HirBlock { statements, expr })
    }

    /// Type check a block and build HIR
    fn check_block(
        &mut self,
        block: &rue_ast::BlockNode,
        expected_type: &RueType,
    ) -> Result<HirBlock, SemanticError> {
        self.push_scope();

        let mut statements = Vec::new();

        // Check statements
        for stmt in &block.statements {
            let hir_stmt = self.check_statement(stmt)?;
            if let Some(hir_stmt) = hir_stmt {
                statements.push(hir_stmt);
            }
        }

        // Check final expression
        let expr = if let Some(final_expr) = &block.final_expr {
            Some(Box::new(self.check_expression_with_expected_type(
                final_expr,
                expected_type,
            )?))
        } else {
            None
        };

        self.pop_scope();

        Ok(HirBlock { statements, expr })
    }

    /// Type check a statement and optionally build HIR
    fn check_statement(
        &mut self,
        stmt: &StatementNode,
    ) -> Result<Option<HirStatement>, SemanticError> {
        match stmt {
            StatementNode::Let(let_stmt) => {
                let name = match &let_stmt.name.kind {
                    TokenKind::Ident(name) => name.clone(),
                    _ => {
                        return Err(SemanticError {
                            message: "Expected variable name".to_string(),
                            span: let_stmt.name.span,
                        });
                    }
                };

                // Determine expected type
                let expected_type = if let Some(type_ann) = &let_stmt.type_annotation {
                    crate::convert_type_node(&type_ann.ty)
                } else {
                    // No annotation, will infer from init
                    self.infer_expression_type(&let_stmt.value)?
                };

                // Type check init expression with expected type
                let init =
                    self.check_expression_with_expected_type(&let_stmt.value, &expected_type)?;
                let actual_type = init.ty().clone();

                // Verify types match
                if actual_type != expected_type {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch: expected '{expected_type}' but found '{actual_type}'"
                        ),
                        span: let_stmt.value.span(),
                    });
                }

                // Add to scope
                self.add_variable(name.clone(), expected_type.clone());

                Ok(Some(HirStatement::Let {
                    name,
                    ty: expected_type,
                    init,
                    span: let_stmt.let_token.span,
                }))
            }
            StatementNode::Assign(assign_stmt) => {
                let name = match &assign_stmt.name.kind {
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
                    self.lookup_variable(&name)
                        .cloned()
                        .ok_or_else(|| SemanticError {
                            message: format!("Cannot assign to undefined variable: {name}"),
                            span: assign_stmt.name.span,
                        })?;

                // Type check value with expected type
                let value =
                    self.check_expression_with_expected_type(&assign_stmt.value, &var_type)?;
                let actual_type = value.ty().clone();

                if actual_type != var_type {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch: cannot assign '{actual_type}' to variable of type '{var_type}'"
                        ),
                        span: assign_stmt.value.span(),
                    });
                }

                Ok(Some(HirStatement::Assign {
                    name,
                    value,
                    span: assign_stmt.name.span,
                }))
            }
            StatementNode::Expression(expr_stmt) => {
                let expr = self.check_expression(&expr_stmt.expression)?;
                Ok(Some(HirStatement::Expr(expr)))
            }
        }
    }

    /// Type check an expression with an expected type (for literals)
    fn check_expression_with_expected_type(
        &mut self,
        expr: &ExpressionNode,
        expected_type: &RueType,
    ) -> Result<HirExpr, SemanticError> {
        match expr {
            ExpressionNode::Literal(token) => match &token.kind {
                TokenKind::Integer(value) => {
                    // Use expected type for integer literals
                    match expected_type {
                        RueType::I32 => {
                            // Use wrapping cast to handle overflow according to language spec
                            Ok(HirExpr::Literal {
                                lit: HirLiteral::Int32(*value as i32),
                                span: token.span,
                            })
                        }
                        RueType::I64 => Ok(HirExpr::Literal {
                            lit: HirLiteral::Int64(*value),
                            span: token.span,
                        }),
                        _ => Err(SemanticError {
                            message: format!(
                                "Cannot use integer literal in context expecting {expected_type}"
                            ),
                            span: token.span,
                        }),
                    }
                }
                _ => self.check_expression(expr), // For non-integer literals
            },
            _ => self.check_expression(expr), // For non-literals
        }
    }

    /// Type check an expression without expected type
    fn check_expression(&mut self, expr: &ExpressionNode) -> Result<HirExpr, SemanticError> {
        match expr {
            ExpressionNode::Literal(token) => match &token.kind {
                TokenKind::Integer(value) => {
                    // Default to i32
                    if *value >= i32::MIN as i64 && *value <= i32::MAX as i64 {
                        Ok(HirExpr::Literal {
                            lit: HirLiteral::Int32(*value as i32),
                            span: token.span,
                        })
                    } else {
                        Ok(HirExpr::Literal {
                            lit: HirLiteral::Int64(*value),
                            span: token.span,
                        })
                    }
                }
                TokenKind::True => Ok(HirExpr::Literal {
                    lit: HirLiteral::Bool(true),
                    span: token.span,
                }),
                TokenKind::False => Ok(HirExpr::Literal {
                    lit: HirLiteral::Bool(false),
                    span: token.span,
                }),
                TokenKind::Unit => Ok(HirExpr::Literal {
                    lit: HirLiteral::Unit,
                    span: token.span,
                }),
                _ => Err(SemanticError {
                    message: "Unexpected literal type".to_string(),
                    span: token.span,
                }),
            },
            ExpressionNode::Identifier(token) => {
                let name = match &token.kind {
                    TokenKind::Ident(name) => name.clone(),
                    _ => {
                        return Err(SemanticError {
                            message: "Expected identifier".to_string(),
                            span: token.span,
                        });
                    }
                };

                let ty = self
                    .lookup_variable(&name)
                    .cloned()
                    .ok_or_else(|| SemanticError {
                        message: format!("Undefined variable: {name}"),
                        span: token.span,
                    })?;

                Ok(HirExpr::Var {
                    name,
                    ty,
                    span: token.span,
                })
            }
            ExpressionNode::Binary(binary_expr) => {
                // Check if we can use contextual type inference for integer literals
                let (lhs, rhs) = match (binary_expr.left.as_ref(), binary_expr.right.as_ref()) {
                    // LHS is literal, RHS determines type
                    (ExpressionNode::Literal(token), _)
                        if matches!(token.kind, TokenKind::Integer(_)) =>
                    {
                        let rhs = Box::new(self.check_expression(&binary_expr.right)?);
                        let rhs_type = rhs.ty();
                        let lhs = Box::new(
                            self.check_expression_with_expected_type(&binary_expr.left, rhs_type)?,
                        );
                        (lhs, rhs)
                    }
                    // RHS is literal, LHS determines type
                    (_, ExpressionNode::Literal(token))
                        if matches!(token.kind, TokenKind::Integer(_)) =>
                    {
                        let lhs = Box::new(self.check_expression(&binary_expr.left)?);
                        let lhs_type = lhs.ty();
                        let rhs = Box::new(
                            self.check_expression_with_expected_type(&binary_expr.right, lhs_type)?,
                        );
                        (lhs, rhs)
                    }
                    // Neither is a literal, check normally
                    _ => {
                        let lhs = Box::new(self.check_expression(&binary_expr.left)?);
                        let rhs = Box::new(self.check_expression(&binary_expr.right)?);
                        (lhs, rhs)
                    }
                };

                let lhs_type = lhs.ty();
                let rhs_type = rhs.ty();

                let (op, result_type) = match &binary_expr.operator.kind {
                    TokenKind::Plus => (BinOp::Add, lhs_type.clone()),
                    TokenKind::Minus => (BinOp::Sub, lhs_type.clone()),
                    TokenKind::Star => (BinOp::Mul, lhs_type.clone()),
                    TokenKind::Slash => (BinOp::Div, lhs_type.clone()),
                    TokenKind::Percent => (BinOp::Mod, lhs_type.clone()),
                    TokenKind::Less => (BinOp::Lt, RueType::Bool),
                    TokenKind::LessEqual => (BinOp::Le, RueType::Bool),
                    TokenKind::Greater => (BinOp::Gt, RueType::Bool),
                    TokenKind::GreaterEqual => (BinOp::Ge, RueType::Bool),
                    TokenKind::Equal => (BinOp::Eq, RueType::Bool),
                    TokenKind::NotEqual => (BinOp::Ne, RueType::Bool),
                    _ => {
                        return Err(SemanticError {
                            message: "Unknown binary operator".to_string(),
                            span: binary_expr.operator.span,
                        });
                    }
                };

                // Type check operands
                if lhs_type != rhs_type {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch: binary operator cannot be applied to types '{lhs_type}' and '{rhs_type}'"
                        ),
                        span: binary_expr.operator.span,
                    });
                }

                // For arithmetic ops, ensure numeric types
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        match lhs_type {
                            RueType::I32 | RueType::I64 => {}
                            _ => {
                                return Err(SemanticError {
                                    message: format!(
                                        "Arithmetic operators require numeric types, found {lhs_type}"
                                    ),
                                    span: binary_expr.operator.span,
                                });
                            }
                        }
                    }
                    _ => {}
                }

                Ok(HirExpr::Binary {
                    op,
                    lhs,
                    rhs,
                    ty: result_type,
                    span: binary_expr.operator.span,
                })
            }
            ExpressionNode::Unary(unary_expr) => {
                let expr = Box::new(self.check_expression(&unary_expr.operand)?);
                let ty = expr.ty().clone();

                let op = match &unary_expr.operator.kind {
                    TokenKind::Minus => UnaryOp::Neg,
                    _ => {
                        return Err(SemanticError {
                            message: "Unknown unary operator".to_string(),
                            span: unary_expr.operator.span,
                        });
                    }
                };

                // Check that operand is numeric
                match &ty {
                    RueType::I32 | RueType::I64 => {}
                    _ => {
                        return Err(SemanticError {
                            message: format!("Unary negation requires numeric type, found {ty}"),
                            span: unary_expr.operator.span,
                        });
                    }
                }

                Ok(HirExpr::Unary {
                    op,
                    expr,
                    ty,
                    span: unary_expr.operator.span,
                })
            }
            ExpressionNode::Call(call_expr) => {
                let func_name = match &*call_expr.function {
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
                            message: "Function calls must use identifiers".to_string(),
                            span: call_expr.function.span(),
                        });
                    }
                };

                // Look up function signature
                let sig = self
                    .functions
                    .get(&func_name)
                    .cloned()
                    .ok_or_else(|| SemanticError {
                        message: format!("Undefined function: {func_name}"),
                        span: call_expr.function.span(),
                    })?;

                // Check argument count
                if call_expr.args.len() != sig.param_types.len() {
                    return Err(SemanticError {
                        message: format!(
                            "Function '{}' expects {} arguments, but {} were provided",
                            func_name,
                            sig.param_types.len(),
                            call_expr.args.len()
                        ),
                        span: call_expr.function.span(),
                    });
                }

                // Type check arguments
                let mut args = Vec::new();
                for (i, arg) in call_expr.args.iter().enumerate() {
                    let expected_type = &sig.param_types[i];
                    let arg_expr = self.check_expression_with_expected_type(arg, expected_type)?;
                    let actual_type = arg_expr.ty();

                    if actual_type != expected_type {
                        return Err(SemanticError {
                            message: format!(
                                "Type mismatch in argument {}: expected '{}' but found '{}'",
                                i + 1,
                                expected_type,
                                actual_type
                            ),
                            span: arg.span(),
                        });
                    }

                    args.push(arg_expr);
                }

                Ok(HirExpr::Call {
                    func: func_name,
                    args,
                    ty: sig.return_type,
                    span: call_expr.function.span(),
                })
            }
            ExpressionNode::If(if_stmt) => {
                // Check condition is bool
                let cond = Box::new(self.check_expression(&if_stmt.condition)?);
                if cond.ty() != &RueType::Bool {
                    return Err(SemanticError {
                        message: format!("If condition must be bool, found {}", cond.ty()),
                        span: if_stmt.condition.span(),
                    });
                }

                // Check then block first without specific expected type
                let then_block = self.check_block_no_expected(&if_stmt.then_block)?;
                let then_type = if let Some(expr) = &then_block.expr {
                    expr.ty().clone()
                } else {
                    RueType::Unit
                };

                let (else_block, else_type) = if let Some(else_clause) = &if_stmt.else_clause {
                    match &else_clause.body {
                        rue_ast::ElseBodyNode::Block(block) => {
                            let else_block = self.check_block_no_expected(block)?;
                            let else_type = if let Some(expr) = &else_block.expr {
                                expr.ty().clone()
                            } else {
                                RueType::Unit
                            };
                            (Some(else_block), else_type)
                        }
                        rue_ast::ElseBodyNode::If(nested_if) => {
                            // Convert to block with if expression
                            let nested_expr =
                                self.check_expression(&ExpressionNode::If(nested_if.clone()))?;
                            let else_type = nested_expr.ty().clone();
                            let else_block = HirBlock {
                                statements: vec![],
                                expr: Some(Box::new(nested_expr)),
                            };
                            (Some(else_block), else_type)
                        }
                    }
                } else {
                    (None, RueType::Unit)
                };

                // Check branch types match
                if then_type != else_type {
                    return Err(SemanticError {
                        message: format!(
                            "If branches have incompatible types: '{then_type}' and '{else_type}'"
                        ),
                        span: if_stmt.if_token.span,
                    });
                }

                Ok(HirExpr::If {
                    cond,
                    then_block,
                    else_block,
                    ty: then_type,
                    span: if_stmt.if_token.span,
                })
            }
            ExpressionNode::While(while_stmt) => {
                // Check condition is bool
                let cond = Box::new(self.check_expression(&while_stmt.condition)?);
                if cond.ty() != &RueType::Bool {
                    return Err(SemanticError {
                        message: format!("While condition must be bool, found {}", cond.ty()),
                        span: while_stmt.condition.span(),
                    });
                }

                let body = self.check_block_no_expected(&while_stmt.body)?;

                Ok(HirExpr::While {
                    cond,
                    body,
                    span: while_stmt.while_token.span,
                })
            }
            ExpressionNode::StructLiteral(struct_lit) => {
                // Get struct name
                let struct_name = match &struct_lit.name.kind {
                    TokenKind::Ident(name) => name.clone(),
                    _ => {
                        return Err(SemanticError {
                            message: "Expected struct name".to_string(),
                            span: struct_lit.name.span,
                        });
                    }
                };

                // Generate struct ID (for now, use simple hash)
                let struct_id = rue_ir::types::StructId::new(crate::hash_string(&struct_name));
                let struct_type = RueType::Struct(struct_id);

                // Type check field initializers
                let mut hir_fields = Vec::new();
                for field_init in &struct_lit.fields {
                    let field_name = match &field_init.name.kind {
                        TokenKind::Ident(name) => name.clone(),
                        _ => {
                            return Err(SemanticError {
                                message: "Expected field name".to_string(),
                                span: field_init.name.span,
                            });
                        }
                    };

                    // For now, accept any expression type for fields
                    // In a full implementation, we'd check against struct definition
                    let field_expr = self.check_expression(&field_init.value)?;
                    hir_fields.push((field_name, field_expr));
                }

                Ok(HirExpr::StructLiteral {
                    struct_id,
                    fields: hir_fields,
                    ty: struct_type,
                    span: struct_lit.name.span,
                })
            }
            ExpressionNode::TupleLiteral(tuple_lit) => {
                // Type check all tuple elements
                let mut hir_elements = Vec::new();
                let mut element_types = Vec::new();

                for element in &tuple_lit.elements {
                    let hir_elem = self.check_expression(element)?;
                    element_types.push(hir_elem.ty().clone());
                    hir_elements.push(hir_elem);
                }

                let tuple_type = RueType::Tuple(element_types);

                Ok(HirExpr::TupleLiteral {
                    elements: hir_elements,
                    ty: tuple_type,
                    span: tuple_lit.open_paren.span,
                })
            }
            ExpressionNode::ArrayLiteral(array_lit) => {
                if array_lit.elements.is_empty() {
                    return Err(SemanticError {
                        message: "Array literals cannot be empty (type cannot be inferred)"
                            .to_string(),
                        span: array_lit.open_bracket.span,
                    });
                }

                // Type check first element to determine array element type
                let first_elem = self.check_expression(&array_lit.elements[0])?;
                let element_type = first_elem.ty().clone();

                let mut hir_elements = vec![first_elem];

                // Type check remaining elements, ensuring they match the first
                for (i, element) in array_lit.elements.iter().enumerate().skip(1) {
                    let hir_elem =
                        self.check_expression_with_expected_type(element, &element_type)?;
                    let actual_type = hir_elem.ty();

                    if *actual_type != element_type {
                        return Err(SemanticError {
                            message: format!(
                                "Array element {i} has type '{actual_type}' but expected '{element_type}'"
                            ),
                            span: element.span(),
                        });
                    }

                    hir_elements.push(hir_elem);
                }

                let array_type = RueType::Array(Box::new(element_type), array_lit.elements.len());

                Ok(HirExpr::ArrayLiteral {
                    elements: hir_elements,
                    ty: array_type,
                    span: array_lit.open_bracket.span,
                })
            }
            ExpressionNode::FieldAccess(field_access) => {
                // Type check base expression
                let base_expr = Box::new(self.check_expression(&field_access.base)?);
                let base_type = base_expr.ty().clone();

                // Determine field and its type based on base type
                let (field_id, field_type) = match &base_type {
                    RueType::Struct(_struct_id) => {
                        // For structs, field must be named
                        match &field_access.field {
                            rue_ast::FieldKindNode::Named(name_token) => {
                                if let TokenKind::Ident(field_name) = &name_token.kind {
                                    // In a full implementation, we'd look up the field type from struct definition
                                    // For now, assume i64 for all struct fields
                                    let field_id =
                                        rue_ir::types::FieldId::from_name(field_name.clone());
                                    (field_id, RueType::I64)
                                } else {
                                    return Err(SemanticError {
                                        message: "Expected field name".to_string(),
                                        span: name_token.span,
                                    });
                                }
                            }
                            rue_ast::FieldKindNode::Positional(_) => {
                                return Err(SemanticError {
                                    message: "Cannot use positional field access on struct"
                                        .to_string(),
                                    span: field_access.dot.span,
                                });
                            }
                        }
                    }
                    RueType::Tuple(element_types) => {
                        // For tuples, field can be named or positional
                        match &field_access.field {
                            rue_ast::FieldKindNode::Positional(index_token) => {
                                if let TokenKind::Integer(index) = &index_token.kind {
                                    let idx = *index as usize;
                                    if idx < element_types.len() {
                                        let field_id = rue_ir::types::FieldId::from_index(idx);
                                        (field_id, element_types[idx].clone())
                                    } else {
                                        return Err(SemanticError {
                                            message: format!(
                                                "Tuple index {} out of bounds (tuple has {} elements)",
                                                idx,
                                                element_types.len()
                                            ),
                                            span: index_token.span,
                                        });
                                    }
                                } else {
                                    return Err(SemanticError {
                                        message: "Expected integer index".to_string(),
                                        span: index_token.span,
                                    });
                                }
                            }
                            rue_ast::FieldKindNode::Named(_) => {
                                return Err(SemanticError {
                                    message: "Cannot use named field access on tuple".to_string(),
                                    span: field_access.dot.span,
                                });
                            }
                        }
                    }
                    _ => {
                        return Err(SemanticError {
                            message: format!(
                                "Cannot access field of type '{base_type}' (not a struct or tuple)"
                            ),
                            span: field_access.dot.span,
                        });
                    }
                };

                Ok(HirExpr::FieldAccess {
                    base: base_expr,
                    field: field_id,
                    ty: field_type,
                    span: field_access.dot.span,
                })
            }
            ExpressionNode::ArrayAccess(array_access) => {
                // Type check base expression
                let base_expr = Box::new(self.check_expression(&array_access.base)?);
                let base_type = base_expr.ty().clone();

                // Ensure base is an array
                let element_type = match &base_type {
                    RueType::Array(elem_type, _) => (**elem_type).clone(),
                    _ => {
                        return Err(SemanticError {
                            message: format!("Cannot index into type '{base_type}' (not an array)"),
                            span: array_access.open_bracket.span,
                        });
                    }
                };

                // Type check index expression (must be integer)
                let index_expr = Box::new(self.check_expression(&array_access.index)?);
                let index_type = index_expr.ty();

                if !matches!(index_type, RueType::I32 | RueType::I64) {
                    return Err(SemanticError {
                        message: format!("Array index must be integer type, found '{index_type}'"),
                        span: array_access.index.span(),
                    });
                }

                Ok(HirExpr::ArrayAccess {
                    base: base_expr,
                    index: index_expr,
                    ty: element_type,
                    span: array_access.open_bracket.span,
                })
            }
        }
    }

    /// Infer the type of an expression (for let statements without annotations)
    fn infer_expression_type(&self, expr: &ExpressionNode) -> Result<RueType, SemanticError> {
        match expr {
            ExpressionNode::Literal(token) => match &token.kind {
                TokenKind::Integer(_) => Ok(RueType::I32), // Default to i32
                TokenKind::True | TokenKind::False => Ok(RueType::Bool),
                TokenKind::Unit => Ok(RueType::Unit),
                _ => Err(SemanticError {
                    message: "Unexpected literal type".to_string(),
                    span: token.span,
                }),
            },
            ExpressionNode::Identifier(token) => {
                if let TokenKind::Ident(name) = &token.kind {
                    self.lookup_variable(name)
                        .cloned()
                        .ok_or_else(|| SemanticError {
                            message: format!("Undefined variable: {name}"),
                            span: token.span,
                        })
                } else {
                    Err(SemanticError {
                        message: "Expected identifier".to_string(),
                        span: token.span,
                    })
                }
            }
            ExpressionNode::Binary(binary_expr) => {
                match &binary_expr.operator.kind {
                    // Arithmetic ops preserve type
                    TokenKind::Plus
                    | TokenKind::Minus
                    | TokenKind::Star
                    | TokenKind::Slash
                    | TokenKind::Percent => self.infer_expression_type(&binary_expr.left),
                    // Comparison ops return bool
                    TokenKind::Less
                    | TokenKind::LessEqual
                    | TokenKind::Greater
                    | TokenKind::GreaterEqual
                    | TokenKind::Equal
                    | TokenKind::NotEqual => Ok(RueType::Bool),
                    _ => Err(SemanticError {
                        message: "Unknown binary operator".to_string(),
                        span: binary_expr.operator.span,
                    }),
                }
            }
            ExpressionNode::Unary(unary_expr) => self.infer_expression_type(&unary_expr.operand),
            ExpressionNode::Call(call_expr) => {
                if let ExpressionNode::Identifier(token) = &*call_expr.function {
                    if let TokenKind::Ident(name) = &token.kind {
                        self.functions
                            .get(name)
                            .map(|sig| sig.return_type.clone())
                            .ok_or_else(|| SemanticError {
                                message: format!("Undefined function: {name}"),
                                span: token.span,
                            })
                    } else {
                        Err(SemanticError {
                            message: "Expected function name".to_string(),
                            span: token.span,
                        })
                    }
                } else {
                    Err(SemanticError {
                        message: "Function calls must use identifiers".to_string(),
                        span: call_expr.function.span(),
                    })
                }
            }
            ExpressionNode::If(if_stmt) => {
                if let Some(then_expr) = &if_stmt.then_block.final_expr {
                    self.infer_expression_type(then_expr)
                } else {
                    Ok(RueType::Unit)
                }
            }
            ExpressionNode::While(_) => Ok(RueType::Unit),
            ExpressionNode::StructLiteral(struct_lit) => {
                // Get struct name and generate ID
                if let TokenKind::Ident(name) = &struct_lit.name.kind {
                    let struct_id = rue_ir::types::StructId::new(crate::hash_string(name));
                    Ok(RueType::Struct(struct_id))
                } else {
                    Err(SemanticError {
                        message: "Expected struct name".to_string(),
                        span: struct_lit.name.span,
                    })
                }
            }
            ExpressionNode::TupleLiteral(tuple_lit) => {
                // Infer types of all elements
                let mut element_types = Vec::new();
                for element in &tuple_lit.elements {
                    element_types.push(self.infer_expression_type(element)?);
                }
                Ok(RueType::Tuple(element_types))
            }
            ExpressionNode::ArrayLiteral(array_lit) => {
                if array_lit.elements.is_empty() {
                    return Err(SemanticError {
                        message: "Cannot infer type of empty array literal".to_string(),
                        span: array_lit.open_bracket.span,
                    });
                }

                // Infer type from first element
                let element_type = self.infer_expression_type(&array_lit.elements[0])?;
                Ok(RueType::Array(
                    Box::new(element_type),
                    array_lit.elements.len(),
                ))
            }
            ExpressionNode::FieldAccess(field_access) => {
                // Infer base type and determine field type
                let base_type = self.infer_expression_type(&field_access.base)?;
                match &base_type {
                    RueType::Struct(_) => {
                        // For structs, assume i64 field type for now
                        Ok(RueType::I64)
                    }
                    RueType::Tuple(element_types) => {
                        // For tuples, need to determine which field is being accessed
                        match &field_access.field {
                            rue_ast::FieldKindNode::Positional(index_token) => {
                                if let TokenKind::Integer(index) = &index_token.kind {
                                    let idx = *index as usize;
                                    if idx < element_types.len() {
                                        Ok(element_types[idx].clone())
                                    } else {
                                        Err(SemanticError {
                                            message: format!("Tuple index {idx} out of bounds"),
                                            span: index_token.span,
                                        })
                                    }
                                } else {
                                    Err(SemanticError {
                                        message: "Expected integer index".to_string(),
                                        span: index_token.span,
                                    })
                                }
                            }
                            _ => Err(SemanticError {
                                message: "Cannot use named field access on tuple".to_string(),
                                span: field_access.dot.span,
                            }),
                        }
                    }
                    _ => Err(SemanticError {
                        message: format!("Cannot access field of type '{base_type}'"),
                        span: field_access.dot.span,
                    }),
                }
            }
            ExpressionNode::ArrayAccess(array_access) => {
                // Infer base type and get element type
                let base_type = self.infer_expression_type(&array_access.base)?;
                match base_type {
                    RueType::Array(element_type, _) => Ok(*element_type),
                    _ => Err(SemanticError {
                        message: format!("Cannot index into type '{base_type}'"),
                        span: array_access.open_bracket.span,
                    }),
                }
            }
        }
    }
}

// Extension trait to get span from expressions
trait HasSpan {
    fn span(&self) -> Span;
}

impl HasSpan for ExpressionNode {
    fn span(&self) -> Span {
        match self {
            ExpressionNode::Literal(token) => token.span,
            ExpressionNode::Identifier(token) => token.span,
            ExpressionNode::Binary(expr) => expr.operator.span,
            ExpressionNode::Unary(expr) => expr.operator.span,
            ExpressionNode::Call(expr) => match &*expr.function {
                ExpressionNode::Identifier(token) => token.span,
                _ => Span { start: 0, end: 0 },
            },
            ExpressionNode::If(expr) => expr.if_token.span,
            ExpressionNode::While(expr) => expr.while_token.span,
            ExpressionNode::StructLiteral(expr) => expr.name.span,
            ExpressionNode::TupleLiteral(expr) => expr.open_paren.span,
            ExpressionNode::ArrayLiteral(expr) => expr.open_bracket.span,
            ExpressionNode::FieldAccess(expr) => expr.dot.span,
            ExpressionNode::ArrayAccess(expr) => expr.open_bracket.span,
        }
    }
}
