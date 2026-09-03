#[allow(unused_imports)]
use super::*;
use crate::{DiscoverySourceAssembler, SourceMetadata};
use rue_span::FileId;

/// The production durable declaration set for a snapshot, produced by the
/// semantic-nucleus batch projection — the same terminal the production
/// pipeline roots.
pub(crate) fn production_declarations(
    snapshot: &SourceSnapshot,
) -> Arc<[crate::durable_semantics::DurableDeclarationSemantic]> {
    let stages = crate::test_support::test_frontend_stages(snapshot).unwrap();
    let merged = &stages.merged;
    let imports = crate::test_support::test_import_graph(snapshot).unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(snapshot), snapshot);
    database.adopt_test_import_graph_for_revision(revision, imports);
    let revision = database.current_semantic_revision().unwrap();
    database
        .projected_declaration_semantics(
            revision,
            merged.ast(),
            rue_target::Target::X86_64Linux,
            &crate::PreviewFeatures::default(),
            CancellationToken::new(),
        )
        .expect("declaration semantics project for the fixture")
        .declarations
}

pub(crate) fn durable_decl<'a>(
    decls: &'a [crate::durable_semantics::DurableDeclarationSemantic],
    kind: crate::StableDefinitionKind,
    name: &str,
) -> &'a crate::durable_semantics::DurableDeclarationSemantic {
    decls
        .iter()
        .find(|record| record.key.kind() == kind && record.key.name() == name)
        .unwrap_or_else(|| panic!("no production {kind:?} named {name}"))
}
/// The durable declaration set projected into the body identity pool's
/// durable source vocabulary. Reads r2's stable-keyed metadata by key — the
/// pool consults it on demand (dedup / poison), the O(consumed) shape.
pub(crate) struct DurableDeclSource {
    pub(super) by_key:
        AHashMap<StableDefinitionKey, crate::durable_semantics::DurableDeclarationSemantic>,
    pub(super) anon_by_identity:
        AHashMap<crate::AnonymousNominalKey, crate::durable_semantics::DurableAnonymousNominal>,
}

impl DurableDeclSource {
    pub(crate) fn from_declarations(
        decls: &[crate::durable_semantics::DurableDeclarationSemantic],
    ) -> Self {
        Self {
            by_key: decls.iter().map(|d| (d.key.clone(), d.clone())).collect(),
            anon_by_identity: AHashMap::new(),
        }
    }

    /// Seed the durable anonymous nominals the pool mints from (RUE-1091 r6b).
    /// The durable anonymous universe is keyed by the CANONICAL producer form
    /// (the `DurableAnonymousSource` contract — the pool collapses its
    /// incoming key on entry and consults shapes under that form), so the
    /// adapter indexes each nominal by the shared collapse rather than its
    /// as-projected identity: the declaration-SIGNATURE projection retains an
    /// empty-argument specialization wrapper production body-export does not.
    pub(crate) fn with_anonymous_nominals(
        mut self,
        anonymous: &[crate::durable_semantics::DurableAnonymousNominal],
    ) -> Self {
        self.anon_by_identity = anonymous
            .iter()
            .map(|nominal| {
                (
                    nominal.identity.with_canonical_producer().into_owned(),
                    nominal.clone(),
                )
            })
            .collect();
        self
    }
}
/// Relocate a durable `StableDefinitionKey` to the exact stable-symbol content
/// the epoch's `stable_definition_symbol_component` (rue-air `anon_structs.rs`)
/// emits for its installed endpoint, so the pool and the epoch spell the same
/// `__anon_*_{digest}` name (RUE-1091 r6b). The durable key carries the module
/// logical path, name, owner NAME, and kind — the same four parts the epoch's
/// endpoint carries — fed to the ONE shared format assembly
/// (`rue_air::stable_digest::stable_definition_component`) the epoch also
/// renders through.
fn durable_definition_symbol_component(key: &StableDefinitionKey) -> String {
    rue_air::stable_digest::stable_definition_component(
        key.module().logical_path(),
        key.name(),
        key.owner().map(|owner| owner.name()),
        key.kind() as u8,
    )
}

fn durable_module_symbol_component(module: &ModuleId) -> String {
    rue_air::stable_digest::stable_module_component(module.logical_path())
}

fn durable_anonymous_shape(
    shape: &crate::durable_semantics::DurableAnonymousNominalShape,
) -> rue_air::DurableAnonymousShape<StableDefinitionKey, ModuleId> {
    use crate::durable_semantics::DurableAnonymousNominalShape as S;
    match shape {
        S::Struct { fields, methods } => rue_air::DurableAnonymousShape::Struct {
            fields: fields.iter().map(|(n, t)| (n.clone(), t.clone())).collect(),
            struct_method_names: methods.iter().map(|m| m.name.clone()).collect(),
        },
        S::Enum { variants } => rue_air::DurableAnonymousShape::Enum {
            variants: variants
                .iter()
                .map(|(n, payload)| (n.clone(), payload.to_vec()))
                .collect(),
        },
    }
}

impl rue_air::DurableAnonymousSource<StableDefinitionKey, ModuleId> for DurableDeclSource {
    fn anonymous_shape(
        &self,
        key: &crate::AnonymousNominalKey,
    ) -> Option<rue_air::DurableAnonymousShape<StableDefinitionKey, ModuleId>> {
        self.anon_by_identity
            .get(key)
            .map(|nominal| durable_anonymous_shape(&nominal.shape))
    }

    fn anonymous_shape_and_digest(
        &self,
        key: &crate::AnonymousNominalKey,
    ) -> Option<(
        rue_air::DurableAnonymousShape<StableDefinitionKey, ModuleId>,
        u128,
    )> {
        let nominal = self.anon_by_identity.get(key)?;
        Some((
            durable_anonymous_shape(&nominal.shape),
            nominal.anonymous_identity_digest(),
        ))
    }

    fn definition_symbol_component(&self, key: &StableDefinitionKey) -> String {
        durable_definition_symbol_component(key)
    }

    fn module_symbol_component(&self, module: &ModuleId) -> String {
        durable_module_symbol_component(module)
    }
}

impl rue_air::DurableNominalSource<StableDefinitionKey, ModuleId> for DurableDeclSource {
    fn module_is_trusted_standard_library(&self, module: &ModuleId) -> bool {
        module.is_trusted_standard_library()
    }

    fn nominal(
        &self,
        key: &StableDefinitionKey,
    ) -> Option<rue_air::DurableNominal<StableDefinitionKey, ModuleId>> {
        use crate::durable_semantics::DurableDeclarationPayload as Payload;
        let decl = self.by_key.get(key)?;
        let body = match &decl.payload {
            Payload::Struct {
                fields,
                is_copy,
                is_linear,
            } => rue_air::DurableNominalBody::Struct {
                fields: fields.clone().into(),
                is_copy: *is_copy,
                is_linear: *is_linear,
            },
            Payload::Enum {
                variants,
                is_non_exhaustive,
            } => rue_air::DurableNominalBody::Enum {
                is_non_exhaustive: *is_non_exhaustive,
                variants: variants
                    .iter()
                    .map(|(n, payload)| (n.clone(), payload.clone().into()))
                    .collect(),
            },
            _ => return None,
        };
        Some(rue_air::DurableNominal {
            name: Arc::from(decl.key.name()),
            module_path: Arc::from(decl.key.module().logical_path()),
            is_public: decl.is_public,
            // A user nominal in the durable set: builtin/lang-item/`@repr(c)`
            // are trusted-provenance / declaration side facts the durable
            // payload does not carry, so this differential's user corpora
            // leave them at their non-set defaults.
            is_builtin: false,
            lang_item: None,
            is_repr_c: false,
            has_destructor: self.by_key.keys().any(|member| {
                member.kind() == crate::StableDefinitionKind::Destructor
                    && member.owner().is_some_and(|owner| {
                        owner.module() == decl.key.module()
                            && owner.kind() == decl.key.kind()
                            && owner.name() == decl.key.name()
                    })
            }),
            body,
        })
    }
}

impl rue_air::DurableCallableSource<StableDefinitionKey, ModuleId> for DurableDeclSource {
    fn function(
        &self,
        key: &StableDefinitionKey,
    ) -> Option<rue_air::DurableFunction<StableDefinitionKey, ModuleId>> {
        use crate::durable_semantics::DurableDeclarationPayload as Payload;
        let decl = self.by_key.get(key)?;
        let Payload::Callable {
            parameters,
            result,
            has_self: _,
            is_unchecked,
            ..
        } = &decl.payload
        else {
            return None;
        };
        // Namespace ownership, not `has_self`, separates free functions
        // from nominal members. Associated functions have no receiver but
        // still live in the owner's method namespace.
        if key.kind().requires_owner() {
            return None;
        }
        Some(rue_air::DurableFunction {
            parameters: parameters.clone(),
            result: result.clone(),
            type_syntax: None,
            is_public: decl.is_public,
            is_unchecked: *is_unchecked,
            is_extern: false,
        })
    }

    fn method(
        &self,
        key: &StableDefinitionKey,
    ) -> Option<rue_air::DurableMethod<StableDefinitionKey, ModuleId>> {
        // r4b-3: the durable method key's receiver preimage is its owner
        // nominal. The `Callable` payload carries the explicit parameters and
        // result (self is separate, tracked by `has_self`); the receiver type
        // is the owner nominal, recovered by joining the method key's
        // `owner()` (module + kind + name) back to the owner nominal's own
        // durable key in this set, so the pool resolves it through the same 2a
        // nominal machinery as any parameter type.
        use crate::durable_semantics::DurableDeclarationPayload as Payload;
        let decl = self.by_key.get(key)?;
        let Payload::Callable {
            parameters,
            result,
            has_self,
            self_mode,
            ..
        } = &decl.payload
        else {
            return None;
        };
        // Nominal ownership admits both receiver-taking methods and
        // associated functions. A genuinely free function has no owner.
        let owner = key.owner()?;
        let owner_key = self
            .by_key
            .keys()
            .find(|candidate| {
                candidate.owner().is_none()
                    && candidate.module() == owner.module()
                    && candidate.kind() == owner.kind()
                    && candidate.name() == owner.name()
            })?
            .clone();
        Some(rue_air::DurableMethod {
            receiver: rue_air::SemanticImportType::Nominal(owner_key),
            parameters: parameters.clone(),
            result: result.clone(),
            type_syntax: None,
            has_self: *has_self,
            self_mode: *self_mode,
            is_accessor: false,
            returns_borrow: false,
            returns_inout: false,
        })
    }
}

impl rue_air::DurableConstSource<StableDefinitionKey, ModuleId> for DurableDeclSource {
    fn constant(
        &self,
        key: &StableDefinitionKey,
    ) -> Option<rue_air::DurableConst<StableDefinitionKey, ModuleId>> {
        use crate::durable_semantics::DurableDeclarationPayload as Payload;
        let decl = self.by_key.get(key)?;
        let Payload::Const { ty, value } = &decl.payload else {
            // Module bindings deliberately STOP here: their durable target
            // is real, but the body pool has no module-registry identity arm
            // from which to mint the epoch-local `Type::Module`.
            return None;
        };
        Some(rue_air::DurableConst {
            is_public: decl.is_public,
            ty: ty.clone(),
            value: value.clone(),
        })
    }

    fn function_name(&self, key: &StableDefinitionKey) -> Option<Arc<str>> {
        use crate::durable_semantics::DurableDeclarationPayload as Payload;
        matches!(self.by_key.get(key)?.payload, Payload::Callable { .. })
            .then(|| Arc::from(key.name()))
    }
}
/// The index-independent render of a nominal pool [`rue_air::Type`]: the
/// display, copyability, visibility, mangled symbol, and member vocabulary a
/// differential compares across two independently-minted pools (the pool
/// mints its own ids; parity is a display/metadata property).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndpointNominalRender {
    pub(crate) display: String,
    pub(crate) is_copy: bool,
    pub(crate) is_pub: bool,
    pub(crate) symbol: String,
    pub(crate) members: Vec<(String, String)>,
}

/// Render any pool [`rue_air::Type`] to its display, recursing through pool
/// indices so it is index-independent and safe to compare across two pools.
pub(crate) fn endpoint_display(pool: &rue_air::TypeInternPool, ty: rue_air::Type) -> String {
    use rue_air::TypeKind;
    match ty.kind() {
        TypeKind::I8 => "i8".into(),
        TypeKind::I16 => "i16".into(),
        TypeKind::I32 => "i32".into(),
        TypeKind::I64 => "i64".into(),
        TypeKind::U8 => "u8".into(),
        TypeKind::U16 => "u16".into(),
        TypeKind::U32 => "u32".into(),
        TypeKind::U64 => "u64".into(),
        TypeKind::Bool => "bool".into(),
        TypeKind::Unit => "()".into(),
        TypeKind::Never => "!".into(),
        TypeKind::ComptimeType => "type".into(),
        TypeKind::Struct(id) => pool.struct_def(id).name.to_string(),
        TypeKind::Enum(id) => pool.enum_def(id).name.to_string(),
        TypeKind::Array(id) => {
            let (element, len) = pool.array_def(id);
            format!("[{}; {}]", endpoint_display(pool, element), len)
        }
        TypeKind::PtrConst(id) => {
            format!(
                "ptr const {}",
                endpoint_display(pool, pool.ptr_const_def(id))
            )
        }
        TypeKind::PtrMut(id) => {
            format!("ptr mut {}", endpoint_display(pool, pool.ptr_mut_def(id)))
        }
        other => format!("{other:?}"),
    }
}

/// Index-independent copyability over the pool's own definitions.
pub(crate) fn endpoint_is_copy(pool: &rue_air::TypeInternPool, ty: rue_air::Type) -> bool {
    use rue_air::TypeKind;
    match ty.kind() {
        TypeKind::Struct(id) => pool.struct_def(id).is_copy,
        TypeKind::Enum(id) => pool
            .enum_def(id)
            .variant_payloads
            .iter()
            .flatten()
            .all(|&ty| endpoint_is_copy(pool, ty)),
        TypeKind::Array(id) => endpoint_is_copy(pool, pool.array_def(id).0),
        _ => true,
    }
}

/// Render a nominal (struct or enum) pool [`rue_air::Type`] to its
/// index-independent metadata.
pub(crate) fn endpoint_nominal_render(
    pool: &rue_air::TypeInternPool,
    ty: rue_air::Type,
) -> EndpointNominalRender {
    use rue_air::TypeKind;
    match ty.kind() {
        TypeKind::Struct(id) => {
            let def = pool.struct_def(id);
            EndpointNominalRender {
                display: def.name.to_string(),
                is_copy: def.is_copy,
                is_pub: def.is_pub,
                symbol: pool.struct_symbol_name(id),
                members: def
                    .fields
                    .iter()
                    .map(|field| (field.name.clone(), endpoint_display(pool, field.ty)))
                    .collect(),
            }
        }
        TypeKind::Enum(id) => {
            let def = pool.enum_def(id);
            EndpointNominalRender {
                display: def.name.to_string(),
                is_copy: endpoint_is_copy(pool, ty),
                is_pub: def.is_pub,
                symbol: pool.enum_symbol_name(id),
                members: def
                    .variants
                    .iter()
                    .map(|variant| (variant.to_string(), String::new()))
                    .collect(),
            }
        }
        other => panic!("endpoint_nominal_render expects a nominal, got {other:?}"),
    }
}
impl RevisionedQueryDatabase {
    #[cfg(test)]
    pub(crate) fn arm_codegen_evaluator_gate_for_test(&self) -> Arc<TestCodegenEvaluatorGate> {
        let gate = Arc::new(new_test_codegen_evaluator_gate());
        let replaced = self
            .codegen_evaluator_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(gate.clone());
        assert!(replaced.is_none(), "only one CodegenUnit gate may be armed");
        gate
    }

    #[cfg(test)]
    pub(crate) fn arm_codegen_batch_evaluator_gate_for_test(
        &self,
        gated_children: usize,
        rendezvous: bool,
    ) -> Arc<TestBackendBatchEvaluatorGate> {
        let gate = Arc::new(new_test_backend_batch_evaluator_gate(
            gated_children,
            rendezvous,
        ));
        let replaced = self
            .codegen_batch_evaluator_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(gate.clone());
        assert!(
            replaced.is_none(),
            "only one CodegenUnit batch gate may be armed"
        );
        gate
    }

    #[cfg(test)]
    pub(crate) fn runtime_metrics_for_test(&self) -> rue_query::RuntimeMetrics {
        self.runtime.metrics()
    }

    #[cfg(test)]
    pub(crate) fn inject_body_transaction_failure_for_test(
        &self,
    ) -> TestBodyTransactionFailureGuard {
        use std::sync::atomic::Ordering::SeqCst;

        assert!(
            !self.inject_body_transaction_failure.swap(true, SeqCst),
            "body transaction failure injection is not nestable"
        );
        TestBodyTransactionFailureGuard(self.inject_body_transaction_failure.clone())
    }

    #[cfg(test)]
    pub(crate) fn cancel_constraint_generation_after_nodes_for_test(
        &self,
        nodes: usize,
    ) -> TestConstraintGenerationCancellationGuard {
        TEST_CGEN_VISITS.store(0, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_ATTEMPTED_SIBLINGS.store(0, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_POST_CANCEL_ATTEMPTS.store(0, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_FRONTIER_STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_PHASE.store(0, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_FRONTIER_ONLY.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            TEST_CGEN_CANCEL_AFTER.swap(nodes, std::sync::atomic::Ordering::SeqCst),
            usize::MAX,
            "constraint-generation cancellation injection is not nestable"
        );
        TestConstraintGenerationCancellationGuard
    }

    #[cfg(test)]
    pub(crate) fn cancel_frontier_constraint_generation_after_nodes_for_test(
        &self,
        nodes: usize,
    ) -> TestConstraintGenerationCancellationGuard {
        TEST_CGEN_VISITS.store(0, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_ATTEMPTED_SIBLINGS.store(0, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_POST_CANCEL_ATTEMPTS.store(0, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_FRONTIER_STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_PHASE.store(0, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_FRONTIER_ONLY.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            TEST_CGEN_CANCEL_AFTER.swap(nodes, std::sync::atomic::Ordering::SeqCst),
            usize::MAX,
            "constraint-generation cancellation injection is not nestable"
        );
        TestConstraintGenerationCancellationGuard
    }

    #[cfg(test)]
    pub(crate) fn arm_frontier_rendezvous_for_test(
        &self,
        rendezvous: Arc<FrontierRendezvous>,
    ) -> FrontierRendezvousGuard {
        let replaced = TEST_FRONTIER_RENDEZVOUS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(rendezvous);
        assert!(
            replaced.is_none(),
            "only one frontier rendezvous may be armed"
        );
        FrontierRendezvousGuard
    }

    #[cfg(test)]
    pub(crate) fn constraint_generation_visits_for_test(&self) -> usize {
        TEST_CGEN_VISITS.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn constraint_generation_attempted_siblings_for_test(&self) -> usize {
        TEST_CGEN_ATTEMPTED_SIBLINGS.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn constraint_generation_post_cancel_attempts_for_test(&self) -> usize {
        TEST_CGEN_POST_CANCEL_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn constraint_generation_phase_for_test(&self) -> u8 {
        TEST_CGEN_PHASE.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn inject_declaration_body_plan_failure_for_test(
        &self,
        definition: &crate::StableDefinitionKey,
        errors: crate::CompileErrors,
    ) {
        let candidate = declaration_candidate_for_stable_key(definition)
            .expect("test plan-failure injection targets a source declaration");
        let mut injection = self
            .declaration_body_plan_failure_injection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(injection.is_none(), "plan-failure injection is single-use");
        *injection = Some((candidate, errors));
    }

    #[cfg(test)]
    pub(super) fn force_body_closure_anonymous_digest_for_test(
        &self,
        identity: crate::AnonymousNominalKey,
        digest: u128,
    ) {
        let mut forcing = self
            .body_closure_anonymous_digest_forcing
            .lock()
            .expect("body-closure forced-digest state is not poisoned");
        assert!(
            !forcing.sealed,
            "body-closure digest forcing must be configured before the first closure evaluation"
        );
        forcing
            .digests
            .insert(identity.with_canonical_producer().into_owned(), digest);
    }
}

pub(super) fn lookup_history_key(name: impl Into<Arc<str>>) -> LookupObservationKey {
    LookupObservationKey::Name(LookupNameKey {
        module: ModuleId::from_logical_path("history.rue").unwrap(),
        namespace: DefinitionNamespace::ModuleItem,
        name: name.into(),
    })
}

pub(super) fn source_snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
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

pub(super) fn semantic_configuration() -> crate::semantic_query_nucleus::SemanticQueryConfiguration
{
    crate::semantic_query_nucleus::SemanticQueryConfiguration {
        target: rue_target::Target::X86_64Linux,
        preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
    }
}

pub(super) fn free_function_instance(module: &ModuleId, name: &str) -> crate::FunctionInstanceKey {
    crate::FunctionInstanceKey::Definition(crate::StableDefinitionKey::from_stable_parts(
        module.clone(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        Arc::from(name),
        None,
    ))
}

pub(super) fn declaration_candidate(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    module: &ModuleId,
    category: crate::declaration_candidate::DeclarationCandidateCategory,
    name: &str,
) -> crate::declaration_candidate::DeclarationCandidateKey {
    let attempt = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        revision,
        ModuleQueryKey(module.clone()),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(value) = attempt.terminal().unwrap().outcome() else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(index) = value else {
        panic!("declaration occurrence index unavailable")
    };
    index
        .capabilities
        .keys()
        .find(|candidate| candidate.category == category && candidate.name.as_ref() == name)
        .cloned()
        .unwrap_or_else(|| panic!("missing {category:?} candidate `{name}`"))
}

pub(super) fn revision_for(
    database: &mut RevisionedQueryDatabase,
    snapshot: &SourceSnapshot,
) -> Revision {
    database.source_revision(
        &super::super::session::ExactSourceInput::new(snapshot),
        snapshot,
    )
}

pub(super) fn request_lookup_name(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    module: &ModuleId,
    namespace: DefinitionNamespace,
    name: &str,
) -> QueryRequestAttempt<LookupNameValue> {
    database.runtime.request_registered(
        &database.lookup_names,
        revision,
        LookupNameKey {
            module: module.clone(),
            namespace,
            name: Arc::from(name),
        },
        CancellationToken::new(),
    )
}

pub(super) fn canonical_of(
    attempt: &QueryRequestAttempt<LookupNameValue>,
) -> CanonicalNameResolution {
    let terminal = attempt.terminal().unwrap();
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!("LookupName publishes typed values")
    };
    CanonicalNameResolution::classify(value)
}

pub(super) fn request_lookup_import(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    module: &ModuleId,
    specifier: &str,
) -> QueryRequestAttempt<LookupImportValue> {
    database.runtime.request_registered(
        &database.lookup_imports,
        revision,
        LookupImportKey {
            module: module.clone(),
            specifier: Arc::from(specifier),
        },
        CancellationToken::new(),
    )
}

pub(super) fn import_binding(
    attempt: &QueryRequestAttempt<LookupImportValue>,
) -> LookupImportValue {
    let terminal = attempt.terminal().unwrap();
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!("LookupImport publishes typed values")
    };
    value.clone()
}

pub(super) fn anonymous_identity_for_digest_test(
    name: &str,
    kind: rue_air::AnonymousNominalKind,
) -> crate::AnonymousNominalKey {
    let module = ModuleId::from_logical_path("digest-test.rue").unwrap();
    let definition = crate::StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        Arc::from(name),
        None,
    );
    crate::AnonymousNominalKey {
        kind,
        producer: crate::StableProducerId::Function(Node::new(
            crate::FunctionInstanceKey::Definition(definition),
        )),
        anchor: rue_rir::RirStructuralAnchor::new(vec![
            rue_rir::RirStructuralPathSegment::Body,
            rue_rir::RirStructuralPathSegment::AnonymousType(0),
        ]),
    }
}

pub(super) fn trusted_option_body_snapshot(
    root_source: &str,
    option_source: &str,
) -> SourceSnapshot {
    let option = FileId::new(2);
    trusted_body_snapshot(root_source, Some((option, option_source)), None)
}

pub(super) fn trusted_body_snapshot(
    root_source: &str,
    option_source: Option<(FileId, &str)>,
    strbuf_source: Option<(FileId, &str)>,
) -> SourceSnapshot {
    let root = FileId::new(1);
    let mut physical = AHashMap::from([(root, "/project/main.rue".to_owned())]);
    let mut logical = AHashMap::from([(root, "main.rue".to_owned())]);
    let mut trusted = AHashSet::new();
    let mut sources = vec![(root, Arc::new(root_source.to_owned()))];
    if let Some((option, source)) = option_source {
        physical.insert(option, "/sdk/option.rue".to_owned());
        logical.insert(option, crate::OPTION_MODULE_LOGICAL_PATH.to_owned());
        trusted.insert(option);
        sources.push((option, Arc::new(source.to_owned())));
    }
    if let Some((strbuf, source)) = strbuf_source {
        physical.insert(strbuf, "/sdk/strbuf.rue".to_owned());
        logical.insert(strbuf, crate::STRBUF_MODULE_LOGICAL_PATH.to_owned());
        trusted.insert(strbuf);
        sources.push((strbuf, Arc::new(source.to_owned())));
    }
    let metadata =
        SourceMetadata::new_with_trusted_standard_library(root, physical, logical, trusted)
            .expect("trusted Option metadata is valid");
    SourceSnapshot::new(metadata, sources).expect("trusted body snapshot is valid")
}

pub(super) fn begin_database_plan(
    database: &mut RevisionedQueryDatabase,
    assembler: &mut DiscoverySourceAssembler,
    context: ImportDiscoveryContext,
) -> (
    SourceSnapshot,
    AcceptedReadManifest,
    ImportInputRevision,
    ImportDiscoveryPlan,
) {
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let revision = database
        .begin_import_inputs(&snapshot, context.clone(), reads.clone())
        .unwrap();
    let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
    let root = ModuleId::from_logical_path("main.rue").unwrap();
    let modules = snapshot
        .source_revision()
        .modules()
        .iter()
        .map(|module| module.module.clone())
        .collect::<Vec<_>>();
    let (program, _) = database.parse_program(runtime_revision, &root, modules);
    let plan = ImportDiscoveryPlan::new(&program.unwrap(), context).unwrap();
    (snapshot, reads, revision, plan)
}

pub(super) fn publish_manifest_observations(
    database: &mut RevisionedQueryDatabase,
    snapshot: &SourceSnapshot,
    reads: AcceptedReadManifest,
    plan: &ImportDiscoveryPlan,
    mut revision: ImportInputRevision,
) -> ImportInputRevision {
    let roots = ImportDemandRoots::whole_plan(plan);
    loop {
        let frontier = database
            .import_frontier(revision, plan, ImportDemandMode::Rooted, &roots)
            .unwrap();
        if frontier.requests().is_empty() {
            return revision;
        }
        let observations = frontier
            .requests()
            .iter()
            .cloned()
            .map(|request| {
                let Some(entry) = reads
                    .iter()
                    .find(|entry| entry.requested_path() == request.requested_path())
                else {
                    return ImportObservation::absent(request);
                };
                let file_id = snapshot
                    .files()
                    .find(|source| snapshot.module_id(source.file_id) == Some(entry.module()))
                    .unwrap()
                    .file_id;
                let accepted = crate::AcceptedImportSource::new(
                    entry.requested_path(),
                    entry.canonical_path(),
                    entry.metadata_identity(),
                    entry.metadata_fingerprint(),
                    snapshot.shared_source_text(file_id).unwrap(),
                )
                .unwrap();
                ImportObservation::accepted(request, accepted).unwrap()
            })
            .collect();
        revision = database
            .publish_import_batch(&frontier, snapshot, reads.clone(), observations)
            .unwrap();
    }
}

pub(super) fn declaration_import_key(
    module: &ModuleId,
    category: crate::declaration_candidate::DeclarationCandidateCategory,
    name: impl Into<Arc<str>>,
    owner: Option<crate::declaration_candidate::DeclarationCandidateOwner>,
    occurrence: u32,
    specifier: &str,
) -> DeclarationImportQueryKey {
    DeclarationImportQueryKey(crate::declaration_candidate::DeclarationImportSiteKey {
        declaration: crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category,
            name: name.into(),
            owner,
            duplicate_discriminator: 0,
        },
        occurrence,
        specifier: Arc::from(specifier),
    })
}

pub(super) fn request_semantic_nucleus(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    key: crate::semantic_query_nucleus::SemanticNucleusKey,
) -> crate::semantic_query_nucleus::SemanticNucleusValue {
    request_semantic_nucleus_observed(database, revision, key).0
}

pub(super) fn request_semantic_nucleus_observed(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    key: crate::semantic_query_nucleus::SemanticNucleusKey,
) -> (
    crate::semantic_query_nucleus::SemanticNucleusValue,
    QueryRequestAttempt<crate::semantic_query_nucleus::SemanticNucleusValue>,
) {
    let attempt = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        key,
        CancellationToken::new(),
    );
    let terminal = attempt
        .terminal()
        .unwrap_or_else(|| panic!("semantic nucleus aborted: {:?}", attempt.abort()));
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!()
    };
    (value.clone(), attempt)
}

/// The current node incarnation of one module-item lookup terminal. Reading a
/// warm key returns its retained incarnation; an evicted key would rebuild a
/// fresh incarnation, which is exactly what a birth-eviction window would
/// leave behind.
pub(super) fn lookup_incarnation(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    module: &ModuleId,
    name: &str,
) -> u64 {
    request_lookup_name(
        database,
        revision,
        module,
        DefinitionNamespace::ModuleItem,
        name,
    )
    .terminal()
    .unwrap()
    .node_incarnation()
}
