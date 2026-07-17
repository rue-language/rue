//! Semantic analysis - RIR to AIR conversion.
//!
//! Sema performs type checking and converts untyped RIR to typed AIR.
//! This is analogous to Zig's Sema phase.
//!
//! # Module Organization
//!
//! This module is split into several submodules for maintainability:
//!
//! - [`context`] - Analysis context and helper types (LocalVar, AnalysisContext, etc.)
//! - [`declarations`] - Declaration gathering (register_type_names, resolve_declarations)
//! - [`builtins`] - Built-in type injection (String, etc.)
//! - [`typeck`] - Type resolution and checking helpers
//! - [`analysis`] - Function analysis, type inference coordination, and RIR-to-AIR lowering
//! - [`info`] - Function, method, and constant info types
//! - [`gather`] - Declaration gathering output
//! - [`output`] - Semantic analysis output types
//! - [`inference_ctx`] - Pre-computed type information for inference
//! - [`visibility`] - Module visibility checking
//! - [`anon_structs`] - Anonymous struct structural equality
//! - [`file_paths`] - File path management for multi-file compilation
//!
//! The main entry points are:
//! - [`Sema::new`] - Create a new semantic analyzer
//! - [`Sema::analyze_all`] - Perform full semantic analysis
//! - [`Sema::analyze_all_bodies`] - Analyze function bodies after declarations

pub(crate) mod analysis;
mod analyze_ops;
mod anon_structs;
mod binding_manifest;
mod builtins;
mod comptime_eval;
mod context;
mod declaration_index;
mod declarations;
mod file_paths;
mod inference_ctx;
mod info;
mod known_symbols;
mod metadata;
mod output;
mod semantic_body_export;
mod typeck;
mod visibility;

// Public re-exports
pub use binding_manifest::{
    BoundSema, DeclarationBindingWork, DeclarationInstallFailure, DeclarationResolutionFailure,
    DeclarationShells, SemanticBinding, SemanticBindingManifest, SemanticBindingManifestWork,
    SemanticDeclarationExport, SemanticDeclarationExportWork, SemanticDeclarationPayload,
    SemanticDeclarationShell, SemanticDeclarationShellIdentity, SemanticExportConstValue,
    SemanticExportFailure, SemanticExportParameter, SemanticExportType, SemanticNominalIdentity,
    SemanticParameterMode,
};
pub use context::ConstValue;
pub use declaration_index::RirDeclarationIndexWork;
pub use inference_ctx::InferenceContext;
pub use info::{AnonMethodSig, ConstInfo, FunctionInfo, MethodInfo};
pub use known_symbols::KnownSymbols;
pub use metadata::SemaMetadata;
pub use output::{
    AnalyzedBodyOwnerEvent, AnalyzedFunction, BodyAnalysisFailure, BodyAnalysisWork,
    BodyNamedDependencyEvent, BodyOwnerEndpoint, BodyOwnerKind, BodyOwnerToken,
    BuiltinTypeCallHead, DeclarationBuiltinTypeCallHeadDependencyEvent,
    DeclarationTypeCallHeadDependencyEvent, DeclarationTypeDependencyEvent,
    DeclarationTypeDependencyKind, DeclarationTypeDependencySourceKind,
    DeclarationTypeDependencyTargetKind, ImplicitDropDependencySourceEvent,
    ImplicitNamedDestructorDependencyEvent, NamedConstDependencyEvent,
    NamedConstDependencyTargetEvent, NamedDestructorDependencyEvent, NamedMethodDependencyEvent,
    NamedMethodDependencyTargetEvent, OrdinaryFreeFunctionDependencyEvent, ParamSlotModes,
    SemaOutput, SpecializedFreeFunctionDependencyEvent, SpecializedFreeFunctionOrigin,
};

use std::collections::{HashMap, HashSet};

use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileErrors, MultiErrorResult, PreviewFeatures};
use rue_rir::Rir;
use rue_span::{FileId, Span};
use rue_target::Target;

use crate::intern_pool::TypeInternPool;
use crate::param_arena::ParamArena;
use crate::types::{EnumId, StructId, Type};

/// The source declaration namespace while declaration binding is in progress.
///
/// This wrapper is the only namespace state that implements `DerefMut`; after
/// binding, it is consumed into [`SourceDeclarations`], whose maps are
/// structurally read-only.
#[doc(hidden)]
pub struct MutableDeclarations(DeclarationNamespace);

/// The closed source declaration namespace consumed by body analysis.
#[doc(hidden)]
pub struct SourceDeclarations(DeclarationNamespace);

#[doc(hidden)]
pub struct DeclarationNamespace {
    functions: HashMap<Spur, FunctionInfo>,
    functions_by_file_name: HashMap<(FileId, Spur), Spur>,
    function_source_names: HashMap<Spur, Spur>,
    builtin_structs: HashMap<Spur, StructId>,
    structs_by_file_name: HashMap<(FileId, Spur), StructId>,
    builtin_enums: HashMap<Spur, EnumId>,
    enums_by_file_name: HashMap<(FileId, Spur), EnumId>,
    methods: HashMap<(StructId, Spur), MethodInfo>,
    named_method_declarations: HashMap<(StructId, Spur), rue_rir::InstRef>,
    constants_by_file_name: HashMap<(FileId, Spur), ConstInfo>,
    module_bindings: HashMap<(FileId, Spur), ConstInfo>,
}

impl DeclarationNamespace {
    fn new() -> Self {
        Self {
            functions: HashMap::new(),
            functions_by_file_name: HashMap::new(),
            function_source_names: HashMap::new(),
            builtin_structs: HashMap::new(),
            structs_by_file_name: HashMap::new(),
            builtin_enums: HashMap::new(),
            enums_by_file_name: HashMap::new(),
            methods: HashMap::new(),
            named_method_declarations: HashMap::new(),
            constants_by_file_name: HashMap::new(),
            module_bindings: HashMap::new(),
        }
    }
}

impl std::ops::Deref for MutableDeclarations {
    type Target = DeclarationNamespace;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for MutableDeclarations {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl std::ops::Deref for SourceDeclarations {
    type Target = DeclarationNamespace;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[doc(hidden)]
pub trait DeclarationPhase: std::ops::Deref<Target = DeclarationNamespace> + Sized {
    fn resolve_indexed_const(
        sema: &mut Sema<'_, Self>,
        name: Spur,
        file_id: FileId,
    ) -> Option<ConstValue>;

    fn collect_free_function_signature(
        sema: &mut Sema<'_, Self>,
        target: Spur,
        file_id: Option<FileId>,
    ) -> rue_error::CompileResult<Option<(FileId, bool)>>;
}

impl DeclarationPhase for MutableDeclarations {
    fn resolve_indexed_const(
        sema: &mut Sema<'_, Self>,
        name: Spur,
        file_id: FileId,
    ) -> Option<ConstValue> {
        sema.resolve_indexed_const_binding_impl(name, file_id)
    }

    fn collect_free_function_signature(
        sema: &mut Sema<'_, Self>,
        target: Spur,
        file_id: Option<FileId>,
    ) -> rue_error::CompileResult<Option<(FileId, bool)>> {
        sema.collect_free_function_signature_binding_impl(target, file_id)
    }
}

impl DeclarationPhase for SourceDeclarations {
    fn resolve_indexed_const(
        _sema: &mut Sema<'_, Self>,
        _name: Spur,
        _file_id: FileId,
    ) -> Option<ConstValue> {
        None
    }

    fn collect_free_function_signature(
        _sema: &mut Sema<'_, Self>,
        _target: Spur,
        _file_id: Option<FileId>,
    ) -> rue_error::CompileResult<Option<(FileId, bool)>> {
        Ok(None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredOwnershipGateKind {
    RequireDroppable,
    RequireTriviallyDroppable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeferredOwnershipGate {
    kind: DeferredOwnershipGateKind,
    ty: Type,
    span: Span,
}

/// Semantic analyzer that converts RIR to AIR.
pub struct Sema<'a, D: DeclarationPhase = MutableDeclarations> {
    declarations: D,
    pub(crate) rir: &'a Rir,
    pub(crate) interner: &'a ThreadedRodeo,
    /// Request-local declaration candidates for this exact RIR arena.
    declaration_index: declaration_index::RirDeclarationIndex,
    /// Function table: maps internal function name symbols to their info.
    ///
    /// The internal key is normally the source name, but functions with the
    /// same source name in distinct files get deterministic module-qualified
    /// keys so they can coexist without colliding in AIR/codegen.
    /// Source declarations live in `declarations`; source-name lookup there is
    /// keyed by defining file/module. Internal function symbols remain the
    /// stable keys used by AIR and codegen.
    pub(crate) anonymous_methods: HashMap<(StructId, Spur), MethodInfo>,
    /// Body-created synthetic and anonymous type-name overlays.
    pub(crate) generated_structs: HashMap<Spur, StructId>,
    pub(crate) generated_enums: HashMap<Spur, EnumId>,
    pub(crate) body_analysis_work: BodyAnalysisWork,
    pub(crate) analyzed_body_owners: Vec<AnalyzedBodyOwnerEvent>,
    pub(crate) ordinary_body_exports: Vec<crate::SemanticBodyExport>,
    pub(crate) specialized_body_exports: Vec<crate::SemanticSpecializedBodyExport>,
    pub(crate) reusable_ordinary_bodies: HashMap<
        BodyOwnerToken,
        crate::SemanticBodyCandidate<
            crate::SemanticBodyDefinitionIdentity,
            crate::SemanticBodyModuleIdentity,
        >,
    >,
    pub(crate) reusable_specialized_bodies: Vec<
        crate::SemanticSpecializedBodyCandidate<
            crate::SemanticBodyDefinitionIdentity,
            std::sync::Arc<str>,
            crate::SemanticBodyModuleIdentity,
        >,
    >,
    pub(crate) body_dependency_observer: Option<AnalyzedBodyOwnerEvent>,
    pub(crate) body_owner_tokens:
        HashMap<(u32, String, Option<String>, BodyOwnerKind), BodyOwnerToken>,
    pub(crate) body_named_dependencies: Vec<BodyNamedDependencyEvent>,
    pub(crate) ordinary_free_function_dependencies: Vec<OrdinaryFreeFunctionDependencyEvent>,
    pub(crate) specialized_free_function_origins: Vec<SpecializedFreeFunctionOrigin>,
    pub(crate) specialized_free_function_dependencies: Vec<SpecializedFreeFunctionDependencyEvent>,
    pub(crate) named_method_dependencies: Vec<NamedMethodDependencyEvent>,
    pub(crate) non_generic_named_method_dependencies_complete: bool,
    pub(crate) named_destructor_dependencies: Vec<NamedDestructorDependencyEvent>,
    pub(crate) declaration_type_dependencies: Vec<DeclarationTypeDependencyEvent>,
    pub(crate) declaration_type_call_head_dependencies: Vec<DeclarationTypeCallHeadDependencyEvent>,
    pub(crate) declaration_builtin_type_call_head_dependencies:
        Vec<DeclarationBuiltinTypeCallHeadDependencyEvent>,
    pub(crate) named_const_dependencies: Vec<NamedConstDependencyEvent>,
    pub(crate) named_const_dependency_source: Option<(FileId, String)>,
    pub(crate) declaration_type_observer: Option<(
        FileId,
        String,
        Option<String>,
        DeclarationTypeDependencySourceKind,
        DeclarationTypeDependencyKind,
    )>,
    /// Active dependency stack while declaration binding resolves constants.
    /// Empty outside the declaration-resolution phase; retained on `Sema` so
    /// recursive type resolution shares one cycle detector without rebuilding
    /// a declaration worklist.
    pub(crate) const_resolution_in_progress: Vec<(FileId, Spur)>,
    /// True only while the current-revision declaration payloads are being
    /// resolved. Once `BoundSema` exists, missing constants are authoritative
    /// lookup misses and may never restart declaration discovery.
    pub(crate) declaration_binding_active: bool,
    /// Enabled preview features
    pub(crate) preview_features: PreviewFeatures,
    /// Requested compilation target.
    pub(crate) target: Target,
    /// EnumId of the synthetic Arch enum (for @target_arch intrinsic).
    pub(crate) builtin_arch_id: Option<EnumId>,
    /// EnumId of the synthetic Os enum (for @target_os intrinsic).
    pub(crate) builtin_os_id: Option<EnumId>,
    /// Pre-interned known symbols for fast comparison.
    pub(crate) known: KnownSymbols,
    /// Canonical composite-type storage for this semantic epoch (ADR-0024).
    pub(crate) type_pool: TypeInternPool,
    /// Canonically prepopulated module registry for the current semantic epoch.
    pub(crate) module_registry: crate::module_registry::ModuleRegistry,
    /// Accepted canonical import-site identities for this semantic epoch.
    pub(crate) canonical_imports: Option<crate::canonical_imports::CanonicalImportContext>,
    /// Maps FileId to request-local source paths used for presentation.
    pub(crate) file_paths: HashMap<FileId, String>,
    /// Maps FileId to relocation-stable paths used in generated symbols.
    pub(crate) symbol_paths: HashMap<FileId, String>,
    /// FileIds whose standard-library provenance was established by the
    /// frontend's import resolver, rather than inferred from path spelling.
    pub(crate) trusted_standard_library_files: HashSet<FileId>,
    /// Explicit semantic root module for root-relative import fallback.
    pub(crate) root_file_id: Option<FileId>,
    /// Arena storage for function/method parameter data.
    pub(crate) param_arena: ParamArena,
    /// Method signatures for anonymous structs, used for structural equality comparison.
    pub(crate) anon_struct_method_sigs: HashMap<StructId, Vec<AnonMethodSig>>,
    /// Captured comptime values for anonymous structs.
    /// When an anonymous struct with methods is created inside a comptime function,
    /// the comptime parameter values (e.g., N=42 in FixedBuffer(comptime N: i32)) are
    /// stored here, keyed by StructId. These values become part of type identity:
    /// FixedBuffer(42) and FixedBuffer(100) are different types.
    pub(crate) anon_struct_captured_values: HashMap<StructId, HashMap<Spur, ConstValue>>,
    /// Captured comptime *type* substitution for anonymous structs produced by
    /// a `-> type` comptime constructor. When `Vec(comptime T: type)` is
    /// instantiated as `Vec(i32)`, this stores `T -> i32` keyed by the
    /// resulting StructId, so the enclosing type parameter resolves not just in
    /// field/method *signatures* (done at registration) but throughout every
    /// method *body* at analysis time (RUE-313). Empty for non-generic anon
    /// structs.
    pub(crate) anon_struct_type_subst: HashMap<StructId, HashMap<Spur, Type>>,
    /// Span of each user-defined `drop fn` declaration, keyed by the struct
    /// it destructs. Used by diagnostics that point at the destructor
    /// (E0456 field-move-out-of-destructor-type, E0457 @copy-with-destructor).
    /// Builtin destructors (e.g. String's) have no entry.
    pub(crate) destructor_spans: HashMap<StructId, Span>,
    /// Structs that became linear via infectious linearity (RUE-40): not
    /// declared `linear`, but containing a field that carries a linear value.
    /// Maps the struct to the (field name, field type name) that caused it,
    /// for diagnostics explaining why the container is linear.
    pub(crate) infectious_linear: HashMap<StructId, (String, String)>,
    /// Ownership gates reached while declaration payloads are still mutable.
    /// Their negative result is not authoritative until nominal payloads,
    /// infectious linearity, and destructors have all been finalized.
    pub(crate) deferred_ownership_gates: Vec<DeferredOwnershipGate>,
    /// Current recursion depth of `-> type` comptime-function reduction
    /// (`eval_comptime_type_call`). Unlike value functions — whose comptime
    /// recursion is bounded by the specialization-round limit — a `-> type`
    /// function reduces eagerly on the host stack, so an unbounded
    /// self-recursive type constructor (`fn Bad() -> type { Bad() }`) would
    /// overflow that stack (SIGABRT) without this guard. Bounded at
    /// `MAX_SPECIALIZATION_ROUNDS`, emitting the same E1200 (RUE-261).
    pub(crate) comptime_type_call_depth: usize,
    /// Internal names of free functions whose signatures are mid-collection in
    /// [`Self::collect_free_function_signature_during_binding`]. Collecting a
    /// signature resolves its parameter/return types, which may name other type
    /// constructors and collect *them* on demand (RUE-603); a signature cycle
    /// (`fn A(x: B(i32))` / `fn B(x: A(i32))`) must not re-enter a
    /// mid-collection signature, or the reduction would recurse forever.
    pub(crate) fn_signatures_in_flight: HashSet<Spur>,
    /// Human-readable instantiation spellings for type-constructor-produced
    /// anonymous types (`ArrayBuf(i64)` for the anon struct otherwise named
    /// `__anon_struct_4`), recorded when [`Self::reduce_type_ctor_body`]
    /// reduces a constructor application. Consulted by
    /// [`Self::format_type_name`] so diagnostics print the spelling the user
    /// wrote (RUE-610). First writer wins: structurally-deduped types keep
    /// the spelling of their first instantiation.
    pub(crate) ctor_type_displays: HashMap<Type, String>,
}

impl<D: DeclarationPhase> std::ops::Deref for Sema<'_, D> {
    type Target = DeclarationNamespace;

    fn deref(&self) -> &Self::Target {
        &self.declarations
    }
}

impl<'a, D: DeclarationPhase> Sema<'a, D> {
    /// Change only the declaration-phase capability while preserving every
    /// other analyzer field by value. Keeping this conversion exhaustive makes
    /// additions to `Sema` fail to compile here until their phase-boundary
    /// semantics are reviewed.
    fn map_declarations<E: DeclarationPhase>(self, map: impl FnOnce(D) -> E) -> Sema<'a, E> {
        let Sema {
            declarations,
            rir,
            interner,
            declaration_index,
            anonymous_methods,
            generated_structs,
            generated_enums,
            body_analysis_work,
            analyzed_body_owners,
            ordinary_body_exports,
            specialized_body_exports,
            reusable_ordinary_bodies,
            reusable_specialized_bodies,
            body_dependency_observer,
            body_owner_tokens,
            body_named_dependencies,
            ordinary_free_function_dependencies,
            specialized_free_function_origins,
            specialized_free_function_dependencies,
            named_method_dependencies,
            non_generic_named_method_dependencies_complete,
            named_destructor_dependencies,
            declaration_type_dependencies,
            declaration_type_call_head_dependencies,
            declaration_builtin_type_call_head_dependencies,
            named_const_dependencies,
            named_const_dependency_source,
            declaration_type_observer,
            const_resolution_in_progress,
            declaration_binding_active,
            preview_features,
            target,
            builtin_arch_id,
            builtin_os_id,
            known,
            type_pool,
            module_registry,
            canonical_imports,
            file_paths,
            symbol_paths,
            trusted_standard_library_files,
            root_file_id,
            param_arena,
            anon_struct_method_sigs,
            anon_struct_captured_values,
            anon_struct_type_subst,
            destructor_spans,
            infectious_linear,
            deferred_ownership_gates,
            comptime_type_call_depth,
            fn_signatures_in_flight,
            ctor_type_displays,
        } = self;
        Sema {
            declarations: map(declarations),
            rir,
            interner,
            declaration_index,
            anonymous_methods,
            generated_structs,
            generated_enums,
            body_analysis_work,
            analyzed_body_owners,
            ordinary_body_exports,
            specialized_body_exports,
            reusable_ordinary_bodies,
            reusable_specialized_bodies,
            body_dependency_observer,
            body_owner_tokens,
            body_named_dependencies,
            ordinary_free_function_dependencies,
            specialized_free_function_origins,
            specialized_free_function_dependencies,
            named_method_dependencies,
            non_generic_named_method_dependencies_complete,
            named_destructor_dependencies,
            declaration_type_dependencies,
            declaration_type_call_head_dependencies,
            declaration_builtin_type_call_head_dependencies,
            named_const_dependencies,
            named_const_dependency_source,
            declaration_type_observer,
            const_resolution_in_progress,
            declaration_binding_active,
            preview_features,
            target,
            builtin_arch_id,
            builtin_os_id,
            known,
            type_pool,
            module_registry,
            canonical_imports,
            file_paths,
            symbol_paths,
            trusted_standard_library_files,
            root_file_id,
            param_arena,
            anon_struct_method_sigs,
            anon_struct_captured_values,
            anon_struct_type_subst,
            destructor_spans,
            infectious_linear,
            deferred_ownership_gates,
            comptime_type_call_depth,
            fn_signatures_in_flight,
            ctor_type_displays,
        }
    }

    pub(crate) fn function_info(&self, name: Spur) -> Option<&FunctionInfo> {
        self.functions.get(&name)
    }

    pub(crate) fn method_info(&self, key: (StructId, Spur)) -> Option<&MethodInfo> {
        self.anonymous_methods
            .get(&key)
            .or_else(|| self.methods.get(&key))
    }

    pub(crate) fn has_method(&self, key: (StructId, Spur)) -> bool {
        self.method_info(key).is_some()
    }

    pub(crate) fn struct_id_for_name(&self, name: Spur) -> Option<StructId> {
        self.generated_structs
            .get(&name)
            .or_else(|| self.builtin_structs.get(&name))
            .copied()
    }

    pub(crate) fn enum_id_for_name(&self, name: Spur) -> Option<EnumId> {
        self.generated_enums
            .get(&name)
            .or_else(|| self.builtin_enums.get(&name))
            .copied()
    }

    pub(crate) fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        span: Span,
    ) -> rue_error::CompileResult<()> {
        // The repository standard library is trusted compiler input and may
        // use the packed-byte substrate that implements its safe StrBuf
        // surface. Authorization comes from canonical import provenance, not
        // a path spelling, and deliberately applies only to RawBytes: every
        // other preview feature remains subject to the consumer's flags.
        let trusted_std_raw_bytes = feature == rue_error::PreviewFeature::RawBytes
            && self.trusted_standard_library_files.contains(&span.file_id);
        if self.preview_features.contains(&feature) || trusted_std_raw_bytes {
            Ok(())
        } else {
            Err(rue_error::CompileError::new(
                rue_error::ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_string(),
                },
                span,
            )
            .with_help(format!(
                "use `--preview {}` to enable this feature ({})",
                feature.name(),
                feature.adr()
            )))
        }
    }

    pub(crate) fn try_resolve_indexed_const_during_binding(
        &mut self,
        name: Spur,
        file_id: FileId,
    ) -> Option<ConstValue> {
        D::resolve_indexed_const(self, name, file_id)
    }

    pub(crate) fn collect_free_function_signature_during_binding(
        &mut self,
        target: Spur,
        file_id: Option<FileId>,
    ) -> rue_error::CompileResult<Option<(FileId, bool)>> {
        D::collect_free_function_signature(self, target, file_id)
    }

    pub(crate) fn record_named_const_dependency(
        &mut self,
        target: NamedConstDependencyTargetEvent,
    ) {
        let Some((file, name)) = self.named_const_dependency_source.clone() else {
            return;
        };
        self.named_const_dependencies
            .push(NamedConstDependencyEvent {
                source_file: file.index(),
                source_name: name,
                target,
            });
        self.body_analysis_work.named_const_dependency_events += 1;
    }

    pub(crate) fn record_body_named_dependency(&mut self, target: NamedConstDependencyTargetEvent) {
        let Some(source) = self.body_dependency_observer.clone() else {
            return;
        };
        self.body_named_dependencies
            .push(BodyNamedDependencyEvent { source, target });
        self.body_analysis_work.named_const_dependency_events += 1;
    }

    pub(crate) fn validate_explicit_call_modes<A>(
        &self,
        args: A,
        expected_modes: impl ExactSizeIterator<Item = rue_rir::RirParamMode>,
    ) -> rue_error::CompileResult<()>
    where
        A: IntoIterator,
        A::IntoIter: ExactSizeIterator,
        A::Item: std::ops::Deref<Target = rue_rir::RirCallArg>,
    {
        let args = args.into_iter();
        assert_eq!(args.len(), expected_modes.len());
        for (arg, expected_mode) in args.zip(expected_modes) {
            let arg = &*arg;
            use rue_rir::{RirArgMode, RirParamMode};
            match (expected_mode, arg.mode) {
                (RirParamMode::Inout, RirArgMode::Inout)
                | (RirParamMode::Borrow, RirArgMode::Borrow)
                | (RirParamMode::Normal, RirArgMode::Normal) => {}
                (RirParamMode::Inout, _) => {
                    return Err(rue_error::CompileError::new(
                        rue_error::ErrorKind::InoutKeywordMissing,
                        self.rir.get(arg.value).span,
                    ));
                }
                (RirParamMode::Borrow, _) => {
                    return Err(rue_error::CompileError::new(
                        rue_error::ErrorKind::BorrowKeywordMissing,
                        self.rir.get(arg.value).span,
                    ));
                }
                (RirParamMode::Normal, actual) => {
                    let mode = match actual {
                        RirArgMode::Inout => "inout",
                        RirArgMode::Borrow => "borrow",
                        RirArgMode::Normal => unreachable!(),
                    };
                    return Err(rue_error::CompileError::new(
                        rue_error::ErrorKind::UnexpectedCallArgumentMode { mode },
                        self.rir.get(arg.value).span,
                    )
                    .with_help(format!(
                        "remove the `{mode}` keyword; this argument is passed without an explicit mode"
                    )));
                }
            }
        }
        Ok(())
    }
}

impl std::ops::DerefMut for Sema<'_, MutableDeclarations> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.declarations
    }
}

pub(crate) type BodySema<'a> = Sema<'a, SourceDeclarations>;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NamespaceBoundarySnapshot {
    pub(crate) source_counts: [usize; 11],
    pub(crate) source_fingerprint: u64,
    pub(crate) generated_structs: usize,
    pub(crate) generated_enums: usize,
    pub(crate) anonymous_methods: usize,
}

#[cfg(test)]
impl BodySema<'_> {
    pub(crate) fn namespace_boundary_snapshot(&self) -> NamespaceBoundarySnapshot {
        use std::hash::{Hash, Hasher};

        fn fingerprint<T: Hash>(tag: u64, values: impl Iterator<Item = T>) -> u64 {
            values.fold(0, |fingerprint, value| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                tag.hash(&mut hasher);
                value.hash(&mut hasher);
                fingerprint ^ hasher.finish()
            })
        }

        let source_fingerprint = fingerprint(0, self.functions.keys())
            ^ fingerprint(1, self.functions_by_file_name.iter())
            ^ fingerprint(2, self.function_source_names.iter())
            ^ fingerprint(3, self.builtin_structs.iter())
            ^ fingerprint(4, self.structs_by_file_name.iter())
            ^ fingerprint(5, self.builtin_enums.iter())
            ^ fingerprint(6, self.enums_by_file_name.iter())
            ^ fingerprint(7, self.methods.keys())
            ^ fingerprint(8, self.named_method_declarations.iter())
            ^ fingerprint(9, self.constants_by_file_name.keys())
            ^ fingerprint(10, self.module_bindings.keys());

        NamespaceBoundarySnapshot {
            source_counts: [
                self.functions.len(),
                self.functions_by_file_name.len(),
                self.function_source_names.len(),
                self.builtin_structs.len(),
                self.structs_by_file_name.len(),
                self.builtin_enums.len(),
                self.enums_by_file_name.len(),
                self.methods.len(),
                self.named_method_declarations.len(),
                self.constants_by_file_name.len(),
                self.module_bindings.len(),
            ],
            source_fingerprint,
            generated_structs: self.generated_structs.len(),
            generated_enums: self.generated_enums.len(),
            anonymous_methods: self.anonymous_methods.len(),
        }
    }
}

impl<D: DeclarationPhase> Sema<'_, D> {
    pub(crate) fn body_owner_token(
        &self,
        file: FileId,
        name: &str,
        owner: Option<&str>,
        kind: BodyOwnerKind,
    ) -> BodyOwnerToken {
        let key = (
            file.index(),
            name.to_owned(),
            owner.map(str::to_owned),
            kind,
        );
        if self.body_owner_tokens.is_empty() {
            // Explicit synthetic fixtures and declaration-failure recovery do
            // not cross a durable identity boundary. Issuer zero is reserved
            // for those present API contracts; successful canonical analysis
            // installs compiler-issued tokens before bodies are analyzed.
            return BodyOwnerToken::new(0, file.index());
        }
        *self.body_owner_tokens.get(&key).unwrap_or_else(|| {
            panic!("supported ordinary body must have a validated owner token: {key:?}; installed={:?}", self.body_owner_tokens.keys().collect::<Vec<_>>())
        })
    }
}

impl<'a> Sema<'a> {
    fn freeze_declarations(self) -> BodySema<'a> {
        self.map_declarations(|MutableDeclarations(namespace)| SourceDeclarations(namespace))
    }

    /// Create a phase-local semantic analyzer for synthetic/test use.
    ///
    /// This compatibility entry point derives explicit synthetic identities
    /// from the RIR and never participates in durable compiler joins. Supported
    /// compiler construction uses [`Self::new_for_target`] with canonical
    /// metadata.
    pub fn new_synthetic(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        preview_features: PreviewFeatures,
    ) -> Self {
        Self::new_for_target(
            rir,
            interner,
            preview_features,
            SemaMetadata::synthetic_for_rir(rir),
            Target::host()
                .expect("Rue cannot choose a default sema target on this unsupported host"),
        )
    }

    /// Create a new semantic analyzer for an explicit compilation target.
    pub fn new_for_target(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        preview_features: PreviewFeatures,
        metadata: SemaMetadata,
        target: Target,
    ) -> Self {
        let SemaMetadata {
            root_file_id,
            physical_paths,
            logical_paths,
        } = metadata;
        let type_pool = TypeInternPool::new();
        type_pool.set_symbol_paths(logical_paths.clone());
        Self {
            declarations: MutableDeclarations(DeclarationNamespace::new()),
            rir,
            interner,
            declaration_index: declaration_index::RirDeclarationIndex::new(rir),
            anonymous_methods: HashMap::new(),
            generated_structs: HashMap::new(),
            generated_enums: HashMap::new(),
            body_analysis_work: BodyAnalysisWork::default(),
            analyzed_body_owners: Vec::new(),
            ordinary_body_exports: Vec::new(),
            specialized_body_exports: Vec::new(),
            reusable_ordinary_bodies: HashMap::new(),
            reusable_specialized_bodies: Vec::new(),
            body_dependency_observer: None,
            body_owner_tokens: HashMap::new(),
            body_named_dependencies: Vec::new(),
            ordinary_free_function_dependencies: Vec::new(),
            specialized_free_function_origins: Vec::new(),
            specialized_free_function_dependencies: Vec::new(),
            named_method_dependencies: Vec::new(),
            non_generic_named_method_dependencies_complete: true,
            named_destructor_dependencies: Vec::new(),
            declaration_type_dependencies: Vec::new(),
            declaration_type_call_head_dependencies: Vec::new(),
            declaration_builtin_type_call_head_dependencies: Vec::new(),
            named_const_dependencies: Vec::new(),
            named_const_dependency_source: None,
            declaration_type_observer: None,
            const_resolution_in_progress: Vec::new(),
            declaration_binding_active: false,
            preview_features,
            target,
            builtin_arch_id: None,
            builtin_os_id: None,
            known: KnownSymbols::new(interner),
            type_pool,
            module_registry: crate::module_registry::ModuleRegistry::new(),
            canonical_imports: None,
            file_paths: physical_paths,
            symbol_paths: logical_paths,
            trusted_standard_library_files: HashSet::new(),
            root_file_id: Some(root_file_id),
            param_arena: ParamArena::new(),
            anon_struct_method_sigs: HashMap::new(),
            anon_struct_captured_values: HashMap::new(),
            anon_struct_type_subst: HashMap::new(),
            destructor_spans: HashMap::new(),
            infectious_linear: HashMap::new(),
            deferred_ownership_gates: Vec::new(),
            comptime_type_call_depth: 0,
            fn_signatures_in_flight: HashSet::new(),
            ctor_type_displays: HashMap::new(),
        }
    }

    /// Exact structural work used to index this semantic request's raw RIR
    /// declaration candidates.
    ///
    /// The counters contain no instruction handles and are safe to retain for
    /// profiling. The indexed declarations themselves remain private and
    /// meaningful only for this `Sema` and its exact RIR/interner epoch.
    #[inline]
    pub fn rir_declaration_index_work(&self) -> RirDeclarationIndexWork {
        self.declaration_index.work()
    }

    /// Perform semantic analysis on the RIR.
    ///
    /// This is the main entry point for semantic analysis. It returns analyzed
    /// functions, struct definitions, enum definitions, and any warnings.
    pub fn analyze_all(self) -> MultiErrorResult<SemaOutput> {
        self.bind_declarations()?.analyze_all_bodies()
    }

    /// Complete all declaration and namespace binding without analyzing bodies.
    pub fn bind_declarations(self) -> MultiErrorResult<BoundSema<'a>> {
        self.predeclare_declaration_shells()?.resolve_declarations()
    }

    /// Validate module-keyed declaration namespaces and predeclare nominal shells.
    ///
    /// This is the explicit boundary before resolved declaration semantics are
    /// installed. The batch adapter [`Self::bind_declarations`] immediately
    /// executes both sides of the boundary and therefore preserves historical
    /// ordering and failure behavior.
    pub fn predeclare_declaration_shells(mut self) -> MultiErrorResult<DeclarationShells<'a>> {
        let mut binding_work =
            DeclarationBindingWork::from_inputs(self.rir.len(), self.declaration_index.work());
        // Phase 0: Inject built-in types (String, etc.) before user code.
        // Canonical merge has already validated function/type/main conflicts;
        // value constants are checked later once module bindings are known.
        self.inject_builtin_types();
        binding_work.namespace_setup_invocations += 1;

        // Phase 1: Register nominal type shells.
        self.register_type_names().map_err(CompileErrors::from)?;
        binding_work.nominal_type_predeclaration_invocations += 1;

        // Phase 2: Freeze stable-joinable identities and authoritative
        // current-revision syntax metadata before resolving semantic payloads.
        // This does not evaluate constants or resolve any signature.
        let (pending_payloads, pending_nominals) = self.predeclare_callable_value_shells();
        binding_work.callable_value_predeclaration_invocations += 1;
        binding_work.callable_value_shells_predeclared = pending_payloads.len();
        binding_work.indexed_declaration_records_visited =
            pending_payloads.len() + pending_nominals.len();
        Ok(DeclarationShells {
            sema: self,
            binding_work,
            pending_payloads,
            pending_nominals,
        })
    }
}

impl BodySema<'_> {
    /// Analyze all function bodies using a structurally frozen source namespace.
    fn analyze_all_bodies(self) -> MultiErrorResult<SemaOutput> {
        analysis::analyze_all_function_bodies(self)
    }

    fn analyze_all_bodies_with_work(self) -> Result<SemaOutput, BodyAnalysisFailure> {
        analysis::analyze_all_function_bodies_with_work(self)
    }
}

#[cfg(test)]
mod consistency_tests;
#[cfg(test)]
mod tests;
