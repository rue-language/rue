//! Endpoint / definition-selection resolution for exact-one-body analysis.
//!
//! Family 1A of the RUE-1091 analyzer rewire (slice r1a). The endpoint,
//! nominal, and module reads that `one_body.rs` performs while turning a
//! canonical body request or reference into a concrete declaration, method,
//! destructor, or materialized [`Type`] no longer touch the semantic epoch
//! tables directly. They flow through [`BodyEndpointProvider`] — the
//! value/definition-world analog of [`crate::SemanticTypeSyntaxProvider`] — so
//! the selection *logic* is provider-generic and a later slice can supply the
//! same facts from a body-fact provider + overlay instead of the epoch `Sema`.
//!
//! [`EpochFacts`] is the generic production adapter: it delegates each point
//! query to a [`BodyEndpointFactSource`] supplied by its host. `Sema` supplies
//! the current declaration-epoch source, preserving the existing reads while
//! keeping the adapter independent of an analyzer representation. Every
//! operation is `&self` and returns owned or `Copy` data, so a caller inside an
//! `&mut Sema` stack constructs a short-lived [`EpochFacts`] per resolution
//! without retaining a borrow across the surrounding mutations.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use lasso::{Spur, ThreadedRodeo};
use rue_rir::{InstRef, Rir};
use rue_span::FileId;

use super::anon_structs::IssuedAnonymousNominalKey;
use super::body_identity::{
    BodyRirIndex, ConstIdentityHandle, DurableAnonymousSource, DurableConstSource,
    DurableNominalSource, ProviderIdentityContext,
};
use super::declaration_index::RirDestructorDeclaration;
use super::info::{ConstInfo, FunctionInfo, MethodInfo};
use super::provider::BodyFactProvider;
use super::{DeclarationPhase, Sema};
use crate::intern_pool::TypeInternPool;
use crate::types::{EnumId, ModuleId, StructId, Type};
use crate::{
    SemanticBodyExportFailure, SemanticDefinitionEndpoint, SemanticDefinitionToken,
    SemanticImportNominalKind, SemanticImportType, SemanticModuleEndpoint, SemanticModuleToken,
    StableDefinitionKind, TypeInstanceKey,
};

/// The exact endpoint/definition-selection fact boundary consumed by
/// `one_body.rs`. Every operation answers one point query against the
/// declaration universe and returns owned/`Copy` data — no borrowed epoch table
/// or live `Sema` handle escapes.
pub(crate) trait BodyEndpointProvider {
    /// Intern a source name to its symbol, or `None` if never interned. Mirrors
    /// `interner.get`.
    fn name_symbol(&self, name: &str) -> Option<Spur>;

    /// The current stable endpoint for a definition token.
    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint>;

    /// The current stable endpoint for a module token.
    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint>;

    /// The internal free-function symbol declared as `(file, name)`.
    fn function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur>;

    /// The struct id declared as `(file, name)`.
    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId>;

    /// The enum id declared as `(file, name)`.
    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId>;

    /// The built-in or compiler-generated struct id for a bare name, preferring
    /// the built-in table. Mirrors `builtin_structs.or_else(generated_structs)`.
    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId>;

    /// The compiler-generated struct id for a bare name.
    fn generated_struct(&self, name: Spur) -> Option<StructId>;

    /// The built-in enum id for a bare name.
    fn builtin_enum(&self, name: Spur) -> Option<EnumId>;

    /// The struct id for an issued anonymous nominal identity.
    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId>;

    /// The enum id for an issued anonymous nominal identity.
    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId>;

    /// Whether a bare name is a built-in or compiler-generated struct.
    fn is_builtin_or_generated_struct(&self, name: Spur) -> bool;

    /// Whether a bare name is a built-in enum.
    fn is_builtin_enum(&self, name: Spur) -> bool;

    /// The signature/binding info for an internal free-function symbol.
    fn function_info(&self, name: Spur) -> Option<FunctionInfo>;

    /// The signature/binding info for a `(struct, name)` member.
    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo>;

    /// The source name a specialized/internal function name derives from.
    /// Mirrors `Sema::source_function_name` (identity when unmapped).
    fn source_function_name(&self, name: Spur) -> Spur;

    /// The first free-function RIR declaration for `(source, file)`.
    fn first_free_function(&self, source: Spur, file_id: FileId) -> Option<InstRef>;

    /// The named-method RIR declaration for the durable-available
    /// `(owner_file, owner_type_name, method_name)` preimage.
    fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: Spur,
        method_name: Spur,
    ) -> Option<InstRef>;

    /// The destructor declaration record for `(file, type_name)`.
    fn destructor(&self, file: u32, type_name: Spur) -> Option<RirDestructorDeclaration>;

    /// The module id whose definition lives in `file`.
    fn module_id_for_file(&self, file: u32) -> Option<ModuleId>;

    /// Intern an array type, or `None` on a type-validation failure.
    fn intern_array(&self, element: Type, len: u64) -> Option<Type>;

    /// Intern a `ptr const` type, or `None` on a type-validation failure.
    fn intern_ptr_const(&self, pointee: Type) -> Option<Type>;

    /// Intern a `ptr mut` type, or `None` on a type-validation failure.
    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type>;
}

/// Raw immutable endpoint reads supplied by a body-analysis host.
pub(super) trait BodyEndpointFactSource {
    fn endpoint_name_symbol(&self, name: &str) -> Option<Spur>;
    fn endpoint_definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint>;
    fn endpoint_module_endpoint(
        &self,
        token: SemanticModuleToken,
    ) -> Option<SemanticModuleEndpoint>;
    fn endpoint_function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur>;
    fn endpoint_struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId>;
    fn endpoint_enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId>;
    fn endpoint_builtin_or_generated_struct(&self, name: Spur) -> Option<StructId>;
    fn endpoint_generated_struct(&self, name: Spur) -> Option<StructId>;
    fn endpoint_builtin_enum(&self, name: Spur) -> Option<EnumId>;
    fn endpoint_anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId>;
    fn endpoint_anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId>;
    fn endpoint_is_builtin_or_generated_struct(&self, name: Spur) -> bool;
    fn endpoint_is_builtin_enum(&self, name: Spur) -> bool;
    fn endpoint_function_info(&self, name: Spur) -> Option<FunctionInfo>;
    fn endpoint_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo>;
    fn endpoint_source_function_name(&self, name: Spur) -> Spur;
    fn endpoint_first_free_function(&self, source: Spur, file: FileId) -> Option<InstRef>;
    fn endpoint_named_method_declaration(
        &self,
        file: FileId,
        ty: Spur,
        method: Spur,
    ) -> Option<InstRef>;
    fn endpoint_destructor(&self, file: u32, ty: Spur) -> Option<RirDestructorDeclaration>;
    fn endpoint_module_id_for_file(&self, file: u32) -> Option<ModuleId>;
    fn endpoint_intern_array(&self, element: Type, len: u64) -> Option<Type>;
    fn endpoint_intern_ptr_const(&self, pointee: Type) -> Option<Type>;
    fn endpoint_intern_ptr_mut(&self, pointee: Type) -> Option<Type>;
}

/// Direct epoch reads for the current host. The generic adapter below owns the
/// read-only abstraction; this impl is only the production source of facts.
impl<D: DeclarationPhase> BodyEndpointFactSource for Sema<'_, D> {
    fn endpoint_name_symbol(&self, name: &str) -> Option<Spur> {
        self.interner.get(name)
    }

    fn endpoint_definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint> {
        self.stable_definition_endpoints.get(&token).cloned()
    }

    fn endpoint_module_endpoint(
        &self,
        token: SemanticModuleToken,
    ) -> Option<SemanticModuleEndpoint> {
        self.stable_module_endpoints.get(&token).copied()
    }

    fn endpoint_function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur> {
        self.record_body_module_item_lookup(file, name);
        self.functions_by_file_name.get(&(file, name)).copied()
    }

    fn endpoint_struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.record_body_module_item_lookup(file, name);
        self.structs_by_file_name.get(&(file, name)).copied()
    }

    fn endpoint_enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.record_body_module_item_lookup(file, name);
        self.enums_by_file_name.get(&(file, name)).copied()
    }

    fn endpoint_builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.builtin_structs
            .get(&name)
            .or_else(|| self.generated_structs.get(&name))
            .copied()
    }

    fn endpoint_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.generated_structs.get(&name).copied()
    }

    fn endpoint_builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.builtin_enums.get(&name).copied()
    }

    fn endpoint_anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        self.anon_struct_identities.get(identity).copied()
    }

    fn endpoint_anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        self.anon_enum_identities.get(identity).copied()
    }

    fn endpoint_is_builtin_or_generated_struct(&self, name: Spur) -> bool {
        self.builtin_structs.contains_key(&name) || self.generated_structs.contains_key(&name)
    }

    fn endpoint_is_builtin_enum(&self, name: Spur) -> bool {
        self.builtin_enums.contains_key(&name)
    }

    fn endpoint_function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.function_info(name).copied()
    }

    fn endpoint_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.record_body_member_lookup(struct_id, name);
        self.method_info((struct_id, name)).copied()
    }

    fn endpoint_source_function_name(&self, name: Spur) -> Spur {
        self.source_function_name(name)
    }

    fn endpoint_first_free_function(&self, source: Spur, file_id: FileId) -> Option<InstRef> {
        self.record_body_module_item_lookup(file_id, source);
        self.declaration_index
            .first_free_function(source, Some(file_id))
    }

    fn endpoint_named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: Spur,
        method_name: Spur,
    ) -> Option<InstRef> {
        self.record_body_module_item_lookup(owner_file, owner_type_name);
        let struct_id = self
            .structs_by_file_name
            .get(&(owner_file, owner_type_name))?;
        self.record_body_member_lookup(*struct_id, method_name);
        self.named_method_declarations
            .get(&(*struct_id, method_name))
            .copied()
    }

    fn endpoint_destructor(&self, file: u32, type_name: Spur) -> Option<RirDestructorDeclaration> {
        self.record_body_destructor_lookup(FileId::new(file), type_name);
        self.declaration_index
            .destructors()
            .iter()
            .find(|record| record.span.file_id.index() == file && record.type_name == type_name)
            .copied()
    }

    fn endpoint_module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        (0..self.module_registry.len())
            .map(|index| ModuleId::new(index as u32))
            .find(|id| self.module_registry.get_def(*id).file_id.index() == file)
    }

    fn endpoint_intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.type_pool.try_intern_array(element, len).ok()
    }

    fn endpoint_intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.type_pool.try_intern_ptr_const(pointee).ok()
    }

    fn endpoint_intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.type_pool.try_intern_ptr_mut(pointee).ok()
    }
}

/// Read-only endpoint adapter used by the canonical body engine.
///
/// It carries only a host capability; the current epoch-backed host is one
/// production source, while an owned body state can supply the same reads next.
pub(crate) struct EpochFacts<'host, H: super::fact_mode::BodyAnalysisReadHost> {
    host: &'host H,
}

impl<'host, H: super::fact_mode::BodyAnalysisReadHost> EpochFacts<'host, H> {
    pub(in crate::sema) fn new(host: &'host H) -> Self {
        Self { host }
    }
}

impl<H: super::fact_mode::BodyAnalysisReadHost> BodyEndpointProvider for EpochFacts<'_, H> {
    fn name_symbol(&self, name: &str) -> Option<Spur> {
        self.host.endpoint_name_symbol(name)
    }
    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint> {
        self.host.endpoint_definition_endpoint(token)
    }
    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint> {
        self.host.endpoint_module_endpoint(token)
    }
    fn function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur> {
        self.host.endpoint_function_by_file_name(file, name)
    }
    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.host.endpoint_struct_by_file_name(file, name)
    }
    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.host.endpoint_enum_by_file_name(file, name)
    }
    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.host.endpoint_builtin_or_generated_struct(name)
    }
    fn generated_struct(&self, name: Spur) -> Option<StructId> {
        self.host.endpoint_generated_struct(name)
    }
    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.host.endpoint_builtin_enum(name)
    }
    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        self.host.endpoint_anon_struct(identity)
    }
    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        self.host.endpoint_anon_enum(identity)
    }
    fn is_builtin_or_generated_struct(&self, name: Spur) -> bool {
        self.host.endpoint_is_builtin_or_generated_struct(name)
    }
    fn is_builtin_enum(&self, name: Spur) -> bool {
        self.host.endpoint_is_builtin_enum(name)
    }
    fn function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.host.endpoint_function_info(name)
    }
    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.host.endpoint_method_info(struct_id, name)
    }
    fn source_function_name(&self, name: Spur) -> Spur {
        self.host.endpoint_source_function_name(name)
    }
    fn first_free_function(&self, source: Spur, file_id: FileId) -> Option<InstRef> {
        self.host.endpoint_first_free_function(source, file_id)
    }
    fn named_method_declaration(&self, file: FileId, ty: Spur, method: Spur) -> Option<InstRef> {
        self.host
            .endpoint_named_method_declaration(file, ty, method)
    }
    fn destructor(&self, file: u32, ty: Spur) -> Option<RirDestructorDeclaration> {
        self.host.endpoint_destructor(file, ty)
    }
    fn module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        self.host.endpoint_module_id_for_file(file)
    }
    fn intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.host.endpoint_intern_array(element, len)
    }
    fn intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.host.endpoint_intern_ptr_const(pointee)
    }
    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.host.endpoint_intern_ptr_mut(pointee)
    }
}

/// Resolve a definition-token function reference to its internal free-function
/// symbol. The endpoint, name interning, and by-file lookup all fail to the
/// same `MissingStableIdentity` in the caller, so a single `None` is faithful.
pub(in crate::sema) fn resolve_free_function_symbol<P: BodyEndpointProvider>(
    facts: &P,
    token: SemanticDefinitionToken,
) -> Option<Spur> {
    let endpoint = facts.definition_endpoint(token)?;
    let symbol = facts.name_symbol(endpoint.name.as_ref())?;
    facts.function_by_file_name(FileId::new(endpoint.file), symbol)
}

/// Materialize a canonical type-instance key into a concrete [`Type`]. The
/// provider-generic form of `one_body::materialize_instance_type`: an exact
/// transcription of the epoch code, with every table read routed through
/// `facts` and every failure mapped to `MissingStableIdentity`.
pub(in crate::sema) fn resolve_instance_type<P: BodyEndpointProvider>(
    facts: &P,
    value: &crate::TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
) -> Result<Type, crate::SemanticBodyExportFailure> {
    use crate::{AnonymousNominalKind as AK, NominalInstanceKey as N, TypeInstanceKey as T};
    let missing = || crate::SemanticBodyExportFailure::MissingStableIdentity;
    Ok(match value {
        T::I8 => Type::I8,
        T::I16 => Type::I16,
        T::I32 => Type::I32,
        T::I64 => Type::I64,
        T::U8 => Type::U8,
        T::U16 => Type::U16,
        T::U32 => Type::U32,
        T::U64 => Type::U64,
        T::Bool => Type::BOOL,
        T::Unit => Type::UNIT,
        T::Never => Type::NEVER,
        T::ComptimeType => Type::COMPTIME_TYPE,
        T::BuiltinNominal { kind, name } => {
            let symbol = facts.name_symbol(name.as_ref()).ok_or_else(missing)?;
            match kind {
                AK::Struct => Type::new_struct(
                    facts
                        .builtin_or_generated_struct(symbol)
                        .ok_or_else(missing)?,
                ),
                AK::Enum => Type::new_enum(facts.builtin_enum(symbol).ok_or_else(missing)?),
            }
        }
        T::Nominal(N::Builtin { kind, name }) => {
            let symbol = facts.name_symbol(name.as_ref()).ok_or_else(missing)?;
            match kind {
                AK::Struct => Type::new_struct(
                    facts
                        .builtin_or_generated_struct(symbol)
                        .ok_or_else(missing)?,
                ),
                AK::Enum => Type::new_enum(facts.builtin_enum(symbol).ok_or_else(missing)?),
            }
        }
        T::Nominal(N::Named(token)) => {
            let endpoint = facts.definition_endpoint(*token).ok_or_else(missing)?;
            let symbol = facts
                .name_symbol(endpoint.name.as_ref())
                .ok_or_else(missing)?;
            match endpoint.kind {
                StableDefinitionKind::Struct => Type::new_struct(
                    facts
                        .struct_by_file_name(FileId::new(endpoint.file), symbol)
                        .ok_or_else(missing)?,
                ),
                StableDefinitionKind::Enum => Type::new_enum(
                    facts
                        .enum_by_file_name(FileId::new(endpoint.file), symbol)
                        .ok_or_else(missing)?,
                ),
                _ => return Err(missing()),
            }
        }
        T::Nominal(N::Anonymous(identity)) => match identity.kind {
            AK::Struct => Type::new_struct(facts.anon_struct(identity).ok_or_else(missing)?),
            AK::Enum => Type::new_enum(facts.anon_enum(identity).ok_or_else(missing)?),
        },
        T::Array { element, len } => facts
            .intern_array(resolve_instance_type(facts, element)?, *len)
            .ok_or_else(missing)?,
        T::Slice { name, .. } => {
            let symbol = facts.name_symbol(name.as_ref()).ok_or_else(missing)?;
            Type::new_struct(facts.generated_struct(symbol).ok_or_else(missing)?)
        }
        T::PtrConst(value) => facts
            .intern_ptr_const(resolve_instance_type(facts, value)?)
            .ok_or_else(missing)?,
        T::PtrMut(value) => facts
            .intern_ptr_mut(resolve_instance_type(facts, value)?)
            .ok_or_else(missing)?,
        T::Module(token) => {
            let endpoint = facts.module_endpoint(*token).ok_or_else(missing)?;
            let id = facts
                .module_id_for_file(endpoint.file)
                .ok_or_else(missing)?;
            Type::new_module(id)
        }
        T::GenericParameter(_) => return Err(missing()),
    })
}

// ---------------------------------------------------------------------------
// `ProviderEndpointFacts` — the endpoint / definition-selection ProviderFacts
// (RUE-1091 slice r4b-2).
//
// The first provider-driven realization of the family-1A `BodyEndpointProvider`
// seam: where [`EpochFacts`] answers each endpoint op from the semantic epoch's
// `Sema` tables, this driver answers them from the body-scoped identity pool
// (slices 2a/2b/2c — minting nominal `StructId`/`Type` identities from the
// durable metadata a [`DurableNominalSource`] supplies) plus the shared
// whole-program `Rir` (the RIR-index handles), with the live body-fact provider
// ([`BodyFactProvider`]) available for the candidate-set presence check. It is
// the endpoint twin of r4b-1's [`super::call_resolution::ProviderCallFacts`].
//
// The core answer REUSES the provider-generic [`resolve_instance_type`] this
// module already exposes, driven over the pool instead of the epoch: the driver
// is a [`BodyEndpointProvider`] whose ops read the pool + a small overlay token
// space, so `resolve_instance_type(self, key)` walks the exact same
// `TypeInstanceKey` algebra production walks — only the fact SOURCE differs.
// The pool mints internally-consistent ids (the pool keystone); a differential
// compares the resolved type's index-independent render + metadata against the
// LIVE epoch, never a pool-relative index.
//
// RUE-1091 flip-era surface: `pub` because rFinal's whole-body differential and
// the step-4 flip drive the provider path from rue-compiler, where the pool's
// durable source is built from concrete nucleus signatures (an opaque
// `BodyFactProvider` associated type rue-air cannot destructure). The sole
// pre-flip caller is the rue-compiler differential; the flip promotes it to the
// production analyzer.
//
// Feasibility (r4a design-checkpoint table): P = answered-by-pool, C =
// composed-from-provider, R = RIR-index answer.
//   - `resolve_instance_type` (every pool-supported `TypeInstanceKey` arm)  → P
//   - `first_free_function` / `named_method_declaration` / `destructor`     → R
//   - `nominal_contains_in_module`                                          → C
// Deferred here, each with its unblocking slice named (reported, never silently
// answered wrong):
//   - `named_method_declaration` → LANDED (flip-prep): the production seam now
//     takes the provider-natural `(owner_file, owner_type_name, method_name)`
//     preimage already computed by its analyzer caller, so this driver answers
//     it directly from the RIR index without minting a pool `StructId`.
//   - `function_info` / `function_by_file_name` → r4b-1's `ProviderCallFacts`
//     (the call family); `method_info` → r4b-3's `ProviderCallFacts::method_info`
//     (receiver→pool identity now threaded through the durable method key). Both
//     stay `None` on THIS endpoint driver — they belong to the call family.
//   - `module_endpoint` / `module_id_for_file` (the `Module` arm) → O: answered
//     by the body-local module overlay registered from the durable module
//     identity + its current request file.
//   - `anon_struct` / `anon_enum` (the anonymous arm) is ANSWERED as of r6b: a
//     caller seeds the issued→durable seam with `register_anonymous_nominal`
//     and the arms mint through the pool's `find_or_create_anon`; an unseeded
//     issued key still fails closed. The well-known `Option` facts are ANSWERED
//     as of r6c: `install_well_known_option_types` ports the trusted registry
//     onto the same machinery, recording the export-as-produced ruling.
//   - `generated_struct` (the `Slice` arm) is ANSWERED as of r6a: a caller seeds
//     each generated slice with `register_generated_slice` and the arm resolves
//     the minted fat-pointer struct. Builtin names beyond the pre-registered
//     `BUILTIN_ENUMS` + `str` set (`Str(N)`) → r6b (generated-struct
//     classification with the anonymous / generated family).
//   - `source_function_name` under specialization → r5; identity otherwise.
// ---------------------------------------------------------------------------

/// The issuer stamped on every [`SemanticDefinitionToken`] this driver mints
/// into its overlay token space. The value is arbitrary but must be stable and
/// distinct so a token minted here never aliases an epoch-issued one; the token
/// is only ever reversed through this driver's own overlay, never an epoch
/// table.
const OVERLAY_ISSUER: u64 = 0x0000_04b2_0000_0001;

/// One overlay endpoint entry: the `(file, name, kind)` the `Named`-nominal arm
/// of [`resolve_instance_type`] reads back through
/// [`BodyEndpointProvider::definition_endpoint`]. The durable key the entry
/// stands for lives in [`EndpointOverlay::by_file_name`], keyed by the same
/// `(file, name)` preimage the endpoint yields.
struct EndpointEntry {
    file: u32,
    name: Arc<str>,
    kind: StableDefinitionKind,
}

/// The driver's overlay token space: the provider-side analog of the epoch's
/// `stable_definition_endpoints` + `structs_by_file_name`/`enums_by_file_name`
/// tables, populated on demand as a differential mints a token per durable
/// nominal key. A token reverses to its `(file, name, kind)` endpoint; the
/// `(file, name)` preimage reverses to the durable key the pool mints from —
/// keyed on the pool's own interner [`Spur`], the symbol
/// [`BodyEndpointProvider::name_symbol`] hands back.
struct EndpointOverlay<K> {
    next_slot: u32,
    tokens: HashMap<SemanticDefinitionToken, EndpointEntry>,
    by_file_name: HashMap<(u32, Spur), K>,
    module_tokens: HashMap<SemanticModuleToken, SemanticModuleEndpoint>,
    /// Generated slice-struct identities minted on demand (RUE-1091 r6a): the
    /// provider-side analog of the epoch's `generated_structs` name→id map. A
    /// caller seeds one per `[T]` slice with [`ProviderEndpointFacts::
    /// register_generated_slice`] (exactly as it seeds a named nominal with
    /// [`ProviderEndpointFacts::register_named_nominal`]), so the `Slice` arm of
    /// [`resolve_instance_type`] resolves the generated-struct name the epoch
    /// mints during declaration gathering.
    generated_slices: HashMap<Spur, StructId>,
}

impl<K> Default for EndpointOverlay<K> {
    fn default() -> Self {
        Self {
            next_slot: 0,
            tokens: HashMap::new(),
            by_file_name: HashMap::new(),
            module_tokens: HashMap::new(),
            generated_slices: HashMap::new(),
        }
    }
}

/// The endpoint-resolution ProviderFacts driver: answers the family-1A endpoint
/// ops from a body identity pool + RIR index + [`BodyFactProvider`], instead of
/// the epoch `Sema` tables [`EpochFacts`] reads.
///
/// Generic over the provider `P`, the pool durable source `S`, and the pool's
/// durable nominal key `K` and module `M` (rue-compiler binds
/// `K = StableDefinitionKey`, `M = ModuleId`). The pool lives behind a
/// [`RefCell`] because a nominal is minted on first consult (`&mut` on the
/// pool) while [`BodyEndpointProvider`]'s ops — and therefore the shared
/// [`resolve_instance_type`] logic driving them — are `&self`; the borrow is
/// never held across a re-entrant consult, so it never conflicts.
pub struct ProviderEndpointFacts<'a, P, S, K, M> {
    provider: &'a P,
    identity: ProviderIdentityContext<K, M, S>,
    rir: &'a Rir,
    rir_index: BodyRirIndex,
    /// The whole-program RIR interner. The RIR-index ops resolve their `&str`
    /// keys through this interner (the shared `Rir`'s symbol space), distinct
    /// from the pool's own interner the nominal ops key on.
    rir_interner: &'a ThreadedRodeo,
    overlay: RefCell<EndpointOverlay<K>>,
    /// The issued→durable anonymous seam (RUE-1091 r6b): the anonymous producer
    /// key `resolve_instance_type` carries is in the issued-token domain, so a
    /// caller seeds the durable key it stands for with
    /// [`Self::register_anonymous_nominal`], exactly as it seeds a named nominal
    /// with [`Self::register_named_nominal`]. The `anon_struct`/`anon_enum` arms
    /// then mint through the pool's [`BodyIdentityPool::find_or_create_anon`].
    anon_by_issued: RefCell<HashMap<IssuedAnonymousNominalKey, crate::AnonymousNominalKey<K, M>>>,
}

impl<'a, P, S, K, M> ProviderEndpointFacts<'a, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M> + DurableAnonymousSource<K, M>,
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
{
    /// Construct the driver over a provider, a durable nominal source, and the
    /// shared whole-program `Rir` + interner. The pool and RIR index are built
    /// here; nominals are minted lazily on first consult and the overlay token
    /// space starts empty (a caller mints a token per nominal with
    /// [`Self::register_named_nominal`]).
    pub fn new(provider: &'a P, source: S, rir: &'a Rir, rir_interner: &'a ThreadedRodeo) -> Self {
        Self::with_identity(
            provider,
            ProviderIdentityContext::new(source),
            rir,
            rir_interner,
        )
    }

    /// Construct the driver inside an existing per-body identity universe.
    pub fn with_identity(
        provider: &'a P,
        identity: ProviderIdentityContext<K, M, S>,
        rir: &'a Rir,
        rir_interner: &'a ThreadedRodeo,
    ) -> Self {
        Self {
            provider,
            identity,
            rir,
            rir_index: BodyRirIndex::new(rir),
            rir_interner,
            overlay: RefCell::new(EndpointOverlay::default()),
            anon_by_issued: RefCell::new(HashMap::new()),
        }
    }

    /// Seed the durable anonymous producer key an issued-token anonymous key
    /// stands for, so the `anon_struct` / `anon_enum` arms of
    /// [`resolve_instance_type`] mint it through the pool (RUE-1091 r6b). The
    /// caller supplies the same durable identity the epoch's
    /// `import_anonymous_identity` reverses the issued key to; the pool then
    /// spells the byte-identical `__anon_*_{digest}` name from that durable key's
    /// stable relocation.
    pub fn register_anonymous_nominal(
        &self,
        issued: IssuedAnonymousNominalKey,
        durable: crate::AnonymousNominalKey<K, M>,
    ) {
        self.anon_by_issued.borrow_mut().insert(issued, durable);
    }

    /// Mint (or dedup) the anonymous nominal for a durable identity key directly,
    /// the inherent form the differential compares against the LIVE epoch's
    /// `find_or_create_anon_struct`/`_enum`. Returns the minted pool [`Type`], or
    /// `None` if the durable key names no anonymous shape / a digest collision
    /// refuses it (fail-closed).
    pub fn mint_anonymous(&self, durable: &crate::AnonymousNominalKey<K, M>) -> Option<Type> {
        self.identity.pool_mut()?.find_or_create_anon(durable).ok()
    }

    /// Install the per-body well-known `Option(payload)` registry (RUE-1112)
    /// through the pool — the provider-facing port of the epoch's
    /// `BoundSema::install_well_known_option_types` (RUE-1091 r6c). `nominals`
    /// are the durable identities of the trusted-std `Option` enums the
    /// per-body demand loop resolved; each mints through the pool's ordinary
    /// anonymous machinery so its full materialization is byte-identical to
    /// the epoch install's, and each is recorded under the export-as-produced
    /// ruling: the installing body EXPORTS these identities as produced
    /// anonymous nominals (the flip-era baseline subtraction consults
    /// [`Self::is_well_known_option_identity`]), never leaking them as
    /// imports. `option_by_payload` records the demand map fallible-intrinsic
    /// resolution consults. Fail-closed: `None` on any refusal (non-enum
    /// shape, absent shape, digest collision, a registry pair naming an
    /// identity the install never minted, unresolvable registry type) —
    /// a refusal is fatal for the requesting body, exactly as the epoch's
    /// failed install fails the body query deterministically. A refusal also
    /// POISONS the pool's well-known registry: repeat installs re-error (still
    /// `None` here) and the accessors below answer as if nothing was installed
    /// — no observable partial success, matching the atomicity of the epoch's
    /// by-value install (which drops the whole mutated `BoundSema` on failure).
    pub fn install_well_known_option_types(
        &self,
        nominals: &[crate::AnonymousNominalKey<K, M>],
        option_by_payload: &[(SemanticImportType<K, M>, SemanticImportType<K, M>)],
    ) -> Option<()>
    where
        K: Ord,
        M: Ord,
    {
        self.identity
            .pool_mut()?
            .install_well_known_option_types(nominals, option_by_payload)
            .ok()
    }

    /// Whether a durable identity (any producer spelling — entry
    /// canonicalization applies) is a well-known `Option` identity installed
    /// on this driver's pool: the export-as-produced ruling the flip-era
    /// baseline subtraction consults (RUE-1091 r6c).
    pub fn is_well_known_option_identity(&self, durable: &crate::AnonymousNominalKey<K, M>) -> bool
    where
        K: Ord,
        M: Ord,
    {
        self.identity.pool().is_well_known_option_identity(durable)
    }

    /// The number of well-known `Option` identities installed on this driver's
    /// pool.
    pub fn well_known_option_identity_count(&self) -> usize {
        self.identity.pool().well_known_option_identity_count()
    }

    /// The trusted std `Option` enum minted for a demanded payload type, or
    /// `None` when the payload was never demanded — the pool-backed answer to
    /// the epoch's `well_known_option_by_payload` consult
    /// (`resolve_option_result_type`).
    pub fn well_known_option_for_payload(&self, payload: Type) -> Option<Type> {
        self.identity.pool().well_known_option_for_payload(payload)
    }

    /// Mint an overlay [`SemanticDefinitionToken`] standing for a durable nominal
    /// key, recording the `(file, name, kind)` endpoint and the `(file, name)`
    /// preimage the `Named`-nominal arm of [`resolve_instance_type`] reverses. A
    /// caller supplies the same `(file, name, kind)` the epoch's stable endpoint
    /// carries, so the driver's token space stays a faithful stand-in for the
    /// epoch's `stable_definition_endpoints` without holding any epoch handle.
    pub fn register_named_nominal(
        &self,
        key: K,
        file: u32,
        name: &str,
        kind: StableDefinitionKind,
    ) -> SemanticDefinitionToken {
        let symbol = self.identity.pool().intern_name(name);
        let mut overlay = self.overlay.borrow_mut();
        let slot = overlay.next_slot;
        overlay.next_slot += 1;
        let token = SemanticDefinitionToken::new(OVERLAY_ISSUER, slot);
        overlay.tokens.insert(
            token,
            EndpointEntry {
                file,
                name: Arc::from(name),
                kind,
            },
        );
        overlay.by_file_name.insert((file, symbol), key);
        token
    }

    /// Mint (or return) the body-local module token and compact module id for a
    /// durable module identity at its current request file. This is the
    /// provider-side analog of the epoch's `stable_module_endpoints` +
    /// `module_registry`: the durable identity prevents two registrations of
    /// one module from minting distinct units, while the file is the exact
    /// declaration-level request fact consumed by the endpoint lookup.
    ///
    /// A conflicting durable→file or file→durable registration is rejected
    /// rather than guessed. The flip fills this from the admitted durable module
    /// set keyed by `BodyFactProvider::ModuleRef`, paired with the canonical
    /// module projection's current file handle.
    pub fn register_module(
        &self,
        module: M,
        file: FileId,
        file_path: &str,
        import_path: &str,
        durable_id: &str,
    ) -> Option<SemanticModuleToken> {
        let id = self.identity.modules_mut().register(
            module,
            file,
            file_path,
            import_path,
            durable_id,
        )?;
        let token = SemanticModuleToken::new(OVERLAY_ISSUER, id.index());
        let mut overlay = self.overlay.borrow_mut();
        overlay.module_tokens.insert(
            token,
            SemanticModuleEndpoint {
                token,
                file: file.index(),
            },
        );
        Some(token)
    }

    /// Recover the current request file for a module type minted by this
    /// driver's body-local registry. Used by the cross-path differential to
    /// compare index-independent module identity.
    pub fn module_file(&self, ty: Type) -> Option<FileId> {
        let id = ty.as_module()?;
        self.identity
            .modules()
            .get(id)
            .map(|definition| definition.file_id)
    }

    /// Mint (on first sight) the generated slice struct for a `[T]` slice and
    /// record its name→id under the pool's own interner, so the `Slice` arm of
    /// [`resolve_instance_type`] resolves the generated-struct name through
    /// [`BodyEndpointProvider::generated_struct`] (RUE-1091 r6a — the builtin /
    /// slice name facts). The pool mints the fat-pointer struct byte-identically
    /// to the epoch's `get_or_create_slice_struct_from_element`
    /// (`import_type_local`'s slice arm), and dedups on repeat, so a second
    /// consult of the same `(element, name)` returns the same id and mints
    /// nothing new. The `element` is the slice's durable element type; a caller
    /// supplies the same durable element the epoch's slice carries.
    pub fn register_generated_slice(
        &self,
        element: &SemanticImportType<K, M>,
        name: &str,
    ) -> Option<StructId>
    where
        M: Clone,
    {
        let symbol = self.identity.pool().intern_name(name);
        let id = self
            .identity
            .pool_mut()?
            .resolve(&SemanticImportType::Slice {
                element: Box::new(element.clone()),
                name: Arc::from(name),
            })
            .ok()?
            .as_struct()?;
        self.overlay
            .borrow_mut()
            .generated_slices
            .insert(symbol, id);
        Some(id)
    }

    /// (P) Materialize a canonical type-instance key into a concrete pool
    /// [`Type`], reusing the provider-generic [`resolve_instance_type`] driven
    /// over this pool-backed [`BodyEndpointProvider`]. Every arm the pool
    /// supports resolves; a deferred arm (generic parameter, an unregistered
    /// anonymous mint, `Str(N)` builtin name) fails closed to
    /// `MissingStableIdentity`, exactly as the pool refuses it. Generated slice
    /// names resolve once seeded with [`Self::register_generated_slice`] (r6a).
    pub fn resolve_instance_type(
        &self,
        value: &TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    ) -> Result<Type, SemanticBodyExportFailure> {
        resolve_instance_type(self, value)
    }

    /// (R) The first free-function RIR declaration for `(source_name, file)`,
    /// answered by [`BodyRirIndex`] over the shared `Rir` — the exact `InstRef`
    /// [`EpochFacts::first_free_function`] returns, keyed provider-naturally by
    /// the source name string.
    pub fn first_free_function(&self, source_name: &str, file: FileId) -> Option<InstRef> {
        let symbol = self.rir_interner.get(source_name)?;
        self.rir_index.first_free_function(symbol, file)
    }

    /// (R) The named-method RIR declaration for `(owner_file, owner_type_name,
    /// method)` — the durable-available preimage of the epoch's `(StructId,
    /// method)` key, answered by [`BodyRirIndex`]. Keyed by the preimage directly
    /// (provider-natural), matching the production seam.
    pub fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<InstRef> {
        let owner = self.rir_interner.get(owner_type_name)?;
        let method_sym = self.rir_interner.get(method)?;
        self.rir_index
            .named_method_declaration(owner_file, owner, method_sym)
    }

    /// (R) The destructor RIR declaration handle for `(file, type_name)`, the
    /// exact scan [`EpochFacts::destructor`] performs, answered by
    /// [`BodyRirIndex`]. Returns the located declaration `InstRef` (the private
    /// destructor record's public handle); the epoch keys the same record by the
    /// same `(file, type_name)` scan.
    pub fn destructor(&self, file: FileId, type_name: &str) -> Option<InstRef> {
        let symbol = self.rir_interner.get(type_name)?;
        self.rir_index
            .destructor(file.index(), symbol)
            .map(|record| record.declaration)
    }

    /// (C) Whether a source name resolves to a declared nominal of `want`
    /// (struct or enum) in `module` — the provider `lookup_unqualified`
    /// (ModuleItem) analog of the epoch's `structs_by_file_name` /
    /// `enums_by_file_name` membership. Selection (the kind filter) stays here
    /// against the returned candidate set, honoring the boundary's
    /// candidate-sets-not-winners contract; a unique or ambiguous candidate of
    /// the wanted kind counts as present, exactly as a populated epoch table
    /// entry does.
    pub fn nominal_contains_in_module(
        &self,
        module: &P::ModuleRef,
        source_name: &str,
        want: crate::AnonymousNominalKind,
    ) -> bool {
        use super::provider::{NameResolution, ProviderDefinitionKind, ProviderNamespace};
        let resolution =
            self.provider
                .lookup_unqualified(module, ProviderNamespace::ModuleItem, source_name);
        let kind = match want {
            crate::AnonymousNominalKind::Struct => ProviderDefinitionKind::Struct,
            crate::AnonymousNominalKind::Enum => ProviderDefinitionKind::Enum,
        };
        matches!(
            resolution.of_kind(kind),
            NameResolution::Unique(_) | NameResolution::Ambiguous(_)
        )
    }

    /// Read the body-local minted [`TypeInternPool`] under a closure, so a
    /// differential renders a resolved pool [`Type`] index-independently (the
    /// pool mints its own ids; parity is asserted through displays / metadata,
    /// never a pool-relative index — the 2a/2b contract). Closure-scoped because
    /// the pool lives behind a [`RefCell`].
    pub fn with_type_pool<R>(&self, read: impl FnOnce(&TypeInternPool) -> R) -> R {
        let pool = self.identity.type_pool();
        read(&pool)
    }

    /// Freeze the pool's containment metadata — the pool-side `freeze()` seam
    /// hook the r4a-2a rider defers to the slice that wires the pool under body
    /// analysis (RUE-1091 rFinal). A caller invokes this at the same point
    /// production calls `finalize_containment_metadata` (after every nominal
    /// the body consumes has been minted, before any drop/ownership read).
    /// Freezing is shared by all drivers in this identity context: later mint
    /// attempts fail closed instead of invalidating the finalized metadata.
    /// `None` on a containment cycle (fail-closed). Before the freeze,
    /// [`Self::type_needs_drop`] / [`Self::type_carries_linear`] answer `None`.
    pub fn finalize_containment_metadata(&self) -> Option<()> {
        self.identity.finalize_containment_metadata()
    }

    /// Whether a minted type transitively needs drop, or `None` before the
    /// [`Self::finalize_containment_metadata`] freeze.
    pub fn type_needs_drop(&self, ty: Type) -> Option<bool> {
        self.identity.pool().type_needs_drop(ty)
    }

    /// Whether a minted type transitively carries a linear component, or `None`
    /// before the freeze.
    pub fn type_carries_linear(&self, ty: Type) -> Option<bool> {
        self.identity.pool().type_carries_linear(ty)
    }
}

impl<P, S, K, M> ProviderEndpointFacts<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M> + DurableAnonymousSource<K, M> + DurableConstSource<K, M>,
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
{
    /// Assemble the value constant identified by durable `key`, joining it to
    /// the exact current-RIR declaration at `(declaring_file, source_name)`.
    ///
    /// The RIR side supplies only the request-local span. The pool side supplies
    /// only declaration-level durable type/value truth. If either side is absent,
    /// or a nested identity cannot be minted exactly, this returns `None`.
    pub fn const_info(
        &self,
        key: &K,
        declaring_file: FileId,
        source_name: &str,
    ) -> Option<ConstInfo> {
        let name = self.rir_interner.get(source_name)?;
        let declaration = self.rir_index.const_declaration(declaring_file, name)?;
        self.identity
            .pool_mut()?
            .resolve_const(
                key,
                ConstIdentityHandle {
                    span: self.rir.get(declaration).span,
                },
            )
            .ok()
    }

    /// Assemble a module-valued constant from the exact declaration span and a
    /// module identity already admitted to this body's shared registry.
    pub fn module_binding_info(
        &self,
        declaring_file: FileId,
        source_name: &str,
        target: &M,
        is_public: bool,
    ) -> Option<ConstInfo> {
        let name = self.rir_interner.get(source_name)?;
        let declaration = self.rir_index.const_declaration(declaring_file, name)?;
        let module = self.identity.modules().id_for_durable(target)?;
        let ty = Type::new_module(module);
        Some(ConstInfo {
            is_pub: is_public,
            ty,
            value: super::ConstValue::Type(ty),
            span: self.rir.get(declaration).span,
        })
    }

    /// Resolve a pool-owned const symbol for index-independent differential
    /// comparison. Function and string values use the pool's own interner.
    pub fn resolve_const_symbol(&self, symbol: Spur) -> String {
        self.identity.pool().resolve_symbol(symbol).to_owned()
    }
}

impl<P, S, K, M> BodyEndpointProvider for ProviderEndpointFacts<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M> + DurableAnonymousSource<K, M>,
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
{
    fn name_symbol(&self, name: &str) -> Option<Spur> {
        Some(self.identity.pool().intern_name(name))
    }

    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint> {
        let overlay = self.overlay.borrow();
        let entry = overlay.tokens.get(&token)?;
        Some(SemanticDefinitionEndpoint {
            token,
            file: entry.file,
            name: entry.name.clone(),
            kind: entry.kind,
            owner: None,
        })
    }

    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint> {
        self.overlay.borrow().module_tokens.get(&token).copied()
    }

    fn function_by_file_name(&self, _file: FileId, _name: Spur) -> Option<Spur> {
        // The call family: r4b-1's `ProviderCallFacts` answers the free-function
        // identity; the `(file, name)`-keyed seam is r4b-3. Not consulted by
        // `resolve_instance_type`.
        None
    }

    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        let key = self
            .overlay
            .borrow()
            .by_file_name
            .get(&(file.index(), name))
            .cloned()?;
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_struct()
    }

    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        let key = self
            .overlay
            .borrow()
            .by_file_name
            .get(&(file.index(), name))
            .cloned()?;
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_enum()
    }

    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        let owned = self.identity.pool().resolve_symbol(name).to_owned();
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::BuiltinNominal {
                name: Arc::from(owned.as_str()),
                kind: SemanticImportNominalKind::Struct,
            })
            .ok()
            .and_then(|ty| ty.as_struct())
            // Mirror the epoch's `builtin_structs.or_else(generated_structs)`:
            // a generated slice struct answers here too (RUE-1091 r6a).
            .or_else(|| self.generated_struct(name))
    }

    fn generated_struct(&self, name: Spur) -> Option<StructId> {
        // The generated slice-struct name, minted and recorded by
        // `register_generated_slice` (RUE-1091 r6a — builtin / slice name
        // facts). A name never seeded fails closed, exactly as the epoch's
        // `generated_structs.get` misses.
        self.overlay.borrow().generated_slices.get(&name).copied()
    }

    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        let name = self.identity.pool().resolve_symbol(name).to_owned();
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::BuiltinNominal {
                name: Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Enum,
            })
            .ok()?
            .as_enum()
    }

    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        // RUE-1091 r6b: mint the anonymous struct through the pool for a durable
        // key seeded by `register_anonymous_nominal`. An unseeded issued key fails
        // closed (the pool never invents an identity), mirroring the epoch's
        // `anon_struct_identities.get` miss.
        let durable = self.anon_by_issued.borrow().get(identity).cloned()?;
        self.identity
            .pool_mut()?
            .find_or_create_anon(&durable)
            .ok()?
            .as_struct()
    }

    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        let durable = self.anon_by_issued.borrow().get(identity).cloned()?;
        self.identity
            .pool_mut()?
            .find_or_create_anon(&durable)
            .ok()?
            .as_enum()
    }

    fn is_builtin_or_generated_struct(&self, name: Spur) -> bool {
        self.builtin_or_generated_struct(name).is_some()
    }

    fn is_builtin_enum(&self, name: Spur) -> bool {
        self.builtin_enum(name).is_some()
    }

    fn function_info(&self, _name: Spur) -> Option<FunctionInfo> {
        // The call family: r4b-1's `ProviderCallFacts::function_info`.
        None
    }

    fn method_info(&self, _struct_id: StructId, _name: Spur) -> Option<MethodInfo> {
        // r4b-3: the receiver→pool identity the endpoint seam owns.
        None
    }

    fn source_function_name(&self, name: Spur) -> Spur {
        // Identity: the specialization name map is r5.
        name
    }

    fn first_free_function(&self, _source: Spur, _file_id: FileId) -> Option<InstRef> {
        // Answered provider-naturally by the inherent
        // `first_free_function(&str, FileId)`; the `Spur`-keyed trait op keys on
        // an ambiguous interner and is not consulted by `resolve_instance_type`.
        None
    }

    fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: Spur,
        method_name: Spur,
    ) -> Option<InstRef> {
        self.rir_index
            .named_method_declaration(owner_file, owner_type_name, method_name)
    }

    fn destructor(&self, _file: u32, _type_name: Spur) -> Option<RirDestructorDeclaration> {
        // Answered provider-naturally by the inherent `destructor(FileId, &str)`.
        None
    }

    fn module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        self.identity.modules().id_for_file(FileId::new(file))
    }

    fn intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.identity
            .pool()
            .type_pool()
            .try_intern_array(element, len)
            .ok()
    }

    fn intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.identity
            .pool()
            .type_pool()
            .try_intern_ptr_const(pointee)
            .ok()
    }

    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.identity
            .pool()
            .type_pool()
            .try_intern_ptr_mut(pointee)
            .ok()
    }
}
