use std::collections::HashMap;
use std::sync::Arc;

use lasso::{Key, Spur};
use rue_air::{AirInstData, AnalyzedFunction, SemanticImportType, Type, TypeKind};
use rue_span::Span;

use crate::DurableAirInstData;

type CanonicalType = SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>;

fn canonical_nominal(value: &crate::NominalInstanceKey) -> CanonicalType {
    match value {
        crate::NominalInstanceKey::Builtin { kind, name } => CanonicalType::BuiltinNominal {
            kind: match kind {
                crate::AnonymousNominalKind::Struct => rue_air::SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
            },
            name: name.clone(),
        },
        crate::NominalInstanceKey::Named(definition) => CanonicalType::Nominal(definition.clone()),
        crate::NominalInstanceKey::Anonymous(identity) => {
            CanonicalType::AnonymousNominal(identity.clone())
        }
    }
}

fn live_instruction_kind(data: &AirInstData) -> rue_air::SemanticBodyInstKind {
    use rue_air::SemanticBodyInstKind as K;
    match data {
        AirInstData::Const(..) => K::Const,
        AirInstData::BoolConst(..) => K::BoolConst,
        AirInstData::StringConst(..) => K::StringConst,
        AirInstData::UnitConst => K::UnitConst,
        AirInstData::TypeConst(..) => K::TypeConst,
        AirInstData::Add(..) => K::Add,
        AirInstData::Sub(..) => K::Sub,
        AirInstData::Mul(..) => K::Mul,
        AirInstData::WrappingAdd(..) => K::WrappingAdd,
        AirInstData::WrappingSub(..) => K::WrappingSub,
        AirInstData::WrappingMul(..) => K::WrappingMul,
        AirInstData::Div(..) => K::Div,
        AirInstData::Mod(..) => K::Mod,
        AirInstData::Eq(..) => K::Eq,
        AirInstData::Ne(..) => K::Ne,
        AirInstData::Lt(..) => K::Lt,
        AirInstData::Gt(..) => K::Gt,
        AirInstData::Le(..) => K::Le,
        AirInstData::Ge(..) => K::Ge,
        AirInstData::And(..) => K::And,
        AirInstData::Or(..) => K::Or,
        AirInstData::BitAnd(..) => K::BitAnd,
        AirInstData::BitOr(..) => K::BitOr,
        AirInstData::BitXor(..) => K::BitXor,
        AirInstData::Shl(..) => K::Shl,
        AirInstData::Shr(..) => K::Shr,
        AirInstData::Neg(..) => K::Neg,
        AirInstData::Not(..) => K::Not,
        AirInstData::BitNot(..) => K::BitNot,
        AirInstData::Branch { .. } => K::Branch,
        AirInstData::Loop { .. } => K::Loop,
        AirInstData::InfiniteLoop { .. } => K::InfiniteLoop,
        AirInstData::Match { .. } => K::Match,
        AirInstData::Break => K::Break,
        AirInstData::Continue => K::Continue,
        AirInstData::Alloc { .. } => K::Alloc,
        AirInstData::Load { .. } => K::Load,
        AirInstData::Store { .. } => K::Store,
        AirInstData::ParamStore { .. } => K::ParamStore,
        AirInstData::Ret(..) => K::Ret,
        AirInstData::Call {
            runtime: Some(_), ..
        } => K::RuntimeCall,
        AirInstData::Call { runtime: None, .. } => K::Call,
        AirInstData::CallGeneric { .. } => K::CallGeneric,
        AirInstData::Intrinsic { .. } => K::Intrinsic,
        AirInstData::Param { .. } => K::Param,
        AirInstData::Block { .. } => K::Block,
        AirInstData::StructInit { .. } => K::StructInit,
        AirInstData::ArrayInit { .. } => K::ArrayInit,
        AirInstData::PlaceRead { .. } => K::PlaceRead,
        AirInstData::PlaceWrite { .. } => K::PlaceWrite,
        AirInstData::EnumVariant { .. } => K::EnumVariant,
        AirInstData::EnumPayloadGet { .. } => K::EnumPayloadGet,
        AirInstData::IntCast { .. } => K::IntCast,
        AirInstData::Drop { .. } => K::Drop,
        AirInstData::StorageLive { .. } => K::StorageLive,
        AirInstData::StorageDead { .. } => K::StorageDead,
        AirInstData::MarkMoved { .. } => K::MarkMoved,
    }
}

pub(crate) fn canonical_type_from_live(
    ty: Type,
    pool: &rue_air::FrozenTypeInternPool,
    aggregates: &HashMap<Type, crate::TypeInstanceKey>,
) -> Result<CanonicalType, CfgDomainFailure> {
    use rue_air::TypeKind as K;
    Ok(match ty.kind() {
        K::I8 => CanonicalType::I8,
        K::I16 => CanonicalType::I16,
        K::I32 => CanonicalType::I32,
        K::I64 => CanonicalType::I64,
        K::U8 => CanonicalType::U8,
        K::U16 => CanonicalType::U16,
        K::U32 => CanonicalType::U32,
        K::U64 => CanonicalType::U64,
        K::Bool => CanonicalType::Bool,
        K::Unit => CanonicalType::Unit,
        K::Never => CanonicalType::Never,
        K::ComptimeType => CanonicalType::ComptimeType,
        K::Array(id) => {
            let (element, len) = pool.array_def(id);
            CanonicalType::Array {
                element: Box::new(canonical_type_from_live(element, pool, aggregates)?),
                len,
            }
        }
        K::PtrConst(id) => CanonicalType::PtrConst(Box::new(canonical_type_from_live(
            pool.ptr_const_def(id),
            pool,
            aggregates,
        )?)),
        K::PtrMut(id) => CanonicalType::PtrMut(Box::new(canonical_type_from_live(
            pool.ptr_mut_def(id),
            pool,
            aggregates,
        )?)),
        K::Struct(_) | K::Enum(_) => {
            let stable = aggregates.get(&ty).ok_or(CfgDomainFailure::Missing)?;
            match stable {
                crate::TypeInstanceKey::BuiltinNominal { kind, name } => {
                    CanonicalType::BuiltinNominal {
                        kind: match kind {
                            crate::AnonymousNominalKind::Struct => {
                                rue_air::SemanticImportNominalKind::Struct
                            }
                            crate::AnonymousNominalKind::Enum => {
                                rue_air::SemanticImportNominalKind::Enum
                            }
                        },
                        name: name.clone(),
                    }
                }
                crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(definition)) => {
                    CanonicalType::Nominal(definition.clone())
                }
                crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(identity)) => {
                    CanonicalType::AnonymousNominal(identity.clone())
                }
                _ => return Err(CfgDomainFailure::Shape),
            }
        }
        K::Module(_) | K::Error => return Err(CfgDomainFailure::Unsupported),
    })
}

fn canonical_primitive(ty: Type) -> Option<CanonicalType> {
    use rue_air::TypeKind as K;
    Some(match ty.kind() {
        K::I8 => CanonicalType::I8,
        K::I16 => CanonicalType::I16,
        K::I32 => CanonicalType::I32,
        K::I64 => CanonicalType::I64,
        K::U8 => CanonicalType::U8,
        K::U16 => CanonicalType::U16,
        K::U32 => CanonicalType::U32,
        K::U64 => CanonicalType::U64,
        K::Bool => CanonicalType::Bool,
        K::Unit => CanonicalType::Unit,
        K::Never => CanonicalType::Never,
        K::ComptimeType => CanonicalType::ComptimeType,
        _ => return None,
    })
}

fn live_primitive(ty: &CanonicalType) -> Option<Type> {
    Some(match ty {
        CanonicalType::I8 => Type::I8,
        CanonicalType::I16 => Type::I16,
        CanonicalType::I32 => Type::I32,
        CanonicalType::I64 => Type::I64,
        CanonicalType::U8 => Type::U8,
        CanonicalType::U16 => Type::U16,
        CanonicalType::U32 => Type::U32,
        CanonicalType::U64 => Type::U64,
        CanonicalType::Bool => Type::BOOL,
        CanonicalType::Unit => Type::UNIT,
        CanonicalType::Never => Type::NEVER,
        CanonicalType::ComptimeType => Type::COMPTIME_TYPE,
        _ => return None,
    })
}

fn foreign_callable_symbol(callable: &crate::FunctionInstanceKey) -> Option<String> {
    match callable {
        crate::FunctionInstanceKey::Definition(definition) => Some(definition.name().to_owned()),
        crate::FunctionInstanceKey::Specialization { base, .. } => foreign_callable_symbol(base),
        crate::FunctionInstanceKey::AnonymousMember { .. }
        | crate::FunctionInstanceKey::DropGlue(_) => None,
    }
}

fn canonical_type_instance(ty: &CanonicalType) -> Option<crate::TypeInstanceKey> {
    Some(match ty {
        CanonicalType::I8 => crate::TypeInstanceKey::I8,
        CanonicalType::I16 => crate::TypeInstanceKey::I16,
        CanonicalType::I32 => crate::TypeInstanceKey::I32,
        CanonicalType::I64 => crate::TypeInstanceKey::I64,
        CanonicalType::U8 => crate::TypeInstanceKey::U8,
        CanonicalType::U16 => crate::TypeInstanceKey::U16,
        CanonicalType::U32 => crate::TypeInstanceKey::U32,
        CanonicalType::U64 => crate::TypeInstanceKey::U64,
        CanonicalType::Bool => crate::TypeInstanceKey::Bool,
        CanonicalType::Unit => crate::TypeInstanceKey::Unit,
        CanonicalType::Never => crate::TypeInstanceKey::Never,
        CanonicalType::ComptimeType => crate::TypeInstanceKey::ComptimeType,
        CanonicalType::BuiltinNominal { kind, name } => crate::TypeInstanceKey::BuiltinNominal {
            kind: match kind {
                rue_air::SemanticImportNominalKind::Struct => crate::AnonymousNominalKind::Struct,
                rue_air::SemanticImportNominalKind::Enum => crate::AnonymousNominalKind::Enum,
            },
            name: name.clone(),
        },
        CanonicalType::Nominal(definition) => {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(definition.clone()))
        }
        CanonicalType::AnonymousNominal(identity) => {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(identity.clone()))
        }
        CanonicalType::Array { element, len } => crate::TypeInstanceKey::Array {
            element: Box::new(canonical_type_instance(element)?),
            len: *len,
        },
        CanonicalType::PtrConst(element) => {
            crate::TypeInstanceKey::PtrConst(Box::new(canonical_type_instance(element)?))
        }
        CanonicalType::PtrMut(element) => {
            crate::TypeInstanceKey::PtrMut(Box::new(canonical_type_instance(element)?))
        }
        CanonicalType::Slice { element, name } => crate::TypeInstanceKey::Slice {
            element: Box::new(canonical_type_instance(element)?),
            name: name.clone(),
        },
        CanonicalType::Module(module) => crate::TypeInstanceKey::Module(module.clone()),
        CanonicalType::GenericParameter(index) => crate::TypeInstanceKey::GenericParameter(*index),
    })
}

fn deduplicate_type_mappings(
    mappings: Vec<(Type, CanonicalType)>,
) -> Result<Vec<(Type, CanonicalType)>, CfgDomainFailure> {
    let mut positions: HashMap<Type, usize> = HashMap::with_capacity(mappings.len());
    let mut unique: Vec<(Type, CanonicalType)> = Vec::with_capacity(mappings.len());
    for (current, stable) in mappings {
        if let Some(&position) = positions.get(&current) {
            if unique[position].1 != stable {
                return Err(CfgDomainFailure::Shape);
            }
        } else {
            positions.insert(current, unique.len());
            unique.push((current, stable));
        }
    }
    Ok(unique)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum StableCfgSymbol {
    Callable(rue_air::FunctionInstanceKey<crate::StableDefinitionKey, crate::ModuleId>),
    #[allow(dead_code)]
    Specialization(
        rue_air::SemanticSpecializationIdentity<crate::StableDefinitionKey, crate::ModuleId>,
    ),
    Runtime(Arc<str>),
    Intrinsic(Arc<str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StableCfgSpan {
    Relative { start: i64, end: i64 },
    Absolute(Span),
}

impl StableCfgSpan {
    fn new(span: Span, body_span: Span) -> Self {
        if span.file_id == body_span.file_id {
            Self::Relative {
                start: i64::from(span.start) - i64::from(body_span.start),
                end: i64::from(span.end) - i64::from(body_span.start),
            }
        } else {
            Self::Absolute(span)
        }
    }

    fn relocate(self, body_span: Span) -> Result<Span, CfgDomainFailure> {
        match self {
            Self::Relative { start, end } => Ok(Span {
                file_id: body_span.file_id,
                start: u32::try_from(i64::from(body_span.start) + start)
                    .map_err(|_| CfgDomainFailure::Shape)?,
                end: u32::try_from(i64::from(body_span.start) + end)
                    .map_err(|_| CfgDomainFailure::Shape)?,
            }),
            Self::Absolute(span) => Ok(span),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CfgDomainProjection {
    body_span: Span,
    types: Vec<(Type, CanonicalType)>,
    strings: Vec<(u32, Arc<str>)>,
    atoms: Vec<rue_air::SemanticBodyLocalAtom<crate::StableDefinitionKey, crate::ModuleId>>,
    spans: Vec<(Span, StableCfgSpan)>,
    symbols: Vec<(Spur, StableCfgSymbol)>,
    incomplete_epoch: Option<Arc<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CfgDomainFailure {
    Shape,
    Unsupported,
    Missing,
    MissingLiveType(Type),
    MissingStableType,
    MissingSymbol,
    MissingString,
    Edit(rue_cfg::CfgEditError),
}

impl CfgDomainProjection {
    pub(crate) fn same_live_domain(&self, other: &Self) -> bool {
        let complete_or_same_epoch = match (&self.incomplete_epoch, &other.incomplete_epoch) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        };
        complete_or_same_epoch
            && self.body_span == other.body_span
            && self.types == other.types
            && self.strings == other.strings
            && self.atoms == other.atoms
            && self.spans == other.spans
            && self.symbols == other.symbols
    }

    /// Build the relocation domain for one canonical analyzed function without
    /// consulting a program-wide CFG cache. Every live type, symbol, string,
    /// local atom, and span is paired with its stable record-local identity.
    pub(crate) fn from_canonical_function(
        function: &AnalyzedFunction,
        stable_identity: &crate::FunctionInstanceKey,
        body_span: Span,
        strings: &[String],
        interner: &lasso::ThreadedRodeo,
        stable_type: impl Fn(Type) -> Result<CanonicalType, CfgDomainFailure>,
        stable_callable: impl Fn(lasso::Spur) -> Option<crate::FunctionInstanceKey>,
    ) -> Result<Self, CfgDomainFailure> {
        let mut types = vec![
            (
                function.air.return_type(),
                stable_type(function.air.return_type())?,
            ),
            (Type::UNIT, CanonicalType::Unit),
        ];
        let mut stable_strings = Vec::new();
        let mut spans = Vec::new();
        let mut symbols = Vec::new();
        let mut atoms = Vec::with_capacity(function.local_atoms.len());
        for atom in &function.local_atoms {
            if atom.identity.producer != function.identity
                || strings.get(atom.dense_id as usize).map(String::as_str)
                    != Some(atom.content.as_ref())
            {
                return Err(CfgDomainFailure::Shape);
            }
            atoms.push(rue_air::SemanticBodyLocalAtom {
                identity: crate::LocalAtomId {
                    producer: stable_identity.clone(),
                    kind: atom.identity.kind,
                    anchor: atom.identity.anchor.clone(),
                },
                content: atom.content.clone(),
            });
        }
        for (_, instruction) in function.air.iter() {
            types.push((instruction.ty, stable_type(instruction.ty)?));
            spans.push((
                instruction.span,
                StableCfgSpan::new(instruction.span, body_span),
            ));
            match &instruction.data {
                AirInstData::StringConst(index) => {
                    stable_strings.push((
                        *index,
                        strings
                            .get(*index as usize)
                            .ok_or(CfgDomainFailure::Shape)?
                            .as_str()
                            .into(),
                    ));
                }
                AirInstData::IntCast { from_ty, .. } => {
                    types.push((*from_ty, stable_type(*from_ty)?));
                }
                AirInstData::Call { name, .. } => {
                    let symbol = stable_callable(*name)
                        .map(StableCfgSymbol::Callable)
                        .unwrap_or_else(|| {
                            StableCfgSymbol::Intrinsic(Arc::from(interner.resolve(name)))
                        });
                    symbols.push((*name, symbol));
                }
                AirInstData::Intrinsic { name, .. } => {
                    symbols.push((
                        *name,
                        StableCfgSymbol::Intrinsic(Arc::from(interner.resolve(name))),
                    ));
                }
                _ => {}
            }
        }
        for place in function.air.places() {
            types.push((place.base_type, stable_type(place.base_type)?));
            for projection in function.air.get_place_projections(place) {
                match projection {
                    rue_air::AirProjection::Field { struct_id, .. } => {
                        let ty = Type::new_struct(*struct_id);
                        types.push((ty, stable_type(ty)?));
                    }
                    rue_air::AirProjection::Index { array_type, .. } => {
                        types.push((*array_type, stable_type(*array_type)?));
                    }
                }
            }
        }
        for (_, ty) in function.air.param_drops() {
            types.push((*ty, stable_type(*ty)?));
        }
        let types = deduplicate_type_mappings(types)?;
        stable_strings.sort_by_key(|(index, _)| *index);
        stable_strings.dedup();
        spans.sort_by_key(|(span, _)| (span.file_id.index(), span.start, span.end));
        spans.dedup();
        symbols.sort_by(|left, right| {
            (left.0.into_usize(), &left.1).cmp(&(right.0.into_usize(), &right.1))
        });
        symbols.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        if symbols.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(CfgDomainFailure::Shape);
        }
        atoms.sort_by(|left, right| {
            (&left.identity, &left.content).cmp(&(&right.identity, &right.content))
        });
        Ok(Self {
            body_span,
            types,
            strings: stable_strings,
            atoms,
            spans,
            symbols,
            incomplete_epoch: None,
        })
    }

    pub(crate) fn stable_types(&self) -> impl Iterator<Item = &CanonicalType> {
        self.types.iter().map(|(_, stable)| stable)
    }

    /// Return the stable callable identities of the source-language calls that
    /// actually survived AIR lowering into this CFG. The durable symbol domain
    /// is deliberately broader: it also closes over compile-time constructors
    /// and cleanup aliases that may never become runtime call instructions.
    pub(crate) fn runtime_callables(
        &self,
        cfg: &rue_cfg::Cfg,
    ) -> Result<std::collections::BTreeSet<crate::FunctionInstanceKey>, CfgDomainFailure> {
        let mut callables = std::collections::BTreeSet::new();
        for block in cfg.blocks() {
            for value in &block.insts {
                let rue_cfg::CfgInstData::Call {
                    runtime: None,
                    name,
                    ..
                } = &cfg.get_inst(*value).data
                else {
                    continue;
                };
                let stable = self
                    .symbols
                    .iter()
                    .find_map(|(live, stable)| (live == name).then_some(stable))
                    .ok_or(CfgDomainFailure::MissingSymbol)?;
                let callable = match stable {
                    StableCfgSymbol::Callable(callable) => callable.clone(),
                    StableCfgSymbol::Specialization(identity) => {
                        crate::semantic_identity::function_instance_from_specialization(identity)
                            .ok_or(CfgDomainFailure::Shape)?
                    }
                    StableCfgSymbol::Runtime(_) | StableCfgSymbol::Intrinsic(_) => {
                        return Err(CfgDomainFailure::Shape);
                    }
                };
                callables.insert(callable);
            }
        }
        Ok(callables)
    }

    /// Project the exact machine-symbol domain consumed by code generation.
    ///
    /// The live names remain paired with the CFG/interner that issued them,
    /// while callable identities and ABI facts determine their durable machine
    /// names. This keeps codegen independent of a whole-program resolver and
    /// makes foreign classification an exact per-CFG dependency.
    pub(crate) fn codegen_domain(
        &self,
        function: &crate::FunctionInstanceKey,
        source_name: &str,
        type_pool: &rue_air::FrozenTypeInternPool,
        interner: &lasso::ThreadedRodeo,
        call_abis: &std::collections::BTreeMap<
            crate::FunctionInstanceKey,
            crate::type_queries::CallAbiFacts,
        >,
    ) -> Result<crate::cfg_query::CfgCodegenDomain, CfgDomainFailure> {
        let defined_symbol: Arc<str> = if source_name == "main" {
            Arc::from("main")
        } else {
            Arc::from(crate::StableSymbolEncoder::encode(
                &crate::StableSymbolId::Callable(crate::StableCallableId::Function(
                    function.clone(),
                )),
            ))
        };
        let mut symbol_mappings = std::collections::BTreeMap::new();
        let mut foreign_symbols = std::collections::BTreeSet::new();
        for (live, stable) in &self.symbols {
            let source = interner.resolve(live).to_owned();
            let (machine, foreign) = match stable {
                StableCfgSymbol::Callable(callable) => {
                    let foreign = call_abis.get(callable).is_some_and(|facts| {
                        matches!(
                            facts.convention,
                            crate::type_queries::CallAbiConvention::TargetC(_)
                        )
                    });
                    let machine = if foreign {
                        foreign_callable_symbol(callable).ok_or(CfgDomainFailure::Shape)?
                    } else if source == "main" {
                        source.clone()
                    } else {
                        crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
                            crate::StableCallableId::Function(callable.clone()),
                        ))
                    };
                    (machine, foreign)
                }
                StableCfgSymbol::Specialization(identity) => {
                    let callable =
                        crate::semantic_identity::function_instance_from_specialization(identity)
                            .ok_or(CfgDomainFailure::Shape)?;
                    let foreign = call_abis.get(&callable).is_some_and(|facts| {
                        matches!(
                            facts.convention,
                            crate::type_queries::CallAbiConvention::TargetC(_)
                        )
                    });
                    let machine = if foreign {
                        foreign_callable_symbol(&callable).ok_or(CfgDomainFailure::Shape)?
                    } else if source == "main" {
                        source.clone()
                    } else {
                        crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
                            crate::StableCallableId::Function(callable),
                        ))
                    };
                    (machine, foreign)
                }
                StableCfgSymbol::Runtime(symbol) | StableCfgSymbol::Intrinsic(symbol) => {
                    (symbol.to_string(), false)
                }
            };
            if foreign {
                foreign_symbols.insert(machine.clone());
            }
            if let Some(previous) = symbol_mappings.insert(source, machine.clone())
                && previous != machine
            {
                return Err(CfgDomainFailure::Shape);
            }
        }
        // Cleanup elaboration may synthesize destructor and drop-glue calls
        // from aggregate metadata after AIR symbol collection. Project those
        // exact aliases from the same local type pool and stable type domain.
        for (current, stable) in &self.types {
            let Some(owner) = canonical_type_instance(stable) else {
                continue;
            };
            let drop_glue_source = match current.kind() {
                TypeKind::Struct(id) => {
                    if let (Some(source), CanonicalType::AnonymousNominal(identity)) =
                        (&type_pool.struct_def(id).destructor, stable)
                    {
                        let callable = crate::FunctionInstanceKey::AnonymousMember {
                            owner: Box::new(crate::TypeInstanceKey::Nominal(
                                crate::NominalInstanceKey::Anonymous(identity.clone()),
                            )),
                            member: crate::AnonymousMemberKey {
                                kind: crate::AnonymousMemberKind::Destructor,
                                name: Arc::from("__drop"),
                            },
                        };
                        let machine =
                            crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
                                crate::StableCallableId::Function(callable),
                            ));
                        if let Some(previous) =
                            symbol_mappings.insert(source.clone(), machine.clone())
                            && previous != machine
                        {
                            return Err(CfgDomainFailure::Shape);
                        }
                    }
                    Some(rue_air::drop_glue_names::struct_drop_glue_name(
                        id, type_pool,
                    ))
                }
                TypeKind::Enum(id) => {
                    Some(rue_air::drop_glue_names::enum_drop_glue_name(id, type_pool))
                }
                TypeKind::Array(id) => Some(rue_air::drop_glue_names::array_drop_glue_name(
                    id, type_pool,
                )),
                _ => None,
            };
            let Some(drop_glue_source) = drop_glue_source else {
                continue;
            };
            let machine = crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
                crate::StableCallableId::Function(crate::FunctionInstanceKey::DropGlue(Box::new(
                    owner,
                ))),
            ));
            if let Some(previous) = symbol_mappings.insert(drop_glue_source, machine.clone())
                && previous != machine
            {
                return Err(CfgDomainFailure::Shape);
            }
        }
        Ok(crate::cfg_query::CfgCodegenDomain {
            defined_symbol,
            symbol_mappings: Arc::new(symbol_mappings),
            foreign_symbols: Arc::new(foreign_symbols),
        })
    }

    #[allow(dead_code)]
    pub(crate) fn remap_span(
        old: &Self,
        new: &Self,
        span: Span,
        new_body_span: Span,
    ) -> Result<Span, CfgDomainFailure> {
        let anchor = old
            .spans
            .iter()
            .find(|(candidate, _)| *candidate == span)
            .map(|(_, anchor)| anchor)
            .ok_or(CfgDomainFailure::Missing)?;
        if !new.spans.iter().any(|(_, candidate)| candidate == anchor) {
            return Err(CfgDomainFailure::Missing);
        }
        anchor.relocate(new_body_span)
    }

    #[allow(dead_code)]
    pub fn validate_cfg(&self, cfg: &rue_cfg::Cfg, span: Span) -> Result<(), CfgDomainFailure> {
        Self::import_cfg(self, self, cfg, span).map(|_| ())
    }
    pub fn from_body(
        function: &AnalyzedFunction,
        stable_function: &crate::FunctionInstanceKey,
        body: &rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
        body_span: Span,
        strings: &[String],
        type_pool: &rue_air::FrozenTypeInternPool,
        interner: &lasso::ThreadedRodeo,
        stable_type: impl Fn(Type) -> Result<CanonicalType, CfgDomainFailure>,
        stable_callable: impl Fn(lasso::Spur) -> Option<crate::FunctionInstanceKey>,
    ) -> Result<Self, CfgDomainFailure> {
        Self::from_body_parts(
            &function.air,
            &function.identity,
            &function.local_atoms,
            stable_function,
            body,
            body_span,
            strings,
            type_pool,
            interner,
            stable_type,
            stable_callable,
        )
    }

    pub fn from_local_body(
        materialization: &rue_air::SemanticLocalMaterialization<
            crate::StableDefinitionKey,
            crate::ModuleId,
        >,
        body: &rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
        stable_type: impl Fn(Type) -> Result<CanonicalType, CfgDomainFailure>,
        stable_callable: impl Fn(lasso::Spur) -> Option<crate::FunctionInstanceKey>,
    ) -> Result<Self, CfgDomainFailure> {
        Self::from_body_parts(
            &materialization.air,
            &materialization.identity,
            &materialization.local_atoms,
            &materialization.identity,
            body,
            materialization.body_span,
            &materialization.strings,
            &materialization.type_pool,
            &materialization.interner,
            stable_type,
            stable_callable,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_body_parts<D: PartialEq, M: PartialEq>(
        air: &rue_air::ValidatedAir,
        current_identity: &rue_air::FunctionInstanceKey<D, M>,
        local_atoms: &[rue_air::LocalAtomRecord<D, M>],
        stable_function: &crate::FunctionInstanceKey,
        body: &rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
        body_span: Span,
        strings: &[String],
        type_pool: &rue_air::FrozenTypeInternPool,
        interner: &lasso::ThreadedRodeo,
        stable_type: impl Fn(Type) -> Result<CanonicalType, CfgDomainFailure>,
        stable_callable: impl Fn(lasso::Spur) -> Option<crate::FunctionInstanceKey>,
    ) -> Result<Self, CfgDomainFailure> {
        if air.instructions().len() != body.instructions.len() {
            return Err(CfgDomainFailure::Shape);
        }
        let mut types = vec![
            (air.return_type(), body.return_type.clone()),
            (Type::I32, CanonicalType::I32),
            (Type::UNIT, CanonicalType::Unit),
        ];
        let mut stable_strings = Vec::new();
        let mut spans = Vec::new();
        let mut symbols = Vec::new();
        if local_atoms.len() != body.local_atoms.len() {
            return Err(CfgDomainFailure::Shape);
        }
        if local_atoms
            .iter()
            .any(|atom| atom.identity.producer != *current_identity)
            || body
                .local_atoms
                .iter()
                .any(|atom| atom.identity.producer != *stable_function)
        {
            return Err(CfgDomainFailure::Shape);
        }
        let mut current_atoms = local_atoms
            .iter()
            .map(|atom| {
                if strings.get(atom.dense_id as usize).map(String::as_str)
                    != Some(atom.content.as_ref())
                {
                    return Err(CfgDomainFailure::Shape);
                }
                Ok((
                    atom.identity.kind,
                    atom.identity.anchor.clone(),
                    atom.content.clone(),
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut stable_atoms = body
            .local_atoms
            .iter()
            .map(|atom| {
                (
                    atom.identity.kind,
                    atom.identity.anchor.clone(),
                    atom.content.clone(),
                )
            })
            .collect::<Vec<_>>();
        current_atoms.sort();
        stable_atoms.sort();
        if current_atoms != stable_atoms || current_atoms.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(CfgDomainFailure::Shape);
        }
        for ((_, current), durable) in air.iter().zip(body.instructions.iter()) {
            let current_kind = live_instruction_kind(&current.data);
            let durable_kind = durable.data.kind();
            if current_kind != durable_kind
                && !(current_kind == rue_air::SemanticBodyInstKind::Call
                    && durable_kind == rue_air::SemanticBodyInstKind::CallSpecialized)
            {
                return Err(CfgDomainFailure::Shape);
            }
            types.push((current.ty, durable.ty.clone()));
            spans.push((current.span, StableCfgSpan::new(current.span, body_span)));
            match (&current.data, &durable.data) {
                (AirInstData::StringConst(old), DurableAirInstData::StringConst(local)) => {
                    let value = body
                        .strings
                        .get(*local as usize)
                        .ok_or(CfgDomainFailure::Shape)?;
                    if strings.get(*old as usize).map(String::as_str) != Some(value) {
                        return Err(CfgDomainFailure::Shape);
                    }
                    stable_strings.push((*old, value.clone()));
                }
                (AirInstData::TypeConst(current), DurableAirInstData::TypeConst(stable)) => {
                    types.push((*current, stable.clone()))
                }
                (
                    AirInstData::IntCast { from_ty, .. },
                    DurableAirInstData::IntCast {
                        from_ty: stable, ..
                    },
                ) => types.push((*from_ty, stable.clone())),
                (
                    AirInstData::Call {
                        runtime: None,
                        name,
                        ..
                    },
                    DurableAirInstData::Call { function, .. },
                ) => symbols.push((*name, StableCfgSymbol::Callable(function.clone()))),
                (
                    AirInstData::Call {
                        runtime: None,
                        name,
                        ..
                    },
                    DurableAirInstData::CallSpecialized { identity, .. },
                ) => symbols.push((*name, StableCfgSymbol::Specialization(identity.clone()))),
                (
                    AirInstData::Call {
                        runtime: Some(current),
                        name,
                        ..
                    },
                    DurableAirInstData::RuntimeCall {
                        runtime: stable, ..
                    },
                ) if current == stable
                    && interner.resolve(name) == current.helper().helper().symbol =>
                {
                    symbols.push((
                        *name,
                        StableCfgSymbol::Runtime(Arc::from(stable.helper().helper().symbol)),
                    ));
                }
                (
                    AirInstData::Intrinsic { runtime, name, .. },
                    DurableAirInstData::Intrinsic {
                        runtime: stable_runtime,
                        name: stable,
                        ..
                    },
                ) if runtime == stable_runtime && interner.resolve(name) == stable.as_ref() => {
                    symbols.push((*name, StableCfgSymbol::Intrinsic(stable.clone())));
                }
                (
                    AirInstData::StructInit { struct_id, .. },
                    DurableAirInstData::StructInit { struct_key, .. },
                ) => types.push((Type::new_struct(*struct_id), canonical_nominal(struct_key))),
                (
                    AirInstData::EnumVariant { enum_id, .. },
                    DurableAirInstData::EnumVariant { enum_key, .. },
                )
                | (
                    AirInstData::EnumPayloadGet { enum_id, .. },
                    DurableAirInstData::EnumPayloadGet { enum_key, .. },
                ) => types.push((Type::new_enum(*enum_id), canonical_nominal(enum_key))),
                (
                    AirInstData::Match { arms, .. },
                    DurableAirInstData::Match { arms: stable, .. },
                ) => {
                    let current = air.get_match_arms(arms).collect::<Vec<_>>();
                    if current.len() != stable.len() {
                        return Err(CfgDomainFailure::Shape);
                    }
                    for ((pattern, _), stable) in current.into_iter().zip(stable.iter()) {
                        match (pattern, &stable.pattern) {
                            (
                                rue_air::AirPattern::EnumVariant { enum_id, .. },
                                rue_air::SemanticBodyPattern::EnumVariant { enum_key, .. },
                            ) => types.push((Type::new_enum(enum_id), canonical_nominal(enum_key))),
                            (rue_air::AirPattern::EnumVariant { .. }, _)
                            | (_, rue_air::SemanticBodyPattern::EnumVariant { .. }) => {
                                return Err(CfgDomainFailure::Shape);
                            }
                            _ => {}
                        }
                    }
                }
                _ if current_kind == durable_kind => {}
                _ => return Err(CfgDomainFailure::Shape),
            }
        }
        for (current, stable) in air.places().iter().zip(body.places.iter()) {
            types.push((current.base_type, stable.base_type.clone()));
            let current_projections = air.get_place_projections(current);
            if current_projections.len() != stable.projections.len() {
                return Err(CfgDomainFailure::Shape);
            }
            for (current, stable) in current_projections.iter().zip(stable.projections.iter()) {
                match (current, stable) {
                    (
                        rue_air::AirProjection::Field { struct_id, .. },
                        crate::DurableProjection::Field { struct_key, .. },
                    ) => types.push((Type::new_struct(*struct_id), canonical_nominal(struct_key))),
                    (
                        rue_air::AirProjection::Index { array_type, .. },
                        crate::DurableProjection::Index {
                            array_type: stable, ..
                        },
                    ) => types.push((*array_type, stable.clone())),
                    _ => return Err(CfgDomainFailure::Shape),
                }
            }
        }
        if air.param_drops().len() != body.param_drops.len() {
            return Err(CfgDomainFailure::Shape);
        }
        for ((current_slot, current), (stable_slot, stable)) in
            air.param_drops().iter().zip(body.param_drops.iter())
        {
            if current_slot != stable_slot {
                return Err(CfgDomainFailure::Shape);
            }
            types.push((*current, stable.clone()));
        }
        // CFG cleanup elaboration recursively reads aggregate fields, variant
        // payloads, arrays, and destructor symbols. Close the live type domain
        // over those facts so every emitted CFG handle can be relocated.
        let mut types = deduplicate_type_mappings(types)?;
        let mut pending = types
            .iter()
            .map(|(current, _)| *current)
            .collect::<Vec<_>>();
        let mut visited = std::collections::HashSet::new();
        let mut incomplete = false;
        while let Some(current) = pending.pop() {
            if !visited.insert(current) {
                continue;
            }
            let children = match current.kind() {
                TypeKind::Struct(id) => {
                    let definition = type_pool.struct_def(id);
                    if let Some(name) = &definition.destructor {
                        let symbol = interner.get_or_intern(name);
                        if let Some(callable) = stable_callable(symbol) {
                            symbols.push((symbol, StableCfgSymbol::Callable(callable)));
                        } else if let Some(CanonicalType::AnonymousNominal(owner)) =
                            types.iter().find_map(|(candidate, stable)| {
                                (*candidate == current).then_some(stable)
                            })
                        {
                            symbols.push((
                                symbol,
                                StableCfgSymbol::Callable(
                                    crate::FunctionInstanceKey::AnonymousMember {
                                        owner: Box::new(crate::TypeInstanceKey::Nominal(
                                            crate::NominalInstanceKey::Anonymous(owner.clone()),
                                        )),
                                        member: crate::AnonymousMemberKey {
                                            kind: crate::AnonymousMemberKind::Destructor,
                                            name: Arc::from("__drop"),
                                        },
                                    },
                                ),
                            ));
                        } else {
                            incomplete = true;
                        }
                    }
                    definition
                        .fields
                        .iter()
                        .map(|field| field.ty)
                        .collect::<Vec<_>>()
                }
                TypeKind::Enum(id) => type_pool
                    .enum_def(id)
                    .variant_payloads
                    .iter()
                    .flatten()
                    .copied()
                    .collect(),
                TypeKind::Array(id) => vec![type_pool.array_def(id).0],
                _ => Vec::new(),
            };
            for child in children {
                if visited.contains(&child) {
                    continue;
                }
                if !types.iter().any(|(current, _)| *current == child) {
                    match stable_type(child) {
                        Ok(stable) => types.push((child, stable)),
                        Err(CfgDomainFailure::Missing | CfgDomainFailure::Unsupported) => {
                            incomplete = true;
                            continue;
                        }
                        Err(failure) => return Err(failure),
                    }
                }
                pending.push(child);
            }
        }
        stable_strings.sort_by_key(|(index, _)| *index);
        stable_strings.dedup();
        spans.sort_by_key(|(span, _)| (span.file_id.index(), span.start, span.end));
        spans.dedup();
        symbols.sort_by(|left, right| {
            (left.0.into_usize(), &left.1).cmp(&(right.0.into_usize(), &right.1))
        });
        symbols.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        if symbols.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(CfgDomainFailure::Shape);
        }
        let mut atoms = body.local_atoms.to_vec();
        atoms.sort_by(|left, right| {
            (&left.identity, &left.content).cmp(&(&right.identity, &right.content))
        });
        Ok(Self {
            body_span,
            types,
            strings: stable_strings,
            atoms,
            spans,
            symbols,
            incomplete_epoch: incomplete.then(|| Arc::new(())),
        })
    }

    fn stable_type(&self, value: Type) -> Result<CanonicalType, CfgDomainFailure> {
        if let Some(stable) = canonical_primitive(value) {
            return Ok(stable);
        }
        self.types
            .iter()
            .find(|(current, _)| *current == value)
            .map(|(_, stable)| stable.clone())
            .ok_or(CfgDomainFailure::MissingLiveType(value))
    }
    fn current_type(&self, value: &CanonicalType) -> Result<Type, CfgDomainFailure> {
        if let Some(current) = live_primitive(value) {
            return Ok(current);
        }
        self.types
            .iter()
            .find(|(_, stable)| stable == value)
            .map(|(current, _)| *current)
            .ok_or(CfgDomainFailure::MissingStableType)
    }
    fn stable_nominal(&self, value: Type) -> Result<CanonicalType, CfgDomainFailure> {
        self.stable_type(value)
    }
    fn current_nominal(&self, value: &CanonicalType) -> Result<Type, CfgDomainFailure> {
        self.current_type(value)
    }

    pub fn import_cfg(
        old: &Self,
        new: &Self,
        cfg: &rue_cfg::Cfg,
        new_span: Span,
    ) -> Result<rue_cfg::CfgEditor, CfgDomainFailure> {
        if old.incomplete_epoch.is_some() || new.incomplete_epoch.is_some() {
            return Err(CfgDomainFailure::Missing);
        }
        if old.atoms != new.atoms {
            return Err(CfgDomainFailure::Shape);
        }
        cfg.try_remap_domains(
            |value| new.current_type(&old.stable_type(value)?),
            |value| match new
                .current_nominal(&old.stable_nominal(Type::new_struct(value))?)?
                .kind()
            {
                TypeKind::Struct(id) => Ok(id),
                _ => Err(CfgDomainFailure::Shape),
            },
            |value| match new
                .current_nominal(&old.stable_nominal(Type::new_enum(value))?)?
                .kind()
            {
                TypeKind::Enum(id) => Ok(id),
                _ => Err(CfgDomainFailure::Shape),
            },
            |value: Spur| {
                let stable = old
                    .symbols
                    .iter()
                    .find(|(symbol, _)| *symbol == value)
                    .map(|(_, value)| value)
                    .ok_or(CfgDomainFailure::MissingSymbol)?;
                new.symbols
                    .iter()
                    .find(|(_, identity)| identity == stable)
                    .map(|(symbol, _)| *symbol)
                    .ok_or(CfgDomainFailure::MissingSymbol)
            },
            |value| {
                let stable = old
                    .strings
                    .iter()
                    .find(|(index, _)| *index == value)
                    .map(|(_, value)| value)
                    .ok_or(CfgDomainFailure::MissingString)?;
                new.strings
                    .iter()
                    .find(|(_, value)| value == stable)
                    .map(|(index, _)| *index)
                    .ok_or(CfgDomainFailure::MissingString)
            },
            |value| {
                let anchor = old
                    .spans
                    .iter()
                    .find(|(span, _)| *span == value)
                    .map(|(_, anchor)| *anchor)
                    .unwrap_or_else(|| StableCfgSpan::new(value, old.body_span));
                anchor.relocate(new_span)
            },
        )
        .map_err(|error| match error {
            rue_cfg::CfgRemapError::Domain(error) => error,
            rue_cfg::CfgRemapError::Edit(error) => CfgDomainFailure::Edit(error),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::Key;
    use rue_cfg::{Cfg, CfgInstData};

    fn projection_with(symbol: Spur, stable: StableCfgSymbol) -> CfgDomainProjection {
        CfgDomainProjection {
            body_span: Span::new(4, 5),
            types: vec![(Type::I32, SemanticImportType::I32)],
            strings: Vec::new(),
            atoms: Vec::new(),
            spans: vec![(
                Span::new(4, 5),
                StableCfgSpan::Relative { start: 0, end: 1 },
            )],
            symbols: vec![(symbol, stable)],
            incomplete_epoch: None,
        }
    }

    fn projection(symbol: Spur) -> CfgDomainProjection {
        projection_with(symbol, StableCfgSymbol::Intrinsic(Arc::from("stable")))
    }

    #[test]
    fn type_mapping_deduplication_preserves_encounter_order_and_rejects_conflicts() {
        let mappings = deduplicate_type_mappings(vec![
            (Type::I64, SemanticImportType::I64),
            (Type::I32, SemanticImportType::I32),
            (Type::I64, SemanticImportType::I64),
        ])
        .unwrap();
        assert_eq!(
            mappings,
            vec![
                (Type::I64, SemanticImportType::I64),
                (Type::I32, SemanticImportType::I32)
            ]
        );

        assert_eq!(
            deduplicate_type_mappings(vec![
                (Type::I32, SemanticImportType::I32),
                (Type::I32, SemanticImportType::I64),
            ]),
            Err(CfgDomainFailure::Shape)
        );
    }

    #[test]
    fn symbol_import_joins_to_existing_current_spur_and_failure_is_atomic() {
        let old = Spur::try_from_usize(3).unwrap();
        let new = Spur::try_from_usize(17).unwrap();
        let mut cfg = Cfg::new(Type::I32, 0, 0, "f".into(), Vec::<bool>::new());
        let block = cfg.new_block();
        cfg.append_call(block, None, old, [], Type::I32, Span::new(4, 5))
            .unwrap();
        let imported = CfgDomainProjection::import_cfg(
            &projection(old),
            &projection(new),
            &cfg,
            Span::new(40, 50),
        )
        .unwrap();
        assert!(
            matches!(imported.get_inst(rue_cfg::CfgValue::from_raw(0)).data, CfgInstData::Call { name, .. } if name == new)
        );

        let mut missing = projection(new);
        missing.symbols.clear();
        assert!(matches!(
            CfgDomainProjection::import_cfg(&projection(old), &missing, &cfg, Span::new(40, 50)),
            Err(CfgDomainFailure::MissingSymbol)
        ));
        assert!(
            matches!(cfg.get_inst(rue_cfg::CfgValue::from_raw(0)).data, CfgInstData::Call { name, .. } if name == old)
        );
    }

    #[test]
    fn runtime_call_symbol_import_uses_stable_runtime_identity() {
        let old = Spur::try_from_usize(5).unwrap();
        let new = Spur::try_from_usize(29).unwrap();
        let stable = StableCfgSymbol::Runtime(Arc::from("__rue_to_string"));
        let mut cfg = Cfg::new(Type::I32, 0, 0, "f".into(), Vec::<bool>::new());
        let block = cfg.new_block();
        cfg.append_call(block, None, old, [], Type::I32, Span::new(4, 5))
            .unwrap();

        let imported = CfgDomainProjection::import_cfg(
            &projection_with(old, stable.clone()),
            &projection_with(new, stable),
            &cfg,
            Span::new(40, 50),
        )
        .unwrap();
        assert!(
            matches!(imported.get_inst(rue_cfg::CfgValue::from_raw(0)).data, CfgInstData::Call { name, .. } if name == new)
        );
    }
}
