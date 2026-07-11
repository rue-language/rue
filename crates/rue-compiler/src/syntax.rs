//! Shared lexing and parsing kernels.
//!
//! Source-snapshot and multi-file compiler entry points use this module so
//! their syntax work is measured once, at the point where it is performed.

use tracing::{info, info_span};

use crate::{
    CompileErrors, Lexer, MultiErrorResult, ParsedFile, ParsedProgram, Parser, SourceFile,
    SourceSnapshot, ThreadedRodeo,
};

/// Work performed while lexing and parsing source files.
///
/// Token counts include the EOF token emitted by the lexer. A file contributes
/// tokens only when lexing succeeds and produces the token vector passed to the
/// parser. Source bytes use UTF-8 byte lengths, matching Rue's byte-based spans.
/// Values describe one syntax run; they are neither process-global totals nor
/// metadata attached to a reusable parsed artifact.
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

struct FileParseOutcome {
    result: MultiErrorResult<ParsedFile>,
    interner: ThreadedRodeo,
    work: SyntaxWork,
}

pub(crate) struct SnapshotParseOutcome {
    pub(crate) result: MultiErrorResult<ParsedProgram>,
    pub(crate) work: SyntaxWork,
}

fn parse_file(source: SourceFile<'_>, interner: ThreadedRodeo) -> FileParseOutcome {
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
                    work,
                };
            }
        }
    };

    work.parser_invocations = 1;
    work.tokens = tokens.len();
    info!(token_count = tokens.len(), "lexing complete");

    let (ast, interner) = {
        let _span = info_span!("parser").entered();
        let parser = Parser::new(tokens, interner);
        match parser.parse_preserving_interner() {
            Ok(output) => output,
            Err((errors, interner)) => {
                return FileParseOutcome {
                    result: Err(errors),
                    interner,
                    work,
                };
            }
        }
    };

    info!(item_count = ast.items.len(), "parsing complete");

    FileParseOutcome {
        result: Ok(ParsedFile {
            path: source.path.to_owned(),
            file_id: source.file_id,
            ast,
        }),
        interner,
        work,
    }
}

pub(crate) fn parse_snapshot(snapshot: &SourceSnapshot) -> SnapshotParseOutcome {
    let _span = info_span!("parse", file_count = snapshot.len()).entered();
    run_snapshot_unspanned(snapshot)
}

/// Run the shared snapshot parser without creating its outer `parse` span.
///
/// Entry points own that span because `CompilationUnit::parse` historically
/// includes symbol merging in it, while direct snapshot parsing ends after the
/// per-file syntax work. The lexer/parser kernel itself remains shared.
pub(crate) fn run_snapshot_unspanned(snapshot: &SourceSnapshot) -> SnapshotParseOutcome {
    let mut files = Vec::with_capacity(snapshot.len());
    let mut interner = ThreadedRodeo::new();
    let mut errors = CompileErrors::new();
    let mut work = SyntaxWork::default();

    for source in snapshot.files() {
        let outcome = parse_file(source, interner);
        interner = outcome.interner;
        work.lexer_invocations += outcome.work.lexer_invocations;
        work.parser_invocations += outcome.work.parser_invocations;
        work.lexed_bytes += outcome.work.lexed_bytes;
        work.tokens += outcome.work.tokens;

        match outcome.result {
            Ok(file) => files.push(file),
            Err(file_errors) => errors.extend(file_errors),
        }
    }

    let result = if errors.is_empty() {
        Ok(ParsedProgram { files, interner })
    } else {
        Err(errors)
    };

    SnapshotParseOutcome { result, work }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use rue_error::ErrorKind;
    use rue_span::FileId;

    use super::*;
    use crate::{Item, SourceMetadata};

    fn snapshot(entries: &[(u32, &str, &str)]) -> SourceSnapshot {
        let physical_paths: HashMap<_, _> = entries
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
    fn valid_files_report_one_lexer_and_parser_invocation_each() {
        let sources = [
            (7, "first.rue", "fn first() {}"),
            (3, "empty.rue", ""),
            (9, "third.rue", "fn third() {}"),
        ];
        let snapshot = snapshot(&sources);

        let SnapshotParseOutcome { result, work } = parse_snapshot(&snapshot);
        let program = result.unwrap();

        assert_eq!(
            program
                .files
                .iter()
                .map(|file| file.file_id)
                .collect::<Vec<_>>(),
            vec![FileId::new(7), FileId::new(3), FileId::new(9)]
        );
        assert_eq!(
            work,
            SyntaxWork {
                lexer_invocations: 3,
                parser_invocations: 3,
                lexed_bytes: sources.iter().map(|(_, _, source)| source.len()).sum(),
                tokens: 15,
            }
        );
    }

    #[test]
    fn snapshot_collects_lex_and_parse_errors_in_caller_order_and_continues() {
        let sources = [
            (30, "lex.rue", "fn lexed() { $ }"),
            (10, "parse.rue", "fn parsed( { }"),
            (20, "good.rue", "fn good() {}"),
        ];
        let snapshot = snapshot(&sources);

        let SnapshotParseOutcome { result, work } = parse_snapshot(&snapshot);
        let errors = result.unwrap_err();

        assert_eq!(errors.len(), 2);
        assert!(matches!(
            errors.as_slice()[0].kind,
            ErrorKind::UnexpectedCharacter('$')
        ));
        assert_eq!(
            errors
                .iter()
                .map(|error| error.span().unwrap().file_id)
                .collect::<Vec<_>>(),
            vec![FileId::new(30), FileId::new(10)]
        );
        assert_eq!(work.lexer_invocations, 3);
        assert_eq!(work.parser_invocations, 2);
        assert_eq!(
            work.lexed_bytes,
            sources
                .iter()
                .map(|(_, _, source)| source.len())
                .sum::<usize>()
        );
        assert_eq!(work.tokens, 13);
    }

    #[test]
    fn one_lex_invocation_preserves_all_lex_errors() {
        let source = "fn broken() { $ # }";
        let snapshot = snapshot(&[(1, "broken.rue", source)]);

        let SnapshotParseOutcome { result, work } = parse_snapshot(&snapshot);
        let errors = result.unwrap_err();

        assert_eq!(errors.len(), 2);
        assert!(matches!(
            errors.as_slice()[0].kind,
            ErrorKind::UnexpectedCharacter('$')
        ));
        assert!(matches!(
            errors.as_slice()[1].kind,
            ErrorKind::UnexpectedCharacter('#')
        ));
        assert_eq!(
            work,
            SyntaxWork {
                lexer_invocations: 1,
                parser_invocations: 0,
                lexed_bytes: source.len(),
                tokens: 0,
            }
        );
    }

    #[test]
    fn returned_interner_survives_lex_and_parse_failures() {
        let lex_source = SourceFile::new("lex.rue", "fn lex_name() { $ }", FileId::new(1));
        let FileParseOutcome {
            result, interner, ..
        } = parse_file(lex_source, ThreadedRodeo::new());
        assert!(result.is_err());
        let lex_name = interner.get("lex_name").unwrap();
        assert_eq!(interner.resolve(&lex_name), "lex_name");

        let parse_source = SourceFile::new("parse.rue", "fn parse_name( { }", FileId::new(2));
        let FileParseOutcome {
            result, interner, ..
        } = parse_file(parse_source, interner);
        assert!(result.is_err());
        let parse_name = interner.get("parse_name").unwrap();
        assert_eq!(interner.resolve(&lex_name), "lex_name");
        assert_eq!(interner.resolve(&parse_name), "parse_name");

        let good_source = SourceFile::new("good.rue", "fn good_name() {}", FileId::new(3));
        let FileParseOutcome {
            result, interner, ..
        } = parse_file(good_source, interner);
        let parsed = result.unwrap();
        let Item::Function(function) = &parsed.ast.items[0] else {
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
            SourceFile::new("valid.rue", valid, FileId::new(1)),
            ThreadedRodeo::new(),
        );
        result.unwrap();
        assert!(valid.len() > valid.chars().count());
        assert_eq!(work.lexed_bytes, valid.len());
        assert_eq!(work.tokens, 7);
        assert_eq!(work.parser_invocations, 1);

        let parse_error = "fn broken( {";
        let FileParseOutcome { result, work, .. } = parse_file(
            SourceFile::new("parse.rue", parse_error, FileId::new(2)),
            ThreadedRodeo::new(),
        );
        assert!(result.is_err());
        assert_eq!(work.lexed_bytes, parse_error.len());
        assert_eq!(work.tokens, 5);
        assert_eq!(work.parser_invocations, 1);

        let lex_error = "kept_name $";
        let FileParseOutcome { result, work, .. } = parse_file(
            SourceFile::new("lex.rue", lex_error, FileId::new(3)),
            ThreadedRodeo::new(),
        );
        assert!(result.is_err());
        assert_eq!(work.lexed_bytes, lex_error.len());
        assert_eq!(work.tokens, 0);
        assert_eq!(work.parser_invocations, 0);
    }
}
