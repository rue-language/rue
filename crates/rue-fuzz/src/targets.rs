//! Fuzz targets for the Rue compiler.
//!
//! Each target exercises a different phase of the compiler:
//! - Lexer: tokenization
//! - Parser: AST construction
//! - Compiler: full compilation pipeline (frontend only, no codegen)

use super::FuzzTarget;

/// Fuzz target for the lexer.
///
/// Goal: The lexer should never panic, always produce tokens or an error.
pub struct LexerTarget;

impl FuzzTarget for LexerTarget {
    fn name(&self) -> &'static str {
        "lexer"
    }

    fn fuzz(&self, input: &[u8]) {
        // Only test valid UTF-8, since the lexer expects valid source text
        if let Ok(source) = std::str::from_utf8(input) {
            let lexer = rue_lexer::Lexer::new(source);
            // The lexer should handle all input without panicking
            let _ = lexer.tokenize();
        }
    }
}

/// Fuzz target for the parser.
///
/// Goal: The parser should never panic, always produce an AST or an error.
pub struct ParserTarget;

impl FuzzTarget for ParserTarget {
    fn name(&self) -> &'static str {
        "parser"
    }

    fn fuzz(&self, input: &[u8]) {
        // Only test valid UTF-8
        if let Ok(source) = std::str::from_utf8(input) {
            let lexer = rue_lexer::Lexer::new(source);
            if let Ok((tokens, interner)) = lexer.tokenize() {
                let parser = rue_parser::Parser::new(tokens, interner);
                // The parser should handle all tokenized input without panicking
                let _ = parser.parse();
            }
        }
    }
}

/// Fuzz target for the full frontend compilation pipeline.
///
/// Goal: Frontend compilation should never panic, always succeed or return errors.
pub struct CompilerTarget;

impl FuzzTarget for CompilerTarget {
    fn name(&self) -> &'static str {
        "compiler"
    }

    fn fuzz(&self, input: &[u8]) {
        // Only test valid UTF-8
        if let Ok(source) = std::str::from_utf8(input) {
            // Use compile_frontend to avoid code generation (which is slower)
            // and focus on the analysis phases where bugs are more likely
            let _ = rue_compiler::compile_frontend(source);
        }
    }
}

/// Get all available fuzz targets.
pub fn all_targets() -> Vec<Box<dyn FuzzTarget>> {
    vec![
        Box::new(LexerTarget),
        Box::new(ParserTarget),
        Box::new(CompilerTarget),
    ]
}

/// Get a fuzz target by name.
pub fn get_target(name: &str) -> Option<Box<dyn FuzzTarget>> {
    match name {
        "lexer" => Some(Box::new(LexerTarget)),
        "parser" => Some(Box::new(ParserTarget)),
        "compiler" => Some(Box::new(CompilerTarget)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexer_target_valid() {
        let target = LexerTarget;
        target.fuzz(b"fn main() -> i32 { 42 }");
    }

    #[test]
    fn test_lexer_target_invalid_utf8() {
        let target = LexerTarget;
        // Invalid UTF-8 should be silently ignored
        target.fuzz(&[0xff, 0xfe, 0x00, 0x01]);
    }

    #[test]
    fn test_lexer_target_garbage() {
        let target = LexerTarget;
        target.fuzz(b"@#$%^&*()!~`");
    }

    #[test]
    fn test_parser_target_valid() {
        let target = ParserTarget;
        target.fuzz(b"fn main() -> i32 { 42 }");
    }

    #[test]
    fn test_parser_target_invalid_syntax() {
        let target = ParserTarget;
        target.fuzz(b"fn fn fn { { { } } }");
    }

    #[test]
    fn test_compiler_target_valid() {
        let target = CompilerTarget;
        target.fuzz(b"fn main() -> i32 { 42 }");
    }

    #[test]
    fn test_compiler_target_type_error() {
        let target = CompilerTarget;
        target.fuzz(b"fn main() -> i32 { true }");
    }

    #[test]
    fn test_all_targets() {
        let targets = all_targets();
        assert_eq!(targets.len(), 3);
    }

    #[test]
    fn test_get_target() {
        assert!(get_target("lexer").is_some());
        assert!(get_target("parser").is_some());
        assert!(get_target("compiler").is_some());
        assert!(get_target("invalid").is_none());
    }
}

/// Proptest-based fuzz tests using structured input generation.
///
/// These tests generate syntactically valid Rue programs and verify
/// that the compiler never panics.
#[cfg(test)]
mod proptest_tests {
    use super::*;
    use crate::generators;
    use proptest::prelude::*;

    proptest! {
        /// The lexer should never panic on any valid expression.
        #[test]
        fn lexer_never_panics_on_expr(expr in generators::arb_expr(3)) {
            let target = LexerTarget;
            target.fuzz(expr.as_bytes());
        }

        /// The lexer should never panic on any generated program.
        #[test]
        fn lexer_never_panics_on_program(program in generators::arb_program(2)) {
            let target = LexerTarget;
            target.fuzz(program.as_bytes());
        }

        /// The parser should never panic on any generated program.
        #[test]
        fn parser_never_panics_on_program(program in generators::arb_program(2)) {
            let target = ParserTarget;
            target.fuzz(program.as_bytes());
        }

        /// The parser should never panic on any generated expression.
        #[test]
        fn parser_never_panics_on_expr(expr in generators::arb_expr(3)) {
            let target = ParserTarget;
            // Wrap expression in a valid function to make it parseable
            let program = format!("fn main() -> i32 {{ {} }}", expr);
            target.fuzz(program.as_bytes());
        }

        /// The full compiler frontend should never panic on valid programs.
        #[test]
        fn compiler_never_panics_on_program(program in generators::arb_program(2)) {
            let target = CompilerTarget;
            target.fuzz(program.as_bytes());
        }

        /// The full compiler frontend should never panic on possibly invalid programs.
        /// This tests error handling in semantic analysis.
        #[test]
        fn compiler_never_panics_on_maybe_invalid(
            program in generators::arb_maybe_invalid_program(2)
        ) {
            let target = CompilerTarget;
            target.fuzz(program.as_bytes());
        }

        /// The lexer should handle arbitrary strings without panicking.
        #[test]
        fn lexer_handles_arbitrary_strings(s in ".*") {
            let target = LexerTarget;
            target.fuzz(s.as_bytes());
        }

        /// The parser should handle arbitrary strings without panicking.
        #[test]
        fn parser_handles_arbitrary_strings(s in ".*") {
            let target = ParserTarget;
            target.fuzz(s.as_bytes());
        }

        /// The compiler should handle arbitrary strings without panicking.
        #[test]
        fn compiler_handles_arbitrary_strings(s in ".*") {
            let target = CompilerTarget;
            target.fuzz(s.as_bytes());
        }
    }
}
