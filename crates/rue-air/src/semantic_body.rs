//! Structured, request-independent representation of one analyzed AIR body.
//!
//! The representation intentionally mirrors AIR operations, but never its packed
//! `extra` array or epoch-local type, symbol, nominal, place, or instruction IDs.

use crate::Node;
use std::sync::Arc;

use crate::ParamSlotModes;
use crate::{
    AirArgMode, AirPlaceBase, BodyOwnerToken, FunctionInstanceKey, NominalInstanceKey,
    SemanticImportConstValue, SemanticImportFailure, SemanticImportType,
};

/// Issuer-scoped snapshot authorization for one stable definition.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticDefinitionToken {
    issuer: u64,
    slot: u32,
}

impl std::fmt::Debug for SemanticDefinitionToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SemanticDefinitionToken")
            .field(&self.slot)
            .finish()
    }
}

impl SemanticDefinitionToken {
    pub fn new(issuer: u64, slot: u32) -> Self {
        Self { issuer, slot }
    }
    pub fn issuer(self) -> u64 {
        self.issuer
    }
    pub fn slot(self) -> u32 {
        self.slot
    }
}

/// Issuer-scoped snapshot authorization for one logical module.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticModuleToken {
    issuer: u64,
    slot: u32,
}

impl std::fmt::Debug for SemanticModuleToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SemanticModuleToken")
            .field(&self.slot)
            .finish()
    }
}

impl SemanticModuleToken {
    pub fn new(issuer: u64, slot: u32) -> Self {
        Self { issuer, slot }
    }
    pub fn issuer(self) -> u64 {
        self.issuer
    }
    pub fn slot(self) -> u32 {
        self.slot
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticDefinitionEndpoint {
    pub token: SemanticDefinitionToken,
    pub file: u32,
    pub name: Arc<str>,
    pub kind: crate::StableDefinitionKind,
    pub owner: Option<Arc<str>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticModuleEndpoint {
    pub token: SemanticModuleToken,
    pub file: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticStableResolutionFailure {
    Missing,
    Ambiguous,
    WrongKind,
    ForeignIssuer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyExport {
    pub owner: BodyOwnerToken,
    pub body: SemanticBody<SemanticDefinitionToken, SemanticModuleToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticAnonymousBodyExport {
    pub identity: FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    pub body: SemanticBody<SemanticDefinitionToken, SemanticModuleToken>,
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
    pub identity: SemanticSpecializationIdentity<SemanticDefinitionToken, SemanticModuleToken>,
    pub body: SemanticBody<SemanticDefinitionToken, SemanticModuleToken>,
    pub dependencies: Arc<[SemanticDefinitionToken]>,
    pub dependency_boundary_complete: bool,
}

#[derive(Debug, Clone)]
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
    pub fn try_map_keys<K2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        key: &impl Fn(&K) -> Result<K2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<SemanticSpecializationIdentity<K2, M2>, E> {
        Ok(SemanticSpecializationIdentity {
            base: key(&self.base)?,
            type_arguments: self
                .type_arguments
                .iter()
                .map(|value| value.try_map_identities(key, module))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
            value_arguments: self
                .value_arguments
                .iter()
                .map(|item| item.try_map_identities(key, module))
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
    MissingStableIdentity,
    AmbiguousStableIdentity,
    WrongStableIdentityKind,
    ForeignStableIdentityIssuer,
}

pub type SemanticBodyRef = u32;
pub type SemanticBodyPlaceRef = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticBodyAnchor {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticBodyPattern<K, M> {
    Wildcard,
    Int(i64),
    Bool(bool),
    EnumVariant {
        enum_key: NominalInstanceKey<K, M>,
        variant_index: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyMatchArm<K, M> {
    pub pattern: SemanticBodyPattern<K, M>,
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
        struct_key: NominalInstanceKey<K, M>,
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
    WrappingAdd(SemanticBodyRef, SemanticBodyRef),
    WrappingSub(SemanticBodyRef, SemanticBodyRef),
    WrappingMul(SemanticBodyRef, SemanticBodyRef),
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
        arms: Arc<[SemanticBodyMatchArm<K, M>]>,
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
        function: FunctionInstanceKey<K, M>,
        args: Arc<[SemanticBodyCallArg]>,
    },
    AccessorCall {
        function: FunctionInstanceKey<K, M>,
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
        struct_key: NominalInstanceKey<K, M>,
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
        enum_key: NominalInstanceKey<K, M>,
        variant_index: u32,
        payload: Arc<[SemanticBodyRef]>,
    },
    EnumPayloadGet {
        base: SemanticBodyRef,
        enum_key: NominalInstanceKey<K, M>,
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

// Keep the canonical instruction's stable tag and diagnostic name in one
// deliberately boring table. The generated `kind` match has no wildcard: a
// newly added `SemanticBodyInstData` variant fails compilation here until its
// schema metadata is supplied. Layout-specific export/import semantics remain
// ordinary Rust matches at their respective boundaries.
macro_rules! semantic_body_inst_schema {
    ($consumer:ident) => {
        $consumer! {
            Const, SemanticBodyInstData::Const(..), 0, "const";
            BoolConst, SemanticBodyInstData::BoolConst(..), 1, "bool_const";
            StringConst, SemanticBodyInstData::StringConst(..), 2, "string_const";
            UnitConst, SemanticBodyInstData::UnitConst, 3, "unit_const";
            TypeConst, SemanticBodyInstData::TypeConst(..), 4, "type_const";
            Add, SemanticBodyInstData::Add(..), 5, "add";
            Sub, SemanticBodyInstData::Sub(..), 6, "sub";
            Mul, SemanticBodyInstData::Mul(..), 7, "mul";
            WrappingAdd, SemanticBodyInstData::WrappingAdd(..), 8, "wrapping_add";
            WrappingSub, SemanticBodyInstData::WrappingSub(..), 9, "wrapping_sub";
            WrappingMul, SemanticBodyInstData::WrappingMul(..), 10, "wrapping_mul";
            Div, SemanticBodyInstData::Div(..), 11, "div";
            Mod, SemanticBodyInstData::Mod(..), 12, "mod";
            Eq, SemanticBodyInstData::Eq(..), 13, "eq";
            Ne, SemanticBodyInstData::Ne(..), 14, "ne";
            Lt, SemanticBodyInstData::Lt(..), 15, "lt";
            Gt, SemanticBodyInstData::Gt(..), 16, "gt";
            Le, SemanticBodyInstData::Le(..), 17, "le";
            Ge, SemanticBodyInstData::Ge(..), 18, "ge";
            And, SemanticBodyInstData::And(..), 19, "and";
            Or, SemanticBodyInstData::Or(..), 20, "or";
            BitAnd, SemanticBodyInstData::BitAnd(..), 21, "bit_and";
            BitOr, SemanticBodyInstData::BitOr(..), 22, "bit_or";
            BitXor, SemanticBodyInstData::BitXor(..), 23, "bit_xor";
            Shl, SemanticBodyInstData::Shl(..), 24, "shl";
            Shr, SemanticBodyInstData::Shr(..), 25, "shr";
            Neg, SemanticBodyInstData::Neg(..), 26, "neg";
            Not, SemanticBodyInstData::Not(..), 27, "not";
            BitNot, SemanticBodyInstData::BitNot(..), 28, "bit_not";
            Branch, SemanticBodyInstData::Branch { .. }, 29, "branch";
            Loop, SemanticBodyInstData::Loop { .. }, 30, "loop";
            InfiniteLoop, SemanticBodyInstData::InfiniteLoop { .. }, 31, "infinite_loop";
            Match, SemanticBodyInstData::Match { .. }, 32, "match";
            Break, SemanticBodyInstData::Break, 33, "break";
            Continue, SemanticBodyInstData::Continue, 34, "continue";
            Alloc, SemanticBodyInstData::Alloc { .. }, 35, "alloc";
            Load, SemanticBodyInstData::Load { .. }, 36, "load";
            Store, SemanticBodyInstData::Store { .. }, 37, "store";
            ParamStore, SemanticBodyInstData::ParamStore { .. }, 38, "param_store";
            Ret, SemanticBodyInstData::Ret(..), 39, "ret";
            Call, SemanticBodyInstData::Call { .. }, 40, "call";
            RuntimeCall, SemanticBodyInstData::RuntimeCall { .. }, 41, "runtime_call";
            CallSpecialized, SemanticBodyInstData::CallSpecialized { .. }, 42, "call_specialized";
            CallGeneric, SemanticBodyInstData::CallGeneric, 43, "call_generic";
            Intrinsic, SemanticBodyInstData::Intrinsic { .. }, 44, "intrinsic";
            Param, SemanticBodyInstData::Param { .. }, 45, "param";
            Block, SemanticBodyInstData::Block { .. }, 46, "block";
            StructInit, SemanticBodyInstData::StructInit { .. }, 47, "struct_init";
            ArrayInit, SemanticBodyInstData::ArrayInit { .. }, 48, "array_init";
            PlaceRead, SemanticBodyInstData::PlaceRead { .. }, 49, "place_read";
            PlaceWrite, SemanticBodyInstData::PlaceWrite { .. }, 50, "place_write";
            EnumVariant, SemanticBodyInstData::EnumVariant { .. }, 51, "enum_variant";
            EnumPayloadGet, SemanticBodyInstData::EnumPayloadGet { .. }, 52, "enum_payload_get";
            IntCast, SemanticBodyInstData::IntCast { .. }, 53, "int_cast";
            Drop, SemanticBodyInstData::Drop { .. }, 54, "drop";
            StorageLive, SemanticBodyInstData::StorageLive { .. }, 55, "storage_live";
            StorageDead, SemanticBodyInstData::StorageDead { .. }, 56, "storage_dead";
            MarkMoved, SemanticBodyInstData::MarkMoved { .. }, 57, "mark_moved";
            AccessorCall, SemanticBodyInstData::AccessorCall { .. }, 58, "accessor_call";
        }
    };
}

macro_rules! define_semantic_body_inst_kind {
    ($( $kind:ident, $pattern:pat, $tag:literal, $name:literal; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u16)]
        pub enum SemanticBodyInstKind {
            $( $kind = $tag, )*
        }

        pub const SEMANTIC_BODY_INST_KINDS: &[SemanticBodyInstKind] = &[
            $( SemanticBodyInstKind::$kind, )*
        ];

        impl SemanticBodyInstKind {
            pub const fn schema_tag(self) -> u16 {
                self as u16
            }

            pub const fn display_name(self) -> &'static str {
                match self {
                    $( Self::$kind => $name, )*
                }
            }
        }

        impl std::fmt::Display for SemanticBodyInstKind {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.display_name())
            }
        }

        impl<K, M> SemanticBodyInstData<K, M> {
            pub const fn kind(&self) -> SemanticBodyInstKind {
                match self {
                    $( $pattern => SemanticBodyInstKind::$kind, )*
                }
            }
        }
    };
}

semantic_body_inst_schema!(define_semantic_body_inst_kind);

/// Location and schema identity attached to a typed projection/import failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyInstFailureContext<E> {
    pub instruction: u32,
    pub kind: SemanticBodyInstKind,
    pub failure: E,
}

impl<E> SemanticBodyInstFailureContext<E> {
    pub fn new<K, M>(instruction: u32, data: &SemanticBodyInstData<K, M>, failure: E) -> Self {
        Self {
            instruction,
            kind: data.kind(),
            failure,
        }
    }
}

impl<E: std::fmt::Display> std::fmt::Display for SemanticBodyInstFailureContext<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "instruction {} ({}, schema tag {}): {}",
            self.instruction,
            self.kind,
            self.kind.schema_tag(),
            self.failure
        )
    }
}

/// Borrowed dependency exposed by the canonical instruction schema. Traversal
/// never clones keys, types, argument arrays, or specialization identities.
#[derive(Debug, Clone, Copy)]
pub enum SemanticBodyInstDependency<'a, K, M> {
    Instruction(SemanticBodyRef),
    Place(SemanticBodyPlaceRef),
    Definition(&'a K),
    Nominal(&'a NominalInstanceKey<K, M>),
    Function(&'a FunctionInstanceKey<K, M>),
    Type(&'a SemanticImportType<K, M>),
    String(SemanticBodyRef),
}

impl<K, M> SemanticBodyInstData<K, M> {
    /// Project definition and module identities without cloning unchanged
    /// dependency arrays. The exhaustive match is intentionally local to the
    /// canonical schema so new variants produce a focused compiler error.
    pub fn try_map_keys<K2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        key: &impl Fn(&K) -> Result<K2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<SemanticBodyInstData<K2, M2>, E> {
        fn pattern<K, M, K2: std::hash::Hash, M2: std::hash::Hash, E>(
            value: &SemanticBodyPattern<K, M>,
            key: &impl Fn(&K) -> Result<K2, E>,
            module: &impl Fn(&M) -> Result<M2, E>,
        ) -> Result<SemanticBodyPattern<K2, M2>, E> {
            Ok(match value {
                SemanticBodyPattern::Wildcard => SemanticBodyPattern::Wildcard,
                SemanticBodyPattern::Int(value) => SemanticBodyPattern::Int(*value),
                SemanticBodyPattern::Bool(value) => SemanticBodyPattern::Bool(*value),
                SemanticBodyPattern::EnumVariant {
                    enum_key,
                    variant_index,
                } => SemanticBodyPattern::EnumVariant {
                    enum_key: match enum_key {
                        NominalInstanceKey::Builtin { kind, name } => NominalInstanceKey::Builtin {
                            kind: *kind,
                            name: name.clone(),
                        },
                        NominalInstanceKey::Named(value) => NominalInstanceKey::Named(key(value)?),
                        NominalInstanceKey::Anonymous(value) => NominalInstanceKey::Anonymous(
                            Node::new(value.try_map_identities(key, module)?),
                        ),
                    },
                    variant_index: *variant_index,
                },
            })
        }

        use SemanticBodyInstData as D;
        Ok(match self {
            D::Const(v) => D::Const(*v),
            D::BoolConst(v) => D::BoolConst(*v),
            D::StringConst(v) => D::StringConst(*v),
            D::UnitConst => D::UnitConst,
            D::TypeConst(v) => D::TypeConst(v.try_map_identities(key, module)?),
            D::Add(a, b) => D::Add(*a, *b),
            D::Sub(a, b) => D::Sub(*a, *b),
            D::Mul(a, b) => D::Mul(*a, *b),
            D::WrappingAdd(a, b) => D::WrappingAdd(*a, *b),
            D::WrappingSub(a, b) => D::WrappingSub(*a, *b),
            D::WrappingMul(a, b) => D::WrappingMul(*a, *b),
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
                            pattern: pattern(&arm.pattern, key, module)?,
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
                function: function.try_map_identities(key, module)?,
                args: args.clone(),
            },
            D::AccessorCall { function, args } => D::AccessorCall {
                function: function.try_map_identities(key, module)?,
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
                struct_key: match struct_key {
                    NominalInstanceKey::Builtin { kind, name } => NominalInstanceKey::Builtin {
                        kind: *kind,
                        name: name.clone(),
                    },
                    NominalInstanceKey::Named(value) => NominalInstanceKey::Named(key(value)?),
                    NominalInstanceKey::Anonymous(value) => NominalInstanceKey::Anonymous(
                        Node::new(value.try_map_identities(key, module)?),
                    ),
                },
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
                enum_key: match enum_key {
                    NominalInstanceKey::Builtin { kind, name } => NominalInstanceKey::Builtin {
                        kind: *kind,
                        name: name.clone(),
                    },
                    NominalInstanceKey::Named(value) => NominalInstanceKey::Named(key(value)?),
                    NominalInstanceKey::Anonymous(value) => NominalInstanceKey::Anonymous(
                        Node::new(value.try_map_identities(key, module)?),
                    ),
                },
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
                enum_key: match enum_key {
                    NominalInstanceKey::Builtin { kind, name } => NominalInstanceKey::Builtin {
                        kind: *kind,
                        name: name.clone(),
                    },
                    NominalInstanceKey::Named(value) => NominalInstanceKey::Named(key(value)?),
                    NominalInstanceKey::Anonymous(value) => NominalInstanceKey::Anonymous(
                        Node::new(value.try_map_identities(key, module)?),
                    ),
                },
                variant_index: *variant_index,
                field_index: *field_index,
            },
            D::IntCast { value, from_ty } => D::IntCast {
                value: *value,
                from_ty: from_ty.try_map_identities(key, module)?,
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
        })
    }

    /// Visit every dependency carried directly by this instruction. Nested
    /// specialization types/values are exposed as their canonical roots; their
    /// own exhaustive visitors retain responsibility for recursive identities.
    pub fn visit_dependencies(
        &self,
        visitor: &mut impl FnMut(SemanticBodyInstDependency<'_, K, M>),
    ) {
        use SemanticBodyInstData as D;
        fn inst<K, M>(
            visitor: &mut impl FnMut(SemanticBodyInstDependency<'_, K, M>),
            value: SemanticBodyRef,
        ) {
            visitor(SemanticBodyInstDependency::Instruction(value));
        }
        fn args<K, M>(
            visitor: &mut impl FnMut(SemanticBodyInstDependency<'_, K, M>),
            values: &[SemanticBodyCallArg],
        ) {
            for value in values {
                inst(visitor, value.value);
            }
        }
        match self {
            D::Const(_)
            | D::BoolConst(_)
            | D::UnitConst
            | D::Break
            | D::Continue
            | D::Load { .. }
            | D::CallGeneric
            | D::Param { .. }
            | D::StorageLive { .. }
            | D::StorageDead { .. } => {}
            D::StringConst(value) => visitor(SemanticBodyInstDependency::String(*value)),
            D::TypeConst(value) => visitor(SemanticBodyInstDependency::Type(value)),
            D::Add(a, b)
            | D::Sub(a, b)
            | D::Mul(a, b)
            | D::WrappingAdd(a, b)
            | D::WrappingSub(a, b)
            | D::WrappingMul(a, b)
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
            | D::Shr(a, b) => {
                inst(visitor, *a);
                inst(visitor, *b);
            }
            D::Neg(value) | D::Not(value) | D::BitNot(value) | D::Drop { value } => {
                inst(visitor, *value)
            }
            D::Branch {
                cond,
                then_value,
                else_value,
            } => {
                inst(visitor, *cond);
                inst(visitor, *then_value);
                if let Some(value) = else_value {
                    inst(visitor, *value);
                }
            }
            D::Loop { cond, body } => {
                inst(visitor, *cond);
                inst(visitor, *body);
            }
            D::InfiniteLoop { body } => inst(visitor, *body),
            D::Match { scrutinee, arms } => {
                inst(visitor, *scrutinee);
                for arm in arms.iter() {
                    if let SemanticBodyPattern::EnumVariant { enum_key, .. } = &arm.pattern {
                        visitor(SemanticBodyInstDependency::Nominal(enum_key));
                    }
                    inst(visitor, arm.body);
                }
            }
            D::Alloc { init, .. } => inst(visitor, *init),
            D::Store { value, .. } | D::ParamStore { value, .. } => inst(visitor, *value),
            D::Ret(value) => {
                if let Some(value) = value {
                    inst(visitor, *value);
                }
            }
            D::Call {
                function,
                args: values,
            }
            | D::AccessorCall {
                function,
                args: values,
            } => {
                visitor(SemanticBodyInstDependency::Function(function));
                args(visitor, values);
            }
            D::RuntimeCall { args: values, .. } | D::Intrinsic { args: values, .. } => {
                args(visitor, values)
            }
            D::CallSpecialized {
                identity,
                args: values,
            } => {
                visitor(SemanticBodyInstDependency::Definition(&identity.base));
                for value in identity.type_arguments.iter() {
                    visitor(SemanticBodyInstDependency::Type(value));
                }
                for value in identity.value_arguments.iter() {
                    match value {
                        SemanticImportConstValue::Type(value) => {
                            visitor(SemanticBodyInstDependency::Type(value))
                        }
                        SemanticImportConstValue::Function(value) => {
                            visitor(SemanticBodyInstDependency::Definition(value))
                        }
                        _ => {}
                    }
                }
                args(visitor, values);
            }
            D::Block { statements, value } => {
                for statement in statements.iter() {
                    inst(visitor, *statement);
                }
                inst(visitor, *value);
            }
            D::StructInit {
                struct_key, fields, ..
            } => {
                visitor(SemanticBodyInstDependency::Nominal(struct_key));
                for field in fields.iter() {
                    inst(visitor, *field);
                }
            }
            D::ArrayInit { elements } => {
                for element in elements.iter() {
                    inst(visitor, *element);
                }
            }
            D::PlaceRead { place } => visitor(SemanticBodyInstDependency::Place(*place)),
            D::PlaceWrite { place, value } => {
                visitor(SemanticBodyInstDependency::Place(*place));
                inst(visitor, *value);
            }
            D::EnumVariant {
                enum_key, payload, ..
            } => {
                visitor(SemanticBodyInstDependency::Nominal(enum_key));
                for value in payload.iter() {
                    inst(visitor, *value);
                }
            }
            D::EnumPayloadGet { base, enum_key, .. } => {
                inst(visitor, *base);
                visitor(SemanticBodyInstDependency::Nominal(enum_key));
            }
            D::IntCast { value, from_ty } => {
                inst(visitor, *value);
                visitor(SemanticBodyInstDependency::Type(from_ty));
            }
            D::MarkMoved { value, place, .. } => {
                inst(visitor, *value);
                if let Some(place) = place {
                    visitor(SemanticBodyInstDependency::Place(*place));
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyWarning {
    pub kind: rue_error::WarningKind,
    pub anchor: SemanticBodyAnchor,
    pub labels: Arc<[SemanticBodyWarningLabel]>,
    pub notes: Arc<[Arc<str>]>,
    pub helps: Arc<[Arc<str>]>,
    pub suggestions: Arc<[SemanticBodyWarningSuggestion]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyWarningLabel {
    pub message: Arc<str>,
    pub anchor: SemanticBodyAnchor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyWarningSuggestion {
    pub message: Arc<str>,
    pub anchor: SemanticBodyAnchor,
    pub replacement: Arc<str>,
    pub applicability: rue_error::Applicability,
}

/// One `(receiver, method)` reference recorded when body analysis selected the
/// method winner and rendered its callable symbol for `Call` emission. The
/// recorded set is the reachability truth for imported bodies, so no consumer
/// ever reverses a rendered callable symbol back into a method key (RUE-1128).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyMethodReference<K, M> {
    pub receiver: NominalInstanceKey<K, M>,
    pub method: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBody<K, M> {
    /// Whether this body has the second-class, mandatory-splice accessor ABI.
    pub is_accessor: bool,
    pub return_type: SemanticImportType<K, M>,
    pub instructions: Arc<[SemanticBodyInst<K, M>]>,
    pub places: Arc<[SemanticBodyPlace<K, M>]>,
    pub strings: Arc<[Arc<str>]>,
    pub local_atoms: Arc<[crate::SemanticBodyLocalAtom<K, M>]>,
    pub param_drops: Arc<[(u32, SemanticImportType<K, M>)]>,
    pub borrow_slots: Arc<[u32]>,
    pub num_locals: u32,
    pub num_param_slots: u32,
    pub param_by_ref: Arc<[bool]>,
    pub param_writable: Arc<[bool]>,
    pub allow_unreachable_code: bool,
    pub warnings: Arc<[SemanticBodyWarning]>,
    /// Method references recorded at resolution time, ordered by the
    /// content-canonical `(receiver symbol, method name)` render so equal
    /// recorded sets serialize identically across sessions and revisions.
    pub method_references: Arc<[SemanticBodyMethodReference<K, M>]>,
}

impl<K, M> SemanticBody<K, M> {
    /// Replace request-independent definition and module keys without creating
    /// any epoch-local AIR values.
    pub fn try_map_keys<K2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        key: &impl Fn(&K) -> Result<K2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<SemanticBody<K2, M2>, E> {
        let places = self
            .places
            .iter()
            .map(|place| {
                Ok(SemanticBodyPlace {
                    base: place.base,
                    base_type: place.base_type.try_map_identities(key, module)?,
                    projections: place
                        .projections
                        .iter()
                        .map(|projection| {
                            Ok(match projection {
                                SemanticBodyProjection::Field {
                                    struct_key,
                                    field_index,
                                } => SemanticBodyProjection::Field {
                                    struct_key: match struct_key {
                                        NominalInstanceKey::Builtin { kind, name } => {
                                            NominalInstanceKey::Builtin {
                                                kind: *kind,
                                                name: name.clone(),
                                            }
                                        }
                                        NominalInstanceKey::Named(value) => {
                                            NominalInstanceKey::Named(key(value)?)
                                        }
                                        NominalInstanceKey::Anonymous(value) => {
                                            NominalInstanceKey::Anonymous(Node::new(
                                                value.try_map_identities(key, module)?,
                                            ))
                                        }
                                    },
                                    field_index: *field_index,
                                },
                                SemanticBodyProjection::Index { array_type, index } => {
                                    SemanticBodyProjection::Index {
                                        array_type: array_type.try_map_identities(key, module)?,
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

        let instructions = self
            .instructions
            .iter()
            .map(|inst| {
                let data = inst.data.try_map_keys(key, module)?;
                Ok(SemanticBodyInst {
                    data,
                    ty: inst.ty.try_map_identities(key, module)?,
                    anchor: inst.anchor,
                })
            })
            .collect::<Result<Vec<_>, E>>()?;
        Ok(SemanticBody {
            is_accessor: self.is_accessor,
            return_type: self.return_type.try_map_identities(key, module)?,
            instructions: instructions.into(),
            places: places.into(),
            strings: self.strings.clone(),
            local_atoms: self
                .local_atoms
                .iter()
                .map(|atom| {
                    Ok(crate::SemanticBodyLocalAtom {
                        identity: atom.identity.try_map_identities(key, module)?,
                        content: atom.content.clone(),
                    })
                })
                .collect::<Result<Vec<_>, E>>()?
                .into(),
            param_drops: self
                .param_drops
                .iter()
                .map(|(slot, value)| Ok((*slot, value.try_map_identities(key, module)?)))
                .collect::<Result<Vec<_>, E>>()?
                .into(),
            borrow_slots: self.borrow_slots.clone(),
            num_locals: self.num_locals,
            num_param_slots: self.num_param_slots,
            param_by_ref: self.param_by_ref.clone(),
            param_writable: self.param_writable.clone(),
            allow_unreachable_code: self.allow_unreachable_code,
            warnings: self.warnings.clone(),
            method_references: self
                .method_references
                .iter()
                .map(|reference| {
                    Ok(SemanticBodyMethodReference {
                        receiver: match &reference.receiver {
                            NominalInstanceKey::Builtin { kind, name } => {
                                NominalInstanceKey::Builtin {
                                    kind: *kind,
                                    name: name.clone(),
                                }
                            }
                            NominalInstanceKey::Named(value) => {
                                NominalInstanceKey::Named(key(value)?)
                            }
                            NominalInstanceKey::Anonymous(value) => NominalInstanceKey::Anonymous(
                                Node::new(value.try_map_identities(key, module)?),
                            ),
                        },
                        method: reference.method.clone(),
                    })
                })
                .collect::<Result<Vec<_>, E>>()?
                .into(),
        })
    }
}

#[derive(Debug)]
pub struct SemanticImportedBody<K, M> {
    pub air: crate::ValidatedAir,
    pub strings: Vec<String>,
    pub local_atoms: Vec<crate::LocalAtomRecord<K, M>>,
    pub num_locals: u32,
    pub num_param_slots: u32,
    pub param_modes: ParamSlotModes,
    pub allow_unreachable_code: bool,
    pub warnings: Arc<[rue_error::CompileWarning]>,
    /// The body's recorded method references projected into the importing
    /// session's live `(receiver, method)` keys. Import hosts that resolve
    /// nominals (the staged-body importer) populate this; validation-only
    /// hosts leave it empty because no reachability consumer reads it there.
    ///
    /// Kept on the std hasher: `rue-compiler` and `rue-cfg` construct and walk
    /// this field, so its hasher is part of AIR's public surface.
    pub method_references: std::collections::HashSet<(crate::StructId, lasso::Spur)>,
}

#[derive(Debug, Clone)]
pub struct SemanticBodyCandidate<K, M> {
    pub owner: crate::BodyOwnerToken,
    pub body_span: rue_span::Span,
    pub body: SemanticBody<K, M>,
}

#[derive(Debug, Clone)]
pub struct SemanticQueriedBodyCandidate<K, M> {
    pub identity: FunctionInstanceKey<K, M>,
    pub ordinary_owner: Option<crate::BodyOwnerToken>,
    pub specialization_identity: Option<SemanticSpecializationIdentity<K, M>>,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBodyImportFailure {
    Semantic(super::SemanticImportFailure),
    StableResolution(SemanticStableResolutionFailure),
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
    AirBuild(crate::AirBuildError),
    AirValidation(crate::AirValidationError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticBodyImportFailureKind {
    Semantic(SemanticImportFailure),
    UnsupportedForm,
    StructuralValidation,
}

impl SemanticBodyImportFailure {
    pub fn kind(&self) -> SemanticBodyImportFailureKind {
        match self {
            Self::Semantic(reason) => SemanticBodyImportFailureKind::Semantic(*reason),
            Self::UnsupportedGenericCall => SemanticBodyImportFailureKind::UnsupportedForm,
            _ => SemanticBodyImportFailureKind::StructuralValidation,
        }
    }
}

impl From<super::SemanticImportFailure> for SemanticBodyImportFailure {
    fn from(value: super::SemanticImportFailure) -> Self {
        Self::Semantic(value)
    }
}

impl From<crate::AirBuildError> for SemanticBodyImportFailure {
    fn from(value: crate::AirBuildError) -> Self {
        Self::AirBuild(value)
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    type Sample = SemanticBodyInstData<&'static str, &'static str>;

    fn call_args(values: &[u32]) -> Arc<[SemanticBodyCallArg]> {
        values
            .iter()
            .map(|value| SemanticBodyCallArg {
                value: *value,
                mode: AirArgMode::Normal,
            })
            .collect::<Vec<_>>()
            .into()
    }

    fn instruction_samples() -> Vec<Sample> {
        use SemanticBodyInstData as D;
        vec![
            D::Const(1),
            D::BoolConst(true),
            D::StringConst(2),
            D::UnitConst,
            D::TypeConst(SemanticImportType::Module("module")),
            D::Add(1, 2),
            D::Sub(1, 2),
            D::Mul(1, 2),
            D::WrappingAdd(1, 2),
            D::WrappingSub(1, 2),
            D::WrappingMul(1, 2),
            D::Div(1, 2),
            D::Mod(1, 2),
            D::Eq(1, 2),
            D::Ne(1, 2),
            D::Lt(1, 2),
            D::Gt(1, 2),
            D::Le(1, 2),
            D::Ge(1, 2),
            D::And(1, 2),
            D::Or(1, 2),
            D::BitAnd(1, 2),
            D::BitOr(1, 2),
            D::BitXor(1, 2),
            D::Shl(1, 2),
            D::Shr(1, 2),
            D::Neg(1),
            D::Not(1),
            D::BitNot(1),
            D::Branch {
                cond: 1,
                then_value: 2,
                else_value: Some(3),
            },
            D::Loop { cond: 1, body: 2 },
            D::InfiniteLoop { body: 1 },
            D::Match {
                scrutinee: 1,
                arms: vec![SemanticBodyMatchArm {
                    pattern: SemanticBodyPattern::EnumVariant {
                        enum_key: NominalInstanceKey::Named("match_enum"),
                        variant_index: 0,
                    },
                    body: 2,
                }]
                .into(),
            },
            D::Break,
            D::Continue,
            D::Alloc { slot: 0, init: 1 },
            D::Load { slot: 0 },
            D::Store { slot: 0, value: 1 },
            D::ParamStore {
                param_slot: 0,
                value: 1,
            },
            D::Ret(Some(1)),
            D::Call {
                function: FunctionInstanceKey::Definition("function"),
                args: call_args(&[1, 2]),
            },
            D::RuntimeCall {
                runtime: crate::RuntimeCallKind::DebugI64,
                args: call_args(&[1]),
            },
            D::CallSpecialized {
                identity: SemanticSpecializationIdentity {
                    base: "generic",
                    type_arguments: vec![SemanticImportType::Nominal("type_argument")].into(),
                    value_arguments: vec![SemanticImportConstValue::Function("value_argument")]
                        .into(),
                },
                args: call_args(&[1]),
            },
            D::CallGeneric,
            D::Intrinsic {
                runtime: None,
                name: Arc::from("intrinsic"),
                args: call_args(&[1]),
            },
            D::Param { index: 0 },
            D::Block {
                statements: vec![1, 2].into(),
                value: 3,
            },
            D::StructInit {
                struct_key: NominalInstanceKey::Named("struct"),
                fields: vec![1, 2].into(),
                source_order: vec![1, 0].into(),
            },
            D::ArrayInit {
                elements: vec![1, 2].into(),
            },
            D::PlaceRead { place: 4 },
            D::PlaceWrite { place: 4, value: 1 },
            D::EnumVariant {
                enum_key: NominalInstanceKey::Named("enum"),
                variant_index: 1,
                payload: vec![1, 2].into(),
            },
            D::EnumPayloadGet {
                base: 1,
                enum_key: NominalInstanceKey::Named("enum"),
                variant_index: 1,
                field_index: 0,
            },
            D::IntCast {
                value: 1,
                from_ty: SemanticImportType::Nominal("cast_type"),
            },
            D::Drop { value: 1 },
            D::StorageLive { slot: 0 },
            D::StorageDead { slot: 0 },
            D::MarkMoved {
                value: 1,
                slot: 0,
                is_param: false,
                place: Some(4),
            },
            D::AccessorCall {
                function: FunctionInstanceKey::Definition("accessor"),
                args: call_args(&[1]),
            },
        ]
    }

    #[test]
    fn semantic_body_instruction_schema_has_stable_unique_metadata() {
        assert_eq!(SEMANTIC_BODY_INST_KINDS.len(), 59);
        for (tag, kind) in SEMANTIC_BODY_INST_KINDS.iter().copied().enumerate() {
            assert_eq!(usize::from(kind.schema_tag()), tag);
            assert!(!kind.display_name().is_empty());
            assert_eq!(kind.to_string(), kind.display_name());
            assert!(
                SEMANTIC_BODY_INST_KINDS[..tag]
                    .iter()
                    .all(|earlier| earlier.display_name() != kind.display_name())
            );
        }
    }

    #[test]
    fn every_instruction_kind_projects_and_exposes_dependencies() {
        let samples = instruction_samples();
        assert_eq!(samples.len(), SEMANTIC_BODY_INST_KINDS.len());
        for (sample, expected_kind) in samples.iter().zip(SEMANTIC_BODY_INST_KINDS) {
            assert_eq!(sample.kind(), *expected_kind);
            let projected = sample
                .try_map_keys(&|key| Ok::<_, ()>(format!("mapped:{key}")), &|module| {
                    Ok::<_, ()>(format!("mapped:{module}"))
                })
                .unwrap();
            assert_eq!(projected.kind(), *expected_kind);
            let mut dependency_count = 0;
            sample.visit_dependencies(&mut |_| dependency_count += 1);
            assert!(
                dependency_count <= 8,
                "unbounded sample for {expected_kind}"
            );
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum SeenDependency<'a> {
        Instruction(u32),
        Place(u32),
        Definition(&'a str),
        Type(crate::SemanticImportTypeKind),
        String(u32),
    }

    fn dependencies(sample: &Sample) -> Vec<SeenDependency<'_>> {
        let mut seen = Vec::new();
        sample.visit_dependencies(&mut |dependency| {
            seen.push(match dependency {
                SemanticBodyInstDependency::Instruction(value) => {
                    SeenDependency::Instruction(value)
                }
                SemanticBodyInstDependency::Place(value) => SeenDependency::Place(value),
                SemanticBodyInstDependency::Definition(value) => SeenDependency::Definition(value),
                SemanticBodyInstDependency::Nominal(NominalInstanceKey::Named(value)) => {
                    SeenDependency::Definition(value)
                }
                SemanticBodyInstDependency::Function(FunctionInstanceKey::Definition(value)) => {
                    SeenDependency::Definition(value)
                }
                SemanticBodyInstDependency::Nominal(NominalInstanceKey::Builtin { .. })
                | SemanticBodyInstDependency::Nominal(NominalInstanceKey::Anonymous(_))
                | SemanticBodyInstDependency::Function(_) => {
                    panic!("the bounded named sample does not use anonymous identities")
                }
                SemanticBodyInstDependency::Type(value) => SeenDependency::Type(value.kind()),
                SemanticBodyInstDependency::String(value) => SeenDependency::String(value),
            });
        });
        seen
    }

    #[test]
    fn representative_dependency_shapes_are_exact() {
        let samples = instruction_samples();
        assert_eq!(dependencies(&samples[2]), [SeenDependency::String(2)]);
        assert_eq!(
            dependencies(&samples[5]),
            [
                SeenDependency::Instruction(1),
                SeenDependency::Instruction(2)
            ]
        );
        assert_eq!(
            dependencies(&samples[29]),
            [
                SeenDependency::Instruction(1),
                SeenDependency::Instruction(2),
                SeenDependency::Instruction(3)
            ]
        );
        assert_eq!(
            dependencies(&samples[32]),
            [
                SeenDependency::Instruction(1),
                SeenDependency::Definition("match_enum"),
                SeenDependency::Instruction(2)
            ]
        );
        assert_eq!(
            dependencies(&samples[46]),
            [
                SeenDependency::Instruction(1),
                SeenDependency::Instruction(2),
                SeenDependency::Instruction(3)
            ]
        );
        assert_eq!(
            dependencies(&samples[47]),
            [
                SeenDependency::Definition("struct"),
                SeenDependency::Instruction(1),
                SeenDependency::Instruction(2)
            ]
        );
        assert_eq!(dependencies(&samples[49]), [SeenDependency::Place(4)]);
        assert_eq!(
            dependencies(&samples[50]),
            [SeenDependency::Place(4), SeenDependency::Instruction(1)]
        );
        assert_eq!(
            dependencies(&samples[51]),
            [
                SeenDependency::Definition("enum"),
                SeenDependency::Instruction(1),
                SeenDependency::Instruction(2)
            ]
        );
        assert_eq!(
            dependencies(&samples[52]),
            [
                SeenDependency::Instruction(1),
                SeenDependency::Definition("enum")
            ]
        );
        assert_eq!(
            dependencies(&samples[57]),
            [SeenDependency::Instruction(1), SeenDependency::Place(4)]
        );
        assert_eq!(
            dependencies(&samples[58]),
            [
                SeenDependency::Definition("accessor"),
                SeenDependency::Instruction(1)
            ]
        );
    }

    #[test]
    fn representative_projection_shapes_map_every_identity_domain() {
        let samples = instruction_samples();
        let map = |sample: &Sample| {
            sample
                .try_map_keys(&|key| Ok::<_, ()>(format!("mapped:{key}")), &|module| {
                    Ok::<_, ()>(format!("mapped:{module}"))
                })
                .unwrap()
        };
        assert!(matches!(
            map(&samples[4]),
            SemanticBodyInstData::TypeConst(SemanticImportType::Module(module))
                if module == "mapped:module"
        ));
        assert!(matches!(
            map(&samples[32]),
            SemanticBodyInstData::Match { arms, .. }
                if matches!(&arms[0].pattern, SemanticBodyPattern::EnumVariant { enum_key: NominalInstanceKey::Named(enum_key), .. } if enum_key == "mapped:match_enum")
        ));
        assert!(matches!(
            map(&samples[47]),
            SemanticBodyInstData::StructInit { struct_key: NominalInstanceKey::Named(struct_key), .. } if struct_key == "mapped:struct"
        ));
        assert!(matches!(
            map(&samples[51]),
            SemanticBodyInstData::EnumVariant { enum_key: NominalInstanceKey::Named(enum_key), .. } if enum_key == "mapped:enum"
        ));
        assert!(matches!(
            map(&samples[53]),
            SemanticBodyInstData::IntCast {
                from_ty: SemanticImportType::Nominal(key),
                ..
            } if key == "mapped:cast_type"
        ));
        assert!(matches!(
            map(&samples[58]),
            SemanticBodyInstData::AccessorCall {
                function: FunctionInstanceKey::Definition(key),
                ..
            } if key == "mapped:accessor"
        ));
    }

    #[test]
    fn instruction_projection_and_dependency_hooks_preserve_borrowed_arrays() {
        let args: Arc<[SemanticBodyCallArg]> = vec![SemanticBodyCallArg {
            value: 7,
            mode: AirArgMode::Borrow,
        }]
        .into();
        let type_arguments: Arc<[SemanticImportType<&str, &str>]> =
            vec![SemanticImportType::Nominal("T")].into();
        let value_arguments: Arc<[SemanticImportConstValue<&str, &str>]> = vec![
            SemanticImportConstValue::Function("helper"),
            SemanticImportConstValue::Type(SemanticImportType::Module("module")),
        ]
        .into();
        let data = SemanticBodyInstData::CallSpecialized {
            identity: SemanticSpecializationIdentity {
                base: "function",
                type_arguments: type_arguments.clone(),
                value_arguments: value_arguments.clone(),
            },
            args: args.clone(),
        };

        assert_eq!(data.kind(), SemanticBodyInstKind::CallSpecialized);
        let mut definitions = Vec::new();
        let mut instruction_refs = Vec::new();
        let mut saw_type_argument_by_reference = false;
        data.visit_dependencies(&mut |dependency| match dependency {
            SemanticBodyInstDependency::Instruction(value) => instruction_refs.push(value),
            SemanticBodyInstDependency::Definition(value) => definitions.push(*value),
            SemanticBodyInstDependency::Nominal(_) | SemanticBodyInstDependency::Function(_) => {}
            SemanticBodyInstDependency::Type(value) => {
                saw_type_argument_by_reference |= std::ptr::eq(value, &type_arguments[0]);
            }
            SemanticBodyInstDependency::Place(_) | SemanticBodyInstDependency::String(_) => {}
        });
        assert_eq!(definitions, ["function", "helper"]);
        assert_eq!(instruction_refs, [7]);
        assert!(saw_type_argument_by_reference);

        let mapped = data
            .try_map_keys(&|key| Ok::<_, ()>(format!("mapped:{key}")), &|module| {
                Ok::<_, ()>(format!("mapped:{module}"))
            })
            .unwrap();
        let SemanticBodyInstData::CallSpecialized {
            identity,
            args: mapped_args,
        } = mapped
        else {
            panic!("projection changed instruction kind")
        };
        assert_eq!(identity.base, "mapped:function");
        assert_eq!(
            identity.type_arguments[0],
            SemanticImportType::Nominal("mapped:T".to_owned())
        );
        assert!(Arc::ptr_eq(&args, &mapped_args));
    }

    #[test]
    fn typed_failure_context_reports_instruction_and_schema_identity() {
        let data = SemanticBodyInstData::<(), ()>::Add(0, 1);
        let context = SemanticBodyInstFailureContext::new(12, &data, "invalid reference");
        assert_eq!(context.instruction, 12);
        assert_eq!(context.kind, SemanticBodyInstKind::Add);
        assert_eq!(
            context.to_string(),
            "instruction 12 (add, schema tag 5): invalid reference"
        );
    }
}
