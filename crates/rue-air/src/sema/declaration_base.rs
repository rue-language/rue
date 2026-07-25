//! Deriving one body-local epoch from an immutable declaration base
//! (RUE-1135, RUE-1091 repair option 2).
//!
//! # What a declaration base is
//!
//! A request builds exactly one fully bound declaration epoch — prepare,
//! project, install, endpoints — and then hands every reached body an epoch
//! *derived* from it rather than rebuilt from its inputs. The base is a private
//! data cache owned by the body bridge for the length of one rooted attempt. It
//! is not a query authority, not a durable artifact, and nothing outside the
//! bridge can obtain one.
//!
//! # Immutable base, body-local overlay
//!
//! Deriving is not copying. [`BoundSema::derive_body_epoch`] splits the epoch's
//! state into three groups, and the split is what makes sharing sound:
//!
//! - **Shared, never written after the base is built.** The closed declaration
//!   namespace, the stable definition/module/body-owner/const-candidate endpoint
//!   tables, the module registry, the RIR declaration index, and the file/symbol
//!   path maps. Each is held behind an `Arc`, so deriving bumps a refcount. The
//!   type system enforces this: `SourceDeclarations` has no `DerefMut`, and a
//!   write to any of the others is an `Arc::make_mut` that shows up in review.
//! - **Layered on the base and appended to locally.** The canonical type pool
//!   and the parameter arena, the two structures a body genuinely extends. Both
//!   use a true base plus append-only overlay: the base's entries keep their
//!   pool indices, and a body's interning appends above `base_len`. A body that
//!   interns a type copies nothing the base already holds.
//! - **Owned per body.** The small per-epoch state a body mutates from the
//!   start: generated-struct overlays, anonymous-nominal identity maps, the
//!   dependency-observation vectors, and the body-analysis work counters. These
//!   are O(anonymous nominals) rather than O(declarations) and are copied.
//!
//! # Independence
//!
//! Two bodies derived from one base share no mutable state. A body's overlay is
//! reachable only from its own epoch, and the epoch is consumed by
//! `analyze_one_body_instance`, so one body's materializations are invisible to
//! another regardless of the order the bodies were scheduled in. That is the
//! property [`EpochDerivationUnits`] is designed to make observable rather than
//! merely asserted: it reports what a derivation shared and what it copied, read
//! from the derived containers themselves.

use super::{BoundSema, DeclarationPhase, Sema};

/// Structural accounting for one epoch derivation (RUE-1135).
///
/// Every field is a count of units a derivation actually moved or shared, read
/// from the derived containers at the moment they were derived — never from an
/// input slice length. A family that stops being copied drops its `copied`
/// counter to zero and its `shared` counter picks the units up, so the two must
/// be read together and neither can hide the other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EpochDerivationUnits {
    /// Declaration bases built from scratch. One per request is the intended
    /// result.
    pub bases_built: u64,
    /// Body-local epochs derived from a base.
    pub epochs_derived: u64,
    /// Declaration-namespace entries shared with the base instead of copied.
    pub declaration_namespace_entries_shared: u64,
    /// Stable endpoint entries shared with the base instead of copied.
    pub endpoint_entries_shared: u64,
    /// Module-registry entries shared with the base instead of copied.
    pub module_registry_entries_shared: u64,
    /// Canonical type-pool entries read from the base overlay's immutable
    /// prefix instead of copied.
    pub type_pool_entries_shared: u64,
    /// Parameter-arena parameters read from the base instead of copied.
    pub parameter_entries_shared: u64,
    /// Entries a derivation genuinely copied: the per-body owned state, which is
    /// proportional to the epoch's anonymous nominals rather than to its
    /// declarations.
    pub body_local_entries_copied: u64,
}

impl EpochDerivationUnits {
    /// Fold another accounting value into this one.
    pub fn absorb(&mut self, other: Self) {
        self.bases_built += other.bases_built;
        self.epochs_derived += other.epochs_derived;
        self.declaration_namespace_entries_shared += other.declaration_namespace_entries_shared;
        self.endpoint_entries_shared += other.endpoint_entries_shared;
        self.module_registry_entries_shared += other.module_registry_entries_shared;
        self.type_pool_entries_shared += other.type_pool_entries_shared;
        self.parameter_entries_shared += other.parameter_entries_shared;
        self.body_local_entries_copied += other.body_local_entries_copied;
    }

    /// Total units a derivation read from the base without copying them.
    pub fn total_units_shared(&self) -> u64 {
        self.declaration_namespace_entries_shared
            + self.endpoint_entries_shared
            + self.module_registry_entries_shared
            + self.type_pool_entries_shared
            + self.parameter_entries_shared
    }
}

impl<D: DeclarationPhase> Sema<'_, D> {
    /// Per-family unit counts for one derivation of this epoch, read from the
    /// live containers rather than from the inputs the epoch was built from.
    fn derivation_unit_sizes(&self) -> EpochDerivationUnits {
        let namespace = &**self;
        let declaration_namespace_entries_shared = namespace.functions.len()
            + namespace.functions_by_file_name.len()
            + namespace.function_source_names.len()
            + namespace.builtin_structs.len()
            + namespace.structs_by_file_name.len()
            + namespace.builtin_enums.len()
            + namespace.enums_by_file_name.len()
            + namespace.methods.len()
            + namespace.named_method_declarations.len()
            + namespace.const_resolutions.len();
        let endpoint_entries_shared = self.stable_definition_tokens.len()
            + self.stable_definition_endpoints.len()
            + self.const_candidate_tokens.len()
            + self.const_candidate_endpoints.len()
            + self.stable_module_tokens.len()
            + self.stable_module_endpoints.len()
            + self.body_owner_tokens.len();
        // The per-body owned state a derivation really does copy. Anonymous
        // nominal identity maps and generated-name overlays dominate it; it is
        // sized by the epoch's anonymous universe, not by its declarations.
        let body_local_entries_copied = self.anonymous_methods.len()
            + self.named_callable_methods_by_symbol.len()
            + self.anonymous_callable_methods_by_symbol.len()
            + self.generated_structs.len()
            + self.generated_enums.len()
            + self.anon_struct_identities.len()
            + self.anon_enum_identities.len()
            + self.anonymous_digest_owners.len()
            + self.anonymous_struct_ids.len()
            + self.anonymous_enum_ids.len()
            + self.canonical_anonymous_types.len()
            + self.anon_struct_method_sigs.len()
            + self.anon_struct_captured_values.len()
            + self.anon_struct_type_subst.len()
            + self.destructor_spans.len()
            + self.infectious_linear.len()
            + self.ctor_type_displays.len()
            + self.well_known_option_by_payload.len()
            + self.well_known_option_identities.len();
        EpochDerivationUnits {
            bases_built: 0,
            epochs_derived: 1,
            declaration_namespace_entries_shared: declaration_namespace_entries_shared as u64,
            endpoint_entries_shared: endpoint_entries_shared as u64,
            module_registry_entries_shared: self.module_registry.len() as u64,
            type_pool_entries_shared: self.type_pool.len() as u64,
            parameter_entries_shared: self.param_arena.total_params() as u64,
            body_local_entries_copied: body_local_entries_copied as u64,
        }
    }
}

impl BoundSema<'_> {
    /// Seal this epoch as a request's declaration base (RUE-1135).
    ///
    /// The type pool and parameter arena are moved into shared immutable layers
    /// with an empty local layer on top. This is what makes every later
    /// [`Self::derive_body_epoch`] O(1) in those two families: an unsealed
    /// universe has to materialize a flat base before it can be shared, and
    /// doing that once per request instead of once per body is the difference
    /// between sharing the type universe and copying it per body.
    ///
    /// Calling this leaves the epoch's contents identical — every canonical
    /// `Type` and `ParamRange` keeps its meaning — so a sealed base analyzes a
    /// body exactly as an unsealed one would.
    pub fn seal_as_declaration_base(&mut self) {
        self.sema.type_pool = self.sema.type_pool.derive_overlay();
        self.sema.param_arena = self.sema.param_arena.derive_overlay();
    }

    /// Derive one body-local epoch from this declaration base, charging what the
    /// derivation shared and what it copied (RUE-1135).
    ///
    /// The derived epoch is independent: nothing it mutates is reachable from
    /// the base or from a sibling body's epoch. What it shares, it can only
    /// read — the shared containers have no mutable path out of the base, and
    /// the two layered containers place every local write above the base's
    /// entries rather than into them.
    pub fn derive_body_epoch(&self, units: &mut EpochDerivationUnits) -> Self {
        units.absorb(self.sema.derivation_unit_sizes());
        let mut derived = self.clone();
        derived.sema.type_pool = self.sema.type_pool.derive_overlay();
        derived.sema.param_arena = self.sema.param_arena.derive_overlay();
        derived
    }

    /// Type-pool entries this epoch interned itself, excluding everything it
    /// reads from a shared base. Zero for a freshly derived epoch.
    pub fn local_type_pool_entries(&self) -> usize {
        self.sema.type_pool.local_len()
    }

    /// Type-pool entries this epoch reads from a shared immutable base.
    pub fn shared_type_pool_entries(&self) -> usize {
        self.sema.type_pool.shared_len()
    }

    /// Parameters this epoch allocated itself, excluding its shared base.
    pub fn local_parameter_entries(&self) -> usize {
        self.sema.param_arena.local_params()
    }

    /// Parameters this epoch reads from a shared immutable base.
    pub fn shared_parameter_entries(&self) -> usize {
        self.sema.param_arena.shared_params()
    }
}
