//! The one decision and the one diagnostic for a private item reached through
//! a module.
//!
//! Privacy is directory-based (spec 10.3:3) and uniform across item kinds
//! (10.3:7): an item is usable outside its defining directory if and only if
//! it is `pub`. Every position that can name an item — a value, a type
//! annotation, a function signature, a struct literal, a match-pattern head,
//! an associated-function receiver — therefore reaches the same answer, and
//! must report it with the same code and the same words.
//!
//! This module owns both halves of that report:
//!
//! * [`PrivateItemKind`] is the complete vocabulary a privacy diagnostic uses
//!   to name what it rejected — one spelling per kind, written down once.
//! * [`private_member_access`] is the only place E0706's payload is built, so
//!   no emitter can drift into its own wording.
//!
//! The *predicate* stays where the caller's file identity lives:
//! [`crate::sema`]'s `is_accessible` answers it from `FileId`s through the
//! memoized domain facts, and [`check_source_path_visibility`] answers it for
//! callers outside the body engine that identify files by source path. Both
//! short-circuit before deriving a domain and both end in the constructor
//! here.

use rue_error::ErrorKind;

use crate::semantic_type_resolution::{SemanticTypeFactKind, SemanticVisibilityDomain};

/// How a privacy diagnostic names the kind of item it rejected.
///
/// This is the whole vocabulary: one variant per item kind, one spelling per
/// variant. `Const` covers value constants, type aliases, and module bindings
/// alike, because a module binding *is* a `const` (spec 10.4:1) and the
/// diagnostic names the binding the source wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrivateItemKind {
    Function,
    Struct,
    Enum,
    Const,
}

impl PrivateItemKind {
    /// Every kind, in declaration order, for guards that pin the vocabulary.
    pub const ALL: [Self; 4] = [Self::Function, Self::Struct, Self::Enum, Self::Const];

    /// The one spelling for this kind. Diagnostics use the source keyword the
    /// reader would have written, so `const` rather than "constant".
    pub const fn spelling(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Const => "const",
        }
    }
}

impl From<SemanticTypeFactKind> for PrivateItemKind {
    fn from(kind: SemanticTypeFactKind) -> Self {
        match kind {
            SemanticTypeFactKind::Struct => Self::Struct,
            SemanticTypeFactKind::Enum => Self::Enum,
            SemanticTypeFactKind::Constant => Self::Const,
            SemanticTypeFactKind::Function => Self::Function,
        }
    }
}

/// Build the E0706 payload for a private item reached through a module.
///
/// This is the sole construction site of [`ErrorKind::PrivateMemberAccess`];
/// `api_inventory` pins that, so a new emitter has to come through here and
/// inherits the vocabulary above.
pub fn private_member_access(kind: PrivateItemKind, name: &str) -> ErrorKind {
    ErrorKind::PrivateMemberAccess {
        item_kind: kind.spelling().to_owned(),
        name: name.to_owned(),
    }
}

/// Is a declaration in `defining_path` reachable from `accessing_path`?
///
/// The shape of the decision for callers that identify a file by its source
/// path rather than by `FileId`. A public item and a self-reference are
/// answered without deriving a domain at all, matching the short-circuit the
/// body engine's `is_accessible` applies.
pub fn source_path_is_accessible(
    accessing_path: &str,
    defining_path: &str,
    is_public: bool,
) -> bool {
    if is_public || accessing_path == defining_path {
        return true;
    }
    SemanticVisibilityDomain::from_file_path(Some(defining_path)).is_visible_from(
        &SemanticVisibilityDomain::from_file_path(Some(accessing_path)),
        is_public,
    )
}

/// Decide and report in one step, for a caller holding source paths.
pub fn check_source_path_visibility(
    kind: PrivateItemKind,
    name: &str,
    accessing_path: &str,
    defining_path: &str,
    is_public: bool,
) -> Result<(), ErrorKind> {
    if source_path_is_accessible(accessing_path, defining_path, is_public) {
        return Ok(());
    }
    Err(private_member_access(kind, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vocabulary lives in exactly one table; this pins it so a rewording
    /// is a deliberate edit here rather than a drift at some emitter.
    #[test]
    fn item_kind_spellings_are_pinned() {
        assert_eq!(
            PrivateItemKind::ALL.map(PrivateItemKind::spelling),
            ["function", "struct", "enum", "const"]
        );
    }

    #[test]
    fn every_semantic_type_fact_kind_maps_into_the_vocabulary() {
        for (fact, expected) in [
            (SemanticTypeFactKind::Struct, PrivateItemKind::Struct),
            (SemanticTypeFactKind::Enum, PrivateItemKind::Enum),
            (SemanticTypeFactKind::Constant, PrivateItemKind::Const),
            (SemanticTypeFactKind::Function, PrivateItemKind::Function),
        ] {
            assert_eq!(PrivateItemKind::from(fact), expected);
        }
    }

    #[test]
    fn a_public_item_and_a_self_reference_skip_the_domain_derivation() {
        assert!(source_path_is_accessible("a/main.rue", "b/lib.rue", true));
        assert!(source_path_is_accessible("a/main.rue", "a/main.rue", false));
    }

    #[test]
    fn a_private_item_is_reachable_only_within_its_directory() {
        assert!(source_path_is_accessible(
            "sub/main.rue",
            "sub/lib.rue",
            false
        ));
        assert!(!source_path_is_accessible("main.rue", "sub/lib.rue", false));
        assert_eq!(
            check_source_path_visibility(
                PrivateItemKind::Const,
                "inner",
                "main.rue",
                "sub/lib.rue",
                false,
            ),
            Err(ErrorKind::PrivateMemberAccess {
                item_kind: "const".to_owned(),
                name: "inner".to_owned(),
            })
        );
    }
}
