//! Parser-independent diagnostic postprocessing.
//!
//! Parser adapters construct [`CompileError`] values, then apply this policy
//! before exposing them. The first diagnostic determines ordering; only equal
//! error kinds at the same complete primary span are duplicates.

use rue_error::{CompileError, ErrorKind};
use rue_span::Span;

/// Drop diagnostics with the same kind and primary span, preserving first-seen
/// order. Distinct diagnostics at one location and identical diagnostics in
/// different files remain visible.
pub(crate) fn dedupe_parse_errors(errors: &mut Vec<CompileError>) {
    // Parse error lists are tiny, so a linear seen set avoids hashing overhead.
    let mut seen: Vec<(Option<Span>, ErrorKind)> = Vec::new();
    errors.retain(|error| {
        let key = (error.span(), error.kind.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_span::FileId;

    fn error(message: &str, file: u32, start: u32) -> CompileError {
        CompileError::new(
            ErrorKind::ParseError(message.to_string()),
            Span::with_file(FileId::new(file), start, start + 1),
        )
    }

    #[test]
    fn dedupe_preserves_order_and_distinctions() {
        let mut errors = vec![
            error("first", 1, 4),
            error("first", 1, 4),
            error("second", 1, 4),
            error("first", 2, 4),
            error("first", 1, 5),
        ];

        dedupe_parse_errors(&mut errors);

        assert_eq!(errors.len(), 4);
        assert_eq!(errors[0].to_string(), "first");
        assert_eq!(errors[1].to_string(), "second");
        assert_eq!(errors[2].span().unwrap().file_id, FileId::new(2));
        assert_eq!(errors[3].span().unwrap().start, 5);
    }
}
