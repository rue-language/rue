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
//! // panics. Construction validates source identities, and the phase methods
//! // return results.
//! let mut unit = CompilationUnit::new(sources, CompileOptions::default())?;
//! unit.parse()?;
//! unit.lower()?;
//! unit.analyze()?;
//! let output = unit.compile()?;
//! // Or, equivalently: let output = unit.run_all()?;
//! ```

use std::{collections::HashMap, marker::PhantomData};

use lasso::ThreadedRodeo;
use tracing::{info, info_span};

use crate::{
    Ast, AstGen, CompileErrors, CompileOptions, CompileOutput, CompileResult, CompileWarning,
    DefinitionSnapshot, FunctionWithCfg, MultiErrorResult, Rir, SourceFile, SourceMetadata,
    SourceSnapshot, SyntaxWork, TypeInternPool, build_functions_and_cfgs, build_sema_for_target,
    compile_backend,
};
use rue_span::FileId;

/// Source work performed by one compilation unit.
///
/// These values are collected by the real frontend rather than by a separate
/// benchmarking pre-pass, so observing them cannot warm a second lexer run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStats {
    /// Number of source files in the unit.
    pub files: usize,
    /// Total source bytes.
    pub bytes: usize,
    /// Total source lines, using [`str::lines`] semantics.
    pub lines: usize,
    /// Tokens produced by successful lexer invocations, matching
    /// [`SyntaxWork::tokens`] for the most recent parse attempt.
    pub tokens: usize,
}

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
    /// Immutable source contents and validated identities being compiled.
    source_snapshot: SourceSnapshot,
    /// Work metrics collected from those sources and the live parse.
    source_stats: SourceStats,
    /// Direct syntax work performed by the most recent parse attempt.
    syntax_work: SyntaxWork,
    /// Retains the lifetime-shaped API of the borrowed compatibility constructors.
    _source_lifetime: PhantomData<&'src ()>,

    // === Phase 1: Parsing ===
    /// Merged AST containing all items (populated by `parse()`).
    merged_ast: Option<Ast>,
    /// String interner shared across all files.
    interner: Option<ThreadedRodeo>,
    /// Durable module/name keys and snapshot-local definition occurrences from
    /// the latest syntactically successful parse attempt.
    definition_snapshot: Option<DefinitionSnapshot>,
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

impl CompilationUnit<'static> {
    /// Create a compilation unit from an already validated, immutable snapshot.
    ///
    /// Unlike the borrowed compatibility constructors, the returned unit does
    /// not retain a source lifetime: the snapshot owns and shares its contents.
    pub fn from_source_snapshot(source_snapshot: SourceSnapshot, options: CompileOptions) -> Self {
        Self::from_validated_snapshot(source_snapshot, options)
    }
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
    /// * `sources` - Source files to compile. The first entry is the designated
    ///   root for root-relative import fallback.
    /// * `options` - Compilation options (target, optimization, etc.)
    ///
    /// # Errors
    ///
    /// Returns an invalid-compiler-input error if there are no sources, source
    /// IDs are duplicated, or physical paths do not form unique identities.
    pub fn new(sources: Vec<SourceFile<'src>>, options: CompileOptions) -> CompileResult<Self> {
        let root_file_id = sources
            .first()
            .map(|source| source.file_id)
            .unwrap_or(FileId::DEFAULT);
        let source_metadata = SourceMetadata::from_sources(&sources, root_file_id, HashMap::new())?;
        let source_snapshot = SourceSnapshot::from_sources(&sources, source_metadata)?;
        Ok(Self::from_validated_snapshot(source_snapshot, options))
    }

    /// Create a compilation unit with validated, caller-provided source identities.
    ///
    /// Unlike [`Self::new`], this constructor lets callers designate a root that
    /// is not the first or numerically smallest file and provide relocation-stable
    /// logical paths. The metadata must describe exactly the supplied sources,
    /// including their physical paths.
    ///
    /// # Errors
    ///
    /// Returns an invalid-compiler-input error if `source_metadata` does not
    /// describe exactly the supplied source IDs and physical paths.
    pub fn with_source_metadata(
        sources: Vec<SourceFile<'src>>,
        source_metadata: SourceMetadata,
        options: CompileOptions,
    ) -> CompileResult<Self> {
        let source_snapshot = SourceSnapshot::from_sources(&sources, source_metadata)?;
        Ok(Self::from_validated_snapshot(source_snapshot, options))
    }

    fn from_validated_snapshot(source_snapshot: SourceSnapshot, options: CompileOptions) -> Self {
        let source_stats = SourceStats {
            files: source_snapshot.len(),
            bytes: source_snapshot
                .files()
                .map(|source| source.source.len())
                .sum(),
            // Count lines lazily in source_stats(); ordinary compilation does
            // not need to scan every source solely for benchmark metadata.
            lines: 0,
            tokens: 0,
        };

        Self {
            options,
            source_snapshot,
            source_stats,
            syntax_work: SyntaxWork::default(),
            _source_lifetime: PhantomData,
            merged_ast: None,
            interner: None,
            definition_snapshot: None,
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
    /// The ASTs are then merged into a single program, detecting any duplicate
    /// symbol definitions.
    ///
    /// # Errors
    ///
    /// Returns errors if:
    /// - Any file fails to lex or parse
    /// - Duplicate function, struct, or enum definitions are found
    pub fn parse(&mut self) -> MultiErrorResult<()> {
        // A failed syntax pass must not leave a snapshot that appears to
        // describe the latest attempt.
        self.definition_snapshot = None;

        // Keep symbol merging nested under the compilation unit's historical
        // `parse` timing boundary. Direct snapshot parsing owns a narrower
        // `parse` span around syntax work only.
        let _span = info_span!("parse", file_count = self.source_snapshot.len()).entered();
        let parsed = crate::syntax::run_snapshot_unspanned(&self.source_snapshot);
        self.syntax_work = parsed.work;
        self.source_stats.tokens = parsed.work.tokens;

        let parsed = parsed.result?;
        let definition_snapshot =
            DefinitionSnapshot::from_parsed_program(&parsed, &self.source_snapshot)
                .map_err(CompileErrors::from)?;
        self.definition_snapshot = Some(definition_snapshot);

        let merged = crate::merge_symbols(parsed)?;
        self.merged_ast = Some(merged.ast);
        self.interner = Some(merged.interner);
        Ok(())
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
        self.rir.as_ref().expect("analyze() called before lower()");
        let ast = self
            .merged_ast
            .as_ref()
            .expect("analyze() called before parse()");
        let interner = self.interner.as_ref().expect("interner not initialized");

        // Keep `self.rir` in positional order for the public phase accessor,
        // while sema consumes a private logical-path-ordered lowering. This
        // makes allocation and artifact layout canonical without changing the
        // caller-visible AST/RIR contract (RUE-624).
        let semantic_ast = crate::semantic_order::for_sema(ast, self.source_snapshot.metadata());
        let semantic_rir = {
            let _span = info_span!("semantic_astgen").entered();
            AstGen::new(&semantic_ast, interner).generate()
        };

        let mut sema = build_sema_for_target(
            &semantic_rir,
            interner,
            self.options.preview_features.clone(),
            self.options.target,
        );

        // Semantic analysis
        let sema_output = {
            let _span = info_span!("sema").entered();
            sema.set_root_file_id(self.source_snapshot.metadata().root_file_id());
            sema.set_file_paths(self.source_snapshot.metadata().physical_path_map().clone());
            sema.set_symbol_paths(self.source_snapshot.metadata().logical_path_map().clone());
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
        self.source_snapshot.metadata().physical_path_map()
    }

    /// Get the validated source identities and designated semantic root.
    pub fn source_metadata(&self) -> &SourceMetadata {
        self.source_snapshot.metadata()
    }

    /// Get the immutable source snapshot owned by this compilation unit.
    pub fn source_snapshot(&self) -> &SourceSnapshot {
        &self.source_snapshot
    }

    /// Get durable module/name keys and snapshot-local definition occurrences
    /// from the latest syntactically successful [`Self::parse`] attempt.
    ///
    /// This remains available when duplicate-symbol merging rejects the parsed
    /// program. It returns `None` before parsing and after a syntax error.
    pub fn definition_snapshot(&self) -> Option<&DefinitionSnapshot> {
        self.definition_snapshot.as_ref()
    }

    /// Direct syntax work performed by the most recent [`Self::parse`] attempt.
    ///
    /// The counters are stored before parse or merge errors are returned and
    /// are replaced, rather than accumulated, by a later parse attempt.
    pub fn syntax_work(&self) -> SyntaxWork {
        self.syntax_work
    }

    /// Get source metrics collected by the live frontend.
    pub fn source_stats(&self) -> SourceStats {
        SourceStats {
            lines: self
                .source_snapshot
                .files()
                .map(|source| source.source.lines().count())
                .sum(),
            ..self.source_stats
        }
    }

    /// Get counters collected during compilation without scanning source text.
    pub(crate) fn collected_source_stats(&self) -> SourceStats {
        self.source_stats
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
    use std::{fmt::Write as _, sync::Arc};

    use lasso::Key as _;

    use super::*;
    use crate::{FileId, Lexer, RirPrinter};

    fn make_sources(source: &str) -> Vec<SourceFile<'_>> {
        vec![SourceFile::new("<test>", source, FileId::new(1))]
    }

    #[test]
    fn test_compilation_unit_basic() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();

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
    fn source_snapshot_unit_owns_and_shares_source_text() {
        let file_id = FileId::new(1);
        let metadata = SourceMetadata::new(
            file_id,
            HashMap::from([(file_id, "main.rue".to_string())]),
            HashMap::from([(file_id, "main.rue".to_string())]),
        )
        .unwrap();

        let mut unit = {
            let source = Arc::new("fn main() -> i32 { 42 }".to_string());
            let snapshot =
                SourceSnapshot::new(metadata, vec![(file_id, Arc::clone(&source))]).unwrap();
            let unit = CompilationUnit::from_source_snapshot(snapshot, CompileOptions::default());
            let retained = unit
                .source_snapshot()
                .shared_source_text(file_id)
                .expect("snapshot should retain the source");

            assert!(Arc::ptr_eq(&source, &retained));
            unit
        };

        fn assert_static<T: 'static>(_: &T) {}
        assert_static(&unit);
        assert_eq!(
            unit.source_snapshot().source_text(file_id),
            Some("fn main() -> i32 { 42 }")
        );
        unit.run_frontend().unwrap();
    }

    #[test]
    fn explicit_source_metadata_preserves_a_nonminimum_root() {
        let helper_id = FileId::new(2);
        let root_id = FileId::new(40);
        let sources = vec![
            SourceFile::new("helper.rue", "fn helper() -> i32 { 1 }", helper_id),
            SourceFile::new("main.rue", "fn main() -> i32 { 42 }", root_id),
        ];
        let metadata = SourceMetadata::from_sources(
            &sources,
            root_id,
            HashMap::from([
                (helper_id, "stable/helper.rue".to_string()),
                (root_id, "stable/main.rue".to_string()),
            ]),
        )
        .unwrap();

        let mut unit =
            CompilationUnit::with_source_metadata(sources, metadata, CompileOptions::default())
                .unwrap();

        assert_eq!(unit.source_metadata().root_file_id(), root_id);
        assert_eq!(
            unit.source_metadata().logical_path(helper_id),
            Some("stable/helper.rue")
        );
        unit.run_frontend().unwrap();
    }

    #[test]
    fn test_phase_ordering() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();

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
    fn source_stats_use_the_tokens_consumed_by_the_live_parse() {
        let first = "fn main() -> i32 { helper() }\n";
        let second = "fn helper() -> i32 { 42 }\n";
        let expected_tokens = Lexer::new(first).tokenize().unwrap().0.len()
            + Lexer::new(second).tokenize().unwrap().0.len();
        let sources = vec![
            SourceFile::new("main.rue", first, FileId::new(1)),
            SourceFile::new("helper.rue", second, FileId::new(2)),
        ];
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();

        assert_eq!(
            unit.source_stats(),
            SourceStats {
                files: 2,
                bytes: first.len() + second.len(),
                lines: 2,
                tokens: 0,
            }
        );
        assert_eq!(unit.syntax_work(), SyntaxWork::default());

        unit.parse().unwrap();
        assert_eq!(unit.source_stats().tokens, expected_tokens);
        let expected_work = SyntaxWork {
            lexer_invocations: 2,
            parser_invocations: 2,
            lexed_bytes: first.len() + second.len(),
            tokens: expected_tokens,
        };
        assert_eq!(unit.syntax_work(), expected_work);

        // Re-running a phase may be unusual, but metrics must describe one
        // parse rather than accumulating observer-induced work.
        unit.parse().unwrap();
        assert_eq!(unit.source_stats().tokens, expected_tokens);
        assert_eq!(unit.syntax_work(), expected_work);
    }

    #[test]
    fn direct_parse_and_merge_matches_compilation_unit_artifacts() {
        let sources = vec![
            SourceFile::new("main.rue", "fn main() -> i32 { helper() }", FileId::new(7)),
            SourceFile::new("helper.rue", "fn helper() -> i32 { 42 }", FileId::new(3)),
        ];
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();

        let direct = crate::merge_symbols(
            crate::parse_all_files_with_source_snapshot(unit.source_snapshot()).unwrap(),
        )
        .unwrap();
        unit.parse().unwrap();

        assert_eq!(&direct.ast, unit.ast());
        let interned_symbols = |interner: &ThreadedRodeo| {
            let mut symbols = interner
                .iter()
                .map(|(key, value)| (key.into_usize(), value.to_owned()))
                .collect::<Vec<_>>();
            symbols.sort_by_key(|(key, _)| *key);
            symbols
        };
        assert_eq!(
            interned_symbols(&direct.interner),
            interned_symbols(unit.interner())
        );
    }

    #[test]
    fn duplicate_merge_errors_preserve_diagnostics_and_replace_parse_artifacts() {
        let source = "fn foo() -> i32 { 1 } fn foo() -> i32 { 2 }";
        let expected_tokens = Lexer::new(source).tokenize().unwrap().0.len();
        let sources = vec![SourceFile::new("a.rue", source, FileId::new(1))];
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();
        assert!(unit.definition_snapshot().is_none());

        let direct_errors = crate::merge_symbols(
            crate::parse_all_files_with_source_snapshot(unit.source_snapshot()).unwrap(),
        )
        .expect_err("direct symbol merging should reject the duplicate");
        let fingerprint = |errors: &crate::CompileErrors| {
            errors
                .iter()
                .map(|error| {
                    (
                        error.kind.code(),
                        error.span(),
                        error.to_string(),
                        format!("{:?}", error.diagnostic()),
                    )
                })
                .collect::<Vec<_>>()
        };
        let expected_work = SyntaxWork {
            lexer_invocations: 1,
            parser_invocations: 1,
            lexed_bytes: source.len(),
            tokens: expected_tokens,
        };
        let expected_stats = SourceStats {
            files: 1,
            bytes: source.len(),
            lines: 1,
            tokens: expected_tokens,
        };

        let first_errors = unit
            .parse()
            .expect_err("the compilation unit should reject the duplicate");
        assert_eq!(fingerprint(&direct_errors), fingerprint(&first_errors));
        assert_eq!(unit.syntax_work(), expected_work);
        assert_eq!(unit.source_stats(), expected_stats);
        let first_snapshot = unit
            .definition_snapshot()
            .expect("syntax success should retain definitions despite merge failure");
        assert_eq!(first_snapshot.definition_count(), 2);
        let definition_ids = first_snapshot
            .definitions()
            .map(|definition| definition.id())
            .collect::<Vec<_>>();
        assert_eq!(definition_ids.len(), 2);
        assert_ne!(definition_ids[0], definition_ids[1]);
        let durable_projection = first_snapshot
            .definitions()
            .map(|definition| (definition.name_key().clone(), definition.kind()))
            .collect::<Vec<_>>();
        assert_eq!(durable_projection.len(), 2);
        assert_eq!(durable_projection[0].0, durable_projection[1].0);

        let second_errors = unit
            .parse()
            .expect_err("a repeated parse should still reject the duplicate");
        assert_eq!(fingerprint(&first_errors), fingerprint(&second_errors));
        assert_eq!(unit.syntax_work(), expected_work);
        assert_eq!(unit.source_stats(), expected_stats);
        let second_snapshot = unit
            .definition_snapshot()
            .expect("repeated syntax success should replace the definition snapshot");
        assert_eq!(second_snapshot.definition_count(), 2);
        let second_projection = second_snapshot
            .definitions()
            .map(|definition| (definition.name_key().clone(), definition.kind()))
            .collect::<Vec<_>>();
        assert_eq!(second_projection, durable_projection);
    }

    #[test]
    fn test_parse_collects_cross_file_lex_and_parse_errors() {
        let lex_bad = "fn lex() -> i32 { $ }";
        let parse_bad = "fn parse( { }";
        let good = "fn good() -> i32 { 42 }";
        let expected_tokens = Lexer::new(parse_bad).tokenize().unwrap().0.len()
            + Lexer::new(good).tokenize().unwrap().0.len();
        let sources = vec![
            SourceFile::new("lex.rue", lex_bad, FileId::new(1)),
            SourceFile::new("parse.rue", parse_bad, FileId::new(2)),
            SourceFile::new("good.rue", good, FileId::new(3)),
        ];
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();

        let direct_errors = crate::parse_all_files_with_source_snapshot(unit.source_snapshot())
            .expect_err("direct parsing should report both broken files");
        let errors = unit.parse().expect_err("both broken files should report");
        assert_eq!(errors.len(), 2);
        let fingerprint = |errors: &crate::CompileErrors| {
            errors
                .iter()
                .map(|error| (error.kind.code(), error.span(), error.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(fingerprint(&direct_errors), fingerprint(&errors));
        assert!(
            unit.definition_snapshot().is_none(),
            "syntax errors must not fabricate a partial definition snapshot"
        );
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
        assert_eq!(
            unit.syntax_work(),
            SyntaxWork {
                lexer_invocations: 3,
                parser_invocations: 2,
                lexed_bytes: lex_bad.len() + parse_bad.len() + good.len(),
                tokens: expected_tokens,
            }
        );
    }

    #[test]
    fn test_parse_collects_multiple_lex_errors_in_one_file() {
        let sources = vec![SourceFile::new(
            "lex.rue",
            "fn lex() -> i32 { $ # }",
            FileId::new(1),
        )];
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();

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
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();
        unit.run_frontend().unwrap();

        assert_eq!(unit.warnings().len(), 1);
        assert!(unit.warnings()[0].to_string().contains("unused"));
    }

    #[derive(Debug)]
    struct ReproFingerprint {
        type_symbols: Vec<String>,
        function_names: Vec<String>,
        strings: Vec<String>,
        observable_rir: String,
        textual_frontend: String,
        artifact: Vec<u8>,
    }

    #[derive(Clone, Copy)]
    enum SourceRoute {
        Borrowed,
        Snapshot,
    }

    fn compile_repro_fingerprint(
        physical_root: &str,
        root_id: FileId,
        left_id: FileId,
        right_id: FileId,
        shared_id: FileId,
        reverse_siblings: bool,
        source_route: SourceRoute,
    ) -> ReproFingerprint {
        let main_path = format!("{physical_root}/main.rue");
        let left_path = format!("{physical_root}/left/types.rue");
        let right_path = format!("{physical_root}/right/types.rue");
        let shared_path = format!("{physical_root}/shared.rue");
        let mut sources = vec![
            SourceFile::new(
                &main_path,
                r#"fn identity(comptime T: type, value: T) -> T { value }
                    fn main() -> i32 {
                        let left = @import("left/types.rue");
                        let right = @import("right/types.rue");
                        left.entry(identity(i32, 10)) + right.entry(identity(i32, 20))
                    }"#,
                root_id,
            ),
            SourceFile::new(
                &left_path,
                r#"const shared = @import("shared.rue");
                    pub struct Payload {
                        value: i32,
                        text: String,
                        fn score(borrow self) -> i32 { self.value + 1 }
                    }
                    drop fn Payload(self) { @dbg(self.value); }
                    pub enum Choice { Empty, Text(String), Many([String; 2]) }
                    pub fn entry(value: i32) -> i32 {
                        let payload = Payload { value: value, text: "left-value" };
                        shared.bump(payload.score())
                    }"#,
                left_id,
            ),
            SourceFile::new(
                &right_path,
                r#"const shared = @import("shared.rue");
                    pub struct Payload {
                        value: i32,
                        extra: i32,
                        text: String,
                        fn score(borrow self) -> i32 { self.value + self.extra }
                    }
                    drop fn Payload(self) { @dbg(self.extra); }
                    pub enum Choice { Empty, Text(String), Many([String; 3]) }
                    pub fn entry(value: i32) -> i32 {
                        let payload = Payload { value: value, extra: 2, text: "right-value" };
                        shared.bump(payload.score())
                    }"#,
                right_id,
            ),
            SourceFile::new(
                &shared_path,
                "pub fn bump(value: i32) -> i32 { value + 5 }",
                shared_id,
            ),
        ];
        if reverse_siblings {
            sources.swap(1, 2);
        }

        let source_metadata = SourceMetadata::from_sources(
            &sources,
            root_id,
            HashMap::from([
                (root_id, "main.rue".to_string()),
                (left_id, "left/types.rue".to_string()),
                (right_id, "right/types.rue".to_string()),
                (shared_id, "shared.rue".to_string()),
            ]),
        )
        .unwrap();
        let mut unit = match source_route {
            SourceRoute::Borrowed => CompilationUnit::with_source_metadata(
                sources,
                source_metadata,
                CompileOptions::default(),
            )
            .unwrap(),
            SourceRoute::Snapshot => {
                let snapshot = SourceSnapshot::from_sources(&sources, source_metadata).unwrap();
                CompilationUnit::from_source_snapshot(snapshot, CompileOptions::default())
            }
        };
        unit.run_frontend().expect("frontend should compile");

        let pool = unit.type_pool();
        let mut type_symbols = Vec::new();
        type_symbols.extend(
            pool.all_struct_ids()
                .into_iter()
                .map(|id| format!("struct:{}", pool.struct_symbol_name(id))),
        );
        type_symbols.extend(
            pool.all_enum_ids()
                .into_iter()
                .map(|id| format!("enum:{}", pool.enum_symbol_name(id))),
        );

        let function_names = unit
            .functions()
            .iter()
            .map(|function| function.analyzed.name.clone())
            .collect();
        let observable_rir = RirPrinter::new(unit.rir(), unit.interner()).to_string();
        let mut textual_frontend = String::new();
        for function in unit.functions() {
            writeln!(
                &mut textual_frontend,
                "function {}:",
                function.analyzed.name
            )
            .unwrap();
            writeln!(
                &mut textual_frontend,
                "{}",
                function.analyzed.air.display_with_interner(unit.interner())
            )
            .unwrap();
            writeln!(
                &mut textual_frontend,
                "{}",
                function.cfg.display_with_interner(unit.interner())
            )
            .unwrap();
        }
        let strings = unit.strings().to_vec();
        let artifact = unit.compile().expect("backend should compile").elf;

        ReproFingerprint {
            type_symbols,
            function_names,
            strings,
            observable_rir,
            textual_frontend,
            artifact,
        }
    }

    #[test]
    fn test_semantic_and_native_output_ignore_sibling_order_and_file_ids() {
        let forward = compile_repro_fingerprint(
            "/tmp/rue-short-root",
            FileId::new(1),
            FileId::new(2),
            FileId::new(3),
            FileId::new(4),
            false,
            SourceRoute::Borrowed,
        );
        let reversed = compile_repro_fingerprint(
            "/tmp/a-deliberately-much-longer-relocated-rue-root",
            FileId::new(100),
            FileId::new(42),
            FileId::new(7),
            FileId::new(55),
            true,
            SourceRoute::Borrowed,
        );

        assert_eq!(forward.type_symbols, reversed.type_symbols);
        assert_eq!(forward.function_names, reversed.function_names);
        assert_eq!(forward.strings, reversed.strings);
        assert_eq!(forward.textual_frontend, reversed.textual_frontend);
        assert_ne!(
            forward.observable_rir, reversed.observable_rir,
            "caller-facing RIR should retain positional source order"
        );
        let forward_left = forward
            .observable_rir
            .find("left-value")
            .expect("forward RIR should contain the left literal");
        let forward_right = forward
            .observable_rir
            .find("right-value")
            .expect("forward RIR should contain the right literal");
        let reversed_left = reversed
            .observable_rir
            .find("left-value")
            .expect("reversed RIR should contain the left literal");
        let reversed_right = reversed
            .observable_rir
            .find("right-value")
            .expect("reversed RIR should contain the right literal");
        assert!(forward_left < forward_right);
        assert!(reversed_right < reversed_left);
        assert!(forward.function_names.is_sorted());
        assert!(forward.strings.is_sorted());
        for expected_name in [
            "Payload$left_2ftypes_2erue.score",
            "Payload$right_2ftypes_2erue.score",
            "Payload$left_2ftypes_2erue.__drop",
            "Payload$right_2ftypes_2erue.__drop",
            "__rue_drop_Choice$left_2ftypes_2erue",
            "__rue_drop_Choice$right_2ftypes_2erue",
        ] {
            assert!(
                forward
                    .function_names
                    .iter()
                    .any(|name| name == expected_name),
                "missing non-vacuity symbol {expected_name}"
            );
        }
        assert_eq!(forward.strings, ["left-value", "right-value"]);
        assert!(
            forward.artifact == reversed.artifact,
            "complete native artifacts differ ({} versus {} bytes)",
            forward.artifact.len(),
            reversed.artifact.len()
        );
    }

    #[test]
    fn borrowed_and_snapshot_units_match_adversarial_frontend_artifacts() {
        let borrowed = compile_repro_fingerprint(
            "/tmp/rue-snapshot-parity",
            FileId::new(40),
            FileId::new(9),
            FileId::new(2),
            FileId::new(77),
            true,
            SourceRoute::Borrowed,
        );
        let snapshot = compile_repro_fingerprint(
            "/tmp/rue-snapshot-parity",
            FileId::new(40),
            FileId::new(9),
            FileId::new(2),
            FileId::new(77),
            true,
            SourceRoute::Snapshot,
        );

        assert_eq!(borrowed.type_symbols, snapshot.type_symbols);
        assert_eq!(borrowed.function_names, snapshot.function_names);
        assert_eq!(borrowed.strings, snapshot.strings);
        assert_eq!(borrowed.observable_rir, snapshot.observable_rir);
        assert_eq!(borrowed.textual_frontend, snapshot.textual_frontend);
        assert_eq!(borrowed.artifact, snapshot.artifact);
    }

    #[test]
    #[should_panic(expected = "lower() called before parse()")]
    fn test_lower_before_parse_panics() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();
        unit.lower().unwrap();
    }

    #[test]
    #[should_panic(expected = "analyze() called before lower()")]
    fn test_analyze_before_lower_panics() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default()).unwrap();
        unit.parse().unwrap();
        unit.analyze().unwrap();
    }
}
