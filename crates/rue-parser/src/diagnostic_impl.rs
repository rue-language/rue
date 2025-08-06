//! Diagnostic trait implementations for parser and lexer errors

use crate::ParseError;
use rue_diagnostic::{Diagnostic, IntoDiagnostic, SourceId};

impl IntoDiagnostic for ParseError {
    fn into_diagnostic(self, source_id: SourceId) -> Diagnostic {
        // Reuse the existing to_diagnostic method
        self.to_diagnostic(source_id)
    }
}
