//! Owned semantic bindings emitted only after declaration binding succeeds.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use lasso::Spur;
use rue_error::{CompileErrors, MultiErrorResult};
use rue_rir::{InstData, InstRef, RirParamMode};
use rue_span::{FileId, Span};

use super::RirDeclarationIndexWork;
use super::{ConstValue, Sema, SemaOutput};
use crate::types::{
    ArrayLen, PtrMutability, Type, TypeKind, parse_array_type_syntax, parse_pointer_type_syntax,
};

/// A nominal declaration identity valid for one successful binding request.
/// Consumers must join this identity to their own stable definition universe
/// while the callback passed to [`BoundSema::with_declaration_semantics`] runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticNominalIdentity {
    pub file_id: FileId,
    pub name: Arc<str>,
    pub kind: SemanticBindingKind,
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
    Nominal(SemanticNominalIdentity),
    Array {
        element: Box<Self>,
        len: u64,
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
    pub ty: SemanticExportType,
    pub mode: SemanticParameterMode,
    pub is_comptime: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticExportConstValue {
    Integer(i128),
    Bool(bool),
    Type(SemanticExportType),
    Function { file_id: FileId, name: Arc<str> },
    Unit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticDeclarationPayload {
    Callable {
        parameters: Arc<[SemanticExportParameter]>,
        result: SemanticExportType,
        has_self: bool,
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
    pub namespace: SemanticBindingNamespace,
    pub kind: SemanticBindingKind,
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
    pub is_generic: bool,
    pub is_public: bool,
    pub is_unchecked: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationInstallFailure {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBindingNamespace {
    Value,
    Type,
    Destructor,
    Method,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBindingKind {
    Function,
    Struct,
    Enum,
    ValueConst,
    ModuleBinding,
    Destructor,
    Method,
    AssociatedFunction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBinding {
    /// Request-local source file containing this declaration.
    pub file_id: FileId,
    /// Request-local declaration location; excluded from stable identity.
    pub declaration_span: Span,
    pub namespace: SemanticBindingNamespace,
    pub kind: SemanticBindingKind,
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

pub struct BoundSema<'a> {
    sema: super::BodySema<'a>,
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
pub struct DeclarationShells<'a> {
    pub(super) sema: Sema<'a>,
    pub(super) binding_work: DeclarationBindingWork,
    pub(super) pending_payloads: Vec<PendingDeclarationPayload>,
    pub(super) pending_nominals: Vec<PendingNominalPayload>,
}

impl<'a> DeclarationShells<'a> {
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

    /// Resolve declaration payloads and finalize a body-analysis-ready binder.
    pub fn resolve_declarations(self) -> MultiErrorResult<BoundSema<'a>> {
        self.resolve_declarations_with_work()
            .map_err(DeclarationResolutionFailure::into_errors)
    }

    /// Resolve declaration payloads while retaining exact work on failure.
    pub fn resolve_declarations_with_work(
        mut self,
    ) -> Result<BoundSema<'a>, DeclarationResolutionFailure> {
        // This is the explicit payload-install boundary. The ordinary adapter
        // deliberately resolves from the authoritative current-revision RIR in
        // historical order. A future importer may validate durable payloads
        // against these shells before choosing an installation path.
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
        self.binding_work.declaration_resolution_invocations += 1;
        if let Err(error) = self.sema.resolve_declarations() {
            self.binding_work.declaration_resolution_failures += 1;
            return Err(DeclarationResolutionFailure::new(
                CompileErrors::from(error),
                self.binding_work,
            ));
        }
        self.binding_work.body_readiness_finalization_invocations += 1;
        Ok(self.sema.into_bound_with_work(self.binding_work))
    }

    /// Install a fully validated, current-revision projection of durable
    /// declaration semantics into these freshly predeclared shells.
    ///
    /// Failure consumes the shells, so partially populated request-local state
    /// can never escape. Callers fall back by creating a fresh binder and
    /// taking [`Self::resolve_declarations`].
    pub fn install_declaration_semantics(
        mut self,
        exports: &[SemanticDeclarationExport],
    ) -> Result<BoundSema<'a>, DeclarationInstallFailure> {
        use std::collections::BTreeMap;

        self.binding_work.durable_install_invocations += 1;
        let mut records = BTreeMap::new();
        for export in exports {
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
                kind: export.identity.kind,
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
                        fields_start,
                        fields_len,
                        ..
                    },
                ) => {
                    let id = *self
                        .sema
                        .structs_by_file_name
                        .get(&(pending.shell.declaration_span.file_id, name))
                        .ok_or(DeclarationInstallFailure::MissingNominal)?;
                    let mut def = self.sema.type_pool.struct_def(id);
                    if *syntax_linear != *is_linear || def.is_copy != *is_copy {
                        return Err(DeclarationInstallFailure::NominalShapeMismatch);
                    }
                    if self
                        .sema
                        .rir
                        .get_field_decls(*fields_start, *fields_len)
                        .iter()
                        .map(|(name, _)| self.sema.interner.resolve(name))
                        .ne(fields.iter().map(|field| field.0.as_ref()))
                    {
                        return Err(DeclarationInstallFailure::NominalShapeMismatch);
                    }
                    def.fields = fields
                        .iter()
                        .map(|(name, ty)| {
                            Ok(crate::StructField {
                                name: name.to_string(),
                                ty: self.sema.import_export_type(ty)?,
                            })
                        })
                        .collect::<Result<_, DeclarationInstallFailure>>()?;
                    self.sema.type_pool.update_struct_def(id, def);
                }
                (SemanticDeclarationPayload::Enum { variants }, InstData::EnumDecl { .. }) => {
                    let id = *self
                        .sema
                        .enums_by_file_name
                        .get(&(pending.shell.declaration_span.file_id, name))
                        .ok_or(DeclarationInstallFailure::MissingNominal)?;
                    let mut def = self.sema.type_pool.enum_def(id);
                    if def
                        .variants
                        .iter()
                        .map(String::as_str)
                        .ne(variants.iter().map(|variant| variant.0.as_ref()))
                    {
                        return Err(DeclarationInstallFailure::NominalShapeMismatch);
                    }
                    def.variant_payloads = variants
                        .iter()
                        .map(|(_, payload)| {
                            payload
                                .iter()
                                .map(|ty| self.sema.import_export_type(ty))
                                .collect()
                        })
                        .collect::<Result<_, DeclarationInstallFailure>>()?;
                    self.sema.type_pool.update_enum_def(id, def);
                }
                _ => return Err(DeclarationInstallFailure::KindMismatch),
            }
        }

        for pending in &self.pending_payloads {
            let record = records[&pending.shell.identity];
            match (&record.payload, pending.source) {
                (
                    SemanticDeclarationPayload::Callable {
                        parameters,
                        result,
                        has_self,
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
                        params_start,
                        params_len,
                        self_mode,
                        directives_start,
                        directives_len,
                        ..
                    } = &self.sema.rir.get(pending.declaration).data
                    else {
                        return Err(DeclarationInstallFailure::KindMismatch);
                    };
                    let type_name = self.sema.interner.get_or_intern("type");
                    let rir_parameters = self.sema.rir.get_params(*params_start, *params_len);
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
                                params: range,
                                return_type: return_type_value,
                                body,
                                span: pending.shell.declaration_span,
                            },
                        );
                        self.sema
                            .named_method_declarations
                            .insert((id, *name), pending.declaration);
                    } else {
                        let internal = self
                            .sema
                            .internal_function_name(*name, pending.shell.declaration_span.file_id);
                        let directives = self
                            .sema
                            .rir
                            .get_directives(*directives_start, *directives_len);
                        let allow_unused_function = self
                            .sema
                            .has_allow_directive(&directives, "unused_function");
                        let allow_unused_variable = self
                            .sema
                            .has_allow_directive(&directives, "unused_variable");
                        let allow_unreachable_code = self
                            .sema
                            .has_allow_directive(&directives, "unreachable_code");
                        self.sema
                            .functions_by_file_name
                            .insert((pending.shell.declaration_span.file_id, *name), internal);
                        self.sema.function_source_names.insert(internal, *name);
                        self.sema.functions.insert(
                            internal,
                            super::FunctionInfo {
                                params: range,
                                return_type: return_type_value,
                                return_type_sym: *return_type,
                                body,
                                rir_params_start: *params_start,
                                rir_params_len: *params_len,
                                span: pending.shell.declaration_span,
                                is_generic: pending.shell.is_generic,
                                is_pub: pending.shell.is_public,
                                is_unchecked: pending.shell.is_unchecked,
                                allow_unused_function,
                                allow_unused_variable,
                                allow_unreachable_code,
                                file_id: pending.shell.declaration_span.file_id,
                            },
                        );
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
                (_, DeclarationPayloadSource::Const { .. }) => {
                    return Err(DeclarationInstallFailure::UnsupportedDeclaration);
                }
                _ => return Err(DeclarationInstallFailure::KindMismatch),
            }
        }
        self.sema
            .check_recursive_value_types()
            .map_err(|_| DeclarationInstallFailure::NominalShapeMismatch)?;
        self.sema.propagate_field_linearity();
        self.sema.capture_resolved_declaration_type_dependencies();
        self.sema.declaration_type_observer = None;
        self.binding_work.durable_payloads_installed = expected;
        self.binding_work.body_readiness_finalization_invocations += 1;
        Ok(self.sema.into_bound_with_work(self.binding_work))
    }
}

impl Sema<'_> {
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
            SemanticExportType::Nominal(nominal) => {
                let name = self.interner.get_or_intern(nominal.name.as_ref());
                match nominal.kind {
                    SemanticBindingKind::Struct => Type::new_struct(
                        *self
                            .structs_by_file_name
                            .get(&(nominal.file_id, name))
                            .ok_or(DeclarationInstallFailure::MissingNominal)?,
                    ),
                    SemanticBindingKind::Enum => Type::new_enum(
                        *self
                            .enums_by_file_name
                            .get(&(nominal.file_id, name))
                            .ok_or(DeclarationInstallFailure::MissingNominal)?,
                    ),
                    _ => return Err(DeclarationInstallFailure::KindMismatch),
                }
            }
            SemanticExportType::Array { element, len } => {
                Type::new_array(self.type_pool.intern_array_from_type(
                    self.import_export_type_with_generics(element, generic_parameters)?,
                    *len,
                ))
            }
            SemanticExportType::PtrConst(value) => {
                Type::new_ptr_const(self.type_pool.intern_ptr_const_from_type(
                    self.import_export_type_with_generics(value, generic_parameters)?,
                ))
            }
            SemanticExportType::PtrMut(value) => {
                Type::new_ptr_mut(self.type_pool.intern_ptr_mut_from_type(
                    self.import_export_type_with_generics(value, generic_parameters)?,
                ))
            }
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
            | SemanticExportType::PtrConst(element)
            | SemanticExportType::PtrMut(element) => {
                Self::validate_generic_references(element, generic_parameter_count)?
            }
            _ => false,
        })
    }
}

impl<'a> BoundSema<'a> {
    /// Resolve durable keys into request-independent declaration identities and
    /// stage candidates for the canonical reachable-body worklist. No AIR type,
    /// instruction, string, nominal, or function ID is allocated here.
    pub fn install_ordinary_body_candidates<K, M>(
        mut self,
        candidates: Vec<crate::SemanticBodyCandidate<K, M>>,
        definition: impl Fn(&K) -> Option<crate::SemanticBodyDefinitionIdentity>,
        module: impl Fn(&M) -> Option<FileId>,
    ) -> Self
    where
        K: Clone + Ord,
        M: Clone + Ord + AsRef<str>,
    {
        for candidate in candidates {
            let mapped = candidate
                .body
                .try_map_keys(&|key| definition(key).ok_or(()), &|key| {
                    module(key)
                        .map(|file| crate::SemanticBodyModuleIdentity {
                            file_id: file.index(),
                            path: std::sync::Arc::from(key.as_ref()),
                        })
                        .ok_or(())
                });
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
        super::analysis::analyze_all_function_bodies_with_namespace_probe(self.sema)
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
                    SemanticBindingKind::Function => super::BodyOwnerKind::FreeFunction,
                    SemanticBindingKind::Method => super::BodyOwnerKind::Method,
                    SemanticBindingKind::AssociatedFunction => {
                        super::BodyOwnerKind::AssociatedFunction
                    }
                    SemanticBindingKind::Destructor => super::BodyOwnerKind::Destructor,
                    _ => return None,
                };
                let name = if binding.kind == SemanticBindingKind::Destructor {
                    binding
                        .owner
                        .as_deref()
                        .unwrap_or(&binding.name)
                        .to_string()
                } else {
                    binding.name.to_string()
                };
                let owner = if binding.kind == SemanticBindingKind::Destructor {
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
        self.sema.body_owner_tokens = installed;
        Ok(self)
    }
    pub fn binding_work(&self) -> DeclarationBindingWork {
        self.binding_work
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

    pub fn analyze_all_bodies(self) -> MultiErrorResult<SemaOutput> {
        self.sema.analyze_all_bodies()
    }

    /// Analyze bodies while retaining value-only work counters if diagnostics
    /// prevent AIR publication.
    pub fn analyze_all_bodies_with_work(self) -> Result<SemaOutput, super::BodyAnalysisFailure> {
        self.sema.analyze_all_bodies_with_work()
    }
}

impl<'a, D: super::DeclarationPhase> Sema<'a, D> {
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
                    SemanticBindingKind::Struct
                } else {
                    SemanticBindingKind::Enum
                };
                nominals.push(PendingNominalPayload {
                    shell: SemanticDeclarationShell {
                        identity: SemanticDeclarationShellIdentity {
                            module_path: module_path(inst.span.file_id),
                            is_trusted_standard_library: self
                                .trusted_standard_library_files
                                .contains(&inst.span.file_id),
                            namespace: SemanticBindingNamespace::Type,
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
                        is_generic: false,
                        is_public: *is_pub,
                        is_unchecked: false,
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
                is_generic,
                is_public,
                is_unchecked,
                source,
            ) = match &inst.data {
                InstData::FnDecl {
                    is_pub,
                    is_unchecked,
                    name,
                    params_start,
                    params_len,
                    body,
                    has_self,
                    ..
                } if !self.declaration_index.is_anonymous_method(inst_ref) => {
                    let owner = candidate.named_method_owner;
                    let kind = match (owner, *has_self) {
                        (Some(_), true) => SemanticBindingKind::Method,
                        (Some(_), false) => SemanticBindingKind::AssociatedFunction,
                        (None, _) => SemanticBindingKind::Function,
                    };
                    let params = self.rir.get_params(*params_start, *params_len);
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
                            SemanticBindingNamespace::Method
                        } else {
                            SemanticBindingNamespace::Value
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
                        comptime.into_iter().any(|value| value),
                        *is_pub,
                        *is_unchecked,
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
                        namespace: SemanticBindingNamespace::Value,
                        // Function aliases are classified only after evaluating
                        // the initializer; their stable value identity is already
                        // complete here.
                        kind: SemanticBindingKind::ValueConst,
                        name: Arc::from(self.interner.resolve(name)),
                        owner: None,
                    },
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    false,
                    false,
                    *is_pub,
                    false,
                    DeclarationPayloadSource::Const { initializer: *init },
                ),
                InstData::DropFnDecl { type_name, body } => (
                    SemanticDeclarationShellIdentity {
                        module_path: module_path(inst.span.file_id),
                        is_trusted_standard_library: self
                            .trusted_standard_library_files
                            .contains(&inst.span.file_id),
                        namespace: SemanticBindingNamespace::Destructor,
                        kind: SemanticBindingKind::Destructor,
                        name: Arc::from(self.interner.resolve(type_name)),
                        owner: Some(Arc::from(self.interner.resolve(type_name))),
                    },
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    true,
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
                    is_generic,
                    is_public,
                    is_unchecked,
                },
                declaration: inst_ref,
                source,
            });
        }
        pending.sort_by(|left, right| left.shell.identity.cmp(&right.shell.identity));
        nominals.sort_by(|left, right| left.shell.identity.cmp(&right.shell.identity));
        (pending, nominals)
    }

    fn export_type(
        &self,
        ty: Type,
        stack: &mut Vec<Type>,
    ) -> Result<SemanticExportType, SemanticExportFailure> {
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
                if def.is_builtin {
                    return Err(SemanticExportFailure::UnmappedNominalType);
                }
                let symbol = self.interner.get_or_intern(&def.name);
                if self.structs_by_file_name.get(&(def.file_id, symbol)) != Some(&id) {
                    return Err(SemanticExportFailure::AnonymousNominalType);
                }
                Ok(SemanticExportType::Nominal(SemanticNominalIdentity {
                    file_id: def.file_id,
                    name: Arc::from(def.name),
                    kind: SemanticBindingKind::Struct,
                }))
            }
            TypeKind::Enum(id) => {
                let def = self.type_pool.enum_def(id);
                Ok(SemanticExportType::Nominal(SemanticNominalIdentity {
                    file_id: def.file_id,
                    name: Arc::from(def.name),
                    kind: SemanticBindingKind::Enum,
                }))
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
            info.rir_params_start,
            info.rir_params_len,
            info.return_type_sym,
        )
    }

    fn export_callable_signature(
        &self,
        range: crate::ParamRange,
        return_type: Type,
        params_start: u32,
        params_len: u32,
        return_type_sym: Spur,
    ) -> Result<(Arc<[SemanticExportParameter]>, SemanticExportType), SemanticExportFailure> {
        let rir_params = self.rir.get_params(params_start, params_len);
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
            .zip(&rir_params)
            .map(|(((&ty, &mode), &is_comptime), rir)| {
                use rue_rir::RirParamMode;
                let mode = match mode {
                    RirParamMode::Normal => SemanticParameterMode::Value,
                    RirParamMode::Borrow => SemanticParameterMode::Borrow,
                    RirParamMode::Inout => SemanticParameterMode::Inout,
                };
                Ok(SemanticExportParameter {
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
        for identity in manifest
            .bindings
            .iter()
            .filter(|b| b.kind != SemanticBindingKind::ModuleBinding)
        {
            let name = self.interner.get_or_intern(identity.name.as_ref());
            let payload = match identity.kind {
                SemanticBindingKind::Function => {
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
                        is_unchecked: info.is_unchecked,
                    }
                }
                SemanticBindingKind::Method | SemanticBindingKind::AssociatedFunction => {
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
                        params_start,
                        params_len,
                        return_type,
                        ..
                    } = &self.rir.get(declaration).data
                    else {
                        return Err(SemanticExportFailure::UnmappedFunction);
                    };
                    let (parameters, result) = self.export_callable_signature(
                        info.params,
                        info.return_type,
                        *params_start,
                        *params_len,
                        *return_type,
                    )?;
                    SemanticDeclarationPayload::Callable {
                        parameters,
                        result,
                        has_self: info.has_self,
                        is_unchecked: false,
                    }
                }
                SemanticBindingKind::Struct => {
                    let sid = *self
                        .structs_by_file_name
                        .get(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedNominalType)?;
                    let def = self.type_pool.struct_def(sid);
                    let fields = def
                        .fields
                        .into_iter()
                        .map(|f| Ok((Arc::from(f.name), self.export_type(f.ty, &mut Vec::new())?)))
                        .collect::<Result<Vec<_>, _>>()?;
                    SemanticDeclarationPayload::Struct {
                        fields: fields.into(),
                        is_copy: def.is_copy,
                        is_linear: def.is_linear,
                    }
                }
                SemanticBindingKind::Enum => {
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
                SemanticBindingKind::ValueConst => {
                    let info = self
                        .constants_by_file_name
                        .get(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let value = match info.value {
                        ConstValue::Integer(v) => SemanticExportConstValue::Integer(v),
                        ConstValue::Bool(v) => SemanticExportConstValue::Bool(v),
                        ConstValue::Type(t) => {
                            SemanticExportConstValue::Type(self.export_type(t, &mut Vec::new())?)
                        }
                        ConstValue::Unit => SemanticExportConstValue::Unit,
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
                SemanticBindingKind::Destructor => SemanticDeclarationPayload::Destructor,
                SemanticBindingKind::ModuleBinding => unreachable!(),
            };
            records.push(SemanticDeclarationExport {
                identity: identity.clone(),
                payload,
            });
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
                            namespace: SemanticBindingNamespace,
                            kind: SemanticBindingKind,
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
                        SemanticBindingNamespace::Value,
                        SemanticBindingKind::Function,
                        name,
                        None,
                        *is_pub,
                    );
                    work.functions_emitted += 1;
                }
                InstData::StructDecl {
                    name,
                    methods_start,
                    methods_len,
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
                        SemanticBindingNamespace::Type,
                        SemanticBindingKind::Struct,
                        name,
                        None,
                        *is_pub,
                    );
                    work.types_emitted += 1;
                    for method_ref in self.rir.get_inst_refs(*methods_start, *methods_len) {
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
                            SemanticBindingNamespace::Method,
                            if *has_self {
                                SemanticBindingKind::Method
                            } else {
                                SemanticBindingKind::AssociatedFunction
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
                        SemanticBindingNamespace::Type,
                        SemanticBindingKind::Enum,
                        name,
                        None,
                        *is_pub,
                    );
                    work.types_emitted += 1;
                }
                InstData::ConstDecl { name, is_pub, .. } => {
                    let key = (inst.span.file_id, *name);
                    let kind = if self.module_bindings.contains_key(&key) {
                        work.module_bindings_emitted += 1;
                        SemanticBindingKind::ModuleBinding
                    } else if self.constants_by_file_name.contains_key(&key) {
                        work.constants_emitted += 1;
                        SemanticBindingKind::ValueConst
                    } else {
                        panic!("manifest const must be a classified bound winner")
                    };
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Value,
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
                        SemanticBindingNamespace::Destructor,
                        SemanticBindingKind::Destructor,
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

impl<'a> Sema<'a> {
    pub(super) fn into_bound_with_work(
        mut self,
        binding_work: DeclarationBindingWork,
    ) -> BoundSema<'a> {
        // This updates source nominal definitions and therefore belongs on
        // the declaration side of the phase boundary. Body analysis receives
        // an immutable namespace with final destructor symbols.
        self.requalify_colliding_destructor_symbols();
        BoundSema {
            binding_work,
            sema: self.freeze_declarations(),
            manifest: OnceLock::new(),
        }
    }
}

impl DeclarationBindingWork {
    pub(super) fn from_inputs(
        input_rir_instructions: usize,
        index: RirDeclarationIndexWork,
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
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new()).bind_declarations()?;
        Ok(bound.binding_manifest().clone())
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
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
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
        let installed = Sema::new(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells()
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
        installed.analyze_all_bodies().unwrap();

        // A shape mismatch consumes the candidate shells and fails closed;
        // ordinary resolution in another fresh epoch remains available.
        let mut mismatched = exports.clone();
        mismatched[0].identity.is_public = !mismatched[0].identity.is_public;
        let failure = Sema::new(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells()
            .unwrap()
            .install_declaration_semantics(&mismatched);
        let Err(failure) = failure else {
            panic!("mismatched durable payload unexpectedly installed")
        };
        assert_eq!(failure, DeclarationInstallFailure::VisibilityMismatch);
        Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap()
            .analyze_all_bodies()
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
        let mut sema = Sema::new(&rir, &interner, PreviewFeatures::new());
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
                && binding.kind == SemanticBindingKind::AssociatedFunction
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
        let mut rir = astgen.finish();
        let method = rir
            .iter()
            .find_map(|(reference, inst)| match inst.data {
                InstData::FnDecl { has_self: true, .. } => Some(reference),
                _ => None,
            })
            .unwrap();
        let InstData::FnDecl { is_pub, .. } = &mut rir.get_mut(method).data else {
            unreachable!()
        };
        *is_pub = true;
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(bound.binding_manifest().bindings().iter().any(|binding| {
            binding.name.as_ref() == "exposed"
                && binding.kind == SemanticBindingKind::Method
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
            binding.name.as_ref() == "value" && binding.kind == SemanticBindingKind::ValueConst
        }));
        assert!(manifest.bindings().iter().any(|binding| {
            binding.name.as_ref() == "imported"
                && binding.kind == SemanticBindingKind::ModuleBinding
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
        let direct = Sema::new(&rir, &interner, PreviewFeatures::new())
            .analyze_all()
            .unwrap();
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(!bound.manifest_is_materialized());
        let explicit = bound.analyze_all_bodies().unwrap();
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
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
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

        let shells = Sema::new(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells()
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
            let mut sema = Sema::new(&rir, &interner, PreviewFeatures::new());
            sema.set_symbol_paths(paths);
            sema.predeclare_declaration_shells()
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
                .all(|identity| identity.namespace == SemanticBindingNamespace::Type)
        );
        assert!(
            left[4..]
                .iter()
                .all(|identity| identity.namespace != SemanticBindingNamespace::Type)
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
        let shells = Sema::new(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells()
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
        let direct = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .err()
            .expect("ordinary binding must fail");
        let split = Sema::new(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells()
            .unwrap()
            .resolve_declarations()
            .err()
            .expect("split binding must fail");
        assert_eq!(format!("{direct:?}"), format!("{split:?}"));
    }

    #[test]
    fn declaration_shell_adapter_preserves_semantic_predeclaration_failure_provenance() {
        let (tokens, interner) = Lexer::new("struct StrBuf {} fn main() {}")
            .tokenize()
            .unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let direct = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .err()
            .expect("reserved type must fail");
        let split = Sema::new(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells()
            .err()
            .expect("reserved type must short-circuit during nominal predeclaration");
        assert_eq!(format!("{direct:?}"), format!("{split:?}"));
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
