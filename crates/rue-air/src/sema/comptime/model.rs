//! The generic value, type, field, and method algebra.

use super::*;

pub trait ComptimeType: Clone {}

/// Value algebra consumed by the canonical dispatcher. Hosts provide any value
/// representation that can carry these four compile-time forms.
pub trait ComptimeValue: Clone {
    type Type: ComptimeType;
    fn integer(value: i128) -> Self;
    fn boolean(value: bool) -> Self;
    fn unit() -> Self;
    fn type_value(value: Self::Type) -> Self;
    fn as_integer(&self) -> Option<i128>;
    fn as_boolean(&self) -> Option<bool>;
    fn as_type(&self) -> Option<Self::Type>;

    /// Whether a reduced value may participate in an anonymous nominal's
    /// captured value substitution.  Semantic domains with lexical handles
    /// (for example modules and target descriptors) can opt out; ordinary
    /// body values retain the historical all-values behavior.
    fn eligible_for_comptime_capture(&self) -> bool {
        true
    }

    /// Recover optional declared integer metadata carried by a host value.
    /// The ordinary body value domain returns `None`; durable hosts may use
    /// this to retain operand typing after a child reduction.
    fn as_integer_type(&self) -> Option<Self::Type> {
        None
    }

    /// Construct an integer while retaining optional semantic type metadata.
    ///
    /// The ordinary body value domain has no metadata to retain, so its
    /// default is deliberately the historical integer constructor. Durable
    /// hosts may override this to carry the declared integer type alongside
    /// the value without changing the generic engine's value algebra.
    fn integer_typed(value: i128, _ty: Option<Self::Type>) -> Self {
        Self::integer(value)
    }
}

#[derive(Debug, Clone)]
pub struct ComptimeField<N, T> {
    pub name: N,
    pub ty: T,
}

/// A resolved method type used when comparing anonymous structural types.
/// `Self` remains a distinct shape until the enclosing anonymous type has been
/// assigned an identity; all other types have already gone through the
/// engine's structured type resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComptimeMethodType<T> {
    SelfType,
    Concrete(T),
    Unsupported(String),
}

/// A method parameter in an anonymous structural type descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComptimeMethodParameter<T> {
    pub ty: ComptimeMethodType<T>,
    pub mode: rue_rir::RirParamMode,
    pub is_comptime: bool,
    /// Whether the source spelling is the unsupported own `comptime type`
    /// parameter form.  Keeping this bit on the descriptor lets the engine
    /// issue RUE-284 at the declaration without making the host decode RIR.
    pub is_comptime_type: bool,
}

/// Canonical, engine-owned metadata for an anonymous method signature.
/// The descriptor carries only resolved signature facts and semantic names.
/// Method-body registration remains an ordinary local concern; generic hosts
/// never receive a child-RIR reference through this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComptimeMethodDescriptor<N, T> {
    pub name: N,
    pub has_self: bool,
    pub self_mode: rue_rir::RirParamMode,
    pub returns_borrow: bool,
    pub returns_inout: bool,
    pub parameters: Vec<ComptimeMethodParameter<T>>,
    /// Parameter names are decoded by the engine so structural consumers do
    /// not need to reopen the owning RIR.
    pub parameter_names: Vec<N>,
    pub result: ComptimeMethodType<T>,
    pub declaration_span: Span,
}

/// Atomic result of resolving a bare semantic name after lexical and
/// substitution shadows have been checked.  The host owns lookup,
/// dependency observation, and visibility as one operation so a durable
/// adapter cannot observe a value without recording its direct dependency.
#[derive(Debug, Clone)]
pub enum ComptimeNamedValueResolution<V> {
    Known(V),
    RuntimeDependent,
    Missing,
}
