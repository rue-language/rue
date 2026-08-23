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
    Type(Arc<TypeInstanceKey<D, M>>),
    Function(Arc<FunctionInstanceKey<D, M>>),
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
    Function(Arc<FunctionInstanceKey<D, M>>),
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
        element: Arc<Self>,
        len: u64,
    },
    Slice {
        element: Arc<Self>,
        name: Arc<str>,
    },
    PtrConst(Arc<Self>),
    PtrMut(Arc<Self>),
    Module(M),
    GenericParameter(u32),
}

/// Canonical identity of one source or synthesized function instance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FunctionInstanceKey<D, M> {
    Definition(D),
    Specialization {
        base: Arc<FunctionInstanceKey<D, M>>,
        arguments: CanonicalArguments<D, M>,
    },
    AnonymousMember {
        owner: Arc<TypeInstanceKey<D, M>>,
        member: AnonymousMemberKey,
    },
    DropGlue(Arc<TypeInstanceKey<D, M>>),
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
            Self::Type(value) => CanonicalArgumentValue::Type(Arc::new(
                value.try_map_identities(definition, module)?,
            )),
            Self::Function(value) => CanonicalArgumentValue::Function(Arc::new(
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
                StableProducerId::Function(value) => StableProducerId::Function(Arc::new(
                    value.try_map_identities(definition, module)?,
                )),
            },
            anchor: self.anchor.clone(),
            arguments: self.arguments.try_map_identities(definition, module)?,
        })
    }
}

impl<D: Clone, M: Clone> AnonymousNominalKey<D, M> {
    /// The canonical producer form of this key: every empty-argument function
    /// `Specialization` wrapper on the producer's base spine collapsed to its
    /// base (see [`FunctionInstanceKey::with_collapsed_empty_specializations`]).
    ///
    /// This is the form the semantic epoch's `canonical_function_producer`
    /// always mints under and the form production body-export carries (the
    /// warm==cold digest invariant requires it), so every identity consumer
    /// that digests or dedups producer-nominal keys must compare keys in this
    /// form. Returns `Cow::Borrowed` when the key is already canonical — the
    /// collapse is a pure normalization, never a change of identity.
    pub fn with_canonical_producer(&self) -> std::borrow::Cow<'_, Self> {
        match &self.producer {
            StableProducerId::Definition(_) => std::borrow::Cow::Borrowed(self),
            StableProducerId::Function(function) => {
                match function.with_collapsed_empty_specializations() {
                    std::borrow::Cow::Borrowed(_) => std::borrow::Cow::Borrowed(self),
                    std::borrow::Cow::Owned(collapsed) => std::borrow::Cow::Owned(Self {
                        kind: self.kind,
                        producer: StableProducerId::Function(Arc::new(collapsed)),
                        anchor: self.anchor.clone(),
                        arguments: self.arguments.clone(),
                    }),
                }
            }
        }
    }
}

impl<D: Clone, M: Clone> FunctionInstanceKey<D, M> {
    /// Collapse every `Specialization` wrapper carrying no arguments to its
    /// base, recursively along the base spine — the canonical producer form
    /// the epoch's `canonical_function_producer` emits
    /// (`Function(Specialization { base, args: [] })` ≡ `Function(base)`).
    ///
    /// The collapse covers the function-producer spine only: an
    /// `AnonymousMember` owner or `DropGlue` pointee is a type instance the
    /// epoch never spells through `canonical_function_producer`, so it is
    /// carried verbatim. Returns `Cow::Borrowed` when the spine is already
    /// collapsed.
    pub fn with_collapsed_empty_specializations(&self) -> std::borrow::Cow<'_, Self> {
        match self {
            Self::Specialization { base, arguments }
                if arguments.types.is_empty() && arguments.values.is_empty() =>
            {
                std::borrow::Cow::Owned(base.with_collapsed_empty_specializations().into_owned())
            }
            Self::Specialization { base, arguments } => {
                match base.with_collapsed_empty_specializations() {
                    std::borrow::Cow::Borrowed(_) => std::borrow::Cow::Borrowed(self),
                    std::borrow::Cow::Owned(base) => {
                        std::borrow::Cow::Owned(Self::Specialization {
                            base: Arc::new(base),
                            arguments: arguments.clone(),
                        })
                    }
                }
            }
            _ => std::borrow::Cow::Borrowed(self),
        }
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
                element: Arc::new(element.try_map_identities(definition, module)?),
                len: *len,
            },
            Self::Slice { element, name } => TypeInstanceKey::Slice {
                element: Arc::new(element.try_map_identities(definition, module)?),
                name: name.clone(),
            },
            Self::PtrConst(value) => {
                TypeInstanceKey::PtrConst(Arc::new(value.try_map_identities(definition, module)?))
            }
            Self::PtrMut(value) => {
                TypeInstanceKey::PtrMut(Arc::new(value.try_map_identities(definition, module)?))
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
                base: Arc::new(base.try_map_identities(definition, module)?),
                arguments: arguments.try_map_identities(definition, module)?,
            },
            Self::AnonymousMember { owner, member } => FunctionInstanceKey::AnonymousMember {
                owner: Arc::new(owner.try_map_identities(definition, module)?),
                member: member.clone(),
            },
            Self::DropGlue(value) => FunctionInstanceKey::DropGlue(Arc::new(
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
    use ahash::AHashSet;
    use std::collections::BTreeSet;

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
        assert_eq!(AHashSet::from([baseline, same, moved]).len(), 2);
    }
}
