//! Provider-generic aggregate, field, and variant resolution.
//!
//! Selection order lives here. [`EpochFacts`] is the production adapter that
//! reproduces the declaration-epoch reads used by body analysis.

use lasso::Spur;
use rue_rir::{InstData, InstRef, Rir};
use rue_span::FileId;

use super::context::{ConstValue, LocalVar};
use super::{ConstInfo, DeclarationPhase, Sema};
use crate::types::{EnumId, ModuleId, StructId, Type};

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
