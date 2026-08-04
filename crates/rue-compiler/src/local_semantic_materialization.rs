//! Canonical body to body-local AIR materialization.
//!
//! This adapter joins one query-owned body with exact declaration, anonymous
//! nominal, nominal-metadata, callable-symbol, and module facts. It deliberately
//! owns no state: the returned AIR carries its issuing pool/interner and the
//! temporary import epoch is consumed by the request. Target and preview
//! configuration are absent because relocation of already-analyzed AIR is
//! configuration-neutral; they remain part of the downstream CFG query key.

use std::sync::Arc;

use crate::durable_semantics::{
    DurableAnonymousNominal, DurableAnonymousNominalShape, DurableDeclarationPayload,
    DurableDeclarationSemantic,
};
use crate::{FunctionInstanceKey, ModuleId, StableDefinitionKey, StableDefinitionKind};

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // RUE-1215 installs this exact input at the CFG boundary.
pub(crate) struct LocalCallableFact {
    pub(crate) identity: FunctionInstanceKey,
    pub(crate) symbol: Arc<str>,
}

/// Exact classification projected from the selected nominal's query-owned
/// lookup/index fact. `None` is meaningful: every supplied named nominal has a
/// record, so omission cannot silently strip compiler-recognized metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // RUE-1215 installs this exact input at the CFG boundary.
pub(crate) struct LocalNominalMetadataFact {
    identity: StableDefinitionKey,
    lang_item: Option<rue_air::LangItem>,
}

#[allow(dead_code)] // RUE-1215 supplies these facts from the CFG request.
impl LocalNominalMetadataFact {
    pub(crate) fn new(identity: StableDefinitionKey, lang_item: Option<rue_air::LangItem>) -> Self {
        Self {
            identity,
            lang_item,
        }
    }

    pub(crate) fn identity(&self) -> &StableDefinitionKey {
        &self.identity
    }

    pub(crate) fn lang_item(&self) -> Option<rue_air::LangItem> {
        self.lang_item
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // RUE-1215 installs this exact failure at the CFG boundary.
pub(crate) enum LocalMaterializationFailure {
    InvalidSpecializationIdentity,
    DuplicateNominalMetadata,
    MissingNominalMetadata,
    ExtraNominalMetadata,
    AmbiguousNamedDestructor,
    Import(rue_air::SemanticImportFailure),
    Body(rue_air::SemanticBodyImportFailure),
}

impl From<rue_air::SemanticImportFailure> for LocalMaterializationFailure {
    fn from(value: rue_air::SemanticImportFailure) -> Self {
        Self::Import(value)
    }
}

impl From<rue_air::SemanticBodyImportFailure> for LocalMaterializationFailure {
    fn from(value: rue_air::SemanticBodyImportFailure) -> Self {
        Self::Body(value)
    }
}

#[allow(dead_code)] // RUE-1215 installs this exact result at the CFG boundary.
pub(crate) type LocalSemanticMaterialization =
    rue_air::SemanticLocalMaterialization<StableDefinitionKey, ModuleId>;

/// Materialize one canonical body from exact query facts.
///
/// The declaration and anonymous slices may contain only the facts required by
/// this body. Missing transitive shapes/callables fail closed in `rue-air`;
/// this adapter never widens the request by discovering a program universe.
#[allow(dead_code)] // RUE-1215 installs this as the canonical CFG producer.
pub(crate) fn materialize_canonical_body(
    canonical: &crate::body_query::CanonicalBody,
    body_span: rue_span::Span,
    declarations: &[DurableDeclarationSemantic],
    anonymous_nominals: &[DurableAnonymousNominal],
    callable_facts: &[LocalCallableFact],
    nominal_metadata: &[LocalNominalMetadataFact],
    modules: &[ModuleId],
) -> Result<LocalSemanticMaterialization, LocalMaterializationFailure> {
    let (identity, body) = match canonical {
        crate::body_query::CanonicalBody::Ordinary { owner, body } => {
            (FunctionInstanceKey::Definition(owner.clone()), body)
        }
        crate::body_query::CanonicalBody::Anonymous { identity, body, .. } => {
            (identity.clone(), body)
        }
        crate::body_query::CanonicalBody::Specialization { identity, body, .. } => (
            crate::semantic_identity::function_instance_from_specialization(identity)
                .ok_or(LocalMaterializationFailure::InvalidSpecializationIdentity)?,
            body,
        ),
    };
    let callable_kind = match &identity {
        FunctionInstanceKey::Definition(key) if key.kind() == StableDefinitionKind::Destructor => {
            rue_air::AnalyzedCallableKind::Destructor
        }
        FunctionInstanceKey::DropGlue(_) => rue_air::AnalyzedCallableKind::DropGlue,
        _ => rue_air::AnalyzedCallableKind::Ordinary,
    };

    let mut destructors = std::collections::BTreeMap::new();
    for candidate in declarations {
        if !matches!(candidate.payload, DurableDeclarationPayload::Destructor) {
            continue;
        }
        let Some(owner) = candidate.key.owner() else {
            continue;
        };
        let owner = (
            owner.module().clone(),
            owner.kind(),
            Arc::<str>::from(owner.name()),
        );
        if destructors
            .insert(
                owner,
                FunctionInstanceKey::Definition(candidate.key.clone()),
            )
            .is_some()
        {
            return Err(LocalMaterializationFailure::AmbiguousNamedDestructor);
        }
    }
    let destructor_for = |owner: &StableDefinitionKey| {
        destructors
            .get(&(
                owner.module().clone(),
                owner.kind(),
                Arc::<str>::from(owner.name()),
            ))
            .cloned()
    };
    let mut metadata = std::collections::BTreeMap::new();
    for fact in nominal_metadata {
        if metadata
            .insert(fact.identity().clone(), fact.lang_item())
            .is_some()
        {
            return Err(LocalMaterializationFailure::DuplicateNominalMetadata);
        }
    }
    let mut nominals = Vec::new();
    for declaration in declarations {
        let (kind, shape) = match &declaration.payload {
            DurableDeclarationPayload::Struct {
                fields,
                is_copy,
                is_linear,
            } => (
                rue_air::SemanticImportNominalKind::Struct,
                rue_air::SemanticLocalNominalShape::Struct {
                    fields: fields.clone(),
                    is_copy: *is_copy,
                    is_linear: *is_linear,
                    destructor: destructor_for(&declaration.key),
                },
            ),
            DurableDeclarationPayload::Enum { variants } => (
                rue_air::SemanticImportNominalKind::Enum,
                rue_air::SemanticLocalNominalShape::Enum {
                    variants: variants.clone(),
                },
            ),
            _ => continue,
        };
        let lang_item = metadata
            .remove(&declaration.key)
            .ok_or(LocalMaterializationFailure::MissingNominalMetadata)?;
        nominals.push(rue_air::SemanticLocalNominal {
            key: rue_air::NominalInstanceKey::Named(declaration.key.clone()),
            module_path: Arc::from(declaration.key.module().as_str()),
            name: Arc::from(declaration.key.name()),
            kind,
            is_public: declaration.is_public,
            lang_item,
            shape,
        });
    }
    if !metadata.is_empty() {
        return Err(LocalMaterializationFailure::ExtraNominalMetadata);
    }
    nominals.extend(anonymous_nominals.iter().map(|nominal| {
        let (kind, shape) = match &nominal.shape {
            DurableAnonymousNominalShape::Struct { fields, methods } => {
                let destructor = methods
                    .iter()
                    .find(|method| method.has_self && method.name.as_ref() == "__drop")
                    .map(|_| FunctionInstanceKey::AnonymousMember {
                        owner: Box::new(crate::TypeInstanceKey::Nominal(
                            crate::NominalInstanceKey::Anonymous(nominal.identity.clone()),
                        )),
                        member: crate::AnonymousMemberKey {
                            kind: crate::AnonymousMemberKind::Destructor,
                            name: Arc::from("__drop"),
                        },
                    });
                (
                    rue_air::SemanticImportNominalKind::Struct,
                    rue_air::SemanticLocalNominalShape::Struct {
                        fields: fields.clone(),
                        // Anonymous structs have no `copy` or `linear`
                        // declaration modifiers. Transitive ownership facts are
                        // derived by the completed local type pool.
                        is_copy: false,
                        is_linear: false,
                        destructor,
                    },
                )
            }
            DurableAnonymousNominalShape::Enum { variants } => (
                rue_air::SemanticImportNominalKind::Enum,
                rue_air::SemanticLocalNominalShape::Enum {
                    variants: variants.clone(),
                },
            ),
        };
        rue_air::SemanticLocalNominal {
            key: rue_air::NominalInstanceKey::Anonymous(nominal.identity.clone()),
            module_path: Arc::from("<anonymous>"),
            name: Arc::from(format!("anonymous-{:?}", nominal.identity)),
            kind,
            is_public: false,
            lang_item: None,
            shape,
        }
    }));
    let callables = callable_facts
        .iter()
        .map(|fact| rue_air::SemanticLocalCallable {
            key: fact.identity.clone(),
            symbol: fact.symbol.clone(),
        })
        .collect();
    let epoch = rue_air::SemanticImportEpoch::new_local(nominals, callables, modules.to_vec())?;
    Ok(epoch.materialize_local_body(identity, callable_kind, body, body_span)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> rue_air::SemanticBody<StableDefinitionKey, ModuleId> {
        use rue_air::{SemanticBody, SemanticBodyAnchor, SemanticBodyInst, SemanticBodyInstData};
        SemanticBody {
            return_type: rue_air::SemanticImportType::I32,
            instructions: vec![
                SemanticBodyInst {
                    data: SemanticBodyInstData::Const(7),
                    ty: rue_air::SemanticImportType::I32,
                    anchor: SemanticBodyAnchor { start: 1, end: 2 },
                },
                SemanticBodyInst {
                    data: SemanticBodyInstData::Ret(Some(0)),
                    ty: rue_air::SemanticImportType::I32,
                    anchor: SemanticBodyAnchor { start: 2, end: 3 },
                },
            ]
            .into(),
            places: Arc::new([]),
            strings: Arc::new([]),
            local_atoms: Arc::new([]),
            param_drops: Arc::new([]),
            borrow_slots: Arc::new([]),
            num_locals: 0,
            num_param_slots: 0,
            param_by_ref: Arc::new([]),
            param_writable: Arc::new([]),
            allow_unreachable_code: false,
            warnings: Arc::new([]),
            method_references: Arc::new([]),
        }
    }

    fn definition(
        module: ModuleId,
        kind: StableDefinitionKind,
        name: &str,
        owner: Option<(StableDefinitionKind, Arc<str>)>,
    ) -> StableDefinitionKey {
        StableDefinitionKey::for_test(module, kind.namespace(), kind, name, owner)
    }

    #[test]
    fn exact_metadata_restores_strbuf_language_item() {
        let module = ModuleId::from_trusted_validated_canonical("\0rue-std/strbuf.rue");
        let function = definition(
            module.clone(),
            StableDefinitionKind::Function,
            "probe",
            None,
        );
        let strbuf = definition(module.clone(), StableDefinitionKind::Struct, "StrBuf", None);
        let canonical = crate::body_query::CanonicalBody::Ordinary {
            owner: function.clone(),
            body: body(),
        };
        let declaration = DurableDeclarationSemantic {
            key: strbuf.clone(),
            is_public: true,
            payload: DurableDeclarationPayload::Struct {
                fields: Arc::new([]),
                is_copy: false,
                is_linear: false,
            },
        };
        assert_eq!(
            materialize_canonical_body(
                &canonical,
                rue_span::Span::with_file(rue_span::FileId::new(3), 100, 200),
                std::slice::from_ref(&declaration),
                &[],
                &[LocalCallableFact {
                    identity: FunctionInstanceKey::Definition(function.clone()),
                    symbol: Arc::from("probe"),
                }],
                &[],
                std::slice::from_ref(&module),
            )
            .err(),
            Some(LocalMaterializationFailure::MissingNominalMetadata)
        );
        let output = materialize_canonical_body(
            &canonical,
            rue_span::Span::with_file(rue_span::FileId::new(3), 100, 200),
            std::slice::from_ref(&declaration),
            &[],
            &[LocalCallableFact {
                identity: FunctionInstanceKey::Definition(function),
                symbol: Arc::from("probe"),
            }],
            &[LocalNominalMetadataFact::new(
                strbuf.clone(),
                Some(rue_air::LangItem::StrBuf),
            )],
            std::slice::from_ref(&module),
        )
        .unwrap();
        let ty = output
            .aggregate_types
            .iter()
            .find_map(|(ty, identity)| {
                (identity
                    == &crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(
                        strbuf.clone(),
                    )))
                    .then_some(*ty)
            })
            .expect("StrBuf stable identity is retained");
        assert_eq!(
            output.type_pool.struct_lang_item(ty.as_struct().unwrap()),
            Some(rue_air::LangItem::StrBuf)
        );
    }

    #[test]
    fn duplicate_named_destructor_facts_are_ambiguous() {
        let module = ModuleId::from_validated_canonical("main.rue");
        let function = definition(
            module.clone(),
            StableDefinitionKind::Function,
            "probe",
            None,
        );
        let record = definition(module.clone(), StableDefinitionKind::Struct, "Record", None);
        let destructor = definition(
            module.clone(),
            StableDefinitionKind::Destructor,
            "Record",
            Some((StableDefinitionKind::Struct, Arc::from("Record"))),
        );
        let declarations = vec![
            DurableDeclarationSemantic {
                key: record.clone(),
                is_public: false,
                payload: DurableDeclarationPayload::Struct {
                    fields: Arc::new([]),
                    is_copy: false,
                    is_linear: false,
                },
            },
            DurableDeclarationSemantic {
                key: destructor.clone(),
                is_public: false,
                payload: DurableDeclarationPayload::Destructor,
            },
            DurableDeclarationSemantic {
                key: destructor,
                is_public: false,
                payload: DurableDeclarationPayload::Destructor,
            },
        ];
        let error = materialize_canonical_body(
            &crate::body_query::CanonicalBody::Ordinary {
                owner: function.clone(),
                body: body(),
            },
            rue_span::Span::with_file(rue_span::FileId::new(3), 100, 200),
            &declarations,
            &[],
            &[LocalCallableFact {
                identity: FunctionInstanceKey::Definition(function),
                symbol: Arc::from("probe"),
            }],
            &[LocalNominalMetadataFact::new(record, None)],
            &[module],
        )
        .err()
        .expect("duplicate destructor records must not select the first");
        assert_eq!(error, LocalMaterializationFailure::AmbiguousNamedDestructor);
    }
}
