//! Error recovery strategies for parsers

use crate::Diagnostic;

/// Error recovery strategy for parsers
pub trait ErrorRecovery<T> {
    /// Attempt to recover from an error
    fn recover(&mut self, error: Diagnostic) -> RecoveryResult<T>;
}

/// Result of an error recovery attempt
#[derive(Debug)]
pub enum RecoveryResult<T> {
    /// Successfully recovered with partial result
    Recovered(T),
    /// Failed to recover, abort parsing
    Failed,
    /// Skip current token and continue
    Skip,
    /// Skip until a synchronization point (e.g., semicolon, closing brace)
    SkipUntil(SyncPoint),
}

/// Synchronization points for error recovery
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPoint {
    /// Semicolon statement terminator
    Semicolon,
    /// Closing brace
    RightBrace,
    /// Closing parenthesis
    RightParen,
    /// Closing bracket
    RightBracket,
    /// End of file
    Eof,
    /// Any of the statement keywords (fn, let, if, while, etc.)
    StatementKeyword,
    /// Any closing delimiter
    AnyClosingDelimiter,
}

impl SyncPoint {
    /// Check if a token kind matches this sync point
    pub fn matches(&self, token_kind: &str) -> bool {
        match self {
            SyncPoint::Semicolon => token_kind == "Semicolon",
            SyncPoint::RightBrace => token_kind == "RightBrace",
            SyncPoint::RightParen => token_kind == "RightParen",
            SyncPoint::RightBracket => token_kind == "RightBracket",
            SyncPoint::Eof => token_kind == "Eof",
            SyncPoint::StatementKeyword => matches!(
                token_kind,
                "Fn" | "Let" | "If" | "While" | "For" | "Return" | "Break" | "Continue"
            ),
            SyncPoint::AnyClosingDelimiter => {
                matches!(token_kind, "RightBrace" | "RightParen" | "RightBracket")
            }
        }
    }
}

/// Common recovery strategies
pub struct RecoveryStrategies;

impl RecoveryStrategies {
    /// Skip until we find a statement boundary
    pub fn skip_to_statement_boundary<T>() -> RecoveryResult<T> {
        RecoveryResult::SkipUntil(SyncPoint::StatementKeyword)
    }

    /// Skip until we find a semicolon
    pub fn skip_to_semicolon<T>() -> RecoveryResult<T> {
        RecoveryResult::SkipUntil(SyncPoint::Semicolon)
    }

    /// Skip until we find a closing delimiter
    pub fn skip_to_closing_delimiter<T>() -> RecoveryResult<T> {
        RecoveryResult::SkipUntil(SyncPoint::AnyClosingDelimiter)
    }

    /// Skip a single token
    pub fn skip_token<T>() -> RecoveryResult<T> {
        RecoveryResult::Skip
    }

    /// Fail and abort parsing
    pub fn fail<T>() -> RecoveryResult<T> {
        RecoveryResult::Failed
    }

    /// Recover with a default/placeholder value
    pub fn recover_with<T>(value: T) -> RecoveryResult<T> {
        RecoveryResult::Recovered(value)
    }
}

/// Helper for tracking recovery state
#[derive(Debug, Default)]
pub struct RecoveryState {
    /// Number of errors recovered from
    pub recovered_errors: usize,
    /// Maximum errors to recover from before giving up
    pub max_recoveries: usize,
    /// Whether we're currently in recovery mode
    pub in_recovery: bool,
}

impl RecoveryState {
    pub fn new(max_recoveries: usize) -> Self {
        Self {
            recovered_errors: 0,
            max_recoveries,
            in_recovery: false,
        }
    }

    /// Check if we should continue recovering
    pub fn should_continue(&self) -> bool {
        self.recovered_errors < self.max_recoveries
    }

    /// Enter recovery mode
    pub fn enter_recovery(&mut self) {
        self.in_recovery = true;
        self.recovered_errors += 1;
    }

    /// Exit recovery mode
    pub fn exit_recovery(&mut self) {
        self.in_recovery = false;
    }

    /// Reset the recovery state
    pub fn reset(&mut self) {
        self.recovered_errors = 0;
        self.in_recovery = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_point_matching() {
        assert!(SyncPoint::Semicolon.matches("Semicolon"));
        assert!(!SyncPoint::Semicolon.matches("Comma"));

        assert!(SyncPoint::StatementKeyword.matches("Fn"));
        assert!(SyncPoint::StatementKeyword.matches("Let"));
        assert!(!SyncPoint::StatementKeyword.matches("Identifier"));

        assert!(SyncPoint::AnyClosingDelimiter.matches("RightBrace"));
        assert!(SyncPoint::AnyClosingDelimiter.matches("RightParen"));
        assert!(!SyncPoint::AnyClosingDelimiter.matches("LeftBrace"));
    }

    #[test]
    fn test_recovery_state() {
        let mut state = RecoveryState::new(3);

        assert!(state.should_continue());
        assert!(!state.in_recovery);

        state.enter_recovery();
        assert!(state.in_recovery);
        assert_eq!(state.recovered_errors, 1);

        state.exit_recovery();
        assert!(!state.in_recovery);

        // Simulate hitting the max
        for _ in 0..2 {
            state.enter_recovery();
            state.exit_recovery();
        }

        assert!(!state.should_continue());
        assert_eq!(state.recovered_errors, 3);
    }
}
