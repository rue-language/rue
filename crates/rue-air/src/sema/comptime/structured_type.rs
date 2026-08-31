//! Structured-type suspension and resumption, sealed to one representation.

/// The typed result of one structured-type host continuation.
///
/// The suspension is opaque to the engine. A host may use the keyed
/// structured-type job from `semantic_type_resolution`, but the engine never
/// receives a program, scope, arena, or syntax reference while resuming it.
///
/// State invariant: `Suspended` owns the only live continuation for a
/// structured-type reduction. The execution core may prepare at most one
/// child call from it, and must feed that child's terminal outcome back to
/// `resume_structured_type_call` before inspecting another syntax node.
pub enum ComptimeStructuredTypeResolution<V, S> {
    Ready(V),
    Suspended(S),
}

pub(super) mod structured_type_seal {
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
