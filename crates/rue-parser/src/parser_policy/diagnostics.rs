//! Parser-independent diagnostic collection policy.
//!
//! Parser adapters emit [`CompileError`] values through [`ParserDiagnostics`].
//! The collector preserves first occurrence order and treats only equal error
//! kinds at the same complete primary span as duplicates. Rich labels, notes,
//! helps, and suggestions are intentionally not part of this parser policy's
//! exact key.

use std::collections::hash_map::RandomState;
use std::fmt::{self, Write as _};
use std::hash::{BuildHasher, Hash, Hasher};

use ahash::AHashMap;
use rue_error::{CompileError, ErrorKind};

use crate::PARSER_DIAGNOSTIC_BUDGET;

/// A formatting sink that feeds an error kind's display representation into a
/// hasher without allocating a temporary `String`.
struct HashWriter<'a, H>(&'a mut H);

impl<H: Hasher> fmt::Write for HashWriter<'_, H> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.0.write(value.as_bytes());
        Ok(())
    }
}

fn fingerprint(state: &RandomState, error: &CompileError) -> u64 {
    let mut hasher = state.build_hasher();
    error.span().hash(&mut hasher);
    std::mem::discriminant(&error.kind).hash(&mut hasher);
    write!(HashWriter(&mut hasher), "{}", error.kind).expect("hash formatting cannot fail");
    hasher.finish()
}

/// Bounded diagnostic state for one parser invocation and therefore one file.
///
/// Fingerprints make exact deduplication expected-linear. Equality against
/// every matching fingerprint keeps hash collisions exact without cloning
/// [`ErrorKind`]. Only retained details participate in the seen index: after
/// the budget is full, every new unique diagnostic is omitted anyway. One
/// unretained latest diagnostic is held separately because recovery decisions
/// must inspect the error that just occurred, not a stale retained error.
#[derive(Default)]
pub(crate) struct ParserDiagnostics {
    retained: Vec<CompileError>,
    fingerprint_state: RandomState,
    seen: AHashMap<u64, Vec<usize>>,
    last_retained: Option<usize>,
    last_unretained: Option<CompileError>,
    summary_span: Option<rue_span::Span>,
    summary_without_span: bool,
    raw_count: usize,
    exact_comparisons: usize,
}

impl ParserDiagnostics {
    /// Record one raw grammar diagnostic through the canonical bounded path.
    pub(crate) fn push(&mut self, error: CompileError) {
        self.raw_count += 1;
        // Preserve the historical temporal subsumption rule: a *current*
        // narrow identifier error is omitted only when its targeted superset
        // already precedes it. The raw current error still becomes `last()` so
        // recovery sees exactly the error just emitted. A later superset never
        // removes or reorders an earlier narrow diagnostic.
        let current_narrow_is_subsumed = matches!(
            &error.kind,
            ErrorKind::UnexpectedToken { expected, .. } if expected == "identifier"
        ) && self.retained.iter().any(|prior| {
            prior.span() == error.span()
                && matches!(
                    &prior.kind,
                    ErrorKind::UnexpectedToken { expected, .. }
                        if expected == "identifier or 'drop'"
                )
        });
        if current_narrow_is_subsumed {
            self.set_unretained_last(error);
            return;
        }

        let fingerprint = fingerprint(&self.fingerprint_state, &error);
        let duplicate = self.seen.get(&fingerprint).is_some_and(|indices| {
            indices.iter().any(|&index| {
                self.exact_comparisons += 1;
                let prior = &self.retained[index];
                prior.span() == error.span() && prior.kind == error.kind
            })
        });

        if duplicate {
            self.set_unretained_last(error);
            return;
        }

        if self.retained.len() < PARSER_DIAGNOSTIC_BUDGET {
            self.seen
                .entry(fingerprint)
                .or_default()
                .push(self.retained.len());
            self.retained.push(error);
            self.last_retained = Some(self.retained.len() - 1);
            self.last_unretained = None;
        } else {
            if self.summary_span.is_none() && !self.summary_without_span {
                match error.span() {
                    Some(span) => self.summary_span = Some(span),
                    None => self.summary_without_span = true,
                }
            }
            self.set_unretained_last(error);
        }
    }

    fn set_unretained_last(&mut self, error: CompileError) {
        self.last_retained = None;
        self.last_unretained = Some(error);
    }

    pub(crate) fn last(&self) -> Option<&CompileError> {
        self.last_unretained.as_ref().or_else(|| {
            self.last_retained
                .and_then(|index| self.retained.get(index))
        })
    }

    pub(crate) fn raw_count(&self) -> usize {
        self.raw_count
    }

    /// Finish collection, appending the deterministic summary only after
    /// recovery no longer needs `last()`.
    pub(crate) fn finish(mut self) -> (Vec<CompileError>, usize) {
        let summary = ErrorKind::ParserDiagnosticsOmitted {
            limit: PARSER_DIAGNOSTIC_BUDGET,
        };
        if let Some(span) = self.summary_span {
            self.retained.push(CompileError::new(summary, span));
        } else if self.summary_without_span {
            self.retained.push(CompileError::without_span(summary));
        }
        (self.retained, self.exact_comparisons)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_span::{FileId, Span};

    fn error(message: &str, file: u32, start: u32) -> CompileError {
        CompileError::new(
            ErrorKind::ParseError(message.to_string()),
            Span::with_file(FileId::new(file), start, start + 1),
        )
    }

    fn unexpected(expected: &'static str, start: u32) -> CompileError {
        CompileError::new(
            ErrorKind::UnexpectedToken {
                expected: expected.into(),
                found: "'@'".into(),
            },
            Span::new(start, start + 1),
        )
    }

    #[test]
    fn dedupe_preserves_order_and_exact_key_distinctions() {
        let mut diagnostics = ParserDiagnostics::default();
        for error in [
            error("first", 1, 4),
            error("first", 1, 4).with_help("rich payload is ignored by parser dedupe"),
            error("second", 1, 4),
            error("first", 2, 4),
            error("first", 1, 5),
        ] {
            diagnostics.push(error);
        }

        let (errors, _) = diagnostics.finish();

        assert_eq!(errors.len(), 4);
        assert_eq!(errors[0].to_string(), "first");
        assert!(errors[0].diagnostic().helps.is_empty());
        assert_eq!(errors[1].to_string(), "second");
        assert_eq!(errors[2].span().unwrap().file_id, FileId::new(2));
        assert_eq!(errors[3].span().unwrap().start, 5);
    }

    #[test]
    fn duplicate_heavy_work_is_linear_in_input_count() {
        const UNIQUE: usize = 50;
        const REPETITIONS: usize = 200;
        let mut diagnostics = ParserDiagnostics::default();
        for _ in 0..REPETITIONS {
            for start in 0..UNIQUE {
                diagnostics.push(error("same kind", 1, start as u32));
            }
        }

        let (errors, comparisons) = diagnostics.finish();

        assert_eq!(errors.len(), UNIQUE);
        assert_eq!(comparisons, UNIQUE * (REPETITIONS - 1));
    }

    #[test]
    fn earlier_broad_identifier_error_suppresses_only_the_later_narrow_error() {
        let mut diagnostics = ParserDiagnostics::default();
        diagnostics.push(unexpected("identifier or 'drop'", 4));
        diagnostics.push(unexpected("identifier", 4));

        assert!(matches!(
            &diagnostics.last().unwrap().kind,
            ErrorKind::UnexpectedToken { expected, .. } if expected == "identifier"
        ));
        let (errors, _) = diagnostics.finish();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0].kind,
            ErrorKind::UnexpectedToken { expected, .. }
                if expected == "identifier or 'drop'"
        ));
    }

    #[test]
    fn earlier_narrow_identifier_error_is_not_removed_by_a_later_broad_error() {
        let mut diagnostics = ParserDiagnostics::default();
        diagnostics.push(unexpected("identifier", 4));
        diagnostics.push(unexpected("identifier or 'drop'", 4));

        let (errors, _) = diagnostics.finish();
        assert_eq!(errors.len(), 2);
        assert!(matches!(
            &errors[0].kind,
            ErrorKind::UnexpectedToken { expected, .. } if expected == "identifier"
        ));
        assert!(matches!(
            &errors[1].kind,
            ErrorKind::UnexpectedToken { expected, .. }
                if expected == "identifier or 'drop'"
        ));
    }

    #[test]
    fn later_broad_error_does_not_reopen_a_slot_or_move_the_summary_boundary() {
        let mut diagnostics = ParserDiagnostics::default();
        diagnostics.push(unexpected("identifier", 4));
        for start in 1..PARSER_DIAGNOSTIC_BUDGET {
            diagnostics.push(error("detail", 1, 100 + start as u32));
        }
        diagnostics.push(error("first omitted", 1, 10_000));
        diagnostics.push(unexpected("identifier or 'drop'", 4));

        let (errors, _) = diagnostics.finish();
        assert_eq!(errors.len(), PARSER_DIAGNOSTIC_BUDGET + 1);
        assert!(matches!(
            &errors[0].kind,
            ErrorKind::UnexpectedToken { expected, .. } if expected == "identifier"
        ));
        assert_eq!(
            errors.last().unwrap().span().unwrap(),
            Span::with_file(FileId::new(1), 10_000, 10_001)
        );
    }

    #[test]
    fn collector_remains_bounded_and_summary_is_deterministic() {
        let mut diagnostics = ParserDiagnostics::default();
        for start in 0..10_000 {
            diagnostics.push(error("malformed item", 7, start as u32));
            assert!(diagnostics.retained.len() <= PARSER_DIAGNOSTIC_BUDGET);
            assert!(diagnostics.seen.len() <= PARSER_DIAGNOSTIC_BUDGET);
            assert_eq!(
                diagnostics.last().unwrap().span().unwrap().start,
                start as u32
            );
        }

        let (errors, _) = diagnostics.finish();

        assert_eq!(errors.len(), PARSER_DIAGNOSTIC_BUDGET + 1);
        assert_eq!(
            errors[PARSER_DIAGNOSTIC_BUDGET - 1].span().unwrap().start,
            99
        );
        let summary = &errors[PARSER_DIAGNOSTIC_BUDGET];
        assert_eq!(summary.span().unwrap().start, 100);
        assert_eq!(
            summary.kind,
            ErrorKind::ParserDiagnosticsOmitted {
                limit: PARSER_DIAGNOSTIC_BUDGET
            }
        );
        assert_eq!(
            summary.to_string(),
            "additional parser diagnostics omitted after the first 100 errors"
        );
    }
}
