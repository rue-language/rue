//! Host-generic canonical compile-time evaluator.
//!
//! This module owns every recursive RIR edge for compile-time evaluation. Hosts
//! provide semantic facts and side effects through named hooks; they never walk
//! child instructions or invoke another evaluator.

use ahash::{AHashMap, AHashSet};
use rue_rir::{InstData, InstRef, RepeatCount, Rir, SymbolHandle, ValidatedRir};
use rue_span::Span;
use std::hash::Hash;
use std::sync::Arc;

use crate::integer_semantics::{CheckedIntegerResult, IntegerType};
/// Maximum number of entered named comptime frames. Expression recursion does
/// not spend this budget.
pub const MAX_COMPTIME_CALL_DEPTH: usize = 48;

/// An owned RIR program available to one comptime evaluation.
///
/// `InstRef` and all payload ranges are meaningful only with the associated
/// program key. Keeping the validated RIR behind `Arc` lets a durable host
/// register a foreign declaration without requiring `Rir: Clone` or invoking
/// another evaluator on a cache miss.
#[derive(Debug, Clone)]
pub struct ComptimeProgram<S, I> {
    pub rir: Arc<ValidatedRir>,
    pub symbols: Arc<[S]>,
    pub imports: I,
}

/// Evaluation-local registry for request-local and foreign durable programs.
/// The declaration/configuration pair is part of the key so a frame cannot
/// accidentally resolve an `InstRef` against a different specialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ComptimeProgramKey<D, C> {
    pub declaration: D,
    pub configuration: C,
}

#[derive(Debug)]
pub struct ComptimeProgramRegistry<D, C, S, I> {
    programs: AHashMap<ComptimeProgramKey<D, C>, ComptimeProgram<S, I>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeProgramRegistrationError {
    AlreadyRegistered,
}

impl<D, C, S, I> Default for ComptimeProgramRegistry<D, C, S, I>
where
    D: Eq + Hash,
    C: Eq + Hash,
{
    fn default() -> Self {
        Self {
            programs: AHashMap::new(),
        }
    }
}

impl<D, C, S, I> ComptimeProgramRegistry<D, C, S, I>
where
    D: Eq + Hash,
    C: Eq + Hash,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        key: ComptimeProgramKey<D, C>,
        program: ComptimeProgram<S, I>,
    ) -> Result<(), ComptimeProgramRegistrationError> {
        if self.programs.contains_key(&key) {
            return Err(ComptimeProgramRegistrationError::AlreadyRegistered);
        }
        self.programs.insert(key, program);
        Ok(())
    }

    pub fn get(&self, key: &ComptimeProgramKey<D, C>) -> Option<&ComptimeProgram<S, I>> {
        self.programs.get(key)
    }

    pub fn contains_key(&self, key: &ComptimeProgramKey<D, C>) -> bool {
        self.programs.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.programs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.programs.is_empty()
    }

    /// Admit one structured-type authority from the exact registered program
    /// snapshot. The root, arena, symbol table, and stable key are copied only
    /// as cheap owned handles; callers cannot pair a key with another arena.
    pub fn structured_type_authority<Scope>(
        &self,
        key: &ComptimeProgramKey<D, C>,
        root_scope: Scope,
        root: rue_rir::RirTypeSyntaxRef,
    ) -> Option<
        crate::semantic_type_resolution::RegisteredComptimeStructuredTypeAuthority<D, C, Scope, S>,
    >
    where
        D: Clone,
        C: Clone,
        S: AsRef<str>,
    {
        let program = self.programs.get(key)?;
        program.rir.type_syntax().node(root)?;
        if !crate::semantic_type_resolution::registered_symbol_authority_is_valid(
            program.rir.type_syntax(),
            &program.symbols,
        ) {
            return None;
        }
        Some(
            crate::semantic_type_resolution::ComptimeStructuredTypeAuthority::from_registered(
                key.clone(),
                root_scope,
                program.rir.type_syntax().clone(),
                Arc::clone(&program.symbols),
                root,
            ),
        )
    }
}

/// Stable key for a completed call fact. The argument slices preserve source
/// order; callers must not construct them from an unordered map iteration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ComptimeCallKey<D, C, T, V> {
    pub declaration: D,
    pub configuration: C,
    pub type_arguments: Arc<[T]>,
    pub value_arguments: Arc<[V]>,
}

#[derive(Debug)]
pub enum ComptimeCallMemoLookup<'a, V> {
    Memoized(&'a ComptimeMemoizedOutcome<V>),
    Miss,
}

/// Outcomes safe to retain as completed semantic facts. Deterministic traps
/// are included; host failures and aborts are deliberately excluded because
/// cancellation and transient query errors must never become cache hits.
#[derive(Debug, Clone)]
pub enum ComptimeMemoizedOutcome<V> {
    Known(V),
    RuntimeDependent,
    NotReady,
    UnsupportedContext,
    Trap(ComptimeTrap),
}

impl<V> ComptimeMemoizedOutcome<V> {
    pub fn into_outcome<F>(self) -> ComptimeOutcome<V, F> {
        match self {
            Self::Known(value) => ComptimeOutcome::Known(value),
            Self::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            Self::NotReady => ComptimeOutcome::NotReady,
            Self::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            Self::Trap(trap) => ComptimeOutcome::Trap(trap),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeMemoInsertError {
    AlreadyMemoized,
}

/// Completed call facts retained only for the lifetime of one evaluation.
/// A missing key is intentionally distinct from a memoized not-ready or
/// runtime-dependent outcome, so callers can turn misses into `Enter` frames.
#[derive(Debug)]
pub struct ComptimeCompletedCallMemo<D, C, T, V, R> {
    outcomes: AHashMap<ComptimeCallKey<D, C, T, V>, ComptimeMemoizedOutcome<R>>,
}

impl<D, C, T, V, R> Default for ComptimeCompletedCallMemo<D, C, T, V, R> {
    fn default() -> Self {
        Self {
            outcomes: AHashMap::new(),
        }
    }
}

impl<D, C, T, V, R> ComptimeCompletedCallMemo<D, C, T, V, R>
where
    D: Eq + Hash,
    C: Eq + Hash,
    T: Eq + Hash,
    V: Eq + Hash,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup<'a>(
        &'a self,
        key: &ComptimeCallKey<D, C, T, V>,
    ) -> ComptimeCallMemoLookup<'a, R> {
        if let Some(outcome) = self.outcomes.get(key) {
            ComptimeCallMemoLookup::Memoized(outcome)
        } else {
            ComptimeCallMemoLookup::Miss
        }
    }

    pub fn insert(
        &mut self,
        key: ComptimeCallKey<D, C, T, V>,
        outcome: ComptimeMemoizedOutcome<R>,
    ) -> Result<(), ComptimeMemoInsertError> {
        if self.outcomes.contains_key(&key) {
            return Err(ComptimeMemoInsertError::AlreadyMemoized);
        }
        self.outcomes.insert(key, outcome);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.outcomes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.outcomes.is_empty()
    }
}

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

/// A declaration-level constant fact in the engine's value domain.  The
/// adapter supplies only the metadata needed for dependency/privacy handling
/// and a value when that declaration is representable by the current domain.
#[derive(Debug, Clone)]
pub struct ComptimeConstInfo<V> {
    pub is_pub: bool,
    pub span: Span,
    pub value: Option<V>,
}

/// An expression argument passed to a semantic intrinsic hook. String
/// literals are represented as their interned semantic name instead of being
/// forced through the four-value comptime algebra; all other arguments have
/// already been recursively evaluated by [`ComptimeEngine`].
#[derive(Debug, Clone)]
pub enum ComptimeIntrinsicArgument<V, N> {
    Value(V),
    String(N),
}

/// The semantic operation whose source occurrence is being resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeSiteKind {
    Intrinsic,
    Import,
    EnumVariant,
    Member,
}

/// An engine-owned semantic site identity. The owning program, operation kind,
/// and source-order occurrence make equal instruction indices and spans from
/// different programs distinct without exposing an instruction reference to a
/// host.
#[derive(Debug, Clone)]
pub struct ComptimeSite<P> {
    program: P,
    kind: ComptimeSiteKind,
    occurrence: u32,
    span: Span,
}

impl<P: Clone> ComptimeSite<P> {
    fn new(program: P, kind: ComptimeSiteKind, occurrence: u32, span: Span) -> Self {
        Self {
            program,
            kind,
            occurrence,
            span,
        }
    }

    pub fn program(&self) -> &P {
        &self.program
    }

    pub fn kind(&self) -> ComptimeSiteKind {
        self.kind
    }

    /// Source-order occurrence within the semantic operation kind. This is
    /// never an RIR index and cannot be used to retrieve an instruction.
    pub fn occurrence(&self) -> u32 {
        self.occurrence
    }

    pub fn span(&self) -> Span {
        self.span
    }
}

pub trait ComptimeName: Clone + Eq + Hash {}

pub trait ComptimeFile: Clone + Eq + Hash {}

pub trait ComptimeIdentity: Clone {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeAnonymousKind {
    Struct,
    Enum,
}

/// Lexical and substitution state shared by the canonical comptime engine.
/// Name and file identities are supplied by the host; the evaluator does not
/// depend on the local interner or file-id representation.
pub struct ComptimeEnv<'a, V, T, N, F, I>
where
    V: ComptimeValue<Type = T>,
    T: ComptimeType,
    N: ComptimeName,
    F: ComptimeFile,
    I: ComptimeIdentity,
{
    pub canonical_identity: Option<I>,
    pub type_subst: AHashMap<N, T>,
    pub value_subst: AHashMap<N, V>,
    pub resolved_types: Option<&'a AHashMap<InstRef, T>>,
    pub runtime_local_names: AHashSet<N>,
    pub runtime_binding_names: AHashSet<N>,
    pub locals: AHashMap<N, V>,
    pub const_module_members: AHashMap<InstRef, V>,
    pub defining_file: Option<F>,
    /// Expected result for the active frame. This is deliberately carried in
    /// the environment rather than keyed by program so concurrent/nested
    /// instantiations cannot observe one another's integer context.
    pub expected_result: Option<T>,
}

impl<'a, V, T, N, F, I> ComptimeEnv<'a, V, T, N, F, I>
where
    V: ComptimeValue<Type = T>,
    T: ComptimeType,
    N: ComptimeName,
    F: ComptimeFile,
    I: ComptimeIdentity,
{
    pub(crate) fn substs_with_locals(&self) -> (AHashMap<N, T>, AHashMap<N, V>) {
        let mut type_subst = self.type_subst.clone();
        let mut value_subst = self.value_subst.clone();
        for (name, val) in &self.locals {
            if let Some(t) = val.as_type() {
                type_subst.insert(name.clone(), t);
            } else {
                value_subst.insert(name.clone(), val.clone());
            }
        }
        (type_subst, value_subst)
    }

    pub fn new() -> Self {
        Self {
            canonical_identity: None,
            type_subst: AHashMap::new(),
            value_subst: AHashMap::new(),
            resolved_types: None,
            runtime_local_names: AHashSet::new(),
            runtime_binding_names: AHashSet::new(),
            locals: AHashMap::new(),
            const_module_members: AHashMap::new(),
            defining_file: None,
            expected_result: None,
        }
    }

    pub fn with_subst(type_subst: &AHashMap<N, T>, value_subst: &AHashMap<N, V>) -> Self {
        Self {
            canonical_identity: None,
            type_subst: type_subst.clone(),
            value_subst: value_subst.clone(),
            resolved_types: None,
            runtime_local_names: AHashSet::new(),
            runtime_binding_names: AHashSet::new(),
            locals: AHashMap::new(),
            const_module_members: AHashMap::new(),
            defining_file: None,
            expected_result: None,
        }
    }
}

impl<'a, V, T, N, F, I> Default for ComptimeEnv<'a, V, T, N, F, I>
where
    V: ComptimeValue<Type = T>,
    T: ComptimeType,
    N: ComptimeName,
    F: ComptimeFile,
    I: ComptimeIdentity,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod value_domain_tests {
    use super::*;
    use lasso::Key;
    use rue_rir::{Inst, RirEditor, RirValidationContext};
    use std::cell::{Cell, RefCell};

    thread_local! {
        static LABEL_CALLS: Cell<usize> = const { Cell::new(0) };
        static TICKET_EVENTS: RefCell<Vec<(usize, bool)>> = const { RefCell::new(Vec::new()) };
        static INTEGER_HINTS: RefCell<Vec<Option<FakeType>>> = const { RefCell::new(Vec::new()) };
        static METHOD_FAILURES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        static TYPE_RESOLUTION_CALLS: Cell<usize> = const { Cell::new(0) };
    }

    #[derive(Clone, Debug, PartialEq)]
    enum FakeValue {
        Integer(i128),
        TypedInteger(i128, FakeType),
        Boolean(bool),
        Unit,
        Type(FakeType),
    }

    #[derive(Clone, Debug, PartialEq, Copy)]
    struct FakeType(u8);

    impl ComptimeType for FakeType {}

    impl ComptimeValue for FakeValue {
        type Type = FakeType;
        fn integer(value: i128) -> Self {
            Self::Integer(value)
        }
        fn boolean(value: bool) -> Self {
            Self::Boolean(value)
        }
        fn unit() -> Self {
            Self::Unit
        }
        fn type_value(_value: FakeType) -> Self {
            Self::Type(_value)
        }
        fn as_integer(&self) -> Option<i128> {
            match self {
                Self::Integer(value) | Self::TypedInteger(value, _) => Some(*value),
                _ => None,
            }
        }

        fn as_integer_type(&self) -> Option<FakeType> {
            match self {
                Self::TypedInteger(_, ty) => Some(*ty),
                _ => None,
            }
        }
        fn as_boolean(&self) -> Option<bool> {
            match self {
                Self::Boolean(value) => Some(*value),
                _ => None,
            }
        }
        fn as_type(&self) -> Option<FakeType> {
            match self {
                Self::Type(value) => Some(*value),
                _ => None,
            }
        }

        fn integer_typed(value: i128, ty: Option<FakeType>) -> Self {
            ty.map_or(Self::Integer(value), |ty| Self::TypedInteger(value, ty))
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct FakeName {
        ordinal: u32,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct FakeFile {
        index: u32,
    }

    impl ComptimeName for FakeName {}
    impl ComptimeFile for FakeFile {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeIdentity {
        token: u32,
    }

    impl ComptimeIdentity for FakeIdentity {}

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FakeFailure {
        Generic,
        NonFunctionMethod,
        OwnComptimeTypeParameter,
    }

    // Keep the existing compact fixture construction readable while allowing
    // the decoder regressions to assert their exact semantic failure reason.
    const FAKE_FAILURE: FakeFailure = FakeFailure::Generic;

    enum FakePreparedCall {
        Enter {
            program: usize,
            body: InstRef,
            expected: Option<FakeType>,
            name_bindings: AHashMap<FakeName, FakeName>,
        },
        UnnamedEnter {
            program: usize,
            body: InstRef,
        },
        Memoized(ComptimeOutcome<FakeValue, FakeFailure>),
    }

    #[derive(Clone)]
    enum FakeFinishOutcome {
        Identity,
        Structured(Vec<FakeStructuredPreparation>),
        RuntimeDependent,
        NotReady,
        UnsupportedContext,
        Trap,
        HostFailure,
        Abort,
        AbortFromPrepare,
        AbortFromArithmetic,
        CanonicalFailure,
    }

    #[derive(Clone, Copy)]
    enum FakeStructuredPreparation {
        Enter,
        Memoized,
        RuntimeDependent,
        NotReady,
        UnsupportedContext,
        Trap,
        HostFailure,
        Abort,
    }

    struct FakeStructuredSuspension {
        preparations: Vec<FakeStructuredPreparation>,
        index: usize,
    }

    impl super::structured_type_seal::Sealed for FakeStructuredSuspension {}
    impl ComptimeStructuredTypeSuspension for FakeStructuredSuspension {}

    struct FakeHost {
        programs: Vec<Rir>,
        type_symbol: SymbolHandle,
        constant: Option<(FakeFile, FakeName, ComptimeConstInfo<FakeValue>)>,
        dependencies: Vec<(FakeFile, FakeName)>,
        call_plans: AHashMap<u32, FakePreparedCall>,
        recursive: Option<(usize, InstRef, InstRef, Option<usize>)>,
        enter_count: usize,
        finish_outcome: FakeFinishOutcome,
        finished: Vec<(usize, Option<FakeType>)>,
        float_evaluations: Cell<usize>,
    }

    impl FakeHost {
        fn admits_durable_forms(&self) -> bool {
            matches!(self.finish_outcome, FakeFinishOutcome::Identity)
        }
    }

    impl ComptimeHost for FakeHost {
        type Type = FakeType;
        type Value = FakeValue;
        type Name = FakeName;
        type File = FakeFile;
        type CanonicalIdentity = FakeIdentity;
        type AnonymousIdentity = FakeIdentity;
        type ProgramKey = usize;
        type Failure = FakeFailure;
        type CallAdmission = ();
        type CompletionTicket = usize;
        type AnonymousStructId = u32;
        type StructuredTypeSuspension = FakeStructuredSuspension;
        fn program_rir(&self, program: &Self::ProgramKey) -> &Rir {
            &self.programs[*program]
        }
        fn name_from_symbol(&self, program: &Self::ProgramKey, symbol: SymbolHandle) -> Self::Name {
            FakeName {
                ordinal: symbol.issuing_interner_ordinal() as u32 + (*program as u32) * 1000,
            }
        }
        fn display_name(&self, name: &Self::Name) -> String {
            if name.ordinal == self.type_symbol.issuing_interner_ordinal() as u32 {
                "type".to_owned()
            } else if name.ordinal % 1000 == 0 {
                "import".to_owned()
            } else {
                format!("fake-name-{}", name.ordinal)
            }
        }
        fn file_from_span(&self, span: &Span) -> Self::File {
            FakeFile {
                index: span.file_id.index(),
            }
        }
        fn value_const(
            &self,
            key: &(Self::File, Self::Name),
        ) -> ComptimeHostResult<Option<ComptimeConstInfo<Self::Value>>, Self::Failure> {
            Ok(self
                .constant
                .as_ref()
                .filter(|(file, name, _)| *file == key.0 && *name == key.1)
                .map(|(_, _, info)| info.clone()))
        }
        fn match_pattern(
            &self,
            _program: &Self::ProgramKey,
            _pattern: &rue_rir::RirPatternView<'_>,
            _value: &Self::Value,
        ) -> Option<bool> {
            None
        }
        fn require_preview(
            &self,
            _feature: rue_error::PreviewFeature,
            _what: &str,
            _span: Span,
        ) -> ComptimeHostResult<(), Self::Failure> {
            Ok(())
        }
        fn depth_exceeded(&self, _name: &Self::Name, _depth: usize, _span: Span) -> Self::Failure {
            FAKE_FAILURE
        }
        fn literal_out_of_range(
            &self,
            _value: u64,
            _ty: &Self::Type,
            _span: Span,
        ) -> Self::Failure {
            FAKE_FAILURE
        }
        fn float_not_implemented(&self, _span: Span) -> Self::Failure {
            self.float_evaluations.set(self.float_evaluations.get() + 1);
            FAKE_FAILURE
        }
        fn cannot_negate(&self, _ty: &Self::Type, _span: Span) -> Self::Failure {
            FAKE_FAILURE
        }
        fn comptime_panic(&self, _reason: String, _span: Span) -> Self::Failure {
            FAKE_FAILURE
        }
        fn unsupported_anon_method_type_param(
            &self,
            _method_span: Span,
            _method_name: &str,
        ) -> Self::Failure {
            METHOD_FAILURES.with(|failures| failures.borrow_mut().push("own_type"));
            FakeFailure::OwnComptimeTypeParameter
        }
        fn non_function_anon_method(&self, _method_span: Span) -> Self::Failure {
            METHOD_FAILURES.with(|failures| failures.borrow_mut().push("non_function"));
            FakeFailure::NonFunctionMethod
        }
        fn record_value_const_dependency(&mut self, file: &Self::File, name: &Self::Name) {
            self.dependencies.push((file.clone(), name.clone()));
        }
        fn resolve_named_array_length(
            &mut self,
            _name: &Self::Name,
            _span: Span,
            _values: Option<&AHashMap<Self::Name, Self::Value>>,
        ) -> ComptimeHostResult<u64, Self::Failure> {
            Ok(0)
        }
        fn rir_type_named_symbol(
            &self,
            _program: &Self::ProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
        ) -> Option<Self::Name> {
            if matches!(self.finish_outcome, FakeFinishOutcome::Structured(_)) {
                None
            } else {
                Some(self.name_from_symbol(&0, self.type_symbol))
            }
        }
        fn render_rir_type(
            &self,
            _program: &Self::ProgramKey,
            syntax: rue_rir::RirTypeSyntaxRef,
        ) -> String {
            format!("{syntax:?}")
        }
        fn get_or_create_array_type(&mut self, _element: Self::Type, _length: u64) -> Self::Type {
            panic!("fake host does not construct array types")
        }
        fn find_or_create_anon_struct(
            &mut self,
            _identity: Self::AnonymousIdentity,
            _fields: &[ComptimeField<Self::Name, Self::Type>],
            _sigs: &[ComptimeMethodDescriptor<Self::Name, Self::Type>],
            _captured: &AHashMap<Self::Name, Self::Value>,
        ) -> ComptimeHostResult<(Self::Type, bool), Self::Failure> {
            panic!("fake host does not construct anonymous structs")
        }
        fn find_or_create_anon_enum(
            &mut self,
            _identity: Self::AnonymousIdentity,
            _names: &[String],
            _payloads: &[Vec<Self::Type>],
        ) -> ComptimeHostResult<Self::Type, Self::Failure> {
            panic!("fake host does not construct anonymous enums")
        }
        fn anonymous_struct_id(&self, _ty: &Self::Type) -> Option<Self::AnonymousStructId> {
            None
        }
        fn has_method(&self, _key: &Self::AnonymousStructId, _method: Self::Name) -> bool {
            false
        }
        fn check_unqualified_visibility(
            &self,
            _item_kind: &str,
            _name: &Self::Name,
            _defining_file_id: Self::File,
            _is_pub: bool,
            _span: Span,
        ) -> ComptimeHostResult<(), Self::Failure> {
            Ok(())
        }
        fn check_require_droppable(
            &mut self,
            _ty: Self::Type,
            _span: Span,
        ) -> ComptimeHostResult<(), Self::Failure> {
            Ok(())
        }
        fn check_trivially_droppable(
            &mut self,
            _ty: Self::Type,
            _span: Span,
        ) -> ComptimeHostResult<(), Self::Failure> {
            Ok(())
        }
        fn const_expr_type(
            &self,
            _program: &Self::ProgramKey,
            _env: &ComptimeEnv<'_, Self::Value, Self::Type, Self::Name, Self::File, FakeIdentity>,
            inst_ref: InstRef,
        ) -> Option<Self::Type> {
            (inst_ref.as_u32() == 2).then_some(FakeType(8))
        }
        fn integer_operation_type(
            &self,
            resolved_type: Option<&Self::Type>,
            lhs: &Self::Value,
            rhs: &Self::Value,
            _span: Span,
        ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure> {
            INTEGER_HINTS.with(|hints| hints.borrow_mut().push(resolved_type.copied()));
            if let (Some(lhs), Some(rhs)) = (lhs.as_integer_type(), rhs.as_integer_type()) {
                if lhs != rhs {
                    return Err(FAKE_FAILURE.into());
                }
            }
            Ok(resolved_type
                .cloned()
                .or_else(|| lhs.as_integer_type())
                .or_else(|| rhs.as_integer_type()))
        }
        fn finish_arith(
            &self,
            result: CheckedIntegerResult,
            _ty: Option<Self::Type>,
            _op: &str,
            _span: Span,
        ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
            if matches!(self.finish_outcome, FakeFinishOutcome::AbortFromArithmetic) {
                return Err(ComptimeHostError::Abort(FAKE_FAILURE));
            }
            Ok(result.checked().map(FakeValue::integer))
        }
        fn type_name(&self, ty: &Self::Type) -> String {
            format!("fake-type-{}", ty.0)
        }
        fn type_is_unsigned(&self, _ty: &Self::Type) -> bool {
            false
        }
        fn type_integer_semantics(&self, ty: &Self::Type) -> Option<IntegerType> {
            (ty.0 != 99).then(|| IntegerType::new(8, true)).flatten()
        }
        fn record_named_type_dependency(&mut self, _ty: &Self::Type) {}
        fn resolve_named_type_value(
            &mut self,
            _name: Self::Name,
            _span: Span,
        ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure> {
            Ok(Some(FakeType(7)))
        }
        fn resolve_comptime_type_path(
            &mut self,
            _file: Self::File,
            _segments: &[Self::Name],
            _span: Span,
        ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
            Ok(None)
        }
        fn resolve_module_comptime_callable(
            &mut self,
            _file_id: Self::File,
            _segments: &[Self::Name],
            _method: Self::Name,
            _span: Span,
        ) -> ComptimeHostResult<Option<Self::Name>, Self::Failure> {
            Ok(None)
        }
        fn admit_comptime_call(
            &mut self,
            name: Self::Name,
            _arg_count: usize,
            _arg_modes: &[ComptimeArgMode],
            _env: &mut ComptimeEnv<
                '_,
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                FakeIdentity,
            >,
            _name_is_resolved_key: bool,
        ) -> ComptimeHostResult<
            Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
            Self::Failure,
        > {
            Ok(Some(ComptimeCallAdmission { name, payload: () }))
        }
        fn bind_comptime_call(
            &self,
            _admission: &ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
            _values: &[Self::Value],
            _span: Span,
        ) -> ComptimeHostResult<
            Option<(
                AHashMap<Self::Name, Self::Type>,
                AHashMap<Self::Name, Self::Value>,
            )>,
            Self::Failure,
        > {
            Ok(Some((AHashMap::new(), AHashMap::new())))
        }
        fn prepare_comptime_call(
            &mut self,
            admission: ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
            _types: AHashMap<Self::Name, Self::Type>,
            _values: AHashMap<Self::Name, Self::Value>,
            _span: Span,
        ) -> ComptimeHostResult<
            Option<
                ComptimeCallPreparation<
                    Self::Value,
                    Self::Type,
                    Self::Name,
                    Self::File,
                    Self::ProgramKey,
                    Self::CanonicalIdentity,
                    Self::Failure,
                    Self::CompletionTicket,
                >,
            >,
            Self::Failure,
        > {
            if matches!(self.finish_outcome, FakeFinishOutcome::AbortFromPrepare) {
                return Err(ComptimeHostError::Abort(FAKE_FAILURE));
            }
            if let Some((max_enters, call_body, terminal_body, memoized_at)) = self.recursive {
                if memoized_at == Some(self.enter_count) {
                    return Ok(Some(ComptimeCallPreparation::Memoized(
                        ComptimeOutcome::Known(FakeValue::Integer(1)),
                    )));
                }
                let expected = Some(FakeType(7 + self.enter_count as u8));
                self.enter_count += 1;
                let body = if self.enter_count == max_enters {
                    terminal_body
                } else {
                    call_body
                };
                return Ok(Some(ComptimeCallPreparation::Enter {
                    frame: ComptimeFrame {
                        program: 1,
                        body,
                        name: Some(admission.name),
                        context: Some(FakeFile { index: 0 }),
                        span: Span::new(0, 0),
                        function_span: Span::new(0, 0),
                        type_bindings: AHashMap::new(),
                        value_bindings: AHashMap::new(),
                        name_bindings: AHashMap::new(),
                        call_identity: None,
                        expected_result: expected,
                    },
                    ticket: self.enter_count,
                }));
            }
            let Some(plan) = self.call_plans.remove(&admission.name.ordinal) else {
                return Ok(None);
            };
            Ok(Some(match plan {
                FakePreparedCall::Enter {
                    program,
                    body,
                    expected,
                    name_bindings,
                } => ComptimeCallPreparation::Enter {
                    frame: ComptimeFrame {
                        program,
                        body,
                        name: Some(admission.name),
                        context: Some(FakeFile {
                            index: program as u32,
                        }),
                        span: Span::new(0, 0),
                        function_span: Span::new(0, 0),
                        type_bindings: AHashMap::new(),
                        value_bindings: AHashMap::new(),
                        name_bindings,
                        call_identity: None,
                        expected_result: expected,
                    },
                    ticket: program,
                },
                FakePreparedCall::UnnamedEnter { program, body } => {
                    ComptimeCallPreparation::Enter {
                        frame: ComptimeFrame {
                            program,
                            body,
                            name: None,
                            context: Some(FakeFile {
                                index: program as u32,
                            }),
                            span: Span::new(0, 0),
                            function_span: Span::new(0, 0),
                            type_bindings: AHashMap::new(),
                            value_bindings: AHashMap::new(),
                            name_bindings: AHashMap::new(),
                            call_identity: None,
                            expected_result: None,
                        },
                        ticket: 777,
                    }
                }
                FakePreparedCall::Memoized(outcome) => ComptimeCallPreparation::Memoized(outcome),
            }))
        }
        fn finish_comptime_call(
            &mut self,
            frame: &ComptimeFrame<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
            >,
            ticket: Self::CompletionTicket,
            result: ComptimeOutcome<Self::Value, Self::Failure>,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            self.finished.push((frame.program, frame.expected_result));
            TICKET_EVENTS.with(|events| {
                events.borrow_mut().push((ticket, false));
            });
            match self.finish_outcome {
                FakeFinishOutcome::Identity
                | FakeFinishOutcome::AbortFromPrepare
                | FakeFinishOutcome::AbortFromArithmetic
                | FakeFinishOutcome::CanonicalFailure => result,
                FakeFinishOutcome::Structured(_) => result,
                FakeFinishOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
                FakeFinishOutcome::NotReady => ComptimeOutcome::NotReady,
                FakeFinishOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
                FakeFinishOutcome::Trap => ComptimeOutcome::Trap(ComptimeTrap {
                    operation: "fake trap",
                    span: Span::new(0, 0),
                }),
                FakeFinishOutcome::HostFailure => ComptimeOutcome::HostFailure(FAKE_FAILURE),
                FakeFinishOutcome::Abort => ComptimeOutcome::Abort(FAKE_FAILURE),
            }
        }
        fn enter_comptime_call(
            &mut self,
            _frame: &ComptimeFrame<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
            >,
            ticket: &Self::CompletionTicket,
        ) -> ComptimeHostResult<(), Self::Failure> {
            TICKET_EVENTS.with(|events| {
                events.borrow_mut().push((*ticket, true));
            });
            Ok(())
        }
        fn label_ctor_instantiation_site(error: Self::Failure, _call_span: Span) -> Self::Failure {
            LABEL_CALLS.with(|calls| calls.set(calls.get() + 1));
            error
        }
        fn canonical_function_producer(
            &self,
            name: Self::Name,
            _types: &AHashMap<Self::Name, Self::Type>,
            _values: &AHashMap<Self::Name, Self::Value>,
            _span: Span,
        ) -> ComptimeHostResult<Self::CanonicalIdentity, Self::Failure> {
            if matches!(self.finish_outcome, FakeFinishOutcome::CanonicalFailure) {
                return Err(FAKE_FAILURE.into());
            }
            Ok(FakeIdentity {
                token: name.ordinal,
            })
        }
        fn issue_anonymous_identity(
            &self,
            _program: &Self::ProgramKey,
            _kind: ComptimeAnonymousKind,
            producer: &Self::CanonicalIdentity,
            _anchor: &rue_rir::RirStructuralAnchor,
        ) -> Self::AnonymousIdentity {
            producer.clone()
        }
        fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
            &mut self,
            _program: &Self::ProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
            _types: &AHashMap<Self::Name, Self::Type>,
            _values: &AHashMap<Self::Name, Self::Value>,
            _span: Span,
        ) -> Option<Self::Type> {
            TYPE_RESOLUTION_CALLS.with(|calls| calls.set(calls.get() + 1));
            None
        }
        fn set_anon_struct_type_subst(
            &mut self,
            _struct_id: &Self::AnonymousStructId,
            _subst: AHashMap<Self::Name, Self::Type>,
        ) {
        }

        fn resolve_string_const(
            &mut self,
            content: Self::Name,
            _span: Span,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            self.dependencies
                .push((FakeFile { index: u32::MAX }, content));
            ComptimeOutcome::Known(FakeValue::Integer(17))
        }

        fn resolve_comptime_intrinsic(
            &mut self,
            name: Self::Name,
            arguments: &[ComptimeIntrinsicArgument<Self::Value, Self::Name>],
            _site: &ComptimeSite<Self::ProgramKey>,
            _span: Span,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            // Encode the argument shape in the existing observation log so
            // these tests do not need a second fake-host state channel.
            let string_count = arguments
                .iter()
                .filter(|argument| matches!(argument, ComptimeIntrinsicArgument::String(_)))
                .count();
            self.dependencies.push((
                FakeFile {
                    index: 0xFFFF_FFFE - (*_site.program() as u32),
                },
                FakeName {
                    ordinal: name.ordinal + string_count as u32 + _site.occurrence(),
                },
            ));
            ComptimeOutcome::Known(FakeValue::Integer(
                arguments
                    .iter()
                    .filter_map(|argument| match argument {
                        ComptimeIntrinsicArgument::Value(value) => value.as_integer(),
                        ComptimeIntrinsicArgument::String(_) => None,
                    })
                    .sum(),
            ))
        }

        fn resolve_comptime_enum_variant(
            &mut self,
            module: Option<Self::Value>,
            type_name: Self::Name,
            variant: Self::Name,
            _site: &ComptimeSite<Self::ProgramKey>,
            _span: Span,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            self.dependencies.push((
                FakeFile {
                    index: module.map_or(0xFFFF_FFFD, |_| 0xFFFF_FFFC),
                },
                FakeName {
                    ordinal: type_name.ordinal ^ variant.ordinal,
                },
            ));
            ComptimeOutcome::Known(FakeValue::Integer(23))
        }

        fn finish_checked(
            &mut self,
            value: Self::Value,
            _span: Span,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            self.dependencies
                .push((FakeFile { index: 0xFFFF_FFFB }, FakeName { ordinal: 0 }));
            ComptimeOutcome::Known(value)
        }

        fn admit_comptime_intrinsic(
            &mut self,
            _name: Self::Name,
            _site: &ComptimeSite<Self::ProgramKey>,
        ) -> ComptimeHostResult<bool, Self::Failure> {
            Ok(self.admits_durable_forms())
        }

        fn admit_comptime_enum_variant(
            &mut self,
            _type_name: Self::Name,
            _variant: Self::Name,
            _site: &ComptimeSite<Self::ProgramKey>,
        ) -> ComptimeHostResult<bool, Self::Failure> {
            Ok(self.admits_durable_forms())
        }

        fn admit_comptime_member(
            &mut self,
            _field: Self::Name,
            _site: &ComptimeSite<Self::ProgramKey>,
        ) -> ComptimeHostResult<bool, Self::Failure> {
            Ok(self.admits_durable_forms())
        }

        fn resolve_comptime_member(
            &mut self,
            _base: Self::Value,
            _field: Self::Name,
            _site: &ComptimeSite<Self::ProgramKey>,
            _span: Span,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            ComptimeOutcome::Known(FakeValue::Integer(31))
        }

        fn reject_non_type_array_repeat(
            &mut self,
            _value: Self::Value,
            _span: Span,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            self.dependencies
                .push((FakeFile { index: 0xFFFF_FFFA }, FakeName { ordinal: 0 }));
            ComptimeOutcome::RuntimeDependent
        }

        fn allow_checked_comptime(&self) -> bool {
            self.admits_durable_forms()
        }

        fn compare_comptime_values(
            &mut self,
            lhs: &Self::Value,
            rhs: &Self::Value,
            equal: bool,
            _span: Span,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            let (Some(lhs), Some(rhs)) = (lhs.as_type(), rhs.as_type()) else {
                return ComptimeOutcome::RuntimeDependent;
            };
            ComptimeOutcome::Known(FakeValue::boolean(if equal {
                lhs == rhs
            } else {
                lhs != rhs
            }))
        }

        fn begin_comptime_type_syntax(
            &mut self,
            _program: &Self::ProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
            _types: &AHashMap<Self::Name, Self::Type>,
            _values: &AHashMap<Self::Name, Self::Value>,
            _span: Span,
        ) -> ComptimeOutcome<
            ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
            Self::Failure,
        > {
            let FakeFinishOutcome::Structured(preparations) =
                std::mem::replace(&mut self.finish_outcome, FakeFinishOutcome::Identity)
            else {
                return ComptimeOutcome::RuntimeDependent;
            };
            ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Suspended(
                FakeStructuredSuspension {
                    preparations,
                    index: 0,
                },
            ))
        }

        fn prepare_structured_type_call(
            &mut self,
            suspension: &Self::StructuredTypeSuspension,
        ) -> ComptimeOutcome<
            Option<
                ComptimeCallPreparation<
                    Self::Value,
                    Self::Type,
                    Self::Name,
                    Self::File,
                    Self::ProgramKey,
                    Self::CanonicalIdentity,
                    Self::Failure,
                    Self::CompletionTicket,
                >,
            >,
            Self::Failure,
        > {
            match suspension.preparations[suspension.index] {
                FakeStructuredPreparation::Enter => {
                    ComptimeOutcome::Known(Some(ComptimeCallPreparation::Enter {
                        frame: ComptimeFrame {
                            program: 1,
                            body: InstRef::from_raw(0),
                            name: Some(FakeName { ordinal: 1 }),
                            context: Some(FakeFile { index: 0 }),
                            span: Span::new(0, 0),
                            function_span: Span::new(0, 0),
                            type_bindings: AHashMap::new(),
                            value_bindings: AHashMap::new(),
                            name_bindings: AHashMap::new(),
                            call_identity: None,
                            expected_result: None,
                        },
                        ticket: 0,
                    }))
                }
                FakeStructuredPreparation::Memoized => {
                    ComptimeOutcome::Known(Some(ComptimeCallPreparation::Memoized(
                        ComptimeOutcome::Known(FakeValue::Integer(1)),
                    )))
                }
                FakeStructuredPreparation::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
                FakeStructuredPreparation::NotReady => ComptimeOutcome::NotReady,
                FakeStructuredPreparation::UnsupportedContext => {
                    ComptimeOutcome::UnsupportedContext
                }
                FakeStructuredPreparation::Trap => ComptimeOutcome::Trap(ComptimeTrap {
                    operation: "structured fake trap",
                    span: Span::new(0, 0),
                }),
                FakeStructuredPreparation::HostFailure => {
                    ComptimeOutcome::HostFailure(FAKE_FAILURE)
                }
                FakeStructuredPreparation::Abort => ComptimeOutcome::Abort(FAKE_FAILURE),
            }
        }

        fn resume_structured_type_call(
            &mut self,
            suspension: Self::StructuredTypeSuspension,
            result: ComptimeOutcome<Self::Value, Self::Failure>,
        ) -> ComptimeOutcome<
            ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
            Self::Failure,
        > {
            if !matches!(&result, ComptimeOutcome::Known(_)) {
                // The sentinel makes the outcome-funnel test observe that
                // every terminal reduction was handed back to the host.
                self.finished.push((usize::MAX, None));
            }
            match result {
                ComptimeOutcome::Known(_)
                    if suspension.index + 1 < suspension.preparations.len() =>
                {
                    ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Suspended(
                        FakeStructuredSuspension {
                            preparations: suspension.preparations,
                            index: suspension.index + 1,
                        },
                    ))
                }
                ComptimeOutcome::Known(_) => {
                    ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(FakeType(9)))
                }
                ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
                ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
                ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
                ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
                ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
                ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
            }
        }
    }

    #[test]
    fn structured_type_engine_uses_one_existing_call_stack() {
        let mut editor = rue_rir::RirEditor::new();
        let root = editor.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
            },
            span: Span::new(0, 0),
        });
        let interner = lasso::ThreadedRodeo::new();
        let mut child_editor = rue_rir::RirEditor::new();
        child_editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish(), child_editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Structured(vec![FakeStructuredPreparation::Enter]),
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Known(FakeValue::Type(FakeType(9)))
        ));
        assert_eq!(host.finished.len(), 1);
        assert_eq!(host.finished[0].0, 1);
    }

    #[test]
    fn structured_type_engine_passes_every_terminal_outcome_through_resume() {
        for preparation in [
            FakeStructuredPreparation::RuntimeDependent,
            FakeStructuredPreparation::NotReady,
            FakeStructuredPreparation::UnsupportedContext,
            FakeStructuredPreparation::Trap,
            FakeStructuredPreparation::HostFailure,
            FakeStructuredPreparation::Abort,
        ] {
            let mut editor = rue_rir::RirEditor::new();
            let root = editor.add_inst(rue_rir::Inst {
                data: InstData::TypeConst {
                    type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
                },
                span: Span::new(0, 0),
            });
            let interner = lasso::ThreadedRodeo::new();
            let mut child_editor = rue_rir::RirEditor::new();
            child_editor.add_inst(rue_rir::Inst {
                data: InstData::IntConst(1),
                span: Span::new(0, 0),
            });
            let mut host = FakeHost {
                programs: vec![editor.finish(), child_editor.finish()],
                type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
                constant: None,
                dependencies: Vec::new(),
                call_plans: AHashMap::new(),
                recursive: None,
                enter_count: 0,
                finish_outcome: FakeFinishOutcome::Structured(vec![preparation]),
                finished: Vec::new(),
                float_evaluations: Cell::new(0),
            };
            let mut env =
                ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
            let result = ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, root), &mut env);
            match preparation {
                FakeStructuredPreparation::RuntimeDependent => {
                    assert!(matches!(result, ComptimeOutcome::RuntimeDependent));
                }
                FakeStructuredPreparation::NotReady => {
                    assert!(matches!(result, ComptimeOutcome::NotReady));
                }
                FakeStructuredPreparation::UnsupportedContext => {
                    assert!(matches!(result, ComptimeOutcome::UnsupportedContext));
                }
                FakeStructuredPreparation::Trap => {
                    assert!(matches!(result, ComptimeOutcome::Trap(_)));
                }
                FakeStructuredPreparation::HostFailure => {
                    assert!(matches!(result, ComptimeOutcome::HostFailure(_)));
                }
                FakeStructuredPreparation::Abort => {
                    assert!(matches!(result, ComptimeOutcome::Abort(_)));
                }
                FakeStructuredPreparation::Enter | FakeStructuredPreparation::Memoized => {
                    unreachable!()
                }
            }
            assert_eq!(host.finished, vec![(usize::MAX, None)]);
        }
    }

    #[test]
    fn structured_type_engine_enters_then_memoizes_without_an_extra_frame() {
        let mut editor = rue_rir::RirEditor::new();
        let root = editor.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
            },
            span: Span::new(0, 0),
        });
        let interner = lasso::ThreadedRodeo::new();
        let mut child_editor = rue_rir::RirEditor::new();
        child_editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish(), child_editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Structured(vec![
                FakeStructuredPreparation::Enter,
                FakeStructuredPreparation::Memoized,
            ]),
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Known(FakeValue::Type(FakeType(9)))
        ));
        assert_eq!(host.finished.len(), 1);
    }

    #[test]
    fn structured_type_entries_share_the_48_frame_boundary() {
        for (recursive_enters, succeeds) in [
            (MAX_COMPTIME_CALL_DEPTH - 1, true),
            (MAX_COMPTIME_CALL_DEPTH, false),
        ] {
            let mut parent = rue_rir::RirEditor::new();
            let root = parent.add_inst(rue_rir::Inst {
                data: InstData::TypeConst {
                    type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
                },
                span: Span::new(0, 0),
            });
            let mut child = rue_rir::RirEditor::new();
            let symbol = lasso::ThreadedRodeo::new().get_or_intern("loop");
            let child_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
            let terminal = child.add_inst(rue_rir::Inst {
                data: InstData::IntConst(1),
                span: Span::new(0, 0),
            });
            let mut host = FakeHost {
                programs: vec![parent.finish(), child.finish()],
                type_symbol: SymbolHandle::new(symbol),
                constant: None,
                dependencies: Vec::new(),
                call_plans: AHashMap::new(),
                recursive: Some((recursive_enters, child_call, terminal, None)),
                enter_count: 0,
                finish_outcome: FakeFinishOutcome::Structured(vec![
                    FakeStructuredPreparation::Enter,
                ]),
                finished: Vec::new(),
                float_evaluations: Cell::new(0),
            };
            let mut env =
                ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
            let result = ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, root), &mut env);
            assert_eq!(
                matches!(result, ComptimeOutcome::Known(FakeValue::Type(FakeType(9)))),
                succeeds
            );
        }
    }

    #[test]
    fn non_local_value_domain_runs_the_real_arithmetic_dispatcher() {
        let mut editor = rue_rir::RirEditor::new();
        let lhs = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(40),
            span: Span::new(0, 0),
        });
        let rhs = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 0),
        });
        let add = editor.add_inst(rue_rir::Inst {
            data: InstData::Add { lhs, rhs },
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, add), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap()
            .unwrap();
        assert_eq!(value, FakeValue::Integer(42));
    }

    #[test]
    fn durable_only_instruction_forms_cross_the_semantic_host_boundary() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let intrinsic_name = interner.get_or_intern("import");
        let type_name = interner.get_or_intern("Color");
        let variant_name = interner.get_or_intern("Red");
        let string_name = interner.get_or_intern("dep");
        let string = editor.add_inst(rue_rir::Inst {
            data: InstData::StringConst {
                content: string_name,
                anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
            },
            span: Span::new(0, 5),
        });
        let integer = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(9),
            span: Span::new(6, 7),
        });
        let intrinsic = editor
            .add_intrinsic(intrinsic_name, &[string, integer], Span::new(0, 7))
            .unwrap();
        let checked = editor.add_inst(rue_rir::Inst {
            data: InstData::Checked { expr: integer },
            span: Span::new(8, 18),
        });
        let enum_variant = editor.add_inst(rue_rir::Inst {
            data: InstData::EnumVariant {
                module: None,
                type_name,
                variant: variant_name,
            },
            span: Span::new(19, 29),
        });
        let repeat = editor.add_inst(rue_rir::Inst {
            data: InstData::ArrayRepeat {
                value: integer,
                count: rue_rir::RepeatCount::Literal(2),
            },
            span: Span::new(30, 35),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut engine = ComptimeEngine::new(&mut host);
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();

        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, string), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(17))
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, intrinsic), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(9))
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, checked), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(9))
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, enum_variant), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(23))
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, repeat), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));

        assert!(
            host.dependencies
                .iter()
                .any(|(file, _)| file.index == u32::MAX)
        );
        assert!(
            host.dependencies
                .iter()
                .any(|(file, _)| file.index == 0xFFFF_FFFE)
        );
        assert!(
            host.dependencies
                .iter()
                .any(|(file, _)| file.index == 0xFFFF_FFFB)
        );
        assert!(
            host.dependencies
                .iter()
                .any(|(file, _)| file.index == 0xFFFF_FFFD)
        );
        assert!(
            host.dependencies
                .iter()
                .any(|(file, _)| file.index == 0xFFFF_FFFA)
        );
    }

    #[test]
    fn semantic_sites_use_import_order_and_owning_program_identity() {
        let interner = lasso::ThreadedRodeo::new();
        let import_name = interner.get_or_intern("import");
        let other_name = interner.get_or_intern("other");
        let string_name = interner.get_or_intern("dep");
        let make_program = || {
            let mut editor = rue_rir::RirEditor::new();
            let other_string = editor.add_inst(rue_rir::Inst {
                data: InstData::StringConst {
                    content: string_name,
                    anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
                },
                span: Span::new(0, 3),
            });
            let _other = editor
                .add_intrinsic(other_name, &[other_string], Span::new(0, 3))
                .unwrap();
            let first_string = editor.add_inst(rue_rir::Inst {
                data: InstData::StringConst {
                    content: string_name,
                    anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
                },
                span: Span::new(10, 13),
            });
            let first = editor
                .add_intrinsic(import_name, &[first_string], Span::new(10, 13))
                .unwrap();
            let second_string = editor.add_inst(rue_rir::Inst {
                data: InstData::StringConst {
                    content: string_name,
                    anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
                },
                span: Span::new(10, 13),
            });
            let second = editor
                .add_intrinsic(import_name, &[second_string], Span::new(10, 13))
                .unwrap();
            (editor.finish(), first, second)
        };
        let (program0, first0, second0) = make_program();
        let (program1, first1, _) = make_program();
        let mut host = FakeHost {
            programs: vec![program0, program1],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let mut engine = ComptimeEngine::new(&mut host);
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, first0), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(0))
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, second0), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(0))
        ));
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(1, first1), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(0))
        ));
        let import_observations = host
            .dependencies
            .iter()
            .filter(|(file, _)| file.index == 0xFFFF_FFFE || file.index == 0xFFFF_FFFD)
            .map(|(file, name)| (file.index, name.ordinal))
            .collect::<Vec<_>>();
        assert_eq!(
            import_observations,
            vec![(0xFFFF_FFFE, 1), (0xFFFF_FFFE, 2), (0xFFFF_FFFD, 1001)]
        );
    }

    #[test]
    fn default_admission_does_not_evaluate_intrinsic_or_enum_children() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let bad = editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("1.0"),
            },
            span: Span::new(0, 3),
        });
        let valid_string = editor.add_inst(rue_rir::Inst {
            data: InstData::StringConst {
                content: interner.get_or_intern("dep"),
                anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
            },
            span: Span::new(0, 3),
        });
        let valid_import = editor
            .add_intrinsic(
                interner.get_or_intern("import"),
                &[valid_string],
                Span::new(0, 3),
            )
            .unwrap();
        let intrinsic = editor
            .add_intrinsic(interner.get_or_intern("import"), &[bad], Span::new(0, 3))
            .unwrap();
        let enum_variant = editor.add_inst(rue_rir::Inst {
            data: InstData::EnumVariant {
                module: Some(bad),
                type_name: interner.get_or_intern("Color"),
                variant: interner.get_or_intern("Red"),
            },
            span: Span::new(0, 3),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Structured(Vec::new()),
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let mut engine = ComptimeEngine::new(&mut host);
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, valid_import), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, intrinsic), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, enum_variant), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        assert_eq!(host.float_evaluations.get(), 0);
    }

    #[test]
    fn checked_propagates_a_non_known_child_terminal() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let child = editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("1.0"),
            },
            span: Span::new(0, 3),
        });
        let checked = editor.add_inst(rue_rir::Inst {
            data: InstData::Checked { expr: child },
            span: Span::new(0, 3),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, checked), &mut env);
        assert!(matches!(result, ComptimeOutcome::HostFailure(FAKE_FAILURE)));
        assert_eq!(host.float_evaluations.get(), 1);
    }

    #[test]
    fn member_fallback_receives_a_qualified_base_value() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let base = editor.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: interner.get_or_intern("module"),
                anchor: None,
            },
            span: Span::new(0, 6),
        });
        let field = editor.add_inst(rue_rir::Inst {
            data: InstData::FieldGet {
                base,
                field: interner.get_or_intern("VALUE"),
            },
            span: Span::new(0, 12),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, field), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap();
        assert_eq!(result, Some(FakeValue::Integer(31)));
    }

    #[test]
    fn typed_integer_metadata_survives_bitwise_and_non_scalar_comparison() {
        let mut editor = rue_rir::RirEditor::new();
        editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });
        editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(1, 2),
        });
        let lhs = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(7),
            span: Span::new(2, 3),
        });
        let rhs = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(3),
            span: Span::new(3, 4),
        });
        let bitand = editor.add_inst(rue_rir::Inst {
            data: InstData::BitAnd { lhs, rhs },
            span: Span::new(2, 4),
        });
        let left_type = editor.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
            },
            span: Span::new(5, 8),
        });
        let right_type = editor.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
            },
            span: Span::new(9, 12),
        });
        let equality = editor.add_inst(rue_rir::Inst {
            data: InstData::Eq {
                lhs: left_type,
                rhs: right_type,
            },
            span: Span::new(5, 12),
        });
        let bitnot = editor.add_inst(rue_rir::Inst {
            data: InstData::BitNot { operand: lhs },
            span: Span::new(2, 3),
        });
        let interner = lasso::ThreadedRodeo::new();
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut engine = ComptimeEngine::new(&mut host);
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let bitand_result = engine.evaluate(ComptimeFrame::expression(0, bitand), &mut env);
        assert!(
            matches!(
                bitand_result,
                ComptimeOutcome::Known(FakeValue::TypedInteger(3, FakeType(8)))
            ),
            "bitand result: {bitand_result:?}"
        );
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, equality), &mut env),
            ComptimeOutcome::Known(FakeValue::Boolean(true))
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, bitnot), &mut env),
            ComptimeOutcome::Known(FakeValue::TypedInteger(-8, FakeType(8)))
        ));
    }

    #[test]
    fn integer_type_mismatch_is_a_host_failure_for_binary_comparisons() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let lhs_symbol = interner.get_or_intern("lhs");
        let rhs_symbol = interner.get_or_intern("rhs");
        let lhs = editor.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: lhs_symbol,
                anchor: None,
            },
            span: Span::new(0, 1),
        });
        let rhs = editor.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: rhs_symbol,
                anchor: None,
            },
            span: Span::new(2, 3),
        });
        let equality = editor.add_inst(rue_rir::Inst {
            data: InstData::Eq { lhs, rhs },
            span: Span::new(0, 3),
        });
        let lhs_name = FakeName {
            ordinal: SymbolHandle::new(lhs_symbol).issuing_interner_ordinal() as u32,
        };
        let rhs_name = FakeName {
            ordinal: SymbolHandle::new(rhs_symbol).issuing_interner_ordinal() as u32,
        };
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        env.value_subst
            .insert(lhs_name, FakeValue::TypedInteger(1, FakeType(8)));
        env.value_subst
            .insert(rhs_name, FakeValue::TypedInteger(1, FakeType(16)));
        assert!(matches!(
            ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, equality), &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));
    }

    #[test]
    fn non_local_failure_domain_receives_engine_float_failure() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let float = editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("1.0"),
            },
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let failure =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, float), &mut env);
        assert!(matches!(failure, ComptimeOutcome::HostFailure(_)));
    }

    #[test]
    fn typed_division_by_zero_is_a_structured_trap() {
        let mut editor = rue_rir::RirEditor::new();
        let lhs = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let rhs = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 0),
        });
        let div = editor.add_inst(rue_rir::Inst {
            data: InstData::Div { lhs, rhs },
            span: Span::new(4, 5),
        });
        let interner = lasso::ThreadedRodeo::new();
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, div), &mut env),
            ComptimeOutcome::Trap(ComptimeTrap {
                operation: "division by zero",
                ..
            })
        ));
    }

    #[test]
    fn equality_evaluates_rhs_only_after_nonterminal_lhs_outcomes() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("runtime");
        let mut editor = rue_rir::RirEditor::new();
        let lhs = editor.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: symbol,
                anchor: None,
            },
            span: Span::new(0, 1),
        });
        let rhs = editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("1.0"),
            },
            span: Span::new(2, 3),
        });
        let eq = editor.add_inst(rue_rir::Inst {
            data: InstData::Eq { lhs, rhs },
            span: Span::new(0, 3),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, eq), &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));
        assert_eq!(host.float_evaluations.get(), 1);

        let mut editor = rue_rir::RirEditor::new();
        let one = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let zero = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(2, 3),
        });
        let trap = editor.add_inst(rue_rir::Inst {
            data: InstData::Div {
                lhs: one,
                rhs: zero,
            },
            span: Span::new(0, 3),
        });
        let rhs = editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("2.0"),
            },
            span: Span::new(4, 5),
        });
        let eq = editor.add_inst(rue_rir::Inst {
            data: InstData::Eq { lhs: trap, rhs },
            span: Span::new(0, 5),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, eq), &mut env),
            ComptimeOutcome::Trap(ComptimeTrap {
                operation: "division by zero",
                ..
            })
        ));
        assert_eq!(host.float_evaluations.get(), 0);
    }

    #[test]
    fn non_local_value_domain_runs_the_real_branch_dispatcher() {
        let mut editor = rue_rir::RirEditor::new();
        let condition = editor.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 0),
        });
        let then_value = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(7),
            span: Span::new(0, 0),
        });
        let else_value = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(9),
            span: Span::new(0, 0),
        });
        let branch = editor.add_inst(rue_rir::Inst {
            data: InstData::Branch {
                cond: condition,
                then_block: then_value,
                else_block: Some(else_value),
            },
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, branch), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap()
            .unwrap();
        assert_eq!(value, FakeValue::Integer(7));
    }

    #[test]
    fn non_local_type_domain_runs_the_real_type_dispatcher() {
        let mut editor = rue_rir::RirEditor::new();
        let type_const = editor.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
            },
            span: Span::new(0, 0),
        });
        let interner = lasso::ThreadedRodeo::new();
        let type_symbol = SymbolHandle::new(interner.get_or_intern("T"));
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol,
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, type_const), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap()
            .unwrap();
        assert_eq!(value, FakeValue::Type(FakeType(7)));
    }

    #[test]
    fn runtime_binding_name_blocks_global_constant_fallback() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let name_symbol = SymbolHandle::new(interner.get_or_intern("n"));
        let name = FakeName {
            ordinal: name_symbol.issuing_interner_ordinal() as u32,
        };
        let reference = editor.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: name_symbol.spur(),
                anchor: None,
            },
            span: Span::new(0, 1),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: Some((
                FakeFile { index: 0 },
                name.clone(),
                ComptimeConstInfo {
                    is_pub: true,
                    span: Span::new(10, 11),
                    value: Some(FakeValue::Integer(99)),
                },
            )),
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        env.runtime_binding_names.insert(name);
        let value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, reference), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn constant_dependency_uses_declaration_file_not_reference_file() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let name_symbol = SymbolHandle::new(interner.get_or_intern("answer"));
        let name = FakeName {
            ordinal: name_symbol.issuing_interner_ordinal() as u32,
        };
        let reference = editor.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: name_symbol.spur(),
                anchor: None,
            },
            span: Span::with_file(rue_span::FileId::new(3), 0, 1),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: Some((
                FakeFile { index: 3 },
                name,
                ComptimeConstInfo {
                    is_pub: true,
                    span: Span::with_file(rue_span::FileId::new(9), 10, 11),
                    value: Some(FakeValue::Integer(42)),
                },
            )),
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, reference), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap();
        assert_eq!(value, Some(FakeValue::Integer(42)));
        assert_eq!(
            host.dependencies,
            vec![(FakeFile { index: 9 }, FakeName { ordinal: 0 })]
        );
    }

    #[test]
    fn runtime_local_name_precedes_same_named_comptime_substitutions() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let name_symbol = SymbolHandle::new(interner.get_or_intern("n"));
        let name = FakeName {
            ordinal: name_symbol.issuing_interner_ordinal() as u32,
        };
        let reference = editor.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: name_symbol.spur(),
                anchor: None,
            },
            span: Span::new(0, 1),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        env.type_subst.insert(name.clone(), FakeType(1));
        env.value_subst.insert(name.clone(), FakeValue::Integer(2));
        env.runtime_local_names.insert(name);
        let value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, reference), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap();
        assert_eq!(value, None);
    }

    fn call_fixture() -> (FakeHost, InstRef, InstRef, u32) {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("f");
        let symbol_handle = SymbolHandle::new(symbol);
        let base = symbol_handle.issuing_interner_ordinal() as u32;

        let mut first = rue_rir::RirEditor::new();
        let first_call = first.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let first_rhs = first.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 0),
        });
        let first_root = first.add_inst(rue_rir::Inst {
            data: InstData::Add {
                lhs: first_call,
                rhs: first_rhs,
            },
            span: Span::new(0, 0),
        });

        let mut second = rue_rir::RirEditor::new();
        let second_call = second.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        second.add_inst(rue_rir::Inst {
            data: InstData::IntConst(20),
            span: Span::new(0, 0),
        });

        let mut third = rue_rir::RirEditor::new();
        let third_terminal = third.add_inst(rue_rir::Inst {
            data: InstData::IntConst(20),
            span: Span::new(0, 0),
        });
        let second_name = FakeName {
            ordinal: base + 1000,
        };
        let third_name = FakeName {
            ordinal: base + 2000,
        };
        let mut name_bindings = AHashMap::new();
        name_bindings.insert(second_name, third_name.clone());
        let mut call_plans = AHashMap::new();
        call_plans.insert(
            base,
            FakePreparedCall::Enter {
                program: 1,
                body: second_call,
                expected: Some(FakeType(7)),
                name_bindings,
            },
        );
        call_plans.insert(
            third_name.ordinal,
            FakePreparedCall::Enter {
                program: 2,
                body: third_terminal,
                expected: Some(FakeType(7)),
                name_bindings: AHashMap::new(),
            },
        );

        let host = FakeHost {
            programs: vec![first.finish(), second.finish(), third.finish()],
            type_symbol: symbol_handle,
            constant: None,
            dependencies: Vec::new(),
            call_plans,
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        (host, first_root, first_rhs, base)
    }

    #[test]
    fn entered_programs_switch_on_colliding_refs_and_resume_the_parent() {
        let (mut host, root, rhs, _base) = call_fixture();
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let (value, resumed) = {
            let mut engine = ComptimeEngine::new(&mut host);
            let value = engine
                .evaluate(ComptimeFrame::expression(0, root), &mut env)
                .into_result(|_| FAKE_FAILURE)
                .unwrap();
            // A second root evaluation proves the parent frame was popped
            // after the child program returned; no ambient program or stack
            // state leaks.
            let resumed = engine
                .evaluate(ComptimeFrame::expression(0, rhs), &mut env)
                .into_result(|_| FAKE_FAILURE)
                .unwrap();
            (value, resumed)
        };
        assert_eq!(value, Some(FakeValue::Integer(22)));
        assert_eq!(
            host.finished,
            vec![(2, Some(FakeType(7))), (1, Some(FakeType(7)))]
        );
        assert_eq!(resumed, Some(FakeValue::Integer(2)));
    }

    #[test]
    fn ordered_same_program_calls_keep_distinct_tickets_in_lifo_order() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("nested");
        let symbol_handle = SymbolHandle::new(symbol);
        let mut root = rue_rir::RirEditor::new();
        let root_call = root.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let mut child = rue_rir::RirEditor::new();
        let nested_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let outer_rhs = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 0),
        });
        let _type_hint_probe = child.add_inst(rue_rir::Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 0),
        });
        let outer_add = child.add_inst(rue_rir::Inst {
            data: InstData::Add {
                lhs: nested_call,
                rhs: outer_rhs,
            },
            span: Span::new(0, 0),
        });
        let inner_lhs = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 0),
        });
        let inner_rhs = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(4),
            span: Span::new(0, 0),
        });
        let inner_add = child.add_inst(rue_rir::Inst {
            data: InstData::Add {
                lhs: inner_lhs,
                rhs: inner_rhs,
            },
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![root.finish(), child.finish()],
            type_symbol: symbol_handle,
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: Some((2, outer_add, inner_add, None)),
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        TICKET_EVENTS.with(|events| events.borrow_mut().clear());
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, root_call), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Known(FakeValue::Integer(9))
        ));
        TICKET_EVENTS.with(|events| {
            assert_eq!(
                *events.borrow(),
                vec![(1, true), (2, true), (2, false), (1, false)]
            );
        });
    }

    #[test]
    fn nested_expected_integer_contexts_restore_for_the_parent_and_root() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("typed_nested");
        let symbol_handle = SymbolHandle::new(symbol);
        let mut root = rue_rir::RirEditor::new();
        let root_call = root.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let root_value = root.add_inst(rue_rir::Inst {
            data: InstData::IntConst(9),
            span: Span::new(0, 0),
        });
        let mut child = rue_rir::RirEditor::new();
        let nested_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let outer_rhs = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 0),
        });
        let _type_hint_probe = child.add_inst(rue_rir::Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 0),
        });
        let outer_add = child.add_inst(rue_rir::Inst {
            data: InstData::Add {
                lhs: nested_call,
                rhs: outer_rhs,
            },
            span: Span::new(0, 0),
        });
        let inner_lhs = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 0),
        });
        let inner_rhs = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(4),
            span: Span::new(0, 0),
        });
        let inner_add = child.add_inst(rue_rir::Inst {
            data: InstData::Add {
                lhs: inner_lhs,
                rhs: inner_rhs,
            },
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![root.finish(), child.finish()],
            type_symbol: symbol_handle,
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: Some((2, outer_add, inner_add, None)),
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        INTEGER_HINTS.with(|hints| hints.borrow_mut().clear());
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, root_call), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Known(FakeValue::Integer(9))
        ));
        INTEGER_HINTS.with(|hints| {
            assert_eq!(*hints.borrow(), vec![Some(FakeType(8)), Some(FakeType(7))]);
        });
        assert!(matches!(
            ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, root_value), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(9))
        ));
        INTEGER_HINTS
            .with(|hints| assert_eq!(*hints.borrow(), vec![Some(FakeType(8)), Some(FakeType(7))]));
    }

    #[test]
    fn entered_frames_use_the_real_48_frame_budget_and_memoized_bypasses_it() {
        let mut editor = rue_rir::RirEditor::new();
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("loop");
        let root_call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let mut child = rue_rir::RirEditor::new();
        let child_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let terminal = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let symbol_handle = SymbolHandle::new(symbol);
        let host_base = FakeHost {
            programs: vec![editor.finish(), child.finish()],
            type_symbol: symbol_handle,
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: Some((MAX_COMPTIME_CALL_DEPTH, child_call, terminal, None)),
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut host = host_base;
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let mut engine = ComptimeEngine::new(&mut host);
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, root_call), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(1))
        ));
        assert_eq!(host.enter_count, MAX_COMPTIME_CALL_DEPTH);

        let mut editor = rue_rir::RirEditor::new();
        let root_call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let mut child = rue_rir::RirEditor::new();
        let child_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let mut host = FakeHost {
            programs: vec![editor.finish(), child.finish()],
            type_symbol: SymbolHandle::new(symbol),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: Some((MAX_COMPTIME_CALL_DEPTH, child_call, child_call, None)),
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut engine = ComptimeEngine::new(&mut host);
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        TICKET_EVENTS.with(|events| events.borrow_mut().clear());
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, root_call), &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));
        assert_eq!(host.enter_count, MAX_COMPTIME_CALL_DEPTH + 1);
        TICKET_EVENTS.with(|events| {
            assert_eq!(events.borrow().len(), MAX_COMPTIME_CALL_DEPTH * 2);
        });

        let mut editor = rue_rir::RirEditor::new();
        let root_call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let mut child = rue_rir::RirEditor::new();
        let child_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let mut host = FakeHost {
            programs: vec![editor.finish(), child.finish()],
            type_symbol: SymbolHandle::new(symbol),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: Some((
                MAX_COMPTIME_CALL_DEPTH,
                child_call,
                child_call,
                Some(MAX_COMPTIME_CALL_DEPTH),
            )),
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut engine = ComptimeEngine::new(&mut host);
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, root_call), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(1))
        ));
        assert_eq!(host.enter_count, MAX_COMPTIME_CALL_DEPTH);
    }

    #[test]
    fn typed_outcomes_survive_enter_finish_and_memoized_calls_cleanup_frames() {
        let run_enter = |finish_outcome| {
            let interner = lasso::ThreadedRodeo::new();
            let symbol = interner.get_or_intern("f");
            let symbol_handle = SymbolHandle::new(symbol);
            let base = symbol_handle.issuing_interner_ordinal() as u32;
            let mut editor = rue_rir::RirEditor::new();
            let call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
            let direct = editor.add_inst(rue_rir::Inst {
                data: InstData::IntConst(3),
                span: Span::new(0, 0),
            });
            let mut child = rue_rir::RirEditor::new();
            let child_body = child.add_inst(rue_rir::Inst {
                data: InstData::IntConst(4),
                span: Span::new(0, 0),
            });
            let mut host = FakeHost {
                programs: vec![editor.finish(), child.finish()],
                type_symbol: symbol_handle,
                constant: None,
                dependencies: Vec::new(),
                call_plans: AHashMap::from([(
                    base,
                    FakePreparedCall::Enter {
                        program: 1,
                        body: child_body,
                        expected: Some(FakeType(7)),
                        name_bindings: AHashMap::new(),
                    },
                )]),
                recursive: None,
                enter_count: 0,
                finish_outcome,
                finished: Vec::new(),
                float_evaluations: Cell::new(0),
            };
            let mut env =
                ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
            let (result, resumed) = {
                let mut engine = ComptimeEngine::new(&mut host);
                let result = engine.evaluate(ComptimeFrame::expression(0, call), &mut env);
                let resumed = engine.evaluate(ComptimeFrame::expression(0, direct), &mut env);
                (result, resumed)
            };
            (result, resumed, host.finished.len())
        };

        assert!(matches!(
            run_enter(FakeFinishOutcome::RuntimeDependent).0,
            ComptimeOutcome::RuntimeDependent
        ));
        assert!(matches!(
            run_enter(FakeFinishOutcome::NotReady).0,
            ComptimeOutcome::NotReady
        ));
        assert!(matches!(
            run_enter(FakeFinishOutcome::UnsupportedContext).0,
            ComptimeOutcome::UnsupportedContext
        ));
        assert!(matches!(
            run_enter(FakeFinishOutcome::Trap).0,
            ComptimeOutcome::Trap(_)
        ));
        assert!(matches!(
            run_enter(FakeFinishOutcome::HostFailure).0,
            ComptimeOutcome::HostFailure(_)
        ));
        let (abort, resumed, finished) = run_enter(FakeFinishOutcome::Abort);
        assert!(matches!(abort, ComptimeOutcome::Abort(_)));
        assert!(matches!(
            resumed,
            ComptimeOutcome::Known(FakeValue::Integer(3))
        ));
        assert_eq!(finished, 1);

        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("memoized");
        let symbol_handle = SymbolHandle::new(symbol);
        let base = symbol_handle.issuing_interner_ordinal() as u32;
        let mut editor = rue_rir::RirEditor::new();
        let call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let direct = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(5),
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: symbol_handle,
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::from([(
                base,
                FakePreparedCall::Memoized(ComptimeOutcome::NotReady),
            )]),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let (memoized, resumed) = {
            let mut engine = ComptimeEngine::new(&mut host);
            let memoized = engine.evaluate(ComptimeFrame::expression(0, call), &mut env);
            let resumed = engine.evaluate(ComptimeFrame::expression(0, direct), &mut env);
            (memoized, resumed)
        };
        assert!(matches!(memoized, ComptimeOutcome::NotReady));
        assert!(matches!(
            resumed,
            ComptimeOutcome::Known(FakeValue::Integer(5))
        ));
        assert!(host.finished.is_empty());
    }

    #[test]
    fn rejected_calls_never_activate_or_finish_their_ticket() {
        let (mut host, root, _, _) = call_fixture();
        host.finish_outcome = FakeFinishOutcome::CanonicalFailure;
        TICKET_EVENTS.with(|events| events.borrow_mut().clear());
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));
        assert!(host.finished.is_empty());
        TICKET_EVENTS.with(|events| assert!(events.borrow().is_empty()));
    }

    #[test]
    fn unnamed_enter_is_rejected_before_ticket_lifecycle() {
        let (mut host, root, rhs, base) = call_fixture();
        host.call_plans.insert(
            base,
            FakePreparedCall::UnnamedEnter {
                program: 1,
                body: InstRef::from_raw(0),
            },
        );
        TICKET_EVENTS.with(|events| events.borrow_mut().clear());
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
        assert!(matches!(result, ComptimeOutcome::UnsupportedContext));
        assert!(host.finished.is_empty());
        TICKET_EVENTS.with(|events| assert!(events.borrow().is_empty()));

        // The invalid preparation did not leave a frame on the stack.
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, rhs), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(2))
        ));
    }

    #[test]
    fn named_frames_cannot_bypass_the_entered_call_lifecycle() {
        let (mut host, _root, _rhs, _base) = call_fixture();
        TICKET_EVENTS.with(|events| events.borrow_mut().clear());
        let mut frame = ComptimeFrame::expression(0, InstRef::from_raw(0));
        frame.name = Some(FakeName { ordinal: 99 });
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result = ComptimeEngine::new(&mut host).evaluate(frame, &mut env);
        assert!(matches!(result, ComptimeOutcome::UnsupportedContext));
        assert!(host.finished.is_empty());
        TICKET_EVENTS.with(|events| assert!(events.borrow().is_empty()));
    }

    #[test]
    fn anonymous_method_decoder_rejects_non_function_entries_exactly() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("method");
        let mut editor = RirEditor::new();
        let non_function = editor.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(4, 5),
        });
        let root = editor
            .add_anon_struct_type(
                &[],
                &[non_function],
                rue_rir::RirStructuralAnchor::new(Vec::new()),
                Span::new(0, 1),
            )
            .unwrap();
        let rir = editor.finish();
        let methods = match &rir.get(root).data {
            InstData::AnonStructType { methods, .. } => methods.clone(),
            _ => unreachable!(),
        };
        let mut host = FakeHost {
            programs: vec![rir],
            type_symbol: SymbolHandle::new(symbol),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        METHOD_FAILURES.with(|failures| failures.borrow_mut().clear());
        let result = ComptimeEngine::new(&mut host).decode_anon_method_descriptors(
            &0,
            &methods,
            &AHashMap::new(),
            &AHashMap::new(),
        );
        assert!(matches!(
            result,
            ComptimeOutcome::HostFailure(FakeFailure::NonFunctionMethod)
        ));
        METHOD_FAILURES.with(|failures| assert_eq!(*failures.borrow(), vec!["non_function"]));
    }

    #[test]
    fn own_comptime_type_parameter_wins_before_later_type_resolution() {
        let interner = lasso::ThreadedRodeo::new();
        let method_name = interner.get_or_intern("method");
        let type_name = interner.get_or_intern("type");
        let mut editor = RirEditor::new();
        let type_syntax = editor.add_named_type(type_name).unwrap();
        let body = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(10, 11),
        });
        let method = editor
            .add_fn_decl(
                &[],
                false,
                false,
                false,
                false,
                method_name,
                &[rue_rir::RirParam {
                    name: type_name,
                    ty: type_syntax,
                    mode: rue_rir::RirParamMode::Normal,
                    is_comptime: true,
                    span: Span::new(12, 13),
                }],
                type_syntax,
                body,
                false,
                rue_rir::RirParamMode::Normal,
                false,
                false,
                Span::new(8, 9),
            )
            .unwrap();
        let root = editor
            .add_anon_struct_type(
                &[],
                &[method],
                rue_rir::RirStructuralAnchor::new(Vec::new()),
                Span::new(0, 1),
            )
            .unwrap();
        let rir = editor.finish();
        let methods = match &rir.get(root).data {
            InstData::AnonStructType { methods, .. } => methods.clone(),
            _ => unreachable!(),
        };
        let mut host = FakeHost {
            programs: vec![rir],
            type_symbol: SymbolHandle::new(type_name),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        METHOD_FAILURES.with(|failures| failures.borrow_mut().clear());
        TYPE_RESOLUTION_CALLS.with(|calls| calls.set(0));
        let result = ComptimeEngine::new(&mut host).decode_anon_method_descriptors(
            &0,
            &methods,
            &AHashMap::new(),
            &AHashMap::new(),
        );
        assert!(matches!(
            result,
            ComptimeOutcome::HostFailure(FakeFailure::OwnComptimeTypeParameter)
        ));
        METHOD_FAILURES.with(|failures| assert_eq!(*failures.borrow(), vec!["own_type"]));
        TYPE_RESOLUTION_CALLS.with(|calls| assert_eq!(calls.get(), 0));
    }

    #[test]
    fn expected_integer_context_is_frame_local_and_integer_only() {
        let mut editor = RirEditor::new();
        let lhs = editor.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let rhs = editor.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 0),
        });
        let _unused = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 0),
        });
        let add = editor.add_inst(Inst {
            data: InstData::Add { lhs, rhs },
            span: Span::new(0, 0),
        });
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("context");
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(symbol),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        env.expected_result = Some(FakeType(3));
        INTEGER_HINTS.with(|hints| hints.borrow_mut().clear());
        let frame = ComptimeFrame {
            program: 0,
            body: add,
            name: None,
            context: None,
            span: Span::new(0, 0),
            function_span: Span::new(0, 0),
            type_bindings: AHashMap::new(),
            value_bindings: AHashMap::new(),
            name_bindings: AHashMap::new(),
            call_identity: None,
            expected_result: Some(FakeType(16)),
        };
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(frame, &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(3))
        ));
        assert_eq!(env.expected_result, Some(FakeType(3)));
        INTEGER_HINTS.with(|hints| assert_eq!(*hints.borrow(), vec![Some(FakeType(16))]));

        INTEGER_HINTS.with(|hints| hints.borrow_mut().clear());
        let non_integer_frame = ComptimeFrame {
            program: 0,
            body: add,
            name: None,
            context: None,
            span: Span::new(0, 0),
            function_span: Span::new(0, 0),
            type_bindings: AHashMap::new(),
            value_bindings: AHashMap::new(),
            name_bindings: AHashMap::new(),
            call_identity: None,
            expected_result: Some(FakeType(99)),
        };
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(non_integer_frame, &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(3))
        ));
        INTEGER_HINTS.with(|hints| assert_eq!(*hints.borrow(), vec![None]));
    }

    #[test]
    fn host_abort_channel_cleans_entered_frames_and_preserves_labels() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("abort");
        let symbol_handle = SymbolHandle::new(symbol);
        let base = symbol_handle.issuing_interner_ordinal() as u32;

        let mut root_editor = RirEditor::new();
        let call = root_editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let direct = root_editor.add_inst(Inst {
            data: InstData::IntConst(9),
            span: Span::new(0, 0),
        });
        let mut child_editor = RirEditor::new();
        let lhs = child_editor.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 0),
        });
        let rhs = child_editor.add_inst(Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 0),
        });
        let child_body = child_editor.add_inst(Inst {
            data: InstData::Add { lhs, rhs },
            span: Span::new(0, 0),
        });

        let mut host = FakeHost {
            programs: vec![root_editor.finish(), child_editor.finish()],
            type_symbol: symbol_handle,
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::from([(
                base,
                FakePreparedCall::Enter {
                    program: 1,
                    body: child_body,
                    expected: Some(FakeType(7)),
                    name_bindings: AHashMap::new(),
                },
            )]),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::AbortFromArithmetic,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        LABEL_CALLS.with(|calls| calls.set(0));
        TICKET_EVENTS.with(|events| events.borrow_mut().clear());
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let mut engine = ComptimeEngine::new(&mut host);
        let aborted = engine.evaluate(ComptimeFrame::expression(0, call), &mut env);
        assert!(matches!(aborted, ComptimeOutcome::Abort(FAKE_FAILURE)));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, direct), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(9))
        ));
        drop(engine);
        assert_eq!(host.finished, vec![(1, Some(FakeType(7)))]);
        LABEL_CALLS.with(|calls| assert_eq!(calls.get(), 0));
        TICKET_EVENTS.with(|events| assert_eq!(*events.borrow(), vec![(1, true), (1, false)]));

        let mut root_editor = RirEditor::new();
        let call = root_editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let direct = root_editor.add_inst(Inst {
            data: InstData::IntConst(11),
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![root_editor.finish()],
            type_symbol: SymbolHandle::new(symbol),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::AbortFromPrepare,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        TICKET_EVENTS.with(|events| events.borrow_mut().clear());
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let mut engine = ComptimeEngine::new(&mut host);
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, call), &mut env),
            ComptimeOutcome::Abort(FAKE_FAILURE)
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, direct), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(11))
        ));
        drop(engine);
        assert!(host.finished.is_empty());
        TICKET_EVENTS.with(|events| assert!(events.borrow().is_empty()));
    }

    fn registry_program(value: u64) -> (Arc<ValidatedRir>, InstRef) {
        let mut editor = RirEditor::new();
        let root = editor.add_inst(Inst {
            data: InstData::IntConst(value),
            span: Span::new(0, 0),
        });
        let context = RirValidationContext {
            symbol_count: 0,
            source_lengths: &[(rue_span::FileId::DEFAULT, 1)],
        };
        (
            Arc::new(ValidatedRir::finish(editor, &context).expect("valid test RIR")),
            root,
        )
    }

    #[test]
    fn registry_keeps_colliding_instruction_refs_program_local() {
        let (first_rir, first_ref) = registry_program(11);
        let (second_rir, second_ref) = registry_program(22);
        assert_eq!(first_ref, second_ref);
        let mut registry = ComptimeProgramRegistry::<u8, u8, u8, u8>::new();
        registry
            .register(
                ComptimeProgramKey {
                    declaration: 1,
                    configuration: 10,
                },
                ComptimeProgram {
                    rir: first_rir,
                    symbols: Arc::from([]),
                    imports: 0,
                },
            )
            .unwrap();
        registry
            .register(
                ComptimeProgramKey {
                    declaration: 2,
                    configuration: 20,
                },
                ComptimeProgram {
                    rir: second_rir,
                    symbols: Arc::from([2]),
                    imports: 22,
                },
            )
            .unwrap();
        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());
        assert_eq!(
            registry
                .get(&ComptimeProgramKey {
                    declaration: 1,
                    configuration: 10,
                })
                .unwrap()
                .rir
                .get(first_ref)
                .data,
            InstData::IntConst(11)
        );
        assert_eq!(
            registry
                .get(&ComptimeProgramKey {
                    declaration: 2,
                    configuration: 20,
                })
                .unwrap()
                .rir
                .get(second_ref)
                .data,
            InstData::IntConst(22)
        );
        let second = registry
            .get(&ComptimeProgramKey {
                declaration: 2,
                configuration: 20,
            })
            .unwrap();
        assert_eq!(&*second.symbols, &[2]);
        assert_eq!(second.imports, 22);
        assert_eq!(
            registry.register(
                ComptimeProgramKey {
                    declaration: 1,
                    configuration: 10,
                },
                ComptimeProgram {
                    rir: registry_program(99).0,
                    symbols: Arc::from([]),
                    imports: 0,
                },
            ),
            Err(ComptimeProgramRegistrationError::AlreadyRegistered)
        );
    }

    #[test]
    fn registry_admits_exact_structured_arena_and_matching_symbol_authority() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("T");
        let mut editor = RirEditor::new();
        let root = editor.add_named_type(symbol).expect("named type syntax");
        let symbol_index = symbol.into_usize();
        let symbol_count = symbol_index + 1;
        let context = RirValidationContext {
            symbol_count,
            source_lengths: &[(rue_span::FileId::DEFAULT, 1)],
        };
        let rir = Arc::new(ValidatedRir::finish(editor, &context).expect("valid structured RIR"));
        let mut symbols = vec![Arc::<str>::from(""); symbol_count];
        symbols[symbol_index] = Arc::from("T");
        let key = ComptimeProgramKey {
            declaration: 7_u8,
            configuration: 9_u8,
        };
        let mut registry = ComptimeProgramRegistry::<u8, u8, Arc<str>, ()>::new();
        registry
            .register(
                key.clone(),
                ComptimeProgram {
                    rir: Arc::clone(&rir),
                    symbols: Arc::from(symbols),
                    imports: (),
                },
            )
            .unwrap();
        assert!(
            registry
                .structured_type_authority(&key, "scope", root)
                .is_some()
        );
        let bad_key = ComptimeProgramKey {
            declaration: 8_u8,
            configuration: 9_u8,
        };
        registry
            .register(
                bad_key.clone(),
                ComptimeProgram {
                    rir,
                    symbols: Arc::from([]),
                    imports: (),
                },
            )
            .unwrap();
        assert!(
            registry
                .structured_type_authority(&bad_key, "scope", root)
                .is_none()
        );
        assert!(
            registry
                .structured_type_authority(&key, "scope", rue_rir::RirTypeSyntaxRef::from_u32(99))
                .is_none()
        );
    }

    #[test]
    fn completed_memo_distinguishes_ordered_args_and_miss() {
        type Memo = ComptimeCompletedCallMemo<u8, u8, u8, u8, u8>;
        let key = |declaration, configuration, types: &[u8], values: &[u8]| ComptimeCallKey {
            declaration,
            configuration,
            type_arguments: Arc::from(types),
            value_arguments: Arc::from(values),
        };
        let mut memo = Memo::new();
        let base = key(7, 3, &[1, 2], &[3, 4]);
        assert!(matches!(memo.lookup(&base), ComptimeCallMemoLookup::Miss));
        memo.insert(base.clone(), ComptimeMemoizedOutcome::NotReady)
            .unwrap();
        assert!(matches!(
            memo.lookup(&base),
            ComptimeCallMemoLookup::Memoized(ComptimeMemoizedOutcome::NotReady)
        ));
        assert!(matches!(
            memo.lookup(&key(8, 3, &[1, 2], &[3, 4])),
            ComptimeCallMemoLookup::Miss
        ));
        assert!(matches!(
            memo.lookup(&key(7, 4, &[1, 2], &[3, 4])),
            ComptimeCallMemoLookup::Miss
        ));
        assert!(matches!(
            memo.lookup(&key(7, 3, &[2, 1], &[3, 4])),
            ComptimeCallMemoLookup::Miss
        ));
        assert!(matches!(
            memo.lookup(&key(7, 3, &[1, 2], &[4, 3])),
            ComptimeCallMemoLookup::Miss
        ));
        assert_eq!(memo.len(), 1);
        assert!(!memo.is_empty());
        assert_eq!(
            memo.insert(base, ComptimeMemoizedOutcome::Known(9)),
            Err(ComptimeMemoInsertError::AlreadyMemoized)
        );
        let trap_key = key(7, 3, &[1, 2], &[5]);
        let trap = ComptimeTrap {
            operation: "division by zero",
            span: Span::new(0, 0),
        };
        memo.insert(trap_key.clone(), ComptimeMemoizedOutcome::Trap(trap))
            .unwrap();
        assert!(matches!(
            memo.lookup(&trap_key),
            ComptimeCallMemoLookup::Memoized(ComptimeMemoizedOutcome::Trap(value))
                if *value == trap
        ));
    }
}

#[derive(Debug)]
pub struct ComptimeFrame<V, T, N, F, P, I> {
    pub program: P,
    pub body: InstRef,
    pub name: Option<N>,
    pub context: Option<F>,
    pub span: Span,
    pub function_span: Span,
    pub type_bindings: AHashMap<N, T>,
    pub value_bindings: AHashMap<N, V>,
    pub name_bindings: AHashMap<N, N>,
    pub call_identity: Option<I>,
    pub expected_result: Option<T>,
}

impl<V, T, N, F, P, I> ComptimeFrame<V, T, N, F, P, I> {
    pub fn expression(program: P, body: InstRef) -> Self {
        Self {
            program,
            body,
            name: None,
            context: None,
            span: Span::new(0, 0),
            function_span: Span::new(0, 0),
            type_bindings: AHashMap::new(),
            value_bindings: AHashMap::new(),
            name_bindings: AHashMap::new(),
            call_identity: None,
            expected_result: None,
        }
    }
}

#[derive(Debug)]
pub enum ComptimeCallPreparation<V, T, N, File, P, I, Failure, K> {
    /// A completed fact from the evaluation-local memo. This includes
    /// not-ready/runtime-dependent facts and therefore must not be confused
    /// with a cache miss.
    Memoized(ComptimeOutcome<V, Failure>),
    /// A cache miss represented by an owned foreign frame. The engine enters
    /// it and evaluates it; hosts never recursively dispatch its RIR.
    Enter {
        frame: ComptimeFrame<V, T, N, File, P, I>,
        ticket: K,
    },
}

#[derive(Debug)]
pub enum ComptimeOutcome<V, F> {
    Known(V),
    RuntimeDependent,
    NotReady,
    UnsupportedContext,
    Trap(ComptimeTrap),
    HostFailure(F),
    Abort(F),
}

impl<V, F> ComptimeOutcome<V, F> {
    pub(crate) fn into_result(self, trap: impl FnOnce(ComptimeTrap) -> F) -> Result<Option<V>, F> {
        match self {
            Self::Known(value) => Ok(Some(value)),
            Self::RuntimeDependent | Self::NotReady | Self::UnsupportedContext => Ok(None),
            Self::Trap(value) => Err(trap(value)),
            Self::HostFailure(error) | Self::Abort(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComptimeTrap {
    pub operation: &'static str,
    pub span: Span,
}

pub type ComptimeArgMode = (rue_rir::RirArgMode, Span);

pub struct ComptimeCallAdmission<A, N> {
    pub name: N,
    pub payload: A,
}

macro_rules! outcome_value {
    ($value:expr) => {
        match $value {
            ComptimeOutcome::Known(value) => value,
            ComptimeOutcome::RuntimeDependent => return ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::NotReady => return ComptimeOutcome::NotReady,
            ComptimeOutcome::UnsupportedContext => return ComptimeOutcome::UnsupportedContext,
            ComptimeOutcome::Trap(trap) => return ComptimeOutcome::Trap(trap),
            ComptimeOutcome::HostFailure(error) => return ComptimeOutcome::HostFailure(error),
            ComptimeOutcome::Abort(error) => return ComptimeOutcome::Abort(error),
        }
    };
}

macro_rules! host_value {
    ($value:expr) => {
        match $value {
            Ok(value) => value,
            Err(ComptimeHostError::HostFailure(error)) => {
                return ComptimeOutcome::HostFailure(error)
            }
            Err(ComptimeHostError::Abort(error)) => return ComptimeOutcome::Abort(error),
        }
    };
}

/// Error classification for fallible semantic host operations. Ordinary host
/// failures are distinct from query cancellation/abort so the engine can
/// preserve aborts through entered frames and keep them out of memoization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComptimeHostError<F> {
    HostFailure(F),
    Abort(F),
}

pub type ComptimeHostResult<T, F> = Result<T, ComptimeHostError<F>>;

impl<F> From<F> for ComptimeHostError<F> {
    fn from(value: F) -> Self {
        Self::HostFailure(value)
    }
}

impl<F> ComptimeHostError<F> {
    pub(crate) fn into_failure(self) -> F {
        match self {
            Self::HostFailure(error) | Self::Abort(error) => error,
        }
    }
}

/// Semantic host boundary for the canonical dispatcher. No method accepts an
/// instruction callback or a child RIR reference for evaluation.
pub trait ComptimeHost {
    type Type: ComptimeType;
    type Value: ComptimeValue<Type = Self::Type>;
    type Name: ComptimeName;
    type File: ComptimeFile;
    type CanonicalIdentity: ComptimeIdentity;
    type AnonymousIdentity: ComptimeIdentity;
    type ProgramKey: Clone;
    type Failure;
    type CallAdmission;
    /// Opaque host-owned completion state issued during ordered preparation.
    type CompletionTicket;
    type AnonymousStructId: Clone;
    /// The sole continuation representation accepted by the engine for a
    /// structured type reduction. This is sealed below to prevent a peer
    /// resolver state machine from being hidden behind the host boundary.
    type StructuredTypeSuspension: ComptimeStructuredTypeSuspension;
    fn program_rir(&self, program: &Self::ProgramKey) -> &Rir;
    fn name_from_symbol(&self, program: &Self::ProgramKey, symbol: SymbolHandle) -> Self::Name;
    fn display_name(&self, name: &Self::Name) -> String;
    fn file_from_span(&self, span: &Span) -> Self::File;
    fn value_const(
        &self,
        key: &(Self::File, Self::Name),
    ) -> ComptimeHostResult<Option<ComptimeConstInfo<Self::Value>>, Self::Failure>;
    fn match_pattern(
        &self,
        program: &Self::ProgramKey,
        pattern: &rue_rir::RirPatternView<'_>,
        value: &Self::Value,
    ) -> Option<bool>;
    fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        span: Span,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn depth_exceeded(&self, name: &Self::Name, depth: usize, span: Span) -> Self::Failure;
    fn literal_out_of_range(&self, value: u64, ty: &Self::Type, span: Span) -> Self::Failure;
    fn float_not_implemented(&self, span: Span) -> Self::Failure;
    fn cannot_negate(&self, ty: &Self::Type, span: Span) -> Self::Failure;
    fn comptime_panic(&self, reason: String, span: Span) -> Self::Failure;
    fn unsupported_anon_method_type_param(
        &self,
        method_span: Span,
        method_name: &str,
    ) -> Self::Failure;
    fn non_function_anon_method(&self, method_span: Span) -> Self::Failure;
    fn record_value_const_dependency(&mut self, file: &Self::File, name: &Self::Name);
    fn resolve_named_array_length(
        &mut self,
        name: &Self::Name,
        span: Span,
        values: Option<&AHashMap<Self::Name, Self::Value>>,
    ) -> ComptimeHostResult<u64, Self::Failure>;
    fn rir_type_named_symbol(
        &self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Option<Self::Name>;
    /// Render an unsupported type syntax using the owning program's arena and
    /// semantic symbol mapping. The engine never derives identity from the
    /// compact syntax reference itself.
    fn render_rir_type(
        &self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> String;
    fn get_or_create_array_type(&mut self, element: Self::Type, length: u64) -> Self::Type;
    fn find_or_create_anon_struct(
        &mut self,
        identity: Self::AnonymousIdentity,
        fields: &[ComptimeField<Self::Name, Self::Type>],
        sigs: &[ComptimeMethodDescriptor<Self::Name, Self::Type>],
        captured: &AHashMap<Self::Name, Self::Value>,
    ) -> ComptimeHostResult<(Self::Type, bool), Self::Failure>;
    fn find_or_create_anon_enum(
        &mut self,
        identity: Self::AnonymousIdentity,
        names: &[String],
        payloads: &[Vec<Self::Type>],
    ) -> ComptimeHostResult<Self::Type, Self::Failure>;
    fn anonymous_struct_id(&self, ty: &Self::Type) -> Option<Self::AnonymousStructId>;
    fn has_method(&self, key: &Self::AnonymousStructId, method: Self::Name) -> bool;
    fn check_unqualified_visibility(
        &self,
        item_kind: &str,
        name: &Self::Name,
        defining_file_id: Self::File,
        is_pub: bool,
        span: Span,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn check_require_droppable(
        &mut self,
        ty: Self::Type,
        span: Span,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn check_trivially_droppable(
        &mut self,
        ty: Self::Type,
        span: Span,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn type_name(&self, ty: &Self::Type) -> String;
    fn type_is_unsigned(&self, ty: &Self::Type) -> bool;
    fn type_integer_semantics(&self, ty: &Self::Type) -> Option<IntegerType>;
    fn record_named_type_dependency(&mut self, ty: &Self::Type);
    fn const_expr_type(
        &self,
        program: &Self::ProgramKey,
        env: &ComptimeEnv<
            '_,
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::CanonicalIdentity,
        >,
        inst_ref: InstRef,
    ) -> Option<Self::Type>;

    /// Select the integer type for a binary operation. The default preserves
    /// the existing resolved-type lookup; durable hosts can fall back to the
    /// typed metadata carried by the reduced operands without inspecting RIR.
    fn integer_operation_type(
        &self,
        resolved_type: Option<&Self::Type>,
        lhs: &Self::Value,
        rhs: &Self::Value,
        _span: Span,
    ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        Ok(resolved_type
            .cloned()
            .or_else(|| lhs.as_integer_type())
            .or_else(|| rhs.as_integer_type()))
    }

    /// Select the integer type for a unary operation. A durable host can
    /// preserve the operand's type metadata after the child has been reduced,
    /// while the default retains the ordinary resolved-type lookup.
    fn unary_integer_type(
        &self,
        resolved_type: Option<&Self::Type>,
        operand: &Self::Value,
        _span: Span,
    ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        Ok(resolved_type.cloned().or_else(|| operand.as_integer_type()))
    }

    /// Compare values that are not represented by the generic integer/bool
    /// algebra (for example target descriptors). The ordinary body domain
    /// keeps those comparisons runtime-dependent.
    fn compare_comptime_values(
        &mut self,
        _lhs: &Self::Value,
        _rhs: &Self::Value,
        _equal: bool,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }
    fn finish_arith(
        &self,
        result: CheckedIntegerResult,
        ty: Option<Self::Type>,
        op: &str,
        span: Span,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure>;
    fn resolve_named_type_value(
        &mut self,
        _name: Self::Name,
        span: Span,
    ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure>;
    fn resolve_comptime_type_path(
        &mut self,
        file: Self::File,
        segments: &[Self::Name],
        span: Span,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure>;
    fn resolve_module_comptime_callable(
        &mut self,
        file_id: Self::File,
        segments: &[Self::Name],
        method: Self::Name,
        span: Span,
    ) -> ComptimeHostResult<Option<Self::Name>, Self::Failure>;
    fn admit_comptime_call(
        &mut self,
        name: Self::Name,
        arg_count: usize,
        arg_modes: &[ComptimeArgMode],
        env: &mut ComptimeEnv<
            '_,
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::CanonicalIdentity,
        >,
        name_is_resolved_key: bool,
    ) -> ComptimeHostResult<
        Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
        Self::Failure,
    >;
    fn bind_comptime_call(
        &self,
        admission: &ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        values: &[Self::Value],
        span: Span,
    ) -> ComptimeHostResult<
        Option<(
            AHashMap<Self::Name, Self::Type>,
            AHashMap<Self::Name, Self::Value>,
        )>,
        Self::Failure,
    >;
    fn prepare_comptime_call(
        &mut self,
        admission: ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        types: AHashMap<Self::Name, Self::Type>,
        values: AHashMap<Self::Name, Self::Value>,
        span: Span,
    ) -> ComptimeHostResult<
        Option<
            ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
                Self::CompletionTicket,
            >,
        >,
        Self::Failure,
    >;
    fn finish_comptime_call(
        &mut self,
        frame: &ComptimeFrame<
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::ProgramKey,
            Self::CanonicalIdentity,
        >,
        ticket: Self::CompletionTicket,
        result: ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure>;
    /// Activate a prepared completion ticket only after the engine has
    /// admitted depth and issued the canonical producer identity.
    fn enter_comptime_call(
        &mut self,
        _frame: &ComptimeFrame<
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::ProgramKey,
            Self::CanonicalIdentity,
        >,
        _ticket: &Self::CompletionTicket,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn label_ctor_instantiation_site(error: Self::Failure, call_span: Span) -> Self::Failure;
    fn canonical_function_producer(
        &self,
        name: Self::Name,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
        span: Span,
    ) -> ComptimeHostResult<Self::CanonicalIdentity, Self::Failure>;
    fn issue_anonymous_identity(
        &self,
        program: &Self::ProgramKey,
        kind: ComptimeAnonymousKind,
        producer: &Self::CanonicalIdentity,
        anchor: &rue_rir::RirStructuralAnchor,
    ) -> Self::AnonymousIdentity;
    fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
        span: Span,
    ) -> Option<Self::Type>;

    /// Begin a structured type reduction. The default is the staged
    /// synchronous adapter; an admitted keyed host may return a canonical
    /// suspension here.
    fn begin_comptime_type_syntax(
        &mut self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
        span: Span,
    ) -> ComptimeOutcome<
        ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
        Self::Failure,
    > {
        self.resolve_rir_type_for_comptime_with_subst_and_values_at_span(
            program, syntax, types, values, span,
        )
        .map_or(ComptimeOutcome::RuntimeDependent, |value| {
            ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(value))
        })
    }

    fn prepare_structured_type_call(
        &mut self,
        suspension: &Self::StructuredTypeSuspension,
    ) -> ComptimeOutcome<
        Option<
            ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
                Self::CompletionTicket,
            >,
        >,
        Self::Failure,
    >;

    fn resume_structured_type_call(
        &mut self,
        suspension: Self::StructuredTypeSuspension,
        result: ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> ComptimeOutcome<
        ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
        Self::Failure,
    >;
    fn set_anon_struct_type_subst(
        &mut self,
        struct_id: &Self::AnonymousStructId,
        subst: AHashMap<Self::Name, Self::Type>,
    );

    /// Resolve a string literal in a semantic context. The ordinary body
    /// value domain has no compile-time string value, so the default keeps
    /// string expressions runtime-dependent. Durable hosts may use this hook
    /// for controls such as `@import` without inspecting the instruction.
    fn resolve_string_const(
        &mut self,
        _content: Self::Name,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }

    fn admit_comptime_intrinsic(
        &mut self,
        _name: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<bool, Self::Failure> {
        Ok(false)
    }

    /// Handle an expression intrinsic after the engine has recursively
    /// evaluated every non-string argument. The host receives semantic names
    /// and values only; it never receives child instruction references.
    fn resolve_comptime_intrinsic(
        &mut self,
        _name: Self::Name,
        _arguments: &[ComptimeIntrinsicArgument<Self::Value, Self::Name>],
        _site: &ComptimeSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }

    /// Resolve a discriminant-only or payload-bearing enum variant after the
    /// optional module expression has been reduced by the engine. The default
    /// preserves ordinary body behavior, where enum values are runtime data.
    fn resolve_comptime_enum_variant(
        &mut self,
        _module: Option<Self::Value>,
        _type_name: Self::Name,
        _variant: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }

    fn admit_comptime_enum_variant(
        &mut self,
        _type_name: Self::Name,
        _variant: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<bool, Self::Failure> {
        Ok(false)
    }

    fn admit_comptime_member(
        &mut self,
        _field: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<bool, Self::Failure> {
        Ok(false)
    }

    fn resolve_comptime_member(
        &mut self,
        _base: Self::Value,
        _field: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }

    /// Preserve checked-block semantics after the child has been evaluated by
    /// the engine. A durable host can attach its own context observation while
    /// the default remains a transparent wrapper.
    fn finish_checked(
        &mut self,
        value: Self::Value,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::Known(value)
    }

    /// Give a durable host a typed rejection point for a non-type array
    /// repeat. The existing engine only folds repeats whose element is a type;
    /// ordinary body evaluation therefore remains runtime-dependent by
    /// default.
    fn reject_non_type_array_repeat(
        &mut self,
        _value: Self::Value,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }

    /// Ordinary body analysis has historically treated `checked { ... }` as
    /// runtime-only during comptime probing. Durable declaration hosts opt in
    /// once they can preserve the checked-context observation.
    fn allow_checked_comptime(&self) -> bool {
        false
    }
}

/// The typed result of one structured-type host continuation.
///
/// The suspension is opaque to the engine. A host may use the keyed
/// structured-type job from `semantic_type_resolution`, but the engine never
/// receives a program, scope, arena, or syntax reference while resuming it.
pub enum ComptimeStructuredTypeResolution<V, S> {
    Ready(V),
    Suspended(S),
}

mod structured_type_seal {
    pub(crate) trait Sealed {}
}

/// Opaque structured continuations are sealed to the canonical AIR resolver
/// job. Test hosts may use a local witness, but production hosts cannot
/// introduce a peer state machine behind this engine boundary.
pub trait ComptimeStructuredTypeSuspension: structured_type_seal::Sealed {}

impl<P, S, C, N, A, T, V, Sym, R> structured_type_seal::Sealed
    for crate::semantic_type_resolution::ComptimeStructuredTypeJob<P, S, C, N, A, T, V, Sym, R>
{
}

impl<P, S, C, N, A, T, V, Sym, R> ComptimeStructuredTypeSuspension
    for crate::semantic_type_resolution::ComptimeStructuredTypeJob<P, S, C, N, A, T, V, Sym, R>
{
}

pub struct ComptimeEngine<'e, H: ComptimeHost> {
    host: &'e mut H,
    frames: Vec<
        ComptimeFrame<H::Value, H::Type, H::Name, H::File, H::ProgramKey, H::CanonicalIdentity>,
    >,
}

impl<'e, H: ComptimeHost> ComptimeEngine<'e, H> {
    pub fn new(host: &'e mut H) -> Self {
        Self {
            host,
            frames: Vec::new(),
        }
    }

    /// Drive one opaque structured-type suspension on this engine's existing
    /// frame stack. Only this method interprets `Memoized` versus `Enter` for
    /// structured-type reductions; hosts merely prepare and resume typed
    /// continuations.
    fn drive_structured_type(
        &mut self,
        suspension: H::StructuredTypeSuspension,
    ) -> ComptimeOutcome<H::Type, H::Failure> {
        let mut suspension = suspension;
        loop {
            let preparation = self.host.prepare_structured_type_call(&suspension);
            let reduced = match preparation {
                ComptimeOutcome::Known(Some(preparation)) => match preparation {
                    ComptimeCallPreparation::Memoized(outcome) => outcome,
                    ComptimeCallPreparation::Enter { frame, ticket } => {
                        let span = frame.span;
                        self.enter_prepared_call(frame, ticket, span)
                    }
                },
                ComptimeOutcome::Known(None) => ComptimeOutcome::RuntimeDependent,
                ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
                ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
                ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
                ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
                ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
                ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
            };
            match self.host.resume_structured_type_call(suspension, reduced) {
                ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(value)) => {
                    return ComptimeOutcome::Known(value);
                }
                ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Suspended(next)) => {
                    suspension = next;
                }
                ComptimeOutcome::RuntimeDependent => return ComptimeOutcome::RuntimeDependent,
                ComptimeOutcome::NotReady => return ComptimeOutcome::NotReady,
                ComptimeOutcome::UnsupportedContext => {
                    return ComptimeOutcome::UnsupportedContext;
                }
                ComptimeOutcome::Trap(trap) => return ComptimeOutcome::Trap(trap),
                ComptimeOutcome::HostFailure(error) => {
                    return ComptimeOutcome::HostFailure(error);
                }
                ComptimeOutcome::Abort(error) => return ComptimeOutcome::Abort(error),
            }
        }
    }

    /// Route a type-bearing instruction through the same engine-owned
    /// structured loop as every other typed reduction. A synchronous host
    /// returns `Ready`; a keyed host may return a canonical job suspension.
    fn evaluate_comptime_type_syntax(
        &mut self,
        program: &H::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<H::Name, H::Type>,
        values: &AHashMap<H::Name, H::Value>,
        span: Span,
    ) -> ComptimeOutcome<H::Type, H::Failure> {
        match self
            .host
            .begin_comptime_type_syntax(program, syntax, types, values, span)
        {
            ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(value)) => {
                ComptimeOutcome::Known(value)
            }
            ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Suspended(suspension)) => {
                self.drive_structured_type(suspension)
            }
            ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
            ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
            ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
            ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
        }
    }

    /// Decode anonymous method signatures from RIR exactly once, at the AIR
    /// boundary. Hosts receive the resolved descriptor below and therefore do
    /// not need to interpret `FnDecl`, parameter ranges, or type syntax.
    pub(crate) fn decode_anon_method_descriptors(
        &mut self,
        program: &H::ProgramKey,
        methods: &rue_rir::RirAnonStructMethodsRange,
        types: &AHashMap<H::Name, H::Type>,
        values: &AHashMap<H::Name, H::Value>,
    ) -> ComptimeOutcome<Vec<ComptimeMethodDescriptor<H::Name, H::Type>>, H::Failure> {
        let method_refs = self
            .host
            .program_rir(program)
            .anon_struct_methods(methods)
            .to_vec();
        let mut descriptors = Vec::with_capacity(method_refs.len());
        for method_ref in method_refs {
            let (instruction_data, method_span) = {
                let instruction = self.host.program_rir(program).get(method_ref);
                (instruction.data.clone(), instruction.span)
            };
            let InstData::FnDecl {
                name,
                params,
                return_type,
                has_self,
                self_mode,
                returns_borrow,
                returns_inout,
                ..
            } = instruction_data
            else {
                return ComptimeOutcome::HostFailure(
                    self.host.non_function_anon_method(method_span),
                );
            };
            let method_name = self.host.name_from_symbol(program, name.into());
            let parameter_data = self.host.program_rir(program).params(&params).to_vec();
            let parameter_names = parameter_data
                .iter()
                .map(|parameter| self.host.name_from_symbol(program, parameter.name.into()))
                .collect();
            // Preserve declaration-level diagnostic priority: reject an own
            // comptime type parameter before any other parameter or result
            // syntax can suspend or fail.
            if parameter_data.iter().any(|parameter| {
                parameter.is_comptime
                    && self
                        .host
                        .rir_type_named_symbol(program, parameter.ty)
                        .is_some_and(|name| self.host.display_name(&name) == "type")
            }) {
                return ComptimeOutcome::HostFailure(self.host.unsupported_anon_method_type_param(
                    method_span,
                    &self.host.display_name(&method_name),
                ));
            }
            let mut parameters = Vec::with_capacity(parameter_data.len());
            for parameter in parameter_data {
                let is_self = self
                    .host
                    .rir_type_named_symbol(program, parameter.ty)
                    .is_some_and(|name| self.host.display_name(&name) == "Self");
                let is_comptime_type = parameter.is_comptime
                    && self
                        .host
                        .rir_type_named_symbol(program, parameter.ty)
                        .is_some_and(|name| self.host.display_name(&name) == "type");
                let ty = if is_self {
                    ComptimeMethodType::SelfType
                } else {
                    match self.evaluate_comptime_type_syntax(
                        program,
                        parameter.ty,
                        types,
                        values,
                        method_span,
                    ) {
                        ComptimeOutcome::Known(ty) => ComptimeMethodType::Concrete(ty),
                        ComptimeOutcome::RuntimeDependent | ComptimeOutcome::UnsupportedContext => {
                            ComptimeMethodType::Unsupported(
                                self.host
                                    .rir_type_named_symbol(program, parameter.ty)
                                    .map_or_else(
                                        || self.host.render_rir_type(program, parameter.ty),
                                        |name| self.host.display_name(&name),
                                    ),
                            )
                        }
                        ComptimeOutcome::NotReady => return ComptimeOutcome::NotReady,
                        ComptimeOutcome::Trap(trap) => return ComptimeOutcome::Trap(trap),
                        ComptimeOutcome::HostFailure(error) => {
                            return ComptimeOutcome::HostFailure(error);
                        }
                        ComptimeOutcome::Abort(error) => return ComptimeOutcome::Abort(error),
                    }
                };
                parameters.push(ComptimeMethodParameter {
                    ty,
                    mode: parameter.mode,
                    is_comptime: parameter.is_comptime,
                    is_comptime_type,
                });
            }
            let result = if self
                .host
                .rir_type_named_symbol(program, return_type)
                .is_some_and(|name| self.host.display_name(&name) == "Self")
            {
                ComptimeMethodType::SelfType
            } else {
                match self.evaluate_comptime_type_syntax(
                    program,
                    return_type,
                    types,
                    values,
                    method_span,
                ) {
                    ComptimeOutcome::Known(ty) => ComptimeMethodType::Concrete(ty),
                    ComptimeOutcome::RuntimeDependent | ComptimeOutcome::UnsupportedContext => {
                        ComptimeMethodType::Unsupported(
                            self.host
                                .rir_type_named_symbol(program, return_type)
                                .map_or_else(
                                    || self.host.render_rir_type(program, return_type),
                                    |name| self.host.display_name(&name),
                                ),
                        )
                    }
                    ComptimeOutcome::NotReady => return ComptimeOutcome::NotReady,
                    ComptimeOutcome::Trap(trap) => return ComptimeOutcome::Trap(trap),
                    ComptimeOutcome::HostFailure(error) => {
                        return ComptimeOutcome::HostFailure(error);
                    }
                    ComptimeOutcome::Abort(error) => return ComptimeOutcome::Abort(error),
                }
            };
            descriptors.push(ComptimeMethodDescriptor {
                name: method_name,
                has_self,
                self_mode,
                returns_borrow,
                returns_inout,
                parameters,
                parameter_names,
                result,
                declaration_span: method_span,
            });
        }
        ComptimeOutcome::Known(descriptors)
    }

    fn program_rir(&self) -> &Rir {
        let frame = self
            .frames
            .last()
            .expect("comptime evaluation requires an active frame");
        self.host.program_rir(&frame.program)
    }

    fn program_key(&self) -> H::ProgramKey {
        self.frames
            .last()
            .expect("comptime evaluation requires an active frame")
            .program
            .clone()
    }

    fn semantic_site(
        &self,
        inst_ref: InstRef,
        kind: ComptimeSiteKind,
        span: Span,
    ) -> ComptimeSite<H::ProgramKey> {
        let program = self.program_key();
        let rir = self.host.program_rir(&program);
        let mut sites = Vec::new();
        for (candidate, instruction) in rir.iter() {
            let candidate_kind = match (&instruction.data, kind) {
                (InstData::Intrinsic { name, args }, ComptimeSiteKind::Import)
                    if self
                        .host
                        .display_name(&self.host.name_from_symbol(&program, (*name).into()))
                        == "import"
                        && rir.intrinsic_args(args).get(0).is_some_and(|argument| {
                            matches!(rir.get(argument).data, InstData::StringConst { .. })
                        })
                        && rir.intrinsic_args(args).len() == 1 =>
                {
                    Some(ComptimeSiteKind::Import)
                }
                (InstData::Intrinsic { .. }, ComptimeSiteKind::Intrinsic) => {
                    Some(ComptimeSiteKind::Intrinsic)
                }
                (InstData::EnumVariant { .. }, ComptimeSiteKind::EnumVariant) => {
                    Some(ComptimeSiteKind::EnumVariant)
                }
                (InstData::FieldGet { .. }, ComptimeSiteKind::Member) => {
                    Some(ComptimeSiteKind::Member)
                }
                _ => None,
            };
            if candidate_kind.is_some() {
                sites.push((instruction.span.start, instruction.span.end, candidate));
            }
        }
        sites.sort_by_key(|(start, end, candidate)| (*start, *end, candidate.as_u32()));
        let occurrence = sites
            .iter()
            .position(|(_, _, candidate)| *candidate == inst_ref)
            .expect("classified comptime site must be present in its owning RIR");
        let occurrence =
            u32::try_from(occurrence).expect("comptime site occurrence must fit in u32");
        ComptimeSite::new(program, kind, occurrence, span)
    }

    fn name_from_rir(&self, symbol: SymbolHandle) -> H::Name {
        let frame = self
            .frames
            .last()
            .expect("comptime evaluation requires an active frame");
        let name = self.host.name_from_symbol(&frame.program, symbol);
        frame.name_bindings.get(&name).cloned().unwrap_or(name)
    }

    pub fn evaluate(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        // Public expression evaluation is intentionally ticket-free. Named
        // frames may only enter through the admitted-call path, after depth
        // and canonical-producer checks have issued their mandatory ticket.
        if frame.name.is_some() {
            return ComptimeOutcome::UnsupportedContext;
        }
        let body = frame.body;
        let previous_expected = env.expected_result.clone();
        env.expected_result = frame.expected_result.clone();
        self.frames.push(frame);
        let result = self.eval(body, env);
        self.frames.pop();
        env.expected_result = previous_expected;
        result
    }

    /// Evaluate a named call through a child call. The body host receives
    /// only the semantically named call operation; recursive expression edges
    /// stay in this engine.
    #[inline(never)]
    fn evaluate_call(
        &mut self,
        name: H::Name,
        args: &rue_rir::RirCallArgsRange,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let args = self.program_rir().call_args(args).to_vec();
        let arg_modes: Vec<ComptimeArgMode> = args
            .iter()
            .map(|arg| (arg.mode, self.program_rir().get(arg.value).span))
            .collect();
        let admission =
            host_value!(
                self.host
                    .admit_comptime_call(name, args.len(), &arg_modes, env, false)
            );
        let Some(admission) = admission else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let mut values = Vec::with_capacity(args.len());
        for arg in &args {
            let value = outcome_value!(self.eval(arg.value, env));
            values.push(value);
        }
        let bound = host_value!(self.host.bind_comptime_call(&admission, &values, span));
        let Some((callee_types, callee_values)) = bound else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let preparation = host_value!(self.host.prepare_comptime_call(
            admission,
            callee_types,
            callee_values,
            span
        ));
        let Some(preparation) = preparation else {
            return ComptimeOutcome::RuntimeDependent;
        };
        match preparation {
            ComptimeCallPreparation::Memoized(outcome) => outcome,
            ComptimeCallPreparation::Enter { frame, ticket } => {
                self.enter_prepared_call(frame, ticket, span)
            }
        }
    }

    #[inline(never)]
    fn enter_prepared_call(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        ticket: H::CompletionTicket,
        call_span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        // Root expression frames are intentionally ticket-free. A host must
        // not be able to smuggle one through Enter and silently bypass the
        // enter/finish lifecycle.
        if frame.name.is_none() {
            return ComptimeOutcome::UnsupportedContext;
        }
        self.enter_call(frame, ticket, call_span)
    }

    #[inline(never)]
    fn enter_call(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        ticket: H::CompletionTicket,
        call_span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        self.run_frame(frame, ticket, call_span)
    }

    pub fn evaluate_frame(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let mut env = ComptimeEnv::with_subst(&frame.type_bindings, &frame.value_bindings);
        self.evaluate(frame, &mut env)
    }

    pub(crate) fn evaluate_entered_frame(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        ticket: H::CompletionTicket,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let span = frame.span;
        self.run_frame(frame, ticket, span)
    }

    #[inline(never)]
    fn run_frame(
        &mut self,
        mut frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        ticket: H::CompletionTicket,
        call_span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let entered_depth = self
            .frames
            .iter()
            .filter(|frame| frame.name.is_some())
            .count();
        if frame.name.is_some() && entered_depth >= MAX_COMPTIME_CALL_DEPTH {
            return ComptimeOutcome::HostFailure(self.host.depth_exceeded(
                frame.name.as_ref().expect("named frame"),
                MAX_COMPTIME_CALL_DEPTH,
                frame.function_span,
            ));
        }
        if let Some(name) = frame.name.clone() {
            let canonical_identity = host_value!(self.host.canonical_function_producer(
                name,
                &frame.type_bindings,
                &frame.value_bindings,
                frame.span,
            ));
            frame.call_identity = Some(canonical_identity);
            // Admission and canonical producer issuance are complete. Only
            // now may a host activate the opaque completion ticket carried by
            // this frame; depth/producer failures above never activate it.
            host_value!(self.host.enter_comptime_call(&frame, &ticket));
        }
        let mut child_env = ComptimeEnv::with_subst(&frame.type_bindings, &frame.value_bindings);
        child_env.canonical_identity = frame.call_identity.clone();
        child_env.defining_file = frame.context.clone();
        child_env.expected_result = frame.expected_result.clone();
        let body = frame.body;
        let is_call = frame.name.is_some();
        self.frames.push(frame);
        let result = self.eval(body, &mut child_env);
        let frame = self.frames.pop().expect("comptime frame stack underflow");
        if is_call {
            let result = match result {
                ComptimeOutcome::HostFailure(error) => {
                    ComptimeOutcome::HostFailure(H::label_ctor_instantiation_site(error, call_span))
                }
                ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
                other => other,
            };
            self.host.finish_comptime_call(&frame, ticket, result)
        } else {
            result
        }
    }

    fn evaluate_method_call(
        &mut self,
        receiver: InstRef,
        method: H::Name,
        args: &rue_rir::RirCallArgsRange,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let args = self.program_rir().call_args(args).to_vec();
        let decoded = self.decode_module_path(receiver, env);
        let Some((file_id, segments)) = decoded else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let name = host_value!(
            self.host
                .resolve_module_comptime_callable(file_id, &segments, method, span)
        );
        let Some(name) = name else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let arg_modes: Vec<ComptimeArgMode> = args
            .iter()
            .map(|arg| (arg.mode, self.program_rir().get(arg.value).span))
            .collect();
        let admission =
            host_value!(
                self.host
                    .admit_comptime_call(name, args.len(), &arg_modes, env, true)
            );
        let Some(admission) = admission else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let mut values = Vec::with_capacity(args.len());
        for arg in &args {
            let value = outcome_value!(self.eval(arg.value, env));
            values.push(value);
        }
        let bound = host_value!(self.host.bind_comptime_call(&admission, &values, span));
        let Some((callee_types, callee_values)) = bound else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let preparation = host_value!(self.host.prepare_comptime_call(
            admission,
            callee_types,
            callee_values,
            span
        ));
        let Some(preparation) = preparation else {
            return ComptimeOutcome::RuntimeDependent;
        };
        match preparation {
            ComptimeCallPreparation::Memoized(outcome) => outcome,
            ComptimeCallPreparation::Enter { frame, ticket } => {
                self.enter_prepared_call(frame, ticket, span)
            }
        }
    }

    /// Decode only the syntactic module path for a method call. Resolution of
    /// the path's declarations and visibility stays in the semantic host; the
    /// engine owns this RIR edge so hosts never need to inspect child
    /// instructions to discover a callable.
    fn decode_module_path(
        &self,
        receiver: InstRef,
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> Option<(H::File, Vec<H::Name>)> {
        let mut chain_rev = Vec::new();
        let mut cursor = receiver;
        let root = loop {
            match self.program_rir().get(cursor).data {
                InstData::VarRef { name, .. } => break self.name_from_rir(name.into()),
                InstData::FieldGet { base, field } => {
                    chain_rev.push(self.name_from_rir(field.into()));
                    cursor = base;
                }
                _ => return None,
            }
        };
        if env.locals.contains_key(&root)
            || env.runtime_local_names.contains(&root)
            || env.runtime_binding_names.contains(&root)
            || env.type_subst.contains_key(&root)
            || env.value_subst.contains_key(&root)
        {
            return None;
        }
        let file_id = env.defining_file.clone()?;
        chain_rev.reverse();
        let mut segments = Vec::with_capacity(chain_rev.len() + 1);
        segments.push(root);
        segments.extend(chain_rev);
        Some((file_id, segments))
    }

    /// Decode a dotted type path before crossing the host boundary. The host
    /// receives only copied semantic path facts; it never needs to inspect the
    /// RIR spine or an evaluation environment to decide whether this is a
    /// module/type path.
    fn decode_type_path(
        &self,
        inst_ref: InstRef,
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> Option<(H::File, Vec<H::Name>)> {
        let mut chain_rev = Vec::new();
        let mut cursor = inst_ref;
        let root = loop {
            match self.program_rir().get(cursor).data {
                InstData::VarRef { name, .. } => break self.name_from_rir(name.into()),
                InstData::FieldGet { base, field } => {
                    chain_rev.push(self.name_from_rir(field.into()));
                    cursor = base;
                }
                _ => return None,
            }
        };
        if env.locals.contains_key(&root)
            || env.runtime_local_names.contains(&root)
            || env.runtime_binding_names.contains(&root)
            || env.type_subst.contains_key(&root)
            || env.value_subst.contains_key(&root)
        {
            return None;
        }
        let file_id = env.defining_file.clone()?;
        chain_rev.reverse();
        let mut segments = Vec::with_capacity(chain_rev.len() + 1);
        segments.push(root);
        segments.extend(chain_rev);
        Some((file_id, segments))
    }

    fn eval_int_operands(
        &mut self,
        lhs: InstRef,
        rhs: InstRef,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<(H::Value, H::Value), H::Failure> {
        let l = outcome_value!(self.eval(lhs, env));
        let Some(_) = l.as_integer() else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let r = outcome_value!(self.eval(rhs, env));
        let Some(_) = r.as_integer() else {
            return ComptimeOutcome::RuntimeDependent;
        };
        ComptimeOutcome::Known((l, r))
    }

    fn integer_pair(values: &(H::Value, H::Value)) -> Option<(i128, i128)> {
        Some((values.0.as_integer()?, values.1.as_integer()?))
    }

    fn integer_type_for(
        &mut self,
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        inst_ref: InstRef,
        lhs: &H::Value,
        rhs: &H::Value,
        span: Span,
    ) -> ComptimeOutcome<Option<H::Type>, H::Failure> {
        let hint = self
            .host
            .const_expr_type(&self.program_key(), env, inst_ref)
            .or_else(|| {
                env.expected_result
                    .as_ref()
                    .filter(|ty| self.host.type_integer_semantics(ty).is_some())
                    .cloned()
            });
        ComptimeOutcome::Known(host_value!(self.host.integer_operation_type(
            hint.as_ref(),
            lhs,
            rhs,
            span
        )))
    }

    fn unary_integer_type_for(
        &mut self,
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        inst_ref: InstRef,
        operand: &H::Value,
        span: Span,
    ) -> ComptimeOutcome<Option<H::Type>, H::Failure> {
        let hint = self
            .host
            .const_expr_type(&self.program_key(), env, inst_ref)
            .or_else(|| {
                env.expected_result
                    .as_ref()
                    .filter(|ty| self.host.type_integer_semantics(ty).is_some())
                    .cloned()
            });
        ComptimeOutcome::Known(host_value!(self.host.unary_integer_type(
            hint.as_ref(),
            operand,
            span
        )))
    }

    fn finish_arith_value(
        &mut self,
        result: CheckedIntegerResult,
        ty: Option<H::Type>,
        op: &str,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let Some(value) = host_value!(self.host.finish_arith(result, ty, op, span)) else {
            return ComptimeOutcome::RuntimeDependent;
        };
        ComptimeOutcome::Known(value)
    }

    /// Keep recursive control-flow and call edges out of the large instruction
    /// dispatcher stack frame. This small trampoline is important for the
    /// shared depth boundary: a deeply recursive comptime call must reach the
    /// engine's 48-frame check before the dispatcher itself exhausts the host
    /// thread stack.
    #[inline(never)]
    fn eval(
        &mut self,
        inst_ref: InstRef,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let (data, span) = {
            let source = self.program_rir().get(inst_ref);
            (source.data.clone(), source.span)
        };
        match data {
            InstData::Call { name, args } => {
                let name = self.name_from_rir(name.into());
                self.evaluate_call(name, &args, env, span)
            }
            InstData::Comptime { expr } => self.eval(expr, env),
            InstData::Block { instructions } => self.eval_block(instructions, env),
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => self.eval_branch(cond, then_block, else_block, env),
            _ => self.eval_dispatch(inst_ref, env),
        }
    }

    #[inline(never)]
    fn eval_block(
        &mut self,
        instructions: rue_rir::RirBlockInstsRange,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let stmt_refs = self.program_rir().block_insts(&instructions).to_vec();
        if stmt_refs.is_empty() {
            return ComptimeOutcome::Known(H::Value::unit());
        }
        let saved_locals = env.locals.clone();
        let mut result = H::Value::unit();
        for (i, stmt_ref) in stmt_refs.iter().copied().enumerate() {
            let is_tail = i + 1 == stmt_refs.len();
            let value = if let InstData::Alloc { name, init, .. } =
                &self.program_rir().get(stmt_ref).data
            {
                let name = name.map(|name| self.name_from_rir(name.into()));
                let init = *init;
                let value = match self.eval(init, env) {
                    ComptimeOutcome::Known(value) => value,
                    other => {
                        env.locals = saved_locals;
                        return other;
                    }
                };
                if let Some(name) = name {
                    env.locals.insert(name, value);
                }
                H::Value::unit()
            } else {
                match self.eval(stmt_ref, env) {
                    ComptimeOutcome::Known(value) => value,
                    other => {
                        env.locals = saved_locals;
                        return other;
                    }
                }
            };
            if is_tail {
                result = value;
            }
        }
        env.locals = saved_locals;
        ComptimeOutcome::Known(result)
    }

    #[inline(never)]
    fn eval_branch(
        &mut self,
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        match self.eval(cond, env) {
            ComptimeOutcome::Known(value) if value.as_boolean() == Some(true) => {
                self.eval(then_block, env)
            }
            ComptimeOutcome::Known(value) if value.as_boolean() == Some(false) => {
                match else_block {
                    Some(else_block) => self.eval(else_block, env),
                    None => ComptimeOutcome::Known(H::Value::unit()),
                }
            }
            other => other,
        }
    }

    /// The single compile-time evaluation engine. See the module docs for the
    /// result encoding is a typed `ComptimeOutcome`; no recursive edge is
    /// collapsed into a legacy optional result inside the engine.
    #[inline(never)]
    fn eval_dispatch(
        &mut self,
        inst_ref: InstRef,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let inst = {
            let source = self.program_rir().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };
        let span = inst.span;
        match &inst.data {
            // Integer literals. The literal itself must fit its resolved type
            // (the inner expression of a comptime block never goes through
            // `analyze_literal`, so this is where `300` at type u8 is caught).
            InstData::IntConst(value) => {
                let v = *value as i128;
                let ty = self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref);
                if let Some(ty) = &ty {
                    if !self
                        .host
                        .type_integer_semantics(ty)
                        .is_some_and(|integer| integer.fits_i128(v))
                    {
                        return ComptimeOutcome::HostFailure(
                            self.host.literal_out_of_range(*value, ty, span),
                        );
                    }
                }
                ComptimeOutcome::Known(H::Value::integer_typed(v, ty))
            }

            // Float literals stop here for the same reason they stop in
            // `analyze_inst_dispatch` (ADR-0065, RUE-1069): there is no
            // `comptime_float` value in the host's value domain yet. Naming the real
            // reason matters more here than elsewhere — falling through to
            // the generic "not knowable at compile time" would be actively
            // wrong about a literal, which is the most compile-time-knowable
            // thing there is. Delete this arm when Phase 4 lands.
            InstData::FloatConst { .. } => {
                host_value!(self.host.require_preview(
                    rue_error::PreviewFeature::Floats,
                    "a floating-point literal",
                    span,
                ));
                ComptimeOutcome::HostFailure(self.host.float_not_implemented(span))
            }

            // String constants are intentionally routed through the host:
            // they are not part of the ordinary four-value comptime algebra,
            // but durable declaration evaluation needs their semantic spelling
            // for controls such as `@import`. The host sees only the name; the
            // engine still owns this instruction dispatch.
            InstData::StringConst { content, .. } => self
                .host
                .resolve_string_const(self.name_from_rir((*content).into()), span),

            // Boolean literals
            InstData::BoolConst(value) => ComptimeOutcome::Known(H::Value::boolean(*value)),

            // Unit literal
            InstData::UnitConst => ComptimeOutcome::Known(H::Value::unit()),

            // Unary negation: -expr
            InstData::Neg { operand } => {
                if let InstData::IntConst(magnitude) = self.program_rir().get(*operand).data {
                    let literal = H::Value::integer(magnitude as i128);
                    let ty =
                        outcome_value!(self.unary_integer_type_for(env, inst_ref, &literal, span,));
                    if let Some(ref ty) = ty {
                        if self.host.type_is_unsigned(ty) {
                            return ComptimeOutcome::HostFailure(self.host.cannot_negate(ty, span));
                        }
                    }
                    // The literal path uses mathematical magnitude semantics:
                    // unlike an ordinary runtime value, `128` must not first
                    // canonicalize to -128 before becoming `-128`.
                    let result = ty
                        .as_ref()
                        .and_then(|ty| self.host.type_integer_semantics(ty))
                        .map_or_else(
                            || CheckedIntegerResult::from_raw((magnitude as i128).checked_neg()),
                            |integer| integer.checked_neg_literal_report_i128(magnitude as i128),
                        );
                    self.finish_arith_value(result, ty, "-", span)
                } else {
                    match self.eval(*operand, env) {
                        ComptimeOutcome::Known(value) => {
                            let ty = outcome_value!(
                                self.unary_integer_type_for(env, inst_ref, &value, span,)
                            );
                            if let Some(ref ty) = ty {
                                if self.host.type_is_unsigned(ty) {
                                    return ComptimeOutcome::HostFailure(
                                        self.host.cannot_negate(ty, span),
                                    );
                                }
                            }
                            let Some(n) = value.as_integer() else {
                                return ComptimeOutcome::RuntimeDependent;
                            };
                            let result = ty
                                .as_ref()
                                .and_then(|ty| self.host.type_integer_semantics(ty))
                                .map_or_else(
                                    || CheckedIntegerResult::from_raw(n.checked_neg()),
                                    |integer| integer.checked_neg_report_i128(n),
                                );
                            self.finish_arith_value(result, ty, "-", span)
                        }
                        other => other,
                    }
                }
            }

            // Logical NOT: !expr
            InstData::Not { operand } => {
                match self.eval(*operand, env) {
                    ComptimeOutcome::Known(value) => match value.as_boolean() {
                        Some(b) => ComptimeOutcome::Known(H::Value::boolean(!b)),
                        None => ComptimeOutcome::RuntimeDependent,
                    },
                    // Can't logical-NOT an integer, type, or unit
                    other => other,
                }
            }

            // Binary arithmetic operations, checked at the operand type
            InstData::Add { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || CheckedIntegerResult::from_raw(l.checked_add(r)),
                        |integer| integer.checked_add_report_i128(l, r),
                    );
                self.finish_arith_value(result, ty, "+", span)
            }
            InstData::Sub { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || CheckedIntegerResult::from_raw(l.checked_sub(r)),
                        |integer| integer.checked_sub_report_i128(l, r),
                    );
                self.finish_arith_value(result, ty, "-", span)
            }
            InstData::Mul { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || CheckedIntegerResult::from_raw(l.checked_mul(r)),
                        |integer| integer.checked_mul_report_i128(l, r),
                    );
                self.finish_arith_value(result, ty, "*", span)
            }
            InstData::Div { lhs, rhs } | InstData::Mod { lhs, rhs } => {
                let is_div = matches!(&inst.data, InstData::Div { .. });
                let op = if is_div { "/" } else { "%" };
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                if r == 0 {
                    return match ty {
                        Some(_) => ComptimeOutcome::Trap(ComptimeTrap {
                            operation: if is_div {
                                "division by zero"
                            } else {
                                "remainder by zero"
                            },
                            span,
                        }),
                        // Untyped fallback: defer to the runtime check.
                        None => ComptimeOutcome::RuntimeDependent,
                    };
                }
                // Untyped evaluation retains its historical i64 fallback;
                // typed MIN / -1 trapping is owned by the kernel report.
                if r == -1 && ty.is_none() && l == i128::from(i64::MIN) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || {
                            CheckedIntegerResult::from_raw(if is_div {
                                l.checked_div(r)
                            } else {
                                l.checked_rem(r)
                            })
                        },
                        |integer| {
                            if is_div {
                                integer.checked_div_report_i128(l, r)
                            } else {
                                integer.checked_rem_report_i128(l, r)
                            }
                        },
                    );
                self.finish_arith_value(result, ty, op, span)
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                let lhs = match self.eval(*lhs, env) {
                    ComptimeOutcome::Known(value) => value,
                    ComptimeOutcome::RuntimeDependent => {
                        return match self.eval(*rhs, env) {
                            ComptimeOutcome::Known(_) | ComptimeOutcome::RuntimeDependent => {
                                ComptimeOutcome::RuntimeDependent
                            }
                            other => other,
                        };
                    }
                    other => return other,
                };
                match self.eval(*rhs, env) {
                    ComptimeOutcome::Known(rhs) => {
                        if lhs.as_integer().is_some() && rhs.as_integer().is_some() {
                            let _ = outcome_value!(
                                self.integer_type_for(env, inst_ref, &lhs, &rhs, span,)
                            );
                        }
                        match (
                            lhs.as_integer(),
                            rhs.as_integer(),
                            lhs.as_boolean(),
                            rhs.as_boolean(),
                        ) {
                            (Some(lhs), Some(rhs), _, _) => {
                                ComptimeOutcome::Known(H::Value::boolean(lhs == rhs))
                            }
                            (_, _, Some(lhs), Some(rhs)) => {
                                ComptimeOutcome::Known(H::Value::boolean(lhs == rhs))
                            }
                            _ => self.host.compare_comptime_values(&lhs, &rhs, true, span),
                        }
                    }
                    other => other,
                }
            }
            InstData::Ne { lhs, rhs } => {
                let lhs = match self.eval(*lhs, env) {
                    ComptimeOutcome::Known(value) => value,
                    ComptimeOutcome::RuntimeDependent => {
                        return match self.eval(*rhs, env) {
                            ComptimeOutcome::Known(_) | ComptimeOutcome::RuntimeDependent => {
                                ComptimeOutcome::RuntimeDependent
                            }
                            other => other,
                        };
                    }
                    other => return other,
                };
                match self.eval(*rhs, env) {
                    ComptimeOutcome::Known(rhs) => {
                        if lhs.as_integer().is_some() && rhs.as_integer().is_some() {
                            let _ = outcome_value!(
                                self.integer_type_for(env, inst_ref, &lhs, &rhs, span,)
                            );
                        }
                        match (
                            lhs.as_integer(),
                            rhs.as_integer(),
                            lhs.as_boolean(),
                            rhs.as_boolean(),
                        ) {
                            (Some(lhs), Some(rhs), _, _) => {
                                ComptimeOutcome::Known(H::Value::boolean(lhs != rhs))
                            }
                            (_, _, Some(lhs), Some(rhs)) => {
                                ComptimeOutcome::Known(H::Value::boolean(lhs != rhs))
                            }
                            _ => self.host.compare_comptime_values(&lhs, &rhs, false, span),
                        }
                    }
                    other => other,
                }
            }
            InstData::Lt { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let _ = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::boolean(l < r))
            }
            InstData::Gt { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let _ = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::boolean(l > r))
            }
            InstData::Le { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let _ = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::boolean(l <= r))
            }
            InstData::Ge { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let _ = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::boolean(l >= r))
            }

            // Logical operations: short-circuit like the runtime, so a
            // non-constant (or would-panic) RHS is irrelevant when the LHS
            // already decides the result.
            InstData::And { lhs, rhs } => match self.eval(*lhs, env) {
                ComptimeOutcome::Known(value) if value.as_boolean() == Some(false) => {
                    ComptimeOutcome::Known(H::Value::boolean(false))
                }
                ComptimeOutcome::Known(value) if value.as_boolean() == Some(true) => {
                    match self.eval(*rhs, env) {
                        ComptimeOutcome::Known(value) if value.as_boolean().is_some() => {
                            ComptimeOutcome::Known(value)
                        }
                        other => other,
                    }
                }
                other => other,
            },
            InstData::Or { lhs, rhs } => match self.eval(*lhs, env) {
                ComptimeOutcome::Known(value) if value.as_boolean() == Some(true) => {
                    ComptimeOutcome::Known(H::Value::boolean(true))
                }
                ComptimeOutcome::Known(value) if value.as_boolean() == Some(false) => {
                    match self.eval(*rhs, env) {
                        ComptimeOutcome::Known(value) if value.as_boolean().is_some() => {
                            ComptimeOutcome::Known(value)
                        }
                        other => other,
                    }
                }
                other => other,
            },

            // Bitwise operations. For values in range of their type these are
            // closed (no overflow possible), so no range check is needed.
            InstData::BitAnd { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::integer_typed(l & r, ty))
            }
            InstData::BitOr { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::integer_typed(l | r, ty))
            }
            InstData::BitXor { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::integer_typed(l ^ r, ty))
            }

            // Shifts: the amount is masked modulo the bit width and the
            // result truncated to the operand width (spec 4.3a:10), exactly
            // matching the runtime semantics (RUE-29).
            InstData::Shl { lhs, rhs } | InstData::Shr { lhs, rhs } => {
                let is_shl = matches!(&inst.data, InstData::Shl { .. });
                let operands = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                match ty.as_ref() {
                    Some(ty) => {
                        let Some(integer) = self.host.type_integer_semantics(ty) else {
                            return ComptimeOutcome::RuntimeDependent;
                        };
                        // Two's-complement AND masks negative amounts the same
                        // way the hardware masks the count register.
                        let v = integer.shift_i128(l, r, is_shl);
                        ComptimeOutcome::Known(H::Value::integer_typed(v, Some(ty.clone())))
                    }
                    None => {
                        // Without the operand type the width is unknown, so
                        // only fold amounts < 8 (safe for every width) and
                        // defer the rest to runtime.
                        if !(0..8).contains(&r) {
                            return ComptimeOutcome::RuntimeDependent;
                        }
                        ComptimeOutcome::Known(H::Value::integer_typed(
                            if is_shl { l << r } else { l >> r },
                            None,
                        ))
                    }
                }
            }

            // Bitwise NOT: truncated to the operand width (`~0` as u8 = 255).
            InstData::BitNot { operand } => {
                let n = outcome_value!(self.eval(*operand, env));
                let Some(raw) = n.as_integer() else {
                    return ComptimeOutcome::RuntimeDependent;
                };
                let ty = outcome_value!(self.unary_integer_type_for(env, inst_ref, &n, span,));
                let v = match ty.as_ref() {
                    Some(ty) => match self.host.type_integer_semantics(ty) {
                        Some(integer) => integer.bitnot_i128(raw),
                        None => return ComptimeOutcome::RuntimeDependent,
                    },
                    None => !raw,
                };
                ComptimeOutcome::Known(H::Value::integer_typed(v, ty))
            }

            // These control-flow and call forms are handled by `eval`'s small
            // trampoline so recursive calls do not retain this large frame.
            InstData::Comptime { .. }
            | InstData::Block { .. }
            | InstData::Branch { .. }
            | InstData::Call { .. } => unreachable!("routed by comptime eval trampoline"),

            // Comptime-known `match`: evaluate the scrutinee, select the first
            // arm whose pattern matches, and reduce to that arm's body value
            // (spec 4.14:19, RUE-262). An enum-variant (`Path`) pattern isn't
            // representable in the host's value domain, and a non-constant scrutinee is
            // not decidable here — both make the `match` non-evaluable.
            InstData::Match { scrutinee, arms } => {
                let scrutinee = *scrutinee;
                let scrut = outcome_value!(self.eval(scrutinee, env));
                let arms = self.program_rir().match_arms(arms).to_vec();
                for (pattern, body) in arms.iter() {
                    match self
                        .host
                        .match_pattern(&self.program_key(), pattern, &scrut)
                    {
                        Some(true) => return self.eval(*body, env),
                        Some(false) => continue,
                        // Undecidable pattern (e.g. an enum-variant `Path`
                        // against a non-representable scrutinee): bail out.
                        None => return ComptimeOutcome::RuntimeDependent,
                    }
                }
                // No arm matched. Exhaustiveness checking should make this
                // unreachable for a well-typed match; treat as non-evaluable.
                ComptimeOutcome::RuntimeDependent
            }

            // Anonymous struct type: evaluate to a comptime type value,
            // resolving field types through the type substitution.
            InstData::AnonStructType {
                fields,
                methods,
                anchor,
            } => {
                let field_decls = self.program_rir().anon_struct_fields(fields).to_vec();

                // Comptime `let` locals in scope participate in field-type
                // resolution (`let Inner = Mk(T); struct { x: Inner }`,
                // RUE-575), alongside the enclosing parameters.
                let (local_type_subst, local_value_subst) = env.substs_with_locals();

                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let field_name = self.name_from_rir(name_sym.into());
                    // Field types resolve through both the type substitution
                    // (`comptime T: type`) and the value substitution
                    // (`comptime N: i32`, so an `[i32; N]` field gets a concrete
                    // length at each specialization; RUE-16).
                    let field_ty = outcome_value!(self.evaluate_comptime_type_syntax(
                        &self.program_key(),
                        type_sym,
                        &local_type_subst,
                        &local_value_subst,
                        span,
                    ));
                    struct_fields.push(ComptimeField {
                        name: field_name,
                        ty: field_ty,
                    });
                }

                // Decode method signatures in the canonical engine. The host
                // receives only resolved semantic descriptors below.
                let method_sigs = outcome_value!(self.decode_anon_method_descriptors(
                    &self.program_key(),
                    methods,
                    &local_type_subst,
                    &local_value_subst,
                ));

                let Some(producer) = env.canonical_identity.clone() else {
                    return ComptimeOutcome::RuntimeDependent;
                };
                let identity = self.host.issue_anonymous_identity(
                    &self.program_key(),
                    ComptimeAnonymousKind::Struct,
                    &producer,
                    anchor,
                );
                let (struct_ty, _is_new) = host_value!(self.host.find_or_create_anon_struct(
                    identity,
                    &struct_fields,
                    &method_sigs,
                    &local_value_subst,
                ));

                // Method body registration is an ordinary analysis concern.
                // The generic comptime host receives only structural method
                // descriptors and never a child-RIR token.
                ComptimeOutcome::Known(H::Value::type_value(struct_ty))
            }

            // Anonymous enum type: evaluate to a comptime type value, resolving
            // each variant's payload types through the type/value substitution.
            // The enum analog of the AnonStructType arm above — this is what
            // makes `fn Option(comptime T: type) -> type { enum { Some(T), None } }`
            // monomorphize per instantiation (ADR-0038, RUE-6 phase 2).
            InstData::AnonEnumType {
                variants,
                payloads,
                anchor,
            } => {
                let variant_syms = self.program_rir().anon_enum_variants(variants).to_vec();
                let payload_symbols: Vec<Vec<rue_rir::RirTypeSyntaxRef>> = self
                    .program_rir()
                    .anon_enum_payloads(payloads, variants)
                    .map(|payload| payload.to_vec())
                    .collect();

                // Decode the self-describing payload region into per-variant
                // type-symbol lists (parallel to `variant_syms`), then resolve
                // each payload type through the substitutions.
                // Comptime `let` locals participate in payload-type
                // resolution, matching the struct arm (RUE-575).
                let (enum_type_subst, enum_value_subst) = env.substs_with_locals();

                let mut variant_names: Vec<String> = Vec::with_capacity(variant_syms.len());
                let mut variant_payloads: Vec<Vec<H::Type>> =
                    Vec::with_capacity(variant_syms.len());
                for (&vsym, symbols) in variant_syms.iter().zip(payload_symbols) {
                    variant_names.push(self.host.display_name(&self.name_from_rir(vsym.into())));
                    let mut tys: Vec<H::Type> = Vec::with_capacity(symbols.len());
                    for ty_sym in symbols {
                        let ty = outcome_value!(self.evaluate_comptime_type_syntax(
                            &self.program_key(),
                            ty_sym,
                            &enum_type_subst,
                            &enum_value_subst,
                            span,
                        ));
                        tys.push(ty);
                    }
                    variant_payloads.push(tys);
                }

                let Some(producer) = env.canonical_identity.clone() else {
                    return ComptimeOutcome::RuntimeDependent;
                };
                let identity = self.host.issue_anonymous_identity(
                    &self.program_key(),
                    ComptimeAnonymousKind::Enum,
                    &producer,
                    anchor,
                );
                let enum_ty = host_value!(self.host.find_or_create_anon_enum(
                    identity,
                    &variant_names,
                    &variant_payloads,
                ));
                ComptimeOutcome::Known(H::Value::type_value(enum_ty))
            }

            // TypeConst: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                let type_name = *type_name;
                // Type parameters in scope substitute first.
                if let Some(type_symbol) = self
                    .host
                    .rir_type_named_symbol(&self.program_key(), type_name)
                {
                    if let Some(ty) = env.type_subst.get(&type_symbol) {
                        return ComptimeOutcome::Known(H::Value::type_value(ty.clone()));
                    }
                    // A named type (primitive / struct / enum) resolves directly.
                    if let Some(ty) =
                        host_value!(self.host.resolve_named_type_value(type_symbol, span))
                    {
                        return ComptimeOutcome::Known(H::Value::type_value(ty));
                    }
                }
                // A *composite* or *unit* type value — `[i32; 2]`, `()`,
                // `ptr const T` — is an equally-valid type argument (Appendix A
                // treats them as unambiguous type spellings; RUE-565). Its
                // TypeConst carries the composite spelling as the interned
                // `type_name`, so decode it through the full comptime type
                // resolver under the current substitutions (an inner element /
                // pointee naming an enclosing `comptime T` still resolves). An
                // unresolvable spelling stays non-evaluable (`None`).
                let ty = outcome_value!(self.evaluate_comptime_type_syntax(
                    &self.program_key(),
                    type_name,
                    &env.type_subst,
                    &env.value_subst,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::type_value(ty))
            }

            // An array-repeat expression `[T; N]` used as a comptime *type* value
            // (RUE-565). The surface form `[i32; 2]` in expression position parses
            // as an array-repeat literal whose element is a type value; when that
            // element reduces to a type-valued comptime value, the whole expression is the
            // array TYPE `[T; N]` — a legal type-constructor argument
            // (`Option([i32; 2])`). A repeat over a *runtime* element is a genuine
            // array value literal and is not comptime-foldable here (`None`).
            InstData::ArrayRepeat { value, count } => {
                let (value, count) = (*value, count.clone());
                let value = outcome_value!(self.eval(value, env));
                let Some(elem_ty) = value.as_type() else {
                    return self.host.reject_non_type_array_repeat(value, span);
                };
                let len = match count {
                    RepeatCount::Literal(n) => n,
                    RepeatCount::Named(sym) => {
                        let name = self.name_from_rir(sym.into());
                        host_value!(self.host.resolve_named_array_length(
                            &name,
                            span,
                            Some(&env.value_subst),
                        ))
                    }
                };
                let array_ty = self.host.get_or_create_array_type(elem_ty, len);
                ComptimeOutcome::Known(H::Value::type_value(array_ty))
            }

            // VarRef: comptime let-bindings, comptime parameters, file-level
            // constants, then type names.
            InstData::VarRef { name, .. } => {
                let name = self.name_from_rir((*name).into());
                // 1. `let` bindings inside the comptime expression
                if let Some(v) = env.locals.get(&name) {
                    return ComptimeOutcome::Known(v.clone());
                }
                // 2. Runtime locals shadow comptime parameters and file-level
                //    constants: a reference that resolves to one is not
                //    compile-time evaluable (spec 4.14:6).
                if env.runtime_local_names.contains(&name) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                // 3. Comptime type parameters in scope
                if let Some(ty) = env.type_subst.get(&name) {
                    return ComptimeOutcome::Known(H::Value::type_value(ty.clone()));
                }
                // 4. Comptime value parameters in scope
                if let Some(v) = env.value_subst.get(&name) {
                    return ComptimeOutcome::Known(v.clone());
                }
                // 5. Runtime parameters shadow file-level constants and type
                //    names. A comptime parameter with a concrete value was
                //    already handled by the substitution maps above.
                if env.runtime_binding_names.contains(&name) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                // 6. File-level constants: the value was evaluated once
                //    (and range-checked against the declared type) during
                //    declaration gathering — use it directly. Re-evaluating
                //    the initializer here would fail for forms only the
                //    declaration collector can resolve (module member
                //    access, RUE-160) and was exponential for const chains.
                //    Module-typed constants never appear in this table
                //    (module bindings are a distinct tagged resolution).
                //    Privacy applies here too (E0460, RUE-183): the table is
                //    global, so a const initializer in one directory could
                //    otherwise read a private constant from another. The
                //    VarRef's own span locates the referencing file;
                //    speculative callers (`try_evaluate_const*`) swallow the
                //    error and defer to runtime analysis, which re-checks.
                let file = self.host.file_from_span(&span);
                let info = host_value!(self.host.value_const(&(file.clone(), name.clone())));
                if let Some(info) = info {
                    let defining_file = self.host.file_from_span(&info.span);
                    self.host
                        .record_value_const_dependency(&defining_file, &name);
                    host_value!(self.host.check_unqualified_visibility(
                        "constant",
                        &name,
                        defining_file,
                        info.is_pub,
                        span,
                    ));
                    // String constants stay out of the comptime engine: no
                    // engine operation consumes them (no comptime string
                    // params or string arithmetic), so treat a reference as
                    // non-evaluable instead of leaking a value the arms
                    // below would mis-type (RUE-957). Use sites materialize
                    // string constants through the runtime path instead.
                    return match info.value {
                        Some(value) => ComptimeOutcome::Known(value),
                        None => ComptimeOutcome::RuntimeDependent,
                    };
                }
                // 7. Type names used as values (e.g. `Point` in
                //    `fn make_type() -> type { Point }`)
                let resolved = host_value!(self.host.resolve_named_type_value(name, span));
                if let Some(ref ty) = resolved {
                    self.host.record_named_type_dependency(ty);
                }
                match resolved {
                    Some(value) => ComptimeOutcome::Known(H::Value::type_value(value)),
                    None => ComptimeOutcome::RuntimeDependent,
                }
            }

            // Module-member access (`m.CONST`) as an operand of a larger const
            // initializer. The value was pre-resolved from the module's file
            // (with privacy checks) before evaluation — see the
            // `const_module_members` field — since the engine has no file or
            // constant-collector context to resolve it here. A member absent
            // from the map may still be a member-access *type* path used as a
            // comptime type-constructor argument (`std.strbuf.StrBuf` in
            // `Result(std.strbuf.StrBuf, i32)`, RUE-948): resolve that chain to
            // its nominal type through the same walker the qualified
            // type-annotation position uses. A base that is neither a
            // pre-resolved member value nor a module type path (a runtime
            // value's field) stays non-evaluable, so the caller reports it
            // (RUE-267).
            InstData::FieldGet { base, field } => {
                if let Some(value) = env.const_module_members.get(&inst_ref) {
                    return ComptimeOutcome::Known(value.clone());
                }
                if let Some((file, segments)) = self.decode_type_path(inst_ref, env) {
                    if let Some(value) =
                        host_value!(self.host.resolve_comptime_type_path(file, &segments, span))
                    {
                        return ComptimeOutcome::Known(value);
                    }
                }
                let field = self.name_from_rir((*field).into());
                let site = self.semantic_site(inst_ref, ComptimeSiteKind::Member, span);
                if !host_value!(self.host.admit_comptime_member(field.clone(), &site)) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                let base = outcome_value!(self.eval(*base, env));
                self.host.resolve_comptime_member(base, field, &site, span)
            }

            // `checked { expr }` does not change the value produced by a
            // comptime expression. Keep the child traversal in this engine,
            // then let a semantic host observe or refine the completed value.
            InstData::Checked { expr } => {
                if !self.host.allow_checked_comptime() {
                    return ComptimeOutcome::RuntimeDependent;
                }
                match self.eval(*expr, env) {
                    ComptimeOutcome::Known(value) => self.host.finish_checked(value, span),
                    other => other,
                }
            }

            // Expression intrinsics receive semantic arguments. String
            // literals are carried as names so `@import("...")` can be
            // handled by a durable host; every other argument is recursively
            // evaluated here before crossing the host boundary.
            InstData::Intrinsic { name, args } => {
                let name = self.name_from_rir((*name).into());
                let arguments = self.program_rir().intrinsic_args(args).to_vec();
                let is_import = self.host.display_name(&name) == "import"
                    && arguments.len() == 1
                    && matches!(
                        self.program_rir().get(arguments[0]).data,
                        InstData::StringConst { .. }
                    );
                let kind = if is_import {
                    ComptimeSiteKind::Import
                } else {
                    ComptimeSiteKind::Intrinsic
                };
                let site = self.semantic_site(inst_ref, kind, span);
                if !host_value!(self.host.admit_comptime_intrinsic(name.clone(), &site)) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                let mut values = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    match self.program_rir().get(argument).data {
                        InstData::StringConst { content, .. } => values.push(
                            ComptimeIntrinsicArgument::String(self.name_from_rir(content.into())),
                        ),
                        _ => values.push(ComptimeIntrinsicArgument::Value(outcome_value!(
                            self.eval(argument, env)
                        ))),
                    }
                }
                self.host
                    .resolve_comptime_intrinsic(name, &values, &site, span)
            }

            // Enum variants are runtime values in the ordinary body domain.
            // Reduce a qualified module expression first, then hand only the
            // resulting semantic value and names to the host.
            InstData::EnumVariant {
                module,
                type_name,
                variant,
            } => {
                let site = self.semantic_site(inst_ref, ComptimeSiteKind::EnumVariant, span);
                let type_name = self.name_from_rir((*type_name).into());
                let variant = self.name_from_rir((*variant).into());
                if !host_value!(self.host.admit_comptime_enum_variant(
                    type_name.clone(),
                    variant.clone(),
                    &site,
                )) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                let module = match module {
                    Some(module) => Some(outcome_value!(self.eval(*module, env))),
                    None => None,
                };
                self.host
                    .resolve_comptime_enum_variant(module, type_name, variant, &site, span)
            }

            // Type intrinsic in comptime position. `@require_droppable(T)` is the
            // owning-container well-formedness gate (RUE-388/RUE-646): std's
            // `ArrayBuf(T)` calls it in its `-> type` constructor body so that
            // instantiating the container with an element type it cannot yet
            // correctly own — one that is `linear` — is rejected at instantiation
            // time (E0499). Droppable-but-non-linear elements are accepted: the
            // container runs each live element's drop glue before freeing its
            // buffer (RUE-646). It reduces to unit so the surrounding block
            // body still yields the `struct { .. }` tail. `@size_of`/`@align_of`
            // are not comptime-foldable here and stay non-evaluable (spec
            // 4.14:29); `@int_max`/`@int_min` depend only on the type identity,
            // not layout, so they evaluate to their integer bound (RUE-694).
            InstData::TypeIntrinsic { name, type_arg } => {
                let (name, type_arg) = (*name, *type_arg);
                let gate_name = self.name_from_rir(name.into());
                let gate = self.host.display_name(&gate_name);
                // Both well-formedness gates reduce to unit at comptime:
                // `@require_droppable` (instantiation-time, rejects `linear`) and
                // `@require_trivially_droppable` (read-time, rejects drop glue —
                // RUE-651). Any other type intrinsic (`@size_of`/`@align_of`) is
                // not comptime-foldable here.
                let is_droppable_gate = gate == "require_droppable";
                let is_trivial_gate = gate == "require_trivially_droppable";
                let is_int_bound = gate == "int_max" || gate == "int_min";
                if is_int_bound {
                    let is_max = gate == "int_max";
                    // A still-unresolved type parameter makes the intrinsic
                    // non-evaluable here; it folds at a concrete instantiation.
                    let int_ty = outcome_value!(self.evaluate_comptime_type_syntax(
                        &self.program_key(),
                        type_arg,
                        &env.type_subst,
                        &env.value_subst,
                        span,
                    ));
                    let bound = self.host.type_integer_semantics(&int_ty).map(|integer| {
                        if is_max {
                            integer.max_i128()
                        } else {
                            integer.min_i128()
                        }
                    });
                    // A non-integer argument is diagnosed by runtime analysis
                    // (`analyze_type_intrinsic`, E0702); stay non-evaluable
                    // rather than duplicating the diagnostic.
                    return match bound {
                        Some(value) => {
                            ComptimeOutcome::Known(H::Value::integer_typed(value, Some(int_ty)))
                        }
                        None => ComptimeOutcome::RuntimeDependent,
                    };
                }
                if !is_droppable_gate && !is_trivial_gate {
                    return ComptimeOutcome::RuntimeDependent;
                }
                // Resolve the element type through the enclosing comptime
                // substitutions (`T -> Inner` for `ArrayBuf(Inner)`); a
                // still-unresolved type parameter makes the gate non-evaluable
                // (it will be re-checked at a concrete instantiation).
                let elem_ty = outcome_value!(self.evaluate_comptime_type_syntax(
                    &self.program_key(),
                    type_arg,
                    &env.type_subst,
                    &env.value_subst,
                    span,
                ));
                if is_trivial_gate {
                    host_value!(self.host.check_trivially_droppable(elem_ty, span));
                } else {
                    host_value!(self.host.check_require_droppable(elem_ty, span));
                }
                ComptimeOutcome::Known(H::Value::unit())
            }

            // Module-qualified comptime type-constructor call in value position,
            // e.g. `let O = b.Mk(T)` inside a `-> type` constructor body that is
            // being reduced (RUE-511). The receiver must be an unshadowed
            // `VarRef` naming a module binding of the *defining* file; membership
            // and visibility are validated before the call is reduced through the
            // same path unqualified calls take. Any other receiver (a runtime
            // value's method, a shadowed name) is a genuine runtime call and
            // stays non-evaluable.
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                let receiver = *receiver;
                let method = self.name_from_rir((*method).into());
                self.evaluate_method_call(receiver, method, args, env, span)
            }

            // Everything else requires runtime evaluation
            _ => ComptimeOutcome::RuntimeDependent,
        }
    }
}
