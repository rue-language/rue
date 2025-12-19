//! Source span and location types for the Rue compiler.
//!
//! This crate provides the fundamental types for tracking source locations
//! throughout the compilation pipeline.

/// A span representing a range in the source code.
///
/// Spans use byte offsets into the source string. They are designed to be
/// small (8 bytes) and cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Span {
    /// Start byte offset (inclusive)
    pub start: u32,
    /// End byte offset (exclusive)
    pub end: u32,
}

impl Span {
    /// Create a new span from start and end byte offsets.
    #[inline]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Create an empty span at a single position.
    #[inline]
    pub const fn point(pos: u32) -> Self {
        Self {
            start: pos,
            end: pos,
        }
    }

    /// Create a span covering two spans (from start of first to end of second).
    #[inline]
    pub const fn cover(a: Span, b: Span) -> Self {
        Self {
            start: if a.start < b.start { a.start } else { b.start },
            end: if a.end > b.end { a.end } else { b.end },
        }
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

    /// Convert to a Range<usize> for slicing.
    #[inline]
    pub const fn as_range(&self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}

impl From<std::ops::Range<usize>> for Span {
    #[inline]
    fn from(range: std::ops::Range<usize>) -> Self {
        Self {
            start: range.start as u32,
            end: range.end as u32,
        }
    }
}

impl From<std::ops::Range<u32>> for Span {
    #[inline]
    fn from(range: std::ops::Range<u32>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

/// A value with an associated span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    /// Create a new spanned value.
    #[inline]
    pub const fn new(value: T, span: Span) -> Self {
        Self { value, span }
    }

    /// Map the inner value while preserving the span.
    #[inline]
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Spanned<U> {
        Spanned {
            value: f(self.value),
            span: self.span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_size() {
        // Ensure Span stays small
        assert_eq!(std::mem::size_of::<Span>(), 8);
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
}
