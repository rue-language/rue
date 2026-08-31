//! Program registration and completed-call memoization.

use super::*;

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
    pub(super) programs: AHashMap<ComptimeProgramKey<D, C>, ComptimeProgram<S, I>>,
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
        self.structured_type_authority_with_program(key, key.clone(), root_scope, root)
    }

    /// Admit the exact registered arena, symbol authority, and root while
    /// carrying a richer caller-owned program identity in the continuation.
    /// The stable key still selects the registry entry; `program` is never
    /// used to select or reconstruct that entry.
    pub fn structured_type_authority_with_program<P, Scope>(
        &self,
        key: &ComptimeProgramKey<D, C>,
        program: P,
        root_scope: Scope,
        root: rue_rir::RirTypeSyntaxRef,
    ) -> Option<
        crate::semantic_type_resolution::ComptimeStructuredTypeAuthorityWithProgram<P, Scope, S>,
    >
    where
        S: AsRef<str>,
    {
        let registered = self.programs.get(key)?;
        registered.rir.type_syntax().node(root)?;
        if !crate::semantic_type_resolution::registered_symbol_authority_is_valid(
            registered.rir.type_syntax(),
            &registered.symbols,
        ) {
            return None;
        }
        Some(
            crate::semantic_type_resolution::ComptimeStructuredTypeAuthority::from_registered(
                program,
                root_scope,
                registered.rir.type_syntax().clone(),
                Arc::clone(&registered.symbols),
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

/// Completed call facts retained for the lifetime chosen by the host.
/// The ordinary body host owns one instance per body analysis, so its entries
/// are dropped at that boundary. A missing key is intentionally distinct from
/// a memoized not-ready or runtime-dependent outcome, so callers can turn
/// misses into `Enter` frames.
#[derive(Debug)]
pub struct ComptimeCompletedCallMemo<D, C, T, V, R> {
    pub(super) outcomes: AHashMap<ComptimeCallKey<D, C, T, V>, ComptimeMemoizedOutcome<R>>,
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
