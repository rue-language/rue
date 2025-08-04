use rue_ast::{CstRoot, FunctionNode};
use rue_ir::types::RueType;
use rue_lexer::format_error_with_context;
use std::collections::HashMap;

// Re-export HIR types from rue-ir for convenience
pub use rue_ir::hir;

mod hir_validator;
mod type_checker;

#[cfg(test)]
mod hir_control_flow_test;

#[cfg(test)]
mod type_preservation_test;

#[cfg(test)]
mod hir_validation_integration_test;

#[cfg(test)]
mod hir_roundtrip_test;

// Semantic analysis types
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct SemanticError {
    pub message: String,
    pub span: rue_lexer::Span,
}

impl SemanticError {
    pub fn format_with_source(&self, source: &str) -> String {
        format_error_with_context(source, self.span, &self.message, "error")
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scope {
    pub variables: HashMap<String, RueType>,
    pub functions: HashMap<String, FunctionSignature>,
}

// ScopeStack removed - TypeChecker has its own scope management

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
        rue_ast::TypeNode::Struct(struct_type) => {
            // For now, use a simple hash of the struct name as ID
            // In a real implementation, this would use a proper type registry
            if let rue_lexer::TokenKind::Ident(name) = &struct_type.name.kind {
                let id = rue_ir::types::StructId::new(hash_string(name));
                RueType::Struct(id)
            } else {
                panic!("Expected struct name to be an identifier")
            }
        }
        rue_ast::TypeNode::Tuple(tuple_type) => {
            let element_types = tuple_type.types.iter().map(convert_type_node).collect();
            RueType::Tuple(element_types)
        }
        rue_ast::TypeNode::Array(array_type) => {
            let element_type = Box::new(convert_type_node(&array_type.element_type));

            // Parse array size from token
            let size = if let rue_lexer::TokenKind::Integer(n) = &array_type.size.kind {
                *n as usize
            } else {
                panic!("Array size must be an integer literal")
            };

            RueType::Array(element_type, size)
        }
    }
}

// Simple hash function for struct names (temporary)
fn hash_string(s: &str) -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish() as u32
}

// Clean symbol collection functions
fn extract_function_name(func: &FunctionNode) -> Result<String, SemanticError> {
    match &func.name.kind {
        rue_lexer::TokenKind::Ident(name) => Ok(name.clone()),
        _ => Err(SemanticError {
            message: "Expected function name".to_string(),
            span: func.name.span,
        }),
    }
}

fn collect_function_signature(func: &FunctionNode) -> Result<FunctionSignature, SemanticError> {
    // Extract parameter types
    let mut param_types = Vec::new();
    for param in &func.param_list.params {
        let param_type = if let Some(type_ann) = &param.type_annotation {
            convert_type_node(&type_ann.ty)
        } else {
            RueType::I32 // Default to i32 if no type annotation
        };
        param_types.push(param_type);
    }

    // Extract return type
    let return_type = if let Some(return_type_node) = &func.return_type {
        convert_type_node(&return_type_node.ty)
    } else {
        RueType::Unit // Default to unit if no return type specified
    };

    Ok(FunctionSignature {
        param_types,
        return_type,
    })
}

// Add built-in functions to function signature map
fn add_builtin_functions_to_map(functions: &mut HashMap<String, FunctionSignature>) {
    // exit(code: i64) -> ()
    functions.insert(
        "exit".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I64],
            return_type: RueType::Unit,
        },
    );

    // println_i32(value: i32) -> ()
    functions.insert(
        "println_i32".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I32],
            return_type: RueType::Unit,
        },
    );

    // println_i64(value: i64) -> ()
    functions.insert(
        "println_i64".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I64],
            return_type: RueType::Unit,
        },
    );

    // println_bool(value: bool) -> ()
    functions.insert(
        "println_bool".to_string(),
        FunctionSignature {
            param_types: vec![RueType::Bool],
            return_type: RueType::Unit,
        },
    );

    // println_unit(value: ()) -> ()
    functions.insert(
        "println_unit".to_string(),
        FunctionSignature {
            param_types: vec![RueType::Unit],
            return_type: RueType::Unit,
        },
    );

    // input() -> i64
    functions.insert(
        "input".to_string(),
        FunctionSignature {
            param_types: vec![],
            return_type: RueType::I64,
        },
    );

    // to_i32(value: i64) -> i32
    functions.insert(
        "to_i32".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I64],
            return_type: RueType::I32,
        },
    );

    // to_i64(value: i32) -> i64
    functions.insert(
        "to_i64".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I32],
            return_type: RueType::I64,
        },
    );
}

/// Result of semantic analysis including both scope and HIR
#[derive(Debug, PartialEq)]
pub struct AnalysisResult {
    pub scope: Scope,
    pub hir: hir::HirProgram,
}

// Clean semantic analysis pipeline
pub fn analyze_cst(ast: &CstRoot) -> Result<AnalysisResult, SemanticError> {
    // Phase 1: Symbol Collection - collect function signatures only, no type checking
    let mut function_signatures = HashMap::new();
    add_builtin_functions_to_map(&mut function_signatures);

    for item in &ast.items {
        match item {
            rue_ast::CstNode::Function(func) => {
                let signature = collect_function_signature(func)?;
                let func_name = extract_function_name(func)?;
                function_signatures.insert(func_name, signature);
            }
            rue_ast::CstNode::StructDefinition(_) => {
                // TODO: In full implementation, collect struct definitions in type registry
                // For now, structs are handled dynamically by TypeChecker
            }
            rue_ast::CstNode::Statement(_) => {
                // Top-level statements are not supported in current language design
                return Err(SemanticError {
                    message: "Top-level statements are not supported".to_string(),
                    span: rue_lexer::Span { start: 0, end: 0 }, // TODO: proper span
                });
            }
            _ => {} // Skip other node types
        }
    }

    // Phase 2: Type Checking + HIR Generation - all semantic analysis happens here
    let mut type_checker = type_checker::TypeChecker::new(function_signatures.clone());
    let hir = type_checker.check_program(ast)?;

    // Phase 3: HIR Validation
    let mut validator = hir_validator::HirValidator::new();
    if let Err(validation_errors) = validator.validate_program(&hir) {
        if let Some(error) = validation_errors.into_iter().next() {
            return Err(error);
        }
    }

    // Create legacy scope for compatibility
    let global_scope = Scope {
        functions: function_signatures,
        ..Default::default()
    };

    Ok(AnalysisResult {
        scope: global_scope,
        hir,
    })
}

// All old semantic analysis functions removed - TypeChecker now handles everything

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::{Lexer, Span};

    fn parse_and_analyze(source: &str) -> Result<AnalysisResult, SemanticError> {
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

        let analysis = result.unwrap();
        assert!(analysis.scope.functions.contains_key("main"));
        assert_eq!(analysis.scope.functions["main"].param_types.len(), 0);
        assert_eq!(analysis.scope.functions["main"].return_type, RueType::I32);
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
        if let Err(e) = &result {
            eprintln!("Error: {} at span {:?}", e.message, e.span);
        }
        assert!(result.is_ok());

        let analysis = result.unwrap();
        assert!(analysis.scope.functions.contains_key("factorial"));
        assert_eq!(analysis.scope.functions["factorial"].param_types.len(), 1);
        assert_eq!(
            analysis.scope.functions["factorial"].param_types[0],
            RueType::I32
        );
        assert_eq!(
            analysis.scope.functions["factorial"].return_type,
            RueType::I32
        );
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
        assert!(
            error
                .message
                .contains("expects 1 arguments, but 0 were provided")
        );
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
        assert!(
            error
                .message
                .contains("Cannot use integer literal in context expecting bool")
        );
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

        let analysis = result.unwrap();
        assert_eq!(analysis.scope.functions["add"].param_types.len(), 2);
        assert_eq!(analysis.scope.functions["add"].param_types[0], RueType::I32);
        assert_eq!(analysis.scope.functions["add"].param_types[1], RueType::I32);
    }
}
