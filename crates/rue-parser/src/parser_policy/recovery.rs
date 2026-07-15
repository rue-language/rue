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
/// modifiers, and directives are preserved as synchronization points.
pub(crate) fn item_recovery_action(
    position: ItemRecoveryPosition,
    token: &TokenKind,
) -> ItemRecoveryAction {
    let starts_item = matches!(
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
    match (position, starts_item) {
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
            TokenKind::Drop,
            TokenKind::Const,
            TokenKind::Pub,
            TokenKind::Linear,
            TokenKind::Unchecked,
            TokenKind::At,
        ] {
            assert_eq!(
                item_recovery_action(ItemRecoveryPosition::AfterProgress, &token),
                ItemRecoveryAction::Synchronize,
                "{token:?}"
            );
        }
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::AfterProgress, &TokenKind::Let),
            ItemRecoveryAction::Consume
        );
        assert_eq!(
            item_recovery_action(ItemRecoveryPosition::AfterProgress, &TokenKind::If),
            ItemRecoveryAction::Consume
        );
    }

    #[test]
    fn initial_position_always_makes_progress() {
        for token in [TokenKind::Fn, TokenKind::At, TokenKind::Let] {
            assert_eq!(
                item_recovery_action(ItemRecoveryPosition::Initial, &token),
                ItemRecoveryAction::Consume,
                "{token:?}"
            );
        }
    }
}
