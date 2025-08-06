//! Fluent builder for constructing diagnostics

use crate::{Diagnostic, DiagnosticCode, Label, LabelStyle, Severity, SourceId};
use rue_lexer::Span;

/// Fluent builder for constructing diagnostics
pub struct DiagnosticBuilder {
    diagnostic: Diagnostic,
}

impl DiagnosticBuilder {
    /// Create a new diagnostic builder with the given severity
    pub fn new(severity: Severity, message: impl Into<String>, source_id: SourceId) -> Self {
        Self {
            diagnostic: Diagnostic {
                severity,
                code: None,
                message: message.into(),
                labels: Vec::new(),
                help: None,
                related: Vec::new(),
                source_id,
            },
        }
    }

    /// Create an error diagnostic builder
    pub fn error(message: impl Into<String>, source_id: SourceId) -> Self {
        Self::new(Severity::Error, message, source_id)
    }

    /// Create a warning diagnostic builder
    pub fn warning(message: impl Into<String>, source_id: SourceId) -> Self {
        Self::new(Severity::Warning, message, source_id)
    }

    /// Create an information diagnostic builder
    pub fn info(message: impl Into<String>, source_id: SourceId) -> Self {
        Self::new(Severity::Information, message, source_id)
    }

    /// Create a hint diagnostic builder
    pub fn hint(message: impl Into<String>, source_id: SourceId) -> Self {
        Self::new(Severity::Hint, message, source_id)
    }

    /// Set the diagnostic code
    pub fn with_code(mut self, code: DiagnosticCode) -> Self {
        self.diagnostic.code = Some(code);
        self
    }

    /// Add a primary label with a message
    pub fn with_primary_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.diagnostic.labels.push(Label {
            span,
            message: Some(message.into()),
            style: LabelStyle::Primary,
        });
        self
    }

    /// Add a primary label without a message
    pub fn with_primary_span(mut self, span: Span) -> Self {
        self.diagnostic.labels.push(Label {
            span,
            message: None,
            style: LabelStyle::Primary,
        });
        self
    }

    /// Add a secondary label with a message
    pub fn with_secondary_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.diagnostic.labels.push(Label {
            span,
            message: Some(message.into()),
            style: LabelStyle::Secondary,
        });
        self
    }

    /// Add a secondary label without a message
    pub fn with_secondary_span(mut self, span: Span) -> Self {
        self.diagnostic.labels.push(Label {
            span,
            message: None,
            style: LabelStyle::Secondary,
        });
        self
    }

    /// Add a note label with a message
    pub fn with_note_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.diagnostic.labels.push(Label {
            span,
            message: Some(message.into()),
            style: LabelStyle::Note,
        });
        self
    }

    /// Add a note label without a message
    pub fn with_note_span(mut self, span: Span) -> Self {
        self.diagnostic.labels.push(Label {
            span,
            message: None,
            style: LabelStyle::Note,
        });
        self
    }

    /// Add a label
    pub fn with_label(mut self, label: Label) -> Self {
        self.diagnostic.labels.push(label);
        self
    }

    /// Add multiple labels
    pub fn with_labels(mut self, labels: impl IntoIterator<Item = Label>) -> Self {
        self.diagnostic.labels.extend(labels);
        self
    }

    /// Set the help text
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.diagnostic.help = Some(help.into());
        self
    }

    /// Add a related diagnostic
    pub fn with_related(mut self, related: Diagnostic) -> Self {
        self.diagnostic.related.push(Box::new(related));
        self
    }

    /// Add multiple related diagnostics
    pub fn with_related_diagnostics(
        mut self,
        related: impl IntoIterator<Item = Diagnostic>,
    ) -> Self {
        self.diagnostic
            .related
            .extend(related.into_iter().map(Box::new));
        self
    }

    /// Build the diagnostic
    pub fn build(self) -> Diagnostic {
        self.diagnostic
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{E2001, E3001};

    #[test]
    fn test_builder_chaining() {
        let source_id = SourceId::new("test.rue");
        let diagnostic = DiagnosticBuilder::error("type mismatch", source_id.clone())
            .with_code(E3001)
            .with_primary_label(Span::new(10, 15), "expected i32")
            .with_secondary_label(Span::new(20, 25), "found bool")
            .with_help("consider using a cast")
            .build();

        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.code, Some(E3001));
        assert_eq!(diagnostic.labels.len(), 2);
        assert_eq!(diagnostic.help.as_deref(), Some("consider using a cast"));
    }

    #[test]
    fn test_related_diagnostics() {
        let source_id = SourceId::new("test.rue");
        let related = DiagnosticBuilder::info("variable defined here", source_id.clone())
            .with_primary_span(Span::new(5, 10))
            .build();

        let main = DiagnosticBuilder::error("undefined variable", source_id.clone())
            .with_code(E2001)
            .with_primary_label(Span::new(20, 25), "not found")
            .with_related(related)
            .build();

        assert_eq!(main.related.len(), 1);
        assert_eq!(main.related[0].severity, Severity::Information);
    }
}
