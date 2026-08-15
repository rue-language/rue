//! Stable, presemantic declaration candidates and position-free shell facts.
//!
//! These values are produced once at the parser boundary. They intentionally
//! cannot represent resolved constant identity: every source `const` remains
//! a [`DeclarationCandidateCategory::ConstCandidate`] until semantic
//! evaluation classifies its initializer.

use std::sync::Arc;

use crate::ModuleId;

/// Exhaustive syntax categories that may own a stable source occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DeclarationCandidateCategory {
    Function,
    ExternFunction,
    Struct,
    Enum,
    ConstCandidate,
    Destructor,
    Method,
    AssociatedFunction,
}

/// Stable owner of a named member candidate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeclarationCandidateOwner {
    pub(crate) category: DeclarationCandidateCategory,
    pub(crate) name: Arc<str>,
}

/// Durable identity of one syntax occurrence within a logical module.
///
/// The discriminator is counted only among otherwise equal candidates. It
/// therefore distinguishes malformed duplicates without coupling unrelated
/// declarations to global source order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeclarationCandidateKey {
    pub(crate) module: ModuleId,
    pub(crate) category: DeclarationCandidateCategory,
    pub(crate) name: Arc<str>,
    pub(crate) owner: Option<DeclarationCandidateOwner>,
    pub(crate) duplicate_discriminator: u32,
}

/// Position-free proof that one exact candidate key is present in a parsed
/// module. The occurrence family publishes only this capability metadata; the
/// parser-owned header remains private until the exact shell evaluator asks
/// for this key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclarationOccurrenceCapability {
    Exact {
        key: DeclarationCandidateKey,
        duplicate_multiplicity: u32,
    },
    Ambiguous {
        key: DeclarationCandidateKey,
        multiplicity: u32,
    },
}

impl DeclarationOccurrenceCapability {
    pub(crate) fn key(&self) -> &DeclarationCandidateKey {
        match self {
            Self::Exact { key, .. } | Self::Ambiguous { key, .. } => key,
        }
    }
}

/// Stable, position-free failure retained by the occurrence query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclarationOccurrenceFailure {
    ParseRejected { module: ModuleId },
}

/// Stable, position-free failure retained by the exact shell query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclarationShellFailure {
    OccurrencesUnavailable(DeclarationOccurrenceFailure),
    Absent(DeclarationCandidateKey),
    Ambiguous(DeclarationCandidateKey),
    ParserCapabilityMismatch(DeclarationCandidateKey),
}

/// Parser-validated source syntax for one constant, detached from its source
/// epoch and parser symbol universe.
///
/// Each fragment is the exact UTF-8 source spelling of the corresponding
/// syntax node, excluding the declaration's `:` / `=` / `;` separators. The
/// fragments contain no positional or interned handles and can therefore be
/// reparsed by a later standalone constant evaluator without retaining an AST,
/// RIR, resolver, or file table.
/// One value-position anonymous type literal transported from the frontend
/// (module) coordinate space into a durable declaration fragment.
///
/// `fragment_start` / `fragment_end` are byte offsets **relative to the start of
/// the reparsed fragment text** (the constant initializer, or the body block) —
/// not module offsets and not identity. They only reconnect the reparsed
/// literal to the anchor `AstGen` already minted for it, once the fragment
/// evaluator translates them into its own fragment-local coordinate space. The
/// `anchor` is the durable, definition-relative frontend anchor (position- and
/// trivia-insensitive by construction); it is the only identity-bearing field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RawAnonymousSite {
    pub(crate) fragment_start: u32,
    pub(crate) fragment_end: u32,
    pub(crate) kind: rue_rir::AnonymousTypeSiteKind,
    pub(crate) anchor: rue_rir::RirStructuralAnchor,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RawConstSyntax {
    pub(crate) declared_type: Option<Arc<str>>,
    pub(crate) initializer: Arc<str>,
    /// Anonymous type literals inside `initializer`, located relative to its
    /// start, each carrying its frontend anchor. Fail-closed transport for the
    /// durable comptime evaluator (RUE-1089).
    pub(crate) anonymous_sites: Arc<[RawAnonymousSite]>,
}

/// Stable, position-free failure retained by the exact raw-constant family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawConstSyntaxFailure {
    OccurrencesUnavailable(DeclarationOccurrenceFailure),
    Absent(DeclarationCandidateKey),
    Ambiguous(DeclarationCandidateKey),
    CategoryMismatch(DeclarationCandidateKey),
    ParserCapabilityMismatch(DeclarationCandidateKey),
}

/// Parser-validated signature syntax retained only for an anonymous member
/// produced during comptime evaluation, detached from its source epoch and
/// parser symbol universe.
///
/// Concatenating `declaration_fragments` reconstructs a body-free declaration
/// for that produced member. Named declarations instead project their exact
/// canonical parsed nodes directly in `compiler.semantic-nucleus`; this
/// transport remains only until anonymous members publish the same structured
/// per-declaration artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RawDeclarationSignatureSyntax {
    pub(crate) declaration_fragments: Arc<[Arc<str>]>,
    pub(crate) extern_abi: Option<Arc<str>>,
    /// Present only for a `-> borrow` accessor: the extra syntax its
    /// declaration rules read (spec 6.6:6, 6.6:7). `None` keeps every ordinary
    /// signature body-agnostic.
    pub(crate) accessor: Option<Arc<RawAccessorSignatureSyntax>>,
}

/// The syntax an accessor declaration's own legality rules read beyond its
/// signature (spec 6.6:6, 6.6:7).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RawAccessorSignatureSyntax {
    /// The accessor's exact body. Its declaration-shape rules include a
    /// trailing-yield check even when no caller demands body analysis, so this
    /// anonymous-member transport legitimately retains its own body.
    pub(crate) body: Arc<str>,
    /// Every method the accessor's owner declares — its name, whether it is
    /// itself an accessor, and the method names its body calls on its own
    /// `self` receiver — sorted by name and with ambiguous duplicate names
    /// dropped.
    ///
    /// 6.6:7 admits a method-call link in the yielded chain only when the
    /// callee is an accessor, and 6.6:14 rejects a cycle of accessor
    /// expansions (RUE-1282). For a link whose receiver is the accessor's own
    /// `self`, the callee is a method of this owner, and both deciding facts —
    /// the sibling's `-> borrow` qualifier and its own `self`-call targets —
    /// are *parsed* facts of the sibling declaration: no signature or body of
    /// that sibling is demanded, so there is no query cycle between mutually
    /// recursive accessors. Retaining the facts here is what records the
    /// dependency: this terminal is materialized from the module's parse, so
    /// editing a sibling method's `-> borrow` qualifier or `self`-call set
    /// changes this value and re-runs every consumer of this accessor's
    /// signature.
    ///
    /// Empty for an accessor with no owning type, or whose owner declares no
    /// other methods.
    pub(crate) owner_methods: Arc<[rue_air::declaration_validation::AccessorOwnerMethod]>,
}

/// Parser-validated syntax for one body-bearing declaration, detached from its
/// source epoch and parser symbol universe.
///
/// The fragment is exactly the declaration's body expression, including its
/// delimiters. It deliberately excludes the signature and trivia between the
/// signature's last token and the body. This is a syntax input only: runtime
/// body analysis remains a later query family, while declaration-time
/// comptime reduction can reparse this exact demanded producer without
/// requesting a whole-module RIR.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RawDeclarationBodySyntax {
    pub(crate) body: Arc<str>,
    /// Anonymous type literals inside `body`, located relative to its start,
    /// each carrying its frontend anchor. Fail-closed transport for the durable
    /// comptime evaluator (RUE-1089).
    pub(crate) anonymous_sites: Arc<[RawAnonymousSite]>,
}

/// Stable, position-free failure retained by the exact raw-body family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawDeclarationBodyFailure {
    OccurrencesUnavailable(DeclarationOccurrenceFailure),
    Absent(DeclarationCandidateKey),
    Ambiguous(DeclarationCandidateKey),
    CategoryMismatch(DeclarationCandidateKey),
    ParserCapabilityMismatch(DeclarationCandidateKey),
}

/// Position-independent identity of one valid `@import` occurrence inside an
/// exact declaration.
///
/// `occurrence` is counted in source order within the declaration, across all
/// specifiers. Keeping the exact decoded specifier in the key makes a stale or
/// malformed caller fail closed instead of silently selecting a different
/// import after an edit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeclarationImportSiteKey {
    pub(crate) declaration: DeclarationCandidateKey,
    pub(crate) occurrence: u32,
    pub(crate) specifier: Arc<str>,
}

/// Stable, position-free failure retained by the declaration-import family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclarationImportFailure {
    OccurrencesUnavailable(DeclarationOccurrenceFailure),
    AbsentDeclaration(DeclarationImportSiteKey),
    AmbiguousDeclaration(DeclarationImportSiteKey),
    CategoryMismatch(DeclarationImportSiteKey),
    SiteOutOfRange {
        key: DeclarationImportSiteKey,
        available: u32,
    },
    SpecifierMismatch {
        key: DeclarationImportSiteKey,
        actual: Arc<str>,
    },
    ParserCapabilityMismatch(DeclarationImportSiteKey),
    ResolutionUnavailable(DeclarationImportSiteKey),
}

impl DeclarationCandidateKey {
    pub(crate) fn stable_identity(&self) -> String {
        // Length-prefix every user-authored component. Query identities must
        // remain collision-free even when Rue identifiers contain separators.
        let owner = self.owner.as_ref().map_or_else(
            || "-".to_owned(),
            |owner| format!("{:?}:{}:{}", owner.category, owner.name.len(), owner.name),
        );
        format!(
            "{}:{}:{:?}:{}:{}:{}:{}",
            self.module.as_str().len(),
            self.module.as_str(),
            self.category,
            self.name.len(),
            self.name,
            owner,
            self.duplicate_discriminator
        )
    }
}

impl DeclarationImportSiteKey {
    pub(crate) fn stable_identity(&self) -> String {
        format!(
            "{}:import:{}:{}:{}",
            self.declaration.stable_identity(),
            self.occurrence,
            self.specifier.len(),
            self.specifier
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DeclarationParameterMode {
    Value,
    Borrow,
    Inout,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeclarationParameterHeader {
    pub(crate) name: Arc<str>,
    pub(crate) mode: DeclarationParameterMode,
    pub(crate) is_comptime: bool,
    pub(crate) is_type_parameter: bool,
}

/// Position-free header fact owned by the keyed declaration-shell family.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeclarationShellFact {
    pub(crate) key: DeclarationCandidateKey,
    pub(crate) is_public: bool,
    pub(crate) parameters: Arc<[DeclarationParameterHeader]>,
    pub(crate) receiver: Option<DeclarationParameterMode>,
    pub(crate) receiver_is_mut: bool,
    pub(crate) is_generic: bool,
    pub(crate) is_unchecked: bool,
    pub(crate) is_extern: bool,
    /// Hash of parser-authored signature partitions with whitespace removed.
    /// Bodies and constant initializers are deliberately excluded.
    pub(crate) signature_fingerprint: [u8; 32],
}
