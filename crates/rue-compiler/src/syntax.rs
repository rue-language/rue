//! Shared lexing and parsing kernels.
//!
//! Source-snapshot and multi-file compiler entry points use this module so
//! their syntax work is measured once, at the point where it is performed.

use tracing::{info, info_span};

use crate::{Lexer, MultiErrorResult, Parser, SourceView, ThreadedRodeo};

/// Work performed while lexing and parsing source files.
///
/// Token counts include the EOF token emitted by the lexer. A file contributes
/// tokens only when lexing succeeds and produces the token vector passed to the
/// parser. Source bytes use UTF-8 byte lengths, matching Rue's byte-based spans.
/// Values describe one bounded syntax run; they are neither process-global
/// totals nor metadata attached to a reusable parsed artifact. Import discovery
/// treats one provenance-preserving fixed-point lifecycle as a bounded run, so
/// its values sum only the actual calls made across that lifecycle's snapshot
/// expansions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyntaxWork {
    /// Number of lexer invocations.
    pub lexer_invocations: usize,
    /// Number of parser invocations.
    pub parser_invocations: usize,
    /// Total source bytes presented to the lexer.
    pub lexed_bytes: usize,
    /// Total tokens produced by successful lexer invocations.
    pub tokens: usize,
}

pub(crate) struct FileParseOutcome {
    pub(crate) result: MultiErrorResult<std::sync::Arc<rue_parser::Ast>>,
    pub(crate) interner: ThreadedRodeo,
    pub(crate) tokens: std::sync::Arc<[rue_lexer::Token]>,
    pub(crate) work: SyntaxWork,
}

pub(crate) fn parse_file(source: SourceView<'_>, interner: ThreadedRodeo) -> FileParseOutcome {
    let _file_span = info_span!("parse_file", path = %source.path).entered();
    let mut work = SyntaxWork {
        lexer_invocations: 1,
        lexed_bytes: source.source.len(),
        ..SyntaxWork::default()
    };

    let lexer = Lexer::with_interner_and_file_id(source.source, interner, source.file_id);
    let (tokens, interner) = {
        let _span = info_span!("lexer").entered();
        match lexer.tokenize_preserving_interner() {
            Ok(output) => output,
            Err((errors, interner)) => {
                return FileParseOutcome {
                    result: Err(errors),
                    interner,
                    tokens: std::sync::Arc::from([]),
                    work,
                };
            }
        }
    };

    work.parser_invocations = 1;
    work.tokens = tokens.len();
    info!(token_count = tokens.len(), "lexing complete");
    let retained_tokens: std::sync::Arc<[rue_lexer::Token]> = tokens.clone().into();

    let (ast, interner) = {
        let _span = info_span!("parser").entered();
        let parser = Parser::new(tokens, interner);
        match parser.parse_preserving_interner() {
            Ok(output) => output,
            Err((errors, interner)) => {
                return FileParseOutcome {
                    result: Err(errors),
                    interner,
                    tokens: retained_tokens,
                    work,
                };
            }
        }
    };

    FileParseOutcome {
        result: Ok(std::sync::Arc::new(ast)),
        interner,
        tokens: retained_tokens,
        work,
    }
}

#[cfg(test)]
mod tests {
    use ahash::AHashMap;
    use std::sync::Arc;

    use rue_error::ErrorKind;
    use rue_span::FileId;

    use super::*;
    use crate::unstable::{ColorChoice, DiagnosticFormatter, JsonDiagnosticFormatter, SourceInfo};
    use crate::{CompilerSession, Item, SourceMetadata, SourceSnapshot};

    fn snapshot(entries: &[(u32, &str, &str)]) -> SourceSnapshot {
        let physical_paths: AHashMap<_, _> = entries
            .iter()
            .map(|&(id, path, _)| (FileId::new(id), path.to_owned()))
            .collect();
        let metadata = SourceMetadata::new(
            FileId::new(entries[0].0),
            physical_paths.clone(),
            physical_paths,
        )
        .unwrap();
        let contents = entries
            .iter()
            .map(|&(id, _, source)| (FileId::new(id), Arc::new(source.to_owned())))
            .collect();
        SourceSnapshot::new(metadata, contents).unwrap()
    }

    #[test]
    fn parser_budget_is_per_file_and_later_files_are_still_parsed() {
        fn malformed_items(prefix: &str) -> String {
            let mut source = String::new();
            for index in 0..rue_parser::PARSER_DIAGNOSTIC_BUDGET + 25 {
                source.push_str(&format!("fn {prefix}_{index}(,) -> i32 {{ 0 }}\n"));
            }
            source
        }

        let first = malformed_items("first");
        let second = malformed_items("second");
        let snapshot = snapshot(&[
            (10, "first.rue", &first),
            (20, "good.rue", "fn good() {}"),
            (30, "second.rue", &second),
        ]);

        let mut session = CompilerSession::new();
        let update = session.update(&snapshot);
        let work = update.work();
        let errors = update.into_result().unwrap_err();
        let summaries = errors
            .iter()
            .filter(|error| matches!(error.kind, ErrorKind::ParserDiagnosticsOmitted { .. }))
            .collect::<Vec<_>>();

        assert_eq!(errors.len(), 2 * (rue_parser::PARSER_DIAGNOSTIC_BUDGET + 1));
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].span().unwrap().file_id, FileId::new(10));
        assert_eq!(summaries[1].span().unwrap().file_id, FileId::new(30));
        assert_eq!(work.syntax.parser_invocations, 3);

        let source_info = SourceInfo::new(&first, "first.rue");
        let text = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never)
            .format_error(summaries[0]);
        assert!(
            text.contains(
                "[E0103]: additional parser diagnostics omitted after the first 100 errors"
            )
        );

        let json_formatter = JsonDiagnosticFormatter::new(&source_info);
        let json = json_formatter.format_error(summaries[0]).to_json();
        assert_eq!(json, json_formatter.format_error(summaries[0]).to_json());
        assert!(json.contains("\"code\":\"E0103\""));
        assert!(json.contains(
            "\"message\":\"additional parser diagnostics omitted after the first 100 errors\""
        ));
    }

    #[test]
    fn lexer_budget_is_per_file_and_later_files_are_still_lexed() {
        fn malformed_tokens() -> String {
            "$".repeat(rue_lexer::LEXER_DIAGNOSTIC_BUDGET + 25)
        }

        let first = malformed_tokens();
        let second = malformed_tokens();
        let snapshot = snapshot(&[
            (10, "first.rue", &first),
            (20, "good.rue", "fn good() {}"),
            (30, "second.rue", &second),
        ]);

        let mut session = CompilerSession::new();
        let update = session.update(&snapshot);
        let work = update.work();
        let errors = update.into_result().unwrap_err();
        let summaries = errors
            .iter()
            .filter(|error| matches!(error.kind, ErrorKind::LexerDiagnosticsOmitted { .. }))
            .collect::<Vec<_>>();

        assert_eq!(errors.len(), 2 * (rue_lexer::LEXER_DIAGNOSTIC_BUDGET + 1));
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].span().unwrap().file_id, FileId::new(10));
        assert_eq!(summaries[1].span().unwrap().file_id, FileId::new(30));
        assert_eq!(work.syntax.lexer_invocations, 3);
        assert_eq!(work.syntax.parser_invocations, 1);

        let source_info = SourceInfo::new(&first, "first.rue");
        let text = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never)
            .format_error(summaries[0]);
        assert!(
            text.contains(
                "[E0010]: additional lexer diagnostics omitted after the first 100 errors"
            )
        );

        let json_formatter = JsonDiagnosticFormatter::new(&source_info);
        let json = json_formatter.format_error(summaries[0]).to_json();
        assert_eq!(json, json_formatter.format_error(summaries[0]).to_json());
        assert!(json.contains("\"code\":\"E0010\""));
        assert!(json.contains(
            "\"message\":\"additional lexer diagnostics omitted after the first 100 errors\""
        ));
    }

    #[test]
    fn returned_interner_survives_lex_and_parse_failures() {
        let lex_source = SourceView::new("lex.rue", "fn lex_name() { $ }", FileId::new(1));
        let FileParseOutcome {
            result, interner, ..
        } = parse_file(lex_source, ThreadedRodeo::new());
        assert!(result.is_err());
        let lex_name = interner.get("lex_name").unwrap();
        assert_eq!(interner.resolve(&lex_name), "lex_name");

        let parse_source = SourceView::new("parse.rue", "fn parse_name( { }", FileId::new(2));
        let FileParseOutcome {
            result, interner, ..
        } = parse_file(parse_source, interner);
        assert!(result.is_err());
        let parse_name = interner.get("parse_name").unwrap();
        assert_eq!(interner.resolve(&lex_name), "lex_name");
        assert_eq!(interner.resolve(&parse_name), "parse_name");

        let good_source = SourceView::new("good.rue", "fn good_name() {}", FileId::new(3));
        let FileParseOutcome {
            result, interner, ..
        } = parse_file(good_source, interner);
        let parsed = result.unwrap();
        let Item::Function(function) = &parsed.items[0] else {
            panic!("expected a function");
        };

        assert_eq!(interner.resolve(&lex_name), "lex_name");
        assert_eq!(interner.resolve(&parse_name), "parse_name");
        assert_eq!(interner.resolve(&function.name.name), "good_name");
    }

    #[test]
    fn work_uses_utf8_bytes_and_successful_lexer_token_vectors() {
        let valid = "fn main() { // café\n}";
        let FileParseOutcome { result, work, .. } = parse_file(
            SourceView::new("valid.rue", valid, FileId::new(1)),
            ThreadedRodeo::new(),
        );
        result.unwrap();
        assert!(valid.len() > valid.chars().count());
        assert_eq!(work.lexed_bytes, valid.len());
        assert_eq!(work.tokens, 7);
        assert_eq!(work.parser_invocations, 1);

        let parse_error = "fn broken( {";
        let FileParseOutcome { result, work, .. } = parse_file(
            SourceView::new("parse.rue", parse_error, FileId::new(2)),
            ThreadedRodeo::new(),
        );
        assert!(result.is_err());
        assert_eq!(work.lexed_bytes, parse_error.len());
        assert_eq!(work.tokens, 5);
        assert_eq!(work.parser_invocations, 1);

        let lex_error = "kept_name $";
        let FileParseOutcome { result, work, .. } = parse_file(
            SourceView::new("lex.rue", lex_error, FileId::new(3)),
            ThreadedRodeo::new(),
        );
        assert!(result.is_err());
        assert_eq!(work.lexed_bytes, lex_error.len());
        assert_eq!(work.tokens, 0);
        assert_eq!(work.parser_invocations, 0);
    }
}
