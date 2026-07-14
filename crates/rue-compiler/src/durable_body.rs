//! Versioned, request-independent ordinary-body candidates.
//!
//! This is deliberately a compiler-owned algebra rather than an alias for
//! `rue_air::SemanticBody`.  AIR's structured body is an exchange DTO; values
//! in this module are the only representation eligible for retention between
//! frontend revisions.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use rue_air::{
    AirArgMode, AirPlaceBase, SemanticBodyDefinitionIdentity, SemanticBodyDefinitionKind,
    SemanticBodyInstData, SemanticBodyPattern, SemanticBodyProjection, SemanticImportType,
};

use crate::{
    BoundDefinitionSet, CanonicalMergedProgram, DurableType, StableBodyDependencyInputRecord,
    StableDefinitionKey, StableDefinitionKind,
};

pub const DURABLE_ORDINARY_BODY_SCHEMA_VERSION: u32 = 2;

pub type DurableAirRef = u32;
pub type DurablePlaceRef = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableBodyAnchor {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurablePattern {
    Wildcard,
    Int(i64),
    Bool(bool),
    EnumVariant {
        enum_key: StableDefinitionKey,
        variant_index: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableMatchArm {
    pub pattern: DurablePattern,
    pub body: DurableAirRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableCallArg {
    pub value: DurableAirRef,
    pub mode: AirArgMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableProjection {
    Field {
        struct_key: StableDefinitionKey,
        field_index: u32,
    },
    Index {
        array_type: DurableType,
        index: DurableAirRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurablePlace {
    pub base: AirPlaceBase,
    pub base_type: DurableType,
    pub projections: Arc<[DurableProjection]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableAirInst {
    pub data: DurableAirInstData,
    pub ty: DurableType,
    pub anchor: DurableBodyAnchor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableAirInstData {
    Const(u64),
    BoolConst(bool),
    StringConst(u32),
    UnitConst,
    TypeConst(DurableType),
    Add(DurableAirRef, DurableAirRef),
    Sub(DurableAirRef, DurableAirRef),
    Mul(DurableAirRef, DurableAirRef),
    Div(DurableAirRef, DurableAirRef),
    Mod(DurableAirRef, DurableAirRef),
    Eq(DurableAirRef, DurableAirRef),
    Ne(DurableAirRef, DurableAirRef),
    Lt(DurableAirRef, DurableAirRef),
    Gt(DurableAirRef, DurableAirRef),
    Le(DurableAirRef, DurableAirRef),
    Ge(DurableAirRef, DurableAirRef),
    And(DurableAirRef, DurableAirRef),
    Or(DurableAirRef, DurableAirRef),
    BitAnd(DurableAirRef, DurableAirRef),
    BitOr(DurableAirRef, DurableAirRef),
    BitXor(DurableAirRef, DurableAirRef),
    Shl(DurableAirRef, DurableAirRef),
    Shr(DurableAirRef, DurableAirRef),
    Neg(DurableAirRef),
    Not(DurableAirRef),
    BitNot(DurableAirRef),
    Branch {
        cond: DurableAirRef,
        then_value: DurableAirRef,
        else_value: Option<DurableAirRef>,
    },
    Loop {
        cond: DurableAirRef,
        body: DurableAirRef,
    },
    InfiniteLoop {
        body: DurableAirRef,
    },
    Match {
        scrutinee: DurableAirRef,
        arms: Arc<[DurableMatchArm]>,
    },
    Break,
    Continue,
    Alloc {
        slot: u32,
        init: DurableAirRef,
    },
    Load {
        slot: u32,
    },
    Store {
        slot: u32,
        value: DurableAirRef,
    },
    ParamStore {
        param_slot: u32,
        value: DurableAirRef,
    },
    Ret(Option<DurableAirRef>),
    Call {
        function: StableDefinitionKey,
        args: Arc<[DurableCallArg]>,
    },
    Intrinsic {
        name: Arc<str>,
        args: Arc<[DurableCallArg]>,
    },
    Param {
        index: u32,
    },
    Block {
        statements: Arc<[DurableAirRef]>,
        value: DurableAirRef,
    },
    StructInit {
        struct_key: StableDefinitionKey,
        fields: Arc<[DurableAirRef]>,
        source_order: Arc<[u32]>,
    },
    ArrayInit {
        elements: Arc<[DurableAirRef]>,
    },
    PlaceRead {
        place: DurablePlaceRef,
    },
    PlaceWrite {
        place: DurablePlaceRef,
        value: DurableAirRef,
    },
    EnumVariant {
        enum_key: StableDefinitionKey,
        variant_index: u32,
        payload: Arc<[DurableAirRef]>,
    },
    EnumPayloadGet {
        base: DurableAirRef,
        enum_key: StableDefinitionKey,
        variant_index: u32,
        field_index: u32,
    },
    IntCast {
        value: DurableAirRef,
        from_ty: DurableType,
    },
    Drop {
        value: DurableAirRef,
    },
    StorageLive {
        slot: u32,
    },
    StorageDead {
        slot: u32,
    },
    MarkMoved {
        value: DurableAirRef,
        slot: u32,
        is_param: bool,
        place: Option<DurablePlaceRef>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableOrdinaryBodyPayload {
    pub schema_version: u32,
    pub owner: StableDefinitionKey,
    /// Exact current-revision body/signature input captured with the live
    /// stable issuer. Finalization rejects a same-owner stale payload.
    pub expected_inputs: crate::StableDefinitionInputFingerprint,
    pub return_type: DurableType,
    pub instructions: Arc<[DurableAirInst]>,
    pub places: Arc<[DurablePlace]>,
    pub strings: Arc<[Arc<str>]>,
    pub param_drops: Arc<[(u32, DurableType)]>,
    pub borrow_slots: Arc<[u32]>,
    pub num_locals: u32,
    pub num_param_slots: u32,
    pub param_by_ref: Arc<[bool]>,
    pub param_writable: Arc<[bool]>,
    // Warning-producing bodies are rejected from this schema. Global unused
    // and CFG warnings are outside this artifact and are always recomputed.
    pub allow_unreachable_code: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableOrdinaryBody {
    pub payload: DurableOrdinaryBodyPayload,
    /// Exact semantic inputs which authorize this candidate. Finalization
    /// validates that its owner equals `payload.owner`.
    pub inputs: StableBodyDependencyInputRecord,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DurableBodyWork {
    pub candidate_comparisons: usize,
    pub candidate_fallbacks: usize,
    pub export_attempts: usize,
    pub export_successes: usize,
    pub export_rejections: usize,
    pub instructions_exported: usize,
    pub places_exported: usize,
    pub strings_exported: usize,
    pub conversion_attempts: usize,
    pub conversion_completions: usize,
    pub conversion_failures: usize,
    pub stable_key_joins: usize,
    pub finalization_attempts: usize,
    pub finalization_completions: usize,
    pub finalization_failures: usize,
    pub projection_attempts: usize,
    pub projection_completions: usize,
    pub projection_failures: usize,
    pub instructions_projected: usize,
    pub places_projected: usize,
    pub strings_projected: usize,
    pub import_attempts: usize,
    pub import_successes: usize,
    pub import_failures: usize,
    pub installed_instructions: usize,
    pub installed_places: usize,
    pub installed_strings: usize,
    pub atomic_discards: usize,
    pub reused_bodies: usize,
    pub skipped_body_analyses: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableBodyProjectionFailure {
    SchemaVersionMismatch,
    OwnerInputMismatch,
    BlockedDependencyInputs,
    InputFingerprintMismatch,
    InvalidInstructionReference,
    ForwardInstructionReference,
    InvalidPlaceReference,
    InvalidStringReference,
    InvalidAnchor,
    InvalidSourceOrder,
    InvalidParameterModes,
    InvalidParameterDrop,
    InvalidBorrowSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableBodyConversionFailure {
    ForeignRevision,
    ForeignOwnerToken,
    MissingStableDefinition,
    AmbiguousStableDefinition,
    WrongDefinitionKind,
    MissingDependencyInputs,
    AmbiguousDependencyInputs,
    OwnerInputMismatch,
    BlockedDependencyInputs,
    WarningProducingBody,
    UnresolvedModule,
    UnsupportedGenericCall,
    FingerprintUnavailable,
}

fn semantic_kind(kind: StableDefinitionKind) -> Option<SemanticBodyDefinitionKind> {
    match kind {
        StableDefinitionKind::Function => Some(SemanticBodyDefinitionKind::FreeFunction),
        StableDefinitionKind::Method => Some(SemanticBodyDefinitionKind::Method),
        StableDefinitionKind::AssociatedFunction => {
            Some(SemanticBodyDefinitionKind::AssociatedFunction)
        }
        StableDefinitionKind::Destructor => Some(SemanticBodyDefinitionKind::Destructor),
        StableDefinitionKind::Struct => Some(SemanticBodyDefinitionKind::Struct),
        StableDefinitionKind::Enum => Some(SemanticBodyDefinitionKind::Enum),
        StableDefinitionKind::ValueConst | StableDefinitionKind::ModuleBinding => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DefinitionJoinIdentity<'a> {
    file_id: u32,
    name: &'a str,
    kind: SemanticBodyDefinitionKind,
    owner: Option<&'a str>,
}

impl<'a> From<&'a SemanticBodyDefinitionIdentity> for DefinitionJoinIdentity<'a> {
    fn from(identity: &'a SemanticBodyDefinitionIdentity) -> Self {
        Self {
            file_id: identity.file_id,
            name: &identity.name,
            kind: identity.kind,
            owner: identity.owner.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum IndexedDefinition<'a> {
    Unique(&'a StableDefinitionKey),
    Ambiguous,
}

struct DefinitionJoinIndex<'a> {
    definitions: HashMap<DefinitionJoinIdentity<'a>, IndexedDefinition<'a>>,
}

impl<'a> DefinitionJoinIndex<'a> {
    fn new(merged: &'a CanonicalMergedProgram, definitions: &'a BoundDefinitionSet) -> Self {
        let module_files = merged
            .ast()
            .modules()
            .iter()
            .map(|module| (module.module_id().clone(), module.file_id().index()))
            .collect::<HashMap<_, _>>();
        let entries = definitions.definitions().iter().filter_map(|record| {
            let key = record.stable_key();
            Some((
                DefinitionJoinIdentity {
                    file_id: *module_files.get(key.module())?,
                    name: key.name(),
                    kind: semantic_kind(key.kind())?,
                    owner: key.owner().map(|owner| owner.name()),
                },
                key,
            ))
        });
        Self::from_entries(entries)
    }

    fn from_entries(
        entries: impl IntoIterator<Item = (DefinitionJoinIdentity<'a>, &'a StableDefinitionKey)>,
    ) -> Self {
        let mut definitions = HashMap::new();
        for (identity, key) in entries {
            definitions
                .entry(identity)
                .and_modify(|entry| *entry = IndexedDefinition::Ambiguous)
                .or_insert(IndexedDefinition::Unique(key));
        }
        Self { definitions }
    }

    fn join(
        &self,
        identity: &SemanticBodyDefinitionIdentity,
        work: &mut DurableBodyWork,
    ) -> Result<&'a StableDefinitionKey, DurableBodyConversionFailure> {
        work.stable_key_joins += 1;
        match self
            .definitions
            .get(&DefinitionJoinIdentity::from(identity))
        {
            Some(IndexedDefinition::Unique(key)) => Ok(key),
            Some(IndexedDefinition::Ambiguous) => {
                Err(DurableBodyConversionFailure::AmbiguousStableDefinition)
            }
            None => Err(DurableBodyConversionFailure::MissingStableDefinition),
        }
    }
}

fn join_definition<'a>(
    identity: &SemanticBodyDefinitionIdentity,
    index: &DefinitionJoinIndex<'a>,
    work: &mut DurableBodyWork,
) -> Result<&'a StableDefinitionKey, DurableBodyConversionFailure> {
    index.join(identity, work)
}

fn durable_type(
    ty: &SemanticImportType<SemanticBodyDefinitionIdentity, Arc<str>>,
    merged: &CanonicalMergedProgram,
    index: &DefinitionJoinIndex<'_>,
    work: &mut DurableBodyWork,
) -> Result<DurableType, DurableBodyConversionFailure> {
    Ok(match ty {
        SemanticImportType::I8 => DurableType::I8,
        SemanticImportType::I16 => DurableType::I16,
        SemanticImportType::I32 => DurableType::I32,
        SemanticImportType::I64 => DurableType::I64,
        SemanticImportType::U8 => DurableType::U8,
        SemanticImportType::U16 => DurableType::U16,
        SemanticImportType::U32 => DurableType::U32,
        SemanticImportType::U64 => DurableType::U64,
        SemanticImportType::Bool => DurableType::Bool,
        SemanticImportType::Unit => DurableType::Unit,
        SemanticImportType::Never => DurableType::Never,
        SemanticImportType::ComptimeType => DurableType::ComptimeType,
        SemanticImportType::BuiltinNominal { name, kind } => durable_builtin_nominal(name, *kind)?,
        SemanticImportType::Nominal(identity) => {
            let key = join_definition(identity, index, work)?;
            if !matches!(
                key.kind(),
                StableDefinitionKind::Struct | StableDefinitionKind::Enum
            ) {
                return Err(DurableBodyConversionFailure::WrongDefinitionKind);
            }
            DurableType::Nominal(key.clone())
        }
        SemanticImportType::Array { element, len } => DurableType::Array {
            element: Box::new(durable_type(element, merged, index, work)?),
            len: *len,
        },
        SemanticImportType::PtrConst(value) => {
            DurableType::PtrConst(Box::new(durable_type(value, merged, index, work)?))
        }
        SemanticImportType::PtrMut(value) => {
            DurableType::PtrMut(Box::new(durable_type(value, merged, index, work)?))
        }
        SemanticImportType::Module(path) => DurableType::Module(
            merged
                .ast()
                .modules()
                .iter()
                .find(|module| module.module_id().as_str() == path.as_ref())
                .map(|module| module.module_id().clone())
                .ok_or(DurableBodyConversionFailure::UnresolvedModule)?,
        ),
        SemanticImportType::GenericParameter(index) => DurableType::GenericParameter(*index),
        SemanticImportType::Tuple(elements) => DurableType::Tuple(
            elements
                .iter()
                .map(|element| durable_type(element, merged, index, work))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        ),
        SemanticImportType::Function { parameters, result } => DurableType::Function {
            parameters: parameters
                .iter()
                .map(|parameter| durable_type(parameter, merged, index, work))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
            result: Box::new(durable_type(result, merged, index, work)?),
        },
    })
}

fn durable_builtin_nominal(
    name: &Arc<str>,
    kind: rue_air::SemanticImportNominalKind,
) -> Result<DurableType, DurableBodyConversionFailure> {
    if crate::durable_semantics::builtin_nominal_kind(name) != Some(kind) {
        return Err(DurableBodyConversionFailure::WrongDefinitionKind);
    }
    Ok(DurableType::BuiltinNominal {
        name: name.clone(),
        kind,
    })
}

/// Atomically join AIR's neutral ordinary-body exports to the authoritative
/// stable-definition universe for this exact revision. Complete dependency
/// inputs are attached later by [`finalize_durable_ordinary_bodies`].
pub fn convert_semantic_body_exports(
    exports: &[rue_air::SemanticBodyExport],
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
    work: &mut DurableBodyWork,
) -> Result<Arc<[DurableOrdinaryBodyPayload]>, DurableBodyConversionFailure> {
    if definitions.source_revision() != merged.ast().source_revision() {
        work.conversion_failures += exports.len();
        work.atomic_discards += usize::from(!exports.is_empty());
        return Err(DurableBodyConversionFailure::ForeignRevision);
    }
    let join_index = DefinitionJoinIndex::new(merged, definitions);
    let convert = |export: &rue_air::SemanticBodyExport,
                   work: &mut DurableBodyWork|
     -> Result<DurableOrdinaryBodyPayload, DurableBodyConversionFailure> {
        work.conversion_attempts += 1;
        let owner_record = definitions
            .definition_for_body_token(export.owner)
            .map_err(|_| DurableBodyConversionFailure::ForeignOwnerToken)?;
        let owner = owner_record.stable_key().clone();
        if !matches!(
            owner.kind(),
            StableDefinitionKind::Function
                | StableDefinitionKind::Method
                | StableDefinitionKind::AssociatedFunction
                | StableDefinitionKind::Destructor
        ) {
            return Err(DurableBodyConversionFailure::WrongDefinitionKind);
        }
        let expected_inputs = crate::session::stable_definition_input_fingerprint(
            merged.definitions().source_snapshot(),
            owner_record,
        )
        .map_err(|_| DurableBodyConversionFailure::FingerprintUnavailable)?;
        let body = &export.body;
        if !body.warnings.is_empty() {
            return Err(DurableBodyConversionFailure::WarningProducingBody);
        }
        let key = |identity: &SemanticBodyDefinitionIdentity, work: &mut DurableBodyWork| {
            join_definition(identity, &join_index, work).cloned()
        };
        let ty = |value: &SemanticImportType<SemanticBodyDefinitionIdentity, Arc<str>>,
                  work: &mut DurableBodyWork| {
            durable_type(value, merged, &join_index, work)
        };
        let arg = |arg: &rue_air::SemanticBodyCallArg| DurableCallArg {
            value: arg.value,
            mode: arg.mode,
        };
        let pattern = |value: &SemanticBodyPattern<SemanticBodyDefinitionIdentity>,
                       work: &mut DurableBodyWork|
         -> Result<DurablePattern, DurableBodyConversionFailure> {
            Ok(match value {
                SemanticBodyPattern::Wildcard => DurablePattern::Wildcard,
                SemanticBodyPattern::Int(value) => DurablePattern::Int(*value),
                SemanticBodyPattern::Bool(value) => DurablePattern::Bool(*value),
                SemanticBodyPattern::EnumVariant {
                    enum_key,
                    variant_index,
                } => {
                    let enum_key = key(enum_key, work)?;
                    if enum_key.kind() != StableDefinitionKind::Enum {
                        return Err(DurableBodyConversionFailure::WrongDefinitionKind);
                    }
                    DurablePattern::EnumVariant {
                        enum_key,
                        variant_index: *variant_index,
                    }
                }
            })
        };
        let data = |value: &SemanticBodyInstData<SemanticBodyDefinitionIdentity, Arc<str>>,
                    work: &mut DurableBodyWork|
         -> Result<DurableAirInstData, DurableBodyConversionFailure> {
            macro_rules! bin {
                ($variant:ident, $a:expr, $b:expr) => {
                    DurableAirInstData::$variant(*$a, *$b)
                };
            }
            Ok(match value {
                SemanticBodyInstData::Const(v) => DurableAirInstData::Const(*v),
                SemanticBodyInstData::BoolConst(v) => DurableAirInstData::BoolConst(*v),
                SemanticBodyInstData::StringConst(v) => DurableAirInstData::StringConst(*v),
                SemanticBodyInstData::UnitConst => DurableAirInstData::UnitConst,
                SemanticBodyInstData::TypeConst(v) => DurableAirInstData::TypeConst(ty(v, work)?),
                SemanticBodyInstData::Add(a, b) => bin!(Add, a, b),
                SemanticBodyInstData::Sub(a, b) => bin!(Sub, a, b),
                SemanticBodyInstData::Mul(a, b) => bin!(Mul, a, b),
                SemanticBodyInstData::Div(a, b) => bin!(Div, a, b),
                SemanticBodyInstData::Mod(a, b) => bin!(Mod, a, b),
                SemanticBodyInstData::Eq(a, b) => bin!(Eq, a, b),
                SemanticBodyInstData::Ne(a, b) => bin!(Ne, a, b),
                SemanticBodyInstData::Lt(a, b) => bin!(Lt, a, b),
                SemanticBodyInstData::Gt(a, b) => bin!(Gt, a, b),
                SemanticBodyInstData::Le(a, b) => bin!(Le, a, b),
                SemanticBodyInstData::Ge(a, b) => bin!(Ge, a, b),
                SemanticBodyInstData::And(a, b) => bin!(And, a, b),
                SemanticBodyInstData::Or(a, b) => bin!(Or, a, b),
                SemanticBodyInstData::BitAnd(a, b) => bin!(BitAnd, a, b),
                SemanticBodyInstData::BitOr(a, b) => bin!(BitOr, a, b),
                SemanticBodyInstData::BitXor(a, b) => bin!(BitXor, a, b),
                SemanticBodyInstData::Shl(a, b) => bin!(Shl, a, b),
                SemanticBodyInstData::Shr(a, b) => bin!(Shr, a, b),
                SemanticBodyInstData::Neg(v) => DurableAirInstData::Neg(*v),
                SemanticBodyInstData::Not(v) => DurableAirInstData::Not(*v),
                SemanticBodyInstData::BitNot(v) => DurableAirInstData::BitNot(*v),
                SemanticBodyInstData::Branch {
                    cond,
                    then_value,
                    else_value,
                } => DurableAirInstData::Branch {
                    cond: *cond,
                    then_value: *then_value,
                    else_value: *else_value,
                },
                SemanticBodyInstData::Loop { cond, body } => DurableAirInstData::Loop {
                    cond: *cond,
                    body: *body,
                },
                SemanticBodyInstData::InfiniteLoop { body } => {
                    DurableAirInstData::InfiniteLoop { body: *body }
                }
                SemanticBodyInstData::Match { scrutinee, arms } => DurableAirInstData::Match {
                    scrutinee: *scrutinee,
                    arms: arms
                        .iter()
                        .map(|arm| {
                            Ok(DurableMatchArm {
                                pattern: pattern(&arm.pattern, work)?,
                                body: arm.body,
                            })
                        })
                        .collect::<Result<Vec<_>, DurableBodyConversionFailure>>()?
                        .into(),
                },
                SemanticBodyInstData::Break => DurableAirInstData::Break,
                SemanticBodyInstData::Continue => DurableAirInstData::Continue,
                SemanticBodyInstData::Alloc { slot, init } => DurableAirInstData::Alloc {
                    slot: *slot,
                    init: *init,
                },
                SemanticBodyInstData::Load { slot } => DurableAirInstData::Load { slot: *slot },
                SemanticBodyInstData::Store { slot, value } => DurableAirInstData::Store {
                    slot: *slot,
                    value: *value,
                },
                SemanticBodyInstData::ParamStore { param_slot, value } => {
                    DurableAirInstData::ParamStore {
                        param_slot: *param_slot,
                        value: *value,
                    }
                }
                SemanticBodyInstData::Ret(value) => DurableAirInstData::Ret(*value),
                SemanticBodyInstData::Call { function, args } => DurableAirInstData::Call {
                    function: key(function, work)?,
                    args: args.iter().map(arg).collect::<Vec<_>>().into(),
                },
                SemanticBodyInstData::CallGeneric => {
                    return Err(DurableBodyConversionFailure::UnsupportedGenericCall);
                }
                SemanticBodyInstData::Intrinsic { name, args } => DurableAirInstData::Intrinsic {
                    name: name.clone(),
                    args: args.iter().map(arg).collect::<Vec<_>>().into(),
                },
                SemanticBodyInstData::Param { index } => {
                    DurableAirInstData::Param { index: *index }
                }
                SemanticBodyInstData::Block { statements, value } => DurableAirInstData::Block {
                    statements: statements.clone(),
                    value: *value,
                },
                SemanticBodyInstData::StructInit {
                    struct_key,
                    fields,
                    source_order,
                } => DurableAirInstData::StructInit {
                    struct_key: key(struct_key, work)?,
                    fields: fields.clone(),
                    source_order: source_order.clone(),
                },
                SemanticBodyInstData::ArrayInit { elements } => DurableAirInstData::ArrayInit {
                    elements: elements.clone(),
                },
                SemanticBodyInstData::PlaceRead { place } => {
                    DurableAirInstData::PlaceRead { place: *place }
                }
                SemanticBodyInstData::PlaceWrite { place, value } => {
                    DurableAirInstData::PlaceWrite {
                        place: *place,
                        value: *value,
                    }
                }
                SemanticBodyInstData::EnumVariant {
                    enum_key,
                    variant_index,
                    payload,
                } => DurableAirInstData::EnumVariant {
                    enum_key: key(enum_key, work)?,
                    variant_index: *variant_index,
                    payload: payload.clone(),
                },
                SemanticBodyInstData::EnumPayloadGet {
                    base,
                    enum_key,
                    variant_index,
                    field_index,
                } => DurableAirInstData::EnumPayloadGet {
                    base: *base,
                    enum_key: key(enum_key, work)?,
                    variant_index: *variant_index,
                    field_index: *field_index,
                },
                SemanticBodyInstData::IntCast { value, from_ty } => DurableAirInstData::IntCast {
                    value: *value,
                    from_ty: ty(from_ty, work)?,
                },
                SemanticBodyInstData::Drop { value } => DurableAirInstData::Drop { value: *value },
                SemanticBodyInstData::StorageLive { slot } => {
                    DurableAirInstData::StorageLive { slot: *slot }
                }
                SemanticBodyInstData::StorageDead { slot } => {
                    DurableAirInstData::StorageDead { slot: *slot }
                }
                SemanticBodyInstData::MarkMoved {
                    value,
                    slot,
                    is_param,
                    place,
                } => DurableAirInstData::MarkMoved {
                    value: *value,
                    slot: *slot,
                    is_param: *is_param,
                    place: *place,
                },
            })
        };
        let instructions = body
            .instructions
            .iter()
            .map(|instruction| {
                Ok(DurableAirInst {
                    data: data(&instruction.data, work)?,
                    ty: ty(&instruction.ty, work)?,
                    anchor: DurableBodyAnchor {
                        start: instruction.anchor.start,
                        end: instruction.anchor.end,
                    },
                })
            })
            .collect::<Result<Vec<_>, DurableBodyConversionFailure>>()?;
        let places = body
            .places
            .iter()
            .map(|place| {
                Ok(DurablePlace {
                    base: place.base,
                    base_type: ty(&place.base_type, work)?,
                    projections: place
                        .projections
                        .iter()
                        .map(|projection| {
                            Ok(match projection {
                                SemanticBodyProjection::Field {
                                    struct_key,
                                    field_index,
                                } => DurableProjection::Field {
                                    struct_key: key(struct_key, work)?,
                                    field_index: *field_index,
                                },
                                SemanticBodyProjection::Index { array_type, index } => {
                                    DurableProjection::Index {
                                        array_type: ty(array_type, work)?,
                                        index: *index,
                                    }
                                }
                            })
                        })
                        .collect::<Result<Vec<_>, DurableBodyConversionFailure>>()?
                        .into(),
                })
            })
            .collect::<Result<Vec<_>, DurableBodyConversionFailure>>()?;
        let result = DurableOrdinaryBodyPayload {
            schema_version: DURABLE_ORDINARY_BODY_SCHEMA_VERSION,
            owner,
            expected_inputs,
            return_type: ty(&body.return_type, work)?,
            instructions: instructions.into(),
            places: places.into(),
            strings: body.strings.clone(),
            param_drops: body
                .param_drops
                .iter()
                .map(|(slot, value)| Ok((*slot, ty(value, work)?)))
                .collect::<Result<Vec<_>, DurableBodyConversionFailure>>()?
                .into(),
            borrow_slots: body.borrow_slots.clone(),
            num_locals: body.num_locals,
            num_param_slots: body.num_param_slots,
            param_by_ref: body.param_by_ref.clone(),
            param_writable: body.param_writable.clone(),
            allow_unreachable_code: body.allow_unreachable_code,
        };
        work.conversion_completions += 1;
        Ok(result)
    };
    let mut converted = Vec::with_capacity(exports.len());
    for export in exports {
        match convert(export, work) {
            Ok(body) => converted.push(body),
            Err(error) => {
                work.conversion_failures += 1;
                work.atomic_discards += 1;
                return Err(error);
            }
        }
    }
    Ok(converted.into())
}

/// Attach the complete dependency records after the session has built them.
/// This stage is all-or-nothing and never mutates canonical semantic work.
pub fn finalize_durable_ordinary_bodies(
    payloads: &[DurableOrdinaryBodyPayload],
    inputs: &[StableBodyDependencyInputRecord],
    work: &mut DurableBodyWork,
) -> Result<Arc<[DurableOrdinaryBody]>, DurableBodyConversionFailure> {
    let mut result = Vec::with_capacity(payloads.len());
    for payload in payloads {
        work.finalization_attempts += 1;
        let matches = inputs
            .iter()
            .filter(|input| input.owner() == &payload.owner)
            .collect::<Vec<_>>();
        let input = match matches.as_slice() {
            [input] => (*input).clone(),
            [] => {
                work.finalization_failures += 1;
                work.atomic_discards += 1;
                return Err(DurableBodyConversionFailure::MissingDependencyInputs);
            }
            _ => {
                work.finalization_failures += 1;
                work.atomic_discards += 1;
                return Err(DurableBodyConversionFailure::AmbiguousDependencyInputs);
            }
        };
        if input.owner() != &payload.owner {
            work.finalization_failures += 1;
            work.atomic_discards += 1;
            return Err(DurableBodyConversionFailure::OwnerInputMismatch);
        }
        if input.fingerprint() != &payload.expected_inputs {
            work.finalization_failures += 1;
            work.atomic_discards += 1;
            return Err(DurableBodyConversionFailure::OwnerInputMismatch);
        }
        if !input.reusable_boundary_supported() {
            work.finalization_failures += 1;
            work.atomic_discards += 1;
            return Err(DurableBodyConversionFailure::BlockedDependencyInputs);
        }
        result.push(DurableOrdinaryBody {
            payload: payload.clone(),
            inputs: input,
        });
        work.finalization_completions += 1;
    }
    Ok(result.into())
}

impl DurableOrdinaryBody {
    pub fn owner(&self) -> &StableDefinitionKey {
        &self.payload.owner
    }

    pub fn inputs(&self) -> &StableBodyDependencyInputRecord {
        &self.inputs
    }

    pub fn project_semantic_body(
        &self,
        work: &mut DurableBodyWork,
    ) -> Result<rue_air::SemanticBody<StableDefinitionKey, Arc<str>>, DurableBodyProjectionFailure>
    {
        if self.inputs.owner() != &self.payload.owner
            || self.inputs.fingerprint() != &self.payload.expected_inputs
        {
            work.projection_attempts += 1;
            work.projection_failures += 1;
            return Err(DurableBodyProjectionFailure::InputFingerprintMismatch);
        }
        self.payload.project_semantic_body(work)
    }
}

impl DurableOrdinaryBodyPayload {
    pub(crate) fn referenced_definition_keys(&self) -> BTreeSet<StableDefinitionKey> {
        fn visit_type(value: &DurableType, keys: &mut BTreeSet<StableDefinitionKey>) {
            match value {
                DurableType::Nominal(key) => {
                    keys.insert(key.clone());
                }
                DurableType::Array { element, .. }
                | DurableType::PtrConst(element)
                | DurableType::PtrMut(element) => visit_type(element, keys),
                DurableType::Tuple(values) => {
                    for value in values.iter() {
                        visit_type(value, keys);
                    }
                }
                DurableType::Function { parameters, result } => {
                    for value in parameters.iter() {
                        visit_type(value, keys);
                    }
                    visit_type(result, keys);
                }
                _ => {}
            }
        }

        let mut keys = BTreeSet::new();
        visit_type(&self.return_type, &mut keys);
        for instruction in self.instructions.iter() {
            visit_type(&instruction.ty, &mut keys);
            match &instruction.data {
                DurableAirInstData::TypeConst(value)
                | DurableAirInstData::IntCast { from_ty: value, .. } => {
                    visit_type(value, &mut keys);
                }
                DurableAirInstData::Call { function, .. } => {
                    keys.insert(function.clone());
                }
                DurableAirInstData::StructInit { struct_key, .. } => {
                    keys.insert(struct_key.clone());
                }
                DurableAirInstData::EnumVariant { enum_key, .. }
                | DurableAirInstData::EnumPayloadGet { enum_key, .. } => {
                    keys.insert(enum_key.clone());
                }
                DurableAirInstData::Match { arms, .. } => {
                    for arm in arms.iter() {
                        if let DurablePattern::EnumVariant { enum_key, .. } = &arm.pattern {
                            keys.insert(enum_key.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        for place in self.places.iter() {
            visit_type(&place.base_type, &mut keys);
            for projection in place.projections.iter() {
                match projection {
                    DurableProjection::Field { struct_key, .. } => {
                        keys.insert(struct_key.clone());
                    }
                    DurableProjection::Index { array_type, .. } => {
                        visit_type(array_type, &mut keys);
                    }
                }
            }
        }
        for (_, value) in self.param_drops.iter() {
            visit_type(value, &mut keys);
        }
        keys
    }

    /// Project the durable algebra into AIR's neutral import DTO. This does not
    /// allocate any AIR instruction, type, string, nominal, or function ID.
    pub fn project_semantic_body(
        &self,
        work: &mut DurableBodyWork,
    ) -> Result<rue_air::SemanticBody<StableDefinitionKey, Arc<str>>, DurableBodyProjectionFailure>
    {
        use rue_air::{SemanticBodyInstData as S, SemanticBodyPattern as P};
        work.projection_attempts += 1;
        if self.schema_version != DURABLE_ORDINARY_BODY_SCHEMA_VERSION {
            work.projection_failures += 1;
            return Err(DurableBodyProjectionFailure::SchemaVersionMismatch);
        }
        if self.param_by_ref.len() != self.num_param_slots as usize
            || self.param_writable.len() != self.num_param_slots as usize
            || self
                .param_writable
                .iter()
                .zip(self.param_by_ref.iter())
                .any(|(writable, by_ref)| *writable && !*by_ref)
        {
            work.projection_failures += 1;
            return Err(DurableBodyProjectionFailure::InvalidParameterModes);
        }
        if self
            .param_drops
            .iter()
            .any(|(slot, _)| *slot >= self.num_param_slots)
        {
            work.projection_failures += 1;
            return Err(DurableBodyProjectionFailure::InvalidParameterDrop);
        }
        if self
            .borrow_slots
            .iter()
            .any(|slot| *slot >= self.num_locals)
        {
            work.projection_failures += 1;
            return Err(DurableBodyProjectionFailure::InvalidBorrowSlot);
        }
        let instruction_len = self.instructions.len();
        let place_len = self.places.len();
        for place in self.places.iter() {
            for projection in place.projections.iter() {
                if let DurableProjection::Index { index, .. } = projection
                    && *index as usize >= instruction_len
                {
                    work.projection_failures += 1;
                    return Err(DurableBodyProjectionFailure::InvalidInstructionReference);
                }
            }
        }
        for (current, instruction) in self.instructions.iter().enumerate() {
            if instruction.anchor.start > instruction.anchor.end {
                work.projection_failures += 1;
                return Err(DurableBodyProjectionFailure::InvalidAnchor);
            }
            let check = |value: DurableAirRef| {
                let index = value as usize;
                if index >= instruction_len {
                    Err(DurableBodyProjectionFailure::InvalidInstructionReference)
                } else if index >= current {
                    Err(DurableBodyProjectionFailure::ForwardInstructionReference)
                } else {
                    Ok(())
                }
            };
            let check_all = |values: &[DurableAirRef]| -> Result<_, _> {
                values.iter().try_for_each(|value| check(*value))
            };
            use DurableAirInstData as D;
            let validated = match &instruction.data {
                D::Const(_)
                | D::BoolConst(_)
                | D::UnitConst
                | D::TypeConst(_)
                | D::Break
                | D::Continue
                | D::Load { .. }
                | D::Param { .. }
                | D::StorageLive { .. }
                | D::StorageDead { .. } => Ok(()),
                D::StringConst(index) => {
                    if *index as usize >= self.strings.len() {
                        Err(DurableBodyProjectionFailure::InvalidStringReference)
                    } else {
                        Ok(())
                    }
                }
                D::Add(a, b)
                | D::Sub(a, b)
                | D::Mul(a, b)
                | D::Div(a, b)
                | D::Mod(a, b)
                | D::Eq(a, b)
                | D::Ne(a, b)
                | D::Lt(a, b)
                | D::Gt(a, b)
                | D::Le(a, b)
                | D::Ge(a, b)
                | D::And(a, b)
                | D::Or(a, b)
                | D::BitAnd(a, b)
                | D::BitOr(a, b)
                | D::BitXor(a, b)
                | D::Shl(a, b)
                | D::Shr(a, b) => check(*a).and_then(|()| check(*b)),
                D::Neg(v)
                | D::Not(v)
                | D::BitNot(v)
                | D::Drop { value: v }
                | D::IntCast { value: v, .. } => check(*v),
                D::Branch {
                    cond,
                    then_value,
                    else_value,
                } => check(*cond)
                    .and_then(|()| check(*then_value))
                    .and_then(|()| else_value.map_or(Ok(()), &check)),
                D::Loop { cond, body } => check(*cond).and_then(|()| check(*body)),
                D::InfiniteLoop { body } => check(*body),
                D::Match { scrutinee, arms } => {
                    check(*scrutinee).and_then(|()| arms.iter().try_for_each(|arm| check(arm.body)))
                }
                D::Alloc { init, .. } => check(*init),
                D::Store { value, .. } | D::ParamStore { value, .. } => check(*value),
                D::Ret(value) => value.map_or(Ok(()), &check),
                D::Call { args, .. } | D::Intrinsic { args, .. } => {
                    args.iter().try_for_each(|arg| check(arg.value))
                }
                D::Block { statements, value } => {
                    check_all(statements).and_then(|()| check(*value))
                }
                D::StructInit {
                    fields,
                    source_order,
                    ..
                } => {
                    let mut seen = vec![false; fields.len()];
                    if fields.len() != source_order.len()
                        || source_order.iter().any(|index| {
                            let index = *index as usize;
                            index >= fields.len() || std::mem::replace(&mut seen[index], true)
                        })
                    {
                        Err(DurableBodyProjectionFailure::InvalidSourceOrder)
                    } else {
                        check_all(fields)
                    }
                }
                D::EnumPayloadGet { base, .. } => check(*base),
                D::ArrayInit { elements }
                | D::EnumVariant {
                    payload: elements, ..
                } => check_all(elements),
                D::PlaceRead { place } => {
                    if *place as usize >= place_len {
                        Err(DurableBodyProjectionFailure::InvalidPlaceReference)
                    } else {
                        Ok(())
                    }
                }
                D::PlaceWrite { place, value } => {
                    if *place as usize >= place_len {
                        Err(DurableBodyProjectionFailure::InvalidPlaceReference)
                    } else {
                        check(*value)
                    }
                }
                D::MarkMoved { value, place, .. } => {
                    if place.is_some_and(|place| place as usize >= place_len) {
                        Err(DurableBodyProjectionFailure::InvalidPlaceReference)
                    } else {
                        check(*value)
                    }
                }
            };
            if let Err(error) = validated {
                work.projection_failures += 1;
                return Err(error);
            }
        }
        let arg = |a: &DurableCallArg| rue_air::SemanticBodyCallArg {
            value: a.value,
            mode: a.mode,
        };
        let pattern = |p: &DurablePattern| match p {
            DurablePattern::Wildcard => P::Wildcard,
            DurablePattern::Int(v) => P::Int(*v),
            DurablePattern::Bool(v) => P::Bool(*v),
            DurablePattern::EnumVariant {
                enum_key,
                variant_index,
            } => P::EnumVariant {
                enum_key: enum_key.clone(),
                variant_index: *variant_index,
            },
        };
        let data = |d: &DurableAirInstData| -> S<StableDefinitionKey, Arc<str>> {
            macro_rules! bin {
                ($v:ident, $a:expr, $b:expr) => {
                    S::$v(*$a, *$b)
                };
            }
            match d {
                DurableAirInstData::Const(v) => S::Const(*v),
                DurableAirInstData::BoolConst(v) => S::BoolConst(*v),
                DurableAirInstData::StringConst(v) => S::StringConst(*v),
                DurableAirInstData::UnitConst => S::UnitConst,
                DurableAirInstData::TypeConst(v) => S::TypeConst(v.import_dto()),
                DurableAirInstData::Add(a, b) => bin!(Add, a, b),
                DurableAirInstData::Sub(a, b) => bin!(Sub, a, b),
                DurableAirInstData::Mul(a, b) => bin!(Mul, a, b),
                DurableAirInstData::Div(a, b) => bin!(Div, a, b),
                DurableAirInstData::Mod(a, b) => bin!(Mod, a, b),
                DurableAirInstData::Eq(a, b) => bin!(Eq, a, b),
                DurableAirInstData::Ne(a, b) => bin!(Ne, a, b),
                DurableAirInstData::Lt(a, b) => bin!(Lt, a, b),
                DurableAirInstData::Gt(a, b) => bin!(Gt, a, b),
                DurableAirInstData::Le(a, b) => bin!(Le, a, b),
                DurableAirInstData::Ge(a, b) => bin!(Ge, a, b),
                DurableAirInstData::And(a, b) => bin!(And, a, b),
                DurableAirInstData::Or(a, b) => bin!(Or, a, b),
                DurableAirInstData::BitAnd(a, b) => bin!(BitAnd, a, b),
                DurableAirInstData::BitOr(a, b) => bin!(BitOr, a, b),
                DurableAirInstData::BitXor(a, b) => bin!(BitXor, a, b),
                DurableAirInstData::Shl(a, b) => bin!(Shl, a, b),
                DurableAirInstData::Shr(a, b) => bin!(Shr, a, b),
                DurableAirInstData::Neg(v) => S::Neg(*v),
                DurableAirInstData::Not(v) => S::Not(*v),
                DurableAirInstData::BitNot(v) => S::BitNot(*v),
                DurableAirInstData::Branch {
                    cond,
                    then_value,
                    else_value,
                } => S::Branch {
                    cond: *cond,
                    then_value: *then_value,
                    else_value: *else_value,
                },
                DurableAirInstData::Loop { cond, body } => S::Loop {
                    cond: *cond,
                    body: *body,
                },
                DurableAirInstData::InfiniteLoop { body } => S::InfiniteLoop { body: *body },
                DurableAirInstData::Match { scrutinee, arms } => S::Match {
                    scrutinee: *scrutinee,
                    arms: arms
                        .iter()
                        .map(|a| rue_air::SemanticBodyMatchArm {
                            pattern: pattern(&a.pattern),
                            body: a.body,
                        })
                        .collect::<Vec<_>>()
                        .into(),
                },
                DurableAirInstData::Break => S::Break,
                DurableAirInstData::Continue => S::Continue,
                DurableAirInstData::Alloc { slot, init } => S::Alloc {
                    slot: *slot,
                    init: *init,
                },
                DurableAirInstData::Load { slot } => S::Load { slot: *slot },
                DurableAirInstData::Store { slot, value } => S::Store {
                    slot: *slot,
                    value: *value,
                },
                DurableAirInstData::ParamStore { param_slot, value } => S::ParamStore {
                    param_slot: *param_slot,
                    value: *value,
                },
                DurableAirInstData::Ret(v) => S::Ret(*v),
                DurableAirInstData::Call { function, args } => S::Call {
                    function: function.clone(),
                    args: args.iter().map(arg).collect::<Vec<_>>().into(),
                },
                DurableAirInstData::Intrinsic { name, args } => S::Intrinsic {
                    name: name.clone(),
                    args: args.iter().map(arg).collect::<Vec<_>>().into(),
                },
                DurableAirInstData::Param { index } => S::Param { index: *index },
                DurableAirInstData::Block { statements, value } => S::Block {
                    statements: statements.clone(),
                    value: *value,
                },
                DurableAirInstData::StructInit {
                    struct_key,
                    fields,
                    source_order,
                } => S::StructInit {
                    struct_key: struct_key.clone(),
                    fields: fields.clone(),
                    source_order: source_order.clone(),
                },
                DurableAirInstData::ArrayInit { elements } => S::ArrayInit {
                    elements: elements.clone(),
                },
                DurableAirInstData::PlaceRead { place } => S::PlaceRead { place: *place },
                DurableAirInstData::PlaceWrite { place, value } => S::PlaceWrite {
                    place: *place,
                    value: *value,
                },
                DurableAirInstData::EnumVariant {
                    enum_key,
                    variant_index,
                    payload,
                } => S::EnumVariant {
                    enum_key: enum_key.clone(),
                    variant_index: *variant_index,
                    payload: payload.clone(),
                },
                DurableAirInstData::EnumPayloadGet {
                    base,
                    enum_key,
                    variant_index,
                    field_index,
                } => S::EnumPayloadGet {
                    base: *base,
                    enum_key: enum_key.clone(),
                    variant_index: *variant_index,
                    field_index: *field_index,
                },
                DurableAirInstData::IntCast { value, from_ty } => S::IntCast {
                    value: *value,
                    from_ty: from_ty.import_dto(),
                },
                DurableAirInstData::Drop { value } => S::Drop { value: *value },
                DurableAirInstData::StorageLive { slot } => S::StorageLive { slot: *slot },
                DurableAirInstData::StorageDead { slot } => S::StorageDead { slot: *slot },
                DurableAirInstData::MarkMoved {
                    value,
                    slot,
                    is_param,
                    place,
                } => S::MarkMoved {
                    value: *value,
                    slot: *slot,
                    is_param: *is_param,
                    place: *place,
                },
            }
        };
        let body = rue_air::SemanticBody {
            return_type: self.return_type.import_dto(),
            instructions: self
                .instructions
                .iter()
                .map(|i| rue_air::SemanticBodyInst {
                    data: data(&i.data),
                    ty: i.ty.import_dto(),
                    anchor: rue_air::SemanticBodyAnchor {
                        start: i.anchor.start,
                        end: i.anchor.end,
                    },
                })
                .collect::<Vec<_>>()
                .into(),
            places: self
                .places
                .iter()
                .map(|p| rue_air::SemanticBodyPlace {
                    base: p.base,
                    base_type: p.base_type.import_dto(),
                    projections: p
                        .projections
                        .iter()
                        .map(|q| match q {
                            DurableProjection::Field {
                                struct_key,
                                field_index,
                            } => rue_air::SemanticBodyProjection::Field {
                                struct_key: struct_key.clone(),
                                field_index: *field_index,
                            },
                            DurableProjection::Index { array_type, index } => {
                                rue_air::SemanticBodyProjection::Index {
                                    array_type: array_type.import_dto(),
                                    index: *index,
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                        .into(),
                })
                .collect::<Vec<_>>()
                .into(),
            strings: self.strings.clone(),
            param_drops: self
                .param_drops
                .iter()
                .map(|(s, t)| (*s, t.import_dto()))
                .collect::<Vec<_>>()
                .into(),
            borrow_slots: self.borrow_slots.clone(),
            num_locals: self.num_locals,
            num_param_slots: self.num_param_slots,
            param_by_ref: self.param_by_ref.clone(),
            param_writable: self.param_writable.clone(),
            allow_unreachable_code: self.allow_unreachable_code,
            warnings: Arc::from([]),
        };
        work.projection_completions += 1;
        work.instructions_projected += body.instructions.len();
        work.places_projected += body.places.len();
        work.strings_projected += body.strings.len();
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModuleId, StableDefinitionNamespace};

    fn owner() -> StableDefinitionKey {
        StableDefinitionKey::for_test(
            ModuleId::from_logical_path("main.rue").unwrap(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "main",
            None,
        )
    }

    fn payload(instructions: Vec<DurableAirInst>) -> DurableOrdinaryBodyPayload {
        let key = owner();
        DurableOrdinaryBodyPayload {
            schema_version: DURABLE_ORDINARY_BODY_SCHEMA_VERSION,
            owner: key.clone(),
            expected_inputs: crate::StableDefinitionInputFingerprint {
                schema_version: 1,
                key,
                declaration: crate::StableDefinitionFingerprint::for_test(1),
                signature: crate::StableDefinitionFingerprint::for_test(2),
                body_or_initializer: Some(crate::StableDefinitionFingerprint::for_test(3)),
                precision: crate::StableDefinitionFingerprintPrecision::SignatureAndBody,
            },
            return_type: DurableType::Unit,
            instructions: instructions.into(),
            places: Arc::from([]),
            strings: Arc::from([]),
            param_drops: Arc::from([]),
            borrow_slots: Arc::from([]),
            num_locals: 0,
            num_param_slots: 0,
            param_by_ref: Arc::from([]),
            param_writable: Arc::from([]),
            allow_unreachable_code: false,
        }
    }

    fn instruction(data: DurableAirInstData) -> DurableAirInst {
        DurableAirInst {
            data,
            ty: DurableType::Unit,
            anchor: DurableBodyAnchor { start: 0, end: 0 },
        }
    }

    #[test]
    fn projection_rejects_forward_instruction_references() {
        let candidate = payload(vec![instruction(DurableAirInstData::Ret(Some(0)))]);
        let mut work = DurableBodyWork::default();
        assert_eq!(
            candidate.project_semantic_body(&mut work),
            Err(DurableBodyProjectionFailure::ForwardInstructionReference)
        );
        assert_eq!(work.projection_attempts, 1);
        assert_eq!(work.projection_completions, 0);
        assert_eq!(work.projection_failures, 1);
    }

    #[test]
    fn projection_rejects_non_permutation_source_order() {
        let candidate = payload(vec![
            instruction(DurableAirInstData::UnitConst),
            instruction(DurableAirInstData::StructInit {
                struct_key: StableDefinitionKey::for_test(
                    ModuleId::from_logical_path("main.rue").unwrap(),
                    StableDefinitionNamespace::Type,
                    StableDefinitionKind::Struct,
                    "S",
                    None,
                ),
                fields: Arc::from([0]),
                source_order: Arc::from([1]),
            }),
        ]);
        let mut work = DurableBodyWork::default();
        assert_eq!(
            candidate.project_semantic_body(&mut work),
            Err(DurableBodyProjectionFailure::InvalidSourceOrder)
        );
        assert_eq!(work.projection_failures, 1);
    }

    #[test]
    fn builtin_nominals_reject_unknown_names_and_wrong_kinds() {
        use rue_air::SemanticImportNominalKind::{Enum, Struct};
        assert_eq!(
            durable_builtin_nominal(&Arc::from("MissingBuiltin"), Struct),
            Err(DurableBodyConversionFailure::WrongDefinitionKind)
        );
        assert_eq!(
            durable_builtin_nominal(&Arc::from("StrBuf"), Enum),
            Err(DurableBodyConversionFailure::WrongDefinitionKind)
        );
        assert_eq!(
            durable_builtin_nominal(&Arc::from("StrBuf"), Struct),
            Ok(DurableType::BuiltinNominal {
                name: Arc::from("StrBuf"),
                kind: Struct,
            })
        );
    }

    fn join_identity<'a>(
        file_id: u32,
        name: &'a str,
        kind: SemanticBodyDefinitionKind,
    ) -> DefinitionJoinIdentity<'a> {
        DefinitionJoinIdentity {
            file_id,
            name,
            kind,
            owner: None,
        }
    }

    fn semantic_identity(
        file_id: u32,
        name: &str,
        kind: SemanticBodyDefinitionKind,
    ) -> SemanticBodyDefinitionIdentity {
        SemanticBodyDefinitionIdentity {
            file_id,
            name: Arc::from(name),
            kind,
            owner: None,
        }
    }

    #[test]
    fn definition_join_index_returns_unique_definition_without_scanning() {
        let key = StableDefinitionKey::for_test(
            ModuleId::from_logical_path("main.rue").unwrap(),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
            "Large",
            None,
        );
        let index = DefinitionJoinIndex::from_entries([(
            join_identity(7, "Large", SemanticBodyDefinitionKind::Struct),
            &key,
        )]);
        let mut work = DurableBodyWork::default();

        assert_eq!(
            index.join(
                &semantic_identity(7, "Large", SemanticBodyDefinitionKind::Struct),
                &mut work,
            ),
            Ok(&key),
        );
        assert_eq!(work.stable_key_joins, 1);
    }

    #[test]
    fn definition_join_index_preserves_ambiguous_and_missing_failures() {
        let first = StableDefinitionKey::for_test(
            ModuleId::from_logical_path("main.rue").unwrap(),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
            "Duplicate",
            None,
        );
        let second = first.clone();
        let identity = join_identity(3, "Duplicate", SemanticBodyDefinitionKind::Struct);
        let index = DefinitionJoinIndex::from_entries([(identity, &first), (identity, &second)]);
        let mut work = DurableBodyWork::default();

        assert_eq!(
            index.join(
                &semantic_identity(3, "Duplicate", SemanticBodyDefinitionKind::Struct),
                &mut work,
            ),
            Err(DurableBodyConversionFailure::AmbiguousStableDefinition),
        );
        assert_eq!(
            index.join(
                &semantic_identity(3, "Missing", SemanticBodyDefinitionKind::Struct),
                &mut work,
            ),
            Err(DurableBodyConversionFailure::MissingStableDefinition),
        );
        assert_eq!(work.stable_key_joins, 2);
    }
}
