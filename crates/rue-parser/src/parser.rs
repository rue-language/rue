//! Parser state and the single recursive-descent grammar entry point.
//!
//! Cohesive grammar domains live in the sibling modules under `parser/`.

use crate::ast::*;
use crate::parser_policy::{condition, diagnostics, nesting, recovery};
use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileErrors, ErrorKind, MultiErrorResult};
use rue_lexer::{Token, TokenKind};
use rue_span::{FileId, Span};
use tracing::{info, info_span};

type PResult<T> = Result<T, ()>;

/// Rue's token-cursor recursive-descent parser.
pub struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    interner: ThreadedRodeo,
    syms: PrimitiveTypeSpurs,
    file_id: FileId,
    errors: diagnostics::ParserDiagnostics,
    interner_error: Option<lasso::LassoErrorKind>,
    /// Count of value-position anonymous `struct {..}` / `enum {..}` literals
    /// parsed so far. Declaration parsers compare it across their body parse to
    /// record `contains_anonymous_type_literal` (RUE-1837); only the difference
    /// across a body matters, so it never needs resetting.
    anonymous_type_literals: usize,
}
struct PrimitiveTypeSpurs {
    i8: Spur,
    i16: Spur,
    i32: Spur,
    i64: Spur,
    u8: Spur,
    u16: Spur,
    u32: Spur,
    u64: Spur,
    bool: Spur,
    self_type: Spur,
    self_value: Spur,
    type_kw: Spur,
    as_kw: Spur,
    /// The contextual `test` keyword (ADR-0083 §1). Only a keyword at item
    /// position when followed by a string literal; an ordinary identifier
    /// everywhere else.
    test_kw: Spur,
    underscore: Spur,
    drop_kw: Spur,
    drop_marker: Spur,
    allow_directive: Spur,
    copy_directive: Spur,
    repr_directive: Spur,
}

impl PrimitiveTypeSpurs {
    fn new(
        interner: &mut ThreadedRodeo,
        _max_entries: Option<usize>,
    ) -> Result<Self, lasso::LassoErrorKind> {
        let intern = |text: &str| {
            #[cfg(test)]
            if _max_entries
                .is_some_and(|limit| interner.len() >= limit && interner.get(text).is_none())
            {
                return Err(lasso::LassoErrorKind::KeySpaceExhaustion);
            }
            rue_lexer::try_intern(interner, text)
        };
        Ok(Self {
            i8: intern("i8")?,
            i16: intern("i16")?,
            i32: intern("i32")?,
            i64: intern("i64")?,
            u8: intern("u8")?,
            u16: intern("u16")?,
            u32: intern("u32")?,
            u64: intern("u64")?,
            bool: intern("bool")?,
            self_type: intern("Self")?,
            self_value: intern("self")?,
            type_kw: intern("type")?,
            as_kw: intern("as")?,
            test_kw: intern("test")?,
            drop_kw: intern("drop")?,
            drop_marker: intern("__drop")?,
            allow_directive: intern("allow")?,
            copy_directive: intern("copy")?,
            repr_directive: intern("repr")?,
            underscore: intern("_")?,
        })
    }

    fn fallback() -> Self {
        let symbol = Spur::default();
        Self {
            i8: symbol,
            i16: symbol,
            i32: symbol,
            i64: symbol,
            u8: symbol,
            u16: symbol,
            u32: symbol,
            u64: symbol,
            bool: symbol,
            self_type: symbol,
            self_value: symbol,
            type_kw: symbol,
            as_kw: symbol,
            test_kw: symbol,
            drop_kw: symbol,
            drop_marker: symbol,
            allow_directive: symbol,
            copy_directive: symbol,
            repr_directive: symbol,
            underscore: symbol,
        }
    }
}

impl Parser {
    fn parse_items_with_recovery(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        while !self.at(TokenKind::Eof) {
            let start = self.cursor;
            match self.item() {
                Ok(item) => items.push(item),
                Err(()) => {
                    self.cursor = start;
                    let span = self.recover_item();
                    items.push(Item::Error(span));
                }
            }
        }
        items
    }

    /// Snapshot the anonymous-type-literal counter before parsing a body.
    fn anonymous_literal_mark(&self) -> usize {
        self.anonymous_type_literals
    }

    /// Whether any anonymous type literal was parsed since `mark`.
    fn saw_anonymous_literal_since(&self, mark: usize) -> bool {
        self.anonymous_type_literals != mark
    }

    /// Create a parser from lexer tokens and their shared symbol interner.
    pub fn new(tokens: Vec<Token>, mut interner: ThreadedRodeo) -> Self {
        let file_id = tokens.first().map(|t| t.span.file_id).unwrap_or_default();
        let (syms, interner_error) = {
            let _span = info_span!("parser_state_setup").entered();
            match PrimitiveTypeSpurs::new(&mut interner, None) {
                Ok(syms) => (syms, None),
                Err(kind) => (PrimitiveTypeSpurs::fallback(), Some(kind)),
            }
        };
        Self {
            tokens,
            cursor: 0,
            interner,
            syms,
            file_id,
            errors: diagnostics::ParserDiagnostics::default(),
            interner_error,
            anonymous_type_literals: 0,
        }
    }

    #[cfg(test)]
    fn new_with_interner_limit_for_test(
        tokens: Vec<Token>,
        mut interner: ThreadedRodeo,
        max_entries: usize,
    ) -> Self {
        let file_id = tokens.first().map(|t| t.span.file_id).unwrap_or_default();
        let (syms, interner_error) = match PrimitiveTypeSpurs::new(&mut interner, Some(max_entries))
        {
            Ok(syms) => (syms, None),
            Err(kind) => (PrimitiveTypeSpurs::fallback(), Some(kind)),
        };
        Self {
            tokens,
            cursor: 0,
            interner,
            syms,
            file_id,
            errors: diagnostics::ParserDiagnostics::default(),
            interner_error,
            anonymous_type_literals: 0,
        }
    }

    /// Parse into an AST, returning all parser diagnostics on failure.
    pub fn parse(self) -> MultiErrorResult<(Ast, ThreadedRodeo)> {
        self.parse_preserving_interner()
            .map_err(|(errors, _interner)| errors)
    }

    /// Parse while retaining the shared interner when this file is malformed.
    pub fn parse_preserving_interner(
        self,
    ) -> Result<(Ast, ThreadedRodeo), (CompileErrors, ThreadedRodeo)> {
        self.parse_preserving_interner_and_tokens()
            .map(|(ast, interner, _tokens)| (ast, interner))
            .map_err(|(errors, interner, _tokens)| (errors, interner))
    }

    /// Parse while returning the exact input token allocation to the caller.
    ///
    /// Compiler consumers that need tokens for a transient post-parse
    /// projection can borrow this returned vector without cloning it. The
    /// ordinary [`Self::parse`] and [`Self::parse_preserving_interner`] entry
    /// points discard it after parsing.
    pub fn parse_preserving_interner_and_tokens(
        mut self,
    ) -> Result<(Ast, ThreadedRodeo, Vec<Token>), (CompileErrors, ThreadedRodeo, Vec<Token>)> {
        if let Some(kind) = self.interner_error {
            return Err((
                CompileErrors::from(CompileError::without_span(rue_lexer::interner_error_kind(
                    kind,
                    "the parser could not intern a required primitive spelling",
                ))),
                self.interner,
                self.tokens,
            ));
        }
        let input_token_count = self.tokens.len();
        let parser_token_count = self
            .tokens
            .iter()
            .filter(|token| token.kind != TokenKind::Eof)
            .count();
        let nesting_error = {
            let _span = info_span!("parser_nesting_scan").entered();
            nesting::check_nesting_depth(&self.tokens)
        };
        if let Some(error) = nesting_error {
            info!(
                outcome = "nesting_error",
                input_token_count,
                parser_token_count,
                ast_item_count = 0,
                raw_parse_error_count = 0,
                parse_error_count = 0,
                validation_error_count = 0,
                "parser complete"
            );
            return Err((CompileErrors::from(vec![error]), self.interner, self.tokens));
        }

        let items = {
            let _span = info_span!("parser_grammar_execution").entered();
            self.parse_items_with_recovery()
        };
        let raw_parse_error_count = self.errors.raw_count();
        let (errors, diagnostic_equality_checks) = std::mem::take(&mut self.errors).finish();
        let parse_error_count = errors.len();
        if !errors.is_empty() {
            info!(
                outcome = "parse_error",
                input_token_count,
                parser_token_count,
                ast_item_count = 0,
                raw_parse_error_count,
                parse_error_count,
                diagnostic_equality_checks,
                validation_error_count = 0,
                "parser complete"
            );
            return Err((CompileErrors::from(errors), self.interner, self.tokens));
        }

        let ast = Ast { items };
        let validation = {
            let _span = info_span!("parser_directive_validation").entered();
            crate::validate::check_directives(&ast, &self.interner)
        };
        if !validation.is_empty() {
            info!(
                outcome = "validation_error",
                input_token_count,
                parser_token_count,
                ast_item_count = ast.items.len(),
                raw_parse_error_count,
                parse_error_count,
                validation_error_count = validation.len(),
                "parser complete"
            );
            return Err((CompileErrors::from(validation), self.interner, self.tokens));
        }

        info!(
            outcome = "success",
            input_token_count,
            parser_token_count,
            ast_item_count = ast.items.len(),
            raw_parse_error_count,
            parse_error_count,
            validation_error_count = 0,
            "parser complete"
        );
        Ok((ast, self.interner, self.tokens))
    }
}

mod declarations;
mod expressions;
mod shared;
mod statements;
mod types;

#[cfg(test)]
impl Parser {
    fn parse_recovered_for_test(mut self) -> (Ast, Vec<CompileError>, ThreadedRodeo) {
        let items = self.parse_items_with_recovery();
        let (errors, _) = std::mem::take(&mut self.errors).finish();
        (Ast { items }, errors, self.interner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_error::MAX_NESTING_DEPTH;
    use rue_lexer::Lexer;
    use std::time::{Duration, Instant};

    fn parse_source(source: &str) -> Result<(Ast, ThreadedRodeo), CompileErrors> {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        Parser::new(tokens, interner).parse()
    }

    #[test]
    fn primitive_symbol_key_limit_is_reported_as_a_resource_limit() {
        let interner = ThreadedRodeo::with_memory_limits(lasso::MemoryLimits::for_memory_usage(1));
        let errors = Parser::new_with_interner_limit_for_test(Vec::new(), interner, 0)
            .parse()
            .unwrap_err();
        assert!(matches!(
            errors.first().map(|error| &error.kind),
            Some(ErrorKind::CompilerResourceLimit(_))
        ));
    }

    #[test]
    fn grammar_domains_compose_through_the_single_parser_entry() {
        let (ast, _) = parse_source(
            "fn choose(xs: [i32; 2], flag: bool,) -> i32 { let value: i32 = 1 + 2 * 3; if flag { value } else { 0 } }",
        )
        .unwrap();
        assert_eq!(ast.items.len(), 1);
        assert!(matches!(ast.items.first(), Some(Item::Function(_))));
    }

    #[test]
    fn enum_display_retains_declaration_directives() {
        let (ast, _) = parse_source("@non_exhaustive pub enum Color { Red, Green }").unwrap();
        let rendered = ast.to_string();
        let enum_start = rendered.find("Enum sym:").expect("enum is rendered");
        assert!(
            rendered[..enum_start].contains("@sym:"),
            "enum directives must remain visible before the declaration: {rendered}"
        );
    }

    #[test]
    fn representative_language_surface_parses() {
        for source in [
            "fn main(a: i32, borrow xs: [i32]) -> i32 { let mut x: i32 = a + 2 * 3; x = x - 1; if x > 0 { x } else { 0 } }",
            "@copy pub struct Pair { first: i32, second: i32, fn sum(borrow self) -> i32 { self.first + self.second } } enum Choice { None, One(i32), Pair(i32, i32), }",
            "fn use_all() -> i32 { let p = Pair { first: 1, second: 2 }; let a = [1, 2, 3,]; let b = [0; 4]; p.sum() + a[0] + b[1] }",
            "fn fixed_string(value: Str(8)) -> Str(8) { value }",
            "fn control(x: i32) -> i32 { while x > 10 { break; } match x { 0 => 1, -1 => 2, Choice.One(v) => v, _ => 0, } }",
            "fn boundaries() -> i32 { { 1 } -2 } fn qualified(x: pkg.Choice) -> i32 { match x { pkg.Choice.One(v) => v, _ => 0 } }",
            "fn generic(comptime T: type, comptime N: i32) -> type { struct { value: T, data: [i32; N], fn get(borrow self) -> T { self.value } } }",
            "pub const io = @import(\"io.rue\"); drop fn Pair(self) { @drop(self.first); }",
        ] {
            parse_source(source).unwrap_or_else(|errors| panic!("{errors:?}\n{source}"));
        }
    }

    #[test]
    fn canonical_item_start_table_matches_every_dispatchable_item() {
        let rows: &[(&str, recovery::ItemStart, fn(&Item) -> bool)] = &[
            (
                "@allow(unused_function) fn directed() {}",
                recovery::ItemStart::Directive,
                |item| matches!(item, Item::Function(_)),
            ),
            ("pub fn public() {}", recovery::ItemStart::Public, |item| {
                matches!(item, Item::Function(_))
            }),
            (
                "unchecked fn raw() {}",
                recovery::ItemStart::UncheckedFunction,
                |item| matches!(item, Item::Function(_)),
            ),
            ("fn plain() {}", recovery::ItemStart::Function, |item| {
                matches!(item, Item::Function(_))
            }),
            (
                "linear struct Buffer {}",
                recovery::ItemStart::LinearStruct,
                |item| matches!(item, Item::Struct(_)),
            ),
            ("struct Record {}", recovery::ItemStart::Struct, |item| {
                matches!(item, Item::Struct(_))
            }),
            ("enum Choice { A }", recovery::ItemStart::Enum, |item| {
                matches!(item, Item::Enum(_))
            }),
            (
                "drop fn Buffer(self) {}",
                recovery::ItemStart::Drop,
                |item| matches!(item, Item::DropFn(_)),
            ),
            (
                "extern \"C\" { fn imported() -> i32; }",
                recovery::ItemStart::Extern,
                |item| matches!(item, Item::Extern(_)),
            ),
            (
                "const ANSWER: i32 = 42;",
                recovery::ItemStart::Const,
                |item| matches!(item, Item::Const(_)),
            ),
            ("test \"works\" {}", recovery::ItemStart::Test, |item| {
                matches!(item, Item::Test(_))
            }),
        ];

        assert_eq!(rows.len(), recovery::ITEM_STARTS.len());
        for metadata in recovery::ITEM_STARTS {
            if let Some(token) = metadata.token {
                assert_eq!(
                    recovery::classify_item_start(&token, false),
                    Some(metadata.start)
                );
            }
            assert!(
                rows.iter()
                    .any(|(_, row_start, _)| *row_start == metadata.start),
                "missing parser case for {:?}",
                metadata.token
            );
        }
        for (source, expected_start, expected_item) in rows {
            let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
            let parser = Parser::new(tokens, interner);
            assert_eq!(
                recovery::classify_item_start(&parser.kind(), parser.at_test_item()),
                Some(*expected_start),
                "{source}"
            );

            let (ast, _) =
                parse_source(source).unwrap_or_else(|errors| panic!("{source}: {errors:?}"));
            assert_eq!(ast.items.len(), 1, "{source}");
            assert!(expected_item(&ast.items[0]), "{source}: {:?}", ast.items[0]);
            let span = match &ast.items[0] {
                Item::Function(item) => item.span,
                Item::Struct(item) => item.span,
                Item::Enum(item) => item.span,
                Item::DropFn(item) => item.span,
                Item::Extern(item) => item.span,
                Item::Const(item) => item.span,
                Item::Test(item) => item.span,
                Item::Error(span) => *span,
            };
            assert_eq!(span, Span::new(0, source.len() as u32), "{source}");
        }

        // The public extern spelling starts with the canonical `pub` prefix,
        // then dispatches through the same `Extern` classification as imports.
        let (ast, _) = parse_source("pub extern \"C\" fn exported() -> i32 { 0 }").unwrap();
        let Item::Function(function) = &ast.items[0] else {
            panic!("expected an extern export, got {:?}", ast.items[0]);
        };
        assert_eq!(function.visibility, Visibility::Public);
        assert_eq!(function.export_abi.as_deref(), Some("C"));
    }

    #[test]
    fn item_recovery_preserves_all_contextual_adjacent_item_boundaries() {
        let cases = [
            (
                "fn broken( -> i32 { 0 }\nextern \"C\" { fn bad( -> i32; }",
                vec![
                    (
                        11,
                        "'comptime' or 'inout' or 'borrow' or identifier or …",
                        "'->'",
                    ),
                    (
                        45,
                        "'comptime' or 'inout' or 'borrow' or identifier or …",
                        "'->'",
                    ),
                ],
            ),
            (
                "fn broken( -> i32 { 0 }\ntest \"next\" 1",
                vec![
                    (
                        11,
                        "'comptime' or 'inout' or 'borrow' or identifier or …",
                        "'->'",
                    ),
                    (36, "'{'", "integer"),
                ],
            ),
            (
                "fn broken( -> i32 { 0 }\n@allow(unused_function) fn next( -> i32 { 0 }",
                vec![
                    (
                        11,
                        "'comptime' or 'inout' or 'borrow' or identifier or …",
                        "'->'",
                    ),
                    (
                        57,
                        "'comptime' or 'inout' or 'borrow' or identifier or …",
                        "'->'",
                    ),
                ],
            ),
        ];

        for (source, expected) in cases {
            let errors = parse_source(source).unwrap_err();
            assert_eq!(errors.len(), expected.len(), "{source}: {errors:?}");
            for (error, (start, expected_text, found)) in errors.iter().zip(expected) {
                assert_eq!(error.span().map(|span| span.start), Some(start), "{source}");
                assert_eq!(
                    error.kind,
                    ErrorKind::UnexpectedToken {
                        expected: expected_text.into(),
                        found: found.into(),
                    },
                    "{source}"
                );
            }
        }
    }

    fn parse_recovered_source(source: &str) -> (Ast, Vec<CompileError>, ThreadedRodeo) {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        Parser::new(tokens, interner).parse_recovered_for_test()
    }

    fn assert_two_malformed_function_diagnostics(
        source: &str,
        errors: &[CompileError],
        starts: [u32; 2],
    ) {
        assert_eq!(errors.len(), 2, "{source}: {errors:?}");
        for (error, start) in errors.iter().zip(starts) {
            assert_eq!(error.span(), Some(Span::new(start, start + 2)), "{source}");
            assert_eq!(
                error.kind,
                ErrorKind::UnexpectedToken {
                    expected: "'comptime' or 'inout' or 'borrow' or identifier or …".into(),
                    found: "'->'".into(),
                },
                "{source}"
            );
        }
    }

    #[test]
    fn recovered_ast_retains_private_and_public_extern_items_between_errors() {
        let private =
            "fn broken( -> i32 { 0 }\nextern \"C\" { fn kept() -> i32; }\nfn later( -> i32 { 0 }";
        let (ast, errors, interner) = parse_recovered_source(private);
        assert_two_malformed_function_diagnostics(private, &errors, [11, 67]);
        assert_eq!(ast.items.len(), 3, "{ast:?}");
        assert_eq!(ast.items[0], Item::Error(Span::new(0, 2)));
        let Item::Extern(extern_block) = &ast.items[1] else {
            panic!("private extern boundary was not retained: {ast:?}");
        };
        assert_eq!(extern_block.span, Span::new(24, 56));
        assert_eq!(extern_block.abi, "C");
        assert_eq!(extern_block.fns.len(), 1);
        assert_eq!(interner.resolve(&extern_block.fns[0].name.name), "kept");
        assert_eq!(ast.items[2], Item::Error(Span::new(57, 59)));

        let public = "fn broken( -> i32 { 0 }\npub extern \"C\" fn kept() -> i32 { 0 }\nfn later( -> i32 { 0 }";
        let (ast, errors, interner) = parse_recovered_source(public);
        assert_two_malformed_function_diagnostics(public, &errors, [11, 72]);
        assert_eq!(ast.items.len(), 3, "{ast:?}");
        assert_eq!(ast.items[0], Item::Error(Span::new(0, 2)));
        let Item::Function(export) = &ast.items[1] else {
            panic!("public extern boundary was not retained: {ast:?}");
        };
        assert_eq!(export.span, Span::new(24, 61));
        assert_eq!(export.visibility, Visibility::Public);
        assert_eq!(export.export_abi.as_deref(), Some("C"));
        assert_eq!(interner.resolve(&export.name.name), "kept");
        assert_eq!(ast.items[2], Item::Error(Span::new(62, 64)));
    }

    #[test]
    fn expected_item_inventory_has_one_source_authority() {
        let sources = [
            ("lib.rs", include_str!("lib.rs")),
            ("ast.rs", include_str!("ast.rs")),
            ("intrinsics.rs", include_str!("intrinsics.rs")),
            ("parser.rs", include_str!("parser.rs")),
            (
                "parser/declarations.rs",
                include_str!("parser/declarations.rs"),
            ),
            (
                "parser/expressions.rs",
                include_str!("parser/expressions.rs"),
            ),
            ("parser/shared.rs", include_str!("parser/shared.rs")),
            ("parser/statements.rs", include_str!("parser/statements.rs")),
            ("parser/types.rs", include_str!("parser/types.rs")),
            (
                "parser_policy/condition.rs",
                include_str!("parser_policy/condition.rs"),
            ),
            (
                "parser_policy/diagnostics.rs",
                include_str!("parser_policy/diagnostics.rs"),
            ),
            ("parser_policy/mod.rs", include_str!("parser_policy/mod.rs")),
            (
                "parser_policy/nesting.rs",
                include_str!("parser_policy/nesting.rs"),
            ),
            (
                "parser_policy/recovery.rs",
                include_str!("parser_policy/recovery.rs"),
            ),
            ("validate.rs", include_str!("validate.rs")),
        ];
        let declarations = include_str!("parser/declarations.rs");
        let item_dispatch = declarations
            .split("pub(super) fn item")
            .nth(1)
            .expect("item parser exists")
            .split("pub(super) fn at_test_item")
            .next()
            .expect("contextual-item classifier follows item parser");
        assert!(
            !item_dispatch.contains("TokenKind::"),
            "item dispatch must use the canonical classification instead of a token inventory"
        );

        let recovery_source = include_str!("parser_policy/recovery.rs");
        let expected_literal = "'@' or 'pub' or 'unchecked' or 'fn' or 'test' or …";
        fn production_source(source: &str) -> &str {
            source
                .split_once("#[cfg(test)]\nmod tests")
                .map_or(source, |(production, _)| production)
        }
        let parser_production = production_source(include_str!("parser.rs"));
        assert!(parser_production.contains("mod types;"));
        assert!(!parser_production.contains("fn canonical_item_start_table_matches"));
        assert_eq!(
            sources
                .iter()
                .map(|(_, source)| production_source(source).matches(expected_literal).count())
                .sum::<usize>(),
            0,
            "expected-item presentation must be derived from metadata"
        );
        assert!(item_dispatch.contains("recovery::classify_item_start"));
        assert!(recovery_source.contains("classify_item_start(token, at_test_item).is_some()"));
        assert_eq!(
            sources
                .iter()
                .map(|(_, source)| production_source(source)
                    .matches("const ITEM_STARTS:")
                    .count())
                .sum::<usize>(),
            1,
            "there must be one canonical item-start metadata table"
        );
        assert_eq!(
            recovery::expected_item(),
            "'@' or 'pub' or 'unchecked' or 'fn' or 'test' or …"
        );
        assert_eq!(
            recovery::expected_after_public(),
            "'unchecked' or 'fn' or 'linear' or 'struct' or …"
        );

        fn modules(source: &str) -> Vec<&str> {
            source
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    let declaration = line
                        .strip_prefix("mod ")
                        .or_else(|| line.strip_prefix("pub mod "))
                        .or_else(|| line.strip_prefix("pub(crate) mod "))?;
                    declaration.strip_suffix(';')
                })
                .filter(|name| *name != "tests" && *name != "size_guards")
                .collect()
        }
        assert_eq!(
            modules(include_str!("lib.rs")),
            ["ast", "intrinsics", "parser", "parser_policy", "validate"]
        );
        assert_eq!(
            modules(include_str!("parser.rs")),
            [
                "declarations",
                "expressions",
                "shared",
                "statements",
                "types"
            ]
        );
        assert_eq!(
            modules(include_str!("parser_policy/mod.rs")),
            ["condition", "diagnostics", "nesting", "recovery"]
        );
    }

    #[test]
    fn test_declarations_parse_with_and_without_directives() {
        let (ast, interner) = parse_source(
            "test \"parses a port\" { let x = 1; }\n\
             @allow(unused_variable) test \"tolerates an unused binding\" { let y = 2; }",
        )
        .unwrap();
        assert_eq!(ast.items.len(), 2);
        let Item::Test(first) = &ast.items[0] else {
            panic!("expected a test declaration, got {:?}", ast.items[0]);
        };
        assert_eq!(interner.resolve(&first.name.value), "parses a port");
        assert!(first.directives.is_empty());
        assert!(matches!(first.body, Expr::Block(_)));
        // The header span covers `test "name"` and nothing of the body.
        assert!(first.header_span.start == first.span.start);
        assert!(first.header_span.end < first.span.end);

        let Item::Test(second) = &ast.items[1] else {
            panic!("expected a test declaration, got {:?}", ast.items[1]);
        };
        assert_eq!(
            interner.resolve(&second.name.value),
            "tolerates an unused binding"
        );
        assert_eq!(second.directives.len(), 1);
    }

    #[test]
    fn contextual_test_keyword_stays_an_ordinary_identifier() {
        // `test` is a keyword only at item position followed by a string
        // (ADR-0083 §1). `fn test`, a local `let test`, and a `const test` all
        // keep their ordinary meaning — `std/bitset.rue` has a live `fn test`.
        let (ast, _) = parse_source(
            "fn test(x: i32) -> i32 { x }\n\
             const test: i32 = 1;\n\
             fn main() -> i32 { let test = 2; test }",
        )
        .unwrap();
        assert!(matches!(ast.items[0], Item::Function(_)));
        assert!(matches!(ast.items[1], Item::Const(_)));
        assert!(matches!(ast.items[2], Item::Function(_)));
    }

    #[test]
    fn malformed_test_declarations_report_unexpected_tokens() {
        for source in [
            // `pub` and `unchecked` are not part of the production.
            "pub test \"named\" { }",
            "unchecked test \"named\" { }",
            // A bare `test` at item position is an unknown item prefix.
            "test { }",
            "test;",
            // A name with no block.
            "test \"named\"",
        ] {
            let errors = parse_source(source)
                .err()
                .unwrap_or_else(|| panic!("expected a parse error for {source}"));
            assert!(
                errors
                    .iter()
                    .any(|error| matches!(error.kind, ErrorKind::UnexpectedToken { .. })),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn test_declaration_display_round_trips_through_the_ast_printer() {
        let (ast, _) = parse_source("test \"round trip\" { let x = 1; }").unwrap();
        let rendered = ast.to_string();
        assert!(
            rendered.starts_with("Test sym:"),
            "test declarations render under their own kind: {rendered}"
        );
        assert!(rendered.contains("Let sym:"), "{rendered}");
    }

    #[test]
    fn item_recovery_resynchronizes_on_a_test_declaration() {
        // A malformed item must not swallow the test declaration that follows
        // it: recovery treats the contextual `test` keyword as an item prefix
        // even though its token is an ordinary identifier (ADR-0083 §1). The
        // second test declaration here is itself malformed, so it reports its
        // own diagnostic exactly when recovery stopped at it.
        let source = "fn broken( -> i32 { 0 }\ntest \"a\" 1";
        let errors = parse_source(source).unwrap_err();
        assert_eq!(errors.len(), 2, "{errors:?}");
        let starts = errors
            .iter()
            .filter_map(|error| error.span().map(|span| span.start))
            .collect::<Vec<_>>();
        assert!(
            starts.contains(&(source.rfind('1').expect("source has the trailing token") as u32)),
            "{errors:?}"
        );
    }

    #[test]
    fn multiple_directives_before_let_parse_as_one_statement() {
        let (ast, _) = parse_source(
            "fn main() -> i32 { \
             @allow(unused_variable, unreachable_code) \
             @allow(unused_function) \
             let x = 1; x }",
        )
        .unwrap();
        let Item::Function(function) = &ast.items[0] else {
            panic!("expected function");
        };
        let Expr::Block(body) = &function.body else {
            panic!("expected block body");
        };
        let Statement::Let(statement) = &body.statements[0] else {
            panic!("expected directed let statement");
        };
        assert_eq!(statement.directives().len(), 2);
        assert_eq!(statement.directives()[0].args.len(), 2);
        assert_eq!(statement.directives()[1].args.len(), 1);
    }

    #[test]
    fn reviewed_grammar_edges_are_bounded_and_targeted() {
        parse_source("fn f() { @probe([i32]); }").unwrap();
        parse_source("fn f() -> type { struct { fn make() -> i32 { 1 } } }").unwrap();
        for source in [
            "fn f() { let x = [0; count(1)]; }",
            "struct S { fn f() {} late: i32 }",
            "fn outer() -> type { struct { fn f() {} late: i32 } }",
            "fn outer() -> type { struct { a: i32 b: i32 } }",
            "fn f(x: struct { fn make() {} }) {}",
        ] {
            assert!(parse_source(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn rich_diagnostic_contract_is_preserved() {
        let errors = parse_source("fn main() -> i32 { let impl = 2; impl }").unwrap_err();
        let error = errors.first().unwrap();
        assert_eq!(
            error.kind,
            ErrorKind::UnexpectedToken {
                expected: "'mut' or identifier or '_'".into(),
                found: "'impl'".into(),
            }
        );
        assert_eq!(error.span(), Some(Span::new(23, 27)));
        assert_eq!(error.diagnostic().helps.len(), 1);

        let errors = parse_source("fn foo(a: [i32; ]) -> i32 { 0 }").unwrap_err();
        assert_eq!(
            errors.first().unwrap().kind,
            ErrorKind::UnexpectedToken {
                expected: "array length".into(),
                found: "']'".into(),
            }
        );
    }

    #[test]
    fn item_recovery_reports_each_malformed_function() {
        let errors =
            parse_source("fn foo( -> i32 { 0 }\nfn bar( -> i32 { 0 }\nfn baz( -> i32 { 0 }")
                .unwrap_err();
        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors
                .iter()
                .filter_map(|error| error.span().map(|span| span.start))
                .collect::<Vec<_>>(),
            vec![8, 29, 50]
        );
    }

    #[test]
    fn recovery_diagnostics_use_offending_spans_and_continue_without_cascades() {
        let cases = [
            // The initial let-pattern error already identifies the reserved
            // keyword. Item recovery must not add a second error for it.
            ("fn main() -> i32 { let fn = 2; 0 }", 1, vec![23..25]),
            ("fn main() -> i32 { let struct = 2; 0 }", 1, vec![23..29]),
            // The missing struct name is followed by a malformed method; the
            // later top-level function remains a recovery synchronization
            // point.
            (
                "struct { fn () {} }\nfn main() -> i32 { 0 }",
                2,
                vec![7..8, 9..11],
            ),
            // `ident_expected` owns the one diagnostic for an intrinsic whose
            // name is followed by a non-identifier.
            ("fn main() -> i32 { @1(2); 0 }", 1, vec![20..21]),
        ];

        for (source, expected_count, expected_ranges) in cases {
            let errors = parse_source(source).unwrap_err();
            assert_eq!(errors.len(), expected_count, "{source}: {errors:?}");
            let ranges = errors
                .iter()
                .map(|error| {
                    let span = error.span().expect("parser diagnostics have spans");
                    assert!(span.start <= span.end, "{source}: {span:?}");
                    span.start..span.end
                })
                .collect::<Vec<_>>();
            assert_eq!(ranges, expected_ranges, "{source}: {errors:?}");
        }
    }

    #[test]
    fn all_recovery_diagnostic_spans_are_non_reversed() {
        for source in [
            "fn main() -> i32 { let fn = 2; 0 }",
            "fn main() -> i32 { let struct = 2; 0 }",
            "struct { fn () {} } fn main() -> i32 { 0 }",
            "fn main() -> i32 { @1(2); 0 }",
            "struct S { fn broken( -> i32 { 0 } fn ok() -> i32 { 0 } }",
        ] {
            let errors = parse_source(source).unwrap_err();
            assert!(
                errors
                    .iter()
                    .filter_map(CompileError::span)
                    .all(|span| span.start <= span.end),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn thousands_of_recovery_points_have_a_bounded_diagnostic_result() {
        let mut source = String::new();
        for index in 0..2_000 {
            source.push_str(&format!("fn broken{index}(,) -> i32 {{ 0 }}\n"));
        }

        let errors = parse_source(&source).unwrap_err();

        assert_eq!(
            errors.len(),
            crate::PARSER_DIAGNOSTIC_BUDGET + 1,
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .take(crate::PARSER_DIAGNOSTIC_BUDGET)
                .all(|error| matches!(error.kind, ErrorKind::UnexpectedToken { .. }))
        );
        assert!(matches!(
            errors.as_slice().last().map(|error| &error.kind),
            Some(ErrorKind::ParserDiagnosticsOmitted { limit })
                if *limit == crate::PARSER_DIAGNOSTIC_BUDGET
        ));
    }

    #[test]
    fn thousands_of_validation_errors_use_the_same_bounded_parser_budget() {
        let mut source = String::new();
        for index in 0..2_000 {
            source.push_str(&format!("@unknown{index} fn valid{index}() {{}}\n"));
        }

        let errors = parse_source(&source).unwrap_err();

        assert_eq!(errors.len(), crate::PARSER_DIAGNOSTIC_BUDGET + 1);
        assert!(
            errors
                .iter()
                .take(crate::PARSER_DIAGNOSTIC_BUDGET)
                .all(|error| matches!(&error.kind, ErrorKind::ParseError(message)
                if message.starts_with("unknown directive")))
        );
        assert!(matches!(
            errors.as_slice().last().map(|error| &error.kind),
            Some(ErrorKind::ParserDiagnosticsOmitted { limit })
                if *limit == crate::PARSER_DIAGNOSTIC_BUDGET
        ));
    }

    #[test]
    fn one_long_discarded_region_makes_progress_without_payload_growth() {
        let source = format!("fn broken( {}", "discarded ".repeat(50_000));

        let errors = parse_source(&source).unwrap_err();

        assert_eq!(errors.len(), 1);
        assert!(errors.first().unwrap().to_string().len() < 100);
    }

    #[test]
    fn struct_body_error_does_not_reparse_sibling_methods() {
        // RUE-726: one real error inside a method body must not resynchronize
        // at the struct's remaining methods and re-report them as malformed
        // free functions at earlier, valid lines.
        let source = "struct S {\n    n: i64,\n    fn a(borrow self) -> i64 { self.n }\n    \
                      fn b(borrow self) -> i64 {\n        match self.n {\n            \
                      Some(x) => x,\n            _ => 0,\n        }\n    }\n}\n\
                      fn main() -> i32 { 0 }";
        let errors = parse_source(source).unwrap_err();
        assert_eq!(errors.len(), 1, "expected only the real error: {errors:?}");
    }

    #[test]
    fn nesting_limit_is_checked_before_recursive_descent() {
        let depth = MAX_NESTING_DEPTH + 1;
        let source = format!(
            "fn main() -> i32 {{ {}0{} }}",
            "(".repeat(depth),
            ")".repeat(depth)
        );
        let errors = parse_source(&source).unwrap_err();
        assert!(matches!(
            errors.first().map(|error| &error.kind),
            Some(ErrorKind::NestingLimitExceeded { .. })
        ));
    }

    fn median_parse(source: &str, iterations: usize) -> Duration {
        let mut samples = Vec::new();
        for _ in 0..7 {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
                let start = Instant::now();
                Parser::new(tokens, interner).parse().unwrap();
                elapsed += start.elapsed();
            }
            samples.push(elapsed);
        }
        samples.sort_unstable();
        samples[samples.len() / 2]
    }

    /// Parse a function whose body is a single tail intrinsic call and return
    /// that call's arguments.
    fn intrinsic_args(source: &str) -> Vec<IntrinsicArg> {
        let (ast, _) = parse_source(source).unwrap_or_else(|errors| panic!("{errors:?}\n{source}"));
        let Item::Function(function) = &ast.items[0] else {
            panic!("expected function in {source}");
        };
        let Expr::Block(body) = &function.body else {
            panic!("expected block body in {source}");
        };
        let Expr::IntrinsicCall(call) = &*body.expr else {
            panic!("expected tail intrinsic call in {source}");
        };
        call.args.clone()
    }

    #[test]
    fn type_position_intrinsics_accept_the_full_type_grammar() {
        // Parity table (RUE-788): every `TypeExpr` form an annotation accepts
        // must parse identically in a type-position intrinsic argument.
        // `TypeExpr::StrFixed` has no dedicated surface spelling here —
        // `Str(8)` parses as a `TypeCall` whose canonicalization AstGen owns —
        // and `TypeExpr::IntArg` only occurs inside a call argument list
        // (covered by the `Buffer(2)` row).
        let rows: &[(&str, fn(&TypeExpr) -> bool)] = &[
            ("i32", |t| matches!(t, TypeExpr::Named(_))),
            ("Point", |t| matches!(t, TypeExpr::Named(_))),
            ("Self", |t| matches!(t, TypeExpr::Named(_))),
            ("lib.geo.Point", |t| matches!(t, TypeExpr::Qualified { .. })),
            ("()", |t| matches!(t, TypeExpr::Unit(_))),
            ("!", |t| matches!(t, TypeExpr::Never(_))),
            ("[i32; 4]", |t| matches!(t, TypeExpr::Array { .. })),
            ("[i32; N]", |t| matches!(t, TypeExpr::Array { .. })),
            ("[i32]", |t| matches!(t, TypeExpr::Slice { .. })),
            ("[[u8; 2]; 3]", |t| matches!(t, TypeExpr::Array { .. })),
            ("ptr const i32", |t| {
                matches!(t, TypeExpr::PointerConst { .. })
            }),
            ("ptr mut ptr const u8", |t| {
                matches!(t, TypeExpr::PointerMut { .. })
            }),
            ("Pair(i32, [i32; 2])", |t| {
                matches!(t, TypeExpr::TypeCall { .. })
            }),
            ("Buffer(2)", |t| {
                matches!(t, TypeExpr::TypeCall { args, .. }
                    if matches!(args[0], TypeExpr::IntArg { value: 2, .. }))
            }),
            ("lib.pair.Pair(i32)", |t| {
                matches!(t, TypeExpr::QualifiedTypeCall { .. })
            }),
        ];
        for (spelling, is_expected_variant) in rows {
            for intrinsic in [
                "size_of",
                "align_of",
                "require_droppable",
                "require_trivially_droppable",
                "int_max",
                "int_min",
            ] {
                let source = format!("fn f() -> i32 {{ @{intrinsic}({spelling}) }}");
                let args = intrinsic_args(&source);
                assert!(
                    matches!(&args[0], IntrinsicArg::Type(ty) if is_expected_variant(ty)),
                    "@{intrinsic}({spelling}) parsed as {args:?}"
                );
            }
            // `@offset_of` parses its first argument as a type and its second
            // as an ordinary expression.
            let source = format!("fn f() -> i32 {{ @offset_of({spelling}, x) }}");
            let args = intrinsic_args(&source);
            assert!(
                matches!(&args[0], IntrinsicArg::Type(ty) if is_expected_variant(ty)),
                "@offset_of({spelling}, x) parsed as {args:?}"
            );
            assert!(matches!(&args[1], IntrinsicArg::Expr(Expr::Ident(_))));
        }

        // Anonymous type literals are creation sites, legal only in comptime
        // expression position (a type constructor's body). The shared type
        // grammar rejects them everywhere it is entered — annotations, returns,
        // and type-position intrinsic arguments alike (RUE-1089).
        for spelling in ["struct { x: i32, y: Pair(i32) }", "enum { A, B(i32) }"] {
            let source = format!("fn f() -> i32 {{ @size_of({spelling}) }}");
            assert!(
                parse_source(&source).is_err(),
                "@size_of({spelling}) must be rejected in type position"
            );
        }
    }

    #[test]
    fn type_position_intrinsics_reject_value_expressions_with_one_diagnostic() {
        for source in [
            "fn f() -> i32 { @size_of(1) }",
            "fn f() -> i32 { @size_of(1 + 2) }",
            "fn f() -> i32 { @size_of(-x) }",
            "fn f() -> i32 { @size_of(\"s\") }",
            "fn f() -> i32 { @size_of(x + 1) }",
            "fn f() -> i32 { @size_of(Point { x: 1 }) }",
            "fn f() -> i32 { @align_of(!x) }",
            "fn f() -> i32 { @require_droppable(!true) }",
            "fn f() -> i32 { @int_max(1 + 2) }",
            "fn f() -> i32 { @int_min(\"s\") }",
            "fn f() -> i32 { @offset_of(1 + 2, x) }",
        ] {
            let errors = parse_source(source).unwrap_err();
            assert_eq!(
                errors.len(),
                1,
                "expected one targeted diagnostic for {source}, got {errors:?}"
            );
        }
    }

    #[test]
    fn value_position_intrinsics_keep_the_expression_grammar() {
        // A value-position intrinsic still takes full expressions, with the
        // unambiguous-type-token carve-out unchanged: a bare `!` is the never
        // type while `!x` stays a prefix-not expression.
        let args = intrinsic_args("fn f() -> i32 { @probe(!, !x, a + 1, (), [1, 2], Point) }");
        assert!(matches!(&args[0], IntrinsicArg::Type(TypeExpr::Never(_))));
        assert!(matches!(&args[1], IntrinsicArg::Expr(Expr::Unary(_))));
        assert!(matches!(&args[2], IntrinsicArg::Expr(Expr::Binary(_))));
        assert!(matches!(&args[3], IntrinsicArg::Type(TypeExpr::Unit(_))));
        assert!(matches!(&args[4], IntrinsicArg::Expr(Expr::ArrayLit(_))));
        assert!(matches!(&args[5], IntrinsicArg::Expr(Expr::Ident(_))));
    }

    #[test]
    fn nested_slice_parsing_avoids_exponential_growth() {
        let source = |depth: usize| {
            format!(
                "fn f(x: {}i32{}) {{}}",
                "[".repeat(depth),
                "]".repeat(depth)
            )
        };
        let shallow = median_parse(&source(32), 100);
        let deep = median_parse(&source(128), 100);
        assert!(
            // The full suite runs test binaries concurrently, so leave enough
            // scheduler headroom while still rejecting the former exponential
            // behavior (96 additional levels was effectively unbounded).
            deep < shallow.saturating_mul(32),
            "32 levels took {shallow:?}; 128 levels took {deep:?}"
        );
    }

    #[test]
    fn continued_blocks_avoid_exponential_speculative_reparsing() {
        let source = |count: usize| {
            format!(
                "fn f() -> i32 {{ {}0 }}",
                "if true { 1 } else { 2 }; ".repeat(count)
            )
        };
        let shallow = median_parse(&source(16), 50);
        let deep = median_parse(&source(64), 50);
        assert!(
            // A 4x input increase may be scheduled unevenly under the parallel
            // unit suite; the former 2^depth behavior exceeds this by orders
            // of magnitude.
            deep < shallow.saturating_mul(32),
            "16 blocks took {shallow:?}; 64 blocks took {deep:?}"
        );
    }
}
