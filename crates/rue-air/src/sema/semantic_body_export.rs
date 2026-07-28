use std::{collections::HashMap, sync::Arc};

use lasso::Spur;
use rue_error::CompileWarning;
use rue_span::Span;

use super::{AnalyzedFunction, BodyOwnerToken, BodySema, DeclarationPhase, Sema};
use crate::{
    AirInstData, AirPattern, AirProjection, SemanticBody, SemanticBodyAnchor, SemanticBodyCallArg,
    SemanticBodyExport, SemanticBodyExportFailure as F, SemanticBodyInst, SemanticBodyInstData,
    SemanticBodyMatchArm, SemanticBodyPattern, SemanticBodyPlace, SemanticBodyProjection,
    SemanticDefinitionToken, SemanticImportConstValue, SemanticImportType, SemanticModuleToken,
    SemanticSpecializationIdentity, SemanticSpecializedBodyExport, StableDefinitionKind, Type,
    TypeKind,
};

impl BodySema<'_> {
    pub(crate) fn export_specialized_body(
        &self,
        base_name: Spur,
        base_info: &super::FunctionInfo,
        type_arguments: &[Type],
        value_arguments: &[super::ConstValue],
        analyzed: &AnalyzedFunction,
        strings: &[String],
        warnings: &[CompileWarning],
        specialized_calls: &HashMap<
            Spur,
            SemanticSpecializationIdentity<SemanticDefinitionToken, SemanticModuleToken>,
        >,
        dependencies: &[SemanticDefinitionToken],
        dependency_boundary_complete: bool,
    ) -> Result<SemanticSpecializedBodyExport, F> {
        let source_name = self.source_function_name(base_name);
        let source_name_str = self.interner.resolve(&source_name);
        let owner = self.body_owner_token(
            base_info.file_id,
            source_name_str,
            None,
            super::BodyOwnerKind::FreeFunction,
        );
        let body = export_body(
            self,
            owner,
            base_info.span,
            analyzed,
            strings,
            warnings,
            Some(specialized_calls),
        )?
        .body;
        let type_arguments = type_arguments
            .iter()
            .map(|value| self.export_body_type(*value))
            .collect::<Result<Vec<_>, _>>()?;
        let value_arguments = value_arguments
            .iter()
            .map(|value| {
                Ok(match value {
                    super::ConstValue::Integer(value) => SemanticImportConstValue::Integer(*value),
                    super::ConstValue::Bool(value) => SemanticImportConstValue::Bool(*value),
                    super::ConstValue::Type(value) => {
                        SemanticImportConstValue::Type(self.export_body_type(*value)?)
                    }
                    super::ConstValue::Function(value) => {
                        SemanticImportConstValue::Function(self.function_identity(*value)?)
                    }
                    super::ConstValue::Unit => SemanticImportConstValue::Unit,
                    // Comptime parameters have no string type, so a string
                    // specialization argument never occurs (RUE-957); export
                    // the content faithfully regardless.
                    super::ConstValue::String(content) => SemanticImportConstValue::String(
                        std::sync::Arc::from(self.interner.resolve(content)),
                    ),
                })
            })
            .collect::<Result<Vec<_>, F>>()?;
        Ok(SemanticSpecializedBodyExport {
            identity: SemanticSpecializationIdentity {
                base: self.function_identity(base_name)?,
                type_arguments: type_arguments.into(),
                value_arguments: value_arguments.into(),
            },
            body,
            dependencies: dependencies.into(),
            dependency_boundary_complete,
        })
    }

    pub(crate) fn export_ordinary_body(
        &self,
        owner: BodyOwnerToken,
        body_span: Span,
        analyzed: &AnalyzedFunction,
        strings: &[String],
        warnings: &[CompileWarning],
    ) -> Result<SemanticBodyExport, F> {
        export_body(self, owner, body_span, analyzed, strings, warnings, None)
    }
}

/// Representation-neutral capabilities consumed by the one canonical AIR to
/// stable semantic-body exporter.
///
/// The exporter owns validation, anchor conversion, instruction/reference
/// rewriting, and warning publication. A host supplies only identity/type
/// projection and symbol spelling from its request-local authority.
pub(crate) trait SemanticBodyExportHost {
    fn export_body_type(
        &self,
        ty: Type,
    ) -> Result<SemanticImportType<SemanticDefinitionToken, SemanticModuleToken>, F>;
    fn body_struct_identity(
        &self,
        id: crate::StructId,
    ) -> Result<crate::NominalInstanceKey<SemanticDefinitionToken, SemanticModuleToken>, F>;
    fn body_enum_identity(
        &self,
        id: crate::EnumId,
    ) -> Result<crate::NominalInstanceKey<SemanticDefinitionToken, SemanticModuleToken>, F>;
    fn body_function_identity(
        &self,
        symbol: Spur,
    ) -> Result<crate::FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>, F>;
    fn resolve_publication_symbol(&self, symbol: &Spur) -> &str;
}

pub(crate) fn export_body<H: SemanticBodyExportHost>(
    host: &H,
    owner: BodyOwnerToken,
    body_span: Span,
    analyzed: &AnalyzedFunction,
    strings: &[String],
    warnings: &[CompileWarning],
    specialized_calls: Option<
        &HashMap<
            Spur,
            SemanticSpecializationIdentity<SemanticDefinitionToken, SemanticModuleToken>,
        >,
    >,
) -> Result<SemanticBodyExport, F> {
    let warning_anchor = |span: Span| {
        if span.file_id != body_span.file_id
            || span.start < body_span.start
            || span.end > body_span.end
        {
            return Err(F::ForeignSpan);
        }
        Ok(SemanticBodyAnchor {
            start: span.start - body_span.start,
            end: span.end - body_span.start,
        })
    };
    let warnings = warnings
        .iter()
        .map(|warning| {
            let anchor = warning
                .span()
                .ok_or(F::ForeignSpan)
                .and_then(warning_anchor)?;
            let diagnostic = warning.diagnostic();
            Ok(crate::SemanticBodyWarning {
                kind: warning.kind.clone(),
                anchor,
                labels: diagnostic
                    .labels
                    .iter()
                    .map(|label| {
                        Ok(crate::SemanticBodyWarningLabel {
                            message: Arc::from(label.message.as_str()),
                            anchor: warning_anchor(label.span)?,
                        })
                    })
                    .collect::<Result<Vec<_>, F>>()?
                    .into(),
                notes: diagnostic
                    .notes
                    .iter()
                    .map(|note| Arc::from(note.0.as_str()))
                    .collect(),
                helps: diagnostic
                    .helps
                    .iter()
                    .map(|help| Arc::from(help.0.as_str()))
                    .collect(),
                suggestions: diagnostic
                    .suggestions
                    .iter()
                    .map(|suggestion| {
                        Ok(crate::SemanticBodyWarningSuggestion {
                            message: Arc::from(suggestion.message.as_str()),
                            anchor: warning_anchor(suggestion.span)?,
                            replacement: Arc::from(suggestion.replacement.as_str()),
                            applicability: suggestion.applicability,
                        })
                    })
                    .collect::<Result<Vec<_>, F>>()?
                    .into(),
            })
        })
        .collect::<Result<Vec<_>, F>>()?;
    let body = &analyzed.air;
    let instruction_count = body.len();
    let place_count = body.places().len();
    let r = |value: crate::AirRef, current: usize| -> Result<u32, F> {
        let index = value.as_u32() as usize;
        if index >= instruction_count || index >= current {
            return Err(F::InvalidInstructionReference);
        }
        Ok(value.as_u32())
    };
    let place = |value: crate::AirPlaceRef| -> Result<u32, F> {
        if value.as_u32() as usize >= place_count {
            return Err(F::InvalidPlaceReference);
        }
        Ok(value.as_u32())
    };

    let mut places = Vec::with_capacity(place_count);
    for source in body.places() {
        let mut projections = Vec::with_capacity(source.projection_count());
        for projection in body.get_place_projections(source) {
            projections.push(match projection {
                AirProjection::Field {
                    struct_id,
                    field_index,
                } => SemanticBodyProjection::Field {
                    struct_key: host.body_struct_identity(*struct_id)?,
                    field_index: *field_index,
                },
                AirProjection::Index { array_type, index } => {
                    if index.as_u32() as usize >= instruction_count {
                        return Err(F::InvalidInstructionReference);
                    }
                    SemanticBodyProjection::Index {
                        array_type: host.export_body_type(*array_type)?,
                        index: index.as_u32(),
                    }
                }
            });
        }
        places.push(SemanticBodyPlace {
            base: source.base,
            base_type: host.export_body_type(source.base_type)?,
            projections: Arc::from(projections),
        });
    }

    let mut instructions = Vec::with_capacity(instruction_count);
    for (current, (_, inst)) in body.iter().enumerate() {
        if inst.span.file_id != body_span.file_id
            || inst.span.start < body_span.start
            || inst.span.end < inst.span.start
            || inst.span.end > body_span.end
        {
            return Err(F::ForeignSpan);
        }
        let unary = |v| r(v, current);
        let binary = |a, b| Ok::<_, F>((r(a, current)?, r(b, current)?));
        let call_args = |range| -> Result<Arc<[SemanticBodyCallArg]>, F> {
            body.get_call_args(range)
                .map(|arg| {
                    Ok(SemanticBodyCallArg {
                        value: r(arg.value, current)?,
                        mode: arg.mode,
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Arc::from)
        };
        let intrinsic_args = |range| -> Result<Arc<[SemanticBodyCallArg]>, F> {
            body.get_intrinsic_args(range)
                .map(|value| {
                    Ok(SemanticBodyCallArg {
                        value: r(value, current)?,
                        mode: crate::AirArgMode::Normal,
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Arc::from)
        };
        let data = match &inst.data {
            AirInstData::Const(v) => SemanticBodyInstData::Const(*v),
            AirInstData::BoolConst(v) => SemanticBodyInstData::BoolConst(*v),
            AirInstData::StringConst(v) => {
                if *v as usize >= strings.len() {
                    return Err(F::InvalidStringReference);
                }
                SemanticBodyInstData::StringConst(*v)
            }
            AirInstData::UnitConst => SemanticBodyInstData::UnitConst,
            AirInstData::TypeConst(v) => {
                SemanticBodyInstData::TypeConst(host.export_body_type(*v)?)
            }
            AirInstData::Add(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Add(a, b)
            }
            AirInstData::Sub(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Sub(a, b)
            }
            AirInstData::Mul(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Mul(a, b)
            }
            AirInstData::WrappingAdd(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::WrappingAdd(a, b)
            }
            AirInstData::WrappingSub(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::WrappingSub(a, b)
            }
            AirInstData::WrappingMul(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::WrappingMul(a, b)
            }
            AirInstData::Div(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Div(a, b)
            }
            AirInstData::Mod(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Mod(a, b)
            }
            AirInstData::Eq(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Eq(a, b)
            }
            AirInstData::Ne(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Ne(a, b)
            }
            AirInstData::Lt(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Lt(a, b)
            }
            AirInstData::Gt(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Gt(a, b)
            }
            AirInstData::Le(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Le(a, b)
            }
            AirInstData::Ge(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Ge(a, b)
            }
            AirInstData::And(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::And(a, b)
            }
            AirInstData::Or(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Or(a, b)
            }
            AirInstData::BitAnd(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::BitAnd(a, b)
            }
            AirInstData::BitOr(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::BitOr(a, b)
            }
            AirInstData::BitXor(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::BitXor(a, b)
            }
            AirInstData::Shl(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Shl(a, b)
            }
            AirInstData::Shr(a, b) => {
                let (a, b) = binary(*a, *b)?;
                SemanticBodyInstData::Shr(a, b)
            }
            AirInstData::Neg(v) => SemanticBodyInstData::Neg(unary(*v)?),
            AirInstData::Not(v) => SemanticBodyInstData::Not(unary(*v)?),
            AirInstData::BitNot(v) => SemanticBodyInstData::BitNot(unary(*v)?),
            AirInstData::Branch {
                cond,
                then_value,
                else_value,
            } => SemanticBodyInstData::Branch {
                cond: r(*cond, current)?,
                then_value: r(*then_value, current)?,
                else_value: else_value.map(|v| r(v, current)).transpose()?,
            },
            AirInstData::Loop { cond, body: value } => SemanticBodyInstData::Loop {
                cond: r(*cond, current)?,
                body: r(*value, current)?,
            },
            AirInstData::InfiniteLoop { body: value } => SemanticBodyInstData::InfiniteLoop {
                body: r(*value, current)?,
            },
            AirInstData::Match { scrutinee, arms } => {
                let arms = body
                    .get_match_arms(arms)
                    .map(|(pattern, value)| {
                        let pattern = match pattern {
                            AirPattern::Wildcard => SemanticBodyPattern::Wildcard,
                            AirPattern::Int(v) => SemanticBodyPattern::Int(v),
                            AirPattern::Bool(v) => SemanticBodyPattern::Bool(v),
                            AirPattern::EnumVariant {
                                enum_id,
                                variant_index,
                            } => SemanticBodyPattern::EnumVariant {
                                enum_key: host.body_enum_identity(enum_id)?,
                                variant_index,
                            },
                        };
                        Ok(SemanticBodyMatchArm {
                            pattern,
                            body: r(value, current)?,
                        })
                    })
                    .collect::<Result<Vec<_>, F>>()?;
                SemanticBodyInstData::Match {
                    scrutinee: r(*scrutinee, current)?,
                    arms: Arc::from(arms),
                }
            }
            AirInstData::Break => SemanticBodyInstData::Break,
            AirInstData::Continue => SemanticBodyInstData::Continue,
            AirInstData::Alloc { slot, init } => SemanticBodyInstData::Alloc {
                slot: *slot,
                init: r(*init, current)?,
            },
            AirInstData::Load { slot } => SemanticBodyInstData::Load { slot: *slot },
            AirInstData::Store { slot, value } => SemanticBodyInstData::Store {
                slot: *slot,
                value: r(*value, current)?,
            },
            AirInstData::ParamStore { param_slot, value } => SemanticBodyInstData::ParamStore {
                param_slot: *param_slot,
                value: r(*value, current)?,
            },
            AirInstData::Ret(value) => {
                SemanticBodyInstData::Ret(value.map(|value| r(value, current)).transpose()?)
            }
            AirInstData::Call {
                runtime,
                name,
                args,
            } => {
                if let Some(runtime) = runtime {
                    SemanticBodyInstData::RuntimeCall {
                        runtime: *runtime,
                        args: call_args(args)?,
                    }
                } else {
                    match specialized_calls.and_then(|calls| calls.get(name)) {
                        Some(identity) => SemanticBodyInstData::CallSpecialized {
                            identity: identity.clone(),
                            args: call_args(args)?,
                        },
                        None => SemanticBodyInstData::Call {
                            function: host.body_function_identity(*name)?,
                            args: call_args(args)?,
                        },
                    }
                }
            }
            AirInstData::CallGeneric { .. } => return Err(F::UnsupportedGenericCall),
            AirInstData::Intrinsic {
                runtime,
                name,
                args,
            } => SemanticBodyInstData::Intrinsic {
                runtime: *runtime,
                name: Arc::from(host.resolve_publication_symbol(name)),
                args: intrinsic_args(args)?,
            },
            AirInstData::Param { index } => SemanticBodyInstData::Param { index: *index },
            AirInstData::Block { statements, value } => SemanticBodyInstData::Block {
                statements: body
                    .get_block_statements(statements)
                    .map(|v| r(v, current))
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
                value: r(*value, current)?,
            },
            AirInstData::StructInit {
                struct_id,
                fields,
                source_order,
            } => SemanticBodyInstData::StructInit {
                struct_key: host.body_struct_identity(*struct_id)?,
                fields: body
                    .get_struct_fields(fields)
                    .map(|value| r(value, current))
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
                source_order: body
                    .get_source_order(source_order)
                    .map(|value| u32::try_from(value).map_err(|_| F::SizeOverflow))
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
            },
            AirInstData::ArrayInit { elements } => SemanticBodyInstData::ArrayInit {
                elements: body
                    .get_array_elements(elements)
                    .map(|v| r(v, current))
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
            },
            AirInstData::PlaceRead { place: value } => SemanticBodyInstData::PlaceRead {
                place: place(*value)?,
            },
            AirInstData::PlaceWrite { place: p, value } => SemanticBodyInstData::PlaceWrite {
                place: place(*p)?,
                value: r(*value, current)?,
            },
            AirInstData::EnumVariant {
                enum_id,
                variant_index,
                payload,
            } => SemanticBodyInstData::EnumVariant {
                enum_key: host.body_enum_identity(*enum_id)?,
                variant_index: *variant_index,
                payload: body
                    .get_enum_payload(payload)
                    .map(|v| r(v, current))
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
            },
            AirInstData::EnumPayloadGet {
                base,
                enum_id,
                variant_index,
                field_index,
            } => SemanticBodyInstData::EnumPayloadGet {
                base: r(*base, current)?,
                enum_key: host.body_enum_identity(*enum_id)?,
                variant_index: *variant_index,
                field_index: *field_index,
            },
            AirInstData::IntCast { value, from_ty } => SemanticBodyInstData::IntCast {
                value: r(*value, current)?,
                from_ty: host.export_body_type(*from_ty)?,
            },
            AirInstData::Drop { value } => SemanticBodyInstData::Drop {
                value: r(*value, current)?,
            },
            AirInstData::StorageLive { slot } => SemanticBodyInstData::StorageLive { slot: *slot },
            AirInstData::StorageDead { slot } => SemanticBodyInstData::StorageDead { slot: *slot },
            AirInstData::MarkMoved {
                value,
                slot,
                is_param,
                place: p,
            } => SemanticBodyInstData::MarkMoved {
                value: r(*value, current)?,
                slot: *slot,
                is_param: *is_param,
                place: p.map(place).transpose()?,
            },
        };
        instructions.push(SemanticBodyInst {
            data,
            ty: host.export_body_type(inst.ty)?,
            anchor: SemanticBodyAnchor {
                start: inst.span.start - body_span.start,
                end: inst.span.end - body_span.start,
            },
        });
    }

    let param_drops = body
        .param_drops()
        .iter()
        .map(|(slot, ty)| Ok((*slot, host.export_body_type(*ty)?)))
        .collect::<Result<Vec<_>, F>>()?;
    let borrow_slots = (0..analyzed.num_locals)
        .filter(|slot| body.is_borrow_slot(*slot))
        .collect::<Vec<_>>();
    Ok(SemanticBodyExport {
        owner,
        body: SemanticBody {
            return_type: host.export_body_type(body.return_type())?,
            instructions: Arc::from(instructions),
            places: Arc::from(places),
            strings: strings
                .iter()
                .map(|value| Arc::from(value.as_str()))
                .collect(),
            local_atoms: analyzed
                .local_atoms
                .iter()
                .map(|atom| crate::SemanticBodyLocalAtom {
                    identity: atom.identity.clone(),
                    content: atom.content.clone(),
                })
                .collect(),
            param_drops: Arc::from(param_drops),
            borrow_slots: Arc::from(borrow_slots),
            num_locals: analyzed.num_locals,
            num_param_slots: analyzed.num_param_slots,
            param_by_ref: Arc::from(analyzed.param_modes.by_ref()),
            param_writable: Arc::from(analyzed.param_modes.writable()),
            allow_unreachable_code: analyzed.allow_unreachable_code,
            warnings: warnings.into(),
        },
    })
}

impl SemanticBodyExportHost for BodySema<'_> {
    fn export_body_type(
        &self,
        ty: Type,
    ) -> Result<SemanticImportType<SemanticDefinitionToken, SemanticModuleToken>, F> {
        BodySema::export_body_type(self, ty)
    }

    fn body_struct_identity(
        &self,
        id: crate::StructId,
    ) -> Result<crate::NominalInstanceKey<SemanticDefinitionToken, SemanticModuleToken>, F> {
        BodySema::body_struct_identity(self, id)
    }

    fn body_enum_identity(
        &self,
        id: crate::EnumId,
    ) -> Result<crate::NominalInstanceKey<SemanticDefinitionToken, SemanticModuleToken>, F> {
        BodySema::body_enum_identity(self, id)
    }

    fn body_function_identity(
        &self,
        symbol: Spur,
    ) -> Result<crate::FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>, F> {
        BodySema::body_function_identity(self, symbol)
    }

    fn resolve_publication_symbol(&self, symbol: &Spur) -> &str {
        self.interner.resolve(symbol)
    }
}

impl<D: DeclarationPhase> Sema<'_, D> {
    pub(crate) fn function_identity(&self, symbol: Spur) -> Result<SemanticDefinitionToken, F> {
        if let Some(info) = self.functions.get(&symbol) {
            return self.stable_definition_token(
                info.file_id.index(),
                self.interner.resolve(&self.source_function_name(symbol)),
                None,
                StableDefinitionKind::Function,
            );
        }
        let (struct_id, method_name, info) = self
            .method_by_callable_symbol(symbol)
            .ok_or(F::UnmappedFunction)?;
        let method = self.interner.resolve(&method_name);
        let owner = self.type_pool.struct_def(struct_id);
        // Membership, not the generated-name prefix (RUE-1050).
        if self.anonymous_struct_ids.contains(&struct_id) {
            return Err(F::AnonymousNominal);
        }
        self.stable_definition_token(
            info.span.file_id.index(),
            method,
            Some(owner.name.as_str()),
            if method == "__drop" {
                StableDefinitionKind::Destructor
            } else if info.has_self {
                StableDefinitionKind::Method
            } else {
                StableDefinitionKind::AssociatedFunction
            },
        )
    }
}

impl BodySema<'_> {
    pub(in crate::sema) fn body_function_identity(
        &self,
        symbol: Spur,
    ) -> Result<crate::FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>, F> {
        if self.functions.contains_key(&symbol) {
            return Ok(crate::FunctionInstanceKey::Definition(
                self.function_identity(symbol)?,
            ));
        }
        let (struct_id, method_name, info) = self
            .method_by_callable_symbol(symbol)
            .ok_or(F::UnmappedFunction)?;
        let method = self.interner.resolve(&method_name);
        let owner = Type::new_struct(struct_id);
        if self.canonical_anonymous_types.contains_key(&owner) {
            let kind = if method == "__drop" {
                crate::AnonymousMemberKind::Destructor
            } else if info.has_self {
                crate::AnonymousMemberKind::Method
            } else {
                crate::AnonymousMemberKind::AssociatedFunction
            };
            return Ok(crate::FunctionInstanceKey::AnonymousMember {
                owner: Box::new(self.canonical_type_instance(owner)?),
                member: crate::AnonymousMemberKey {
                    kind,
                    name: Arc::from(method),
                },
            });
        }
        Ok(crate::FunctionInstanceKey::Definition(
            self.function_identity(symbol)?,
        ))
    }

    fn body_struct_identity(
        &self,
        id: crate::StructId,
    ) -> Result<crate::NominalInstanceKey<SemanticDefinitionToken, SemanticModuleToken>, F> {
        let ty = Type::new_struct(id);
        Ok(match self.canonical_anonymous_types.get(&ty) {
            Some(key) => crate::NominalInstanceKey::Anonymous(key.clone()),
            None if self.type_pool.struct_def(id).is_builtin
                || self.type_pool.struct_def(id).name == "str" =>
            {
                crate::NominalInstanceKey::Builtin {
                    kind: crate::AnonymousNominalKind::Struct,
                    name: Arc::from(self.type_pool.struct_def(id).name.as_str()),
                }
            }
            None => crate::NominalInstanceKey::Named(self.struct_identity(id)?),
        })
    }

    fn body_enum_identity(
        &self,
        id: crate::EnumId,
    ) -> Result<crate::NominalInstanceKey<SemanticDefinitionToken, SemanticModuleToken>, F> {
        let ty = Type::new_enum(id);
        Ok(match self.canonical_anonymous_types.get(&ty) {
            Some(key) => crate::NominalInstanceKey::Anonymous(key.clone()),
            None if rue_builtins::BUILTIN_ENUMS
                .iter()
                .any(|builtin| builtin.name == self.type_pool.enum_def(id).name) =>
            {
                crate::NominalInstanceKey::Builtin {
                    kind: crate::AnonymousNominalKind::Enum,
                    name: Arc::from(self.type_pool.enum_def(id).name.as_str()),
                }
            }
            None => crate::NominalInstanceKey::Named(self.enum_identity(id)?),
        })
    }
}

impl<D: DeclarationPhase> Sema<'_, D> {
    pub(crate) fn struct_identity(
        &self,
        id: crate::StructId,
    ) -> Result<SemanticDefinitionToken, F> {
        let def = self
            .type_pool
            .struct_metadata(id)
            .ok_or(F::UnsupportedType)?;
        // Membership, not the generated-name prefix: `__anon_struct_N` is a
        // legal source declaration and must still receive a named identity
        // (RUE-1050).
        if self.anonymous_struct_ids.contains(&id) {
            return Err(F::AnonymousNominal);
        }
        self.stable_definition_token(
            def.file_id.index(),
            def.name.as_str(),
            None,
            StableDefinitionKind::Struct,
        )
    }

    pub(crate) fn enum_identity(&self, id: crate::EnumId) -> Result<SemanticDefinitionToken, F> {
        let def = self.type_pool.enum_metadata(id).ok_or(F::UnsupportedType)?;
        // Membership, not the generated-name prefix (RUE-1050).
        if self.anonymous_enum_ids.contains(&id) {
            return Err(F::AnonymousNominal);
        }
        self.stable_definition_token(
            def.file_id.index(),
            def.name.as_str(),
            None,
            StableDefinitionKind::Enum,
        )
    }

    pub(crate) fn stable_definition_token(
        &self,
        file: u32,
        name: &str,
        owner: Option<&str>,
        kind: StableDefinitionKind,
    ) -> Result<SemanticDefinitionToken, F> {
        let owner = owner.map(str::to_owned);
        let key = (file, name.to_owned(), owner.clone(), kind);
        if let Some(token) = self.stable_definition_tokens.get(&key) {
            return Ok(*token);
        }
        if self.stable_definition_tokens.is_empty() {
            // The synthetic test constructor has no compiler-issued stable
            // universe. Keep that explicitly non-authoritative seam usable
            // without reintroducing textual identities into exported bodies.
            let mut slot = 2_166_136_261_u32;
            for byte in file
                .to_le_bytes()
                .into_iter()
                .chain(name.bytes())
                .chain(owner.as_deref().unwrap_or("").bytes())
                .chain([kind as u8])
            {
                slot = (slot ^ u32::from(byte)).wrapping_mul(16_777_619);
            }
            return Ok(SemanticDefinitionToken::new(0, slot));
        }
        let mut candidates = self.stable_definition_tokens.keys().filter(
            |(candidate_file, candidate_name, candidate_owner, _)| {
                *candidate_file == file && candidate_name == name && candidate_owner == &owner
            },
        );
        let Some(_) = candidates.next() else {
            return Err(F::MissingStableIdentity);
        };
        if candidates.next().is_some() {
            return Err(F::AmbiguousStableIdentity);
        }
        Err(F::WrongStableIdentityKind)
    }
}

impl BodySema<'_> {
    pub(crate) fn export_body_type(
        &self,
        ty: Type,
    ) -> Result<SemanticImportType<SemanticDefinitionToken, SemanticModuleToken>, F> {
        self.type_pool
            .validate_complete_type(ty)
            .map_err(|_| F::UnsupportedType)?;
        Ok(match ty.kind() {
            TypeKind::I8 => SemanticImportType::I8,
            TypeKind::I16 => SemanticImportType::I16,
            TypeKind::I32 => SemanticImportType::I32,
            TypeKind::I64 => SemanticImportType::I64,
            TypeKind::U8 => SemanticImportType::U8,
            TypeKind::U16 => SemanticImportType::U16,
            TypeKind::U32 => SemanticImportType::U32,
            TypeKind::U64 => SemanticImportType::U64,
            TypeKind::Bool => SemanticImportType::Bool,
            TypeKind::Unit => SemanticImportType::Unit,
            TypeKind::Never => SemanticImportType::Never,
            TypeKind::ComptimeType => SemanticImportType::ComptimeType,
            TypeKind::Struct(id) => {
                let def = self.type_pool.struct_def(id);
                if let Some(identity) = self.canonical_anonymous_types.get(&ty) {
                    // Producer-nominal: the single key mapped to this live type is
                    // its identity (ADR-0066).
                    SemanticImportType::AnonymousNominal(identity.clone())
                } else if let Some(element) = self.slice_element_type(ty)
                    && def.name.starts_with('[')
                {
                    SemanticImportType::Slice {
                        element: Box::new(self.export_body_type(element)?),
                        name: Arc::from(def.name.as_str()),
                    }
                } else if def.is_builtin || def.name == "str" {
                    SemanticImportType::BuiltinNominal {
                        name: Arc::from(def.name.as_str()),
                        kind: crate::SemanticImportNominalKind::Struct,
                    }
                } else {
                    SemanticImportType::Nominal(self.struct_identity(id)?)
                }
            }
            TypeKind::Enum(id) => {
                let def = self.type_pool.enum_def(id);
                if let Some(identity) = self.canonical_anonymous_types.get(&ty) {
                    SemanticImportType::AnonymousNominal(identity.clone())
                } else if rue_builtins::BUILTIN_ENUMS
                    .iter()
                    .any(|builtin| builtin.name == def.name)
                {
                    SemanticImportType::BuiltinNominal {
                        name: Arc::from(def.name.as_str()),
                        kind: crate::SemanticImportNominalKind::Enum,
                    }
                } else {
                    SemanticImportType::Nominal(self.enum_identity(id)?)
                }
            }
            TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                SemanticImportType::Array {
                    element: Box::new(self.export_body_type(element)?),
                    len,
                }
            }
            TypeKind::PtrConst(id) => SemanticImportType::PtrConst(Box::new(
                self.export_body_type(self.type_pool.ptr_const_def(id))?,
            )),
            TypeKind::PtrMut(id) => SemanticImportType::PtrMut(Box::new(
                self.export_body_type(self.type_pool.ptr_mut_def(id))?,
            )),
            TypeKind::Module(id) => {
                let file = self.module_registry.get_def(id).file_id;
                SemanticImportType::Module(
                    self.stable_module_tokens
                        .get(&file)
                        .copied()
                        .or_else(|| {
                            self.stable_module_tokens
                                .is_empty()
                                .then(|| SemanticModuleToken::new(0, file.index()))
                        })
                        .ok_or(F::MissingStableIdentity)?,
                )
            }
            TypeKind::Error => return Err(F::UnsupportedType),
        })
    }
}
