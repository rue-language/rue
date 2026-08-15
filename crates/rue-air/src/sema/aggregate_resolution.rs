//! Provider-generic aggregate, field, and variant resolution.
//!
//! Selection order lives here. Both body-analysis hosts implement the one
//! [`AggregateFacts`] boundary directly.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;

use lasso::Spur;
use rue_rir::{InstData, InstRef, Rir};
use rue_span::FileId;

use super::body_identity::{DurableNominalSource, ProviderIdentityContext};
use super::context::{ConstValue, LocalVar};
use super::{ConstInfo, DeclarationPhase, Sema};
use crate::intern_pool::TypeInternPool;
use crate::types::{EnumId, ModuleId, StructId, Type};
use crate::{SemanticImportNominalKind, SemanticImportType};

pub(crate) trait AggregateFacts {
    fn aggregate_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo>;
    fn aggregate_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo>;
    fn aggregate_struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId>;
    fn aggregate_enum_in_file(&self, file: FileId, name: Spur) -> Option<EnumId>;
    fn aggregate_builtin_struct(&self, name: Spur) -> Option<StructId>;
    fn aggregate_builtin_enum(&self, name: Spur) -> Option<EnumId>;
    fn aggregate_module(&self, module: ModuleId) -> AggregateModuleFact;
    fn aggregate_file_path(&self, file: FileId) -> Option<&str>;
    #[allow(dead_code)]
    fn aggregate_source_path(&self, span: rue_span::Span) -> Option<&str>;
    fn aggregate_visibility_domain(&self, file: FileId) -> crate::SemanticVisibilityDomain {
        crate::SemanticVisibilityDomain::from_file_path(self.aggregate_file_path(file))
    }
}

pub(crate) struct AggregateModuleFact {
    pub(crate) file: FileId,
    #[allow(dead_code)]
    file_path: String,
    import_path: String,
}

impl AggregateModuleFact {
    #[allow(dead_code)]
    pub(crate) fn file_path(&self) -> &str {
        &self.file_path
    }

    pub(crate) fn import_path(&self) -> &str {
        &self.import_path
    }
}

impl<D: DeclarationPhase> AggregateFacts for Sema<'_, D> {
    fn aggregate_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.declarations.value_const(&(file, name)).cloned()
    }

    fn aggregate_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.declarations.module_binding(&(file, name)).cloned()
    }

    fn aggregate_struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.structs_by_file_name.get(&(file, name)).copied()
    }

    fn aggregate_enum_in_file(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.enums_by_file_name.get(&(file, name)).copied()
    }

    fn aggregate_builtin_struct(&self, name: Spur) -> Option<StructId> {
        self.resolve_builtin_struct_name(name)
    }

    fn aggregate_builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.resolve_builtin_enum_name(name)
    }

    fn aggregate_module(&self, module: ModuleId) -> AggregateModuleFact {
        let def = self.module_registry.get_def(module);
        AggregateModuleFact {
            file: def.file_id,
            file_path: def.file_path,
            import_path: def.import_path,
        }
    }

    fn aggregate_file_path(&self, file: FileId) -> Option<&str> {
        self.get_file_path(file)
    }

    fn aggregate_source_path(&self, span: rue_span::Span) -> Option<&str> {
        self.get_source_path(span)
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
    if let Some(info) = facts.aggregate_value_const(file, name)
        && let ConstValue::Type(ty) = info.value
    {
        return ty.as_enum().map(|id| (id, true));
    }
    facts
        .aggregate_enum_in_file(file, name)
        .or_else(|| facts.aggregate_builtin_enum(name))
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
    if let Some(info) = facts.aggregate_value_const(file, name)
        && let ConstValue::Type(ty) = info.value
    {
        return ty.as_struct().map(|id| (id, true));
    }
    facts
        .aggregate_struct_in_file(file, name)
        .or_else(|| facts.aggregate_builtin_struct(name))
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
    if let Some(info) = facts.aggregate_value_const(file, name)
        && let ConstValue::Type(ty) = info.value
    {
        return StructLiteralHead::Bound(ty);
    }
    facts
        .aggregate_struct_in_file(file, name)
        .or_else(|| facts.aggregate_builtin_struct(name))
        .map_or(StructLiteralHead::Absent, StructLiteralHead::Named)
}

pub(crate) fn select_module_type_member<P: AggregateFacts>(
    facts: &P,
    file: FileId,
    name: Spur,
) -> ModuleTypeMember {
    if let Some(id) = facts.aggregate_struct_in_file(file, name) {
        return ModuleTypeMember::Struct(id);
    }
    if let Some(id) = facts.aggregate_enum_in_file(file, name) {
        return ModuleTypeMember::Enum(id);
    }
    if let Some(info) = facts
        .aggregate_module_binding(file, name)
        .or_else(|| facts.aggregate_value_const(file, name))
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
    if let Some(id) = facts.aggregate_enum_in_file(file, name) {
        return QualifiedType::Enum(id);
    }
    facts
        .aggregate_struct_in_file(file, name)
        .map_or(QualifiedType::Absent, QualifiedType::Struct)
}

pub(crate) fn select_qualified_enum<P: AggregateFacts>(
    facts: &P,
    file: FileId,
    name: Spur,
) -> Option<EnumId> {
    facts.aggregate_enum_in_file(file, name)
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
                .aggregate_module_binding(root_file, name)
                .and_then(|binding| binding.ty.as_module())
        }
        InstData::FieldGet { base, field } => {
            let parent = resolve_aggregate_module_ref(facts, rir, base, root_file, locals)?;
            let parent_file = facts.aggregate_module(parent).file;
            facts
                .aggregate_module_binding(parent_file, field)
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
                .aggregate_module_binding(inst.span.file_id, name)
                .map(|binding| binding.ty)
        }),
        InstData::FieldGet { base, field } => {
            let parent = resolve_visibility_module_ref(facts, rir, base, locals)?;
            let parent_file = facts.aggregate_module(parent).file;
            facts
                .aggregate_module_binding(parent_file, field)
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
    if is_public || accessing_file == defining_file {
        return true;
    }
    let accessing = facts.aggregate_visibility_domain(accessing_file);
    let defining = facts.aggregate_visibility_domain(defining_file);
    defining.is_visible_from(&accessing, is_public)
}

// ---------------------------------------------------------------------------
// `ProviderAggregateFacts` — the aggregate / field / variant ProviderFacts.
//
// This driver answers aggregate facts from the body-scoped identity pool plus
// a request-local overlay for `(file, name) → durable key` and file paths.
//
// The selection ORDER is not this driver's concern: it lives in the
// provider-generic free functions above ([`select_module_type_member`]'s
// struct→enum→const short-circuit, [`select_qualified_type`]'s enum→struct,
// [`select_struct_literal_head`]'s bound→const→struct→builtin) which this driver
// merely supplies facts to, so every consumer replays the same candidate order
// and short-circuits. The driver's
// inherent `select_*` wrappers run those free functions over itself and hand back
// the winner as a pool [`Type`].
//
// This surface is public because rue-compiler supplies the concrete durable
// signature source behind the opaque provider boundary. No aggregate op consults the live
// `BodyFactProvider` boundary — struct/enum-by-file-name, the builtins, and
// `is_accessible` are all answered by the pool + the caller-populated overlay —
// so this driver holds no provider handle and records no provider edge by
// construction.
// ---------------------------------------------------------------------------

/// The winner [`select_module_type_member`] selects, projected to a pool
/// [`Type`] a differential renders index-independently. `Const` reports the
/// installed value-constant or module-binding arm.
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

/// Query-backed aggregate fact state: answers [`AggregateFacts`] from a body
/// identity pool plus a caller-populated overlay, while the epoch host reads
/// its own tables directly.
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
    identity: ProviderIdentityContext<K, M, S>,
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
    value_consts: RefCell<HashMap<(u32, Spur), ConstInfo>>,
    module_bindings: RefCell<HashMap<(u32, Spur), ConstInfo>>,
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
        Self::with_identity(ProviderIdentityContext::new(source))
    }

    /// Construct the driver over the shared identity context used by every
    /// provider fact family for body analysis.
    pub fn with_identity(identity: ProviderIdentityContext<K, M, S>) -> Self {
        Self::with_overlay_identity(identity.fail_closed())
    }

    fn with_overlay_identity(identity: ProviderIdentityContext<K, M, S>) -> Self {
        Self {
            identity,
            by_file_name: HashMap::new(),
            file_paths: HashMap::new(),
            value_consts: RefCell::new(HashMap::new()),
            module_bindings: RefCell::new(HashMap::new()),
        }
    }

    /// Construct the aggregate fact state from the one task-local provider
    /// authority shared with endpoint and call resolution.
    pub fn with_state(state: &super::ProviderBodyAnalysisState<K, M, S>) -> Self {
        Self::with_overlay_identity(state.identity_context())
    }

    /// Record that a durable nominal key is declared as `(file, name)`, the
    /// provider-side analog of the epoch's `structs_by_file_name` /
    /// `enums_by_file_name` insert. The name is interned into the pool's own
    /// interner so a later `select_*`/`struct_in_file` consult reverses the same
    /// `(file, pool-name)` key.
    /// Callers populate this from durable declaration keys or from an upstream
    /// lookup whose provider edge has already been recorded.
    pub fn register_named_nominal(&mut self, key: K, file: FileId, name: &str) {
        let symbol = self.identity.pool().intern_name(name);
        self.by_file_name.insert((file.index(), symbol), key);
    }

    /// Record the request-local physical file path for `file`, so
    /// [`Self::is_accessible`] reproduces the epoch's visibility domain exactly.
    pub fn register_file_path(&mut self, file: FileId, path: &str) {
        self.file_paths.insert(file, path.to_owned());
    }

    pub fn register_value_const(&self, file: FileId, name: &str, info: ConstInfo) {
        let name = self.identity.pool().intern_name(name);
        self.value_consts
            .borrow_mut()
            .insert((file.index(), name), info);
    }

    /// Recover an installed value constant with its complete assembled payload.
    pub fn value_const(&self, file: FileId, name: &str) -> Option<ConstInfo> {
        let name = self.identity.pool().intern_name(name);
        self.value_consts
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    pub fn register_module_binding(&self, file: FileId, name: &str, info: ConstInfo) {
        let name = self.identity.pool().intern_name(name);
        self.module_bindings
            .borrow_mut()
            .insert((file.index(), name), info);
    }

    /// Recover an installed module binding with its complete assembled payload.
    pub fn module_binding(&self, file: FileId, name: &str) -> Option<ConstInfo> {
        let name = self.identity.pool().intern_name(name);
        self.module_bindings
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    /// Register one durable module and its request-local presentation facts in
    /// the body-local module registry. The returned compact id is the provider
    /// counterpart of the epoch's canonical `ModuleId`; downstream aggregate
    /// spines recover the exact file/import paths through [`AggregateFacts::module`].
    pub fn register_module(
        &self,
        module: M,
        file: FileId,
        file_path: &str,
        import_path: &str,
        durable_id: &str,
    ) -> Option<ModuleId> {
        self.identity
            .modules_mut()
            .register(module, file, file_path, import_path, durable_id)
    }

    /// Owned, index-independent module facts for a registered compact id.
    pub fn module_fact(&self, module: ModuleId) -> Option<(FileId, String, String)> {
        self.identity.modules().get(module).map(|definition| {
            (
                definition.file_id,
                definition.file_path,
                definition.import_path,
            )
        })
    }

    /// (P) The struct declared as `(file, name)`, minted through the pool's 2a
    /// nominal machinery, as a pool [`Type`]. Equal to the epoch's
    /// `structs_by_file_name` lookup under the durable-key bijection.
    pub fn struct_in_file(&self, file: FileId, name: &str) -> Option<Type> {
        let symbol = self.identity.pool().intern_name(name);
        AggregateFacts::aggregate_struct_in_file(self, file, symbol).map(Type::new_struct)
    }

    /// (P) The enum declared as `(file, name)`, as a pool [`Type`].
    pub fn enum_in_file(&self, file: FileId, name: &str) -> Option<Type> {
        let symbol = self.identity.pool().intern_name(name);
        AggregateFacts::aggregate_enum_in_file(self, file, symbol).map(Type::new_enum)
    }

    /// (P) The builtin struct for a bare name (the pool's pre-registered `str`),
    /// as a pool [`Type`]. Names beyond the pre-registered builtin set fail closed
    /// (r6 builtin facts).
    pub fn builtin_struct(&self, name: &str) -> Option<Type> {
        let symbol = self.identity.pool().intern_name(name);
        AggregateFacts::aggregate_builtin_struct(self, symbol).map(Type::new_struct)
    }

    /// (P) The builtin enum for a bare name (one of `BUILTIN_ENUMS`), as a pool
    /// [`Type`].
    pub fn builtin_enum(&self, name: &str) -> Option<Type> {
        let symbol = self.identity.pool().intern_name(name);
        AggregateFacts::aggregate_builtin_enum(self, symbol).map(Type::new_enum)
    }

    /// Run the provider-generic [`select_module_type_member`] over this driver:
    /// the r1c struct→enum→const short-circuit, driven from the pool. The const
    /// fall-through consults the installed body-local const overlay.
    pub fn select_module_type_member(&self, file: FileId, name: &str) -> ProviderModuleMember {
        let symbol = self.identity.pool().intern_name(name);
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
        let symbol = self.identity.pool().intern_name(name);
        match select_qualified_type(self, file, symbol) {
            QualifiedType::Enum(id) => ProviderQualifiedType::Enum(Type::new_enum(id)),
            QualifiedType::Struct(id) => ProviderQualifiedType::Struct(Type::new_struct(id)),
            QualifiedType::Absent => ProviderQualifiedType::Absent,
        }
    }

    /// Run the provider-generic [`select_qualified_enum`] over this driver.
    pub fn select_qualified_enum(&self, file: FileId, name: &str) -> Option<Type> {
        let symbol = self.identity.pool().intern_name(name);
        select_qualified_enum(self, file, symbol).map(Type::new_enum)
    }

    /// Run the provider-generic [`select_struct_literal_head`] over this driver
    /// for an unqualified head (`local_type = None`): the const→struct→builtin
    /// order, including an installed const arm.
    pub fn select_struct_literal_head(&self, file: FileId, name: &str) -> ProviderStructHead {
        let symbol = self.identity.pool().intern_name(name);
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
        let pool = self.identity.type_pool();
        read(&pool)
    }
}

impl<K, M, S> AggregateFacts for ProviderAggregateFacts<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Eq + Hash,
    S: DurableNominalSource<K, M>,
{
    fn aggregate_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.value_consts
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    fn aggregate_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.module_bindings
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    fn aggregate_struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId> {
        let key = self.by_file_name.get(&(file.index(), name)).cloned()?;
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_struct()
    }

    fn aggregate_enum_in_file(&self, file: FileId, name: Spur) -> Option<EnumId> {
        let key = self.by_file_name.get(&(file.index(), name)).cloned()?;
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_enum()
    }

    fn aggregate_builtin_struct(&self, name: Spur) -> Option<StructId> {
        let name = self.identity.pool().resolve_symbol(name).to_owned();
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::BuiltinNominal {
                name: std::sync::Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Struct,
            })
            .ok()?
            .as_struct()
    }

    fn aggregate_builtin_enum(&self, name: Spur) -> Option<EnumId> {
        let name = self.identity.pool().resolve_symbol(name).to_owned();
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::BuiltinNominal {
                name: std::sync::Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Enum,
            })
            .ok()?
            .as_enum()
    }

    fn aggregate_module(&self, module: ModuleId) -> AggregateModuleFact {
        let definition = self
            .identity
            .modules()
            .get(module)
            .expect("provider module id must be registered before aggregate resolution");
        AggregateModuleFact {
            file: definition.file_id,
            file_path: definition.file_path,
            import_path: definition.import_path,
        }
    }

    fn aggregate_file_path(&self, file: FileId) -> Option<&str> {
        self.file_paths.get(&file).map(String::as_str)
    }

    fn aggregate_source_path(&self, _span: rue_span::Span) -> Option<&str> {
        // Deferred to the flip: consumed only by inline `aggregates.rs`
        // diagnostic paths, never by the provider-generic selection logic.
        None
    }
}
