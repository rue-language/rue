//! HIR Validator - Ensures HIR is well-formed and internally consistent
//!
//! This module provides validation for HIR after it's been built. It checks:
//! - Type consistency across expressions
//! - Variable and function references are valid
//! - Structural invariants are maintained
//! - Return types match declarations

use crate::SemanticError;
use rue_ir::hir::{BinOp, HirBlock, HirExpr, HirFunction, HirProgram, HirStatement, UnaryOp};
use rue_ir::types::RueType;
use rue_lexer::Span;
use std::collections::HashMap;

/// Context for HIR validation
pub struct HirValidator {
    /// Function signatures for validation
    functions: HashMap<String, (Vec<RueType>, RueType)>,
    /// Currently available variables in scope
    variables: HashMap<String, RueType>,
    /// Stack of scopes for nested blocks
    scope_stack: Vec<HashMap<String, RueType>>,
    /// Errors collected during validation
    errors: Vec<SemanticError>,
}

impl HirValidator {
    /// Create a new HIR validator
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
            variables: HashMap::new(),
            scope_stack: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Validate a complete HIR program
    pub fn validate_program(&mut self, program: &HirProgram) -> Result<(), Vec<SemanticError>> {
        // First pass: collect all function signatures
        self.collect_function_signatures(program);

        // Add built-in functions
        self.add_builtin_functions();

        // Second pass: validate each function
        for function in &program.functions {
            self.validate_function(function);
        }

        // Don't check for main function in HIR validation
        // This should be done at a higher level if needed
        // (Many tests don't have main functions)

        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(std::mem::take(&mut self.errors))
        }
    }

    /// Collect function signatures for validation
    fn collect_function_signatures(&mut self, program: &HirProgram) {
        for function in &program.functions {
            let param_types: Vec<RueType> =
                function.params.iter().map(|(_, ty)| ty.clone()).collect();

            self.functions.insert(
                function.name.clone(),
                (param_types, function.return_type.clone()),
            );
        }
    }

    /// Add built-in function signatures
    fn add_builtin_functions(&mut self) {
        // exit(code: i64) -> ()
        self.functions
            .insert("exit".to_string(), (vec![RueType::I64], RueType::Unit));

        // print(value: i64) -> ()
        self.functions
            .insert("print".to_string(), (vec![RueType::I64], RueType::Unit));

        // println_i32(value: i32) -> ()
        self.functions.insert(
            "println_i32".to_string(),
            (vec![RueType::I32], RueType::Unit),
        );

        // println_i64(value: i64) -> ()
        self.functions.insert(
            "println_i64".to_string(),
            (vec![RueType::I64], RueType::Unit),
        );

        // println_bool(value: bool) -> ()
        self.functions.insert(
            "println_bool".to_string(),
            (vec![RueType::Bool], RueType::Unit),
        );

        // println_unit(value: ()) -> ()
        self.functions.insert(
            "println_unit".to_string(),
            (vec![RueType::Unit], RueType::Unit),
        );

        // input() -> i64
        self.functions
            .insert("input".to_string(), (vec![], RueType::I64));

        // to_i32(value: i64) -> i32
        self.functions
            .insert("to_i32".to_string(), (vec![RueType::I64], RueType::I32));

        // to_i64(value: i32) -> i64
        self.functions
            .insert("to_i64".to_string(), (vec![RueType::I32], RueType::I64));
    }

    /// Validate a single function
    fn validate_function(&mut self, function: &HirFunction) {
        // Clear variable scope for new function
        self.variables.clear();
        self.scope_stack.clear();

        // Add parameters to scope
        for (name, ty) in &function.params {
            self.variables.insert(name.clone(), ty.clone());
        }

        // Validate function body
        let body_type = self.validate_block(&function.body);

        // Check return type matches
        if body_type != function.return_type {
            self.errors.push(SemanticError {
                message: format!(
                    "Function '{}' declares return type {} but body returns {}",
                    function.name, function.return_type, body_type
                ),
                span: function.span,
            });
        }
    }

    /// Validate a block and return its type
    fn validate_block(&mut self, block: &HirBlock) -> RueType {
        // Push new scope
        self.push_scope();

        // Validate statements
        for stmt in &block.statements {
            self.validate_statement(stmt);
        }

        // Validate final expression
        let block_type = if let Some(expr) = &block.expr {
            self.validate_expression(expr)
        } else {
            RueType::Unit
        };

        // Pop scope
        self.pop_scope();

        block_type
    }

    /// Validate a statement
    fn validate_statement(&mut self, stmt: &HirStatement) {
        match stmt {
            HirStatement::Let {
                name,
                ty,
                init,
                span,
            } => {
                // Validate init expression
                let init_type = self.validate_expression(init);

                // Check type matches
                if &init_type != ty {
                    self.errors.push(SemanticError {
                        message: format!(
                            "Let binding '{name}' declares type {ty} but initializer has type {init_type}"
                        ),
                        span: *span,
                    });
                }

                // Add variable to current scope
                if let Some(scope) = self.scope_stack.last_mut() {
                    scope.insert(name.clone(), ty.clone());
                } else {
                    self.variables.insert(name.clone(), ty.clone());
                }
            }
            HirStatement::Assign { name, value, span } => {
                // Check variable exists
                let var_type = match self.lookup_variable(name) {
                    Some(ty) => ty,
                    None => {
                        self.errors.push(SemanticError {
                            message: format!("Assignment to undefined variable '{name}'"),
                            span: *span,
                        });
                        return;
                    }
                };

                // Validate value expression
                let value_type = self.validate_expression(value);

                // Check type matches
                if value_type != var_type {
                    self.errors.push(SemanticError {
                        message: format!(
                            "Cannot assign {value_type} to variable '{name}' of type {var_type}"
                        ),
                        span: *span,
                    });
                }
            }
            HirStatement::Expr(expr) => {
                // Just validate the expression
                self.validate_expression(expr);
            }
        }
    }

    /// Validate an expression and return its type
    fn validate_expression(&mut self, expr: &HirExpr) -> RueType {
        match expr {
            HirExpr::Literal { .. } => {
                // Literals are always valid, just return their type
                expr.ty().clone()
            }
            HirExpr::Var { name, ty, span } => {
                // Check variable exists and has correct type
                match self.lookup_variable(name) {
                    Some(expected_ty) => {
                        if ty != &expected_ty {
                            self.errors.push(SemanticError {
                                message: format!(
                                    "Variable '{name}' has type {ty} in HIR but {expected_ty} in scope"
                                ),
                                span: *span,
                            });
                        }
                        ty.clone()
                    }
                    None => {
                        self.errors.push(SemanticError {
                            message: format!("Reference to undefined variable '{name}'"),
                            span: *span,
                        });
                        ty.clone()
                    }
                }
            }
            HirExpr::Binary {
                op,
                lhs,
                rhs,
                ty,
                span,
            } => {
                let lhs_type = self.validate_expression(lhs);
                let rhs_type = self.validate_expression(rhs);

                // Check operand types match
                if lhs_type != rhs_type {
                    self.errors.push(SemanticError {
                        message: format!(
                            "Binary operator {op:?} requires matching operand types, found {lhs_type} and {rhs_type}"
                        ),
                        span: *span,
                    });
                }

                // Validate result type
                let expected_type = match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        // Arithmetic ops preserve operand type
                        if !matches!(lhs_type, RueType::I32 | RueType::I64) {
                            self.errors.push(SemanticError {
                                message: format!(
                                    "Arithmetic operator {op:?} requires numeric types, found {lhs_type}"
                                ),
                                span: *span,
                            });
                        }
                        lhs_type.clone()
                    }
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                        // Comparison ops return bool
                        RueType::Bool
                    }
                };

                if ty != &expected_type {
                    self.errors.push(SemanticError {
                        message: format!(
                            "Binary expression has type {ty} in HIR but should be {expected_type}"
                        ),
                        span: *span,
                    });
                }

                ty.clone()
            }
            HirExpr::Unary { op, expr, ty, span } => {
                let operand_type = self.validate_expression(expr);

                // Check operand type is numeric
                match op {
                    UnaryOp::Neg => {
                        if !matches!(operand_type, RueType::I32 | RueType::I64) {
                            self.errors.push(SemanticError {
                                message: format!(
                                    "Unary negation requires numeric type, found {operand_type}"
                                ),
                                span: *span,
                            });
                        }
                    }
                }

                // Result type should match operand type
                if ty != &operand_type {
                    self.errors.push(SemanticError {
                        message: format!(
                            "Unary expression has type {ty} in HIR but operand has type {operand_type}"
                        ),
                        span: *span,
                    });
                }

                ty.clone()
            }
            HirExpr::Call {
                func,
                args,
                ty,
                span,
            } => {
                // Look up function (clone to avoid borrow issues)
                let func_sig = self.functions.get(func).cloned();

                match func_sig {
                    Some((param_types, return_type)) => {
                        // Check argument count
                        if args.len() != param_types.len() {
                            self.errors.push(SemanticError {
                                message: format!(
                                    "Function '{}' expects {} arguments but {} provided",
                                    func,
                                    param_types.len(),
                                    args.len()
                                ),
                                span: *span,
                            });
                        } else {
                            // Check argument types
                            for (i, (arg, expected)) in args.iter().zip(&param_types).enumerate() {
                                let arg_type = self.validate_expression(arg);
                                if &arg_type != expected {
                                    self.errors.push(SemanticError {
                                        message: format!(
                                            "Argument {} to function '{}' has type {} but {} expected",
                                            i + 1,
                                            func,
                                            arg_type,
                                            expected
                                        ),
                                        span: arg.span(),
                                    });
                                }
                            }
                        }

                        // Check return type
                        if ty != &return_type {
                            self.errors.push(SemanticError {
                                message: format!(
                                    "Function call has type {ty} in HIR but function returns {return_type}"
                                ),
                                span: *span,
                            });
                        }
                    }
                    None => {
                        self.errors.push(SemanticError {
                            message: format!("Call to undefined function '{func}'"),
                            span: *span,
                        });
                    }
                }

                ty.clone()
            }
            HirExpr::If {
                cond,
                then_block,
                else_block,
                ty,
                span,
            } => {
                // Validate condition is bool
                let cond_type = self.validate_expression(cond);
                if cond_type != RueType::Bool {
                    self.errors.push(SemanticError {
                        message: format!("If condition must be bool, found {cond_type}"),
                        span: cond.span(),
                    });
                }

                // Validate branches
                let then_type = self.validate_block(then_block);
                let else_type = if let Some(else_block) = else_block {
                    self.validate_block(else_block)
                } else {
                    RueType::Unit
                };

                // Check branch types match
                if then_type != else_type {
                    self.errors.push(SemanticError {
                        message: format!(
                            "If branches have different types: {then_type} and {else_type}"
                        ),
                        span: *span,
                    });
                }

                // Check overall type
                if ty != &then_type {
                    self.errors.push(SemanticError {
                        message: format!(
                            "If expression has type {ty} in HIR but branches return {then_type}"
                        ),
                        span: *span,
                    });
                }

                ty.clone()
            }
            HirExpr::While { cond, body, .. } => {
                // Validate condition is bool
                let cond_type = self.validate_expression(cond);
                if cond_type != RueType::Bool {
                    self.errors.push(SemanticError {
                        message: format!("While condition must be bool, found {cond_type}"),
                        span: cond.span(),
                    });
                }

                // Validate body
                self.validate_block(body);

                // While always returns unit
                RueType::Unit
            }
            HirExpr::StructLiteral {
                struct_id: _,
                fields,
                ty,
                span: _,
            } => {
                // Validate all field expressions
                for (_field_name, field_expr) in fields {
                    self.validate_expression(field_expr);
                }
                // Return struct type
                ty.clone()
            }
            HirExpr::FieldAccess {
                base,
                field: _,
                ty,
                span: _,
            } => {
                // Validate base expression
                let base_type = self.validate_expression(base);

                // Verify base type supports field access
                match &base_type {
                    RueType::Struct(_) | RueType::Tuple(_) => {
                        // Valid base types for field access
                    }
                    _ => {
                        self.errors.push(SemanticError {
                            message: format!(
                                "Cannot access field of type '{base_type}' (not a struct or tuple)"
                            ),
                            span: base.span(),
                        });
                    }
                }

                ty.clone()
            }
            HirExpr::TupleLiteral {
                elements,
                ty,
                span: _,
            } => {
                // Validate all tuple elements
                for element in elements {
                    self.validate_expression(element);
                }
                ty.clone()
            }
            HirExpr::ArrayLiteral {
                elements,
                ty,
                span: _,
            } => {
                // Validate all array elements have same type
                if let RueType::Array(expected_elem_type, expected_len) = ty {
                    if elements.len() != *expected_len {
                        self.errors.push(SemanticError {
                            message: format!(
                                "Array literal has {} elements but type specifies {}",
                                elements.len(),
                                expected_len
                            ),
                            span: expr.span(),
                        });
                    }

                    for element in elements {
                        let elem_type = self.validate_expression(element);
                        if elem_type != **expected_elem_type {
                            self.errors.push(SemanticError {
                                message: format!(
                                    "Array element has type '{elem_type}' but expected '{expected_elem_type}'"
                                ),
                                span: element.span(),
                            });
                        }
                    }
                }
                ty.clone()
            }
            HirExpr::ArrayAccess {
                base,
                index,
                ty,
                span: _,
            } => {
                // Validate base is array
                let base_type = self.validate_expression(base);
                match &base_type {
                    RueType::Array(_, _) => {
                        // Valid
                    }
                    _ => {
                        self.errors.push(SemanticError {
                            message: format!("Cannot index into type '{base_type}' (not an array)"),
                            span: base.span(),
                        });
                    }
                }

                // Validate index is integer
                let index_type = self.validate_expression(index);
                if !matches!(index_type, RueType::I32 | RueType::I64) {
                    self.errors.push(SemanticError {
                        message: format!("Array index must be integer type, found '{index_type}'"),
                        span: index.span(),
                    });
                }

                ty.clone()
            }
        }
    }

    /// Push a new scope
    fn push_scope(&mut self) {
        self.scope_stack.push(HashMap::new());
    }

    /// Pop the current scope
    fn pop_scope(&mut self) {
        self.scope_stack.pop();
    }

    /// Look up a variable in the current scope chain
    fn lookup_variable(&self, name: &str) -> Option<RueType> {
        // Check scope stack from innermost to outermost
        for scope in self.scope_stack.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty.clone());
            }
        }

        // Check function-level variables
        self.variables.get(name).cloned()
    }
}

/// Extension trait to get span from expressions
trait HasSpan {
    fn span(&self) -> Span;
}

impl HasSpan for HirExpr {
    fn span(&self) -> Span {
        match self {
            HirExpr::Literal { span, .. } => *span,
            HirExpr::Var { span, .. } => *span,
            HirExpr::Binary { span, .. } => *span,
            HirExpr::Unary { span, .. } => *span,
            HirExpr::Call { span, .. } => *span,
            HirExpr::If { span, .. } => *span,
            HirExpr::While { span, .. } => *span,
            HirExpr::StructLiteral { span, .. } => *span,
            HirExpr::FieldAccess { span, .. } => *span,
            HirExpr::TupleLiteral { span, .. } => *span,
            HirExpr::ArrayLiteral { span, .. } => *span,
            HirExpr::ArrayAccess { span, .. } => *span,
        }
    }
}

#[cfg(test)]
mod tests;
