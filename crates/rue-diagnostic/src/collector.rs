//! Multi-error collector for batch processing

use crate::{Diagnostic, Severity};

/// Multi-error collector for batch processing
#[derive(Debug, Default, Clone)]
pub struct DiagnosticCollector {
    diagnostics: Vec<Diagnostic>,
    max_errors: usize,
    error_count: usize,
}

impl DiagnosticCollector {
    /// Create a new diagnostic collector with the specified maximum error count
    pub fn new(max_errors: usize) -> Self {
        Self {
            diagnostics: Vec::new(),
            max_errors,
            error_count: 0,
        }
    }

    /// Create a diagnostic collector with unlimited errors
    pub fn unlimited() -> Self {
        Self::new(usize::MAX)
    }

    /// Add a diagnostic to the collector
    /// Returns false if the maximum error count has been reached
    pub fn add(&mut self, diagnostic: Diagnostic) -> bool {
        if diagnostic.is_error() {
            self.error_count += 1;
        }
        self.diagnostics.push(diagnostic);
        self.error_count < self.max_errors
    }

    /// Push a diagnostic without checking the error limit
    pub fn push(&mut self, diagnostic: Diagnostic) {
        if diagnostic.is_error() {
            self.error_count += 1;
        }
        self.diagnostics.push(diagnostic);
    }

    /// Check if the collector has any errors
    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    /// Check if the collector has any warnings
    pub fn has_warnings(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Warning)
    }

    /// Check if the maximum error count has been reached
    pub fn is_full(&self) -> bool {
        self.error_count >= self.max_errors
    }

    /// Check if too many errors have been collected
    pub fn has_too_many_errors(&self) -> bool {
        self.max_errors != usize::MAX && self.error_count > self.max_errors
    }

    /// Get the number of errors collected
    pub fn error_count(&self) -> usize {
        self.error_count
    }

    /// Get the total number of diagnostics
    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    /// Check if the collector is empty
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// Get a reference to the diagnostics
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Get a mutable reference to the diagnostics
    pub fn diagnostics_mut(&mut self) -> &mut Vec<Diagnostic> {
        &mut self.diagnostics
    }

    /// Take ownership of the diagnostics
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    /// Clear all diagnostics
    pub fn clear(&mut self) {
        self.diagnostics.clear();
        self.error_count = 0;
    }

    /// Sort diagnostics by their primary span
    pub fn sort_by_span(&mut self) {
        self.diagnostics
            .sort_by_key(|d| d.primary_span().map(|s| s.start));
    }

    /// Deduplicate similar errors to reduce noise
    pub fn deduplicate(&mut self) {
        // We need to be careful here - we only want to deduplicate truly identical errors
        // For now, we'll use a simple approach based on code, message, and primary span
        self.diagnostics.dedup_by(|a, b| {
            a.code == b.code
                && a.message == b.message
                && a.severity == b.severity
                && a.primary_span() == b.primary_span()
        });

        // Recount errors after deduplication
        self.error_count = self.diagnostics.iter().filter(|d| d.is_error()).count();
    }

    /// Filter diagnostics by severity
    pub fn filter_by_severity(&mut self, severity: Severity) {
        self.diagnostics.retain(|d| d.severity == severity);
        if severity == Severity::Error {
            self.error_count = self.diagnostics.len();
        } else if self.error_count > 0 {
            self.error_count = self.diagnostics.iter().filter(|d| d.is_error()).count();
        }
    }

    /// Get only error diagnostics
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.is_error())
    }

    /// Get only warning diagnostics
    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
    }

    /// Merge another collector into this one
    pub fn extend(&mut self, other: DiagnosticCollector) {
        let additional_errors = other.diagnostics.iter().filter(|d| d.is_error()).count();
        self.error_count += additional_errors;
        self.diagnostics.extend(other.diagnostics);
    }

    /// Create an iterator over the diagnostics
    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter()
    }
}

impl IntoIterator for DiagnosticCollector {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.diagnostics.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{E2001, SourceId};
    use rue_lexer::Span;

    #[test]
    fn test_collector_limits() {
        let mut collector = DiagnosticCollector::new(2);
        let source_id = SourceId::new("test.rue");

        let d1 = Diagnostic::error("error 1", source_id.clone()).build();
        let d2 = Diagnostic::error("error 2", source_id.clone()).build();
        let d3 = Diagnostic::error("error 3", source_id.clone()).build();

        assert!(collector.add(d1));
        assert!(!collector.add(d2)); // Should return false as we hit the limit
        collector.push(d3); // But we can still push

        assert_eq!(collector.error_count(), 3);
        assert!(collector.is_full());
    }

    #[test]
    fn test_collector_deduplication() {
        let mut collector = DiagnosticCollector::unlimited();
        let source_id = SourceId::new("test.rue");
        let span = Span::new(10, 15);

        // Add duplicate diagnostics
        for _ in 0..3 {
            let d = Diagnostic::error("undefined variable", source_id.clone())
                .with_code(E2001)
                .with_primary_span(span)
                .build();
            collector.push(d);
        }

        assert_eq!(collector.len(), 3);
        collector.deduplicate();
        assert_eq!(collector.len(), 1);
    }

    #[test]
    fn test_collector_filtering() {
        let mut collector = DiagnosticCollector::unlimited();
        let source_id = SourceId::new("test.rue");

        collector.push(Diagnostic::error("error", source_id.clone()).build());
        collector.push(Diagnostic::warning("warning", source_id.clone()).build());
        collector.push(Diagnostic::info("info", source_id.clone()).build());

        assert_eq!(collector.len(), 3);
        assert_eq!(collector.errors().count(), 1);
        assert_eq!(collector.warnings().count(), 1);

        collector.filter_by_severity(Severity::Error);
        assert_eq!(collector.len(), 1);
    }
}
