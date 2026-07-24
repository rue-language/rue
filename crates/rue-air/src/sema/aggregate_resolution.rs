//! Provider-generic aggregate, field, and variant resolution.
//!
//! Selection order lives here. [`EpochFacts`] is the production adapter that
//! reproduces the declaration-epoch reads used by body analysis.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;

use lasso::Spur;
use rue_rir::{InstData, InstRef, Rir};
use rue_span::FileId;

use super::body_identity::{BodyIdentityPool, DurableNominalSource};
use super::context::{ConstValue, LocalVar};
use super::{ConstInfo, DeclarationPhase, Sema};
use crate::intern_pool::TypeInternPool;
use crate::types::{EnumId, ModuleId, StructId, Type};
use crate::{SemanticImportNominalKind, SemanticImportType};

pub(crate) trait AggregateFacts {
    fn value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo>;
    fn module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo>;
    fn struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId>;
    fn enum_in_file(&self, file: FileId, name: Spur) -> Option<EnumId>;
    fn builtin_struct(&self, name: Spur) -> Option<StructId>;
    fn builtin_enum(&self, name: Spur) -> Option<EnumId>;
    fn module(&self, module: ModuleId) -> AggregateModuleFact;
    fn file_path(&self, file: FileId) -> Option<&str>;
    fn source_path(&self, span: rue_span::Span) -> Option<&str>;
}

pub(crate) struct AggregateModuleFact {
    pub(crate) file: FileId,
    file_path: String,
    import_path: String,
}

impl AggregateModuleFact {
    pub(crate) fn file_path(&self) -> &str {
        &self.file_path
    }

    pub(crate) fn import_path(&self) -> &str {
        &self.import_path
    }
}

pub(crate) struct EpochFacts<'s, 'a, D: DeclarationPhase> {
    sema: &'s Sema<'a, D>,
}

impl<'s, 'a, D: DeclarationPhase> EpochFacts<'s, 'a, D> {
    pub(crate) fn new(sema: &'s Sema<'a, D>) -> Self {
        Self { sema }
    }
}

impl<D: DeclarationPhase> AggregateFacts for EpochFacts<'_, '_, D> {
    fn value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.sema.value_const(&(file, name)).cloned()
    }

    fn module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.sema.module_binding(&(file, name)).cloned()
    }

    fn struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.sema.structs_by_file_name.get(&(file, name)).copied()
    }

    fn enum_in_file(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.sema.enums_by_file_name.get(&(file, name)).copied()
    }

    fn builtin_struct(&self, name: Spur) -> Option<StructId> {
        self.sema.resolve_builtin_struct_name(name)
    }

    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.sema.resolve_builtin_enum_name(name)
    }

    fn module(&self, module: ModuleId) -> AggregateModuleFact {
        let def = self.sema.module_registry.get_def(module);
        AggregateModuleFact {
            file: def.file_id,
            file_path: def.file_path,
            import_path: def.import_path,
        }
    }

    fn file_path(&self, file: FileId) -> Option<&str> {
        self.sema.get_file_path(file)
    }

    fn source_path(&self, span: rue_span::Span) -> Option<&str> {
        self.sema.get_source_path(span)
    }
}

pub(crate) enum StructLiteralHead {
    Bound(Type),
    Named(StructId),
    Absent,
}

pub(crate) enum ModuleTypeMember {
    Struct(StructId),
    Enum(EnumId),
    Const(ConstInfo),
    Absent,
}

#[derive(Clone, Copy)]
pub(crate) enum QualifiedType {
    Enum(EnumId),
    Struct(StructId),
    Absent,
}

pub(crate) fn resolve_enum_type_name<P: AggregateFacts>(
    facts: &P,
    local_type: Option<Type>,
    file: FileId,
    name: Spur,
) -> Option<(EnumId, bool)> {
    if let Some(ty) = local_type {
        return ty.as_enum().map(|id| (id, true));
    }
    if let Some(info) = facts.value_const(file, name)
        && let ConstValue::Type(ty) = info.value
    {
        return ty.as_enum().map(|id| (id, true));
    }
    facts
        .enum_in_file(file, name)
        .or_else(|| facts.builtin_enum(name))
        .map(|id| (id, false))
}

pub(crate) fn resolve_struct_type_name<P: AggregateFacts>(
    facts: &P,
    local_type: Option<Type>,
    file: FileId,
    name: Spur,
) -> Option<(StructId, bool)> {
    if let Some(ty) = local_type {
        return ty.as_struct().map(|id| (id, true));
    }
    if let Some(info) = facts.value_const(file, name)
        && let ConstValue::Type(ty) = info.value
    {
        return ty.as_struct().map(|id| (id, true));
    }
    facts
        .struct_in_file(file, name)
        .or_else(|| facts.builtin_struct(name))
        .map(|id| (id, false))
}

pub(crate) fn select_struct_literal_head<P: AggregateFacts>(
    facts: &P,
    local_type: Option<Type>,
    file: FileId,
    name: Spur,
) -> StructLiteralHead {
    if let Some(ty) = local_type {
        return StructLiteralHead::Bound(ty);
    }
    if let Some(info) = facts.value_const(file, name)
        && let ConstValue::Type(ty) = info.value
    {
        return StructLiteralHead::Bound(ty);
    }
    facts
        .struct_in_file(file, name)
        .or_else(|| facts.builtin_struct(name))
        .map_or(StructLiteralHead::Absent, StructLiteralHead::Named)
}

pub(crate) fn select_module_type_member<P: AggregateFacts>(
    facts: &P,
    file: FileId,
    name: Spur,
) -> ModuleTypeMember {
    if let Some(id) = facts.struct_in_file(file, name) {
        return ModuleTypeMember::Struct(id);
    }
    if let Some(id) = facts.enum_in_file(file, name) {
        return ModuleTypeMember::Enum(id);
    }
    if let Some(info) = facts
        .module_binding(file, name)
        .or_else(|| facts.value_const(file, name))
    {
        return ModuleTypeMember::Const(info);
    }
    ModuleTypeMember::Absent
}

pub(crate) fn select_qualified_type<P: AggregateFacts>(
    facts: &P,
    file: FileId,
    name: Spur,
) -> QualifiedType {
    if let Some(id) = facts.enum_in_file(file, name) {
        return QualifiedType::Enum(id);
    }
    facts
        .struct_in_file(file, name)
        .map_or(QualifiedType::Absent, QualifiedType::Struct)
}

pub(crate) fn select_qualified_enum<P: AggregateFacts>(
    facts: &P,
    file: FileId,
    name: Spur,
) -> Option<EnumId> {
    facts.enum_in_file(file, name)
}

pub(crate) fn resolve_aggregate_module_ref<P: AggregateFacts>(
    facts: &P,
    rir: &Rir,
    inst_ref: InstRef,
    root_file: FileId,
    locals: &std::collections::HashMap<Spur, LocalVar>,
) -> Option<ModuleId> {
    match rir.get(inst_ref).data {
        InstData::VarRef { name, .. } => {
            if let Some(local) = locals.get(&name) {
                if let Some(module_id) = local.ty.as_module() {
                    return Some(module_id);
                }
            }
            facts
                .module_binding(root_file, name)
                .and_then(|binding| binding.ty.as_module())
        }
        InstData::FieldGet { base, field } => {
            let parent = resolve_aggregate_module_ref(facts, rir, base, root_file, locals)?;
            let parent_file = facts.module(parent).file;
            facts
                .module_binding(parent_file, field)
                .and_then(|binding| binding.ty.as_module())
        }
        _ => None,
    }
}

pub(crate) fn resolve_visibility_module_ref<P: AggregateFacts>(
    facts: &P,
    rir: &Rir,
    inst_ref: InstRef,
    locals: &std::collections::HashMap<Spur, LocalVar>,
) -> Option<ModuleId> {
    let inst = rir.get(inst_ref);
    let module_ty = match inst.data {
        InstData::VarRef { name, .. } => locals.get(&name).map(|local| local.ty).or_else(|| {
            facts
                .module_binding(inst.span.file_id, name)
                .map(|binding| binding.ty)
        }),
        InstData::FieldGet { base, field } => {
            let parent = resolve_visibility_module_ref(facts, rir, base, locals)?;
            let parent_file = facts.module(parent).file;
            facts
                .module_binding(parent_file, field)
                .map(|binding| binding.ty)
        }
        _ => None,
    }?;
    module_ty.as_module()
}

pub(crate) fn is_accessible<P: AggregateFacts>(
    facts: &P,
    accessing_file: FileId,
    defining_file: FileId,
    is_public: bool,
) -> bool {
    let accessing =
        crate::SemanticVisibilityDomain::from_file_path(facts.file_path(accessing_file));
    let defining = crate::SemanticVisibilityDomain::from_file_path(facts.file_path(defining_file));
    defining.is_visible_from(&accessing, is_public)
}

// ---------------------------------------------------------------------------
// `ProviderAggregateFacts` — the aggregate / field / variant ProviderFacts
// (RUE-1091 slice r4b-3).
//
// The provider-driven realization of the family-1D [`AggregateFacts`] seam: where
// [`EpochFacts`] answers each aggregate op from the semantic epoch's `Sema`
// tables, this driver answers them from the body-scoped identity pool (slice
// 2a — minting nominal `StructId`/`EnumId` identities from the durable metadata a
// [`DurableNominalSource`] supplies) plus a small caller-populated overlay (the
// `(file, name) → durable key` reverse and the request-local file paths). It is
// the aggregate sibling of r4b-1's [`super::call_resolution::ProviderCallFacts`]
// and r4b-2's [`super::body_endpoint::ProviderEndpointFacts`].
//
// The selection ORDER is not this driver's concern: it lives in the
// provider-generic free functions above ([`select_module_type_member`]'s
// struct→enum→const short-circuit, [`select_qualified_type`]'s enum→struct,
// [`select_struct_literal_head`]'s bound→const→struct→builtin) which this driver
// merely supplies facts to, so `EpochFacts` and this driver replay the exact same
// r1c candidate order and short-circuits (RUE-1091 risk R1). The driver's
// inherent `select_*` wrappers run those free functions over itself and hand back
// the winner as a pool [`Type`] a differential renders index-independently.
//
// RUE-1091 flip-era surface: `pub` because rFinal's whole-body differential and
// the step-4 flip drive the provider path from rue-compiler, where the pool's
// durable source is built from concrete nucleus signatures (an opaque
// `BodyFactProvider` associated type rue-air cannot destructure). The sole
// pre-flip caller is the rue-compiler differential; the flip promotes it to the
// production analyzer. No aggregate deliverable op consults the live
// `BodyFactProvider` boundary — struct/enum-by-file-name, the builtins, and
// `is_accessible` are all answered by the pool + the caller-populated overlay —
// so this driver holds no provider handle and records no provider edge by
// construction (stronger than the sibling drivers' empty-edge assertion).
//
// Feasibility (r4a design-checkpoint table): P = answered-by-pool, O =
// overlay-held request-local fact.
//   - struct_in_file / enum_in_file (via the `(file, name)` overlay reverse) → P
//   - builtin_struct / builtin_enum                                         → P
//   - file_path (is_accessible)                                             → O
// Deferred here, each with its unblocking slice named (reported, never silently
// answered wrong):
//   - value_const / module_binding / resolve_const_info_in_file → the flip. The
//     pool's const-minting arm and exact RIR declaration handle now exist
//     (`body_identity.rs`, flip-prep), but this aggregate driver deliberately
//     remains unwired pre-flip. The const fall-through of every `select_*` above
//     therefore answers `Absent` where the epoch answers `Const`; the
//     differential pins this const-arm divergence.
//   - module (module_def) and the module spines
//     ([`resolve_aggregate_module_ref`] / [`resolve_visibility_module_ref`]) →
//     the flip: `module` answers a rue-air-internal `ModuleId` registry index the
//     provider has no durable preimage for. The spines are gated behind
//     `module_binding` (deferred), so they never reach `module`, and this driver
//     exposes no spine wrapper.
//   - source_path → the flip: consumed only by inline `aggregates.rs` diagnostic
//     paths, never by the provider-generic selection logic this driver drives.
// ---------------------------------------------------------------------------

/// The winner [`select_module_type_member`] selects, projected to a pool
/// [`Type`] a differential renders index-independently. `Const` is the pinned
/// deferred arm (the const identity machinery is inert until this driver is
/// wired at the flip), never a resolved value.
pub enum ProviderModuleMember {
    Struct(Type),
    Enum(Type),
    Const,
    Absent,
}

/// The winner [`select_qualified_type`] selects (enum→struct order), as a pool
/// [`Type`].
pub enum ProviderQualifiedType {
    Enum(Type),
    Struct(Type),
    Absent,
}

/// The winner [`select_struct_literal_head`] selects for an unqualified head, as
/// a pool [`Type`]. `Bound` (a local-type-driven head) never arises through this
/// driver's `local_type = None` wrapper; it is carried for completeness.
pub enum ProviderStructHead {
    Bound(Type),
    Named(Type),
    Absent,
}

/// The aggregate-resolution ProviderFacts driver: answers the family-1D
/// [`AggregateFacts`] from a body identity pool + a caller-populated overlay,
/// instead of the epoch `Sema` tables [`EpochFacts`] reads.
///
/// Generic over the pool durable source `S` and the pool's durable nominal key
/// `K` and module `M` (rue-compiler binds `K = StableDefinitionKey`,
/// `M = ModuleId`). The pool lives behind a [`RefCell`] because a nominal is
/// minted on first consult (`&mut` on the pool) while [`AggregateFacts`]'s ops —
/// and therefore the provider-generic `select_*` logic driving them — are
/// `&self`; the borrow is never held across a re-entrant consult, so it never
/// conflicts. The `(file, name)` reverse and file paths are plain maps populated
/// by a caller through `&mut self` registration before any consult.
pub struct ProviderAggregateFacts<K, M, S> {
    pool: RefCell<BodyIdentityPool<K, M, S>>,
    /// The provider-side analog of the epoch's `structs_by_file_name` /
    /// `enums_by_file_name` key maps: `(file, pool-interned name) → durable key`,
    /// populated on demand as a caller registers a nominal. The `Spur` is the
    /// pool's own interner symbol (the same one the `select_*` wrappers intern
    /// their name argument to), never the shared whole-program interner's.
    by_file_name: HashMap<(u32, Spur), K>,
    /// Request-local file paths (a body-query input, exactly like the RIR — not a
    /// durable fact the pool mints), owned so [`AggregateFacts::file_path`] hands
    /// back a borrowed `&str` without a seam-signature change. A caller registers
    /// the same physical path the epoch's `get_file_path` returns, so
    /// [`is_accessible`]'s visibility-domain computation matches byte-for-byte.
    file_paths: HashMap<FileId, String>,
}

impl<K, M, S> ProviderAggregateFacts<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Eq + Hash,
    S: DurableNominalSource<K, M>,
{
    /// Construct the driver over a durable nominal source. The pool is built here
    /// (builtin enums + `str` pre-registered); named nominals are minted lazily on
    /// first consult and the overlay maps start empty.
    pub fn new(source: S) -> Self {
        Self {
            pool: RefCell::new(BodyIdentityPool::new(source)),
            by_file_name: HashMap::new(),
            file_paths: HashMap::new(),
        }
    }

    /// Record that a durable nominal key is declared as `(file, name)`, the
    /// provider-side analog of the epoch's `structs_by_file_name` /
    /// `enums_by_file_name` insert. The name is interned into the pool's own
    /// interner so a later `select_*`/`struct_in_file` consult reverses the same
    /// `(file, pool-name)` key.
    ///
    /// FLIP OBLIGATION (recorded by the r4b-3 review): this driver's
    /// "zero provider edges by construction" property is complete only if the
    /// flip-era caller fills these entries from the durable declaration set
    /// (whose keys already carry module/kind/name — no boundary consult), OR
    /// consults the lookup boundary upstream with edges recorded there. The
    /// flip slice must state which and assert the resulting edge story; an
    /// unspecified fill source is not an option.
    pub fn register_named_nominal(&mut self, key: K, file: FileId, name: &str) {
        let symbol = self.pool.borrow().intern_name(name);
        self.by_file_name.insert((file.index(), symbol), key);
    }

    /// Record the request-local physical file path for `file`, so
    /// [`Self::is_accessible`] reproduces the epoch's visibility domain exactly.
    pub fn register_file_path(&mut self, file: FileId, path: &str) {
        self.file_paths.insert(file, path.to_owned());
    }

    /// (P) The struct declared as `(file, name)`, minted through the pool's 2a
    /// nominal machinery, as a pool [`Type`]. Equal to the epoch's
    /// `structs_by_file_name` lookup under the durable-key bijection.
    pub fn struct_in_file(&self, file: FileId, name: &str) -> Option<Type> {
        let symbol = self.pool.borrow().intern_name(name);
        AggregateFacts::struct_in_file(self, file, symbol).map(Type::new_struct)
    }

    /// (P) The enum declared as `(file, name)`, as a pool [`Type`].
    pub fn enum_in_file(&self, file: FileId, name: &str) -> Option<Type> {
        let symbol = self.pool.borrow().intern_name(name);
        AggregateFacts::enum_in_file(self, file, symbol).map(Type::new_enum)
    }

    /// (P) The builtin struct for a bare name (the pool's pre-registered `str`),
    /// as a pool [`Type`]. Names beyond the pre-registered builtin set fail closed
    /// (r6 builtin facts).
    pub fn builtin_struct(&self, name: &str) -> Option<Type> {
        let symbol = self.pool.borrow().intern_name(name);
        AggregateFacts::builtin_struct(self, symbol).map(Type::new_struct)
    }

    /// (P) The builtin enum for a bare name (one of `BUILTIN_ENUMS`), as a pool
    /// [`Type`].
    pub fn builtin_enum(&self, name: &str) -> Option<Type> {
        let symbol = self.pool.borrow().intern_name(name);
        AggregateFacts::builtin_enum(self, symbol).map(Type::new_enum)
    }

    /// Run the provider-generic [`select_module_type_member`] over this driver:
    /// the r1c struct→enum→const short-circuit, driven from the pool. The const
    /// fall-through is the pinned deferred arm.
    pub fn select_module_type_member(&self, file: FileId, name: &str) -> ProviderModuleMember {
        let symbol = self.pool.borrow().intern_name(name);
        match select_module_type_member(self, file, symbol) {
            ModuleTypeMember::Struct(id) => ProviderModuleMember::Struct(Type::new_struct(id)),
            ModuleTypeMember::Enum(id) => ProviderModuleMember::Enum(Type::new_enum(id)),
            ModuleTypeMember::Const(_) => ProviderModuleMember::Const,
            ModuleTypeMember::Absent => ProviderModuleMember::Absent,
        }
    }

    /// Run the provider-generic [`select_qualified_type`] over this driver: the
    /// r1c enum→struct order.
    pub fn select_qualified_type(&self, file: FileId, name: &str) -> ProviderQualifiedType {
        let symbol = self.pool.borrow().intern_name(name);
        match select_qualified_type(self, file, symbol) {
            QualifiedType::Enum(id) => ProviderQualifiedType::Enum(Type::new_enum(id)),
            QualifiedType::Struct(id) => ProviderQualifiedType::Struct(Type::new_struct(id)),
            QualifiedType::Absent => ProviderQualifiedType::Absent,
        }
    }

    /// Run the provider-generic [`select_qualified_enum`] over this driver.
    pub fn select_qualified_enum(&self, file: FileId, name: &str) -> Option<Type> {
        let symbol = self.pool.borrow().intern_name(name);
        select_qualified_enum(self, file, symbol).map(Type::new_enum)
    }

    /// Run the provider-generic [`select_struct_literal_head`] over this driver
    /// for an unqualified head (`local_type = None`): the const→struct→builtin
    /// order. The const arm is the pinned deferred arm.
    pub fn select_struct_literal_head(&self, file: FileId, name: &str) -> ProviderStructHead {
        let symbol = self.pool.borrow().intern_name(name);
        match select_struct_literal_head(self, None, file, symbol) {
            StructLiteralHead::Bound(ty) => ProviderStructHead::Bound(ty),
            StructLiteralHead::Named(id) => ProviderStructHead::Named(Type::new_struct(id)),
            StructLiteralHead::Absent => ProviderStructHead::Absent,
        }
    }

    /// Run the provider-generic [`is_accessible`] over this driver's registered
    /// file paths — the visibility short-circuit answered from the request-local
    /// path facts, byte-identical to the epoch when the same paths are registered.
    pub fn is_accessible(&self, accessing: FileId, defining: FileId, is_public: bool) -> bool {
        is_accessible(self, accessing, defining, is_public)
    }

    /// Read the body-local minted [`TypeInternPool`] under a closure, so a
    /// differential renders a resolved pool [`Type`] index-independently (the pool
    /// mints its own ids; parity is asserted through displays / metadata, never a
    /// pool-relative index — the 2a contract).
    pub fn with_type_pool<R>(&self, read: impl FnOnce(&TypeInternPool) -> R) -> R {
        read(self.pool.borrow().type_pool())
    }
}

impl<K, M, S> AggregateFacts for ProviderAggregateFacts<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Eq + Hash,
    S: DurableNominalSource<K, M>,
{
    fn value_const(&self, _file: FileId, _name: Spur) -> Option<ConstInfo> {
        // Deferred to the flip: the pool const arm + RIR handle are landed but
        // intentionally not installed under this production-shaped seam yet.
        // The const fall-through of the `select_*` logic answers `Absent`; pinned.
        None
    }

    fn module_binding(&self, _file: FileId, _name: Spur) -> Option<ConstInfo> {
        // Deferred to the flip (same const RIR handle). Also gates the module
        // spines, which therefore never reach `module`.
        None
    }

    fn struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId> {
        let key = self.by_file_name.get(&(file.index(), name)).cloned()?;
        self.pool
            .borrow_mut()
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_struct()
    }

    fn enum_in_file(&self, file: FileId, name: Spur) -> Option<EnumId> {
        let key = self.by_file_name.get(&(file.index(), name)).cloned()?;
        self.pool
            .borrow_mut()
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_enum()
    }

    fn builtin_struct(&self, name: Spur) -> Option<StructId> {
        let name = self.pool.borrow().resolve_symbol(name).to_owned();
        self.pool
            .borrow_mut()
            .resolve(&SemanticImportType::BuiltinNominal {
                name: std::sync::Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Struct,
            })
            .ok()?
            .as_struct()
    }

    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        let name = self.pool.borrow().resolve_symbol(name).to_owned();
        self.pool
            .borrow_mut()
            .resolve(&SemanticImportType::BuiltinNominal {
                name: std::sync::Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Enum,
            })
            .ok()?
            .as_enum()
    }

    fn module(&self, _module: ModuleId) -> AggregateModuleFact {
        // Deferred to the flip: `module_def` answers a rue-air-internal `ModuleId`
        // registry index with no durable provider preimage. Unreachable while
        // `module_binding` is deferred (the module spines short-circuit before
        // reaching this op); a benign empty fact keeps the refusal total.
        AggregateModuleFact {
            file: FileId::DEFAULT,
            file_path: String::new(),
            import_path: String::new(),
        }
    }

    fn file_path(&self, file: FileId) -> Option<&str> {
        self.file_paths.get(&file).map(String::as_str)
    }

    fn source_path(&self, _span: rue_span::Span) -> Option<&str> {
        // Deferred to the flip: consumed only by inline `aggregates.rs`
        // diagnostic paths, never by the provider-generic selection logic.
        None
    }
}
