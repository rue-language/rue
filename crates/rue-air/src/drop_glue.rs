//! Representation-independent drop-glue policy.
//!
//! Live AIR types and durable query types have different identities and graph
//! owners.  They project those graphs into [`DropGlueShape`] and feed exact
//! child decisions to [`requires_drop_glue`]; the language policy itself lives
//! only here.

/// The part of a type's shape that can affect its own drop-glue decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropGlueShape {
    /// A scalar, pointer, slice, module, builtin, or other non-owning leaf.
    Trivial,
    /// A named or anonymous aggregate.
    Aggregate { has_destructor: bool },
    /// An inline array. Zero elements have zero ownership multiplicity.
    Array { len: u64 },
}

/// Decide whether a type requires drop glue from its shape and by-value
/// children. Callers must not include pointees or slice elements as children.
pub fn requires_drop_glue(shape: DropGlueShape, children: impl IntoIterator<Item = bool>) -> bool {
    match shape {
        DropGlueShape::Trivial => false,
        DropGlueShape::Aggregate { has_destructor } => {
            has_destructor || children.into_iter().any(|child| child)
        }
        DropGlueShape::Array { len: 0 } => false,
        DropGlueShape::Array { .. } => children.into_iter().any(|child| child),
    }
}

/// Conservative eligibility for requesting an exact drop-glue fact.
///
/// This is the same shape policy as [`requires_drop_glue`] evaluated with
/// conservative child eligibility rather than exact child facts.
pub fn may_require_drop_glue(
    shape: DropGlueShape,
    children: impl IntoIterator<Item = bool>,
) -> bool {
    requires_drop_glue(shape, children)
}

/// The reserved anonymous destructor is an instance method named `__drop`.
/// A same-named associated function is an ordinary associated function.
pub fn is_anonymous_destructor(name: &str, has_self: bool) -> bool {
    has_self && name == "__drop"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_policy_preserves_zero_multiplicity_and_explicit_destructors() {
        assert!(!requires_drop_glue(DropGlueShape::Trivial, [true]));
        assert!(!requires_drop_glue(DropGlueShape::Array { len: 0 }, [true]));
        assert!(requires_drop_glue(DropGlueShape::Array { len: 1 }, [true]));
        assert!(requires_drop_glue(
            DropGlueShape::Aggregate {
                has_destructor: true
            },
            []
        ));
    }

    #[test]
    fn anonymous_destructor_requires_receiver_and_reserved_name() {
        assert!(is_anonymous_destructor("__drop", true));
        assert!(!is_anonymous_destructor("__drop", false));
        assert!(!is_anonymous_destructor("drop", true));
    }
}
