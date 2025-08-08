//! Type checker with enhanced diagnostic collection
//!
//! This module extends the type checker to collect rich diagnostics during
//! semantic analysis, providing detailed error messages with suggestions.

use crate::type_checker::TypeChecker;
use crate::{Scope, SemanticError};
use rue_ast::{ExpressionNode, StatementNode};
use rue_diagnostic::{
    Diagnostic, DiagnosticBuilder, DiagnosticCollector, E2001, E2002, E3001, E3002, SourceId,
};
use rue_ir::hir::{HirExpr, HirStatement};
use rue_ir::types::RueType;
use rue_lexer::Span;
use std::collections::HashMap;

/// Type checker that collects diagnostics during analysis
pub struct DiagnosticTypeChecker {
    /// The underlying type checker (will be used when TypeChecker methods become public)
    #[allow(dead_code)]
    checker: TypeChecker,
    /// Diagnostic collector
    diagnostics: DiagnosticCollector,
    /// Source ID for diagnostics
    source_id: SourceId,
    /// Track variable definition locations for better error messages
    variable_spans: HashMap<String, Span>,
    /// Track function definition locations (will be used for function call errors)
    #[allow(dead_code)]
    function_spans: HashMap<String, Span>,
}

impl DiagnosticTypeChecker {
    /// Create a new diagnostic type checker
    pub fn new(source_id: SourceId, global_scope: Scope) -> Self {
        Self {
            checker: TypeChecker::new(global_scope),
            diagnostics: DiagnosticCollector::new(100),
            source_id,
            variable_spans: HashMap::new(),
            function_spans: HashMap::new(),
        }
    }

    /// Check an expression and collect diagnostics
    pub fn check_expression_with_diagnostics(
        &mut self,
        expr: &ExpressionNode,
    ) -> (HirExpr, RueType) {
        // Note: TypeChecker doesn't have a public method for checking expressions
        // For now, return a placeholder
        let error = SemanticError {
            message: "Expression checking not yet implemented in diagnostic mode".to_string(),
            span: rue_lexer::Span::new(0, 0),
        };
        let diagnostic = self.create_expression_diagnostic(&error, expr);
        self.diagnostics.push(diagnostic);

        // Return a literal 0 as error recovery
        (
            HirExpr::Literal {
                lit: rue_ir::hir::HirLiteral::Int32(0),
                span: rue_lexer::Span::new(0, 0),
            },
            RueType::I32,
        )
    }

    /// Check a statement and collect diagnostics
    pub fn check_statement_with_diagnostics(
        &mut self,
        stmt: &StatementNode,
    ) -> Option<HirStatement> {
        // Note: TypeChecker's check_statement is private, so we need to handle this differently
        // For now, we'll just return None and create a diagnostic
        let error = SemanticError {
            message: "Statement checking not yet implemented in diagnostic mode".to_string(),
            span: rue_lexer::Span::new(0, 0),
        };
        let diagnostic = self.create_statement_diagnostic(&error, stmt);
        self.diagnostics.push(diagnostic);
        None
    }

    /// Create a diagnostic for an expression error
    fn create_expression_diagnostic(
        &self,
        error: &SemanticError,
        expr: &ExpressionNode,
    ) -> Diagnostic {
        // Check for specific error types
        if error.message.contains("Undefined variable") {
            self.create_undefined_variable_diagnostic(error, expr)
        } else if error.message.contains("Type mismatch") {
            self.create_type_mismatch_diagnostic(error, expr)
        } else if error.message.contains("Wrong number of arguments") {
            self.create_argument_count_diagnostic(error, expr)
        } else {
            // Generic expression error
            DiagnosticBuilder::error(error.message.clone(), self.source_id.clone())
                .with_code(E3001)
                .with_primary_span(error.span)
                .build()
        }
    }

    /// Create a diagnostic for a statement error
    fn create_statement_diagnostic(
        &self,
        error: &SemanticError,
        _stmt: &StatementNode,
    ) -> Diagnostic {
        if error.message.contains("Duplicate") {
            DiagnosticBuilder::error(error.message.clone(), self.source_id.clone())
                .with_code(E2002)
                .with_primary_span(error.span)
                .with_help("Each variable must have a unique name in its scope.".to_string())
                .build()
        } else {
            DiagnosticBuilder::error(error.message.clone(), self.source_id.clone())
                .with_primary_span(error.span)
                .build()
        }
    }

    /// Create an undefined variable diagnostic with suggestions
    fn create_undefined_variable_diagnostic(
        &self,
        error: &SemanticError,
        _expr: &ExpressionNode,
    ) -> Diagnostic {
        // Extract variable name
        let var_name = self.extract_variable_name(&error.message);

        let mut builder = DiagnosticBuilder::error(error.message.clone(), self.source_id.clone())
            .with_code(E2001)
            .with_primary_span(error.span);

        if let Some(name) = var_name {
            // Find similar variable names using edit distance
            let suggestions = self.find_similar_variables(&name);

            if !suggestions.is_empty() {
                let suggestion_text = if suggestions.len() == 1 {
                    format!("Did you mean '{}'?", suggestions[0])
                } else {
                    format!(
                        "Did you mean one of: {}?",
                        suggestions
                            .iter()
                            .map(|s| format!("'{s}'"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                builder = builder.with_help(suggestion_text);
            } else {
                builder = builder.with_help(format!(
                    "Use 'let {name} = ...' to declare the variable before using it."
                ));
            }
        }

        builder.build()
    }

    /// Create a type mismatch diagnostic with detailed information
    fn create_type_mismatch_diagnostic(
        &self,
        error: &SemanticError,
        _expr: &ExpressionNode,
    ) -> Diagnostic {
        // Try to extract expected and actual types
        let (expected, actual) = self.extract_types_from_message(&error.message);

        let mut builder = DiagnosticBuilder::error(error.message.clone(), self.source_id.clone())
            .with_code(E3001)
            .with_primary_span(error.span);

        if let (Some(expected), Some(actual)) = (expected, actual) {
            builder = builder.with_help(format!(
                "Cannot convert '{actual}' to '{expected}'. Consider using an explicit cast or changing the value type."
            ));
        }

        builder.build()
    }

    /// Create an argument count diagnostic
    fn create_argument_count_diagnostic(
        &self,
        error: &SemanticError,
        _expr: &ExpressionNode,
    ) -> Diagnostic {
        DiagnosticBuilder::error(error.message.clone(), self.source_id.clone())
            .with_code(E3002)
            .with_primary_span(error.span)
            .with_help(
                "Check the function signature for the correct number and types of arguments."
                    .to_string(),
            )
            .build()
    }

    /// Extract variable name from error message
    fn extract_variable_name(&self, message: &str) -> Option<String> {
        if let Some(start) = message.find('\'')
            && let Some(end) = message[start + 1..].find('\'')
        {
            return Some(message[start + 1..start + 1 + end].to_string());
        }
        None
    }

    /// Extract expected and actual types from error message
    fn extract_types_from_message(&self, message: &str) -> (Option<String>, Option<String>) {
        if let Some(idx) = message.find("expected") {
            let rest = &message[idx..];
            if let Some(found_idx) = rest.find("found") {
                let expected = rest[8..found_idx].trim().trim_end_matches(',');
                let actual = rest[found_idx + 5..].trim().trim_end_matches('.');
                return (Some(expected.to_string()), Some(actual.to_string()));
            }
        }
        (None, None)
    }

    /// Find similar variable names using edit distance
    fn find_similar_variables(&self, name: &str) -> Vec<String> {
        use rue_diagnostic::suggestions::edit_distance;

        let mut suggestions = Vec::new();
        const MAX_DISTANCE: usize = 2;

        for var_name in self.variable_spans.keys() {
            let distance = edit_distance(name, var_name);
            if distance <= MAX_DISTANCE && distance > 0 {
                suggestions.push(var_name.clone());
            }
        }

        // Sort by edit distance
        suggestions.sort_by_key(|s| edit_distance(name, s));
        suggestions.truncate(3); // Keep top 3 suggestions

        suggestions
    }

    /// Get all collected diagnostics
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics.into_diagnostics()
    }

    /// Check if there are any errors
    pub fn has_errors(&self) -> bool {
        self.diagnostics.has_errors()
    }
}
