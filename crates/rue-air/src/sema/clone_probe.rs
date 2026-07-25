//! Clone-from-template epoch copying (RUE-1133, RUE-1091 ordered probe #1).
//!
//! # What this is
//!
//! RUE-1091 lists clone-from-template as its first ordered probe: a fully bound
//! declaration epoch may be deep-cloned per body **solely to measure
//! feasibility and expose hidden copying**. The probe replaces the per-body
//! *derivation* of the declaration epoch (prepare → project → install →
//! endpoints) with a *copy* of one prebuilt template, so the measured per-body
//! prefix can be split into "derivation cost" and "structural cost that
//! survives any sharing scheme".
//!
//! # What this is not
//!
//! It is not, and can never become, a production boundary. Copying an epoch per
//! body is still O(declarations × bodies) — the copy is exactly the cost being
//! quantified, and a cheap clone is performance evidence only: it proves
//! nothing about exact invalidation. No production path in the compiler
//! constructs a template epoch or calls [`BoundSema::clone_from_template`].
//!
//! # Accounting
//!
//! [`EpochCloneUnits`] records what each copy actually moved, read from the
//! containers the copy traverses at the moment it traverses them — the
//! declaration namespace, the canonical type pool, the parameter arena, the
//! module registry, and the stable endpoint tables. Charging from the epoch's
//! own live container lengths (rather than from an input slice handed to the
//! probe) is what makes the number honest: a template that carries fewer
//! declarations copies fewer units, and a sharing scheme that stopped copying a
//! family would drop that family's counter to zero.

use super::{BoundSema, DeclarationPhase, Sema};

/// Structural accounting for the clone-from-template probe (RUE-1091 probe #1
/// "Record separately"). Every field is a count of units actually copied by a
/// template clone, sourced from the copied container itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EpochCloneUnits {
    /// Bound declaration epochs built from scratch to serve as templates.
    pub templates_built: u64,
    /// Deep copies taken from a template.
    pub templates_cloned: u64,
    /// Declaration-namespace entries copied (functions, methods, nominal
    /// lookups, const resolutions) across all clones.
    pub declaration_namespace_entries_copied: u64,
    /// Canonical type-pool entries copied across all clones.
    pub type_pool_entries_copied: u64,
    /// Parameter-arena parameters copied across all clones.
    pub parameter_entries_copied: u64,
    /// Module-registry entries copied across all clones.
    pub module_registry_entries_copied: u64,
    /// Stable definition/module/body-owner/const-candidate endpoint entries
    /// copied across all clones.
    pub endpoint_entries_copied: u64,
}

impl EpochCloneUnits {
    /// Fold another accounting value into this one.
    pub fn absorb(&mut self, other: Self) {
        self.templates_built += other.templates_built;
        self.templates_cloned += other.templates_cloned;
        self.declaration_namespace_entries_copied += other.declaration_namespace_entries_copied;
        self.type_pool_entries_copied += other.type_pool_entries_copied;
        self.parameter_entries_copied += other.parameter_entries_copied;
        self.module_registry_entries_copied += other.module_registry_entries_copied;
        self.endpoint_entries_copied += other.endpoint_entries_copied;
    }

    /// Total units copied across every metered family. This is the probe's
    /// single "structural cost" number: it is the size of the epoch that any
    /// per-body copy must move, independent of how fast the copy is.
    pub fn total_units_copied(&self) -> u64 {
        self.declaration_namespace_entries_copied
            + self.type_pool_entries_copied
            + self.parameter_entries_copied
            + self.module_registry_entries_copied
            + self.endpoint_entries_copied
    }
}

impl<D: DeclarationPhase> Sema<'_, D> {
    /// The per-family unit counts a single deep copy of this epoch moves.
    ///
    /// Read from the live containers themselves, so it describes this exact
    /// epoch rather than the inputs it was built from.
    pub(super) fn clone_unit_sizes(&self) -> EpochCloneUnits {
        let namespace = &**self;
        let declaration_namespace_entries_copied = namespace.functions.len()
            + namespace.functions_by_file_name.len()
            + namespace.function_source_names.len()
            + namespace.builtin_structs.len()
            + namespace.structs_by_file_name.len()
            + namespace.builtin_enums.len()
            + namespace.enums_by_file_name.len()
            + namespace.methods.len()
            + namespace.named_method_declarations.len()
            + namespace.const_resolutions.len();
        let endpoint_entries_copied = self.stable_definition_tokens.len()
            + self.stable_definition_endpoints.len()
            + self.const_candidate_tokens.len()
            + self.const_candidate_endpoints.len()
            + self.stable_module_tokens.len()
            + self.stable_module_endpoints.len()
            + self.body_owner_tokens.len();
        EpochCloneUnits {
            templates_built: 0,
            templates_cloned: 1,
            declaration_namespace_entries_copied: declaration_namespace_entries_copied as u64,
            type_pool_entries_copied: self.type_pool.len() as u64,
            parameter_entries_copied: self.param_arena.total_params() as u64,
            module_registry_entries_copied: self.module_registry.len() as u64,
            endpoint_entries_copied: endpoint_entries_copied as u64,
        }
    }
}

impl BoundSema<'_> {
    /// Deep-copy a bound declaration epoch, charging what the copy moved.
    ///
    /// Measurement only (RUE-1133 / RUE-1091 probe #1). The copy shares only the
    /// two borrowed inputs every epoch already borrows — the RIR arena and the
    /// string interner — and owns every mutable structure independently, which
    /// is what makes the clone a valid stand-in for a freshly derived epoch.
    pub fn clone_from_template(&self, units: &mut EpochCloneUnits) -> Self {
        units.absorb(self.sema.clone_unit_sizes());
        self.clone()
    }
}
