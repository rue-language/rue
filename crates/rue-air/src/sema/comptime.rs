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
}

#[derive(Debug, Clone)]
pub struct ComptimeField<N, T> {
    pub name: N,
    pub ty: T,
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
    use rue_rir::{Inst, RirEditor, RirValidationContext};
    use std::cell::Cell;

    #[derive(Clone, Debug, PartialEq)]
    enum FakeValue {
        Integer(i128),
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
                Self::Integer(value) => Some(*value),
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

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeFailure;

    enum FakePreparedCall {
        Enter {
            program: usize,
            body: InstRef,
            expected: Option<FakeType>,
            name_bindings: AHashMap<FakeName, FakeName>,
        },
        Memoized(ComptimeOutcome<FakeValue, FakeFailure>),
    }

    #[derive(Clone, Copy)]
    enum FakeFinishOutcome {
        Identity,
        RuntimeDependent,
        NotReady,
        UnsupportedContext,
        Trap,
        HostFailure,
        Abort,
    }

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
        type AnonymousStructId = u32;
        type AnonMethodSigs = ();
        fn program_rir(&self, program: &Self::ProgramKey) -> &Rir {
            &self.programs[*program]
        }
        fn name_from_symbol(&self, program: &Self::ProgramKey, symbol: SymbolHandle) -> Self::Name {
            FakeName {
                ordinal: symbol.issuing_interner_ordinal() as u32 + (*program as u32) * 1000,
            }
        }
        fn display_name(&self, name: &Self::Name) -> String {
            format!("fake-name-{}", name.ordinal)
        }
        fn file_from_span(&self, span: &Span) -> Self::File {
            FakeFile {
                index: span.file_id.index(),
            }
        }
        fn value_const(
            &self,
            key: &(Self::File, Self::Name),
        ) -> Option<ComptimeConstInfo<Self::Value>> {
            self.constant
                .as_ref()
                .filter(|(file, name, _)| *file == key.0 && *name == key.1)
                .map(|(_, _, info)| info.clone())
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
        ) -> Result<(), Self::Failure> {
            Ok(())
        }
        fn depth_exceeded(&self, _name: &Self::Name, _depth: usize, _span: Span) -> Self::Failure {
            FakeFailure
        }
        fn literal_out_of_range(
            &self,
            _value: u64,
            _ty: &Self::Type,
            _span: Span,
        ) -> Self::Failure {
            FakeFailure
        }
        fn float_not_implemented(&self, _span: Span) -> Self::Failure {
            self.float_evaluations.set(self.float_evaluations.get() + 1);
            FakeFailure
        }
        fn cannot_negate(&self, _ty: &Self::Type, _span: Span) -> Self::Failure {
            FakeFailure
        }
        fn comptime_panic(&self, _reason: String, _span: Span) -> Self::Failure {
            FakeFailure
        }
        fn unsupported_anon_method_type_param(
            &self,
            _method_span: Span,
            _method_name: &str,
        ) -> Self::Failure {
            FakeFailure
        }
        fn record_value_const_dependency(&mut self, file: &Self::File, name: &Self::Name) {
            self.dependencies.push((file.clone(), name.clone()));
        }
        fn resolve_named_array_length(
            &mut self,
            _name: &Self::Name,
            _span: Span,
            _values: Option<&AHashMap<Self::Name, Self::Value>>,
        ) -> Result<u64, Self::Failure> {
            Ok(0)
        }
        fn rir_type_named_symbol(
            &self,
            _program: &Self::ProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
        ) -> Option<Self::Name> {
            Some(self.name_from_symbol(&0, self.type_symbol))
        }
        fn get_or_create_array_type(&mut self, _element: Self::Type, _length: u64) -> Self::Type {
            panic!("fake host does not construct array types")
        }
        fn extract_anon_method_sigs(
            &mut self,
            _program: &Self::ProgramKey,
            _methods: &rue_rir::RirAnonStructMethodsRange,
            _types: &AHashMap<Self::Name, Self::Type>,
            _values: &AHashMap<Self::Name, Self::Value>,
        ) -> Self::AnonMethodSigs {
            panic!("fake host does not construct anonymous methods")
        }
        fn find_method_own_comptime_type_param(
            &self,
            _program: &Self::ProgramKey,
            _methods: &rue_rir::RirAnonStructMethodsRange,
        ) -> Option<(Span, String)> {
            None
        }
        fn find_or_create_anon_struct(
            &mut self,
            _identity: Self::AnonymousIdentity,
            _fields: &[ComptimeField<Self::Name, Self::Type>],
            _sigs: &Self::AnonMethodSigs,
            _captured: &AHashMap<Self::Name, Self::Value>,
        ) -> Result<(Self::Type, bool), Self::Failure> {
            panic!("fake host does not construct anonymous structs")
        }
        fn find_or_create_anon_enum(
            &mut self,
            _identity: Self::AnonymousIdentity,
            _names: &[String],
            _payloads: &[Vec<Self::Type>],
        ) -> Result<Self::Type, Self::Failure> {
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
        ) -> Result<(), Self::Failure> {
            Ok(())
        }
        fn check_require_droppable(
            &mut self,
            _ty: Self::Type,
            _span: Span,
        ) -> Result<(), Self::Failure> {
            Ok(())
        }
        fn check_trivially_droppable(
            &mut self,
            _ty: Self::Type,
            _span: Span,
        ) -> Result<(), Self::Failure> {
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
        fn finish_arith(
            &self,
            result: CheckedIntegerResult,
            _ty: Option<Self::Type>,
            _op: &str,
            _span: Span,
        ) -> Result<Option<Self::Value>, Self::Failure> {
            Ok(result.checked().map(FakeValue::integer))
        }
        fn type_name(&self, ty: &Self::Type) -> String {
            format!("fake-type-{}", ty.0)
        }
        fn type_is_unsigned(&self, _ty: &Self::Type) -> bool {
            false
        }
        fn type_integer_semantics(&self, _ty: &Self::Type) -> Option<IntegerType> {
            None
        }
        fn record_named_type_dependency(&mut self, _ty: &Self::Type) {}
        fn resolve_named_type_value(
            &mut self,
            _name: Self::Name,
            _span: Span,
        ) -> Result<Option<Self::Type>, Self::Failure> {
            Ok(Some(FakeType(7)))
        }
        fn resolve_comptime_type_path(
            &mut self,
            _file: Self::File,
            _segments: &[Self::Name],
            _span: Span,
        ) -> Result<Option<Self::Value>, Self::Failure> {
            Ok(None)
        }
        fn resolve_module_comptime_callable(
            &mut self,
            _file_id: Self::File,
            _segments: &[Self::Name],
            _method: Self::Name,
            _span: Span,
        ) -> Result<Option<Self::Name>, Self::Failure> {
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
        ) -> Result<Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>, Self::Failure>
        {
            Ok(Some(ComptimeCallAdmission { name, payload: () }))
        }
        fn bind_comptime_call(
            &self,
            _admission: &ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
            _values: &[Self::Value],
            _span: Span,
        ) -> Result<
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
        ) -> Result<
            Option<
                ComptimeCallPreparation<
                    Self::Value,
                    Self::Type,
                    Self::Name,
                    Self::File,
                    Self::ProgramKey,
                    Self::CanonicalIdentity,
                    Self::Failure,
                >,
            >,
            Self::Failure,
        > {
            if let Some((max_enters, call_body, terminal_body, memoized_at)) = self.recursive {
                if memoized_at == Some(self.enter_count) {
                    return Ok(Some(ComptimeCallPreparation::Memoized(
                        ComptimeOutcome::Known(FakeValue::Integer(1)),
                    )));
                }
                self.enter_count += 1;
                let body = if self.enter_count == max_enters {
                    terminal_body
                } else {
                    call_body
                };
                return Ok(Some(ComptimeCallPreparation::Enter(ComptimeFrame {
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
                    expected_result: Some(FakeType(7)),
                })));
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
                } => ComptimeCallPreparation::Enter(ComptimeFrame {
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
                }),
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
            result: ComptimeOutcome<Self::Value, Self::Failure>,
        ) -> ComptimeOutcome<Self::Value, Self::Failure> {
            self.finished.push((frame.program, frame.expected_result));
            match self.finish_outcome {
                FakeFinishOutcome::Identity => result,
                FakeFinishOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
                FakeFinishOutcome::NotReady => ComptimeOutcome::NotReady,
                FakeFinishOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
                FakeFinishOutcome::Trap => ComptimeOutcome::Trap(ComptimeTrap {
                    operation: "fake trap",
                    span: Span::new(0, 0),
                }),
                FakeFinishOutcome::HostFailure => ComptimeOutcome::HostFailure(FakeFailure),
                FakeFinishOutcome::Abort => ComptimeOutcome::Abort(FakeFailure),
            }
        }
        fn label_ctor_instantiation_site(error: Self::Failure, _call_span: Span) -> Self::Failure {
            error
        }
        fn canonical_function_producer(
            &self,
            name: Self::Name,
            _types: &AHashMap<Self::Name, Self::Type>,
            _values: &AHashMap<Self::Name, Self::Value>,
            _span: Span,
        ) -> Result<Self::CanonicalIdentity, Self::Failure> {
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
            None
        }
        fn register_anon_struct_methods_for_comptime_with_subst(
            &mut self,
            _program: &Self::ProgramKey,
            _struct_id: &Self::AnonymousStructId,
            _struct_type: Self::Type,
            _methods: &rue_rir::RirAnonStructMethodsRange,
            _types: &AHashMap<Self::Name, Self::Type>,
            _values: &AHashMap<Self::Name, Self::Value>,
        ) -> Option<()> {
            None
        }
        fn set_anon_struct_type_subst(
            &mut self,
            _struct_id: &Self::AnonymousStructId,
            _subst: AHashMap<Self::Name, Self::Type>,
        ) {
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
            .into_result(|_| FakeFailure)
            .unwrap()
            .unwrap();
        assert_eq!(value, FakeValue::Integer(42));
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
            ComptimeOutcome::HostFailure(FakeFailure)
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
            .into_result(|_| FakeFailure)
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
            .into_result(|_| FakeFailure)
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
            .into_result(|_| FakeFailure)
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
            .into_result(|_| FakeFailure)
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
            .into_result(|_| FakeFailure)
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
                .into_result(|_| FakeFailure)
                .unwrap();
            // A second root evaluation proves the parent frame was popped
            // after the child program returned; no ambient program or stack
            // state leaks.
            let resumed = engine
                .evaluate(ComptimeFrame::expression(0, rhs), &mut env)
                .into_result(|_| FakeFailure)
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
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, root_call), &mut env),
            ComptimeOutcome::HostFailure(FakeFailure)
        ));
        assert_eq!(host.enter_count, MAX_COMPTIME_CALL_DEPTH + 1);

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
pub enum ComptimeCallPreparation<V, T, N, File, P, I, Failure> {
    /// A completed fact from the evaluation-local memo. This includes
    /// not-ready/runtime-dependent facts and therefore must not be confused
    /// with a cache miss.
    Memoized(ComptimeOutcome<V, Failure>),
    /// A cache miss represented by an owned foreign frame. The engine enters
    /// it and evaluates it; hosts never recursively dispatch its RIR.
    Enter(ComptimeFrame<V, T, N, File, P, I>),
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
            Err(error) => return ComptimeOutcome::HostFailure(error),
        }
    };
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
    type AnonymousStructId: Clone;
    type AnonMethodSigs;
    fn program_rir(&self, program: &Self::ProgramKey) -> &Rir;
    fn name_from_symbol(&self, program: &Self::ProgramKey, symbol: SymbolHandle) -> Self::Name;
    fn display_name(&self, name: &Self::Name) -> String;
    fn file_from_span(&self, span: &Span) -> Self::File;
    fn value_const(&self, key: &(Self::File, Self::Name))
    -> Option<ComptimeConstInfo<Self::Value>>;
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
    ) -> Result<(), Self::Failure>;
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
    fn record_value_const_dependency(&mut self, file: &Self::File, name: &Self::Name);
    fn resolve_named_array_length(
        &mut self,
        name: &Self::Name,
        span: Span,
        values: Option<&AHashMap<Self::Name, Self::Value>>,
    ) -> Result<u64, Self::Failure>;
    fn rir_type_named_symbol(
        &self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Option<Self::Name>;
    fn get_or_create_array_type(&mut self, element: Self::Type, length: u64) -> Self::Type;
    fn extract_anon_method_sigs(
        &mut self,
        program: &Self::ProgramKey,
        methods: &rue_rir::RirAnonStructMethodsRange,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
    ) -> Self::AnonMethodSigs;
    fn find_method_own_comptime_type_param(
        &self,
        program: &Self::ProgramKey,
        methods: &rue_rir::RirAnonStructMethodsRange,
    ) -> Option<(Span, String)>;
    fn find_or_create_anon_struct(
        &mut self,
        identity: Self::AnonymousIdentity,
        fields: &[ComptimeField<Self::Name, Self::Type>],
        sigs: &Self::AnonMethodSigs,
        captured: &AHashMap<Self::Name, Self::Value>,
    ) -> Result<(Self::Type, bool), Self::Failure>;
    fn find_or_create_anon_enum(
        &mut self,
        identity: Self::AnonymousIdentity,
        names: &[String],
        payloads: &[Vec<Self::Type>],
    ) -> Result<Self::Type, Self::Failure>;
    fn anonymous_struct_id(&self, ty: &Self::Type) -> Option<Self::AnonymousStructId>;
    fn has_method(&self, key: &Self::AnonymousStructId, method: Self::Name) -> bool;
    fn check_unqualified_visibility(
        &self,
        item_kind: &str,
        name: &Self::Name,
        defining_file_id: Self::File,
        is_pub: bool,
        span: Span,
    ) -> Result<(), Self::Failure>;
    fn check_require_droppable(&mut self, ty: Self::Type, span: Span) -> Result<(), Self::Failure>;
    fn check_trivially_droppable(
        &mut self,
        ty: Self::Type,
        span: Span,
    ) -> Result<(), Self::Failure>;
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
    fn finish_arith(
        &self,
        result: CheckedIntegerResult,
        ty: Option<Self::Type>,
        op: &str,
        span: Span,
    ) -> Result<Option<Self::Value>, Self::Failure>;
    fn resolve_named_type_value(
        &mut self,
        _name: Self::Name,
        span: Span,
    ) -> Result<Option<Self::Type>, Self::Failure>;
    fn resolve_comptime_type_path(
        &mut self,
        file: Self::File,
        segments: &[Self::Name],
        span: Span,
    ) -> Result<Option<Self::Value>, Self::Failure>;
    fn resolve_module_comptime_callable(
        &mut self,
        file_id: Self::File,
        segments: &[Self::Name],
        method: Self::Name,
        span: Span,
    ) -> Result<Option<Self::Name>, Self::Failure>;
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
    ) -> Result<Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>, Self::Failure>;
    fn bind_comptime_call(
        &self,
        admission: &ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        values: &[Self::Value],
        span: Span,
    ) -> Result<
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
    ) -> Result<
        Option<
            ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
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
        result: ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure>;
    fn label_ctor_instantiation_site(error: Self::Failure, call_span: Span) -> Self::Failure;
    fn canonical_function_producer(
        &self,
        name: Self::Name,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
        span: Span,
    ) -> Result<Self::CanonicalIdentity, Self::Failure>;
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
    fn register_anon_struct_methods_for_comptime_with_subst(
        &mut self,
        program: &Self::ProgramKey,
        struct_id: &Self::AnonymousStructId,
        struct_type: Self::Type,
        methods: &rue_rir::RirAnonStructMethodsRange,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
    ) -> Option<()>;
    fn set_anon_struct_type_subst(
        &mut self,
        struct_id: &Self::AnonymousStructId,
        subst: AHashMap<Self::Name, Self::Type>,
    );
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
        if frame.name.is_some() {
            let span = frame.span;
            return self.run_frame(frame, span);
        }
        let body = frame.body;
        self.frames.push(frame);
        let result = self.eval(body, env);
        self.frames.pop();
        result
    }

    /// Evaluate a named call through a child call. The body host receives
    /// only the semantically named call operation; recursive expression edges
    /// stay in this engine.
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
            ComptimeCallPreparation::Enter(frame) => self.enter_call(frame, span),
        }
    }

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
        call_span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        self.run_frame(frame, call_span)
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
        let span = frame.span;
        let result = self.run_frame(frame, span);
        result
    }

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
        }
        let mut child_env = ComptimeEnv::with_subst(&frame.type_bindings, &frame.value_bindings);
        child_env.canonical_identity = frame.call_identity.clone();
        child_env.defining_file = frame.context.clone();
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
                ComptimeOutcome::Abort(error) => {
                    ComptimeOutcome::Abort(H::label_ctor_instantiation_site(error, call_span))
                }
                other => other,
            };
            self.host.finish_comptime_call(&frame, result)
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
        let decoded = host_value!(self.decode_module_path(receiver, env));
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
            ComptimeCallPreparation::Enter(frame) => self.enter_call(frame, span),
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
    ) -> Result<Option<(H::File, Vec<H::Name>)>, H::Failure> {
        let mut chain_rev = Vec::new();
        let mut cursor = receiver;
        let root = loop {
            match self.program_rir().get(cursor).data {
                InstData::VarRef { name, .. } => break self.name_from_rir(name.into()),
                InstData::FieldGet { base, field } => {
                    chain_rev.push(self.name_from_rir(field.into()));
                    cursor = base;
                }
                _ => return Ok(None),
            }
        };
        if env.locals.contains_key(&root)
            || env.runtime_local_names.contains(&root)
            || env.runtime_binding_names.contains(&root)
            || env.type_subst.contains_key(&root)
            || env.value_subst.contains_key(&root)
        {
            return Ok(None);
        }
        let Some(file_id) = env.defining_file.clone() else {
            return Ok(None);
        };
        chain_rev.reverse();
        let mut segments = Vec::with_capacity(chain_rev.len() + 1);
        segments.push(root);
        segments.extend(chain_rev);
        Ok(Some((file_id, segments)))
    }

    /// Decode a dotted type path before crossing the host boundary. The host
    /// receives only copied semantic path facts; it never needs to inspect the
    /// RIR spine or an evaluation environment to decide whether this is a
    /// module/type path.
    fn decode_type_path(
        &self,
        inst_ref: InstRef,
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> Result<Option<(H::File, Vec<H::Name>)>, H::Failure> {
        let mut chain_rev = Vec::new();
        let mut cursor = inst_ref;
        let root = loop {
            match self.program_rir().get(cursor).data {
                InstData::VarRef { name, .. } => break self.name_from_rir(name.into()),
                InstData::FieldGet { base, field } => {
                    chain_rev.push(self.name_from_rir(field.into()));
                    cursor = base;
                }
                _ => return Ok(None),
            }
        };
        if env.locals.contains_key(&root)
            || env.runtime_local_names.contains(&root)
            || env.runtime_binding_names.contains(&root)
            || env.type_subst.contains_key(&root)
            || env.value_subst.contains_key(&root)
        {
            return Ok(None);
        }
        let Some(file_id) = env.defining_file.clone() else {
            return Ok(None);
        };
        chain_rev.reverse();
        let mut segments = Vec::with_capacity(chain_rev.len() + 1);
        segments.push(root);
        segments.extend(chain_rev);
        Ok(Some((file_id, segments)))
    }

    fn eval_int_operands(
        &mut self,
        lhs: InstRef,
        rhs: InstRef,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<(i128, i128), H::Failure> {
        let l = outcome_value!(self.eval(lhs, env));
        let Some(l) = l.as_integer() else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let r = outcome_value!(self.eval(rhs, env));
        let Some(r) = r.as_integer() else {
            return ComptimeOutcome::RuntimeDependent;
        };
        ComptimeOutcome::Known((l, r))
    }

    /// The single compile-time evaluation engine. See the module docs for the
    /// result encoding is a typed `ComptimeOutcome`; no recursive edge is
    /// collapsed into a legacy optional result inside the engine.
    fn eval(
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
                if let Some(ty) = self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref)
                {
                    if !self
                        .host
                        .type_integer_semantics(&ty)
                        .is_some_and(|integer| integer.fits_i128(v))
                    {
                        return ComptimeOutcome::HostFailure(
                            self.host.literal_out_of_range(*value, &ty, span),
                        );
                    }
                }
                ComptimeOutcome::Known(H::Value::integer(v))
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

            // Boolean literals
            InstData::BoolConst(value) => ComptimeOutcome::Known(H::Value::boolean(*value)),

            // Unit literal
            InstData::UnitConst => ComptimeOutcome::Known(H::Value::unit()),

            // Unary negation: -expr
            InstData::Neg { operand } => {
                let ty = self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref);
                if let Some(ref ty) = ty {
                    if self.host.type_is_unsigned(ty) {
                        return ComptimeOutcome::HostFailure(self.host.cannot_negate(ty, span));
                    }
                }
                if let InstData::IntConst(magnitude) = &self.program_rir().get(*operand).data {
                    // The literal path uses mathematical magnitude semantics:
                    // unlike an ordinary runtime value, `128` must not first
                    // canonicalize to -128 before becoming `-128`.
                    let result = ty
                        .as_ref()
                        .and_then(|ty| self.host.type_integer_semantics(ty))
                        .map_or_else(
                            || CheckedIntegerResult::from_raw((*magnitude as i128).checked_neg()),
                            |integer| integer.checked_neg_literal_report_i128(*magnitude as i128),
                        );
                    match self.host.finish_arith(result, ty, "-", span) {
                        Ok(Some(value)) => ComptimeOutcome::Known(value),
                        Ok(None) => ComptimeOutcome::RuntimeDependent,
                        Err(error) => ComptimeOutcome::HostFailure(error),
                    }
                } else {
                    match self.eval(*operand, env) {
                        ComptimeOutcome::Known(value) => {
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
                            match self.host.finish_arith(result, ty, "-", span) {
                                Ok(Some(value)) => ComptimeOutcome::Known(value),
                                Ok(None) => ComptimeOutcome::RuntimeDependent,
                                Err(error) => ComptimeOutcome::HostFailure(error),
                            }
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
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let ty = self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref);
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || CheckedIntegerResult::from_raw(l.checked_add(r)),
                        |integer| integer.checked_add_report_i128(l, r),
                    );
                match self.host.finish_arith(result, ty, "+", span) {
                    Ok(Some(value)) => ComptimeOutcome::Known(value),
                    Ok(None) => ComptimeOutcome::RuntimeDependent,
                    Err(error) => ComptimeOutcome::HostFailure(error),
                }
            }
            InstData::Sub { lhs, rhs } => {
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let ty = self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref);
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || CheckedIntegerResult::from_raw(l.checked_sub(r)),
                        |integer| integer.checked_sub_report_i128(l, r),
                    );
                match self.host.finish_arith(result, ty, "-", span) {
                    Ok(Some(value)) => ComptimeOutcome::Known(value),
                    Ok(None) => ComptimeOutcome::RuntimeDependent,
                    Err(error) => ComptimeOutcome::HostFailure(error),
                }
            }
            InstData::Mul { lhs, rhs } => {
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let ty = self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref);
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || CheckedIntegerResult::from_raw(l.checked_mul(r)),
                        |integer| integer.checked_mul_report_i128(l, r),
                    );
                match self.host.finish_arith(result, ty, "*", span) {
                    Ok(Some(value)) => ComptimeOutcome::Known(value),
                    Ok(None) => ComptimeOutcome::RuntimeDependent,
                    Err(error) => ComptimeOutcome::HostFailure(error),
                }
            }
            InstData::Div { lhs, rhs } | InstData::Mod { lhs, rhs } => {
                let is_div = matches!(&inst.data, InstData::Div { .. });
                let op = if is_div { "/" } else { "%" };
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                let ty = self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref);
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
                match self.host.finish_arith(result, ty, op, span) {
                    Ok(Some(value)) => ComptimeOutcome::Known(value),
                    Ok(None) => ComptimeOutcome::RuntimeDependent,
                    Err(error) => ComptimeOutcome::HostFailure(error),
                }
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
                    ComptimeOutcome::Known(rhs) => match (
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
                        _ => ComptimeOutcome::RuntimeDependent,
                    },
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
                    ComptimeOutcome::Known(rhs) => match (
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
                        _ => ComptimeOutcome::RuntimeDependent,
                    },
                    other => other,
                }
            }
            InstData::Lt { lhs, rhs } => {
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                ComptimeOutcome::Known(H::Value::boolean(l < r))
            }
            InstData::Gt { lhs, rhs } => {
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                ComptimeOutcome::Known(H::Value::boolean(l > r))
            }
            InstData::Le { lhs, rhs } => {
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                ComptimeOutcome::Known(H::Value::boolean(l <= r))
            }
            InstData::Ge { lhs, rhs } => {
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
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
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                ComptimeOutcome::Known(H::Value::integer(l & r))
            }
            InstData::BitOr { lhs, rhs } => {
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                ComptimeOutcome::Known(H::Value::integer(l | r))
            }
            InstData::BitXor { lhs, rhs } => {
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                ComptimeOutcome::Known(H::Value::integer(l ^ r))
            }

            // Shifts: the amount is masked modulo the bit width and the
            // result truncated to the operand width (spec 4.3a:10), exactly
            // matching the runtime semantics (RUE-29).
            InstData::Shl { lhs, rhs } | InstData::Shr { lhs, rhs } => {
                let is_shl = matches!(&inst.data, InstData::Shl { .. });
                let (l, r) = outcome_value!(self.eval_int_operands(*lhs, *rhs, env));
                match self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref)
                {
                    Some(ty) => {
                        let integer = self
                            .host
                            .type_integer_semantics(&ty)
                            .expect("const_expr_type returned non-integer");
                        // Two's-complement AND masks negative amounts the same
                        // way the hardware masks the count register.
                        let v = integer.shift_i128(l, r, is_shl);
                        ComptimeOutcome::Known(H::Value::integer(v))
                    }
                    None => {
                        // Without the operand type the width is unknown, so
                        // only fold amounts < 8 (safe for every width) and
                        // defer the rest to runtime.
                        if !(0..8).contains(&r) {
                            return ComptimeOutcome::RuntimeDependent;
                        }
                        ComptimeOutcome::Known(H::Value::integer(if is_shl {
                            l << r
                        } else {
                            l >> r
                        }))
                    }
                }
            }

            // Bitwise NOT: truncated to the operand width (`~0` as u8 = 255).
            InstData::BitNot { operand } => {
                let n = outcome_value!(self.eval(*operand, env));
                let Some(n) = n.as_integer() else {
                    return ComptimeOutcome::RuntimeDependent;
                };
                let v = match self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref)
                {
                    Some(ty) => self
                        .host
                        .type_integer_semantics(&ty)
                        .expect("bitnot requires an integer type")
                        .bitnot_i128(n),
                    None => !n,
                };
                ComptimeOutcome::Known(H::Value::integer(v))
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => self.eval(*expr, env),

            // Block: evaluate `let` statements into the environment, then the
            // tail expression. Loops, assignments and calls are not supported
            // and make the block non-evaluable.
            InstData::Block { instructions } => {
                let stmt_refs = self.program_rir().block_insts(instructions).to_vec();
                if stmt_refs.is_empty() {
                    return ComptimeOutcome::Known(H::Value::unit());
                }
                // Bindings are scoped to the block.
                let saved_locals = env.locals.clone();
                let mut result = H::Value::unit();
                for (i, stmt_ref) in stmt_refs.iter().copied().enumerate() {
                    let is_tail = i + 1 == stmt_refs.len();
                    let value = if let InstData::Alloc { name, init, .. } =
                        &self.program_rir().get(stmt_ref).data
                    {
                        let name = name.map(|name| self.name_from_rir(name.into()));
                        let init = *init;
                        let v = match self.eval(init, env) {
                            ComptimeOutcome::Known(value) => value,
                            other => {
                                env.locals = saved_locals;
                                return other;
                            }
                        };
                        if let Some(name) = name {
                            env.locals.insert(name, v);
                        }
                        // A `let` statement itself evaluates to unit.
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

            // Comptime-known `if`: select the taken branch and reduce to its
            // value. This is what lets an `if` in a `-> type` body pick a
            // struct/enum branch at compile time (spec 4.14:17, RUE-262) — the
            // same branch selection ordinary comptime values already relied on
            // through the block/let path, now available as an expression. A
            // non-constant condition makes the whole `if` non-evaluable.
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let (cond, then_block, else_block) = (*cond, *then_block, *else_block);
                match self.eval(cond, env) {
                    ComptimeOutcome::Known(value) if value.as_boolean() == Some(true) => {
                        self.eval(then_block, env)
                    }
                    ComptimeOutcome::Known(value) if value.as_boolean() == Some(false) => {
                        match else_block {
                            Some(else_block) => self.eval(else_block, env),
                            // `if c { .. }` with no else yields unit when false.
                            None => ComptimeOutcome::Known(H::Value::unit()),
                        }
                    }
                    // Non-constant (or non-bool) condition: not evaluable.
                    other => other,
                }
            }

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
                    let Some(field_ty) = self
                        .host
                        .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                            &self.program_key(),
                            type_sym,
                            &local_type_subst,
                            &local_value_subst,
                            span,
                        )
                    else {
                        return ComptimeOutcome::RuntimeDependent;
                    };
                    struct_fields.push(ComptimeField {
                        name: field_name,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                let method_sigs = self.host.extract_anon_method_sigs(
                    &self.program_key(),
                    methods,
                    &local_type_subst,
                    &local_value_subst,
                );

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

                // Register methods if present and not yet registered for this
                // struct (it may have been created earlier without methods).
                if !self.program_rir().anon_struct_methods(methods).is_empty() {
                    // A method that declares its own `comptime T: type`
                    // parameter would need per-call monomorphization over that
                    // parameter, which is unsupported (RUE-284). Reject it at
                    // the method declaration so the enclosing `-> type`
                    // reduction cannot degrade into an unrelated E1200 at the
                    // instantiation site.
                    if let Some((method_span, method_name)) = self
                        .host
                        .find_method_own_comptime_type_param(&self.program_key(), methods)
                    {
                        return ComptimeOutcome::HostFailure(
                            self.host
                                .unsupported_anon_method_type_param(method_span, &method_name),
                        );
                    }
                    let Some(struct_id) = self.host.anonymous_struct_id(&struct_ty) else {
                        return ComptimeOutcome::RuntimeDependent;
                    };

                    let method_refs = self.program_rir().anon_struct_methods(methods);
                    let first_method_ref = method_refs.get(0).unwrap();
                    let first_method_inst = self.program_rir().get(first_method_ref);
                    if let InstData::FnDecl {
                        name: method_name, ..
                    } = &first_method_inst.data
                    {
                        let needs_registration = !self
                            .host
                            .has_method(&struct_id, self.name_from_rir((*method_name).into()));

                        if needs_registration
                            && self
                                .host
                                .register_anon_struct_methods_for_comptime_with_subst(
                                    &self.program_key(),
                                    &struct_id,
                                    struct_ty.clone(),
                                    methods,
                                    &local_type_subst,
                                    &local_value_subst,
                                )
                                .is_none()
                        {
                            // Registration failure (e.g. duplicate method
                            // names) makes the type non-evaluable; the
                            // caller reports the comptime failure.
                            return ComptimeOutcome::RuntimeDependent;
                        }

                        // Remember the enclosing type substitution (e.g.
                        // `T -> i32` for `Vec(i32)`) so it resolves inside every
                        // method *body*, not just the signatures registered
                        // above (RUE-313). Method bodies are analyzed later, in
                        // a separate pass that has no other way to recover the
                        // constructor's type parameters.
                        if needs_registration && !local_type_subst.is_empty() {
                            self.host
                                .set_anon_struct_type_subst(&struct_id, local_type_subst.clone());
                        }
                    }
                }
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
                        let Some(ty) = self
                            .host
                            .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                                &self.program_key(),
                                ty_sym,
                                &enum_type_subst,
                                &enum_value_subst,
                                span,
                            )
                        else {
                            return ComptimeOutcome::RuntimeDependent;
                        };
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
                self.host
                    .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                        &self.program_key(),
                        type_name,
                        &env.type_subst,
                        &env.value_subst,
                        span,
                    )
                    .map(H::Value::type_value)
                    .map_or(ComptimeOutcome::RuntimeDependent, ComptimeOutcome::Known)
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
                    return ComptimeOutcome::RuntimeDependent;
                };
                let len = match count {
                    RepeatCount::Literal(n) => n,
                    RepeatCount::Named(sym) => {
                        let name = self.name_from_rir(sym.into());
                        match self.host.resolve_named_array_length(
                            &name,
                            span,
                            Some(&env.value_subst),
                        ) {
                            Ok(n) => n,
                            Err(error) => return ComptimeOutcome::HostFailure(error),
                        }
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
                if let Some(info) = self.host.value_const(&(file.clone(), name.clone())) {
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

            // Call to a `-> type` function: reduce it to the resulting type
            // value when the callee is a type constructor and every argument
            // is compile-time known. This makes comptime type-function calls
            // compose in ANY position — a delegating return body
            // (`fn Alias() -> type { Point() }`), a nested argument
            // (`WrapA(WrapA(i32))`), and chains thereof (RUE-251).
            InstData::Call { name, args } => {
                let name = self.name_from_rir((*name).into());
                self.evaluate_call(name, args, env, span)
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
            InstData::FieldGet { .. } => {
                if let Some(value) = env.const_module_members.get(&inst_ref) {
                    return ComptimeOutcome::Known(value.clone());
                }
                let decoded = host_value!(self.decode_type_path(inst_ref, env));
                let Some((file, segments)) = decoded else {
                    return ComptimeOutcome::RuntimeDependent;
                };
                match host_value!(self.host.resolve_comptime_type_path(file, &segments, span)) {
                    Some(value) => ComptimeOutcome::Known(value),
                    None => ComptimeOutcome::RuntimeDependent,
                }
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
                    let Some(int_ty) = self
                        .host
                        .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                            &self.program_key(),
                            type_arg,
                            &env.type_subst,
                            &env.value_subst,
                            span,
                        )
                    else {
                        return ComptimeOutcome::RuntimeDependent;
                    };
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
                        Some(value) => ComptimeOutcome::Known(H::Value::integer(value)),
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
                let Some(elem_ty) = self
                    .host
                    .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                        &self.program_key(),
                        type_arg,
                        &env.type_subst,
                        &env.value_subst,
                        span,
                    )
                else {
                    return ComptimeOutcome::RuntimeDependent;
                };
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
