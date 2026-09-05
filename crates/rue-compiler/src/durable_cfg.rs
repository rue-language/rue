use ahash::{AHashMap, AHashSet};
use rue_air::Node;
use std::sync::Arc;

use lasso::{Key, Spur};
use rue_air::{AirInstData, SemanticImportType, Type, TypeKind};
use rue_span::Span;

use crate::retained_charge::RetainedCharge;

type CanonicalType = SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>;

pub(crate) trait AggregateTypeLookup {
    fn aggregate_type(&self, ty: Type) -> Option<&crate::TypeInstanceKey>;
}

impl AggregateTypeLookup
    for rue_air::SemanticLocalMaterialization<crate::StableDefinitionKey, crate::ModuleId>
{
    fn aggregate_type(&self, ty: Type) -> Option<&crate::TypeInstanceKey> {
        self.aggregate_type(ty)
    }
}

#[cfg(test)]
pub(crate) struct CfgTypeAdmissionIndex<'a> {
    pool: &'a rue_air::FrozenTypeInternPool,
    aggregates: &'a dyn AggregateTypeLookup,
    live_by_stable: Option<AHashMap<CanonicalType, Type>>,
}

#[cfg(test)]
impl<'a> CfgTypeAdmissionIndex<'a> {
    pub(crate) fn new(
        pool: &'a rue_air::FrozenTypeInternPool,
        aggregates: &'a dyn AggregateTypeLookup,
    ) -> Self {
        Self {
            pool,
            aggregates,
            live_by_stable: None,
        }
    }

    fn current(&mut self, stable: &CanonicalType) -> Option<Type> {
        let pool = self.pool;
        let aggregates = self.aggregates;
        self.live_by_stable
            .get_or_insert_with(|| {
                let mut live_by_stable = AHashMap::with_capacity(pool.len());
                let mut stable_by_live = AHashMap::with_capacity(pool.len());
                for live in pool.all_types() {
                    if let Ok(stable) =
                        canonical_type_from_live_cached(live, pool, aggregates, &mut stable_by_live)
                    {
                        live_by_stable.entry(stable).or_insert(live);
                    }
                }
                live_by_stable
            })
            .get(stable)
            .copied()
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
        AirInstData::AccessorCall { .. } => K::AccessorCall,
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

fn canonical_type_from_live_cached(
    ty: Type,
    pool: &rue_air::FrozenTypeInternPool,
    aggregates: &dyn AggregateTypeLookup,
    stable_by_live: &mut AHashMap<Type, CanonicalType>,
) -> Result<CanonicalType, CfgDomainFailure> {
    if let Some(stable) = stable_by_live.get(&ty) {
        return Ok(stable.clone());
    }
    use rue_air::TypeKind as K;
    let stable = match ty.kind() {
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
        K::F32 => CanonicalType::F32,
        K::F64 => CanonicalType::F64,
        K::ComptimeFloat => CanonicalType::ComptimeFloat,
        K::Array(id) => {
            let (element, len) = pool.array_def(id);
            CanonicalType::Array {
                element: Arc::new(canonical_type_from_live_cached(
                    element,
                    pool,
                    aggregates,
                    stable_by_live,
                )?),
                len,
            }
        }
        K::PtrConst(id) => CanonicalType::PtrConst(Arc::new(canonical_type_from_live_cached(
            pool.ptr_const_def(id),
            pool,
            aggregates,
            stable_by_live,
        )?)),
        K::PtrMut(id) => CanonicalType::PtrMut(Arc::new(canonical_type_from_live_cached(
            pool.ptr_mut_def(id),
            pool,
            aggregates,
            stable_by_live,
        )?)),
        K::Struct(_) | K::Enum(_) => canonical_type_from_instance(
            aggregates
                .aggregate_type(ty)
                .ok_or(CfgDomainFailure::Missing)?,
        )?,
        K::Module(_) | K::Error => return Err(CfgDomainFailure::Unsupported),
    };
    stable_by_live.insert(ty, stable.clone());
    Ok(stable)
}

fn canonical_type_from_instance(
    value: &crate::TypeInstanceKey,
) -> Result<CanonicalType, CfgDomainFailure> {
    use crate::TypeInstanceKey as T;
    Ok(match value {
        T::I8 => CanonicalType::I8,
        T::I16 => CanonicalType::I16,
        T::I32 => CanonicalType::I32,
        T::I64 => CanonicalType::I64,
        T::U8 => CanonicalType::U8,
        T::U16 => CanonicalType::U16,
        T::U32 => CanonicalType::U32,
        T::U64 => CanonicalType::U64,
        T::Bool => CanonicalType::Bool,
        T::Unit => CanonicalType::Unit,
        T::Never => CanonicalType::Never,
        T::ComptimeType => CanonicalType::ComptimeType,
        T::F32 => CanonicalType::F32,
        T::F64 => CanonicalType::F64,
        T::ComptimeFloat => CanonicalType::ComptimeFloat,
        T::BuiltinNominal { kind, name }
        | T::Nominal(crate::NominalInstanceKey::Builtin { kind, name }) => {
            CanonicalType::BuiltinNominal {
                kind: match kind {
                    crate::AnonymousNominalKind::Struct => {
                        rue_air::SemanticImportNominalKind::Struct
                    }
                    crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
                },
                name: name.clone(),
            }
        }
        T::Nominal(crate::NominalInstanceKey::Named(definition)) => {
            CanonicalType::Nominal(definition.clone())
        }
        T::Nominal(crate::NominalInstanceKey::Anonymous(identity)) => {
            CanonicalType::AnonymousNominal((**identity).clone())
        }
        T::Array { element, len } => CanonicalType::Array {
            element: Arc::new(canonical_type_from_instance(element)?),
            len: *len,
        },
        T::Slice { element, name } => CanonicalType::Slice {
            element: Arc::new(canonical_type_from_instance(element)?),
            name: name.clone(),
        },
        T::PtrConst(element) => {
            CanonicalType::PtrConst(Arc::new(canonical_type_from_instance(element)?))
        }
        T::PtrMut(element) => {
            CanonicalType::PtrMut(Arc::new(canonical_type_from_instance(element)?))
        }
        T::Module(module) => CanonicalType::Module(module.clone()),
        T::GenericParameter(index) => CanonicalType::GenericParameter(*index),
    })
}

fn record_cfg_type(
    types: &mut Vec<(Type, CanonicalType)>,
    ty: Type,
    pool: &rue_air::FrozenTypeInternPool,
    aggregates: &dyn AggregateTypeLookup,
    stable_by_live: &mut AHashMap<Type, CanonicalType>,
) -> Result<(), CfgDomainFailure> {
    match canonical_type_from_live_cached(ty, pool, aggregates, stable_by_live) {
        Ok(stable) => types.push((ty, stable)),
        // Module and error values are compile-time-only AIR facts. They do not
        // survive CFG lowering and therefore do not belong to its relocation
        // domain.
        Err(CfgDomainFailure::Unsupported) => {}
        Err(failure) => return Err(failure),
    }
    Ok(())
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
        K::F32 => CanonicalType::F32,
        K::F64 => CanonicalType::F64,
        K::ComptimeFloat => CanonicalType::ComptimeFloat,
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
        CanonicalType::F32 => Type::F32,
        CanonicalType::F64 => Type::F64,
        CanonicalType::ComptimeFloat => Type::COMPTIME_FLOAT,
        _ => return None,
    })
}

fn foreign_callable_symbol(callable: &crate::FunctionInstanceKey) -> Option<String> {
    crate::semantic_identity::function_base_definition(callable)
        .map(|definition| definition.name().to_owned())
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
        CanonicalType::F32 => crate::TypeInstanceKey::F32,
        CanonicalType::F64 => crate::TypeInstanceKey::F64,
        CanonicalType::ComptimeFloat => crate::TypeInstanceKey::ComptimeFloat,
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
        CanonicalType::AnonymousNominal(identity) => crate::TypeInstanceKey::Nominal(
            crate::NominalInstanceKey::Anonymous(Node::new(identity.clone())),
        ),
        CanonicalType::Array { element, len } => crate::TypeInstanceKey::Array {
            element: Node::new(canonical_type_instance(element)?),
            len: *len,
        },
        CanonicalType::PtrConst(element) => {
            crate::TypeInstanceKey::PtrConst(Node::new(canonical_type_instance(element)?))
        }
        CanonicalType::PtrMut(element) => {
            crate::TypeInstanceKey::PtrMut(Node::new(canonical_type_instance(element)?))
        }
        CanonicalType::Slice { element, name } => crate::TypeInstanceKey::Slice {
            element: Node::new(canonical_type_instance(element)?),
            name: name.clone(),
        },
        CanonicalType::Module(module) => crate::TypeInstanceKey::Module(module.clone()),
        CanonicalType::GenericParameter(index) => crate::TypeInstanceKey::GenericParameter(*index),
    })
}

fn deduplicate_type_mappings(
    mappings: Vec<(Type, CanonicalType)>,
) -> Result<Vec<(Type, CanonicalType)>, CfgDomainFailure> {
    let mut positions: AHashMap<Type, usize> = AHashMap::with_capacity(mappings.len());
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

fn validate_type_mappings(mappings: &[(Type, CanonicalType)]) -> Result<(), CfgDomainFailure> {
    let mut stable_by_live = AHashMap::with_capacity(mappings.len());
    for (live, stable) in mappings {
        if let Some(previous) = stable_by_live.insert(*live, stable)
            && previous != stable
        {
            return Err(CfgDomainFailure::Shape);
        }
    }
    Ok(())
}

fn validate_symbol_mappings(mappings: &[(Spur, StableCfgSymbol)]) -> Result<(), CfgDomainFailure> {
    let mut stable_by_live = AHashMap::with_capacity(mappings.len());
    let mut live_by_stable = AHashMap::with_capacity(mappings.len());
    for (live, stable) in mappings {
        if let Some(previous) = stable_by_live.insert(*live, stable)
            && previous != stable
        {
            return Err(CfgDomainFailure::ConflictingLiveSymbol);
        }
        if let Some(previous) = live_by_stable.insert(stable, *live)
            && previous != *live
        {
            return Err(CfgDomainFailure::ConflictingStableSymbol);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum StableCfgSymbol {
    Callable(rue_air::FunctionInstanceKey<crate::StableDefinitionKey, crate::ModuleId>),
    #[allow(dead_code)]
    Specialization(
        rue_air::SemanticSpecializationIdentity<crate::StableDefinitionKey, crate::ModuleId>,
    ),
    Runtime(Arc<str>),
    Intrinsic(Arc<str>),
}

impl RetainedCharge for StableCfgSymbol {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Callable(value) => value.retained_charge(),
            Self::Specialization(value) => value.retained_charge(),
            Self::Runtime(value) | Self::Intrinsic(value) => value.retained_charge(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StableCfgSpan {
    Relative { start: i64, end: i64 },
    Absolute(Span),
}

/// The order `CfgDomainProjection::spans` is sorted and searched by.
///
/// `Span` is exactly these three fields, so this key is injective: two spans
/// share a key only when they are equal. Both the sort and the binary search
/// call this, because the search is only correct while they agree.
fn span_sort_key(span: &Span) -> (u32, u32, u32) {
    (span.file_id.index(), span.start, span.end)
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
    /// Sorted by `span_sort_key`, so `stable_span_anchor` can binary search.
    /// Deduplication is by the whole pair, so one span may keep more than one
    /// anchor; the lower bound is the first, which is the one a scan found.
    spans: Vec<(Span, StableCfgSpan)>,
    symbols: Vec<(Spur, StableCfgSymbol)>,
    incomplete_epoch: Option<Arc<()>>,
}

pub(crate) struct CfgDomainSplicePlan {
    symbols: Vec<(Spur, StableCfgSymbol)>,
    strings: Vec<(u32, Arc<str>)>,
}

struct CfgTypeDomainIndex<'a> {
    stable_by_live: AHashMap<Type, &'a CanonicalType>,
    live_by_stable: AHashMap<&'a CanonicalType, Type>,
}

impl<'a> CfgTypeDomainIndex<'a> {
    fn new(old: &'a [(Type, CanonicalType)], current: &'a [(Type, CanonicalType)]) -> Self {
        let mut stable_by_live = AHashMap::with_capacity(old.len());
        let mut live_by_stable = AHashMap::with_capacity(current.len());
        for (live, stable) in old {
            stable_by_live.entry(*live).or_insert(stable);
        }
        for (live, stable) in current {
            live_by_stable.entry(stable).or_insert(*live);
        }
        Self {
            stable_by_live,
            live_by_stable,
        }
    }

    fn stable(&self, value: Type) -> Result<CanonicalType, CfgDomainFailure> {
        canonical_primitive(value)
            .or_else(|| {
                self.stable_by_live
                    .get(&value)
                    .map(|stable| (*stable).clone())
            })
            .ok_or(CfgDomainFailure::MissingLiveType(value))
    }

    fn current(&self, stable: &CanonicalType) -> Result<Type, CfgDomainFailure> {
        live_primitive(stable)
            .or_else(|| self.live_by_stable.get(stable).copied())
            .ok_or_else(|| CfgDomainFailure::MissingStableType(stable.clone()))
    }

    fn remap(&self, value: Type) -> Result<Type, CfgDomainFailure> {
        self.current(&self.stable(value)?)
    }
}

impl RetainedCharge for CfgDomainProjection {
    fn retained_charge(&self) -> u64 {
        let types = (self.types.len() * std::mem::size_of::<(Type, CanonicalType)>()) as u64;
        let types = self.types.iter().fold(types, |charge, (_, ty)| {
            charge.saturating_add(ty.retained_charge())
        });
        let symbols = (self.symbols.len() * std::mem::size_of::<(Spur, StableCfgSymbol)>()) as u64;
        let symbols = self.symbols.iter().fold(symbols, |charge, (_, symbol)| {
            charge.saturating_add(symbol.retained_charge())
        });
        types
            .saturating_add(self.strings.retained_charge())
            .saturating_add(self.atoms.retained_charge())
            .saturating_add(
                (self.spans.len() * std::mem::size_of::<(Span, StableCfgSpan)>()) as u64,
            )
            .saturating_add(symbols)
            .saturating_add(self.incomplete_epoch.retained_charge())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CfgDomainFailure {
    Interner(lasso::LassoErrorKind),
    Shape,
    Unsupported,
    Missing,
    MissingLiveType(Type),
    MissingStableType(CanonicalType),
    MissingSymbol,
    MissingString,
    ConflictingLiveSymbol,
    ConflictingStableSymbol,
    Edit(rue_cfg::CfgEditError),
}

impl CfgDomainProjection {
    #[cfg(test)]
    pub(crate) fn inject_conflicting_live_symbol_for_test(&mut self) {
        let Some((live, _)) = self.symbols.first().cloned() else {
            return;
        };
        self.symbols.push((
            live,
            StableCfgSymbol::Intrinsic(Arc::from("__conflicting_live_symbol")),
        ));
    }

    #[cfg(test)]
    pub(crate) fn inject_conflicting_stable_symbol_for_test(&mut self) {
        let Some((live, stable)) = self.symbols.first().cloned() else {
            return;
        };
        let alternate = Spur::try_from_usize(live.into_usize().saturating_add(1))
            .expect("test symbol remains in the Spur domain");
        self.symbols.push((alternate, stable));
    }

    pub(crate) fn stable_debug_snapshot(&self, air: &rue_air::ValidatedAir) -> String {
        let instruction_kinds = air
            .iter()
            .map(|(_, instruction)| live_instruction_kind(&instruction.data))
            .collect::<Vec<_>>();
        let types = self
            .types
            .iter()
            .map(|(_, stable)| stable)
            .collect::<Vec<_>>();
        let strings = self
            .strings
            .iter()
            .map(|(_, stable)| stable)
            .collect::<Vec<_>>();
        let spans = self
            .spans
            .iter()
            .map(|(_, stable)| stable)
            .collect::<Vec<_>>();
        // `symbols` is stored ordered by live interner handle, which is a
        // private lookup order rather than a semantic one. Render it in stable
        // symbol order so this snapshot describes the body's durable shape and
        // not the order its interner happened to issue handles in (ADR-0076).
        let mut symbols = self
            .symbols
            .iter()
            .map(|(_, stable)| stable)
            .collect::<Vec<_>>();
        symbols.sort();
        format!(
            "{instruction_kinds:?}|{types:?}|{strings:?}|{:?}|{spans:?}|{symbols:?}",
            self.atoms
        )
    }

    /// Admit stable types already owned by a surrounding semantic output.
    #[cfg(test)]
    pub(crate) fn admit_stable_types(
        &mut self,
        old: &Self,
        admission: &mut CfgTypeAdmissionIndex<'_>,
    ) -> Result<(), CfgDomainFailure> {
        let mut stable_types = self
            .types
            .iter()
            .map(|(_, stable)| stable.clone())
            .collect::<ahash::AHashSet<_>>();
        for (_, stable) in &old.types {
            if live_primitive(stable).is_some() || !stable_types.insert(stable.clone()) {
                continue;
            }
            let current = admission
                .current(stable)
                .ok_or_else(|| CfgDomainFailure::MissingStableType(stable.clone()))?;
            self.types.push((current, stable.clone()));
        }
        self.types = deduplicate_type_mappings(std::mem::take(&mut self.types))?;
        Ok(())
    }

    /// Extend this function-local live domain with the stable values used by
    /// an accessor CFG, then remap that CFG into the extended domain. Accessor
    /// splicing preserves the callee's source spans and stable local atoms;
    /// only dense string ids and live type/symbol ids are relocated.
    pub(crate) fn check_importable(&self, old: &Self) -> Result<(), CfgDomainFailure> {
        if old.incomplete_epoch.is_some() || self.incomplete_epoch.is_some() {
            return Err(CfgDomainFailure::Missing);
        }
        // Mirror the type-domain portion of import_accessor_cfg without
        // mutating or allocating for the caller projection. This is
        // intentionally based on durable stable-type mappings, not live
        // handles, so a repeated unimportable site can be refused before
        // cloning staging resources.
        for (_, stable) in &old.types {
            if live_primitive(stable).is_none()
                && !self.types.iter().any(|(_, current)| current == stable)
            {
                return Err(CfgDomainFailure::MissingStableType(stable.clone()));
            }
        }
        Ok(())
    }

    pub(crate) fn import_accessor_cfg(
        &self,
        old: &Self,
        cfg: &rue_cfg::Cfg,
        old_interner: &lasso::ThreadedRodeo,
        mut symbol_for: impl FnMut(&str) -> Result<Spur, CfgDomainFailure>,
        mut string_for: impl FnMut(&str) -> Result<u32, CfgDomainFailure>,
        new_body_span: Span,
    ) -> Result<
        (
            rue_cfg::CfgEditor,
            std::collections::BTreeMap<u32, u32>,
            CfgDomainSplicePlan,
        ),
        CfgDomainFailure,
    > {
        if old.incomplete_epoch.is_some() || self.incomplete_epoch.is_some() {
            return Err(CfgDomainFailure::Missing);
        }
        validate_symbol_mappings(&self.symbols)?;
        validate_symbol_mappings(&old.symbols)?;
        validate_type_mappings(&self.types)?;
        let type_index = CfgTypeDomainIndex::new(&old.types, &self.types);
        for (_, stable) in &old.types {
            type_index.current(stable)?;
        }
        let mut current_symbols = AHashMap::with_capacity(self.symbols.len() + old.symbols.len());
        let mut current_stable_by_live =
            AHashMap::with_capacity(self.symbols.len() + old.symbols.len());
        for (live, stable) in &self.symbols {
            current_symbols.insert(stable, *live);
            current_stable_by_live.insert(*live, stable);
        }
        let mut planned_symbols = Vec::new();
        let mut planned_by_stable = AHashMap::new();
        let mut planned_stable_by_live = AHashMap::new();
        for (live, stable) in &old.symbols {
            if current_symbols.contains_key(stable) || planned_by_stable.contains_key(stable) {
                continue;
            }
            let symbol = symbol_for(old_interner.resolve(live))?;
            if current_stable_by_live
                .get(&symbol)
                .is_some_and(|previous| **previous != *stable)
                || planned_stable_by_live
                    .get(&symbol)
                    .is_some_and(|previous| previous != stable)
            {
                return Err(CfgDomainFailure::ConflictingLiveSymbol);
            }
            planned_by_stable.insert(stable.clone(), symbol);
            planned_stable_by_live.insert(symbol, stable.clone());
            planned_symbols.push((symbol, stable.clone()));
        }
        let mut old_symbols = AHashMap::with_capacity(old.symbols.len());
        for (live, stable) in &old.symbols {
            old_symbols.insert(*live, stable);
        }
        let mut planned_domain_strings = Vec::new();
        let mut string_map = std::collections::BTreeMap::new();
        for (old_index, stable) in &old.strings {
            let new_index = string_for(stable)?;
            string_map.insert(*old_index, new_index);
            if !self
                .strings
                .iter()
                .any(|current| current.0 == new_index && current.1 == *stable)
                && !planned_domain_strings
                    .iter()
                    .any(|current: &(u32, Arc<str>)| current.0 == new_index && current.1 == *stable)
            {
                planned_domain_strings.push((new_index, stable.clone()));
            }
        }

        let imported = cfg
            .try_remap_domains(
                |value| type_index.remap(value),
                |value| match type_index.remap(Type::new_struct(value))?.kind() {
                    TypeKind::Struct(id) => Ok(id),
                    _ => Err(CfgDomainFailure::Shape),
                },
                |value| match type_index.remap(Type::new_enum(value))?.kind() {
                    TypeKind::Enum(id) => Ok(id),
                    _ => Err(CfgDomainFailure::Shape),
                },
                |value: Spur| {
                    let stable = old_symbols
                        .get(&value)
                        .copied()
                        .ok_or(CfgDomainFailure::MissingSymbol)?;
                    current_symbols
                        .get(stable)
                        .copied()
                        .or_else(|| planned_by_stable.get(stable).copied())
                        .ok_or(CfgDomainFailure::MissingSymbol)
                },
                |value| {
                    string_map
                        .get(&value)
                        .copied()
                        .ok_or(CfgDomainFailure::MissingString)
                },
                |value| old.stable_span_anchor(value).relocate(new_body_span),
            )
            .map_err(|error| match error {
                rue_cfg::CfgRemapError::Domain(error) => error,
                rue_cfg::CfgRemapError::Edit(error) => CfgDomainFailure::Edit(error),
            })?;
        Ok((
            imported,
            string_map,
            CfgDomainSplicePlan {
                symbols: planned_symbols,
                strings: planned_domain_strings,
            },
        ))
    }

    pub(crate) fn apply_splice_plan(&mut self, plan: CfgDomainSplicePlan) {
        self.symbols.extend(plan.symbols);
        self.symbols.sort_by(|left, right| {
            (left.0.into_usize(), &left.1).cmp(&(right.0.into_usize(), &right.1))
        });
        self.symbols.dedup();
        self.strings.extend(plan.strings);
    }

    /// The stable anchor this domain recorded for `value`, falling back to an
    /// anchor derived against this domain's own body span when the span is not
    /// part of the domain.
    ///
    /// `spans` is sorted by `span_sort_key` and that key is injective, so the
    /// lower bound is the first entry carrying this span. That matters where
    /// `dedup` left two anchors for one span: the first is what the scan this
    /// replaced returned, and stable sorting keeps it first.
    fn stable_span_anchor(&self, value: Span) -> StableCfgSpan {
        let key = span_sort_key(&value);
        let position = self
            .spans
            .partition_point(|(span, _)| span_sort_key(span) < key);
        match self.spans.get(position) {
            Some((span, anchor)) if *span == value => *anchor,
            _ => StableCfgSpan::new(value, self.body_span),
        }
    }

    /// The live-handle order this searches is the same one insertion sorts by,
    /// so the answer is a function of the handle alone and never of the order
    /// the interner issued handles in — the property ADR-0076 needs from every
    /// ordered symbol-handle use that survives.
    pub(crate) fn callable_for_symbol(&self, name: Spur) -> Option<crate::FunctionInstanceKey> {
        let position = self
            .symbols
            .partition_point(|(live, _)| live.into_usize() < name.into_usize());
        let (live, stable) = self.symbols.get(position)?;
        if *live != name {
            return None;
        }
        match stable {
            StableCfgSymbol::Callable(callable) => Some(callable.clone()),
            StableCfgSymbol::Specialization(identity) => {
                crate::semantic_identity::function_instance_from_specialization(identity)
            }
            StableCfgSymbol::Runtime(_) | StableCfgSymbol::Intrinsic(_) => None,
        }
    }

    /// Whether this live symbol is explicitly a runtime or intrinsic symbol.
    /// This is separate from [`Self::callable_for_symbol`], whose `None` also
    /// covers malformed or absent canonical callable identities.
    pub(crate) fn is_known_non_callable_symbol(&self, name: Spur) -> bool {
        let position = self
            .symbols
            .partition_point(|(live, _)| live.into_usize() < name.into_usize());
        self.symbols.get(position).is_some_and(|(live, stable)| {
            *live == name
                && matches!(
                    stable,
                    StableCfgSymbol::Runtime(_) | StableCfgSymbol::Intrinsic(_)
                )
        })
    }

    pub(crate) fn stable_types(&self) -> impl Iterator<Item = &CanonicalType> {
        self.types.iter().map(|(_, stable)| stable)
    }

    pub(crate) fn stable_type_count(&self) -> usize {
        self.types.len()
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
        let mut symbols_by_live = None;
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
                let symbols_by_live = symbols_by_live.get_or_insert_with(|| {
                    let mut index = AHashMap::with_capacity(self.symbols.len());
                    for (live, stable) in &self.symbols {
                        index.entry(*live).or_insert(stable);
                    }
                    index
                });
                let stable = symbols_by_live
                    .get(name)
                    .copied()
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
        call_abis: &ahash::AHashMap<crate::FunctionInstanceKey, crate::type_queries::CallAbiFacts>,
        drop_glue_symbols: &ahash::AHashMap<crate::TypeInstanceKey, Arc<str>>,
        destructor_symbols: &ahash::AHashMap<crate::TypeInstanceKey, Arc<str>>,
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
                    // One probe answers both questions. The convention and the
                    // native symbol come from the same facts, and this map is
                    // keyed by a recursive callable identity.
                    let abi = call_abis.get(callable);
                    let foreign = abi.is_some_and(|facts| facts.convention.is_c());
                    let machine = if foreign {
                        foreign_callable_symbol(callable).ok_or(CfgDomainFailure::Shape)?
                    } else if source == "main" {
                        source.clone()
                    } else {
                        abi.and_then(|facts| facts.native_symbol.as_ref())
                            .map(ToString::to_string)
                            .unwrap_or_else(|| {
                                crate::StableSymbolEncoder::encode(
                                    &crate::StableSymbolId::Callable(
                                        crate::StableCallableId::Function(callable.clone()),
                                    ),
                                )
                            })
                    };
                    (machine, foreign)
                }
                StableCfgSymbol::Specialization(identity) => {
                    let callable =
                        crate::semantic_identity::function_instance_from_specialization(identity)
                            .ok_or(CfgDomainFailure::Shape)?;
                    let abi = call_abis.get(&callable);
                    let foreign = abi.is_some_and(|facts| facts.convention.is_c());
                    let machine = if foreign {
                        foreign_callable_symbol(&callable).ok_or(CfgDomainFailure::Shape)?
                    } else if source == "main" {
                        source.clone()
                    } else {
                        abi.and_then(|facts| facts.native_symbol.as_ref())
                            .map(ToString::to_string)
                            .unwrap_or_else(|| {
                                crate::StableSymbolEncoder::encode(
                                    &crate::StableSymbolId::Callable(
                                        crate::StableCallableId::Function(callable),
                                    ),
                                )
                            })
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
                            owner: Node::new(crate::TypeInstanceKey::Nominal(
                                crate::NominalInstanceKey::Anonymous(Node::new(identity.clone())),
                            )),
                            member: crate::AnonymousMemberKey {
                                kind: crate::AnonymousMemberKind::Destructor,
                                name: Arc::from("__drop"),
                            },
                        };
                        let machine = destructor_symbols
                            .get(&owner)
                            .map(ToString::to_string)
                            .unwrap_or_else(|| {
                                crate::StableSymbolEncoder::encode(
                                    &crate::StableSymbolId::Callable(
                                        crate::StableCallableId::Function(callable),
                                    ),
                                )
                            });
                        if let Some(previous) =
                            symbol_mappings.insert(source.to_string(), machine.clone())
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
            let machine = drop_glue_symbols
                .get(&owner)
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
                        crate::StableCallableId::Function(crate::FunctionInstanceKey::DropGlue(
                            Node::new(owner.clone()),
                        )),
                    ))
                });
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

    pub fn from_local_body(
        materialization: &rue_air::SemanticLocalMaterialization<
            crate::StableDefinitionKey,
            crate::ModuleId,
        >,
        stable_callable: impl Fn(lasso::Spur) -> Option<crate::FunctionInstanceKey>,
    ) -> Result<Self, CfgDomainFailure> {
        let type_pool = &materialization.type_pool;
        let interner = &materialization.interner;
        let air = &materialization.air;
        let mut stable_by_live = AHashMap::with_capacity(materialization.materialized_types.len());
        let mut types = Vec::with_capacity(materialization.materialized_types.len() + 2);
        types.push((Type::I32, CanonicalType::I32));
        types.push((Type::UNIT, CanonicalType::Unit));
        for (current, stable) in &materialization.materialized_types {
            if matches!(current.kind(), TypeKind::Module(_) | TypeKind::Error) {
                continue;
            }
            if let Some(previous) = stable_by_live.insert(*current, stable.clone())
                && previous != *stable
            {
                return Err(CfgDomainFailure::Shape);
            }
            types.push((*current, stable.clone()));
        }
        record_cfg_type(
            &mut types,
            air.return_type(),
            type_pool,
            materialization,
            &mut stable_by_live,
        )?;
        let mut stable_strings = Vec::new();
        let mut spans = Vec::with_capacity(air.len());
        let mut symbols = Vec::new();
        for (_, current) in air.iter() {
            record_cfg_type(
                &mut types,
                current.ty,
                type_pool,
                materialization,
                &mut stable_by_live,
            )?;
            spans.push((
                current.span,
                StableCfgSpan::new(current.span, materialization.body_span),
            ));
            match &current.data {
                AirInstData::StringConst(index) => {
                    let value = materialization
                        .strings
                        .get(*index as usize)
                        .ok_or(CfgDomainFailure::Shape)?;
                    stable_strings.push((*index, Arc::from(value.as_str())));
                }
                AirInstData::TypeConst(ty) => record_cfg_type(
                    &mut types,
                    *ty,
                    type_pool,
                    materialization,
                    &mut stable_by_live,
                )?,
                AirInstData::IntCast { from_ty, .. } => {
                    record_cfg_type(
                        &mut types,
                        *from_ty,
                        type_pool,
                        materialization,
                        &mut stable_by_live,
                    )?;
                }
                AirInstData::Call {
                    runtime: None,
                    name,
                    ..
                }
                | AirInstData::AccessorCall { name, .. } => {
                    let callable = stable_callable(*name).ok_or(CfgDomainFailure::MissingSymbol)?;
                    symbols.push((*name, StableCfgSymbol::Callable(callable)));
                }
                AirInstData::Call {
                    runtime: Some(runtime),
                    name,
                    ..
                } => {
                    let stable = runtime.helper().helper().symbol;
                    if interner.resolve(name) != stable {
                        return Err(CfgDomainFailure::Shape);
                    }
                    symbols.push((*name, StableCfgSymbol::Runtime(Arc::from(stable))));
                }
                AirInstData::Intrinsic { name, .. } => {
                    symbols.push((
                        *name,
                        StableCfgSymbol::Intrinsic(Arc::from(interner.resolve(name))),
                    ));
                }
                AirInstData::StructInit { struct_id, .. } => {
                    let ty = Type::new_struct(*struct_id);
                    record_cfg_type(
                        &mut types,
                        ty,
                        type_pool,
                        materialization,
                        &mut stable_by_live,
                    )?;
                }
                AirInstData::EnumVariant { enum_id, .. }
                | AirInstData::EnumPayloadGet { enum_id, .. } => {
                    let ty = Type::new_enum(*enum_id);
                    record_cfg_type(
                        &mut types,
                        ty,
                        type_pool,
                        materialization,
                        &mut stable_by_live,
                    )?;
                }
                AirInstData::Match { arms, .. } => {
                    for (pattern, _) in air.get_match_arms(arms) {
                        if let rue_air::AirPattern::EnumVariant { enum_id, .. } = pattern {
                            let ty = Type::new_enum(enum_id);
                            record_cfg_type(
                                &mut types,
                                ty,
                                type_pool,
                                materialization,
                                &mut stable_by_live,
                            )?;
                        }
                    }
                }
                _ => {}
            }
        }
        for place in air.places() {
            record_cfg_type(
                &mut types,
                place.base_type,
                type_pool,
                materialization,
                &mut stable_by_live,
            )?;
            for projection in air.get_place_projections(place) {
                let ty = match projection {
                    rue_air::AirProjection::Field { struct_id, .. } => Type::new_struct(*struct_id),
                    rue_air::AirProjection::Index { array_type, .. } => *array_type,
                };
                record_cfg_type(
                    &mut types,
                    ty,
                    type_pool,
                    materialization,
                    &mut stable_by_live,
                )?;
            }
        }
        for (_, ty) in air.param_drops() {
            record_cfg_type(
                &mut types,
                *ty,
                type_pool,
                materialization,
                &mut stable_by_live,
            )?;
        }
        let mut atoms = materialization
            .local_atoms
            .iter()
            .map(|atom| rue_air::SemanticBodyLocalAtom {
                identity: atom.identity.clone(),
                content: atom.content.clone(),
            })
            .collect::<Vec<_>>();

        // CFG cleanup elaboration recursively reads aggregate fields, variant
        // payloads, arrays, and destructor symbols. Close the live type domain
        // over those facts so every emitted CFG handle can be relocated.
        let mut types = deduplicate_type_mappings(types)?;
        let mut type_positions = types
            .iter()
            .enumerate()
            .map(|(position, (current, _))| (*current, position))
            .collect::<AHashMap<_, _>>();
        let mut pending = types
            .iter()
            .map(|(current, _)| *current)
            .collect::<Vec<_>>();
        let mut enqueued = pending.iter().copied().collect::<ahash::AHashSet<_>>();
        let mut visited = AHashSet::new();
        let mut incomplete = false;
        while let Some(current) = pending.pop() {
            if !visited.insert(current) {
                continue;
            }
            let children = match current.kind() {
                TypeKind::Struct(id) => {
                    let definition = type_pool.struct_def(id);
                    if let Some(name) = &definition.destructor {
                        let symbol = interner
                            .try_get_or_intern(name)
                            .map_err(|error| CfgDomainFailure::Interner(error.kind()))?;
                        if let Some(callable) = stable_callable(symbol) {
                            symbols.push((symbol, StableCfgSymbol::Callable(callable)));
                        } else if let Some(CanonicalType::AnonymousNominal(owner)) = type_positions
                            .get(&current)
                            .map(|position| &types[*position].1)
                        {
                            symbols.push((
                                symbol,
                                StableCfgSymbol::Callable(
                                    crate::FunctionInstanceKey::AnonymousMember {
                                        owner: Node::new(crate::TypeInstanceKey::Nominal(
                                            crate::NominalInstanceKey::Anonymous(Node::new(
                                                owner.clone(),
                                            )),
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
                if enqueued.contains(&child) {
                    continue;
                }
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    type_positions.entry(child)
                {
                    match canonical_type_from_live_cached(
                        child,
                        type_pool,
                        materialization,
                        &mut stable_by_live,
                    ) {
                        Ok(stable) => {
                            entry.insert(types.len());
                            types.push((child, stable));
                        }
                        Err(CfgDomainFailure::Missing | CfgDomainFailure::Unsupported) => {
                            incomplete = true;
                            continue;
                        }
                        Err(failure) => return Err(failure),
                    }
                }
                enqueued.insert(child);
                pending.push(child);
            }
        }
        stable_strings.sort_by_key(|(index, _)| *index);
        stable_strings.dedup();
        spans.sort_by_key(|(span, _)| span_sort_key(span));
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
            body_span: materialization.body_span,
            types,
            strings: stable_strings,
            atoms,
            spans,
            symbols,
            incomplete_epoch: incomplete.then(|| Arc::new(())),
        })
    }

    #[cfg(test)]
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
        let type_index = CfgTypeDomainIndex::new(&old.types, &new.types);
        let mut old_symbols = AHashMap::with_capacity(old.symbols.len());
        let mut new_symbols = AHashMap::with_capacity(new.symbols.len());
        for (live, stable) in &old.symbols {
            old_symbols.entry(*live).or_insert(stable);
        }
        for (live, stable) in &new.symbols {
            new_symbols.entry(stable).or_insert(*live);
        }
        let mut old_strings = AHashMap::with_capacity(old.strings.len());
        let mut new_strings = AHashMap::with_capacity(new.strings.len());
        for (index, stable) in &old.strings {
            old_strings.entry(*index).or_insert(stable.as_ref());
        }
        for (index, stable) in &new.strings {
            new_strings.entry(stable.as_ref()).or_insert(*index);
        }
        let mut old_spans = AHashMap::with_capacity(old.spans.len());
        for (span, stable) in &old.spans {
            old_spans.entry(*span).or_insert(*stable);
        }
        cfg.try_remap_domains(
            |value| type_index.remap(value),
            |value| match type_index.remap(Type::new_struct(value))?.kind() {
                TypeKind::Struct(id) => Ok(id),
                _ => Err(CfgDomainFailure::Shape),
            },
            |value| match type_index.remap(Type::new_enum(value))?.kind() {
                TypeKind::Enum(id) => Ok(id),
                _ => Err(CfgDomainFailure::Shape),
            },
            |value: Spur| {
                let stable = old_symbols
                    .get(&value)
                    .copied()
                    .ok_or(CfgDomainFailure::MissingSymbol)?;
                new_symbols
                    .get(stable)
                    .copied()
                    .ok_or(CfgDomainFailure::MissingSymbol)
            },
            |value| {
                let stable = old_strings
                    .get(&value)
                    .copied()
                    .ok_or(CfgDomainFailure::MissingString)?;
                new_strings
                    .get(stable)
                    .copied()
                    .ok_or(CfgDomainFailure::MissingString)
            },
            |value| {
                let anchor = old_spans
                    .get(&value)
                    .copied()
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
    use rue_span::FileId;

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

    struct EmptyAggregateTypes;

    impl AggregateTypeLookup for EmptyAggregateTypes {
        fn aggregate_type(&self, _ty: Type) -> Option<&crate::TypeInstanceKey> {
            None
        }
    }

    #[test]
    fn type_admission_index_stays_lazy_when_domains_already_cover_types() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("f");
        let old = projection(symbol);
        let mut current = projection(symbol);
        let pool = rue_air::TypeInternPool::new().freeze();
        let aggregates = EmptyAggregateTypes;
        let mut admission = CfgTypeAdmissionIndex::new(&pool, &aggregates);

        current.admit_stable_types(&old, &mut admission).unwrap();

        assert!(admission.live_by_stable.is_none());
    }

    #[test]
    fn accessor_string_relocation_indexes_first_occurrences_and_new_content_once() {
        let old_interner = lasso::ThreadedRodeo::new();
        let new_interner = lasso::ThreadedRodeo::new();
        let old_symbol = old_interner.get_or_intern("callee");
        let new_symbol = new_interner.get_or_intern("callee");
        let stable_symbol = StableCfgSymbol::Intrinsic(Arc::from("callee"));
        let mut old = projection_with(old_symbol, stable_symbol.clone());
        old.strings = vec![
            (10, Arc::from("same")),
            (11, Arc::from("same")),
            (12, Arc::from("added")),
        ];
        let mut current = projection_with(new_symbol, stable_symbol);
        current.strings = vec![(0, Arc::from("same"))];
        let mut strings = vec!["same".to_string(), "other".to_string(), "same".to_string()];
        let mut cfg = Cfg::new(Type::I32, 0, 0, "f".into(), Vec::<bool>::new());
        let block = cfg.new_block();
        cfg.append_call(block, None, old_symbol, [], Type::I32, Span::new(4, 5))
            .unwrap();

        let (_, string_map, plan) = current
            .import_accessor_cfg(
                &old,
                &cfg,
                &old_interner,
                |spelling| {
                    new_interner
                        .try_get_or_intern(spelling)
                        .map_err(|error| CfgDomainFailure::Interner(error.kind()))
                },
                |spelling| {
                    if let Some(index) = strings.iter().position(|value| value == spelling) {
                        return u32::try_from(index).map_err(|_| CfgDomainFailure::Shape);
                    }
                    let index =
                        u32::try_from(strings.len()).map_err(|_| CfgDomainFailure::Shape)?;
                    strings.push(spelling.to_owned());
                    Ok(index)
                },
                Span::new(40, 50),
            )
            .unwrap();
        current.apply_splice_plan(plan);

        assert_eq!(string_map, [(10, 0), (11, 0), (12, 3)].into());
        assert_eq!(strings, ["same", "other", "same", "added"]);
        assert_eq!(
            current
                .strings
                .iter()
                .filter(|(index, stable)| *index == 0 && stable.as_ref() == "same")
                .count(),
            1
        );
        let source = include_str!("durable_cfg.rs");
        let relocation = source
            .split_once("pub(crate) fn import_accessor_cfg(")
            .unwrap()
            .1
            .split_once("pub(crate) fn callable_for_symbol")
            .unwrap()
            .0;
        assert!(!relocation.contains(".position("));
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
    fn symbol_mapping_validation_rejects_both_non_injective_directions() {
        let first = Spur::try_from_usize(1).unwrap();
        let second = Spur::try_from_usize(2).unwrap();
        let stable = StableCfgSymbol::Intrinsic(Arc::from("stable"));
        let other = StableCfgSymbol::Intrinsic(Arc::from("other"));

        assert_eq!(
            validate_symbol_mappings(&[(first, stable.clone()), (first, other)]),
            Err(CfgDomainFailure::ConflictingLiveSymbol)
        );
        assert_eq!(
            validate_symbol_mappings(&[(first, stable.clone()), (second, stable)]),
            Err(CfgDomainFailure::ConflictingStableSymbol)
        );
    }

    #[test]
    fn symbol_import_rejects_spelling_aliases_with_distinct_stable_identities() {
        let old_interner = lasso::ThreadedRodeo::new();
        let current_interner = lasso::ThreadedRodeo::new();
        let old_live = old_interner.get_or_intern("same-spelling");
        let current_live = current_interner.get_or_intern("same-spelling");
        let old = projection_with(
            old_live,
            StableCfgSymbol::Intrinsic(Arc::from("old-identity")),
        );
        let current = projection_with(
            current_live,
            StableCfgSymbol::Intrinsic(Arc::from("current-identity")),
        );
        let before = current.symbols.clone();
        let cfg = Cfg::new(Type::I32, 0, 0, "f".into(), Vec::<bool>::new());
        assert!(matches!(
            current.import_accessor_cfg(
                &old,
                &cfg,
                &old_interner,
                |spelling| {
                    current_interner
                        .try_get_or_intern(spelling)
                        .map_err(|error| CfgDomainFailure::Interner(error.kind()))
                },
                |_| Err(CfgDomainFailure::MissingString),
                Span::new(40, 50),
            ),
            Err(CfgDomainFailure::ConflictingLiveSymbol)
        ));
        assert_eq!(current.symbols, before);
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

        let source = include_str!("durable_cfg.rs");
        let relocation = source
            .split_once("pub(crate) fn import_accessor_cfg(")
            .unwrap()
            .1
            .split_once("pub(crate) fn callable_for_symbol")
            .unwrap()
            .0;
        {
            let compact = relocation
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();
            assert!(!compact.contains(".symbols.iter().any("));
            assert!(!compact.contains(".symbols.iter().find("));
        }

        let general_import = source
            .split_once("pub fn import_cfg(")
            .unwrap()
            .1
            .split_once("#[cfg(test)]")
            .unwrap()
            .0;
        let compact = general_import
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        for domain in ["types", "symbols", "strings", "spans"] {
            assert!(!compact.contains(&format!(".{domain}.iter().find(")));
        }
        let compact_accessor = relocation
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(!compact_accessor.contains("self.current_type("));
        assert!(!compact_accessor.contains("old.stable_type("));
        let type_admission = source
            .split_once("pub(crate) fn admit_stable_types(")
            .unwrap()
            .1
            .split_once("pub(crate) fn import_accessor_cfg(")
            .unwrap()
            .0
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(!type_admission.contains("self.current_type("));
        assert!(!type_admission.contains(".types.iter().find("));
        assert!(!type_admission.contains(".all_types()"));
        assert!(!type_admission.contains("canonical_type_from_live("));

        let queries = include_str!("queries.rs");
        assert!(!queries.contains("CfgTypeAdmissionIndex::new("));
        assert!(!queries.contains(".admit_stable_types("));

        let type_closure = source
            .split_once("// CFG cleanup elaboration recursively reads aggregate fields")
            .unwrap()
            .1
            .split_once("stable_strings.sort_by_key")
            .unwrap()
            .0
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(type_closure.contains("type_positions"));
        assert!(!type_closure.contains("types.iter().any("));
        assert!(!type_closure.contains("types.iter().find"));

        let runtime_callables = source
            .split_once("pub(crate) fn runtime_callables(")
            .unwrap()
            .1
            .split_once("/// Project the exact machine-symbol domain")
            .unwrap()
            .0
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(runtime_callables.contains("symbols_by_live"));
        assert!(!runtime_callables.contains("self.symbols.iter().find"));
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

    #[test]
    fn accessor_import_reanchors_reused_callee_spans() {
        let old_interner = lasso::ThreadedRodeo::new();
        let new_interner = lasso::ThreadedRodeo::new();
        let old_symbol = old_interner.get_or_intern("callee");
        let new_symbol = new_interner.get_or_intern("callee");
        let stable = StableCfgSymbol::Intrinsic(Arc::from("callee"));
        let mut old = projection_with(old_symbol, stable.clone());
        old.body_span = Span::new(10, 20);
        old.spans = vec![(
            Span::new(12, 13),
            StableCfgSpan::Relative { start: 2, end: 3 },
        )];
        let mut current = projection_with(new_symbol, stable);
        current.body_span = Span::new(30, 40);
        current.spans.clear();
        let mut cfg = Cfg::new(Type::I32, 0, 0, "f".into(), Vec::<bool>::new());
        let block = cfg.new_block();
        cfg.append_call(block, None, old_symbol, [], Type::I32, Span::new(12, 13))
            .unwrap();

        let (imported, _, plan) = current
            .import_accessor_cfg(
                &old,
                &cfg,
                &old_interner,
                |spelling| {
                    new_interner
                        .try_get_or_intern(spelling)
                        .map_err(|error| CfgDomainFailure::Interner(error.kind()))
                },
                |_| Err(CfgDomainFailure::MissingString),
                Span::new(50, 60),
            )
            .unwrap();
        current.apply_splice_plan(plan);

        assert_eq!(
            imported.get_inst(rue_cfg::CfgValue::from_raw(0)).span,
            Span::new(52, 53)
        );
    }

    #[test]
    fn stable_span_anchor_matches_a_linear_scan_over_a_wide_domain() {
        let symbol = lasso::ThreadedRodeo::new().get_or_intern("callee");
        let mut projection = projection_with(symbol, StableCfgSymbol::Intrinsic(Arc::from("s")));
        projection.body_span = Span::new(0, 4096);

        // Two files so the search exercises the file component of the key, and
        // an interleaved build order so sorting is doing real work.
        let mut spans = Vec::new();
        for index in 0..512u32 {
            let start = index * 4;
            spans.push((
                Span::with_file(FileId::DEFAULT, start, start + 2),
                StableCfgSpan::Relative {
                    start: i64::from(start),
                    end: i64::from(start) + 2,
                },
            ));
            spans.push((
                Span::with_file(FileId::new(1), start, start + 2),
                StableCfgSpan::Absolute(Span::with_file(FileId::new(1), start, start + 2)),
            ));
        }
        let scanned = spans.clone();
        spans.sort_by_key(|(span, _)| span_sort_key(span));
        spans.dedup();
        projection.spans = spans;

        // Every recorded span resolves to what a scan of the pre-sort order
        // would have returned, and the fallback still covers absent spans.
        for (span, _) in &scanned {
            let expected = scanned
                .iter()
                .find(|(candidate, _)| candidate == span)
                .map(|(_, anchor)| *anchor)
                .unwrap();
            assert_eq!(projection.stable_span_anchor(*span), expected);
        }
        let absent = Span::new(3, 5);
        assert_eq!(
            projection.stable_span_anchor(absent),
            StableCfgSpan::new(absent, projection.body_span)
        );
    }

    #[test]
    fn stable_span_anchor_keeps_the_first_of_two_anchors_for_one_span() {
        let symbol = lasso::ThreadedRodeo::new().get_or_intern("callee");
        let mut projection = projection_with(symbol, StableCfgSymbol::Intrinsic(Arc::from("s")));
        projection.body_span = Span::new(0, 64);
        // `dedup` only drops equal pairs, so one span can keep two anchors.
        // A scan answered with the first; the lower bound has to agree.
        let repeated = Span::new(8, 12);
        projection.spans = vec![
            (
                Span::new(4, 6),
                StableCfgSpan::Relative { start: 4, end: 6 },
            ),
            (repeated, StableCfgSpan::Relative { start: 8, end: 12 }),
            (repeated, StableCfgSpan::Absolute(Span::new(99, 100))),
            (
                Span::new(16, 20),
                StableCfgSpan::Relative { start: 16, end: 20 },
            ),
        ];

        assert_eq!(
            projection.stable_span_anchor(repeated),
            StableCfgSpan::Relative { start: 8, end: 12 }
        );
    }
}
