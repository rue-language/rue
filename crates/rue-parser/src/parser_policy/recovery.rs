//! Top-level recovery synchronization policy.

use rue_lexer::TokenKind;

/// Position within one top-level recovery attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemRecoveryPosition {
    /// No token has been consumed since the failed item parse.
    Initial,
    /// Recovery has consumed at least one token and may stop at a new item.
    AfterProgress,
}

/// Action for the current token during top-level recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemRecoveryAction {
    Consume,
    Synchronize,
}

/// Choose whether to consume the current token or preserve it as the start of
/// the next item. Every recovery consumes its first token, even when it looks
/// like an item prefix, because stopping before progress would retry the same
/// failed parse forever. Once progress is established, item keywords,
/// modifiers, and directives are preserved as synchronization points — but
/// only at brace depth zero. An item prefix nested inside the failed item's
/// braces is one of that item's own members: synchronizing there re-parses a
/// struct's remaining methods as free functions, emitting phantom errors at
/// valid lines before the real one (RUE-726).
///
/// `at_test_item` reports whether the current token opens a `test "name"`
/// declaration (ADR-0083 §1). `test` is a contextual keyword, so this policy
/// cannot recognize it from the token alone; the caller decides with the same
/// lookahead the item parser uses and passes the answer in.
pub(crate) fn item_recovery_action(
    position: ItemRecoveryPosition,
    token: &TokenKind,
    brace_depth: usize,
    at_test_item: bool,
) -> ItemRecoveryAction {
    let starts_item = at_test_item
        || matches!(
            token,
            TokenKind::Fn
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::Drop
                | TokenKind::Const
                | TokenKind::Pub
                | TokenKind::Linear
                | TokenKind::Unchecked
                | TokenKind::At
        );
    match (position, starts_item && brace_depth == 0) {
        (ItemRecoveryPosition::AfterProgress, true) => ItemRecoveryAction::Synchronize,
        _ => ItemRecoveryAction::Consume,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::Spur;

    #[test]
    fn recognizes_all_item_prefixes_without_expression_tokens() {
        for token in [
            TokenKind::Fn,
            TokenKind::Struct,
            TokenKind::Enum,
            TokenKind::Drop,
            TokenKind::Const,
            TokenKind::Pub,
            TokenKind::Linear,
            TokenKind::Unchecked,
            TokenKind::At,
        ] {
            assert_eq!(
                item_recovery_action(ItemRecoveryPosition::AfterProgress, &token, 0, false),
                ItemRecoveryAction::Synchronize,
                "{token:?}"
            );
        }
        assert_eq!(
            item_recovery_action(
                ItemRecoveryPosition::AfterProgress,
                &TokenKind::Let,
                0,
                false
            ),
            ItemRecoveryAction::Consume
        );
        assert_eq!(
            item_recovery_action(
                ItemRecoveryPosition::AfterProgress,
                &TokenKind::If,
                0,
                false
            ),
            ItemRecoveryAction::Consume
        );
    }

    #[test]
    fn contextual_test_keyword_is_a_synchronization_point() {
        // `test "name" {` at brace depth zero starts an item even though its
        // token is an ordinary identifier (ADR-0083 §1). Without the caller's
        // lookahead the same identifier is consumed like any other.
        let ident = TokenKind::Ident(Spur::default());
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::AfterProgress, &ident, 0, true),
            ItemRecoveryAction::Synchronize
        );
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::AfterProgress, &ident, 0, false),
            ItemRecoveryAction::Consume
        );
        // Still only at top level, and never before progress.
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::AfterProgress, &ident, 1, true),
            ItemRecoveryAction::Consume
        );
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::Initial, &ident, 0, true),
            ItemRecoveryAction::Consume
        );
    }

    #[test]
    fn nested_item_prefixes_are_not_synchronization_points() {
        // A method's `fn` inside a failed struct's body must be consumed, not
        // treated as the start of a new top-level item (RUE-726).
        for depth in [1usize, 2, 7] {
            assert_eq!(
                item_recovery_action(
                    ItemRecoveryPosition::AfterProgress,
                    &TokenKind::Fn,
                    depth,
                    false
                ),
                ItemRecoveryAction::Consume,
                "depth {depth}"
            );
        }
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::Initial, &TokenKind::Fn, 0, false),
            ItemRecoveryAction::Consume
        );
    }
}
