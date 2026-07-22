//! Canonical semantic identities shared by live bindings and durable keys.

use std::sync::Arc;

use rue_runtime_abi::{ReservedExportId, RuntimeHelperId};

/// The kind of an anonymous nominal is part of its identity even when its
/// producer, structural site, and arguments happen to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnonymousNominalKind {
    Struct,
    Enum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnonymousMemberKind {
    Method,
    AssociatedFunction,
    Destructor,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnonymousMemberKey {
    pub kind: AnonymousMemberKind,
    pub name: Arc<str>,
}

/// A canonical specialization value. Strings are represented by content;
/// interner symbols are never stable values.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalArgumentValue<D, M> {
    Integer(i128),
    Bool(bool),
    Type(Box<TypeInstanceKey<D, M>>),
    Function(Box<FunctionInstanceKey<D, M>>),
    Unit,
    String(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalArguments<D, M> {
    /// Type-valued comptime arguments in their declaration-relative order.
    pub types: Arc<[TypeInstanceKey<D, M>]>,
    /// Non-type comptime arguments in their declaration-relative order.
    pub values: Arc<[CanonicalArgumentValue<D, M>]>,
}

// The two streams deliberately avoid storing a redundant tag per element.
// Their mixed positional order is reconstructed only against the base
// function's durable parameter schema (`parameter_comptime` plus the
// corresponding semantic parameter types). Every specialization key includes
// that base function identity, so arguments are never compared without the
// schema which tells consumers how to interleave these declaration-ordered
// streams.

impl<D, M> Default for CanonicalArguments<D, M> {
    fn default() -> Self {
        Self {
            types: Arc::new([]),
            values: Arc::new([]),
        }
    }
}

/// The stable producer of an anonymous nominal. A specialized or anonymous
/// function is a producer in its own right.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableProducerId<D, M> {
    Definition(D),
    Function(Box<FunctionInstanceKey<D, M>>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnonymousNominalKey<D, M> {
    pub kind: AnonymousNominalKind,
    pub producer: StableProducerId<D, M>,
    pub anchor: rue_rir::RirStructuralAnchor,
    pub arguments: CanonicalArguments<D, M>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NominalInstanceKey<D, M> {
    Builtin {
        kind: AnonymousNominalKind,
        name: Arc<str>,
    },
    Named(D),
    Anonymous(AnonymousNominalKey<D, M>),
}

/// Canonical identity of a concrete type instance. `D` and `M` are the
/// definition and module identity domains selected by the owning semantic
/// boundary (issuer-scoped tokens in AIR, durable keys in the compiler).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypeInstanceKey<D, M> {
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
    BuiltinNominal {
        kind: AnonymousNominalKind,
        name: Arc<str>,
    },
    Nominal(NominalInstanceKey<D, M>),
    Array {
        element: Box<Self>,
        len: u64,
    },
    Slice {
        element: Box<Self>,
        name: Arc<str>,
    },
    PtrConst(Box<Self>),
    PtrMut(Box<Self>),
    Module(M),
    GenericParameter(u32),
}

/// Canonical identity of one source or synthesized function instance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FunctionInstanceKey<D, M> {
    Definition(D),
    Specialization {
        base: Box<FunctionInstanceKey<D, M>>,
        arguments: CanonicalArguments<D, M>,
    },
    AnonymousMember {
        owner: Box<TypeInstanceKey<D, M>>,
        member: AnonymousMemberKey,
    },
    DropGlue(Box<TypeInstanceKey<D, M>>),
}

impl<D, M> CanonicalArgumentValue<D, M> {
    /// Relocate every definition and module identity carried by this stable
    /// argument value without changing its language-level value.
    pub fn try_map_identities<D2, M2, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<CanonicalArgumentValue<D2, M2>, E> {
        Ok(match self {
            Self::Integer(value) => CanonicalArgumentValue::Integer(*value),
            Self::Bool(value) => CanonicalArgumentValue::Bool(*value),
            Self::Type(value) => CanonicalArgumentValue::Type(Box::new(
                value.try_map_identities(definition, module)?,
            )),
            Self::Function(value) => CanonicalArgumentValue::Function(Box::new(
                value.try_map_identities(definition, module)?,
            )),
            Self::Unit => CanonicalArgumentValue::Unit,
            Self::String(value) => CanonicalArgumentValue::String(value.clone()),
        })
    }
}

impl<D, M> CanonicalArguments<D, M> {
    pub fn try_map_identities<D2, M2, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<CanonicalArguments<D2, M2>, E> {
        let types = self
            .types
            .iter()
            .map(|value| value.try_map_identities(definition, module))
            .collect::<Result<Vec<_>, _>>()?;
        let values = self
            .values
            .iter()
            .map(|value| value.try_map_identities(definition, module))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CanonicalArguments {
            types: types.into(),
            values: values.into(),
        })
    }
}

impl<D, M> AnonymousNominalKey<D, M> {
    /// Relocate the complete recursive identity graph without changing its
    /// language-level identity. Durable body projection and current-request
    /// validation deliberately share this traversal.
    pub fn try_map_identities<D2, M2, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<AnonymousNominalKey<D2, M2>, E> {
        Ok(AnonymousNominalKey {
            kind: self.kind,
            producer: match &self.producer {
                StableProducerId::Definition(value) => {
                    StableProducerId::Definition(definition(value)?)
                }
                StableProducerId::Function(value) => StableProducerId::Function(Box::new(
                    value.try_map_identities(definition, module)?,
                )),
            },
            anchor: self.anchor.clone(),
            arguments: self.arguments.try_map_identities(definition, module)?,
        })
    }
}

impl<D, M> TypeInstanceKey<D, M> {
    pub fn try_map_identities<D2, M2, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<TypeInstanceKey<D2, M2>, E> {
        Ok(match self {
            Self::I8 => TypeInstanceKey::I8,
            Self::I16 => TypeInstanceKey::I16,
            Self::I32 => TypeInstanceKey::I32,
            Self::I64 => TypeInstanceKey::I64,
            Self::U8 => TypeInstanceKey::U8,
            Self::U16 => TypeInstanceKey::U16,
            Self::U32 => TypeInstanceKey::U32,
            Self::U64 => TypeInstanceKey::U64,
            Self::Bool => TypeInstanceKey::Bool,
            Self::Unit => TypeInstanceKey::Unit,
            Self::Never => TypeInstanceKey::Never,
            Self::ComptimeType => TypeInstanceKey::ComptimeType,
            Self::BuiltinNominal { kind, name } => TypeInstanceKey::BuiltinNominal {
                kind: *kind,
                name: name.clone(),
            },
            Self::Nominal(value) => TypeInstanceKey::Nominal(match value {
                NominalInstanceKey::Builtin { kind, name } => NominalInstanceKey::Builtin {
                    kind: *kind,
                    name: name.clone(),
                },
                NominalInstanceKey::Named(value) => NominalInstanceKey::Named(definition(value)?),
                NominalInstanceKey::Anonymous(value) => {
                    NominalInstanceKey::Anonymous(value.try_map_identities(definition, module)?)
                }
            }),
            Self::Array { element, len } => TypeInstanceKey::Array {
                element: Box::new(element.try_map_identities(definition, module)?),
                len: *len,
            },
            Self::Slice { element, name } => TypeInstanceKey::Slice {
                element: Box::new(element.try_map_identities(definition, module)?),
                name: name.clone(),
            },
            Self::PtrConst(value) => {
                TypeInstanceKey::PtrConst(Box::new(value.try_map_identities(definition, module)?))
            }
            Self::PtrMut(value) => {
                TypeInstanceKey::PtrMut(Box::new(value.try_map_identities(definition, module)?))
            }
            Self::Module(value) => TypeInstanceKey::Module(module(value)?),
            Self::GenericParameter(index) => TypeInstanceKey::GenericParameter(*index),
        })
    }
}

impl<D, M> FunctionInstanceKey<D, M> {
    pub fn try_map_identities<D2, M2, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<FunctionInstanceKey<D2, M2>, E> {
        Ok(match self {
            Self::Definition(value) => FunctionInstanceKey::Definition(definition(value)?),
            Self::Specialization { base, arguments } => FunctionInstanceKey::Specialization {
                base: Box::new(base.try_map_identities(definition, module)?),
                arguments: arguments.try_map_identities(definition, module)?,
            },
            Self::AnonymousMember { owner, member } => FunctionInstanceKey::AnonymousMember {
                owner: Box::new(owner.try_map_identities(definition, module)?),
                member: member.clone(),
            },
            Self::DropGlue(value) => FunctionInstanceKey::DropGlue(Box::new(
                value.try_map_identities(definition, module)?,
            )),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompilerCallableId {
    ProgramEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableCallableId<D, M> {
    Function(FunctionInstanceKey<D, M>),
    Runtime(RuntimeHelperId),
    Compiler(CompilerCallableId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocalAtomKind {
    String,
    ReadOnlyData,
    WritableData,
}

/// Logical occurrence identity for data owned by a function record. The
/// structural anchor is definition-relative and independent of source spans,
/// pool allocation, string content, and request-local dense indices.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalAtomId<D, M> {
    pub producer: FunctionInstanceKey<D, M>,
    pub kind: LocalAtomKind,
    pub anchor: rue_rir::RirStructuralAnchor,
}

impl<D, M> LocalAtomId<D, M> {
    pub fn try_map_identities<D2, M2, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<LocalAtomId<D2, M2>, E> {
        Ok(LocalAtomId {
            producer: self.producer.try_map_identities(definition, module)?,
            kind: self.kind,
            anchor: self.anchor.clone(),
        })
    }
}

/// One occurrence-preserving local data record. `dense_id` is only the current
/// request's projection into its content table; aliases may share it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAtomRecord<D, M> {
    pub identity: LocalAtomId<D, M>,
    pub content: Arc<str>,
    pub dense_id: u32,
}

/// Request-independent representation of one local-data occurrence. Dense
/// table indices are deliberately excluded: they are reconstructed when a
/// durable semantic body is installed into a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyLocalAtom<D, M> {
    pub identity: LocalAtomId<D, M>,
    pub content: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableSymbolId<D, M> {
    Callable(StableCallableId<D, M>),
    ReservedRuntime(ReservedExportId),
    LocalAtom(LocalAtomId<D, M>),
}

/// The exhaustive namespace of a semantically bound definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableDefinitionNamespace {
    Value,
    Type,
    Destructor,
    Method,
}

/// Reviewable inventory of every stable semantic namespace.
pub const STABLE_DEFINITION_NAMESPACES: &[StableDefinitionNamespace] = &[
    StableDefinitionNamespace::Value,
    StableDefinitionNamespace::Type,
    StableDefinitionNamespace::Destructor,
    StableDefinitionNamespace::Method,
];

// One reviewable source generates the stable kind enum, inventory, namespace,
// and ownership policy. Adding a kind cannot compile until all taxonomy fields
// are supplied here.
macro_rules! stable_definition_kind_schema {
    ($consumer:ident) => {
        $consumer! {
            Function, Value, true, false;
            Struct, Type, false, false;
            Enum, Type, false, false;
            ValueConst, Value, false, false;
            ModuleBinding, Value, false, false;
            Destructor, Destructor, true, true;
            Method, Method, true, true;
            AssociatedFunction, Method, true, true;
        }
    };
}

macro_rules! define_stable_definition_kind_schema {
    ($( $kind:ident, $namespace:ident, $owns_body:literal, $requires_owner:literal; )*) => {
        /// The exhaustive kind of a semantically bound definition.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum StableDefinitionKind {
            $( $kind, )*
        }

        /// Reviewable inventory of every stable semantic definition kind.
        pub const STABLE_DEFINITION_KINDS: &[StableDefinitionKind] = &[
            $( StableDefinitionKind::$kind, )*
        ];

        impl StableDefinitionKind {
            /// The only namespace in which this kind can be issued.
            pub const fn namespace(self) -> StableDefinitionNamespace {
                match self {
                    $( Self::$kind => StableDefinitionNamespace::$namespace, )*
                }
            }

            /// Whether this definition owns an executable semantic body.
            pub const fn owns_body(self) -> bool {
                match self {
                    $( Self::$kind => $owns_body, )*
                }
            }

            /// Whether this definition must name an owning nominal type.
            pub const fn requires_owner(self) -> bool {
                match self {
                    $( Self::$kind => $requires_owner, )*
                }
            }
        }
    };
}

stable_definition_kind_schema!(define_stable_definition_kind_schema);

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};

    use super::*;

    #[test]
    fn taxonomy_declares_every_kind_namespace_and_owner_shape_once() {
        use StableDefinitionKind as K;
        use StableDefinitionNamespace as N;

        let cases = [
            (K::Function, N::Value, true, false),
            (K::Struct, N::Type, false, false),
            (K::Enum, N::Type, false, false),
            (K::ValueConst, N::Value, false, false),
            (K::ModuleBinding, N::Value, false, false),
            (K::Destructor, N::Destructor, true, true),
            (K::Method, N::Method, true, true),
            (K::AssociatedFunction, N::Method, true, true),
        ];
        for (kind, namespace, owns_body, requires_owner) in cases {
            assert_eq!(kind.namespace(), namespace);
            assert_eq!(kind.owns_body(), owns_body);
            assert_eq!(kind.requires_owner(), requires_owner);
        }
    }

    #[test]
    fn generic_identity_algebra_has_exact_equality_ordering_and_hashing() {
        use rue_rir::RirStructuralPathSegment as S;

        type T = TypeInstanceKey<&'static str, &'static str>;
        let make = |definition, module, path| {
            T::Nominal(NominalInstanceKey::Anonymous(AnonymousNominalKey {
                kind: AnonymousNominalKind::Struct,
                producer: StableProducerId::Definition(definition),
                anchor: rue_rir::RirStructuralAnchor::new(path),
                arguments: CanonicalArguments {
                    types: Arc::from([T::Module(module)]),
                    values: Arc::new([]),
                },
            }))
        };
        let baseline = make("make", "pkg", vec![S::Body, S::AnonymousType(0)]);
        let same = make("make", "pkg", vec![S::Body, S::AnonymousType(0)]);
        let moved = make(
            "make",
            "pkg",
            vec![S::Body, S::Statement(0), S::AnonymousType(0)],
        );
        let other_definition = make("other", "pkg", vec![S::Body, S::AnonymousType(0)]);

        assert_eq!(baseline, same);
        assert_ne!(baseline, moved);
        assert_ne!(baseline, other_definition);
        assert_eq!(
            BTreeSet::from([baseline.clone(), same.clone(), moved.clone()]).len(),
            2
        );
        assert_eq!(HashSet::from([baseline, same, moved]).len(), 2);
    }
}
