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
pub(crate) fn item_recovery_action(
    position: ItemRecoveryPosition,
    token: &TokenKind,
    brace_depth: usize,
) -> ItemRecoveryAction {
    let starts_item = matches!(
        token,
        TokenKind::Fn
            | TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Interface
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

    #[test]
    fn recognizes_all_item_prefixes_without_expression_tokens() {
        for token in [
            TokenKind::Fn,
            TokenKind::Struct,
            TokenKind::Enum,
            TokenKind::Interface,
            TokenKind::Drop,
            TokenKind::Const,
            TokenKind::Pub,
            TokenKind::Linear,
            TokenKind::Unchecked,
            TokenKind::At,
        ] {
            assert_eq!(
                item_recovery_action(ItemRecoveryPosition::AfterProgress, &token, 0),
                ItemRecoveryAction::Synchronize,
                "{token:?}"
            );
        }
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::AfterProgress, &TokenKind::Let, 0),
            ItemRecoveryAction::Consume
        );
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::AfterProgress, &TokenKind::If, 0),
            ItemRecoveryAction::Consume
        );
    }

    #[test]
    fn nested_item_prefixes_are_not_synchronization_points() {
        // A method's `fn` inside a failed struct's body must be consumed, not
        // treated as the start of a new top-level item (RUE-726).
        for depth in [1usize, 2, 7] {
            assert_eq!(
                item_recovery_action(ItemRecoveryPosition::AfterProgress, &TokenKind::Fn, depth),
                ItemRecoveryAction::Consume,
                "depth {depth}"
            );
        }
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::Initial, &TokenKind::Fn, 0),
            ItemRecoveryAction::Consume
        );
    }
}
