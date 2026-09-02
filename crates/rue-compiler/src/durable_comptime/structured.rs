//! Structured-type suspension, one-shot foreign probing, and resume.

use super::lifecycle::*;
use super::projection::*;
#[cfg(test)]
use super::services::*;
use super::*;

/// The canonical structured-type job owned by one durable program core. The
/// job retains the cloned type arena and symbol authority internally; callers
/// can only resume the exact continuation returned by the AIR resolver.
#[allow(dead_code)]
pub(crate) type DurableStructuredTypeJob = rue_air::ComptimeStructuredTypeJob<
    DurableStructuredTypeProgramCapability,
    ModuleId,
    crate::StableDefinitionKey,
    Arc<str>,
    crate::StableDefinitionKey,
    DurableType,
    DurableConstValue,
    lasso::Spur,
    Arc<[Arc<str>]>,
>;

#[allow(dead_code)]
pub(crate) type DurableStructuredTypePoll = rue_air::ComptimeStructuredTypePoll<
    DurableStructuredTypeProgramCapability,
    ModuleId,
    crate::StableDefinitionKey,
    Arc<str>,
    crate::StableDefinitionKey,
    DurableType,
    DurableConstValue,
    lasso::Spur,
    Arc<[Arc<str>]>,
>;

pub(crate) type DurableStructuredTypeAuthority = rue_air::ComptimeStructuredTypeAuthority<
    DurableStructuredTypeProgramCapability,
    ModuleId,
    lasso::Spur,
    Arc<[Arc<str>]>,
>;

/// The immutable request contract copied from a canonical AIR job. The job
/// itself remains owned by AIR for resume and never enters the probe package.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DurableStructuredTypeProgramCapability {
    key: crate::body_query::DurableComptimeProgramKey,
    pub(super) owner: u64,
}

impl DurableStructuredTypeProgramCapability {
    pub(super) fn new(key: crate::body_query::DurableComptimeProgramKey, owner: u64) -> Self {
        Self { key, owner }
    }

    pub(crate) fn key(&self) -> &crate::body_query::DurableComptimeProgramKey {
        &self.key
    }
}

#[allow(dead_code)]
pub(super) struct DurableStructuredTypeRequest {
    pub(super) program: crate::body_query::DurableComptimeProgramKey,
    pub(super) head_key: crate::StableDefinitionKey,
    pub(super) parameters: Arc<[rue_air::SemanticTypeConstructorParameter<Arc<str>>]>,
    pub(super) returns_type: bool,
    pub(super) type_arguments: Vec<(Arc<str>, DurableType)>,
    pub(super) value_arguments: Vec<(Arc<str>, DurableConstValue)>,
    pub(super) call_span: rue_span::Span,
}

/// A structured-type call after its canonical AIR job has supplied the
/// immutable request facts. It does not own the AIR job, so the continuation
/// cannot be replayed or cross-paired by this coordinator.
#[allow(dead_code)]
pub(crate) struct DurableStructuredTypePendingCall {
    pub(super) request: DurableStructuredTypeRequest,
    pub(super) edge: DurableComptimeCallEdge,
}

/// A pending structured call after the exact callable contract has been
/// checked. The typed bindings are produced here, before a foreign query is
/// probed, so the eventual frame cannot be paired with a different request.
#[allow(dead_code)]
pub(crate) struct DurableStructuredTypeValidatedCall {
    pub(super) pending: DurableStructuredTypePendingCall,
    pub(super) admission: DurableComptimeCallableAdmission,
    pub(super) type_bindings: AHashMap<DurableComptimeName, DurableComptimeType>,
    pub(super) value_bindings: AHashMap<DurableComptimeName, EvaluatedSemanticConst>,
    pub(super) expected_result: DurableComptimeType,
}

/// The result of one and only one structured foreign probe. It is consuming
/// and non-cloneable by construction. The canonical AIR job remains owned by
/// the engine; the pending request snapshots only its keyed facts and the
/// original call span supplied separately by the engine.
#[allow(dead_code)]
pub(crate) struct DurableStructuredTypeProbedCall {
    pub(super) pending: DurableStructuredTypeValidatedCall,
    pub(super) lookup: ForeignComptimeCallLookup,
}

#[allow(dead_code)]
pub(crate) enum DurableStructuredTypeCall {
    Ready {
        result: crate::semantic_query_nucleus::ComptimeCallResultProjection,
    },
    Enter {
        program: crate::body_query::OwnedForeignComptimeProgram,
        frame: Box<DurableComptimeForeignFrame>,
        ticket: Box<DurableComptimeCallTicket>,
    },
    NotReady,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum DurableStructuredTypeBeginError<E, F> {
    UnregisteredProgram,
    InvalidProgramAuthority,
    Resolution(rue_air::SemanticTypeSyntaxError<E, F, crate::StableDefinitionKey, Arc<str>>),
}

/// Begin the canonical AIR structured resolver against an owning program.
/// This is deliberately generic over the existing semantic provider so the
/// adapter adds no second type-syntax traversal or query authority.
#[allow(dead_code)]
pub(crate) fn begin_durable_structured_type<Q>(
    session: &DurableComptimeSession,
    key: &crate::body_query::DurableComptimeProgramKey,
    root: rue_rir::RirTypeSyntaxRef,
    type_substitutions: Vec<(Arc<str>, DurableType)>,
    value_substitutions: Vec<(Arc<str>, DurableConstValue)>,
    provider: &mut Q,
) -> Result<DurableStructuredTypePoll, DurableStructuredTypeBeginError<Q::Abort, Q::Failure>>
where
    Q: rue_air::SemanticTypeSyntaxProvider<
            ModuleId,
            ModuleId,
            crate::StableDefinitionKey,
            crate::StableDefinitionKey,
            Arc<str>,
            DurableType,
            DurableConstValue,
        >,
{
    let Some(authority) = session.structured_type_authority(key, root) else {
        if session.registered_program(key).is_none() {
            return Err(DurableStructuredTypeBeginError::UnregisteredProgram);
        }
        return Err(DurableStructuredTypeBeginError::InvalidProgramAuthority);
    };
    DurableStructuredTypeJob::begin::<ModuleId, Q>(
        provider,
        authority,
        type_substitutions,
        value_substitutions,
    )
    .map_err(DurableStructuredTypeBeginError::Resolution)
}

/// Resume one consuming canonical structured continuation. The reduced call
/// result is supplied by the same engine that owns the enclosing expression.
#[allow(dead_code)]
pub(crate) fn resume_durable_structured_type<Q>(
    job: DurableStructuredTypeJob,
    provider: &mut Q,
    reduced: rue_air::SemanticProviderResult<
        Option<rue_air::SemanticComptimeCallResult<DurableType, DurableConstValue>>,
        Q::Abort,
        Q::Failure,
    >,
) -> Result<
    DurableStructuredTypePoll,
    rue_air::SemanticTypeSyntaxError<Q::Abort, Q::Failure, crate::StableDefinitionKey, Arc<str>>,
>
where
    Q: rue_air::SemanticTypeSyntaxProvider<
            ModuleId,
            ModuleId,
            crate::StableDefinitionKey,
            crate::StableDefinitionKey,
            Arc<str>,
            DurableType,
            DurableConstValue,
        >,
{
    job.resume::<ModuleId, Q>(provider, reduced)
}

#[cfg(test)]
pub(super) mod structured_type_adapter_tests {
    use super::*;
    use crate::durable_semantics::{DurableParameterMode, DurableSemanticParameter};
    use std::cell::{Cell, RefCell};
    use std::convert::Infallible;

    fn assert_frame_domains<
        V: rue_air::ComptimeValue<Type = T>,
        T: rue_air::ComptimeType,
        N: rue_air::ComptimeName,
        F: rue_air::ComptimeFile,
        P: Clone,
        I: rue_air::ComptimeIdentity,
    >(
        _frame: Option<rue_air::ComptimeFrame<V, T, N, F, P, I>>,
    ) {
    }

    struct Provider {
        scope: ModuleId,
    }

    impl rue_air::SemanticModulePathProvider<ModuleId, ModuleId, crate::StableDefinitionKey>
        for Provider
    {
        type Abort = Infallible;
        type Failure = Infallible;

        fn root_module_binding(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticModuleBinding<ModuleId, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_binding(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticModuleBinding<ModuleId, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_display_name(&self, module: &ModuleId) -> Arc<str> {
            Arc::from(module.as_str())
        }

        fn accessing_domain(&self, scope: &ModuleId) -> rue_air::SemanticVisibilityDomain {
            assert_eq!(
                scope, &self.scope,
                "the registry key supplies the exact root scope"
            );
            rue_air::SemanticVisibilityDomain::from_file_path(Some(scope.as_str()))
        }
    }

    #[rustfmt::skip]
    impl rue_air::SemanticTypeSyntaxProvider<ModuleId, ModuleId, crate::StableDefinitionKey, crate::StableDefinitionKey, Arc<str>, DurableType, DurableConstValue> for Provider {
        fn substituted_type(
            &mut self,
            scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<Option<DurableType>, Self::Abort, Self::Failure>
        {
            assert_eq!(scope, &self.scope);
            Ok(None)
        }

        fn primitive_type(
            &mut self,
            name: &str,
        ) -> rue_air::SemanticProviderResult<Option<DurableType>, Self::Abort, Self::Failure>
        {
            Ok(match name {
                "i32" => Some(DurableType::I32),
                "i64" => Some(DurableType::I64),
                _ => None,
            })
        }

        fn builtin_type(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<Option<DurableType>, Self::Abort, Self::Failure>
        {
            Ok(None)
        }

        fn root_struct_type(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn root_enum_type(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn root_type_alias(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_struct_type(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_enum_type(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_type_alias(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn resolve_array_length(
            &mut self,
            _scope: &ModuleId,
            _length: rue_air::SemanticValueSyntax<'_>,
        ) -> rue_air::SemanticProviderResult<Option<u64>, Self::Abort, Self::Failure> {
            unreachable!("fixture has no array syntax")
        }

        fn array_length_from_value(
            &mut self,
            _scope: &ModuleId,
            _value: &DurableConstValue,
        ) -> rue_air::SemanticProviderResult<Option<u64>, Self::Abort, Self::Failure> {
            unreachable!("fixture has no array syntax")
        }

        fn array_type(
            &mut self,
            _element: DurableType,
            _length: Option<u64>,
        ) -> rue_air::SemanticProviderResult<DurableType, Self::Abort, Self::Failure> {
            unreachable!("fixture has no array syntax")
        }

        fn ptr_const_type(
            &mut self,
            _pointee: DurableType,
        ) -> rue_air::SemanticProviderResult<DurableType, Self::Abort, Self::Failure> {
            unreachable!("fixture has no pointer syntax")
        }

        fn ptr_mut_type(
            &mut self,
            _pointee: DurableType,
        ) -> rue_air::SemanticProviderResult<DurableType, Self::Abort, Self::Failure> {
            unreachable!("fixture has no pointer syntax")
        }

        fn slice_type(
            &mut self,
            _scope: &ModuleId,
            _syntax: &str,
            _element: DurableType,
        ) -> rue_air::SemanticProviderResult<DurableType, Self::Abort, Self::Failure> {
            unreachable!("fixture has no slice syntax")
        }

        fn builtin_type_call(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
            _arguments: &[rue_air::SemanticValueSyntax<'_>],
        ) -> rue_air::SemanticProviderResult<Option<DurableType>, Self::Abort, Self::Failure>
        {
            Ok(None)
        }

        fn root_constructor(
            &mut self,
            scope: &ModuleId,
            name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<
                rue_air::SemanticTypeConstructorHead<
                    crate::StableDefinitionKey,
                    Arc<str>,
                    crate::StableDefinitionKey,
                >,
            >,
            Self::Abort,
            Self::Failure,
        > {
            assert_eq!(scope, &self.scope);
            if name != "Wrap" {
                return Ok(None);
            }
            let interleaved = scope.as_str().contains("structured-ordered-frame");
            let constructor = crate::StableDefinitionKey::from_stable_parts(
                scope.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::Function,
                name,
                None,
            );
            Ok(Some(rue_air::SemanticTypeConstructorHead {
                key: constructor.clone(),
                site: constructor,
                parameters: if interleaved {
                    Arc::from([
                        rue_air::SemanticTypeConstructorParameter {
                            name: Arc::from("T0"),
                            is_comptime: true,
                            is_type: true,
                        },
                        rue_air::SemanticTypeConstructorParameter {
                            name: Arc::from("x0"),
                            is_comptime: true,
                            is_type: false,
                        },
                        rue_air::SemanticTypeConstructorParameter {
                            name: Arc::from("T1"),
                            is_comptime: true,
                            is_type: true,
                        },
                        rue_air::SemanticTypeConstructorParameter {
                            name: Arc::from("x1"),
                            is_comptime: true,
                            is_type: false,
                        },
                    ])
                } else {
                    Arc::from([rue_air::SemanticTypeConstructorParameter {
                        name: Arc::from("T"),
                        is_comptime: true,
                        is_type: true,
                    }])
                },
                returns_type: true,
                is_public: true,
                defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                    scope.as_str(),
                )),
                defining_file: Arc::from(scope.as_str()),
            }))
        }

        fn module_constructor(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<
                rue_air::SemanticTypeConstructorHead<
                    crate::StableDefinitionKey,
                    Arc<str>,
                    crate::StableDefinitionKey,
                >,
            >,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn resolve_value_argument(
            &mut self,
            scope: &ModuleId,
            _constructor: &str,
            _head: &rue_air::SemanticTypeConstructorHead<
                crate::StableDefinitionKey,
                Arc<str>,
                crate::StableDefinitionKey,
            >,
            _parameter_index: usize,
            _type_arguments: &[(Arc<str>, DurableType)],
            _value_arguments: &[(Arc<str>, DurableConstValue)],
            _syntax: rue_air::SemanticValueSyntax<'_>,
        ) -> rue_air::SemanticProviderResult<DurableConstValue, Self::Abort, Self::Failure>
        {
            if scope.as_str().contains("structured-ordered-frame") {
                return match _syntax {
                    rue_air::SemanticValueSyntax::Integer(value) => {
                        Ok(DurableConstValue::Integer(value))
                    }
                    rue_air::SemanticValueSyntax::Name(_) => Ok(DurableConstValue::Integer(0)),
                };
            }
            unreachable!("fixture constructor has no value argument")
        }

        fn reduce_comptime_call(
            &mut self,
            _head: &rue_air::SemanticTypeConstructorHead<
                crate::StableDefinitionKey,
                Arc<str>,
                crate::StableDefinitionKey,
            >,
            _type_arguments: &[(Arc<str>, DurableType)],
            _value_arguments: &[(Arc<str>, DurableConstValue)],
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticComptimeCallResult<DurableType, DurableConstValue>>,
            Self::Abort,
            Self::Failure,
        > {
            unreachable!("the durable host supplies the reduced call result on resume")
        }
    }

    pub(crate) fn const_program(
        path: &str,
        argument: &str,
    ) -> Arc<crate::body_query::OwnedComptimeProgramCore> {
        let snapshot = crate::SourceSnapshot::single(
            path,
            format!("const target: Wrap({argument}) = @import(\"{path}\");"),
        )
        .unwrap();
        let module = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
            .unwrap()
            .modules()[0]
            .clone();
        let candidate = module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|candidate| candidate.name.as_ref() == "target")
            .unwrap()
            .clone();
        let artifacts =
            crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || {
                Ok(())
            })
            .unwrap();
        let key = crate::body_query::DurableComptimeProgramKey {
            declaration: crate::StableDefinitionKey::from_stable_parts(
                candidate.module.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::ValueConst,
                "target",
                None,
            ),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: rue_target::Target::X86_64Linux,
                preview_features: crate::StablePreviewFeatures::new(
                    &crate::PreviewFeatures::default(),
                ),
            },
        };
        crate::body_query::OwnedComptimeProgramCore::from_const_body_plan(
            crate::body_query::DurableComptimeProgramPlan { key, candidate },
            &artifacts,
            || Ok(()),
        )
        .unwrap()
    }

    fn const_program_without_imports(
        path: &str,
        argument: &str,
    ) -> Arc<crate::body_query::OwnedComptimeProgramCore> {
        let snapshot = crate::SourceSnapshot::single(
            path,
            format!("const target: Wrap({argument}) = @import(\"{path}\");"),
        )
        .unwrap();
        let module = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
            .unwrap()
            .modules()[0]
            .clone();
        let candidate = module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|candidate| candidate.name.as_ref() == "target")
            .unwrap()
            .clone();
        let artifacts =
            crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || {
                Ok(())
            })
            .unwrap();
        let key = crate::body_query::DurableComptimeProgramKey {
            declaration: crate::StableDefinitionKey::from_stable_parts(
                candidate.module.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::ValueConst,
                "target",
                None,
            ),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: rue_target::Target::X86_64Linux,
                preview_features: crate::StablePreviewFeatures::new(
                    &crate::PreviewFeatures::default(),
                ),
            },
        };
        crate::body_query::OwnedComptimeProgramCore::from_const_body_plan_without_imports(
            crate::body_query::DurableComptimeProgramPlan { key, candidate },
            &artifacts,
            || Ok(()),
        )
        .unwrap()
    }

    pub(crate) fn callable_program(path: &str) -> Arc<crate::body_query::OwnedComptimeProgramCore> {
        callable_program_named(path, "target")
    }

    fn callable_program_named(
        path: &str,
        name: &str,
    ) -> Arc<crate::body_query::OwnedComptimeProgramCore> {
        let source = if path.contains("structured-ordered-frame") {
            format!(
                "fn {name}(comptime T0: type, comptime x0: i32, comptime T1: type, comptime x1: i64) -> type {{ [T0; x0] }}"
            )
        } else {
            format!("fn {name}() -> i32 {{ 1 }}")
        };
        let snapshot = crate::SourceSnapshot::single(path, source).unwrap();
        let module = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
            .unwrap()
            .modules()[0]
            .clone();
        let candidate = module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|candidate| candidate.name.as_ref() == name)
            .unwrap()
            .clone();
        let artifacts =
            crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || {
                Ok(())
            })
            .unwrap();
        let producer = crate::StableDefinitionKey::from_stable_parts(
            candidate.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            name,
            None,
        );
        let program = crate::body_query::OwnedForeignComptimeProgram::from_body_plan(
            crate::body_query::DurableComptimeProgramPlan {
                key: crate::body_query::DurableComptimeProgramKey {
                    declaration: producer,
                    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                        target: rue_target::Target::X86_64Linux,
                        preview_features: crate::StablePreviewFeatures::new(
                            &crate::PreviewFeatures::default(),
                        ),
                    },
                },
                candidate,
            },
            &artifacts,
            crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            },
            || Ok(()),
        )
        .unwrap();
        program.core
    }

    pub(crate) fn session() -> DurableComptimeSession {
        let module = ModuleId::from_logical_path("structured-parent.rue").unwrap();
        let producer = crate::StableDefinitionKey::from_stable_parts(
            module,
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "parent",
            None,
        );
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&producer)
                .unwrap();
        DurableComptimeSession::new(producer, declaration).unwrap()
    }

    fn fresh_session() -> DurableComptimeSession {
        session()
    }

    fn bound_call(
        admitted: &DurableComptimeAdmittedCall,
        value: Option<i128>,
    ) -> DurableComptimeBoundCall {
        let mut binding = DurableComptimeBinding::new(admitted);
        if let Some(value) = value {
            bind_durable_comptime_argument(
                &mut binding,
                "T",
                &crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from("T"),
                    ty: DurableType::ComptimeType,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                    bounds: Arc::from([]),
                },
                TypedSemanticConst {
                    value: DurableConstValue::Type(DurableType::I32),
                    ty: Some(DurableType::ComptimeType),
                },
                false,
            )
            .unwrap();
            bind_durable_comptime_argument(
                &mut binding,
                "x",
                &crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from("x"),
                    ty: DurableType::I32,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                    bounds: Arc::from([]),
                },
                TypedSemanticConst {
                    value: DurableConstValue::Integer(value),
                    ty: Some(DurableType::I32),
                },
                false,
            )
            .unwrap();
        }
        binding.finish()
    }

    fn test_admitted(
        admission: DurableComptimeCallableAdmission,
        ordinal: u32,
    ) -> DurableComptimeAdmittedCall {
        super::lifecycle::test_support::admitted_call_fixture(admission, ordinal)
    }

    fn prepare_call(
        session: &mut DurableComptimeSession,
        ordinal: u32,
        admission: DurableComptimeCallableAdmission,
        value: Option<i128>,
    ) -> DurableComptimePendingCall {
        let admitted = admitted_call(session, ordinal, admission);
        let bound = bound_call(&admitted, value);
        session
            .prepare_bound_expression_call(admitted, bound)
            .unwrap()
    }

    fn admitted_call(
        session: &mut DurableComptimeSession,
        ordinal: u32,
        admission: DurableComptimeCallableAdmission,
    ) -> DurableComptimeAdmittedCall {
        while session.next_call_ordinal_for_test() < ordinal {
            let _ = session.reserve_bound_expression_call();
        }
        let reservation = session.reserve_bound_expression_call();
        session
            .admit_bound_expression_call(reservation, admission)
            .unwrap()
    }

    struct PreparedProbeAuthority {
        calls: Cell<usize>,
        expected: RefCell<
            Vec<(
                Vec<(Arc<str>, DurableType)>,
                Vec<(Arc<str>, DurableConstValue)>,
            )>,
        >,
        lookups: RefCell<Vec<ForeignComptimeCallLookup>>,
        abort: Cell<bool>,
    }

    impl DurableComptimeForeignCallAuthority for PreparedProbeAuthority {
        fn probe_comptime_call(
            &self,
            _producer: &crate::StableDefinitionKey,
            type_arguments: &[(Arc<str>, DurableType)],
            value_arguments: &[(Arc<str>, DurableConstValue)],
        ) -> Result<ForeignComptimeCallLookup, QueryAbort> {
            self.calls.set(self.calls.get() + 1);
            if self.abort.get() {
                return Err(QueryAbort::Canceled);
            }
            let (expected_types, expected_values) = self.expected.borrow_mut().remove(0);
            assert_eq!(type_arguments, expected_types.as_slice());
            assert_eq!(value_arguments, expected_values.as_slice());
            Ok(self.lookups.borrow_mut().remove(0))
        }
    }

    fn prepared_admission(
        core: &crate::body_query::OwnedComptimeProgramCore,
    ) -> DurableComptimeCallableAdmission {
        DurableComptimeCallableAdmission {
            candidate: core.plan.candidate.clone(),
            identity: crate::semantic_query_nucleus::DeclarationIdentityProjection {
                key: core.plan.key.declaration.clone(),
                is_public: true,
            },
            configuration: core.plan.key.configuration.clone(),
            parameters: Arc::from([
                crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from("T"),
                    ty: DurableType::ComptimeType,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                    bounds: Arc::from([]),
                },
                crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from("x"),
                    ty: DurableType::I32,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                    bounds: Arc::from([]),
                },
            ]),
            result: DurableType::I32,
            shell_parameters: Arc::from([
                crate::declaration_candidate::DeclarationParameterHeader {
                    name: Arc::from("T"),
                    mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                    is_comptime: true,
                    is_type_parameter: true,
                },
                crate::declaration_candidate::DeclarationParameterHeader {
                    name: Arc::from("x"),
                    mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                    is_comptime: true,
                    is_type_parameter: false,
                },
            ]),
        }
    }

    fn structured_admission(
        pending: &DurableStructuredTypePendingCall,
    ) -> DurableComptimeCallableAdmission {
        let request = &pending.request;
        let parameters: Arc<[_]> = request
            .parameters
            .iter()
            .map(|parameter| DurableSemanticParameter {
                name: parameter.name.clone(),
                ty: if parameter.is_type {
                    DurableType::ComptimeType
                } else {
                    DurableType::I32
                },
                mode: DurableParameterMode::Value,
                is_comptime: parameter.is_comptime,
                bounds: Arc::from([]),
            })
            .collect::<Vec<_>>()
            .into();
        let shell_parameters: Arc<[_]> = request
            .parameters
            .iter()
            .map(
                |parameter| crate::declaration_candidate::DeclarationParameterHeader {
                    name: parameter.name.clone(),
                    mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                    is_comptime: parameter.is_comptime,
                    is_type_parameter: parameter.is_type,
                },
            )
            .collect::<Vec<_>>()
            .into();
        DurableComptimeCallableAdmission {
            candidate: crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &request.head_key,
            )
            .unwrap(),
            identity: crate::semantic_query_nucleus::DeclarationIdentityProjection {
                key: request.head_key.clone(),
                is_public: true,
            },
            configuration: request.program.configuration.clone(),
            parameters,
            result: DurableType::ComptimeType,
            shell_parameters,
        }
    }

    fn ordered_admission(
        core: &crate::body_query::OwnedComptimeProgramCore,
    ) -> DurableComptimeCallableAdmission {
        let mut admission = prepared_admission(core);
        admission.parameters = Arc::from([
            crate::durable_semantics::DurableSemanticParameter {
                name: Arc::from("T0"),
                ty: DurableType::ComptimeType,
                mode: crate::durable_semantics::DurableParameterMode::Value,
                is_comptime: true,
                bounds: Arc::from([]),
            },
            crate::durable_semantics::DurableSemanticParameter {
                name: Arc::from("T1"),
                ty: DurableType::ComptimeType,
                mode: crate::durable_semantics::DurableParameterMode::Value,
                is_comptime: true,
                bounds: Arc::from([]),
            },
            crate::durable_semantics::DurableSemanticParameter {
                name: Arc::from("x0"),
                ty: DurableType::I32,
                mode: crate::durable_semantics::DurableParameterMode::Value,
                is_comptime: true,
                bounds: Arc::from([]),
            },
            crate::durable_semantics::DurableSemanticParameter {
                name: Arc::from("x1"),
                ty: DurableType::I64,
                mode: crate::durable_semantics::DurableParameterMode::Value,
                is_comptime: true,
                bounds: Arc::from([]),
            },
        ]);
        admission.shell_parameters = Arc::from([
            crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("T0"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: true,
            },
            crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("T1"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: true,
            },
            crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("x0"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: false,
            },
            crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("x1"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: false,
            },
        ]);
        admission
    }

    fn ordered_bound_call(
        admitted: &DurableComptimeAdmittedCall,
        reverse_types: bool,
        reverse_values: bool,
    ) -> DurableComptimeBoundCall {
        let mut binding = DurableComptimeBinding::new(admitted);
        let types = [("T0", DurableType::I32), ("T1", DurableType::I64)];
        let values = [
            ("x0", DurableConstValue::Integer(10), DurableType::I32),
            ("x1", DurableConstValue::Integer(20), DurableType::I64),
        ];
        let type_order: Vec<_> = if reverse_types {
            types.into_iter().rev().collect()
        } else {
            types.into_iter().collect()
        };
        for (name, ty) in type_order {
            bind_durable_comptime_argument(
                &mut binding,
                name,
                &crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from(name),
                    ty: DurableType::ComptimeType,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                    bounds: Arc::from([]),
                },
                TypedSemanticConst {
                    value: DurableConstValue::Type(ty),
                    ty: Some(DurableType::ComptimeType),
                },
                false,
            )
            .unwrap();
        }
        let value_order: Vec<_> = if reverse_values {
            values.into_iter().rev().collect()
        } else {
            values.into_iter().collect()
        };
        for (name, value, ty) in value_order {
            bind_durable_comptime_argument(
                &mut binding,
                name,
                &crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from(name),
                    ty: ty.clone(),
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                    bounds: Arc::from([]),
                },
                TypedSemanticConst {
                    value,
                    ty: Some(ty),
                },
                false,
            )
            .unwrap();
        }
        binding.finish()
    }

    fn prepared_authority(
        expected_types: Vec<(Arc<str>, DurableType)>,
        expected_values: Vec<(Arc<str>, DurableConstValue)>,
        lookup: ForeignComptimeCallLookup,
    ) -> PreparedProbeAuthority {
        PreparedProbeAuthority {
            calls: Cell::new(0),
            expected: RefCell::new(vec![(expected_types, expected_values)]),
            lookups: RefCell::new(vec![lookup]),
            abort: Cell::new(false),
        }
    }

    fn prepared_ready_projection(
        ordinal: u32,
    ) -> crate::semantic_query_nucleus::ComptimeCallProjection {
        crate::semantic_query_nucleus::ComptimeCallProjection {
            result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                DurableConstValue::Integer(ordinal.into()),
            ),
            anonymous_nominals: Arc::from([]),
            dependencies: Arc::from([]),
            deferred_ownership: Arc::from([prepared_gate(ordinal)]),
        }
    }

    fn prepared_gate(ordinal: u32) -> DeferredOwnershipGate {
        DeferredOwnershipGate {
            kind: crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireDroppable,
            ty: DurableType::I32,
            source: Arc::new(crate::semantic_query_nucleus::DeferredOwnershipGateSource {
                declaration:
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &prepared_definition("child"),
                    )
                    .unwrap(),
                start: ordinal,
                end: ordinal + 1,
            }),
            application: None,
        }
    }

    fn prepared_definition(name: &str) -> crate::StableDefinitionKey {
        crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("effects.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from(name),
            None,
        )
    }

    #[test]
    fn prepared_call_probe_is_one_shot_and_preserves_ready_ordinal_and_type() {
        let core = callable_program("prepared-ready.rue");
        let types = vec![(Arc::from("T"), DurableType::I32)];
        let values = vec![(Arc::from("x"), DurableConstValue::Integer(7))];
        let mut session = session();
        let admission = prepared_admission(&core);
        let pending = prepare_call(&mut session, 17, admission.clone(), Some(7));
        let mut authority = prepared_authority(
            types,
            values,
            ForeignComptimeCallLookup::Ready(prepared_ready_projection(17)),
        );
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(pending)
            .unwrap();
        let prepared = session
            .consume_probed_call(probed, rue_span::Span::new(3, 4))
            .unwrap();
        assert!(matches!(
            prepared,
            DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(17)
                ),
                expected_result: DurableType::I32,
            }
        ));
        assert_eq!(authority.calls.get(), 1);
        let effects = session.drain_root_effects().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
                .collect::<Vec<_>>(),
            vec![17]
        );
    }

    #[test]
    fn prepared_call_admission_keeps_frame_and_ticket_from_one_bound_payload() {
        let core = callable_program("prepared-enter.rue");
        let mut session = session();
        let admission = ordered_admission(&core);
        let admitted_call = admitted_call(&mut session, 21, admission.clone());
        let bound = ordered_bound_call(&admitted_call, false, false);
        let pending = session
            .prepare_bound_expression_call(admitted_call, bound)
            .unwrap();
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([
                    (Arc::from("T0"), DurableType::I32),
                    (Arc::from("T1"), DurableType::I64),
                ]),
                value_arguments: Arc::from([
                    (Arc::from("x0"), DurableConstValue::Integer(10)),
                    (Arc::from("x1"), DurableConstValue::Integer(20)),
                ]),
            },
        };
        let mut authority = prepared_authority(
            vec![
                (Arc::from("T0"), DurableType::I32),
                (Arc::from("T1"), DurableType::I64),
            ],
            vec![
                (Arc::from("x0"), DurableConstValue::Integer(10)),
                (Arc::from("x1"), DurableConstValue::Integer(20)),
            ],
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(pending)
            .unwrap();
        let prepared = session
            .consume_probed_call(probed, rue_span::Span::new(11, 15))
            .unwrap();
        let DurableComptimePreparedCall::Enter { frame, mut ticket } = prepared else {
            panic!("admitted prepared call must produce an AIR frame");
        };
        assert_eq!(frame.program, core.plan.key);
        assert_eq!(frame.span, rue_span::Span::new(11, 15));
        assert_eq!(
            frame.expected_result,
            Some(DurableComptimeType(DurableType::I32))
        );
        assert_eq!(
            ticket.canonical_function_producer(&core.plan.key).unwrap(),
            canonical_specialized_function_producer(
                &core.plan.key.declaration,
                &[
                    (Arc::from("T0"), DurableType::I32),
                    (Arc::from("T1"), DurableType::I64),
                ],
                &[
                    (Arc::from("x0"), DurableConstValue::Integer(10)),
                    (Arc::from("x1"), DurableConstValue::Integer(20)),
                ],
            )
            .unwrap()
        );
        let issued = ticket.canonical_function_producer(&core.plan.key).unwrap();
        let type_reversed = canonical_specialized_function_producer(
            &core.plan.key.declaration,
            &[
                (Arc::from("T1"), DurableType::I64),
                (Arc::from("T0"), DurableType::I32),
            ],
            &[
                (Arc::from("x0"), DurableConstValue::Integer(10)),
                (Arc::from("x1"), DurableConstValue::Integer(20)),
            ],
        )
        .unwrap();
        let value_reversed = canonical_specialized_function_producer(
            &core.plan.key.declaration,
            &[
                (Arc::from("T0"), DurableType::I32),
                (Arc::from("T1"), DurableType::I64),
            ],
            &[
                (Arc::from("x1"), DurableConstValue::Integer(20)),
                (Arc::from("x0"), DurableConstValue::Integer(10)),
            ],
        )
        .unwrap();
        assert_ne!(
            issued, type_reversed,
            "type stream order affects ticket identity"
        );
        assert_ne!(
            issued, value_reversed,
            "value stream order affects ticket identity"
        );
        assert_eq!(
            frame.type_bindings,
            AHashMap::from([
                (
                    DurableComptimeName::from("T0"),
                    DurableComptimeType(DurableType::I32),
                ),
                (
                    DurableComptimeName::from("T1"),
                    DurableComptimeType(DurableType::I64),
                ),
            ])
        );
        assert_eq!(
            frame.value_bindings,
            AHashMap::from([
                (
                    DurableComptimeName::from("x0"),
                    EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        DurableConstValue::Integer(10),
                        DurableType::I32,
                    )),
                ),
                (
                    DurableComptimeName::from("x1"),
                    EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        DurableConstValue::Integer(20),
                        DurableType::I64,
                    )),
                ),
            ])
        );
        assert_eq!(session.active_call_count_for_test(), 0);
        session.enter_call(&ticket).unwrap();
        session
            .finish_call(&mut ticket, &rue_air::ComptimeOutcome::<(), ()>::Known(()))
            .unwrap();
        assert!(session.drain_root_effects().unwrap().is_empty());
        assert_eq!(authority.calls.get(), 1);
    }

    #[test]
    fn prepared_call_rejects_cross_paired_admitted_authority_before_registration() {
        let admitted_core = callable_program("prepared-admitted.rue");
        let pending_core = callable_program("prepared-pending.rue");
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: admitted_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([(Arc::from("x"), DurableConstValue::Integer(9))]),
            },
        };
        let mut session = session();
        let admission = prepared_admission(&pending_core);
        let pending = prepare_call(&mut session, 22, admission.clone(), Some(9));
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(9))],
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(pending)
            .unwrap();
        assert!(matches!(
            session.consume_probed_call(probed, rue_span::Span::new(22, 24)),
            Err(DurableComptimeForeignCallError::FrameAdmission(
                DurableComptimeForeignFrameAdmissionError::RegistryMismatch
            ))
        ));
        assert!(
            session
                .registered_program(&admitted_core.plan.key)
                .is_none()
        );
        assert_eq!(session.active_call_count_for_test(), 0);

        // Even identical semantic admissions receive distinct call tokens;
        // crossing sibling bound payloads is rejected before an edge exists.
        let first = admitted_call(&mut session, 27, admission.clone());
        let second = admitted_call(&mut session, 28, admission);
        let second_bound = bound_call(&second, Some(2));
        assert!(matches!(
            session.prepare_bound_expression_call(first, second_bound),
            Err(DurableComptimeLifecycleError::BindingMismatch)
        ));
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.drain_root_effects().unwrap().is_empty());
        assert_eq!(authority.calls.get(), 1);
    }

    #[test]
    fn prepared_call_rejects_crossed_admission_contract_before_edge_issuance() {
        let core = callable_program("prepared-contract.rue");
        let admission = prepared_admission(&core);
        let mut session = session();

        let mut wrong_result = admission.clone();
        wrong_result.result = DurableType::I64;
        assert!(matches!(
            {
                let admitted = admitted_call(&mut session, 23, admission.clone());
                let wrong = admitted_call(&mut session, 25, wrong_result.clone());
                session.prepare_bound_expression_call(admitted, bound_call(&wrong, Some(1)))
            },
            Err(DurableComptimeLifecycleError::BindingMismatch)
        ));
        assert_eq!(session.active_call_count_for_test(), 0);

        let mut wrong_configuration = admission.clone();
        wrong_configuration.configuration.target = rue_target::Target::Aarch64Linux;
        assert!(matches!(
            {
                let admitted = admitted_call(&mut session, 24, admission);
                let wrong = admitted_call(&mut session, 26, wrong_configuration);
                session.prepare_bound_expression_call(admitted, bound_call(&wrong, Some(1)))
            },
            Err(DurableComptimeLifecycleError::BindingMismatch)
        ));
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn prepared_call_preserves_two_ordered_type_and_value_streams() {
        let core = callable_program("prepared-ordered-streams.rue");
        let admission = ordered_admission(&core);
        let mut session = session();
        let first = admitted_call(&mut session, 0, admission.clone());
        let second = admitted_call(&mut session, 1, admission);
        let first_bound = ordered_bound_call(&first, false, false);
        let swapped_bound = ordered_bound_call(&second, true, true);
        let first_view = first_bound.query_view();
        let swapped_view = swapped_bound.query_view();
        assert_ne!(
            first_view.type_arguments(),
            swapped_view.type_arguments(),
            "type argument order is part of the query"
        );
        assert_ne!(
            first_view.value_arguments(),
            swapped_view.value_arguments(),
            "value argument order is part of the query"
        );
        assert!(matches!(
            session.prepare_bound_expression_call(first, swapped_bound),
            Err(DurableComptimeLifecycleError::BindingMismatch)
        ));
        assert_eq!(session.active_call_count_for_test(), 0);
    }

    #[test]
    fn prepared_call_siblings_keep_original_ordinals_and_recover_after_abort() {
        let core = callable_program("prepared-siblings.rue");
        let admission = prepared_admission(&core);
        let mut session = session();
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(1))],
            ForeignComptimeCallLookup::NotReady,
        );
        authority.abort.set(true);
        let aborted = prepare_call(&mut session, 31, admission.clone(), Some(1));
        assert!(matches!(
            DurableComptimeServices::new(&mut authority).probe_prepared_call(aborted),
            Err(QueryAbort::Canceled)
        ));
        authority.abort.set(false);
        authority.expected.borrow_mut().clear();
        authority.expected.borrow_mut().push((
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(2))],
        ));
        authority.lookups.borrow_mut().clear();
        authority
            .lookups
            .borrow_mut()
            .push(ForeignComptimeCallLookup::Ready(prepared_ready_projection(
                32,
            )));
        let sibling = prepare_call(&mut session, 32, admission.clone(), Some(2));
        let sibling = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(sibling)
            .unwrap();
        assert!(matches!(
            session.consume_probed_call(sibling, rue_span::Span::new(32, 33)),
            Ok(DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(32)
                ),
                ..
            })
        ));
        assert_eq!(authority.calls.get(), 2);
        let effects = session.drain_root_effects().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
                .collect::<Vec<_>>(),
            vec![32]
        );

        authority.expected.borrow_mut().extend([
            (
                vec![(Arc::from("T"), DurableType::I32)],
                vec![(Arc::from("x"), DurableConstValue::Integer(3))],
            ),
            (
                vec![(Arc::from("T"), DurableType::I32)],
                vec![(Arc::from("x"), DurableConstValue::Integer(4))],
            ),
        ]);
        authority.lookups.borrow_mut().extend([
            ForeignComptimeCallLookup::Ready(prepared_ready_projection(33)),
            ForeignComptimeCallLookup::Ready(prepared_ready_projection(34)),
        ]);
        let first = prepare_call(&mut session, 33, admission.clone(), Some(3));
        let second = prepare_call(&mut session, 34, admission.clone(), Some(4));
        let services = DurableComptimeServices::new(&mut authority);
        let first = services.probe_prepared_call(first).unwrap();
        let second = services.probe_prepared_call(second).unwrap();
        drop(services);
        assert!(matches!(
            session.consume_probed_call(second, rue_span::Span::new(34, 35)),
            Ok(DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(34)
                ),
                ..
            })
        ));
        assert!(matches!(
            session.consume_probed_call(first, rue_span::Span::new(33, 34)),
            Ok(DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(33)
                ),
                ..
            })
        ));
        let effects = session.drain_root_effects().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
                .collect::<Vec<_>>(),
            vec![33, 34]
        );
    }

    #[test]
    fn prepared_call_not_ready_then_successful_sibling_uses_one_session() {
        let core = callable_program("prepared-not-ready-sibling.rue");
        let admission = prepared_admission(&core);
        let mut session = session();
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(1))],
            ForeignComptimeCallLookup::NotReady,
        );
        let not_ready = prepare_call(&mut session, 40, admission.clone(), Some(1));
        let not_ready = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(not_ready)
            .unwrap();
        assert!(matches!(
            session.consume_probed_call(not_ready, rue_span::Span::new(40, 41)),
            Ok(DurableComptimePreparedCall::NotReady)
        ));

        authority.expected.borrow_mut().push((
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(2))],
        ));
        authority
            .lookups
            .borrow_mut()
            .push(ForeignComptimeCallLookup::Ready(prepared_ready_projection(
                41,
            )));
        let sibling = prepare_call(&mut session, 41, admission, Some(2));
        let sibling = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(sibling)
            .unwrap();
        assert!(matches!(
            session.consume_probed_call(sibling, rue_span::Span::new(41, 42)),
            Ok(DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(41)
                ),
                ..
            })
        ));
        assert_eq!(authority.calls.get(), 2);
        assert_eq!(
            session
                .drain_root_effects()
                .unwrap()
                .deferred_ownership()
                .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
                .collect::<Vec<_>>(),
            vec![41]
        );
    }

    #[test]
    fn prepared_call_terminals_are_consumed_without_retry_or_effects() {
        let terminals = vec![
            ForeignComptimeCallLookup::NotReady,
            ForeignComptimeCallLookup::ReadyFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(Arc::from("ready")),
            ),
            ForeignComptimeCallLookup::ReadyQueryFailure(rue_query::QueryFailure::new(
                "query", "ready",
            )),
            ForeignComptimeCallLookup::AdmissionFailure(
                crate::body_query::ComptimeProgramProjectionFailure::IdentityMismatch,
            ),
            ForeignComptimeCallLookup::UnexpectedReadyProjection,
        ];
        for lookup in terminals {
            let core = callable_program("prepared-terminal.rue");
            let mut session = session();
            let admission = prepared_admission(&core);
            let pending = prepare_call(&mut session, 29, admission.clone(), Some(1));
            let mut authority = prepared_authority(
                vec![(Arc::from("T"), DurableType::I32)],
                vec![(Arc::from("x"), DurableConstValue::Integer(1))],
                lookup,
            );
            let probed = DurableComptimeServices::new(&mut authority)
                .probe_prepared_call(pending)
                .unwrap();
            assert!(matches!(
                session.consume_probed_call(probed, rue_span::Span::new(1, 2)),
                Ok(DurableComptimePreparedCall::NotReady) | Err(_)
            ));
            assert_eq!(authority.calls.get(), 1);
            assert!(session.drain_root_effects().unwrap().is_empty());
        }

        let core = callable_program("prepared-abort.rue");
        let mut session = session();
        let admission = prepared_admission(&core);
        let pending = prepare_call(&mut session, 30, admission.clone(), Some(1));
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(1))],
            ForeignComptimeCallLookup::NotReady,
        );
        authority.abort.set(true);
        assert!(matches!(
            DurableComptimeServices::new(&mut authority).probe_prepared_call(pending),
            Err(QueryAbort::Canceled)
        ));
        assert_eq!(authority.calls.get(), 1);
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn durable_registry_owns_structured_jobs_across_colliding_programs() {
        let first = const_program("first.rue", "i32");
        let second = const_program("second.rue", "i64");
        let mut session = session();
        session.register_program(&first).unwrap();
        session.register_program(&second).unwrap();
        assert_eq!(
            session.register_program(&first),
            Err(rue_air::ComptimeProgramRegistrationError::AlreadyRegistered)
        );

        let (_, Some(first_root), _) = first.const_root().unwrap() else {
            panic!("first const retains its declared type root");
        };
        let (_, Some(second_root), _) = second.const_root().unwrap() else {
            panic!("second const retains its declared type root");
        };
        assert_eq!(first_root, second_root, "fixture uses colliding dense refs");

        let first_registered = session.registered_program(&first.plan.key).unwrap();
        let second_registered = session.registered_program(&second.plan.key).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first_registered.rir, &first.rir));
        assert!(std::sync::Arc::ptr_eq(&second_registered.rir, &second.rir));
        assert_ne!(first_registered.symbols, second_registered.symbols);
        assert_eq!(first_registered.imports.imports.len(), 1);
        assert_eq!(second_registered.imports.imports.len(), 1);
        assert_eq!(
            first_registered.imports.imports[0].specifier,
            Arc::<str>::from("first.rue")
        );
        assert_eq!(
            second_registered.imports.imports[0].specifier,
            Arc::<str>::from("second.rue")
        );
        let mut wrong_configuration = first.plan.key.configuration.clone();
        wrong_configuration.target = rue_target::Target::Aarch64Linux;
        let wrong_key = rue_air::ComptimeProgramKey {
            declaration: first.plan.key.declaration.clone(),
            configuration: wrong_configuration,
        };
        assert!(session.registered_program(&wrong_key).is_none());

        let mut first_provider = Provider {
            scope: first.plan.candidate.module.clone(),
        };
        let first_poll = begin_durable_structured_type(
            &session,
            &first.plan.key,
            first_root,
            Vec::new(),
            Vec::new(),
            &mut first_provider,
        )
        .unwrap();
        let DurableStructuredTypePoll::Suspended(first_job) = first_poll else {
            panic!("Wrap(i32) suspends for the durable call result");
        };
        assert_eq!(first_job.program().key(), &first.plan.key);
        assert_eq!(
            first_job.type_arguments(),
            &[(Arc::from("T"), DurableType::I32)]
        );
        let first_ready = resume_durable_structured_type(
            *first_job,
            &mut first_provider,
            Ok(Some(rue_air::SemanticComptimeCallResult::Type(
                DurableType::I64,
            ))),
        )
        .unwrap();
        assert!(matches!(
            first_ready,
            DurableStructuredTypePoll::Ready(DurableType::I64)
        ));

        let mut second_provider = Provider {
            scope: second.plan.candidate.module.clone(),
        };
        let second_poll = begin_durable_structured_type(
            &session,
            &second.plan.key,
            second_root,
            Vec::new(),
            Vec::new(),
            &mut second_provider,
        )
        .unwrap();
        let DurableStructuredTypePoll::Suspended(second_job) = second_poll else {
            panic!("colliding program independently suspends");
        };
        assert_eq!(second_job.program().key(), &second.plan.key);
        assert_ne!(second_job.program().key(), &first.plan.key);
        assert_eq!(
            second_job.type_arguments(),
            &[(Arc::from("T"), DurableType::I64)],
            "the second key selects the second arena despite its colliding root ref"
        );

        let mut missing_key = first.plan.key.clone();
        missing_key.declaration = crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("missing.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
            "target",
            None,
        );
        assert!(matches!(
            begin_durable_structured_type(
                &session,
                &missing_key,
                first_root,
                Vec::new(),
                Vec::new(),
                &mut first_provider,
            ),
            Err(DurableStructuredTypeBeginError::UnregisteredProgram)
        ));
        assert!(matches!(
            begin_durable_structured_type(
                &session,
                &first.plan.key,
                rue_rir::RirTypeSyntaxRef::from_u32(u32::MAX),
                Vec::new(),
                Vec::new(),
                &mut first_provider
            ),
            Err(DurableStructuredTypeBeginError::InvalidProgramAuthority)
        ));
    }

    #[test]
    fn structured_type_call_coordinator_consumes_job_and_probe_once() {
        let core = const_program("structured-call-coordinator.rue", "i32");
        let (_, Some(root), _) = core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&core).unwrap();
        let mut provider = Provider {
            scope: core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(9, 10))
            .unwrap();
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Ready(prepared_ready_projection(73)),
        );
        let admission = structured_admission(&pending);
        let validated = match session.validate_structured_type_call(pending, admission) {
            Ok(value) => value,
            Err(error) => panic!("structured fixture contract: {error:?}"),
        };
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let DurableStructuredTypeCall::Ready { result } =
            session.consume_structured_type_call(probed).unwrap()
        else {
            panic!("ready structured call must retain its job");
        };
        assert_eq!(job.program().key(), &core.plan.key);
        assert_eq!(job.type_arguments(), &[(Arc::from("T"), DurableType::I32)]);
        assert!(matches!(
            result,
            crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                DurableConstValue::Integer(73)
            )
        ));
        assert_eq!(authority.calls.get(), 1);
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(!session.drain_root_effects().unwrap().is_empty());

        // A second continuation proves NotReady is a terminal handoff that
        // retains the canonical job without retrying the probe or publishing.
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("second fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(11, 12))
            .unwrap();
        authority
            .expected
            .borrow_mut()
            .push((vec![(Arc::from("T"), DurableType::I32)], Vec::new()));
        authority
            .lookups
            .borrow_mut()
            .push(ForeignComptimeCallLookup::NotReady);
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let DurableStructuredTypeCall::NotReady =
            session.consume_structured_type_call(probed).unwrap()
        else {
            panic!("not-ready structured call must retain its job");
        };
        assert_eq!(authority.calls.get(), 2);
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn structured_type_call_preserves_failure_and_abort_without_publication() {
        let core = const_program("structured-call-terminals.rue", "i32");
        let (_, Some(root), _) = core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&core).unwrap();
        let mut provider = Provider {
            scope: core.plan.candidate.module.clone(),
        };
        let make_pending = |session: &mut DurableComptimeSession, provider: &mut Provider| {
            let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
                session,
                &core.plan.key,
                root,
                Vec::new(),
                Vec::new(),
                provider,
            )
            .unwrap() else {
                panic!("fixture structured call must suspend");
            };
            let pending = session
                .prepare_structured_type_call(&job, rue_span::Span::new(13, 14))
                .unwrap();
            let admission = structured_admission(&pending);
            session
                .validate_structured_type_call(pending, admission)
                .unwrap()
        };

        let mut failure_authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::ReadyFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(Arc::from(
                    "structured failure",
                )),
            ),
        );
        let probed = DurableComptimeServices::new(&mut failure_authority)
            .probe_structured_type_call(make_pending(&mut session, &mut provider))
            .unwrap();
        assert!(matches!(
            session.consume_structured_type_call(probed),
            Err(DurableComptimeForeignCallError::ReadyFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(_)
            ))
        ));
        assert_eq!(failure_authority.calls.get(), 1);
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.drain_root_effects().unwrap().is_empty());

        let mut abort_authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::NotReady,
        );
        abort_authority.abort.set(true);
        assert!(matches!(
            DurableComptimeServices::new(&mut abort_authority)
                .probe_structured_type_call(make_pending(&mut session, &mut provider)),
            Err(QueryAbort::Canceled)
        ));
        assert_eq!(abort_authority.calls.get(), 1);
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn structured_frame_validation_rejects_before_probe_or_registry_mutation() {
        let core = const_program("structured-frame-validation.rue", "i32");
        let (_, Some(root), _) = core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&core).unwrap();
        let mut provider = Provider {
            scope: core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(suspension) = begin_durable_structured_type(
            &session,
            &core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&suspension, rue_span::Span::new(90, 91))
            .unwrap();
        let mut invalid = structured_admission(&pending);
        invalid.result = DurableType::I32;
        let serial_before = session.next_lifecycle_serial_for_test();
        let programs_before = session.program_count_for_test();
        assert!(matches!(
            session.validate_structured_type_call(pending, invalid),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::ResultNotType
            ))
        ));
        assert_eq!(session.program_count_for_test(), programs_before);
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.root_effects_are_empty_for_test());
        assert_eq!(session.next_lifecycle_serial_for_test(), serial_before);
    }

    #[test]
    fn structured_non_callable_is_rejected_before_ticket_or_registration() {
        let root_core = const_program("structured-non-callable.rue", "i32");
        let callable_core = callable_program_named("structured-non-callable.rue", "Wrap");
        let (_, Some(root), _) = root_core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&root_core).unwrap();
        let mut provider = Provider {
            scope: root_core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(suspension) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&suspension, rue_span::Span::new(92, 93))
            .unwrap();
        let mut non_callable_core = (*root_core).clone();
        non_callable_core.plan.key = callable_core.plan.key.clone();
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: Arc::new(non_callable_core),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([]),
            },
        };
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let serial_before = session.next_lifecycle_serial_for_test();
        let programs_before = session.program_count_for_test();
        assert!(matches!(
            session.consume_structured_type_call(probed),
            Err(DurableComptimeForeignCallError::FrameAdmission(
                DurableComptimeForeignFrameAdmissionError::NotCallable
            ))
        ));
        assert_eq!(authority.calls.get(), 1);
        assert_eq!(session.next_lifecycle_serial_for_test(), serial_before);
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.root_effects_are_empty_for_test());
        assert_eq!(session.program_count_for_test(), programs_before);
        assert!(
            session
                .registered_program(&callable_core.plan.key)
                .is_none()
        );
    }

    #[test]
    fn structured_frame_validation_rejects_name_order_kind_and_value_fit() {
        let root_core = const_program("structured-frame-contract.rue", "i32");
        let callable_core = callable_program_named("structured-frame-contract.rue", "Wrap");
        let (_, Some(root), _) = root_core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&root_core).unwrap();
        let mut provider = Provider {
            scope: root_core.plan.candidate.module.clone(),
        };
        let pending = |session: &mut DurableComptimeSession, provider: &mut Provider| {
            let DurableStructuredTypePoll::Suspended(suspension) = begin_durable_structured_type(
                &*session,
                &root_core.plan.key,
                root,
                Vec::new(),
                Vec::new(),
                provider,
            )
            .unwrap() else {
                panic!("fixture structured call must suspend");
            };
            session
                .prepare_structured_type_call(&suspension, rue_span::Span::new(94, 95))
                .unwrap()
        };

        let name_pending = pending(&mut session, &mut provider);
        let mut name_admission = structured_admission(&name_pending);
        let mut name_parameter = name_admission.parameters[0].clone();
        name_parameter.name = Arc::from("Wrong");
        name_admission.parameters = Arc::from([name_parameter]);
        assert!(matches!(
            session.validate_structured_type_call(name_pending, name_admission),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::InvalidContract
            ))
        ));

        let kind_pending = pending(&mut session, &mut provider);
        let mut kind_admission = structured_admission(&kind_pending);
        let mut kind_shell = kind_admission.shell_parameters[0].clone();
        kind_shell.is_type_parameter = false;
        kind_admission.shell_parameters = Arc::from([kind_shell]);
        assert!(matches!(
            session.validate_structured_type_call(kind_pending, kind_admission),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::InvalidContract
            ))
        ));

        let mut order_pending = pending(&mut session, &mut provider);
        order_pending.request.parameters = Arc::from([
            rue_air::SemanticTypeConstructorParameter {
                name: Arc::from("T0"),
                is_comptime: true,
                is_type: true,
            },
            rue_air::SemanticTypeConstructorParameter {
                name: Arc::from("T1"),
                is_comptime: true,
                is_type: true,
            },
        ]);
        order_pending.request.type_arguments = vec![
            (Arc::from("T0"), DurableType::I32),
            (Arc::from("T1"), DurableType::I64),
        ];
        let mut order_admission = ordered_admission(&callable_core);
        order_admission.result = DurableType::ComptimeType;
        order_admission.parameters = Arc::from([
            order_admission.parameters[1].clone(),
            order_admission.parameters[0].clone(),
        ]);
        order_admission.shell_parameters = Arc::from([
            order_admission.shell_parameters[1].clone(),
            order_admission.shell_parameters[0].clone(),
        ]);
        assert!(matches!(
            session.validate_structured_type_call(order_pending, order_admission),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::InvalidContract
            ))
        ));

        let mut fit_pending = pending(&mut session, &mut provider);
        fit_pending.request.parameters = Arc::from([rue_air::SemanticTypeConstructorParameter {
            name: Arc::from("x"),
            is_comptime: true,
            is_type: false,
        }]);
        fit_pending.request.type_arguments.clear();
        fit_pending.request.value_arguments =
            vec![(Arc::from("x"), DurableConstValue::Integer(i128::MAX))];
        let mut fit_admission = structured_admission(&fit_pending);
        fit_admission.result = DurableType::ComptimeType;
        fit_admission.parameters = Arc::from([DurableSemanticParameter {
            name: Arc::from("x"),
            ty: DurableType::I32,
            mode: DurableParameterMode::Value,
            is_comptime: true,
            bounds: Arc::from([]),
        }]);
        fit_admission.shell_parameters =
            Arc::from([crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("x"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: false,
            }]);
        assert!(matches!(
            session.validate_structured_type_call(fit_pending, fit_admission),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::ValueFit(_)
            ))
        ));
    }

    #[test]
    fn structured_frame_admission_preserves_ordered_typed_bindings_and_lifecycle() {
        let root_core = const_program("structured-ordered-frame.rue", "i32, 10, i64, 20");
        let callable_core = callable_program_named("structured-ordered-frame.rue", "Wrap");
        let (_, Some(root), _) = root_core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&root_core).unwrap();
        let mut provider = Provider {
            scope: root_core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(suspension) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&suspension, rue_span::Span::new(1000, 1001))
            .unwrap();
        let mut admission = ordered_admission(&callable_core);
        admission.result = DurableType::ComptimeType;
        admission.parameters = Arc::from([
            admission.parameters[0].clone(),
            admission.parameters[2].clone(),
            admission.parameters[1].clone(),
            admission.parameters[3].clone(),
        ]);
        admission.shell_parameters = Arc::from([
            admission.shell_parameters[0].clone(),
            admission.shell_parameters[2].clone(),
            admission.shell_parameters[1].clone(),
            admission.shell_parameters[3].clone(),
        ]);
        assert_eq!(
            admission.candidate,
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &pending.request.head_key
            )
            .unwrap()
        );
        assert_eq!(admission.identity.key, pending.request.head_key);
        assert_eq!(
            admission.configuration,
            pending.request.program.configuration
        );
        assert_eq!(admission.parameters.len(), 4);
        assert_eq!(admission.shell_parameters.len(), 4);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: callable_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([
                    (Arc::from("T0"), DurableType::I32),
                    (Arc::from("T1"), DurableType::I64),
                ]),
                value_arguments: Arc::from([
                    (Arc::from("x0"), DurableConstValue::Integer(10)),
                    (Arc::from("x1"), DurableConstValue::Integer(20)),
                ]),
            },
        };
        let mut authority = prepared_authority(
            vec![
                (Arc::from("T0"), DurableType::I32),
                (Arc::from("T1"), DurableType::I64),
            ],
            vec![
                (Arc::from("x0"), DurableConstValue::Integer(10)),
                (Arc::from("x1"), DurableConstValue::Integer(20)),
            ],
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let DurableStructuredTypeCall::Enter {
            frame, mut ticket, ..
        } = session.consume_structured_type_call(probed).unwrap()
        else {
            panic!("ordered structured call must admit a frame");
        };
        assert_eq!(authority.calls.get(), 1);
        assert_eq!(frame.span, rue_span::Span::new(1000, 1001));
        assert_ne!(frame.span, frame.function_span);
        assert_eq!(frame.call_identity, None);
        assert_eq!(
            frame.type_bindings.get(&DurableComptimeName::from("T0")),
            Some(&DurableComptimeType::from(DurableType::I32))
        );
        assert_eq!(
            frame.type_bindings.get(&DurableComptimeName::from("T1")),
            Some(&DurableComptimeType::from(DurableType::I64))
        );
        assert_eq!(
            frame.value_bindings.get(&DurableComptimeName::from("x0")),
            Some(&EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(10),
                DurableType::I32,
            )))
        );
        assert_eq!(
            frame.value_bindings.get(&DurableComptimeName::from("x1")),
            Some(&EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(20),
                DurableType::I64,
            )))
        );
        assert_eq!(
            frame.expected_result,
            Some(DurableComptimeType::from(DurableType::ComptimeType))
        );
        assert_eq!(session.active_call_count_for_test(), 0);
        session.enter_call(&ticket).unwrap();
        assert_eq!(session.active_call_count_for_test(), 1);
        session
            .finish_call(&mut ticket, &rue_air::ComptimeOutcome::<(), ()>::Known(()))
            .unwrap();
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn structured_type_call_admission_returns_dormant_ticket_after_validation() {
        let root_core = const_program("structured-call-enter.rue", "i32");
        let callable_core = callable_program_named("structured-call-enter.rue", "Wrap");
        let (_, Some(root), _) = root_core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&root_core).unwrap();
        let mut provider = Provider {
            scope: root_core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(15, 16))
            .unwrap();
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: callable_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([]),
            },
        };
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let DurableStructuredTypeCall::Enter {
            program,
            frame,
            mut ticket,
        } = session.consume_structured_type_call(probed).unwrap()
        else {
            panic!("admitted structured call must issue a dormant ticket");
        };
        assert_eq!(program.plan.key, callable_core.plan.key);
        assert_eq!(frame.span, rue_span::Span::new(15, 16));
        assert_eq!(frame.call_identity, None);
        assert_eq!(
            frame.expected_result,
            Some(DurableComptimeType::from(DurableType::ComptimeType))
        );
        assert_eq!(
            frame.context.as_ref().unwrap().program(),
            &callable_core.plan.key
        );
        assert_eq!(
            frame.type_bindings.get(&DurableComptimeName::from("T")),
            Some(&DurableComptimeType::from(DurableType::I32))
        );
        assert!(frame.value_bindings.is_empty());
        assert!(
            session
                .registered_program(&callable_core.plan.key)
                .is_some()
        );
        assert_eq!(session.active_call_count_for_test(), 0);
        assert_eq!(
            ticket
                .canonical_function_producer(&callable_core.plan.key)
                .unwrap(),
            canonical_specialized_function_producer(
                &callable_core.plan.key.declaration,
                &[(Arc::from("T"), DurableType::I32)],
                &[],
            )
            .unwrap()
        );
        assert_eq!(
            session.finish_call(&mut ticket, &rue_air::ComptimeOutcome::<(), ()>::Known(())),
            Err(DurableComptimeLifecycleError::NotEntered)
        );
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.drain_root_effects().unwrap().is_empty());

        // A returned program with the wrong head contract is rejected only
        // after the dormant ticket is issued, and cannot enter the registry.
        let wrong_core = callable_program_named("structured-call-enter.rue", "Other");
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("second fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(17, 18))
            .unwrap();
        let wrong_program = crate::body_query::OwnedForeignComptimeProgram {
            core: wrong_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([]),
            },
        };
        let mut wrong_authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Admitted(wrong_program),
        );
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let probed = DurableComptimeServices::new(&mut wrong_authority)
            .probe_structured_type_call(validated)
            .unwrap();
        assert!(matches!(
            session.consume_structured_type_call(probed),
            Err(DurableComptimeForeignCallError::FrameAdmission(
                DurableComptimeForeignFrameAdmissionError::RegistryMismatch
            ))
        ));
        assert!(session.registered_program(&wrong_core.plan.key).is_none());
        assert_eq!(session.active_call_count_for_test(), 0);

        // An edge issued by another session fails before any registry
        // mutation, even when its probe carries an otherwise valid admitted
        // child.  This exercises the full ticket/authority handoff rather
        // than only the Ready projection path.
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("third fixture structured call must suspend");
        };

        // A capability issued by this session cannot be prepared by another
        // session, even when that session has registered the same stable key.
        // Reject before the foreign session issues a serial/edge or changes
        // its lifecycle/effect state.
        let mut other_session = fresh_session();
        other_session.register_program(&root_core).unwrap();
        let serial_before = other_session.next_lifecycle_serial_for_test();
        let active_before = other_session.active_calls_for_test();
        assert!(matches!(
            other_session.prepare_structured_type_call(&job, rue_span::Span::new(19, 20)),
            Err(DurableComptimeLifecycleError::InvalidProgramAuthority)
        ));
        assert_eq!(
            other_session.next_lifecycle_serial_for_test(),
            serial_before
        );
        assert_eq!(other_session.active_calls_for_test(), active_before);
        assert!(other_session.root_effects_are_empty_for_test());
        assert!(
            other_session
                .registered_program(&root_core.plan.key)
                .is_some()
        );

        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(19, 20))
            .unwrap();
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let mut foreign_session = fresh_session();
        let foreign_program = crate::body_query::OwnedForeignComptimeProgram {
            core: callable_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([]),
            },
        };
        let mut foreign_authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Admitted(foreign_program),
        );
        let probed = DurableComptimeServices::new(&mut foreign_authority)
            .probe_structured_type_call(validated)
            .unwrap();
        assert!(matches!(
            foreign_session.consume_structured_type_call(probed),
            Err(DurableComptimeForeignCallError::Lifecycle(
                DurableComptimeLifecycleError::TicketMismatch
            ))
        ));
        assert!(
            foreign_session
                .registered_program(&root_core.plan.key)
                .is_none()
        );
        assert_eq!(foreign_session.active_call_count_for_test(), 0);
    }

    #[test]
    fn keyed_import_sites_preserve_program_local_occurrences_and_reject_mismatches() {
        let first = const_program("first-import.rue", "i32");
        let second = const_program("second-import.rue", "i64");
        let mut session = session();
        session.register_program(&first).unwrap();
        session.register_program(&second).unwrap();

        let first_registered = session.registered_program(&first.plan.key).unwrap();
        let second_registered = session.registered_program(&second.plan.key).unwrap();
        let first_occurrence = first_registered.imports.imports[0].inst;
        let second_occurrence = second_registered.imports.imports[0].inst;
        assert_eq!(first_occurrence, second_occurrence);

        let first_site = session
            .import_site_for_instruction(&first.plan.key, first_occurrence, "first-import.rue")
            .unwrap();
        assert_eq!(first_site.occurrence(), 0);
        assert_eq!(first_site.kind(), rue_air::ComptimeSiteKind::Import);
        assert_eq!(first_site.program(), &first.plan.key);

        let second_site = session
            .import_site_for_instruction(&second.plan.key, second_occurrence, "second-import.rue")
            .unwrap();
        assert_eq!(second_site.occurrence(), 0);
        assert_eq!(second_site.kind(), rue_air::ComptimeSiteKind::Import);
        assert_eq!(second_site.program(), &second.plan.key);
        assert_ne!(first_site.program(), second_site.program());

        assert!(matches!(
            session.import_site_for_instruction(
                &first.plan.key,
                first_occurrence,
                "second-import.rue",
            ),
            Err(DurableComptimeKeyedImportError::SpecifierMismatch)
        ));
        assert!(matches!(
            session.import_site_for_instruction(
                &first.plan.key,
                rue_rir::InstRef::from_raw(u32::MAX),
                "first-import.rue",
            ),
            Err(DurableComptimeKeyedImportError::UnknownInstruction)
        ));
        let mut wrong_key = first.plan.key.clone();
        wrong_key.configuration.target = rue_target::Target::Aarch64Linux;
        assert!(matches!(
            session.import_site_for_instruction(&wrong_key, first_occurrence, "first-import.rue",),
            Err(DurableComptimeKeyedImportError::UnknownProgram)
        ));

        // A caller cannot pair the first key with the second program: the
        // second instruction is interpreted only against the first registry
        // entry and therefore fails before any import query/effect exists.
        assert!(matches!(
            session.import_site_for_instruction(
                &first.plan.key,
                second_occurrence,
                "second-import.rue",
            ),
            Err(DurableComptimeKeyedImportError::SpecifierMismatch)
                | Err(DurableComptimeKeyedImportError::UnknownInstruction)
        ));
    }

    enum ImportServiceMode {
        Missing,
        Failure(crate::declaration_candidate::DeclarationImportFailure),
        Abort,
    }

    #[derive(Clone, Copy)]
    enum CallFinishMode {
        Success,
        Failure,
        Abort,
    }

    struct ImportServiceAuthority {
        calls: Cell<usize>,
        mode: ImportServiceMode,
        keyed_heads: RefCell<Vec<crate::StableDefinitionKey>>,
        finish_modes: RefCell<Vec<Vec<crate::durable_semantics::DurableParameterMode>>>,
        finish_mode: CallFinishMode,
    }

    impl DurableComptimeSemanticAuthority for ImportServiceAuthority {
        fn check_canceled(&self) -> Result<(), QueryAbort> {
            panic!("not part of keyed import service test")
        }

        fn resolve_type_syntax(
            &mut self,
            _program: &crate::body_query::DurableComptimeProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
        ) -> Result<
            DurableType,
            rue_air::SemanticTypeSyntaxError<
                QueryAbort,
                SemanticNucleusFailure,
                crate::StableDefinitionKey,
                Arc<str>,
            >,
        > {
            panic!("not part of keyed import service test")
        }

        fn resolve_type_syntax_with_substitutions(
            &mut self,
            _program: &crate::body_query::DurableComptimeProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
            _type_substitutions: &[(Arc<str>, DurableType)],
            _value_substitutions: &[(Arc<str>, DurableConstValue)],
        ) -> Result<
            DurableType,
            rue_air::SemanticTypeSyntaxError<
                QueryAbort,
                SemanticNucleusFailure,
                crate::StableDefinitionKey,
                Arc<str>,
            >,
        > {
            panic!("not part of keyed import service test")
        }

        fn begin_structured_type(
            &mut self,
            _program: &crate::body_query::DurableComptimeProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
            _type_substitutions: Vec<(Arc<str>, DurableType)>,
            _value_substitutions: Vec<(Arc<str>, DurableConstValue)>,
        ) -> Result<
            DurableStructuredTypePoll,
            DurableStructuredTypeBeginError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn resume_structured_type(
            &mut self,
            _job: DurableStructuredTypeJob,
            _reduced: rue_air::SemanticProviderResult<
                Option<rue_air::SemanticComptimeCallResult<DurableType, DurableConstValue>>,
                QueryAbort,
                SemanticNucleusFailure,
            >,
        ) -> Result<
            DurableStructuredTypePoll,
            rue_air::SemanticTypeSyntaxError<
                QueryAbort,
                SemanticNucleusFailure,
                crate::StableDefinitionKey,
                Arc<str>,
            >,
        > {
            panic!("not part of keyed import service test")
        }

        fn begin_comptime_call_admission(
            &self,
            _accessing_source: &crate::StableDefinitionKey,
            _module: &ModuleId,
            _name: &str,
        ) -> Result<
            DurableComptimeCallableAdmissionStart,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn begin_comptime_call_admission_for_key(
            &self,
            accessing_source: &crate::StableDefinitionKey,
            head: &crate::StableDefinitionKey,
        ) -> Result<
            DurableComptimeCallableAdmissionStart,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            self.keyed_heads.borrow_mut().push(head.clone());
            let candidate =
                crate::revisioned_query_database::declaration_candidate_for_stable_key(head)
                    .expect("test keyed head has a canonical candidate");
            let identity = crate::semantic_query_nucleus::DeclarationIdentityProjection {
                key: head.clone(),
                is_public: true,
            };
            Ok(DurableComptimeCallableAdmissionStart {
                candidate,
                identity,
                configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                    target: rue_target::Target::X86_64Linux,
                    preview_features: crate::StablePreviewFeatures::new(
                        &crate::PreviewFeatures::default(),
                    ),
                },
                name: Arc::from(head.name()),
                dependency: SemanticDeclarationDependency {
                    source: accessing_source.clone(),
                    kind: rue_air::DeclarationTypeDependencyKind::Body,
                    target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        head.clone(),
                    ),
                },
            })
        }

        fn finish_comptime_call_admission(
            &self,
            start: DurableComptimeCallableAdmissionStart,
            argument_modes: &[crate::durable_semantics::DurableParameterMode],
        ) -> Result<
            DurableComptimeCallableAdmission,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            self.finish_modes.borrow_mut().push(argument_modes.to_vec());
            match self.finish_mode {
                CallFinishMode::Success => {
                    let parameters: Arc<[crate::durable_semantics::DurableSemanticParameter]> =
                        Arc::from([
                            crate::durable_semantics::DurableSemanticParameter {
                                name: Arc::from("first"),
                                ty: DurableType::I32,
                                mode: crate::durable_semantics::DurableParameterMode::Value,
                                is_comptime: true,
                                bounds: Arc::from([]),
                            },
                            crate::durable_semantics::DurableSemanticParameter {
                                name: Arc::from("second"),
                                ty: DurableType::I64,
                                mode: crate::durable_semantics::DurableParameterMode::Value,
                                is_comptime: true,
                                bounds: Arc::from([]),
                            },
                        ]);
                    let shell_parameters: Arc<
                        [crate::declaration_candidate::DeclarationParameterHeader],
                    > = Arc::from([
                        crate::declaration_candidate::DeclarationParameterHeader {
                            name: Arc::from("first"),
                            mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                            is_comptime: true,
                            is_type_parameter: false,
                        },
                        crate::declaration_candidate::DeclarationParameterHeader {
                            name: Arc::from("second"),
                            mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                            is_comptime: true,
                            is_type_parameter: false,
                        },
                    ]);
                    Ok(DurableComptimeCallableAdmission {
                        candidate: start.candidate,
                        identity: start.identity,
                        configuration: start.configuration,
                        parameters,
                        result: DurableType::ComptimeType,
                        shell_parameters,
                    })
                }
                CallFinishMode::Failure => Err(rue_air::SemanticProviderError::Failure(
                    SemanticNucleusFailure::Resolution(Arc::from("structured finish failed")),
                )),
                CallFinishMode::Abort => {
                    Err(rue_air::SemanticProviderError::Abort(QueryAbort::Canceled))
                }
            }
        }

        fn resolve_named_value(
            &self,
            _accessing_source: &crate::StableDefinitionKey,
            _module: &ModuleId,
            _name: &str,
        ) -> Result<
            Option<DurableComptimeNamedValueProjection>,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn resolve_module_member(
            &self,
            _accessing_source: &crate::StableDefinitionKey,
            _module: &ModuleId,
            _member: &str,
        ) -> Result<
            DurableComptimeNamedValueProjection,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn resolve_import(
            &self,
            _site: &DurableImportSite,
        ) -> Result<DurableImportResolution, QueryAbort> {
            panic!("keyed import service must not use the unkeyed import operation")
        }

        fn resolve_keyed_import(
            &self,
            _site: &rue_air::ComptimeSite<crate::body_query::DurableComptimeProgramKey>,
            _specifier: &str,
        ) -> Result<DurableImportResolution, DurableComptimeKeyedImportError> {
            self.calls.set(self.calls.get() + 1);
            match &self.mode {
                ImportServiceMode::Missing => Ok(DurableImportResolution::Missing),
                ImportServiceMode::Failure(failure) => {
                    Ok(DurableImportResolution::Failure(failure.clone()))
                }
                ImportServiceMode::Abort => Err(DurableComptimeKeyedImportError::ProviderAbort(
                    QueryAbort::Canceled,
                )),
            }
        }

        fn resolve_target_intrinsic(
            &self,
            _intrinsic: ComptimeTargetIntrinsic,
            _argument_count: usize,
        ) -> Result<
            TargetEnumValue,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn resolve_target_enum_variant(
            &self,
            _type_name: &str,
            _variant: &str,
        ) -> Result<
            TargetEnumValue,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }
    }

    #[test]
    fn keyed_callable_admission_uses_exact_head_without_spelling_lookup() {
        let first = super::structured_type_adapter_tests::callable_program("structured-head-a.rue");
        let second =
            super::structured_type_adapter_tests::callable_program("structured-head-b.rue");
        let first_head = first.plan.key.declaration.clone();
        let second_head = second.plan.key.declaration.clone();
        assert_ne!(first_head, second_head);
        let accessing_source = crate::StableDefinitionKey::from_stable_parts(
            first_head.module().clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "caller",
            None,
        );
        let mut authority = ImportServiceAuthority {
            calls: Cell::new(0),
            mode: ImportServiceMode::Missing,
            keyed_heads: RefCell::new(Vec::new()),
            finish_modes: RefCell::new(Vec::new()),
            finish_mode: CallFinishMode::Success,
        };
        let services = DurableComptimeServices::new(&mut authority);
        let first_admission = services
            .begin_comptime_call_admission_for_key(&accessing_source, &first_head)
            .unwrap();
        let second_admission = services
            .begin_comptime_call_admission_for_key(&accessing_source, &second_head)
            .unwrap();

        assert_eq!(first_admission.identity.key, first_head);
        assert_eq!(second_admission.identity.key, second_head);
        assert_eq!(
            first_admission.candidate.module,
            first_head.module().clone()
        );
        assert_eq!(
            second_admission.candidate.module,
            second_head.module().clone()
        );
        assert_eq!(
            authority.keyed_heads.borrow().as_slice(),
            &[first_head, second_head]
        );
        // This test authority intentionally has no module/name candidate path;
        // reaching both starts proves the structured seam carries the exact
        // canonical head through the services facade.
    }

    #[test]
    fn structured_callable_finish_uses_all_value_modes_and_preserves_begin_effects() {
        let program = callable_program("structured-finish.rue");
        let head = program.plan.key.declaration.clone();
        let source = crate::StableDefinitionKey::from_stable_parts(
            head.module().clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "structured-caller",
            None,
        );
        let dependency = |start: &DurableComptimeCallableAdmissionStart| start.dependency.clone();
        let mut authority = ImportServiceAuthority {
            calls: Cell::new(0),
            mode: ImportServiceMode::Missing,
            keyed_heads: RefCell::new(Vec::new()),
            finish_modes: RefCell::new(Vec::new()),
            finish_mode: CallFinishMode::Success,
        };
        let mut session = session();
        let initial_serial = session.next_lifecycle_serial_for_test();
        let initial_active = session.active_calls_for_test();
        let initial_programs = session.program_count_for_test();
        let start = {
            let services = DurableComptimeServices::new(&mut authority);
            services
                .begin_comptime_call_admission_for_key(&source, &head)
                .unwrap()
        };
        session.observe_dependency(dependency(&start));
        let admission = {
            let services = DurableComptimeServices::new(&mut authority);
            services
                .finish_structured_comptime_call_admission(start, 2)
                .unwrap()
        };
        assert_eq!(admission.identity.key, head);
        assert_eq!(admission.result, DurableType::ComptimeType);
        assert_eq!(admission.parameters[0].name.as_ref(), "first");
        assert_eq!(admission.parameters[0].ty, DurableType::I32);
        assert_eq!(admission.parameters[1].name.as_ref(), "second");
        assert_eq!(admission.parameters[1].ty, DurableType::I64);
        assert_eq!(
            admission
                .shell_parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect::<Vec<_>>(),
            vec![Arc::from("first"), Arc::from("second")]
        );
        assert_eq!(
            authority.finish_modes.borrow().as_slice(),
            &[[
                crate::durable_semantics::DurableParameterMode::Value,
                crate::durable_semantics::DurableParameterMode::Value,
            ]]
        );
        assert_eq!(
            session.drain_root_effects().unwrap().dependencies().count(),
            1
        );
        assert!(session.drain_root_effects().unwrap().is_empty());
        assert_eq!(session.next_lifecycle_serial_for_test(), initial_serial);
        assert_eq!(session.active_calls_for_test(), initial_active);
        assert_eq!(session.program_count_for_test(), initial_programs);

        for finish_mode in [CallFinishMode::Failure, CallFinishMode::Abort] {
            let mut authority = ImportServiceAuthority {
                calls: Cell::new(0),
                mode: ImportServiceMode::Missing,
                keyed_heads: RefCell::new(Vec::new()),
                finish_modes: RefCell::new(Vec::new()),
                finish_mode,
            };
            let start = {
                let services = DurableComptimeServices::new(&mut authority);
                services
                    .begin_comptime_call_admission_for_key(&source, &head)
                    .unwrap()
            };
            let mut session = fresh_session();
            let initial_serial = session.next_lifecycle_serial_for_test();
            let initial_active = session.active_calls_for_test();
            let initial_programs = session.program_count_for_test();
            session.observe_dependency(start.dependency.clone());
            let result = {
                let services = DurableComptimeServices::new(&mut authority);
                services.finish_structured_comptime_call_admission(start, 2)
            };
            match finish_mode {
                CallFinishMode::Failure => assert!(matches!(
                    result,
                    Err(rue_air::SemanticProviderError::Failure(
                        SemanticNucleusFailure::Resolution(_)
                    ))
                )),
                CallFinishMode::Abort => assert!(matches!(
                    result,
                    Err(rue_air::SemanticProviderError::Abort(QueryAbort::Canceled))
                )),
                CallFinishMode::Success => unreachable!(),
            }
            assert_eq!(
                session.drain_root_effects().unwrap().dependencies().count(),
                1
            );
            assert!(session.drain_root_effects().unwrap().is_empty());
            assert_eq!(session.next_lifecycle_serial_for_test(), initial_serial);
            assert_eq!(session.active_calls_for_test(), initial_active);
            assert_eq!(session.program_count_for_test(), initial_programs);
        }
    }

    #[test]
    fn keyed_import_service_preserves_terminals_and_skips_structural_rejections() {
        let first = const_program("service-import.rue", "i32");
        let mut session = session();
        session.register_program(&first).unwrap();
        let instruction = session
            .registered_program(&first.plan.key)
            .unwrap()
            .imports
            .imports[0]
            .inst;
        let site = session
            .import_site_for_instruction(&first.plan.key, instruction, "service-import.rue")
            .unwrap();
        let import_key = crate::declaration_candidate::DeclarationImportSiteKey {
            declaration: first.plan.candidate.clone(),
            occurrence: 0,
            specifier: Arc::from("service-import.rue"),
        };

        for (mode, expected) in [
            (ImportServiceMode::Missing, DurableImportResolution::Missing),
            (
                ImportServiceMode::Failure(
                    crate::declaration_candidate::DeclarationImportFailure::ResolutionUnavailable(
                        import_key.clone(),
                    ),
                ),
                DurableImportResolution::Failure(
                    crate::declaration_candidate::DeclarationImportFailure::ResolutionUnavailable(
                        import_key.clone(),
                    ),
                ),
            ),
        ] {
            let calls = Cell::new(0);
            let mut authority = ImportServiceAuthority {
                calls,
                mode,
                keyed_heads: RefCell::new(Vec::new()),
                finish_modes: RefCell::new(Vec::new()),
                finish_mode: CallFinishMode::Success,
            };
            let services = DurableComptimeServices::new(&mut authority);
            assert_eq!(
                services
                    .resolve_keyed_import(&site, "service-import.rue")
                    .unwrap(),
                expected
            );
            assert_eq!(authority.calls.get(), 1);
        }

        let calls = Cell::new(0);
        let mut authority = ImportServiceAuthority {
            calls,
            mode: ImportServiceMode::Abort,
            keyed_heads: RefCell::new(Vec::new()),
            finish_modes: RefCell::new(Vec::new()),
            finish_mode: CallFinishMode::Success,
        };
        let services = DurableComptimeServices::new(&mut authority);
        assert!(matches!(
            services.resolve_keyed_import(&site, "service-import.rue"),
            Err(DurableComptimeKeyedImportError::ProviderAbort(
                QueryAbort::Canceled
            ))
        ));
        assert_eq!(authority.calls.get(), 1);

        let wrong_kind = rue_air::ComptimeSite::from_occurrence(
            first.plan.key.clone(),
            rue_air::ComptimeSiteKind::Intrinsic,
            site.occurrence(),
            site.span(),
        );
        let mut authority = ImportServiceAuthority {
            calls: Cell::new(0),
            mode: ImportServiceMode::Missing,
            keyed_heads: RefCell::new(Vec::new()),
            finish_modes: RefCell::new(Vec::new()),
            finish_mode: CallFinishMode::Success,
        };
        let services = DurableComptimeServices::new(&mut authority);
        assert!(matches!(
            services.resolve_keyed_import(&wrong_kind, "service-import.rue"),
            Err(DurableComptimeKeyedImportError::WrongSiteKind)
        ));
        assert_eq!(authority.calls.get(), 0);
    }

    #[test]
    fn registered_const_core_receives_finalized_imports_without_authority_replacement() {
        let core = const_program_without_imports("finalize.rue", "i32");
        let key = core.plan.key.clone();
        let mut session = session();
        session.register_program(&core).unwrap();
        assert!(
            session
                .registered_program(&key)
                .unwrap()
                .imports
                .imports
                .is_empty()
        );

        let finalized =
            crate::body_query::OwnedComptimeProgramCore::finalize_imports(core, || Ok(())).unwrap();
        session.finalize_registered_imports(&finalized).unwrap();
        let registered = session.registered_program(&key).unwrap();
        assert_eq!(registered.imports.imports.len(), 1);
        assert_eq!(
            registered.imports.imports[0].specifier,
            Arc::<str>::from("finalize.rue")
        );

        let mismatched = const_program("finalize.rue", "i64");
        assert_eq!(
            session.finalize_registered_imports(&mismatched),
            Err(DurableComptimeProgramFinalizationError::AuthorityMismatch)
        );
        assert_eq!(
            session.registered_program(&key).unwrap().imports.imports[0].specifier,
            Arc::<str>::from("finalize.rue")
        );
    }

    #[test]
    fn foreign_frame_admission_is_keyed_atomic_and_keeps_ticket_unentered() {
        let core = callable_program("foreign-frame.rue");
        let seed = crate::body_query::ForeignComptimeCallSeed {
            type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
            value_arguments: Arc::from([(
                Arc::from("x"),
                crate::durable_semantics::DurableConstValue::Integer(9),
            )]),
        };
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: core.clone(),
            seed: seed.clone(),
        };
        let mut session = session();
        let edge = session.prepare_expression_edge(7).unwrap();
        let ticket = session
            .ticket_from_admitted_edge_for_test(edge, &admitted)
            .unwrap();
        let admission = prepared_admission(&core);
        let bound_admitted = test_admitted(admission, 7);
        let (frame, _ticket) = session
            .admit_foreign_frame(
                admitted,
                Box::new(ticket),
                rue_span::Span::new(17, 23),
                bound_call(&bound_admitted, Some(9)),
            )
            .unwrap();
        assert_eq!(frame.program, core.plan.key);
        assert_eq!(frame.body, core.callable().unwrap().body);
        assert_eq!(frame.name.as_ref().unwrap().as_str(), "target");
        assert_eq!(
            frame.context.as_ref().map(DurableComptimeFile::program),
            Some(&core.plan.key)
        );
        assert_eq!(frame.span, rue_span::Span::new(17, 23));
        assert_eq!(
            frame.function_span,
            core.rir.get(core.callable().unwrap().root).span
        );
        assert!(frame.call_identity.is_none());
        assert_eq!(
            frame.type_bindings.get(&DurableComptimeName::from("T")),
            Some(&DurableComptimeType(DurableType::I32))
        );
        assert_eq!(
            frame.value_bindings.get(&DurableComptimeName::from("x")),
            Some(&EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(9),
                DurableType::I32,
            )))
        );
        assert_eq!(
            frame.expected_result,
            Some(DurableComptimeType(DurableType::I32))
        );
        assert_eq!(session.active_call_count_for_test(), 0);
        assert!(session.registered_program(&core.plan.key).is_some());

        // Binding validation happens before a cold program is inserted.
        let invalid_core = callable_program("invalid-bindings.rue");
        let invalid_admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: invalid_core.clone(),
            seed: seed.clone(),
        };
        let invalid_edge = session.prepare_expression_edge(8).unwrap();
        let invalid_ticket = session
            .ticket_from_admitted_edge_for_test(invalid_edge, &invalid_admitted)
            .unwrap();
        let invalid_admission = prepared_admission(&invalid_core);
        let invalid_bound_admitted = test_admitted(invalid_admission, 8);
        assert!(matches!(
            session.admit_foreign_frame(
                invalid_admitted,
                Box::new(invalid_ticket),
                rue_span::Span::new(24, 27),
                bound_call(&invalid_bound_admitted, Some(10)),
            ),
            Err(DurableComptimeForeignFrameAdmissionError::TicketMismatch)
        ));
        assert!(session.registered_program(&invalid_core.plan.key).is_none());

        // A separately materialized equivalent core is valid, but it cannot
        // replace the first authority in the keyed registry.
        let equivalent = callable_program("foreign-frame.rue");
        let repeat_seed = crate::body_query::ForeignComptimeCallSeed {
            type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
            value_arguments: Arc::from([(
                Arc::from("x"),
                crate::durable_semantics::DurableConstValue::Integer(10),
            )]),
        };
        let equivalent_admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: equivalent,
            seed: repeat_seed,
        };
        let edge = session.prepare_expression_edge(8).unwrap();
        let ticket = session
            .ticket_from_admitted_edge_for_test(edge, &equivalent_admitted)
            .unwrap();
        let equivalent_admission = prepared_admission(&equivalent_admitted.core);
        let equivalent_bound_admitted = test_admitted(equivalent_admission, 8);
        let (second_frame, _) = session
            .admit_foreign_frame(
                equivalent_admitted,
                Box::new(ticket),
                rue_span::Span::new(24, 27),
                bound_call(&equivalent_bound_admitted, Some(10)),
            )
            .unwrap();
        assert_eq!(second_frame.program, core.plan.key);
        assert_eq!(second_frame.body, core.callable().unwrap().body);
        assert_eq!(
            second_frame
                .context
                .as_ref()
                .map(DurableComptimeFile::program),
            Some(&core.plan.key)
        );
        assert_eq!(
            second_frame.function_span,
            core.rir.get(core.callable().unwrap().root).span
        );
        assert_eq!(second_frame.span, rue_span::Span::new(24, 27));
        assert!(second_frame.call_identity.is_none());
        assert_eq!(
            second_frame
                .value_bindings
                .get(&DurableComptimeName::from("x")),
            Some(&EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(10),
                DurableType::I32,
            )))
        );
        assert_eq!(session.active_call_count_for_test(), 0);
        let registered = session.registered_program(&core.plan.key).unwrap();
        assert!(std::sync::Arc::ptr_eq(&registered.rir, &core.rir));
    }

    #[test]
    fn foreign_frame_admission_rejects_non_callable_without_registration() {
        let core = const_program("foreign-const.rue", "i32");
        let ticket_core = callable_program("foreign-const.rue");
        let ticket_admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: ticket_core,
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            },
        };
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            },
        };
        let mut session = session();
        let edge = session.prepare_expression_edge(0).unwrap();
        let ticket = session
            .ticket_from_admitted_edge_for_test(edge, &ticket_admitted)
            .unwrap();
        let ticket_admission = prepared_admission(&ticket_admitted.core);
        let ticket_bound_admitted = test_admitted(ticket_admission, 0);
        assert!(matches!(
            session.admit_foreign_frame(
                admitted,
                Box::new(ticket),
                rue_span::Span::new(0, 1),
                bound_call(&ticket_bound_admitted, None),
            ),
            Err(DurableComptimeForeignFrameAdmissionError::NotCallable)
        ));
        assert!(session.registered_program(&core.plan.key).is_none());
        assert_eq!(session.active_call_count_for_test(), 0);
    }

    #[test]
    fn const_root_admission_returns_keyed_ticket_free_frames_atomically() {
        assert_frame_domains::<
            EvaluatedSemanticConst,
            DurableComptimeType,
            DurableComptimeName,
            DurableComptimeFile,
            crate::body_query::DurableComptimeProgramKey,
            DurableComptimeIdentity,
        >(None);
        let first = const_program("frame-first.rue", "i32");
        let second = const_program("frame-second.rue", "i64");
        let callable = callable_program("frame-callable.rue");
        let mut session = session();
        assert_eq!(
            session.file_for_program(&callable.plan.key),
            Err(DurableComptimeDiagnosticSiteError::UnknownProgram)
        );

        let specialized_producer = crate::StableProducerId::Function(rue_air::Node::new(
            crate::FunctionInstanceKey::Specialization {
                base: rue_air::Node::new(crate::FunctionInstanceKey::Definition(
                    first.plan.key.declaration.clone(),
                )),
                arguments: Default::default(),
            },
        ));
        let durable_identity = DurableComptimeIdentity::from(specialized_producer.clone());
        assert_eq!(durable_identity.as_ref(), &specialized_producer);

        let first_frame = session.admit_const_root(first.clone(), None).unwrap();
        assert_eq!(first_frame.program, first.plan.key);
        assert_eq!(first_frame.body, first.const_root().unwrap().0);
        assert_eq!(
            first_frame
                .context
                .as_ref()
                .map(DurableComptimeFile::program),
            Some(&first.plan.key)
        );
        assert_eq!(first_frame.span, first.rir.get(first_frame.body).span);
        assert_eq!(
            first_frame.function_span,
            first.rir.get(first.const_root().unwrap().2).span
        );
        assert!(first_frame.name.is_none());
        assert!(first_frame.call_identity.is_none());
        assert!(first_frame.type_bindings.is_empty());
        assert!(first_frame.value_bindings.is_empty());
        assert!(first_frame.name_bindings.is_empty());
        assert!(first_frame.expected_result.is_none());

        let second_frame = session
            .admit_const_root(second.clone(), Some(DurableComptimeType(DurableType::I64)))
            .unwrap();
        assert_eq!(second_frame.program, second.plan.key);
        assert_eq!(second_frame.body, second.const_root().unwrap().0);
        assert_eq!(
            second_frame.expected_result,
            Some(DurableComptimeType(DurableType::I64))
        );
        assert_eq!(
            first_frame.body, second_frame.body,
            "dense refs intentionally collide"
        );
        assert_ne!(first_frame.program, second_frame.program);
        assert_eq!(
            first_frame
                .context
                .as_ref()
                .map(DurableComptimeFile::program),
            Some(&first.plan.key)
        );
        assert_eq!(
            second_frame
                .context
                .as_ref()
                .map(DurableComptimeFile::program),
            Some(&second.plan.key)
        );
        assert_ne!(first_frame.context, second_frame.context);
        assert_eq!(session.program_count_for_test(), 2);
        assert_ne!(
            session
                .registered_program(&first_frame.program)
                .unwrap()
                .symbols,
            session
                .registered_program(&second_frame.program)
                .unwrap()
                .symbols,
            "colliding refs retain distinct owning symbol authorities"
        );

        assert!(matches!(
            session.admit_const_root(first, None),
            Err(DurableComptimeConstRootAdmissionError::DuplicateProgram)
        ));
        assert!(matches!(
            session.admit_const_root(callable, None),
            Err(DurableComptimeConstRootAdmissionError::NotConstRoot)
        ));
        assert_eq!(
            session.program_count_for_test(),
            2,
            "rejected admissions are atomic"
        );
    }
}
