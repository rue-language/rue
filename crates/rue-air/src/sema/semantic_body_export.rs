use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use lasso::Spur;
use rue_error::CompileWarning;
use rue_span::Span;

use super::{AnalyzedFunction, BodyOwnerToken};
use crate::{
    AirInstData, AirPattern, AirProjection, SemanticBody, SemanticBodyAnchor, SemanticBodyCallArg,
    SemanticBodyExport, SemanticBodyExportFailure as F, SemanticBodyInst, SemanticBodyInstData,
    SemanticBodyMatchArm, SemanticBodyPattern, SemanticBodyPlace, SemanticBodyProjection,
    SemanticDefinitionToken, SemanticImportType, SemanticModuleToken,
    SemanticSpecializationIdentity, Type,
};

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
    /// The content-canonical rendered symbol component of a struct receiver
    /// (`{name}${mangled module}` for user nominals, the bare name for
    /// builtin/language-item/generated exemptions, the digest name for
    /// anonymous nominals). Used only to order the recorded method-reference
    /// payload deterministically across sessions and revisions.
    fn body_struct_symbol(&self, id: crate::StructId) -> String;
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
    method_references: &HashSet<(crate::StructId, Spur)>,
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
        let base = match source.base {
            crate::AirPlaceBase::Local(slot) => crate::AirPlaceBase::Local(slot),
            crate::AirPlaceBase::Param(slot) => crate::AirPlaceBase::Param(slot),
            crate::AirPlaceBase::Accessor(call) => {
                if call.as_u32() as usize >= instruction_count {
                    return Err(F::InvalidInstructionReference);
                }
                crate::AirPlaceBase::Accessor(call)
            }
        };
        places.push(SemanticBodyPlace {
            base,
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
            AirInstData::AccessorCall { name, args } => SemanticBodyInstData::AccessorCall {
                function: host.body_function_identity(*name)?,
                args: call_args(args)?,
            },
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
    // The recorded references were captured when method resolution selected
    // each winner. Order the payload by content-canonical render so equal
    // recorded sets serialize identically regardless of session-local ids.
    let mut method_references = method_references
        .iter()
        .map(|(struct_id, method)| {
            Ok((
                host.body_struct_symbol(*struct_id),
                host.resolve_publication_symbol(method).to_owned(),
                crate::SemanticBodyMethodReference {
                    receiver: host.body_struct_identity(*struct_id)?,
                    method: Arc::from(host.resolve_publication_symbol(method)),
                },
            ))
        })
        .collect::<Result<Vec<_>, F>>()?;
    method_references.sort_by(
        |(left_symbol, left_method, _), (right_symbol, right_method, _)| {
            (left_symbol, left_method).cmp(&(right_symbol, right_method))
        },
    );
    let method_references = method_references
        .into_iter()
        .map(|(_, _, reference)| reference)
        .collect::<Vec<_>>();
    Ok(SemanticBodyExport {
        owner,
        body: SemanticBody {
            is_accessor: analyzed.callable_kind == crate::AnalyzedCallableKind::Accessor,
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
            method_references: method_references.into(),
        },
    })
}
