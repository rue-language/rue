//! Owned semantic bindings emitted only after declaration binding succeeds.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use lasso::Spur;
use rue_error::{CompileError, CompileErrors, CompileResult, ErrorKind, MultiErrorResult};
use rue_rir::{InstData, InstRef, RirParamMode};
use rue_span::{FileId, Span};

use super::RirDeclarationIndexWork;
use super::{ConstValue, Sema, SemaOutput};
use crate::types::{
    ArrayLen, PtrMutability, Type, TypeKind, parse_array_type_syntax, parse_pointer_type_syntax,
};
use crate::{StableDefinitionKind, StableDefinitionNamespace};

/// A nominal declaration identity valid for one successful binding request.
/// Consumers must join this identity to their own stable definition universe
/// while the callback passed to [`BoundSema::with_declaration_semantics`] runs.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticAnonymousNominalExport {
    pub identity: SemanticAnonymousNominalIdentity,
    pub shape: SemanticAnonymousNominalShape,
    pub type_captures: Arc<[(Arc<str>, SemanticExportType)]>,
    pub value_captures: Arc<[(Arc<str>, SemanticExportConstValue)]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticAnonymousNominalShape {
    Struct {
        fields: Arc<[(Arc<str>, SemanticExportType)]>,
        methods: Arc<[SemanticAnonymousMethodSignature]>,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[SemanticExportType]>)]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticAnonymousMethodSignature {
    pub name: Arc<str>,
    pub has_self: bool,
    pub self_mode: SemanticParameterMode,
    pub parameters: Arc<[(SemanticAnonymousMethodType, SemanticParameterMode, bool)]>,
    pub result: SemanticAnonymousMethodType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticAnonymousMethodType {
    SelfType,
    Concrete(SemanticExportType),
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticParameterMode {
    Value,
    Borrow,
    Inout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticExportParameter {
    pub name: Arc<str>,
    pub ty: SemanticExportType,
    pub mode: SemanticParameterMode,
    pub is_comptime: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticExportConstValue {
    Integer(i128),
    Bool(bool),
    Type(SemanticExportType),
    Function {
        file_id: FileId,
        name: Arc<str>,
    },
    Unit,
    /// String constant content (RUE-957), carried as the literal text.
    String(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticDeclarationPayload {
    Callable {
        parameters: Arc<[SemanticExportParameter]>,
        result: SemanticExportType,
        has_self: bool,
        self_mode: SemanticParameterMode,
        is_unchecked: bool,
    },
    Struct {
        fields: Arc<[(Arc<str>, SemanticExportType)]>,
        is_copy: bool,
        is_linear: bool,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[SemanticExportType]>)]>,
    },
    Const {
        ty: SemanticExportType,
        value: SemanticExportConstValue,
    },
    /// A top-level constant whose initializer resolved to a module. The
    /// target is the canonical semantic epoch's durable module identity, not
    /// its compact request-local module handle.
    ModuleBinding {
        target: SemanticExportType,
    },
    Destructor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticDeclarationExport {
    pub identity: SemanticBinding,
    pub payload: SemanticDeclarationPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticExportFailure {
    ErrorType,
    AnonymousNominalType,
    UnmappedNominalType,
    UnmappedFunction,
    UnsupportedParameterMode,
    UnsupportedGenericSignature,
    RecursiveStructuralType,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticDeclarationExportWork {
    pub build_invocations: usize,
    pub declarations_exported: usize,
    pub rir_instructions_visited: usize,
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

/// Diagnostics and value-only structural work from failed declaration
/// resolution. Partially resolved semantic state remains private.
#[derive(Debug, Clone)]
pub struct DeclarationResolutionFailure {
    errors: CompileErrors,
    work: DeclarationBindingWork,
}

impl DeclarationResolutionFailure {
    fn new(errors: CompileErrors, work: DeclarationBindingWork) -> Self {
        Self { errors, work }
    }

    pub fn work(&self) -> DeclarationBindingWork {
        self.work
    }

    pub fn into_errors(self) -> CompileErrors {
        self.errors
    }
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

#[derive(Debug, Clone, Copy)]
pub(super) enum DeclarationPayloadSource {
    Callable { body: InstRef },
    Const { initializer: InstRef },
    Destructor { body: InstRef },
}

#[derive(Debug, Clone)]
pub(super) struct PendingDeclarationPayload {
    pub shell: SemanticDeclarationShell,
    pub declaration: InstRef,
    pub source: DeclarationPayloadSource,
}

#[derive(Debug, Clone)]
pub(super) struct PendingNominalPayload {
    pub shell: SemanticDeclarationShell,
    pub declaration: InstRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationInstallFailure {
    /// Stable endpoint validation failed before installation.
    EndpointResolution(crate::SemanticStableResolutionFailure),
    DuplicatePayload,
    MissingPayload,
    UnexpectedPayload,
    IdentityMismatch,
    KindMismatch,
    VisibilityMismatch,
    CallableShapeMismatch,
    NominalShapeMismatch,
    MissingNominal,
    UnsupportedType,
    UnsupportedDeclaration,
    /// Two distinct anonymous producer-nominal keys hashed to one 128-bit symbol
    /// digest while installing a durable nominal (RUE-1089, Theme 4b). The digest
    /// is a presentation name only and must never collapse producer-distinct
    /// types; the carried message names both stable keys and the digest. Surfaces
    /// as a fail-closed E9000 internal error.
    AnonymousDigestCollision(Box<str>),
    /// Two `extern "C"` foreign declarations name the same C symbol with
    /// disagreeing signatures (RUE-1218, spec 9.3:5). Unlike every other
    /// variant here this is a *source* error, not a broken installation
    /// invariant, so it carries the finished diagnostic for the caller to
    /// report verbatim instead of collapsing into an internal error.
    ForeignSignatureConflict(Box<CompileError>),
}

impl From<crate::SemanticStableResolutionFailure> for DeclarationInstallFailure {
    fn from(failure: crate::SemanticStableResolutionFailure) -> Self {
        Self::EndpointResolution(failure)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBinding {
    /// Request-local source file containing this declaration.
    pub file_id: FileId,
    /// Request-local declaration location; excluded from stable identity.
    pub declaration_span: Span,
    pub namespace: StableDefinitionNamespace,
    pub kind: StableDefinitionKind,
    pub name: Arc<str>,
    pub owner: Option<Arc<str>>,
    /// Source visibility; destructors are never public.
    pub is_public: bool,
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

#[derive(Debug, Clone)]
pub struct SemanticBindingManifest {
    bindings: Arc<[SemanticBinding]>,
    work: SemanticBindingManifestWork,
}

impl SemanticBindingManifest {
    /// Successful bindings in deterministic declaration order, with named
    /// methods grouped beneath their owning named struct.
    pub fn bindings(&self) -> &[SemanticBinding] {
        &self.bindings
    }
    pub fn work(&self) -> SemanticBindingManifestWork {
        self.work
    }
}

/// A bound semantic request.
///
pub struct BoundSema<'a> {
    pub(super) sema: super::BodySema<'a>,
    manifest: OnceLock<SemanticBindingManifest>,
    binding_work: DeclarationBindingWork,
}

/// A semantic request whose module-keyed declaration namespace and nominal shells
/// are complete, but whose declaration payloads have not yet been resolved.
///
/// This boundary deliberately owns the request-local `Sema` state.  A future
/// durable-declaration importer can populate that state here without making
/// raw AIR handles part of the reusable representation.  Today the only
/// transition performs the ordinary current-revision resolution pass.
///
pub struct DeclarationShells<'a> {
    pub(super) sema: Sema<'a, super::MutableDeclarations>,
    pub(super) binding_work: DeclarationBindingWork,
    pub(super) pending_payloads: Vec<PendingDeclarationPayload>,
    pub(super) pending_nominals: Vec<PendingNominalPayload>,
}

impl<'a> DeclarationShells<'a> {
    pub fn install_stable_identity_endpoints(
        mut self,
        definitions: &[crate::SemanticDefinitionEndpoint],
        modules: &[crate::SemanticModuleEndpoint],
    ) -> Result<Self, crate::SemanticStableResolutionFailure> {
        let expected_definitions = self
            .declaration_shells()
            // Const shells are presemantic candidates, not stable definition
            // identities. They receive endpoints only after initializer
            // evaluation classifies ValueConst versus ModuleBinding.
            .filter(|shell| shell.identity.kind != StableDefinitionKind::ValueConst)
            .map(|shell| {
                (
                    shell.declaration_span.file_id.index(),
                    shell.identity.name.to_string(),
                    shell.identity.owner.as_deref().map(str::to_owned),
                    shell.identity.kind,
                )
            })
            .collect::<Vec<_>>();
        let expected_modules = stable_module_files(&self.sema);
        install_stable_identity_endpoints(
            &mut self.sema,
            definitions,
            modules,
            &expected_definitions,
            &expected_modules,
        )?;
        install_const_candidate_endpoints(&mut self.sema, &self.pending_payloads)?;
        Ok(self)
    }

    /// Work completed before declaration payload resolution.
    pub fn binding_work(&self) -> DeclarationBindingWork {
        self.binding_work
    }

    /// Deterministic identities and source metadata available before semantic
    /// payload resolution. Arena references remain private to this request.
    pub fn callable_value_shells(
        &self,
    ) -> impl ExactSizeIterator<Item = &SemanticDeclarationShell> {
        self.pending_payloads.iter().map(|pending| &pending.shell)
    }

    /// All stable-joinable declaration shells in deterministic category order:
    /// nominal types by stable identity, followed by callable/value shells by
    /// stable identity.
    pub fn declaration_shells(&self) -> impl Iterator<Item = &SemanticDeclarationShell> {
        self.pending_nominals
            .iter()
            .map(|pending| &pending.shell)
            .chain(self.pending_payloads.iter().map(|pending| &pending.shell))
    }

    /// Test-support adapter for resolving source-owned declaration payloads.
    #[doc(hidden)]
    pub fn resolve_declarations_for_test(self) -> MultiErrorResult<BoundSema<'a>> {
        self.resolve_declarations_with_work_for_test()
            .map_err(DeclarationResolutionFailure::into_errors)
    }

    #[cfg(test)]
    pub fn resolve_declarations(self) -> MultiErrorResult<BoundSema<'a>> {
        self.resolve_declarations_for_test()
    }

    /// Test-support adapter retaining exact work on source-owned failure.
    #[doc(hidden)]
    pub fn resolve_declarations_with_work_for_test(
        mut self,
    ) -> Result<BoundSema<'a>, DeclarationResolutionFailure> {
        // This is the explicit payload-install boundary. Const resolution may
        // inspect only the exact occurrence set admitted by these shells. In a
        // canonical epoch that set came from keyed compiler queries; the RIR
        // supplies current-epoch locators and initializer payloads, not an
        // independent declaration-discovery authority.
        debug_assert!(
            self.pending_payloads
                .iter()
                .all(
                    |pending| pending.declaration.as_u32() < self.sema.rir.len() as u32
                        && match pending.source {
                            DeclarationPayloadSource::Callable { body }
                            | DeclarationPayloadSource::Destructor { body } =>
                                body.as_u32() < self.sema.rir.len() as u32,
                            DeclarationPayloadSource::Const { initializer } =>
                                initializer.as_u32() < self.sema.rir.len() as u32,
                        }
                )
        );
        debug_assert!(
            self.pending_nominals
                .iter()
                .all(|pending| pending.declaration.as_u32() < self.sema.rir.len() as u32)
        );
        self.sema.bound_const_candidates = Some(std::sync::Arc::new(
            super::declaration_index::BoundConstCandidateIndex::new(
                self.sema.rir,
                self.pending_payloads.iter().filter_map(|pending| {
                    matches!(pending.source, DeclarationPayloadSource::Const { .. })
                        .then_some((pending.shell.source_order, pending.declaration))
                }),
            ),
        ));
        self.binding_work.declaration_resolution_invocations += 1;
        if let Err(error) = self.sema.resolve_declarations_for_test() {
            self.binding_work.declaration_resolution_failures += 1;
            return Err(DeclarationResolutionFailure::new(
                CompileErrors::from(error),
                self.binding_work,
            ));
        }
        self.binding_work.body_readiness_finalization_invocations += 1;
        Ok(self.sema.into_bound_with_work(self.binding_work))
    }

    #[cfg(test)]
    pub fn resolve_declarations_with_work(
        self,
    ) -> Result<BoundSema<'a>, DeclarationResolutionFailure> {
        self.resolve_declarations_with_work_for_test()
    }

    /// Install a fully validated, current-revision projection of durable
    /// declaration semantics into these freshly predeclared shells.
    ///
    /// Failure consumes the shells, so partially populated request-local state
    /// can never escape.
    pub fn install_declaration_semantics(
        self,
        exports: &[SemanticDeclarationExport],
    ) -> Result<BoundSema<'a>, DeclarationInstallFailure> {
        self.install_declaration_semantics_with_anonymous(exports, &[])
    }

    pub fn install_declaration_semantics_with_anonymous(
        mut self,
        exports: &[SemanticDeclarationExport],
        anonymous_nominals: &[SemanticAnonymousNominalExport],
    ) -> Result<BoundSema<'a>, DeclarationInstallFailure> {
        use std::collections::BTreeMap;

        self.binding_work.durable_install_invocations += 1;
        let mut records = BTreeMap::new();
        for export in exports {
            if matches!(
                export.payload,
                SemanticDeclarationPayload::ModuleBinding { .. }
            ) != (export.identity.kind == StableDefinitionKind::ModuleBinding)
            {
                return Err(DeclarationInstallFailure::KindMismatch);
            }
            let module_path = self
                .sema
                .get_symbol_path(export.identity.file_id)
                .map(crate::path_norm::normalize_module_path)
                .unwrap_or_else(|| format!("file{}", export.identity.file_id.index()));
            let identity = SemanticDeclarationShellIdentity {
                module_path: Arc::from(module_path),
                is_trusted_standard_library: self
                    .sema
                    .trusted_standard_library_files
                    .contains(&export.identity.file_id),
                namespace: export.identity.namespace,
                // Const shells are captured before initializer evaluation can
                // classify module bindings. The distinct payload and exported
                // kind are validated above, then joined to that exact const
                // shell for installation.
                kind: if export.identity.kind == StableDefinitionKind::ModuleBinding {
                    StableDefinitionKind::ValueConst
                } else {
                    export.identity.kind
                },
                name: export.identity.name.clone(),
                owner: export.identity.owner.clone(),
            };
            if records.insert(identity, export).is_some() {
                return Err(DeclarationInstallFailure::DuplicatePayload);
            }
        }
        let expected = self.pending_nominals.len() + self.pending_payloads.len();
        if records.len() != expected {
            return Err(DeclarationInstallFailure::MissingPayload);
        }
        for shell in self.declaration_shells() {
            let record = records
                .get(&shell.identity)
                .ok_or(DeclarationInstallFailure::MissingPayload)?;
            if record.identity.is_public != shell.is_public {
                return Err(DeclarationInstallFailure::VisibilityMismatch);
            }
        }

        // Resolve every module target before mutating any nominal or callable
        // state. An absent or ambiguous durable ID therefore fails the whole
        // installation atomically.
        let mut module_by_durable_id = BTreeMap::new();
        for index in 0..self.sema.module_registry.len() {
            let id = crate::ModuleId::new(index as u32);
            let durable_id = self.sema.module_registry.get_def(id).durable_id;
            if module_by_durable_id.insert(durable_id, id).is_some() {
                return Err(DeclarationInstallFailure::UnsupportedType);
            }
        }
        let mut module_targets = BTreeMap::new();
        for pending in &self.pending_payloads {
            let record = records[&pending.shell.identity];
            if let SemanticDeclarationPayload::ModuleBinding { target } = &record.payload {
                let SemanticExportType::Module(target) = target else {
                    return Err(DeclarationInstallFailure::UnsupportedType);
                };
                let target = *module_by_durable_id
                    .get(target.as_ref())
                    .ok_or(DeclarationInstallFailure::UnsupportedType)?;
                let InstData::ConstDecl { ty: None, .. } =
                    &self.sema.rir.get(pending.declaration).data
                else {
                    return Err(DeclarationInstallFailure::KindMismatch);
                };
                module_targets.insert(pending.shell.identity.clone(), Type::new_module(target));
            }
        }

        // Query-owned declaration payloads can mention synthetic string types
        // before body analysis has had an opportunity to demand them. Create
        // those ordinary AIR builtins at the declaration-install boundary so
        // importing `str` and `Str(N)` has the same epoch-local identity as a
        // cold semantic request.
        fn collect_string_builtins(
            ty: &SemanticExportType,
            needs_str: &mut bool,
            fixed: &mut std::collections::BTreeSet<u64>,
        ) -> Result<(), DeclarationInstallFailure> {
            match ty {
                SemanticExportType::BuiltinNominal { name, kind }
                    if *kind == crate::SemanticImportNominalKind::Struct =>
                {
                    if name.as_ref() == "str" {
                        *needs_str = true;
                    } else if let Some(capacity) = name
                        .strip_prefix("Str(")
                        .and_then(|value| value.strip_suffix(')'))
                    {
                        fixed.insert(
                            capacity
                                .parse()
                                .map_err(|_| DeclarationInstallFailure::UnsupportedType)?,
                        );
                    }
                }
                SemanticExportType::Array { element, .. }
                | SemanticExportType::Slice { element, .. }
                | SemanticExportType::PtrConst(element)
                | SemanticExportType::PtrMut(element) => {
                    collect_string_builtins(element, needs_str, fixed)?;
                }
                _ => {}
            }
            Ok(())
        }
        let mut needs_str = false;
        let mut fixed_strings = std::collections::BTreeSet::new();
        for record in records.values() {
            match &record.payload {
                SemanticDeclarationPayload::Callable {
                    parameters, result, ..
                } => {
                    for parameter in parameters.iter() {
                        collect_string_builtins(&parameter.ty, &mut needs_str, &mut fixed_strings)?;
                    }
                    collect_string_builtins(result, &mut needs_str, &mut fixed_strings)?;
                }
                SemanticDeclarationPayload::Struct { fields, .. } => {
                    for (_, ty) in fields.iter() {
                        collect_string_builtins(ty, &mut needs_str, &mut fixed_strings)?;
                    }
                }
                SemanticDeclarationPayload::Enum { variants } => {
                    for (_, payload) in variants.iter() {
                        for ty in payload.iter() {
                            collect_string_builtins(ty, &mut needs_str, &mut fixed_strings)?;
                        }
                    }
                }
                SemanticDeclarationPayload::Const { ty, value } => {
                    collect_string_builtins(ty, &mut needs_str, &mut fixed_strings)?;
                    if let SemanticExportConstValue::Type(ty) = value {
                        collect_string_builtins(ty, &mut needs_str, &mut fixed_strings)?;
                    }
                }
                SemanticDeclarationPayload::ModuleBinding { .. }
                | SemanticDeclarationPayload::Destructor => {}
            }
        }
        if needs_str {
            self.sema
                .get_or_create_str_struct(Span::default())
                .map_err(|_| DeclarationInstallFailure::UnsupportedType)?;
        }
        for capacity in fixed_strings {
            self.sema
                .get_or_create_str_fixed_struct(capacity, Span::default())
                .map_err(|_| DeclarationInstallFailure::UnsupportedType)?;
        }

        // Slice DTOs carry the already-resolved element type. Materialize
        // them from the inside out before payload installation, so all later
        // type imports are pure identity lookups within this AIR epoch.
        fn collect_slices(
            ty: &SemanticExportType,
            slices: &mut Vec<(Arc<str>, SemanticExportType)>,
        ) {
            match ty {
                SemanticExportType::Slice { element, name } => {
                    collect_slices(element, slices);
                    slices.push((name.clone(), (**element).clone()));
                }
                SemanticExportType::Array { element, .. }
                | SemanticExportType::PtrConst(element)
                | SemanticExportType::PtrMut(element) => collect_slices(element, slices),
                _ => {}
            }
        }
        let mut slices = Vec::new();
        for record in records.values() {
            match &record.payload {
                SemanticDeclarationPayload::Callable {
                    parameters, result, ..
                } => {
                    for parameter in parameters.iter() {
                        collect_slices(&parameter.ty, &mut slices);
                    }
                    collect_slices(result, &mut slices);
                }
                SemanticDeclarationPayload::Struct { fields, .. } => {
                    for (_, ty) in fields.iter() {
                        collect_slices(ty, &mut slices);
                    }
                }
                SemanticDeclarationPayload::Enum { variants } => {
                    for (_, payload) in variants.iter() {
                        for ty in payload.iter() {
                            collect_slices(ty, &mut slices);
                        }
                    }
                }
                SemanticDeclarationPayload::Const { ty, value } => {
                    collect_slices(ty, &mut slices);
                    if let SemanticExportConstValue::Type(ty) = value {
                        collect_slices(ty, &mut slices);
                    }
                }
                SemanticDeclarationPayload::ModuleBinding { .. }
                | SemanticDeclarationPayload::Destructor => {}
            }
        }
        for (name, element) in slices {
            // Generic-dependent signatures are represented by the ordinary
            // declaration placeholder until specialization, so there is no
            // concrete slice nominal to install yet.
            if contains_generic_parameter(&element) {
                continue;
            }
            let element = self.sema.import_export_type(&element)?;
            self.sema
                .get_or_create_slice_struct_from_element(name.as_ref(), element, Span::default())
                .map_err(|_| DeclarationInstallFailure::UnsupportedType)?;
        }

        fn contains_generic_parameter(ty: &SemanticExportType) -> bool {
            fn identity_type_contains_generic(
                ty: &crate::TypeInstanceKey<SemanticDefinitionIdentity, Arc<str>>,
            ) -> bool {
                match ty {
                    crate::TypeInstanceKey::GenericParameter(_) => true,
                    crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(
                        identity,
                    )) => identity
                        .arguments
                        .types
                        .iter()
                        .any(identity_type_contains_generic),
                    crate::TypeInstanceKey::Array { element, .. }
                    | crate::TypeInstanceKey::Slice { element, .. }
                    | crate::TypeInstanceKey::PtrConst(element)
                    | crate::TypeInstanceKey::PtrMut(element) => {
                        identity_type_contains_generic(element)
                    }
                    _ => false,
                }
            }
            match ty {
                SemanticExportType::GenericParameter(_) => true,
                SemanticExportType::AnonymousNominal(identity) => identity
                    .arguments
                    .types
                    .iter()
                    .any(identity_type_contains_generic),
                SemanticExportType::Array { element, .. }
                | SemanticExportType::Slice { element, .. }
                | SemanticExportType::PtrConst(element)
                | SemanticExportType::PtrMut(element) => contains_generic_parameter(element),
                _ => false,
            }
        }
        fn producer_definition(
            producer: &crate::StableProducerId<SemanticDefinitionIdentity, Arc<str>>,
        ) -> Option<&SemanticDefinitionIdentity> {
            fn function_definition(
                function: &crate::FunctionInstanceKey<SemanticDefinitionIdentity, Arc<str>>,
            ) -> Option<&SemanticDefinitionIdentity> {
                match function {
                    crate::FunctionInstanceKey::Definition(definition) => Some(definition),
                    crate::FunctionInstanceKey::Specialization { base, .. } => {
                        function_definition(base)
                    }
                    crate::FunctionInstanceKey::AnonymousMember { owner, .. }
                    | crate::FunctionInstanceKey::DropGlue(owner) => type_definition(owner),
                }
            }
            fn type_definition(
                ty: &crate::TypeInstanceKey<SemanticDefinitionIdentity, Arc<str>>,
            ) -> Option<&SemanticDefinitionIdentity> {
                match ty {
                    crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(
                        definition,
                    )) => Some(definition),
                    crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(
                        identity,
                    )) => producer_definition(&identity.producer),
                    crate::TypeInstanceKey::Array { element, .. }
                    | crate::TypeInstanceKey::Slice { element, .. }
                    | crate::TypeInstanceKey::PtrConst(element)
                    | crate::TypeInstanceKey::PtrMut(element) => type_definition(element),
                    _ => None,
                }
            }
            match producer {
                crate::StableProducerId::Definition(definition) => Some(definition),
                crate::StableProducerId::Function(function) => function_definition(function),
            }
        }

        fn import_capture_value(
            sema: &mut Sema<'_, super::MutableDeclarations>,
            value: &SemanticExportConstValue,
        ) -> Result<ConstValue, DeclarationInstallFailure> {
            Ok(match value {
                SemanticExportConstValue::Integer(value) => ConstValue::Integer(*value),
                SemanticExportConstValue::Bool(value) => ConstValue::Bool(*value),
                SemanticExportConstValue::Type(value) => {
                    ConstValue::Type(sema.import_export_type(value)?)
                }
                SemanticExportConstValue::Function { file_id, name } => {
                    let source = sema.interner.get_or_intern(name.as_ref());
                    ConstValue::Function(
                        sema.resolve_function_name_local(source, *file_id)
                            .or_else(|| {
                                sema.functions_by_file_name
                                    .get(&(*file_id, source))
                                    .copied()
                            })
                            .ok_or(DeclarationInstallFailure::MissingPayload)?,
                    )
                }
                SemanticExportConstValue::Unit => ConstValue::Unit,
                SemanticExportConstValue::String(value) => {
                    ConstValue::String(sema.interner.get_or_intern(value.as_ref()))
                }
            })
        }

        fn import_method_type(
            sema: &mut Sema<'_, super::MutableDeclarations>,
            value: &SemanticAnonymousMethodType,
        ) -> Result<super::AnonMethodType, DeclarationInstallFailure> {
            Ok(match value {
                SemanticAnonymousMethodType::SelfType => super::AnonMethodType::SelfType,
                SemanticAnonymousMethodType::Concrete(value) => {
                    super::AnonMethodType::Concrete(sema.import_export_type(value)?)
                }
            })
        }

        fn rir_mode(mode: SemanticParameterMode) -> RirParamMode {
            match mode {
                SemanticParameterMode::Value => RirParamMode::Normal,
                SemanticParameterMode::Borrow => RirParamMode::Borrow,
                SemanticParameterMode::Inout => RirParamMode::Inout,
            }
        }

        let mut pending_anonymous = anonymous_nominals.iter().collect::<Vec<_>>();
        while !pending_anonymous.is_empty() {
            let mut progressed = false;
            let mut next = Vec::new();
            for nominal in pending_anonymous {
                let is_generic_shape = match &nominal.shape {
                    SemanticAnonymousNominalShape::Struct { fields, .. } => {
                        fields.iter().any(|(_, ty)| contains_generic_parameter(ty))
                    }
                    SemanticAnonymousNominalShape::Enum { variants } => variants
                        .iter()
                        .any(|(_, payload)| payload.iter().any(contains_generic_parameter)),
                };
                if is_generic_shape {
                    // Generic declaration signatures use AIR's established
                    // top-level comptime placeholder. The concrete anonymous
                    // nominal is materialized by the keyed comptime-call query
                    // after it receives canonical arguments.
                    progressed = true;
                    continue;
                }
                let installed = (|| -> Result<(), DeclarationInstallFailure> {
                    let identity = self.sema.import_anonymous_identity(&nominal.identity)?;
                    match &nominal.shape {
                        SemanticAnonymousNominalShape::Struct { fields, methods } => {
                            let fields = fields
                                .iter()
                                .map(|(name, ty)| {
                                    Ok(crate::types::StructField {
                                        name: name.to_string(),
                                        ty: self.sema.import_export_type(ty)?,
                                    })
                                })
                                .collect::<Result<Vec<_>, DeclarationInstallFailure>>()?;
                            let source = producer_definition(&nominal.identity.producer);
                            let source_span = source.and_then(|source| {
                                self.pending_payloads
                                    .iter()
                                    .find(|pending| {
                                        pending.shell.declaration_span.file_id == source.file_id
                                            && pending.shell.identity.name == source.name
                                            && pending.shell.identity.owner == source.owner
                                            && pending.shell.identity.kind == source.kind
                                    })
                                    .map(|pending| pending.shell.declaration_span)
                            });
                            let query_occurrence = nominal.identity.anchor.segments().last();
                            let method_materialization = source_span.and_then(|source_span| {
                                let candidates = self
                                    .sema
                                    .rir
                                    .iter()
                                    .filter_map(|(_, instruction)| match &instruction.data {
                                        InstData::AnonStructType {
                                            methods: rir_methods,
                                            anchor,
                                            ..
                                        } if anchor.segments().last() == query_occurrence
                                            && instruction.span.file_id == source_span.file_id
                                            && instruction.span.start >= source_span.start
                                            && instruction.span.end <= source_span.end =>
                                        {
                                            let method_refs =
                                                self.sema.rir.anon_struct_methods(rir_methods);
                                            let mut source_names = method_refs
                                                .iter()
                                                .filter_map(|method_ref| {
                                                    let InstData::FnDecl { name, .. } =
                                                        &self.sema.rir.get(*method_ref).data
                                                    else {
                                                        return None;
                                                    };
                                                    Some(self.sema.interner.resolve(name))
                                                })
                                                .collect::<Vec<_>>();
                                            let mut projected_names = methods
                                                .iter()
                                                .map(|method| method.name.as_ref())
                                                .collect::<Vec<_>>();
                                            source_names.sort_unstable();
                                            projected_names.sort_unstable();
                                            (source_names == projected_names)
                                                .then(|| (rir_methods.clone(), anchor.clone()))
                                        }
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>();
                                // Producer-nominal identity is exact (RUE-1089):
                                // the durable anchor transported from the frontend
                                // equals the anchor AstGen minted on this RIR
                                // nominal, so only a full-anchor match resolves the
                                // method materialization. No relative-occurrence
                                // fallback: divergence fails closed downstream.
                                candidates
                                    .iter()
                                    .find(|(_, anchor)| anchor == &nominal.identity.anchor)
                                    .cloned()
                            });
                            let has_methods = !methods.is_empty();
                            if has_methods && method_materialization.is_none() {
                                return Err(DeclarationInstallFailure::MissingPayload);
                            }
                            let type_captures = if has_methods {
                                nominal
                                    .type_captures
                                    .iter()
                                    .map(|(name, ty)| {
                                        Ok((
                                            self.sema.interner.get_or_intern(name.as_ref()),
                                            self.sema.import_export_type(ty)?,
                                        ))
                                    })
                                    .collect::<Result<HashMap<_, _>, DeclarationInstallFailure>>()?
                            } else {
                                HashMap::new()
                            };
                            let value_captures = if has_methods {
                                nominal
                                    .value_captures
                                    .iter()
                                    .map(|(name, value)| {
                                        Ok((
                                            self.sema.interner.get_or_intern(name.as_ref()),
                                            import_capture_value(&mut self.sema, value)?,
                                        ))
                                    })
                                    .collect::<Result<HashMap<_, _>, DeclarationInstallFailure>>()?
                            } else {
                                HashMap::new()
                            };
                            let method_sigs = methods
                                .iter()
                                .map(|method| {
                                    Ok(super::AnonMethodSig {
                                        name: self
                                            .sema
                                            .interner
                                            .get_or_intern(method.name.as_ref()),
                                        has_self: method.has_self,
                                        self_mode: rir_mode(method.self_mode),
                                        param_types: method
                                            .parameters
                                            .iter()
                                            .map(|(ty, _, _)| {
                                                import_method_type(&mut self.sema, ty)
                                            })
                                            .collect::<Result<Vec<_>, _>>()?,
                                        param_modes: method
                                            .parameters
                                            .iter()
                                            .map(|(_, mode, _)| rir_mode(*mode))
                                            .collect(),
                                        param_comptime: method
                                            .parameters
                                            .iter()
                                            .map(|(_, _, comptime)| *comptime)
                                            .collect(),
                                        return_type: import_method_type(
                                            &mut self.sema,
                                            &method.result,
                                        )?,
                                    })
                                })
                                .collect::<Result<Vec<_>, DeclarationInstallFailure>>()?;
                            let query_identity = identity;
                            let (struct_ty, _) = self
                                .sema
                                .find_or_create_anon_struct(
                                    query_identity.clone(),
                                    &fields,
                                    &method_sigs,
                                    &value_captures,
                                )
                                .map_err(|error| {
                                    DeclarationInstallFailure::AnonymousDigestCollision(
                                        error.to_string().into_boxed_str(),
                                    )
                                })?;
                            let struct_id = struct_ty
                                .as_struct()
                                .ok_or(DeclarationInstallFailure::NominalShapeMismatch)?;
                            // No same-producer/different-anchor alias install
                            // (RUE-1089): the durable query anchor now equals the
                            // RIR anchor AstGen minted, so the member is referenced
                            // under exactly the identity its producer publishes.
                            if !type_captures.is_empty() {
                                self.sema
                                    .anon_struct_type_subst
                                    .insert(struct_id, type_captures.clone());
                            }
                            if let Some((methods, _)) = method_materialization
                                && !self.sema.rir.anon_struct_methods(&methods).is_empty()
                            {
                                let first = self
                                    .sema
                                    .rir
                                    .anon_struct_methods(&methods)
                                    .get(0)
                                    .expect("nonempty anonymous method range");
                                let needs_registration = match &self.sema.rir.get(first).data {
                                    InstData::FnDecl { name, .. } => {
                                        !self.sema.has_method((struct_id, *name))
                                    }
                                    _ => false,
                                };
                                if needs_registration {
                                    self.sema
                                        .register_projected_anon_struct_methods(
                                            struct_id,
                                            struct_ty,
                                            &methods,
                                            &method_sigs,
                                        )
                                        .ok_or(DeclarationInstallFailure::NominalShapeMismatch)?;
                                }
                            }
                        }
                        SemanticAnonymousNominalShape::Enum { variants } => {
                            let names = variants
                                .iter()
                                .map(|(name, _)| name.to_string())
                                .collect::<Vec<_>>();
                            let payloads = variants
                                .iter()
                                .map(|(_, payload)| {
                                    payload
                                        .iter()
                                        .map(|ty| self.sema.import_export_type(ty))
                                        .collect::<Result<Vec<_>, _>>()
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            self.sema
                                .find_or_create_anon_enum(identity, &names, &payloads)
                                .map_err(|error| {
                                    DeclarationInstallFailure::AnonymousDigestCollision(
                                        error.to_string().into_boxed_str(),
                                    )
                                })?;
                        }
                    }
                    Ok(())
                })();
                match installed {
                    Ok(()) => progressed = true,
                    Err(DeclarationInstallFailure::MissingNominal) => next.push(nominal),
                    Err(failure) => return Err(failure),
                }
            }
            if !progressed {
                return Err(DeclarationInstallFailure::MissingNominal);
            }
            pending_anonymous = next;
        }

        for pending in &self.pending_nominals {
            let record = records[&pending.shell.identity];
            let name = self
                .sema
                .interner
                .get_or_intern(pending.shell.identity.name.as_ref());
            match (
                &record.payload,
                &self.sema.rir.get(pending.declaration).data,
            ) {
                (
                    SemanticDeclarationPayload::Struct {
                        fields,
                        is_copy,
                        is_linear,
                    },
                    InstData::StructDecl {
                        is_linear: syntax_linear,
                        fields: rir_fields,
                        ..
                    },
                ) => {
                    let id = *self
                        .sema
                        .structs_by_file_name
                        .get(&(pending.shell.declaration_span.file_id, name))
                        .ok_or(DeclarationInstallFailure::MissingNominal)?;
                    let metadata = self
                        .sema
                        .type_pool
                        .struct_declaration_metadata(id)
                        .ok_or(DeclarationInstallFailure::NominalShapeMismatch)?;
                    if *syntax_linear != *is_linear || metadata.is_copy != *is_copy {
                        return Err(DeclarationInstallFailure::NominalShapeMismatch);
                    }
                    if self
                        .sema
                        .rir
                        .struct_fields(rir_fields)
                        .values()
                        .map(|(name, _)| self.sema.interner.resolve(&name))
                        .ne(fields.iter().map(|field| field.0.as_ref()))
                    {
                        return Err(DeclarationInstallFailure::NominalShapeMismatch);
                    }
                    let resolved_fields = fields
                        .iter()
                        .map(|(name, ty)| {
                            Ok(crate::StructField {
                                name: name.to_string(),
                                ty: self.sema.import_export_type(ty)?,
                            })
                        })
                        .collect::<Result<_, DeclarationInstallFailure>>()?;
                    let def = crate::StructDef {
                        name: metadata.name,
                        fields: resolved_fields,
                        is_copy: metadata.is_copy,
                        is_linear: metadata.is_linear,
                        destructor: metadata.destructor,
                        is_builtin: metadata.is_builtin,
                        is_pub: metadata.is_pub,
                        file_id: metadata.file_id,
                    };
                    self.sema.type_pool.complete_declared_struct(id, def);
                }
                (SemanticDeclarationPayload::Enum { variants }, InstData::EnumDecl { .. }) => {
                    let id = *self
                        .sema
                        .enums_by_file_name
                        .get(&(pending.shell.declaration_span.file_id, name))
                        .ok_or(DeclarationInstallFailure::MissingNominal)?;
                    let metadata = self
                        .sema
                        .type_pool
                        .enum_declaration_metadata(id)
                        .ok_or(DeclarationInstallFailure::NominalShapeMismatch)?;
                    if metadata
                        .variants
                        .iter()
                        .map(String::as_str)
                        .ne(variants.iter().map(|variant| variant.0.as_ref()))
                    {
                        return Err(DeclarationInstallFailure::NominalShapeMismatch);
                    }
                    let variant_payloads = variants
                        .iter()
                        .map(|(_, payload)| {
                            payload
                                .iter()
                                .map(|ty| self.sema.import_export_type(ty))
                                .collect()
                        })
                        .collect::<Result<_, DeclarationInstallFailure>>()?;
                    let def = crate::EnumDef {
                        name: metadata.name,
                        variants: metadata.variants,
                        variant_payloads,
                        is_pub: metadata.is_pub,
                        file_id: metadata.file_id,
                    };
                    self.sema.type_pool.complete_declared_enum(id, def);
                }
                _ => return Err(DeclarationInstallFailure::KindMismatch),
            }
        }

        let mut payload_install_order = self.pending_payloads.iter().collect::<Vec<_>>();
        payload_install_order.sort_by_key(|pending| {
            let record = records[&pending.shell.identity];
            if matches!(record.payload, SemanticDeclarationPayload::Callable { .. }) {
                0_u8
            } else {
                1_u8
            }
        });
        for pending in payload_install_order {
            let record = records[&pending.shell.identity];
            match (&record.payload, pending.source) {
                (
                    SemanticDeclarationPayload::Callable {
                        parameters,
                        result,
                        has_self,
                        self_mode: payload_self_mode,
                        is_unchecked,
                    },
                    DeclarationPayloadSource::Callable { body },
                ) => {
                    if parameters.len() != pending.shell.parameter_names.len()
                        || *has_self != pending.shell.has_self
                        || *is_unchecked != pending.shell.is_unchecked
                    {
                        return Err(DeclarationInstallFailure::CallableShapeMismatch);
                    }
                    for (parameter, (mode, comptime)) in parameters.iter().zip(
                        pending
                            .shell
                            .parameter_modes
                            .iter()
                            .zip(pending.shell.parameter_comptime.iter()),
                    ) {
                        let mode = match mode {
                            RirParamMode::Borrow => SemanticParameterMode::Borrow,
                            RirParamMode::Inout => SemanticParameterMode::Inout,
                            _ => SemanticParameterMode::Value,
                        };
                        if parameter.mode != mode || parameter.is_comptime != *comptime {
                            return Err(DeclarationInstallFailure::CallableShapeMismatch);
                        }
                    }
                    let InstData::FnDecl {
                        name,
                        return_type,
                        params,
                        self_mode,
                        self_is_mut,
                        directives,
                        // The Rue-to-C export marker (ADR-0064 P4) is a property
                        // of the current RIR, so it is read here rather than
                        // threaded through the durable declaration shell.
                        is_c_export,
                        // The `-> borrow T` accessor marker (ADR-0062) is the
                        // same kind of property: the durable signature records
                        // the result type's source spelling, which never
                        // contains the result-position `borrow` qualifier.
                        returns_borrow,
                        ..
                    } = &self.sema.rir.get(pending.declaration).data
                    else {
                        return Err(DeclarationInstallFailure::KindMismatch);
                    };
                    let payload_self_mode = match payload_self_mode {
                        SemanticParameterMode::Value => RirParamMode::Normal,
                        SemanticParameterMode::Borrow => RirParamMode::Borrow,
                        SemanticParameterMode::Inout => RirParamMode::Inout,
                    };
                    if payload_self_mode != *self_mode {
                        return Err(DeclarationInstallFailure::CallableShapeMismatch);
                    }
                    let is_c_export = *is_c_export;
                    let returns_borrow = *returns_borrow;
                    let type_name = self.sema.interner.get_or_intern("type");
                    let rir_parameters = self.sema.rir.params(params);
                    if parameters
                        .iter()
                        .zip(rir_parameters.iter())
                        .any(|(parameter, rir)| {
                            rir.is_comptime
                                && rir.ty == type_name
                                && parameter.ty != SemanticExportType::ComptimeType
                        })
                    {
                        return Err(DeclarationInstallFailure::CallableShapeMismatch);
                    }
                    let names = pending
                        .shell
                        .parameter_names
                        .iter()
                        .map(|name| self.sema.interner.get_or_intern(name.as_ref()));
                    let generic_parameters = rir_parameters
                        .iter()
                        .filter(|parameter| parameter.is_comptime && parameter.ty == type_name)
                        .map(|_| Type::COMPTIME_TYPE)
                        .collect::<Vec<_>>();
                    let types = parameters
                        .iter()
                        .map(|parameter| {
                            self.sema.import_export_type_with_generics(
                                &parameter.ty,
                                Some(&generic_parameters),
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let range = self.sema.param_arena.alloc(
                        names,
                        types,
                        pending.shell.parameter_modes.iter().copied(),
                        pending.shell.parameter_comptime.iter().copied(),
                    );
                    let return_type_value = self
                        .sema
                        .import_export_type_with_generics(result, Some(&generic_parameters))?;
                    if let Some(owner) = &pending.shell.identity.owner {
                        let owner = self.sema.interner.get_or_intern(owner.as_ref());
                        let id = *self
                            .sema
                            .structs_by_file_name
                            .get(&(pending.shell.declaration_span.file_id, owner))
                            .ok_or(DeclarationInstallFailure::MissingNominal)?;
                        self.sema.methods.insert(
                            (id, *name),
                            super::MethodInfo {
                                struct_type: Type::new_struct(id),
                                has_self: *has_self,
                                self_mode: *self_mode,
                                self_is_mut: *self_is_mut,
                                params: range,
                                return_type: return_type_value,
                                body,
                                span: pending.shell.declaration_span,
                                returns_borrow,
                            },
                        );
                        self.sema
                            .named_method_declarations
                            .insert((id, *name), pending.declaration);
                    } else {
                        let internal = self
                            .sema
                            .internal_function_name(*name, pending.shell.declaration_span.file_id);
                        let directives = self.sema.rir.directives(directives);
                        let allow_unused_function = self
                            .sema
                            .has_allow_directive(directives.iter(), "unused_function");
                        let allow_unused_variable = self
                            .sema
                            .has_allow_directive(directives.iter(), "unused_variable");
                        let allow_unreachable_code = self
                            .sema
                            .has_allow_directive(directives.iter(), "unreachable_code");
                        let info = super::FunctionInfo {
                            params: range,
                            return_type: return_type_value,
                            return_type_sym: *return_type,
                            body,
                            declaration: pending.declaration,
                            span: pending.shell.declaration_span,
                            is_generic: pending.shell.is_generic,
                            is_pub: pending.shell.is_public,
                            is_unchecked: pending.shell.is_unchecked,
                            is_extern: pending.shell.is_extern,
                            is_c_export,
                            allow_unused_function,
                            allow_unused_variable,
                            allow_unreachable_code,
                            file_id: pending.shell.declaration_span.file_id,
                        };
                        // Foreign declarations from different modules share one
                        // key here — the raw C symbol (RUE-1125). Disagreeing
                        // signatures are rejected before one silently wins
                        // (RUE-1218, spec 9.3:5). This is a real user
                        // diagnostic, not an installation invariant, so it
                        // travels in its own variant rather than as an ICE.
                        self.sema
                            .check_foreign_redeclaration(internal, &info)
                            .map_err(|error| {
                                DeclarationInstallFailure::ForeignSignatureConflict(Box::new(error))
                            })?;
                        self.sema
                            .functions_by_file_name
                            .insert((pending.shell.declaration_span.file_id, *name), internal);
                        self.sema.function_source_names.insert(internal, *name);
                        self.sema.functions.insert(internal, info);
                    }
                }
                (
                    SemanticDeclarationPayload::Destructor,
                    DeclarationPayloadSource::Destructor { .. },
                ) => {
                    let owner = pending
                        .shell
                        .identity
                        .owner
                        .as_deref()
                        .ok_or(DeclarationInstallFailure::IdentityMismatch)?;
                    let owner = self.sema.interner.get_or_intern(owner);
                    self.sema
                        .collect_destructor(owner, pending.shell.declaration_span)
                        .map_err(|_| DeclarationInstallFailure::NominalShapeMismatch)?;
                }
                (
                    SemanticDeclarationPayload::ModuleBinding { .. },
                    DeclarationPayloadSource::Const { .. },
                ) => {
                    let target = *module_targets
                        .get(&pending.shell.identity)
                        .ok_or(DeclarationInstallFailure::UnsupportedType)?;
                    let name = self
                        .sema
                        .interner
                        .get_or_intern(pending.shell.identity.name.as_ref());
                    self.sema.const_resolutions.insert(
                        (pending.shell.declaration_span.file_id, name),
                        super::ConstResolution::ModuleBinding(super::ConstInfo {
                            is_pub: pending.shell.is_public,
                            ty: target,
                            value: ConstValue::Type(target),
                            span: pending.shell.declaration_span,
                        }),
                    );
                }
                (
                    SemanticDeclarationPayload::Const { ty, value },
                    DeclarationPayloadSource::Const { .. },
                ) => {
                    let ty = self.sema.import_export_type(ty)?;
                    let value = match value {
                        SemanticExportConstValue::Integer(value) => ConstValue::Integer(*value),
                        SemanticExportConstValue::Bool(value) => ConstValue::Bool(*value),
                        SemanticExportConstValue::Type(value) => {
                            ConstValue::Type(self.sema.import_export_type(value)?)
                        }
                        SemanticExportConstValue::Function { file_id, name } => {
                            let name = self.sema.interner.get_or_intern(name.as_ref());
                            let symbol = self
                                .sema
                                .resolve_function_name_local(name, *file_id)
                                .or_else(|| {
                                    self.sema
                                        .functions_by_file_name
                                        .get(&(*file_id, name))
                                        .copied()
                                })
                                .ok_or(DeclarationInstallFailure::MissingPayload)?;
                            ConstValue::Function(symbol)
                        }
                        SemanticExportConstValue::Unit => ConstValue::Unit,
                        SemanticExportConstValue::String(value) => {
                            ConstValue::String(self.sema.interner.get_or_intern(value.as_ref()))
                        }
                    };
                    let name = self
                        .sema
                        .interner
                        .get_or_intern(pending.shell.identity.name.as_ref());
                    self.sema.const_resolutions.insert(
                        (pending.shell.declaration_span.file_id, name),
                        super::ConstResolution::Value(super::ConstInfo {
                            is_pub: pending.shell.is_public,
                            ty,
                            value,
                            span: pending.shell.declaration_span,
                        }),
                    );
                }
                _ => return Err(DeclarationInstallFailure::KindMismatch),
            }
        }
        // The keyed nominal-well-formedness query is the language authority
        // for by-value cycles. Re-running the canonical containment fold here
        // validates that this request-local AIR materialization is exactly the
        // projected stable graph; any failure now indicates projection drift.
        self.sema
            .check_recursive_value_types()
            .map_err(|_| DeclarationInstallFailure::NominalShapeMismatch)?;
        self.sema
            .validate_deferred_ownership_gates()
            .map_err(|_| DeclarationInstallFailure::NominalShapeMismatch)?;
        self.sema.capture_resolved_declaration_type_dependencies();
        self.sema.declaration_type_observer = None;
        self.binding_work.durable_payloads_installed = expected;
        self.binding_work.body_readiness_finalization_invocations += 1;
        Ok(self.sema.into_bound_with_work(self.binding_work))
    }
}

fn install_const_candidate_endpoints<D: super::DeclarationPhase>(
    sema: &mut super::Sema<'_, D>,
    pending: &[PendingDeclarationPayload],
) -> Result<(), crate::SemanticStableResolutionFailure> {
    let issuer = sema
        .stable_definition_endpoints
        .keys()
        .next()
        .map(|token| token.issuer())
        .or_else(|| {
            sema.stable_module_endpoints
                .keys()
                .next()
                .map(|token| token.issuer())
        })
        .ok_or(crate::SemanticStableResolutionFailure::Missing)?;
    let mut next_slot = sema
        .stable_definition_endpoints
        .keys()
        .map(|token| token.slot())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(crate::SemanticStableResolutionFailure::Ambiguous)?;
    let mut tokens = HashMap::new();
    let mut endpoints = HashMap::new();
    let mut multiplicity = HashMap::new();
    for candidate in pending
        .iter()
        .filter(|candidate| matches!(candidate.source, DeclarationPayloadSource::Const { .. }))
    {
        *multiplicity
            .entry((
                candidate.shell.declaration_span.file_id.index(),
                candidate.shell.identity.name.to_string(),
            ))
            .or_insert(0_u32) += 1;
    }
    for candidate in pending
        .iter()
        .filter(|candidate| matches!(candidate.source, DeclarationPayloadSource::Const { .. }))
    {
        let key = (
            candidate.shell.declaration_span.file_id.index(),
            candidate.shell.identity.name.to_string(),
        );
        if multiplicity.get(&key).copied() != Some(1) {
            continue;
        }
        let token = super::EpochLocalConstCandidateToken(crate::SemanticDefinitionToken::new(
            issuer, next_slot,
        ));
        next_slot = next_slot
            .checked_add(1)
            .ok_or(crate::SemanticStableResolutionFailure::Ambiguous)?;
        let endpoint = super::EpochLocalConstCandidateEndpoint {
            file: key.0,
            name: key.1.clone(),
        };
        if tokens.insert(key, token).is_some() || endpoints.insert(token, endpoint).is_some() {
            return Err(crate::SemanticStableResolutionFailure::Ambiguous);
        }
    }
    sema.const_candidate_tokens = std::sync::Arc::new(tokens);
    sema.const_candidate_endpoints = std::sync::Arc::new(endpoints);
    Ok(())
}

impl<D: super::DeclarationPhase> Sema<'_, D> {
    fn import_anonymous_identity(
        &self,
        identity: &SemanticAnonymousNominalIdentity,
    ) -> Result<super::anon_structs::IssuedAnonymousNominalKey, DeclarationInstallFailure> {
        identity.try_map_identities(
            &|definition| {
                if matches!(
                    definition.kind,
                    StableDefinitionKind::ValueConst | StableDefinitionKind::ModuleBinding
                ) && definition.owner.is_none()
                    && let Some(token) = self
                        .const_candidate_tokens
                        .get(&(definition.file_id.index(), definition.name.to_string()))
                {
                    return Ok(token.0);
                }
                self.stable_definition_token(
                    definition.file_id.index(),
                    &definition.name,
                    definition.owner.as_deref(),
                    definition.kind,
                )
                .map_err(|_| DeclarationInstallFailure::MissingNominal)
            },
            &|module| {
                let file = (0..self.module_registry.len())
                    .map(|index| {
                        self.module_registry
                            .get_def(crate::ModuleId::new(index as u32))
                    })
                    .find(|definition| definition.durable_id.as_str() == module.as_ref())
                    .map(|definition| definition.file_id)
                    .ok_or(DeclarationInstallFailure::MissingNominal)?;
                self.stable_module_tokens
                    .get(&file)
                    .copied()
                    .ok_or(DeclarationInstallFailure::MissingNominal)
            },
        )
    }

    fn import_export_type(
        &self,
        value: &SemanticExportType,
    ) -> Result<Type, DeclarationInstallFailure> {
        self.import_export_type_with_generics(value, None)
    }

    fn import_export_type_with_generics(
        &self,
        value: &SemanticExportType,
        generic_parameters: Option<&[Type]>,
    ) -> Result<Type, DeclarationInstallFailure> {
        if let Some(generic_parameters) = generic_parameters
            && Self::validate_generic_references(value, generic_parameters.len())?
        {
            // Ordinary declaration binding represents every generic-dependent
            // signature type, including composites, as one top-level
            // placeholder until specialization. Preserve the recursive DTO for
            // durable identity, but reconstruct that exact AIR representation.
            return Ok(Type::COMPTIME_TYPE);
        }
        Ok(match value {
            SemanticExportType::I8 => Type::I8,
            SemanticExportType::I16 => Type::I16,
            SemanticExportType::I32 => Type::I32,
            SemanticExportType::I64 => Type::I64,
            SemanticExportType::U8 => Type::U8,
            SemanticExportType::U16 => Type::U16,
            SemanticExportType::U32 => Type::U32,
            SemanticExportType::U64 => Type::U64,
            SemanticExportType::Bool => Type::BOOL,
            SemanticExportType::Unit => Type::UNIT,
            SemanticExportType::Never => Type::NEVER,
            SemanticExportType::ComptimeType => Type::COMPTIME_TYPE,
            SemanticExportType::GenericParameter(index) => *generic_parameters
                .and_then(|parameters| parameters.get(*index as usize))
                .ok_or(DeclarationInstallFailure::UnsupportedType)?,
            SemanticExportType::BuiltinNominal { name, kind } => {
                let name = self.interner.get_or_intern(name.as_ref());
                match kind {
                    crate::SemanticImportNominalKind::Struct => Type::new_struct(
                        self.resolve_builtin_struct_name(name)
                            .or_else(|| self.generated_structs.get(&name).copied())
                            .ok_or(DeclarationInstallFailure::MissingNominal)?,
                    ),
                    crate::SemanticImportNominalKind::Enum => Type::new_enum(
                        self.resolve_builtin_enum_name(name)
                            .ok_or(DeclarationInstallFailure::MissingNominal)?,
                    ),
                }
            }
            SemanticExportType::Nominal(nominal) => {
                let name = self.interner.get_or_intern(nominal.name.as_ref());
                match nominal.kind {
                    StableDefinitionKind::Struct => Type::new_struct(
                        *self
                            .structs_by_file_name
                            .get(&(nominal.file_id, name))
                            .ok_or(DeclarationInstallFailure::MissingNominal)?,
                    ),
                    StableDefinitionKind::Enum => Type::new_enum(
                        *self
                            .enums_by_file_name
                            .get(&(nominal.file_id, name))
                            .ok_or(DeclarationInstallFailure::MissingNominal)?,
                    ),
                    _ => return Err(DeclarationInstallFailure::KindMismatch),
                }
            }
            SemanticExportType::AnonymousNominal(identity) => {
                let identity = self.import_anonymous_identity(identity)?;
                match identity.kind {
                    crate::AnonymousNominalKind::Struct => Type::new_struct(
                        *self
                            .anon_struct_identities
                            .get(&identity)
                            .ok_or(DeclarationInstallFailure::MissingNominal)?,
                    ),
                    crate::AnonymousNominalKind::Enum => Type::new_enum(
                        *self
                            .anon_enum_identities
                            .get(&identity)
                            .ok_or(DeclarationInstallFailure::MissingNominal)?,
                    ),
                }
            }
            SemanticExportType::Array { element, len } => self
                .type_pool
                .try_intern_array(
                    self.import_export_type_with_generics(element, generic_parameters)?,
                    *len,
                )
                .map_err(|_| DeclarationInstallFailure::UnsupportedType)?,
            SemanticExportType::Slice { element, name } => {
                let element = self.import_export_type_with_generics(element, generic_parameters)?;
                let symbol = self.interner.get_or_intern(name.as_ref());
                let id = self
                    .generated_structs
                    .get(&symbol)
                    .copied()
                    .ok_or(DeclarationInstallFailure::MissingNominal)?;
                let def = self.type_pool.struct_def(id);
                let matches_element = def.fields.first().is_some_and(|field| {
                    matches!(field.ty.kind(), TypeKind::PtrConst(pointer)
                        if self.type_pool.ptr_const_def(pointer) == element)
                });
                if !matches_element {
                    return Err(DeclarationInstallFailure::KindMismatch);
                }
                Type::new_struct(id)
            }
            SemanticExportType::PtrConst(value) => self
                .type_pool
                .try_intern_ptr_const(
                    self.import_export_type_with_generics(value, generic_parameters)?,
                )
                .map_err(|_| DeclarationInstallFailure::UnsupportedType)?,
            SemanticExportType::PtrMut(value) => self
                .type_pool
                .try_intern_ptr_mut(
                    self.import_export_type_with_generics(value, generic_parameters)?,
                )
                .map_err(|_| DeclarationInstallFailure::UnsupportedType)?,
            SemanticExportType::Module(_) => {
                return Err(DeclarationInstallFailure::UnsupportedType);
            }
        })
    }

    fn validate_generic_references(
        value: &SemanticExportType,
        generic_parameter_count: usize,
    ) -> Result<bool, DeclarationInstallFailure> {
        Ok(match value {
            SemanticExportType::GenericParameter(index) => {
                if *index as usize >= generic_parameter_count {
                    return Err(DeclarationInstallFailure::UnsupportedType);
                }
                true
            }
            SemanticExportType::Array { element, .. }
            | SemanticExportType::Slice { element, .. }
            | SemanticExportType::PtrConst(element)
            | SemanticExportType::PtrMut(element) => {
                Self::validate_generic_references(element, generic_parameter_count)?
            }
            SemanticExportType::AnonymousNominal(_) => generic_parameter_count > 0,
            _ => false,
        })
    }
}

fn install_stable_identity_endpoints<D: super::DeclarationPhase>(
    sema: &mut super::Sema<'_, D>,
    definitions: &[crate::SemanticDefinitionEndpoint],
    modules: &[crate::SemanticModuleEndpoint],
    expected_definitions: &[(u32, String, Option<String>, StableDefinitionKind)],
    expected_modules: &[FileId],
) -> Result<(), crate::SemanticStableResolutionFailure> {
    let issuer = definitions
        .first()
        .map(|endpoint| endpoint.token.issuer())
        .or_else(|| modules.first().map(|endpoint| endpoint.token.issuer()))
        .ok_or(crate::SemanticStableResolutionFailure::Missing)?;
    let mut definition_tokens = HashMap::new();
    let mut definition_endpoints = HashMap::new();
    for endpoint in definitions {
        if endpoint.token.issuer() != issuer {
            return Err(crate::SemanticStableResolutionFailure::ForeignIssuer);
        }
        if endpoint.kind.requires_owner() != endpoint.owner.is_some() {
            return Err(crate::SemanticStableResolutionFailure::WrongKind);
        }
        let key = (
            endpoint.file,
            endpoint.name.to_string(),
            endpoint.owner.as_deref().map(str::to_owned),
            endpoint.kind,
        );
        if definition_tokens.insert(key, endpoint.token).is_some()
            || definition_endpoints
                .insert(endpoint.token, endpoint.clone())
                .is_some()
        {
            return Err(crate::SemanticStableResolutionFailure::Ambiguous);
        }
    }
    let actual_definition_keys = definition_tokens
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let expected_definition_keys = expected_definitions
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if actual_definition_keys != expected_definition_keys {
        let wrong_kind = actual_definition_keys.iter().any(|actual| {
            expected_definition_keys.iter().any(|expected| {
                actual.0 == expected.0
                    && actual.1 == expected.1
                    && actual.2 == expected.2
                    && actual.3 != expected.3
            })
        });
        return Err(if wrong_kind {
            crate::SemanticStableResolutionFailure::WrongKind
        } else {
            crate::SemanticStableResolutionFailure::Missing
        });
    }
    let mut module_tokens = HashMap::new();
    let mut module_endpoints = HashMap::new();
    for endpoint in modules {
        if endpoint.token.issuer() != issuer {
            return Err(crate::SemanticStableResolutionFailure::ForeignIssuer);
        }
        if module_tokens
            .insert(FileId::new(endpoint.file), endpoint.token)
            .is_some()
            || module_endpoints.insert(endpoint.token, *endpoint).is_some()
        {
            return Err(crate::SemanticStableResolutionFailure::Ambiguous);
        }
    }
    let actual_module_files = module_tokens
        .keys()
        .map(|file| file.index())
        .collect::<std::collections::BTreeSet<_>>();
    let expected_module_files = expected_modules
        .iter()
        .map(|file| file.index())
        .collect::<std::collections::BTreeSet<_>>();
    if actual_module_files != expected_module_files {
        return Err(crate::SemanticStableResolutionFailure::Missing);
    }

    // Declaration resolution can materialize anonymous nominals while using
    // the provisional endpoint issuer. Reissuing the same exact manifest for
    // the authoritative body epoch must relocate those retained identities;
    // otherwise a declaration-signature anonymous type becomes foreign to
    // its own request. This changes identity tokens only, never AIR types or
    // structural representatives.
    if !sema.stable_definition_endpoints.is_empty()
        && sema
            .stable_definition_endpoints
            .keys()
            .next()
            .is_some_and(|token| token.issuer() != issuer)
    {
        fn remap_key(
            key: &crate::AnonymousNominalKey<
                crate::SemanticDefinitionToken,
                crate::SemanticModuleToken,
            >,
            old_definitions: &HashMap<
                crate::SemanticDefinitionToken,
                crate::SemanticDefinitionEndpoint,
            >,
            old_const_candidates: &HashMap<
                super::EpochLocalConstCandidateToken,
                super::EpochLocalConstCandidateEndpoint,
            >,
            new_definitions: &HashMap<
                (u32, String, Option<String>, StableDefinitionKind),
                crate::SemanticDefinitionToken,
            >,
            old_modules: &HashMap<crate::SemanticModuleToken, crate::SemanticModuleEndpoint>,
            new_modules: &HashMap<FileId, crate::SemanticModuleToken>,
        ) -> Result<
            crate::AnonymousNominalKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
            crate::SemanticStableResolutionFailure,
        > {
            key.try_map_identities(
                &|token| {
                    if let Some(endpoint) = old_definitions.get(token) {
                        return new_definitions
                            .get(&(
                                endpoint.file,
                                endpoint.name.to_string(),
                                endpoint.owner.as_deref().map(str::to_owned),
                                endpoint.kind,
                            ))
                            .copied()
                            .ok_or(crate::SemanticStableResolutionFailure::Missing);
                    }
                    let endpoint = old_const_candidates
                        .get(&super::EpochLocalConstCandidateToken(*token))
                        .ok_or(crate::SemanticStableResolutionFailure::ForeignIssuer)?;
                    new_definitions
                        .get(&(
                            endpoint.file,
                            endpoint.name.clone(),
                            None,
                            StableDefinitionKind::ValueConst,
                        ))
                        .copied()
                        .ok_or(crate::SemanticStableResolutionFailure::Missing)
                },
                &|token| {
                    let endpoint = old_modules
                        .get(token)
                        .ok_or(crate::SemanticStableResolutionFailure::ForeignIssuer)?;
                    new_modules
                        .get(&FileId::new(endpoint.file))
                        .copied()
                        .ok_or(crate::SemanticStableResolutionFailure::Missing)
                },
            )
        }

        let old_definition_endpoints = sema.stable_definition_endpoints.clone();
        let old_const_candidate_endpoints = sema.const_candidate_endpoints.clone();
        let old_module_endpoints = sema.stable_module_endpoints.clone();
        let remap = |key| {
            remap_key(
                key,
                &old_definition_endpoints,
                &old_const_candidate_endpoints,
                &definition_tokens,
                &old_module_endpoints,
                &module_tokens,
            )
        };
        let anon_struct_identities = sema
            .anon_struct_identities
            .iter()
            .map(|(key, id)| Ok((remap(key)?, *id)))
            .collect::<Result<_, crate::SemanticStableResolutionFailure>>()?;
        let anon_enum_identities = sema
            .anon_enum_identities
            .iter()
            .map(|(key, id)| Ok((remap(key)?, *id)))
            .collect::<Result<_, crate::SemanticStableResolutionFailure>>()?;
        let canonical_anonymous_types = sema
            .canonical_anonymous_types
            .iter()
            .map(|(ty, key)| Ok((*ty, remap(key)?)))
            .collect::<Result<_, crate::SemanticStableResolutionFailure>>()?;
        sema.anon_struct_identities = anon_struct_identities;
        sema.anon_enum_identities = anon_enum_identities;
        sema.canonical_anonymous_types = canonical_anonymous_types;
    }
    sema.stable_definition_tokens = std::sync::Arc::new(definition_tokens);
    sema.stable_definition_endpoints = std::sync::Arc::new(definition_endpoints);
    sema.const_candidate_tokens = std::sync::Arc::default();
    sema.const_candidate_endpoints = std::sync::Arc::default();
    sema.stable_module_tokens = std::sync::Arc::new(module_tokens);
    sema.stable_module_endpoints = std::sync::Arc::new(module_endpoints);
    Ok(())
}

fn stable_module_files<D: super::DeclarationPhase>(sema: &super::Sema<'_, D>) -> Vec<FileId> {
    (0..sema.module_registry.len())
        .map(|index| {
            sema.module_registry
                .get_def(crate::ModuleId::new(index as u32))
                .file_id
        })
        .collect()
}

impl<'a> BoundSema<'a> {
    /// Install a per-body well-known `Option(payload)` registry (RUE-1112)
    /// narrowly, outside this body's composition/import universe.
    ///
    /// `nominals` are the trusted std `Option` enum specializations resolved by
    /// the per-body demand loop; they are materialized into this epoch's pool by
    /// reusing the ordinary anonymous-nominal minting. `option_by_payload` pairs
    /// each demanded payload type with its resolved `Option` enum type, recorded
    /// so fallible-intrinsic resolution (`resolve_option_result_type`) can bind
    /// the trusted `Option` under `?` even when the body never `@import`s it.
    ///
    /// The enum is never appended to `BodyReferences`, reachability, or the
    /// revision-global anonymous universe. Its producer's definition and module
    /// endpoints already exist in this epoch because those endpoints are issued
    /// whole-program, so this install performs no composition change and the
    /// fail-closed validation for ordinary nominals stays intact.
    pub fn install_well_known_option_types(
        mut self,
        nominals: &[SemanticAnonymousNominalExport],
        option_by_payload: &[(SemanticExportType, SemanticExportType)],
    ) -> Result<Self, DeclarationInstallFailure> {
        // Materialize the resolved enums. A bounded fixpoint tolerates a nominal
        // whose payload references another not-yet-installed well-known nominal;
        // the trusted `Option` shapes are flat today, so one pass suffices.
        let mut pending = nominals.iter().collect::<Vec<_>>();
        while !pending.is_empty() {
            let mut progressed = false;
            let mut next = Vec::new();
            for nominal in pending {
                let SemanticAnonymousNominalShape::Enum { variants } = &nominal.shape else {
                    return Err(DeclarationInstallFailure::NominalShapeMismatch);
                };
                let installed = (|| -> Result<(), DeclarationInstallFailure> {
                    let identity = self.sema.import_anonymous_identity(&nominal.identity)?;
                    let names = variants
                        .iter()
                        .map(|(name, _)| name.to_string())
                        .collect::<Vec<_>>();
                    let payloads = variants
                        .iter()
                        .map(|(_, payload)| {
                            payload
                                .iter()
                                .map(|ty| self.sema.import_export_type(ty))
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    self.sema
                        .find_or_create_anon_enum(identity.clone(), &names, &payloads)
                        .map_err(|error| {
                            DeclarationInstallFailure::AnonymousDigestCollision(
                                error.to_string().into_boxed_str(),
                            )
                        })?;
                    // Record the installed identity so the body that binds this
                    // `Option` exports it as a produced anonymous nominal (see
                    // `well_known_option_identities`): the registry-rooted
                    // nominal must enter durable composition exactly as an
                    // ordinarily-materialized specialization would.
                    self.sema.well_known_option_identities.insert(identity);
                    Ok(())
                })();
                match installed {
                    Ok(()) => progressed = true,
                    Err(DeclarationInstallFailure::MissingNominal) => next.push(nominal),
                    Err(failure) => return Err(failure),
                }
            }
            if !progressed {
                return Err(DeclarationInstallFailure::MissingNominal);
            }
            pending = next;
        }
        // Record the demand map. Both endpoints are pure identity lookups now
        // that the enums are installed.
        for (payload, option) in option_by_payload {
            let payload_ty = self.sema.import_export_type(payload)?;
            let option_ty = self.sema.import_export_type(option)?;
            self.sema
                .well_known_option_by_payload
                .insert(payload_ty, option_ty);
        }
        Ok(self)
    }

    /// Install stable declaration dependency observations captured by the
    /// semantic query nucleus. These events describe the declaration payloads
    /// already installed in this epoch; AIR must not rediscover them by
    /// re-running declaration resolution.
    pub fn install_query_declaration_dependencies(
        mut self,
        type_dependencies: &[crate::DeclarationTypeDependencyEvent],
        type_call_heads: &[crate::DeclarationTypeCallHeadDependencyEvent],
        builtin_type_call_heads: &[crate::DeclarationBuiltinTypeCallHeadDependencyEvent],
        named_const_dependencies: &[crate::NamedConstDependencyEvent],
    ) -> Self {
        self.sema
            .declaration_type_dependencies
            .extend_from_slice(type_dependencies);
        self.sema
            .declaration_type_call_head_dependencies
            .extend_from_slice(type_call_heads);
        self.sema
            .declaration_builtin_type_call_head_dependencies
            .extend_from_slice(builtin_type_call_heads);
        self.sema
            .named_const_dependencies
            .extend_from_slice(named_const_dependencies);
        self.sema
            .body_analysis_work
            .declaration_type_dependency_events += type_dependencies.len();
        self.sema
            .body_analysis_work
            .declaration_type_call_head_dependency_events +=
            type_call_heads.len() + builtin_type_call_heads.len();
        self.sema.body_analysis_work.named_const_dependency_events +=
            named_const_dependencies.len();
        self
    }

    pub fn install_stable_identity_endpoints(
        mut self,
        definitions: &[crate::SemanticDefinitionEndpoint],
        modules: &[crate::SemanticModuleEndpoint],
    ) -> Result<Self, DeclarationInstallFailure> {
        let expected_definitions = self
            .binding_manifest()
            .bindings()
            .iter()
            .map(|binding| {
                (
                    binding.file_id.index(),
                    binding.name.to_string(),
                    binding.owner.as_deref().map(str::to_owned),
                    binding.kind,
                )
            })
            .collect::<Vec<_>>();
        let expected_modules = stable_module_files(&self.sema);
        install_stable_identity_endpoints(
            &mut self.sema,
            definitions,
            modules,
            &expected_definitions,
            &expected_modules,
        )?;
        Ok(self)
    }

    pub fn install_specialized_body_candidates<K, M>(
        mut self,
        candidates: Vec<crate::SemanticSpecializedBodyCandidate<K, M>>,
        definition: impl Fn(
            &K,
        ) -> Result<
            crate::SemanticDefinitionToken,
            crate::SemanticStableResolutionFailure,
        >,
        module: impl Fn(
            &M,
        )
            -> Result<crate::SemanticModuleToken, crate::SemanticStableResolutionFailure>,
    ) -> (Self, crate::SemanticSpecializedCandidateInstallWork)
    where
        K: Clone + Ord,
        M: Clone + Ord,
    {
        let mut work = crate::SemanticSpecializedCandidateInstallWork::default();
        for candidate in candidates {
            work.attempts += 1;
            let map_module = |key: &M| module(key);
            let map_definition = |key: &K| definition(key);
            let identity = candidate
                .identity
                .try_map_keys(&map_definition, &map_module);
            let body = candidate.body.try_map_keys(&map_definition, &map_module);
            let dependencies = candidate
                .dependencies
                .iter()
                .map(&map_definition)
                .collect::<Result<Vec<_>, _>>();
            if let (Ok(identity), Ok(body), Ok(dependencies)) = (identity, body, dependencies) {
                self.sema.reusable_specialized_bodies.push(
                    crate::SemanticSpecializedBodyCandidate {
                        identity,
                        body_span: candidate.body_span,
                        body,
                        dependencies: dependencies.into(),
                        dependency_boundary_complete: candidate.dependency_boundary_complete,
                    },
                );
                work.successes += 1;
            } else {
                work.mapping_failures += 1;
            }
        }
        (self, work)
    }

    /// Resolve durable keys into request-independent declaration identities and
    /// stage candidates for the canonical reachable-body worklist. No AIR type,
    /// instruction, string, nominal, or function ID is allocated here.
    pub fn install_ordinary_body_candidates<K, M>(
        mut self,
        candidates: Vec<crate::SemanticBodyCandidate<K, M>>,
        definition: impl Fn(
            &K,
        ) -> Result<
            crate::SemanticDefinitionToken,
            crate::SemanticStableResolutionFailure,
        >,
        module: impl Fn(
            &M,
        )
            -> Result<crate::SemanticModuleToken, crate::SemanticStableResolutionFailure>,
    ) -> Self
    where
        K: Clone + Ord,
        M: Clone + Ord,
    {
        for candidate in candidates {
            let mapped = candidate.body.try_map_keys(&definition, &module);
            if let Ok(body) = mapped {
                self.sema.reusable_ordinary_bodies.insert(
                    candidate.owner,
                    crate::SemanticBodyCandidate {
                        owner: candidate.owner,
                        body_span: candidate.body_span,
                        body,
                    },
                );
            }
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn source_free_function_signatures_are_complete(&self) -> bool {
        self.sema.source_free_function_signatures_are_complete()
    }

    #[cfg(test)]
    pub(crate) fn source_free_function_signature_count(&self) -> usize {
        self.sema.functions_by_file_name.len()
    }

    #[cfg(test)]
    pub(crate) fn analyze_all_bodies_with_namespace_probe(
        self,
    ) -> (
        MultiErrorResult<SemaOutput>,
        super::NamespaceBoundarySnapshot,
        super::NamespaceBoundarySnapshot,
    ) {
        super::analysis::analyze_all_function_bodies_with_namespace_probe_for_test(self.sema)
    }

    /// Install the compiler-issued identity universe used by ordinary body
    /// analysis. Installation is atomic and rejects duplicate, mixed-issuer,
    /// or non-existent endpoints.
    pub fn install_body_owner_tokens(
        mut self,
        endpoints: &[super::BodyOwnerEndpoint],
    ) -> Result<Self, DeclarationInstallFailure> {
        let issuer = endpoints.first().map(|e| e.token.issuer());
        let mut installed = HashMap::with_capacity(endpoints.len());
        let mut tokens = HashSet::with_capacity(endpoints.len());
        for endpoint in endpoints {
            if Some(endpoint.token.issuer()) != issuer {
                return Err(DeclarationInstallFailure::KindMismatch);
            }
            let key = (
                endpoint.file,
                endpoint.name.clone(),
                endpoint.owner_name.clone(),
                endpoint.kind,
            );
            if installed.insert(key, endpoint.token).is_some() {
                return Err(DeclarationInstallFailure::DuplicatePayload);
            }
            if !tokens.insert(endpoint.token) {
                return Err(DeclarationInstallFailure::DuplicatePayload);
            }
        }
        let mut expected = self
            .binding_manifest()
            .bindings()
            .iter()
            .filter_map(|binding| {
                let kind = match binding.kind {
                    StableDefinitionKind::Function => super::BodyOwnerKind::FreeFunction,
                    StableDefinitionKind::Method => super::BodyOwnerKind::Method,
                    StableDefinitionKind::AssociatedFunction => {
                        super::BodyOwnerKind::AssociatedFunction
                    }
                    StableDefinitionKind::Destructor => super::BodyOwnerKind::Destructor,
                    _ => return None,
                };
                let name = if binding.kind == StableDefinitionKind::Destructor {
                    binding
                        .owner
                        .as_deref()
                        .unwrap_or(&binding.name)
                        .to_string()
                } else {
                    binding.name.to_string()
                };
                let owner = if binding.kind == StableDefinitionKind::Destructor {
                    Some(name.clone())
                } else {
                    binding.owner.as_ref().map(ToString::to_string)
                };
                Some((binding.file_id.index(), name, owner, kind))
            })
            .collect::<Vec<_>>();
        expected.sort();
        let mut actual = installed.keys().cloned().collect::<Vec<_>>();
        actual.sort();
        if actual != expected {
            return Err(DeclarationInstallFailure::IdentityMismatch);
        }
        self.sema.body_owner_tokens = std::sync::Arc::new(installed);
        Ok(self)
    }
    pub fn binding_work(&self) -> DeclarationBindingWork {
        self.binding_work
    }

    /// The request-local [`BodySema`] whose declaration maps this bind
    /// populated — the exact reference [`super::body_endpoint::EpochFacts`]
    /// wraps. Test-only: the pool-arc twin-parity tests
    /// (`body_identity.rs`) read the epoch's populated `FunctionInfo` /
    /// `MethodInfo` / RIR-index answers through this handle to compare against
    /// the provider-side pool + RIR index.
    #[cfg(test)]
    pub(in crate::sema) fn body_sema(&self) -> &super::BodySema<'a> {
        &self.sema
    }

    /// Return the frozen semantic answer for a free function's identity.
    /// Provider-boundary parity tests compare this canonical answer with the
    /// independently assembled provider result.
    pub fn function_info(&self, name: lasso::Spur) -> Option<super::info::FunctionInfo> {
        self.sema.function_info(name).copied()
    }

    /// Map a source free-function name in one file to the internal symbol its
    /// declaration was bound under.
    ///
    /// An ordinary function's internal symbol is module-qualified (RUE-1125),
    /// so it is never the source spelling; a caller holding a source name needs
    /// this binding before consulting [`Self::function_info`].
    pub fn free_function_symbol(&self, file: FileId, source_name: Spur) -> Option<Spur> {
        self.sema.resolve_function_name_local(source_name, file)
    }

    /// Map an internal function symbol back to the source name it was declared
    /// under, leaving any other symbol unchanged.
    ///
    /// Parity checks against the durable provider path compare declarations,
    /// and the provider assembles its facts in source-name handles while an
    /// epoch names callables by internal symbol; this is the projection that
    /// puts both in the same terms.
    pub fn function_source_name(&self, symbol: Spur) -> Spur {
        self.sema.source_function_name(symbol)
    }

    /// Return the canonical resolved nominal [`Type`] for `(file, name)`.
    /// Provider-boundary parity checks compare it with the independently
    /// assembled `ProviderEndpointFacts` result.
    /// Trivial delegation to the frozen `BodySema`'s endpoint facts — the exact
    /// `struct_by_file_name` / `enum_by_file_name` lookup the `Named`-nominal arm
    /// of [`super::body_endpoint::resolve_instance_type`] performs, wrapped into
    /// a `Type`. The returned id is epoch-pool-relative, so a caller compares its
    /// index-independent render / metadata (via [`Self::with_type_pool`]), never
    /// the raw id.
    pub fn epoch_nominal_type(&self, file: FileId, name: Spur) -> Option<crate::types::Type> {
        use super::body_endpoint::BodyEndpointProvider;
        let facts = self.sema.endpoint_facts();
        facts
            .struct_by_file_name(file, name)
            .map(crate::types::Type::new_struct)
            .or_else(|| {
                facts
                    .enum_by_file_name(file, name)
                    .map(crate::types::Type::new_enum)
            })
    }

    /// Resolve the module whose stable endpoint names `file` through the
    /// canonical endpoint and module-registry path. The returned compact id is
    /// epoch-relative, so parity checks compare the
    /// module endpoint/file relationship rather than raw indices.
    pub fn epoch_module_type(&self, file: FileId) -> Option<crate::types::Type> {
        use super::body_endpoint::resolve_instance_type;
        let endpoint = self
            .sema
            .stable_module_endpoints
            .values()
            .find(|endpoint| endpoint.file == file.index())?;
        let facts = self.sema.endpoint_facts();
        resolve_instance_type(&facts, &crate::TypeInstanceKey::Module(endpoint.token)).ok()
    }

    /// Recover the current request file for an epoch module type. This is the
    /// index-independent projection compared with
    /// `ProviderEndpointFacts::module_file`.
    pub fn epoch_module_file(&self, ty: crate::types::Type) -> Option<FileId> {
        let id = ty.as_module()?;
        Some(self.sema.module_registry.get_def(id).file_id)
    }

    /// Owned, index-independent presentation facts for an epoch module type.
    pub fn epoch_module_fact(
        &self,
        ty: crate::types::Type,
    ) -> Option<(FileId, String, String, String)> {
        let id = ty.as_module()?;
        let definition = self.sema.module_registry.get_def(id);
        Some((
            definition.file_id,
            definition.file_path,
            definition.import_path,
            definition.durable_id,
        ))
    }

    /// RUE-1091 slice r6a provider surface: the epoch's resolved generated
    /// slice-struct [`Type`] for a generated-struct name, exposed so the
    /// rue-compiler differential compares the provider-driven
    /// `ProviderEndpointFacts` slice minting against the LIVE epoch's generated
    /// slice struct. Delegates to the same `generated_struct` endpoint op the
    /// `Slice` arm of [`super::body_endpoint::resolve_instance_type`] consults,
    /// reading the epoch's `generated_structs` map (populated when the program
    /// materializes the slice). The returned id is epoch-pool-relative, so a
    /// caller compares its index-independent render / metadata via
    /// [`Self::with_type_pool`], never the raw id.
    pub fn epoch_generated_struct_type(&self, name: Spur) -> Option<crate::types::Type> {
        use super::body_endpoint::BodyEndpointProvider;
        let facts = self.sema.endpoint_facts();
        facts
            .generated_struct(name)
            .map(crate::types::Type::new_struct)
    }

    /// RUE-1091 slice r6b provider surface: every anonymous nominal [`Type`] the
    /// epoch minted during declaration binding, exposed so the rue-compiler
    /// differential compares the provider-driven pool
    /// [`super::body_identity::BodyIdentityPool::find_or_create_anon`] against the
    /// LIVE epoch's own `find_or_create_anon_struct` / `find_or_create_anon_enum`
    /// materialization (never the shared digest fn alone). Each id is drawn from
    /// the `anon_*_identities` maps the anonymous-install path populated; a caller
    /// pairs a minted nominal to the epoch's by its `__anon_*_{digest}` name and
    /// compares the full index-independent render / metadata via
    /// [`Self::with_type_pool`] — the digest name matching independently on both
    /// sides is itself the byte-equal-relocation proof.
    pub fn epoch_anonymous_types(&self) -> Vec<crate::types::Type> {
        self.sema
            .anon_struct_identities
            .values()
            .map(|id| crate::types::Type::new_struct(*id))
            .chain(
                self.sema
                    .anon_enum_identities
                    .values()
                    .map(|id| crate::types::Type::new_enum(*id)),
            )
            .collect()
    }

    /// RUE-1091 slice r6c provider surface: the anonymous nominal [`Type`]s of
    /// the identities recorded in the epoch's `well_known_option_identities` —
    /// the well-known `Option` enums the registry install
    /// ([`Self::install_well_known_option_types`]) materialized and marked for
    /// the export-as-produced ruling. `analyze_body_analysis` subtracts exactly this
    /// set from its initial anonymous baseline, so the single export funnel
    /// (`semantic_body_export.rs` via `produced_anonymous_nominals`) publishes
    /// these identities as PRODUCED anonymous nominals of the analyzed body,
    /// never as pre-existing imports. Exposed so the rue-compiler differential
    /// proves the provider-side pool records the identical ruling for
    /// byte-identical identities. Deterministic order: the identity set is a
    /// `BTreeSet`.
    pub fn epoch_well_known_option_types(&self) -> Vec<crate::types::Type> {
        self.sema
            .well_known_option_identities
            .iter()
            .filter_map(|identity| {
                self.sema
                    .anon_enum_identities
                    .get(identity)
                    .map(|id| crate::types::Type::new_enum(*id))
                    .or_else(|| {
                        self.sema
                            .anon_struct_identities
                            .get(identity)
                            .map(|id| crate::types::Type::new_struct(*id))
                    })
            })
            .collect()
    }

    /// RUE-1091 slice r6c provider surface: the epoch's per-body well-known
    /// `Option` demand registry — each `(payload, option)` [`Type`] pair the
    /// install recorded into `well_known_option_by_payload`, the map
    /// fallible-intrinsic resolution (`resolve_option_result_type`) consults.
    /// Exposed so the rue-compiler differential compares the pool-side registry
    /// against the LIVE epoch's. Iteration order is map-internal; a caller
    /// sorts by an index-independent render before comparing.
    pub fn epoch_well_known_option_registry(
        &self,
    ) -> Vec<(crate::types::Type, crate::types::Type)> {
        self.sema
            .well_known_option_by_payload
            .iter()
            .map(|(payload, option)| (*payload, *option))
            .collect()
    }

    /// Return canonical value-constant info for the exact storage key
    /// `(declaring_file, source_name)`. Parity checks compare this result with
    /// the body pool's durable-record + RIR-handle assembly.
    pub fn epoch_const_info(
        &self,
        declaring_file: FileId,
        source_name: Spur,
    ) -> Option<super::info::ConstInfo> {
        self.sema
            .value_const(&(declaring_file, source_name))
            .cloned()
    }

    /// Return canonical module-binding info for the exact storage key.
    pub fn epoch_module_binding_info(
        &self,
        declaring_file: FileId,
        source_name: Spur,
    ) -> Option<super::info::ConstInfo> {
        self.sema
            .module_binding(&(declaring_file, source_name))
            .cloned()
    }

    /// Return canonical `MethodInfo` for a `(file, type_name, method)` named
    /// method. Provider-boundary parity checks compare this with the
    /// independently assembled `ProviderCallFacts` result. The
    /// receiver preimage `(file, type_name)` is resolved to the epoch's
    /// pool-minted `StructId` through the endpoint facts — the exact
    /// `struct_by_file_name` → `method_info((struct_id, method))` path the epoch
    /// analyzer performs. The returned info's ids are epoch-pool-relative, so a
    /// caller compares its index-independent render / metadata, never a raw id.
    pub fn epoch_method_info(
        &self,
        file: FileId,
        type_name: Spur,
        method: Spur,
    ) -> Option<super::info::MethodInfo> {
        use super::body_endpoint::BodyEndpointProvider;
        let facts = self.sema.endpoint_facts();
        let struct_id = facts.struct_by_file_name(file, type_name)?;
        facts.method_info(struct_id, method)
    }

    /// Return the canonical
    /// [`select_module_type_member`](super::aggregate_resolution::select_module_type_member)
    /// winner for `(file, name)`, projected to the shared
    /// [`ProviderModuleMember`](crate::ProviderModuleMember) so the differential
    /// lets parity checks compare `ProviderAggregateFacts` selection against
    /// the canonical struct→enum→const short-circuit. Runs the same free function the analyzer
    /// runs, driven over the epoch `AggregateFacts`.
    pub fn epoch_module_type_member(
        &self,
        file: FileId,
        name: Spur,
    ) -> crate::ProviderModuleMember {
        use super::aggregate_resolution::{ModuleTypeMember, select_module_type_member};
        let facts = self.sema.aggregate_facts();
        match select_module_type_member(&facts, file, name) {
            ModuleTypeMember::Struct(id) => {
                crate::ProviderModuleMember::Struct(crate::types::Type::new_struct(id))
            }
            ModuleTypeMember::Enum(id) => {
                crate::ProviderModuleMember::Enum(crate::types::Type::new_enum(id))
            }
            ModuleTypeMember::Const(_) => crate::ProviderModuleMember::Const,
            ModuleTypeMember::Absent => crate::ProviderModuleMember::Absent,
        }
    }

    /// Return the canonical
    /// [`select_qualified_type`](super::aggregate_resolution::select_qualified_type)
    /// winner (enum→struct order) for `(file, name)`.
    pub fn epoch_qualified_type(&self, file: FileId, name: Spur) -> crate::ProviderQualifiedType {
        use super::aggregate_resolution::{QualifiedType, select_qualified_type};
        let facts = self.sema.aggregate_facts();
        match select_qualified_type(&facts, file, name) {
            QualifiedType::Enum(id) => {
                crate::ProviderQualifiedType::Enum(crate::types::Type::new_enum(id))
            }
            QualifiedType::Struct(id) => {
                crate::ProviderQualifiedType::Struct(crate::types::Type::new_struct(id))
            }
            QualifiedType::Absent => crate::ProviderQualifiedType::Absent,
        }
    }

    /// Return the canonical
    /// [`select_struct_literal_head`](super::aggregate_resolution::select_struct_literal_head)
    /// winner for an unqualified head `(file, name)` (no local type).
    pub fn epoch_struct_literal_head(&self, file: FileId, name: Spur) -> crate::ProviderStructHead {
        use super::aggregate_resolution::{StructLiteralHead, select_struct_literal_head};
        let facts = self.sema.aggregate_facts();
        match select_struct_literal_head(&facts, None, file, name) {
            StructLiteralHead::Bound(ty) => crate::ProviderStructHead::Bound(ty),
            StructLiteralHead::Named(id) => {
                crate::ProviderStructHead::Named(crate::types::Type::new_struct(id))
            }
            StructLiteralHead::Absent => crate::ProviderStructHead::Absent,
        }
    }

    /// Return the canonical
    /// [`is_accessible`](super::aggregate_resolution::is_accessible) visibility
    /// decision for `(accessing_file, defining_file, is_public)`, so parity
    /// checks prove the provider driver's registered-path visibility domain
    /// matches the epoch's `get_file_path`-derived one.
    pub fn epoch_is_accessible(
        &self,
        accessing_file: FileId,
        defining_file: FileId,
        is_public: bool,
    ) -> bool {
        use super::aggregate_resolution::is_accessible;
        let facts = self.sema.aggregate_facts();
        is_accessible(&facts, accessing_file, defining_file, is_public)
    }

    /// Return the canonical physical file path for `file`, so parity checks
    /// register the same path into the provider
    /// driver's request-local path overlay that `is_accessible` consults.
    pub fn epoch_file_path(&self, file: FileId) -> Option<String> {
        self.sema.get_file_path(file).map(str::to_owned)
    }

    /// Read the populated [`TypeInternPool`](crate::intern_pool::TypeInternPool)
    /// under a closure so provider-boundary parity checks use the same
    /// index-independent rendering on both sides.
    pub fn with_type_pool<R>(
        &self,
        read: impl FnOnce(&crate::intern_pool::TypeInternPool) -> R,
    ) -> R {
        read(&self.sema.type_pool)
    }

    /// Materialize the owned manifest on demand. Ordinary body analysis does
    /// not pay for this additional RIR traversal.
    pub fn binding_manifest(&self) -> &SemanticBindingManifest {
        self.manifest
            .get_or_init(|| self.sema.build_binding_manifest())
    }

    /// Whether a caller has requested the optional binding manifest.
    pub fn manifest_is_materialized(&self) -> bool {
        self.manifest.get().is_some()
    }

    /// Lazily export resolved declaration semantics and invoke `convert` while
    /// the binder, type pool, interner and module registry are all alive.
    /// No RIR traversal or second bind is performed.
    pub fn with_declaration_semantics<R>(
        &self,
        convert: impl FnOnce(&[SemanticDeclarationExport], SemanticDeclarationExportWork) -> R,
    ) -> Result<R, SemanticExportFailure> {
        let manifest = self.binding_manifest();
        let (records, work) = self.sema.build_declaration_semantics(manifest)?;
        Ok(convert(&records, work))
    }

    /// Export resolved declarations using the stable shells captured before
    /// resolution. Unlike [`Self::with_declaration_semantics`], this does not
    /// materialize the binding manifest or traverse RIR.
    pub fn with_declaration_semantics_from_shells<R>(
        &self,
        shells: &[SemanticDeclarationShell],
        convert: impl FnOnce(&[SemanticDeclarationExport], SemanticDeclarationExportWork) -> R,
    ) -> Result<R, SemanticExportFailure> {
        let bindings = shells
            .iter()
            .map(|shell| SemanticBinding {
                file_id: shell.declaration_span.file_id,
                declaration_span: shell.declaration_span,
                namespace: shell.identity.namespace,
                kind: shell.identity.kind,
                name: shell.identity.name.clone(),
                owner: shell.identity.owner.clone(),
                is_public: shell.is_public,
            })
            .collect();
        let manifest = SemanticBindingManifest {
            bindings,
            work: SemanticBindingManifestWork::default(),
        };
        let (records, work) = self.sema.build_declaration_semantics(&manifest)?;
        Ok(convert(&records, work))
    }

    /// Test-only oracle for the retired whole-program body driver.
    pub fn analyze_all_bodies_for_test(self) -> MultiErrorResult<SemaOutput> {
        self.sema.analyze_all_bodies_for_test()
    }
    pub fn compose_queried_bodies<K, M>(
        self,
        candidates: Vec<crate::SemanticQueriedBodyCandidate<K, M>>,
        definition: impl Fn(
            &K,
        ) -> Result<
            crate::SemanticDefinitionToken,
            crate::SemanticStableResolutionFailure,
        >,
        module: impl Fn(
            &M,
        )
            -> Result<crate::SemanticModuleToken, crate::SemanticStableResolutionFailure>,
    ) -> Result<SemaOutput, super::BodyAnalysisFailure>
    where
        K: Clone,
        M: Clone,
    {
        let candidates = candidates
            .into_iter()
            .map(|candidate| {
                Ok(crate::SemanticQueriedBodyCandidate {
                    identity: candidate
                        .identity
                        .try_map_identities(&definition, &module)?,
                    ordinary_owner: candidate.ordinary_owner,
                    specialization_identity: candidate
                        .specialization_identity
                        .map(|identity| identity.try_map_keys(&definition, &module))
                        .transpose()?,
                    body_span: candidate.body_span,
                    body: candidate.body.try_map_keys(&definition, &module)?,
                })
            })
            .collect::<Result<Vec<_>, crate::SemanticStableResolutionFailure>>()
            .map_err(|failure| {
                super::BodyAnalysisFailure::new(
                    CompileErrors::from(CompileError::without_span(ErrorKind::InternalError(
                        format!("queried body candidate mapping failed: {failure:?}"),
                    ))),
                    super::BodyAnalysisWork::default(),
                )
            })?;
        super::analysis::compose_queried_bodies(self.sema, candidates)
    }
}

impl<'a, D: super::DeclarationPhase> Sema<'a, D> {
    pub(super) fn attach_imported_declaration_shells(
        &self,
        imported: &[SemanticDeclarationShell],
    ) -> CompileResult<(Vec<PendingDeclarationPayload>, Vec<PendingNominalPayload>)> {
        let mut remaining = HashMap::with_capacity(imported.len());
        for shell in imported {
            if remaining.insert(shell.declaration_span, shell).is_some() {
                return Err(CompileError::new(
                    ErrorKind::InternalError(
                        "query-owned declaration shells have an ambiguous current locator".into(),
                    ),
                    shell.declaration_span,
                ));
            }
        }
        let mut pending = Vec::new();
        let mut nominals = Vec::new();
        for candidate in self.declaration_index.shell_declarations() {
            let inst_ref = candidate.declaration;
            let inst = self.rir.get(inst_ref);
            let Some(imported_shell) = remaining.remove(&inst.span) else {
                return Err(CompileError::new(
                    ErrorKind::InternalError(
                        "query-owned declaration shell has no current RIR locator".into(),
                    ),
                    inst.span,
                ));
            };
            let mut shell = imported_shell.clone();
            shell.source_order = candidate.source_order;
            let expected_module = self
                .get_symbol_path(inst.span.file_id)
                .map(crate::path_norm::normalize_module_path)
                .unwrap_or_else(|| format!("file{}", inst.span.file_id.index()));
            if shell.identity.module_path.as_ref() != expected_module
                || shell.identity.is_trusted_standard_library
                    != self
                        .trusted_standard_library_files
                        .contains(&inst.span.file_id)
            {
                return Err(CompileError::new(
                    ErrorKind::InternalError(
                        "query-owned declaration shell has foreign module identity".into(),
                    ),
                    inst.span,
                ));
            }
            let source = match &inst.data {
                InstData::StructDecl { name, is_pub, .. }
                | InstData::EnumDecl { name, is_pub, .. } => {
                    let kind = if matches!(inst.data, InstData::StructDecl { .. }) {
                        StableDefinitionKind::Struct
                    } else {
                        StableDefinitionKind::Enum
                    };
                    validate_imported_shell(
                        self,
                        &shell,
                        *name,
                        None,
                        kind,
                        *is_pub,
                        &[],
                        false,
                        RirParamMode::Normal,
                        false,
                        false,
                        false,
                    )?;
                    nominals.push(PendingNominalPayload {
                        shell,
                        declaration: inst_ref,
                    });
                    continue;
                }
                InstData::FnDecl {
                    is_pub,
                    is_unchecked,
                    is_extern,
                    name,
                    params,
                    body,
                    has_self,
                    self_mode,
                    self_is_mut,
                    ..
                } if !self.declaration_index.is_anonymous_method(inst_ref) => {
                    let owner = candidate.named_method_owner;
                    let kind = match (owner, *has_self) {
                        (Some(_), true) => StableDefinitionKind::Method,
                        (Some(_), false) => StableDefinitionKind::AssociatedFunction,
                        (None, _) => StableDefinitionKind::Function,
                    };
                    let params = self.rir.params(params).to_vec();
                    validate_imported_shell(
                        self,
                        &shell,
                        *name,
                        owner,
                        kind,
                        *is_pub,
                        &params,
                        *has_self,
                        *self_mode,
                        *self_is_mut,
                        *is_unchecked,
                        *is_extern,
                    )?;
                    DeclarationPayloadSource::Callable { body: *body }
                }
                InstData::ConstDecl {
                    is_pub, name, init, ..
                } => {
                    validate_imported_shell(
                        self,
                        &shell,
                        *name,
                        None,
                        StableDefinitionKind::ValueConst,
                        *is_pub,
                        &[],
                        false,
                        RirParamMode::Normal,
                        false,
                        false,
                        false,
                    )?;
                    DeclarationPayloadSource::Const { initializer: *init }
                }
                InstData::DropFnDecl { type_name, body } => {
                    validate_imported_shell(
                        self,
                        &shell,
                        *type_name,
                        Some(*type_name),
                        StableDefinitionKind::Destructor,
                        false,
                        &[],
                        true,
                        RirParamMode::Normal,
                        false,
                        false,
                        false,
                    )?;
                    DeclarationPayloadSource::Destructor { body: *body }
                }
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::InternalError(
                            "query-owned shell selected an unsupported RIR declaration".into(),
                        ),
                        inst.span,
                    ));
                }
            };
            pending.push(PendingDeclarationPayload {
                shell,
                declaration: inst_ref,
                source,
            });
        }
        if let Some(unmatched) = remaining.values().next() {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "query-owned declaration shell has no indexed RIR declaration".into(),
                ),
                unmatched.declaration_span,
            ));
        }
        pending.sort_by(|left, right| left.shell.identity.cmp(&right.shell.identity));
        nominals.sort_by(|left, right| left.shell.identity.cmp(&right.shell.identity));
        Ok((pending, nominals))
    }

    pub(super) fn predeclare_callable_value_shells(
        &self,
    ) -> (Vec<PendingDeclarationPayload>, Vec<PendingNominalPayload>) {
        let module_path = |file_id: FileId| -> Arc<str> {
            Arc::from(
                self.get_symbol_path(file_id)
                    .map(crate::path_norm::normalize_module_path)
                    .unwrap_or_else(|| format!("file{}", file_id.index())),
            )
        };
        let mut pending = Vec::new();
        let mut nominals = Vec::new();
        for candidate in self.declaration_index.shell_declarations() {
            let inst_ref = candidate.declaration;
            let source_order = candidate.source_order;
            let inst = self.rir.get(inst_ref);
            if let InstData::StructDecl { name, is_pub, .. }
            | InstData::EnumDecl { name, is_pub, .. } = &inst.data
            {
                let kind = if matches!(inst.data, InstData::StructDecl { .. }) {
                    StableDefinitionKind::Struct
                } else {
                    StableDefinitionKind::Enum
                };
                nominals.push(PendingNominalPayload {
                    shell: SemanticDeclarationShell {
                        identity: SemanticDeclarationShellIdentity {
                            module_path: module_path(inst.span.file_id),
                            is_trusted_standard_library: self
                                .trusted_standard_library_files
                                .contains(&inst.span.file_id),
                            namespace: StableDefinitionNamespace::Type,
                            kind,
                            name: Arc::from(self.interner.resolve(name)),
                            owner: None,
                        },
                        declaration_span: inst.span,
                        parameter_names: Arc::from([]),
                        parameter_modes: Arc::from([]),
                        parameter_comptime: Arc::from([]),
                        source_order,
                        has_self: false,
                        receiver_mode: None,
                        receiver_is_mut: false,
                        is_generic: false,
                        is_public: *is_pub,
                        is_unchecked: false,
                        is_extern: false,
                        signature_fingerprint: [0; 32],
                    },
                    declaration: inst_ref,
                });
            }
            let (
                identity,
                parameter_names,
                parameter_modes,
                parameter_comptime,
                has_self,
                receiver_mode,
                receiver_is_mut,
                is_generic,
                is_public,
                is_unchecked,
                is_extern,
                source,
            ) = match &inst.data {
                InstData::FnDecl {
                    is_pub,
                    is_unchecked,
                    is_extern,
                    name,
                    params,
                    body,
                    has_self,
                    self_mode,
                    self_is_mut,
                    ..
                } if !self.declaration_index.is_anonymous_method(inst_ref) => {
                    let owner = candidate.named_method_owner;
                    let kind = match (owner, *has_self) {
                        (Some(_), true) => StableDefinitionKind::Method,
                        (Some(_), false) => StableDefinitionKind::AssociatedFunction,
                        (None, _) => StableDefinitionKind::Function,
                    };
                    let params = self.rir.params(params);
                    let names = params
                        .iter()
                        .map(|param| Arc::from(self.interner.resolve(&param.name)))
                        .collect::<Vec<_>>();
                    let modes = params.iter().map(|param| param.mode).collect::<Vec<_>>();
                    let comptime = params
                        .iter()
                        .map(|param| param.is_comptime)
                        .collect::<Vec<_>>();
                    let identity = SemanticDeclarationShellIdentity {
                        module_path: module_path(inst.span.file_id),
                        is_trusted_standard_library: self
                            .trusted_standard_library_files
                            .contains(&inst.span.file_id),
                        namespace: if owner.is_some() {
                            StableDefinitionNamespace::Method
                        } else {
                            StableDefinitionNamespace::Value
                        },
                        kind,
                        name: Arc::from(self.interner.resolve(name)),
                        owner: owner.map(|owner| Arc::from(self.interner.resolve(&owner))),
                    };
                    (
                        identity,
                        names,
                        modes,
                        comptime.clone(),
                        *has_self,
                        has_self.then_some(*self_mode),
                        *self_is_mut,
                        comptime.into_iter().any(|value| value),
                        *is_pub,
                        *is_unchecked,
                        *is_extern,
                        DeclarationPayloadSource::Callable { body: *body },
                    )
                }
                InstData::ConstDecl {
                    is_pub, name, init, ..
                } => (
                    SemanticDeclarationShellIdentity {
                        module_path: module_path(inst.span.file_id),
                        is_trusted_standard_library: self
                            .trusted_standard_library_files
                            .contains(&inst.span.file_id),
                        namespace: StableDefinitionNamespace::Value,
                        // Function aliases are classified only after evaluating
                        // the initializer; their stable value identity is already
                        // complete here.
                        kind: StableDefinitionKind::ValueConst,
                        name: Arc::from(self.interner.resolve(name)),
                        owner: None,
                    },
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    false,
                    None,
                    false,
                    false,
                    *is_pub,
                    false,
                    false,
                    DeclarationPayloadSource::Const { initializer: *init },
                ),
                InstData::DropFnDecl { type_name, body } => (
                    SemanticDeclarationShellIdentity {
                        module_path: module_path(inst.span.file_id),
                        is_trusted_standard_library: self
                            .trusted_standard_library_files
                            .contains(&inst.span.file_id),
                        namespace: StableDefinitionNamespace::Destructor,
                        kind: StableDefinitionKind::Destructor,
                        name: Arc::from(self.interner.resolve(type_name)),
                        owner: Some(Arc::from(self.interner.resolve(type_name))),
                    },
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    true,
                    Some(RirParamMode::Normal),
                    false,
                    false,
                    false,
                    false,
                    false,
                    DeclarationPayloadSource::Destructor { body: *body },
                ),
                _ => continue,
            };
            pending.push(PendingDeclarationPayload {
                shell: SemanticDeclarationShell {
                    identity,
                    declaration_span: inst.span,
                    parameter_names: parameter_names.into(),
                    parameter_modes: parameter_modes.into(),
                    parameter_comptime: parameter_comptime.into(),
                    source_order,
                    has_self,
                    receiver_mode,
                    receiver_is_mut,
                    is_generic,
                    is_public,
                    is_unchecked,
                    is_extern,
                    signature_fingerprint: [0; 32],
                },
                declaration: inst_ref,
                source,
            });
        }
        pending.sort_by(|left, right| left.shell.identity.cmp(&right.shell.identity));
        nominals.sort_by(|left, right| left.shell.identity.cmp(&right.shell.identity));
        (pending, nominals)
    }
}

fn validate_imported_shell<D: super::DeclarationPhase>(
    sema: &Sema<'_, D>,
    shell: &SemanticDeclarationShell,
    name: Spur,
    owner: Option<Spur>,
    kind: StableDefinitionKind,
    is_public: bool,
    params: &[rue_rir::RirParam],
    has_self: bool,
    receiver_mode: RirParamMode,
    receiver_is_mut: bool,
    is_unchecked: bool,
    is_extern: bool,
) -> CompileResult<()> {
    let expected_name = sema.interner.resolve(&name);
    let expected_owner = owner.map(|owner| sema.interner.resolve(&owner));
    let expected_generic = params.iter().any(|parameter| parameter.is_comptime);
    let parameter_shape_matches = shell.parameter_names.len() == params.len()
        && shell.parameter_modes.len() == params.len()
        && shell.parameter_comptime.len() == params.len()
        && params.iter().enumerate().all(|(index, parameter)| {
            shell.parameter_names[index].as_ref() == sema.interner.resolve(&parameter.name)
                && shell.parameter_modes[index] == parameter.mode
                && shell.parameter_comptime[index] == parameter.is_comptime
        });
    if shell.identity.namespace != kind.namespace()
        || shell.identity.kind != kind
        || shell.identity.name.as_ref() != expected_name
        || shell.identity.owner.as_deref() != expected_owner
        || shell.is_public != is_public
        || shell.has_self != has_self
        || shell.receiver_mode != has_self.then_some(receiver_mode)
        || shell.receiver_is_mut != receiver_is_mut
        || shell.is_generic != expected_generic
        || shell.is_unchecked != is_unchecked
        || shell.is_extern != is_extern
        || !parameter_shape_matches
    {
        return Err(CompileError::new(
            ErrorKind::InternalError(
                "query-owned declaration shell does not match its current RIR declaration".into(),
            ),
            shell.declaration_span,
        ));
    }
    Ok(())
}

impl<'a, D: super::DeclarationPhase> Sema<'a, D> {
    fn export_type(
        &self,
        ty: Type,
        stack: &mut Vec<Type>,
    ) -> Result<SemanticExportType, SemanticExportFailure> {
        self.type_pool
            .validate_complete_type(ty)
            .map_err(|_| SemanticExportFailure::ErrorType)?;
        if stack.contains(&ty) {
            return Err(SemanticExportFailure::RecursiveStructuralType);
        }
        let primitive = match ty.kind() {
            TypeKind::I8 => Some(SemanticExportType::I8),
            TypeKind::I16 => Some(SemanticExportType::I16),
            TypeKind::I32 => Some(SemanticExportType::I32),
            TypeKind::I64 => Some(SemanticExportType::I64),
            TypeKind::U8 => Some(SemanticExportType::U8),
            TypeKind::U16 => Some(SemanticExportType::U16),
            TypeKind::U32 => Some(SemanticExportType::U32),
            TypeKind::U64 => Some(SemanticExportType::U64),
            TypeKind::Bool => Some(SemanticExportType::Bool),
            TypeKind::Unit => Some(SemanticExportType::Unit),
            TypeKind::Never => Some(SemanticExportType::Never),
            TypeKind::ComptimeType => Some(SemanticExportType::ComptimeType),
            TypeKind::Error => return Err(SemanticExportFailure::ErrorType),
            _ => None,
        };
        if let Some(value) = primitive {
            return Ok(value);
        }
        stack.push(ty);
        let result = match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self.type_pool.struct_def(id);
                if let Some(element) = self.slice_element_type(ty)
                    && def.name.starts_with('[')
                {
                    Ok(SemanticExportType::Slice {
                        element: Box::new(self.export_type(element, stack)?),
                        name: Arc::from(def.name.as_str()),
                    })
                } else if def.is_builtin {
                    Ok(SemanticExportType::BuiltinNominal {
                        name: Arc::from(def.name.as_str()),
                        kind: crate::SemanticImportNominalKind::Struct,
                    })
                } else {
                    let symbol = self.interner.get_or_intern(&def.name);
                    if self.structs_by_file_name.get(&(def.file_id, symbol)) != Some(&id) {
                        return Err(SemanticExportFailure::AnonymousNominalType);
                    }
                    Ok(SemanticExportType::Nominal(SemanticNominalIdentity {
                        file_id: def.file_id,
                        name: Arc::from(def.name.as_str()),
                        kind: StableDefinitionKind::Struct,
                    }))
                }
            }
            TypeKind::Enum(id) => {
                let def = self.type_pool.enum_def(id);
                let symbol = self.interner.get_or_intern(&def.name);
                if self.builtin_enums.get(&symbol) == Some(&id) {
                    Ok(SemanticExportType::BuiltinNominal {
                        name: Arc::from(def.name.as_str()),
                        kind: crate::SemanticImportNominalKind::Enum,
                    })
                } else {
                    Ok(SemanticExportType::Nominal(SemanticNominalIdentity {
                        file_id: def.file_id,
                        name: Arc::from(def.name.as_str()),
                        kind: StableDefinitionKind::Enum,
                    }))
                }
            }
            TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                Ok(SemanticExportType::Array {
                    element: Box::new(self.export_type(element, stack)?),
                    len,
                })
            }
            TypeKind::PtrConst(id) => Ok(SemanticExportType::PtrConst(Box::new(
                self.export_type(self.type_pool.ptr_const_def(id), stack)?,
            ))),
            TypeKind::PtrMut(id) => Ok(SemanticExportType::PtrMut(Box::new(
                self.export_type(self.type_pool.ptr_mut_def(id), stack)?,
            ))),
            TypeKind::Module(id) => Ok(SemanticExportType::Module(Arc::from(
                self.module_registry.get_def(id).durable_id,
            ))),
            _ => unreachable!(),
        };
        stack.pop();
        result
    }

    fn export_function_signature(
        &self,
        info: &super::FunctionInfo,
    ) -> Result<(Arc<[SemanticExportParameter]>, SemanticExportType), SemanticExportFailure> {
        self.export_callable_signature(
            info.params,
            info.return_type,
            info.rir_params(self.rir),
            info.return_type_sym,
        )
    }

    fn export_callable_signature(
        &self,
        range: crate::ParamRange,
        return_type: Type,
        rir_params: &rue_rir::RirParamsRange,
        return_type_sym: Spur,
    ) -> Result<(Arc<[SemanticExportParameter]>, SemanticExportType), SemanticExportFailure> {
        let rir_params = self.rir.params(rir_params);
        let type_name = self.interner.get("type");
        let generic_names = rir_params
            .iter()
            .filter(|param| param.is_comptime && Some(param.ty) == type_name)
            .map(|param| param.name)
            .collect::<Vec<_>>();
        let convert = |ty: Type, symbol: Spur| {
            if ty == Type::COMPTIME_TYPE {
                let syntax = self.interner.resolve(&symbol);
                if syntax != "type" {
                    return self.export_deferred_signature_type(syntax, &generic_names);
                }
            }
            self.export_type(ty, &mut Vec::new())
        };
        let parameters = self
            .param_arena
            .types(range)
            .iter()
            .zip(self.param_arena.modes(range))
            .zip(self.param_arena.comptime(range))
            .zip(self.param_arena.names(range))
            .zip(&rir_params)
            .map(|((((&ty, &mode), &is_comptime), &name), rir)| {
                use rue_rir::RirParamMode;
                let mode = match mode {
                    RirParamMode::Normal => SemanticParameterMode::Value,
                    RirParamMode::Borrow => SemanticParameterMode::Borrow,
                    RirParamMode::Inout => SemanticParameterMode::Inout,
                };
                Ok(SemanticExportParameter {
                    name: Arc::from(self.interner.resolve(&name)),
                    ty: convert(ty, rir.ty)?,
                    mode,
                    is_comptime,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((parameters.into(), convert(return_type, return_type_sym)?))
    }

    fn export_deferred_signature_type(
        &self,
        syntax: &str,
        generic_names: &[Spur],
    ) -> Result<SemanticExportType, SemanticExportFailure> {
        if let Some(index) = generic_names
            .iter()
            .position(|name| self.interner.resolve(name) == syntax)
        {
            return Ok(SemanticExportType::GenericParameter(index as u32));
        }
        if let Some((element, len)) = parse_array_type_syntax(syntax) {
            let ArrayLen::Literal(len) = len else {
                return Err(SemanticExportFailure::UnsupportedGenericSignature);
            };
            return Ok(SemanticExportType::Array {
                element: Box::new(self.export_deferred_signature_type(&element, generic_names)?),
                len,
            });
        }
        if let Some((pointee, mutability)) = parse_pointer_type_syntax(syntax) {
            let pointee = Box::new(self.export_deferred_signature_type(&pointee, generic_names)?);
            return Ok(match mutability {
                PtrMutability::Const => SemanticExportType::PtrConst(pointee),
                PtrMutability::Mut => SemanticExportType::PtrMut(pointee),
            });
        }
        Err(SemanticExportFailure::UnsupportedGenericSignature)
    }

    fn build_declaration_semantics(
        &self,
        manifest: &SemanticBindingManifest,
    ) -> Result<
        (
            Vec<SemanticDeclarationExport>,
            SemanticDeclarationExportWork,
        ),
        SemanticExportFailure,
    > {
        let mut records = Vec::with_capacity(manifest.bindings.len());
        for identity in manifest.bindings.iter() {
            let name = self.interner.get_or_intern(identity.name.as_ref());
            // Pre-resolution const shells cannot distinguish value constants
            // from module bindings. Classify them from the successfully bound
            // namespace while exporting, then carry that exact kind across
            // the owned declaration boundary.
            let mut identity = identity.clone();
            if identity.kind == StableDefinitionKind::ValueConst
                && self.module_binding(&(identity.file_id, name)).is_some()
            {
                identity.kind = StableDefinitionKind::ModuleBinding;
            }
            let payload = match identity.kind {
                StableDefinitionKind::Function => {
                    let internal = *self
                        .functions_by_file_name
                        .get(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let info = self
                        .functions
                        .get(&internal)
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let (parameters, result) = self.export_function_signature(info)?;
                    SemanticDeclarationPayload::Callable {
                        parameters,
                        result,
                        has_self: false,
                        self_mode: SemanticParameterMode::Value,
                        is_unchecked: info.is_unchecked,
                    }
                }
                StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction => {
                    let owner = self.interner.get_or_intern(
                        identity
                            .owner
                            .as_deref()
                            .ok_or(SemanticExportFailure::UnmappedNominalType)?,
                    );
                    let sid = *self
                        .structs_by_file_name
                        .get(&(identity.file_id, owner))
                        .ok_or(SemanticExportFailure::UnmappedNominalType)?;
                    let info = self
                        .methods
                        .get(&(sid, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let declaration = *self
                        .named_method_declarations
                        .get(&(sid, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let InstData::FnDecl {
                        params,
                        return_type,
                        ..
                    } = &self.rir.get(declaration).data
                    else {
                        return Err(SemanticExportFailure::UnmappedFunction);
                    };
                    let (parameters, result) = self.export_callable_signature(
                        info.params,
                        info.return_type,
                        params,
                        *return_type,
                    )?;
                    SemanticDeclarationPayload::Callable {
                        parameters,
                        result,
                        has_self: info.has_self,
                        self_mode: match info.self_mode {
                            RirParamMode::Borrow => SemanticParameterMode::Borrow,
                            RirParamMode::Inout => SemanticParameterMode::Inout,
                            _ => SemanticParameterMode::Value,
                        },
                        is_unchecked: false,
                    }
                }
                StableDefinitionKind::Struct => {
                    let sid = *self
                        .structs_by_file_name
                        .get(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedNominalType)?;
                    let def = self.type_pool.struct_def(sid);
                    let fields = def
                        .fields
                        .iter()
                        .map(|f| {
                            Ok((
                                Arc::from(f.name.as_str()),
                                self.export_type(f.ty, &mut Vec::new())?,
                            ))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    SemanticDeclarationPayload::Struct {
                        fields: fields.into(),
                        is_copy: def.is_copy,
                        is_linear: def.is_linear,
                    }
                }
                StableDefinitionKind::Enum => {
                    let eid = *self
                        .enums_by_file_name
                        .get(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedNominalType)?;
                    let def = self.type_pool.enum_def(eid);
                    let variants = def
                        .variants
                        .iter()
                        .enumerate()
                        .map(|(i, n)| {
                            let payload = def
                                .variant_payload(i)
                                .iter()
                                .map(|&t| self.export_type(t, &mut Vec::new()))
                                .collect::<Result<Vec<_>, _>>()?;
                            Ok((Arc::from(n.as_str()), Arc::from(payload)))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    SemanticDeclarationPayload::Enum {
                        variants: variants.into(),
                    }
                }
                StableDefinitionKind::ValueConst => {
                    let info = self
                        .value_const(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let value = match info.value {
                        ConstValue::Integer(v) => SemanticExportConstValue::Integer(v),
                        ConstValue::Bool(v) => SemanticExportConstValue::Bool(v),
                        ConstValue::Type(t) => {
                            SemanticExportConstValue::Type(self.export_type(t, &mut Vec::new())?)
                        }
                        ConstValue::Unit => SemanticExportConstValue::Unit,
                        ConstValue::String(content) => SemanticExportConstValue::String(Arc::from(
                            self.interner.resolve(&content),
                        )),
                        ConstValue::Function(symbol) => {
                            let fi = self
                                .functions
                                .get(&symbol)
                                .ok_or(SemanticExportFailure::UnmappedFunction)?;
                            let source =
                                *self.function_source_names.get(&symbol).unwrap_or(&symbol);
                            SemanticExportConstValue::Function {
                                file_id: fi.file_id,
                                name: Arc::from(self.interner.resolve(&source)),
                            }
                        }
                    };
                    SemanticDeclarationPayload::Const {
                        ty: self.export_type(info.ty, &mut Vec::new())?,
                        value,
                    }
                }
                StableDefinitionKind::ModuleBinding => {
                    let info = self
                        .module_binding(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    SemanticDeclarationPayload::ModuleBinding {
                        target: self.export_type(info.ty, &mut Vec::new())?,
                    }
                }
                StableDefinitionKind::Destructor => SemanticDeclarationPayload::Destructor,
            };
            records.push(SemanticDeclarationExport { identity, payload });
        }
        let len = records.len();
        Ok((
            records,
            SemanticDeclarationExportWork {
                build_invocations: 1,
                declarations_exported: len,
                rir_instructions_visited: 0,
            },
        ))
    }
    fn build_binding_manifest(&self) -> SemanticBindingManifest {
        let mut bindings = Vec::new();
        let mut work = SemanticBindingManifestWork {
            build_invocations: 1,
            anonymous_methods_deferred: self.declaration_index.work().anonymous_methods_indexed,
            ..SemanticBindingManifestWork::default()
        };
        for (inst_ref, inst) in self.rir.iter() {
            work.rir_instructions_visited += 1;
            let mut emit = |file_id: FileId,
                            declaration_span: Span,
                            namespace: StableDefinitionNamespace,
                            kind: StableDefinitionKind,
                            name: &Spur,
                            owner: Option<Arc<str>>,
                            is_public: bool| {
                assert_eq!(file_id, declaration_span.file_id);
                bindings.push(SemanticBinding {
                    file_id,
                    declaration_span,
                    namespace,
                    kind,
                    name: Arc::from(self.interner.resolve(name)),
                    owner,
                    is_public,
                })
            };
            match &inst.data {
                InstData::FnDecl { name, is_pub, .. }
                    if !self.declaration_index.is_type_scoped_method(inst_ref) =>
                {
                    assert!(
                        self.functions_by_file_name
                            .contains_key(&(inst.span.file_id, *name)),
                        "manifest free function must be a bound winner"
                    );
                    emit(
                        inst.span.file_id,
                        inst.span,
                        StableDefinitionNamespace::Value,
                        StableDefinitionKind::Function,
                        name,
                        None,
                        *is_pub,
                    );
                    work.functions_emitted += 1;
                }
                InstData::StructDecl {
                    name,
                    methods,
                    is_pub,
                    ..
                } => {
                    let struct_id = *self
                        .structs_by_file_name
                        .get(&(inst.span.file_id, *name))
                        .expect("manifest struct must be a bound winner");
                    let owner: Arc<str> = Arc::from(self.interner.resolve(name));
                    emit(
                        inst.span.file_id,
                        inst.span,
                        StableDefinitionNamespace::Type,
                        StableDefinitionKind::Struct,
                        name,
                        None,
                        *is_pub,
                    );
                    work.types_emitted += 1;
                    for method_ref in self.rir.struct_methods(methods) {
                        work.named_method_edges_visited += 1;
                        let method_inst = self.rir.get(method_ref);
                        let InstData::FnDecl {
                            name,
                            has_self,
                            is_pub,
                            ..
                        } = &method_inst.data
                        else {
                            unreachable!("named struct method edge must target FnDecl");
                        };
                        assert_eq!(
                            self.named_method_declarations.get(&(struct_id, *name)),
                            Some(&method_ref),
                            "manifest named method must be the bound winner"
                        );
                        emit(
                            method_inst.span.file_id,
                            method_inst.span,
                            StableDefinitionNamespace::Method,
                            if *has_self {
                                StableDefinitionKind::Method
                            } else {
                                StableDefinitionKind::AssociatedFunction
                            },
                            name,
                            Some(owner.clone()),
                            *is_pub,
                        );
                        work.named_methods_emitted += 1;
                    }
                }
                InstData::EnumDecl { name, is_pub, .. } => {
                    assert!(
                        self.enums_by_file_name
                            .contains_key(&(inst.span.file_id, *name)),
                        "manifest enum must be a bound winner"
                    );
                    emit(
                        inst.span.file_id,
                        inst.span,
                        StableDefinitionNamespace::Type,
                        StableDefinitionKind::Enum,
                        name,
                        None,
                        *is_pub,
                    );
                    work.types_emitted += 1;
                }
                InstData::ConstDecl { name, is_pub, .. } => {
                    let key = (inst.span.file_id, *name);
                    let kind = if self.module_binding(&key).is_some() {
                        work.module_bindings_emitted += 1;
                        StableDefinitionKind::ModuleBinding
                    } else if self.value_const(&key).is_some() {
                        work.constants_emitted += 1;
                        StableDefinitionKind::ValueConst
                    } else {
                        panic!("manifest const must be a classified bound winner")
                    };
                    emit(
                        inst.span.file_id,
                        inst.span,
                        StableDefinitionNamespace::Value,
                        kind,
                        name,
                        None,
                        *is_pub,
                    );
                }
                InstData::DropFnDecl { type_name, .. } => {
                    let struct_id = *self
                        .structs_by_file_name
                        .get(&(inst.span.file_id, *type_name))
                        .expect("manifest destructor target must be a bound named struct");
                    assert_eq!(
                        self.destructor_spans.get(&struct_id),
                        Some(&inst.span),
                        "manifest destructor must be the bound winner"
                    );
                    emit(
                        inst.span.file_id,
                        inst.span,
                        StableDefinitionNamespace::Destructor,
                        StableDefinitionKind::Destructor,
                        type_name,
                        Some(Arc::from(self.interner.resolve(type_name))),
                        false,
                    );
                    work.destructors_emitted += 1;
                }
                _ => {}
            }
        }
        work.bindings_emitted = bindings.len();
        SemanticBindingManifest {
            bindings: bindings.into(),
            work,
        }
    }
}

impl<'a> Sema<'a, super::MutableDeclarations> {
    pub(super) fn into_bound_with_work(
        mut self,
        binding_work: DeclarationBindingWork,
    ) -> BoundSema<'a> {
        // This updates source nominal definitions and therefore belongs on
        // the declaration side of the phase boundary. Body analysis receives
        // an immutable namespace with final destructor symbols.
        self.requalify_colliding_destructor_symbols();
        let sema = self.freeze_declarations();
        BoundSema {
            binding_work,
            sema,
            manifest: OnceLock::new(),
        }
    }
}

impl DeclarationBindingWork {
    pub(super) fn from_inputs(
        input_rir_instructions: usize,
        index: RirDeclarationIndexWork,
        modules_registered: usize,
    ) -> Self {
        Self {
            bind_invocations: 1,
            namespace_setup_invocations: 0,
            nominal_type_predeclaration_invocations: 0,
            callable_value_predeclaration_invocations: 0,
            callable_value_shells_predeclared: 0,
            indexed_declaration_records_visited: 0,
            declaration_resolution_invocations: 0,
            declaration_resolution_failures: 0,
            body_readiness_finalization_invocations: 0,
            durable_install_invocations: 0,
            durable_payloads_installed: 0,
            input_rir_instructions,
            modules_registered,
            declaration_index_build_invocations: index.build_invocations,
            indexed_free_functions: index.free_functions_indexed,
            indexed_named_methods: index.named_methods_indexed,
            indexed_anonymous_methods: index.anonymous_methods_indexed,
            indexed_destructors: index.destructors_indexed,
            indexed_const_candidates: index.const_candidates_indexed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lasso::ThreadedRodeo;
    use rue_error::{CompileErrors, CompileResult, PreviewFeatures};
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::{AstGen, Rir};
    use rue_span::FileId;

    use super::*;

    fn bind(source: &str) -> Result<SemanticBindingManifest, CompileErrors> {
        let (tokens, interner) = Lexer::new(source)
            .tokenize()
            .map_err(CompileErrors::from_error)?;
        let (ast, interner) = Parser::new(tokens, interner).parse()?;
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let bound =
            Sema::new_synthetic(&rir, &interner, PreviewFeatures::new()).bind_declarations()?;
        Ok(bound.binding_manifest().clone())
    }

    fn install_stable_endpoints(
        definitions: &[crate::SemanticDefinitionEndpoint],
        modules: &[crate::SemanticModuleEndpoint],
    ) -> Result<(), crate::DeclarationInstallFailure> {
        let (tokens, interner) = Lexer::new("fn main() {}").tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap()
            .install_stable_identity_endpoints(definitions, modules)
            .map(|_| ())
    }

    #[test]
    fn stable_endpoint_install_reports_every_typed_resolution_failure() {
        use crate::SemanticStableResolutionFailure as F;
        let endpoint = crate::SemanticDefinitionEndpoint {
            token: crate::SemanticDefinitionToken::new(7, 0),
            file: 0,
            name: Arc::from("main"),
            kind: StableDefinitionKind::Function,
            owner: None,
        };
        let module = crate::SemanticModuleEndpoint {
            token: crate::SemanticModuleToken::new(7, 0),
            file: 0,
        };
        assert_eq!(install_stable_endpoints(&[endpoint.clone()], &[]), Ok(()));
        assert_eq!(
            install_stable_endpoints(&[], &[]),
            Err(crate::DeclarationInstallFailure::from(F::Missing))
        );

        let mut duplicate = endpoint.clone();
        duplicate.token = crate::SemanticDefinitionToken::new(7, 1);
        assert_eq!(
            install_stable_endpoints(&[endpoint.clone(), duplicate], &[]),
            Err(crate::DeclarationInstallFailure::from(F::Ambiguous))
        );

        let mut wrong_kind = endpoint.clone();
        // `ValueConst` has the same owner shape as `Function`, so this checks
        // exact manifest membership rather than the coarse shape gate.
        wrong_kind.kind = StableDefinitionKind::ValueConst;
        assert_eq!(
            install_stable_endpoints(&[wrong_kind], &[]),
            Err(crate::DeclarationInstallFailure::from(F::WrongKind))
        );

        let mut nonexistent = endpoint.clone();
        nonexistent.name = Arc::from("other");
        assert_eq!(
            install_stable_endpoints(&[nonexistent], &[]),
            Err(crate::DeclarationInstallFailure::from(F::Missing))
        );
        assert_eq!(
            install_stable_endpoints(&[], &[]),
            Err(crate::DeclarationInstallFailure::from(F::Missing))
        );
        let extra_module = crate::SemanticModuleEndpoint {
            token: crate::SemanticModuleToken::new(7, 1),
            file: 99,
        };
        assert_eq!(
            install_stable_endpoints(&[endpoint.clone()], &[module, extra_module]),
            Err(crate::DeclarationInstallFailure::from(F::Missing))
        );

        let foreign_module = crate::SemanticModuleEndpoint {
            token: crate::SemanticModuleToken::new(8, 0),
            file: 0,
        };
        assert_eq!(
            install_stable_endpoints(&[endpoint], &[foreign_module]),
            Err(crate::DeclarationInstallFailure::from(F::ForeignIssuer))
        );
    }

    #[test]
    fn declaration_shell_endpoint_install_checks_the_unresolved_shell_universe() {
        use crate::SemanticStableResolutionFailure as F;
        let (tokens, interner) = Lexer::new("struct S {} fn main() {}").tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let shells = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()
            .unwrap();
        let only_main = crate::SemanticDefinitionEndpoint {
            token: crate::SemanticDefinitionToken::new(11, 0),
            file: 0,
            name: Arc::from("main"),
            kind: StableDefinitionKind::Function,
            owner: None,
        };
        assert!(matches!(
            shells.install_stable_identity_endpoints(&[only_main], &[]),
            Err(F::Missing)
        ));
    }

    fn lower_files(files: &[(&str, FileId)]) -> (Rir, ThreadedRodeo) {
        let mut interner = ThreadedRodeo::default();
        let mut items = Vec::new();
        for &(source, file_id) in files {
            let (tokens, next) = Lexer::with_interner_and_file_id(source, interner, file_id)
                .tokenize()
                .unwrap();
            let (ast, next) = Parser::new(tokens, next).parse().unwrap();
            items.extend(ast.items);
            interner = next;
        }
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&items);
        let rir = astgen.finish();
        (rir, interner)
    }

    fn export(
        source: &str,
    ) -> Result<
        (
            Vec<SemanticDeclarationExport>,
            SemanticDeclarationExportWork,
        ),
        SemanticExportFailure,
    > {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let bound = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        bound.with_declaration_semantics(|records, work| (records.to_vec(), work))
    }

    #[test]
    fn declaration_export_is_resolved_owned_lazy_and_rir_free() {
        let (records, work) = export(
            r#"
            struct Leaf { value: i32 }
            struct Boxed { nested: [[Leaf; 2]; 4] }
            enum Maybe { None, Some(Leaf) }
            const LIMIT: i32 = 7;
            fn id(comptime T: type, value: T) -> T { value }
            fn nested(comptime T: type, value: [[T; 2]; 2]) -> [[T; 2]; 2] { value }
            fn pointed(comptime T: type, value: ptr const T) -> ptr const T { value }
            fn ordered(comptime T: type, comptime U: type, left: [T; 2], right: ptr const U) -> ptr const U { right }
            fn helper(value: ptr const Leaf) -> bool { true }
            const alias = helper;
            drop fn Boxed(self) {}
            fn main() {}
            "#,
        )
        .unwrap();
        assert_eq!(work.build_invocations, 1);
        assert_eq!(work.rir_instructions_visited, 0);
        assert_eq!(work.declarations_exported, records.len());
        assert!(records.iter().any(|record| matches!(
            &record.payload,
            SemanticDeclarationPayload::Callable { parameters, result, .. }
                if record.identity.name.as_ref() == "id"
                    && parameters.iter().map(|p| p.is_comptime).collect::<Vec<_>>() == [true, false]
                    && parameters[1].ty == SemanticExportType::GenericParameter(0)
                    && *result == SemanticExportType::GenericParameter(0)
        )));
        let nested = SemanticExportType::Array {
            element: Box::new(SemanticExportType::Array {
                element: Box::new(SemanticExportType::GenericParameter(0)),
                len: 2,
            }),
            len: 2,
        };
        assert!(records.iter().any(|record| matches!(
            &record.payload,
            SemanticDeclarationPayload::Callable { parameters, result, .. }
                if record.identity.name.as_ref() == "nested"
                    && parameters[1].ty == nested
                    && *result == nested
        )));
        assert!(records.iter().any(|record| matches!(
            &record.payload,
            SemanticDeclarationPayload::Callable { parameters, result, .. }
                if record.identity.name.as_ref() == "pointed"
                    && parameters[1].ty
                        == SemanticExportType::PtrConst(Box::new(
                            SemanticExportType::GenericParameter(0)
                        ))
                    && *result == parameters[1].ty
        )));
        assert!(records.iter().any(|record| matches!(
            &record.payload,
            SemanticDeclarationPayload::Callable { parameters, result, .. }
                if record.identity.name.as_ref() == "ordered"
                    && parameters[2].ty
                        == SemanticExportType::Array {
                            element: Box::new(SemanticExportType::GenericParameter(0)),
                            len: 2,
                        }
                    && parameters[3].ty
                        == SemanticExportType::PtrConst(Box::new(
                            SemanticExportType::GenericParameter(1)
                        ))
                    && *result == parameters[3].ty
        )));
        assert!(records.iter().any(|record| matches!(
            &record.payload,
            SemanticDeclarationPayload::Const { value: SemanticExportConstValue::Function { name, .. }, .. }
                if record.identity.name.as_ref() == "alias" && name.as_ref() == "helper"
        )));
        assert!(records.iter().any(|record| {
            record.identity.name.as_ref() == "Boxed"
                && matches!(record.payload, SemanticDeclarationPayload::Destructor)
        }));
        // The callback DTO contains no request-local Type/Spur/InstRef/nominal IDs.
        assert!(std::mem::size_of::<SemanticExportType>() > 0);
    }

    #[test]
    fn value_dependent_composite_signature_export_fails_closed() {
        assert!(matches!(
            export("fn sized(comptime N: i32, values: [i32; N]) -> i32 { values[0] } fn main() {}"),
            Err(SemanticExportFailure::UnsupportedGenericSignature)
        ));
    }

    #[test]
    fn durable_payload_install_matches_ordinary_binding_in_a_fresh_epoch() {
        let source = r#"
            struct Resource {
                value: i32,
                fn get(self) -> i32 { self.value }
                fn make(value: i32) -> Resource { Resource { value } }
            }
            enum Choice { None, Some(Resource) }
            drop fn Resource(self) {}
            fn helper(value: ptr const Resource) -> i32 { value.value }
            fn id(comptime T: type, value: T) -> T { value }
            fn nested(comptime T: type, value: [[T; 2]; 2]) -> [[T; 2]; 2] { value }
            fn pointed(comptime T: type, value: ptr const T) -> ptr const T { value }
            fn main() -> i32 { 0 }
        "#;
        let exports = export(source).unwrap().0;

        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let installed = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()
            .unwrap()
            .install_declaration_semantics(&exports)
            .unwrap();
        let work = installed.binding_work();
        assert_eq!(work.declaration_resolution_invocations, 0);
        assert_eq!(work.durable_install_invocations, 1);
        assert_eq!(work.durable_payloads_installed, exports.len());
        let installed_exports = installed
            .with_declaration_semantics(|records, work| {
                assert_eq!(work.rir_instructions_visited, 0);
                records.to_vec()
            })
            .unwrap();
        assert_eq!(installed_exports, exports);
        installed.analyze_all_bodies_for_test().unwrap();

        // A shape mismatch consumes the candidate shells and fails closed;
        // ordinary resolution in another fresh epoch remains available.
        let mut mismatched = exports.clone();
        mismatched[0].identity.is_public = !mismatched[0].identity.is_public;
        let failure = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()
            .unwrap()
            .install_declaration_semantics(&mismatched);
        let Err(failure) = failure else {
            panic!("mismatched durable payload unexpectedly installed")
        };
        assert_eq!(failure, DeclarationInstallFailure::VisibilityMismatch);
        Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap()
            .analyze_all_bodies_for_test()
            .unwrap();
    }

    fn bind_with_module_paths(source: &str) -> Result<SemanticBindingManifest, CompileErrors> {
        struct FixtureView {
            import_offset: u32,
        }

        impl crate::CanonicalImportView for FixtureView {
            fn visit_modules(
                &self,
                visitor: &mut dyn FnMut(&str, FileId, &str) -> CompileResult<()>,
            ) -> CompileResult<()> {
                visitor("main.rue", FileId::DEFAULT, "/main.rue")?;
                visitor("other.rue", FileId::new(1), "/other.rue")
            }

            fn visit_resolved_sites(
                &self,
                visitor: &mut dyn FnMut(&str, u32, &str, &str) -> CompileResult<()>,
            ) -> CompileResult<()> {
                visitor("main.rue", self.import_offset, "other.rue", "other.rue")
            }
        }

        let (tokens, interner) = Lexer::new(source)
            .tokenize()
            .map_err(CompileErrors::from_error)?;
        let (ast, interner) = Parser::new(tokens, interner).parse()?;
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let import_offset = rir
            .iter()
            .find_map(|(_, inst)| {
                matches!(inst.data, InstData::Intrinsic { .. }).then_some(inst.span.start)
            })
            .unwrap_or_default();
        let mut sema = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new());
        sema.set_root_file_id(FileId::DEFAULT);
        sema.set_file_paths(HashMap::from([
            (FileId::DEFAULT, "/main.rue".to_owned()),
            (FileId::new(1), "/other.rue".to_owned()),
        ]));
        sema.set_canonical_imports(&FixtureView { import_offset })?;
        let bound = sema.bind_declarations()?;
        Ok(bound.binding_manifest().clone())
    }

    #[test]
    fn manifest_is_owned_deterministic_and_complete_after_binding() {
        let source = r#"
            struct Resource {
                value: i32,
                fn get(self) -> i32 { self.value }
                fn make() -> Resource { Resource { value: 0 } }
            }
            enum Choice { None, Some(i32) }
            const LIMIT: i32 = 4;
            drop fn Resource(self) {}
            fn helper() -> i32 { LIMIT }
            fn main() -> i32 { helper() }
        "#;
        let first = bind(source).unwrap();
        let second = bind(source).unwrap();
        assert_eq!(first.bindings(), second.bindings());
        assert_eq!(first.work(), second.work());
        assert_eq!(first.work().build_invocations, 1);
        assert_eq!(first.work().functions_emitted, 2);
        assert_eq!(first.work().types_emitted, 2);
        assert_eq!(first.work().constants_emitted, 1);
        assert_eq!(first.work().destructors_emitted, 1);
        assert_eq!(first.work().named_methods_emitted, 2);
        assert_eq!(first.work().named_method_edges_visited, 2);
        assert_eq!(
            first.work().named_method_edges_visited,
            first.work().named_methods_emitted
        );
        assert_eq!(first.work().anonymous_methods_deferred, 0);
        assert_eq!(first.work().bindings_emitted, 8);
        assert_eq!(first.work().parser_invocations, 0);
        assert_eq!(first.work().ast_payload_clones, 0);
        assert_eq!(first.work().source_text_clones, 0);
        assert!(first.bindings().iter().any(|binding| {
            binding.name.as_ref() == "make"
                && binding.owner.as_deref() == Some("Resource")
                && binding.kind == StableDefinitionKind::AssociatedFunction
        }));
        assert!(
            first
                .bindings()
                .iter()
                .all(|binding| binding.file_id == binding.declaration_span.file_id)
        );
    }

    #[test]
    fn rejected_duplicate_method_never_produces_a_manifest() {
        let error = bind("struct Bad { fn duplicate(self) {} fn duplicate(self) {} } fn main() {}")
            .unwrap_err();
        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn synthetic_public_method_visibility_is_preserved() {
        let (tokens, interner) =
            Lexer::new("struct PublicApi { fn exposed(self) {} } fn main() {}")
                .tokenize()
                .unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let mut rir = astgen.finish_editor();
        let method = rir
            .iter()
            .find_map(|(reference, inst)| match inst.data {
                InstData::FnDecl { has_self: true, .. } => Some(reference),
                _ => None,
            })
            .unwrap();
        rir.set_function_public(method, true).unwrap();
        let bound = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(bound.binding_manifest().bindings().iter().any(|binding| {
            binding.name.as_ref() == "exposed"
                && binding.kind == StableDefinitionKind::Method
                && binding.is_public
        }));
    }

    #[test]
    fn rejected_const_collision_and_duplicate_destructor_emit_no_manifest() {
        assert!(bind("const same: i32 = 1; fn same() {} fn main() {}").is_err());
        assert!(
            bind(
                "struct Resource {} drop fn Resource(self) {} drop fn Resource(self) {} fn main() {}"
            )
            .is_err()
        );
    }

    #[test]
    fn constants_are_classified_only_after_successful_evaluation() {
        let manifest = bind_with_module_paths(
            "const value: i32 = 1; const imported = @import(\"other.rue\"); fn main() {}",
        )
        .unwrap();
        assert!(manifest.bindings().iter().any(|binding| {
            binding.name.as_ref() == "value" && binding.kind == StableDefinitionKind::ValueConst
        }));
        assert!(manifest.bindings().iter().any(|binding| {
            binding.name.as_ref() == "imported"
                && binding.kind == StableDefinitionKind::ModuleBinding
        }));
        assert_eq!(manifest.work().constants_emitted, 1);
        assert_eq!(manifest.work().module_bindings_emitted, 1);
    }

    #[test]
    fn analyze_all_matches_explicit_bind_then_analyze() {
        let source = "fn helper(x: i32) -> i32 { x + 1 } fn main() -> i32 { helper(41) }";
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let direct = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .analyze_all()
            .unwrap();
        let bound = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(!bound.manifest_is_materialized());
        let explicit = bound.analyze_all_bodies_for_test().unwrap();
        let summarize = |output: &SemaOutput| {
            (
                output
                    .functions
                    .iter()
                    .map(|function| {
                        (
                            function.name.clone(),
                            function.air.display_with_interner(&interner).to_string(),
                        )
                    })
                    .collect::<Vec<_>>(),
                output.strings.clone(),
                output
                    .warnings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                output.type_pool.stats(),
                output.body_analysis_work,
            )
        };
        assert_eq!(summarize(&direct), summarize(&explicit));
    }

    #[test]
    fn manifest_scan_is_lazy_and_materialized_only_on_request() {
        let (tokens, interner) = Lexer::new("fn main() {}").tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let bound = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(!bound.manifest_is_materialized());
        assert_eq!(bound.binding_work().bind_invocations, 1);
        assert_eq!(bound.binding_work().namespace_setup_invocations, 1);
        assert_eq!(
            bound.binding_work().nominal_type_predeclaration_invocations,
            1
        );
        assert_eq!(bound.binding_work().declaration_resolution_invocations, 1);
        assert_eq!(
            bound.binding_work().body_readiness_finalization_invocations,
            1
        );
        assert_eq!(bound.binding_work().input_rir_instructions, rir.len());
        assert_eq!(bound.binding_work().declaration_index_build_invocations, 1);
        assert_eq!(bound.binding_manifest().work().build_invocations, 1);
        assert!(bound.manifest_is_materialized());
    }

    #[test]
    fn declaration_shell_boundary_accounts_each_phase_once() {
        let (tokens, interner) = Lexer::new("struct Item { value: i32 } fn main() {}")
            .tokenize()
            .unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let shells = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()
            .unwrap();
        let prepared = shells.binding_work();
        assert_eq!(prepared.bind_invocations, 1);
        assert_eq!(prepared.namespace_setup_invocations, 1);
        assert_eq!(prepared.nominal_type_predeclaration_invocations, 1);
        assert_eq!(prepared.callable_value_predeclaration_invocations, 1);
        assert_eq!(prepared.callable_value_shells_predeclared, 1);
        assert_eq!(prepared.indexed_declaration_records_visited, 2);
        assert_eq!(
            prepared.indexed_declaration_records_visited,
            shells.declaration_shells().count()
        );
        assert_eq!(prepared.declaration_resolution_invocations, 0);
        assert_eq!(prepared.body_readiness_finalization_invocations, 0);
        assert_eq!(prepared.input_rir_instructions, rir.len());
        assert_eq!(prepared.declaration_index_build_invocations, 1);
        assert_eq!(
            shells
                .sema
                .rir_declaration_index_work()
                .rir_instructions_visited,
            rir.len()
        );

        let bound = shells.resolve_declarations().unwrap();
        let resolved = bound.binding_work();
        assert_eq!(resolved.declaration_resolution_invocations, 1);
        assert_eq!(resolved.body_readiness_finalization_invocations, 1);
        assert!(!bound.manifest_is_materialized());
    }

    #[test]
    fn declaration_shell_identities_ignore_file_ids_relocation_and_input_order() {
        fn identities(
            files: &[(&str, FileId)],
            paths: HashMap<FileId, String>,
        ) -> Vec<SemanticDeclarationShellIdentity> {
            let (rir, interner) = lower_files(files);
            let mut sema = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new());
            sema.set_symbol_paths(paths);
            sema.predeclare_declaration_shells_for_test()
                .unwrap()
                .declaration_shells()
                .map(|shell| shell.identity.clone())
                .collect()
        }

        let left = identities(
            &[
                (
                    "struct Zebra {} enum Amber { One } fn alpha(x: i32) -> i32 { x }",
                    FileId::new(4),
                ),
                (
                    "struct Birch {} enum Violet { One } const alias = alpha;",
                    FileId::new(9),
                ),
            ],
            HashMap::from([
                (FileId::new(4), "/checkout-a/pkg/a.rue".into()),
                (FileId::new(9), "/checkout-a/pkg/b.rue".into()),
            ]),
        );
        let right = identities(
            &[
                (
                    "struct Birch {} enum Violet { One } const alias = alpha;",
                    FileId::new(31),
                ),
                (
                    "struct Zebra {} enum Amber { One } fn alpha(x: i32) -> i32 { x }",
                    FileId::new(2),
                ),
            ],
            HashMap::from([
                (FileId::new(2), "/checkout-a/pkg/a.rue".into()),
                (FileId::new(31), "/checkout-a/pkg/b.rue".into()),
            ]),
        );
        assert_eq!(left, right);
        assert_eq!(left.len(), 6);
        assert!(
            left[..4]
                .iter()
                .all(|identity| identity.namespace == StableDefinitionNamespace::Type)
        );
        assert!(
            left[4..]
                .iter()
                .all(|identity| identity.namespace != StableDefinitionNamespace::Type)
        );
    }

    #[test]
    fn callable_shell_keeps_syntax_metadata_outside_unresolved_payload() {
        let source = "struct Box { fn map(self, comptime T: type, value: T) -> T { value } fn make(value: i32) -> i32 { value } } const alias = Box.make; drop fn Box(self) {}";
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let shells = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()
            .unwrap();
        let records = shells.callable_value_shells().collect::<Vec<_>>();
        assert_eq!(records.len(), 4);
        let map = records
            .iter()
            .find(|shell| shell.identity.name.as_ref() == "map")
            .unwrap();
        assert_eq!(map.identity.owner.as_deref(), Some("Box"));
        assert_eq!(
            map.parameter_names
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>(),
            vec!["T", "value"]
        );
        assert_eq!(map.parameter_comptime.as_ref(), &[true, false]);
        assert!(map.has_self);
        assert!(map.is_generic);
        assert_eq!(shells.binding_work().declaration_resolution_invocations, 0);
    }

    #[test]
    fn payload_boundary_preserves_resolution_failure_provenance() {
        let source = "fn broken(value: Missing) -> i32 { 0 } fn main() {}";
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let direct = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .err()
            .expect("ordinary binding must fail");
        let split = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()
            .unwrap()
            .resolve_declarations()
            .err()
            .expect("split binding must fail");
        assert_eq!(format!("{direct:?}"), format!("{split:?}"));
    }

    #[test]
    fn user_strbuf_declaration_is_ordinary_and_split_binding_agrees() {
        let (tokens, interner) = Lexer::new("struct StrBuf {} fn main() {}")
            .tokenize()
            .unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let direct = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .expect("StrBuf is not a reserved root type");
        let split = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()
            .expect("StrBuf nominal predeclaration succeeds")
            .resolve_declarations()
            .expect("StrBuf declaration resolution succeeds");
        assert_eq!(
            format!("{:?}", direct.binding_manifest()),
            format!("{:?}", split.binding_manifest())
        );
    }

    #[test]
    fn anonymous_methods_are_explicitly_deferred() {
        let manifest = bind(
            "fn Factory(comptime T: type) -> type { struct { value: T, fn get(self) -> T { self.value } } } fn main() {}",
        )
        .unwrap();
        assert_eq!(manifest.work().anonymous_methods_deferred, 1);
        assert!(
            !manifest
                .bindings()
                .iter()
                .any(|binding| binding.name.as_ref() == "get")
        );
    }
}
