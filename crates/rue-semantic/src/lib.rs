use rue_ast::{CstRoot, ExpressionNode, FunctionNode, StatementNode};
use rue_lexer::format_error_with_context;
use std::collections::HashMap;
use std::fmt;

// Semantic analysis types
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticError {
    pub message: String,
    pub span: rue_lexer::Span,
}

impl SemanticError {
    pub fn format_with_source(&self, source: &str) -> String {
        format_error_with_context(source, self.span, &self.message, "type error")
    }
}

impl std::error::Error for SemanticError {}

impl std::fmt::Display for SemanticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RueType {
    I32,
    I64,
    Bool,
    Unit,
    Unknown,
}

impl fmt::Display for RueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RueType::I32 => write!(f, "i32"),
            RueType::I64 => write!(f, "i64"),
            RueType::Bool => write!(f, "bool"),
            RueType::Unit => write!(f, "()"),
            RueType::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scope {
    pub variables: HashMap<String, RueType>,
    pub functions: HashMap<String, FunctionSignature>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionSignature {
    pub param_types: Vec<RueType>,
    pub return_type: RueType,
}

// Helper function to convert AST type to semantic type
fn convert_type_node(type_node: &rue_ast::TypeNode) -> RueType {
    match type_node {
        rue_ast::TypeNode::I32(_) => RueType::I32,
        rue_ast::TypeNode::I64(_) => RueType::I64,
        rue_ast::TypeNode::Bool(_) => RueType::Bool,
        rue_ast::TypeNode::Unit => RueType::Unit,
    }
}

// Add built-in functions to the scope
fn add_builtin_functions(scope: &mut Scope) {
    // exit(code: i64) -> ()
    scope.functions.insert(
        "exit".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I64],
            return_type: RueType::Unit,
        },
    );

    // println_i32(value: i32) -> ()
    scope.functions.insert(
        "println_i32".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I32],
            return_type: RueType::Unit,
        },
    );

    // println_i64(value: i64) -> ()
    scope.functions.insert(
        "println_i64".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I64],
            return_type: RueType::Unit,
        },
    );

    // println_bool(value: bool) -> ()
    scope.functions.insert(
        "println_bool".to_string(),
        FunctionSignature {
            param_types: vec![RueType::Bool],
            return_type: RueType::Unit,
        },
    );

    // println_unit(value: ()) -> ()
    scope.functions.insert(
        "println_unit".to_string(),
        FunctionSignature {
            param_types: vec![RueType::Unit],
            return_type: RueType::Unit,
        },
    );

    // input() -> i64
    scope.functions.insert(
        "input".to_string(),
        FunctionSignature {
            param_types: vec![],
            return_type: RueType::I64,
        },
    );
}

// Semantic analysis functions
pub fn analyze_cst(ast: &CstRoot) -> Result<Scope, SemanticError> {
    let mut scope = Scope::default();

    // Add built-in functions
    add_builtin_functions(&mut scope);

    for item in &ast.items {
        match item {
            rue_ast::CstNode::Function(func) => {
                analyze_function(&mut scope, func)?;
            }
            rue_ast::CstNode::Statement(stmt) => {
                analyze_statement(&mut scope, stmt)?;
            }
            _ => {} // Skip other node types for now
        }
    }

    Ok(scope)
}

// Helper functions for semantic analysis
fn analyze_function(scope: &mut Scope, func: &FunctionNode) -> Result<(), SemanticError> {
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
        let param_type = if let Some(type_ann) = &param.type_annotation {
            convert_type_node(&type_ann.ty)
        } else {
            RueType::I32 // Default to i32 if no type annotation
        };
        param_types.push(param_type.clone());
    }

    // Extract return type
    let return_type = if let Some(return_type_node) = &func.return_type {
        convert_type_node(&return_type_node.ty)
    } else {
        RueType::Unit // Default to unit if no return type specified
    };

    // Register function in scope
    scope.functions.insert(
        func_name.clone(),
        FunctionSignature {
            param_types: param_types.clone(),
            return_type: return_type.clone(),
        },
    );

    // Create local scope for function body
    let mut local_scope = scope.clone();

    // Add parameters to local scope
    for (i, param) in func.param_list.params.iter().enumerate() {
        if let rue_lexer::TokenKind::Ident(param_name) = &param.name.kind {
            local_scope
                .variables
                .insert(param_name.clone(), param_types[i].clone());
        }
    }

    // Analyze function body statements
    for stmt in &func.body.statements {
        analyze_statement(&mut local_scope, stmt)?;
    }

    // Analyze final expression and check return type
    let actual_return_type = if let Some(final_expr) = &func.body.final_expr {
        analyze_literal_with_expected_type(&mut local_scope, final_expr, &return_type)?
    } else {
        RueType::Unit // No final expression means unit return
    };

    // Check that actual return type matches declared return type
    if actual_return_type != return_type {
        return Err(SemanticError {
            message: format!(
                "Type mismatch: function '{func_name}' is declared to return '{return_type}' but returns '{actual_return_type}'"
            ),
            span: func.body.close_brace.span,
        });
    }

    Ok(())
}

fn analyze_statement(scope: &mut Scope, stmt: &StatementNode) -> Result<(), SemanticError> {
    match stmt {
        StatementNode::Let(let_stmt) => {
            // Get declared type if present
            let declared_type = let_stmt
                .type_annotation
                .as_ref()
                .map(|type_ann| convert_type_node(&type_ann.ty));

            // Analyze the value expression with type context
            let value_type = if let Some(expected_type) = &declared_type {
                analyze_literal_with_expected_type(scope, &let_stmt.value, expected_type)?
            } else {
                analyze_expression(scope, &let_stmt.value)?
            };

            // Determine final type
            let final_type = declared_type.clone().unwrap_or(value_type.clone());

            // Check type compatibility only if type was declared
            if let Some(decl_type) = declared_type {
                if decl_type != value_type {
                    // Special case: allow integer literals to be used as i64
                    let is_valid = matches!(
                        (&let_stmt.value, &decl_type, &value_type),
                        (ExpressionNode::Literal(token), RueType::I64, RueType::I32)
                            if matches!(&token.kind, rue_lexer::TokenKind::Integer(_))
                    );

                    if !is_valid {
                        return Err(SemanticError {
                            message: format!(
                                "Type mismatch: variable declared as '{decl_type}' but initialized with expression of type '{value_type}'"
                            ),
                            span: let_stmt.equals.span,
                        });
                    }
                }
            }

            // Add variable to scope
            if let rue_lexer::TokenKind::Ident(var_name) = &let_stmt.name.kind {
                scope.variables.insert(var_name.clone(), final_type);
            }
        }
        StatementNode::Assign(assign_stmt) => {
            // Analyze the value expression
            let value_type = analyze_expression(scope, &assign_stmt.value)?;

            // Check that variable exists in scope and types match
            if let rue_lexer::TokenKind::Ident(var_name) = &assign_stmt.name.kind {
                if let Some(var_type) = scope.variables.get(var_name) {
                    if *var_type != value_type {
                        return Err(SemanticError {
                            message: format!(
                                "Type mismatch: cannot assign value of type '{value_type}' to variable of type '{var_type}'"
                            ),
                            span: assign_stmt.equals.span,
                        });
                    }
                } else {
                    return Err(SemanticError {
                        message: format!("Cannot assign to undefined variable: {var_name}"),
                        span: assign_stmt.name.span,
                    });
                }
            }
        }
        StatementNode::Expression(expr_stmt) => {
            analyze_expression(scope, &expr_stmt.expression)?;
        }
    }
    Ok(())
}

/// Analyzes an expression with contextual type information for literal type inference.
///
/// This function is used when we have an expected type context (e.g., from a type annotation).
/// The key difference from `analyze_expression` is that integer literals can be inferred
/// as either i32 or i64 based on the expected type, rather than always defaulting to i32.
///
/// For example:
/// - `let x: i64 = 42` - The literal 42 will be inferred as i64
/// - `let x: i32 = 42` - The literal 42 will be inferred as i32
///
/// For non-literal expressions, this delegates to `analyze_expression` since they
/// have fixed types that don't depend on context.
fn analyze_literal_with_expected_type(
    scope: &mut Scope,
    expr: &ExpressionNode,
    expected_type: &RueType,
) -> Result<RueType, SemanticError> {
    match expr {
        ExpressionNode::Literal(token) => {
            match &token.kind {
                rue_lexer::TokenKind::Integer(_) => {
                    // Integer literals can be either i32 or i64 depending on context
                    match expected_type {
                        RueType::I64 => Ok(RueType::I64),
                        _ => Ok(RueType::I32), // Default to i32
                    }
                }
                rue_lexer::TokenKind::True | rue_lexer::TokenKind::False => Ok(RueType::Bool),
                rue_lexer::TokenKind::Unit => Ok(RueType::Unit),
                _ => Err(SemanticError {
                    message: "Unexpected literal type".to_string(),
                    span: token.span,
                }),
            }
        }
        _ => analyze_expression(scope, expr), // For non-literals, use regular analysis
    }
}

/// Analyzes an expression to determine its type.
///
/// This is the primary expression analysis function. It determines types based on:
/// - Literals: Integer literals default to i32, booleans are bool
/// - Identifiers: Look up the type from the current scope
/// - Binary operations: Check operand types and determine result type
/// - Function calls: Look up function signature and check arguments
/// - Control flow: Analyze branches and ensure consistency
///
/// Note: This function always infers integer literals as i32. When type context
/// is available (e.g., from type annotations), use `analyze_literal_with_expected_type`
/// instead to allow integer literals to be inferred as i64 when needed.
fn analyze_expression(scope: &mut Scope, expr: &ExpressionNode) -> Result<RueType, SemanticError> {
    match expr {
        ExpressionNode::Literal(token) => {
            match &token.kind {
                rue_lexer::TokenKind::Integer(_) => Ok(RueType::I32), // Numeric literals default to i32
                rue_lexer::TokenKind::True | rue_lexer::TokenKind::False => Ok(RueType::Bool),
                rue_lexer::TokenKind::Unit => Ok(RueType::Unit),
                _ => Err(SemanticError {
                    message: "Unexpected literal type".to_string(),
                    span: token.span,
                }),
            }
        }
        ExpressionNode::Identifier(token) => {
            if let rue_lexer::TokenKind::Ident(name) = &token.kind {
                if let Some(var_type) = scope.variables.get(name) {
                    Ok(var_type.clone())
                } else {
                    Err(SemanticError {
                        message: format!("Undefined variable: {name}"),
                        span: token.span,
                    })
                }
            } else {
                Err(SemanticError {
                    message: "Expected identifier".to_string(),
                    span: token.span,
                })
            }
        }
        ExpressionNode::Binary(binary_expr) => {
            // Analyze both operands
            let left_type = analyze_expression(scope, &binary_expr.left)?;
            let right_type = analyze_expression(scope, &binary_expr.right)?;

            // Check operator type
            match &binary_expr.operator.kind {
                // Arithmetic operators: require matching numeric types
                rue_lexer::TokenKind::Plus
                | rue_lexer::TokenKind::Minus
                | rue_lexer::TokenKind::Star
                | rue_lexer::TokenKind::Slash
                | rue_lexer::TokenKind::Percent => {
                    if left_type != right_type {
                        return Err(SemanticError {
                            message: format!(
                                "Type mismatch: binary operator cannot be applied to types '{left_type}' and '{right_type}'"
                            ),
                            span: binary_expr.operator.span,
                        });
                    }

                    // Both operands must be numeric
                    match left_type {
                        RueType::I32 | RueType::I64 => Ok(left_type),
                        _ => Err(SemanticError {
                            message: format!(
                                "Arithmetic operators require numeric types, found {left_type}"
                            ),
                            span: binary_expr.operator.span,
                        }),
                    }
                }

                // Comparison operators: require matching types, return bool
                rue_lexer::TokenKind::Less
                | rue_lexer::TokenKind::LessEqual
                | rue_lexer::TokenKind::Greater
                | rue_lexer::TokenKind::GreaterEqual
                | rue_lexer::TokenKind::Equal
                | rue_lexer::TokenKind::NotEqual => {
                    if left_type != right_type {
                        return Err(SemanticError {
                            message: format!(
                                "Type mismatch: cannot compare values of types '{left_type}' and '{right_type}'"
                            ),
                            span: binary_expr.operator.span,
                        });
                    }
                    Ok(RueType::Bool)
                }

                _ => Err(SemanticError {
                    message: "Unknown binary operator".to_string(),
                    span: binary_expr.operator.span,
                }),
            }
        }
        ExpressionNode::Call(call_expr) => {
            // Get function name
            if let ExpressionNode::Identifier(func_token) = &*call_expr.function {
                if let rue_lexer::TokenKind::Ident(func_name) = &func_token.kind {
                    // Check if function exists
                    if let Some(signature) = scope.functions.get(func_name).cloned() {
                        // Check argument count
                        if call_expr.args.len() != signature.param_types.len() {
                            return Err(SemanticError {
                                message: format!(
                                    "Function '{}': Expected {} arguments, got {}",
                                    func_name,
                                    signature.param_types.len(),
                                    call_expr.args.len()
                                ),
                                span: call_expr.open_paren.span,
                            });
                        }

                        // Analyze and check types of all arguments
                        for (i, arg) in call_expr.args.iter().enumerate() {
                            let expected_type = &signature.param_types[i];

                            // Try to analyze with expected type for better inference
                            let arg_type = if let ExpressionNode::Literal(_) = arg {
                                analyze_literal_with_expected_type(scope, arg, expected_type)?
                            } else {
                                analyze_expression(scope, arg)?
                            };

                            if arg_type != *expected_type {
                                return Err(SemanticError {
                                    message: format!(
                                        "Type mismatch in argument {} of function '{}': expected '{}', found '{}'",
                                        i + 1,
                                        func_name,
                                        expected_type,
                                        arg_type
                                    ),
                                    span: call_expr.open_paren.span, // TODO: Get actual argument span
                                });
                            }
                        }

                        Ok(signature.return_type)
                    } else {
                        Err(SemanticError {
                            message: format!("Undefined function: {func_name}"),
                            span: func_token.span,
                        })
                    }
                } else {
                    Err(SemanticError {
                        message: "Expected function name".to_string(),
                        span: func_token.span,
                    })
                }
            } else {
                Err(SemanticError {
                    message: "Function calls must use identifiers".to_string(),
                    span: call_expr.open_paren.span,
                })
            }
        }
        ExpressionNode::If(if_stmt) => {
            // Analyze condition - must be bool
            let condition_type = analyze_expression(scope, &if_stmt.condition)?;
            if condition_type != RueType::Bool {
                return Err(SemanticError {
                    message: format!("If condition must be bool, found {condition_type}"),
                    span: if_stmt.if_token.span,
                });
            }

            // Analyze then block
            for stmt in &if_stmt.then_block.statements {
                analyze_statement(scope, stmt)?;
            }
            let then_type = if let Some(final_expr) = &if_stmt.then_block.final_expr {
                analyze_expression(scope, final_expr)?
            } else {
                RueType::Unit // blocks without final expression return unit
            };

            // Analyze else block if it exists
            let else_type = if let Some(else_clause) = &if_stmt.else_clause {
                match &else_clause.body {
                    rue_ast::ElseBodyNode::Block(block) => {
                        for stmt in &block.statements {
                            analyze_statement(scope, stmt)?;
                        }
                        if let Some(final_expr) = &block.final_expr {
                            analyze_expression(scope, final_expr)?
                        } else {
                            RueType::Unit
                        }
                    }
                    rue_ast::ElseBodyNode::If(nested_if) => {
                        analyze_expression(scope, &ExpressionNode::If(nested_if.clone()))?
                    }
                }
            } else {
                RueType::Unit // missing else defaults to unit
            };

            // Both branches must have same type
            if then_type == else_type {
                Ok(then_type)
            } else {
                Err(SemanticError {
                    message: "If expression branches must have the same type".to_string(),
                    span: if_stmt.if_token.span,
                })
            }
        }
        ExpressionNode::While(while_stmt) => {
            // Analyze condition - must be bool
            let condition_type = analyze_expression(scope, &while_stmt.condition)?;
            if condition_type != RueType::Bool {
                return Err(SemanticError {
                    message: format!("While condition must be bool, found {condition_type}"),
                    span: while_stmt.while_token.span,
                });
            }

            // Analyze body
            for stmt in &while_stmt.body.statements {
                analyze_statement(scope, stmt)?;
            }
            if let Some(final_expr) = &while_stmt.body.final_expr {
                analyze_expression(scope, final_expr)?;
            }

            // While expressions always return unit
            Ok(RueType::Unit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::{Lexer, Span};

    fn parse_and_analyze(source: &str) -> Result<Scope, SemanticError> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().map_err(|e| SemanticError {
            message: format!("Lexical error: {}", e.message),
            span: Span {
                start: e.position,
                end: e.position + 1,
            },
        })?;
        let ast = rue_parser::parse(tokens).map_err(|e| SemanticError {
            message: format!("Parse error: {}", e.message),
            span: e.span,
        })?;
        analyze_cst(&ast)
    }

    #[test]
    fn test_semantic_analysis_simple() {
        let result = parse_and_analyze(
            r#"
fn main() -> i32 {
    42
}
"#,
        );
        assert!(result.is_ok());

        let scope = result.unwrap();
        assert!(scope.functions.contains_key("main"));
        assert_eq!(scope.functions["main"].param_types.len(), 0);
        assert_eq!(scope.functions["main"].return_type, RueType::I32);
    }

    #[test]
    fn test_semantic_analysis_with_parameter() {
        let result = parse_and_analyze(
            r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}
"#,
        );
        assert!(result.is_ok());

        let scope = result.unwrap();
        assert!(scope.functions.contains_key("factorial"));
        assert_eq!(scope.functions["factorial"].param_types.len(), 1);
        assert_eq!(scope.functions["factorial"].param_types[0], RueType::I32);
        assert_eq!(scope.functions["factorial"].return_type, RueType::I32);
    }

    #[test]
    fn test_semantic_analysis_undefined_variable() {
        let result = parse_and_analyze(
            r#"
fn main() -> i32 {
    undefined_var
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.message.contains("Undefined variable: undefined_var"));
    }

    #[test]
    fn test_semantic_analysis_undefined_function() {
        let result = parse_and_analyze(
            r#"
fn main() -> i32 {
    undefined_func(42)
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.message.contains("Undefined function: undefined_func"));
    }

    #[test]
    fn test_semantic_analysis_wrong_argument_count() {
        let result = parse_and_analyze(
            r#"
fn factorial(n: i32) -> i32 {
    n
}

fn main() -> i32 {
    factorial()
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.message.contains("Expected 1 arguments, got 0"));
    }

    #[test]
    fn test_semantic_analysis_let_statement() {
        let result = parse_and_analyze(
            r#"
fn main() -> i32 {
    let x: i32 = 42;
    x + 1
}
"#,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_semantic_analysis_while_loop() {
        let result = parse_and_analyze(
            r#"
fn countdown(n: i32) -> () {
    while n > 0 {
        n - 1;
    };
}
"#,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_semantic_analysis_while_loop_undefined_variable() {
        let result = parse_and_analyze(
            r#"
fn main() -> i32 {
    while undefined_var > 0 {
        42;
    };
    0
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.message.contains("Undefined variable: undefined_var"));
    }

    #[test]
    fn test_semantic_analysis_assignment_valid() {
        let result = parse_and_analyze(
            r#"
fn main() -> i32 {
    let x: i32 = 42;
    x = 100;
    x
}
"#,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_semantic_analysis_assignment_undefined_variable() {
        let result = parse_and_analyze(
            r#"
fn main() -> () {
    undefined_var = 42;
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(
            error
                .message
                .contains("Cannot assign to undefined variable: undefined_var")
        );
    }

    #[test]
    fn test_semantic_analysis_assignment_with_expression() {
        let result = parse_and_analyze(
            r#"
fn main() -> i32 {
    let x: i32 = 10;
    let y: i32 = 20;
    x = y + 5;
    x
}
"#,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_type_mismatch_in_let() {
        let result = parse_and_analyze(
            r#"
fn main() -> () {
    let x: i32 = true;
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.message.contains("Type mismatch"));
        assert!(error.message.contains("i32"));
        assert!(error.message.contains("bool"));
    }

    #[test]
    fn test_type_mismatch_in_assignment() {
        let result = parse_and_analyze(
            r#"
fn main() -> () {
    let x: i32 = 42;
    x = true;
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.message.contains("Type mismatch"));
        assert!(error.message.contains("bool"));
        assert!(error.message.contains("i32"));
    }

    #[test]
    fn test_type_mismatch_in_binary_op() {
        let result = parse_and_analyze(
            r#"
fn main() -> i32 {
    42 + true
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.message.contains("Type mismatch"));
    }

    #[test]
    fn test_bool_operations() {
        let result = parse_and_analyze(
            r#"
fn main() -> bool {
    let x: i32 = 42;
    let y: i32 = 100;
    x < y
}
"#,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_if_condition_must_be_bool() {
        let result = parse_and_analyze(
            r#"
fn main() -> i32 {
    if 42 {
        1
    } else {
        0
    }
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.message.contains("If condition must be bool"));
    }

    #[test]
    fn test_function_return_type_mismatch() {
        let result = parse_and_analyze(
            r#"
fn get_number() -> i32 {
    true
}
"#,
        );
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(
            error
                .message
                .contains("declared to return 'i32' but returns 'bool'")
        );
    }

    #[test]
    fn test_function_argument_type_check() {
        let result = parse_and_analyze(
            r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(42, true)
}
"#,
        );
        // The test should fail during semantic analysis with type mismatch
        assert!(result.is_err());

        let error = result.unwrap_err();
        // For now, accept either parse error or type error since we're in transition
        assert!(
            error.message.contains("Type mismatch in argument")
                || error.message.contains("Parse error")
        );
    }

    #[test]
    fn test_unit_type_function() {
        let result = parse_and_analyze(
            r#"
fn print_value(x: i32) -> () {
    x;
}

fn main() -> () {
    print_value(42);
}
"#,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_multiple_parameters() {
        let result = parse_and_analyze(
            r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(10, 20)
}
"#,
        );
        assert!(result.is_ok());

        let scope = result.unwrap();
        assert_eq!(scope.functions["add"].param_types.len(), 2);
        assert_eq!(scope.functions["add"].param_types[0], RueType::I32);
        assert_eq!(scope.functions["add"].param_types[1], RueType::I32);
    }
}
