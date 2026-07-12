//! Structured, request-independent representation of one analyzed AIR body.
//!
//! The representation intentionally mirrors AIR operations, but never its packed
//! `extra` array or epoch-local type, symbol, nominal, place, or instruction IDs.

use std::sync::Arc;

use crate::{Air, ParamSlotModes};
use crate::{AirArgMode, AirPlaceBase, BodyOwnerToken, SemanticImportType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBodyDefinitionKind {
    FreeFunction,
    Method,
    AssociatedFunction,
    Destructor,
    Struct,
    Enum,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyExport {
    pub owner: BodyOwnerToken,
    pub body: SemanticBody<SemanticBodyDefinitionIdentity, Arc<str>>,
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
    CallGeneric,
    Intrinsic {
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
    FieldGet {
        base: SemanticBodyRef,
        struct_key: K,
        field_index: u32,
    },
    FieldSet {
        slot: u32,
        struct_key: K,
        field_index: u32,
        value: SemanticBodyRef,
    },
    ParamFieldSet {
        param_slot: u32,
        inner_offset: u32,
        struct_key: K,
        field_index: u32,
        value: SemanticBodyRef,
    },
    ArrayInit {
        elements: Arc<[SemanticBodyRef]>,
    },
    IndexGet {
        base: SemanticBodyRef,
        array_type: SemanticImportType<K, M>,
        index: SemanticBodyRef,
    },
    IndexSet {
        slot: u32,
        array_type: SemanticImportType<K, M>,
        index: SemanticBodyRef,
        value: SemanticBodyRef,
    },
    ParamIndexSet {
        param_slot: u32,
        array_type: SemanticImportType<K, M>,
        index: SemanticBodyRef,
        value: SemanticBodyRef,
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
