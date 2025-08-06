//! Rich diagnostic formatter with color and Unicode support

use crate::{
    Diagnostic, Label, LabelStyle, Severity, SourceId, SourceManager, get_line_content,
    span_to_line_col,
};
use rue_lexer::Span;
use std::fmt::{self, Write};

/// Rich diagnostic formatter with color and Unicode support
#[derive(Debug, Clone)]
pub struct DiagnosticFormatter {
    use_colors: bool,
    use_unicode: bool,
}

impl DiagnosticFormatter {
    /// Create a new formatter with color and Unicode support
    pub fn new(use_colors: bool, use_unicode: bool) -> Self {
        Self {
            use_colors,
            use_unicode,
        }
    }

    /// Create a formatter for terminals
    pub fn terminal() -> Self {
        Self::new(true, true)
    }

    /// Create a plain text formatter
    pub fn plain() -> Self {
        Self::new(false, false)
    }

    /// Format a diagnostic with source context
    pub fn format_diagnostic(
        &self,
        diagnostic: &Diagnostic,
        source_manager: &SourceManager,
    ) -> Result<String, fmt::Error> {
        let mut output = String::new();

        // Write header
        self.write_header(&mut output, diagnostic)?;

        // Write source location and context
        if let Some(source) = source_manager.get_source(&diagnostic.source_id) {
            if let Some(primary_label) = diagnostic
                .labels
                .iter()
                .find(|l| l.style == LabelStyle::Primary)
            {
                self.write_location(
                    &mut output,
                    &diagnostic.source_id,
                    primary_label.span,
                    &source.line_starts,
                )?;
                self.write_source_context(&mut output, diagnostic, source_manager)?;
            }
        }

        // Write help text
        if let Some(help) = &diagnostic.help {
            self.write_help(&mut output, help)?;
        }

        // Write related diagnostics
        for related in &diagnostic.related {
            writeln!(output)?;
            writeln!(output, "  {}:", self.colorize("related", Color::Cyan))?;
            let related_output = self.format_diagnostic(related, source_manager)?;
            for line in related_output.lines() {
                writeln!(output, "    {line}")?;
            }
        }

        Ok(output)
    }

    /// Write the diagnostic header (severity, code, message)
    fn write_header(&self, output: &mut String, diagnostic: &Diagnostic) -> fmt::Result {
        let severity_str = match diagnostic.severity {
            Severity::Error => self.colorize("error", Color::Red),
            Severity::Warning => self.colorize("warning", Color::Yellow),
            Severity::Information => self.colorize("info", Color::Blue),
            Severity::Hint => self.colorize("hint", Color::Cyan),
        };

        write!(output, "{severity_str}")?;

        if let Some(code) = &diagnostic.code {
            write!(
                output,
                "[{}]",
                self.colorize(&code.to_string(), Color::BrightBlack)
            )?;
        }

        writeln!(output, ": {}", diagnostic.message)?;
        Ok(())
    }

    /// Write the source location
    fn write_location(
        &self,
        output: &mut String,
        source_id: &SourceId,
        span: Span,
        line_starts: &[usize],
    ) -> fmt::Result {
        let (line, col) = span_to_line_col(line_starts, span.start);
        writeln!(
            output,
            " {} {}:{}:{}",
            self.arrow_symbol(),
            source_id,
            line + 1,
            col + 1
        )?;
        Ok(())
    }

    /// Write source context with labels
    fn write_source_context(
        &self,
        output: &mut String,
        diagnostic: &Diagnostic,
        source_manager: &SourceManager,
    ) -> fmt::Result {
        let source = source_manager
            .get_source(&diagnostic.source_id)
            .ok_or(fmt::Error)?;

        if diagnostic.labels.is_empty() {
            return Ok(());
        }

        // Find the range of lines to display
        let mut min_line = usize::MAX;
        let mut max_line = 0;

        for label in &diagnostic.labels {
            let (start_line, _) = span_to_line_col(&source.line_starts, label.span.start);
            let (end_line, _) =
                span_to_line_col(&source.line_starts, label.span.end.saturating_sub(1));
            min_line = min_line.min(start_line);
            max_line = max_line.max(end_line);
        }

        // Add context lines
        let context_lines = 1;
        let display_start = min_line.saturating_sub(context_lines);
        let display_end = (max_line + context_lines + 1).min(source.line_starts.len());

        let line_num_width = (display_end).to_string().len();

        // Write the source lines with labels
        writeln!(output, "{} |", " ".repeat(line_num_width + 1))?;

        for line_idx in display_start..display_end {
            let line_num = line_idx + 1;

            if let Some(line_content) =
                get_line_content(&source.content, &source.line_starts, line_idx)
            {
                // Check if this line has any labels
                let has_labels = diagnostic.labels.iter().any(|label| {
                    let (start_line, _) = span_to_line_col(&source.line_starts, label.span.start);
                    let (end_line, _) =
                        span_to_line_col(&source.line_starts, label.span.end.saturating_sub(1));
                    line_idx >= start_line && line_idx <= end_line
                });

                // Format line number
                let line_num_str = if has_labels {
                    self.colorize(&format!("{line_num:>line_num_width$}"), Color::Blue)
                } else {
                    format!("{line_num:>line_num_width$}")
                };

                writeln!(output, "{line_num_str} | {line_content}")?;

                // Add underlines for labels on this line
                if has_labels {
                    self.write_label_underlines(
                        output,
                        line_idx,
                        &diagnostic.labels,
                        &source.line_starts,
                        line_num_width,
                    )?;
                }
            }
        }

        writeln!(output, "{} |", " ".repeat(line_num_width + 1))?;
        Ok(())
    }

    /// Write label underlines for a specific line
    fn write_label_underlines(
        &self,
        output: &mut String,
        line_idx: usize,
        labels: &[Label],
        line_starts: &[usize],
        line_num_width: usize,
    ) -> fmt::Result {
        let mut underlines = Vec::new();

        for label in labels {
            let (label_start_line, label_start_col) =
                span_to_line_col(line_starts, label.span.start);
            let (label_end_line, label_end_col) =
                span_to_line_col(line_starts, label.span.end.saturating_sub(1));

            if line_idx >= label_start_line && line_idx <= label_end_line {
                let start_col = if line_idx == label_start_line {
                    label_start_col
                } else {
                    0
                };
                let end_col = if line_idx == label_end_line {
                    label_end_col + 1
                } else {
                    // Extend to end of line
                    line_starts
                        .get(line_idx + 1)
                        .map(|&next| next - line_starts[line_idx] - 1)
                        .unwrap_or(80)
                };

                underlines.push((start_col, end_col, label));
            }
        }

        if !underlines.is_empty() {
            underlines.sort_by_key(|(start, _, _)| *start);

            write!(output, "{} | ", " ".repeat(line_num_width))?;
            let mut pos = 0;

            for (start, end, label) in underlines {
                // Pad to start position
                write!(output, "{}", " ".repeat(start.saturating_sub(pos)))?;

                // Draw underline
                let (underline_char, color) = match label.style {
                    LabelStyle::Primary => ("^", Color::Red),
                    LabelStyle::Secondary => ("-", Color::Blue),
                    LabelStyle::Note => ("~", Color::Green),
                };

                let len = (end - start).max(1);
                write!(
                    output,
                    "{}",
                    self.colorize(&underline_char.repeat(len), color)
                )?;

                // Add label message if present
                if let Some(message) = &label.message {
                    write!(output, " {}", self.colorize(message, color))?;
                }

                pos = end;
            }
            writeln!(output)?;
        }

        Ok(())
    }

    /// Write help text
    fn write_help(&self, output: &mut String, help: &str) -> fmt::Result {
        writeln!(output, " {} {}", self.colorize("help:", Color::Cyan), help)
    }

    /// Apply color to text if colors are enabled
    fn colorize(&self, text: &str, color: Color) -> String {
        if self.use_colors {
            format!("{}{}{}", color.ansi_code(), text, Color::Reset.ansi_code())
        } else {
            text.to_string()
        }
    }

    /// Get the arrow symbol
    fn arrow_symbol(&self) -> &str {
        if self.use_unicode { "→" } else { "->" }
    }
}

impl Default for DiagnosticFormatter {
    fn default() -> Self {
        Self::terminal()
    }
}

/// ANSI color codes
#[derive(Debug, Clone, Copy)]
enum Color {
    Red,
    Yellow,
    Blue,
    Cyan,
    Green,
    BrightBlack,
    Reset,
}

impl Color {
    fn ansi_code(self) -> &'static str {
        match self {
            Color::Red => "\x1b[31m",
            Color::Yellow => "\x1b[33m",
            Color::Blue => "\x1b[34m",
            Color::Cyan => "\x1b[36m",
            Color::Green => "\x1b[32m",
            Color::BrightBlack => "\x1b[90m",
            Color::Reset => "\x1b[0m",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiagnosticBuilder, E2001};

    #[test]
    fn test_plain_formatter() {
        let formatter = DiagnosticFormatter::plain();
        let mut source_manager = SourceManager::new();
        let source_id = source_manager.add_source("test.rue", "fn main() {\n    let x = y;\n}");

        let diagnostic = DiagnosticBuilder::error("undefined variable", source_id.clone())
            .with_code(E2001)
            .with_primary_label(Span::new(20, 21), "not found in this scope")
            .with_help("did you mean `x`?")
            .build();

        let output = formatter
            .format_diagnostic(&diagnostic, &source_manager)
            .unwrap();

        assert!(output.contains("error[E2001]: undefined variable"));
        assert!(output.contains("test.rue:2:9"));
        assert!(output.contains("not found in this scope"));
        assert!(output.contains("help: did you mean `x`?"));
    }

    #[test]
    fn test_multi_span_diagnostic() {
        let formatter = DiagnosticFormatter::plain();
        let mut source_manager = SourceManager::new();
        let source_id = source_manager.add_source(
            "test.rue",
            "fn main() {\n    let x = 5;\n    let y = x + true;\n}",
        );

        let diagnostic =
            DiagnosticBuilder::error("type mismatch in binary operation", source_id.clone())
                .with_code(crate::E3001)
                .with_primary_label(Span::new(35, 43), "cannot add bool to i32")
                .with_secondary_label(Span::new(16, 17), "x is i32 here")
                .with_help("consider using a type cast")
                .build();

        let output = formatter
            .format_diagnostic(&diagnostic, &source_manager)
            .unwrap();

        assert!(output.contains("error[E3001]: type mismatch"));
        assert!(output.contains("cannot add bool to i32"));
        assert!(output.contains("x is i32 here"));
        assert!(output.contains("help: consider using a type cast"));
    }
}
