use rue_ast::{CstRoot, StructDefinitionNode};
use rue_diagnostic::{Diagnostic, DiagnosticBuilder, E2001, E2002, E3001, E3002, SourceId};
use rue_ir::types::{RueType, StructDef, StructId, TypeContext};
use std::collections::HashMap;

// Re-export HIR types from rue-ir for convenience
pub use rue_ir::hir;

mod diagnostics;
mod hir_validator;
pub(crate) mod type_checker;
mod type_checker_diagnostics;

pub use diagnostics::{
    SemanticDiagnostic, SemanticSeverity, semantic_error_to_diagnostic,
    type_mismatch_with_locations, unreachable_code_warning,
};
pub use type_checker_diagnostics::DiagnosticTypeChecker;

#[cfg(test)]
mod hir_control_flow_test;

#[cfg(test)]
mod type_preservation_test;

#[cfg(test)]
mod hir_validation_integration_test;

#[cfg(test)]
mod hir_roundtrip_test;

#[cfg(test)]
mod aggregate_type_tests;

#[cfg(test)]
mod constraint_inference_test;

// Semantic analysis types - now unified with diagnostic system
#[derive(Debug, Clone)]
pub struct SemanticError {
    pub message: String,
    pub span: rue_lexer::Span,
}

/// Create a diagnostic for undefined variable errors
pub fn create_undefined_variable_error(name: &str, span: rue_lexer::Span) -> Diagnostic {
    DiagnosticBuilder::error(format!("Undefined variable '{name}'"), SourceId::stdin())
        .with_code(E2001)
        .with_primary_label(span, "not found in this scope")
        .with_help(format!("Did you mean to declare '{name}' before using it?"))
        .build()
}

/// Create a diagnostic for type mismatch errors
pub fn create_type_mismatch_error(
    expected: &RueType,
    found: &RueType,
    span: rue_lexer::Span,
) -> Diagnostic {
    DiagnosticBuilder::error(
        format!("Type mismatch: expected '{expected}', found '{found}'"),
        SourceId::stdin(),
    )
    .with_code(E3001)
    .with_primary_label(span, format!("has type '{found}'"))
    .with_help(format!(
        "Expected type '{expected}', consider adding a cast or changing the value"
    ))
    .build()
}

/// Create a diagnostic for arity mismatch errors
pub fn create_arity_mismatch_error(
    expected: usize,
    found: usize,
    span: rue_lexer::Span,
) -> Diagnostic {
    DiagnosticBuilder::error(
        format!("Arity mismatch: expected {expected} arguments, found {found}"),
        SourceId::stdin(),
    )
    .with_code(E3002)
    .with_primary_label(
        span,
        format!(
            "{} {} provided",
            found,
            if found == 1 { "argument" } else { "arguments" }
        ),
    )
    .with_help(format!("This function expects {expected} arguments"))
    .build()
}

/// Create a diagnostic for duplicate definition errors
pub fn create_duplicate_definition_error(
    name: &str,
    item_type: &str,
    span: rue_lexer::Span,
) -> Diagnostic {
    DiagnosticBuilder::error(
        format!("Duplicate {item_type} definition: '{name}'"),
        SourceId::stdin(),
    )
    .with_code(E2002)
    .with_primary_label(span, "duplicate definition")
    .with_help(format!(
        "Each {item_type} must have a unique name in its scope"
    ))
    .build()
}

/// Create a generic semantic error
pub fn create_semantic_error(message: &str, span: rue_lexer::Span) -> SemanticError {
    SemanticError {
        message: message.to_string(),
        span,
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scope {
    pub variables: HashMap<String, RueType>,
    pub functions: HashMap<String, FunctionSignature>,
    pub structs: HashMap<String, StructDef>,
    pub struct_id_counter: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionSignature {
    pub param_types: Vec<RueType>,
    pub return_type: RueType,
}

impl Scope {
    /// Register a new struct definition and return its ID
    pub fn register_struct(&mut self, name: String, fields: Vec<(String, RueType)>) -> StructId {
        let struct_id = StructId::new(self.struct_id_counter);
        self.struct_id_counter += 1;

        let struct_def = StructDef::new(struct_id, name.clone(), fields);
        self.structs.insert(name, struct_def);

        struct_id
    }

    /// Look up a struct definition by name
    pub fn get_struct(&self, name: &str) -> Option<&StructDef> {
        self.structs.get(name)
    }

    /// Look up a struct definition by ID (for convenience)
    pub fn get_struct_by_id(&self, id: StructId) -> Option<&StructDef> {
        self.structs.values().find(|def| def.id == id)
    }
}

// Helper function to convert AST type to semantic type with scope context
fn convert_type_node_with_scope(
    type_node: &rue_ast::TypeNode,
    scope: &Scope,
) -> Result<RueType, SemanticError> {
    match type_node {
        rue_ast::TypeNode::I32(_) => Ok(RueType::I32),
        rue_ast::TypeNode::I64(_) => Ok(RueType::I64),
        rue_ast::TypeNode::Bool(_) => Ok(RueType::Bool),
        rue_ast::TypeNode::Unit => Ok(RueType::Unit),
        rue_ast::TypeNode::Struct(struct_type) => {
            // Extract struct name from the token
            if let rue_lexer::TokenKind::Ident(struct_name) = &struct_type.name.kind {
                if let Some(struct_def) = scope.get_struct(struct_name) {
                    Ok(RueType::Struct(struct_def.id))
                } else {
                    Err(SemanticError {
                        message: format!("Undefined struct type: {struct_name}"),
                        span: struct_type.name.span,
                    })
                }
            } else {
                Err(SemanticError {
                    message: "Expected struct name".to_string(),
                    span: struct_type.name.span,
                })
            }
        }
        rue_ast::TypeNode::Tuple(tuple_type) => {
            let mut element_types = Vec::new();
            for element_type in &tuple_type.types {
                element_types.push(convert_type_node_with_scope(element_type, scope)?);
            }
            Ok(RueType::Tuple(element_types))
        }
        rue_ast::TypeNode::Array(array_type) => {
            let element_type = convert_type_node_with_scope(&array_type.element_type, scope)?;

            // Extract array size from the integer literal token
            if let rue_lexer::TokenKind::Integer(size_value) = array_type.size.kind {
                if size_value < 0 {
                    return Err(SemanticError {
                        message: format!("Array size must be non-negative, found: {size_value}"),
                        span: array_type.size.span,
                    });
                }
                let size = size_value as usize;
                Ok(RueType::Array(Box::new(element_type), size))
            } else {
                Err(SemanticError {
                    message: "Expected array size as integer literal".to_string(),
                    span: array_type.size.span,
                })
            }
        }
    }
}

// Helper function to analyze a struct definition
fn analyze_struct_definition(
    scope: &mut Scope,
    struct_def: &StructDefinitionNode,
) -> Result<(), SemanticError> {
    // Extract struct name
    let struct_name = if let rue_lexer::TokenKind::Ident(name) = &struct_def.name.kind {
        name.clone()
    } else {
        return Err(SemanticError {
            message: "Expected struct name".to_string(),
            span: struct_def.name.span,
        });
    };

    // Check if struct already exists
    if scope.structs.contains_key(&struct_name) {
        return Err(SemanticError {
            message: format!("Struct '{struct_name}' is already defined"),
            span: struct_def.name.span,
        });
    }

    // Extract field definitions
    let mut fields = Vec::new();
    for field in &struct_def.fields {
        let field_name = if let rue_lexer::TokenKind::Ident(name) = &field.name.kind {
            name.clone()
        } else {
            return Err(SemanticError {
                message: "Expected field name".to_string(),
                span: field.name.span,
            });
        };

        let field_type = convert_type_node_with_scope(&field.type_annotation.ty, scope)?;
        fields.push((field_name, field_type));
    }

    // Register the struct in the scope
    scope.register_struct(struct_name, fields);
    Ok(())
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

    // to_i32(value: i64) -> i32
    scope.functions.insert(
        "to_i32".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I64],
            return_type: RueType::I32,
        },
    );

    // to_i64(value: i32) -> i64
    scope.functions.insert(
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
    let mut global_scope = Scope::default();

    // Add built-in functions
    add_builtin_functions(&mut global_scope);

    // First pass: collect all struct definitions and function signatures
    for item in &ast.items {
        match item {
            rue_ast::CstNode::StructDefinition(struct_def) => {
                analyze_struct_definition(&mut global_scope, struct_def)?;
            }
            rue_ast::CstNode::Function(func) => {
                // Extract function signature for later use
                let func_name = if let rue_lexer::TokenKind::Ident(name) = &func.name.kind {
                    name.clone()
                } else {
                    return Err(SemanticError {
                        message: "Expected function name".to_string(),
                        span: func.name.span,
                    });
                };

                // Extract parameter types
                let mut param_types = Vec::new();
                for param in &func.param_list.params {
                    let param_type = if let Some(type_ann) = &param.type_annotation {
                        convert_type_node_with_scope(&type_ann.ty, &global_scope)?
                    } else {
                        RueType::I64 // Default to i64 if no type annotation
                    };
                    param_types.push(param_type);
                }

                // Extract return type
                let return_type = if let Some(return_type_node) = &func.return_type {
                    convert_type_node_with_scope(&return_type_node.ty, &global_scope)?
                } else {
                    RueType::Unit // Default to unit if no return type specified
                };

                let signature = FunctionSignature {
                    param_types,
                    return_type,
                };

                global_scope.functions.insert(func_name, signature);
            }
            rue_ast::CstNode::Statement(_) => {
                // Top-level statements are not supported in current language design
                return Err(SemanticError {
                    message: "Top-level statements are not supported".to_string(),
                    span: rue_lexer::Span { start: 0, end: 0 },
                });
            }
            rue_ast::CstNode::Expression(_)
            | rue_ast::CstNode::Token(_)
            | rue_ast::CstNode::Error(_) => {
                return Err(SemanticError {
                    message: "Unexpected top-level node type".to_string(),
                    span: rue_lexer::Span { start: 0, end: 0 },
                });
            }
        }
    }

    // Phase 2: Type Checking + HIR Generation - all semantic analysis happens here
    let mut type_checker = type_checker::TypeChecker::new(global_scope.clone());
    let hir = type_checker.check_program(ast)?;

    // Phase 3: HIR Validation - ensure HIR is well-formed and internally consistent
    let mut hir_validator = hir_validator::HirValidator::new();
    hir_validator.validate_program(&hir).map_err(|errors| {
        // Return the first validation error if any occur
        errors.into_iter().next().unwrap_or_else(|| SemanticError {
            message: "HIR validation failed".to_string(),
            span: rue_lexer::Span { start: 0, end: 0 },
        })
    })?;

    Ok(AnalysisResult {
        scope: global_scope,
        hir,
    })
}

/// Convert a Scope containing struct definitions to a TypeContext
///
/// This function extracts struct definitions from the semantic analysis scope
/// and creates a TypeContext that can be used by the MIR and codegen phases
/// for proper struct layout computation.
pub fn scope_to_type_context(scope: &Scope) -> TypeContext {
    let mut type_context = TypeContext::new();

    // Copy all struct definitions to the type context, preserving IDs
    for (struct_name, struct_def) in &scope.structs {
        type_context.define_struct_with_id(
            struct_def.id,
            struct_name.clone(),
            struct_def.fields.clone(),
        );
    }

    type_context
}
