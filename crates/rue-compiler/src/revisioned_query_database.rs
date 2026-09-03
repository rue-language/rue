//! Canonical revisioned compiler query database.
//!
//! The database owner and its registered query families are kept in one
//! module tree. Phase modules below are source-level partitions of that one
//! owner; they do not introduce peer runtimes or alternate authorities.

use rue_air::{Node, SemanticTypeSyntaxProvider};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ahash::{AHashMap, AHashSet, RandomState};
use lasso::Key;

use rue_query::{
    CancellationToken, InputIdentity, QueryAbort, QueryContext, QueryFamily, QueryKey, QueryOutput,
    QueryRequestAttempt, QueryRuntime, QuerySelection, QueryTerminalKind, RequestExecution,
    Revision, WorkItem,
};

use crate::canonical_lower::CandidateModuleRirOutput;
use crate::durable_comptime::{EvaluatedSemanticConst, TypedSemanticConst};
use crate::parsed_modules::{ParsedModule, ParsedModulesWork, ParsedProgram};
use crate::retained_charge::RetainedCharge;
use crate::session::{AttemptId, QueryStructuralWork};
use crate::typed_query_store::{
    AbortedQueryReason, AttemptExecution as CompilerAttemptExecution, AttemptOutcomeKind,
    AttemptView, RuntimeObservation, TerminalKind, TypedQueryFamily,
};
use crate::{
    AcceptedReadManifest, AcceptedReadManifestEntry, CompileError, CompileResult, DefinitionKind,
    DefinitionNamespace, ErrorKind, ImportDemandFrontier, ImportDemandMode, ImportDemandRoots,
    ImportDiscoveryContext, ImportDiscoveryPlan, ImportDiscoveryRequest, ImportInputRevision,
    ImportObservation, ImportObservationLedger, ModuleId, ModuleRevision, SourceRevision,
    SourceSnapshot, Span, StableDefinitionKey, SyntaxWork,
};

mod backend;
mod body;
mod parse_import;
mod provider;
mod registrations;
mod semantic;
mod shared;

pub(crate) use backend::*;
pub(crate) use body::*;
pub(crate) use parse_import::*;
pub(crate) use provider::*;
#[cfg(test)]
pub(crate) use registrations::REGISTRATION_MANIFEST;
pub(crate) use semantic::*;
pub(crate) use shared::*;

#[cfg(test)]
pub(crate) const REVISIONED_DATABASE_SOURCE: &str = concat!(
    include_str!("revisioned_query_database/shared.rs"),
    include_str!("revisioned_query_database/backend.rs"),
    include_str!("revisioned_query_database/parse_import.rs"),
    include_str!("revisioned_query_database/parse_import/program_assembly.rs"),
    include_str!("revisioned_query_database/semantic.rs"),
    include_str!("revisioned_query_database/body.rs"),
    include_str!("revisioned_query_database/body/closure_nucleus.rs"),
    include_str!("revisioned_query_database/body/durable_comptime_adapters.rs"),
    include_str!("revisioned_query_database/body/provider_body.rs"),
    include_str!("revisioned_query_database/body/revision_symbol_space.rs"),
    include_str!("revisioned_query_database/body/transactions.rs"),
    include_str!("revisioned_query_database/registrations.rs"),
    include_str!("revisioned_query_database/provider.rs"),
    include_str!("revisioned_query_database/registrations/parse_import/parse_modules.rs"),
    include_str!("revisioned_query_database/registrations/parse_import/parse_module_batches.rs"),
    include_str!("revisioned_query_database/registrations/parse_import/module_source_bases.rs"),
    include_str!("revisioned_query_database/registrations/parse_import/module_indexes.rs"),
    include_str!(
        "revisioned_query_database/registrations/parse_import/declaration_occurrence_indexes.rs"
    ),
    include_str!("revisioned_query_database/registrations/parse_import/declaration_orders.rs"),
    include_str!("revisioned_query_database/registrations/semantic/declaration_shells.rs"),
    include_str!(
        "revisioned_query_database/registrations/semantic/stable_declaration_classifications.rs"
    ),
    include_str!(
        "revisioned_query_database/registrations/semantic/declaration_body_plan_artifacts.rs"
    ),
    include_str!("revisioned_query_database/registrations/parse_import/lookup_names.rs"),
    include_str!("revisioned_query_database/registrations/parse_import/lookup_imports.rs"),
    include_str!("revisioned_query_database/registrations/parse_import/resolve_imports.rs"),
    include_str!("revisioned_query_database/registrations/parse_import/declaration_imports.rs"),
    include_str!("revisioned_query_database/registrations/body/body_source_bases.rs"),
    include_str!("revisioned_query_database/registrations/body/body_inputs.rs"),
    include_str!("revisioned_query_database/registrations/body/warning_call_head_projections.rs"),
    include_str!("revisioned_query_database/registrations/body/warning_body_references.rs"),
    include_str!("revisioned_query_database/registrations/body/warning_body_reference_batches.rs"),
    include_str!("revisioned_query_database/registrations/body/body_transactions.rs"),
    include_str!("revisioned_query_database/registrations/body/body_toolchain_demands.rs"),
    include_str!("revisioned_query_database/registrations/body/body_produced_anonymous.rs"),
    include_str!("revisioned_query_database/registrations/semantic/semantic_nucleus.rs"),
    include_str!(
        "revisioned_query_database/registrations/semantic/declaration_semantics_projection.rs"
    ),
    include_str!(
        "revisioned_query_database/registrations/semantic/declaration_semantics_publications.rs"
    ),
    include_str!("revisioned_query_database/registrations/semantic/type_shapes.rs"),
    include_str!("revisioned_query_database/registrations/semantic/type_facts.rs"),
    include_str!("revisioned_query_database/registrations/semantic/layouts.rs"),
    include_str!("revisioned_query_database/registrations/semantic/call_abis.rs"),
    include_str!("revisioned_query_database/registrations/semantic/drop_glues.rs"),
    include_str!("revisioned_query_database/registrations/backend/cfgs.rs"),
    include_str!("revisioned_query_database/registrations/backend/raw_cfg_batches.rs"),
    include_str!("revisioned_query_database/registrations/backend/optimized_cfgs.rs"),
    include_str!("revisioned_query_database/registrations/backend/optimized_cfg_batches.rs"),
    include_str!("revisioned_query_database/registrations/backend/codegen_units.rs"),
    include_str!("revisioned_query_database/registrations/backend/codegen_unit_batches.rs"),
    include_str!("revisioned_query_database/registrations/backend/object_projections.rs"),
    include_str!("revisioned_query_database/registrations/backend/object_projection_batches.rs"),
    include_str!("revisioned_query_database/registrations/backend/backend_root_publications.rs"),
    include_str!("revisioned_query_database/registrations/body/body_analysis_bundles.rs"),
    include_str!("revisioned_query_database/registrations/body/body_reachability.rs"),
    include_str!("revisioned_query_database/registrations/body/body_closures.rs"),
    include_str!("revisioned_query_database/registrations/body/body_closure_publications.rs"),
    include_str!("revisioned_query_database/registrations/parse_import/parse.rs"),
    include_str!("revisioned_query_database/registrations/parse_import/test_candidate_scans.rs"),
    include_str!("revisioned_query_database/registrations/provider_probe.rs"),
    "\n#[cfg(test)]\npub(crate) mod test_support {\n",
    include_str!("revisioned_query_database/test_support.rs"),
    "\n}\n#[cfg(test)]\nmod tests {\n",
    include_str!("revisioned_query_database/tests.rs"),
    include_str!("revisioned_query_database/tests/backend.rs"),
    include_str!("revisioned_query_database/tests/body_provider.rs"),
    include_str!("revisioned_query_database/tests/body_provider/body.rs"),
    include_str!("revisioned_query_database/tests/body_provider/provider.rs"),
    include_str!("revisioned_query_database/tests/parse_import.rs"),
    include_str!("revisioned_query_database/tests/retention_cancellation.rs"),
    include_str!("revisioned_query_database/tests/semantic_declaration.rs"),
    "\n}\n",
);

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

// Keep the owner and all family authorities in this module tree. These
// imports intentionally make the phase files' narrow `use super::*` edges
// resolve through this hub while preserving the existing crate-facing names.
