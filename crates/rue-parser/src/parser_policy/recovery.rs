//! Top-level recovery synchronization policy.

use rue_lexer::TokenKind;

/// The item production selected by a token at top level.
///
/// This is classification metadata, not a second grammar: `Parser::item`
/// remains the authority on which modifiers and declaration bodies are valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemStart {
    Directive,
    Public,
    UncheckedFunction,
    Function,
    Test,
    LinearStruct,
    Struct,
    Enum,
    Drop,
    Extern,
    Const,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ItemStartMetadata {
    pub(crate) token: Option<TokenKind>,
    pub(crate) start: ItemStart,
    diagnostic_label: &'static str,
    show_in_item_expected: bool,
    show_after_public: bool,
}

/// Metadata for every form that can begin a top-level item.
///
/// Keep this table as the single item-start inventory used by dispatch,
/// recovery, and expected-item presentation. Ordinary starts carry their
/// token; contextual `test` has `None` because parser lookahead recognizes it
/// rather than a distinct token kind.
pub(crate) const ITEM_STARTS: &[ItemStartMetadata] = &[
    item_start(
        Some(TokenKind::At),
        ItemStart::Directive,
        "'@'",
        true,
        false,
    ),
    item_start(
        Some(TokenKind::Pub),
        ItemStart::Public,
        "'pub'",
        true,
        false,
    ),
    item_start(
        Some(TokenKind::Unchecked),
        ItemStart::UncheckedFunction,
        "'unchecked'",
        true,
        true,
    ),
    item_start(Some(TokenKind::Fn), ItemStart::Function, "'fn'", true, true),
    // `test` is contextual and therefore has no dedicated token kind.
    item_start(None, ItemStart::Test, "'test'", true, false),
    item_start(
        Some(TokenKind::Linear),
        ItemStart::LinearStruct,
        "'linear'",
        false,
        true,
    ),
    item_start(
        Some(TokenKind::Struct),
        ItemStart::Struct,
        "'struct'",
        false,
        true,
    ),
    item_start(
        Some(TokenKind::Enum),
        ItemStart::Enum,
        "'enum'",
        false,
        false,
    ),
    item_start(
        Some(TokenKind::Drop),
        ItemStart::Drop,
        "'drop'",
        false,
        false,
    ),
    item_start(
        Some(TokenKind::Extern),
        ItemStart::Extern,
        "'extern'",
        false,
        false,
    ),
    item_start(
        Some(TokenKind::Const),
        ItemStart::Const,
        "'const'",
        false,
        false,
    ),
];

const fn item_start(
    token: Option<TokenKind>,
    start: ItemStart,
    diagnostic_label: &'static str,
    show_in_item_expected: bool,
    show_after_public: bool,
) -> ItemStartMetadata {
    ItemStartMetadata {
        token,
        start,
        diagnostic_label,
        show_in_item_expected,
        show_after_public,
    }
}

fn expected_from_metadata(include: impl Fn(&ItemStartMetadata) -> bool) -> String {
    ITEM_STARTS
        .iter()
        .filter(|metadata| include(metadata))
        .map(|metadata| metadata.diagnostic_label)
        .chain(std::iter::once("…"))
        .collect::<Vec<_>>()
        .join(" or ")
}

/// Stable expected-item presentation derived from the canonical metadata.
pub(crate) fn expected_item() -> String {
    expected_from_metadata(|metadata| metadata.show_in_item_expected)
}

pub(crate) fn expected_after_public() -> String {
    expected_from_metadata(|metadata| metadata.show_after_public)
}

pub(crate) fn classify_item_start(token: &TokenKind, at_test_item: bool) -> Option<ItemStart> {
    if at_test_item {
        return ITEM_STARTS
            .iter()
            .find_map(|metadata| metadata.token.is_none().then_some(metadata.start));
    }
    ITEM_STARTS
        .iter()
        .find_map(|metadata| (metadata.token.as_ref() == Some(token)).then_some(metadata.start))
}

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
    let starts_item = classify_item_start(token, at_test_item).is_some();
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
        for metadata in ITEM_STARTS {
            let Some(token) = metadata.token else {
                continue;
            };
            assert_eq!(
                item_recovery_action(ItemRecoveryPosition::AfterProgress, &token, 0, false,),
                ItemRecoveryAction::Synchronize,
                "{:?}",
                token
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
