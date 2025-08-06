//! Domain-specific types for better type safety and API ergonomics

use std::fmt;
use std::str::FromStr;

/// A newtype for error codes to provide better type safety
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ErrorCode(pub u32);

impl ErrorCode {
    pub const fn new(code: u32) -> Self {
        Self(code)
    }

    pub fn code(&self) -> u32 {
        self.0
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}", self.0)
    }
}

impl From<u32> for ErrorCode {
    fn from(code: u32) -> Self {
        Self(code)
    }
}

impl FromStr for ErrorCode {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u32>().map(Self)
    }
}

/// A newtype for diagnostic messages with better string handling
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticMessage(String);

impl DiagnosticMessage {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// Create a formatted message without unnecessary allocations
    pub fn formatted(args: fmt::Arguments<'_>) -> Self {
        Self(args.to_string())
    }
}

impl fmt::Display for DiagnosticMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for DiagnosticMessage {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DiagnosticMessage {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<std::borrow::Cow<'_, str>> for DiagnosticMessage {
    fn from(s: std::borrow::Cow<'_, str>) -> Self {
        Self(s.into_owned())
    }
}

/// A newtype for file paths with validation
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FilePath(String);

impl FilePath {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn stdin() -> Self {
        Self("<stdin>".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// Check if this is a special stdin path
    pub fn is_stdin(&self) -> bool {
        self.0 == "<stdin>"
    }
}

impl fmt::Display for FilePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for FilePath {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for FilePath {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<std::path::PathBuf> for FilePath {
    fn from(p: std::path::PathBuf) -> Self {
        Self(p.to_string_lossy().into_owned())
    }
}

impl From<&std::path::Path> for FilePath {
    fn from(p: &std::path::Path) -> Self {
        Self(p.to_string_lossy().into_owned())
    }
}

/// A newtype for line numbers (1-indexed)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineNumber(pub usize);

impl LineNumber {
    pub const fn new(line: usize) -> Self {
        Self(line)
    }

    pub const fn get(&self) -> usize {
        self.0
    }

    /// Convert from 0-indexed to 1-indexed line number
    pub const fn from_zero_indexed(line: usize) -> Self {
        Self(line + 1)
    }

    /// Convert to 0-indexed line number
    pub const fn to_zero_indexed(self) -> usize {
        self.0.saturating_sub(1)
    }
}

impl fmt::Display for LineNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<usize> for LineNumber {
    fn from(line: usize) -> Self {
        Self(line)
    }
}

/// A newtype for column numbers (1-indexed)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColumnNumber(pub usize);

impl ColumnNumber {
    pub const fn new(col: usize) -> Self {
        Self(col)
    }

    pub const fn get(&self) -> usize {
        self.0
    }

    /// Convert from 0-indexed to 1-indexed column number
    pub const fn from_zero_indexed(col: usize) -> Self {
        Self(col + 1)
    }

    /// Convert to 0-indexed column number
    pub const fn to_zero_indexed(self) -> usize {
        self.0.saturating_sub(1)
    }
}

impl fmt::Display for ColumnNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<usize> for ColumnNumber {
    fn from(col: usize) -> Self {
        Self(col)
    }
}

/// A position in source code with proper line/column types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    pub line: LineNumber,
    pub column: ColumnNumber,
}

impl SourcePosition {
    pub fn new(line: LineNumber, column: ColumnNumber) -> Self {
        Self { line, column }
    }

    pub fn from_indices(line: usize, column: usize) -> Self {
        Self {
            line: LineNumber::from_zero_indexed(line),
            column: ColumnNumber::from_zero_indexed(column),
        }
    }
}

impl fmt::Display for SourcePosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code() {
        let code = ErrorCode::new(42);
        assert_eq!(code.to_string(), "0042");
        assert_eq!(code.code(), 42);

        let parsed: ErrorCode = "123".parse().unwrap();
        assert_eq!(parsed.code(), 123);
    }

    #[test]
    fn test_diagnostic_message() {
        let msg = DiagnosticMessage::new("test");
        assert_eq!(msg.as_str(), "test");

        let from_string = DiagnosticMessage::from("hello".to_string());
        assert_eq!(from_string.as_str(), "hello");
    }

    #[test]
    fn test_line_column_conversion() {
        let line = LineNumber::from_zero_indexed(0);
        assert_eq!(line.get(), 1);
        assert_eq!(line.to_zero_indexed(), 0);

        let col = ColumnNumber::from_zero_indexed(5);
        assert_eq!(col.get(), 6);
        assert_eq!(col.to_zero_indexed(), 5);
    }

    #[test]
    fn test_source_position() {
        let pos = SourcePosition::from_indices(0, 0);
        assert_eq!(pos.line.get(), 1);
        assert_eq!(pos.column.get(), 1);
        assert_eq!(pos.to_string(), "1:1");
    }
}
