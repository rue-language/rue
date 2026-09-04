//! Request-independent semantic values imported into a fresh AIR epoch.
//!
//! This module deliberately stops short of installing declarations into a
//! live analyzer. Bodies, source spans, and RIR declaration handles belong to
//! an exact semantic request. The importer reconstructs only values that AIR
//! can represent without borrowing handles from the exporting request.

use std::collections::BTreeMap;
use std::hash::Hash;
use std::sync::Arc;

use ahash::AHashMap;
use lasso::{LassoErrorKind, Spur, ThreadedRodeo};
use rue_span::FileId;

use crate::Node;
use crate::builtin_universe::BuiltinUniverse;
use crate::{
    Air, AirCallArg, AirInst, AirInstData, AirPattern, AirProjection, AirRef, AnonymousNominalKey,
    ConstValue, EnumDef, EnumId, FunctionInstanceKey, ModuleId, ModuleRegistry, NominalInstanceKey,
    SemanticBody, SemanticBodyImportFailure, SemanticBodyInstData, SemanticBodyPattern,
    SemanticBodyProjection, SemanticImportedBody, StructDef, StructField, StructId, Type,
    TypeInstanceKey, TypeInternPool,
};
use rue_rir::SharedSymbolSpace;
use rue_span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticImportNominalKind {
    Struct,
    Enum,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticImportNominal<K> {
    pub key: K,
    pub module_path: Arc<str>,
    pub name: Arc<str>,
    pub kind: SemanticImportNominalKind,
    pub is_public: bool,
    pub is_non_exhaustive: bool,
    pub lang_item: Option<crate::LangItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticImportType<K, M> {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,
    Unit,
    Never,
    ComptimeType,
    F32,
    F64,
    ComptimeFloat,
    BuiltinNominal {
        name: Arc<str>,
        kind: SemanticImportNominalKind,
    },
    Nominal(K),
    AnonymousNominal(AnonymousNominalKey<K, M>),
    Array {
        element: Arc<Self>,
        len: u64,
    },
    PtrConst(Arc<Self>),
    PtrMut(Arc<Self>),
    Slice {
        element: Arc<Self>,
        name: Arc<str>,
    },
    Module(M),
    GenericParameter(u32),
}

macro_rules! semantic_import_type_schema {
    ($consumer:ident) => {
        $consumer! {
            I8, SemanticImportType::I8, 0, "i8";
            I16, SemanticImportType::I16, 1, "i16";
            I32, SemanticImportType::I32, 2, "i32";
            I64, SemanticImportType::I64, 3, "i64";
            U8, SemanticImportType::U8, 4, "u8";
            U16, SemanticImportType::U16, 5, "u16";
            U32, SemanticImportType::U32, 6, "u32";
            U64, SemanticImportType::U64, 7, "u64";
            Bool, SemanticImportType::Bool, 8, "bool";
            Unit, SemanticImportType::Unit, 9, "unit";
            Never, SemanticImportType::Never, 10, "never";
            ComptimeType, SemanticImportType::ComptimeType, 11, "comptime_type";
            BuiltinNominal, SemanticImportType::BuiltinNominal { .. }, 12, "builtin_nominal";
            Nominal, SemanticImportType::Nominal(..), 13, "nominal";
            Array, SemanticImportType::Array { .. }, 14, "array";
            PtrConst, SemanticImportType::PtrConst(..), 15, "ptr_const";
            PtrMut, SemanticImportType::PtrMut(..), 16, "ptr_mut";
            Module, SemanticImportType::Module(..), 17, "module";
            GenericParameter, SemanticImportType::GenericParameter(..), 18, "generic_parameter";
            AnonymousNominal, SemanticImportType::AnonymousNominal(..), 19, "anonymous_nominal";
            Slice, SemanticImportType::Slice { .. }, 20, "slice";
            F32, SemanticImportType::F32, 21, "f32";
            F64, SemanticImportType::F64, 22, "f64";
            ComptimeFloat, SemanticImportType::ComptimeFloat, 23, "comptime_float";
        }
    };
}

macro_rules! define_semantic_import_type_schema {
    ($( $kind:ident, $pattern:pat, $tag:literal, $name:literal; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u8)]
        pub enum SemanticImportTypeKind {
            $( $kind = $tag, )*
        }

        pub const SEMANTIC_IMPORT_TYPE_KINDS: &[SemanticImportTypeKind] = &[
            $( SemanticImportTypeKind::$kind, )*
        ];

        impl SemanticImportTypeKind {
            pub const fn schema_tag(self) -> u8 {
                self as u8
            }

            pub const fn display_name(self) -> &'static str {
                match self {
                    $( Self::$kind => $name, )*
                }
            }
        }

        impl std::fmt::Display for SemanticImportTypeKind {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.display_name())
            }
        }

        impl<K, M> SemanticImportType<K, M> {
            pub const fn kind(&self) -> SemanticImportTypeKind {
                match self {
                    $( $pattern => SemanticImportTypeKind::$kind, )*
                }
            }
        }
    };
}

semantic_import_type_schema!(define_semantic_import_type_schema);

/// One post-order step in the canonical type algebra. Recursive children have
/// already been folded to `T`, so projection, validation, and import share one
/// exhaustive schema traversal.
pub enum SemanticImportTypeFold<'a, K, M, T> {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,
    Unit,
    Never,
    ComptimeType,
    F32,
    F64,
    ComptimeFloat,
    BuiltinNominal {
        name: &'a Arc<str>,
        kind: SemanticImportNominalKind,
    },
    Nominal(&'a K),
    AnonymousNominal(&'a AnonymousNominalKey<K, M>),
    Array {
        element: T,
        len: u64,
    },
    PtrConst(T),
    PtrMut(T),
    Slice {
        element: T,
        name: &'a Arc<str>,
    },
    Module(&'a M),
    GenericParameter(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticImportConstValue<K, M> {
    Integer(i128),
    Bool(bool),
    Type(SemanticImportType<K, M>),
    Function(K),
    Unit,
    /// String constant content (RUE-957). Carried as the literal text —
    /// interner symbols are process-local and cannot cross the import
    /// boundary.
    String(std::sync::Arc<str>),
    Float(std::sync::Arc<str>),
}

macro_rules! semantic_import_const_schema {
    ($consumer:ident) => {
        $consumer! {
            Integer, SemanticImportConstValue::Integer(..), 0, "integer";
            Bool, SemanticImportConstValue::Bool(..), 1, "bool";
            Type, SemanticImportConstValue::Type(..), 2, "type";
            Function, SemanticImportConstValue::Function(..), 3, "function";
            Unit, SemanticImportConstValue::Unit, 4, "unit";
            String, SemanticImportConstValue::String(..), 5, "string";
            Float, SemanticImportConstValue::Float(..), 6, "float";
        }
    };
}

macro_rules! define_semantic_import_const_schema {
    ($( $kind:ident, $pattern:pat, $tag:literal, $name:literal; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u8)]
        pub enum SemanticImportConstKind {
            $( $kind = $tag, )*
        }

        pub const SEMANTIC_IMPORT_CONST_KINDS: &[SemanticImportConstKind] = &[
            $( SemanticImportConstKind::$kind, )*
        ];

        impl SemanticImportConstKind {
            pub const fn schema_tag(self) -> u8 {
                self as u8
            }

            pub const fn display_name(self) -> &'static str {
                match self {
                    $( Self::$kind => $name, )*
                }
            }
        }

        impl std::fmt::Display for SemanticImportConstKind {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.display_name())
            }
        }

        impl<K, M> SemanticImportConstValue<K, M> {
            pub const fn kind(&self) -> SemanticImportConstKind {
                match self {
                    $( $pattern => SemanticImportConstKind::$kind, )*
                }
            }
        }
    };
}

semantic_import_const_schema!(define_semantic_import_const_schema);

impl<K, M> SemanticImportType<K, M> {
    /// Fold this value in post-order through the canonical schema visitor.
    pub fn try_fold<T, E>(
        &self,
        fold: &mut impl FnMut(SemanticImportTypeFold<'_, K, M, T>) -> Result<T, E>,
    ) -> Result<T, E> {
        use SemanticImportType as S;
        use SemanticImportTypeFold as F;
        let node = match self {
            S::I8 => F::I8,
            S::I16 => F::I16,
            S::I32 => F::I32,
            S::I64 => F::I64,
            S::U8 => F::U8,
            S::U16 => F::U16,
            S::U32 => F::U32,
            S::U64 => F::U64,
            S::Bool => F::Bool,
            S::Unit => F::Unit,
            S::Never => F::Never,
            S::ComptimeType => F::ComptimeType,
            S::F32 => F::F32,
            S::F64 => F::F64,
            S::ComptimeFloat => F::ComptimeFloat,
            S::BuiltinNominal { name, kind } => F::BuiltinNominal { name, kind: *kind },
            S::Nominal(key) => F::Nominal(key),
            S::AnonymousNominal(key) => F::AnonymousNominal(key),
            S::Array { element, len } => F::Array {
                element: element.try_fold(fold)?,
                len: *len,
            },
            S::PtrConst(value) => F::PtrConst(value.try_fold(fold)?),
            S::PtrMut(value) => F::PtrMut(value.try_fold(fold)?),
            S::Slice { element, name } => F::Slice {
                element: element.try_fold(fold)?,
                name,
            },
            S::Module(module) => F::Module(module),
            S::GenericParameter(index) => F::GenericParameter(*index),
        };
        fold(node)
    }

    /// Relocate every identity in this canonical type without changing its
    /// structural shape. This is the single traversal used by body,
    /// specialization, and durable declaration adapters.
    pub fn try_map_identities<K2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        key: &impl Fn(&K) -> Result<K2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<SemanticImportType<K2, M2>, E> {
        use SemanticImportType as T;
        use SemanticImportTypeFold as F;
        self.try_fold(&mut |node| {
            Ok(match node {
                F::I8 => T::I8,
                F::I16 => T::I16,
                F::I32 => T::I32,
                F::I64 => T::I64,
                F::U8 => T::U8,
                F::U16 => T::U16,
                F::U32 => T::U32,
                F::U64 => T::U64,
                F::Bool => T::Bool,
                F::Unit => T::Unit,
                F::Never => T::Never,
                F::ComptimeType => T::ComptimeType,
                F::F32 => T::F32,
                F::F64 => T::F64,
                F::ComptimeFloat => T::ComptimeFloat,
                F::BuiltinNominal { name, kind } => T::BuiltinNominal {
                    name: name.clone(),
                    kind,
                },
                F::Nominal(value) => T::Nominal(key(value)?),
                F::AnonymousNominal(value) => {
                    T::AnonymousNominal(value.try_map_identities(key, module)?)
                }
                F::Array { element, len } => T::Array {
                    element: Arc::new(element),
                    len,
                },
                F::PtrConst(value) => T::PtrConst(Arc::new(value)),
                F::PtrMut(value) => T::PtrMut(Arc::new(value)),
                F::Slice { element, name } => T::Slice {
                    element: Arc::new(element),
                    name: name.clone(),
                },
                F::Module(value) => T::Module(module(value)?),
                F::GenericParameter(index) => T::GenericParameter(index),
            })
        })
    }
}

impl<K, M> SemanticImportConstValue<K, M> {
    /// Relocate identities using the canonical type traversal.
    pub fn try_map_identities<K2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        key: &impl Fn(&K) -> Result<K2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<SemanticImportConstValue<K2, M2>, E> {
        Ok(match self {
            Self::Integer(value) => SemanticImportConstValue::Integer(*value),
            Self::Bool(value) => SemanticImportConstValue::Bool(*value),
            Self::Type(value) => {
                SemanticImportConstValue::Type(value.try_map_identities(key, module)?)
            }
            Self::Function(value) => SemanticImportConstValue::Function(key(value)?),
            Self::Unit => SemanticImportConstValue::Unit,
            Self::String(value) => SemanticImportConstValue::String(value.clone()),
            Self::Float(value) => SemanticImportConstValue::Float(value.clone()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticImportFailure {
    /// The body-local symbol domain rejected a new spelling. Preserve the
    /// lasso classification so the compiler can distinguish E1401 from E1402.
    Interner(LassoErrorKind),
    DuplicateNominal,
    DuplicateNominalLocalIdentity,
    DuplicateFunction,
    DuplicateFunctionLocalIdentity,
    DeclarationKindMismatch,
    NominalKindMismatch,
    MissingNominal,
    MissingModule,
    MissingFunction,
    UnknownBuiltinNominal,
    BuiltinNominalKindMismatch,
    GenericParameterNeedsDeclarationContext,
    GenericParameterOutOfRange,
    InvalidStructuralType,
    NominalAlreadyComplete,
    ForeignLocalType,
    ForeignLocalValue,
    DuplicateCallable,
    DuplicateCallableLocalIdentity,
    DuplicateModule,
    BuiltinNominalShadow,
    MissingBodyIdentity,
    IncompleteMaterialization,
}

/// One exact nominal fact supplied to a body-local semantic epoch.
///
/// The key supplies named and producer-owned anonymous identities. Builtin
/// identities are epoch-owned and must not appear in this input; fixed builtins
/// and dynamic `Str(N)` instances resolve through the canonical builtin
/// registry and issuing type pool. A local epoch may materialize supplied
/// anonymous structs/enums, but never invent their identity or rediscover their
/// shape from a whole-program semantic universe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticLocalNominal<K, M> {
    pub key: NominalInstanceKey<K, M>,
    pub module_path: Arc<str>,
    pub name: Arc<str>,
    pub kind: SemanticImportNominalKind,
    pub is_public: bool,
    pub lang_item: Option<crate::LangItem>,
    pub shape: SemanticLocalNominalShape<K, M>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticLocalNominalShape<K, M> {
    Struct {
        fields: Arc<[(Arc<str>, SemanticImportType<K, M>)]>,
        is_copy: bool,
        is_linear: bool,
        /// Whether the struct was declared `linear` in source. Anonymous and
        /// builtin nominals cannot be, so their producers pass `false`; the
        /// named durable path carries the declaration bit verbatim.
        declared_linear: bool,
        destructor: Option<FunctionInstanceKey<K, M>>,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[SemanticImportType<K, M>]>)]>,
        is_non_exhaustive: bool,
    },
}

/// One exact callable symbol fact supplied to a body-local semantic epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticLocalCallable<K, M> {
    pub key: FunctionInstanceKey<K, M>,
    pub symbol: Arc<str>,
}

fn nominal_import_type<K: Clone, M: Clone>(
    key: NominalInstanceKey<K, M>,
) -> SemanticImportType<K, M> {
    match key {
        NominalInstanceKey::Builtin { kind, name } => SemanticImportType::BuiltinNominal {
            name,
            kind: match kind {
                crate::AnonymousNominalKind::Struct => SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => SemanticImportNominalKind::Enum,
            },
        },
        NominalInstanceKey::Named(key) => SemanticImportType::Nominal(key),
        NominalInstanceKey::Anonymous(key) => {
            SemanticImportType::AnonymousNominal(key.into_inner())
        }
    }
}

fn import_type_identity<K: Clone + std::hash::Hash, M: Clone + std::hash::Hash>(
    ty: &SemanticImportType<K, M>,
) -> TypeInstanceKey<K, M> {
    use SemanticImportType as S;
    match ty {
        S::I8 => TypeInstanceKey::I8,
        S::I16 => TypeInstanceKey::I16,
        S::I32 => TypeInstanceKey::I32,
        S::I64 => TypeInstanceKey::I64,
        S::U8 => TypeInstanceKey::U8,
        S::U16 => TypeInstanceKey::U16,
        S::U32 => TypeInstanceKey::U32,
        S::U64 => TypeInstanceKey::U64,
        S::Bool => TypeInstanceKey::Bool,
        S::Unit => TypeInstanceKey::Unit,
        S::Never => TypeInstanceKey::Never,
        S::ComptimeType => TypeInstanceKey::ComptimeType,
        S::F32 => TypeInstanceKey::F32,
        S::F64 => TypeInstanceKey::F64,
        S::ComptimeFloat => TypeInstanceKey::ComptimeFloat,
        S::BuiltinNominal { name, kind } => TypeInstanceKey::BuiltinNominal {
            name: name.clone(),
            kind: match kind {
                SemanticImportNominalKind::Struct => crate::AnonymousNominalKind::Struct,
                SemanticImportNominalKind::Enum => crate::AnonymousNominalKind::Enum,
            },
        },
        S::Nominal(key) => TypeInstanceKey::Nominal(NominalInstanceKey::Named(key.clone())),
        S::AnonymousNominal(key) => {
            TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(Node::new(key.clone())))
        }
        S::Array { element, len } => TypeInstanceKey::Array {
            element: Node::new(import_type_identity(element)),
            len: *len,
        },
        S::PtrConst(inner) => TypeInstanceKey::PtrConst(Node::new(import_type_identity(inner))),
        S::PtrMut(inner) => TypeInstanceKey::PtrMut(Node::new(import_type_identity(inner))),
        S::Slice { element, name } => TypeInstanceKey::Slice {
            element: Node::new(import_type_identity(element)),
            name: name.clone(),
        },
        S::Module(module) => TypeInstanceKey::Module(module.clone()),
        S::GenericParameter(index) => TypeInstanceKey::GenericParameter(*index),
    }
}

fn specialization_key<K: Clone + std::hash::Hash, M: Clone + std::hash::Hash>(
    identity: &crate::SemanticSpecializationIdentity<K, M>,
) -> FunctionInstanceKey<K, M> {
    let values = identity
        .value_arguments
        .iter()
        .map(|value| match value {
            SemanticImportConstValue::Integer(value) => {
                crate::CanonicalArgumentValue::Integer(*value)
            }
            SemanticImportConstValue::Bool(value) => crate::CanonicalArgumentValue::Bool(*value),
            SemanticImportConstValue::Type(value) => {
                crate::CanonicalArgumentValue::Type(Node::new(import_type_identity(value)))
            }
            SemanticImportConstValue::Function(value) => crate::CanonicalArgumentValue::Function(
                Node::new(FunctionInstanceKey::Definition(value.clone())),
            ),
            SemanticImportConstValue::Unit => crate::CanonicalArgumentValue::Unit,
            SemanticImportConstValue::String(value) => {
                crate::CanonicalArgumentValue::String(value.clone())
            }
            SemanticImportConstValue::Float(value) => {
                crate::CanonicalArgumentValue::Float(value.clone())
            }
        })
        .collect::<Vec<_>>();
    FunctionInstanceKey::Specialization {
        base: Node::new(FunctionInstanceKey::Definition(identity.base.clone())),
        arguments: crate::CanonicalArguments {
            types: identity
                .type_arguments
                .iter()
                .map(import_type_identity)
                .collect::<Vec<_>>()
                .into(),
            values: values.into(),
        },
    }
}

/// Completeness witness retained with an owned local materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticLocalCompleteness {
    nominals_declared: usize,
    nominals_completed: usize,
    callables_registered: usize,
    modules_registered: usize,
}

impl SemanticLocalCompleteness {
    pub fn is_complete(self) -> bool {
        self.nominals_declared == self.nominals_completed
    }
}

/// An owned, body-local semantic artifact suitable for a future CFG query.
///
/// Compact AIR indexes never escape without the exact pool/interner which
/// issued them.  Stable identities remain alongside the local aggregate map;
/// the epoch is a computation helper, not a retained semantic authority.
pub struct SemanticLocalMaterialization<K, M> {
    pub identity: FunctionInstanceKey<K, M>,
    pub name: String,
    pub callable_kind: crate::AnalyzedCallableKind,
    pub air: crate::ValidatedAir,
    pub local_atoms: Vec<crate::LocalAtomRecord<K, M>>,
    pub num_locals: u32,
    pub num_param_slots: u32,
    pub param_modes: crate::ParamSlotModes,
    pub allow_unreachable_code: bool,
    pub type_pool: crate::FrozenTypeInternPool,
    pub interner: Arc<ThreadedRodeo>,
    aggregate_types: ahash::AHashMap<crate::Type, TypeInstanceKey<K, M>>,
    /// Exact caller-owned handles explicitly pre-materialized for a mandatory
    /// accessor splice, paired with their stable type identities.
    pub materialized_types: Vec<(crate::Type, SemanticImportType<K, M>)>,
    pub strings: Vec<String>,
    pub warnings: Arc<[rue_error::CompileWarning]>,
    pub body_span: Span,
    pub completeness: SemanticLocalCompleteness,
}

impl<K, M> SemanticLocalMaterialization<K, M> {
    /// Look up the stable identity for one complete local aggregate type.
    ///
    /// The backing table is deliberately private: callers should depend on
    /// this narrow lookup rather than on AIR's map implementation or hasher.
    pub fn aggregate_type(&self, ty: crate::Type) -> Option<&TypeInstanceKey<K, M>> {
        self.aggregate_types.get(&ty)
    }

    pub fn has_aggregate_type(&self, ty: crate::Type) -> bool {
        self.aggregate_types.contains_key(&ty)
    }

    pub fn aggregate_type_count(&self) -> usize {
        self.aggregate_types.len()
    }

    /// Iterate aggregate identities in the canonical local type-pool order.
    pub fn aggregate_type_entries(
        &self,
    ) -> impl Iterator<Item = (&crate::Type, &TypeInstanceKey<K, M>)> {
        let mut entries = self.aggregate_types.iter().collect::<Vec<_>>();
        entries.sort_unstable_by_key(|(ty, _)| ty.as_u32());
        entries.into_iter()
    }
}

/// An AIR type branded with the import epoch which issued it.
#[derive(Debug, Clone)]
pub struct SemanticImportedType {
    epoch: Arc<()>,
    value: Type,
}

impl PartialEq for SemanticImportedType {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.epoch, &other.epoch) && self.value == other.value
    }
}
impl Eq for SemanticImportedType {}

/// An AIR constant branded with the import epoch which issued it.
#[derive(Debug, Clone)]
pub struct SemanticImportedConstValue {
    epoch: Arc<()>,
    value: ConstValue,
}

impl PartialEq for SemanticImportedConstValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.epoch, &other.epoch) && self.value == other.value
    }
}
impl Eq for SemanticImportedConstValue {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LocalNominal {
    Struct(StructId),
    Enum(EnumId),
}

/// A fresh AIR-owned interning epoch joined to caller-owned stable keys.
///
/// Stable keys never become AIR IDs. Constructors sort exact facts before
/// minting any local ID or symbol; that ordered input, not map iteration,
/// defines the deterministic allocation order. The nominal and callable maps
/// are independently keyed exact-lookup joins, while local-to-stable indexes
/// support export. Imported types and constants are returned through opaque,
/// epoch-branded wrappers, so request-local IDs cannot cross epoch boundaries.
pub struct SemanticImportEpoch<K: Ord, M: Ord> {
    epoch: Arc<()>,
    interner: Arc<ThreadedRodeo>,
    /// The owner of the local interner. Keeping insertion behind this space
    /// lets canonical callers inject their request-local bound without a
    /// mutable or thread-local test override.
    symbol_space: SharedSymbolSpace,
    type_pool: TypeInternPool,
    module_registry: ModuleRegistry,
    nominals: AHashMap<NominalInstanceKey<K, M>, LocalNominal>,
    functions: AHashMap<FunctionInstanceKey<K, M>, Spur>,
    modules: BTreeMap<M, ModuleId>,
    builtins: BTreeMap<(Arc<str>, SemanticImportNominalKind), LocalNominal>,
    nominal_exports: AHashMap<LocalNominal, NominalInstanceKey<K, M>>,
    function_exports: AHashMap<Spur, FunctionInstanceKey<K, M>>,
    module_exports: AHashMap<ModuleId, M>,
    builtin_exports: AHashMap<LocalNominal, (Arc<str>, SemanticImportNominalKind)>,
    local_completeness: Option<SemanticLocalCompleteness>,
}

impl<K, M> SemanticImportEpoch<K, M>
where
    K: Clone + Ord + Hash,
    M: Clone + Ord + Hash,
{
    fn rebuild_export_indexes(&mut self) {
        self.nominal_exports = self
            .nominals
            .iter()
            .map(|(stable, local)| (*local, stable.clone()))
            .collect();
        self.function_exports = self
            .functions
            .iter()
            .map(|(stable, local)| (*local, stable.clone()))
            .collect();
        self.module_exports = self
            .modules
            .iter()
            .map(|(stable, local)| (*local, stable.clone()))
            .collect();
        self.builtin_exports = self
            .builtins
            .iter()
            .map(|(stable, local)| (*local, stable.clone()))
            .collect();
        assert_eq!(self.nominal_exports.len(), self.nominals.len());
        assert_eq!(self.function_exports.len(), self.functions.len());
        assert_eq!(self.module_exports.len(), self.modules.len());
        assert_eq!(self.builtin_exports.len(), self.builtins.len());
    }

    /// Reconstruct a structured durable body without publishing partial state.
    ///
    /// The first pass performs every fallible reconstruction step against the
    /// type-pool transaction snapshot and an isolated symbol interner. Only a
    /// body that passes that preflight is rebuilt with the live interner, so a
    /// returned error changes neither pool nor symbol allocation order.
    pub fn import_body(
        &self,
        body: &SemanticBody<K, M>,
        body_span: Span,
    ) -> Result<SemanticImportedBody<K, M>, SemanticBodyImportFailure> {
        self.type_pool.transaction(|type_pool| {
            let scratch_interner = ThreadedRodeo::new();
            self.import_body_in_pool(body, body_span, type_pool, &scratch_interner)?;

            // The same immutable body has passed every error path, and the
            // transaction snapshot now contains every structural type it
            // needs. This pass can therefore publish symbols without a later
            // recoverable failure leaving a prefix behind.
            self.import_body_in_pool(body, body_span, type_pool, &self.interner)
        })
    }

    fn import_body_in_pool(
        &self,
        body: &SemanticBody<K, M>,
        body_span: Span,
        type_pool: &TypeInternPool,
        interner: &ThreadedRodeo,
    ) -> Result<SemanticImportedBody<K, M>, SemanticBodyImportFailure> {
        Self::import_body_with(
            body,
            body_span,
            type_pool,
            false,
            |ty| {
                self.import_type_local_with(ty, type_pool, None)
                    .map_err(Into::into)
            },
            |key| match self.resolve_nominal_in_pool(type_pool, key)? {
                LocalNominal::Struct(id) => Ok(id),
                LocalNominal::Enum(_) => Err(SemanticBodyImportFailure::WrongNominalKind),
            },
            |key| match self.resolve_nominal_in_pool(type_pool, key)? {
                LocalNominal::Enum(id) => Ok(id),
                LocalNominal::Struct(_) => Err(SemanticBodyImportFailure::WrongNominalKind),
            },
            |key| match key {
                FunctionInstanceKey::Definition(key) => self
                    .functions
                    .get(&FunctionInstanceKey::Definition(key.clone()))
                    .copied()
                    .ok_or(SemanticBodyImportFailure::Semantic(
                        SemanticImportFailure::MissingFunction,
                    )),
                _ => Err(SemanticBodyImportFailure::Semantic(
                    SemanticImportFailure::MissingFunction,
                )),
            },
            |_| Err(SemanticBodyImportFailure::UnsupportedGenericCall),
            |name| {
                interner.try_get_or_intern(name).map_err(|error| {
                    SemanticBodyImportFailure::Semantic(SemanticImportFailure::Interner(
                        error.kind(),
                    ))
                })
            },
        )
    }

    pub(crate) fn import_body_with(
        body: &SemanticBody<K, M>,
        body_span: Span,
        type_pool: &TypeInternPool,
        specialized_calls_are_direct: bool,
        import_type: impl Fn(&SemanticImportType<K, M>) -> Result<Type, SemanticBodyImportFailure>,
        struct_id: impl Fn(&NominalInstanceKey<K, M>) -> Result<StructId, SemanticBodyImportFailure>,
        enum_id: impl Fn(&NominalInstanceKey<K, M>) -> Result<EnumId, SemanticBodyImportFailure>,
        resolve_function: impl Fn(&FunctionInstanceKey<K, M>) -> Result<Spur, SemanticBodyImportFailure>,
        resolve_specialization: impl Fn(
            &crate::SemanticSpecializationIdentity<K, M>,
        ) -> Result<
            (Spur, Vec<Type>, Vec<crate::sema::ConstValue>),
            SemanticBodyImportFailure,
        >,
        intern: impl Fn(&str) -> Result<Spur, SemanticBodyImportFailure>,
    ) -> Result<SemanticImportedBody<K, M>, SemanticBodyImportFailure> {
        use SemanticBodyImportFailure as F;
        let body_len = body_span
            .end
            .checked_sub(body_span.start)
            .ok_or(F::InvalidAnchor)?;
        let current_anchor = |anchor: crate::SemanticBodyAnchor| -> Result<Span, F> {
            if anchor.start > anchor.end || anchor.end > body_len {
                return Err(F::InvalidAnchor);
            }
            let start = body_span
                .start
                .checked_add(anchor.start)
                .ok_or(F::InvalidAnchor)?;
            let end = body_span
                .start
                .checked_add(anchor.end)
                .ok_or(F::InvalidAnchor)?;
            Ok(Span::with_file(body_span.file_id, start, end))
        };
        if body.param_by_ref.len() != body.num_param_slots as usize
            || body.param_writable.len() != body.num_param_slots as usize
        {
            return Err(F::InvalidParameterModes);
        }
        if body
            .param_drops
            .iter()
            .any(|(slot, _)| *slot >= body.num_param_slots)
        {
            return Err(F::InvalidParameterDrop);
        }
        if body
            .borrow_slots
            .iter()
            .any(|slot| *slot >= body.num_locals)
        {
            return Err(F::InvalidBorrowSlot);
        }
        let return_type = import_type(&body.return_type)?;
        let mut air = Air::new(return_type);
        let inst_len = body.instructions.len();
        air.reserve_instructions(inst_len);
        let place_len = body.places.len();
        let check_ref = |r: u32, current: usize| -> Result<AirRef, F> {
            let index = r as usize;
            if index >= inst_len {
                return Err(F::InvalidInstructionReference);
            }
            if index >= current {
                return Err(F::ForwardInstructionReference);
            }
            Ok(AirRef::from_raw(r))
        };
        let call_args = |args: &[crate::SemanticBodyCallArg], current: usize| {
            let mut imported = Vec::with_capacity(args.len());
            for arg in args {
                imported.push(AirCallArg {
                    value: check_ref(arg.value, current)?,
                    mode: arg.mode,
                });
            }
            Ok::<_, F>(imported)
        };
        let refs = |values: &[u32], current: usize| {
            let mut imported = Vec::with_capacity(values.len());
            for value in values {
                imported.push(check_ref(*value, current)?);
            }
            Ok::<_, F>(imported)
        };
        for (current, inst) in body.instructions.iter().enumerate() {
            let span = current_anchor(inst.anchor)?;
            let ty = import_type(&inst.ty)?;
            let r = |value| check_ref(value, current);
            let binary =
                |a, b, ctor: fn(AirRef, AirRef) -> AirInstData| Ok::<_, F>(ctor(r(a)?, r(b)?));
            let data = match &inst.data {
                SemanticBodyInstData::Const(v) => AirInstData::Const(*v),
                SemanticBodyInstData::BoolConst(v) => AirInstData::BoolConst(*v),
                SemanticBodyInstData::StringConst(v) => {
                    if *v as usize >= body.strings.len() {
                        return Err(F::InvalidStringReference);
                    }
                    AirInstData::StringConst(*v)
                }
                SemanticBodyInstData::UnitConst => AirInstData::UnitConst,
                SemanticBodyInstData::TypeConst(v) => AirInstData::TypeConst(import_type(v)?),
                SemanticBodyInstData::Add(a, b) => binary(*a, *b, AirInstData::Add)?,
                SemanticBodyInstData::Sub(a, b) => binary(*a, *b, AirInstData::Sub)?,
                SemanticBodyInstData::Mul(a, b) => binary(*a, *b, AirInstData::Mul)?,
                SemanticBodyInstData::WrappingAdd(a, b) => {
                    binary(*a, *b, AirInstData::WrappingAdd)?
                }
                SemanticBodyInstData::WrappingSub(a, b) => {
                    binary(*a, *b, AirInstData::WrappingSub)?
                }
                SemanticBodyInstData::WrappingMul(a, b) => {
                    binary(*a, *b, AirInstData::WrappingMul)?
                }
                SemanticBodyInstData::Div(a, b) => binary(*a, *b, AirInstData::Div)?,
                SemanticBodyInstData::Mod(a, b) => binary(*a, *b, AirInstData::Mod)?,
                SemanticBodyInstData::Eq(a, b) => binary(*a, *b, AirInstData::Eq)?,
                SemanticBodyInstData::Ne(a, b) => binary(*a, *b, AirInstData::Ne)?,
                SemanticBodyInstData::Lt(a, b) => binary(*a, *b, AirInstData::Lt)?,
                SemanticBodyInstData::Gt(a, b) => binary(*a, *b, AirInstData::Gt)?,
                SemanticBodyInstData::Le(a, b) => binary(*a, *b, AirInstData::Le)?,
                SemanticBodyInstData::Ge(a, b) => binary(*a, *b, AirInstData::Ge)?,
                SemanticBodyInstData::And(a, b) => binary(*a, *b, AirInstData::And)?,
                SemanticBodyInstData::Or(a, b) => binary(*a, *b, AirInstData::Or)?,
                SemanticBodyInstData::BitAnd(a, b) => binary(*a, *b, AirInstData::BitAnd)?,
                SemanticBodyInstData::BitOr(a, b) => binary(*a, *b, AirInstData::BitOr)?,
                SemanticBodyInstData::BitXor(a, b) => binary(*a, *b, AirInstData::BitXor)?,
                SemanticBodyInstData::Shl(a, b) => binary(*a, *b, AirInstData::Shl)?,
                SemanticBodyInstData::Shr(a, b) => binary(*a, *b, AirInstData::Shr)?,
                SemanticBodyInstData::Neg(v) => AirInstData::Neg(r(*v)?),
                SemanticBodyInstData::Not(v) => AirInstData::Not(r(*v)?),
                SemanticBodyInstData::BitNot(v) => AirInstData::BitNot(r(*v)?),
                SemanticBodyInstData::Branch {
                    cond,
                    then_value,
                    else_value,
                } => AirInstData::Branch {
                    cond: r(*cond)?,
                    then_value: r(*then_value)?,
                    else_value: else_value.map(r).transpose()?,
                },
                SemanticBodyInstData::Loop { cond, body } => AirInstData::Loop {
                    cond: r(*cond)?,
                    body: r(*body)?,
                },
                SemanticBodyInstData::InfiniteLoop { body } => {
                    AirInstData::InfiniteLoop { body: r(*body)? }
                }
                SemanticBodyInstData::Match { scrutinee, arms } => {
                    let mut imported_arms = Vec::new();
                    for arm in arms.iter() {
                        let pat = match &arm.pattern {
                            SemanticBodyPattern::Wildcard => AirPattern::Wildcard,
                            SemanticBodyPattern::Int(v) => AirPattern::Int(*v),
                            SemanticBodyPattern::Bool(v) => AirPattern::Bool(*v),
                            SemanticBodyPattern::EnumVariant {
                                enum_key,
                                variant_index,
                            } => AirPattern::EnumVariant {
                                enum_id: enum_id(enum_key)?,
                                variant_index: *variant_index,
                            },
                        };
                        imported_arms.push((pat, r(arm.body)?));
                    }
                    air.add_match(r(*scrutinee)?, &imported_arms, ty, span)?;
                    continue;
                }
                SemanticBodyInstData::Break => AirInstData::Break,
                SemanticBodyInstData::Continue => AirInstData::Continue,
                SemanticBodyInstData::Alloc { slot, init } => AirInstData::Alloc {
                    slot: *slot,
                    init: r(*init)?,
                },
                SemanticBodyInstData::Load { slot } => AirInstData::Load { slot: *slot },
                SemanticBodyInstData::Store { slot, value } => AirInstData::Store {
                    slot: *slot,
                    value: r(*value)?,
                },
                SemanticBodyInstData::ParamStore { param_slot, value } => AirInstData::ParamStore {
                    param_slot: *param_slot,
                    value: r(*value)?,
                },
                SemanticBodyInstData::Ret(v) => AirInstData::Ret(v.map(r).transpose()?),
                SemanticBodyInstData::Call { function, args } => {
                    let name = resolve_function(function)?;
                    let args = call_args(args, current)?;
                    air.add_call(None, name, &args, ty, span)?;
                    continue;
                }
                SemanticBodyInstData::AccessorCall { function, args } => {
                    let name = resolve_function(function)?;
                    let args = call_args(args, current)?;
                    air.add_accessor_call(name, &args, ty, span)?;
                    continue;
                }
                SemanticBodyInstData::RuntimeCall { runtime, args } => {
                    let args = call_args(args, current)?;
                    air.add_call(
                        Some(*runtime),
                        intern(runtime.helper().helper().symbol)?,
                        &args,
                        ty,
                        span,
                    )?;
                    continue;
                }
                SemanticBodyInstData::CallSpecialized { identity, args } => {
                    let (name, type_args, value_args) = resolve_specialization(identity)?;
                    let args = call_args(args, current)?;
                    if specialized_calls_are_direct {
                        air.add_call(None, name, &args, ty, span)?;
                    } else {
                        air.add_call_generic(name, &type_args, &value_args, &args, ty, span)?;
                    }
                    continue;
                }
                SemanticBodyInstData::CallGeneric => return Err(F::UnsupportedGenericCall),
                SemanticBodyInstData::Intrinsic {
                    operation,
                    name,
                    args,
                } => {
                    let source_name = name.as_ref();
                    if args.iter().any(|arg| arg.mode != crate::AirArgMode::Normal) {
                        return Err(F::InvalidParameterModes);
                    }
                    let values = args.iter().map(|arg| arg.value).collect::<Vec<_>>();
                    let values = refs(&values, current)?;
                    if source_name != operation.expected_spelling() {
                        return Err(F::InvalidIntrinsicOperation);
                    }
                    let arguments = values
                        .iter()
                        .map(|value| {
                            crate::intrinsic_air_argument_with_place_lookup(
                                &air,
                                *value,
                                crate::AirArgMode::Normal,
                                |place| {
                                    matches!(
                                        body.places[place.as_u32() as usize].projections.last(),
                                        Some(SemanticBodyProjection::Field { .. })
                                    )
                                },
                            )
                        })
                        .collect::<Vec<_>>();
                    if !operation.validate_call(type_pool, &arguments, ty) {
                        return Err(F::InvalidIntrinsicOperation);
                    }
                    let name = intern(source_name)?;
                    air.add_intrinsic(*operation, name, &values, ty, span)?;
                    continue;
                }
                SemanticBodyInstData::Param { index } => AirInstData::Param { index: *index },
                SemanticBodyInstData::Block { statements, value } => {
                    let statements = refs(statements, current)?;
                    air.add_block(&statements, r(*value)?, ty, span)?;
                    continue;
                }
                SemanticBodyInstData::StructInit {
                    struct_key,
                    fields,
                    source_order,
                } => {
                    let mut order_seen = vec![false; fields.len()];
                    if fields.len() != source_order.len()
                        || source_order.iter().any(|i| {
                            let index = *i as usize;
                            index >= fields.len() || std::mem::replace(&mut order_seen[index], true)
                        })
                    {
                        return Err(F::InvalidSourceOrder);
                    }
                    let fields = refs(fields, current)?;
                    air.add_struct_init(struct_id(struct_key)?, &fields, source_order, ty, span)?;
                    continue;
                }
                SemanticBodyInstData::ArrayInit { elements } => {
                    let elements = refs(elements, current)?;
                    air.add_array_init(&elements, ty, span)?;
                    continue;
                }
                SemanticBodyInstData::PlaceRead { place } => {
                    if *place as usize >= place_len {
                        return Err(F::InvalidPlaceReference);
                    }
                    AirInstData::PlaceRead {
                        place: crate::AirPlaceRef::from_raw(*place),
                    }
                }
                SemanticBodyInstData::PlaceWrite { place, value } => {
                    if *place as usize >= place_len {
                        return Err(F::InvalidPlaceReference);
                    }
                    AirInstData::PlaceWrite {
                        place: crate::AirPlaceRef::from_raw(*place),
                        value: r(*value)?,
                    }
                }
                SemanticBodyInstData::EnumVariant {
                    enum_key,
                    variant_index,
                    payload,
                } => {
                    let payload = refs(payload, current)?;
                    air.add_enum_variant(enum_id(enum_key)?, *variant_index, &payload, ty, span)?;
                    continue;
                }
                SemanticBodyInstData::EnumPayloadGet {
                    base,
                    enum_key,
                    variant_index,
                    field_index,
                } => AirInstData::EnumPayloadGet {
                    base: r(*base)?,
                    enum_id: enum_id(enum_key)?,
                    variant_index: *variant_index,
                    field_index: *field_index,
                },
                SemanticBodyInstData::IntCast { value, from_ty } => AirInstData::IntCast {
                    value: r(*value)?,
                    from_ty: import_type(from_ty)?,
                },
                SemanticBodyInstData::Drop { value } => AirInstData::Drop { value: r(*value)? },
                SemanticBodyInstData::StorageLive { slot } => {
                    AirInstData::StorageLive { slot: *slot }
                }
                SemanticBodyInstData::StorageDead { slot } => {
                    AirInstData::StorageDead { slot: *slot }
                }
                SemanticBodyInstData::MarkMoved {
                    value,
                    slot,
                    is_param,
                    place,
                } => {
                    if place.is_some_and(|p| p as usize >= place_len) {
                        return Err(F::InvalidPlaceReference);
                    }
                    AirInstData::MarkMoved {
                        value: r(*value)?,
                        slot: *slot,
                        is_param: *is_param,
                        place: place.map(crate::AirPlaceRef::from_raw),
                    }
                }
            };
            air.add_inst(AirInst { data, ty, span });
        }
        // Instructions may name places and index projections may name
        // instructions. Publish the instruction stream first, then construct
        // places atomically once every index reference has an owner.
        for place in body.places.iter() {
            let base_type = import_type(&place.base_type)?;
            let mut projections = Vec::with_capacity(place.projections.len());
            for projection in place.projections.iter() {
                projections.push(match projection {
                    SemanticBodyProjection::Field {
                        struct_key,
                        field_index,
                    } => AirProjection::Field {
                        struct_id: struct_id(struct_key)?,
                        field_index: *field_index,
                    },
                    SemanticBodyProjection::Index { array_type, index } => {
                        if *index as usize >= inst_len {
                            return Err(F::InvalidInstructionReference);
                        }
                        AirProjection::Index {
                            array_type: import_type(array_type)?,
                            index: AirRef::from_raw(*index),
                        }
                    }
                });
            }
            air.make_place(place.base, base_type, projections)?;
        }
        let mut drops = Vec::with_capacity(body.param_drops.len());
        for (slot, ty) in body.param_drops.iter() {
            drops.push((*slot, import_type(ty)?));
        }
        air.set_param_drops(drops);
        for slot in body.borrow_slots.iter() {
            air.add_borrow_slot(*slot)
        }
        let warnings = body
            .warnings
            .iter()
            .map(|warning| {
                let span = current_anchor(warning.anchor)?;
                let mut projected = rue_error::CompileWarning::new(warning.kind.clone(), span);
                for label in warning.labels.iter() {
                    projected =
                        projected.with_label(label.message.as_ref(), current_anchor(label.anchor)?);
                }
                for note in warning.notes.iter() {
                    projected = projected.with_note(note.as_ref());
                }
                for help in warning.helps.iter() {
                    projected = projected.with_help(help.as_ref());
                }
                for suggestion in warning.suggestions.iter() {
                    projected = projected.with_suggestion(
                        rue_error::Suggestion::new(
                            suggestion.message.as_ref(),
                            current_anchor(suggestion.anchor)?,
                            suggestion.replacement.as_ref(),
                        )
                        .with_applicability(suggestion.applicability),
                    );
                }
                Ok(projected)
            })
            .collect::<Result<Vec<_>, F>>()?;
        // Most bodies carry only a handful of stable local atoms, so keep the
        // allocation-free scan for that common case. Literal-heavy retained
        // bodies amortize one index over every relocation instead of rescanning
        // the full string table for each atom.
        const LOCAL_ATOM_STRING_INDEX_MIN_COMPARISONS: usize = 256;
        let estimated_scan_comparisons = body.local_atoms.len().saturating_mul(body.strings.len());
        let string_positions = (body.local_atoms.len() > 1
            && body.strings.len() > 1
            && estimated_scan_comparisons >= LOCAL_ATOM_STRING_INDEX_MIN_COMPARISONS)
            .then(|| {
                let mut positions = AHashMap::with_capacity(body.strings.len());
                for (index, content) in body.strings.iter().enumerate() {
                    if let Ok(dense_id) = u32::try_from(index) {
                        // Match `position`: malformed duplicate entries resolve
                        // to the first occurrence.
                        positions.entry(content.as_ref()).or_insert(dense_id);
                    }
                }
                positions
            });
        let local_atoms = body
            .local_atoms
            .iter()
            .map(|atom| {
                let dense_id = match &string_positions {
                    Some(positions) => positions.get(atom.content.as_ref()).copied(),
                    None => body
                        .strings
                        .iter()
                        .position(|content| content == &atom.content)
                        .and_then(|index| u32::try_from(index).ok()),
                }
                .ok_or(F::InvalidStringReference)?;
                Ok(crate::LocalAtomRecord {
                    identity: atom.identity.clone(),
                    content: atom.content.clone(),
                    dense_id,
                })
            })
            .collect::<Result<Vec<_>, F>>()?;
        Ok(SemanticImportedBody {
            air: crate::ValidatedAir::from_semantic_air(air, type_pool)
                .map_err(F::AirValidation)?,
            strings: body.strings.iter().map(|s| s.to_string()).collect(),
            local_atoms,
            num_locals: body.num_locals,
            num_param_slots: body.num_param_slots,
            param_modes: crate::ParamSlotModes::new(
                body.param_by_ref.to_vec(),
                body.param_writable.to_vec(),
            ),
            allow_unreachable_code: body.allow_unreachable_code,
            warnings: Arc::from(warnings),
        })
    }
    pub fn new(
        nominals: Vec<SemanticImportNominal<K>>,
        function_keys: Vec<(K, Arc<str>)>,
        module_keys: Vec<M>,
    ) -> Result<Self, SemanticImportFailure> {
        Self::new_in_space(
            nominals,
            function_keys,
            module_keys,
            SharedSymbolSpace::private(),
        )
    }

    fn new_in_space(
        mut nominals: Vec<SemanticImportNominal<K>>,
        mut function_keys: Vec<(K, Arc<str>)>,
        mut module_keys: Vec<M>,
        symbol_space: SharedSymbolSpace,
    ) -> Result<Self, SemanticImportFailure> {
        nominals.sort_by(|a, b| a.key.cmp(&b.key));
        function_keys.sort_by(|a, b| a.0.cmp(&b.0));
        module_keys.sort();

        let interner = symbol_space.interner().clone();
        let type_pool = TypeInternPool::new();
        let module_registry = ModuleRegistry::new();
        let mut universe = BuiltinUniverse::begin(&type_pool, &symbol_space)
            .map_err(SemanticImportFailure::Interner)?;
        let mut builtins = BTreeMap::new();
        for (name, id) in universe.enum_entries() {
            builtins.insert(
                (Arc::clone(name), SemanticImportNominalKind::Enum),
                LocalNominal::Enum(id),
            );
        }
        let mut modules = BTreeMap::new();
        for (index, key) in module_keys.into_iter().enumerate() {
            let path = format!("semantic-module-{index}");
            let id = module_registry.push_canonical(crate::ModuleDef::new(
                path.clone(),
                path.clone(),
                path,
                FileId::DEFAULT,
            ));
            modules.insert(key, id);
        }

        let mut functions = AHashMap::with_capacity(function_keys.len());
        let mut function_identities = std::collections::BTreeSet::new();
        for (key, name) in function_keys {
            if !function_identities.insert(name.clone()) {
                return Err(SemanticImportFailure::DuplicateFunctionLocalIdentity);
            }
            let symbol = symbol_space
                .try_intern(name.as_ref())
                .map_err(SemanticImportFailure::Interner)?;
            if functions
                .insert(FunctionInstanceKey::Definition(key), symbol)
                .is_some()
            {
                return Err(SemanticImportFailure::DuplicateFunction);
            }
        }

        let mut module_files = BTreeMap::<Arc<str>, FileId>::new();
        for nominal in &nominals {
            let next = u32::try_from(module_files.len() + 1).expect("too many semantic modules");
            module_files
                .entry(nominal.module_path.clone())
                .or_insert(FileId::new(next));
        }
        type_pool.set_symbol_paths(
            module_files
                .iter()
                .map(|(path, file_id)| (*file_id, path.to_string()))
                .collect(),
        );
        let mut local = AHashMap::with_capacity(nominals.len());
        let mut local_identities = std::collections::BTreeSet::new();
        for nominal in nominals {
            if local.contains_key(&NominalInstanceKey::Named(nominal.key.clone())) {
                return Err(SemanticImportFailure::DuplicateNominal);
            }
            if !local_identities.insert((
                nominal.kind,
                nominal.module_path.clone(),
                nominal.name.clone(),
            )) {
                return Err(SemanticImportFailure::DuplicateNominalLocalIdentity);
            }
            let file_id = module_files[&nominal.module_path];
            let name = nominal.name.clone();
            let symbol = symbol_space
                .try_intern(name.as_ref())
                .map_err(SemanticImportFailure::Interner)?;
            let value = match nominal.kind {
                SemanticImportNominalKind::Struct => {
                    let (id, _) = type_pool.declare_struct(
                        symbol,
                        StructDef {
                            name,
                            fields: vec![],
                            is_copy: false,
                            is_linear: false,
                            declared_linear: false,
                            destructor: None,
                            is_builtin: false,
                            is_pub: nominal.is_public,
                            file_id,
                        },
                    );
                    if let Some(lang_item) = nominal.lang_item {
                        type_pool.set_struct_lang_item(id, lang_item);
                    }
                    LocalNominal::Struct(id)
                }
                SemanticImportNominalKind::Enum => {
                    let (id, _) = type_pool.declare_enum(
                        symbol,
                        EnumDef {
                            name,
                            variants: Arc::from([]),
                            variant_payloads: vec![],
                            is_pub: nominal.is_public,
                            is_non_exhaustive: nominal.is_non_exhaustive,
                            file_id,
                        },
                    );
                    LocalNominal::Enum(id)
                }
            };
            local.insert(NominalInstanceKey::Named(nominal.key), value);
        }
        // Ordinary sema registers source nominals before lazily creating the
        // stable core `str` identity. Preserve that order so a fresh epoch's
        // packed nominal IDs match exported AIR exactly. StrBuf is deliberately
        // absent: it is an ordinary source nominal supplied by std.
        universe
            .finish_core_str(&type_pool, &symbol_space)
            .map_err(SemanticImportFailure::Interner)?;
        let (str_name, str_id) = universe
            .core_str_entry()
            .expect("builtin universe always finishes core str");
        builtins.insert(
            (str_name, SemanticImportNominalKind::Struct),
            LocalNominal::Struct(str_id),
        );
        let mut epoch = Self {
            epoch: Arc::new(()),
            interner,
            symbol_space,
            type_pool,
            module_registry,
            nominals: local,
            functions,
            modules,
            builtins,
            nominal_exports: AHashMap::new(),
            function_exports: AHashMap::new(),
            module_exports: AHashMap::new(),
            builtin_exports: AHashMap::new(),
            local_completeness: None,
        };
        epoch.rebuild_export_indexes();
        Ok(epoch)
    }

    /// Construct a body-local epoch from exact query-owned facts.
    ///
    /// All nominal shells are declared before any shape is completed, so
    /// recursive aggregates are supported without widening the input to a
    /// reachable-program universe. Duplicate identities and duplicate local
    /// symbols fail before a body can be imported.
    pub fn new_local(
        nominals: Vec<SemanticLocalNominal<K, M>>,
        callables: Vec<SemanticLocalCallable<K, M>>,
        modules: Vec<M>,
    ) -> Result<Self, SemanticImportFailure>
    where
        K: Eq,
        M: Clone + Eq,
    {
        Self::new_local_in_space(nominals, callables, modules, SharedSymbolSpace::private())
    }

    /// Construct a body-local epoch using the caller-owned symbol space.
    ///
    /// The compiler uses this for request-local CFG materialization so the
    /// actual AIR insertion path, rather than a post-hoc length check, is
    /// governed by the same bounded/fallible policy as other canonical names.
    pub fn new_local_in_space(
        mut nominals: Vec<SemanticLocalNominal<K, M>>,
        mut callables: Vec<SemanticLocalCallable<K, M>>,
        modules: Vec<M>,
        symbol_space: SharedSymbolSpace,
    ) -> Result<Self, SemanticImportFailure>
    where
        K: Eq,
        M: Clone + Eq,
    {
        nominals.sort_by(|left, right| left.key.cmp(&right.key));
        callables.sort_by(|left, right| left.key.cmp(&right.key));
        if nominals.iter().any(|nominal| {
            let NominalInstanceKey::Builtin { name, .. } = &nominal.key else {
                return false;
            };
            name.as_ref() == BuiltinUniverse::CORE_STR_NAME
                || BuiltinUniverse::builtin_enum_name(name)
                || crate::types::fixed_string_capacity(name).is_some()
        }) {
            return Err(SemanticImportFailure::BuiltinNominalShadow);
        }
        let mut modules = modules;
        modules.sort();
        if modules.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(SemanticImportFailure::DuplicateModule);
        }
        let mut epoch = Self::new_in_space(Vec::new(), Vec::new(), modules, symbol_space)?;
        epoch.functions.reserve(callables.len());
        epoch.nominals.reserve(nominals.len());

        let mut callable_symbols = std::collections::BTreeSet::new();
        for callable in &callables {
            if !callable_symbols.insert(callable.symbol.clone()) {
                return Err(SemanticImportFailure::DuplicateCallableLocalIdentity);
            }
            let symbol = epoch
                .symbol_space
                .try_intern(callable.symbol.as_ref())
                .map_err(SemanticImportFailure::Interner)?;
            if epoch
                .functions
                .insert(callable.key.clone(), symbol)
                .is_some()
            {
                return Err(SemanticImportFailure::DuplicateCallable);
            }
        }

        let mut module_files = BTreeMap::<Arc<str>, FileId>::new();
        for nominal in &nominals {
            let next = u32::try_from(module_files.len() + 1).expect("too many local modules");
            module_files
                .entry(nominal.module_path.clone())
                .or_insert(FileId::new(next));
        }
        epoch.type_pool.set_symbol_paths(
            module_files
                .iter()
                .map(|(path, file)| (*file, path.to_string()))
                .collect(),
        );
        let mut local_identities = std::collections::BTreeSet::new();
        for nominal in &nominals {
            let is_anonymous = matches!(&nominal.key, NominalInstanceKey::Anonymous(_));
            let builtin_key = match &nominal.key {
                NominalInstanceKey::Builtin { kind, name } => Some((
                    name.clone(),
                    match kind {
                        crate::AnonymousNominalKind::Struct => SemanticImportNominalKind::Struct,
                        crate::AnonymousNominalKind::Enum => SemanticImportNominalKind::Enum,
                    },
                )),
                _ => None,
            };
            if epoch.nominals.contains_key(&nominal.key)
                || builtin_key
                    .as_ref()
                    .is_some_and(|key| epoch.builtins.contains_key(key))
            {
                return Err(SemanticImportFailure::DuplicateNominal);
            }
            if !local_identities.insert((
                nominal.kind,
                nominal.module_path.clone(),
                nominal.name.clone(),
            )) {
                return Err(SemanticImportFailure::DuplicateNominalLocalIdentity);
            }
            let file_id = module_files[&nominal.module_path];
            let symbol = epoch
                .symbol_space
                .try_intern(nominal.name.as_ref())
                .map_err(SemanticImportFailure::Interner)?;
            let local = match nominal.kind {
                SemanticImportNominalKind::Struct => {
                    let (id, _) = epoch.type_pool.declare_struct(
                        symbol,
                        StructDef {
                            name: nominal.name.clone(),
                            fields: Vec::new(),
                            is_copy: false,
                            is_linear: false,
                            declared_linear: false,
                            destructor: None,
                            is_builtin: builtin_key.is_some(),
                            is_pub: nominal.is_public,
                            file_id,
                        },
                    );
                    if let Some(lang_item) = nominal.lang_item {
                        epoch.type_pool.set_struct_lang_item(id, lang_item);
                    }
                    if is_anonymous {
                        epoch.type_pool.mark_anonymous_struct(id);
                    }
                    LocalNominal::Struct(id)
                }
                SemanticImportNominalKind::Enum => {
                    if nominal.lang_item.is_some() {
                        return Err(SemanticImportFailure::NominalKindMismatch);
                    }
                    let (id, _) = epoch.type_pool.declare_enum(
                        symbol,
                        EnumDef {
                            name: nominal.name.clone(),
                            variants: Arc::from([]),
                            variant_payloads: Vec::new(),
                            is_pub: nominal.is_public,
                            is_non_exhaustive: match &nominal.shape {
                                SemanticLocalNominalShape::Enum {
                                    is_non_exhaustive, ..
                                } => *is_non_exhaustive,
                                SemanticLocalNominalShape::Struct { .. } => false,
                            },
                            file_id,
                        },
                    );
                    if is_anonymous {
                        epoch.type_pool.mark_anonymous_enum(id);
                    }
                    LocalNominal::Enum(id)
                }
            };
            epoch.nominals.insert(nominal.key.clone(), local);
            if let Some(key) = builtin_key {
                epoch.builtins.insert(key, local);
            }
        }

        epoch.complete_local_nominals(&nominals)?;
        epoch.local_completeness = Some(SemanticLocalCompleteness {
            nominals_declared: nominals.len(),
            nominals_completed: nominals.len(),
            callables_registered: callables.len(),
            modules_registered: epoch.modules.len(),
        });
        epoch.rebuild_export_indexes();
        Ok(epoch)
    }

    /// Consume this exact local epoch into an owned function artifact.
    pub fn materialize_local_body(
        self,
        identity: FunctionInstanceKey<K, M>,
        callable_kind: crate::AnalyzedCallableKind,
        body: &SemanticBody<K, M>,
        body_span: Span,
    ) -> Result<SemanticLocalMaterialization<K, M>, SemanticBodyImportFailure>
    where
        K: Eq + Hash,
        M: Clone + Eq + Hash,
    {
        self.materialize_local_body_with_types(identity, callable_kind, body, body_span, &[])
    }

    pub fn materialize_local_body_with_types(
        self,
        identity: FunctionInstanceKey<K, M>,
        callable_kind: crate::AnalyzedCallableKind,
        body: &SemanticBody<K, M>,
        body_span: Span,
        additional_types: &[SemanticImportType<K, M>],
    ) -> Result<SemanticLocalMaterialization<K, M>, SemanticBodyImportFailure>
    where
        K: Clone + Eq + Hash,
        M: Clone + Eq + Hash,
    {
        let completeness = self
            .local_completeness
            .ok_or(SemanticBodyImportFailure::Semantic(
                SemanticImportFailure::IncompleteMaterialization,
            ))?;
        if !completeness.is_complete()
            || completeness.nominals_declared != self.nominals.len()
            || completeness.callables_registered != self.functions.len()
            || completeness.modules_registered != self.modules.len()
        {
            return Err(SemanticBodyImportFailure::Semantic(
                SemanticImportFailure::IncompleteMaterialization,
            ));
        }
        let name =
            self.functions
                .get(&identity)
                .copied()
                .ok_or(SemanticBodyImportFailure::Semantic(
                    SemanticImportFailure::MissingBodyIdentity,
                ))?;
        let imported = Self::import_body_with(
            body,
            body_span,
            &self.type_pool,
            true,
            |ty| self.import_type_local(ty).map_err(Into::into),
            |key| match self.resolve_nominal_in_pool(&self.type_pool, key)? {
                LocalNominal::Struct(id) => Ok(id),
                LocalNominal::Enum(_) => Err(SemanticBodyImportFailure::WrongNominalKind),
            },
            |key| match self.resolve_nominal_in_pool(&self.type_pool, key)? {
                LocalNominal::Enum(id) => Ok(id),
                LocalNominal::Struct(_) => Err(SemanticBodyImportFailure::WrongNominalKind),
            },
            |key| {
                self.functions
                    .get(key)
                    .copied()
                    .ok_or(SemanticBodyImportFailure::Semantic(
                        SemanticImportFailure::MissingFunction,
                    ))
            },
            |specialization| {
                let key = specialization_key(specialization);
                let symbol = self.functions.get(&key).copied().ok_or(
                    SemanticBodyImportFailure::Semantic(SemanticImportFailure::MissingFunction),
                )?;
                Ok((symbol, Vec::new(), Vec::new()))
            },
            |value| {
                self.symbol_space.try_intern(value).map_err(|kind| {
                    SemanticBodyImportFailure::Semantic(SemanticImportFailure::Interner(kind))
                })
            },
        )?;
        let materialized_types = additional_types
            .iter()
            .map(|stable| {
                Ok((
                    self.import_type_local(stable)
                        .map_err(SemanticBodyImportFailure::Semantic)?,
                    stable.clone(),
                ))
            })
            .collect::<Result<Vec<_>, SemanticBodyImportFailure>>()?;
        let complete_types = self.type_pool.complete_type_handles();
        let mut aggregate_types = ahash::AHashMap::with_capacity(complete_types.len());
        for ty in complete_types {
            if matches!(
                ty.kind(),
                crate::TypeKind::Struct(_) | crate::TypeKind::Enum(_) | crate::TypeKind::Array(_)
            ) {
                let stable = self
                    .export_type_local(ty)
                    .map_err(SemanticBodyImportFailure::Semantic)?;
                aggregate_types.insert(ty, import_type_identity(&stable));
            }
        }
        let SemanticImportedBody {
            air,
            strings,
            local_atoms,
            num_locals,
            num_param_slots,
            param_modes,
            allow_unreachable_code,
            warnings,
        } = imported;
        Ok(SemanticLocalMaterialization {
            identity,
            name: self.interner.resolve(&name).to_owned(),
            callable_kind,
            air,
            local_atoms,
            num_locals,
            num_param_slots,
            param_modes,
            allow_unreachable_code,
            type_pool: self.type_pool.freeze(),
            interner: self.interner,
            aggregate_types,
            materialized_types,
            strings,
            warnings,
            body_span,
            completeness,
        })
    }

    pub fn import_type(
        &self,
        value: &SemanticImportType<K, M>,
    ) -> Result<SemanticImportedType, SemanticImportFailure> {
        self.import_type_local(value)
            .map(|value| SemanticImportedType {
                epoch: self.epoch.clone(),
                value,
            })
    }

    fn resolve_builtin_nominal_in_pool(
        &self,
        type_pool: &TypeInternPool,
        name: &Arc<str>,
        kind: SemanticImportNominalKind,
    ) -> Result<LocalNominal, SemanticImportFailure> {
        if crate::types::fixed_string_capacity(name).is_some() {
            if kind != SemanticImportNominalKind::Struct {
                return Err(SemanticImportFailure::BuiltinNominalKindMismatch);
            }
            let symbol = self
                .symbol_space
                .try_intern(name.as_ref())
                .map_err(SemanticImportFailure::Interner)?;
            if let Some(existing) = type_pool.get_struct_by_file_name(FileId::DEFAULT, symbol) {
                return Ok(LocalNominal::Struct(
                    existing
                        .as_struct()
                        .expect("fixed string lookup returns a struct"),
                ));
            }
            let pointer = type_pool.intern_ptr_const_from_type(Type::U8);
            let (id, _) = type_pool.register_struct(
                symbol,
                StructDef {
                    name: Arc::from(name.as_ref()),
                    fields: vec![
                        StructField {
                            name: "ptr".to_owned(),
                            ty: Type::new_ptr_const(pointer),
                        },
                        StructField {
                            name: "len".to_owned(),
                            ty: Type::U64,
                        },
                    ],
                    is_copy: true,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                    is_builtin: true,
                    is_pub: true,
                    file_id: FileId::DEFAULT,
                },
            );
            return Ok(LocalNominal::Struct(id));
        }

        self.builtins
            .get(&(name.clone(), kind))
            .copied()
            .ok_or_else(|| {
                if self.builtins.keys().any(|(known, _)| known == name) {
                    SemanticImportFailure::BuiltinNominalKindMismatch
                } else {
                    SemanticImportFailure::UnknownBuiltinNominal
                }
            })
    }

    fn resolve_nominal_in_pool(
        &self,
        type_pool: &TypeInternPool,
        key: &NominalInstanceKey<K, M>,
    ) -> Result<LocalNominal, SemanticImportFailure> {
        match key {
            NominalInstanceKey::Builtin { kind, name } => self.resolve_builtin_nominal_in_pool(
                type_pool,
                name,
                match kind {
                    crate::AnonymousNominalKind::Struct => SemanticImportNominalKind::Struct,
                    crate::AnonymousNominalKind::Enum => SemanticImportNominalKind::Enum,
                },
            ),
            NominalInstanceKey::Named(_) | NominalInstanceKey::Anonymous(_) => self
                .nominals
                .get(key)
                .copied()
                .ok_or(SemanticImportFailure::MissingNominal),
        }
    }

    fn import_type_local(
        &self,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, SemanticImportFailure> {
        self.import_type_local_with(value, &self.type_pool, None)
    }

    fn import_type_local_with(
        &self,
        value: &SemanticImportType<K, M>,
        type_pool: &TypeInternPool,
        generic_parameters: Option<&[Type]>,
    ) -> Result<Type, SemanticImportFailure> {
        use SemanticImportTypeFold as F;
        value.try_fold(&mut |node| {
            Ok(match node {
                F::I8 => Type::I8,
                F::I16 => Type::I16,
                F::I32 => Type::I32,
                F::I64 => Type::I64,
                F::U8 => Type::U8,
                F::U16 => Type::U16,
                F::U32 => Type::U32,
                F::U64 => Type::U64,
                F::Bool => Type::BOOL,
                F::Unit => Type::UNIT,
                F::Never => Type::NEVER,
                F::ComptimeType => Type::COMPTIME_TYPE,
                F::F32 => Type::F32,
                F::F64 => Type::F64,
                F::ComptimeFloat => Type::COMPTIME_FLOAT,
                F::BuiltinNominal { name, kind } => {
                    match self.resolve_builtin_nominal_in_pool(type_pool, name, kind)? {
                        LocalNominal::Struct(id) => Type::new_struct(id),
                        LocalNominal::Enum(id) => Type::new_enum(id),
                    }
                }
                F::Nominal(key) => match self.nominals.get(&NominalInstanceKey::Named(key.clone()))
                {
                    Some(LocalNominal::Struct(id)) => Type::new_struct(*id),
                    Some(LocalNominal::Enum(id)) => Type::new_enum(*id),
                    None => return Err(SemanticImportFailure::MissingNominal),
                },
                F::AnonymousNominal(key) => match self
                    .nominals
                    .get(&NominalInstanceKey::Anonymous(Node::new(key.clone())))
                {
                    Some(LocalNominal::Struct(id)) => Type::new_struct(*id),
                    Some(LocalNominal::Enum(id)) => Type::new_enum(*id),
                    None => return Err(SemanticImportFailure::MissingNominal),
                },
                F::Array { element, len } => type_pool
                    .try_intern_array(element, len)
                    .map_err(|_| SemanticImportFailure::InvalidStructuralType)?,
                F::PtrConst(value) => type_pool
                    .try_intern_ptr_const(value)
                    .map_err(|_| SemanticImportFailure::InvalidStructuralType)?,
                F::PtrMut(value) => type_pool
                    .try_intern_ptr_mut(value)
                    .map_err(|_| SemanticImportFailure::InvalidStructuralType)?,
                F::Slice { element, name } => {
                    let symbol = self
                        .symbol_space
                        .try_intern(name.as_ref())
                        .map_err(SemanticImportFailure::Interner)?;
                    let pointer = type_pool.intern_ptr_const_from_type(element);
                    let (id, _) = type_pool.register_struct(
                        symbol,
                        StructDef {
                            name: Arc::from(name.as_ref()),
                            fields: vec![
                                StructField {
                                    name: "ptr".to_owned(),
                                    ty: Type::new_ptr_const(pointer),
                                },
                                StructField {
                                    name: "len".to_owned(),
                                    ty: Type::U64,
                                },
                            ],
                            is_copy: true,
                            is_linear: false,
                            declared_linear: false,
                            destructor: None,
                            is_builtin: true,
                            is_pub: true,
                            file_id: FileId::DEFAULT,
                        },
                    );
                    Type::new_struct(id)
                }
                F::Module(key) => Type::new_module(
                    *self
                        .modules
                        .get(key)
                        .ok_or(SemanticImportFailure::MissingModule)?,
                ),
                F::GenericParameter(index) => *generic_parameters
                    .ok_or(SemanticImportFailure::GenericParameterNeedsDeclarationContext)?
                    .get(index as usize)
                    .ok_or(SemanticImportFailure::GenericParameterOutOfRange)?,
            })
        })
    }

    /// Validate one callable signature in an isolated type epoch.
    ///
    /// The ordered generic environment is derived only from comptime `type`
    /// parameters. A generic reference outside that environment is rejected,
    /// and structural types interned before a later failure remain confined to
    /// the scratch pool.
    pub fn validate_callable_signature(
        &self,
        parameters: &[(SemanticImportType<K, M>, bool)],
        result: &SemanticImportType<K, M>,
    ) -> Result<(), SemanticImportFailure> {
        let type_pool = self.type_pool.clone();
        let generic_parameters = parameters
            .iter()
            .filter(|(ty, is_comptime)| {
                *is_comptime && matches!(ty, SemanticImportType::ComptimeType)
            })
            .map(|_| Type::COMPTIME_TYPE)
            .collect::<Vec<_>>();
        for (ty, _) in parameters {
            self.import_type_local_with(ty, &type_pool, Some(&generic_parameters))?;
        }
        self.import_type_local_with(result, &type_pool, Some(&generic_parameters))?;
        Ok(())
    }

    /// Import an ordered declaration parameter or payload sequence.
    pub fn import_types(
        &self,
        values: &[SemanticImportType<K, M>],
    ) -> Result<Arc<[SemanticImportedType]>, SemanticImportFailure> {
        values
            .iter()
            .map(|value| self.import_type(value))
            .collect::<Result<Vec<_>, _>>()
            .map(Arc::from)
    }

    pub fn import_const_value(
        &self,
        value: &SemanticImportConstValue<K, M>,
    ) -> Result<SemanticImportedConstValue, SemanticImportFailure> {
        self.import_const_value_local(value)
            .map(|value| SemanticImportedConstValue {
                epoch: self.epoch.clone(),
                value,
            })
    }

    fn import_const_value_local(
        &self,
        value: &SemanticImportConstValue<K, M>,
    ) -> Result<ConstValue, SemanticImportFailure> {
        Ok(match value {
            SemanticImportConstValue::Integer(v) => ConstValue::Integer(*v),
            SemanticImportConstValue::Bool(v) => ConstValue::Bool(*v),
            SemanticImportConstValue::Type(v) => ConstValue::Type(self.import_type_local(v)?),
            SemanticImportConstValue::Function(key) => ConstValue::Function(
                (*self
                    .functions
                    .get(&FunctionInstanceKey::Definition(key.clone()))
                    .ok_or(SemanticImportFailure::MissingFunction)?)
                .into(),
            ),
            SemanticImportConstValue::Unit => ConstValue::Unit,
            // The epoch owns an isolated interner, so the content round-trips
            // through it for validation; durable const payloads are never
            // installed into a live analyzer (install fails closed on consts).
            SemanticImportConstValue::String(content) => ConstValue::String(
                self.symbol_space
                    .try_intern(content.as_ref())
                    .map_err(SemanticImportFailure::Interner)?
                    .into(),
            ),
            SemanticImportConstValue::Float(content) => ConstValue::Float(
                self.symbol_space
                    .try_intern(content.as_ref())
                    .map_err(SemanticImportFailure::Interner)?
                    .into(),
            ),
        })
    }

    /// Project an imported local type back through this epoch's stable join.
    /// This rejects local values which were not issued by this epoch.
    pub fn export_type(
        &self,
        value: SemanticImportedType,
    ) -> Result<SemanticImportType<K, M>, SemanticImportFailure> {
        if !Arc::ptr_eq(&value.epoch, &self.epoch) {
            return Err(SemanticImportFailure::ForeignLocalType);
        }
        self.export_type_local(value.value)
    }

    fn export_type_local(
        &self,
        value: Type,
    ) -> Result<SemanticImportType<K, M>, SemanticImportFailure> {
        self.type_pool
            .validate_complete_type(value)
            .map_err(|_| SemanticImportFailure::ForeignLocalType)?;
        self.export_type_local_validated(value)
    }

    /// Project a type whose complete reachable graph was checked by the root
    /// export boundary. Recursive projection must not revalidate every suffix
    /// of that same immutable graph.
    fn export_type_local_validated(
        &self,
        value: Type,
    ) -> Result<SemanticImportType<K, M>, SemanticImportFailure> {
        Ok(match value.kind() {
            crate::TypeKind::I8 => SemanticImportType::I8,
            crate::TypeKind::I16 => SemanticImportType::I16,
            crate::TypeKind::I32 => SemanticImportType::I32,
            crate::TypeKind::I64 => SemanticImportType::I64,
            crate::TypeKind::U8 => SemanticImportType::U8,
            crate::TypeKind::U16 => SemanticImportType::U16,
            crate::TypeKind::U32 => SemanticImportType::U32,
            crate::TypeKind::U64 => SemanticImportType::U64,
            crate::TypeKind::Bool => SemanticImportType::Bool,
            crate::TypeKind::Unit => SemanticImportType::Unit,
            crate::TypeKind::Never => SemanticImportType::Never,
            crate::TypeKind::ComptimeType => SemanticImportType::ComptimeType,
            crate::TypeKind::F32 => SemanticImportType::F32,
            crate::TypeKind::F64 => SemanticImportType::F64,
            crate::TypeKind::ComptimeFloat => SemanticImportType::ComptimeFloat,
            crate::TypeKind::Struct(id) => {
                let def = self.type_pool.struct_def(id);
                if crate::types::is_slice_struct_name(&def.name) {
                    let Some(field) = def.fields.first() else {
                        return Err(SemanticImportFailure::ForeignLocalType);
                    };
                    let crate::TypeKind::PtrConst(pointer) = field.ty.kind() else {
                        return Err(SemanticImportFailure::ForeignLocalType);
                    };
                    SemanticImportType::Slice {
                        element: Arc::new(
                            self.export_type_local_validated(
                                self.type_pool.ptr_const_def(pointer),
                            )?,
                        ),
                        name: def.name.clone(),
                    }
                } else if let Some((name, kind)) =
                    self.builtin_exports.get(&LocalNominal::Struct(id))
                {
                    SemanticImportType::BuiltinNominal {
                        name: name.clone(),
                        kind: *kind,
                    }
                } else if crate::types::fixed_string_struct_capacity(&def).is_some() {
                    // `Str(N)` is registered lazily in the exact transaction
                    // pool. Its compiler-builtin bit plus canonical capacity
                    // spelling is the durable classification; unrelated source
                    // structs never acquire that bit.
                    SemanticImportType::BuiltinNominal {
                        name: def.name.clone(),
                        kind: SemanticImportNominalKind::Struct,
                    }
                } else {
                    nominal_import_type(
                        self.nominal_exports
                            .get(&LocalNominal::Struct(id))
                            .cloned()
                            .ok_or(SemanticImportFailure::ForeignLocalType)?,
                    )
                }
            }
            crate::TypeKind::Enum(id) => {
                if let Some((name, kind)) = self.builtin_exports.get(&LocalNominal::Enum(id)) {
                    SemanticImportType::BuiltinNominal {
                        name: name.clone(),
                        kind: *kind,
                    }
                } else {
                    nominal_import_type(
                        self.nominal_exports
                            .get(&LocalNominal::Enum(id))
                            .cloned()
                            .ok_or(SemanticImportFailure::ForeignLocalType)?,
                    )
                }
            }
            crate::TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                SemanticImportType::Array {
                    element: Arc::new(self.export_type_local_validated(element)?),
                    len,
                }
            }
            crate::TypeKind::PtrConst(id) => SemanticImportType::PtrConst(Arc::new(
                self.export_type_local_validated(self.type_pool.ptr_const_def(id))?,
            )),
            crate::TypeKind::PtrMut(id) => SemanticImportType::PtrMut(Arc::new(
                self.export_type_local_validated(self.type_pool.ptr_mut_def(id))?,
            )),
            crate::TypeKind::Module(id) => SemanticImportType::Module(
                self.module_exports
                    .get(&id)
                    .cloned()
                    .ok_or(SemanticImportFailure::ForeignLocalType)?,
            ),
            crate::TypeKind::Error => return Err(SemanticImportFailure::ForeignLocalType),
        })
    }

    pub fn export_const_value(
        &self,
        value: SemanticImportedConstValue,
    ) -> Result<SemanticImportConstValue<K, M>, SemanticImportFailure> {
        if !Arc::ptr_eq(&value.epoch, &self.epoch) {
            return Err(SemanticImportFailure::ForeignLocalValue);
        }
        Ok(match value.value {
            ConstValue::Integer(v) => SemanticImportConstValue::Integer(v),
            ConstValue::Bool(v) => SemanticImportConstValue::Bool(v),
            ConstValue::Type(v) => SemanticImportConstValue::Type(self.export_type_local(v)?),
            ConstValue::Function(symbol) => {
                let Some(FunctionInstanceKey::Definition(key)) =
                    self.function_exports.get(&symbol.spur())
                else {
                    return Err(SemanticImportFailure::ForeignLocalValue);
                };
                SemanticImportConstValue::Function(key.clone())
            }
            ConstValue::Unit => SemanticImportConstValue::Unit,
            ConstValue::String(content) => {
                SemanticImportConstValue::String(Arc::from(self.interner.resolve(&content.spur())))
            }
            ConstValue::Float(content) => {
                SemanticImportConstValue::Float(Arc::from(self.interner.resolve(&content.spur())))
            }
        })
    }

    pub fn complete_struct(
        &self,
        key: &K,
        fields: &[(Arc<str>, SemanticImportType<K, M>)],
        is_copy: bool,
        is_linear: bool,
        declared_linear: bool,
    ) -> Result<(), SemanticImportFailure> {
        self.complete_nominal_struct(
            &NominalInstanceKey::Named(key.clone()),
            fields,
            is_copy,
            is_linear,
            declared_linear,
        )
    }

    fn complete_nominal_struct(
        &self,
        key: &NominalInstanceKey<K, M>,
        fields: &[(Arc<str>, SemanticImportType<K, M>)],
        is_copy: bool,
        is_linear: bool,
        declared_linear: bool,
    ) -> Result<(), SemanticImportFailure> {
        self.type_pool.transaction(|type_pool| {
            self.complete_nominal_struct_in_pool(
                type_pool,
                key,
                fields,
                is_copy,
                is_linear,
                declared_linear,
            )
        })
    }

    fn complete_nominal_struct_in_pool(
        &self,
        type_pool: &TypeInternPool,
        key: &NominalInstanceKey<K, M>,
        fields: &[(Arc<str>, SemanticImportType<K, M>)],
        is_copy: bool,
        is_linear: bool,
        declared_linear: bool,
    ) -> Result<(), SemanticImportFailure> {
        let LocalNominal::Struct(id) = self
            .nominals
            .get(key)
            .copied()
            .ok_or(SemanticImportFailure::MissingNominal)?
        else {
            return Err(SemanticImportFailure::NominalKindMismatch);
        };
        let metadata = type_pool.struct_declaration_metadata(id).ok_or_else(|| {
            if type_pool.try_struct_def(id).is_some() {
                SemanticImportFailure::NominalAlreadyComplete
            } else {
                SemanticImportFailure::NominalKindMismatch
            }
        })?;
        let fields: Vec<StructField> = fields
            .iter()
            .map(|(name, ty)| {
                Ok(StructField {
                    name: name.to_string(),
                    ty: self.import_type_local_with(ty, type_pool, None)?,
                })
            })
            .collect::<Result<_, _>>()?;
        let is_copy = if matches!(key, NominalInstanceKey::Anonymous(_)) {
            is_copy
                && fields
                    .iter()
                    .all(|field| field.ty.is_copy_in_pool(type_pool))
        } else {
            is_copy
        };
        type_pool.complete_declared_struct(
            id,
            StructDef {
                name: metadata.name,
                fields,
                is_copy,
                is_linear,
                declared_linear,
                destructor: metadata.destructor,
                is_builtin: metadata.is_builtin,
                is_pub: metadata.is_pub,
                file_id: metadata.file_id,
            },
        );
        Ok(())
    }

    pub fn complete_enum(
        &self,
        key: &K,
        variants: &[(Arc<str>, Arc<[SemanticImportType<K, M>]>)],
    ) -> Result<(), SemanticImportFailure> {
        self.complete_nominal_enum(&NominalInstanceKey::Named(key.clone()), variants)
    }

    fn complete_nominal_enum(
        &self,
        key: &NominalInstanceKey<K, M>,
        variants: &[(Arc<str>, Arc<[SemanticImportType<K, M>]>)],
    ) -> Result<(), SemanticImportFailure> {
        self.type_pool
            .transaction(|type_pool| self.complete_nominal_enum_in_pool(type_pool, key, variants))
    }

    fn complete_nominal_enum_in_pool(
        &self,
        type_pool: &TypeInternPool,
        key: &NominalInstanceKey<K, M>,
        variants: &[(Arc<str>, Arc<[SemanticImportType<K, M>]>)],
    ) -> Result<(), SemanticImportFailure> {
        let LocalNominal::Enum(id) = self
            .nominals
            .get(key)
            .copied()
            .ok_or(SemanticImportFailure::MissingNominal)?
        else {
            return Err(SemanticImportFailure::NominalKindMismatch);
        };
        let metadata = type_pool.enum_declaration_metadata(id).ok_or_else(|| {
            if type_pool.try_enum_def(id).is_some() {
                SemanticImportFailure::NominalAlreadyComplete
            } else {
                SemanticImportFailure::NominalKindMismatch
            }
        })?;
        let variant_payloads = variants
            .iter()
            .map(|(_, payload)| {
                payload
                    .iter()
                    .map(|ty| self.import_type_local_with(ty, type_pool, None))
                    .collect()
            })
            .collect::<Result<_, _>>()?;
        type_pool.complete_declared_enum(
            id,
            EnumDef {
                name: metadata.name,
                variants: variants.iter().map(|(name, _)| name.clone()).collect(),
                variant_payloads,
                is_pub: metadata.is_pub,
                is_non_exhaustive: metadata.is_non_exhaustive,
                file_id: metadata.file_id,
            },
        );
        Ok(())
    }

    /// Complete one constructor's nominal universe behind one rollback
    /// boundary. The epoch is still unpublished, so cloning the growing type
    /// pool once per nominal would add no isolation beyond this batch.
    fn complete_local_nominals(
        &self,
        nominals: &[SemanticLocalNominal<K, M>],
    ) -> Result<(), SemanticImportFailure> {
        if nominals.is_empty() {
            return Ok(());
        }
        self.type_pool.transaction(|type_pool| {
            for nominal in nominals {
                match &nominal.shape {
                    SemanticLocalNominalShape::Struct {
                        fields,
                        is_copy,
                        is_linear,
                        declared_linear,
                        destructor,
                    } => {
                        self.complete_nominal_struct_in_pool(
                            type_pool,
                            &nominal.key,
                            fields,
                            *is_copy,
                            *is_linear,
                            *declared_linear,
                        )?;
                        if let Some(destructor) = destructor {
                            let symbol = self
                                .functions
                                .get(destructor)
                                .copied()
                                .ok_or(SemanticImportFailure::MissingFunction)?;
                            let Some(LocalNominal::Struct(id)) = self.nominals.get(&nominal.key)
                            else {
                                return Err(SemanticImportFailure::NominalKindMismatch);
                            };
                            type_pool.set_struct_destructor(
                                *id,
                                self.interner.resolve(&symbol).to_owned(),
                            );
                        }
                    }
                    SemanticLocalNominalShape::Enum { variants, .. } => {
                        self.complete_nominal_enum_in_pool(type_pool, &nominal.key, variants)?;
                    }
                }
            }
            Ok(())
        })
    }

    pub fn type_pool(&self) -> &TypeInternPool {
        &self.type_pool
    }
    pub fn interner(&self) -> &ThreadedRodeo {
        self.interner.as_ref()
    }
    pub fn module_registry(&self) -> &ModuleRegistry {
        &self.module_registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TypeKind;

    type Epoch = SemanticImportEpoch<&'static str, &'static str>;
    type ImportType = SemanticImportType<&'static str, &'static str>;

    fn nominal(
        key: &'static str,
        name: &'static str,
        kind: SemanticImportNominalKind,
    ) -> SemanticImportNominal<&'static str> {
        SemanticImportNominal {
            key,
            module_path: Arc::from("pkg/main.rue"),
            name: Arc::from(name),
            kind,
            is_public: true,
            is_non_exhaustive: false,
            lang_item: None,
        }
    }

    #[test]
    fn canonical_schema_kinds_have_stable_unique_tags_and_names() {
        assert_eq!(SEMANTIC_IMPORT_TYPE_KINDS.len(), 24);
        for (tag, kind) in SEMANTIC_IMPORT_TYPE_KINDS.iter().copied().enumerate() {
            assert_eq!(usize::from(kind.schema_tag()), tag);
            assert_eq!(kind.to_string(), kind.display_name());
            assert!(
                SEMANTIC_IMPORT_TYPE_KINDS[..tag]
                    .iter()
                    .all(|earlier| earlier.display_name() != kind.display_name())
            );
        }

        assert_eq!(SEMANTIC_IMPORT_CONST_KINDS.len(), 7);
        for (tag, kind) in SEMANTIC_IMPORT_CONST_KINDS.iter().copied().enumerate() {
            assert_eq!(usize::from(kind.schema_tag()), tag);
            assert_eq!(kind.to_string(), kind.display_name());
            assert!(
                SEMANTIC_IMPORT_CONST_KINDS[..tag]
                    .iter()
                    .all(|earlier| earlier.display_name() != kind.display_name())
            );
        }
    }

    fn projection(epoch: &Epoch, ty: Type) -> String {
        match ty.kind() {
            TypeKind::I32 => "i32".into(),
            TypeKind::Bool => "bool".into(),
            TypeKind::Struct(id) => format!("struct {}", epoch.type_pool().struct_def(id).name),
            TypeKind::Enum(id) => format!("enum {}", epoch.type_pool().enum_def(id).name),
            TypeKind::PtrConst(id) => format!(
                "ptr const {}",
                projection(epoch, epoch.type_pool().ptr_const_def(id))
            ),
            TypeKind::PtrMut(id) => format!(
                "ptr mut {}",
                projection(epoch, epoch.type_pool().ptr_mut_def(id))
            ),
            TypeKind::Array(id) => {
                let (element, len) = epoch.type_pool().array_def(id);
                format!("[{element}; {len}]", element = projection(epoch, element))
            }
            TypeKind::Module(id) => {
                format!("module {}", epoch.module_registry().get_def(id).file_path)
            }
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn canonical_identity_visitor_relocates_every_type_and_const_variant() {
        let types = vec![
            ImportType::I8,
            ImportType::I16,
            ImportType::I32,
            ImportType::I64,
            ImportType::U8,
            ImportType::U16,
            ImportType::U32,
            ImportType::U64,
            ImportType::Bool,
            ImportType::Unit,
            ImportType::Never,
            ImportType::ComptimeType,
            ImportType::F32,
            ImportType::F64,
            ImportType::ComptimeFloat,
            ImportType::BuiltinNominal {
                name: Arc::from("str"),
                kind: SemanticImportNominalKind::Struct,
            },
            ImportType::Nominal("Record"),
            ImportType::Array {
                element: Arc::new(ImportType::Nominal("Record")),
                len: 7,
            },
            ImportType::Slice {
                element: Arc::new(ImportType::Nominal("Record")),
                name: Arc::from("[Record]"),
            },
            ImportType::PtrConst(Arc::new(ImportType::Nominal("Record"))),
            ImportType::PtrMut(Arc::new(ImportType::Nominal("Record"))),
            ImportType::Module("pkg/main.rue"),
            ImportType::GenericParameter(3),
            ImportType::AnonymousNominal(crate::AnonymousNominalKey {
                kind: crate::AnonymousNominalKind::Struct,
                producer: crate::StableProducerId::Definition("make"),
                anchor: rue_rir::RirStructuralAnchor::new(vec![
                    rue_rir::RirStructuralPathSegment::Body,
                    rue_rir::RirStructuralPathSegment::AnonymousType(0),
                ]),
            }),
        ];
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Tag {
            I8,
            I16,
            I32,
            I64,
            U8,
            U16,
            U32,
            U64,
            Bool,
            Unit,
            Never,
            Comptime,
            F32,
            F64,
            ComptimeFloat,
            Builtin,
            Nominal,
            Array,
            Slice,
            PtrConst,
            PtrMut,
            Module,
            Generic,
            AnonymousNominal,
        }
        let expected = [
            Tag::I8,
            Tag::I16,
            Tag::I32,
            Tag::I64,
            Tag::U8,
            Tag::U16,
            Tag::U32,
            Tag::U64,
            Tag::Bool,
            Tag::Unit,
            Tag::Never,
            Tag::Comptime,
            Tag::F32,
            Tag::F64,
            Tag::ComptimeFloat,
            Tag::Builtin,
            Tag::Nominal,
            Tag::Array,
            Tag::Slice,
            Tag::PtrConst,
            Tag::PtrMut,
            Tag::Module,
            Tag::Generic,
            Tag::AnonymousNominal,
        ];
        for (ty, expected) in types.into_iter().zip(expected) {
            use SemanticImportTypeFold as F;
            let tag = ty
                .try_fold(&mut |node| {
                    Ok::<_, ()>(match node {
                        F::I8 => Tag::I8,
                        F::I16 => Tag::I16,
                        F::I32 => Tag::I32,
                        F::I64 => Tag::I64,
                        F::U8 => Tag::U8,
                        F::U16 => Tag::U16,
                        F::U32 => Tag::U32,
                        F::U64 => Tag::U64,
                        F::Bool => Tag::Bool,
                        F::Unit => Tag::Unit,
                        F::Never => Tag::Never,
                        F::ComptimeType => Tag::Comptime,
                        F::F32 => Tag::F32,
                        F::F64 => Tag::F64,
                        F::ComptimeFloat => Tag::ComptimeFloat,
                        F::BuiltinNominal { .. } => Tag::Builtin,
                        F::Nominal(_) => Tag::Nominal,
                        F::Array { .. } => Tag::Array,
                        F::Slice { .. } => Tag::Slice,
                        F::PtrConst(_) => Tag::PtrConst,
                        F::PtrMut(_) => Tag::PtrMut,
                        F::Module(_) => Tag::Module,
                        F::GenericParameter(_) => Tag::Generic,
                        F::AnonymousNominal(_) => Tag::AnonymousNominal,
                    })
                })
                .unwrap();
            assert_eq!(tag, expected);
            let mapped = ty
                .try_map_identities(&|key| Ok::<_, ()>(format!("key:{key}")), &|module| {
                    Ok::<_, ()>(format!("module:{module}"))
                })
                .unwrap();
            assert!(!format!("{mapped:?}").is_empty());
        }

        let values = [
            SemanticImportConstValue::Integer(1),
            SemanticImportConstValue::Bool(true),
            SemanticImportConstValue::Type(ImportType::Module("pkg/main.rue")),
            SemanticImportConstValue::Function("callable"),
            SemanticImportConstValue::Unit,
            SemanticImportConstValue::String(std::sync::Arc::from("hello")),
        ];
        for value in values {
            let mapped = value
                .try_map_identities(&|key| Ok::<_, ()>(format!("key:{key}")), &|module| {
                    Ok::<_, ()>(format!("module:{module}"))
                })
                .unwrap();
            assert!(!format!("{mapped:?}").is_empty());
        }
    }

    #[test]
    fn fresh_epochs_remap_to_equivalent_values_despite_different_local_ids() {
        let a = Epoch::new(
            vec![nominal("node", "Node", SemanticImportNominalKind::Struct)],
            vec![("f", Arc::from("f"))],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        let b = Epoch::new(
            vec![
                nominal("noise", "Aardvark", SemanticImportNominalKind::Enum),
                nominal("node", "Node", SemanticImportNominalKind::Struct),
            ],
            vec![("f", Arc::from("f"))],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        a.complete_struct(&"node", &[], false, false, false)
            .unwrap();
        b.complete_struct(&"node", &[], false, false, false)
            .unwrap();
        let durable = ImportType::PtrConst(Arc::new(ImportType::Nominal("node")));
        let ta = a.import_type(&durable).unwrap();
        let tb = b.import_type(&durable).unwrap();
        assert_ne!(
            ta.value, tb.value,
            "the test must exercise different request-local IDs"
        );
        assert_eq!(projection(&a, ta.value), projection(&b, tb.value));
        let va = a
            .import_const_value(&SemanticImportConstValue::Function("f"))
            .unwrap();
        let vb = b
            .import_const_value(&SemanticImportConstValue::Function("f"))
            .unwrap();
        assert_eq!(
            matches!(va.value, ConstValue::Function(_)),
            matches!(vb.value, ConstValue::Function(_))
        );
    }

    #[test]
    fn two_phase_shells_support_cycles_and_input_order_is_irrelevant() {
        let declarations = vec![
            nominal("right", "Right", SemanticImportNominalKind::Struct),
            nominal("left", "Left", SemanticImportNominalKind::Struct),
        ];
        let first = Epoch::new(declarations.clone(), vec![], vec!["pkg/main.rue"]).unwrap();
        let second = Epoch::new(
            declarations.into_iter().rev().collect(),
            vec![],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        for epoch in [&first, &second] {
            let left = epoch.nominals[&NominalInstanceKey::Named("left")];
            let LocalNominal::Struct(left) = left else {
                panic!("left must be a struct")
            };
            let left_ty = Type::new_struct(left);
            assert!(epoch.type_pool().get(left_ty).is_none());
            assert!(epoch.type_pool().try_struct_def(left).is_none());
            assert_eq!(
                epoch.type_pool().validate_complete_type(left_ty),
                Err(crate::TypeValidationError::IncompleteDefinition)
            );
        }
        let left_fields = [(
            Arc::from("next"),
            ImportType::PtrConst(Arc::new(ImportType::Nominal("right"))),
        )];
        let right_fields = [(
            Arc::from("next"),
            ImportType::PtrMut(Arc::new(ImportType::Nominal("left"))),
        )];
        first
            .complete_struct(&"left", &left_fields, false, false, false)
            .unwrap();
        first
            .complete_struct(&"right", &right_fields, false, false, false)
            .unwrap();
        second
            .complete_struct(&"right", &right_fields, false, false, false)
            .unwrap();
        second
            .complete_struct(&"left", &left_fields, false, false, false)
            .unwrap();
        for key in ["left", "right"] {
            let ty = ImportType::Nominal(key);
            assert_eq!(
                projection(&first, first.import_type(&ty).unwrap().value),
                projection(&second, second.import_type(&ty).unwrap().value)
            );
        }
        let LocalNominal::Struct(left) = first.nominals[&NominalInstanceKey::Named("left")] else {
            panic!("left must be a struct")
        };
        assert!(first.type_pool().try_struct_def(left).is_some());
        assert_eq!(
            first.complete_struct(&"left", &left_fields, false, false, false),
            Err(SemanticImportFailure::NominalAlreadyComplete)
        );
    }

    #[test]
    fn enum_shell_completes_once_and_public_reads_are_complete_only() {
        let epoch = Epoch::new(
            vec![nominal("choice", "Choice", SemanticImportNominalKind::Enum)],
            vec![],
            vec![],
        )
        .unwrap();
        let LocalNominal::Enum(id) = epoch.nominals[&NominalInstanceKey::Named("choice")] else {
            panic!("choice must be an enum")
        };
        let ty = Type::new_enum(id);
        assert!(epoch.type_pool().get(ty).is_none());
        assert!(epoch.type_pool().try_enum_def(id).is_none());

        let variants = [(Arc::from("Value"), Arc::from([ImportType::I32]))];
        epoch.complete_enum(&"choice", &variants).unwrap();
        assert_eq!(
            epoch.type_pool().enum_def(id).variant_payloads,
            vec![vec![Type::I32]]
        );
        assert_eq!(
            epoch.complete_enum(&"choice", &variants),
            Err(SemanticImportFailure::NominalAlreadyComplete)
        );
    }

    #[test]
    fn imported_enum_shell_preserves_non_exhaustive_metadata() {
        let mut imported = nominal("colors", "Color", SemanticImportNominalKind::Enum);
        imported.is_non_exhaustive = true;
        let epoch = Epoch::new(vec![imported], vec![], vec![]).unwrap();
        let LocalNominal::Enum(id) = epoch.nominals[&NominalInstanceKey::Named("colors")] else {
            panic!("colors must be an enum")
        };
        epoch.complete_enum(&"colors", &[]).unwrap();
        assert!(epoch.type_pool().enum_def(id).is_non_exhaustive);
    }

    #[test]
    fn nominal_completion_is_atomic_after_earlier_structural_imports() {
        let epoch = Epoch::new(
            vec![
                nominal("node", "Node", SemanticImportNominalKind::Struct),
                nominal("choice", "Choice", SemanticImportNominalKind::Enum),
            ],
            vec![],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        let before = epoch.type_pool().stats();
        let fields = [
            (
                Arc::from("good"),
                ImportType::Array {
                    element: Arc::new(ImportType::U8),
                    len: 4,
                },
            ),
            (
                Arc::from("bad"),
                ImportType::PtrConst(Arc::new(ImportType::Module("pkg/main.rue"))),
            ),
        ];

        assert_eq!(
            epoch.complete_struct(&"node", &fields, false, false, false),
            Err(SemanticImportFailure::InvalidStructuralType)
        );
        assert_eq!(epoch.type_pool().stats(), before);
        assert_eq!(epoch.type_pool().get_array(Type::U8, 4), None);
        let LocalNominal::Struct(id) = epoch.nominals[&NominalInstanceKey::Named("node")] else {
            panic!("node must be a struct")
        };
        assert!(epoch.type_pool().try_struct_def(id).is_none());
        assert!(epoch.type_pool().struct_declaration_metadata(id).is_some());

        let variants = [(
            Arc::from("Value"),
            Arc::from([
                ImportType::Array {
                    element: Arc::new(ImportType::U16),
                    len: 5,
                },
                ImportType::PtrMut(Arc::new(ImportType::Module("pkg/main.rue"))),
            ]),
        )];
        assert_eq!(
            epoch.complete_enum(&"choice", &variants),
            Err(SemanticImportFailure::InvalidStructuralType)
        );
        assert_eq!(epoch.type_pool().stats(), before);
        assert_eq!(epoch.type_pool().get_array(Type::U16, 5), None);
        let LocalNominal::Enum(id) = epoch.nominals[&NominalInstanceKey::Named("choice")] else {
            panic!("choice must be an enum")
        };
        assert!(epoch.type_pool().try_enum_def(id).is_none());
        assert!(epoch.type_pool().enum_declaration_metadata(id).is_some());
    }

    #[test]
    fn local_nominal_batch_rolls_back_every_completion() {
        let epoch = Epoch::new(
            vec![
                nominal("good", "Good", SemanticImportNominalKind::Struct),
                nominal("bad", "Bad", SemanticImportNominalKind::Struct),
            ],
            vec![],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        let facts = vec![
            SemanticLocalNominal {
                key: NominalInstanceKey::Named("good"),
                module_path: Arc::from("pkg/main.rue"),
                name: Arc::from("Good"),
                kind: SemanticImportNominalKind::Struct,
                is_public: false,
                lang_item: None,
                shape: SemanticLocalNominalShape::Struct {
                    fields: Arc::new([(
                        Arc::from("items"),
                        ImportType::Array {
                            element: Arc::new(ImportType::U8),
                            len: 4,
                        },
                    )]),
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                },
            },
            SemanticLocalNominal {
                key: NominalInstanceKey::Named("bad"),
                module_path: Arc::from("pkg/main.rue"),
                name: Arc::from("Bad"),
                kind: SemanticImportNominalKind::Struct,
                is_public: false,
                lang_item: None,
                shape: SemanticLocalNominalShape::Struct {
                    fields: Arc::new([(
                        Arc::from("invalid"),
                        ImportType::PtrConst(Arc::new(ImportType::Module("pkg/main.rue"))),
                    )]),
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                },
            },
        ];
        let before = epoch.type_pool().stats();

        assert_eq!(
            epoch.complete_local_nominals(&facts),
            Err(SemanticImportFailure::InvalidStructuralType)
        );

        assert_eq!(epoch.type_pool().stats(), before);
        assert_eq!(epoch.type_pool().get_array(Type::U8, 4), None);
        for key in ["good", "bad"] {
            let LocalNominal::Struct(id) = epoch.nominals[&NominalInstanceKey::Named(key)] else {
                panic!("{key} must be a struct")
            };
            assert!(epoch.type_pool().try_struct_def(id).is_none());
            assert!(epoch.type_pool().struct_declaration_metadata(id).is_some());
        }
    }

    #[test]
    fn missing_context_and_foreign_values_fail_closed() {
        let epoch = Epoch::new(vec![], vec![], vec![]).unwrap();
        assert_eq!(
            epoch.import_type(&ImportType::Nominal("missing")),
            Err(SemanticImportFailure::MissingNominal)
        );
        assert_eq!(
            epoch.import_type(&ImportType::Module("missing")),
            Err(SemanticImportFailure::MissingModule)
        );
        assert_eq!(
            epoch.import_type(&ImportType::GenericParameter(0)),
            Err(SemanticImportFailure::GenericParameterNeedsDeclarationContext)
        );
        assert_eq!(
            epoch.import_const_value(&SemanticImportConstValue::Function("missing")),
            Err(SemanticImportFailure::MissingFunction)
        );

        let epoch = Epoch::new(vec![], vec![], vec!["pkg/main.rue"]).unwrap();
        assert_eq!(
            epoch.import_type(&ImportType::Array {
                element: Arc::new(ImportType::ComptimeType),
                len: 1,
            }),
            Err(SemanticImportFailure::InvalidStructuralType)
        );
        assert_eq!(
            epoch.import_type(&ImportType::PtrConst(Arc::new(ImportType::Module(
                "pkg/main.rue",
            )))),
            Err(SemanticImportFailure::InvalidStructuralType)
        );
    }

    #[test]
    fn callable_generic_environment_is_ordered_bounded_and_atomic() {
        let epoch = Epoch::new(vec![], vec![], vec![]).unwrap();
        let valid = [
            (ImportType::ComptimeType, true),
            (ImportType::GenericParameter(0), false),
        ];
        epoch
            .validate_callable_signature(&valid, &ImportType::GenericParameter(0))
            .unwrap();

        let before = epoch.type_pool().stats();
        let invalid = [
            (ImportType::ComptimeType, true),
            (
                ImportType::Array {
                    element: Arc::new(ImportType::U8),
                    len: 4,
                },
                false,
            ),
            (
                ImportType::Array {
                    element: Arc::new(ImportType::PtrConst(Arc::new(
                        ImportType::GenericParameter(1),
                    ))),
                    len: 2,
                },
                false,
            ),
        ];
        assert_eq!(
            epoch.validate_callable_signature(&invalid, &ImportType::GenericParameter(0)),
            Err(SemanticImportFailure::GenericParameterOutOfRange)
        );
        assert_eq!(epoch.type_pool().stats(), before);
    }

    #[test]
    fn supported_types_and_values_round_trip_exactly() {
        let epoch = Epoch::new(
            vec![nominal("node", "Node", SemanticImportNominalKind::Struct)],
            vec![("f", Arc::from("f"))],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        epoch
            .complete_struct(&"node", &[], false, false, false)
            .unwrap();
        let values = [
            ImportType::Array {
                element: Arc::new(ImportType::PtrConst(Arc::new(ImportType::Nominal("node")))),
                len: 7,
            },
            ImportType::Module("pkg/main.rue"),
            ImportType::Bool,
        ];
        for value in values {
            assert_eq!(
                epoch
                    .export_type(epoch.import_type(&value).unwrap())
                    .unwrap(),
                value
            );
        }
        assert_eq!(
            epoch
                .import_types(&[ImportType::Bool, ImportType::I32])
                .unwrap()
                .as_ref(),
            &[
                SemanticImportedType {
                    epoch: epoch.epoch.clone(),
                    value: Type::BOOL
                },
                SemanticImportedType {
                    epoch: epoch.epoch.clone(),
                    value: Type::I32
                },
            ]
        );
        let value = SemanticImportConstValue::Function("f");
        assert_eq!(
            epoch
                .export_const_value(epoch.import_const_value(&value).unwrap())
                .unwrap(),
            value
        );
    }

    #[test]
    fn large_local_epoch_indexes_every_reverse_identity_join() {
        type OwnedEpoch = SemanticImportEpoch<String, String>;
        type OwnedType = SemanticImportType<String, String>;

        let mut nominals = (0..24)
            .map(|index| SemanticLocalNominal {
                key: NominalInstanceKey::Named(format!("nominal-{index}")),
                module_path: Arc::from(format!("module-{index}")),
                name: Arc::from(format!("Nominal{index}")),
                kind: SemanticImportNominalKind::Struct,
                is_public: true,
                lang_item: None,
                shape: SemanticLocalNominalShape::Struct {
                    fields: Arc::new([]),
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                },
            })
            .collect::<Vec<_>>();
        let anonymous = crate::AnonymousNominalKey {
            kind: crate::AnonymousNominalKind::Struct,
            producer: crate::StableProducerId::Definition("producer".to_string()),
            anchor: rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::Body,
                rue_rir::RirStructuralPathSegment::AnonymousType(0),
            ]),
        };
        nominals.push(SemanticLocalNominal {
            key: NominalInstanceKey::Anonymous(Node::new(anonymous.clone())),
            module_path: Arc::from("module-anonymous"),
            name: Arc::from("Anonymous"),
            kind: SemanticImportNominalKind::Struct,
            is_public: false,
            lang_item: None,
            shape: SemanticLocalNominalShape::Struct {
                fields: Arc::new([]),
                is_copy: false,
                is_linear: false,
                declared_linear: false,
                destructor: None,
            },
        });
        let callables = (0..24)
            .map(|index| SemanticLocalCallable {
                key: FunctionInstanceKey::Definition(format!("function-{index}")),
                symbol: Arc::from(format!("function#{index}")),
            })
            .collect::<Vec<_>>();
        let modules = (0..24)
            .map(|index| format!("module-{index}"))
            .collect::<Vec<_>>();
        let epoch = OwnedEpoch::new_local(nominals, callables, modules).unwrap();

        for stable in [
            OwnedType::Nominal("nominal-23".to_string()),
            OwnedType::AnonymousNominal(anonymous),
            OwnedType::Module("module-23".to_string()),
            OwnedType::BuiltinNominal {
                name: Arc::from("Arch"),
                kind: SemanticImportNominalKind::Enum,
            },
        ] {
            assert_eq!(
                epoch
                    .export_type(epoch.import_type(&stable).unwrap())
                    .unwrap(),
                stable
            );
        }
        let callable = SemanticImportConstValue::Function("function-23".to_string());
        assert_eq!(
            epoch
                .export_const_value(epoch.import_const_value(&callable).unwrap())
                .unwrap(),
            callable
        );
        assert_eq!(epoch.nominal_exports.len(), 25);
        assert_eq!(epoch.function_exports.len(), 24);
        assert_eq!(epoch.module_exports.len(), 24);
        assert_eq!(epoch.builtin_exports.len(), epoch.builtins.len());
    }

    #[test]
    fn foreign_epoch_values_fail_closed_even_when_raw_ids_alias() {
        let a = Epoch::new(
            vec![nominal("node", "Node", SemanticImportNominalKind::Struct)],
            vec![("f", Arc::from("f"))],
            vec![],
        )
        .unwrap();
        let b = Epoch::new(
            vec![nominal("node", "Node", SemanticImportNominalKind::Struct)],
            vec![("f", Arc::from("f"))],
            vec![],
        )
        .unwrap();
        let ty = a.import_type(&ImportType::Nominal("node")).unwrap();
        let value = a
            .import_const_value(&SemanticImportConstValue::Function("f"))
            .unwrap();
        assert_eq!(
            b.export_type(ty),
            Err(SemanticImportFailure::ForeignLocalType)
        );
        assert_eq!(
            b.export_const_value(value),
            Err(SemanticImportFailure::ForeignLocalValue)
        );
        assert_eq!(
            a.export_type(SemanticImportedType {
                epoch: a.epoch.clone(),
                value: Type::ERROR,
            }),
            Err(SemanticImportFailure::ForeignLocalType),
            "the checked root boundary must still reject a branded recovery type"
        );
    }

    #[test]
    fn duplicate_stable_and_local_identities_are_rejected() {
        assert!(matches!(
            Epoch::new(
                vec![],
                vec![("f", Arc::from("a")), ("f", Arc::from("b"))],
                vec![]
            ),
            Err(SemanticImportFailure::DuplicateFunction)
        ));
        assert!(matches!(
            Epoch::new(
                vec![],
                vec![("a", Arc::from("same")), ("b", Arc::from("same"))],
                vec![]
            ),
            Err(SemanticImportFailure::DuplicateFunctionLocalIdentity)
        ));
        assert!(matches!(
            Epoch::new(
                vec![
                    nominal("a", "Node", SemanticImportNominalKind::Struct),
                    nominal("b", "Node", SemanticImportNominalKind::Struct),
                ],
                vec![],
                vec![]
            ),
            Err(SemanticImportFailure::DuplicateNominalLocalIdentity)
        ));
    }

    fn body(
        data: Vec<crate::SemanticBodyInstData<&'static str, &'static str>>,
    ) -> crate::SemanticBody<&'static str, &'static str> {
        use crate::{SemanticBody, SemanticBodyAnchor, SemanticBodyInst, SemanticImportType};
        SemanticBody {
            is_accessor: false,
            return_type: SemanticImportType::I32,
            instructions: data
                .into_iter()
                .map(|data| SemanticBodyInst {
                    data,
                    ty: SemanticImportType::I32,
                    anchor: SemanticBodyAnchor { start: 1, end: 2 },
                })
                .collect::<Vec<_>>()
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

    #[test]
    fn structured_body_import_rebuilds_packed_air_in_fresh_epoch() {
        use crate::SemanticBodyInstData as D;
        let epoch =
            Epoch::new(vec![], vec![("callee", Arc::from("callee#stable"))], vec![]).unwrap();
        let input = body(vec![
            D::Const(20),
            D::Const(22),
            D::Add(0, 1),
            D::Call {
                function: FunctionInstanceKey::Definition("callee"),
                args: vec![crate::SemanticBodyCallArg {
                    value: 2,
                    mode: crate::AirArgMode::Normal,
                }]
                .into(),
            },
            D::Ret(Some(3)),
        ]);
        let imported = epoch
            .import_body(&input, Span::with_file(FileId::new(9), 100, 200))
            .unwrap();
        assert_eq!(imported.air.len(), 5);
        assert_eq!(
            imported.air.get(crate::AirRef::from_raw(4)).span.file_id,
            FileId::new(9)
        );
        let crate::AirInstData::Call { ref args, .. } =
            imported.air.get(crate::AirRef::from_raw(3)).data
        else {
            panic!("call not reconstructed")
        };
        assert_eq!(
            imported
                .air
                .get_call_args(args)
                .next()
                .unwrap()
                .value
                .as_u32(),
            2
        );
    }

    fn local_callable(
        key: FunctionInstanceKey<&'static str, &'static str>,
        symbol: &'static str,
    ) -> SemanticLocalCallable<&'static str, &'static str> {
        SemanticLocalCallable {
            key,
            symbol: Arc::from(symbol),
        }
    }

    fn materialize_local(
        identity: FunctionInstanceKey<&'static str, &'static str>,
        callable_kind: crate::AnalyzedCallableKind,
        input: &SemanticBody<&'static str, &'static str>,
        nominals: Vec<SemanticLocalNominal<&'static str, &'static str>>,
        callables: Vec<SemanticLocalCallable<&'static str, &'static str>>,
    ) -> Result<SemanticLocalMaterialization<&'static str, &'static str>, SemanticBodyImportFailure>
    {
        let epoch = Epoch::new_local(nominals, callables, vec!["main"]).unwrap();
        epoch.materialize_local_body(
            identity,
            callable_kind,
            input,
            Span::with_file(FileId::new(7), 100, 200),
        )
    }

    #[test]
    fn local_materialization_owns_ordinary_air_strings_and_domains() {
        use crate::SemanticBodyInstData as D;
        let identity = FunctionInstanceKey::Definition("main");
        let mut input = body(vec![
            D::Const(42),
            D::Intrinsic {
                operation: crate::IntrinsicOperation::BitCast,
                name: Arc::from("bitCast"),
                args: vec![crate::SemanticBodyCallArg {
                    value: 0,
                    mode: crate::AirArgMode::Normal,
                }]
                .into(),
            },
            D::Ret(Some(1)),
        ]);
        input.strings = vec![Arc::from("body-local")].into();
        input.local_atoms = vec![crate::SemanticBodyLocalAtom {
            identity: crate::LocalAtomId {
                producer: identity.clone(),
                kind: crate::LocalAtomKind::String,
                anchor: rue_rir::RirStructuralAnchor::new(vec![
                    rue_rir::RirStructuralPathSegment::Body,
                    rue_rir::RirStructuralPathSegment::StringLiteral(0),
                ]),
            },
            content: Arc::from("body-local"),
        }]
        .into();
        input.num_locals = 3;
        input.num_param_slots = 2;
        input.param_by_ref = vec![true, false].into();
        input.param_writable = vec![false, true].into();
        input.allow_unreachable_code = true;
        input.warnings = vec![crate::SemanticBodyWarning {
            kind: rue_error::WarningKind::UnreachableCode,
            anchor: crate::SemanticBodyAnchor { start: 3, end: 4 },
            labels: Arc::new([]),
            notes: Arc::new([]),
            helps: Arc::new([]),
            suggestions: Arc::new([]),
        }]
        .into();
        let output = materialize_local(
            identity.clone(),
            crate::AnalyzedCallableKind::Ordinary,
            &input,
            vec![],
            vec![local_callable(identity.clone(), "main")],
        )
        .unwrap();
        assert_eq!(output.identity, identity);
        assert_eq!(output.name, "main");
        assert_eq!(output.strings, ["body-local"]);
        assert_eq!(output.air.len(), 3);
        assert_eq!(output.body_span.file_id, FileId::new(7));
        assert!(output.completeness.is_complete());
        assert_eq!(output.interner.get("main"), output.interner.get("main"));
        assert!(output.interner.get("bitCast").is_some());
        assert_eq!(output.num_locals, 3);
        assert_eq!(output.num_param_slots, 2);
        assert_eq!(output.param_modes.by_ref(), [true, false]);
        assert_eq!(output.param_modes.writable(), [false, true]);
        assert!(output.allow_unreachable_code);
        assert_eq!(output.warnings.len(), 1);
        assert_eq!(output.local_atoms.len(), 1);
        assert_eq!(output.local_atoms[0].identity.producer, identity);
        assert_eq!(output.local_atoms[0].dense_id, 0);
    }

    #[test]
    fn local_materialization_indexes_large_local_atom_payloads() {
        use crate::SemanticBodyInstData as D;
        let identity = FunctionInstanceKey::Definition("main");
        let mut input = body(vec![D::Const(0), D::Ret(Some(0))]);
        let mut strings = vec![Arc::<str>::from("duplicate"), Arc::from("duplicate")];
        strings.extend((0..16).map(|index| Arc::from(format!("literal-{index}"))));
        input.local_atoms = std::iter::once(Arc::from("duplicate"))
            .chain(strings.iter().skip(2).cloned())
            .enumerate()
            .map(|(anchor, content)| crate::SemanticBodyLocalAtom {
                identity: crate::LocalAtomId {
                    producer: identity.clone(),
                    kind: crate::LocalAtomKind::String,
                    anchor: rue_rir::RirStructuralAnchor::new(vec![
                        rue_rir::RirStructuralPathSegment::Body,
                        rue_rir::RirStructuralPathSegment::StringLiteral(anchor as u32),
                    ]),
                },
                content,
            })
            .collect::<Vec<_>>()
            .into();
        input.strings = strings.into();

        let output = materialize_local(
            identity.clone(),
            crate::AnalyzedCallableKind::Ordinary,
            &input,
            vec![],
            vec![local_callable(identity.clone(), "main")],
        )
        .unwrap();

        assert_eq!(output.local_atoms[0].dense_id, 0);
        for (atom, expected) in output.local_atoms.iter().skip(1).zip(2..) {
            assert_eq!(atom.dense_id, expected);
        }

        let mut missing = input.clone();
        Arc::make_mut(&mut missing.local_atoms)[0].content = Arc::from("missing");
        assert!(matches!(
            materialize_local(
                identity.clone(),
                crate::AnalyzedCallableKind::Ordinary,
                &missing,
                vec![],
                vec![local_callable(identity, "main")],
            ),
            Err(SemanticBodyImportFailure::InvalidStringReference)
        ));
    }

    #[test]
    fn local_materialization_resolves_specializations_directly() {
        use crate::SemanticBodyInstData as D;
        let arguments = crate::CanonicalArguments {
            types: vec![TypeInstanceKey::I32].into(),
            values: vec![crate::CanonicalArgumentValue::Integer(7)].into(),
        };
        let owner = FunctionInstanceKey::Specialization {
            base: Node::new(FunctionInstanceKey::Definition("generic")),
            arguments: arguments.clone(),
        };
        let callee = FunctionInstanceKey::Specialization {
            base: Node::new(FunctionInstanceKey::Definition("callee")),
            arguments,
        };
        let input = body(vec![
            D::Const(1),
            D::CallSpecialized {
                identity: crate::SemanticSpecializationIdentity {
                    base: "callee",
                    type_arguments: vec![SemanticImportType::I32].into(),
                    value_arguments: vec![SemanticImportConstValue::Integer(7)].into(),
                },
                args: Arc::new([]),
            },
            D::Ret(Some(1)),
        ]);
        let output = materialize_local(
            owner.clone(),
            crate::AnalyzedCallableKind::Ordinary,
            &input,
            vec![],
            vec![
                local_callable(owner, "generic::<stable>"),
                local_callable(callee, "callee::<stable>"),
            ],
        )
        .unwrap();
        assert!(matches!(
            output.air.get(crate::AirRef::from_raw(1)).data,
            crate::AirInstData::Call { .. }
        ));
    }

    #[test]
    fn local_materialization_round_trips_fixed_and_dynamic_builtin_nominals() {
        use crate::SemanticBodyInstData as D;
        let identity = FunctionInstanceKey::Definition("main");
        let arch_key = NominalInstanceKey::Builtin {
            kind: crate::AnonymousNominalKind::Enum,
            name: Arc::from("Arch"),
        };
        let arch_ty = SemanticImportType::BuiltinNominal {
            name: Arc::from("Arch"),
            kind: SemanticImportNominalKind::Enum,
        };
        let mut fixed = body(vec![
            D::EnumVariant {
                enum_key: arch_key,
                variant_index: 0,
                payload: Arc::new([]),
            },
            D::Ret(Some(0)),
        ]);
        fixed.return_type = arch_ty.clone();
        let mut instructions = fixed.instructions.to_vec();
        instructions[0].ty = arch_ty.clone();
        instructions[1].ty = arch_ty;
        fixed.instructions = instructions.into();
        let fixed = materialize_local(
            identity.clone(),
            crate::AnalyzedCallableKind::Ordinary,
            &fixed,
            vec![],
            vec![local_callable(identity.clone(), "main")],
        )
        .unwrap();
        assert!(fixed.aggregate_types.values().any(|identity| matches!(
            identity,
            TypeInstanceKey::BuiltinNominal { name, kind }
                if name.as_ref() == "Arch" && *kind == crate::AnonymousNominalKind::Enum
        )));

        let fixed_string = SemanticImportType::BuiltinNominal {
            name: Arc::from("Str(8)"),
            kind: SemanticImportNominalKind::Struct,
        };
        let fixed_string_key = NominalInstanceKey::Builtin {
            kind: crate::AnonymousNominalKind::Struct,
            name: Arc::from("Str(8)"),
        };
        let mut dynamic = body(vec![D::PlaceRead { place: 0 }, D::Ret(Some(0))]);
        dynamic.return_type = SemanticImportType::U64;
        dynamic.num_locals = 1;
        dynamic.instructions = vec![
            crate::SemanticBodyInst {
                data: D::PlaceRead { place: 0 },
                ty: SemanticImportType::U64,
                anchor: crate::SemanticBodyAnchor { start: 1, end: 2 },
            },
            crate::SemanticBodyInst {
                data: D::Ret(Some(0)),
                ty: SemanticImportType::U64,
                anchor: crate::SemanticBodyAnchor { start: 2, end: 3 },
            },
        ]
        .into();
        dynamic.places = vec![crate::SemanticBodyPlace {
            base: crate::AirPlaceBase::Local(0),
            base_type: fixed_string,
            projections: vec![crate::SemanticBodyProjection::Field {
                struct_key: fixed_string_key,
                field_index: 1,
            }]
            .into(),
        }]
        .into();
        let dynamic = materialize_local(
            identity.clone(),
            crate::AnalyzedCallableKind::Ordinary,
            &dynamic,
            vec![],
            vec![local_callable(identity, "main")],
        )
        .unwrap();
        assert!(dynamic.aggregate_types.values().any(|identity| matches!(
            identity,
            TypeInstanceKey::BuiltinNominal { name, kind }
                if name.as_ref() == "Str(8)" && *kind == crate::AnonymousNominalKind::Struct
        )));
    }

    #[test]
    fn local_epoch_rejects_fixed_builtin_nominal_facts() {
        let error = Epoch::new_local(
            vec![SemanticLocalNominal {
                key: NominalInstanceKey::Builtin {
                    kind: crate::AnonymousNominalKind::Enum,
                    name: Arc::from("Arch"),
                },
                module_path: Arc::from("<builtin>"),
                name: Arc::from("Arch"),
                kind: SemanticImportNominalKind::Enum,
                is_public: true,
                lang_item: None,
                shape: SemanticLocalNominalShape::Enum {
                    variants: Arc::new([]),
                    is_non_exhaustive: false,
                },
            }],
            vec![],
            vec![],
        )
        .err();
        assert_eq!(error, Some(SemanticImportFailure::BuiltinNominalShadow));
    }

    #[test]
    fn local_epoch_rejects_dynamic_string_builtin_nominal_facts() {
        let error = Epoch::new_local(
            vec![SemanticLocalNominal {
                key: NominalInstanceKey::Builtin {
                    kind: crate::AnonymousNominalKind::Struct,
                    name: Arc::from("Str(8)"),
                },
                module_path: Arc::from("<builtin>"),
                name: Arc::from("Str(8)"),
                kind: SemanticImportNominalKind::Struct,
                is_public: true,
                lang_item: None,
                shape: SemanticLocalNominalShape::Struct {
                    fields: Arc::new([]),
                    is_copy: true,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                },
            }],
            vec![],
            vec![],
        )
        .err();
        assert_eq!(error, Some(SemanticImportFailure::BuiltinNominalShadow));
    }

    fn anonymous_key() -> crate::AnonymousNominalKey<&'static str, &'static str> {
        crate::AnonymousNominalKey {
            kind: crate::AnonymousNominalKind::Struct,
            producer: crate::StableProducerId::Definition("producer"),
            anchor: rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::Body,
                rue_rir::RirStructuralPathSegment::AnonymousType(0),
            ]),
        }
    }

    #[test]
    fn local_materialization_joins_anonymous_nominal_and_member_identity() {
        use crate::SemanticBodyInstData as D;
        let anonymous = anonymous_key();
        let owner_type =
            TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(Node::new(anonymous.clone())));
        let identity = FunctionInstanceKey::AnonymousMember {
            owner: Node::new(owner_type.clone()),
            member: crate::AnonymousMemberKey {
                kind: crate::AnonymousMemberKind::Method,
                name: Arc::from("value"),
            },
        };
        let nominal = SemanticLocalNominal {
            key: NominalInstanceKey::Anonymous(Node::new(anonymous.clone())),
            module_path: Arc::from("main"),
            name: Arc::from("anonymous-record"),
            kind: SemanticImportNominalKind::Struct,
            is_public: false,
            lang_item: None,
            shape: SemanticLocalNominalShape::Struct {
                fields: Arc::new([]),
                is_copy: false,
                is_linear: false,
                declared_linear: false,
                destructor: None,
            },
        };
        let mut input = body(vec![D::UnitConst, D::Ret(None)]);
        input.return_type = SemanticImportType::Unit;
        input.instructions = vec![
            crate::SemanticBodyInst {
                data: D::UnitConst,
                ty: SemanticImportType::Unit,
                anchor: crate::SemanticBodyAnchor { start: 1, end: 2 },
            },
            crate::SemanticBodyInst {
                data: D::Ret(None),
                ty: SemanticImportType::Unit,
                anchor: crate::SemanticBodyAnchor { start: 2, end: 3 },
            },
        ]
        .into();
        let output = materialize_local(
            identity.clone(),
            crate::AnalyzedCallableKind::Ordinary,
            &input,
            vec![nominal],
            vec![local_callable(identity, "anonymous-record.value")],
        )
        .unwrap();
        assert!(output.aggregate_types.values().any(|ty| ty == &owner_type));
        let local = output
            .aggregate_types
            .iter()
            .find_map(|(local, stable)| (stable == &owner_type).then_some(*local))
            .unwrap();
        let struct_id = local.as_struct().unwrap();
        assert!(output.type_pool.is_anonymous_struct(struct_id));
        assert_eq!(
            output.type_pool.struct_symbol_name(struct_id),
            "anonymous-record"
        );
    }

    #[test]
    fn local_materialization_preserves_named_destructor_kind() {
        use crate::SemanticBodyInstData as D;
        let identity = FunctionInstanceKey::Definition("record-drop");
        let mut input = body(vec![D::UnitConst, D::Ret(None)]);
        input.return_type = SemanticImportType::Unit;
        input.instructions = vec![
            crate::SemanticBodyInst {
                data: D::UnitConst,
                ty: SemanticImportType::Unit,
                anchor: crate::SemanticBodyAnchor { start: 1, end: 2 },
            },
            crate::SemanticBodyInst {
                data: D::Ret(None),
                ty: SemanticImportType::Unit,
                anchor: crate::SemanticBodyAnchor { start: 2, end: 3 },
            },
        ]
        .into();
        let output = materialize_local(
            identity.clone(),
            crate::AnalyzedCallableKind::Destructor,
            &input,
            vec![SemanticLocalNominal {
                key: NominalInstanceKey::Named("record"),
                module_path: Arc::from("main"),
                name: Arc::from("Record"),
                kind: SemanticImportNominalKind::Struct,
                is_public: false,
                lang_item: None,
                shape: SemanticLocalNominalShape::Struct {
                    fields: Arc::new([]),
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: Some(identity.clone()),
                },
            }],
            vec![local_callable(identity, "Record.__drop")],
        )
        .unwrap();
        assert_eq!(
            output.callable_kind,
            crate::AnalyzedCallableKind::Destructor
        );
        let record = output
            .aggregate_types
            .iter()
            .find_map(|(ty, stable)| {
                (stable == &TypeInstanceKey::Nominal(NominalInstanceKey::Named("record")))
                    .then_some(*ty)
            })
            .unwrap();
        assert_eq!(
            output
                .type_pool
                .struct_def(record.as_struct().unwrap())
                .destructor
                .as_deref(),
            Some("Record.__drop")
        );
    }

    #[test]
    fn local_materialization_fails_closed_for_missing_exact_facts() {
        use crate::SemanticBodyInstData as D;
        let identity = FunctionInstanceKey::Definition("main");
        let input = body(vec![D::Call {
            function: FunctionInstanceKey::Definition("missing"),
            args: Arc::new([]),
        }]);
        let error = materialize_local(
            identity.clone(),
            crate::AnalyzedCallableKind::Ordinary,
            &input,
            vec![],
            vec![local_callable(identity, "main")],
        )
        .err()
        .expect("missing callable fact must fail");
        assert_eq!(
            error.kind(),
            crate::SemanticBodyImportFailureKind::Semantic(SemanticImportFailure::MissingFunction)
        );
    }

    #[test]
    fn local_materialization_rejects_ambiguous_and_incomplete_fact_sets() {
        let identity = FunctionInstanceKey::Definition("main");
        assert_eq!(
            Epoch::new_local(
                vec![],
                vec![
                    local_callable(identity.clone(), "main-a"),
                    local_callable(identity.clone(), "main-b"),
                ],
                vec!["main"],
            )
            .err(),
            Some(SemanticImportFailure::DuplicateCallable)
        );
        assert_eq!(
            Epoch::new_local(
                vec![],
                vec![
                    local_callable(identity.clone(), "main"),
                    local_callable(FunctionInstanceKey::Definition("other"), "main"),
                ],
                vec!["main"],
            )
            .err(),
            Some(SemanticImportFailure::DuplicateCallableLocalIdentity)
        );
        assert_eq!(
            Epoch::new_local(
                vec![],
                vec![local_callable(identity.clone(), "main")],
                vec!["main", "main"],
            )
            .err(),
            Some(SemanticImportFailure::DuplicateModule)
        );

        let mut epoch = Epoch::new_local(
            vec![],
            vec![local_callable(identity.clone(), "main")],
            vec!["main"],
        )
        .unwrap();
        epoch
            .local_completeness
            .as_mut()
            .expect("local epoch carries its own witness")
            .nominals_declared += 1;
        let error = epoch
            .materialize_local_body(
                identity,
                crate::AnalyzedCallableKind::Ordinary,
                &body(vec![crate::SemanticBodyInstData::Const(0)]),
                Span::with_file(FileId::new(7), 100, 200),
            )
            .err()
            .expect("incomplete witness must fail");
        assert_eq!(
            error.kind(),
            crate::SemanticBodyImportFailureKind::Semantic(
                SemanticImportFailure::IncompleteMaterialization
            )
        );

        for corrupt in [
            |witness: &mut SemanticLocalCompleteness| witness.callables_registered += 1,
            |witness: &mut SemanticLocalCompleteness| witness.modules_registered += 1,
        ] {
            let mut epoch = Epoch::new_local(
                vec![],
                vec![local_callable(
                    FunctionInstanceKey::Definition("main"),
                    "main",
                )],
                vec!["main"],
            )
            .unwrap();
            corrupt(
                epoch
                    .local_completeness
                    .as_mut()
                    .expect("local epoch carries its own witness"),
            );
            let error = epoch
                .materialize_local_body(
                    FunctionInstanceKey::Definition("main"),
                    crate::AnalyzedCallableKind::Ordinary,
                    &body(vec![crate::SemanticBodyInstData::Const(0)]),
                    Span::with_file(FileId::new(7), 100, 200),
                )
                .err()
                .expect("corrupt internal count must fail");
            assert_eq!(
                error.kind(),
                crate::SemanticBodyImportFailureKind::Semantic(
                    SemanticImportFailure::IncompleteMaterialization
                )
            );
        }
    }

    #[test]
    fn local_materialization_is_independent_of_exact_fact_input_order() {
        use crate::SemanticBodyInstData as D;
        let identity = FunctionInstanceKey::Definition("main");
        let input = body(vec![D::Const(7), D::Ret(Some(0))]);
        let facts = vec![
            SemanticLocalNominal {
                key: NominalInstanceKey::Named("z"),
                module_path: Arc::from("main"),
                name: Arc::from("Z"),
                kind: SemanticImportNominalKind::Struct,
                is_public: false,
                lang_item: None,
                shape: SemanticLocalNominalShape::Struct {
                    fields: Arc::new([]),
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                },
            },
            SemanticLocalNominal {
                key: NominalInstanceKey::Named("a"),
                module_path: Arc::from("main"),
                name: Arc::from("A"),
                kind: SemanticImportNominalKind::Enum,
                is_public: false,
                lang_item: None,
                shape: SemanticLocalNominalShape::Enum {
                    variants: Arc::new([]),
                    is_non_exhaustive: false,
                },
            },
        ];
        let forward = materialize_local(
            identity.clone(),
            crate::AnalyzedCallableKind::Ordinary,
            &input,
            facts.clone(),
            vec![local_callable(identity.clone(), "main")],
        )
        .unwrap();
        let reverse = materialize_local(
            identity,
            crate::AnalyzedCallableKind::Ordinary,
            &input,
            facts.into_iter().rev().collect(),
            vec![local_callable(
                FunctionInstanceKey::Definition("main"),
                "main",
            )],
        )
        .unwrap();
        assert_eq!(format!("{:?}", forward.air), format!("{:?}", reverse.air));
        assert_eq!(forward.aggregate_types, reverse.aggregate_types);
        assert_eq!(
            forward.type_pool.all_types().collect::<Vec<_>>(),
            reverse.type_pool.all_types().collect::<Vec<_>>()
        );
    }

    #[test]
    fn local_materialization_matches_the_exact_body_import_boundary() {
        use crate::SemanticBodyInstData as D;
        let input = body(vec![
            D::Const(4),
            D::Const(5),
            D::Add(0, 1),
            D::Ret(Some(2)),
        ]);
        let span = Span::with_file(FileId::new(7), 100, 200);
        let imported = Epoch::new(vec![], vec![("main", Arc::from("main"))], vec!["main"])
            .unwrap()
            .import_body(&input, span)
            .unwrap();
        let local = materialize_local(
            FunctionInstanceKey::Definition("main"),
            crate::AnalyzedCallableKind::Ordinary,
            &input,
            vec![],
            vec![local_callable(
                FunctionInstanceKey::Definition("main"),
                "main",
            )],
        )
        .unwrap();
        assert_eq!(format!("{:?}", local.air), format!("{:?}", imported.air));
        assert_eq!(local.strings, imported.strings);
        assert_eq!(local.local_atoms, imported.local_atoms);
        assert_eq!(local.param_modes, imported.param_modes);
        assert_eq!(local.warnings, imported.warnings);
    }

    #[test]
    fn body_import_fails_closed_for_forward_refs_generic_calls_and_bad_modes() {
        use crate::{SemanticBodyImportFailure as F, SemanticBodyInstData as D};
        let epoch = Epoch::new(vec![], vec![], vec![]).unwrap();
        assert!(matches!(
            epoch.import_body(
                &body(vec![D::Add(0, 0)]),
                Span::with_file(FileId::DEFAULT, 0, 100)
            ),
            Err(F::ForwardInstructionReference)
        ));
        assert!(matches!(
            epoch.import_body(
                &body(vec![D::CallGeneric]),
                Span::with_file(FileId::DEFAULT, 0, 100)
            ),
            Err(F::UnsupportedGenericCall)
        ));
        let mut mutable_by_value = body(vec![D::Const(0)]);
        mutable_by_value.num_param_slots = 1;
        mutable_by_value.param_by_ref = vec![false].into();
        mutable_by_value.param_writable = vec![true].into();
        assert!(
            epoch
                .import_body(&mutable_by_value, Span::with_file(FileId::DEFAULT, 0, 100),)
                .is_ok(),
            "mut self is writable by value"
        );
        let mut invalid = mutable_by_value;
        invalid.param_writable = Arc::new([]);
        assert!(matches!(
            epoch.import_body(&invalid, Span::with_file(FileId::DEFAULT, 0, 100)),
            Err(F::InvalidParameterModes)
        ));
        let mut invalid_drop = body(vec![D::Const(0)]);
        invalid_drop.param_drops = vec![(1, crate::SemanticImportType::I32)].into();
        assert!(matches!(
            epoch.import_body(&invalid_drop, Span::with_file(FileId::DEFAULT, 0, 100)),
            Err(F::InvalidParameterDrop)
        ));
        let mut invalid_borrow = body(vec![D::Const(0)]);
        invalid_borrow.borrow_slots = vec![0].into();
        assert!(matches!(
            epoch.import_body(&invalid_borrow, Span::with_file(FileId::DEFAULT, 0, 100)),
            Err(F::InvalidBorrowSlot)
        ));
        // A failed import cannot mutate the epoch; a subsequent valid import is exact.
        assert_eq!(
            epoch
                .import_body(
                    &body(vec![D::Const(7)]),
                    Span::with_file(FileId::DEFAULT, 0, 100),
                )
                .unwrap()
                .air
                .len(),
            1
        );
    }

    #[test]
    fn body_import_rolls_back_structural_types_before_later_failures() {
        use crate::{
            SemanticBodyAnchor, SemanticBodyImportFailure as F, SemanticBodyInstData as D,
        };
        let epoch = Epoch::new(vec![], vec![], vec![]).unwrap();
        let array = ImportType::Array {
            element: Arc::new(ImportType::U8),
            len: 9,
        };
        let before = epoch.type_pool().stats();

        let mut forward = body(vec![D::Const(0), D::Add(2, 0)]);
        let mut instructions = forward.instructions.to_vec();
        instructions[0].ty = array.clone();
        forward.instructions = instructions.into();
        let result = epoch.import_body(&forward, Span::with_file(FileId::DEFAULT, 0, 100));
        assert!(
            matches!(result, Err(F::InvalidInstructionReference)),
            "{result:?}"
        );
        assert_eq!(epoch.type_pool().stats(), before);
        assert_eq!(epoch.type_pool().get_array(Type::U8, 9), None);

        let mut warning = body(vec![D::Const(0)]);
        let mut instructions = warning.instructions.to_vec();
        instructions[0].ty = array;
        warning.instructions = instructions.into();
        warning.warnings = vec![crate::SemanticBodyWarning {
            kind: rue_error::WarningKind::UnreachableCode,
            anchor: SemanticBodyAnchor {
                start: 101,
                end: 102,
            },
            labels: Arc::new([]),
            notes: Arc::new([]),
            helps: Arc::new([]),
            suggestions: Arc::new([]),
        }]
        .into();
        assert!(matches!(
            epoch.import_body(&warning, Span::with_file(FileId::DEFAULT, 0, 100)),
            Err(F::InvalidAnchor)
        ));
        assert_eq!(epoch.type_pool().stats(), before);
        assert_eq!(epoch.type_pool().get_array(Type::U8, 9), None);
    }

    #[test]
    fn failed_body_import_preserves_live_symbol_order_and_packed_air() {
        use crate::{SemanticBodyImportFailure as F, SemanticBodyInstData as D};
        let after_failure = Epoch::new(vec![], vec![], vec![]).unwrap();
        let fresh = Epoch::new(vec![], vec![], vec![]).unwrap();
        let baseline_symbols = fresh.interner().len();
        assert_eq!(after_failure.interner().len(), baseline_symbols);

        let invalid = body(vec![
            D::Const(0),
            D::Intrinsic {
                operation: crate::IntrinsicOperation::BitCast,
                name: Arc::from("bitCast"),
                args: vec![crate::SemanticBodyCallArg {
                    value: 0,
                    mode: crate::AirArgMode::Normal,
                }]
                .into(),
            },
            D::Add(3, 0),
        ]);
        assert!(matches!(
            after_failure.import_body(&invalid, Span::with_file(FileId::DEFAULT, 0, 100)),
            Err(F::InvalidInstructionReference)
        ));
        assert_eq!(
            after_failure.interner().len(),
            baseline_symbols,
            "preflight must not publish an intrinsic symbol before a later failure"
        );

        let valid = body(vec![
            D::Const(0),
            D::Intrinsic {
                operation: crate::IntrinsicOperation::BitCast,
                name: Arc::from("bitCast"),
                args: vec![crate::SemanticBodyCallArg {
                    value: 0,
                    mode: crate::AirArgMode::Normal,
                }]
                .into(),
            },
        ]);
        let after_failure_body = after_failure
            .import_body(&valid, Span::with_file(FileId::DEFAULT, 0, 100))
            .unwrap();
        let fresh_body = fresh
            .import_body(&valid, Span::with_file(FileId::DEFAULT, 0, 100))
            .unwrap();
        let crate::AirInstData::Intrinsic {
            name: after_failure_name,
            ..
        } = after_failure_body.air.instructions()[1].data
        else {
            panic!("expected reconstructed intrinsic")
        };
        let crate::AirInstData::Intrinsic {
            name: fresh_name, ..
        } = fresh_body.air.instructions()[1].data
        else {
            panic!("expected reconstructed intrinsic")
        };
        assert_eq!(after_failure_name, fresh_name);
        assert_eq!(
            format!("{:?}", after_failure_body.air),
            format!("{:?}", fresh_body.air),
            "a failed import must not perturb any packed AIR word"
        );
    }

    #[test]
    fn intrinsic_counterfeit_matrix_rolls_back_before_symbols_types_or_air_are_published() {
        use crate::{
            SemanticBodyImportFailure as F, SemanticBodyInstData as D, SemanticImportType,
        };
        let after_failures = Epoch::new(vec![], vec![], vec![]).unwrap();
        let fresh = Epoch::new(vec![], vec![], vec![]).unwrap();
        let valid = body(vec![
            D::Const(0),
            D::Intrinsic {
                operation: crate::IntrinsicOperation::BitCast,
                name: Arc::from("bitCast"),
                args: vec![crate::SemanticBodyCallArg {
                    value: 0,
                    mode: crate::AirArgMode::Normal,
                }]
                .into(),
            },
        ]);

        let mutate =
            |mut body: crate::SemanticBody<&'static str, &'static str>,
             edit: fn(&mut [crate::SemanticBodyInst<&'static str, &'static str>])| {
                let mut instructions = body.instructions.to_vec();
                edit(&mut instructions);
                body.instructions = instructions.into();
                body
            };
        let counterfeits = [
            (
                "operation",
                mutate(valid.clone(), |instructions| {
                    let D::Intrinsic { operation, .. } = &mut instructions[1].data else {
                        unreachable!()
                    };
                    *operation = crate::IntrinsicOperation::PtrRead;
                }),
                F::InvalidIntrinsicOperation,
            ),
            (
                "diagnostic name",
                mutate(valid.clone(), |instructions| {
                    let D::Intrinsic { name, .. } = &mut instructions[1].data else {
                        unreachable!()
                    };
                    *name = Arc::from("counterfeit");
                }),
                F::InvalidIntrinsicOperation,
            ),
            (
                "missing argument",
                mutate(valid.clone(), |instructions| {
                    let D::Intrinsic { args, .. } = &mut instructions[1].data else {
                        unreachable!()
                    };
                    *args = Arc::new([]);
                }),
                F::InvalidIntrinsicOperation,
            ),
            (
                "extra argument",
                mutate(valid.clone(), |instructions| {
                    let D::Intrinsic { args, .. } = &mut instructions[1].data else {
                        unreachable!()
                    };
                    *args = vec![
                        crate::SemanticBodyCallArg {
                            value: 0,
                            mode: crate::AirArgMode::Normal,
                        },
                        crate::SemanticBodyCallArg {
                            value: 0,
                            mode: crate::AirArgMode::Normal,
                        },
                    ]
                    .into();
                }),
                F::InvalidIntrinsicOperation,
            ),
            (
                "argument mode",
                mutate(valid.clone(), |instructions| {
                    let D::Intrinsic { args, .. } = &mut instructions[1].data else {
                        unreachable!()
                    };
                    *args = vec![crate::SemanticBodyCallArg {
                        value: 0,
                        mode: crate::AirArgMode::Borrow,
                    }]
                    .into();
                }),
                F::InvalidParameterModes,
            ),
            (
                "operand type",
                mutate(valid.clone(), |instructions| {
                    instructions[0].ty = SemanticImportType::U64;
                }),
                F::InvalidIntrinsicOperation,
            ),
            (
                "result type",
                mutate(valid.clone(), |instructions| {
                    instructions[1].ty = SemanticImportType::U64;
                }),
                F::InvalidIntrinsicOperation,
            ),
        ];

        let initial_symbols = after_failures.interner().len();
        let initial_types = after_failures.type_pool().stats();
        for (counterfeit, body, expected) in counterfeits {
            let result = after_failures
                .import_body(&body, Span::with_file(FileId::DEFAULT, 0, 100))
                .expect_err(counterfeit);
            assert_eq!(result, expected, "counterfeit {counterfeit}");
            assert_eq!(
                after_failures.interner().len(),
                initial_symbols,
                "counterfeit {counterfeit} published a diagnostic name"
            );
            assert_eq!(
                after_failures.type_pool().stats(),
                initial_types,
                "counterfeit {counterfeit} published a type"
            );
        }

        let after_failures_body = after_failures
            .import_body(&valid, Span::with_file(FileId::DEFAULT, 0, 100))
            .unwrap();
        let fresh_body = fresh
            .import_body(&valid, Span::with_file(FileId::DEFAULT, 0, 100))
            .unwrap();
        assert_eq!(
            format!("{:?}", after_failures_body.air),
            format!("{:?}", fresh_body.air)
        );
        assert_eq!(
            after_failures.interner().get("bitCast"),
            fresh.interner().get("bitCast")
        );
    }

    #[test]
    fn address_taking_intrinsics_validate_durable_place_provenance_before_publication() {
        use crate::{
            AirArgMode, AirPlaceBase, IntrinsicOperation, NominalInstanceKey, SemanticBodyCallArg,
            SemanticBodyImportFailure as F, SemanticBodyInstData as D, SemanticBodyPlace,
            SemanticBodyProjection,
        };

        let epoch = Epoch::new(
            vec![nominal(
                "record",
                "Record",
                SemanticImportNominalKind::Struct,
            )],
            vec![],
            vec![],
        )
        .unwrap();
        epoch
            .complete_struct(
                &"record",
                &[
                    (Arc::from("field"), ImportType::I32),
                    (Arc::from("bottom"), ImportType::Never),
                ],
                false,
                false,
                false,
            )
            .unwrap();

        let intrinsic_body = |source: D<&'static str, &'static str>,
                              source_ty: ImportType,
                              operation: IntrinsicOperation,
                              result_ty: ImportType| {
            let spelling = operation.expected_spelling();
            let mut input = body(vec![
                source,
                D::Intrinsic {
                    operation,
                    name: Arc::from(spelling),
                    args: vec![SemanticBodyCallArg {
                        value: 0,
                        mode: AirArgMode::Normal,
                    }]
                    .into(),
                },
            ]);
            input.return_type = result_ty.clone();
            let instructions = Arc::make_mut(&mut input.instructions);
            instructions[0].ty = source_ty;
            instructions[1].ty = result_ty;
            input
        };
        let local = |mut input: crate::SemanticBody<&'static str, &'static str>| {
            input.num_locals = 1;
            input
        };
        let param = |mut input: crate::SemanticBody<&'static str, &'static str>| {
            input.num_param_slots = 1;
            input.param_by_ref = Arc::from([false]);
            input.param_writable = Arc::from([true]);
            input
        };
        let bare_place = |mut input: crate::SemanticBody<&'static str, &'static str>| {
            input.num_locals = 1;
            input.places = Arc::from([SemanticBodyPlace {
                base: AirPlaceBase::Local(0),
                base_type: ImportType::I32,
                projections: Arc::new([]),
            }]);
            input
        };
        let field_place = |mut input: crate::SemanticBody<&'static str, &'static str>| {
            input.num_locals = 1;
            input.places = Arc::from([SemanticBodyPlace {
                base: AirPlaceBase::Local(0),
                base_type: ImportType::Nominal("record"),
                projections: Arc::from([SemanticBodyProjection::Field {
                    struct_key: NominalInstanceKey::Named("record"),
                    field_index: 0,
                }]),
            }]);
            input
        };
        let bottom_place = |mut input: crate::SemanticBody<&'static str, &'static str>| {
            input.num_locals = 1;
            input.places = Arc::from([SemanticBodyPlace {
                base: AirPlaceBase::Local(0),
                base_type: ImportType::Never,
                projections: Arc::new([]),
            }]);
            input
        };
        let bottom_field_place = |mut input: crate::SemanticBody<&'static str, &'static str>| {
            input.num_locals = 1;
            input.places = Arc::from([SemanticBodyPlace {
                base: AirPlaceBase::Local(0),
                base_type: ImportType::Nominal("record"),
                projections: Arc::from([SemanticBodyProjection::Field {
                    struct_key: NominalInstanceKey::Named("record"),
                    field_index: 1,
                }]),
            }]);
            input
        };
        let ptr_const_i32 = ImportType::PtrConst(Arc::new(ImportType::I32));
        let ptr_mut_i32 = ImportType::PtrMut(Arc::new(ImportType::I32));
        let ptr_const_never = ImportType::PtrConst(Arc::new(ImportType::Never));
        let ptr_mut_never = ImportType::PtrMut(Arc::new(ImportType::Never));

        let invalid = [
            (
                "const to raw",
                intrinsic_body(
                    D::Const(0),
                    ImportType::I32,
                    IntrinsicOperation::Raw,
                    ptr_const_i32.clone(),
                ),
            ),
            (
                "const to raw_mut",
                intrinsic_body(
                    D::Const(0),
                    ImportType::I32,
                    IntrinsicOperation::RawMut,
                    ptr_mut_i32.clone(),
                ),
            ),
            (
                "load to field_ptr",
                local(intrinsic_body(
                    D::Load { slot: 0 },
                    ImportType::I32,
                    IntrinsicOperation::FieldPtr,
                    ptr_mut_i32.clone(),
                )),
            ),
            (
                "param to field_ptr",
                param(intrinsic_body(
                    D::Param { index: 0 },
                    ImportType::I32,
                    IntrinsicOperation::FieldPtr,
                    ptr_mut_i32.clone(),
                )),
            ),
            (
                "non-field place to field_ptr",
                bare_place(intrinsic_body(
                    D::PlaceRead { place: 0 },
                    ImportType::I32,
                    IntrinsicOperation::FieldPtr,
                    ptr_mut_i32.clone(),
                )),
            ),
        ];
        let initial_symbols = epoch.interner().len();
        let initial_types = epoch.type_pool().stats();
        for (label, input) in invalid {
            assert_eq!(
                epoch
                    .import_body(&input, Span::with_file(FileId::DEFAULT, 0, 100))
                    .expect_err(label),
                F::InvalidIntrinsicOperation,
                "counterfeit {label}"
            );
            assert_eq!(epoch.interner().len(), initial_symbols, "{label} symbols");
            assert_eq!(epoch.type_pool().stats(), initial_types, "{label} types");
        }

        let accepted = [
            (
                "load to raw",
                local(intrinsic_body(
                    D::Load { slot: 0 },
                    ImportType::I32,
                    IntrinsicOperation::Raw,
                    ptr_const_i32.clone(),
                )),
            ),
            (
                "param to raw",
                param(intrinsic_body(
                    D::Param { index: 0 },
                    ImportType::I32,
                    IntrinsicOperation::Raw,
                    ptr_const_i32.clone(),
                )),
            ),
            (
                "place to raw",
                bare_place(intrinsic_body(
                    D::PlaceRead { place: 0 },
                    ImportType::I32,
                    IntrinsicOperation::Raw,
                    ptr_const_i32.clone(),
                )),
            ),
            (
                "bottom place to raw",
                bottom_place(intrinsic_body(
                    D::PlaceRead { place: 0 },
                    ImportType::Never,
                    IntrinsicOperation::Raw,
                    ptr_const_never,
                )),
            ),
            (
                "load to raw_mut",
                local(intrinsic_body(
                    D::Load { slot: 0 },
                    ImportType::I32,
                    IntrinsicOperation::RawMut,
                    ptr_mut_i32.clone(),
                )),
            ),
            (
                "param to raw_mut",
                param(intrinsic_body(
                    D::Param { index: 0 },
                    ImportType::I32,
                    IntrinsicOperation::RawMut,
                    ptr_mut_i32.clone(),
                )),
            ),
            (
                "place to raw_mut",
                bare_place(intrinsic_body(
                    D::PlaceRead { place: 0 },
                    ImportType::I32,
                    IntrinsicOperation::RawMut,
                    ptr_mut_i32.clone(),
                )),
            ),
            (
                "terminal field to field_ptr",
                field_place(intrinsic_body(
                    D::PlaceRead { place: 0 },
                    ImportType::I32,
                    IntrinsicOperation::FieldPtr,
                    ptr_mut_i32,
                )),
            ),
            (
                "terminal bottom field to field_ptr",
                bottom_field_place(intrinsic_body(
                    D::PlaceRead { place: 0 },
                    ImportType::Never,
                    IntrinsicOperation::FieldPtr,
                    ptr_mut_never,
                )),
            ),
        ];
        for (label, input) in accepted {
            epoch
                .import_body(&input, Span::with_file(FileId::DEFAULT, 0, 100))
                .unwrap_or_else(|error| panic!("canonical {label} rejected: {error:?}"));
        }
    }

    #[test]
    fn runtime_option_result_counterfeits_roll_back_before_publication() {
        use crate::{
            IntrinsicOperation, SemanticBodyImportFailure as F, SemanticBodyInstData as D,
        };

        let shapes = [
            ("valid", "ValidOption"),
            ("payload_none_extra", "PayloadNoneExtra"),
            ("payload_none", "PayloadNone"),
            ("extra", "ExtraOption"),
            ("duplicate", "DuplicateOption"),
            ("wide_some", "WideSomeOption"),
        ];
        let epoch = Epoch::new(
            shapes
                .iter()
                .map(|(key, name)| nominal(key, name, SemanticImportNominalKind::Enum))
                .collect(),
            vec![],
            vec![],
        )
        .unwrap();
        let variants = |values: &[(&'static str, &[ImportType])]| {
            values
                .iter()
                .map(|(name, payload)| (Arc::from(*name), Arc::from(*payload)))
                .collect::<Vec<_>>()
        };
        epoch
            .complete_enum(
                &"valid",
                &variants(&[("Some", &[ImportType::I32]), ("None", &[])]),
            )
            .unwrap();
        epoch
            .complete_enum(
                &"payload_none_extra",
                &variants(&[
                    ("Some", &[ImportType::I32]),
                    ("None", &[ImportType::I64, ImportType::I64]),
                    ("Extra", &[]),
                ]),
            )
            .unwrap();
        epoch
            .complete_enum(
                &"payload_none",
                &variants(&[("Some", &[ImportType::I32]), ("None", &[ImportType::I64])]),
            )
            .unwrap();
        epoch
            .complete_enum(
                &"extra",
                &variants(&[("Some", &[ImportType::I32]), ("None", &[]), ("Extra", &[])]),
            )
            .unwrap();
        epoch
            .complete_enum(
                &"duplicate",
                &variants(&[("Some", &[ImportType::I32]), ("Some", &[])]),
            )
            .unwrap();
        epoch
            .complete_enum(
                &"wide_some",
                &variants(&[("Some", &[ImportType::I32, ImportType::I32]), ("None", &[])]),
            )
            .unwrap();

        let parse_i32 = |key: &'static str| {
            let result = ImportType::Nominal(key);
            let text = ImportType::BuiltinNominal {
                name: Arc::from("str"),
                kind: SemanticImportNominalKind::Struct,
            };
            let mut input = body(vec![
                D::StringConst(0),
                D::Intrinsic {
                    operation: IntrinsicOperation::ParseI32,
                    name: Arc::from("parse_i32"),
                    args: Arc::from([crate::SemanticBodyCallArg {
                        value: 0,
                        mode: crate::AirArgMode::Normal,
                    }]),
                },
            ]);
            input.return_type = result.clone();
            let instructions = Arc::make_mut(&mut input.instructions);
            instructions[0].ty = text;
            instructions[1].ty = result;
            input.strings = Arc::from([Arc::from("0")]);
            input
        };
        let initial_symbols = epoch.interner().len();
        let initial_types = epoch.type_pool().stats();
        for key in [
            "payload_none_extra",
            "payload_none",
            "extra",
            "duplicate",
            "wide_some",
        ] {
            assert_eq!(
                epoch
                    .import_body(&parse_i32(key), Span::with_file(FileId::DEFAULT, 0, 100),)
                    .expect_err(key),
                F::InvalidIntrinsicOperation
            );
            assert_eq!(epoch.interner().len(), initial_symbols, "{key} symbols");
            assert_eq!(epoch.type_pool().stats(), initial_types, "{key} types");
        }
        epoch
            .import_body(
                &parse_i32("valid"),
                Span::with_file(FileId::DEFAULT, 0, 100),
            )
            .expect("exact Option body must still import after counterfeits");
    }

    #[test]
    fn body_import_relocates_relative_anchors_into_current_body_span() {
        use crate::SemanticBodyInstData as D;
        let epoch = Epoch::new(vec![], vec![], vec![]).unwrap();
        let input = body(vec![D::Const(7)]);
        let first = epoch
            .import_body(&input, Span::with_file(FileId::new(3), 100, 110))
            .unwrap();
        let shifted = epoch
            .import_body(&input, Span::with_file(FileId::new(4), 700, 710))
            .unwrap();
        assert_eq!(first.air.get(crate::AirRef::from_raw(0)).span.start, 101);
        assert_eq!(first.air.get(crate::AirRef::from_raw(0)).span.end, 102);
        assert_eq!(shifted.air.get(crate::AirRef::from_raw(0)).span.start, 701);
        assert_eq!(shifted.air.get(crate::AirRef::from_raw(0)).span.end, 702);
        assert_eq!(
            shifted.air.get(crate::AirRef::from_raw(0)).span.file_id,
            FileId::new(4)
        );
    }

    #[test]
    fn body_import_rejects_out_of_range_and_overflowing_anchor_domains() {
        use crate::{SemanticBodyImportFailure as F, SemanticBodyInstData as D};
        let epoch = Epoch::new(vec![], vec![], vec![]).unwrap();
        let mut outside = body(vec![D::Const(7)]);
        Arc::make_mut(&mut outside.instructions)[0].anchor =
            crate::SemanticBodyAnchor { start: 1, end: 3 };
        assert!(matches!(
            epoch.import_body(&outside, Span::with_file(FileId::DEFAULT, 50, 52)),
            Err(F::InvalidAnchor)
        ));

        let valid = body(vec![D::Const(7)]);
        assert!(matches!(
            epoch.import_body(&valid, Span::with_file(FileId::DEFAULT, u32::MAX, 0),),
            Err(F::InvalidAnchor)
        ));
    }

    #[test]
    fn builtin_nominals_are_closed_validated_and_round_trip_in_fresh_epochs() {
        let epoch = Epoch::new(vec![], vec![], vec![]).unwrap();
        let arch = SemanticImportType::BuiltinNominal {
            name: Arc::from("Arch"),
            kind: SemanticImportNominalKind::Enum,
        };
        let local = epoch.import_type(&arch).unwrap();
        assert_eq!(epoch.export_type(local).unwrap(), arch);
        let str_ty = SemanticImportType::BuiltinNominal {
            name: Arc::from("str"),
            kind: SemanticImportNominalKind::Struct,
        };
        let local = epoch.import_type(&str_ty).unwrap();
        let str_id = local.value.as_struct().expect("builtin str is a struct");
        assert_eq!(epoch.export_type(local).unwrap(), str_ty);
        assert_eq!(str_id, crate::StructId(4));
        let str_def = epoch.type_pool().struct_def(str_id);
        assert_eq!(str_def.name.as_ref(), "str");
        assert_eq!(
            str_def
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["ptr", "len"]
        );
        assert!(str_def.is_copy && str_def.is_builtin && str_def.is_pub);
        assert!(!str_def.is_linear && !str_def.declared_linear);
        assert_eq!(str_def.file_id, FileId::DEFAULT);
        assert_eq!(epoch.type_pool().struct_lang_item(str_id), None);
        assert_eq!(str_def.fields[1].ty, Type::U64);
        assert!(matches!(
            str_def.fields[0].ty.kind(),
            crate::TypeKind::PtrConst(_)
        ));

        for (id, (name, variants)) in [
            (crate::EnumId(0), ("Arch", ["X86_64", "Aarch64"].as_slice())),
            (crate::EnumId(1), ("Os", ["Linux", "Macos"].as_slice())),
            (
                crate::EnumId(2),
                ("DataModel", ["Ilp32", "Lp64", "Llp64"].as_slice()),
            ),
        ] {
            let enum_ty = epoch
                .import_type(&SemanticImportType::BuiltinNominal {
                    name: Arc::from(name),
                    kind: SemanticImportNominalKind::Enum,
                })
                .unwrap();
            assert_eq!(enum_ty.value.as_enum(), Some(id));
            let def = epoch.type_pool().enum_def(id);
            assert_eq!(
                def.variants
                    .iter()
                    .map(|variant| variant.as_ref())
                    .collect::<Vec<_>>(),
                variants
            );
            assert!(def.variant_payloads.is_empty() && def.is_pub);
            assert_eq!(def.file_id, FileId::DEFAULT);
        }

        assert!(matches!(
            epoch.import_type(&SemanticImportType::BuiltinNominal {
                name: Arc::from("NotABuiltin"),
                kind: SemanticImportNominalKind::Struct,
            }),
            Err(SemanticImportFailure::UnknownBuiltinNominal)
        ));
        assert!(matches!(
            epoch.import_type(&SemanticImportType::BuiltinNominal {
                name: Arc::from("StrBuf"),
                kind: SemanticImportNominalKind::Struct,
            }),
            Err(SemanticImportFailure::UnknownBuiltinNominal)
        ));
        assert!(matches!(
            epoch.import_type(&SemanticImportType::BuiltinNominal {
                name: Arc::from("Arch"),
                kind: SemanticImportNominalKind::Struct,
            }),
            Err(SemanticImportFailure::BuiltinNominalKindMismatch)
        ));
    }
}
