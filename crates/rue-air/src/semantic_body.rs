//! Structured, request-independent representation of one analyzed AIR body.
//!
//! The representation intentionally mirrors AIR operations, but never its packed
//! `extra` array or epoch-local type, symbol, nominal, place, or instruction IDs.

use std::sync::Arc;

use crate::{Air, ParamSlotModes};
use crate::{
    AirArgMode, AirPlaceBase, BodyOwnerToken, SemanticImportConstValue, SemanticImportType,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBodyDefinitionKind {
    FreeFunction,
    Method,
    AssociatedFunction,
    Destructor,
    Struct,
    Enum,
    ValueConst,
    ModuleBinding,
}

/// Request-independent textual identity used only until the compiler joins it
/// to its authoritative stable-definition universe.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticBodyDefinitionIdentity {
    pub file_id: u32,
    pub name: Arc<str>,
    pub kind: SemanticBodyDefinitionKind,
    pub owner: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticBodyModuleIdentity {
    pub file_id: u32,
    pub path: Arc<str>,
}

impl AsRef<str> for SemanticBodyModuleIdentity {
    fn as_ref(&self) -> &str {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyExport {
    pub owner: BodyOwnerToken,
    pub body: SemanticBody<SemanticBodyDefinitionIdentity, Arc<str>>,
}

/// Request-independent identity of one completed generic specialization.
///
/// The base and every nested nominal/function value are declaration identities;
/// no request-local symbol, type-pool, file, or AIR identifier crosses this seam.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticSpecializationIdentity<K, M> {
    pub base: K,
    pub type_arguments: Arc<[SemanticImportType<K, M>]>,
    pub value_arguments: Arc<[SemanticImportConstValue<K, M>]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticSpecializedBodyExport {
    pub identity: SemanticSpecializationIdentity<SemanticBodyDefinitionIdentity, Arc<str>>,
    pub body: SemanticBody<SemanticBodyDefinitionIdentity, Arc<str>>,
    pub dependencies: Arc<[SemanticBodyDefinitionIdentity]>,
    pub dependency_boundary_complete: bool,
}

#[derive(Debug)]
pub struct SemanticSpecializedBodyCandidate<K, IM, BM = IM> {
    pub identity: SemanticSpecializationIdentity<K, IM>,
    pub body_span: rue_span::Span,
    pub body: SemanticBody<K, BM>,
    pub dependencies: Arc<[K]>,
    pub dependency_boundary_complete: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticSpecializedCandidateInstallWork {
    pub attempts: usize,
    pub successes: usize,
    pub mapping_failures: usize,
}

impl<K, M> SemanticSpecializationIdentity<K, M> {
    pub fn try_map_keys<K2, M2, E>(
        &self,
        key: &impl Fn(&K) -> Result<K2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<SemanticSpecializationIdentity<K2, M2>, E> {
        fn ty<K, M, K2, M2, E>(
            value: &SemanticImportType<K, M>,
            key: &impl Fn(&K) -> Result<K2, E>,
            module: &impl Fn(&M) -> Result<M2, E>,
        ) -> Result<SemanticImportType<K2, M2>, E> {
            use SemanticImportType as T;
            Ok(match value {
                T::I8 => T::I8,
                T::I16 => T::I16,
                T::I32 => T::I32,
                T::I64 => T::I64,
                T::U8 => T::U8,
                T::U16 => T::U16,
                T::U32 => T::U32,
                T::U64 => T::U64,
                T::Bool => T::Bool,
                T::Unit => T::Unit,
                T::Never => T::Never,
                T::ComptimeType => T::ComptimeType,
                T::BuiltinNominal { name, kind } => T::BuiltinNominal {
                    name: name.clone(),
                    kind: *kind,
                },
                T::Nominal(value) => T::Nominal(key(value)?),
                T::Array { element, len } => T::Array {
                    element: Box::new(ty(element, key, module)?),
                    len: *len,
                },
                T::PtrConst(value) => T::PtrConst(Box::new(ty(value, key, module)?)),
                T::PtrMut(value) => T::PtrMut(Box::new(ty(value, key, module)?)),
                T::Module(value) => T::Module(module(value)?),
                T::GenericParameter(index) => T::GenericParameter(*index),
            })
        }
        fn value<K, M, K2, M2, E>(
            value: &SemanticImportConstValue<K, M>,
            key: &impl Fn(&K) -> Result<K2, E>,
            module: &impl Fn(&M) -> Result<M2, E>,
        ) -> Result<SemanticImportConstValue<K2, M2>, E> {
            Ok(match value {
                SemanticImportConstValue::Integer(value) => {
                    SemanticImportConstValue::Integer(*value)
                }
                SemanticImportConstValue::Bool(value) => SemanticImportConstValue::Bool(*value),
                SemanticImportConstValue::Type(value) => {
                    SemanticImportConstValue::Type(ty(value, key, module)?)
                }
                SemanticImportConstValue::Function(value) => {
                    SemanticImportConstValue::Function(key(value)?)
                }
                SemanticImportConstValue::Unit => SemanticImportConstValue::Unit,
            })
        }
        Ok(SemanticSpecializationIdentity {
            base: key(&self.base)?,
            type_arguments: self
                .type_arguments
                .iter()
                .map(|value| ty(value, key, module))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
            value_arguments: self
                .value_arguments
                .iter()
                .map(|item| value(item, key, module))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBodyExportFailure {
    UnsupportedGenericCall,
    UnmappedFunction,
    UnmappedNominal,
    AnonymousNominal,
    UnmappedModule,
    UnsupportedType,
    InvalidInstructionReference,
    InvalidPlaceReference,
    InvalidStringReference,
    ForeignSpan,
    UnsupportedWarningMetadata,
    SizeOverflow,
}

pub type SemanticBodyRef = u32;
pub type SemanticBodyPlaceRef = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticBodyAnchor {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticBodyPattern<K> {
    Wildcard,
    Int(i64),
    Bool(bool),
    EnumVariant { enum_key: K, variant_index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyMatchArm<K> {
    pub pattern: SemanticBodyPattern<K>,
    pub body: SemanticBodyRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyCallArg {
    pub value: SemanticBodyRef,
    pub mode: AirArgMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticBodyProjection<K, M> {
    Field {
        struct_key: K,
        field_index: u32,
    },
    Index {
        array_type: SemanticImportType<K, M>,
        index: SemanticBodyRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyPlace<K, M> {
    pub base: AirPlaceBase,
    pub base_type: SemanticImportType<K, M>,
    pub projections: Arc<[SemanticBodyProjection<K, M>]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyInst<K, M> {
    pub data: SemanticBodyInstData<K, M>,
    pub ty: SemanticImportType<K, M>,
    pub anchor: SemanticBodyAnchor,
}

/// Structured equivalent of every AIR instruction which can occur in an
/// ordinary, non-generic body. `CallGeneric` is represented explicitly so the
/// importer can reject it instead of silently accepting a pre-specialization
/// artifact with unresolved comptime values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticBodyInstData<K, M> {
    Const(u64),
    BoolConst(bool),
    StringConst(u32),
    UnitConst,
    TypeConst(SemanticImportType<K, M>),
    Add(SemanticBodyRef, SemanticBodyRef),
    Sub(SemanticBodyRef, SemanticBodyRef),
    Mul(SemanticBodyRef, SemanticBodyRef),
    Div(SemanticBodyRef, SemanticBodyRef),
    Mod(SemanticBodyRef, SemanticBodyRef),
    Eq(SemanticBodyRef, SemanticBodyRef),
    Ne(SemanticBodyRef, SemanticBodyRef),
    Lt(SemanticBodyRef, SemanticBodyRef),
    Gt(SemanticBodyRef, SemanticBodyRef),
    Le(SemanticBodyRef, SemanticBodyRef),
    Ge(SemanticBodyRef, SemanticBodyRef),
    And(SemanticBodyRef, SemanticBodyRef),
    Or(SemanticBodyRef, SemanticBodyRef),
    BitAnd(SemanticBodyRef, SemanticBodyRef),
    BitOr(SemanticBodyRef, SemanticBodyRef),
    BitXor(SemanticBodyRef, SemanticBodyRef),
    Shl(SemanticBodyRef, SemanticBodyRef),
    Shr(SemanticBodyRef, SemanticBodyRef),
    Neg(SemanticBodyRef),
    Not(SemanticBodyRef),
    BitNot(SemanticBodyRef),
    Branch {
        cond: SemanticBodyRef,
        then_value: SemanticBodyRef,
        else_value: Option<SemanticBodyRef>,
    },
    Loop {
        cond: SemanticBodyRef,
        body: SemanticBodyRef,
    },
    InfiniteLoop {
        body: SemanticBodyRef,
    },
    Match {
        scrutinee: SemanticBodyRef,
        arms: Arc<[SemanticBodyMatchArm<K>]>,
    },
    Break,
    Continue,
    Alloc {
        slot: u32,
        init: SemanticBodyRef,
    },
    Load {
        slot: u32,
    },
    Store {
        slot: u32,
        value: SemanticBodyRef,
    },
    ParamStore {
        param_slot: u32,
        value: SemanticBodyRef,
    },
    Ret(Option<SemanticBodyRef>),
    Call {
        function: K,
        args: Arc<[SemanticBodyCallArg]>,
    },
    RuntimeCall {
        runtime: crate::RuntimeCallKind,
        args: Arc<[SemanticBodyCallArg]>,
    },
    /// A call to another concrete specialization. Its stable generic origin
    /// and ordered canonical arguments replace the request-local mangled name.
    CallSpecialized {
        identity: SemanticSpecializationIdentity<K, M>,
        args: Arc<[SemanticBodyCallArg]>,
    },
    CallGeneric,
    Intrinsic {
        runtime: Option<crate::RuntimeCallKind>,
        name: Arc<str>,
        args: Arc<[SemanticBodyCallArg]>,
    },
    Param {
        index: u32,
    },
    Block {
        statements: Arc<[SemanticBodyRef]>,
        value: SemanticBodyRef,
    },
    StructInit {
        struct_key: K,
        fields: Arc<[SemanticBodyRef]>,
        source_order: Arc<[u32]>,
    },
    ArrayInit {
        elements: Arc<[SemanticBodyRef]>,
    },
    PlaceRead {
        place: SemanticBodyPlaceRef,
    },
    PlaceWrite {
        place: SemanticBodyPlaceRef,
        value: SemanticBodyRef,
    },
    EnumVariant {
        enum_key: K,
        variant_index: u32,
        payload: Arc<[SemanticBodyRef]>,
    },
    EnumPayloadGet {
        base: SemanticBodyRef,
        enum_key: K,
        variant_index: u32,
        field_index: u32,
    },
    IntCast {
        value: SemanticBodyRef,
        from_ty: SemanticImportType<K, M>,
    },
    Drop {
        value: SemanticBodyRef,
    },
    StorageLive {
        slot: u32,
    },
    StorageDead {
        slot: u32,
    },
    MarkMoved {
        value: SemanticBodyRef,
        slot: u32,
        is_param: bool,
        place: Option<SemanticBodyPlaceRef>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyWarning {
    pub code: Arc<str>,
    pub message: Arc<str>,
    pub anchor: SemanticBodyAnchor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBody<K, M> {
    pub return_type: SemanticImportType<K, M>,
    pub instructions: Arc<[SemanticBodyInst<K, M>]>,
    pub places: Arc<[SemanticBodyPlace<K, M>]>,
    pub strings: Arc<[Arc<str>]>,
    pub param_drops: Arc<[(u32, SemanticImportType<K, M>)]>,
    pub borrow_slots: Arc<[u32]>,
    pub num_locals: u32,
    pub num_param_slots: u32,
    pub param_by_ref: Arc<[bool]>,
    pub param_writable: Arc<[bool]>,
    pub allow_unreachable_code: bool,
    pub warnings: Arc<[SemanticBodyWarning]>,
}

impl<K, M> SemanticBody<K, M> {
    /// Replace request-independent definition and module keys without creating
    /// any epoch-local AIR values.
    pub fn try_map_keys<K2, M2, E>(
        &self,
        key: &impl Fn(&K) -> Result<K2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<SemanticBody<K2, M2>, E> {
        fn ty<K, M, K2, M2, E>(
            value: &SemanticImportType<K, M>,
            key: &impl Fn(&K) -> Result<K2, E>,
            module: &impl Fn(&M) -> Result<M2, E>,
        ) -> Result<SemanticImportType<K2, M2>, E> {
            use SemanticImportType as T;
            Ok(match value {
                T::I8 => T::I8,
                T::I16 => T::I16,
                T::I32 => T::I32,
                T::I64 => T::I64,
                T::U8 => T::U8,
                T::U16 => T::U16,
                T::U32 => T::U32,
                T::U64 => T::U64,
                T::Bool => T::Bool,
                T::Unit => T::Unit,
                T::Never => T::Never,
                T::ComptimeType => T::ComptimeType,
                T::BuiltinNominal { name, kind } => T::BuiltinNominal {
                    name: name.clone(),
                    kind: *kind,
                },
                T::Nominal(value) => T::Nominal(key(value)?),
                T::Array { element, len } => T::Array {
                    element: Box::new(ty(element, key, module)?),
                    len: *len,
                },
                T::PtrConst(value) => T::PtrConst(Box::new(ty(value, key, module)?)),
                T::PtrMut(value) => T::PtrMut(Box::new(ty(value, key, module)?)),
                T::Module(value) => T::Module(module(value)?),
                T::GenericParameter(index) => T::GenericParameter(*index),
            })
        }

        fn pattern<K, K2, E>(
            value: &SemanticBodyPattern<K>,
            key: &impl Fn(&K) -> Result<K2, E>,
        ) -> Result<SemanticBodyPattern<K2>, E> {
            Ok(match value {
                SemanticBodyPattern::Wildcard => SemanticBodyPattern::Wildcard,
                SemanticBodyPattern::Int(value) => SemanticBodyPattern::Int(*value),
                SemanticBodyPattern::Bool(value) => SemanticBodyPattern::Bool(*value),
                SemanticBodyPattern::EnumVariant {
                    enum_key,
                    variant_index,
                } => SemanticBodyPattern::EnumVariant {
                    enum_key: key(enum_key)?,
                    variant_index: *variant_index,
                },
            })
        }

        let places = self
            .places
            .iter()
            .map(|place| {
                Ok(SemanticBodyPlace {
                    base: place.base,
                    base_type: ty(&place.base_type, key, module)?,
                    projections: place
                        .projections
                        .iter()
                        .map(|projection| {
                            Ok(match projection {
                                SemanticBodyProjection::Field {
                                    struct_key,
                                    field_index,
                                } => SemanticBodyProjection::Field {
                                    struct_key: key(struct_key)?,
                                    field_index: *field_index,
                                },
                                SemanticBodyProjection::Index { array_type, index } => {
                                    SemanticBodyProjection::Index {
                                        array_type: ty(array_type, key, module)?,
                                        index: *index,
                                    }
                                }
                            })
                        })
                        .collect::<Result<Vec<_>, E>>()?
                        .into(),
                })
            })
            .collect::<Result<Vec<_>, E>>()?;

        use SemanticBodyInstData as D;
        let instructions = self
            .instructions
            .iter()
            .map(|inst| {
                let data = match &inst.data {
                    D::Const(v) => D::Const(*v),
                    D::BoolConst(v) => D::BoolConst(*v),
                    D::StringConst(v) => D::StringConst(*v),
                    D::UnitConst => D::UnitConst,
                    D::TypeConst(v) => D::TypeConst(ty(v, key, module)?),
                    D::Add(a, b) => D::Add(*a, *b),
                    D::Sub(a, b) => D::Sub(*a, *b),
                    D::Mul(a, b) => D::Mul(*a, *b),
                    D::Div(a, b) => D::Div(*a, *b),
                    D::Mod(a, b) => D::Mod(*a, *b),
                    D::Eq(a, b) => D::Eq(*a, *b),
                    D::Ne(a, b) => D::Ne(*a, *b),
                    D::Lt(a, b) => D::Lt(*a, *b),
                    D::Gt(a, b) => D::Gt(*a, *b),
                    D::Le(a, b) => D::Le(*a, *b),
                    D::Ge(a, b) => D::Ge(*a, *b),
                    D::And(a, b) => D::And(*a, *b),
                    D::Or(a, b) => D::Or(*a, *b),
                    D::BitAnd(a, b) => D::BitAnd(*a, *b),
                    D::BitOr(a, b) => D::BitOr(*a, *b),
                    D::BitXor(a, b) => D::BitXor(*a, *b),
                    D::Shl(a, b) => D::Shl(*a, *b),
                    D::Shr(a, b) => D::Shr(*a, *b),
                    D::Neg(v) => D::Neg(*v),
                    D::Not(v) => D::Not(*v),
                    D::BitNot(v) => D::BitNot(*v),
                    D::Branch {
                        cond,
                        then_value,
                        else_value,
                    } => D::Branch {
                        cond: *cond,
                        then_value: *then_value,
                        else_value: *else_value,
                    },
                    D::Loop { cond, body } => D::Loop {
                        cond: *cond,
                        body: *body,
                    },
                    D::InfiniteLoop { body } => D::InfiniteLoop { body: *body },
                    D::Match { scrutinee, arms } => D::Match {
                        scrutinee: *scrutinee,
                        arms: arms
                            .iter()
                            .map(|arm| {
                                Ok(SemanticBodyMatchArm {
                                    pattern: pattern(&arm.pattern, key)?,
                                    body: arm.body,
                                })
                            })
                            .collect::<Result<Vec<_>, E>>()?
                            .into(),
                    },
                    D::Break => D::Break,
                    D::Continue => D::Continue,
                    D::Alloc { slot, init } => D::Alloc {
                        slot: *slot,
                        init: *init,
                    },
                    D::Load { slot } => D::Load { slot: *slot },
                    D::Store { slot, value } => D::Store {
                        slot: *slot,
                        value: *value,
                    },
                    D::ParamStore { param_slot, value } => D::ParamStore {
                        param_slot: *param_slot,
                        value: *value,
                    },
                    D::Ret(v) => D::Ret(*v),
                    D::Call { function, args } => D::Call {
                        function: key(function)?,
                        args: args.clone(),
                    },
                    D::RuntimeCall { runtime, args } => D::RuntimeCall {
                        runtime: *runtime,
                        args: args.clone(),
                    },
                    D::CallSpecialized { identity, args } => D::CallSpecialized {
                        identity: identity.try_map_keys(key, module)?,
                        args: args.clone(),
                    },
                    D::CallGeneric => D::CallGeneric,
                    D::Intrinsic {
                        runtime,
                        name,
                        args,
                    } => D::Intrinsic {
                        runtime: *runtime,
                        name: name.clone(),
                        args: args.clone(),
                    },
                    D::Param { index } => D::Param { index: *index },
                    D::Block { statements, value } => D::Block {
                        statements: statements.clone(),
                        value: *value,
                    },
                    D::StructInit {
                        struct_key,
                        fields,
                        source_order,
                    } => D::StructInit {
                        struct_key: key(struct_key)?,
                        fields: fields.clone(),
                        source_order: source_order.clone(),
                    },
                    D::ArrayInit { elements } => D::ArrayInit {
                        elements: elements.clone(),
                    },
                    D::PlaceRead { place } => D::PlaceRead { place: *place },
                    D::PlaceWrite { place, value } => D::PlaceWrite {
                        place: *place,
                        value: *value,
                    },
                    D::EnumVariant {
                        enum_key,
                        variant_index,
                        payload,
                    } => D::EnumVariant {
                        enum_key: key(enum_key)?,
                        variant_index: *variant_index,
                        payload: payload.clone(),
                    },
                    D::EnumPayloadGet {
                        base,
                        enum_key,
                        variant_index,
                        field_index,
                    } => D::EnumPayloadGet {
                        base: *base,
                        enum_key: key(enum_key)?,
                        variant_index: *variant_index,
                        field_index: *field_index,
                    },
                    D::IntCast { value, from_ty } => D::IntCast {
                        value: *value,
                        from_ty: ty(from_ty, key, module)?,
                    },
                    D::Drop { value } => D::Drop { value: *value },
                    D::StorageLive { slot } => D::StorageLive { slot: *slot },
                    D::StorageDead { slot } => D::StorageDead { slot: *slot },
                    D::MarkMoved {
                        value,
                        slot,
                        is_param,
                        place,
                    } => D::MarkMoved {
                        value: *value,
                        slot: *slot,
                        is_param: *is_param,
                        place: *place,
                    },
                };
                Ok(SemanticBodyInst {
                    data,
                    ty: ty(&inst.ty, key, module)?,
                    anchor: inst.anchor,
                })
            })
            .collect::<Result<Vec<_>, E>>()?;
        Ok(SemanticBody {
            return_type: ty(&self.return_type, key, module)?,
            instructions: instructions.into(),
            places: places.into(),
            strings: self.strings.clone(),
            param_drops: self
                .param_drops
                .iter()
                .map(|(slot, value)| Ok((*slot, ty(value, key, module)?)))
                .collect::<Result<Vec<_>, E>>()?
                .into(),
            borrow_slots: self.borrow_slots.clone(),
            num_locals: self.num_locals,
            num_param_slots: self.num_param_slots,
            param_by_ref: self.param_by_ref.clone(),
            param_writable: self.param_writable.clone(),
            allow_unreachable_code: self.allow_unreachable_code,
            warnings: self.warnings.clone(),
        })
    }
}

#[derive(Debug)]
pub struct SemanticImportedBody {
    pub air: Air,
    pub strings: Vec<String>,
    pub num_locals: u32,
    pub num_param_slots: u32,
    pub param_modes: ParamSlotModes,
    pub allow_unreachable_code: bool,
    pub warnings: Arc<[SemanticBodyWarning]>,
}

#[derive(Debug)]
pub struct SemanticBodyCandidate<K, M> {
    pub owner: crate::BodyOwnerToken,
    pub body_span: rue_span::Span,
    pub body: SemanticBody<K, M>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticBodyCandidateInstallWork {
    pub attempts: usize,
    pub successes: usize,
    pub failures: usize,
    pub instructions_installed: usize,
    pub places_installed: usize,
    pub strings_installed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBodyImportFailure {
    Semantic(super::SemanticImportFailure),
    UnsupportedGenericCall,
    InvalidInstructionReference,
    ForwardInstructionReference,
    InvalidPlaceReference,
    InvalidStringReference,
    InvalidSourceOrder,
    InvalidParameterModes,
    InvalidParameterDrop,
    InvalidBorrowSlot,
    InvalidAnchor,
    WrongNominalKind,
    SizeOverflow,
}

impl From<super::SemanticImportFailure> for SemanticBodyImportFailure {
    fn from(value: super::SemanticImportFailure) -> Self {
        Self::Semantic(value)
    }
}
