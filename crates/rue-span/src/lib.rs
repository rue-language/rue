//! Source span and location types for the Rue compiler.
//!
//! This crate provides the fundamental types for tracking source locations
//! throughout the compilation pipeline.

/// A file identifier used to track which source file a span belongs to.
///
/// File IDs are indices into a file table maintained by the compiler.
/// `FileId(0)` is reserved as the default/unknown file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct FileId(pub u32);

impl FileId {
    /// The default file ID, used for single-file compilation or when
    /// the file is unknown.
    pub const DEFAULT: FileId = FileId(0);

    /// Create a new file ID from an index.
    #[inline]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Get the raw index value.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A span representing a range in the source code.
///
/// Spans use byte offsets into the source string and include a file identifier
/// for multi-file compilation. They are designed to be small (12 bytes) and
/// cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Span {
    /// The file this span belongs to
    pub file_id: FileId,
    /// Start byte offset (inclusive)
    pub start: u32,
    /// End byte offset (exclusive)
    pub end: u32,
}

impl Span {
    /// Create a new span from start and end byte offsets.
    ///
    /// Uses the default file ID. For multi-file compilation, use `with_file`.
    #[inline]
    pub const fn new(start: u32, end: u32) -> Self {
        Self {
            file_id: FileId::DEFAULT,
            start,
            end,
        }
    }

    /// Create a new span with a specific file ID.
    #[inline]
    pub const fn with_file(file_id: FileId, start: u32, end: u32) -> Self {
        Self {
            file_id,
            start,
            end,
        }
    }

    /// Create an empty span at a single position.
    ///
    /// Uses the default file ID. For multi-file compilation, use `point_in_file`.
    #[inline]
    pub const fn point(pos: u32) -> Self {
        Self {
            file_id: FileId::DEFAULT,
            start: pos,
            end: pos,
        }
    }

    /// Create an empty span at a single position in a specific file.
    #[inline]
    pub const fn point_in_file(file_id: FileId, pos: u32) -> Self {
        Self {
            file_id,
            start: pos,
            end: pos,
        }
    }

    /// Extend this span to a new end position, preserving the file ID.
    ///
    /// Creates a span from `self.start` to `end` with the same file ID.
    #[inline]
    pub const fn extend_to(&self, end: u32) -> Self {
        Self {
            file_id: self.file_id,
            start: self.start,
            end,
        }
    }

    /// Get the start byte offset.
    #[inline]
    pub const fn start(&self) -> u32 {
        self.start
    }

    /// The length of this span in bytes.
    #[inline]
    pub const fn len(&self) -> u32 {
        self.end - self.start
    }

    /// Whether this span is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Compute the 1-based line number for this span's start position.
    ///
    /// Returns the line number (1-indexed) where this span begins.
    ///
    /// # Panics
    ///
    /// In debug builds, panics if `self.start` exceeds `source.len()`.
    /// In release builds, out-of-bounds offsets are clamped to `source.len()`.
    #[inline]
    pub fn line_number(&self, source: &str) -> usize {
        debug_assert!(
            (self.start as usize) <= source.len(),
            "span start {} exceeds source length {}",
            self.start,
            source.len()
        );
        count_newlines(&source[..(self.start as usize).min(source.len())]) + 1
    }
}

/// Count line terminators in `s`, per the spec's newline classification
/// (2.3:1): LF (`\n`), CR (`\r`), and CRLF (`\r\n`) are each ONE newline. A
/// bare CR — which older code ignored — begins a new line (RUE-534).
#[inline]
fn count_newlines(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => count += 1,
            b'\r' => {
                count += 1;
                // CRLF is a single terminator: skip the LF so it is not
                // counted again.
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    count
}

/// Convert a byte offset to 1-based line and character-column numbers.
///
/// Returns `(line, column)` where both are 1-indexed. The column counts Unicode
/// scalar values, matching diagnostic rendering.
///
/// If `offset` exceeds `source.len()`, the final source position is returned.
#[inline]
pub fn offset_to_line_col(source: &str, offset: u32) -> (u32, u32) {
    let offset = offset as usize;
    let mut line = 1;
    let mut col = 1;
    // Track whether the previous char was a CR so a following LF (CRLF) is not
    // counted as a second line break (RUE-534).
    let mut prev_cr = false;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        match ch {
            '\r' => {
                // CR (and CR of a CRLF) begins a new line, per spec 2.3:1.
                line += 1;
                col = 1;
                prev_cr = true;
                continue;
            }
            '\n' if prev_cr => {
                // The LF of a CRLF: the CR already advanced the line.
                col = 1;
            }
            '\n' => {
                line += 1;
                col = 1;
            }
            _ => col += 1,
        }
        prev_cr = false;
    }
    (line, col)
}

impl From<std::ops::Range<usize>> for Span {
    #[inline]
    fn from(range: std::ops::Range<usize>) -> Self {
        Self {
            file_id: FileId::DEFAULT,
            start: range.start as u32,
            end: range.end as u32,
        }
    }
}

impl From<std::ops::Range<u32>> for Span {
    #[inline]
    fn from(range: std::ops::Range<u32>) -> Self {
        Self {
            file_id: FileId::DEFAULT,
            start: range.start,
            end: range.end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_size() {
        assert_eq!(std::mem::size_of::<Span>(), 12);
    }

    #[test]
    fn test_file_id() {
        assert_eq!(FileId::DEFAULT.index(), 0);
        assert_eq!(FileId::new(42).index(), 42);
    }

    #[test]
    fn test_span_constructors_and_accessors() {
        let span = Span::new(5, 10);
        assert_eq!(span.file_id, FileId::DEFAULT);
        assert_eq!(span.start(), 5);
        assert_eq!(span.len(), 5);
        assert!(!span.is_empty());

        let file = FileId::new(7);
        let with_file = Span::with_file(file, 11, 20);
        assert_eq!(with_file.file_id, file);
        assert_eq!(with_file.start, 11);
        assert_eq!(with_file.end, 20);

        let point = Span::point(3);
        assert_eq!(point, Span::new(3, 3));
        assert!(point.is_empty());

        let point_in_file = Span::point_in_file(file, 4);
        assert_eq!(point_in_file.file_id, file);
        assert_eq!(point_in_file.start, 4);
        assert_eq!(point_in_file.end, 4);

        let extended = with_file.extend_to(30);
        assert_eq!(extended.file_id, file);
        assert_eq!(extended.start, 11);
        assert_eq!(extended.end, 30);
    }

    #[test]
    fn test_span_from_ranges() {
        let span: Span = (5usize..10usize).into();
        assert_eq!(span, Span::new(5, 10));

        let span: Span = (7u32..12u32).into();
        assert_eq!(span, Span::new(7, 12));
    }

    #[test]
    fn test_span_line_number() {
        let source = "let x = 1;\nlet y = 2;\nlet z = 3;";

        assert_eq!(Span::new(0, 10).line_number(source), 1);
        assert_eq!(Span::new(11, 21).line_number(source), 2);
        assert_eq!(Span::new(22, 32).line_number(source), 3);
    }

    #[test]
    fn test_span_line_number_at_newline() {
        let source = "a\nb";
        assert_eq!(Span::new(1, 2).line_number(source), 1);
        assert_eq!(Span::new(2, 3).line_number(source), 2);
    }

    #[test]
    fn test_offset_to_line_col_basic() {
        let source = "line1\nline2\nline3";

        assert_eq!(offset_to_line_col(source, 0), (1, 1));
        assert_eq!(offset_to_line_col(source, 4), (1, 5));
        assert_eq!(offset_to_line_col(source, 5), (1, 6));
        assert_eq!(offset_to_line_col(source, 6), (2, 1));
        assert_eq!(offset_to_line_col(source, 10), (2, 5));
        assert_eq!(offset_to_line_col(source, 12), (3, 1));
        assert_eq!(offset_to_line_col(source, 16), (3, 5));
    }

    #[test]
    fn test_offset_to_line_col_bounds() {
        assert_eq!(offset_to_line_col("", 0), (1, 1));
        assert_eq!(offset_to_line_col("hello", 0), (1, 1));
        assert_eq!(offset_to_line_col("hello", 2), (1, 3));
        assert_eq!(offset_to_line_col("hello", 5), (1, 6));
        assert_eq!(offset_to_line_col("hello", 100), (1, 6));
    }

    #[test]
    fn test_offset_to_line_col_at_newline() {
        let source = "a\nb";
        assert_eq!(offset_to_line_col(source, 0), (1, 1));
        assert_eq!(offset_to_line_col(source, 1), (1, 2));
        assert_eq!(offset_to_line_col(source, 2), (2, 1));
    }

    #[test]
    fn test_offset_to_line_col_counts_chars_not_bytes() {
        let source = "éx\n🙂z";
        assert_eq!(offset_to_line_col(source, 0), (1, 1));
        assert_eq!(offset_to_line_col(source, 2), (1, 2)); // after `é`
        assert_eq!(offset_to_line_col(source, 3), (1, 3)); // after `x`
        assert_eq!(offset_to_line_col(source, 4), (2, 1)); // after newline
        assert_eq!(offset_to_line_col(source, 8), (2, 2)); // after `🙂`
    }

    #[test]
    fn test_line_calc_treats_bare_cr_as_newline() {
        // Spec 2.3:1: CR, LF, and CRLF are each one newline (RUE-534).
        // Bare CR (`a\rb`): the `b` is on line 2.
        let cr = "a\rb";
        assert_eq!(offset_to_line_col(cr, 2), (2, 1));
        assert_eq!(Span::new(2, 3).line_number(cr), 2);
        // CRLF (`a\r\nb`): still ONE newline, `b` on line 2, not line 3.
        let crlf = "a\r\nb";
        assert_eq!(offset_to_line_col(crlf, 3), (2, 1));
        assert_eq!(Span::new(3, 4).line_number(crlf), 2);
        // LF unchanged.
        let lf = "a\nb";
        assert_eq!(offset_to_line_col(lf, 2), (2, 1));
        assert_eq!(Span::new(2, 3).line_number(lf), 2);
        // Mixed: two CR-only lines then the target on line 3.
        let mixed = "l1\rl2\rx";
        assert_eq!(offset_to_line_col(mixed, 6), (3, 1));
        assert_eq!(Span::new(6, 7).line_number(mixed), 3);
    }
}
