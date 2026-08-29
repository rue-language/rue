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
}

/// A reusable source coordinate index.
///
/// The index records the logical start and content bounds of every source
/// line once. Queries use binary search for the line and scan only that line's
/// content for its Unicode-scalar column, so consumers do not rescan the
/// source prefix for every diagnostic.
/// LF, CR, and CRLF are each one line terminator (spec 2.3:1). For CRLF, the
/// logical next-line start is the byte after CR: this preserves the historical
/// coordinate behavior for offsets on the LF byte while the column start is
/// the byte after the complete terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex<'a> {
    source: &'a str,
    source_len: usize,
    lines: Vec<LineEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LineEntry {
    logical_start: usize,
    content_start: usize,
    content_end: usize,
}

impl<'a> LineIndex<'a> {
    /// Build an index for `source`.
    pub fn new(source: &'a str) -> Self {
        let bytes = source.as_bytes();
        let mut lines = vec![LineEntry {
            logical_start: 0,
            content_start: 0,
            content_end: source.len(),
        }];

        for (offset, ch) in source.char_indices() {
            match ch {
                '\r' => {
                    let crlf = bytes.get(offset + 1) == Some(&b'\n');
                    lines
                        .last_mut()
                        .expect("line index always has a first line")
                        .content_end = offset;
                    lines.push(LineEntry {
                        logical_start: offset + 1,
                        content_start: offset + 1 + usize::from(crlf),
                        content_end: source.len(),
                    });
                }
                '\n' if offset > 0 && bytes[offset - 1] == b'\r' => {}
                '\n' => {
                    lines
                        .last_mut()
                        .expect("line index always has a first line")
                        .content_end = offset;
                    lines.push(LineEntry {
                        logical_start: offset + 1,
                        content_start: offset + 1,
                        content_end: source.len(),
                    });
                }
                _ => {}
            }
        }

        Self {
            source,
            source_len: source.len(),
            lines,
        }
    }

    /// Return the 1-based line number for a byte offset, clamped to the source.
    #[inline]
    pub fn line_number(&self, offset: u32) -> u32 {
        (self.line_index(offset as usize).saturating_add(1)) as u32
    }

    /// Return 1-based line and Unicode-scalar column numbers for a byte offset.
    ///
    /// Offsets inside a UTF-8 scalar resolve to the scalar's following column;
    /// offsets at scalar boundaries resolve to that scalar's column, matching
    /// the source-coordinate behavior used by diagnostics.
    /// Out-of-bounds offsets resolve to the final source position.
    #[inline]
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let offset = (offset as usize).min(self.source_len);
        let line_idx = self.line_index(offset);
        let line = &self.lines[line_idx];
        let coordinate_offset = offset.clamp(line.content_start, line.content_end);
        // `content_start` is always a valid UTF-8 boundary. Keep the slice at
        // that boundary and stop by absolute byte offset, so an offset inside
        // a scalar is never used as a slicing boundary.
        let scalar_count = self.source[line.content_start..]
            .char_indices()
            .take_while(|(start, _)| line.content_start + *start < coordinate_offset)
            .count();
        (
            (line_idx as u32).saturating_add(1),
            (scalar_count as u32).saturating_add(1),
        )
    }

    #[inline]
    fn line_index(&self, offset: usize) -> usize {
        self.lines
            .partition_point(|line| line.logical_start <= offset)
            .saturating_sub(1)
    }

    /// Return the original source content for a 1-based line number.
    ///
    /// The returned slice excludes its LF, CR, or CRLF terminator. Bounds are
    /// indexed once during construction and are therefore always valid UTF-8
    /// boundaries.
    pub fn line_content(&self, line_number: u32) -> Option<&'a str> {
        let line = self.lines.get(line_number.checked_sub(1)? as usize)?;
        Some(&self.source[line.content_start..line.content_end])
    }
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

    fn historical_offset_to_line_col(source: &str, offset: u32) -> (u32, u32) {
        let offset = offset as usize;
        let mut line = 1;
        let mut col = 1;
        let mut prev_cr = false;
        for (index, ch) in source.char_indices() {
            if index >= offset {
                break;
            }
            match ch {
                '\r' => {
                    line += 1;
                    col = 1;
                    prev_cr = true;
                    continue;
                }
                '\n' if prev_cr => col = 1,
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
    fn test_line_index_line_number() {
        let source = "let x = 1;\nlet y = 2;\nlet z = 3;";
        let index = LineIndex::new(source);

        assert_eq!(index.line_number(0), 1);
        assert_eq!(index.line_number(11), 2);
        assert_eq!(index.line_number(22), 3);
    }

    #[test]
    fn test_span_line_number_at_newline() {
        let source = "a\nb";
        let index = LineIndex::new(source);
        assert_eq!(index.line_number(1), 1);
        assert_eq!(index.line_number(2), 2);
    }

    #[test]
    fn test_line_index_basic() {
        let source = "line1\nline2\nline3";
        let index = LineIndex::new(source);

        assert_eq!(index.line_col(0), (1, 1));
        assert_eq!(index.line_col(4), (1, 5));
        assert_eq!(index.line_col(5), (1, 6));
        assert_eq!(index.line_col(6), (2, 1));
        assert_eq!(index.line_col(10), (2, 5));
        assert_eq!(index.line_col(12), (3, 1));
        assert_eq!(index.line_col(16), (3, 5));
    }

    #[test]
    fn test_line_index_bounds() {
        assert_eq!(LineIndex::new("").line_col(0), (1, 1));
        assert_eq!(LineIndex::new("hello").line_col(0), (1, 1));
        assert_eq!(LineIndex::new("hello").line_col(2), (1, 3));
        assert_eq!(LineIndex::new("hello").line_col(5), (1, 6));
        assert_eq!(LineIndex::new("hello").line_col(100), (1, 6));
    }

    #[test]
    fn test_line_index_at_newline() {
        let source = "a\nb";
        let index = LineIndex::new(source);
        assert_eq!(index.line_col(0), (1, 1));
        assert_eq!(index.line_col(1), (1, 2));
        assert_eq!(index.line_col(2), (2, 1));
    }

    #[test]
    fn test_line_index_counts_chars_not_bytes() {
        let source = "éx\n🙂z";
        let index = LineIndex::new(source);
        assert_eq!(index.line_col(0), (1, 1));
        assert_eq!(index.line_col(1), (1, 2)); // inside `é`
        assert_eq!(index.line_col(2), (1, 2)); // after `é`
        assert_eq!(index.line_col(3), (1, 3)); // after `x`
        assert_eq!(index.line_col(4), (2, 1)); // after newline
        assert_eq!(index.line_col(5), (2, 2)); // inside `🙂`
        assert_eq!(index.line_col(8), (2, 2)); // after `🙂`
    }

    #[test]
    fn test_line_calc_treats_bare_cr_as_newline() {
        // Spec 2.3:1: CR, LF, and CRLF are each one newline (RUE-534).
        // Bare CR (`a\rb`): the `b` is on line 2.
        let cr = "a\rb";
        let cr_index = LineIndex::new(cr);
        assert_eq!(cr_index.line_col(2), (2, 1));
        assert_eq!(cr_index.line_number(2), 2);
        // CRLF (`a\r\nb`): still ONE newline, `b` on line 2, not line 3.
        let crlf = "a\r\nb";
        let crlf_index = LineIndex::new(crlf);
        assert_eq!(crlf_index.line_col(1), (1, 2)); // CR
        assert_eq!(crlf_index.line_col(2), (2, 1)); // LF
        assert_eq!(crlf_index.line_col(3), (2, 1)); // after terminator
        assert_eq!(crlf_index.line_number(3), 2);
        // LF unchanged.
        let lf = "a\nb";
        let lf_index = LineIndex::new(lf);
        assert_eq!(lf_index.line_col(2), (2, 1));
        assert_eq!(lf_index.line_number(2), 2);
        // Mixed: two CR-only lines then the target on line 3.
        let mixed = "l1\rl2\rx";
        let mixed_index = LineIndex::new(mixed);
        assert_eq!(mixed_index.line_col(6), (3, 1));
        assert_eq!(mixed_index.line_number(6), 3);
    }

    #[test]
    fn test_line_index_mixed_newlines_and_clamped_offsets() {
        let source = "a\r\nb\rc\n終\n";
        let index = LineIndex::new(source);

        assert_eq!(index.line_content(1), Some("a"));
        assert_eq!(index.line_content(2), Some("b"));
        assert_eq!(index.line_content(3), Some("c"));
        assert_eq!(index.line_content(4), Some("終"));
        assert_eq!(index.line_content(5), Some(""));
        assert_eq!(index.line_content(0), None);
        assert_eq!(index.line_content(6), None);
        assert_eq!(index.line_col(1), (1, 2)); // CR
        assert_eq!(index.line_col(2), (2, 1)); // LF in CRLF
        assert_eq!(index.line_col(4), (2, 2)); // bare CR
        assert_eq!(index.line_col(5), (3, 1));
        assert_eq!(index.line_col(7), (4, 1)); // first byte of `終`
        assert_eq!(index.line_col(8), (4, 2)); // inside `終`
        assert_eq!(index.line_col(10), (4, 2)); // LF after `終`
        assert_eq!(index.line_col(source.len() as u32), (5, 1)); // EOF
        assert_eq!(index.line_col(u32::MAX), (5, 1)); // out of bounds
    }

    #[test]
    fn line_index_matches_historical_coordinates_at_every_byte_offset() {
        for source in ["", "a", "\n", "\r", "\r\n", "a\r\nb\rc\n", "é\t終\r\n🙂\n"] {
            let index = LineIndex::new(source);
            for offset in 0..=(source.len() as u32 + 2) {
                assert_eq!(
                    index.line_col(offset),
                    historical_offset_to_line_col(source, offset),
                    "source={source:?}, offset={offset}"
                );
            }
            assert_eq!(
                index.line_col(u32::MAX),
                historical_offset_to_line_col(source, u32::MAX),
                "source={source:?}, offset=u32::MAX"
            );
        }
    }
}
