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

    /// Mutably access only the metadata of one already-registered program
    /// without exposing its RIR, symbols, or keyed identity.
    pub fn metadata_mut(&mut self, key: &ComptimeProgramKey<D, C>) -> Option<&mut I> {
        self.programs
            .get_mut(key)
            .map(|program| &mut program.imports)
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

/// An expression argument passed to a semantic intrinsic hook. String
/// literals are represented as their interned semantic name instead of being
/// forced through the four-value comptime algebra; all other arguments have
/// already been recursively evaluated by [`ComptimeEngine`].
#[derive(Debug, Clone)]
pub enum ComptimeIntrinsicArgument<V, N> {
    Value(V),
    String(N),
}

/// The finite set of type intrinsics which can participate in declaration-time
/// comptime evaluation. Classification is owned by AIR so compiler hosts do
/// not maintain a second spelling table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeTypeIntrinsic {
    RequireDroppable,
    RequireTriviallyDroppable,
    IntegerBound(ComptimeIntegerBound),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeIntegerBound {
    Min,
    Max,
}

impl ComptimeIntegerBound {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Min => "int_min",
            Self::Max => "int_max",
        }
    }
}

impl ComptimeTypeIntrinsic {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "require_droppable" => Some(Self::RequireDroppable),
            "require_trivially_droppable" => Some(Self::RequireTriviallyDroppable),
            "int_min" => Some(Self::IntegerBound(ComptimeIntegerBound::Min)),
            "int_max" => Some(Self::IntegerBound(ComptimeIntegerBound::Max)),
            _ => None,
        }
    }
}

/// The semantic operation whose source occurrence is being resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeSiteKind {
    Intrinsic,
    Import,
    EnumVariant,
    Member,
}

/// Controls whether a comptime method receiver is evaluated as a semantic
/// value before callable admission. Ordinary body probing keeps the historical
/// path-only behavior; durable declaration hosts can opt into receiver
/// evaluation for exact module-receiver identity and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeMethodReceiverPolicy {
    SyntacticModulePath,
    EvaluateReceiver,
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

/// The exact owning program and source range for an engine-created diagnostic.
/// Unlike `ComptimeSite`, this carries no semantic occurrence classification:
/// terminal hooks need only the active program and the span supplied by the
/// engine.
#[derive(Debug, Clone)]
pub struct ComptimeDiagnosticSite<P> {
    program: P,
    span: Span,
}

impl<P> ComptimeDiagnosticSite<P> {
    /// Constructs the producer-keyed site for the active engine frame.
    ///
    /// Kept private so hosts cannot manufacture a site for an unrelated
    /// program authority.
    fn new(program: P, span: Span) -> Self {
        Self { program, span }
    }

    pub fn program(&self) -> &P {
        &self.program
    }

    pub fn span(&self) -> Span {
        self.span
    }
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

/// Lexical classification supplied with a named array-length lookup.
///
/// The engine owns precedence and the ordinary host may deliberately ignore
/// this durable-only fact while retaining its historical substitution input.
pub enum ComptimeArrayLengthBinding<V> {
    /// A lexical value, including non-integers. The host owns the semantic
    /// conversion; the engine must not discard the value before lookup.
    LocalValue(V),
    Shadowed,
    RuntimeDependent,
    Unbound,
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
                value_subst.remove(name);
            } else {
                value_subst.insert(name.clone(), val.clone());
                type_subst.remove(name);
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
        static PRODUCER_CALLS: RefCell<Vec<(usize, usize, u32)>> = const { RefCell::new(Vec::new()) };
        static INTEGER_HINTS: RefCell<Vec<Option<FakeType>>> = const { RefCell::new(Vec::new()) };
        static METHOD_FAILURES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        static TYPE_RESOLUTION_CALLS: Cell<usize> = const { Cell::new(0) };
        static CHECKPOINTS: Cell<usize> = const { Cell::new(0) };
        static ABORT_AT_CHECKPOINT: Cell<Option<usize>> = const { Cell::new(None) };
        static EVALUATE_RHS_AFTER_REJECTION: Cell<bool> = const { Cell::new(true) };
        static CALL_ARGUMENTS: RefCell<Vec<(FakeValue, bool)>> = const { RefCell::new(Vec::new()) };
        static BINDING_FINISHES: Cell<usize> = const { Cell::new(0) };
        static PREPARE_CALLS: Cell<usize> = const { Cell::new(0) };
        static ALLOW_MODULE_CALLS: Cell<bool> = const { Cell::new(false) };
        static EVALUATED_METHOD_RECEIVER_MODE: Cell<u8> = const { Cell::new(0) };
        static EVALUATED_METHOD_RECEIVERS: RefCell<Vec<FakeValue>> = const { RefCell::new(Vec::new()) };
        static EVALUATED_METHOD_EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        static EVALUATED_METHOD_ARGUMENT_CALLS: Cell<usize> = const { Cell::new(0) };
        static EVALUATED_METHOD_FAIL_ON_UNIT: Cell<bool> = const { Cell::new(false) };
        static REJECT_ADMISSION: Cell<bool> = const { Cell::new(false) };
        static REJECT_BIND_AT: Cell<Option<usize>> = const { Cell::new(None) };
        static NAMED_VALUE_CALLS: Cell<usize> = const { Cell::new(0) };
        static REJECT_VISIBILITY: Cell<bool> = const { Cell::new(false) };
        static NAMED_TYPE_MISSING: Cell<bool> = const { Cell::new(false) };
        static TYPE_VALUE_PROGRAMS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
        static ARRAY_LENGTH_CALLS: Cell<usize> = const { Cell::new(0) };
        static ARRAY_LENGTH_INPUTS: RefCell<Vec<Option<i128>>> = const { RefCell::new(Vec::new()) };
        static ARRAY_LENGTH_ABORT: Cell<bool> = const { Cell::new(false) };
        static ANON_STRUCT_CAPTURES: RefCell<Vec<(Vec<(u32, FakeType)>, Vec<(u32, FakeValue)>)>> =
            const { RefCell::new(Vec::new()) };
        static ANON_ENUM_CAPTURES: RefCell<Vec<(Vec<(u32, FakeType)>, Vec<(u32, FakeValue)>)>> =
            const { RefCell::new(Vec::new()) };
        static KEYED_FILE_RESOLUTION: Cell<bool> = const { Cell::new(false) };
        static FILE_RESOLUTION_CALLS: RefCell<Vec<(usize, u32)>> = const { RefCell::new(Vec::new()) };
        static TYPE_INTRINSIC_EVENTS: RefCell<Vec<(ComptimeTypeIntrinsic, FakeType)>> =
            const { RefCell::new(Vec::new()) };
        static TYPE_INTRINSIC_FAILURE: Cell<bool> = const { Cell::new(false) };
        static TYPE_INTRINSIC_ABORT: Cell<bool> = const { Cell::new(false) };
        static TYPE_INTRINSIC_NAME: RefCell<Option<(u32, &'static str)>> = const { RefCell::new(None) };
        static MATCH_PATTERN_MATCHES: Cell<bool> = const { Cell::new(false) };
        static MATCH_PATTERN_FORCE_FALSE: Cell<bool> = const { Cell::new(false) };
        static MATCH_NO_SELECTED_FAILURE: Cell<bool> = const { Cell::new(false) };
        static MATCH_NO_SELECTED_SITES: RefCell<Vec<(usize, u32, u32)>> =
            const { RefCell::new(Vec::new()) };
        static REJECTION_EVENTS: RefCell<Vec<ComptimeSemanticRejection<FakeValue>>> =
            const { RefCell::new(Vec::new()) };
        static REJECTION_SITES: RefCell<Vec<(usize, u32, u32)>> =
            const { RefCell::new(Vec::new()) };
        static MATCH_PATTERN_EVENTS: RefCell<Vec<ComptimeMatchPattern<FakeName>>> =
            const { RefCell::new(Vec::new()) };
        static MATCH_SYMBOL_CALLS: Cell<usize> = const { Cell::new(0) };
        static DIAGNOSTIC_SITES: RefCell<Vec<(usize, u32, u32)>> =
            const { RefCell::new(Vec::new()) };
    }

    #[test]
    fn entered_frame_runner_remains_engine_private() {
        let source = include_str!("comptime.rs");
        assert!(source.contains("pub(crate) fn evaluate_entered_frame("));
        let public_signature = ["pub", " fn evaluate_entered_frame("].concat();
        assert!(!source.contains(&public_signature));
    }

    #[test]
    fn semantic_pattern_decoder_uses_the_supplied_program_name_authority() {
        let mut editor = RirEditor::new();
        let unit = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let interner = lasso::ThreadedRodeo::new();
        let type_name = interner.get_or_intern("Os");
        let variant = interner.get_or_intern("Macos");
        let matched = editor
            .add_match(
                unit,
                &[(
                    rue_rir::RirPattern::Path {
                        module: None,
                        ctor_head: None,
                        type_name,
                        variant,
                        bindings: Vec::new(),
                        span: Span::new(0, 1),
                    },
                    unit,
                )],
                Span::new(0, 1),
            )
            .unwrap();
        let rir = editor.finish();
        let InstData::Match { arms, .. } = &rir.get(matched).data else {
            panic!("expected match instruction");
        };
        let (pattern, _) = rir.match_arms(arms).iter().next().unwrap();
        let first = decode_comptime_match_pattern(&pattern, |symbol| {
            format!("program-1-{}", symbol.issuing_interner_ordinal())
        });
        let second = decode_comptime_match_pattern(&pattern, |symbol| {
            format!("program-2-{}", symbol.issuing_interner_ordinal())
        });
        assert_ne!(first, second);
        assert!(matches!(
            first,
            ComptimeMatchPattern::Path {
                module_qualified: false,
                ctor_qualified: false,
                binding_count: 0,
                ..
            }
        ));
    }

    #[test]
    fn engine_decodes_match_patterns_lazily_per_active_program() {
        let interner = lasso::ThreadedRodeo::new();
        let type_name = interner.get_or_intern("Os");
        let variant = interner.get_or_intern("Macos");
        let later_type = interner.get_or_intern("Arch");
        let later_variant = interner.get_or_intern("X86_64");
        let make_program = || {
            let mut editor = RirEditor::new();
            let unit = editor.add_inst(Inst {
                data: InstData::UnitConst,
                span: Span::new(0, 1),
            });
            let root = editor
                .add_match(
                    unit,
                    &[
                        (
                            rue_rir::RirPattern::Path {
                                module: None,
                                ctor_head: None,
                                type_name,
                                variant,
                                bindings: Vec::new(),
                                span: Span::new(0, 1),
                            },
                            unit,
                        ),
                        (
                            rue_rir::RirPattern::Path {
                                module: None,
                                ctor_head: None,
                                type_name: later_type,
                                variant: later_variant,
                                bindings: vec![type_name],
                                span: Span::new(0, 1),
                            },
                            unit,
                        ),
                    ],
                    Span::new(0, 1),
                )
                .unwrap();
            (editor.finish(), root)
        };
        let (program0, root0) = make_program();
        let (program1, root1) = make_program();
        MATCH_PATTERN_MATCHES.with(|matches| matches.set(true));
        MATCH_PATTERN_EVENTS.with(|events| events.borrow_mut().clear());
        MATCH_SYMBOL_CALLS.with(|calls| calls.set(0));
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
            engine.evaluate(ComptimeFrame::expression(0, root0), &mut env),
            ComptimeOutcome::Known(FakeValue::Unit)
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(1, root1), &mut env),
            ComptimeOutcome::Known(FakeValue::Unit)
        ));
        let events = MATCH_PATTERN_EVENTS.with(|events| events.borrow().clone());
        assert_eq!(
            events.len(),
            2,
            "the later arm must not be decoded or offered"
        );
        assert_ne!(events[0], events[1], "active program must own symbol names");
        assert_eq!(MATCH_SYMBOL_CALLS.with(Cell::get), 4);
        MATCH_PATTERN_MATCHES.with(|matches| matches.set(false));
    }

    #[test]
    fn match_without_a_selected_arm_uses_the_host_terminal_policy() {
        let make_program = || {
            let mut editor = RirEditor::new();
            let scrutinee = editor.add_inst(Inst {
                data: InstData::BoolConst(false),
                span: Span::new(0, 1),
            });
            let body = editor.add_inst(Inst {
                data: InstData::UnitConst,
                span: Span::new(1, 2),
            });
            let root = editor
                .add_match(
                    scrutinee,
                    &[(rue_rir::RirPattern::Bool(true, Span::new(0, 1)), body)],
                    Span::new(0, 2),
                )
                .unwrap();
            (editor.finish(), root)
        };
        let (program0, root0) = make_program();
        let (program1, root1) = make_program();
        let mut host = FakeHost {
            programs: vec![program0, program1],
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
        MATCH_PATTERN_MATCHES.with(|matches| matches.set(true));
        MATCH_PATTERN_FORCE_FALSE.with(|force| force.set(true));
        MATCH_NO_SELECTED_FAILURE.with(|failure| failure.set(true));
        MATCH_NO_SELECTED_SITES.with(|sites| sites.borrow_mut().clear());
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root0), &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(1, root1), &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));
        MATCH_NO_SELECTED_SITES.with(|sites| {
            assert_eq!(sites.borrow().as_slice(), &[(0, 0, 2), (1, 0, 2)]);
        });

        MATCH_NO_SELECTED_FAILURE.with(|failure| failure.set(false));
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root0), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));

        MATCH_NO_SELECTED_FAILURE.with(|failure| failure.set(true));
        MATCH_NO_SELECTED_SITES.with(|sites| sites.borrow_mut().clear());
        MATCH_PATTERN_MATCHES.with(|matches| matches.set(false));
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root0), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        MATCH_NO_SELECTED_SITES.with(|sites| assert!(sites.borrow().is_empty()));

        MATCH_PATTERN_FORCE_FALSE.with(|force| force.set(false));
        MATCH_PATTERN_MATCHES.with(|matches| matches.set(false));
        MATCH_NO_SELECTED_FAILURE.with(|failure| failure.set(false));
    }

    #[test]
    fn semantic_rejections_are_emitted_by_real_engine_dispatch() {
        let mut editor = RirEditor::new();
        let unit = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let boolean = editor.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(1, 2),
        });
        let not_unit = editor.add_inst(Inst {
            data: InstData::Not { operand: unit },
            span: Span::new(2, 3),
        });
        let add_unit = editor.add_inst(Inst {
            data: InstData::Add {
                lhs: unit,
                rhs: boolean,
            },
            span: Span::new(3, 4),
        });
        let then_block = editor.add_block(&[unit], Span::new(4, 5)).unwrap();
        let branch_unit = editor.add_inst(Inst {
            data: InstData::Branch {
                cond: unit,
                then_block,
                else_block: None,
            },
            span: Span::new(5, 6),
        });
        let empty_block = editor.add_block(&[], Span::new(6, 7)).unwrap();
        let loop_unit = editor.add_inst(Inst {
            data: InstData::Loop {
                cond: boolean,
                body: unit,
            },
            span: Span::new(7, 8),
        });
        let assignment = editor.add_inst(Inst {
            data: InstData::Assign {
                name: lasso::Spur::default(),
                value: unit,
            },
            span: Span::new(8, 9),
        });
        let non_tail_assignment = editor
            .add_block(&[assignment, unit], Span::new(9, 10))
            .unwrap();
        let tail_assignment = editor
            .add_block(&[unit, assignment], Span::new(10, 11))
            .unwrap();
        let program = editor.finish();
        let mut host = FakeHost {
            programs: vec![program],
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
        REJECTION_EVENTS.with(|events| events.borrow_mut().clear());
        let mut engine = ComptimeEngine::new(&mut host);
        for root in [
            not_unit,
            add_unit,
            branch_unit,
            empty_block,
            loop_unit,
            non_tail_assignment,
            tail_assignment,
        ] {
            assert!(matches!(
                engine.evaluate(ComptimeFrame::expression(0, root), &mut env),
                ComptimeOutcome::RuntimeDependent
            ));
        }
        assert_eq!(
            REJECTION_EVENTS.with(|events| events.borrow().clone()),
            vec![
                ComptimeSemanticRejection::ConditionNotBoolean(FakeValue::Unit),
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation: ComptimeIntegerOperation::Add,
                    lhs: FakeValue::Unit,
                    rhs: Some(FakeValue::Boolean(true)),
                },
                ComptimeSemanticRejection::ConditionNotBoolean(FakeValue::Unit),
                ComptimeSemanticRejection::EmptyBlock,
                ComptimeSemanticRejection::UnsupportedExpression,
                ComptimeSemanticRejection::Assignment,
                ComptimeSemanticRejection::UnsupportedExpression,
            ]
        );
        configure_checkpoint_abort(None);
        configure_binary_rhs_policy(false);
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, add_unit), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        assert_eq!(checkpoint_count(), 2);
        configure_checkpoint_abort(Some(3));
        configure_binary_rhs_policy(true);
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, add_unit), &mut env),
            ComptimeOutcome::Abort(FakeFailure::Canceled)
        ));
        assert_eq!(checkpoint_count(), 3);
        configure_checkpoint_abort(None);
        configure_binary_rhs_policy(true);
    }

    #[test]
    fn semantic_rejection_sites_preserve_program_identity_for_colliding_spans() {
        let make_program = || {
            let mut editor = RirEditor::new();
            let unit = editor.add_inst(Inst {
                data: InstData::UnitConst,
                span: Span::new(40, 41),
            });
            let rejected = editor.add_inst(Inst {
                data: InstData::Neg { operand: unit },
                span: Span::new(40, 41),
            });
            (editor.finish(), rejected)
        };
        let (first, first_root) = make_program();
        let (second, second_root) = make_program();
        let mut host = FakeHost {
            programs: vec![first, second],
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
        REJECTION_SITES.with(|sites| sites.borrow_mut().clear());

        assert!(matches!(
            ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, first_root), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        assert!(matches!(
            ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(1, second_root), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        assert_eq!(
            REJECTION_SITES.with(|sites| sites.borrow().clone()),
            vec![(0, 40, 41), (1, 40, 41)]
        );
    }

    #[test]
    fn non_tail_assignment_restores_locals_before_rejection_and_reuse() {
        let mut editor = RirEditor::new();
        let unit = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let assignment = editor.add_inst(Inst {
            data: InstData::Assign {
                name: lasso::Spur::default(),
                value: unit,
            },
            span: Span::new(1, 2),
        });
        let allocation = editor
            .add_alloc(
                &[],
                Some(lasso::Spur::default()),
                false,
                None,
                unit,
                false,
                Span::new(0, 1),
            )
            .unwrap();
        let non_tail = editor
            .add_block(&[allocation, assignment, unit], Span::new(1, 3))
            .unwrap();
        let var = editor.add_inst(Inst {
            data: InstData::VarRef {
                name: lasso::Spur::default(),
                anchor: None,
            },
            span: Span::new(3, 4),
        });
        let program = editor.finish();
        let mut host = FakeHost {
            programs: vec![program],
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
        assert!(matches!(
            ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, non_tail), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        assert!(!env.locals.contains_key(&FakeName { ordinal: 0 }));
        NAMED_TYPE_MISSING.with(|missing| missing.set(true));
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, var), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        NAMED_TYPE_MISSING.with(|missing| missing.set(false));
    }

    #[test]
    fn unary_aggregate_and_unknown_type_intrinsic_use_real_rejection_dispatch() {
        let mut editor = RirEditor::new();
        let unit = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(20, 21),
        });
        let neg = editor.add_inst(Inst {
            data: InstData::Neg { operand: unit },
            span: Span::new(20, 21),
        });
        let bitnot = editor.add_inst(Inst {
            data: InstData::BitNot { operand: unit },
            span: Span::new(20, 21),
        });
        let typed = editor.add_inst(Inst {
            data: InstData::VarRef {
                name: lasso::Spur::default(),
                anchor: None,
            },
            span: Span::new(20, 21),
        });
        let typed_neg = editor.add_inst(Inst {
            data: InstData::Neg { operand: typed },
            span: Span::new(20, 21),
        });
        let typed_bitnot = editor.add_inst(Inst {
            data: InstData::BitNot { operand: typed },
            span: Span::new(20, 21),
        });
        let aggregate = editor
            .add_struct_init(
                None,
                None,
                lasso::Spur::default(),
                &[],
                None,
                Span::new(20, 21),
            )
            .unwrap();
        let type_arg = editor.add_unit_type().unwrap();
        let unknown_type_intrinsic = editor.add_inst(Inst {
            data: InstData::TypeIntrinsic {
                name: lasso::Spur::default(),
                type_arg,
            },
            span: Span::new(20, 21),
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
        env.locals.insert(
            FakeName { ordinal: 0 },
            FakeValue::TypedInteger(1, FakeType(99)),
        );
        REJECTION_EVENTS.with(|events| events.borrow_mut().clear());
        let mut engine = ComptimeEngine::new(&mut host);
        for root in [
            neg,
            bitnot,
            typed_neg,
            typed_bitnot,
            aggregate,
            unknown_type_intrinsic,
        ] {
            assert!(matches!(
                engine.evaluate(ComptimeFrame::expression(0, root), &mut env),
                ComptimeOutcome::RuntimeDependent
            ));
        }
        assert_eq!(
            REJECTION_EVENTS.with(|events| events.borrow().clone()),
            vec![
                ComptimeSemanticRejection::UnaryOperandNotInteger(FakeValue::Unit),
                ComptimeSemanticRejection::UnaryOperandNotInteger(FakeValue::Unit),
                ComptimeSemanticRejection::UnaryTypeNotInteger {
                    operation: ComptimeUnaryOperation::Neg,
                    value: FakeValue::TypedInteger(1, FakeType(99)),
                },
                ComptimeSemanticRejection::UnaryTypeNotInteger {
                    operation: ComptimeUnaryOperation::BitNot,
                    value: FakeValue::TypedInteger(1, FakeType(99)),
                },
                ComptimeSemanticRejection::AggregateExpression,
                ComptimeSemanticRejection::UnsupportedIntrinsic("type".to_owned()),
            ]
        );
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
        Canceled,
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

    struct FakeCallBinding {
        arguments: Vec<(FakeValue, bool)>,
    }

    struct FakeBoundCall {
        arguments: Vec<(FakeValue, bool)>,
    }

    impl super::structured_type_seal::Sealed for FakeStructuredSuspension {}
    impl ComptimeStructuredTypeSuspension for FakeStructuredSuspension {}

    struct FakeHost {
        programs: Vec<Rir>,
        type_symbol: SymbolHandle,
        constant: Option<(FakeFile, FakeName, FakeConstInfo)>,
        dependencies: Vec<(FakeFile, FakeName)>,
        call_plans: AHashMap<u32, FakePreparedCall>,
        recursive: Option<(usize, InstRef, InstRef, Option<usize>)>,
        enter_count: usize,
        finish_outcome: FakeFinishOutcome,
        finished: Vec<(usize, Option<FakeType>)>,
        float_evaluations: Cell<usize>,
    }

    #[derive(Clone)]
    struct FakeConstInfo {
        span: Span,
        value: Option<FakeValue>,
    }

    impl FakeHost {
        fn admits_durable_forms(&self) -> bool {
            matches!(self.finish_outcome, FakeFinishOutcome::Identity)
        }
    }

    fn configure_checkpoint_abort(abort_at: Option<usize>) {
        CHECKPOINTS.with(|count| count.set(0));
        ABORT_AT_CHECKPOINT.with(|configured| configured.set(abort_at));
    }

    fn configure_binary_rhs_policy(evaluate_rhs: bool) {
        EVALUATE_RHS_AFTER_REJECTION.with(|policy| policy.set(evaluate_rhs));
    }

    fn checkpoint_count() -> usize {
        CHECKPOINTS.with(Cell::get)
    }

    fn clear_call_argument_observations() {
        CALL_ARGUMENTS.with(|arguments| arguments.borrow_mut().clear());
        ALLOW_MODULE_CALLS.with(|allowed| allowed.set(false));
        REJECT_ADMISSION.with(|rejected| rejected.set(false));
        REJECT_BIND_AT.with(|rejected| rejected.set(None));
        BINDING_FINISHES.with(|count| count.set(0));
        PREPARE_CALLS.with(|count| count.set(0));
        EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.set(0));
        EVALUATED_METHOD_RECEIVERS.with(|receivers| receivers.borrow_mut().clear());
        EVALUATED_METHOD_EVENTS.with(|events| events.borrow_mut().clear());
        EVALUATED_METHOD_ARGUMENT_CALLS.with(|count| count.set(0));
        EVALUATED_METHOD_FAIL_ON_UNIT.with(|fail| fail.set(false));
    }

    fn clear_named_value_observations() {
        NAMED_VALUE_CALLS.with(|count| count.set(0));
        REJECT_VISIBILITY.with(|reject| reject.set(false));
        NAMED_TYPE_MISSING.with(|missing| missing.set(false));
    }

    #[test]
    fn named_array_length_classifies_lexical_bindings_before_global_lookup() {
        let name = FakeName { ordinal: 7 };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();

        assert!(matches!(
            ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
            ComptimeArrayLengthBinding::Unbound
        ));
        env.value_subst.insert(name.clone(), FakeValue::Integer(4));
        assert!(matches!(
            ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
            ComptimeArrayLengthBinding::LocalValue(FakeValue::Integer(4))
        ));
        env.value_subst
            .insert(name.clone(), FakeValue::Type(FakeType(1)));
        assert!(matches!(
            ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
            ComptimeArrayLengthBinding::LocalValue(FakeValue::Type(FakeType(1)))
        ));
        env.value_subst.clear();
        env.runtime_local_names.insert(name.clone());
        assert!(matches!(
            ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
            ComptimeArrayLengthBinding::RuntimeDependent
        ));
        env.runtime_local_names.clear();
        env.locals.insert(name.clone(), FakeValue::Integer(9));
        assert!(matches!(
            ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
            ComptimeArrayLengthBinding::LocalValue(FakeValue::Integer(9))
        ));
        env.locals
            .insert(name.clone(), FakeValue::Type(FakeType(2)));
        assert!(matches!(
            ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
            ComptimeArrayLengthBinding::LocalValue(FakeValue::Type(FakeType(2)))
        ));
    }

    #[test]
    fn named_array_length_dispatch_preserves_shadow_and_abort_channels() {
        let interner = lasso::ThreadedRodeo::new();
        let count_symbol = interner.get_or_intern("N");
        let type_symbol = interner.get_or_intern("T");
        let mut editor = rue_rir::RirEditor::new();
        let type_syntax = editor.add_named_type(type_symbol).unwrap();
        let element = editor.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: type_syntax,
            },
            span: Span::new(0, 1),
        });
        let array = editor.add_inst(rue_rir::Inst {
            data: InstData::ArrayRepeat {
                value: element,
                count: rue_rir::RepeatCount::Named(count_symbol),
            },
            span: Span::new(0, 2),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(type_symbol),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let name = FakeName {
            ordinal: count_symbol.into_usize() as u32,
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let eval = |host: &mut FakeHost,
                    env: &mut ComptimeEnv<
            '_,
            FakeValue,
            FakeType,
            FakeName,
            FakeFile,
            FakeIdentity,
        >| {
            ComptimeEngine::new(host).evaluate(ComptimeFrame::expression(0, array), env)
        };

        ARRAY_LENGTH_CALLS.with(|calls| calls.set(0));
        ARRAY_LENGTH_INPUTS.with(|inputs| inputs.borrow_mut().clear());
        assert!(matches!(
            eval(&mut host, &mut env),
            ComptimeOutcome::Known(_)
        ));
        assert_eq!(ARRAY_LENGTH_CALLS.with(Cell::get), 1);
        assert_eq!(
            ARRAY_LENGTH_INPUTS.with(|inputs| inputs.borrow().clone()),
            vec![None]
        );

        env.value_subst.insert(name.clone(), FakeValue::Integer(4));
        assert!(matches!(
            eval(&mut host, &mut env),
            ComptimeOutcome::Known(_)
        ));
        assert_eq!(
            ARRAY_LENGTH_INPUTS.with(|inputs| inputs.borrow().last().copied()),
            Some(Some(4))
        );

        env.value_subst.clear();
        env.locals
            .insert(name.clone(), FakeValue::Type(FakeType(3)));
        assert!(matches!(
            eval(&mut host, &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));

        env.locals.insert(name.clone(), FakeValue::Boolean(true));
        assert!(matches!(
            eval(&mut host, &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));

        env.locals.clear();
        env.runtime_local_names.insert(name.clone());
        assert!(matches!(
            eval(&mut host, &mut env),
            ComptimeOutcome::RuntimeDependent
        ));

        env.runtime_local_names.clear();
        env.runtime_binding_names.insert(name.clone());
        assert!(matches!(
            eval(&mut host, &mut env),
            ComptimeOutcome::RuntimeDependent
        ));

        env.runtime_binding_names.clear();
        env.value_subst.insert(name.clone(), FakeValue::Integer(6));
        env.runtime_binding_names.insert(name.clone());
        assert!(matches!(
            eval(&mut host, &mut env),
            ComptimeOutcome::Known(_)
        ));
        assert_eq!(
            ARRAY_LENGTH_INPUTS.with(|inputs| inputs.borrow().last().copied()),
            Some(Some(6))
        );

        env.runtime_binding_names.clear();
        env.value_subst.clear();
        env.type_subst.insert(name.clone(), FakeType(4));
        assert!(matches!(
            eval(&mut host, &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));

        env.type_subst.clear();
        ARRAY_LENGTH_ABORT.with(|abort| abort.set(true));
        assert!(matches!(
            eval(&mut host, &mut env),
            ComptimeOutcome::Abort(FAKE_FAILURE)
        ));
        ARRAY_LENGTH_ABORT.with(|abort| abort.set(false));
    }

    #[test]
    fn local_capture_substitution_removes_the_shadowed_opposite_map() {
        let name = FakeName { ordinal: 11 };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        env.type_subst.insert(name.clone(), FakeType(1));
        env.value_subst.insert(name.clone(), FakeValue::Integer(2));

        env.locals
            .insert(name.clone(), FakeValue::Type(FakeType(3)));
        let (types, values) = env.substs_with_locals();
        assert_eq!(types.get(&name), Some(&FakeType(3)));
        assert!(!values.contains_key(&name));

        env.locals.insert(name.clone(), FakeValue::Integer(4));
        let (types, values) = env.substs_with_locals();
        assert!(!types.contains_key(&name));
        assert_eq!(values.get(&name), Some(&FakeValue::Integer(4)));
    }

    #[test]
    fn anonymous_struct_and_enum_hooks_receive_disjoint_type_and_value_captures() {
        let mut struct_editor = rue_rir::RirEditor::new();
        let struct_root = struct_editor
            .add_anon_struct_type(
                &[],
                &[],
                rue_rir::RirStructuralAnchor::new(vec![
                    rue_rir::RirStructuralPathSegment::Statement(1),
                ]),
                Span::new(0, 1),
            )
            .unwrap();
        let mut enum_editor = rue_rir::RirEditor::new();
        let enum_root = enum_editor
            .add_anon_enum_type(
                &[],
                &[],
                rue_rir::RirStructuralAnchor::new(vec![
                    rue_rir::RirStructuralPathSegment::Statement(2),
                ]),
                Span::new(0, 1),
            )
            .unwrap();
        let mut host = FakeHost {
            programs: vec![struct_editor.finish(), enum_editor.finish()],
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
        let type_name = FakeName { ordinal: 4 };
        let value_name = FakeName { ordinal: 5 };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        env.canonical_identity = Some(FakeIdentity { token: 9 });
        env.type_subst.insert(type_name.clone(), FakeType(1));
        env.value_subst
            .insert(type_name.clone(), FakeValue::Integer(7));
        env.value_subst
            .insert(value_name.clone(), FakeValue::Integer(2));
        env.type_subst.insert(value_name.clone(), FakeType(8));
        env.locals
            .insert(type_name.clone(), FakeValue::Type(FakeType(3)));
        env.locals.insert(value_name.clone(), FakeValue::Integer(4));
        ANON_STRUCT_CAPTURES.with(|captures| captures.borrow_mut().clear());
        ANON_ENUM_CAPTURES.with(|captures| captures.borrow_mut().clear());

        assert!(matches!(
            ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, struct_root), &mut env,),
            ComptimeOutcome::Known(FakeValue::Type(FakeType(20)))
        ));
        assert!(matches!(
            ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(1, enum_root), &mut env,),
            ComptimeOutcome::Known(FakeValue::Type(FakeType(21)))
        ));
        ANON_STRUCT_CAPTURES.with(|captures| {
            assert_eq!(
                *captures.borrow(),
                vec![(vec![(4, FakeType(3))], vec![(5, FakeValue::Integer(4))])]
            );
        });
        ANON_ENUM_CAPTURES.with(|captures| {
            assert_eq!(
                *captures.borrow(),
                vec![(vec![(4, FakeType(3))], vec![(5, FakeValue::Integer(4))])]
            );
        });
    }

    fn clear_type_intrinsic_observations() {
        TYPE_INTRINSIC_EVENTS.with(|events| events.borrow_mut().clear());
        TYPE_INTRINSIC_FAILURE.with(|failure| failure.set(false));
        TYPE_INTRINSIC_ABORT.with(|abort| abort.set(false));
        TYPE_INTRINSIC_NAME.with(|name| *name.borrow_mut() = None);
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
        type CallBinding = FakeCallBinding;
        type BoundCall = FakeBoundCall;
        type CompletionTicket = usize;
        type StructuredTypeSuspension = FakeStructuredSuspension;
        fn check_canceled(&self) -> ComptimeHostResult<(), Self::Failure> {
            let checkpoint = CHECKPOINTS.with(|count| {
                let next = count.get() + 1;
                count.set(next);
                next
            });
            if ABORT_AT_CHECKPOINT.with(|abort_at| abort_at.get() == Some(checkpoint)) {
                return Err(ComptimeHostError::Abort(FakeFailure::Canceled));
            }
            Ok(())
        }
        fn program_rir(&self, program: &Self::ProgramKey) -> &Rir {
            &self.programs[*program]
        }
        fn name_from_symbol(&self, program: &Self::ProgramKey, symbol: SymbolHandle) -> Self::Name {
            if MATCH_PATTERN_MATCHES.with(Cell::get) {
                MATCH_SYMBOL_CALLS.with(|calls| calls.set(calls.get() + 1));
            }
            FakeName {
                ordinal: symbol.issuing_interner_ordinal() as u32 + (*program as u32) * 1000,
            }
        }
        fn display_name(&self, name: &Self::Name) -> String {
            if let Some((_, intrinsic)) = TYPE_INTRINSIC_NAME.with(|configured| {
                configured
                    .borrow()
                    .as_ref()
                    .copied()
                    .filter(|(ordinal, _)| *ordinal == name.ordinal)
            }) {
                return intrinsic.to_owned();
            }
            if name.ordinal == self.type_symbol.issuing_interner_ordinal() as u32 {
                "type".to_owned()
            } else if name.ordinal % 1000 == 0 {
                "import".to_owned()
            } else {
                format!("fake-name-{}", name.ordinal)
            }
        }
        fn file_for_program_span(&self, program: &Self::ProgramKey, span: &Span) -> Self::File {
            if KEYED_FILE_RESOLUTION.with(Cell::get) {
                let file = span.file_id.index() + (*program as u32) * 100;
                FILE_RESOLUTION_CALLS.with(|calls| {
                    calls.borrow_mut().push((*program, file));
                });
                return FakeFile { index: file };
            }
            FakeFile {
                index: span.file_id.index(),
            }
        }
        fn resolve_comptime_named_value(
            &mut self,
            file: Self::File,
            name: Self::Name,
            span: Span,
        ) -> ComptimeHostResult<ComptimeNamedValueResolution<Self::Value>, Self::Failure> {
            NAMED_VALUE_CALLS.with(|count| count.set(count.get() + 1));
            if EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.get() != 0) {
                EVALUATED_METHOD_EVENTS.with(|events| events.borrow_mut().push("receiver_eval"));
            }
            let info = self
                .constant
                .as_ref()
                .filter(|(constant_file, constant_name, _)| {
                    *constant_file == file && *constant_name == name
                })
                .map(|(_, _, info)| info.clone());
            if let Some(info) = info {
                let defining_file = FakeFile {
                    index: info.span.file_id.index(),
                };
                self.dependencies
                    .push((defining_file.clone(), name.clone()));
                if REJECT_VISIBILITY.with(Cell::get) {
                    return Err(FAKE_FAILURE.into());
                }
                return Ok(match info.value {
                    Some(value) => ComptimeNamedValueResolution::Known(value),
                    None => ComptimeNamedValueResolution::RuntimeDependent,
                });
            }
            let resolved = self.resolve_named_type_value(&0, name, span)?;
            Ok(match resolved {
                Some(ty) => ComptimeNamedValueResolution::Known(FakeValue::Type(ty)),
                None => ComptimeNamedValueResolution::Missing,
            })
        }
        fn match_pattern(
            &self,
            pattern: &ComptimeMatchPattern<Self::Name>,
            _value: &Self::Value,
        ) -> Option<bool> {
            if !MATCH_PATTERN_MATCHES.with(Cell::get) {
                return None;
            }
            MATCH_PATTERN_EVENTS.with(|events| events.borrow_mut().push(pattern.clone()));
            Some(!MATCH_PATTERN_FORCE_FALSE.with(Cell::get))
        }
        fn match_no_selected_arm(
            &self,
            site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            MATCH_NO_SELECTED_SITES.with(|sites| {
                sites
                    .borrow_mut()
                    .push((*site.program(), site.span().start, site.span().end));
            });
            if MATCH_NO_SELECTED_FAILURE.with(Cell::get) {
                ComptimeOutcome::HostFailure(FAKE_FAILURE)
            } else {
                ComptimeOutcome::RuntimeDependent
            }
        }
        fn reject_comptime_expression(
            &self,
            rejection: ComptimeSemanticRejection<Self::Value>,
            site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            REJECTION_EVENTS.with(|events| events.borrow_mut().push(rejection));
            REJECTION_SITES.with(|sites| {
                sites
                    .borrow_mut()
                    .push((*site.program(), site.span().start, site.span().end));
            });
            ComptimeOutcome::RuntimeDependent
        }
        fn evaluate_binary_rhs_after_rejection(&self) -> bool {
            EVALUATE_RHS_AFTER_REJECTION.with(Cell::get)
        }
        fn require_preview(
            &self,
            _feature: rue_error::PreviewFeature,
            _what: &str,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> ComptimeHostResult<(), Self::Failure> {
            Ok(())
        }
        fn depth_exceeded(
            &self,
            _name: &Self::Name,
            _depth: usize,
            site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> Self::Failure {
            DIAGNOSTIC_SITES.with(|sites| {
                sites
                    .borrow_mut()
                    .push((*site.program(), site.span().start, site.span().end))
            });
            FAKE_FAILURE
        }
        fn literal_out_of_range(
            &self,
            _value: u64,
            _ty: &Self::Type,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> Self::Failure {
            FAKE_FAILURE
        }
        fn float_not_implemented(
            &self,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> Self::Failure {
            self.float_evaluations.set(self.float_evaluations.get() + 1);
            FAKE_FAILURE
        }
        fn cannot_negate(
            &self,
            _ty: &Self::Type,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> Self::Failure {
            FAKE_FAILURE
        }
        fn unsupported_anon_method_type_param(
            &self,
            _method_name: &str,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> Self::Failure {
            METHOD_FAILURES.with(|failures| failures.borrow_mut().push("own_type"));
            FakeFailure::OwnComptimeTypeParameter
        }
        fn non_function_anon_method(
            &self,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> Self::Failure {
            METHOD_FAILURES.with(|failures| failures.borrow_mut().push("non_function"));
            FakeFailure::NonFunctionMethod
        }
        fn resolve_named_array_length(
            &mut self,
            _name: &Self::Name,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
            values: Option<&AHashMap<Self::Name, Self::Value>>,
            binding: ComptimeArrayLengthBinding<Self::Value>,
        ) -> ComptimeOutcome<u64, Self::Failure> {
            ARRAY_LENGTH_CALLS.with(|calls| calls.set(calls.get() + 1));
            ARRAY_LENGTH_INPUTS.with(|inputs| {
                inputs.borrow_mut().push(
                    values.and_then(|values| {
                        values.values().next().and_then(ComptimeValue::as_integer)
                    }),
                )
            });
            let shadowed = match &binding {
                ComptimeArrayLengthBinding::Shadowed => true,
                ComptimeArrayLengthBinding::LocalValue(value) => value.as_integer().is_none(),
                _ => false,
            };
            if shadowed {
                return ComptimeOutcome::HostFailure(FAKE_FAILURE);
            }
            if ARRAY_LENGTH_ABORT.with(Cell::get) {
                return ComptimeOutcome::Abort(FAKE_FAILURE);
            }
            if matches!(binding, ComptimeArrayLengthBinding::RuntimeDependent) {
                return ComptimeOutcome::RuntimeDependent;
            }
            ComptimeOutcome::Known(0)
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
            FakeType(8)
        }
        fn find_or_create_anon_struct(
            &mut self,
            _identity: Self::AnonymousIdentity,
            _fields: &[ComptimeField<Self::Name, Self::Type>],
            _sigs: &[ComptimeMethodDescriptor<Self::Name, Self::Type>],
            type_subst: &AHashMap<Self::Name, Self::Type>,
            value_subst: &AHashMap<Self::Name, Self::Value>,
        ) -> ComptimeHostResult<(Self::Type, bool), Self::Failure> {
            ANON_STRUCT_CAPTURES.with(|captures| {
                captures.borrow_mut().push((
                    type_subst
                        .iter()
                        .map(|(name, ty)| (name.ordinal, *ty))
                        .collect(),
                    value_subst
                        .iter()
                        .map(|(name, value)| (name.ordinal, value.clone()))
                        .collect(),
                ));
            });
            Ok((FakeType(20), true))
        }
        fn find_or_create_anon_enum(
            &mut self,
            _identity: Self::AnonymousIdentity,
            _names: &[String],
            _payloads: &[Vec<Self::Type>],
            type_subst: &AHashMap<Self::Name, Self::Type>,
            value_subst: &AHashMap<Self::Name, Self::Value>,
        ) -> ComptimeHostResult<Self::Type, Self::Failure> {
            ANON_ENUM_CAPTURES.with(|captures| {
                captures.borrow_mut().push((
                    type_subst
                        .iter()
                        .map(|(name, ty)| (name.ordinal, *ty))
                        .collect(),
                    value_subst
                        .iter()
                        .map(|(name, value)| (name.ordinal, value.clone()))
                        .collect(),
                ));
            });
            Ok(FakeType(21))
        }
        fn check_require_droppable(
            &mut self,
            _ty: Self::Type,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> ComptimeHostResult<(), Self::Failure> {
            Ok(())
        }
        fn check_trivially_droppable(
            &mut self,
            _ty: Self::Type,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
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
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
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
            site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
            DIAGNOSTIC_SITES.with(|sites| {
                sites
                    .borrow_mut()
                    .push((*site.program(), site.span().start, site.span().end))
            });
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
        fn resolve_comptime_type_intrinsic(
            &mut self,
            intrinsic: ComptimeTypeIntrinsic,
            ty: Self::Type,
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
            TYPE_INTRINSIC_EVENTS.with(|events| events.borrow_mut().push((intrinsic, ty)));
            if TYPE_INTRINSIC_ABORT.with(Cell::get) {
                return Err(ComptimeHostError::Abort(FAKE_FAILURE));
            }
            if TYPE_INTRINSIC_FAILURE.with(Cell::get) {
                return Err(ComptimeHostError::HostFailure(FAKE_FAILURE));
            }
            Ok(Some(match intrinsic {
                ComptimeTypeIntrinsic::IntegerBound(ComptimeIntegerBound::Max) => {
                    FakeValue::integer_typed(127, Some(ty))
                }
                ComptimeTypeIntrinsic::IntegerBound(ComptimeIntegerBound::Min) => {
                    FakeValue::integer_typed(-128, Some(ty))
                }
                ComptimeTypeIntrinsic::RequireDroppable
                | ComptimeTypeIntrinsic::RequireTriviallyDroppable => FakeValue::Unit,
            }))
        }
        fn resolve_named_type_value(
            &mut self,
            program: &Self::ProgramKey,
            _name: Self::Name,
            _span: Span,
        ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure> {
            TYPE_VALUE_PROGRAMS.with(|programs| programs.borrow_mut().push(*program));
            Ok((!NAMED_TYPE_MISSING.with(Cell::get)).then_some(FakeType(7)))
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
            method: Self::Name,
            _span: Span,
        ) -> ComptimeHostResult<Option<Self::Name>, Self::Failure> {
            Ok(ALLOW_MODULE_CALLS
                .with(|allowed| allowed.get())
                .then_some(method))
        }
        fn comptime_method_receiver_policy(&self) -> ComptimeMethodReceiverPolicy {
            EVALUATED_METHOD_RECEIVER_MODE.with(|mode| {
                if mode.get() == 0 {
                    ComptimeMethodReceiverPolicy::SyntacticModulePath
                } else {
                    ComptimeMethodReceiverPolicy::EvaluateReceiver
                }
            })
        }
        fn admit_evaluated_comptime_method(
            &mut self,
            receiver: Self::Value,
            method: Self::Name,
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
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
            _span: Span,
        ) -> ComptimeOutcome<
            Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
            Self::Failure,
        > {
            EVALUATED_METHOD_EVENTS.with(|events| events.borrow_mut().push("receiver_hook"));
            EVALUATED_METHOD_RECEIVERS
                .with(|receivers| receivers.borrow_mut().push(receiver.clone()));
            let mode = EVALUATED_METHOD_RECEIVER_MODE.with(Cell::get);
            if EVALUATED_METHOD_FAIL_ON_UNIT.with(Cell::get) && receiver == FakeValue::Unit {
                return ComptimeOutcome::HostFailure(FAKE_FAILURE);
            }
            match mode {
                1 => ComptimeOutcome::Known(Some(ComptimeCallAdmission {
                    name: FakeName {
                        ordinal: receiver
                            .as_type()
                            .map_or(method.ordinal, |ty| method.ordinal + ty.0 as u32),
                    },
                    payload: (),
                })),
                2 => ComptimeOutcome::Known(None),
                3 => ComptimeOutcome::RuntimeDependent,
                4 => ComptimeOutcome::NotReady,
                5 => ComptimeOutcome::UnsupportedContext,
                6 => ComptimeOutcome::Trap(ComptimeTrap {
                    operation: "receiver trap",
                    span: Span::new(0, 0),
                }),
                7 => ComptimeOutcome::HostFailure(FAKE_FAILURE),
                _ => ComptimeOutcome::Abort(FAKE_FAILURE),
            }
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
            if REJECT_ADMISSION.with(|rejected| rejected.get()) {
                return Ok(None);
            }
            Ok(Some(ComptimeCallAdmission { name, payload: () }))
        }
        fn begin_comptime_call_binding(
            &self,
            _admission: &ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
            _argument_count: usize,
            _span: Span,
        ) -> ComptimeHostResult<Self::CallBinding, Self::Failure> {
            Ok(FakeCallBinding {
                arguments: Vec::new(),
            })
        }
        fn bind_comptime_call_argument(
            &self,
            binding: &mut Self::CallBinding,
            argument: ComptimeCallArgument<Self::Value>,
            index: usize,
            _span: Span,
        ) -> ComptimeHostResult<bool, Self::Failure> {
            if REJECT_BIND_AT.with(|rejected| rejected.get() == Some(index)) {
                return Ok(false);
            }
            if EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.get() != 0) {
                EVALUATED_METHOD_EVENTS.with(|events| events.borrow_mut().push("argument"));
                EVALUATED_METHOD_ARGUMENT_CALLS.with(|count| count.set(count.get() + 1));
            }
            binding
                .arguments
                .push((argument.value().clone(), argument.is_direct_unit_literal()));
            Ok(true)
        }
        fn finish_comptime_call_binding(
            &mut self,
            _binding: Self::CallBinding,
            _span: Span,
        ) -> ComptimeHostResult<Option<Self::BoundCall>, Self::Failure> {
            BINDING_FINISHES.with(|count| count.set(count.get() + 1));
            let arguments = _binding.arguments;
            CALL_ARGUMENTS.with(|observed| observed.borrow_mut().extend(arguments.iter().cloned()));
            Ok(Some(FakeBoundCall { arguments }))
        }
        fn prepare_comptime_call(
            &mut self,
            admission: ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
            bound: Self::BoundCall,
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
            PREPARE_CALLS.with(|count| count.set(count.get() + 1));
            let _bound_argument_count = bound.arguments.len();
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
            program: &Self::ProgramKey,
            ticket: &Self::CompletionTicket,
            name: Self::Name,
            _types: &AHashMap<Self::Name, Self::Type>,
            _values: &AHashMap<Self::Name, Self::Value>,
            _span: Span,
        ) -> ComptimeHostResult<Self::CanonicalIdentity, Self::Failure> {
            PRODUCER_CALLS.with(|calls| {
                calls.borrow_mut().push((*program, *ticket, name.ordinal));
            });
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
            TYPE_INTRINSIC_NAME
                .with(|configured| configured.borrow().is_some())
                .then_some(FakeType(7))
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
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
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
            _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
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
            if TYPE_INTRINSIC_NAME.with(|configured| configured.borrow().is_some()) {
                return ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(FakeType(
                    7,
                )));
            }
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
    fn type_intrinsic_hook_receives_typed_bound_and_preserves_failure_channel() {
        clear_type_intrinsic_observations();
        let interner = lasso::ThreadedRodeo::new();
        let intrinsic_name = interner.get_or_intern("int_max");
        let mut editor = rue_rir::RirEditor::new();
        let type_arg = editor.add_unit_type().expect("unit type syntax");
        let root = editor.add_inst(rue_rir::Inst {
            data: InstData::TypeIntrinsic {
                name: intrinsic_name,
                type_arg,
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
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        TYPE_INTRINSIC_NAME.with(|configured| {
            *configured.borrow_mut() = Some((
                SymbolHandle::new(intrinsic_name).issuing_interner_ordinal() as u32,
                "int_max",
            ));
        });
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
        assert!(
            matches!(
                result,
                ComptimeOutcome::Known(FakeValue::TypedInteger(127, FakeType(7)))
            ),
            "unexpected type-intrinsic result: {result:?}"
        );
        assert_eq!(
            TYPE_INTRINSIC_EVENTS.with(|events| events.borrow().clone()),
            vec![(
                ComptimeTypeIntrinsic::IntegerBound(ComptimeIntegerBound::Max),
                FakeType(7),
            )]
        );

        TYPE_INTRINSIC_FAILURE.with(|failure| failure.set(true));
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::HostFailure(FakeFailure::Generic)
        ));

        TYPE_INTRINSIC_FAILURE.with(|failure| failure.set(false));
        TYPE_INTRINSIC_ABORT.with(|abort| abort.set(true));
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Abort(FakeFailure::Generic)
        ));
        clear_type_intrinsic_observations();
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
    fn cancellation_checkpoints_abort_only_entered_block_branch_nodes() {
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
        let then_block = editor.add_block(&[then_value], Span::new(0, 0)).unwrap();
        let else_block = editor.add_block(&[else_value], Span::new(0, 0)).unwrap();
        let branch = editor.add_inst(rue_rir::Inst {
            data: InstData::Branch {
                cond: condition,
                then_block,
                else_block: Some(else_block),
            },
            span: Span::new(0, 0),
        });
        let sibling = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(11),
            span: Span::new(0, 0),
        });
        let root = editor
            .add_block(&[branch, sibling], Span::new(0, 0))
            .unwrap();
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
        // root block, branch, condition, and selected block are entered; the
        // selected value is the first node rejected by this checkpoint.
        configure_checkpoint_abort(Some(5));
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Abort(FakeFailure::Canceled)
        ));
        assert_eq!(checkpoint_count(), 5);
        configure_checkpoint_abort(None);
    }

    #[test]
    fn cancellation_abort_in_entered_frame_finishes_and_cleans_up() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("f");
        let symbol_handle = SymbolHandle::new(symbol);
        let mut root_editor = rue_rir::RirEditor::new();
        let call = root_editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let after = root_editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(20),
            span: Span::new(0, 0),
        });
        let mut child_editor = rue_rir::RirEditor::new();
        let child_body = child_editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let base = symbol_handle.issuing_interner_ordinal() as u32;
        let mut call_plans = AHashMap::new();
        call_plans.insert(
            base,
            FakePreparedCall::Enter {
                program: 1,
                body: child_body,
                expected: None,
                name_bindings: AHashMap::new(),
            },
        );
        let mut host = FakeHost {
            programs: vec![root_editor.finish(), child_editor.finish()],
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
        LABEL_CALLS.with(|calls| calls.set(0));
        TICKET_EVENTS.with(|events| events.borrow_mut().clear());
        configure_checkpoint_abort(Some(2));
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let aborted =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
        assert!(matches!(
            aborted,
            ComptimeOutcome::Abort(FakeFailure::Canceled)
        ));
        assert_eq!(host.finished.len(), 1);
        assert_eq!(host.finished[0].0, 1);
        TICKET_EVENTS.with(|events| {
            assert_eq!(*events.borrow(), vec![(1, true), (1, false)]);
        });
        assert_eq!(LABEL_CALLS.with(Cell::get), 0);
        configure_checkpoint_abort(None);
        let resumed =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, after), &mut env);
        assert!(matches!(
            resumed,
            ComptimeOutcome::Known(FakeValue::Integer(20))
        ));
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
        let mut second_editor = rue_rir::RirEditor::new();
        let second_type_const = second_editor.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
            },
            span: Span::new(0, 0),
        });
        let interner = lasso::ThreadedRodeo::new();
        let type_symbol = SymbolHandle::new(interner.get_or_intern("T"));
        let mut host = FakeHost {
            programs: vec![editor.finish(), second_editor.finish()],
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
        clear_named_value_observations();
        TYPE_VALUE_PROGRAMS.with(|programs| programs.borrow_mut().clear());
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, type_const), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap()
            .unwrap();
        let second_value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(1, second_type_const), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap()
            .unwrap();
        assert_eq!(value, FakeValue::Type(FakeType(7)));
        assert_eq!(second_value, FakeValue::Type(FakeType(7)));
        assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 0);
        TYPE_VALUE_PROGRAMS.with(|programs| assert_eq!(*programs.borrow(), vec![0, 1]));
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
                FakeConstInfo {
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
        clear_named_value_observations();
        env.runtime_binding_names.insert(name);
        let value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, reference), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap();
        assert_eq!(value, None);
        assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 0);
    }

    #[test]
    fn file_resolution_is_keyed_by_the_active_program() {
        let interner = lasso::ThreadedRodeo::new();
        let name = interner.get_or_intern("value");
        let mut first = rue_rir::RirEditor::new();
        let first_reference = first.add_inst(rue_rir::Inst {
            data: InstData::VarRef { name, anchor: None },
            span: Span::with_file(rue_span::FileId::new(7), 0, 1),
        });
        let mut second = rue_rir::RirEditor::new();
        let second_reference = second.add_inst(rue_rir::Inst {
            data: InstData::VarRef { name, anchor: None },
            span: Span::with_file(rue_span::FileId::new(7), 0, 1),
        });
        let mut host = FakeHost {
            programs: vec![first.finish(), second.finish()],
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
        KEYED_FILE_RESOLUTION.with(|enabled| enabled.set(true));
        NAMED_TYPE_MISSING.with(|missing| missing.set(true));
        FILE_RESOLUTION_CALLS.with(|calls| calls.borrow_mut().clear());
        let mut engine = ComptimeEngine::new(&mut host);
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, first_reference), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(1, second_reference), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        FILE_RESOLUTION_CALLS.with(|calls| {
            assert_eq!(*calls.borrow(), vec![(0, 7), (1, 107)]);
        });
        KEYED_FILE_RESOLUTION.with(|enabled| enabled.set(false));
        NAMED_TYPE_MISSING.with(|missing| missing.set(false));
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
                FakeConstInfo {
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
        clear_named_value_observations();
        let value = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, reference), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap();
        assert_eq!(value, Some(FakeValue::Integer(42)));
        assert_eq!(
            host.dependencies,
            vec![(FakeFile { index: 9 }, FakeName { ordinal: 0 })]
        );
        assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 1);
    }

    #[test]
    fn atomic_named_value_hook_preserves_states_dependency_order_and_visibility() {
        let interner = lasso::ThreadedRodeo::new();
        let name = FakeName { ordinal: 1 };
        let mut host = FakeHost {
            programs: Vec::new(),
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: Some((
                FakeFile { index: 3 },
                name.clone(),
                FakeConstInfo {
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
        clear_named_value_observations();
        let file = FakeFile { index: 3 };
        let known = host
            .resolve_comptime_named_value(file.clone(), name.clone(), Span::new(0, 1))
            .unwrap();
        assert!(matches!(
            known,
            ComptimeNamedValueResolution::Known(FakeValue::Integer(42))
        ));
        host.constant.as_mut().unwrap().2.value = None;
        let runtime_dependent = host
            .resolve_comptime_named_value(file.clone(), name.clone(), Span::new(0, 1))
            .unwrap();
        assert!(matches!(
            runtime_dependent,
            ComptimeNamedValueResolution::RuntimeDependent
        ));
        host.constant = None;
        NAMED_TYPE_MISSING.with(|missing| missing.set(true));
        let missing = host
            .resolve_comptime_named_value(file.clone(), name.clone(), Span::new(0, 1))
            .unwrap();
        assert!(matches!(missing, ComptimeNamedValueResolution::Missing));
        assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 3);
        assert_eq!(
            host.dependencies,
            vec![
                (FakeFile { index: 9 }, name.clone()),
                (FakeFile { index: 9 }, name.clone()),
            ]
        );

        host.constant = Some((
            file,
            name.clone(),
            FakeConstInfo {
                span: Span::with_file(rue_span::FileId::new(9), 10, 11),
                value: Some(FakeValue::Integer(7)),
            },
        ));
        REJECT_VISIBILITY.with(|reject| reject.set(true));
        assert!(
            host.resolve_comptime_named_value(FakeFile { index: 3 }, name, Span::new(0, 1))
                .is_err()
        );
        assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 4);
        assert_eq!(host.dependencies.len(), 3);
        clear_named_value_observations();
    }

    #[test]
    fn earlier_terminal_skips_atomic_named_value_hook_and_later_sibling() {
        let interner = lasso::ThreadedRodeo::new();
        let mut editor = rue_rir::RirEditor::new();
        let terminal = editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("1.0"),
            },
            span: Span::new(0, 3),
        });
        let later = editor.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: interner.get_or_intern("later"),
                anchor: None,
            },
            span: Span::new(0, 8),
        });
        let block = editor
            .add_block(&[terminal, later], Span::new(0, 8))
            .unwrap();
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
        clear_named_value_observations();
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, block), &mut env);
        assert!(matches!(result, ComptimeOutcome::HostFailure(FAKE_FAILURE)));
        assert_eq!(host.float_evaluations.get(), 1);
        assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 0);
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
    fn call_argument_provenance_is_left_to_right_and_engine_owned() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("f");
        let symbol_handle = SymbolHandle::new(symbol);
        let mut editor = rue_rir::RirEditor::new();
        let direct_unit = editor.add_inst(rue_rir::Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });
        let wrapped_unit = editor.add_inst(rue_rir::Inst {
            data: InstData::Comptime { expr: direct_unit },
            span: Span::new(0, 2),
        });
        let call = editor
            .add_call(
                symbol,
                &[
                    rue_rir::RirCallArg {
                        value: direct_unit,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                    rue_rir::RirCallArg {
                        value: wrapped_unit,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                ],
                Span::new(0, 2),
            )
            .unwrap();
        let mut child = rue_rir::RirEditor::new();
        let child_body = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let base = symbol_handle.issuing_interner_ordinal() as u32;
        let mut call_plans = AHashMap::new();
        call_plans.insert(
            base,
            FakePreparedCall::Enter {
                program: 1,
                body: child_body,
                expected: None,
                name_bindings: AHashMap::new(),
            },
        );
        let mut host = FakeHost {
            programs: vec![editor.finish(), child.finish()],
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
        clear_call_argument_observations();
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Known(FakeValue::Integer(1))
        ));
        CALL_ARGUMENTS.with(|observed| {
            assert_eq!(
                *observed.borrow(),
                vec![(FakeValue::Unit, true), (FakeValue::Unit, false)]
            );
        });
    }

    #[test]
    fn qualified_call_uses_the_same_argument_provenance_helper() {
        let interner = lasso::ThreadedRodeo::new();
        let method = interner.get_or_intern("Id");
        let method_handle = SymbolHandle::new(method);
        let mut editor = rue_rir::RirEditor::new();
        let receiver = editor.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: interner.get_or_intern("lib"),
                anchor: None,
            },
            span: Span::new(0, 3),
        });
        let direct_unit = editor.add_inst(rue_rir::Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });
        let call = editor
            .add_method_call(
                receiver,
                method,
                &[rue_rir::RirCallArg {
                    value: direct_unit,
                    mode: rue_rir::RirArgMode::Normal,
                }],
                Span::new(0, 3),
            )
            .unwrap();
        let mut child = rue_rir::RirEditor::new();
        let child_body = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 0),
        });
        let method_ordinal = method_handle.issuing_interner_ordinal() as u32;
        let mut call_plans = AHashMap::new();
        call_plans.insert(
            method_ordinal,
            FakePreparedCall::Enter {
                program: 1,
                body: child_body,
                expected: None,
                name_bindings: AHashMap::new(),
            },
        );
        let mut host = FakeHost {
            programs: vec![editor.finish(), child.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans,
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        clear_call_argument_observations();
        ALLOW_MODULE_CALLS.with(|allowed| allowed.set(true));
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        env.defining_file = Some(FakeFile { index: 0 });
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Known(FakeValue::Integer(2))
        ));
        CALL_ARGUMENTS.with(|observed| {
            assert_eq!(*observed.borrow(), vec![(FakeValue::Unit, true)]);
        });
        clear_call_argument_observations();
    }

    #[test]
    fn evaluated_method_receiver_is_admitted_before_arguments_and_preserves_terminals() {
        let interner = lasso::ThreadedRodeo::new();
        let receiver_symbol = interner.get_or_intern("lib");
        let method_symbol = interner.get_or_intern("run");
        let receiver_handle = SymbolHandle::new(receiver_symbol);
        let method_handle = SymbolHandle::new(method_symbol);
        let inner_symbol = interner.get_or_intern("inner");
        let inner_handle = SymbolHandle::new(inner_symbol);
        let mut parent = rue_rir::RirEditor::new();
        let receiver = parent.add_inst(Inst {
            data: InstData::VarRef {
                name: receiver_symbol,
                anchor: None,
            },
            span: Span::new(0, 3),
        });
        let argument = parent.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(4, 6),
        });
        let call = parent
            .add_method_call(
                receiver,
                method_symbol,
                &[rue_rir::RirCallArg {
                    value: argument,
                    mode: rue_rir::RirArgMode::Normal,
                }],
                Span::new(0, 7),
            )
            .unwrap();
        let terminal_receiver = parent
            .add_call(inner_symbol, &[], Span::new(8, 13))
            .unwrap();
        let terminal_call = parent
            .add_method_call(
                terminal_receiver,
                method_symbol,
                &[rue_rir::RirCallArg {
                    value: argument,
                    mode: rue_rir::RirArgMode::Normal,
                }],
                Span::new(8, 20),
            )
            .unwrap();
        let non_module_receiver = parent.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(21, 22),
        });
        let non_module_call = parent
            .add_method_call(
                non_module_receiver,
                method_symbol,
                &[rue_rir::RirCallArg {
                    value: argument,
                    mode: rue_rir::RirArgMode::Normal,
                }],
                Span::new(21, 29),
            )
            .unwrap();
        let mut child = rue_rir::RirEditor::new();
        let child_body = child.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(10, 12),
        });
        let receiver_name = FakeName {
            ordinal: receiver_handle.issuing_interner_ordinal() as u32,
        };
        let method_name = method_handle.issuing_interner_ordinal() as u32;
        let selected_name = method_name + 7;
        let inner_name = inner_handle.issuing_interner_ordinal() as u32;
        let mut call_plans = AHashMap::new();
        call_plans.insert(
            selected_name,
            FakePreparedCall::Enter {
                program: 1,
                body: child_body,
                expected: None,
                name_bindings: AHashMap::new(),
            },
        );
        let mut host = FakeHost {
            programs: vec![parent.finish(), child.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: Some((
                FakeFile { index: 0 },
                receiver_name.clone(),
                FakeConstInfo {
                    span: Span::new(0, 3),
                    value: Some(FakeValue::Type(FakeType(7))),
                },
            )),
            dependencies: Vec::new(),
            call_plans,
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };

        // Ordinary hosts retain the path-only shortcut and do not evaluate the
        // receiver before module-path resolution.
        clear_call_argument_observations();
        clear_named_value_observations();
        host.dependencies.clear();
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        assert!(matches!(
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
        assert!(EVALUATED_METHOD_EVENTS.with(|events| events.borrow().is_empty()));
        assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 0);
        assert!(host.dependencies.is_empty());

        // Durable-style hosts evaluate even a syntactically decodable path.
        // The receiver token is retained by the admission hook, so the caller
        // cannot accidentally select a same-spelled callable in its own module.
        EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.set(1));
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Known(FakeValue::Integer(42))
        ));
        assert_eq!(
            EVALUATED_METHOD_EVENTS.with(|events| events.borrow().clone()),
            vec!["receiver_eval", "receiver_hook", "argument"]
        );
        assert_eq!(
            EVALUATED_METHOD_RECEIVERS.with(|receivers| receivers.borrow().clone()),
            vec![FakeValue::Type(FakeType(7))]
        );
        assert_eq!(
            EVALUATED_METHOD_ARGUMENT_CALLS.with(Cell::get),
            1,
            "arguments are evaluated only after receiver admission"
        );
        assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 1);
        assert_eq!(
            host.dependencies,
            vec![(FakeFile { index: 0 }, receiver_name.clone())]
        );

        // A known non-module receiver is rejected by its semantic value, and
        // never reaches ordinary argument binding or preparation.
        clear_call_argument_observations();
        EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.set(1));
        EVALUATED_METHOD_FAIL_ON_UNIT.with(|fail| fail.set(true));
        let non_module_result = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, non_module_call), &mut env);
        assert!(matches!(
            non_module_result,
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));
        assert_eq!(
            EVALUATED_METHOD_RECEIVERS.with(|receivers| receivers.borrow().clone()),
            vec![FakeValue::Unit]
        );
        assert_eq!(EVALUATED_METHOD_ARGUMENT_CALLS.with(Cell::get), 0);
        assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
        assert_eq!(PREPARE_CALLS.with(Cell::get), 0);

        // A receiver hook terminal propagates before argument evaluation.
        for mode in 2..=8 {
            clear_call_argument_observations();
            EVALUATED_METHOD_RECEIVER_MODE.with(|configured| configured.set(mode));
            let result = ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, call), &mut env);
            match mode {
                2 | 3 => assert!(matches!(result, ComptimeOutcome::RuntimeDependent)),
                4 => assert!(matches!(result, ComptimeOutcome::NotReady)),
                5 => assert!(matches!(result, ComptimeOutcome::UnsupportedContext)),
                6 => assert!(matches!(result, ComptimeOutcome::Trap(_))),
                7 => assert!(matches!(result, ComptimeOutcome::HostFailure(_))),
                8 => assert!(matches!(result, ComptimeOutcome::Abort(_))),
                _ => unreachable!(),
            }
            assert_eq!(
                EVALUATED_METHOD_EVENTS.with(|events| events.borrow().clone()),
                vec!["receiver_eval", "receiver_hook"]
            );
            assert_eq!(EVALUATED_METHOD_ARGUMENT_CALLS.with(Cell::get), 0);
        }

        // The same terminals must also propagate when they are genuinely
        // produced while evaluating the receiver, before the receiver hook is
        // reached. This covers the legacy receiver-evaluation ordering.
        for mode in 3..=8 {
            clear_call_argument_observations();
            EVALUATED_METHOD_RECEIVER_MODE.with(|configured| configured.set(1));
            let receiver_outcome = match mode {
                3 => ComptimeOutcome::RuntimeDependent,
                4 => ComptimeOutcome::NotReady,
                5 => ComptimeOutcome::UnsupportedContext,
                6 => ComptimeOutcome::Trap(ComptimeTrap {
                    operation: "receiver trap",
                    span: Span::new(0, 0),
                }),
                7 => ComptimeOutcome::HostFailure(FAKE_FAILURE),
                _ => ComptimeOutcome::Abort(FAKE_FAILURE),
            };
            host.call_plans
                .insert(inner_name, FakePreparedCall::Memoized(receiver_outcome));
            let result = ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, terminal_call), &mut env);
            match mode {
                3 => assert!(matches!(result, ComptimeOutcome::RuntimeDependent)),
                4 => assert!(matches!(result, ComptimeOutcome::NotReady)),
                5 => assert!(matches!(result, ComptimeOutcome::UnsupportedContext)),
                6 => assert!(matches!(result, ComptimeOutcome::Trap(_))),
                7 => assert!(matches!(result, ComptimeOutcome::HostFailure(_))),
                8 => assert!(matches!(result, ComptimeOutcome::Abort(_))),
                _ => unreachable!(),
            }
            assert!(EVALUATED_METHOD_EVENTS.with(|events| events.borrow().is_empty()));
            assert!(EVALUATED_METHOD_RECEIVERS.with(|receivers| receivers.borrow().is_empty()));
            assert_eq!(EVALUATED_METHOD_ARGUMENT_CALLS.with(Cell::get), 0);
        }
        clear_call_argument_observations();
        clear_named_value_observations();
        host.dependencies.clear();
    }

    #[test]
    fn argument_provenance_restores_parent_program_after_a_foreign_argument() {
        let interner = lasso::ThreadedRodeo::new();
        let outer_symbol = interner.get_or_intern("outer");
        let inner_symbol = interner.get_or_intern("inner");
        let outer_handle = SymbolHandle::new(outer_symbol);
        let inner_handle = SymbolHandle::new(inner_symbol);
        let mut parent = rue_rir::RirEditor::new();
        let inner_call = parent.add_call(inner_symbol, &[], Span::new(0, 0)).unwrap();
        let direct_unit = parent.add_inst(rue_rir::Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 0),
        });
        let outer_call = parent
            .add_call(
                outer_symbol,
                &[
                    rue_rir::RirCallArg {
                        value: inner_call,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                    rue_rir::RirCallArg {
                        value: direct_unit,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                ],
                Span::new(0, 0),
            )
            .unwrap();
        let mut inner_program = rue_rir::RirEditor::new();
        let inner_body = inner_program.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let mut outer_program = rue_rir::RirEditor::new();
        let outer_body = outer_program.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 0),
        });
        let outer_ordinal = outer_handle.issuing_interner_ordinal() as u32;
        let inner_ordinal = inner_handle.issuing_interner_ordinal() as u32;
        let mut call_plans = AHashMap::new();
        call_plans.insert(
            outer_ordinal,
            FakePreparedCall::Enter {
                program: 2,
                body: outer_body,
                expected: None,
                name_bindings: AHashMap::new(),
            },
        );
        call_plans.insert(
            inner_ordinal,
            FakePreparedCall::Enter {
                program: 1,
                body: inner_body,
                expected: None,
                name_bindings: AHashMap::new(),
            },
        );
        let mut host = FakeHost {
            programs: vec![
                parent.finish(),
                inner_program.finish(),
                outer_program.finish(),
            ],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans,
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        clear_call_argument_observations();
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, outer_call), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Known(FakeValue::Integer(2))
        ));
        CALL_ARGUMENTS.with(|observed| {
            assert_eq!(
                *observed.borrow(),
                vec![(FakeValue::Integer(1), false), (FakeValue::Unit, true)]
            );
        });
    }

    #[test]
    fn argument_checkpoint_abort_precedes_provenance_and_binding() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("f");
        let mut editor = rue_rir::RirEditor::new();
        let first = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let later = editor.add_inst(rue_rir::Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 0),
        });
        let call = editor
            .add_call(
                symbol,
                &[
                    rue_rir::RirCallArg {
                        value: first,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                    rue_rir::RirCallArg {
                        value: later,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                ],
                Span::new(0, 0),
            )
            .unwrap();
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
        clear_call_argument_observations();
        // Checkpoint 1 enters the call; checkpoint 2 is the first argument.
        // The abort must happen before that argument's provenance lookup,
        // binding, or the later argument's evaluation.
        configure_checkpoint_abort(Some(2));
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let mut engine = ComptimeEngine::new(&mut host);
        let result = engine.evaluate(ComptimeFrame::expression(0, call), &mut env);
        assert!(matches!(
            result,
            ComptimeOutcome::Abort(FakeFailure::Canceled)
        ));
        assert_eq!(checkpoint_count(), 2);
        assert_eq!(engine.provenance_classification_count(), 0);
        CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));
        assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
        assert_eq!(PREPARE_CALLS.with(Cell::get), 0);
        configure_checkpoint_abort(None);
    }

    #[test]
    fn incremental_binding_rejects_before_evaluating_later_arguments() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("f");
        let mut editor = rue_rir::RirEditor::new();
        let first = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(7),
            span: Span::new(0, 0),
        });
        let later_trap = editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("2.0"),
            },
            span: Span::new(0, 0),
        });
        let call = editor
            .add_call(
                symbol,
                &[
                    rue_rir::RirCallArg {
                        value: first,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                    rue_rir::RirCallArg {
                        value: later_trap,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                ],
                Span::new(0, 0),
            )
            .unwrap();
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
        clear_call_argument_observations();
        REJECT_BIND_AT.with(|rejected| rejected.set(Some(0)));
        configure_checkpoint_abort(None);
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
        assert!(matches!(result, ComptimeOutcome::RuntimeDependent));
        assert_eq!(host.float_evaluations.get(), 0);
        CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));
        assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
        assert_eq!(PREPARE_CALLS.with(Cell::get), 0);
        clear_call_argument_observations();
    }

    #[test]
    fn ordinary_binding_shape_mismatch_does_not_mask_later_terminal() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("f");
        let type_symbol = interner.get_or_intern("i32");
        let mut editor = rue_rir::RirEditor::new();
        let type_syntax = editor.add_named_type(type_symbol).unwrap();
        // A type value is deliberately supplied where an ordinary value
        // parameter would reject it. Ordinary binding stores that shape in
        // its owned transaction; the later terminal must still win before
        // finish performs whole-batch validation.
        let invalid_for_value = editor.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: type_syntax,
            },
            span: Span::new(0, 0),
        });
        let later_terminal = editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("2.0"),
            },
            span: Span::new(0, 0),
        });
        let call = editor
            .add_call(
                symbol,
                &[
                    rue_rir::RirCallArg {
                        value: invalid_for_value,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                    rue_rir::RirCallArg {
                        value: later_terminal,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                ],
                Span::new(0, 0),
            )
            .unwrap();
        let mut host = FakeHost {
            programs: vec![editor.finish()],
            type_symbol: SymbolHandle::new(type_symbol),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        clear_call_argument_observations();
        configure_checkpoint_abort(None);
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
        assert!(matches!(result, ComptimeOutcome::HostFailure(FAKE_FAILURE)));
        assert_eq!(host.float_evaluations.get(), 1);
        CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));
        assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
        assert_eq!(PREPARE_CALLS.with(Cell::get), 0);
    }

    #[test]
    fn admission_rejection_and_argument_terminal_stop_before_binding_or_later_args() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("f");
        let mut rejected_editor = rue_rir::RirEditor::new();
        let trapped = rejected_editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("1.0"),
            },
            span: Span::new(0, 3),
        });
        let rejected_call = rejected_editor
            .add_call(
                symbol,
                &[rue_rir::RirCallArg {
                    value: trapped,
                    mode: rue_rir::RirArgMode::Normal,
                }],
                Span::new(0, 3),
            )
            .unwrap();
        let mut rejected_host = FakeHost {
            programs: vec![rejected_editor.finish()],
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
        clear_call_argument_observations();
        REJECT_ADMISSION.with(|rejected| rejected.set(true));
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result = ComptimeEngine::new(&mut rejected_host)
            .evaluate(ComptimeFrame::expression(0, rejected_call), &mut env);
        assert!(matches!(result, ComptimeOutcome::RuntimeDependent));
        assert_eq!(rejected_host.float_evaluations.get(), 0);
        CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));

        let mut terminal_editor = rue_rir::RirEditor::new();
        let first = terminal_editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let terminal = terminal_editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("2.0"),
            },
            span: Span::new(0, 3),
        });
        let later = terminal_editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 0),
        });
        let terminal_call = terminal_editor
            .add_call(
                symbol,
                &[
                    rue_rir::RirCallArg {
                        value: first,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                    rue_rir::RirCallArg {
                        value: terminal,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                    rue_rir::RirCallArg {
                        value: later,
                        mode: rue_rir::RirArgMode::Normal,
                    },
                ],
                Span::new(0, 3),
            )
            .unwrap();
        let mut terminal_host = FakeHost {
            programs: vec![terminal_editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T2")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Identity,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        clear_call_argument_observations();
        configure_checkpoint_abort(None);
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result = ComptimeEngine::new(&mut terminal_host)
            .evaluate(ComptimeFrame::expression(0, terminal_call), &mut env);
        assert!(matches!(result, ComptimeOutcome::HostFailure(FAKE_FAILURE)));
        assert_eq!(terminal_host.float_evaluations.get(), 1);
        assert_eq!(checkpoint_count(), 3);
        CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));
        assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
        assert_eq!(PREPARE_CALLS.with(Cell::get), 0);
    }

    #[test]
    fn entered_programs_switch_on_colliding_refs_and_resume_the_parent() {
        let (mut host, root, rhs, base) = call_fixture();
        PRODUCER_CALLS.with(|calls| calls.borrow_mut().clear());
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
        PRODUCER_CALLS.with(|calls| {
            assert_eq!(
                calls.borrow().as_slice(),
                &[(1, 1, base), (2, 2, base + 2000)]
            );
        });
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
        PRODUCER_CALLS.with(|calls| calls.borrow_mut().clear());
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
        PRODUCER_CALLS.with(|calls| {
            let calls = calls.borrow();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].0, 1);
            assert_eq!(calls[1].0, 1);
            assert_eq!(calls[0].1, 1);
            assert_eq!(calls[1].1, 2);
            assert_ne!(calls[0].2, calls[1].2);
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
    fn nested_child_and_parent_diagnostics_use_the_active_program_in_one_evaluation() {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("diagnostic_child");
        let symbol_handle = SymbolHandle::new(symbol);
        let base = symbol_handle.issuing_interner_ordinal() as u32;

        let mut root = RirEditor::new();
        let call = root.add_call(symbol, &[], Span::new(1, 2)).unwrap();
        let root_rhs = root.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(2, 3),
        });
        let root_add = root.add_inst(Inst {
            data: InstData::Add {
                lhs: call,
                rhs: root_rhs,
            },
            span: Span::new(3, 4),
        });

        let mut child = RirEditor::new();
        let child_lhs = child.add_inst(Inst {
            data: InstData::IntConst(4),
            span: Span::new(10, 11),
        });
        let child_rhs = child.add_inst(Inst {
            data: InstData::IntConst(5),
            span: Span::new(11, 12),
        });
        let child_add = child.add_inst(Inst {
            data: InstData::Add {
                lhs: child_lhs,
                rhs: child_rhs,
            },
            span: Span::new(12, 13),
        });
        let mut call_plans = AHashMap::new();
        call_plans.insert(
            base,
            FakePreparedCall::Enter {
                program: 1,
                body: child_add,
                expected: None,
                name_bindings: AHashMap::new(),
            },
        );
        let mut host = FakeHost {
            programs: vec![root.finish(), child.finish()],
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
        DIAGNOSTIC_SITES.with(|sites| sites.borrow_mut().clear());
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        assert!(matches!(
            ComptimeEngine::new(&mut host)
                .evaluate(ComptimeFrame::expression(0, root_add), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(11))
        ));
        DIAGNOSTIC_SITES.with(|sites| {
            let programs = sites
                .borrow()
                .iter()
                .map(|(program, _, _)| *program)
                .collect::<Vec<_>>();
            assert_eq!(programs, vec![1, 0]);
        });
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
        DIAGNOSTIC_SITES.with(|sites| sites.borrow_mut().clear());
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, root_call), &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));
        assert_eq!(host.enter_count, MAX_COMPTIME_CALL_DEPTH + 1);
        TICKET_EVENTS.with(|events| {
            assert_eq!(events.borrow().len(), MAX_COMPTIME_CALL_DEPTH * 2);
        });
        DIAGNOSTIC_SITES.with(|sites| {
            assert_eq!(
                sites.borrow().as_slice(),
                &[(1, 0, 0)],
                "depth rejection uses the rejected child program"
            );
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
        let (mut host, root, rhs, base) = call_fixture();
        host.finish_outcome = FakeFinishOutcome::CanonicalFailure;
        PRODUCER_CALLS.with(|calls| calls.borrow_mut().clear());
        TICKET_EVENTS.with(|events| events.borrow_mut().clear());
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let mut engine = ComptimeEngine::new(&mut host);
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, root), &mut env),
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        ));
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, rhs), &mut env),
            ComptimeOutcome::Known(FakeValue::Integer(2))
        ));
        drop(engine);
        assert!(host.finished.is_empty());
        TICKET_EVENTS.with(|events| assert!(events.borrow().is_empty()));
        PRODUCER_CALLS.with(|calls| {
            assert_eq!(calls.borrow().as_slice(), &[(1, 1, base)]);
        });
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

/// A match pattern decoded by the canonical AIR engine into semantic facts.
/// The compact RIR representation, including symbol handles and instruction
/// references used by qualified paths, never crosses the host boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComptimeMatchPattern<N> {
    Wildcard,
    Bool(bool),
    Integer(i128),
    Path {
        module_qualified: bool,
        ctor_qualified: bool,
        type_name: N,
        variant: N,
        binding_count: usize,
    },
}

/// A semantic reason why the canonical engine cannot reduce an expression.
/// The ordinary host maps every reason to runtime dependence; a durable host
/// can preserve the declaration-time failure associated with the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeIntegerOperation {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Gt,
    Le,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeUnaryOperation {
    Neg,
    BitNot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComptimeSemanticRejection<V> {
    ConditionNotBoolean(V),
    ArithmeticOperandNotInteger {
        operation: ComptimeIntegerOperation,
        lhs: V,
        rhs: Option<V>,
    },
    UnaryOperandNotInteger(V),
    UnaryTypeNotInteger {
        operation: ComptimeUnaryOperation,
        value: V,
    },
    Assignment,
    AggregateExpression,
    EmptyBlock,
    UnsupportedIntrinsic(String),
    UnsupportedExpression,
}

/// Decode one compact RIR pattern into semantic facts.  Callers supply the
/// owning-program symbol mapping; the pattern itself is never exposed beyond
/// this canonical decoder.
pub fn decode_comptime_match_pattern<N>(
    pattern: &rue_rir::RirPatternView<'_>,
    mut name_from_symbol: impl FnMut(SymbolHandle) -> N,
) -> ComptimeMatchPattern<N> {
    match pattern {
        rue_rir::RirPatternView::Wildcard(_) => ComptimeMatchPattern::Wildcard,
        rue_rir::RirPatternView::Bool(value, _) => ComptimeMatchPattern::Bool(*value),
        rue_rir::RirPatternView::Int {
            value, negative, ..
        } => ComptimeMatchPattern::Integer(if *negative {
            -(*value as i128)
        } else {
            *value as i128
        }),
        rue_rir::RirPatternView::Path {
            module,
            ctor_head,
            type_name,
            variant,
            bindings,
            ..
        } => ComptimeMatchPattern::Path {
            module_qualified: module.is_some(),
            ctor_qualified: ctor_head.is_some(),
            type_name: name_from_symbol((*type_name).into()),
            variant: name_from_symbol((*variant).into()),
            binding_count: bindings.len(),
        },
    }
}

/// An already-evaluated call argument together with the engine-derived fact
/// that its source node was an immediate `UnitConst`. The source instruction
/// and owning program remain engine-private; hosts receive only this semantic
/// provenance alongside the reduced value.
#[derive(Debug, Clone)]
pub struct ComptimeCallArgument<V> {
    value: V,
    direct_unit_literal: bool,
}

impl<V> ComptimeCallArgument<V> {
    pub fn value(&self) -> &V {
        &self.value
    }

    pub fn is_direct_unit_literal(&self) -> bool {
        self.direct_unit_literal
    }

    fn new(value: V, direct_unit_literal: bool) -> Self {
        Self {
            value,
            direct_unit_literal,
        }
    }
}

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
    /// Host-owned, non-replayable binding state. The engine creates one state
    /// immediately after admission and feeds it source-order arguments before
    /// evaluating the next child.
    type CallBinding;
    /// Opaque, host-owned completed binding. The engine does not reconstruct
    /// ordered arguments or couple preparation to a map representation.
    type BoundCall;
    /// Opaque host-owned completion state issued during ordered preparation.
    type CompletionTicket;
    /// The sole continuation representation accepted by the engine for a
    /// structured type reduction. This is sealed below to prevent a peer
    /// resolver state machine from being hidden behind the host boundary.
    type StructuredTypeSuspension: ComptimeStructuredTypeSuspension;
    /// Check the owning query's cancellation state before reading any RIR for
    /// an evaluation node. This is deliberately required so every host makes
    /// abort semantics explicit; the engine performs the checkpoint exactly
    /// once at the entry to `eval`.
    fn check_canceled(&self) -> ComptimeHostResult<(), Self::Failure>;
    fn program_rir(&self, program: &Self::ProgramKey) -> &Rir;
    fn name_from_symbol(&self, program: &Self::ProgramKey, symbol: SymbolHandle) -> Self::Name;
    fn display_name(&self, name: &Self::Name) -> String;
    fn file_for_program_span(&self, program: &Self::ProgramKey, span: &Span) -> Self::File;
    fn resolve_comptime_named_value(
        &mut self,
        file: Self::File,
        name: Self::Name,
        span: Span,
    ) -> ComptimeHostResult<ComptimeNamedValueResolution<Self::Value>, Self::Failure>;
    fn match_pattern(
        &self,
        pattern: &ComptimeMatchPattern<Self::Name>,
        value: &Self::Value,
    ) -> Option<bool>;
    /// Resolve the terminal policy when every reached match arm declined.
    /// Ordinary body evaluation remains runtime-dependent; durable hosts may
    /// preserve a declaration-time failure through this semantic hook.
    fn match_no_selected_arm(
        &self,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure>;
    fn reject_comptime_expression(
        &self,
        rejection: ComptimeSemanticRejection<Self::Value>,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure>;
    /// Whether a durable semantic host needs both source-order operands before
    /// validating an integer operation. Ordinary body evaluation short-circuits
    /// after a known invalid lhs; durable declaration evaluation preserves its
    /// historical evaluate-both-before-validation order.
    fn evaluate_binary_rhs_after_rejection(&self) -> bool;
    fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn depth_exceeded(
        &self,
        name: &Self::Name,
        depth: usize,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn literal_out_of_range(
        &self,
        value: u64,
        ty: &Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn float_not_implemented(
        &self,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn cannot_negate(
        &self,
        ty: &Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn unsupported_anon_method_type_param(
        &self,
        method_name: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn non_function_anon_method(
        &self,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn resolve_named_array_length(
        &mut self,
        name: &Self::Name,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        // The historical substitution view used by ordinary hosts. The
        // engine separately supplies `binding`, which classifies lexical
        // locals/runtime shadows before any host or global lookup.
        values: Option<&AHashMap<Self::Name, Self::Value>>,
        binding: ComptimeArrayLengthBinding<Self::Value>,
    ) -> ComptimeOutcome<u64, Self::Failure>;
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
        type_subst: &AHashMap<Self::Name, Self::Type>,
        value_subst: &AHashMap<Self::Name, Self::Value>,
    ) -> ComptimeHostResult<(Self::Type, bool), Self::Failure>;
    fn find_or_create_anon_enum(
        &mut self,
        identity: Self::AnonymousIdentity,
        names: &[String],
        payloads: &[Vec<Self::Type>],
        type_subst: &AHashMap<Self::Name, Self::Type>,
        value_subst: &AHashMap<Self::Name, Self::Value>,
    ) -> ComptimeHostResult<Self::Type, Self::Failure>;
    fn check_require_droppable(
        &mut self,
        ty: Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn check_trivially_droppable(
        &mut self,
        ty: Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn type_name(&self, ty: &Self::Type) -> String;
    fn type_is_unsigned(&self, ty: &Self::Type) -> bool;
    fn type_integer_semantics(&self, ty: &Self::Type) -> Option<IntegerType>;
    /// Resolve a classified type intrinsic after its type argument has been
    /// reduced. The default delegates to the ordinary ownership hooks and
    /// integer-bound behavior; durable hosts can override this one typed seam
    /// to preserve their immediate mismatch diagnostics.
    fn resolve_comptime_type_intrinsic(
        &mut self,
        intrinsic: ComptimeTypeIntrinsic,
        ty: Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        match intrinsic {
            ComptimeTypeIntrinsic::RequireDroppable => {
                self.check_require_droppable(ty, site)?;
                Ok(Some(Self::Value::unit()))
            }
            ComptimeTypeIntrinsic::RequireTriviallyDroppable => {
                self.check_trivially_droppable(ty, site)?;
                Ok(Some(Self::Value::unit()))
            }
            ComptimeTypeIntrinsic::IntegerBound(bound) => {
                let Some(integer) = self.type_integer_semantics(&ty) else {
                    return Ok(None);
                };
                let value = match bound {
                    ComptimeIntegerBound::Max => integer.max_i128(),
                    ComptimeIntegerBound::Min => integer.min_i128(),
                };
                Ok(Some(Self::Value::integer_typed(value, Some(ty))))
            }
        }
    }
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
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
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
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
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
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }
    fn finish_arith(
        &self,
        result: CheckedIntegerResult,
        ty: Option<Self::Type>,
        op: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure>;
    fn resolve_named_type_value(
        &mut self,
        program: &Self::ProgramKey,
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
    fn comptime_method_receiver_policy(&self) -> ComptimeMethodReceiverPolicy {
        ComptimeMethodReceiverPolicy::SyntacticModulePath
    }
    /// Admit a method call after its receiver has been evaluated. The
    /// receiver remains in the host-owned admission payload, so a durable
    /// host cannot accidentally resolve the method against an unqualified
    /// spelling in the caller's module.
    fn admit_evaluated_comptime_method(
        &mut self,
        _receiver: Self::Value,
        _method: Self::Name,
        _arg_count: usize,
        _arg_modes: &[ComptimeArgMode],
        _env: &mut ComptimeEnv<
            '_,
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::CanonicalIdentity,
        >,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<
        Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
        Self::Failure,
    > {
        ComptimeOutcome::RuntimeDependent
    }
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
    fn begin_comptime_call_binding(
        &self,
        admission: &ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        argument_count: usize,
        span: Span,
    ) -> ComptimeHostResult<Self::CallBinding, Self::Failure>;
    /// Push one already-evaluated argument. `false` rejects the call as
    /// runtime-dependent and stops the engine before the next child runs.
    fn bind_comptime_call_argument(
        &self,
        binding: &mut Self::CallBinding,
        argument: ComptimeCallArgument<Self::Value>,
        index: usize,
        span: Span,
    ) -> ComptimeHostResult<bool, Self::Failure>;
    fn finish_comptime_call_binding(
        &mut self,
        binding: Self::CallBinding,
        span: Span,
    ) -> ComptimeHostResult<Option<Self::BoundCall>, Self::Failure>;
    fn prepare_comptime_call(
        &mut self,
        admission: ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        bound: Self::BoundCall,
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
        program: &Self::ProgramKey,
        ticket: &Self::CompletionTicket,
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
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
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
    #[cfg(test)]
    provenance_classifications: usize,
}

impl<'e, H: ComptimeHost> ComptimeEngine<'e, H> {
    pub fn new(host: &'e mut H) -> Self {
        Self {
            host,
            frames: Vec::new(),
            #[cfg(test)]
            provenance_classifications: 0,
        }
    }

    fn decode_match_pattern(
        &self,
        program: &H::ProgramKey,
        pattern: &rue_rir::RirPatternView<'_>,
    ) -> ComptimeMatchPattern<H::Name> {
        decode_comptime_match_pattern(pattern, |symbol| {
            self.host.name_from_symbol(program, symbol)
        })
    }

    fn classify_array_length_binding(
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        name: &H::Name,
    ) -> ComptimeArrayLengthBinding<H::Value> {
        if let Some(value) = env.locals.get(name) {
            return ComptimeArrayLengthBinding::LocalValue(value.clone());
        }
        if env.runtime_local_names.contains(name) {
            return ComptimeArrayLengthBinding::RuntimeDependent;
        }
        if env.type_subst.contains_key(name) {
            return ComptimeArrayLengthBinding::Shadowed;
        }
        if let Some(value) = env.value_subst.get(name) {
            return ComptimeArrayLengthBinding::LocalValue(value.clone());
        }
        if env.runtime_binding_names.contains(name) {
            return ComptimeArrayLengthBinding::RuntimeDependent;
        }
        ComptimeArrayLengthBinding::Unbound
    }

    #[cfg(test)]
    fn provenance_classification_count(&self) -> usize {
        self.provenance_classifications
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
                let site = ComptimeDiagnosticSite::new(program.clone(), method_span);
                return ComptimeOutcome::HostFailure(self.host.non_function_anon_method(&site));
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
                let site = ComptimeDiagnosticSite::new(program.clone(), method_span);
                return ComptimeOutcome::HostFailure(self.host.unsupported_anon_method_type_param(
                    &self.host.display_name(&method_name),
                    &site,
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

    fn diagnostic_site(&self, span: Span) -> ComptimeDiagnosticSite<H::ProgramKey> {
        ComptimeDiagnosticSite::new(self.program_key(), span)
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
        let mut binding = host_value!(self.host.begin_comptime_call_binding(
            &admission,
            args.len(),
            span,
        ));
        outcome_value!(self.evaluate_call_arguments(&args, env, &mut binding, span));
        let bound = host_value!(self.host.finish_comptime_call_binding(binding, span));
        let Some(bound) = bound else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let preparation = host_value!(self.host.prepare_comptime_call(admission, bound, span));
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

    /// Reduce call arguments in source order while retaining only the
    /// engine-derived provenance needed by semantic binding. Each child is
    /// reduced before its source node is inspected, while the owning program
    /// key is retained across foreign-frame evaluation.
    #[inline(never)]
    fn evaluate_call_arguments(
        &mut self,
        args: &[rue_rir::RirCallArg],
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        binding: &mut H::CallBinding,
        span: Span,
    ) -> ComptimeOutcome<(), H::Failure> {
        for (index, arg) in args.iter().enumerate() {
            let program = self.program_key();
            let value = outcome_value!(self.eval(arg.value, env));
            let direct_unit_literal = matches!(
                &self.host.program_rir(&program).get(arg.value).data,
                InstData::UnitConst
            );
            #[cfg(test)]
            {
                self.provenance_classifications += 1;
            }
            let accepted = host_value!(self.host.bind_comptime_call_argument(
                binding,
                ComptimeCallArgument::new(value, direct_unit_literal),
                index,
                span,
            ));
            if !accepted {
                return ComptimeOutcome::RuntimeDependent;
            }
        }
        ComptimeOutcome::Known(())
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

    /// Evaluate an owned frame admitted by this engine's host on the current
    /// engine stack. The caller must pass the exact non-replayable completion
    /// ticket returned with the frame by `prepare_comptime_call`; this entry
    /// point never creates a child engine or dispatches a peer RIR walker.
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
            let site = ComptimeDiagnosticSite::new(frame.program.clone(), frame.function_span);
            return ComptimeOutcome::HostFailure(self.host.depth_exceeded(
                frame.name.as_ref().expect("named frame"),
                MAX_COMPTIME_CALL_DEPTH,
                &site,
            ));
        }
        if let Some(name) = frame.name.clone() {
            let canonical_identity = host_value!(self.host.canonical_function_producer(
                &frame.program,
                &ticket,
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
        if matches!(
            self.host.comptime_method_receiver_policy(),
            ComptimeMethodReceiverPolicy::EvaluateReceiver
        ) {
            let receiver = outcome_value!(self.eval(receiver, env));
            let arg_modes: Vec<ComptimeArgMode> = args
                .iter()
                .map(|arg| (arg.mode, self.program_rir().get(arg.value).span))
                .collect();
            let admission = outcome_value!(self.host.admit_evaluated_comptime_method(
                receiver,
                method,
                args.len(),
                &arg_modes,
                env,
                &self.diagnostic_site(span),
                span,
            ));
            let Some(admission) = admission else {
                return ComptimeOutcome::RuntimeDependent;
            };
            return self.evaluate_admitted_call(admission, &args, env, span);
        }

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
        self.evaluate_admitted_call(admission, &args, env, span)
    }

    fn evaluate_admitted_call(
        &mut self,
        admission: ComptimeCallAdmission<H::CallAdmission, H::Name>,
        args: &[rue_rir::RirCallArg],
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let mut binding = host_value!(self.host.begin_comptime_call_binding(
            &admission,
            args.len(),
            span,
        ));
        outcome_value!(self.evaluate_call_arguments(args, env, &mut binding, span));
        let bound = host_value!(self.host.finish_comptime_call_binding(binding, span));
        let Some(bound) = bound else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let preparation = host_value!(self.host.prepare_comptime_call(admission, bound, span));
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
        operation: ComptimeIntegerOperation,
        lhs: InstRef,
        rhs: InstRef,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        span: Span,
    ) -> ComptimeOutcome<(H::Value, H::Value), H::Failure> {
        let l = match self.eval(lhs, env) {
            ComptimeOutcome::Known(value) => value,
            other => return Self::discard_rejection(other),
        };
        if l.as_integer().is_none() && !self.host.evaluate_binary_rhs_after_rejection() {
            return Self::discard_rejection(self.host.reject_comptime_expression(
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation,
                    lhs: l,
                    rhs: None,
                },
                &self.diagnostic_site(span),
            ));
        }
        let r = match self.eval(rhs, env) {
            ComptimeOutcome::Known(value) => value,
            other => return Self::discard_rejection(other),
        };
        if l.as_integer().is_none() || r.as_integer().is_none() {
            return Self::discard_rejection(self.host.reject_comptime_expression(
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation,
                    lhs: l,
                    rhs: Some(r),
                },
                &self.diagnostic_site(span),
            ));
        }
        ComptimeOutcome::Known((l, r))
    }

    fn integer_pair(values: &(H::Value, H::Value)) -> Option<(i128, i128)> {
        Some((values.0.as_integer()?, values.1.as_integer()?))
    }

    fn discard_rejection<T>(
        outcome: ComptimeOutcome<H::Value, H::Failure>,
    ) -> ComptimeOutcome<T, H::Failure> {
        match outcome {
            ComptimeOutcome::Known(_) => ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
            ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
            ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
            ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
        }
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
        let site = self.diagnostic_site(span);
        ComptimeOutcome::Known(host_value!(self.host.integer_operation_type(
            hint.as_ref(),
            lhs,
            rhs,
            &site,
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
        let site = self.diagnostic_site(span);
        ComptimeOutcome::Known(host_value!(self.host.unary_integer_type(
            hint.as_ref(),
            operand,
            &site,
        )))
    }

    fn finish_arith_value(
        &mut self,
        result: CheckedIntegerResult,
        ty: Option<H::Type>,
        op: &str,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let site = self.diagnostic_site(span);
        let Some(value) = host_value!(self.host.finish_arith(result, ty, op, &site)) else {
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
        host_value!(self.host.check_canceled());
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
            InstData::Block { instructions } => self.eval_block(instructions, env, span),
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
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let stmt_refs = self.program_rir().block_insts(&instructions).to_vec();
        if stmt_refs.is_empty() {
            return self.host.reject_comptime_expression(
                ComptimeSemanticRejection::EmptyBlock,
                &self.diagnostic_site(span),
            );
        }
        let saved_locals = env.locals.clone();
        let mut result = H::Value::unit();
        for (i, stmt_ref) in stmt_refs.iter().copied().enumerate() {
            let is_tail = i + 1 == stmt_refs.len();
            if !is_tail
                && matches!(
                    self.program_rir().get(stmt_ref).data,
                    InstData::Assign { .. }
                )
            {
                env.locals = saved_locals;
                return self.host.reject_comptime_expression(
                    ComptimeSemanticRejection::Assignment,
                    &self.diagnostic_site(self.program_rir().get(stmt_ref).span),
                );
            }
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
            ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                ComptimeSemanticRejection::ConditionNotBoolean(value),
                &self.diagnostic_site(self.program_rir().get(cond).span),
            ),
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
                        return ComptimeOutcome::HostFailure(self.host.literal_out_of_range(
                            *value,
                            ty,
                            &self.diagnostic_site(span),
                        ));
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
                    &self.diagnostic_site(span),
                ));
                ComptimeOutcome::HostFailure(
                    self.host.float_not_implemented(&self.diagnostic_site(span)),
                )
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
                            return ComptimeOutcome::HostFailure(
                                self.host.cannot_negate(ty, &self.diagnostic_site(span)),
                            );
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
                            let Some(n) = value.as_integer() else {
                                return self.host.reject_comptime_expression(
                                    ComptimeSemanticRejection::UnaryOperandNotInteger(value),
                                    &self.diagnostic_site(span),
                                );
                            };
                            let ty = outcome_value!(
                                self.unary_integer_type_for(env, inst_ref, &value, span,)
                            );
                            if let Some(ref ty) = ty {
                                if self.host.type_is_unsigned(ty) {
                                    return ComptimeOutcome::HostFailure(
                                        self.host.cannot_negate(ty, &self.diagnostic_site(span)),
                                    );
                                }
                            }
                            let result = match ty
                                .as_ref()
                                .and_then(|ty| self.host.type_integer_semantics(ty))
                            {
                                Some(integer) => integer.checked_neg_report_i128(n),
                                None if ty.is_some() => {
                                    return self.host.reject_comptime_expression(
                                        ComptimeSemanticRejection::UnaryTypeNotInteger {
                                            operation: ComptimeUnaryOperation::Neg,
                                            value,
                                        },
                                        &self.diagnostic_site(span),
                                    );
                                }
                                None => CheckedIntegerResult::from_raw(n.checked_neg()),
                            };
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
                        None => self.host.reject_comptime_expression(
                            ComptimeSemanticRejection::ConditionNotBoolean(value),
                            &self.diagnostic_site(span),
                        ),
                    },
                    // Can't logical-NOT an integer, type, or unit
                    other => other,
                }
            }

            // Binary arithmetic operations, checked at the operand type
            InstData::Add { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Add,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Sub,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Mul,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                let operands = outcome_value!(self.eval_int_operands(
                    if is_div {
                        ComptimeIntegerOperation::Div
                    } else {
                        ComptimeIntegerOperation::Mod
                    },
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                            _ => {
                                let site = self.diagnostic_site(span);
                                self.host.compare_comptime_values(&lhs, &rhs, true, &site)
                            }
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
                            _ => {
                                let site = self.diagnostic_site(span);
                                self.host.compare_comptime_values(&lhs, &rhs, false, &site)
                            }
                        }
                    }
                    other => other,
                }
            }
            InstData::Lt { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Lt,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Gt,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Le,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Ge,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                        ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                            ComptimeSemanticRejection::ConditionNotBoolean(value),
                            &self.diagnostic_site(span),
                        ),
                        other => other,
                    }
                }
                ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                    ComptimeSemanticRejection::ConditionNotBoolean(value),
                    &self.diagnostic_site(span),
                ),
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
                        ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                            ComptimeSemanticRejection::ConditionNotBoolean(value),
                            &self.diagnostic_site(span),
                        ),
                        other => other,
                    }
                }
                ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                    ComptimeSemanticRejection::ConditionNotBoolean(value),
                    &self.diagnostic_site(span),
                ),
                other => other,
            },

            // Bitwise operations. For values in range of their type these are
            // closed (no overflow possible), so no range check is needed.
            InstData::BitAnd { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::BitAnd,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::BitOr,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::BitXor,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                let operands = outcome_value!(self.eval_int_operands(
                    if is_shl {
                        ComptimeIntegerOperation::Shl
                    } else {
                        ComptimeIntegerOperation::Shr
                    },
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
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
                    return self.host.reject_comptime_expression(
                        ComptimeSemanticRejection::UnaryOperandNotInteger(n),
                        &self.diagnostic_site(span),
                    );
                };
                let ty = outcome_value!(self.unary_integer_type_for(env, inst_ref, &n, span,));
                let v = match ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                {
                    Some(integer) => integer.bitnot_i128(raw),
                    None if ty.is_some() => {
                        return self.host.reject_comptime_expression(
                            ComptimeSemanticRejection::UnaryTypeNotInteger {
                                operation: ComptimeUnaryOperation::BitNot,
                                value: n,
                            },
                            &self.diagnostic_site(span),
                        );
                    }
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
                    let semantic_pattern = self.decode_match_pattern(&self.program_key(), pattern);
                    match self.host.match_pattern(&semantic_pattern, &scrut) {
                        Some(true) => return self.eval(*body, env),
                        Some(false) => continue,
                        // Undecidable pattern (e.g. an enum-variant `Path`
                        // against a non-representable scrutinee): bail out.
                        None => return ComptimeOutcome::RuntimeDependent,
                    }
                }
                self.host.match_no_selected_arm(&self.diagnostic_site(span))
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
                    &local_type_subst,
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
                    &enum_type_subst,
                    &enum_value_subst,
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
                    if let Some(ty) = host_value!(self.host.resolve_named_type_value(
                        &self.program_key(),
                        type_symbol,
                        span,
                    )) {
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
                    let site = self.diagnostic_site(span);
                    return self.host.reject_non_type_array_repeat(value, &site);
                };
                let len = match count {
                    RepeatCount::Literal(n) => n,
                    RepeatCount::Named(sym) => {
                        let name = self.name_from_rir(sym.into());
                        let site = self.diagnostic_site(span);
                        let binding = Self::classify_array_length_binding(env, &name);
                        outcome_value!(self.host.resolve_named_array_length(
                            &name,
                            &site,
                            Some(&env.value_subst),
                            binding,
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
                // 6. File-level constants and named types are one atomic
                //    semantic lookup. The host owns direct dependency
                //    observation and visibility so durable adapters cannot
                //    split those effects across side channels.
                let program = self.program_key();
                let file = self.host.file_for_program_span(&program, &span);
                match host_value!(self.host.resolve_comptime_named_value(file, name, span)) {
                    ComptimeNamedValueResolution::Known(value) => ComptimeOutcome::Known(value),
                    ComptimeNamedValueResolution::RuntimeDependent
                    | ComptimeNamedValueResolution::Missing => ComptimeOutcome::RuntimeDependent,
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
                let Some(intrinsic) = ComptimeTypeIntrinsic::from_name(&gate) else {
                    return self.host.reject_comptime_expression(
                        ComptimeSemanticRejection::UnsupportedIntrinsic(gate),
                        &self.diagnostic_site(span),
                    );
                };
                // Both well-formedness gates reduce to unit at comptime:
                // `@require_droppable` (instantiation-time, rejects `linear`) and
                // `@require_trivially_droppable` (read-time, rejects drop glue —
                // RUE-651). Any other type intrinsic (`@size_of`/`@align_of`) is
                // not comptime-foldable here.
                // Resolve the element type through the enclosing comptime
                // substitutions (`T -> Inner` for `ArrayBuf(Inner)`); a
                // still-unresolved type parameter makes the gate non-evaluable
                // (it will be re-checked at a concrete instantiation).
                let intrinsic_ty = outcome_value!(self.evaluate_comptime_type_syntax(
                    &self.program_key(),
                    type_arg,
                    &env.type_subst,
                    &env.value_subst,
                    span,
                ));
                match host_value!(self.host.resolve_comptime_type_intrinsic(
                    intrinsic,
                    intrinsic_ty,
                    &self.diagnostic_site(span),
                )) {
                    Some(value) => ComptimeOutcome::Known(value),
                    None => ComptimeOutcome::RuntimeDependent,
                }
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

            InstData::StructInit { .. } | InstData::ArrayInit { .. } => {
                self.host.reject_comptime_expression(
                    ComptimeSemanticRejection::AggregateExpression,
                    &self.diagnostic_site(span),
                )
            }

            // Everything else requires runtime evaluation. The semantic
            // rejection hook lets durable hosts preserve the exact
            // declaration-time reason while ordinary evaluation remains
            // runtime-dependent.
            _ => self.host.reject_comptime_expression(
                ComptimeSemanticRejection::UnsupportedExpression,
                &self.diagnostic_site(span),
            ),
        }
    }
}
