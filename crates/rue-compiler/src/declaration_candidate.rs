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
