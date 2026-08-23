//! Canonical, provenance-safe lowering from parsed modules to RIR.

use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use ahash::AHashMap;
use lasso::Key;
use rue_error::{CompileError, ErrorKind};
use rue_rir::{
    AstGen, InstRef, PackedRirMetadata, PackedRirMethodOwner, PackedRirProjection,
    PackedValidatedRir, Rir, RirEditor, RirPayloadBuildError, RirValidationContext, ValidatedRir,
};
use rue_span::FileId;

use crate::retained_charge::RetainedCharge;
use crate::{CanonicalMergedProgram, SemanticSymbolUniverse, SourceRevision};

fn interner_resource_error(kind: lasso::LassoErrorKind) -> CompileError {
    CompileError::without_span(rue_lexer::interner_error_kind(
        kind,
        format!("this compilation could not intern another spelling: {kind}"),
    ))
}

/// Classify a RIR construction failure for the user.
///
/// Spec C.1:2 makes exceeding a published implementation limit a diagnosable
/// compile-time failure, not an internal compiler error: a program that is too
/// large for the `u32` instruction array or the `u32`-indexed payload word
/// store (Appendix C.6:1) is rejected with `E1401` naming the limit it hit.
/// Only a genuine producer bug (a malformed builder request) stays an ICE, and
/// a failed reservation for a representable request is `E1402`.
pub(crate) fn rir_build_error_kind(context: &str, error: &RirPayloadBuildError) -> ErrorKind {
    if error.is_resource_limit() {
        ErrorKind::CompilerResourceLimit(error.to_string())
    } else if error.is_resource_exhaustion() {
        ErrorKind::CompilerResourceExhaustion(error.to_string())
    } else {
        ErrorKind::InternalError(format!("{context}: {error}"))
    }
}

/// Structural work performed by canonical RIR lowering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CanonicalRirWork {
    /// Module lowerers that executed for this request (cache hits contribute zero).
    pub modules_visited: usize,
    pub items_visited: usize,
    pub symbol_fields_translated: usize,
    pub semantic_intern_attempts: usize,
    pub unique_semantic_strings: usize,
    /// All strings retained by the final RIR universe, including synthesized names.
    pub semantic_strings_retained: usize,
    pub parser_invocations: usize,
    pub ast_payload_clones: usize,
    pub source_text_clones: usize,
    /// Deterministic compatibility projection work, accounted separately from
    /// module lowering so terminal reuse remains visible.
    pub modules_projected: usize,
    pub instructions_appended: usize,
    pub payload_words_appended: usize,
}

/// RIR paired with the exact source revision and symbol universe that created it.
#[derive(Debug)]
pub struct CanonicalRirOutput {
    source_revision: SourceRevision,
    rir: ValidatedRir,
    symbols: SemanticSymbolUniverse,
    work: CanonicalRirWork,
    module_ranges: Vec<CanonicalRirModuleRange>,
    sources: Vec<CanonicalRirSource>,
}

/// Independently reusable lowering result for exactly one module source leaf.
#[derive(Debug)]
pub(crate) struct CandidateModuleRirOutput {
    revision: crate::ModuleRevision,
    source_length: u32,
    rir: ValidatedRir,
    symbols: SemanticSymbolUniverse,
    work: CanonicalRirWork,
    #[cfg(test)]
    declaration_roots:
        AHashMap<crate::declaration_candidate::DeclarationCandidateKey, ModuleDeclarationRoot>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleDeclarationRoot {
    pub(crate) declaration: InstRef,
    /// Exact contiguous instruction interval emitted for this declaration.
    /// Canonical AstGen is post-order and never shares instruction nodes across
    /// declaration producers; module publication validates that every child
    /// edge remains inside this interval before retaining it.
    pub(crate) instructions: Range<u32>,
    /// Exact enclosing named-struct declaration for a method or associated
    /// function. Projection retains this direct edge while omitting unrelated
    /// sibling methods.
    pub(crate) owner: Option<InstRef>,
}

/// Immutable RIR lowered for exactly one declaration candidate. The bundle is
/// constructed once at the candidate-keyed query boundary and shared by the
/// ordinary body plus every specialization.
#[derive(Debug)]
pub(crate) struct DeclarationBodyPlan {
    packed: PackedValidatedRir,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BodyPlanMaterializationAttribution {
    pub(crate) span_remap_validation_ns: u64,
    pub(crate) base_symbol_rebuild_ns: u64,
    pub(crate) base_symbols_rebuilt: u64,
    pub(crate) index: rue_air::BodyRirIndexAttribution,
    pub(crate) rir_instructions: u64,
    pub(crate) rir_payload_words: u64,
}

#[derive(Debug)]
pub(crate) enum BodyPlanMaterializationFailure {
    Query(rue_query::QueryAbort),
    Build(RirPayloadBuildError),
    Invalid(Arc<str>),
}

impl std::fmt::Display for BodyPlanMaterializationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Query(error) => write!(formatter, "{error:?}"),
            Self::Build(error) => write!(formatter, "{error}"),
            Self::Invalid(detail) => formatter.write_str(detail),
        }
    }
}

impl DeclarationBodyPlan {
    pub(crate) fn structurally_eq(&self, other: &Self) -> bool {
        self.packed == other.packed
    }

    /// Reconstitute the existing rue-air body boundary from this canonical
    /// candidate plan and the current source-coordinate basis.
    ///
    /// This is the sole temporary adapter while rue-air still requires an
    /// owned, current-coordinate `ValidatedRir` and one mutable
    /// `ThreadedRodeo` per analysis transaction. It performs no parsing or
    /// AstGen lowering: instruction identity, payloads, symbols, and span-slot
    /// topology all come from the plan. The remaining remap, validation,
    /// interner reconstruction, and index build are deliberately visible here
    /// for the subsequent immutable-base/projected-view tranche.
    pub(crate) fn materialize_body_rir_bundle(
        &self,
        space: &rue_rir::SharedSymbolSpace,
        file_id: FileId,
        declaration_start: u32,
        source_length: u32,
        checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<rue_air::BodyRirBundle, BodyPlanMaterializationFailure> {
        self.materialize_body_rir_bundle_internal(
            space,
            file_id,
            declaration_start,
            source_length,
            false,
            checkpoint,
        )
        .map(|(bundle, _, _)| bundle)
    }

    pub(crate) fn materialize_body_rir_bundle_with_attribution(
        &self,
        space: &rue_rir::SharedSymbolSpace,
        file_id: FileId,
        declaration_start: u32,
        source_length: u32,
        checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<
        (rue_air::BodyRirBundle, BodyPlanMaterializationAttribution),
        BodyPlanMaterializationFailure,
    > {
        self.materialize_body_rir_bundle_internal(
            space,
            file_id,
            declaration_start,
            source_length,
            true,
            checkpoint,
        )
        .map(|(bundle, attribution, _)| (bundle, attribution))
    }

    /// Materialize the producer candidate and retain its exact decoded
    /// declaration root. Anonymous-member analysis uses that root to select a
    /// nested producer/member directly from this same candidate graph.
    pub(crate) fn materialize_body_rir_bundle_with_declaration(
        &self,
        space: &rue_rir::SharedSymbolSpace,
        file_id: FileId,
        declaration_start: u32,
        source_length: u32,
        checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<(rue_air::BodyRirBundle, InstRef), BodyPlanMaterializationFailure> {
        self.materialize_body_rir_bundle_internal(
            space,
            file_id,
            declaration_start,
            source_length,
            false,
            checkpoint,
        )
        .map(|(bundle, _, declaration)| (bundle, declaration))
    }

    pub(crate) fn materialize_body_rir_bundle_with_declaration_and_attribution(
        &self,
        space: &rue_rir::SharedSymbolSpace,
        file_id: FileId,
        declaration_start: u32,
        source_length: u32,
        checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<
        (
            rue_air::BodyRirBundle,
            InstRef,
            BodyPlanMaterializationAttribution,
        ),
        BodyPlanMaterializationFailure,
    > {
        self.materialize_body_rir_bundle_internal(
            space,
            file_id,
            declaration_start,
            source_length,
            true,
            checkpoint,
        )
        .map(|(bundle, attribution, declaration)| (bundle, declaration, attribution))
    }

    fn materialize_body_rir_bundle_internal(
        &self,
        space: &rue_rir::SharedSymbolSpace,
        file_id: FileId,
        declaration_start: u32,
        source_length: u32,
        attribution_enabled: bool,
        mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<
        (
            rue_air::BodyRirBundle,
            BodyPlanMaterializationAttribution,
            InstRef,
        ),
        BodyPlanMaterializationFailure,
    > {
        let (rir, declaration, _remap_finished_ns, symbol_finished_ns, validation_finished_ns) =
            self.materialize_candidate_rir_internal(
                space,
                file_id,
                declaration_start,
                source_length,
                true,
                attribution_enabled,
                &mut checkpoint,
            )?;
        let rir_instructions = rir.len() as u64;
        let rir_payload_words = rir.extra_len() as u64;
        // `BodyRirIndexAttribution::duration_ns` documents itself as charged by
        // the compiler-owned contiguous lowering clock, and until RUE-1515
        // nothing charged it: the field was summed into
        // `semantic_body_input_attributed_total` and published as
        // `body_rir_index_ns` while always reading zero, so published phase
        // attribution understated the index build by exactly its own cost. The
        // clock lives here rather than in `rue-air` because the doc says the
        // compiler owns it, and because this is the only construction site.
        let index_started = attribution_enabled.then(std::time::Instant::now);
        let (bundle, mut index) = rue_air::BodyRirBundle::new_with_index_attribution(
            rir,
            space.clone(),
            attribution_enabled,
        );
        index.duration_ns = index_started.map_or(0, |started| {
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
        });
        checkpoint().map_err(BodyPlanMaterializationFailure::Query)?;
        Ok((
            bundle,
            BodyPlanMaterializationAttribution {
                // The historical schema field is now the complete bounded
                // plan-materialization adapter interval: span projection,
                // temporary base-symbol reconstruction, and validation. The
                // symbol-only subset is also published separately so this
                // transitional cost remains visible rather than masquerading
                // as deleted parsing or RIR lowering.
                span_remap_validation_ns: validation_finished_ns,
                base_symbol_rebuild_ns: symbol_finished_ns,
                base_symbols_rebuilt: self.packed.symbol_count() as u64,
                index,
                rir_instructions,
                rir_payload_words,
            },
            declaration,
        ))
    }

    #[cfg(test)]
    pub(crate) fn materialize_candidate_rir(
        &self,
        space: &rue_rir::SharedSymbolSpace,
        file_id: FileId,
        declaration_start: u32,
        source_length: u32,
        mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<ValidatedRir, BodyPlanMaterializationFailure> {
        self.materialize_candidate_rir_internal(
            space,
            file_id,
            declaration_start,
            source_length,
            true,
            false,
            &mut checkpoint,
        )
        .map(|(rir, _, _, _, _)| rir)
    }

    /// Decode this candidate for declaration-time constant/comptime
    /// evaluation. Coordinates remain declaration-relative: the semantic
    /// nucleus retains producer-relative diagnostic ranges and projects them
    /// through the current source basis only when diagnostics are presented.
    ///
    /// This is the canonical replacement for rebuilding a fake declaration
    /// source and reparsing it. It performs no lexer, parser, or AstGen work.
    pub(crate) fn materialize_semantic_candidate_rir(
        &self,
        mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<(ValidatedRir, Vec<&str>, InstRef), BodyPlanMaterializationFailure> {
        checkpoint().map_err(BodyPlanMaterializationFailure::Query)?;
        let packed_symbols = self.packed.symbols();
        let mut symbols = Vec::new();
        symbols
            .try_reserve_exact(packed_symbols.len())
            .map_err(|_| {
                BodyPlanMaterializationFailure::Build(RirPayloadBuildError::CapacityFailure {
                    family: "semantic candidate symbol view",
                })
            })?;
        for (ordinal, spelling) in packed_symbols.enumerate() {
            if ordinal & 63 == 0 {
                checkpoint().map_err(BodyPlanMaterializationFailure::Query)?;
            }
            symbols.push(spelling);
        }

        let symbol_count = symbols.len();
        let (rir, metadata) = self
            .packed
            .try_decode_validated(
                PackedRirProjection {
                    symbol_count,
                    file_id: FileId::DEFAULT,
                    declaration_start: 0,
                    source_length: u32::MAX,
                },
                || checkpoint().map_err(BodyPlanMaterializationFailure::Query),
            )
            .map_err(|error| match error {
                rue_rir::PackedRirAppendError::Checkpoint(failure)
                | rue_rir::PackedRirAppendError::SymbolRemap(failure)
                | rue_rir::PackedRirAppendError::SpanRemap { error: failure, .. } => failure,
                rue_rir::PackedRirAppendError::Build(error) => {
                    BodyPlanMaterializationFailure::Build(error)
                }
                rue_rir::PackedRirAppendError::Decode(error) => {
                    BodyPlanMaterializationFailure::Invalid(Arc::from(format!(
                        "private packed semantic candidate is invalid: {error}"
                    )))
                }
            })?;
        checkpoint().map_err(BodyPlanMaterializationFailure::Query)?;
        Ok((rir, symbols, metadata.declaration))
    }

    /// Materialize this body plan's RIR into the revision-shared equality space
    /// (ADR-0076 §1).
    ///
    /// The packed envelope speaks the body-private dense encoding space: a
    /// symbol *is* its ordinal in the dense spelling section. This builds the
    /// body's dense remap — ordinal to shared handle, one entry per ordinal,
    /// interned once per revision rather than once per body — and decodes the
    /// candidate through it, so the RIR names symbols in the same space its
    /// analysis state does. The dense space holds only that remap; it does not
    /// re-intern the section into a private table.
    fn materialize_candidate_rir_internal(
        &self,
        space: &rue_rir::SharedSymbolSpace,
        file_id: FileId,
        declaration_start: u32,
        source_length: u32,
        include_method_owner: bool,
        attribution_enabled: bool,
        checkpoint: &mut impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<(ValidatedRir, InstRef, u64, u64, u64), BodyPlanMaterializationFailure> {
        checkpoint().map_err(BodyPlanMaterializationFailure::Query)?;
        let started = attribution_enabled.then(Instant::now);
        let elapsed = || {
            started.map_or(0, |started| {
                u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
            })
        };
        let interner = space.interner();
        let packed_symbols = self.packed.symbols();
        let mut dense_remap = Vec::new();
        dense_remap
            .try_reserve_exact(packed_symbols.len())
            .map_err(|_| {
                BodyPlanMaterializationFailure::Build(RirPayloadBuildError::CapacityFailure {
                    family: "body-plan dense symbol remap",
                })
            })?;
        for (ordinal, spelling) in packed_symbols.enumerate() {
            if ordinal & 63 == 0 {
                checkpoint().map_err(BodyPlanMaterializationFailure::Query)?;
            }
            // The dense space's own invariant, unchanged in meaning from the
            // per-body interner it replaces: the remap entry a spelling lands
            // at *is* the ordinal the packed envelope encodes it as. A shared
            // handle's numeric value is a revision-wide, scheduling-dependent
            // datum and deliberately says nothing about this ordinal.
            if dense_remap.len() != ordinal {
                return Err(BodyPlanMaterializationFailure::Invalid(Arc::from(
                    "body-plan symbol universe did not preserve stable ordinals",
                )));
            }
            let symbol = rue_lexer::try_intern(interner, spelling).map_err(|kind| {
                BodyPlanMaterializationFailure::Build(RirPayloadBuildError::InternerFailure {
                    family: "interned strings",
                    kind,
                })
            })?;
            dense_remap.push(symbol);
        }
        checkpoint().map_err(BodyPlanMaterializationFailure::Query)?;
        let symbol_finished_ns = elapsed();
        let symbol_count = dense_remap.len();
        let projection = PackedRirProjection {
            symbol_count,
            file_id,
            declaration_start,
            source_length,
        };
        let checkpoint_decode = || checkpoint().map_err(BodyPlanMaterializationFailure::Query);
        let (rir, metadata) = self
            .packed
            .try_decode_validated_remapped(
                projection,
                include_method_owner,
                checkpoint_decode,
                |ordinal| dense_remap.get(ordinal as usize).copied(),
            )
            .map_err(|error| match error {
                rue_rir::PackedRirAppendError::Checkpoint(failure)
                | rue_rir::PackedRirAppendError::SymbolRemap(failure)
                | rue_rir::PackedRirAppendError::SpanRemap { error: failure, .. } => failure,
                rue_rir::PackedRirAppendError::Build(error) => {
                    BodyPlanMaterializationFailure::Build(error)
                }
                rue_rir::PackedRirAppendError::Decode(error) => {
                    BodyPlanMaterializationFailure::Invalid(Arc::from(format!(
                        "private packed body plan is invalid: {error}"
                    )))
                }
            })?;
        let remap_finished_ns = elapsed();
        checkpoint().map_err(BodyPlanMaterializationFailure::Query)?;
        let validation_finished_ns = elapsed();
        Ok((
            rir,
            metadata.declaration,
            remap_finished_ns,
            symbol_finished_ns,
            validation_finished_ns,
        ))
    }

    pub(crate) fn instruction_count(&self) -> usize {
        self.packed.instruction_count()
    }

    pub(crate) fn fallible_intrinsics(&self) -> rue_rir::RirFallibleIntrinsicSet {
        self.packed.fallible_intrinsics()
    }
}

#[derive(Debug)]
pub(crate) struct DeclarationBodyPlanArtifacts {
    pub(crate) plan: DeclarationBodyPlan,
}

#[derive(Debug)]
pub(crate) enum DeclarationBodyPlanBuildFailure {
    Query(rue_query::QueryAbort),
    MissingCandidate,
    ForeignSymbol(Arc<str>),
    Build(RirPayloadBuildError),
    Payload(Arc<str>),
    Validation(Arc<str>),
    SpanProjection(Arc<str>),
}

impl RetainedCharge for DeclarationBodyPlan {
    fn retained_charge(&self) -> u64 {
        self.packed.retained_allocation_charge()
    }
}

impl RetainedCharge for DeclarationBodyPlanArtifacts {
    fn retained_charge(&self) -> u64 {
        self.plan.retained_charge()
    }
}

/// Lower one exact parser candidate directly into its reusable body plan.
/// The candidate owns the only AstGen invocation; whole-RIR presentation
/// composes the same candidate artifacts rather than lowering the module AST
/// through a peer generator.
pub(crate) fn lower_parsed_declaration_body_plan(
    module: &crate::parsed_modules::ParsedModule,
    candidate: &crate::declaration_candidate::DeclarationCandidateKey,
    checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
) -> Result<DeclarationBodyPlanArtifacts, DeclarationBodyPlanBuildFailure> {
    let anchors = module
        .declaration_anonymous_sites(candidate)
        .ok_or(DeclarationBodyPlanBuildFailure::MissingCandidate)?;
    lower_parsed_declaration_body_plan_internal(module, candidate, anchors, checkpoint)
}

fn lower_parsed_declaration_body_plan_internal(
    module: &crate::parsed_modules::ParsedModule,
    candidate: &crate::declaration_candidate::DeclarationCandidateKey,
    authoritative_anchors: &[rue_rir::AnonymousTypeSite],
    mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
) -> Result<DeclarationBodyPlanArtifacts, DeclarationBodyPlanBuildFailure> {
    checkpoint().map_err(DeclarationBodyPlanBuildFailure::Query)?;
    let ast = module
        .declaration_ast(candidate)
        .ok_or(DeclarationBodyPlanBuildFailure::MissingCandidate)?;
    let symbols = lasso::ThreadedRodeo::new();
    let symbol_failure = RefCell::<Option<Arc<str>>>::new(None);
    let interner_failure = RefCell::<Option<lasso::LassoErrorKind>>::new(None);
    let aborted = RefCell::new(None);
    let mut cancellation_check = || match checkpoint() {
        Ok(()) => true,
        Err(abort) => {
            if aborted.borrow().is_none() {
                *aborted.borrow_mut() = Some(abort);
            }
            false
        }
    };
    let (editor, declaration, method_owner) = {
        // AstGen's historical symbol-normalizer callback is infallible.  A
        // failed insertion therefore needs a control-flow sentinel while the
        // walk finishes; `interner_failure` is checked immediately after that
        // walk and before validation or `finish_declaration_body_plan`, so the
        // sentinel can never key or publish a successful declaration artifact.
        let mut generator = AstGen::with_symbol_normalizer(&symbols, |local| {
            match module.try_resolve_raw_symbol(local) {
                Some(spelling) => match rue_lexer::try_intern(&symbols, spelling) {
                    Ok(symbol) => symbol,
                    Err(kind) => {
                        *interner_failure.borrow_mut() = Some(kind);
                        lasso::Spur::default()
                    }
                },
                None => {
                    if symbol_failure.borrow().is_none() {
                        *symbol_failure.borrow_mut() = Some(Arc::from(format!(
                            "candidate AST references foreign symbol ordinal {}",
                            local.into_usize()
                        )));
                    }
                    match rue_lexer::try_intern(&symbols, "_@rue:invalid-candidate-symbol") {
                        Ok(symbol) => symbol,
                        Err(kind) => {
                            *interner_failure.borrow_mut() = Some(kind);
                            lasso::Spur::default()
                        }
                    }
                }
            }
        });
        generator.install_cancellation_check(&mut cancellation_check);
        generator
            .install_authoritative_anonymous_anchors(
                authoritative_anchors
                    .iter()
                    .map(|site| (site.span, site.kind, site.anchor.clone())),
            )
            .map_err(|error| {
                DeclarationBodyPlanBuildFailure::Payload(Arc::from(error.to_string()))
            })?;
        let (producer, owner) = match ast {
            crate::parsed_modules::ParsedDeclarationAstRef::Function(value) => {
                (rue_rir::AstGenCandidate::Function(value), None)
            }
            crate::parsed_modules::ParsedDeclarationAstRef::Struct(value) => {
                (rue_rir::AstGenCandidate::StructShell(value), None)
            }
            crate::parsed_modules::ParsedDeclarationAstRef::Enum(value) => {
                (rue_rir::AstGenCandidate::Enum(value), None)
            }
            crate::parsed_modules::ParsedDeclarationAstRef::Const(value) => {
                (rue_rir::AstGenCandidate::Const(value), None)
            }
            crate::parsed_modules::ParsedDeclarationAstRef::Destructor(value) => {
                (rue_rir::AstGenCandidate::DropFn(value), None)
            }
            crate::parsed_modules::ParsedDeclarationAstRef::Method {
                owner,
                method,
                ordinal,
            } => (
                rue_rir::AstGenCandidate::Method { method, ordinal },
                Some(owner),
            ),
            crate::parsed_modules::ParsedDeclarationAstRef::ExternFunction { function, .. } => {
                (rue_rir::AstGenCandidate::ExternFn(function), None)
            }
        };
        let root = generator.append_candidate_with_root(producer);
        let editor = match generator.try_finish_editor_with_cancellation() {
            Ok(editor) => editor,
            Err(rue_rir::AstGenFinishError::Canceled) => {
                let abort = aborted.borrow_mut().take().ok_or_else(|| {
                    DeclarationBodyPlanBuildFailure::Validation(Arc::from(
                        "candidate AstGen canceled without a query cancellation",
                    ))
                })?;
                return Err(DeclarationBodyPlanBuildFailure::Query(abort));
            }
            Err(rue_rir::AstGenFinishError::Payload(error)) => {
                return Err(
                    if error.is_resource_limit() || error.is_resource_exhaustion() {
                        DeclarationBodyPlanBuildFailure::Build(error)
                    } else {
                        DeclarationBodyPlanBuildFailure::Payload(Arc::from(error.to_string()))
                    },
                );
            }
        };
        (editor, root.declaration, owner)
    };
    if let Some(abort) = aborted.into_inner() {
        return Err(DeclarationBodyPlanBuildFailure::Query(abort));
    }
    if let Some(error) = symbol_failure.into_inner() {
        return Err(DeclarationBodyPlanBuildFailure::ForeignSymbol(error));
    }
    if let Some(kind) = interner_failure.into_inner() {
        return Err(DeclarationBodyPlanBuildFailure::Build(
            RirPayloadBuildError::InternerFailure {
                family: "interned strings",
                kind,
            },
        ));
    }
    validate_candidate_root(&editor, declaration, candidate, &symbols)?;
    let method_owner = if let Some(owner) = method_owner {
        let owner_name = module
            .try_resolve_raw_symbol(owner.name.name)
            .ok_or_else(|| {
                DeclarationBodyPlanBuildFailure::ForeignSymbol(Arc::from(
                    "method owner name is foreign to the parsed module symbol universe",
                ))
            })?;
        let owner_name = rue_lexer::try_intern(&symbols, owner_name).map_err(|kind| {
            DeclarationBodyPlanBuildFailure::Build(RirPayloadBuildError::InternerFailure {
                family: "interned strings",
                kind,
            })
        })?;
        Some(PackedRirMethodOwner {
            declaration,
            name: owner_name,
            is_public: owner.visibility == rue_parser::ast::Visibility::Public,
            is_linear: owner.is_linear,
        })
    } else {
        None
    };
    let source_length = u32::try_from(module.source_text().len()).map_err(|_| {
        DeclarationBodyPlanBuildFailure::Validation(Arc::from(
            "candidate source length exceeds RIR span capacity",
        ))
    })?;
    let declaration_start = module
        .definitions()
        .declaration_locator(candidate)
        .ok_or(DeclarationBodyPlanBuildFailure::MissingCandidate)?
        .declaration_span
        .start;
    finish_declaration_body_plan(
        editor,
        symbols,
        declaration,
        method_owner,
        module.file_id(),
        declaration_start,
        source_length,
        checkpoint,
    )
}

fn validate_candidate_root(
    editor: &RirEditor,
    declaration: InstRef,
    candidate: &crate::declaration_candidate::DeclarationCandidateKey,
    symbols: &lasso::ThreadedRodeo,
) -> Result<(), DeclarationBodyPlanBuildFailure> {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let instruction = editor.get(declaration);
    let name_matches =
        |name: &lasso::Spur| symbols.try_resolve(name) == Some(candidate.name.as_ref());
    let exact = match (&instruction.data, candidate.category) {
        (
            rue_rir::InstData::FnDecl {
                name,
                has_self: false,
                is_extern: false,
                ..
            },
            Category::Function | Category::AssociatedFunction,
        ) => name_matches(name),
        (
            rue_rir::InstData::FnDecl {
                name,
                has_self: true,
                is_extern: false,
                ..
            },
            Category::Method,
        ) => name_matches(name),
        (
            rue_rir::InstData::FnDecl {
                name,
                has_self: false,
                is_extern: true,
                ..
            },
            Category::ExternFunction,
        ) => name_matches(name),
        (rue_rir::InstData::StructDecl { name, methods, .. }, Category::Struct) => {
            name_matches(name) && editor.struct_methods(methods).is_empty()
        }
        (rue_rir::InstData::EnumDecl { name, .. }, Category::Enum)
        | (rue_rir::InstData::ConstDecl { name, .. }, Category::ConstCandidate) => {
            name_matches(name)
        }
        (rue_rir::InstData::DropFnDecl { type_name, .. }, Category::Destructor) => {
            name_matches(type_name)
        }
        _ => false,
    };
    if !exact {
        return Err(DeclarationBodyPlanBuildFailure::Validation(Arc::from(
            "candidate AstGen root does not match its exact declaration key",
        )));
    }
    Ok(())
}

fn finish_declaration_body_plan(
    extracted: RirEditor,
    symbols: lasso::ThreadedRodeo,
    declaration: InstRef,
    method_owner: Option<PackedRirMethodOwner>,
    file_id: FileId,
    declaration_start: u32,
    source_length: u32,
    mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
) -> Result<DeclarationBodyPlanArtifacts, DeclarationBodyPlanBuildFailure> {
    let source_lengths = [(file_id, source_length)];
    let rir = ValidatedRir::finish(
        extracted,
        &RirValidationContext {
            symbol_count: symbols.len(),
            source_lengths: &source_lengths,
        },
    )
    .map_err(|error| DeclarationBodyPlanBuildFailure::Validation(Arc::from(error.to_string())))?;
    checkpoint().map_err(DeclarationBodyPlanBuildFailure::Query)?;

    #[derive(Debug)]
    enum NormalizeFailure {
        Query(rue_query::QueryAbort),
        BeforeDeclaration,
    }
    let packed = rir
        .try_pack_candidate(
            &symbols,
            PackedRirMetadata {
                declaration,
                method_owner,
            },
            || checkpoint().map_err(NormalizeFailure::Query),
            |_slot, span| {
                let start = span
                    .start
                    .checked_sub(declaration_start)
                    .ok_or(NormalizeFailure::BeforeDeclaration)?;
                let end = span
                    .end
                    .checked_sub(declaration_start)
                    .ok_or(NormalizeFailure::BeforeDeclaration)?;
                Ok((start, end))
            },
        )
        .map_err(|error| match error {
            rue_rir::PackedRirEncodeError::Checkpoint(NormalizeFailure::Query(abort))
            | rue_rir::PackedRirEncodeError::SpanProjection {
                error: NormalizeFailure::Query(abort),
                ..
            } => DeclarationBodyPlanBuildFailure::Query(abort),
            rue_rir::PackedRirEncodeError::Checkpoint(NormalizeFailure::BeforeDeclaration)
            | rue_rir::PackedRirEncodeError::SpanProjection {
                error: NormalizeFailure::BeforeDeclaration,
                ..
            } => DeclarationBodyPlanBuildFailure::SpanProjection(Arc::from(
                "body-plan span lies before its exact declaration origin",
            )),
            rue_rir::PackedRirEncodeError::CapacityFailure => {
                DeclarationBodyPlanBuildFailure::Build(RirPayloadBuildError::CapacityFailure {
                    family: "packed body-plan bytes",
                })
            }
            rue_rir::PackedRirEncodeError::ResourceLimit => DeclarationBodyPlanBuildFailure::Build(
                RirPayloadBuildError::ResourceLimitExceeded {
                    family: "packed body-plan bytes",
                },
            ),
            rue_rir::PackedRirEncodeError::Validation(error) => {
                DeclarationBodyPlanBuildFailure::Validation(Arc::from(error.to_string()))
            }
            other => DeclarationBodyPlanBuildFailure::Validation(Arc::from(format!("{other:?}"))),
        })?;
    Ok(DeclarationBodyPlanArtifacts {
        plan: DeclarationBodyPlan { packed },
    })
}

impl CandidateModuleRirOutput {
    pub(crate) fn revision(&self) -> &crate::ModuleRevision {
        &self.revision
    }

    pub(crate) fn work(&self) -> CanonicalRirWork {
        self.work
    }
}

impl RetainedCharge for CandidateModuleRirOutput {
    fn retained_charge(&self) -> u64 {
        let instructions = self
            .rir
            .len()
            .saturating_mul(std::mem::size_of::<rue_rir::Inst>()) as u64;
        let payload = self
            .rir
            .extra_len()
            .saturating_mul(std::mem::size_of::<u32>()) as u64;
        self.revision
            .retained_charge()
            .saturating_add(instructions)
            .saturating_add(payload)
            .saturating_add(self.symbols.retained_charge())
    }
}

#[derive(Debug)]
struct CanonicalRirModuleRange {
    file_id: FileId,
    instructions: Range<u32>,
    extra: Range<u32>,
}

#[derive(Debug)]
struct CanonicalRirSource {
    file_id: FileId,
    revision: crate::ModuleRevision,
    length: u32,
}

/// Ephemeral caller-order indices consumed by the read-only RIR printer.
pub struct CanonicalRirPresentationOrder {
    pub instructions: Vec<InstRef>,
    pub extra: Vec<u32>,
}

impl CanonicalRirOutput {
    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }

    pub fn rir(&self) -> &Rir {
        &self.rir
    }

    pub fn semantic_symbols(&self) -> &SemanticSymbolUniverse {
        &self.symbols
    }

    pub fn work(&self) -> CanonicalRirWork {
        self.work
    }

    pub(crate) fn source_identity_and_length(
        &self,
        file_id: FileId,
    ) -> (&crate::ModuleRevision, u32) {
        let source = self
            .sources
            .iter()
            .find(|source| source.file_id == file_id)
            .expect("validated RIR spans name a retained canonical source");
        (&source.revision, source.length)
    }

    /// Return a read-only instruction presentation order for caller-ordered files.
    ///
    /// Canonical RIR remains in stable module identity order; this permutation is
    /// consumed only by presentation printers and never changes semantic refs.
    pub fn presentation_order(
        &self,
        files: impl IntoIterator<Item = FileId>,
    ) -> CanonicalRirPresentationOrder {
        let mut instructions = Vec::with_capacity(self.rir.len());
        let mut extra = Vec::with_capacity(self.rir.extra_len());
        for file in files {
            let range = self
                .module_ranges
                .iter()
                .find(|candidate| candidate.file_id == file)
                .expect("RIR presentation file belongs to the canonical source revision");
            instructions.extend(range.instructions.clone().map(InstRef::from_raw));
            extra.extend(range.extra.clone());
        }
        assert_eq!(instructions.len(), self.rir.len());
        assert_eq!(extra.len(), self.rir.extra_len());
        CanonicalRirPresentationOrder {
            instructions,
            extra,
        }
    }
}

impl CanonicalRirWork {
    pub(crate) fn accumulate(&mut self, other: Self) {
        self.modules_visited += other.modules_visited;
        self.items_visited += other.items_visited;
        self.symbol_fields_translated += other.symbol_fields_translated;
        self.semantic_intern_attempts += other.semantic_intern_attempts;
        self.unique_semantic_strings += other.unique_semantic_strings;
        self.parser_invocations += other.parser_invocations;
        self.ast_payload_clones += other.ast_payload_clones;
        self.source_text_clones += other.source_text_clones;
    }
}

#[derive(Clone)]
#[cfg(test)]
struct EmittedDeclarationRoot {
    declaration: InstRef,
    instructions: Range<u32>,
    owner: Option<InstRef>,
}

#[cfg(test)]
fn candidate_root_index(
    module: &crate::parsed_modules::ParsedModule,
    rir: &ValidatedRir,
    symbols: &SemanticSymbolUniverse,
    item_roots: &[rue_rir::AstGenItemRoots],
) -> Result<
    AHashMap<crate::declaration_candidate::DeclarationCandidateKey, ModuleDeclarationRoot>,
    &'static str,
> {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use rue_rir::{AstGenItemRoots as Roots, InstData};

    let mut emitted = Vec::new();
    for roots in item_roots {
        match roots {
            Roots::Function(declaration)
            | Roots::Enum(declaration)
            | Roots::DropFn(declaration)
            | Roots::Const(declaration) => emitted.push(EmittedDeclarationRoot {
                declaration: declaration.declaration,
                instructions: declaration.start..declaration.end,
                owner: None,
            }),
            Roots::Struct {
                declaration,
                methods,
            } => {
                emitted.push(EmittedDeclarationRoot {
                    declaration: declaration.declaration,
                    instructions: declaration.start..declaration.end,
                    owner: None,
                });
                emitted.extend(methods.iter().map(|method| EmittedDeclarationRoot {
                    declaration: method.declaration,
                    instructions: method.start..method.end,
                    owner: Some(declaration.declaration),
                }));
            }
            Roots::Extern(functions) => {
                emitted.extend(functions.iter().map(|declaration| EmittedDeclarationRoot {
                    declaration: declaration.declaration,
                    instructions: declaration.start..declaration.end,
                    owner: None,
                }));
            }
            Roots::Error => {}
        }
    }

    let keys = module
        .definitions()
        .declaration_keys_in_source_order()
        .collect::<Vec<_>>();
    if keys.len() != emitted.len() {
        return Err("AstGen declaration roots disagree with the parser candidate count");
    }

    let mut arena_ranges = emitted
        .iter()
        .map(|root| (root.instructions.clone(), root.declaration))
        .collect::<Vec<_>>();
    arena_ranges.sort_unstable_by_key(|(range, _)| range.start);
    let mut expected_start = 0_u32;
    for (range, root) in &arena_ranges {
        if range.start != expected_start
            || range.start >= range.end
            || root.as_u32() < range.start
            || root.as_u32() >= range.end
        {
            return Err("AstGen declaration interval is not exact and contiguous");
        }
        expected_start = range.end;
    }
    if usize::try_from(expected_start).ok() != Some(rir.len()) {
        return Err("AstGen emitted instructions outside every declaration interval");
    }

    let mut index = AHashMap::with_capacity(keys.len());
    for (key, emitted) in keys.into_iter().zip(emitted) {
        let instruction = rir.get(emitted.declaration);
        if instruction.span.file_id != module.file_id() {
            return Err("AstGen declaration root has foreign file provenance");
        }
        let (category_matches, name) = match &instruction.data {
            InstData::FnDecl {
                is_extern,
                name,
                has_self,
                ..
            } => (
                match key.category {
                    Category::Function => !is_extern && emitted.owner.is_none() && !has_self,
                    Category::ExternFunction => *is_extern && emitted.owner.is_none(),
                    Category::Method => emitted.owner.is_some() && *has_self,
                    Category::AssociatedFunction => emitted.owner.is_some() && !has_self,
                    _ => false,
                },
                *name,
            ),
            InstData::StructDecl { name, .. } => (key.category == Category::Struct, *name),
            InstData::EnumDecl { name, .. } => (key.category == Category::Enum, *name),
            InstData::ConstDecl { name, .. } => (key.category == Category::ConstCandidate, *name),
            InstData::DropFnDecl { type_name, .. } => {
                (key.category == Category::Destructor, *type_name)
            }
            _ => return Err("AstGen candidate root is not a declaration instruction"),
        };
        if !category_matches || symbols.interner().resolve(&name) != key.name.as_ref() {
            return Err("AstGen declaration root disagrees with its exact parser candidate");
        }
        match (&key.owner, emitted.owner) {
            (Some(owner), Some(owner_ref)) => {
                let InstData::StructDecl { name, methods, .. } = &rir.get(owner_ref).data else {
                    return Err("method root owner is not a struct declaration");
                };
                if owner.category != Category::Struct
                    || symbols.interner().resolve(name) != owner.name.as_ref()
                    || !rir
                        .struct_methods(methods)
                        .values()
                        .any(|method| method == emitted.declaration)
                {
                    return Err("method root has a foreign or missing direct owner edge");
                }
            }
            (Some(owner), None) if key.category == Category::Destructor => {
                if owner.category != Category::Struct || owner.name != key.name {
                    return Err("destructor candidate has a foreign owner identity");
                }
            }
            (None, None) => {}
            _ => return Err("AstGen declaration owner shape disagrees with parser candidate"),
        }
        if index
            .insert(
                key.clone(),
                ModuleDeclarationRoot {
                    declaration: emitted.declaration,
                    instructions: emitted.instructions.clone(),
                    owner: emitted.owner,
                },
            )
            .is_some()
        {
            return Err("duplicate exact declaration candidate in AstGen root index");
        }
    }
    for (key, root) in &index {
        if !matches!(
            key.category,
            Category::Function
                | Category::ExternFunction
                | Category::ConstCandidate
                | Category::Destructor
                | Category::Method
                | Category::AssociatedFunction
        ) {
            continue;
        }
        for ordinal in root.instructions.clone() {
            let instruction = InstRef::from_raw(ordinal);
            let mut children = Vec::new();
            rir.child_instructions(instruction, &mut children);
            if children.iter().any(|child| {
                child.as_u32() < root.instructions.start || child.as_u32() >= root.instructions.end
            }) {
                return Err("AstGen declaration interval has a foreign child edge");
            }
        }
    }
    Ok(index)
}

#[cfg(test)]
pub(crate) fn lower_module_rir_with_work(
    module: std::sync::Arc<crate::parsed_modules::ParsedModule>,
) -> Result<CandidateModuleRirOutput, (CompileError, CanonicalRirWork)> {
    lower_module_rir_with_work_internal(module, None, None)
}

#[cfg(test)]
pub(crate) fn lower_module_rir_with_work_and_checkpoint(
    module: std::sync::Arc<crate::parsed_modules::ParsedModule>,
    mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
) -> Result<Result<CandidateModuleRirOutput, (CompileError, CanonicalRirWork)>, rue_query::QueryAbort>
{
    let aborted = RefCell::new(None);
    let mut cancellation_check = || match checkpoint() {
        Ok(()) => true,
        Err(abort) => {
            if aborted.borrow().is_none() {
                *aborted.borrow_mut() = Some(abort);
            }
            false
        }
    };
    let result = lower_module_rir_with_work_internal(module, None, Some(&mut cancellation_check));
    match aborted.into_inner() {
        Some(abort) => Err(abort),
        None => Ok(result),
    }
}

/// Compose one module from the same candidate artifacts consumed by body
/// analysis. This performs no AST lowering; recipes carry the sole typed
/// cross-fragment edge from a struct shell to its method roots.
fn project_candidate_span(
    file_id: FileId,
    declaration_start: u32,
    source_length: u32,
    relative_start: u32,
    relative_end: u32,
) -> Result<rue_span::Span, DeclarationBodyPlanBuildFailure> {
    let start = declaration_start
        .checked_add(relative_start)
        .ok_or_else(|| {
            DeclarationBodyPlanBuildFailure::SpanProjection(Arc::from(
                "projected candidate span start overflows current source",
            ))
        })?;
    let end = declaration_start.checked_add(relative_end).ok_or_else(|| {
        DeclarationBodyPlanBuildFailure::SpanProjection(Arc::from(
            "projected candidate span end overflows current source",
        ))
    })?;
    if start > end || end > source_length {
        return Err(DeclarationBodyPlanBuildFailure::SpanProjection(Arc::from(
            "projected candidate span is outside current source",
        )));
    }
    Ok(rue_span::Span::with_file(file_id, start, end))
}

pub(crate) fn compose_module_rir_from_candidate_artifacts(
    module: std::sync::Arc<crate::parsed_modules::ParsedModule>,
    artifacts: &AHashMap<
        crate::declaration_candidate::DeclarationCandidateKey,
        Arc<DeclarationBodyPlanArtifacts>,
    >,
    mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
) -> Result<CandidateModuleRirOutput, DeclarationBodyPlanBuildFailure> {
    let source_length = u32::try_from(module.source_text().len()).map_err(|_| {
        DeclarationBodyPlanBuildFailure::Validation(Arc::from(
            "module source length exceeds RIR span capacity",
        ))
    })?;
    let symbols =
        SemanticSymbolUniverse::from_modules(std::slice::from_ref(&module)).map_err(|error| {
            DeclarationBodyPlanBuildFailure::Build(RirPayloadBuildError::InternerFailure {
                family: "interned strings",
                kind: error.0,
            })
        })?;
    let mut editor = RirEditor::new();
    #[cfg(test)]
    let mut declaration_roots = AHashMap::with_capacity(artifacts.len());
    let mut work = CanonicalRirWork {
        modules_visited: 1,
        items_visited: module.definitions().rir_recipes().len(),
        ..CanonicalRirWork::default()
    };

    let append = |editor: &mut RirEditor,
                  key: &crate::declaration_candidate::DeclarationCandidateKey,
                  methods: Option<&[InstRef]>,
                  checkpoint: &mut dyn FnMut() -> Result<(), rue_query::QueryAbort>|
     -> Result<(Range<u32>, InstRef, usize), DeclarationBodyPlanBuildFailure> {
        let artifact = artifacts
            .get(key)
            .ok_or(DeclarationBodyPlanBuildFailure::MissingCandidate)?;
        let declaration_start = module
            .definitions()
            .declaration_locator(key)
            .ok_or(DeclarationBodyPlanBuildFailure::MissingCandidate)?
            .declaration_span
            .start;
        let mut local_symbols = Vec::with_capacity(artifact.plan.packed.symbols().len());
        for (ordinal, spelling) in artifact.plan.packed.symbols().enumerate() {
            if ordinal & 63 == 0 {
                checkpoint().map_err(DeclarationBodyPlanBuildFailure::Query)?;
            }
            local_symbols.push(rue_lexer::try_intern(symbols.interner(), spelling).map_err(
                |kind| {
                    DeclarationBodyPlanBuildFailure::Build(RirPayloadBuildError::InternerFailure {
                        family: "interned strings",
                        kind,
                    })
                },
            )?);
        }
        let mut symbols_translated = 0usize;
        let appended = if let Some(methods) = methods {
            artifact.plan.packed.try_append_remapped_with_root_methods(
                editor,
                methods,
                || checkpoint().map_err(DeclarationBodyPlanBuildFailure::Query),
                |ordinal| {
                    symbols_translated = symbols_translated.saturating_add(1);
                    local_symbols.get(ordinal as usize).copied().ok_or_else(|| {
                        DeclarationBodyPlanBuildFailure::Validation(Arc::from(
                            "candidate packed symbol ordinal is absent",
                        ))
                    })
                },
                |_slot, (relative_start, relative_end)| {
                    project_candidate_span(
                        module.file_id(),
                        declaration_start,
                        source_length,
                        relative_start,
                        relative_end,
                    )
                },
            )
        } else {
            artifact.plan.packed.try_append_remapped(
                editor,
                || checkpoint().map_err(DeclarationBodyPlanBuildFailure::Query),
                |ordinal| {
                    symbols_translated = symbols_translated.saturating_add(1);
                    local_symbols.get(ordinal as usize).copied().ok_or_else(|| {
                        DeclarationBodyPlanBuildFailure::Validation(Arc::from(
                            "candidate packed symbol ordinal is absent",
                        ))
                    })
                },
                |_slot, (relative_start, relative_end)| {
                    project_candidate_span(
                        module.file_id(),
                        declaration_start,
                        source_length,
                        relative_start,
                        relative_end,
                    )
                },
            )
        }
        .map_err(|error| match error {
            rue_rir::PackedRirAppendError::Checkpoint(failure)
            | rue_rir::PackedRirAppendError::SymbolRemap(failure)
            | rue_rir::PackedRirAppendError::SpanRemap { error: failure, .. } => failure,
            rue_rir::PackedRirAppendError::Build(error) => {
                DeclarationBodyPlanBuildFailure::Build(error)
            }
            rue_rir::PackedRirAppendError::Decode(error) => {
                DeclarationBodyPlanBuildFailure::Validation(Arc::from(format!(
                    "private candidate packed RIR is invalid: {error}"
                )))
            }
        })?;
        Ok((
            appended.range.instructions,
            appended.metadata.declaration,
            symbols_translated,
        ))
    };

    for recipe in module.definitions().rir_recipes() {
        checkpoint().map_err(DeclarationBodyPlanBuildFailure::Query)?;
        match recipe {
            crate::parsed_modules::ParsedRirRecipe::Single(key) => {
                let (instructions, declaration, symbols_translated) =
                    append(&mut editor, key, None, &mut checkpoint)?;
                work.symbol_fields_translated = work
                    .symbol_fields_translated
                    .saturating_add(symbols_translated);
                #[cfg(not(test))]
                let _ = (instructions, declaration);
                #[cfg(test)]
                declaration_roots.insert(
                    key.clone(),
                    ModuleDeclarationRoot {
                        declaration,
                        instructions,
                        owner: None,
                    },
                );
            }
            crate::parsed_modules::ParsedRirRecipe::Extern { functions } => {
                for key in functions.iter() {
                    let (instructions, declaration, symbols_translated) =
                        append(&mut editor, key, None, &mut checkpoint)?;
                    work.symbol_fields_translated = work
                        .symbol_fields_translated
                        .saturating_add(symbols_translated);
                    #[cfg(not(test))]
                    let _ = (instructions, declaration);
                    #[cfg(test)]
                    declaration_roots.insert(
                        key.clone(),
                        ModuleDeclarationRoot {
                            declaration,
                            instructions,
                            owner: None,
                        },
                    );
                }
            }
            crate::parsed_modules::ParsedRirRecipe::Struct { shell, methods } => {
                let mut method_roots = Vec::with_capacity(methods.len());
                for key in methods.iter() {
                    let (instructions, declaration, symbols_translated) =
                        append(&mut editor, key, None, &mut checkpoint)?;
                    work.symbol_fields_translated = work
                        .symbol_fields_translated
                        .saturating_add(symbols_translated);
                    #[cfg(not(test))]
                    let _ = instructions;
                    method_roots.push(declaration);
                    #[cfg(test)]
                    declaration_roots.insert(
                        key.clone(),
                        ModuleDeclarationRoot {
                            declaration,
                            instructions,
                            owner: None,
                        },
                    );
                }
                let (instructions, declaration, symbols_translated) =
                    append(&mut editor, shell, Some(&method_roots), &mut checkpoint)?;
                work.symbol_fields_translated = work
                    .symbol_fields_translated
                    .saturating_add(symbols_translated);
                #[cfg(not(test))]
                let _ = (instructions, declaration);
                #[cfg(test)]
                declaration_roots.insert(
                    shell.clone(),
                    ModuleDeclarationRoot {
                        declaration,
                        instructions,
                        owner: None,
                    },
                );
                #[cfg(test)]
                for key in methods.iter() {
                    declaration_roots
                        .get_mut(key)
                        .expect("method root was inserted before its shell")
                        .owner = Some(declaration);
                }
            }
        }
    }
    let rir = ValidatedRir::finish(
        editor,
        &RirValidationContext {
            symbol_count: symbols.interner().len(),
            source_lengths: &[(module.file_id(), source_length)],
        },
    )
    .map_err(|error| {
        DeclarationBodyPlanBuildFailure::Validation(Arc::from(format!(
            "composed module RIR is invalid: {error}"
        )))
    })?;
    work.instructions_appended = rir.len();
    work.payload_words_appended = rir.extra_len();
    work.semantic_intern_attempts = work.symbol_fields_translated;
    work.semantic_strings_retained = symbols.interner().len();
    Ok(CandidateModuleRirOutput {
        revision: module.revision().clone(),
        source_length,
        rir,
        symbols,
        work,
        #[cfg(test)]
        declaration_roots,
    })
}

#[cfg(test)]
fn lower_module_rir_with_work_internal(
    module: std::sync::Arc<crate::parsed_modules::ParsedModule>,
    authoritative_anchors: Option<
        &[(
            rue_span::Span,
            rue_rir::AnonymousTypeSiteKind,
            rue_rir::RirStructuralAnchor,
        )],
    >,
    cancellation_check: Option<&mut dyn FnMut() -> bool>,
) -> Result<CandidateModuleRirOutput, (CompileError, CanonicalRirWork)> {
    let symbols = SemanticSymbolUniverse::from_modules(std::slice::from_ref(&module))
        .expect("test module symbol universe must fit the published interner bound");
    let view = crate::parsed_modules::ParsedAstView::from_module(module.clone());
    let first_error = RefCell::<Option<CompileError>>::new(None);
    let mut work = CanonicalRirWork {
        modules_visited: 1,
        items_visited: module.ast().items.len(),
        ..CanonicalRirWork::default()
    };
    let (editor, item_roots) = {
        let mut generator =
            AstGen::with_symbol_normalizer(symbols.interner(), |local| {
                match symbols.translate_ast_symbol(&view, local) {
                    Ok(symbol) => symbol.spur(),
                    Err(error) => {
                        let mut slot = first_error.borrow_mut();
                        if slot.is_none() {
                            *slot = Some(error);
                        }
                        rue_lexer::try_intern(symbols.interner(), "__rue_invalid_local_symbol")
                            .expect("test symbol universe must fit the published interner bound")
                    }
                }
            });
        if let Some(checkpoint) = cancellation_check {
            generator.install_cancellation_check(checkpoint);
        }
        if let Some(anchors) = authoritative_anchors {
            generator
                .install_authoritative_anonymous_anchors(anchors.iter().cloned())
                .map_err(|error| {
                    (
                        CompileError::new(
                            ErrorKind::InternalError(format!(
                                "authoritative anonymous-anchor transport failed: {error}"
                            )),
                            rue_span::Span::new(0, 0),
                        ),
                        work,
                    )
                })?;
        }
        let item_roots = generator.append_items_with_roots(&module.ast().items);
        if let Some(error) = first_error.borrow_mut().take() {
            return Err((error, work));
        }
        let editor = generator.try_finish_editor().map_err(|error| {
            (
                CompileError::new(
                    rir_build_error_kind("RIR module payload construction failed", &error),
                    rue_span::Span::new(0, 0),
                ),
                work,
            )
        })?;
        (editor, item_roots)
    };
    let source_length = u32::try_from(module.source_text().len()).map_err(|_| {
        (
            CompileError::new(
                ErrorKind::InternalError(
                    "canonical module source length exceeds RIR span capacity".to_string(),
                ),
                rue_span::Span::new(0, 0),
            ),
            work,
        )
    })?;
    let source_lengths = [(module.file_id(), source_length)];
    let validation = RirValidationContext {
        symbol_count: symbols.interner().len(),
        source_lengths: &source_lengths,
    };
    let rir = ValidatedRir::finish(editor, &validation).map_err(|error| {
        (
            CompileError::new(
                ErrorKind::InternalError(format!("RIR module payload validation failed: {error}")),
                rue_span::Span::new(0, 0),
            ),
            work,
        )
    })?;
    let declaration_roots =
        candidate_root_index(&module, &rir, &symbols, &item_roots).map_err(|reason| {
            (
                CompileError::new(
                    ErrorKind::InternalError(reason.to_owned()),
                    rue_span::Span::new(0, 0),
                ),
                work,
            )
        })?;
    let translation = symbols.work();
    work.symbol_fields_translated = translation.local_symbol_resolutions;
    work.semantic_intern_attempts = translation.semantic_intern_attempts;
    work.unique_semantic_strings = translation.unique_semantic_strings;
    work.semantic_strings_retained = symbols.interner().len();
    Ok(CandidateModuleRirOutput {
        revision: module.revision().clone(),
        source_length,
        rir,
        symbols,
        work,
        #[cfg(test)]
        declaration_roots,
    })
}

/// Assemble the deterministic whole-program compatibility view from canonical
/// module lowering terminals. This projection never traverses parser AST.
pub(crate) fn project_candidate_module_rirs_with_work(
    merged: &CanonicalMergedProgram,
    modules: &[std::sync::Arc<CandidateModuleRirOutput>],
    query_work: CanonicalRirWork,
    max_interner_entries: usize,
) -> Result<CanonicalRirOutput, (CompileError, CanonicalRirWork)> {
    let ast = merged.ast();
    if modules.len() != ast.modules().len()
        || modules
            .iter()
            .zip(ast.modules())
            .any(|(lowered, parsed)| lowered.revision() != parsed.revision())
    {
        return Err((
            CompileError::new(
                ErrorKind::InternalError(
                    "module RIR terminals do not match the canonical parsed projection".to_string(),
                ),
                rue_span::Span::new(0, 0),
            ),
            query_work,
        ));
    }
    let symbols =
        SemanticSymbolUniverse::from_modules_with_limit(ast.modules(), max_interner_entries)
            .map_err(|error| (interner_resource_error(error.0), query_work))?;
    // `append_remapped_with_spans` deliberately exposes an infallible symbol
    // callback, so consume every module symbol through the bounded interner
    // before entering that append boundary.  This keeps exhaustion typed and
    // leaves the callback as a pure lookup over the proven complete map.
    for lowered in modules {
        for (_, spelling) in lowered.symbols.interner().iter() {
            if symbols.interner().get(spelling).is_none()
                && symbols.interner().len() >= max_interner_entries
            {
                return Err((
                    interner_resource_error(lasso::LassoErrorKind::KeySpaceExhaustion),
                    query_work,
                ));
            }
            rue_lexer::try_intern(symbols.interner(), spelling)
                .map_err(|kind| (interner_resource_error(kind), query_work))?;
        }
    }
    let mut editor = RirEditor::new();
    let mut module_ranges = Vec::with_capacity(modules.len());
    let mut work = query_work;
    for (lowered, parsed) in modules.iter().zip(ast.modules()) {
        let appended = editor
            .append_remapped_with_spans(
                &lowered.rir,
                |local| {
                    let text = lowered
                        .symbols
                        .interner()
                        .try_resolve(&local)
                        .expect("validated module RIR symbol belongs to its module universe");
                    symbols
                        .interner()
                        .get(text)
                        .expect("module projection pre-interned every source symbol")
                },
                |span| rue_span::Span::with_file(parsed.file_id(), span.start, span.end),
            )
            .map_err(|error| {
                (
                    CompileError::new(
                        rir_build_error_kind("RIR module projection failed", &error),
                        rue_span::Span::new(0, 0),
                    ),
                    work,
                )
            })?;
        work.modules_projected += 1;
        work.instructions_appended += appended.instructions.len();
        work.payload_words_appended += appended.extra.len();
        module_ranges.push(CanonicalRirModuleRange {
            file_id: parsed.file_id(),
            instructions: appended.instructions,
            extra: appended.extra,
        });
    }
    let sources = modules
        .iter()
        .zip(ast.modules())
        .map(|(module, parsed)| CanonicalRirSource {
            file_id: parsed.file_id(),
            revision: module.revision.clone(),
            length: module.source_length,
        })
        .collect::<Vec<_>>();
    let source_lengths = sources
        .iter()
        .map(|source| (source.file_id, source.length))
        .collect::<Vec<_>>();
    let validation = RirValidationContext {
        symbol_count: symbols.interner().len(),
        source_lengths: &source_lengths,
    };
    let rir = ValidatedRir::finish(editor, &validation).map_err(|error| {
        (
            CompileError::new(
                ErrorKind::InternalError(format!("RIR payload validation failed: {error}")),
                rue_span::Span::new(0, 0),
            ),
            work,
        )
    })?;
    work.semantic_strings_retained = symbols.interner().len();
    Ok(CanonicalRirOutput {
        source_revision: ast.source_revision().clone(),
        rir,
        symbols,
        work,
        module_ranges,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rue_rir::RirPrinter;
    use rue_span::FileId;

    use super::*;
    use crate::parsed_modules::{ParsedProgram, parse_source_snapshot_modules};
    use crate::{SourceMetadata, SourceSnapshot};

    #[test]
    fn rir_capacity_rejections_are_resource_limits_not_internal_errors() {
        // Spec C.1:2 / RUE-1221: a program too large for the u32 instruction
        // array or the u32-indexed payload word store is a diagnosable
        // compile-time failure (E1401) naming the limit, not an ICE.
        let limit = rir_build_error_kind(
            "ctx",
            &RirPayloadBuildError::ResourceLimitExceeded {
                family: "payload words",
            },
        );
        assert_eq!(limit.code(), rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT);
        assert!(limit.to_string().contains("payload words"));
        assert!(limit.to_string().contains("4294967295"));
        assert!(!limit.to_string().contains("internal compiler"));

        assert_eq!(
            rir_build_error_kind(
                "ctx",
                &RirPayloadBuildError::CapacityFailure {
                    family: "call args"
                },
            )
            .code(),
            rue_error::ErrorCode::COMPILER_RESOURCE_EXHAUSTION
        );
        assert_eq!(
            rir_build_error_kind(
                "ctx",
                &RirPayloadBuildError::InternerFailure {
                    family: "interned strings",
                    kind: lasso::LassoErrorKind::FailedAllocation,
                },
            )
            .code(),
            rue_error::ErrorCode::COMPILER_RESOURCE_EXHAUSTION
        );
        assert_eq!(
            rir_build_error_kind(
                "ctx",
                &RirPayloadBuildError::InvalidBuilderInput {
                    family: "call args",
                    reason: "bad request",
                },
            )
            .code(),
            rue_error::ErrorCode::INTERNAL_ERROR
        );
    }

    #[test]
    fn canonical_merge_interning_exhaustion_is_a_resource_diagnostic() {
        // The bound is injected into the session-owned revision symbol space,
        // so canonical materialization and its worker queries observe it
        // without process- or thread-local mutation.
        let snapshot = snapshot(
            &[
                (1, "/first.rue", "first.rue", "fn first() {}"),
                (2, "/second.rue", "second.rue", "fn second() {}"),
            ],
            1,
        );
        let mut session = crate::CompilerSession::with_interner_limit(20);
        session.update(&snapshot).into_result().unwrap();
        let errors = session.canonical_rir().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.kind.code() == rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT),
            "canonical merge must report E1401, got {errors:?}"
        );
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn canonical_output_is_send_and_sync() {
        assert_send_sync::<SemanticSymbolUniverse>();
        assert_send_sync::<CanonicalRirOutput>();
    }

    #[test]
    fn candidate_artifact_composition_matches_module_astgen_for_all_declaration_categories() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let source = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                r#"
@copy
pub struct Box {
    value: i32,
    fn get(borrow self, comptime T: type) -> i32 { self.value }
    fn make(value: i32) -> Box { Box { value: value } }
}
enum Choice { A, B(i32) }
const selected: i32 = 1;
drop fn Box(self) {}
unchecked fn run(value: i32) -> i32 { value }
extern "C" { fn getpid() -> i32; }
"#,
            )],
            1,
        );
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(&source).unwrap();
        let module = parsed.modules()[0].clone();
        let old = lower_module_rir_with_work(module.clone()).unwrap();
        let mut artifacts = AHashMap::new();
        let mut categories = Vec::new();
        for key in module.definitions().declaration_keys_in_source_order() {
            categories.push(key.category);
            let artifact = lower_parsed_declaration_body_plan(&module, key, || Ok(())).unwrap();
            artifacts.insert(key.clone(), Arc::new(artifact));
        }
        let composed =
            compose_module_rir_from_candidate_artifacts(module, &artifacts, || Ok(())).unwrap();
        categories.sort_unstable();
        categories.dedup();
        for expected in [
            Category::Function,
            Category::Struct,
            Category::Enum,
            Category::ConstCandidate,
            Category::Destructor,
            Category::Method,
            Category::AssociatedFunction,
            Category::ExternFunction,
        ] {
            assert!(categories.contains(&expected), "missing {expected:?}");
        }
        assert_eq!(
            RirPrinter::new(&composed.rir, composed.symbols.interner()).to_string(),
            RirPrinter::new(&old.rir, old.symbols.interner()).to_string()
        );
        assert_eq!(composed.declaration_roots, old.declaration_roots);
    }

    #[test]
    fn candidate_composition_is_parity_safe_with_local_counters_and_anonymous_types() {
        let source = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                r#"
fn index() -> u64 { 1 }
fn before(inout values: [i32; 4]) {
    for _ in [1, 2] {}
    values[index()] += 1;
}
fn target(inout values: [i32; 4]) -> type {
    for _ in [1, 2] {}
    values[index()] += 1;
    struct { value: i32, fn nested(self) -> i32 { self.value } }
}
"#,
            )],
            1,
        );
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(&source).unwrap();
        let module = parsed.modules()[0].clone();
        let old = lower_module_rir_with_work(module.clone()).unwrap();
        let artifacts = module
            .definitions()
            .declaration_keys_in_source_order()
            .map(|key| {
                let artifact = lower_parsed_declaration_body_plan(&module, key, || Ok(())).unwrap();
                (key.clone(), Arc::new(artifact))
            })
            .collect::<AHashMap<_, _>>();
        let target_key = artifacts
            .keys()
            .find(|key| key.name.as_ref() == "target")
            .unwrap();
        let indexed_anchors = module
            .declaration_anonymous_sites(target_key)
            .unwrap()
            .iter()
            .map(|site| site.anchor.clone())
            .collect::<Vec<_>>();
        let target_artifact = artifacts.get(target_key).unwrap();
        let declaration_start = module
            .definitions()
            .declaration_locator(target_key)
            .unwrap()
            .declaration_span
            .start;
        let target_rir = target_artifact
            .plan
            .materialize_candidate_rir(
                &rue_rir::SharedSymbolSpace::private(),
                module.file_id(),
                declaration_start,
                module.source_text().len() as u32,
                || Ok(()),
            )
            .unwrap();
        let emitted_anchors = target_rir
            .iter()
            .filter_map(|(_, instruction)| match &instruction.data {
                rue_rir::InstData::AnonStructType { anchor, .. }
                | rue_rir::InstData::AnonEnumType { anchor, .. } => Some(anchor.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(emitted_anchors, indexed_anchors);

        let mut corrupted_index = module
            .declaration_anonymous_sites(target_key)
            .unwrap()
            .to_vec();
        assert_eq!(corrupted_index.len(), 1);
        corrupted_index[0].kind = rue_rir::AnonymousTypeSiteKind::Enum;
        assert!(matches!(
            lower_parsed_declaration_body_plan_internal(
                &module,
                target_key,
                &corrupted_index,
                || Ok(()),
            ),
            Err(DeclarationBodyPlanBuildFailure::Payload(_))
        ));

        let composed =
            compose_module_rir_from_candidate_artifacts(module, &artifacts, || Ok(())).unwrap();
        let old = RirPrinter::new(&old.rir, old.symbols.interner()).to_string();
        let composed = RirPrinter::new(&composed.rir, composed.symbols.interner()).to_string();
        assert_eq!(old, composed);
    }

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<AHashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<AHashMap<_, _>>();
        let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
        SourceSnapshot::new(
            metadata,
            entries
                .iter()
                .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    fn print(output: &CanonicalRirOutput) -> String {
        RirPrinter::new(output.rir(), output.symbols.interner()).to_string()
    }

    fn print_in_snapshot_order(output: &CanonicalRirOutput, source: &SourceSnapshot) -> String {
        let order = output.presentation_order(source.files().map(|file| file.file_id));
        RirPrinter::with_presentation_order(
            output.rir(),
            output.symbols.interner(),
            order.instructions,
            order.extra,
        )
        .to_string()
    }

    #[test]
    fn equal_local_spurs_lower_to_distinct_semantic_names() {
        let source = snapshot(
            &[
                (1, "/a.rue", "a.rue", "fn alpha() {}"),
                (2, "/b.rue", "b.rue", "fn beta() {}"),
            ],
            1,
        );
        let stages = crate::test_support::test_frontend_stages(&source).unwrap();
        let merged = &stages.merged;
        let output = &stages.rir;
        let rendered = print(output);

        assert!(rendered.contains("alpha"));
        assert!(rendered.contains("beta"));
        assert_eq!(output.source_revision(), merged.ast().source_revision());
        assert_eq!(output.work().modules_visited, 2);
        assert_eq!(output.work().items_visited, 2);
        assert_eq!(output.work().parser_invocations, 0);
        assert_eq!(output.work().ast_payload_clones, 0);
        assert_eq!(output.work().source_text_clones, 0);
    }

    #[test]
    fn module_lowering_failure_retains_completed_work() {
        let source = snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }")], 1);
        let parsed = parse_source_snapshot_modules(&source).unwrap();
        let faulty = parsed.modules()[0].with_test_foreign_ast_symbol();
        let (error, work) = lower_module_rir_with_work(faulty).unwrap_err();
        assert!(error.to_string().contains("AST symbol is absent"));
        assert_eq!(work.modules_visited, 1);
        assert_eq!(work.items_visited, 1);
        assert_eq!(work.modules_projected, 0);
    }

    fn candidate_named(
        module: &crate::parsed_modules::ParsedModule,
        name: &str,
    ) -> crate::declaration_candidate::DeclarationCandidateKey {
        module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|key| key.name.as_ref() == name)
            .expect("test declaration candidate is indexed")
            .clone()
    }

    #[test]
    fn declaration_body_plan_ignores_sibling_vocabulary_but_refreshes_current_basis() {
        let first = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn chosen() -> i32 { 1 + 2 }\nfn old_sibling() -> i32 { 3 }",
            )],
            1,
        );
        let second = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "\n\nfn chosen() -> i32 { 1 + 2 }\nfn completely_new_vocabulary() -> i32 { 99 }",
            )],
            1,
        );
        let first_module = parse_source_snapshot_modules(&first).unwrap().modules()[0].clone();
        let second_module = parse_source_snapshot_modules(&second).unwrap().modules()[0].clone();
        let key = candidate_named(&first_module, "chosen");
        assert_eq!(key, candidate_named(&second_module, "chosen"));
        let first_plan =
            lower_parsed_declaration_body_plan(&first_module, &key, || Ok(())).unwrap();
        let second_plan =
            lower_parsed_declaration_body_plan(&second_module, &key, || Ok(())).unwrap();

        assert!(first_plan.plan.structurally_eq(&second_plan.plan));
        assert!(
            first_plan
                .plan
                .packed
                .symbols()
                .all(|symbol| symbol != "old_sibling")
        );
        assert!(
            second_plan
                .plan
                .packed
                .symbols()
                .all(|symbol| symbol != "completely_new_vocabulary")
        );
    }

    #[test]
    fn declaration_body_plan_basis_tracks_in_declaration_coordinate_changes() {
        let source = |body| {
            snapshot(
                &[(
                    1,
                    "/main.rue",
                    "main.rue",
                    &format!("fn chosen() -> i32 {{ {body} }}"),
                )],
                1,
            )
        };
        let first = parse_source_snapshot_modules(&source("1 + 2"))
            .unwrap()
            .modules()[0]
            .clone();
        let second = parse_source_snapshot_modules(&source("(1  +  2)"))
            .unwrap()
            .modules()[0]
            .clone();
        let key = candidate_named(&first, "chosen");
        assert_eq!(key, candidate_named(&second, "chosen"));
        let first = lower_parsed_declaration_body_plan(&first, &key, || Ok(())).unwrap();
        let second = lower_parsed_declaration_body_plan(&second, &key, || Ok(())).unwrap();

        assert!(!first.plan.structurally_eq(&second.plan));
    }

    #[test]
    fn declaration_body_plan_charge_is_exactly_one_packed_envelope() {
        let source = snapshot(
            &[(1, "/main.rue", "main.rue", "fn selected() -> i32 { 1 + 2 }")],
            1,
        );
        let module = parse_source_snapshot_modules(&source).unwrap().modules()[0].clone();
        let key = candidate_named(&module, "selected");
        let artifact =
            Arc::new(lower_parsed_declaration_body_plan(&module, &key, || Ok(())).unwrap());
        assert_eq!(
            artifact.retained_charge(),
            std::mem::size_of::<DeclarationBodyPlanArtifacts>() as u64
                + artifact.plan.packed.retained_allocation_charge(),
        );
    }

    /// ADR-0076: two bodies of one revision share the equality space, and the
    /// space a body is materialized into does not change the program it names.
    ///
    /// The second candidate is materialized into a space the first has already
    /// populated, so its handles are offset out of the dense ordinals the
    /// packed envelope encodes. Both renders match the private-space render,
    /// and the shared space holds each spelling once rather than once per body.
    #[test]
    fn bodies_of_one_revision_share_one_equality_space() {
        let source = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn first() -> i32 { 1 + 2 }\nfn second() -> i32 { 3 + 4 }",
            )],
            1,
        );
        let module = parse_source_snapshot_modules(&source).unwrap().modules()[0].clone();
        let source_length = module.source_text().len() as u32;
        let render = |name: &str, space: &rue_rir::SharedSymbolSpace| {
            let key = candidate_named(&module, name);
            let declaration_start = module
                .definitions()
                .declaration_locator(&key)
                .unwrap()
                .declaration_span
                .start;
            let artifacts = lower_parsed_declaration_body_plan(&module, &key, || Ok(())).unwrap();
            let rir = artifacts
                .plan
                .materialize_candidate_rir(
                    space,
                    module.file_id(),
                    declaration_start,
                    source_length,
                    || Ok(()),
                )
                .unwrap();
            rue_rir::RirPrinter::new(&rir, space.interner()).to_string()
        };

        let shared = rue_rir::SharedSymbolSpace::private();
        let first_shared = render("first", &shared);
        let interned_after_first = shared.interner().len();
        let second_shared = render("second", &shared);

        assert_eq!(
            first_shared,
            render("first", &rue_rir::SharedSymbolSpace::private())
        );
        assert_eq!(
            second_shared,
            render("second", &rue_rir::SharedSymbolSpace::private())
        );
        assert!(
            shared.interner().len() < interned_after_first * 2,
            "the second body reused the first body's interned spellings"
        );
        // The dense ordinals the second body's packed envelope encodes are no
        // longer its handles: the shared space had already issued them.
        assert!(shared.interner().get("second").is_some());
        assert!(
            shared
                .interner()
                .get("second")
                .is_some_and(|symbol| symbol.into_usize() >= interned_after_first)
        );
    }

    #[test]
    fn body_plan_materialization_reprojects_fails_closed_and_retries() {
        let source = snapshot(
            &[(1, "/main.rue", "main.rue", "fn selected() -> i32 { 1 + 2 }")],
            1,
        );
        let module = parse_source_snapshot_modules(&source).unwrap().modules()[0].clone();
        let key = candidate_named(&module, "selected");
        let file_id = module.file_id();
        let declaration_start = module
            .definitions()
            .declaration_locator(&key)
            .unwrap()
            .declaration_span
            .start;
        let artifacts = lower_parsed_declaration_body_plan(&module, &key, || Ok(())).unwrap();
        let source_length = module.source_text().len() as u32;
        let materialized = artifacts
            .plan
            .materialize_candidate_rir(
                &rue_rir::SharedSymbolSpace::private(),
                file_id,
                declaration_start,
                source_length,
                || Ok(()),
            )
            .unwrap();
        assert!(
            materialized
                .iter()
                .all(|(_, instruction)| instruction.span.file_id == file_id)
        );
        assert!(matches!(
            artifacts.plan.materialize_body_rir_bundle(
                &rue_rir::SharedSymbolSpace::private(),
                file_id,
                declaration_start,
                1,
                || Ok(()),
            ),
            Err(BodyPlanMaterializationFailure::Invalid(_))
        ));

        assert!(matches!(
            artifacts.plan.materialize_body_rir_bundle(
                &rue_rir::SharedSymbolSpace::private(),
                file_id,
                declaration_start,
                source_length,
                || Err(rue_query::QueryAbort::Canceled),
            ),
            Err(BodyPlanMaterializationFailure::Query(
                rue_query::QueryAbort::Canceled
            ))
        ));
        assert!(
            artifacts
                .plan
                .materialize_body_rir_bundle(
                    &rue_rir::SharedSymbolSpace::private(),
                    file_id,
                    declaration_start,
                    source_length,
                    || Ok(()),
                )
                .is_ok()
        );
    }

    #[test]
    fn middle_method_plan_retains_only_its_exact_owner_edge() {
        let source = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "struct S { fn first(borrow self) {} fn middle(borrow self) {} fn last(borrow self) {} }",
            )],
            1,
        );
        let module = parse_source_snapshot_modules(&source).unwrap().modules()[0].clone();
        let key = candidate_named(&module, "middle");
        let method_span = module
            .definitions()
            .declaration_locator(&key)
            .expect("middle method has an exact parser locator")
            .declaration_span;
        let artifacts = lower_parsed_declaration_body_plan(&module, &key, || Ok(())).unwrap();
        let spellings = artifacts.plan.packed.symbols().collect::<Vec<_>>();

        assert!(spellings.contains(&"S"));
        assert!(spellings.contains(&"middle"));
        assert!(!spellings.contains(&"first"));
        assert!(!spellings.contains(&"last"));
        let materialized = artifacts
            .plan
            .materialize_candidate_rir(
                &rue_rir::SharedSymbolSpace::private(),
                module.file_id(),
                method_span.start,
                module.source_text().len() as u32,
                || Ok(()),
            )
            .unwrap();
        let owner_count = materialized
            .iter()
            .filter(|(_, instruction)| {
                matches!(instruction.data, rue_rir::InstData::StructDecl { .. })
            })
            .count();
        assert_eq!(owner_count, 1);

        let (owner_ref, method_ref) = materialized
            .iter()
            .find_map(|(owner_ref, instruction)| {
                let rue_rir::InstData::StructDecl { methods, .. } = &instruction.data else {
                    return None;
                };
                Some((
                    owner_ref,
                    *materialized
                        .struct_methods(methods)
                        .iter()
                        .next()
                        .expect("synthetic owner retains the selected method"),
                ))
            })
            .expect("plan retains one synthetic owner edge");
        let owner_span = materialized.get(owner_ref).span;
        let selected_span = materialized.get(method_ref).span;
        assert_eq!(owner_span, selected_span);
        assert!(method_span.start <= owner_span.start && owner_span.end <= method_span.end);
    }

    #[test]
    fn module_rir_lowering_cancellation_aborts_and_retry_completes() {
        let statements = (0..4_096)
            .map(|index| format!("let value_{index} = {index};"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = format!("fn main() -> i32 {{ {statements}\n0 }}");
        let source = snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
        let module = parse_source_snapshot_modules(&source).unwrap().modules()[0].clone();
        let mut checkpoints = 0_usize;
        let canceled = lower_module_rir_with_work_and_checkpoint(module.clone(), || {
            checkpoints += 1;
            if checkpoints == 16 {
                Err(rue_query::QueryAbort::Canceled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(canceled, Err(rue_query::QueryAbort::Canceled)));
        assert_eq!(checkpoints, 16);

        let retried = lower_module_rir_with_work_and_checkpoint(module, || Ok(()))
            .unwrap()
            .unwrap();
        assert!(retried.rir.len() > checkpoints);
    }

    #[test]
    fn projection_failure_preserves_incoming_query_work() {
        let source = snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }")], 1);
        let merged = crate::test_support::test_merged_program(&source).unwrap();
        let query_work = CanonicalRirWork {
            modules_visited: 1,
            items_visited: 1,
            ..CanonicalRirWork::default()
        };
        let (_, failure_work) = project_candidate_module_rirs_with_work(
            &merged,
            &[],
            query_work,
            rue_lexer::MAX_INTERNED_STRINGS,
        )
        .unwrap_err();
        assert_eq!(failure_work, query_work);
    }

    /// Lowering consumes whatever module order [`ParsedProgram`] publishes, and
    /// that order is canonical regardless of assembly order — so a reversed
    /// module vector cannot reach lowering as a different program.
    #[test]
    fn reordered_arc_assembly_publishes_one_canonical_module_order() {
        let source = snapshot(
            &[
                (8, "/z.rue", "z.rue", "fn zed() {}"),
                (3, "/a.rue", "a.rue", "fn alpha() {}"),
            ],
            8,
        );
        let first = parse_source_snapshot_modules(&source).unwrap();
        let mut modules = first.modules().to_vec();
        modules.reverse();
        let second = ParsedProgram::new(first.root().clone(), modules).unwrap();

        assert!(
            first
                .modules()
                .iter()
                .zip(second.modules())
                .all(|(left, right)| Arc::ptr_eq(left, right))
        );
        let rir = crate::test_support::test_canonical_rir(&source).unwrap();
        assert_eq!(rir.source_revision(), second.source_revision());
    }

    #[test]
    fn caller_order_presentation_differs_from_canonical_semantic_order() {
        let source = snapshot(
            &[
                (
                    8,
                    "/checkout/z.rue",
                    "z.rue",
                    "fn zed() -> i32 { let z = 40; z + 2 }",
                ),
                (
                    3,
                    "/checkout/a.rue",
                    "a.rue",
                    "fn alpha() -> i32 { let a = 1; a }",
                ),
            ],
            8,
        );
        let canonical = crate::test_support::test_canonical_rir(&source).unwrap();
        let semantic = print(&canonical);
        let presentation = print_in_snapshot_order(&canonical, &source);
        assert!(semantic.find("alpha").unwrap() < semantic.find("zed").unwrap());
        assert!(presentation.find("zed").unwrap() < presentation.find("alpha").unwrap());
        assert_eq!(canonical.work().parser_invocations, 0);
    }

    #[test]
    fn presentation_survives_file_id_and_physical_path_relocation() {
        let first = snapshot(
            &[
                (9, "/old/z.rue", "z.rue", "fn zed() -> i32 { 9 }"),
                (2, "/old/a.rue", "a.rue", "fn alpha() -> i32 { 2 }"),
            ],
            9,
        );
        let relocated = snapshot(
            &[
                (91, "/new/z.rue", "z.rue", "fn zed() -> i32 { 9 }"),
                (27, "/new/a.rue", "a.rue", "fn alpha() -> i32 { 2 }"),
            ],
            91,
        );
        let lower =
            |source: &SourceSnapshot| crate::test_support::test_canonical_rir(source).unwrap();
        let first_rir = lower(&first);
        let relocated_rir = lower(&relocated);

        assert_eq!(
            print_in_snapshot_order(&first_rir, &first),
            print_in_snapshot_order(&relocated_rir, &relocated)
        );
    }

    #[test]
    fn adversarial_symbol_surfaces_are_translated() {
        let source = snapshot(
            &[
                (
                    1,
                    "/seed.rue",
                    "a-seed.rue",
                    "fn seed() { let displaced = 0; }",
                ),
                (
                    2,
                    "/symbols.rue",
                    "b-symbols.rue",
                    r#"
                        struct Resource {
                            value: i32,
                            fn set(self, next: i32) { self.value = next; }
                            fn make() -> Resource { Resource { value: 0 } }
                        }
                        enum Choice { None, Some(i32) }
                        const imported = @import("other.rue");
                        const LENGTH: u64 = 2;
                        drop fn Resource(self) { () }

                        @allow(unused_function)
                        fn exercise(values: [i32; LENGTH], text: StrBuf) -> i32 {
                            let mut resource = Resource.make();
                            resource.set(1);
                            resource.value = 2;
                            let field = resource.value;
                            let choice = Choice.Some(field);
                            let payload = match choice {
                                Choice.Some(inner) => inner,
                                Choice.None => 0,
                            };
                            let _ = "symbolic text";
                            for element in values { resource.value = element; }
                            for byte in text { resource.value = byte; }
                            @dbg(payload);
                            @sizeOf([i32; LENGTH]);
                            payload
                        }

                        fn TypeFactory(comptime T: type) -> type {
                            struct {
                                member: T,
                                fn get(self) -> T { self.member }
                            }
                        }
                    "#,
                ),
                (
                    3,
                    "/other.rue",
                    "c-other.rue",
                    "fn imported_name() -> i32 { 1 }",
                ),
            ],
            2,
        );
        let canonical = crate::test_support::test_canonical_rir(&source).unwrap();
        let rendered = print(&canonical);

        assert!(canonical.work().symbol_fields_translated > 40);
        assert!(
            canonical
                .semantic_symbols()
                .interner()
                .get("unused_function")
                .is_some(),
            "directive arguments must resolve in the destination universe"
        );
        for expected in [
            "Resource",
            "Some",
            "inner",
            "LENGTH",
            "symbolic text",
            "element",
            "member",
        ] {
            assert!(
                rendered.contains(expected),
                "missing normalized `{expected}`"
            );
        }
    }
}
