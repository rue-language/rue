use rue_ast::CstRoot;
use rue_diagnostic::Diagnostic;
use rue_semantic::{analyze_cst, scope_to_type_context};
use std::sync::Arc;

pub mod error;
pub mod logging;
pub mod pipeline;

// Re-export the error type for convenience
pub use error::RueError;

// Re-export the compilation functions for external use
pub use pipeline::{
    CompileError, compile_hir_via_mir_to_assembly, compile_hir_via_mir_to_executable,
};

// MIR emission functions are defined in this module

// Input structs
#[salsa::input]
pub struct SourceFile {
    #[return_ref]
    pub path: String,
    #[return_ref]
    pub text: String,
}

// Tracked functions
#[salsa::tracked]
pub fn parse_file(
    db: &dyn salsa::Database,
    file: SourceFile,
) -> Result<Arc<CstRoot>, Arc<RueError>> {
    let text = file.text(db);
    let path = file.path(db);

    // Use the new diagnostic-based parser internally
    rue_parser::parse_with_recovery(&text, &path)
        .map(Arc::new)
        .map_err(|diagnostics| {
            // Use the new unified diagnostic system
            Arc::new(RueError::from_diagnostics(diagnostics))
        })
}

/// Parse file with error recovery and collect diagnostics
#[salsa::tracked]
pub fn parse_file_with_diagnostics(
    db: &dyn salsa::Database,
    file: SourceFile,
) -> (Option<Arc<CstRoot>>, Arc<Vec<Diagnostic>>) {
    let text = file.text(db);
    let path = file.path(db);

    rue_parser::parse_with_recovery(&text, &path)
        .map(|cst| (Some(Arc::new(cst)), Arc::new(Vec::new())))
        .unwrap_or_else(|diagnostics| (None, Arc::new(diagnostics)))
}

#[salsa::tracked]
pub fn analyze_file(
    _db: &dyn salsa::Database,
    file: SourceFile,
) -> Result<Arc<rue_semantic::AnalysisResult>, Arc<RueError>> {
    // Parse and analyze with functional chaining - use CST path (AST2 pipeline)
    let cst = parse_file(_db, file)?;
    analyze_cst(&cst)
        .map(Arc::new)
        .map_err(|e| Arc::new(e.into()))
}

// Re-export Salsa's default database implementation
pub type RueDatabase = salsa::DatabaseImpl;

/// Compile source code using the AST path
pub fn compile_with_ast(source: &str) -> Result<Vec<u8>, RueError> {
    // Parse the source
    let cst = rue_parser::parse_with_recovery(source, "benchmark.rue")
        .map_err(RueError::from_diagnostics)?;

    // Analyze using CST path (AST2 pipeline)
    let analysis = analyze_cst(&cst).map_err(RueError::from)?;

    // Compile to executable using the HIR
    let type_context = rue_semantic::scope_to_type_context(&analysis.scope);
    pipeline::compile_hir_via_mir_to_executable(&analysis.hir, type_context, false)
        .map_err(RueError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use salsa::Setter;

    #[test]
    fn test_parse_file() {
        let mut db = RueDatabase::default();

        // Create a source file
        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            "fn main() -> i32 { 42 }".to_string(),
        );

        // Parse it
        let result = parse_file(&db, file);
        assert!(result.is_ok());

        // Modify the file
        file.set_text(&mut db)
            .to("fn main() -> i32 { 2 + 3 }".to_string());

        // Parse again (Salsa will recompute)
        let result = parse_file(&db, file);
        assert!(result.is_ok());
    }

    #[test]
    fn test_incremental_parsing() {
        let db = RueDatabase::default();

        // Create a source file
        let file = SourceFile::new(
            &db,
            "factorial.rue".to_string(),
            r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}"#
            .to_string(),
        );

        // Parse it
        let result = parse_file(&db, file);
        assert!(result.is_ok());

        // Parse again without changes (should be cached)
        let result2 = parse_file(&db, file);
        assert!(result.is_ok());
        assert!(Arc::ptr_eq(&result.unwrap(), &result2.unwrap())); // Same Arc = cached
    }

    #[test]
    fn test_incremental_analysis() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            r#"
fn main() -> i32 {
    42
}
"#
            .to_string(),
        );

        // Analyze it
        let result = analyze_file(&db, file);
        assert!(result.is_ok());

        // Analyze again without changes (should be cached)
        let result2 = analyze_file(&db, file);
        assert!(result2.is_ok());

        // Check that we got the same Arc back (meaning it was cached)
        assert!(
            Arc::ptr_eq(&result.unwrap(), &result2.unwrap()),
            "analyze_file should return cached result for unchanged input"
        );
    }

    #[test]
    fn test_semantic_analysis_simple() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            r#"
fn main() -> i32 {
    42
}
"#
            .to_string(),
        );

        let result = analyze_file(&db, file);
        assert!(result.is_ok());

        let analysis = result.unwrap();
        assert!(analysis.scope.functions.contains_key("main"));
        assert_eq!(analysis.scope.functions["main"].param_types.len(), 0);
        assert_eq!(
            analysis.scope.functions["main"].return_type,
            rue_ir::types::RueType::I32
        );
    }

    #[test]
    fn test_semantic_analysis_with_parameter() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}
"#
            .to_string(),
        );

        let result = analyze_file(&db, file);
        assert!(result.is_ok());

        let analysis = result.unwrap();
        assert!(analysis.scope.functions.contains_key("factorial"));
        assert_eq!(analysis.scope.functions["factorial"].param_types.len(), 1);
    }

    #[test]
    fn test_semantic_analysis_undefined_variable() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            r#"
fn main() -> i32 {
    undefined_var
}
"#
            .to_string(),
        );

        let result = analyze_file(&db, file);
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Undefined variable: undefined_var")
        );
    }

    #[test]
    fn test_semantic_analysis_undefined_function() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            r#"
fn main() -> i32 {
    undefined_func(42)
}
"#
            .to_string(),
        );

        let result = analyze_file(&db, file);
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Undefined function: undefined_func")
        );
    }

    #[test]
    fn test_implicit_return_final_expression() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            r#"
fn main() -> i32 {
    42
}
"#
            .to_string(),
        );

        let result = analyze_file(&db, file);
        assert!(
            result.is_ok(),
            "Should successfully analyze function with implicit return"
        );

        // The HIR should contain a return instruction for the final expression
        let analysis_result = result.unwrap();
        let hir = &analysis_result.hir;

        // Check that we have a main function
        assert!(hir.main_function.is_some(), "Should have a main function");

        // Get the main function
        let main_func = hir
            .get_function_by_index(hir.main_function.unwrap())
            .expect("Main function should exist");

        // Check that the main function's body block has a Return terminator
        let found_return = matches!(main_func.body.term, rue_ir::hir::Terminator::Return { .. });

        assert!(
            found_return,
            "Main function should have a return terminator for implicit return, found: {:?}",
            main_func.body.term
        );
    }

    #[test]
    fn test_semantic_analysis_wrong_argument_count() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            r#"
fn factorial(n: i32) -> i32 {
    n
}

fn main() -> i32 {
    factorial()
}
"#
            .to_string(),
        );

        let result = analyze_file(&db, file);
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expects 1 arguments, but 0 were provided")
        );
    }

    #[test]
    fn test_semantic_analysis_let_statement() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            r#"
fn main() -> i32 {
    let x: i32 = 42;
    x + 1
}
"#
            .to_string(),
        );

        let result = analyze_file(&db, file);
        assert!(result.is_ok());
    }

    #[test]
    fn test_compile_simple_program() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "test.rue".to_string(),
            r#"
fn main() -> i32 {
    42
}
"#
            .to_string(),
        );

        let result = compile_file(&db, file);
        assert!(result.is_ok());

        let executable = result.unwrap();
        // Should be a valid ELF executable
        assert_eq!(&executable[0..4], &[0x7f, 0x45, 0x4c, 0x46]);
    }

    #[test]
    fn test_compile_factorial() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "factorial.rue".to_string(),
            r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(5)
}
"#
            .to_string(),
        );

        let result = compile_file(&db, file);
        assert!(
            result.is_ok(),
            "Factorial should now compile successfully with working spill support: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_compile_assignment() {
        let db = RueDatabase::default();

        let file = SourceFile::new(
            &db,
            "assignment.rue".to_string(),
            r#"
fn main() -> i32 {
    let x: i32 = 42;
    x = 100;
    x
}
"#
            .to_string(),
        );

        let result = compile_file(&db, file);
        // Simple assignment programs should compile successfully with current register limit
        assert!(
            result.is_ok(),
            "Simple assignment should compile successfully, got error: {:?}",
            result.err()
        );
    }
}

// Compilation options
#[salsa::input]
pub struct CompileOptions {
    pub optimize: bool,
    pub use_rust_runtime: bool,
}

#[salsa::tracked]
pub fn compile_file(
    db: &dyn salsa::Database,
    file: SourceFile,
) -> Result<Arc<Vec<u8>>, Arc<RueError>> {
    // Parse, analyze, and compile with functional chaining
    let analysis = analyze_file(db, file)?;
    let type_context = scope_to_type_context(&analysis.scope);
    pipeline::compile_hir_via_mir_to_executable(&analysis.hir, type_context, false)
        .map(Arc::new)
        .map_err(|e| Arc::new(RueError::from(e)))
}

#[salsa::tracked]
pub fn compile_file_to_assembly(
    db: &dyn salsa::Database,
    file: SourceFile,
) -> Result<Arc<String>, Arc<RueError>> {
    // Parse and analyze the file first
    let analysis = match analyze_file(db, file) {
        Ok(analysis) => analysis,
        Err(error) => {
            return Err(error);
        }
    };

    // Generate assembly from HIR via MIR (without optimizations)
    let type_context = scope_to_type_context(&analysis.scope);
    match pipeline::compile_hir_via_mir_to_assembly(&analysis.hir, type_context, false) {
        Ok(assembly) => Ok(Arc::new(assembly)),
        Err(e) => Err(Arc::new(RueError::from(e))),
    }
}

#[salsa::tracked]
pub fn compile_file_with_options(
    db: &dyn salsa::Database,
    file: SourceFile,
    options: CompileOptions,
) -> Result<Arc<Vec<u8>>, Arc<RueError>> {
    // Parse and analyze the file first using AST path
    let analysis = match analyze_file(db, file) {
        Ok(analysis) => analysis,
        Err(error) => {
            return Err(error);
        }
    };

    // Generate executable from HIR via MIR with optimization settings
    let type_context = scope_to_type_context(&analysis.scope);
    match pipeline::compile_hir_via_mir_to_executable(
        &analysis.hir,
        type_context,
        options.optimize(db),
    ) {
        Ok(executable) => Ok(Arc::new(executable)),
        Err(e) => Err(Arc::new(RueError::from(e))),
    }
}

#[salsa::tracked]
pub fn compile_file_to_assembly_with_options(
    db: &dyn salsa::Database,
    file: SourceFile,
    options: CompileOptions,
) -> Result<Arc<String>, Arc<RueError>> {
    // Parse and analyze the file first using AST path
    let analysis = match analyze_file(db, file) {
        Ok(analysis) => analysis,
        Err(error) => {
            return Err(error);
        }
    };

    // Generate assembly from HIR via MIR with optimization settings
    let type_context = scope_to_type_context(&analysis.scope);
    match pipeline::compile_hir_via_mir_to_assembly(
        &analysis.hir,
        type_context,
        options.optimize(db),
    ) {
        Ok(assembly) => Ok(Arc::new(assembly)),
        Err(e) => Err(Arc::new(RueError::from(e))),
    }
}

/// Compile a file with diagnostic support
pub fn compile_file_with_diagnostics(
    db: &dyn salsa::Database,
    file: SourceFile,
    options: CompileOptions,
) -> Result<Arc<Vec<u8>>, Vec<Diagnostic>> {
    // Parse with error recovery
    let (cst, parse_diagnostics) = parse_file_with_diagnostics(db, file);

    // If we have parse errors, return them
    if !parse_diagnostics.is_empty() {
        return Err((*parse_diagnostics).clone());
    }

    // If no CST was produced (shouldn't happen if no diagnostics), create an error
    let cst = match cst {
        Some(cst) => cst,
        None => {
            let diagnostic = rue_diagnostic::DiagnosticBuilder::error(
                "Failed to parse file",
                rue_diagnostic::SourceId::new(file.path(db)),
            )
            .build();
            return Err(vec![diagnostic]);
        }
    };

    // Analyze the CST using CST path (AST2 pipeline)
    let analysis_result = analyze_cst(&cst);

    match analysis_result {
        Ok(analysis) => {
            // Generate executable from HIR via MIR
            let type_context = rue_semantic::scope_to_type_context(&analysis.scope);
            match pipeline::compile_hir_via_mir_to_executable(
                &analysis.hir,
                type_context,
                options.optimize(db),
            ) {
                Ok(executable) => Ok(Arc::new(executable)),
                Err(e) => {
                    let diagnostic = rue_diagnostic::DiagnosticBuilder::error(
                        format!("Compilation error: {e}"),
                        rue_diagnostic::SourceId::new(file.path(db)),
                    )
                    .build();
                    Err(vec![diagnostic])
                }
            }
        }
        Err(semantic_error) => {
            // Convert semantic error to diagnostic
            let diagnostic = rue_semantic::semantic_error_to_diagnostic(
                &semantic_error,
                rue_diagnostic::SourceId::new(file.path(db)),
            );
            Err(vec![diagnostic])
        }
    }
}

/// Emit MIR representation for a file
#[salsa::tracked]
pub fn emit_mir_for_file(
    db: &dyn salsa::Database,
    file: SourceFile,
    options: CompileOptions,
) -> Result<Arc<String>, Arc<RueError>> {
    // Parse and analyze the file first using AST path
    let analysis = match analyze_file(db, file) {
        Ok(analysis) => analysis,
        Err(error) => {
            return Err(error);
        }
    };

    // Generate MIR from HIR
    let type_context = scope_to_type_context(&analysis.scope);
    let mut mir = rue_lowering::lower_hir_to_mir(&analysis.hir, type_context);

    // Apply optimizations if requested
    if options.optimize(db) {
        rue_optimize::OptimizationProfileFactory::optimize_program(
            &mut mir,
            rue_optimize::OptimizationLevel::Full,
        );
    }

    // Format MIR as string
    let mir_string = format!("{}", mir);
    Ok(Arc::new(mir_string))
}

/// Emit MIR with diagnostic support
pub fn emit_mir_with_diagnostics(
    db: &dyn salsa::Database,
    file: SourceFile,
    options: CompileOptions,
) -> Result<String, Vec<Diagnostic>> {
    // Parse with error recovery
    let (cst, parse_diagnostics) = parse_file_with_diagnostics(db, file);

    // If we have parse errors, return them
    if !parse_diagnostics.is_empty() {
        return Err((*parse_diagnostics).clone());
    }

    // If no CST was produced (shouldn't happen if no diagnostics), create an error
    let cst = match cst {
        Some(cst) => cst,
        None => {
            let diagnostic = rue_diagnostic::DiagnosticBuilder::error(
                "Failed to parse file",
                rue_diagnostic::SourceId::new(file.path(db)),
            )
            .build();
            return Err(vec![diagnostic]);
        }
    };

    // Analyze the CST using CST path (AST2 pipeline)
    let analysis_result = analyze_cst(&cst);

    match analysis_result {
        Ok(analysis) => {
            // Generate MIR from HIR
            let type_context = rue_semantic::scope_to_type_context(&analysis.scope);
            let mut mir = rue_lowering::lower_hir_to_mir(&analysis.hir, type_context);

            // Apply optimizations if requested
            if options.optimize(db) {
                rue_optimize::OptimizationProfileFactory::optimize_program(
                    &mut mir,
                    rue_optimize::OptimizationLevel::Full,
                );
            }

            // Format MIR as string
            let mir_string = format!("{}", mir);
            Ok(mir_string)
        }
        Err(semantic_error) => {
            // Convert semantic error to diagnostic
            let diagnostic = rue_semantic::semantic_error_to_diagnostic(
                &semantic_error,
                rue_diagnostic::SourceId::new(file.path(db)),
            );
            Err(vec![diagnostic])
        }
    }
}
