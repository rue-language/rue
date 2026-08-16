//! Durable declaration-transport vocabulary shared with the compiler's
//! query graph.
//!
//! The compiler's revisioned queries construct [`SemanticDeclarationShell`]s
//! from durable declaration truth and report structural work through the
//! `*Work` counters here. Every type is position-free and owns its data: no
//! `Type`, pool ID, interner symbol, or RIR handle crosses this boundary.

use std::sync::Arc;

use rue_rir::RirParamMode;
use rue_span::{FileId, Span};

use crate::{StableDefinitionKind, StableDefinitionNamespace};

/// A nominal declaration identity valid for one successful binding request.
/// Consumers must join this identity to their own stable definition universe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticNominalIdentity {
    pub file_id: FileId,
    pub name: Arc<str>,
    pub kind: StableDefinitionKind,
}

/// Position-independent definition endpoint used inside an anonymous nominal
/// producer identity while crossing the declaration-query boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SemanticDefinitionIdentity {
    pub file_id: FileId,
    pub name: Arc<str>,
    pub owner: Option<Arc<str>>,
    pub kind: StableDefinitionKind,
}

pub type SemanticAnonymousNominalIdentity =
    crate::AnonymousNominalKey<SemanticDefinitionIdentity, Arc<str>>;

/// An owned resolved type with no `Type`, pool ID, interner symbol, or RIR handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticExportType {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,
    Unit,
    Never,
    ComptimeType,
    GenericParameter(u32),
    /// A compiler-injected nominal whose identity is independent of source
    /// file numbering. This must never be encoded as a synthetic `FileId`.
    BuiltinNominal {
        name: Arc<str>,
        kind: crate::SemanticImportNominalKind,
    },
    Nominal(SemanticNominalIdentity),
    AnonymousNominal(SemanticAnonymousNominalIdentity),
    Array {
        element: Box<Self>,
        len: u64,
    },
    /// A second-class slice view. The syntax name is carried explicitly so
    /// the AIR epoch can materialize the ordinary synthetic nominal without
    /// reparsing or re-resolving its element type.
    Slice {
        element: Box<Self>,
        name: Arc<str>,
    },
    PtrConst(Box<Self>),
    PtrMut(Box<Self>),
    /// Resolved module path. It is deliberately converted by the compiler
    /// during the callback rather than retained as AIR's request-local ModuleId.
    Module(Arc<str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticParameterMode {
    Value,
    Borrow,
    Inout,
}

/// Structural descriptors for one completed declaration-binding pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeclarationBindingWork {
    pub bind_invocations: usize,
    /// Module-local collision validation and builtin/module namespace setup.
    pub namespace_setup_invocations: usize,
    /// Deterministic named struct/enum shell predeclaration.
    pub nominal_type_predeclaration_invocations: usize,
    /// Deterministic callable/value identity predeclaration, before payload
    /// resolution or constant evaluation.
    pub callable_value_predeclaration_invocations: usize,
    pub callable_value_shells_predeclared: usize,
    /// Declaration-index records visited while predeclaring callable, value,
    /// and nominal shells. This must equal the number of produced shells.
    pub indexed_declaration_records_visited: usize,
    /// Resolution of declaration payloads, constants, and cycles.
    pub declaration_resolution_invocations: usize,
    /// Declaration-resolution invocations that returned diagnostics before a
    /// body-analysis-ready binder could be finalized.
    pub declaration_resolution_failures: usize,
    /// Construction of the body-analysis-ready state.
    pub body_readiness_finalization_invocations: usize,
    /// Durable payload installation attempts at the declaration-shell seam.
    pub durable_install_invocations: usize,
    pub durable_payloads_installed: usize,
    /// Size of the input RIR, not a claim that binding visited every entry.
    pub input_rir_instructions: usize,
    /// Canonical modules inserted into this semantic epoch's compact registry.
    /// This is a production-path count of registration work, not a module
    /// universe size inferred by a consumer.
    pub modules_registered: usize,
    pub declaration_index_build_invocations: usize,
    pub indexed_free_functions: usize,
    pub indexed_named_methods: usize,
    pub indexed_anonymous_methods: usize,
    pub indexed_destructors: usize,
    pub indexed_const_candidates: usize,
}

/// Stable-joinable identity of a declaration whose semantic payload is not yet
/// installed. `module_path` is the caller-provided logical symbol path; neither
/// the request-local `FileId` nor an RIR arena offset participates in identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticDeclarationShellIdentity {
    pub module_path: Arc<str>,
    pub is_trusted_standard_library: bool,
    pub namespace: StableDefinitionNamespace,
    pub kind: StableDefinitionKind,
    pub name: Arc<str>,
    pub owner: Option<Arc<str>>,
}

/// Current-revision syntax metadata retained separately from resolved payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticDeclarationShell {
    pub identity: SemanticDeclarationShellIdentity,
    pub declaration_span: Span,
    pub parameter_names: Arc<[Arc<str>]>,
    pub parameter_modes: Arc<[RirParamMode]>,
    pub parameter_comptime: Arc<[bool]>,
    pub source_order: u32,
    pub has_self: bool,
    pub receiver_mode: Option<RirParamMode>,
    pub receiver_is_mut: bool,
    pub is_generic: bool,
    pub is_public: bool,
    pub is_unchecked: bool,
    /// Whether this is a foreign `extern "C"` declaration (ADR-0064 C FFI).
    pub is_extern: bool,
    /// Parser/token-algebra signature identity. Current positions and payloads
    /// never participate.
    pub signature_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticBindingManifestWork {
    pub build_invocations: usize,
    pub rir_instructions_visited: usize,
    pub bindings_emitted: usize,
    pub functions_emitted: usize,
    pub types_emitted: usize,
    pub constants_emitted: usize,
    pub module_bindings_emitted: usize,
    pub destructors_emitted: usize,
    pub named_methods_emitted: usize,
    pub named_method_edges_visited: usize,
    pub anonymous_methods_deferred: usize,
    pub parser_invocations: usize,
    pub ast_payload_clones: usize,
    pub source_text_clones: usize,
}
