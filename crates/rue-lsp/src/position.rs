use rue_lexer::Span;

/// Converts a character offset to a line and column position
pub struct PositionCalculator<'a> {
    text: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> PositionCalculator<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0];
        for (idx, ch) in text.char_indices() {
            if ch == '\n' {
                line_starts.push(idx + 1);
            }
        }

        Self { text, line_starts }
    }

    /// Convert a character offset to (line, column) where both are 0-indexed
    pub fn offset_to_position(&self, offset: usize) -> (u32, u32) {
        // Binary search to find the line
        let line = match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(line) => line.saturating_sub(1),
        };

        let line_start = self.line_starts[line];
        let column = self.text[line_start..offset].chars().count();

        (line as u32, column as u32)
    }

    /// Convert a span to an LSP Range
    pub fn span_to_range(&self, span: Span) -> tower_lsp::lsp_types::Range {
        let (start_line, start_col) = self.offset_to_position(span.start);
        let (end_line, end_col) = self.offset_to_position(span.end);

        tower_lsp::lsp_types::Range {
            start: tower_lsp::lsp_types::Position {
                line: start_line,
                character: start_col,
            },
            end: tower_lsp::lsp_types::Position {
                line: end_line,
                character: end_col,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_line() {
        let text = "hello world";
        let calc = PositionCalculator::new(text);

        assert_eq!(calc.offset_to_position(0), (0, 0));
        assert_eq!(calc.offset_to_position(6), (0, 6));
        assert_eq!(calc.offset_to_position(11), (0, 11));
    }

    #[test]
    fn test_multiple_lines() {
        let text = "hello\nworld\ntest";
        let calc = PositionCalculator::new(text);

        assert_eq!(calc.offset_to_position(0), (0, 0)); // 'h'
        assert_eq!(calc.offset_to_position(5), (0, 5)); // '\n'
        assert_eq!(calc.offset_to_position(6), (1, 0)); // 'w'
        assert_eq!(calc.offset_to_position(11), (1, 5)); // '\n'
        assert_eq!(calc.offset_to_position(12), (2, 0)); // 't'
    }

    #[test]
    fn test_empty_lines() {
        let text = "hello\n\nworld";
        let calc = PositionCalculator::new(text);

        assert_eq!(calc.offset_to_position(0), (0, 0)); // 'h'
        assert_eq!(calc.offset_to_position(6), (1, 0)); // first '\n'
        assert_eq!(calc.offset_to_position(7), (2, 0)); // 'w'
    }

    #[test]
    fn test_unicode() {
        let text = "hello 🦀\nworld";
        let calc = PositionCalculator::new(text);

        assert_eq!(calc.offset_to_position(0), (0, 0)); // 'h'
        assert_eq!(calc.offset_to_position(6), (0, 6)); // '🦀'
        assert_eq!(calc.offset_to_position(10), (0, 7)); // '\n'
        assert_eq!(calc.offset_to_position(11), (1, 0)); // 'w'
    }
}
