//! Evaluation sites, diagnostic sites, identity, and the environment.

use super::*;

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
    pub(super) program: P,
    pub(super) kind: ComptimeSiteKind,
    pub(super) occurrence: u32,
    pub(super) span: Span,
}

/// The exact owning program and source range for an engine-created diagnostic.
/// Unlike `ComptimeSite`, this carries no semantic occurrence classification:
/// terminal hooks need only the active program and the span supplied by the
/// engine.
#[derive(Debug, Clone)]
pub struct ComptimeDiagnosticSite<P> {
    pub(super) program: P,
    pub(super) span: Span,
}

impl<P> ComptimeDiagnosticSite<P> {
    /// Constructs the producer-keyed site for the active engine frame.
    ///
    /// Kept private so hosts cannot manufacture a site for an unrelated
    /// program authority.
    pub(super) fn new(program: P, span: Span) -> Self {
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
    /// Constructs a semantic site from an operation kind and source-order
    /// occurrence. The occurrence is never an instruction reference.
    pub fn from_occurrence(
        program: P,
        kind: ComptimeSiteKind,
        occurrence: u32,
        span: Span,
    ) -> Self {
        Self::new(program, kind, occurrence, span)
    }

    /// Constructs an import site from its semantic source-order occurrence.
    ///
    /// The occurrence is an operation-order fact, not an `InstRef`; callers
    /// must obtain it from the owning program's semantic import metadata.
    pub fn from_import_occurrence(program: P, occurrence: u32, span: Span) -> Self {
        Self::from_occurrence(program, ComptimeSiteKind::Import, occurrence, span)
    }

    pub(super) fn new(program: P, kind: ComptimeSiteKind, occurrence: u32, span: Span) -> Self {
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
    /// Optional persistent membership view supplied by staged inference. It
    /// keeps the canonical evaluator from flattening a lexical checkpoint;
    /// ordinary comptime frames continue to use `runtime_local_names`.
    pub runtime_local_name_membership: Option<std::sync::Arc<dyn Fn(&N) -> bool + 'a>>,
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
            } else if val.eligible_for_comptime_capture() {
                value_subst.insert(name.clone(), val.clone());
                type_subst.remove(name);
            } else {
                value_subst.remove(name);
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
            runtime_local_name_membership: None,
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
            runtime_local_name_membership: None,
            runtime_binding_names: AHashSet::new(),
            locals: AHashMap::new(),
            const_module_members: AHashMap::new(),
            defining_file: None,
            expected_result: None,
        }
    }

    pub(crate) fn is_runtime_local_name(&self, name: &N) -> bool {
        self.runtime_local_names.contains(name)
            || self
                .runtime_local_name_membership
                .as_ref()
                .is_some_and(|membership| membership(name))
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
