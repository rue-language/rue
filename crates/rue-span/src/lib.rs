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

    /// Create a span covering two spans (from start of first to end of second).
    ///
    /// Uses the file ID from span `a`. Both spans should be from the same file.
    #[inline]
    pub const fn cover(a: Span, b: Span) -> Self {
        Self {
            file_id: a.file_id,
            start: if a.start < b.start { a.start } else { b.start },
            end: if a.end > b.end { a.end } else { b.end },
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

    /// Returns `true` if `other` is entirely contained within this span.
    ///
    /// A span `a` contains span `b` if `a.start <= b.start` and `b.end <= a.end`.
    /// An empty span at a boundary is considered contained.
    ///
    /// # Example
    ///
    /// ```
    /// use rue_span::Span;
    ///
    /// let outer = Span::new(5, 20);
    /// let inner = Span::new(10, 15);
    /// let overlapping = Span::new(15, 25);
    ///
    /// assert!(outer.contains(inner));
    /// assert!(!outer.contains(overlapping));
    /// assert!(outer.contains(Span::point(10)));
    /// ```
    #[inline]
    pub const fn contains(&self, other: Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// Returns `true` if this span contains the given byte position.
    ///
    /// The position is contained if `self.start <= pos < self.end`.
    /// Note: the end position is exclusive, so `pos == self.end` returns `false`.
    ///
    /// # Example
    ///
    /// ```
    /// use rue_span::Span;
    ///
    /// let span = Span::new(5, 10);
    /// assert!(!span.contains_pos(4));  // before span
    /// assert!(span.contains_pos(5));   // at start (inclusive)
    /// assert!(span.contains_pos(7));   // in middle
    /// assert!(!span.contains_pos(10)); // at end (exclusive)
    /// ```
    #[inline]
    pub const fn contains_pos(&self, pos: u32) -> bool {
        self.start <= pos && pos < self.end
    }

    /// Convert to a Range<usize> for slicing.
    #[inline]
    pub const fn as_range(&self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
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
        byte_offset_to_line(source, self.start as usize)
    }

    /// Compute the 1-based line and column numbers for this span's start position.
    ///
    /// Returns `(line, column)` where both are 1-indexed. The column is the
    /// number of bytes from the start of the line, plus 1.
    ///
    /// # Panics
    ///
    /// In debug builds, panics if `self.start` exceeds `source.len()`.
    /// In release builds, out-of-bounds offsets are clamped to `source.len()`.
    #[inline]
    pub fn line_col(&self, source: &str) -> (usize, usize) {
        debug_assert!(
            (self.start as usize) <= source.len(),
            "span start {} exceeds source length {}",
            self.start,
            source.len()
        );
        byte_offset_to_line_col(source, self.start as usize)
    }
}

/// Convert a byte offset to a 1-based line number.
///
/// Counts the number of newlines before the given byte offset and adds 1.
/// If `offset` exceeds `source.len()`, it is clamped to `source.len()`.
///
/// **Performance note**: This function is O(n) in the source length.
/// For repeated lookups on the same source, use [`LineIndex`] for O(log n) lookups.
#[inline]
pub fn byte_offset_to_line(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1
}

/// Convert a byte offset to 1-based line and column numbers.
///
/// Returns `(line, column)` where both are 1-indexed.
/// The column is the number of bytes from the start of the line, plus 1.
/// If `offset` exceeds `source.len()`, it is clamped to `source.len()`.
///
/// **Performance note**: This function is O(n) in the source length.
/// For repeated lookups on the same source, use [`LineIndex`] for O(log n) lookups.
#[inline]
pub fn byte_offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let prefix = &source[..offset];

    // Find the last newline before offset
    match prefix.rfind('\n') {
        Some(newline_pos) => {
            let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
            let col = offset - newline_pos; // offset - newline_pos gives bytes after newline
            (line, col)
        }
        None => {
            // No newline before offset, so we're on line 1
            (1, offset + 1)
        }
    }
}

/// Precomputed line offset index for efficient byte offset to line number conversion.
///
/// Building the index is O(n) in source length, but subsequent lookups are O(log n).
/// Use this when you need to perform multiple line number lookups on the same source.
///
/// # Example
///
/// ```
/// use rue_span::LineIndex;
///
/// let source = "line1\nline2\nline3";
/// let index = LineIndex::new(source);
///
/// assert_eq!(index.line_number(0), 1);  // Start of line 1
/// assert_eq!(index.line_number(6), 2);  // Start of line 2
/// assert_eq!(index.line_number(12), 3); // Start of line 3
/// ```
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offsets where each line starts. line_starts[0] is always 0.
    /// line_starts[i] is the byte offset of the start of line i+1 (1-indexed line number).
    line_starts: Vec<u32>,
    /// Total length of the source in bytes.
    source_len: u32,
}

impl LineIndex {
    /// Build a line index from source text.
    ///
    /// This scans the entire source once to find all newline positions.
    /// Time complexity: O(n) where n is the source length.
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        Self {
            line_starts,
            source_len: source.len() as u32,
        }
    }

    /// Get the 1-based line number for a byte offset.
    ///
    /// Time complexity: O(log n) where n is the number of lines.
    ///
    /// # Panics
    ///
    /// In debug builds, panics if `offset` exceeds the source length.
    /// In release builds, out-of-bounds offsets are clamped to the source length.
    #[inline]
    pub fn line_number(&self, offset: u32) -> usize {
        debug_assert!(
            offset <= self.source_len,
            "offset {} exceeds source length {}",
            offset,
            self.source_len
        );
        let offset = offset.min(self.source_len);

        // Binary search for the line containing this offset.
        // We want the largest line_start <= offset, which is partition_point - 1.
        let line_idx = self.line_starts.partition_point(|&start| start <= offset);
        // partition_point returns the first index where the predicate is false,
        // so line_idx - 1 is the line containing offset (but line_idx is already 1-indexed)
        line_idx
    }

    /// Get the 1-based line number for a span's start position.
    #[inline]
    pub fn span_line_number(&self, span: Span) -> usize {
        self.line_number(span.start)
    }

    /// Get the 1-based line and column numbers for a byte offset.
    ///
    /// Returns `(line, column)` where both are 1-indexed. The column is the
    /// number of bytes from the start of the line, plus 1.
    ///
    /// Time complexity: O(log n) where n is the number of lines.
    ///
    /// # Panics
    ///
    /// In debug builds, panics if `offset` exceeds the source length.
    /// In release builds, out-of-bounds offsets are clamped to the source length.
    #[inline]
    pub fn line_col(&self, offset: u32) -> (usize, usize) {
        debug_assert!(
            offset <= self.source_len,
            "offset {} exceeds source length {}",
            offset,
            self.source_len
        );
        let offset = offset.min(self.source_len);

        // Binary search for the line containing this offset.
        let line_idx = self.line_starts.partition_point(|&start| start <= offset);
        // line_idx is 1-indexed (partition_point returns first index where predicate is false)
        let line_start = self.line_starts[line_idx - 1];
        let col = (offset - line_start) as usize + 1;
        (line_idx, col)
    }

    /// Get the 1-based line and column numbers for a span's start position.
    ///
    /// Returns `(line, column)` where both are 1-indexed.
    #[inline]
    pub fn span_line_col(&self, span: Span) -> (usize, usize) {
        self.line_col(span.start)
    }

    /// Returns the number of lines in the source.
    #[inline]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
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

    #[test]
    fn test_span_size() {
        // Ensure Span stays small (12 bytes with file_id)
        assert_eq!(std::mem::size_of::<Span>(), 12);
    }

    #[test]
    fn test_file_id() {
        assert_eq!(FileId::DEFAULT.index(), 0);
        assert_eq!(FileId::new(42).index(), 42);
    }

    #[test]
    fn test_span_with_file() {
        let file = FileId::new(5);
        let span = Span::with_file(file, 10, 20);
        assert_eq!(span.file_id, file);
        assert_eq!(span.start, 10);
        assert_eq!(span.end, 20);
    }

    #[test]
    fn test_span_point_in_file() {
        let file = FileId::new(3);
        let span = Span::point_in_file(file, 15);
        assert_eq!(span.file_id, file);
        assert_eq!(span.start, 15);
        assert_eq!(span.end, 15);
    }

    #[test]
    fn test_span_cover_preserves_file_id() {
        let file = FileId::new(7);
        let a = Span::with_file(file, 5, 10);
        let b = Span::with_file(file, 15, 20);
        let covered = Span::cover(a, b);
        assert_eq!(covered.file_id, file);
        assert_eq!(covered.start, 5);
        assert_eq!(covered.end, 20);
    }

    #[test]
    fn test_span_cover() {
        let a = Span::new(5, 10);
        let b = Span::new(15, 20);
        let covered = Span::cover(a, b);
        assert_eq!(covered, Span::new(5, 20));
    }

    #[test]
    fn test_span_from_range() {
        let span: Span = (5usize..10usize).into();
        assert_eq!(span.start, 5);
        assert_eq!(span.end, 10);
    }

    #[test]
    fn test_byte_offset_to_line() {
        let source = "line1\nline2\nline3";
        // First line (offset 0-4)
        assert_eq!(byte_offset_to_line(source, 0), 1);
        assert_eq!(byte_offset_to_line(source, 4), 1);
        // Second line (offset 6-10)
        assert_eq!(byte_offset_to_line(source, 6), 2);
        assert_eq!(byte_offset_to_line(source, 10), 2);
        // Third line (offset 12+)
        assert_eq!(byte_offset_to_line(source, 12), 3);
        assert_eq!(byte_offset_to_line(source, 16), 3);
    }

    #[test]
    fn test_span_line_number() {
        let source = "let x = 1;\nlet y = 2;\nlet z = 3;";
        // Span on line 1
        let span1 = Span::new(0, 10);
        assert_eq!(span1.line_number(source), 1);
        // Span on line 2
        let span2 = Span::new(11, 21);
        assert_eq!(span2.line_number(source), 2);
        // Span on line 3
        let span3 = Span::new(22, 32);
        assert_eq!(span3.line_number(source), 3);
    }

    #[test]
    fn test_byte_offset_to_line_at_bounds() {
        let source = "hello";
        // At exactly the end of source
        assert_eq!(byte_offset_to_line(source, 5), 1);
        // Beyond source bounds - should clamp to source length
        assert_eq!(byte_offset_to_line(source, 100), 1);
    }

    #[test]
    fn test_byte_offset_to_line_empty_source() {
        let source = "";
        // Empty source should return line 1
        assert_eq!(byte_offset_to_line(source, 0), 1);
    }

    #[test]
    fn test_span_line_number_at_newline() {
        let source = "a\nb";
        // Span at the newline character itself
        let span = Span::new(1, 2);
        assert_eq!(span.line_number(source), 1);
        // Span right after the newline
        let span2 = Span::new(2, 3);
        assert_eq!(span2.line_number(source), 2);
    }

    // ========================================================================
    // LineIndex tests
    // ========================================================================

    #[test]
    fn test_line_index_basic() {
        let source = "line1\nline2\nline3";
        let index = LineIndex::new(source);

        // First line (offset 0-4)
        assert_eq!(index.line_number(0), 1);
        assert_eq!(index.line_number(4), 1);

        // Second line (offset 6-10)
        assert_eq!(index.line_number(6), 2);
        assert_eq!(index.line_number(10), 2);

        // Third line (offset 12+)
        assert_eq!(index.line_number(12), 3);
        assert_eq!(index.line_number(16), 3);
    }

    #[test]
    fn test_line_index_matches_byte_offset_to_line() {
        let source = "let x = 1;\nlet y = 2;\nlet z = 3;";
        let index = LineIndex::new(source);

        // Verify LineIndex matches byte_offset_to_line for all offsets
        for offset in 0..=source.len() {
            assert_eq!(
                index.line_number(offset as u32),
                byte_offset_to_line(source, offset),
                "mismatch at offset {}",
                offset
            );
        }
    }

    #[test]
    fn test_line_index_empty_source() {
        let source = "";
        let index = LineIndex::new(source);
        assert_eq!(index.line_number(0), 1);
        assert_eq!(index.line_count(), 1);
    }

    #[test]
    fn test_line_index_single_line() {
        let source = "hello";
        let index = LineIndex::new(source);
        assert_eq!(index.line_number(0), 1);
        assert_eq!(index.line_number(4), 1);
        assert_eq!(index.line_count(), 1);
    }

    #[test]
    fn test_line_index_at_newline() {
        let source = "a\nb";
        let index = LineIndex::new(source);
        // At the newline character itself (offset 1)
        assert_eq!(index.line_number(1), 1);
        // Right after the newline (offset 2)
        assert_eq!(index.line_number(2), 2);
    }

    #[test]
    fn test_line_index_trailing_newline() {
        let source = "line1\n";
        let index = LineIndex::new(source);
        assert_eq!(index.line_number(0), 1);
        assert_eq!(index.line_number(5), 1); // At the newline
        assert_eq!(index.line_number(6), 2); // After the newline
        assert_eq!(index.line_count(), 2);
    }

    #[test]
    fn test_line_index_span_line_number() {
        let source = "let x = 1;\nlet y = 2;\nlet z = 3;";
        let index = LineIndex::new(source);

        let span1 = Span::new(0, 10);
        assert_eq!(index.span_line_number(span1), 1);

        let span2 = Span::new(11, 21);
        assert_eq!(index.span_line_number(span2), 2);

        let span3 = Span::new(22, 32);
        assert_eq!(index.span_line_number(span3), 3);
    }

    #[test]
    fn test_line_index_at_bounds() {
        let source = "hello";
        let index = LineIndex::new(source);
        // At exactly the end of source
        assert_eq!(index.line_number(5), 1);
    }

    #[test]
    fn test_line_index_line_count() {
        assert_eq!(LineIndex::new("").line_count(), 1);
        assert_eq!(LineIndex::new("a").line_count(), 1);
        assert_eq!(LineIndex::new("a\n").line_count(), 2);
        assert_eq!(LineIndex::new("a\nb").line_count(), 2);
        assert_eq!(LineIndex::new("a\nb\n").line_count(), 3);
        assert_eq!(LineIndex::new("a\nb\nc").line_count(), 3);
    }

    // ========================================================================
    // line_col tests
    // ========================================================================

    #[test]
    fn test_byte_offset_to_line_col_basic() {
        let source = "line1\nline2\nline3";
        // "line1\nline2\nline3"
        //  01234 5 6789A B CDEF0
        // First line: offsets 0-4 are "line1", 5 is newline
        assert_eq!(byte_offset_to_line_col(source, 0), (1, 1)); // 'l'
        assert_eq!(byte_offset_to_line_col(source, 4), (1, 5)); // '1'
        assert_eq!(byte_offset_to_line_col(source, 5), (1, 6)); // '\n' (still line 1)

        // Second line: offsets 6-10 are "line2", 11 is newline
        assert_eq!(byte_offset_to_line_col(source, 6), (2, 1)); // 'l'
        assert_eq!(byte_offset_to_line_col(source, 10), (2, 5)); // '2'

        // Third line: offsets 12-16 are "line3"
        assert_eq!(byte_offset_to_line_col(source, 12), (3, 1)); // 'l'
        assert_eq!(byte_offset_to_line_col(source, 16), (3, 5)); // '3'
    }

    #[test]
    fn test_byte_offset_to_line_col_empty_source() {
        let source = "";
        assert_eq!(byte_offset_to_line_col(source, 0), (1, 1));
    }

    #[test]
    fn test_byte_offset_to_line_col_single_line() {
        let source = "hello";
        assert_eq!(byte_offset_to_line_col(source, 0), (1, 1));
        assert_eq!(byte_offset_to_line_col(source, 2), (1, 3));
        assert_eq!(byte_offset_to_line_col(source, 4), (1, 5));
        assert_eq!(byte_offset_to_line_col(source, 5), (1, 6)); // end of source
    }

    #[test]
    fn test_byte_offset_to_line_col_at_newline() {
        let source = "a\nb";
        // offset 0: 'a' -> (1, 1)
        // offset 1: '\n' -> (1, 2)
        // offset 2: 'b' -> (2, 1)
        assert_eq!(byte_offset_to_line_col(source, 0), (1, 1));
        assert_eq!(byte_offset_to_line_col(source, 1), (1, 2));
        assert_eq!(byte_offset_to_line_col(source, 2), (2, 1));
    }

    #[test]
    fn test_span_line_col() {
        let source = "let x = 1;\nlet y = 2;\nlet z = 3;";
        // Line 1: "let x = 1;\n" (offsets 0-10, newline at 10)
        // Line 2: "let y = 2;\n" (offsets 11-21, newline at 21)
        // Line 3: "let z = 3;" (offsets 22-31)

        let span1 = Span::new(0, 10);
        assert_eq!(span1.line_col(source), (1, 1));

        let span2 = Span::new(11, 21);
        assert_eq!(span2.line_col(source), (2, 1));

        let span3 = Span::new(22, 32);
        assert_eq!(span3.line_col(source), (3, 1));

        // Span starting in the middle of a line
        let span_mid = Span::new(4, 10); // "x = 1;" on line 1
        assert_eq!(span_mid.line_col(source), (1, 5)); // 'x' is at column 5
    }

    #[test]
    fn test_line_index_line_col_basic() {
        let source = "line1\nline2\nline3";
        let index = LineIndex::new(source);

        // First line
        assert_eq!(index.line_col(0), (1, 1));
        assert_eq!(index.line_col(4), (1, 5));

        // Second line
        assert_eq!(index.line_col(6), (2, 1));
        assert_eq!(index.line_col(10), (2, 5));

        // Third line
        assert_eq!(index.line_col(12), (3, 1));
        assert_eq!(index.line_col(16), (3, 5));
    }

    #[test]
    fn test_line_index_line_col_matches_byte_offset() {
        let source = "let x = 1;\nlet y = 2;\nlet z = 3;";
        let index = LineIndex::new(source);

        // Verify LineIndex matches byte_offset_to_line_col for all offsets
        for offset in 0..=source.len() {
            assert_eq!(
                index.line_col(offset as u32),
                byte_offset_to_line_col(source, offset),
                "mismatch at offset {}",
                offset
            );
        }
    }

    #[test]
    fn test_line_index_span_line_col() {
        let source = "let x = 1;\nlet y = 2;\nlet z = 3;";
        let index = LineIndex::new(source);

        let span1 = Span::new(0, 10);
        assert_eq!(index.span_line_col(span1), (1, 1));

        let span2 = Span::new(11, 21);
        assert_eq!(index.span_line_col(span2), (2, 1));

        let span3 = Span::new(22, 32);
        assert_eq!(index.span_line_col(span3), (3, 1));

        // Span starting in the middle of a line
        let span_mid = Span::new(4, 10);
        assert_eq!(index.span_line_col(span_mid), (1, 5));
    }

    #[test]
    fn test_line_index_line_col_empty_source() {
        let source = "";
        let index = LineIndex::new(source);
        assert_eq!(index.line_col(0), (1, 1));
    }

    #[test]
    fn test_line_index_line_col_at_newline() {
        let source = "a\nb";
        let index = LineIndex::new(source);
        assert_eq!(index.line_col(0), (1, 1)); // 'a'
        assert_eq!(index.line_col(1), (1, 2)); // '\n'
        assert_eq!(index.line_col(2), (2, 1)); // 'b'
    }

    // ========================================================================
    // Span::contains tests
    // ========================================================================

    #[test]
    fn test_span_contains_span() {
        let outer = Span::new(5, 20);

        // Inner span fully contained
        assert!(outer.contains(Span::new(5, 20))); // exact match
        assert!(outer.contains(Span::new(5, 10))); // at start
        assert!(outer.contains(Span::new(15, 20))); // at end
        assert!(outer.contains(Span::new(10, 15))); // in middle

        // Not contained
        assert!(!outer.contains(Span::new(0, 5))); // before (touching)
        assert!(!outer.contains(Span::new(0, 10))); // overlaps start
        assert!(!outer.contains(Span::new(15, 25))); // overlaps end
        assert!(!outer.contains(Span::new(20, 25))); // after (touching)
        assert!(!outer.contains(Span::new(0, 25))); // encompasses outer
    }

    #[test]
    fn test_span_contains_point() {
        let outer = Span::new(5, 20);

        // Point spans (empty spans)
        assert!(outer.contains(Span::point(5))); // at start
        assert!(outer.contains(Span::point(10))); // in middle
        assert!(outer.contains(Span::point(20))); // at end (point is contained)

        // Point spans outside
        assert!(!outer.contains(Span::point(4))); // before
        assert!(!outer.contains(Span::point(21))); // after
    }

    #[test]
    fn test_span_contains_empty_span() {
        let empty = Span::point(10);

        // Empty span only contains itself
        assert!(empty.contains(Span::point(10)));
        assert!(!empty.contains(Span::point(9)));
        assert!(!empty.contains(Span::new(10, 11)));
    }

    #[test]
    fn test_span_contains_pos() {
        let span = Span::new(5, 10);

        // Before span
        assert!(!span.contains_pos(0));
        assert!(!span.contains_pos(4));

        // At boundaries and inside
        assert!(span.contains_pos(5)); // start (inclusive)
        assert!(span.contains_pos(7)); // middle
        assert!(span.contains_pos(9)); // just before end
        assert!(!span.contains_pos(10)); // end (exclusive)

        // After span
        assert!(!span.contains_pos(11));
        assert!(!span.contains_pos(100));
    }

    #[test]
    fn test_span_contains_pos_empty_span() {
        let empty = Span::point(10);

        // Empty span contains no positions (start == end)
        assert!(!empty.contains_pos(9));
        assert!(!empty.contains_pos(10));
        assert!(!empty.contains_pos(11));
    }
}
