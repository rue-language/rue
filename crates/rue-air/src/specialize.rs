//! Generic function specialization pass.
//!
//! This module provides the specialization pass that transforms `CallGeneric`
//! instructions into regular `Call` instructions by:
//!
//! 1. Collecting all `CallGeneric` instructions in the analyzed functions
//! 2. For each unique (func_name, type_args, value_args) combination, creating
//!    a specialized function
//! 3. Rewriting `CallGeneric` to `Call` with the specialized function name
//!
//! Specialization covers both comptime *type* parameters (`comptime T: type`)
//! and comptime *value* parameters (`comptime n: i32`, RUE-166): each distinct
//! combination of type and value arguments gets its own specialized body, in
//! which the value parameters are compile-time constants (available to
//! `comptime` blocks and forwardable to other comptime parameters).
//!
//! Specialized bodies can contain further `CallGeneric` instructions (a generic
//! function forwarding its type parameter to another generic, or a
//! comptime-recursive function like `fact(comptime n)` calling `fact(n - 1)`),
//! so these steps repeat until no new specializations are discovered.
//!
//! # Architecture
//!
//! Specialization participates in semantic analysis's reachability fixed point:
//! it transforms generic calls in-place, appends specialized bodies, and returns
//! any ordinary calls those bodies discover to the source-body worklist. String
//! tables and the type pool are finalized only after that joint fixed point.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;

use lasso::{Key, Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind, WarningKind};
use rue_rir::RirParamMode;
use rue_span::Span;

use crate::inst::{Air, AirInstData};
use crate::sema::{
    AnalyzedFunction, ConstValue, FunctionInfo, InferenceContext, OrdinaryBodyAnalysisHost,
    OrdinaryBodyEngine, SemanticBodyExportHost,
};
use crate::types::{StructId, Type};

/// A key for a specialized function:
/// (base_function_name, type_arguments, value_arguments).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpecializationKey {
    /// Base function name (e.g., "identity")
    pub base_name: Spur,
    /// Type arguments (e.g., [Type::I32]), one per comptime type parameter
    pub type_args: Vec<Type>,
    /// Comptime value arguments (e.g., [Integer(5)]), one per comptime value
    /// parameter (RUE-166)
    pub value_args: Vec<ConstValue>,
}

/// Info about a specialization: the mangled name and the first call site span.
struct SpecializationInfo {
    /// The mangled name for the specialized function.
    mangled_name: Spur,
    /// The span of the first call site (for error reporting if the function doesn't exist).
    call_site_span: Span,
}

/// Source-level references discovered while analyzing specialized bodies.
#[derive(Default)]
pub(crate) struct DiscoveredReferences {
    pub(crate) functions: HashSet<Spur>,
    pub(crate) methods: HashSet<(StructId, Spur)>,
}

/// Persistent specialization state shared with the demand-driven sema worklist.
///
/// Source bodies discovered after an earlier specialization wave may request
/// either new or already-created specializations. Retaining both the request
/// map and scan frontier makes those later waves incremental and deduplicated.
#[derive(Default)]
pub(crate) struct Specializer {
    specializations: HashMap<SpecializationKey, SpecializationInfo>,
    next_unscanned: usize,
    /// Warnings already surfaced from specialized bodies, keyed by kind
    /// discriminant + source span, so N specializations of one generic body
    /// emit each warning once (RUE-555, RUE-574).
    seen_warnings: HashSet<(std::mem::Discriminant<WarningKind>, Span)>,
    /// Expansion waves consumed across the entire joint sema fixed point.
    rounds: usize,
    completed_exports: Vec<CompletedExport>,
    next_unexported: usize,
}

struct CompletedExport {
    key: SpecializationKey,
    base_info: FunctionInfo,
    function_index: usize,
    warning_free: bool,
    dependencies: Vec<crate::SemanticDefinitionToken>,
    dependency_boundary_complete: bool,
    /// The body's recorded `(receiver, method)` references, retained for the
    /// durable export payload after reachability consumes the discovered set.
    method_references: HashSet<(StructId, Spur)>,
}

struct SpecializedBody {
    function: AnalyzedFunction,
    warnings: Vec<CompileWarning>,
    local_strings: Vec<String>,
    referenced_functions: HashSet<Spur>,
    referenced_methods: HashSet<(StructId, Spur)>,
    dependencies: Vec<crate::SemanticDefinitionToken>,
    dependency_boundary_complete: bool,
}

/// Maximum number of specialization rounds before giving up.
///
/// Each round creates the bodies for the specializations discovered in the
/// previous round. The budget persists when specialization temporarily yields
/// to sema's ordinary function/method worklists: otherwise a chain that
/// alternates between a specialization and a newly registered method can reset
/// the counter forever (RUE-580).
///
/// This is deliberately a conservative, program-wide expansion budget rather
/// than exact causal-depth tracking. Independent branches can share the budget,
/// but that tradeoff keeps every form of generic/comptime expansion bounded —
/// including comptime-value recursion (`fact(comptime n)` calling
/// `fact(n - 1)`, RUE-166) and ever-growing type instantiation such as
/// `f(Pair(T), ...)` inside `f` — without threading provenance through every
/// sema work queue.
pub const MAX_SPECIALIZATION_ROUNDS: usize = 64;

impl Specializer {
    fn stable_identity(
        key: &SpecializationKey,
        sema: &crate::sema::BodySema<'_>,
    ) -> Result<
        crate::SemanticSpecializationIdentity<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
        crate::SemanticBodyExportFailure,
    > {
        let base = sema.function_identity(key.base_name)?;
        let type_arguments = key
            .type_args
            .iter()
            .map(|value| sema.export_body_type(*value))
            .collect::<Result<Vec<_>, _>>()?;
        let value_arguments = key
            .value_args
            .iter()
            .map(|value| {
                Ok(match value {
                    ConstValue::Integer(value) => crate::SemanticImportConstValue::Integer(*value),
                    ConstValue::Bool(value) => crate::SemanticImportConstValue::Bool(*value),
                    ConstValue::Type(value) => {
                        crate::SemanticImportConstValue::Type(sema.export_body_type(*value)?)
                    }
                    ConstValue::Function(value) => {
                        crate::SemanticImportConstValue::Function(sema.function_identity(*value)?)
                    }
                    ConstValue::Unit => crate::SemanticImportConstValue::Unit,
                    // Comptime parameters have no string type (RUE-957);
                    // export the content faithfully regardless.
                    ConstValue::String(content) => crate::SemanticImportConstValue::String(
                        std::sync::Arc::from(sema.interner.resolve(content)),
                    ),
                })
            })
            .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
        Ok(crate::SemanticSpecializationIdentity {
            base,
            type_arguments: type_arguments.into(),
            value_arguments: value_arguments.into(),
        })
    }

    fn take_reusable_body(
        &self,
        key: &SpecializationKey,
        sema: &mut crate::sema::BodySema<'_>,
    ) -> Option<
        crate::SemanticSpecializedBodyCandidate<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    > {
        let identity = Self::stable_identity(key, sema).ok()?;
        sema.reusable_specialized_bodies.remove(&identity)
    }

    /// Analyze and rewrite every specialization reachable from newly appended
    /// bodies, returning ordinary references that must re-enter sema's worklist.
    ///
    /// Generic-to-generic calls reach an internal fixed point here. The
    /// `Specializer` itself persists across outer sema waves so an ordinary
    /// helper analyzed later can request a specialization without duplicating
    /// one created by an earlier wave.
    pub(crate) fn run_to_fixpoint(
        &mut self,
        functions_with_strings: &mut Vec<(AnalyzedFunction, Vec<String>)>,
        all_warnings: &mut Vec<CompileWarning>,
        sema: &mut crate::sema::BodySema<'_>,
        infer_ctx: &InferenceContext,
        interner: &ThreadedRodeo,
    ) -> CompileResult<DiscoveredReferences> {
        let mut discovered = DiscoveredReferences::default();
        loop {
            // Only scan bodies appended since the previous internal or outer
            // wave. `scan_end` excludes specializations created below; they are
            // picked up on the next internal round.
            let scan_end = functions_with_strings.len();
            let mut pending: Vec<SpecializationKey> = Vec::new();
            let mut rewrite_needed = Vec::with_capacity(scan_end - self.next_unscanned);
            for (function, _) in &functions_with_strings[self.next_unscanned..scan_end] {
                rewrite_needed.push(collect_specializations(
                    &function.air,
                    interner,
                    &mut self.specializations,
                    &mut pending,
                    &mut sema.body_analysis_work,
                ));
            }

            // Previously scanned bodies have no CallGeneric instructions left.
            let mut unscanned = functions_with_strings.split_off(self.next_unscanned);
            let later = unscanned.split_off(scan_end - self.next_unscanned);
            for ((mut function, strings), needs_rewrite) in
                unscanned.into_iter().zip(rewrite_needed)
            {
                if needs_rewrite {
                    let mut editor = function.air.into_editor();
                    rewrite_call_generic(
                        &mut editor,
                        &self.specializations,
                        &mut sema.body_analysis_work,
                    );
                    function.air = editor.finish(
                        crate::AirValidationContext::SemanticWithSymbols(&sema.type_pool, interner),
                    )?;
                }
                functions_with_strings.push((function, strings));
            }
            functions_with_strings.extend(later);
            self.next_unscanned = scan_end;

            if pending.is_empty() {
                let specialized_calls = self
                    .specializations
                    .iter()
                    .filter_map(|(key, info)| {
                        Self::stable_identity(key, sema)
                            .ok()
                            .map(|identity| (info.mangled_name, identity))
                    })
                    .collect::<HashMap<_, _>>();
                for candidate in &self.completed_exports[self.next_unexported..] {
                    sema.body_analysis_work.specialized_body_exports_attempted += 1;
                    let (function, strings) = &functions_with_strings[candidate.function_index];
                    let warnings = if candidate.warning_free {
                        &[][..]
                    } else {
                        // The durable warning algebra is intentionally lossless;
                        // passing a marker preserves fail-closed behavior.
                        sema.body_analysis_work.specialized_body_exports_rejected += 1;
                        sema.body_analysis_work.last_specialized_body_export_failure =
                            Some(crate::SemanticBodyExportFailure::UnsupportedWarningMetadata);
                        continue;
                    };
                    match sema.export_specialized_body(
                        candidate.key.base_name,
                        &candidate.base_info,
                        &candidate.key.type_args,
                        &candidate.key.value_args,
                        function,
                        strings,
                        warnings,
                        &specialized_calls,
                        &candidate.dependencies,
                        candidate.dependency_boundary_complete,
                        &candidate.method_references,
                    ) {
                        Ok(export) => {
                            sema.body_analysis_work.specialized_body_exports_succeeded += 1;
                            sema.body_analysis_work
                                .specialized_body_export_instructions_emitted +=
                                export.body.instructions.len();
                            sema.body_analysis_work
                                .specialized_body_export_places_emitted += export.body.places.len();
                            sema.body_analysis_work
                                .specialized_body_export_strings_emitted +=
                                export.body.strings.len();
                            sema.specialized_body_exports.push(export);
                        }
                        Err(reason) => {
                            sema.body_analysis_work.specialized_body_exports_rejected += 1;
                            sema.body_analysis_work.last_specialized_body_export_failure =
                                Some(reason);
                        }
                    }
                }
                self.next_unexported = self.completed_exports.len();
                return Ok(discovered);
            }

            // Every non-empty expansion wave consumes the persistent
            // fixed-point budget. Reuse skips analysis, not expansion depth.
            self.rounds += 1;
            sema.body_analysis_work.specialization_rounds += 1;
            if self.rounds > MAX_SPECIALIZATION_ROUNDS {
                sema.body_analysis_work.specialization_driver_failures += 1;
                let key = &pending[0];
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: format!(
                            "specialization of '{}' exceeded the maximum nesting depth ({}); \
                             is a comptime-recursive function missing a compile-time-known \
                             base case, or a generic function recursively instantiating \
                             itself with new types?",
                            interner.resolve(&key.base_name),
                            MAX_SPECIALIZATION_ROUNDS
                        ),
                    },
                    self.specializations[key].call_site_span,
                ));
            }

            // Create specialized function bodies by re-analyzing with type
            // substitution. Newly created bodies are scanned next round.
            for key in &pending {
                let info = &self.specializations[key];
                let function_identity = sema
                    .canonical_specialization_instance(
                        key.base_name,
                        &key.type_args,
                        &key.value_args,
                    )
                    .map_err(|failure| {
                        CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to issue canonical specialization identity: {failure:?}"
                            )),
                            info.call_site_span,
                        )
                    })?;
                let base_info = match sema.function_info(key.base_name) {
                    Some(info) => info.clone(),
                    None => {
                        let func_name = interner.resolve(&key.base_name);
                        return Err(CompileError::new(
                            ErrorKind::UndefinedFunction(func_name.to_string()),
                            info.call_site_span,
                        ));
                    }
                };
                let reusable = self.take_reusable_body(key, sema).and_then(|candidate| {
                    sema.body_analysis_work.specialized_body_import_attempts += 1;
                    match crate::sema::analysis::import_staged_body(
                        sema,
                        &candidate.body,
                        candidate.body_span,
                    ) {
                        Ok(imported) => {
                            sema.body_analysis_work.specialized_body_import_successes += 1;
                            sema.body_analysis_work.specialized_body_import_instructions_installed +=
                                imported.air.len();
                            sema.body_analysis_work.specialized_body_import_places_installed +=
                                imported.air.places().len();
                            sema.body_analysis_work.specialized_body_import_strings_installed +=
                                imported.strings.len();
                            sema.body_analysis_work.specialized_bodies_reused += 1;
                            sema.body_analysis_work.specialized_body_analyses_skipped += 1;
                            let (referenced_functions, referenced_methods) =
                                crate::sema::analysis::imported_body_references(sema, &imported);
                            Some(SpecializedBody {
                                function: AnalyzedFunction {
                                    identity: function_identity.clone(),
                                    callable_kind: crate::AnalyzedCallableKind::Ordinary,
                                    ordinary_owner: None,
                                    name: interner.resolve(&info.mangled_name).to_string(),
                                    implicit_drop_source: Some(
                                        crate::sema::ImplicitDropDependencySourceEvent::Specialization {
                                            identity: candidate.identity.clone(),
                                        },
                                    ),
                                    air: imported.air,
                                    local_atoms: imported.local_atoms,
                                    num_locals: imported.num_locals,
                                    num_param_slots: imported.num_param_slots,
                                    param_modes: imported.param_modes,
                                    allow_unreachable_code: imported.allow_unreachable_code,
                                },
                                warnings: imported.warnings.to_vec(),
                                local_strings: imported.strings,
                                referenced_functions,
                                referenced_methods,
                                dependencies: candidate.dependencies.to_vec(),
                                dependency_boundary_complete: candidate
                                    .dependency_boundary_complete,
                            })
                        }
                        _ => {
                            sema.body_analysis_work.specialized_body_import_failures += 1;
                            None
                        }
                    }
                });
                let was_reused = reusable.is_some();
                if !was_reused {
                    sema.body_analysis_work.specialized_bodies_attempted += 1;
                }
                let specialized = match reusable.map(Ok).unwrap_or_else(|| {
                    create_specialized_function(
                        sema,
                        infer_ctx,
                        key,
                        info.mangled_name,
                        &base_info,
                        interner,
                    )
                }) {
                    Ok(body) => body,
                    Err(error) => {
                        sema.body_analysis_work.specialized_bodies_failed += 1;
                        return Err(error);
                    }
                };
                if !was_reused {
                    sema.body_analysis_work.specialized_bodies_succeeded += 1;
                }
                sema.body_analysis_work.air_instructions_produced +=
                    specialized.function.air.instructions().len();
                sema.body_analysis_work.local_strings_produced += specialized.local_strings.len();
                let mut dependencies = specialized.dependencies;
                let mut dependency_boundary_complete = specialized.dependency_boundary_complete;
                for function in &specialized.referenced_functions {
                    match sema.function_identity(*function) {
                        Ok(identity) => dependencies.push(identity),
                        Err(_) => dependency_boundary_complete = false,
                    }
                }
                if !specialized.referenced_methods.is_empty() {
                    dependency_boundary_complete = false;
                }
                dependencies.sort();
                dependencies.dedup();

                discovered
                    .functions
                    .extend(specialized.referenced_functions);
                let method_references = specialized.referenced_methods.clone();
                discovered.methods.extend(specialized.referenced_methods);

                // Surface warnings from the specialized body, once per (kind,
                // source span) across all specializations (RUE-555 for
                // unreachable patterns, RUE-574 for every other kind — a
                // generic body's unused variable used to warn only when the
                // function was non-generic). A warning that fires in ANY
                // specialization is surfaced: with comptime-pruned branches
                // the body that triggers it is really compiled.
                let warning_free = specialized.warnings.is_empty();
                for warning in specialized.warnings {
                    if let Some(span) = warning.span()
                        && self
                            .seen_warnings
                            .insert((std::mem::discriminant(&warning.kind), span))
                    {
                        all_warnings.push(warning);
                    }
                }
                let function_index = functions_with_strings.len();
                functions_with_strings.push((specialized.function, specialized.local_strings));
                self.completed_exports.push(CompletedExport {
                    key: key.clone(),
                    base_info,
                    function_index,
                    warning_free,
                    dependencies,
                    dependency_boundary_complete,
                    method_references,
                });
            }
        }
    }
}

fn collect_specializations(
    air: &Air,
    interner: &ThreadedRodeo,
    specializations: &mut HashMap<SpecializationKey, SpecializationInfo>,
    pending: &mut Vec<SpecializationKey>,
    work: &mut crate::BodyAnalysisWork,
) -> bool {
    let mut has_call_generic = false;
    for inst in air.instructions() {
        work.specialization_air_instructions_scanned += 1;
        if let AirInstData::CallGeneric {
            name,
            type_args,
            value_args,
            ..
        } = &inst.data
        {
            has_call_generic = true;
            work.generic_calls_observed += 1;
            let type_args: Vec<Type> = air.get_type_args(type_args).collect();
            let value_args = air.get_const_values(value_args).collect();

            let key = SpecializationKey {
                base_name: *name,
                type_args,
                value_args,
            };

            if let Entry::Vacant(entry) = specializations.entry(key) {
                work.specialization_requests_unique += 1;
                let base_name = interner.resolve(name);
                let mangled = mangle_specialized_name(
                    base_name,
                    &entry.key().type_args,
                    &entry.key().value_args,
                );
                let mangled_sym = interner.get_or_intern(&mangled);
                pending.push(entry.key().clone());
                entry.insert(SpecializationInfo {
                    mangled_name: mangled_sym,
                    call_site_span: inst.span,
                });
            } else {
                work.specialization_requests_duplicate += 1;
            }
        }
    }
    has_call_generic
}

fn rewrite_call_generic(
    air: &mut crate::AirEditor,
    specializations: &HashMap<SpecializationKey, SpecializationInfo>,
    work: &mut crate::BodyAnalysisWork,
) {
    let mut rewrites: Vec<(usize, Spur)> = Vec::new();

    for (i, inst) in air.instructions().iter().enumerate() {
        if let AirInstData::CallGeneric {
            name,
            type_args,
            value_args,
            args: _,
        } = &inst.data
        {
            let type_args: Vec<Type> = air.get_type_args(type_args).collect();
            let value_args = air.get_const_values(value_args).collect();

            let key = SpecializationKey {
                base_name: *name,
                type_args,
                value_args,
            };

            if let Some(info) = specializations.get(&key) {
                rewrites.push((i, info.mangled_name));
            }
        }
    }

    for (index, name) in rewrites {
        work.specialization_rewrites += 1;
        air.rewrite_generic_call_to_call(index, name);
    }
}

fn host_stable_identity<H>(
    key: &SpecializationKey,
    host: &H,
) -> Result<
    crate::SemanticSpecializationIdentity<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >,
    crate::SemanticBodyExportFailure,
>
where
    H: OrdinaryBodyAnalysisHost + SemanticBodyExportHost,
{
    let base = host.function_identity(key.base_name)?;
    let type_arguments = key
        .type_args
        .iter()
        .map(|value| host.export_body_type(*value))
        .collect::<Result<Vec<_>, _>>()?;
    let value_arguments = key
        .value_args
        .iter()
        .map(|value| {
            Ok(match value {
                ConstValue::Integer(value) => crate::SemanticImportConstValue::Integer(*value),
                ConstValue::Bool(value) => crate::SemanticImportConstValue::Bool(*value),
                ConstValue::Type(value) => {
                    crate::SemanticImportConstValue::Type(host.export_body_type(*value)?)
                }
                ConstValue::Function(value) => {
                    crate::SemanticImportConstValue::Function(host.function_identity(*value)?)
                }
                ConstValue::Unit => crate::SemanticImportConstValue::Unit,
                ConstValue::String(content) => crate::SemanticImportConstValue::String(
                    std::sync::Arc::from(host.resolve_publication_symbol(content)),
                ),
            })
        })
        .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
    Ok(crate::SemanticSpecializationIdentity {
        base,
        type_arguments: type_arguments.into(),
        value_arguments: value_arguments.into(),
    })
}

pub(crate) fn select_provider_body_specializations<H>(
    host: &mut H,
    mut function: AnalyzedFunction,
) -> CompileResult<(
    AnalyzedFunction,
    Vec<(
        Spur,
        crate::SemanticSpecializationIdentity<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    )>,
    Vec<crate::FunctionInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
)>
where
    H: OrdinaryBodyAnalysisHost + SemanticBodyExportHost,
{
    let mut specializations = HashMap::new();
    let mut pending = Vec::new();
    let mut work = crate::BodyAnalysisWork::default();
    if collect_specializations(
        &function.air,
        host.body_interner(),
        &mut specializations,
        &mut pending,
        &mut work,
    ) {
        let mut editor = function.air.into_editor();
        rewrite_call_generic(&mut editor, &specializations, &mut work);
        function.air = editor.finish(crate::AirValidationContext::SemanticWithSymbols(
            host.body_type_pool(),
            host.body_interner(),
        ))?;
    }
    let mut selected = pending
        .iter()
        .map(|key| {
            let info = &specializations[key];
            let identity = host_stable_identity(key, host).map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to select a canonical specialization identity: {failure:?}"
                    )),
                    info.call_site_span,
                )
            })?;
            let base = crate::FunctionInstanceKey::Definition(
                host.function_identity(key.base_name).map_err(|failure| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "failed to select a canonical specialization base: {failure:?}"
                        )),
                        info.call_site_span,
                    )
                })?,
            );
            let arguments = crate::CanonicalArguments {
                types: key
                    .type_args
                    .iter()
                    .map(|ty| host.canonical_type_instance(*ty))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|failure| {
                        CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to select canonical type arguments: {failure:?}"
                            )),
                            info.call_site_span,
                        )
                    })?
                    .into(),
                values: key
                    .value_args
                    .iter()
                    .copied()
                    .map(|value| host.canonical_argument_value(value))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|failure| {
                        CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to select canonical value arguments: {failure:?}"
                            )),
                            info.call_site_span,
                        )
                    })?
                    .into(),
            };
            let instance = crate::FunctionInstanceKey::Specialization {
                base: Box::new(base),
                arguments,
            };
            Ok((info.mangled_name, identity, instance))
        })
        .collect::<CompileResult<Vec<_>>>()?;
    selected.sort_by(|left, right| left.1.cmp(&right.1));
    selected.dedup_by(|left, right| left.1 == right.1);
    let instances = selected
        .iter()
        .map(|(_, _, instance)| instance.clone())
        .collect();
    let identities = selected
        .into_iter()
        .map(|(name, identity, _)| (name, identity))
        .collect();
    Ok((function, identities, instances))
}

pub(crate) struct OneSpecializedBody {
    pub(crate) function: AnalyzedFunction,
    pub(crate) warnings: Vec<CompileWarning>,
    pub(crate) local_strings: Vec<String>,
    pub(crate) referenced_functions: HashSet<Spur>,
    pub(crate) referenced_methods: HashSet<(StructId, Spur)>,
    pub(crate) dependencies: Vec<crate::SemanticDefinitionToken>,
    pub(crate) dependency_boundary_complete: bool,
    pub(crate) identity: crate::SemanticSpecializationIdentity<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >,
}

/// Run the substitution-aware ordinary expression engine for one provider
/// specialization without consulting or mutating a declaration epoch.
pub(crate) fn analyze_one_specialization_with_host<H>(
    host: &mut H,
    infer_ctx: &InferenceContext,
    key: SpecializationKey,
) -> CompileResult<OneSpecializedBody>
where
    H: OrdinaryBodyAnalysisHost + SemanticBodyExportHost,
{
    let base_info = host.function_body_info(key.base_name).ok_or_else(|| {
        CompileError::without_span(ErrorKind::UndefinedFunction(
            host.body_interner().resolve(&key.base_name).to_owned(),
        ))
    })?;
    let base_name = host.body_interner().resolve(&key.base_name).to_owned();
    let specialized_name_text =
        mangle_specialized_name(&base_name, &key.type_args, &key.value_args);
    let base_call_info = crate::sema::FunctionCallInfo::from_body(base_info);
    let param_comptime_type = host.comptime_type_param_flags(&base_call_info);
    let base_params = host
        .body_param_data(base_info.params)
        .iter()
        .map(|(name, ty, mode, is_comptime)| (*name, *ty, *mode, *is_comptime))
        .collect::<Vec<_>>();

    let mut type_subst = HashMap::new();
    let mut value_subst = HashMap::new();
    let mut type_arg_idx = 0;
    let mut value_arg_idx = 0;
    for (param_index, (name, _, _, is_comptime)) in base_params.iter().enumerate() {
        if !*is_comptime {
            continue;
        }
        if param_comptime_type[param_index] {
            let ty = key.type_args.get(type_arg_idx).copied().ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "specialization is missing type argument {type_arg_idx} for '{}'",
                        host.body_interner().resolve(name)
                    )),
                    base_info.span,
                )
            })?;
            type_subst.insert(*name, ty);
            type_arg_idx += 1;
        } else {
            let value = key.value_args.get(value_arg_idx).copied().ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "specialization is missing value argument {value_arg_idx} for '{}'",
                        host.body_interner().resolve(name)
                    )),
                    base_info.span,
                )
            })?;
            value_subst.insert(*name, value);
            value_arg_idx += 1;
        }
    }
    if type_arg_idx != key.type_args.len() || value_arg_idx != key.value_args.len() {
        return Err(CompileError::new(
            ErrorKind::InternalError(format!(
                "specialization argument streams do not match declaration: consumed \
                 {type_arg_idx} type/{value_arg_idx} value, received {}/{}",
                key.type_args.len(),
                key.value_args.len()
            )),
            base_info.span,
        ));
    }

    let canonical_arguments = crate::CanonicalArguments {
        types: key
            .type_args
            .iter()
            .map(|ty| host.canonical_type_instance(*ty))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue provider specialization arguments: {failure:?}"
                    )),
                    base_info.span,
                )
            })?
            .into(),
        values: key
            .value_args
            .iter()
            .copied()
            .map(|value| host.canonical_argument_value(value))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue provider specialization arguments: {failure:?}"
                    )),
                    base_info.span,
                )
            })?
            .into(),
    };
    let identity = OrdinaryBodyEngine::new(host)
        .canonical_specialization_instance(key.base_name, &key.type_args, &key.value_args)
        .map_err(|failure| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "failed to issue provider specialization identity: {failure:?}"
                )),
                base_info.span,
            )
        })?;
    // The requested callable remains an exact specialization identity even
    // when its argument streams are empty. Anonymous producer identity has a
    // narrower canonical form: an empty specialization is the base function.
    // Normalize at the producer boundary so the body, its produced facts, and
    // final composition all name the same anonymous nominal.
    let producer = (
        crate::StableProducerId::Function(Box::new(
            identity.with_collapsed_empty_specializations().into_owned(),
        )),
        canonical_arguments,
    );
    let previous = host.replace_active_anonymous_producer(Some(producer));
    let analysis = {
        let mut engine = OrdinaryBodyEngine::new(host);
        let return_type = engine.resolve_substituted_return_type(
            &base_call_info,
            &type_subst,
            &value_subst,
            base_info.span,
        )?;
        let mut specialized_params = Vec::with_capacity(base_params.len());
        for (param_index, (name, ty, mode, is_comptime)) in base_params.into_iter().enumerate() {
            if param_comptime_type[param_index] {
                continue;
            }
            let concrete_ty = engine.resolve_substituted_param_type(
                &base_call_info,
                param_index,
                ty,
                &type_subst,
                &value_subst,
                base_info.span,
            )?;
            specialized_params.push((name, concrete_ty, mode, is_comptime));
        }
        engine.analyze_specialized_function(
            infer_ctx,
            return_type,
            &specialized_params,
            base_info.body,
            &type_subst,
            &value_subst,
        )
    };
    host.replace_active_anonymous_producer(previous);
    let (
        air,
        num_locals,
        num_param_slots,
        param_modes,
        warnings,
        local_strings,
        local_atoms,
        referenced_functions,
        referenced_methods,
    ) = analysis?;
    let stable_identity = host_stable_identity(&key, host).map_err(|failure| {
        CompileError::new(
            ErrorKind::InternalError(format!(
                "failed to export provider specialization identity: {failure:?}"
            )),
            base_info.span,
        )
    })?;
    let mut dependencies = referenced_functions
        .iter()
        .filter_map(|symbol| host.function_identity(*symbol).ok())
        .collect::<Vec<_>>();
    dependencies.sort();
    dependencies.dedup();
    let dependency_boundary_complete =
        referenced_methods.is_empty() && dependencies.len() == referenced_functions.len();
    Ok(OneSpecializedBody {
        function: AnalyzedFunction {
            identity,
            callable_kind: crate::AnalyzedCallableKind::Ordinary,
            ordinary_owner: None,
            name: specialized_name_text,
            implicit_drop_source: Some(
                crate::sema::ImplicitDropDependencySourceEvent::Specialization {
                    identity: stable_identity.clone(),
                },
            ),
            air: crate::ValidatedAir::from_semantic_air_with_symbols(
                air,
                host.body_type_pool(),
                host.body_interner(),
            )?,
            local_atoms,
            num_locals,
            num_param_slots,
            param_modes,
            allow_unreachable_code: base_info.allow_unreachable_code,
        },
        warnings,
        local_strings,
        referenced_functions,
        referenced_methods,
        dependencies,
        dependency_boundary_complete,
        identity: stable_identity,
    })
}

/// Separator between mangled segments in a specialized symbol name.
///
/// A `.` is deliberately *illegal* in a Rue identifier (which is
/// `[a-zA-Z_][a-zA-Z0-9_]*`), so a specialized symbol like `identity.i32`
/// lives in a namespace disjoint from every name a user can spell. This is
/// the targeted fix for RUE-41: previously the separator was `__`, so
/// `identity<i32>` mangled to `identity__i32` — a name a user could legally
/// define as a plain function, producing a duplicate-symbol link error for a
/// valid program. Switching to an identifier-illegal separator makes the
/// collision impossible by construction, without a full user-symbol mangling
/// overhaul (that is RUE-178). The full user-symbol mangling scheme (RUE-178)
/// is still future work; this only guarantees specialized names can't clash
/// with user names.
const SPEC_SEP: char = '.';

/// Generate a mangled name for a specialized function.
///
/// Type arguments and value arguments each contribute a [`SPEC_SEP`]-separated
/// segment, so distinct comptime values yield distinct symbols
/// (`fact.v5`, `fact.v4`, ...; RUE-166) exactly like distinct types do
/// (RUE-100's lesson: colliding mangles mean duplicate symbols at link time).
/// The `.` separator is illegal in Rue identifiers, so these names cannot
/// collide with a user-spellable function name (RUE-41).
pub(crate) fn mangle_specialized_name(
    base_name: &str,
    type_args: &[Type],
    value_args: &[ConstValue],
) -> String {
    let mut mangled = base_name.to_string();
    for ty in type_args {
        mangled.push(SPEC_SEP);
        mangled.push_str(&mangle_type(*ty));
    }
    for value in value_args {
        mangled.push(SPEC_SEP);
        mangled.push_str(&mangle_const_value(value));
    }
    mangled
}

/// Mangle a single comptime value argument into a unique symbol fragment.
///
/// Negative integers use an `m` (minus) prefix on the magnitude because `-`
/// is not a safe symbol character (`-3` -> `vm3`).
fn mangle_const_value(value: &ConstValue) -> String {
    match value {
        ConstValue::Integer(n) if *n < 0 => format!("vm{}", n.unsigned_abs()),
        ConstValue::Integer(n) => format!("v{}", n),
        ConstValue::Bool(b) => format!("v{}", b),
        ConstValue::Unit => "vunit".to_string(),
        ConstValue::Type(ty) => format!("v{}", mangle_type(*ty)),
        ConstValue::Function(name) => format!("vfn{}", name.into_usize()),
        // Comptime parameters have no string type, so strings never appear
        // in a specialization key (RUE-957). The interner index still gives
        // a unique, symbol-safe fragment should that ever change.
        ConstValue::String(content) => format!("vstr{}", content.into_usize()),
    }
}

/// Mangle a single type argument into a unique string.
///
/// Scalar types use their source-level name. Aggregate types must encode their
/// identity (the struct/enum/array/pointer ID): `Type::name()` returns a generic
/// placeholder like `"<struct>"` for them, which made every struct instantiation
/// of a generic function collide on a single specialized symbol (RUE-100).
fn mangle_type(ty: Type) -> String {
    use crate::types::TypeKind;
    match ty.kind() {
        TypeKind::Struct(id) => format!("struct{}", id.0),
        TypeKind::Enum(id) => format!("enum{}", id.0),
        TypeKind::Array(id) => format!("array{}", id.0),
        TypeKind::PtrConst(id) => format!("ptr_const{}", id.0),
        TypeKind::PtrMut(id) => format!("ptr_mut{}", id.0),
        TypeKind::Module(id) => format!("module{}", id.0),
        _ => ty.name().to_string(),
    }
}

/// Create a specialized function by re-analyzing the body with type and
/// value substitution.
///
/// This builds a type substitution map from the comptime type parameters to
/// their concrete type arguments and a value substitution map from the
/// comptime value parameters to their concrete values (RUE-166), then
/// re-analyzes the function body with these substitutions.
fn create_specialized_function(
    sema: &mut crate::sema::BodySema<'_>,
    infer_ctx: &InferenceContext,
    key: &SpecializationKey,
    specialized_name: Spur,
    base_info: &FunctionInfo,
    interner: &ThreadedRodeo,
) -> CompileResult<SpecializedBody> {
    let specialized_name_str = interner.resolve(&specialized_name).to_string();

    // Pair each comptime parameter with its argument: type parameters
    // (declared `: type`) consume the type_args stream, value parameters
    // consume the value_args stream — mirroring how the call site collected
    // them (both in parameter order).
    let mut type_subst: HashMap<Spur, Type> = HashMap::new();
    let mut value_subst: HashMap<Spur, ConstValue> = HashMap::new();
    let mut type_arg_idx = 0;
    let mut value_arg_idx = 0;
    let param_comptime_type = sema.comptime_type_param_flags(base_info);
    for (param_index, (name, _, _, is_comptime)) in
        sema.param_arena.iter(base_info.params).enumerate()
    {
        if !*is_comptime {
            continue;
        }
        if param_comptime_type[param_index] {
            let ty = key.type_args.get(type_arg_idx).copied().ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "specialization is missing type argument {} for '{}'",
                        type_arg_idx,
                        interner.resolve(name)
                    )),
                    base_info.span,
                )
            })?;
            type_subst.insert(*name, ty);
            type_arg_idx += 1;
        } else {
            let value = key.value_args.get(value_arg_idx).copied().ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "specialization is missing value argument {} for '{}'",
                        value_arg_idx,
                        interner.resolve(name)
                    )),
                    base_info.span,
                )
            })?;
            value_subst.insert(*name, value);
            value_arg_idx += 1;
        }
    }
    if type_arg_idx != key.type_args.len() || value_arg_idx != key.value_args.len() {
        return Err(CompileError::new(
            ErrorKind::InternalError(format!(
                "specialization argument streams do not match declaration: consumed {type_arg_idx} type/{value_arg_idx} value, received {}/{}",
                key.type_args.len(),
                key.value_args.len()
            )),
            base_info.span,
        ));
    }

    // Resolve the return type, substituting type parameters - bare (`-> T`)
    // or inside a composite (`-> [T; 3]`, RUE-172). Value parameters also
    // substitute so an array length can name a `comptime N: i32` (`-> [i32; N]`,
    // RUE-16).
    let return_type = sema.resolve_substituted_return_type(base_info, &type_subst, &value_subst)?;

    // Copy out the param data first: substitution needs `&mut Sema` (composite
    // types like `[T; 3]` may intern new array types), which can't overlap a
    // borrow of the param arena.
    let base_params: Vec<(Spur, Type, RirParamMode, bool)> = sema
        .param_arena
        .iter(base_info.params)
        .map(|(name, ty, mode, is_comptime)| (*name, *ty, *mode, *is_comptime))
        .collect();
    let mut specialized_params: Vec<(Spur, Type, RirParamMode, bool)> =
        Vec::with_capacity(base_params.len());
    for (param_index, (name, ty, mode, is_comptime)) in base_params.into_iter().enumerate() {
        if param_comptime_type[param_index] {
            // Comptime TYPE parameters are erased from the specialized
            // signature (types don't exist at runtime).
            continue;
        }
        let concrete_ty = sema.resolve_substituted_param_type(
            base_info,
            param_index,
            ty,
            &type_subst,
            &value_subst,
        )?;
        // Comptime VALUE parameters stay in the runtime signature — the call
        // site still passes them. Their constant value is additionally
        // substituted into the body via value_subst (RUE-166).
        specialized_params.push((name, concrete_ty, mode, is_comptime));
    }

    let source_name = sema.source_function_name(key.base_name);
    let source_name_text = sema.interner.resolve(&source_name).to_string();
    let owner = crate::sema::AnalyzedBodyOwnerEvent::FreeFunction {
        token: sema.body_owner_token(
            base_info.file_id,
            &source_name_text,
            None,
            crate::sema::BodyOwnerKind::FreeFunction,
        ),
        file: base_info.file_id.index(),
        name: source_name_text.clone(),
    };
    let body_dependency_start = sema.body_named_dependencies.len();
    let type_dependency_start = sema.declaration_type_dependencies.len();
    let type_call_head_start = sema.declaration_type_call_head_dependencies.len();
    let builtin_call_head_start = sema.declaration_builtin_type_call_head_dependencies.len();
    let previous_body_observer = sema.body_dependency_observer.replace(owner);
    let previous_type_observer = sema.declaration_type_observer.replace((
        base_info.file_id,
        source_name_text.clone(),
        None,
        crate::sema::DeclarationTypeDependencySourceKind::Function,
        crate::sema::DeclarationTypeDependencyKind::Body,
    ));

    let mut producer = sema
        .canonical_function_producer(key.base_name, &type_subst, &value_subst)
        .map_err(|failure| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "failed to issue canonical specialization producer: {failure:?}"
                )),
                base_info.span,
            )
        })?;
    // A body-analysis specialization request already carries its exact durable
    // identity. Reconstructing that identity from materialized structural
    // types can select a different-but-equal anonymous representative and
    // therefore change nested argument anchors. Keep the request identity for
    // this top-level body; ordinary whole-program specialization has no such
    // request and continues to derive its canonical producer normally.
    if let Some(crate::StableProducerId::Function(requested)) =
        sema.body_analysis_requested_producer.as_ref()
    {
        let requested_arguments = match requested.as_ref() {
            crate::FunctionInstanceKey::Specialization { base, arguments }
                if matches!(
                    base.as_ref(),
                    crate::FunctionInstanceKey::Definition(definition)
                        if *definition == sema.function_identity(key.base_name).map_err(|failure| {
                            CompileError::new(
                                ErrorKind::InternalError(format!(
                                    "failed to match requested specialization producer: {failure:?}"
                                )),
                                base_info.span,
                            )
                        })?
                ) =>
            {
                Some(arguments.clone())
            }
            _ => None,
        };
        if let Some(arguments) = requested_arguments {
            producer = (
                crate::StableProducerId::Function(requested.clone()),
                arguments,
            );
        }
    }
    let crate::StableProducerId::Function(function_identity) = &producer.0 else {
        unreachable!("a specialization producer is always a function")
    };
    let function_identity = (**function_identity).clone();
    let previous_producer = sema.active_anonymous_producer.replace(producer);
    let analysis = sema.analyze_specialized_function(
        infer_ctx,
        return_type,
        &specialized_params,
        base_info.body,
        &type_subst,
        &value_subst,
        // Specialization covers free functions; they have no receiver.
        false,
    );
    sema.active_anonymous_producer = previous_producer;
    sema.body_dependency_observer = previous_body_observer;
    sema.declaration_type_observer = previous_type_observer;
    let (
        air,
        num_locals,
        num_param_slots,
        param_modes,
        warnings,
        local_strings,
        local_atoms,
        referenced_functions,
        referenced_methods,
    ) = analysis?;

    let mut dependencies = Vec::new();
    let mut stable_dependency_resolution_complete = true;
    for event in &sema.body_named_dependencies[body_dependency_start..] {
        let (file_id, name, kind) = match &event.target {
            crate::sema::NamedConstDependencyTargetEvent::ValueConst { file, name } => (
                *file,
                name.as_str(),
                crate::StableDefinitionKind::ValueConst,
            ),
            crate::sema::NamedConstDependencyTargetEvent::FreeFunction { file, name } => {
                (*file, name.as_str(), crate::StableDefinitionKind::Function)
            }
            crate::sema::NamedConstDependencyTargetEvent::NamedType { file, name, kind } => (
                *file,
                name.as_str(),
                match kind {
                    crate::sema::DeclarationTypeDependencyTargetKind::Struct => {
                        crate::StableDefinitionKind::Struct
                    }
                    crate::sema::DeclarationTypeDependencyTargetKind::Enum => {
                        crate::StableDefinitionKind::Enum
                    }
                    crate::sema::DeclarationTypeDependencyTargetKind::ValueConst => {
                        crate::StableDefinitionKind::ValueConst
                    }
                },
            ),
            crate::sema::NamedConstDependencyTargetEvent::ModuleBinding { file, name } => (
                *file,
                name.as_str(),
                crate::StableDefinitionKind::ModuleBinding,
            ),
        };
        if let Ok(token) = sema.stable_definition_token(file_id, name, None, kind) {
            dependencies.push(token);
        } else {
            stable_dependency_resolution_complete = false;
        }
    }
    for event in &sema.declaration_type_dependencies[type_dependency_start..] {
        let kind = match event.target_kind {
            crate::sema::DeclarationTypeDependencyTargetKind::Struct => {
                crate::StableDefinitionKind::Struct
            }
            crate::sema::DeclarationTypeDependencyTargetKind::Enum => {
                crate::StableDefinitionKind::Enum
            }
            crate::sema::DeclarationTypeDependencyTargetKind::ValueConst => {
                crate::StableDefinitionKind::ValueConst
            }
        };
        if let Ok(token) =
            sema.stable_definition_token(event.target_file, event.target_name.as_str(), None, kind)
        {
            dependencies.push(token);
        } else {
            stable_dependency_resolution_complete = false;
        }
    }
    for event in &sema.declaration_type_call_head_dependencies[type_call_head_start..] {
        if let Ok(token) = sema.stable_definition_token(
            event.callable_file,
            event.callable_name.as_str(),
            None,
            crate::StableDefinitionKind::Function,
        ) {
            dependencies.push(token);
        } else {
            stable_dependency_resolution_complete = false;
        }
    }
    // Builtin type call heads are compiler-schema inputs rather than source
    // definitions; their meaning is covered by the durable schema version.
    let dependency_boundary_complete = stable_dependency_resolution_complete
        && sema.declaration_builtin_type_call_head_dependencies[builtin_call_head_start..]
            .iter()
            .all(|event| {
                matches!(
                    event.builtin,
                    crate::sema::BuiltinTypeCallHead::FixedCapacityString
                )
            });
    dependencies.sort();
    dependencies.dedup();

    Ok(SpecializedBody {
        function: AnalyzedFunction {
            identity: function_identity,
            callable_kind: crate::AnalyzedCallableKind::Ordinary,
            ordinary_owner: None,
            name: specialized_name_str,
            implicit_drop_source: Some(Specializer::stable_identity(key, sema).map_or(
                crate::sema::ImplicitDropDependencySourceEvent::Anonymous,
                |identity| crate::sema::ImplicitDropDependencySourceEvent::Specialization {
                    identity,
                },
            )),
            air: crate::ValidatedAir::from_semantic_air_with_symbols(
                air,
                &sema.type_pool,
                sema.interner,
            )?,
            local_atoms,
            num_locals,
            num_param_slots,
            param_modes,
            allow_unreachable_code: base_info.allow_unreachable_code,
        },
        warnings,
        local_strings,
        referenced_functions,
        referenced_methods,
        dependencies,
        dependency_boundary_complete,
    })
}
