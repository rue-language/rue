use std::{collections::HashMap, sync::Arc};

use lasso::Spur;
use rue_error::CompileWarning;
use rue_span::Span;

use super::{AnalyzedFunction, BodyOwnerToken, BodySema};
use crate::{
    AirInstData, AirPattern, AirProjection, SemanticBody, SemanticBodyAnchor, SemanticBodyCallArg,
    SemanticBodyDefinitionIdentity, SemanticBodyDefinitionKind, SemanticBodyExport,
    SemanticBodyExportFailure as F, SemanticBodyInst, SemanticBodyInstData, SemanticBodyMatchArm,
    SemanticBodyPattern, SemanticBodyPlace, SemanticBodyProjection, SemanticImportConstValue,
    SemanticImportType, SemanticSpecializationIdentity, SemanticSpecializedBodyExport, Type,
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
            SemanticSpecializationIdentity<SemanticBodyDefinitionIdentity, Arc<str>>,
        >,
        dependencies: &[SemanticBodyDefinitionIdentity],
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
        let body = self
            .export_body(
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
        self.export_body(owner, body_span, analyzed, strings, warnings, None)
    }

    fn export_body(
        &self,
        owner: BodyOwnerToken,
        body_span: Span,
        analyzed: &AnalyzedFunction,
        strings: &[String],
        warnings: &[CompileWarning],
        specialized_calls: Option<
            &HashMap<
                Spur,
                SemanticSpecializationIdentity<SemanticBodyDefinitionIdentity, Arc<str>>,
            >,
        >,
    ) -> Result<SemanticBodyExport, F> {
        // CompileWarning carries structured labels, notes, help, and suggestions
        // which the first durable DTO intentionally does not model. Publishing a
        // lossy warning record would make a reuse hit observably different.
        if !warnings.is_empty() {
            return Err(F::UnsupportedWarningMetadata);
        }
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
            let mut projections = Vec::with_capacity(source.projections_len as usize);
            for projection in body.get_place_projections(source) {
                projections.push(match projection {
                    AirProjection::Field {
                        struct_id,
                        field_index,
                    } => SemanticBodyProjection::Field {
                        struct_key: self.struct_identity(*struct_id)?,
                        field_index: *field_index,
                    },
                    AirProjection::Index { array_type, index } => {
                        if index.as_u32() as usize >= instruction_count {
                            return Err(F::InvalidInstructionReference);
                        }
                        SemanticBodyProjection::Index {
                            array_type: self.export_body_type(*array_type)?,
                            index: index.as_u32(),
                        }
                    }
                });
            }
            places.push(SemanticBodyPlace {
                base: source.base,
                base_type: self.export_body_type(source.base_type)?,
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
            let call_args = |start, len| -> Result<Arc<[SemanticBodyCallArg]>, F> {
                body.get_call_args(start, len)
                    .map(|arg| {
                        Ok(SemanticBodyCallArg {
                            value: r(arg.value, current)?,
                            mode: arg.mode,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Arc::from)
            };
            let intrinsic_args = |start, len| -> Result<Arc<[SemanticBodyCallArg]>, F> {
                body.get_air_refs(start, len)
                    .map(|value| {
                        Ok(SemanticBodyCallArg {
                            value: r(value, current)?,
                            mode: crate::AirArgMode::Normal,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Arc::from)
            };
            let refs = |start, len| -> Result<Arc<[u32]>, F> {
                body.get_air_refs(start, len)
                    .map(|value| r(value, current))
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
                    SemanticBodyInstData::TypeConst(self.export_body_type(*v)?)
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
                AirInstData::Match {
                    scrutinee,
                    arms_start,
                    arms_len,
                } => {
                    let arms = body
                        .get_match_arms(*arms_start, *arms_len)
                        .map(|(pattern, value)| {
                            let pattern = match pattern {
                                AirPattern::Wildcard => SemanticBodyPattern::Wildcard,
                                AirPattern::Int(v) => SemanticBodyPattern::Int(v),
                                AirPattern::Bool(v) => SemanticBodyPattern::Bool(v),
                                AirPattern::EnumVariant {
                                    enum_id,
                                    variant_index,
                                } => SemanticBodyPattern::EnumVariant {
                                    enum_key: self.enum_identity(enum_id)?,
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
                    args_start,
                    args_len,
                } => {
                    if let Some(runtime) = runtime {
                        SemanticBodyInstData::RuntimeCall {
                            runtime: *runtime,
                            args: call_args(*args_start, *args_len)?,
                        }
                    } else {
                        match specialized_calls.and_then(|calls| calls.get(name)) {
                            Some(identity) => SemanticBodyInstData::CallSpecialized {
                                identity: identity.clone(),
                                args: call_args(*args_start, *args_len)?,
                            },
                            None => SemanticBodyInstData::Call {
                                function: self.function_identity(*name)?,
                                args: call_args(*args_start, *args_len)?,
                            },
                        }
                    }
                }
                AirInstData::CallGeneric { .. } => return Err(F::UnsupportedGenericCall),
                AirInstData::Intrinsic {
                    runtime,
                    name,
                    args_start,
                    args_len,
                } => SemanticBodyInstData::Intrinsic {
                    runtime: *runtime,
                    name: Arc::from(self.interner.resolve(name)),
                    args: intrinsic_args(*args_start, *args_len)?,
                },
                AirInstData::Param { index } => SemanticBodyInstData::Param { index: *index },
                AirInstData::Block {
                    stmts_start,
                    stmts_len,
                    value,
                } => SemanticBodyInstData::Block {
                    statements: refs(*stmts_start, *stmts_len)?,
                    value: r(*value, current)?,
                },
                AirInstData::StructInit {
                    struct_id,
                    fields_start,
                    fields_len,
                    source_order_start,
                } => {
                    let (fields, order) =
                        body.get_struct_init(*fields_start, *fields_len, *source_order_start);
                    SemanticBodyInstData::StructInit {
                        struct_key: self.struct_identity(*struct_id)?,
                        fields: fields
                            .map(|value| r(value, current))
                            .collect::<Result<Vec<_>, _>>()?
                            .into(),
                        source_order: order
                            .map(|value| u32::try_from(value).map_err(|_| F::SizeOverflow))
                            .collect::<Result<Vec<_>, _>>()?
                            .into(),
                    }
                }
                AirInstData::ArrayInit {
                    elems_start,
                    elems_len,
                } => SemanticBodyInstData::ArrayInit {
                    elements: refs(*elems_start, *elems_len)?,
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
                    payload_start,
                    payload_len,
                } => SemanticBodyInstData::EnumVariant {
                    enum_key: self.enum_identity(*enum_id)?,
                    variant_index: *variant_index,
                    payload: refs(*payload_start, *payload_len)?,
                },
                AirInstData::EnumPayloadGet {
                    base,
                    enum_id,
                    variant_index,
                    field_index,
                } => SemanticBodyInstData::EnumPayloadGet {
                    base: r(*base, current)?,
                    enum_key: self.enum_identity(*enum_id)?,
                    variant_index: *variant_index,
                    field_index: *field_index,
                },
                AirInstData::IntCast { value, from_ty } => SemanticBodyInstData::IntCast {
                    value: r(*value, current)?,
                    from_ty: self.export_body_type(*from_ty)?,
                },
                AirInstData::Drop { value } => SemanticBodyInstData::Drop {
                    value: r(*value, current)?,
                },
                AirInstData::StorageLive { slot } => {
                    SemanticBodyInstData::StorageLive { slot: *slot }
                }
                AirInstData::StorageDead { slot } => {
                    SemanticBodyInstData::StorageDead { slot: *slot }
                }
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
                ty: self.export_body_type(inst.ty)?,
                anchor: SemanticBodyAnchor {
                    start: inst.span.start - body_span.start,
                    end: inst.span.end - body_span.start,
                },
            });
        }

        let param_drops = body
            .param_drops()
            .iter()
            .map(|(slot, ty)| Ok((*slot, self.export_body_type(*ty)?)))
            .collect::<Result<Vec<_>, F>>()?;
        let borrow_slots = (0..analyzed.num_locals)
            .filter(|slot| body.is_borrow_slot(*slot))
            .collect::<Vec<_>>();
        Ok(SemanticBodyExport {
            owner,
            body: SemanticBody {
                return_type: self.export_body_type(body.return_type())?,
                instructions: Arc::from(instructions),
                places: Arc::from(places),
                strings: strings
                    .iter()
                    .map(|value| Arc::from(value.as_str()))
                    .collect(),
                param_drops: Arc::from(param_drops),
                borrow_slots: Arc::from(borrow_slots),
                num_locals: analyzed.num_locals,
                num_param_slots: analyzed.num_param_slots,
                param_by_ref: Arc::from(analyzed.param_modes.by_ref()),
                param_writable: Arc::from(analyzed.param_modes.writable()),
                allow_unreachable_code: analyzed.allow_unreachable_code,
                warnings: Arc::new([]),
            },
        })
    }

    pub(crate) fn function_identity(
        &self,
        symbol: Spur,
    ) -> Result<SemanticBodyDefinitionIdentity, F> {
        if let Some(info) = self.functions.get(&symbol) {
            return Ok(SemanticBodyDefinitionIdentity {
                file_id: info.file_id.index(),
                name: Arc::from(self.interner.resolve(&self.source_function_name(symbol))),
                kind: SemanticBodyDefinitionKind::FreeFunction,
                owner: None,
            });
        }
        let resolved = self.interner.resolve(&symbol);
        for (&(struct_id, method_name), info) in &self.methods {
            let method = self.interner.resolve(&method_name);
            if self.method_symbol(struct_id, method, info.has_self) != resolved {
                continue;
            }
            let owner = self.type_pool.struct_def(struct_id);
            if owner.name.starts_with("__anon_struct_") {
                return Err(F::AnonymousNominal);
            }
            return Ok(SemanticBodyDefinitionIdentity {
                file_id: info.span.file_id.index(),
                name: Arc::from(method),
                kind: if method == "__drop" {
                    SemanticBodyDefinitionKind::Destructor
                } else if info.has_self {
                    SemanticBodyDefinitionKind::Method
                } else {
                    SemanticBodyDefinitionKind::AssociatedFunction
                },
                owner: Some(Arc::from(owner.name.as_str())),
            });
        }
        Err(F::UnmappedFunction)
    }

    fn struct_identity(&self, id: crate::StructId) -> Result<SemanticBodyDefinitionIdentity, F> {
        let def = self.type_pool.struct_def(id);
        if def.name.starts_with("__anon_struct_") {
            return Err(F::AnonymousNominal);
        }
        Ok(SemanticBodyDefinitionIdentity {
            file_id: def.file_id.index(),
            name: Arc::from(def.name.as_str()),
            kind: SemanticBodyDefinitionKind::Struct,
            owner: None,
        })
    }

    fn enum_identity(&self, id: crate::EnumId) -> Result<SemanticBodyDefinitionIdentity, F> {
        let def = self.type_pool.enum_def(id);
        if def.name.starts_with("__anon_enum_") {
            return Err(F::AnonymousNominal);
        }
        Ok(SemanticBodyDefinitionIdentity {
            file_id: def.file_id.index(),
            name: Arc::from(def.name.as_str()),
            kind: SemanticBodyDefinitionKind::Enum,
            owner: None,
        })
    }

    pub(crate) fn export_body_type(
        &self,
        ty: Type,
    ) -> Result<SemanticImportType<SemanticBodyDefinitionIdentity, Arc<str>>, F> {
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
                if def.is_builtin {
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
                if rue_builtins::BUILTIN_ENUMS
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
            TypeKind::Module(id) => SemanticImportType::Module(Arc::from(
                self.module_registry.get_def(id).durable_id.as_str(),
            )),
            TypeKind::Error => return Err(F::UnsupportedType),
        })
    }
}
