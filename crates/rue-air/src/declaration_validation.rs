//! Position-free declaration rules shared by semantic producers.
//!
//! Callers own query scheduling and source locators. These helpers own only
//! deterministic policy over canonical declaration facts, so the one-shot AIR
//! adapter and the keyed semantic graph cannot drift on the rule itself.

use ahash::AHashSet;

use rue_error::ErrorKind;

pub fn duplicate_parameter(names: impl IntoIterator<Item = impl AsRef<str>>) -> Option<ErrorKind> {
    duplicate_parameter_with_ordinal(names).map(|(kind, _)| kind)
}

pub fn duplicate_parameter_with_ordinal(
    names: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<(ErrorKind, usize)> {
    let mut seen = AHashSet::new();
    for (ordinal, name) in names.into_iter().enumerate() {
        let name = name.as_ref();
        if !seen.insert(name.to_owned()) {
            return Some((
                ErrorKind::DuplicateParameter {
                    name: name.to_owned(),
                },
                ordinal,
            ));
        }
    }
    None
}

pub fn reserved_function_name(name: &str) -> Option<ErrorKind> {
    rue_builtins::is_reserved_function_name(name).then(|| ErrorKind::ReservedFunctionName {
        function_name: name.to_owned(),
    })
}

pub fn duplicate_constant(name: &str) -> ErrorKind {
    ErrorKind::DuplicateConstant {
        name: name.to_owned(),
        kind: "constant".to_owned(),
    }
}

pub fn const_cross_kind_collision(
    name: &str,
    is_value_const: bool,
    has_non_const_declaration: bool,
) -> Option<ErrorKind> {
    (is_value_const && has_non_const_declaration).then(|| ErrorKind::DuplicateFunctionDefinition {
        function_name: name.to_owned(),
    })
}

pub fn duplicate_field(
    struct_name: &str,
    fields: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<ErrorKind> {
    let mut seen = AHashSet::new();
    for field in fields {
        let field = field.as_ref();
        if !seen.insert(field.to_owned()) {
            return Some(ErrorKind::DuplicateField {
                struct_name: struct_name.to_owned(),
                field_name: field.to_owned(),
            });
        }
    }
    None
}

pub fn duplicate_variant(
    enum_name: &str,
    variants: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<ErrorKind> {
    let mut seen = AHashSet::new();
    for variant in variants {
        let variant = variant.as_ref();
        if !seen.insert(variant.to_owned()) {
            return Some(ErrorKind::DuplicateVariant {
                enum_name: enum_name.to_owned(),
                variant_name: variant.to_owned(),
            });
        }
    }
    None
}

pub fn linear_copy_struct(struct_name: &str, is_linear: bool, is_copy: bool) -> Option<ErrorKind> {
    (is_linear && is_copy).then(|| ErrorKind::LinearStructCopy(struct_name.to_owned()))
}

pub fn destructor_unknown_type(type_name: &str) -> ErrorKind {
    ErrorKind::DestructorUnknownType {
        type_name: type_name.to_owned(),
    }
}

pub fn duplicate_destructor(type_name: &str) -> ErrorKind {
    ErrorKind::DuplicateDestructor {
        type_name: type_name.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// `-> borrow` accessor declaration legality (spec 6.6:3-6.6:7, ADR-0062).
// ---------------------------------------------------------------------------
//
// These rules are legality rules on the *declaration*, so a declared-but-
// uncalled accessor is exactly as ill-formed as a called one, and every
// declaration producer runs them: RIR predeclaration (`sema::declarations`)
// and the driver's reparsed signature query
// (`rue_compiler::semantic_query_nucleus`). The demanded body walk
// (`sema::control_flow`) additionally resolves whether a method-call link in a
// yielded projection names another accessor. The producers necessarily differ
// in *what they walk* — RIR instructions or a reparsed AST — so the forms below
// are named after the spec's own vocabulary rather than either IR. The mapping
// from a form to its diagnostic, and the order in which the rules apply, live
// here exactly once; without that, the two declaration producers drift on
// wording the way they did before RUE-1232.

/// The preview feature the accessor form is gated behind (6.6:3).
pub const ACCESSOR_PREVIEW_FEATURE: rue_error::PreviewFeature =
    rue_error::PreviewFeature::BorrowAccessors;

/// The subject every producer names in the 6.6:3 gate diagnostic. The result
/// position alone demands the preview, so this is reported before any shape
/// rule about a form the program cannot yet name.
pub const ACCESSOR_PREVIEW_SUBJECT: &str = "a `-> borrow` accessor";

/// The note that points an `inout self` accessor at the phase that will admit
/// it (6.6:4).
pub const ACCESSOR_INOUT_SELF_NOTE: &str =
    "mutable accessors (`inout self` -> exclusive result) are a later phase (RUE-1016)";

/// What a producer found in the receiver position of a `-> borrow`
/// declaration (6.6:4). Only [`AccessorReceiverForm::BorrowSelf`] is legal in
/// ADR-0062 phase 1: an accessor hands out a shared projection of its
/// receiver, so the receiver has to exist and be a shared borrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessorReceiverForm {
    /// `borrow self`.
    BorrowSelf,
    /// `inout self`.
    InoutSelf,
    /// A by-value `self`.
    ValueSelf,
    /// A declaration with no owning type at all.
    FreeFunction,
    /// A declaration with an owning type but no receiver parameter.
    AssociatedFunction,
}

/// The mode of one non-receiver accessor parameter (6.6:5). Guard inputs are
/// plain by-value runtime values evaluated once at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessorParameterForm {
    ByValue,
    Borrow,
    Inout,
    Comptime,
}

/// A body element that exits an accessor body without falling through to its
/// trailing `yield` (6.6:6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessorExitForm {
    SecondYield,
    Return,
    Try,
}

/// What the trailing `yield`'s projection chain bottoms out at, when that is
/// *not* the receiver (6.6:7).
///
/// A chain descends through field, index, and accessor-call links; the
/// receiver parameter `self` is the only legal root, so a producer names one
/// of these forms exactly when the chain is illegal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessorYieldRootForm {
    /// Some other named binding: a local, a guard parameter, a global.
    Named(std::sync::Arc<str>),
    /// A computed value rather than a place.
    Value,
    /// A method-call link the producer *proved* is not an accessor. A producer
    /// that cannot decide the callee stays permissive and never reports this;
    /// see [`accessor_method_link_error`].
    PlainMethod,
}

/// What a producer knows about one method-call link in a yielded projection
/// chain (6.6:7).
///
/// A link is a legal projection only when the callee is itself an accessor,
/// which is a resolved-callee question rather than a syntactic one. Producers
/// differ in how much of it they can answer: the demanded body walk has the
/// receiver's type, the driver's signature query can decide a link whose
/// receiver is the accessor's own `self` from the owner's other declarations,
/// and RIR predeclaration cannot decide any. A producer that cannot decide
/// reports [`AccessorMethodLink::Unresolved`] and the rule stays permissive —
/// it never guesses a rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessorMethodLink {
    /// The callee is an accessor: a legal link, keep descending.
    Accessor,
    /// The callee is proven to be a plain method.
    PlainMethod,
    /// The producer cannot name the callee.
    Unresolved,
}

/// 6.6:7 for one method-call link: rejected only when the callee is *proven*
/// to be a plain method.
pub fn accessor_method_link_error(link: AccessorMethodLink) -> Option<ErrorKind> {
    matches!(link, AccessorMethodLink::PlainMethod)
        .then(|| accessor_yield_root_error(&AccessorYieldRootForm::PlainMethod))
}

/// The 6.6:6/6.6:7 verdict for one accessor body, decided by a producer's own
/// walk over its own representation of that body.
///
/// This is the producer-neutral currency of the body rules: the driver retains
/// it in the declaration's durable signature and reports it later, while the
/// RIR producers pair it with the span of the offending form and report it
/// immediately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessorBodyVerdict {
    /// Well-formed as far as the deciding producer can tell.
    WellFormed,
    /// The body's final statement is not a `yield`.
    MissingTrailingYield,
    /// A trailing `yield` exists, but another exit bypasses it.
    OtherExit(AccessorExitForm),
    /// The trailing `yield` hands out something other than a receiver-rooted
    /// place.
    YieldNotReceiverRooted(AccessorYieldRootForm),
}

/// A violation of the accessor signature rules, in the order the rules apply.
pub enum AccessorSignatureViolation {
    /// 6.6:4. `note` is the phase pointer for `inout self`.
    Receiver {
        kind: ErrorKind,
        note: Option<&'static str>,
    },
    /// 6.6:5. `ordinal` indexes the non-receiver parameter list the caller
    /// passed, so a producer holding parameter positions can anchor there.
    Parameter { kind: ErrorKind, ordinal: usize },
}

/// 6.6:3: the accessor form requires its preview gate. `enabled` is the
/// caller's answer for [`ACCESSOR_PREVIEW_FEATURE`], since producers carry
/// their preview set in different shapes.
pub fn accessor_preview_gate(enabled: bool) -> Option<ErrorKind> {
    (!enabled).then(|| ErrorKind::PreviewFeatureRequired {
        feature: ACCESSOR_PREVIEW_FEATURE,
        what: ACCESSOR_PREVIEW_SUBJECT.to_owned(),
    })
}

/// 6.6:4 then 6.6:5 over one accessor declaration's signature. `parameters`
/// are the non-receiver parameters in declaration order.
pub fn accessor_signature(
    receiver: AccessorReceiverForm,
    parameters: impl IntoIterator<Item = AccessorParameterForm>,
) -> Option<AccessorSignatureViolation> {
    if let Some((kind, note)) = accessor_receiver_error(receiver) {
        return Some(AccessorSignatureViolation::Receiver { kind, note });
    }
    for (ordinal, parameter) in parameters.into_iter().enumerate() {
        if let Some(kind) = accessor_parameter_error(parameter) {
            return Some(AccessorSignatureViolation::Parameter { kind, ordinal });
        }
    }
    None
}

/// 6.6:4 alone, for a producer that anchors the receiver diagnostic itself.
pub fn accessor_receiver_error(
    receiver: AccessorReceiverForm,
) -> Option<(ErrorKind, Option<&'static str>)> {
    let (found, note) = match receiver {
        AccessorReceiverForm::BorrowSelf => return None,
        AccessorReceiverForm::InoutSelf => {
            ("an `inout self` receiver", Some(ACCESSOR_INOUT_SELF_NOTE))
        }
        AccessorReceiverForm::ValueSelf => ("a by-value `self` receiver", None),
        AccessorReceiverForm::FreeFunction => ("a free function", None),
        AccessorReceiverForm::AssociatedFunction => {
            ("an associated function with no receiver", None)
        }
    };
    Some((
        ErrorKind::AccessorRequiresBorrowSelf {
            found: found.to_owned(),
        },
        note,
    ))
}

/// 6.6:5 alone, for one non-receiver parameter. `comptime` is reported ahead
/// of the passing mode: a comptime guard input is rejected for being absent at
/// run time, whatever mode it was spelled with.
pub fn accessor_parameter_error(parameter: AccessorParameterForm) -> Option<ErrorKind> {
    let mode = match parameter {
        AccessorParameterForm::ByValue => return None,
        AccessorParameterForm::Comptime => "`comptime`",
        AccessorParameterForm::Borrow => "`borrow`",
        AccessorParameterForm::Inout => "`inout`",
    };
    Some(ErrorKind::AccessorParamModeUnsupported {
        mode: mode.to_owned(),
    })
}

/// The diagnostic for a body verdict, or `None` when the body is well-formed.
pub fn accessor_body_error(verdict: &AccessorBodyVerdict) -> Option<ErrorKind> {
    match verdict {
        AccessorBodyVerdict::WellFormed => None,
        AccessorBodyVerdict::MissingTrailingYield => Some(accessor_missing_yield_error()),
        AccessorBodyVerdict::OtherExit(exit) => Some(accessor_exit_error(*exit)),
        AccessorBodyVerdict::YieldNotReceiverRooted(root) => Some(accessor_yield_root_error(root)),
    }
}

/// The first half of 6.6:6: the body does not end in a trailing `yield`.
pub fn accessor_missing_yield_error() -> ErrorKind {
    ErrorKind::AccessorBodyMissingYield
}

/// The rest of 6.6:6: an exit that bypasses the trailing `yield`.
pub fn accessor_exit_error(exit: AccessorExitForm) -> ErrorKind {
    let found = match exit {
        AccessorExitForm::SecondYield => "a second `yield`",
        AccessorExitForm::Return => "a `return`",
        AccessorExitForm::Try => "a `?`",
    };
    ErrorKind::AccessorBodyOtherExit {
        found: found.to_owned(),
    }
}

/// 6.6:7: the illegal root a producer's chain walk bottomed out at.
pub fn accessor_yield_root_error(root: &AccessorYieldRootForm) -> ErrorKind {
    let found = match root {
        AccessorYieldRootForm::Named(name) => format!("a place rooted at `{name}`"),
        AccessorYieldRootForm::Value => "a value expression".to_owned(),
        AccessorYieldRootForm::PlainMethod => {
            "a method-call result that is not an accessor projection".to_owned()
        }
    };
    ErrorKind::AccessorYieldNotReceiverRooted { found }
}

/// One method of an accessor's owner, as the 6.6:14 declaration-seam cycle
/// rule reads it (RUE-1282).
///
/// `self_call_targets` are the method names the method's own body calls on its
/// `self` receiver — a *parsed* fact of that sibling declaration, position-free
/// and normalized (sorted, deduplicated), so a body edit that does not change
/// the set leaves the value equal. A call through any other receiver is not
/// recorded: per the well-founded component order, a cycle of accessor
/// expansions on one owner can otherwise only pass through a guard parameter
/// of the owner's own type, and those exotic edges stay with the demanded-path
/// checks (the analysis-time identity check and the CFG dependency graph).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessorOwnerMethod {
    pub name: std::sync::Arc<str>,
    pub is_accessor: bool,
    pub self_call_targets: std::sync::Arc<[std::sync::Arc<str>]>,
}

/// The note every producer attaches to the 6.6:14 recursion diagnostic.
pub const ACCESSOR_RECURSION_NOTE: &str = "an accessor call is expanded by mandatory CFG splicing, so a cycle of accessor calls has no finite expansion";

/// 6.6:14: the diagnostic for an accessor that participates in an expansion
/// cycle.
pub fn accessor_recursion_error(method: &str) -> ErrorKind {
    ErrorKind::AccessorRecursion {
        method: method.to_owned(),
    }
}

/// 6.6:14 at the declaration seam: whether the accessor named `start` reaches
/// itself over the owner's `self`-receiver accessor-call edges.
///
/// An edge exists from one accessor to another exactly when the first's body
/// calls the second on `self` — `self.m()` where `m` is an accessor of the
/// same owner resolves to exactly that accessor, so the edge is certain with
/// no type resolved. Plain-method targets are not edges: an ordinary call is
/// not spliced, so it cannot extend an expansion.
pub fn accessor_self_call_cycle(start: &str, methods: &[AccessorOwnerMethod]) -> bool {
    let accessor = |name: &str| {
        methods
            .iter()
            .find(|method| method.is_accessor && method.name.as_ref() == name)
    };
    let Some(origin) = accessor(start) else {
        return false;
    };
    let mut pending: Vec<&str> = origin.self_call_targets.iter().map(AsRef::as_ref).collect();
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    while let Some(name) = pending.pop() {
        let Some(method) = accessor(name) else {
            continue;
        };
        if method.name.as_ref() == start {
            return true;
        }
        if seen.insert(name) {
            pending.extend(method.self_call_targets.iter().map(AsRef::as_ref));
        }
    }
    false
}
