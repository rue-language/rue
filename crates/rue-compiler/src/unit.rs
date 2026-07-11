//! Unified compilation unit that owns all compilation artifacts.
//!
//! The [`CompilationUnit`] provides a single source of truth for all compilation
//! state, from source files through to machine code. Phase artifacts are held in
//! `Option` fields and phase ordering is enforced *at runtime*: each phase method
//! panics if a prerequisite phase has not run yet. The order is
//! [`parse()`](CompilationUnit::parse) → [`lower()`](CompilationUnit::lower) →
//! [`analyze()`](CompilationUnit::analyze) →
//! [`compile()`](CompilationUnit::compile);
//! [`run_all()`](CompilationUnit::run_all) runs them in sequence for you.
//!
//! # Example
//!
//! ```ignore
//! use rue_compiler::{CompilationUnit, SourceFile, CompileOptions};
//! use rue_span::FileId;
//!
//! // Create source files
//! let sources = vec![
//!     SourceFile::new("main.rue", "fn main() -> i32 { 42 }", FileId::new(1)),
//! ];
//!
//! // Run the phases IN ORDER — skipping one (e.g. analyze() before lower())
//! // panics. `new` is infallible; the phase methods return results.
//! let mut unit = CompilationUnit::new(sources, CompileOptions::default());
//! unit.parse()?;
//! unit.lower()?;
//! unit.analyze()?;
//! let output = unit.compile()?;
//! // Or, equivalently: let output = unit.run_all()?;
//! ```

use std::collections::HashMap;

use lasso::ThreadedRodeo;
use tracing::{info, info_span};

use crate::{
    Ast, AstGen, CompileErrors, CompileOptions, CompileOutput, CompileWarning, FunctionWithCfg,
    Lexer, MultiErrorResult, Parser, Rir, Sema, SourceFile, TypeInternPool,
    build_functions_and_cfgs, compile_backend,
};
use rue_span::FileId;

/// A unified compilation unit that owns all artifacts from source to machine code.
///
/// The compilation unit progresses through phases:
/// 1. **New**: Just source files
/// 2. **Parsed**: merged AST and interner from parsing
/// 3. **Lowered**: RIR (untyped intermediate representation)
/// 4. **Analyzed**: AIR (typed IR) and CFGs for all functions
///
/// Each phase builds on the previous one. The unit validates that phases
/// are run in order - you can't analyze before parsing.
///
/// # Thread Safety
///
/// The compilation unit uses [`ThreadedRodeo`] for string interning, which is
/// thread-safe. Parallel operations (like per-function CFG construction) can
/// safely share the interner.
#[derive(Debug)]
pub struct CompilationUnit<'src> {
    // === Configuration ===
    /// Compilation options (target, optimization level, etc.)
    options: CompileOptions,

    // === Source ===
    /// Source files being compiled.
    sources: Vec<SourceFile<'src>>,

    // === Phase 1: Parsing ===
    /// Merged AST containing all items (populated by `parse()`).
    merged_ast: Option<Ast>,
    /// String interner shared across all files.
    interner: Option<ThreadedRodeo>,
    /// Maps FileId to source file path (for error messages).
    file_paths: HashMap<FileId, String>,
    /// Maps FileId to a relocation-stable path for generated symbols.
    symbol_paths: HashMap<FileId, String>,

    // === Phase 2: RIR Generation ===
    /// Untyped intermediate representation (populated by `lower()`).
    rir: Option<Rir>,

    // === Phase 3: Semantic Analysis + CFG ===
    /// Analyzed functions with typed IR and control flow graphs.
    functions: Option<Vec<FunctionWithCfg>>,
    /// Type intern pool containing all struct and enum definitions.
    type_pool: Option<TypeInternPool>,
    /// String literals indexed by their string_const index.
    strings: Option<Vec<String>>,
    /// Warnings collected during compilation.
    warnings: Vec<CompileWarning>,
}

impl<'src> CompilationUnit<'src> {
    /// Create a new compilation unit from source files.
    ///
    /// This initializes the unit with source files but does not run any
    /// compilation phases. Call [`parse()`](Self::parse), [`lower()`](Self::lower),
    /// and [`analyze()`](Self::analyze) to progress through compilation.
    ///
    /// # Arguments
    ///
    /// * `sources` - Source files to compile
    /// * `options` - Compilation options (target, optimization, etc.)
    pub fn new(sources: Vec<SourceFile<'src>>, options: CompileOptions) -> Self {
        let file_paths: HashMap<FileId, String> = sources
            .iter()
            .map(|s| (s.file_id, s.path.to_string()))
            .collect();
        let symbol_paths = file_paths.clone();

        Self {
            options,
            sources,
            merged_ast: None,
            interner: None,
            file_paths,
            symbol_paths,
            rir: None,
            functions: None,
            type_pool: None,
            strings: None,
            warnings: Vec::new(),
        }
    }

    // =========================================================================
    // Phase 1: Parsing
    // =========================================================================

    /// Parse all source files.
    ///
    /// This runs lexing and parsing on each source file, producing ASTs.
    /// The ASTs are then merged into a single program, detecting any
    /// duplicate symbol definitions.
    ///
    /// # Errors
    ///
    /// Returns errors if:
    /// - Any file fails to lex or parse
    /// - Duplicate function, struct, or enum definitions are found
    pub fn parse(&mut self) -> MultiErrorResult<()> {
        let _span = info_span!("parse", file_count = self.sources.len()).entered();

        // Parse all files with a shared interner
        let mut parsed_files = Vec::with_capacity(self.sources.len());
        let mut interner = ThreadedRodeo::new();
        let mut errors = CompileErrors::new();

        for source in &self.sources {
            let _file_span = info_span!("parse_file", path = %source.path).entered();

            // Create lexer with shared interner and file ID
            let lexer = Lexer::with_interner_and_file_id(source.source, interner, source.file_id);

            // Tokenize
            let tokens = match lexer.tokenize_preserving_interner() {
                Ok((tokens, returned_interner)) => {
                    interner = returned_interner;
                    tokens
                }
                Err((lex_errors, returned_interner)) => {
                    interner = returned_interner;
                    errors.extend(lex_errors);
                    continue;
                }
            };

            info!(token_count = tokens.len(), "lexing complete");

            // Parse
            let parser = Parser::new(tokens, interner);
            let ast = match parser.parse_preserving_interner() {
                Ok((ast, returned_interner)) => {
                    interner = returned_interner;
                    ast
                }
                Err((parse_errors, returned_interner)) => {
                    interner = returned_interner;
                    errors.extend(parse_errors);
                    continue;
                }
            };

            info!(item_count = ast.items.len(), "parsing complete");

            parsed_files.push((source.path.to_string(), ast));
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        // Merge symbols and check for duplicates
        let merged_ast = self.merge_symbols(&parsed_files, &interner)?;

        self.merged_ast = Some(merged_ast);
        self.interner = Some(interner);

        Ok(())
    }

    /// Merge symbols from all parsed files, checking for duplicates.
    fn merge_symbols(
        &self,
        files: &[(String, Ast)],
        interner: &ThreadedRodeo,
    ) -> MultiErrorResult<Ast> {
        let _span = info_span!("merge_symbols", file_count = files.len()).entered();

        let check = crate::detect_duplicate_symbols(
            files.iter().map(|(path, ast)| (path.as_str(), ast)),
            interner,
        );

        if !check.errors.is_empty() {
            return Err(CompileErrors::from(check.errors));
        }

        let all_items: Vec<_> = files
            .iter()
            .flat_map(|(_, ast)| ast.items.iter().cloned())
            .collect();

        info!(
            function_count = check.function_count,
            struct_count = check.struct_count,
            enum_count = check.enum_count,
            "symbol merging complete"
        );

        Ok(Ast { items: all_items })
    }

    // =========================================================================
    // Phase 2: RIR Generation
    // =========================================================================

    /// Generate untyped intermediate representation (RIR).
    ///
    /// This transforms the merged AST into RIR, which is a more uniform
    /// representation suitable for semantic analysis.
    ///
    /// # Panics
    ///
    /// Panics if called before [`parse()`](Self::parse).
    pub fn lower(&mut self) -> MultiErrorResult<()> {
        let ast = self
            .merged_ast
            .as_ref()
            .expect("lower() called before parse()");
        let interner = self.interner.as_ref().expect("interner not initialized");

        let _span = info_span!("astgen").entered();

        let astgen = AstGen::new(ast, interner);
        let rir = astgen.generate();

        info!(instruction_count = rir.len(), "RIR generation complete");

        self.rir = Some(rir);
        Ok(())
    }

    // =========================================================================
    // Phase 3: Semantic Analysis + CFG Construction
    // =========================================================================

    /// Perform semantic analysis and build control flow graphs.
    ///
    /// This runs type checking, symbol resolution, and other semantic checks,
    /// then builds CFGs for each function. Optimizations are applied based
    /// on the configured optimization level.
    ///
    /// # Panics
    ///
    /// Panics if called before [`lower()`](Self::lower).
    pub fn analyze(&mut self) -> MultiErrorResult<()> {
        let rir = self.rir.as_ref().expect("analyze() called before lower()");
        let interner = self.interner.as_ref().expect("interner not initialized");

        // Semantic analysis
        let sema_output = {
            let _span = info_span!("sema").entered();
            let mut sema = Sema::new_for_target(
                rir,
                interner,
                self.options.preview_features.clone(),
                self.options.target,
            );
            sema.set_file_paths(self.file_paths.clone());
            sema.set_symbol_paths(self.symbol_paths.clone());
            let output = sema.analyze_all()?;
            info!(
                function_count = output.functions.len(),
                struct_count = output.type_pool.stats().struct_count,
                "semantic analysis complete"
            );
            output
        };

        let cfg_output = build_functions_and_cfgs(sema_output, self.options.opt_level, interner)?;

        self.functions = Some(cfg_output.functions);
        self.type_pool = Some(cfg_output.type_pool);
        self.strings = Some(cfg_output.strings);
        self.warnings.extend(cfg_output.warnings);

        Ok(())
    }

    // =========================================================================
    // Phase 4: Code Generation + Linking
    // =========================================================================

    /// Generate machine code and link into an executable.
    ///
    /// This is the final compilation phase. It generates machine code for
    /// all functions and links them into an executable binary.
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze).
    pub fn compile(&self) -> MultiErrorResult<CompileOutput> {
        let functions = self
            .functions
            .as_ref()
            .expect("compile() called before analyze()");
        let type_pool = self.type_pool.as_ref().expect("type_pool not available");
        let strings = self.strings.as_ref().expect("strings not available");
        let interner = self.interner.as_ref().expect("interner not available");

        compile_backend(
            functions,
            type_pool,
            strings,
            interner,
            &self.options,
            &self.warnings,
        )
    }

    // =========================================================================
    // Convenience Methods
    // =========================================================================

    /// Run all frontend phases (parse, lower, analyze).
    ///
    /// This is a convenience method that runs the complete frontend pipeline.
    /// Equivalent to calling `parse()`, `lower()`, and `analyze()` in sequence.
    pub fn run_frontend(&mut self) -> MultiErrorResult<()> {
        self.parse()?;
        self.lower()?;
        self.analyze()?;
        Ok(())
    }

    /// Run all phases and produce a compiled binary.
    ///
    /// This is a convenience method that runs the complete compilation pipeline.
    /// Equivalent to calling `run_frontend()` followed by `compile()`.
    pub fn run_all(&mut self) -> MultiErrorResult<CompileOutput> {
        self.run_frontend()?;
        self.compile()
    }

    /// Check if parsing has been completed.
    pub fn is_parsed(&self) -> bool {
        self.merged_ast.is_some()
    }

    /// Check if RIR generation has been completed.
    pub fn is_lowered(&self) -> bool {
        self.rir.is_some()
    }

    /// Check if semantic analysis has been completed.
    pub fn is_analyzed(&self) -> bool {
        self.functions.is_some()
    }

    // =========================================================================
    // Accessors
    // =========================================================================

    /// Get the compilation options.
    pub fn options(&self) -> &CompileOptions {
        &self.options
    }

    /// Get the merged AST (after parsing).
    ///
    /// # Panics
    ///
    /// Panics if called before [`parse()`](Self::parse).
    pub fn ast(&self) -> &Ast {
        self.merged_ast
            .as_ref()
            .expect("ast() called before parse()")
    }

    /// Get the string interner.
    ///
    /// # Panics
    ///
    /// Panics if called before [`parse()`](Self::parse).
    pub fn interner(&self) -> &ThreadedRodeo {
        self.interner
            .as_ref()
            .expect("interner() called before parse()")
    }

    /// Get the RIR (after lowering).
    ///
    /// # Panics
    ///
    /// Panics if called before [`lower()`](Self::lower).
    pub fn rir(&self) -> &Rir {
        self.rir.as_ref().expect("rir() called before lower()")
    }

    /// Get the analyzed functions with CFGs (after analysis).
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze).
    pub fn functions(&self) -> &[FunctionWithCfg] {
        self.functions
            .as_ref()
            .expect("functions() called before analyze()")
    }

    /// Get the type pool (after analysis).
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze).
    pub fn type_pool(&self) -> &TypeInternPool {
        self.type_pool
            .as_ref()
            .expect("type_pool() called before analyze()")
    }

    /// Get string literals (after analysis).
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze).
    pub fn strings(&self) -> &[String] {
        self.strings
            .as_ref()
            .expect("strings() called before analyze()")
    }

    /// Get all warnings collected during compilation.
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }

    /// Get the file paths map.
    pub fn file_paths(&self) -> &HashMap<FileId, String> {
        &self.file_paths
    }

    /// Replace the paths used to qualify otherwise-colliding generated names.
    ///
    /// These identities do not affect diagnostics or module resolution. Any
    /// missing entry falls back to the corresponding physical file path.
    pub fn set_symbol_paths(&mut self, symbol_paths: HashMap<FileId, String>) {
        self.symbol_paths = symbol_paths;
    }

    /// Take the interner out of the compilation unit.
    ///
    /// This is useful when you need ownership of the interner (e.g., for
    /// code generation).
    ///
    /// # Panics
    ///
    /// Panics if called before [`parse()`](Self::parse) or if the interner
    /// has already been taken.
    pub fn take_interner(&mut self) -> ThreadedRodeo {
        self.interner
            .take()
            .expect("interner not available (not parsed or already taken)")
    }

    /// Take the functions out of the compilation unit.
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze) or if the
    /// functions have already been taken.
    pub fn take_functions(&mut self) -> Vec<FunctionWithCfg> {
        self.functions
            .take()
            .expect("functions not available (not analyzed or already taken)")
    }

    /// Take the type pool out of the compilation unit.
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze) or if the
    /// type pool has already been taken.
    pub fn take_type_pool(&mut self) -> TypeInternPool {
        self.type_pool
            .take()
            .expect("type_pool not available (not analyzed or already taken)")
    }

    /// Take the strings out of the compilation unit.
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze) or if the
    /// strings have already been taken.
    pub fn take_strings(&mut self) -> Vec<String> {
        self.strings
            .take()
            .expect("strings not available (not analyzed or already taken)")
    }

    /// Take the warnings out of the compilation unit.
    pub fn take_warnings(&mut self) -> Vec<CompileWarning> {
        std::mem::take(&mut self.warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileId;

    fn make_sources(source: &str) -> Vec<SourceFile<'_>> {
        vec![SourceFile::new("<test>", source, FileId::new(1))]
    }

    #[test]
    fn test_compilation_unit_basic() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());

        assert!(!unit.is_parsed());
        assert!(!unit.is_lowered());
        assert!(!unit.is_analyzed());

        unit.run_frontend().unwrap();

        assert!(unit.is_parsed());
        assert!(unit.is_lowered());
        assert!(unit.is_analyzed());
        assert_eq!(unit.functions().len(), 1);
    }

    #[test]
    fn test_phase_ordering() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());

        // Parse first
        unit.parse().unwrap();
        assert!(unit.is_parsed());
        assert!(!unit.is_lowered());

        // Then lower
        unit.lower().unwrap();
        assert!(unit.is_lowered());
        assert!(!unit.is_analyzed());

        // Then analyze
        unit.analyze().unwrap();
        assert!(unit.is_analyzed());
    }

    #[test]
    fn test_duplicate_function_error() {
        let sources = vec![SourceFile::new(
            "a.rue",
            "fn foo() -> i32 { 1 } fn foo() -> i32 { 2 }",
            FileId::new(1),
        )];
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());

        let result = unit.parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("function"));
    }

    #[test]
    fn test_parse_collects_cross_file_lex_and_parse_errors() {
        let sources = vec![
            SourceFile::new("lex.rue", "fn lex() -> i32 { $ }", FileId::new(1)),
            SourceFile::new("parse.rue", "fn parse( { }", FileId::new(2)),
            SourceFile::new("good.rue", "fn good() -> i32 { 42 }", FileId::new(3)),
        ];
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());

        let errors = unit.parse().expect_err("both broken files should report");
        assert_eq!(errors.len(), 2);
        let file_ids: Vec<_> = errors
            .iter()
            .map(|error| {
                error
                    .span()
                    .expect("parse errors should be spanned")
                    .file_id
            })
            .collect();
        assert_eq!(file_ids, vec![FileId::new(1), FileId::new(2)]);
    }

    #[test]
    fn test_parse_collects_multiple_lex_errors_in_one_file() {
        let sources = vec![SourceFile::new(
            "lex.rue",
            "fn lex() -> i32 { $ # }",
            FileId::new(1),
        )];
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());

        let errors = unit
            .parse()
            .expect_err("both lexer errors in one file should report");
        assert_eq!(errors.len(), 2);
        let kinds: Vec<_> = errors.iter().map(|error| &error.kind).collect();
        assert!(matches!(
            kinds[0],
            crate::ErrorKind::UnexpectedCharacter('$')
        ));
        assert!(matches!(
            kinds[1],
            crate::ErrorKind::UnexpectedCharacter('#')
        ));
    }

    #[test]
    fn test_warnings_collected() {
        let sources = make_sources("fn main() -> i32 { let x = 42; 0 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());
        unit.run_frontend().unwrap();

        assert_eq!(unit.warnings().len(), 1);
        assert!(unit.warnings()[0].to_string().contains("unused"));
    }

    #[test]
    #[should_panic(expected = "lower() called before parse()")]
    fn test_lower_before_parse_panics() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());
        unit.lower().unwrap();
    }

    #[test]
    #[should_panic(expected = "analyze() called before lower()")]
    fn test_analyze_before_lower_panics() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());
        unit.parse().unwrap();
        unit.analyze().unwrap();
    }
}
