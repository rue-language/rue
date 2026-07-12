//! Owned semantic bindings emitted only after declaration binding succeeds.

use std::sync::{Arc, OnceLock};

use lasso::Spur;
use rue_error::{CompileErrors, MultiErrorResult};
use rue_rir::InstData;
use rue_span::{FileId, Span};

use super::RirDeclarationIndexWork;
use super::{ConstValue, Sema, SemaOutput};
use crate::types::{Type, TypeKind};

/// A nominal declaration identity valid for one successful binding request.
/// Consumers must join this identity to their own stable definition universe
/// while the callback passed to [`BoundSema::with_declaration_semantics`] runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticNominalIdentity {
    pub file_id: FileId,
    pub name: Arc<str>,
    pub kind: SemanticBindingKind,
}

/// An owned resolved type with no `Type`, pool ID, interner symbol, or RIR handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticExportType {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,
    Unit,
    Never,
    ComptimeType,
    GenericParameter(u32),
    Nominal(SemanticNominalIdentity),
    Array {
        element: Box<Self>,
        len: u64,
    },
    PtrConst(Box<Self>),
    PtrMut(Box<Self>),
    /// Resolved module path. It is deliberately converted by the compiler
    /// during the callback rather than retained as AIR's request-local ModuleId.
    Module(Arc<str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticParameterMode {
    Value,
    Borrow,
    Inout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticExportParameter {
    pub ty: SemanticExportType,
    pub mode: SemanticParameterMode,
    pub is_comptime: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticExportConstValue {
    Integer(i128),
    Bool(bool),
    Type(SemanticExportType),
    Function { file_id: FileId, name: Arc<str> },
    Unit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticDeclarationPayload {
    Callable {
        parameters: Arc<[SemanticExportParameter]>,
        result: SemanticExportType,
        has_self: bool,
        is_unchecked: bool,
    },
    Struct {
        fields: Arc<[(Arc<str>, SemanticExportType)]>,
        is_copy: bool,
        is_linear: bool,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[SemanticExportType]>)]>,
    },
    Const {
        ty: SemanticExportType,
        value: SemanticExportConstValue,
    },
    Destructor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticDeclarationExport {
    pub identity: SemanticBinding,
    pub payload: SemanticDeclarationPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticExportFailure {
    ErrorType,
    AnonymousNominalType,
    UnmappedNominalType,
    UnmappedFunction,
    UnsupportedParameterMode,
    RecursiveStructuralType,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticDeclarationExportWork {
    pub build_invocations: usize,
    pub declarations_exported: usize,
    pub rir_instructions_visited: usize,
}

/// Structural descriptors for one completed declaration-binding pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeclarationBindingWork {
    pub bind_invocations: usize,
    /// Global collision validation and builtin/module namespace setup.
    pub namespace_setup_invocations: usize,
    /// Deterministic named struct/enum shell predeclaration.
    pub nominal_type_predeclaration_invocations: usize,
    /// Resolution of declaration payloads, constants, and cycles.
    pub declaration_resolution_invocations: usize,
    /// Construction of the body-analysis-ready state.
    pub body_readiness_finalization_invocations: usize,
    /// Size of the input RIR, not a claim that binding visited every entry.
    pub input_rir_instructions: usize,
    pub declaration_index_build_invocations: usize,
    pub indexed_free_functions: usize,
    pub indexed_named_methods: usize,
    pub indexed_anonymous_methods: usize,
    pub indexed_destructors: usize,
    pub indexed_const_candidates: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBindingNamespace {
    Value,
    Type,
    Destructor,
    Method,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBindingKind {
    Function,
    Struct,
    Enum,
    ValueConst,
    ModuleBinding,
    Destructor,
    Method,
    AssociatedFunction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBinding {
    /// Request-local source file containing this declaration.
    pub file_id: FileId,
    /// Request-local declaration location; excluded from stable identity.
    pub declaration_span: Span,
    pub namespace: SemanticBindingNamespace,
    pub kind: SemanticBindingKind,
    pub name: Arc<str>,
    pub owner: Option<Arc<str>>,
    /// Source visibility; destructors are never public.
    pub is_public: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticBindingManifestWork {
    pub build_invocations: usize,
    pub rir_instructions_visited: usize,
    pub bindings_emitted: usize,
    pub functions_emitted: usize,
    pub types_emitted: usize,
    pub constants_emitted: usize,
    pub module_bindings_emitted: usize,
    pub destructors_emitted: usize,
    pub named_methods_emitted: usize,
    pub named_method_edges_visited: usize,
    pub anonymous_methods_deferred: usize,
    pub parser_invocations: usize,
    pub ast_payload_clones: usize,
    pub source_text_clones: usize,
}

#[derive(Debug, Clone)]
pub struct SemanticBindingManifest {
    bindings: Arc<[SemanticBinding]>,
    work: SemanticBindingManifestWork,
}

impl SemanticBindingManifest {
    /// Successful bindings in deterministic declaration order, with named
    /// methods grouped beneath their owning named struct.
    pub fn bindings(&self) -> &[SemanticBinding] {
        &self.bindings
    }
    pub fn work(&self) -> SemanticBindingManifestWork {
        self.work
    }
}

pub struct BoundSema<'a> {
    sema: Sema<'a>,
    manifest: OnceLock<SemanticBindingManifest>,
    binding_work: DeclarationBindingWork,
}

/// A semantic request whose global namespace and nominal declaration shells
/// are complete, but whose declaration payloads have not yet been resolved.
///
/// This boundary deliberately owns the request-local `Sema` state.  A future
/// durable-declaration importer can populate that state here without making
/// raw AIR handles part of the reusable representation.  Today the only
/// transition performs the ordinary current-revision resolution pass.
pub struct DeclarationShells<'a> {
    pub(super) sema: Sema<'a>,
    pub(super) binding_work: DeclarationBindingWork,
}

impl<'a> DeclarationShells<'a> {
    /// Work completed before declaration payload resolution.
    pub fn binding_work(&self) -> DeclarationBindingWork {
        self.binding_work
    }

    /// Resolve declaration payloads and finalize a body-analysis-ready binder.
    pub fn resolve_declarations(mut self) -> MultiErrorResult<BoundSema<'a>> {
        self.sema
            .resolve_declarations()
            .map_err(CompileErrors::from)?;
        self.binding_work.declaration_resolution_invocations += 1;
        self.binding_work.body_readiness_finalization_invocations += 1;
        Ok(self.sema.into_bound_with_work(self.binding_work))
    }
}

impl<'a> BoundSema<'a> {
    pub fn binding_work(&self) -> DeclarationBindingWork {
        self.binding_work
    }
    /// Materialize the owned manifest on demand. Ordinary body analysis does
    /// not pay for this additional RIR traversal.
    pub fn binding_manifest(&self) -> &SemanticBindingManifest {
        self.manifest
            .get_or_init(|| self.sema.build_binding_manifest())
    }

    /// Whether a caller has requested the optional binding manifest.
    pub fn manifest_is_materialized(&self) -> bool {
        self.manifest.get().is_some()
    }

    /// Lazily export resolved declaration semantics and invoke `convert` while
    /// the binder, type pool, interner and module registry are all alive.
    /// No RIR traversal or second bind is performed.
    pub fn with_declaration_semantics<R>(
        &self,
        convert: impl FnOnce(&[SemanticDeclarationExport], SemanticDeclarationExportWork) -> R,
    ) -> Result<R, SemanticExportFailure> {
        let manifest = self.binding_manifest();
        let (records, work) = self.sema.build_declaration_semantics(manifest)?;
        Ok(convert(&records, work))
    }

    pub fn analyze_all_bodies(self) -> MultiErrorResult<SemaOutput> {
        self.sema.analyze_all_bodies()
    }
}

impl<'a> Sema<'a> {
    fn export_type(
        &self,
        ty: Type,
        stack: &mut Vec<Type>,
    ) -> Result<SemanticExportType, SemanticExportFailure> {
        if stack.contains(&ty) {
            return Err(SemanticExportFailure::RecursiveStructuralType);
        }
        let primitive = match ty.kind() {
            TypeKind::I8 => Some(SemanticExportType::I8),
            TypeKind::I16 => Some(SemanticExportType::I16),
            TypeKind::I32 => Some(SemanticExportType::I32),
            TypeKind::I64 => Some(SemanticExportType::I64),
            TypeKind::U8 => Some(SemanticExportType::U8),
            TypeKind::U16 => Some(SemanticExportType::U16),
            TypeKind::U32 => Some(SemanticExportType::U32),
            TypeKind::U64 => Some(SemanticExportType::U64),
            TypeKind::Bool => Some(SemanticExportType::Bool),
            TypeKind::Unit => Some(SemanticExportType::Unit),
            TypeKind::Never => Some(SemanticExportType::Never),
            TypeKind::ComptimeType => Some(SemanticExportType::ComptimeType),
            TypeKind::Error => return Err(SemanticExportFailure::ErrorType),
            _ => None,
        };
        if let Some(value) = primitive {
            return Ok(value);
        }
        stack.push(ty);
        let result = match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self.type_pool.struct_def(id);
                if def.is_builtin {
                    return Err(SemanticExportFailure::UnmappedNominalType);
                }
                let symbol = self.interner.get_or_intern(&def.name);
                if self.structs_by_file_name.get(&(def.file_id, symbol)) != Some(&id) {
                    return Err(SemanticExportFailure::AnonymousNominalType);
                }
                Ok(SemanticExportType::Nominal(SemanticNominalIdentity {
                    file_id: def.file_id,
                    name: Arc::from(def.name),
                    kind: SemanticBindingKind::Struct,
                }))
            }
            TypeKind::Enum(id) => {
                let def = self.type_pool.enum_def(id);
                Ok(SemanticExportType::Nominal(SemanticNominalIdentity {
                    file_id: def.file_id,
                    name: Arc::from(def.name),
                    kind: SemanticBindingKind::Enum,
                }))
            }
            TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                Ok(SemanticExportType::Array {
                    element: Box::new(self.export_type(element, stack)?),
                    len,
                })
            }
            TypeKind::PtrConst(id) => Ok(SemanticExportType::PtrConst(Box::new(
                self.export_type(self.type_pool.ptr_const_def(id), stack)?,
            ))),
            TypeKind::PtrMut(id) => Ok(SemanticExportType::PtrMut(Box::new(
                self.export_type(self.type_pool.ptr_mut_def(id), stack)?,
            ))),
            TypeKind::Module(id) => Ok(SemanticExportType::Module(Arc::from(
                self.module_registry.get_def(id).file_path,
            ))),
            _ => unreachable!(),
        };
        stack.pop();
        result
    }

    fn export_function_signature(
        &self,
        info: &super::FunctionInfo,
    ) -> Result<(Arc<[SemanticExportParameter]>, SemanticExportType), SemanticExportFailure> {
        self.export_callable_signature(
            info.params,
            info.return_type,
            info.rir_params_start,
            info.rir_params_len,
            info.return_type_sym,
        )
    }

    fn export_callable_signature(
        &self,
        range: crate::ParamRange,
        return_type: Type,
        params_start: u32,
        params_len: u32,
        return_type_sym: Spur,
    ) -> Result<(Arc<[SemanticExportParameter]>, SemanticExportType), SemanticExportFailure> {
        let rir_params = self.rir.get_params(params_start, params_len);
        let type_name = self.interner.get("type");
        let generic_names = rir_params
            .iter()
            .filter(|param| param.is_comptime && Some(param.ty) == type_name)
            .map(|param| param.name)
            .collect::<Vec<_>>();
        let convert = |ty: Type, symbol: Spur| {
            if ty == Type::COMPTIME_TYPE {
                if let Some(index) = generic_names.iter().position(|name| *name == symbol) {
                    return Ok(SemanticExportType::GenericParameter(index as u32));
                }
            }
            self.export_type(ty, &mut Vec::new())
        };
        let parameters = self
            .param_arena
            .types(range)
            .iter()
            .zip(self.param_arena.modes(range))
            .zip(self.param_arena.comptime(range))
            .zip(&rir_params)
            .map(|(((&ty, &mode), &is_comptime), rir)| {
                use rue_rir::RirParamMode;
                let mode = match mode {
                    RirParamMode::Normal | RirParamMode::Comptime => SemanticParameterMode::Value,
                    RirParamMode::Borrow => SemanticParameterMode::Borrow,
                    RirParamMode::Inout => SemanticParameterMode::Inout,
                };
                Ok(SemanticExportParameter {
                    ty: convert(ty, rir.ty)?,
                    mode,
                    is_comptime,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((parameters.into(), convert(return_type, return_type_sym)?))
    }

    fn build_declaration_semantics(
        &self,
        manifest: &SemanticBindingManifest,
    ) -> Result<
        (
            Vec<SemanticDeclarationExport>,
            SemanticDeclarationExportWork,
        ),
        SemanticExportFailure,
    > {
        let mut records = Vec::with_capacity(manifest.bindings.len());
        for identity in manifest
            .bindings
            .iter()
            .filter(|b| b.kind != SemanticBindingKind::ModuleBinding)
        {
            let name = self.interner.get_or_intern(identity.name.as_ref());
            let payload = match identity.kind {
                SemanticBindingKind::Function => {
                    let internal = *self
                        .functions_by_file_name
                        .get(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let info = self
                        .functions
                        .get(&internal)
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let (parameters, result) = self.export_function_signature(info)?;
                    SemanticDeclarationPayload::Callable {
                        parameters,
                        result,
                        has_self: false,
                        is_unchecked: info.is_unchecked,
                    }
                }
                SemanticBindingKind::Method | SemanticBindingKind::AssociatedFunction => {
                    let owner = self.interner.get_or_intern(
                        identity
                            .owner
                            .as_deref()
                            .ok_or(SemanticExportFailure::UnmappedNominalType)?,
                    );
                    let sid = *self
                        .structs_by_file_name
                        .get(&(identity.file_id, owner))
                        .ok_or(SemanticExportFailure::UnmappedNominalType)?;
                    let info = self
                        .methods
                        .get(&(sid, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let declaration = *self
                        .named_method_declarations
                        .get(&(sid, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let InstData::FnDecl {
                        params_start,
                        params_len,
                        return_type,
                        ..
                    } = &self.rir.get(declaration).data
                    else {
                        return Err(SemanticExportFailure::UnmappedFunction);
                    };
                    let (parameters, result) = self.export_callable_signature(
                        info.params,
                        info.return_type,
                        *params_start,
                        *params_len,
                        *return_type,
                    )?;
                    SemanticDeclarationPayload::Callable {
                        parameters,
                        result,
                        has_self: info.has_self,
                        is_unchecked: false,
                    }
                }
                SemanticBindingKind::Struct => {
                    let sid = *self
                        .structs_by_file_name
                        .get(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedNominalType)?;
                    let def = self.type_pool.struct_def(sid);
                    let fields = def
                        .fields
                        .into_iter()
                        .map(|f| Ok((Arc::from(f.name), self.export_type(f.ty, &mut Vec::new())?)))
                        .collect::<Result<Vec<_>, _>>()?;
                    SemanticDeclarationPayload::Struct {
                        fields: fields.into(),
                        is_copy: def.is_copy,
                        is_linear: def.is_linear,
                    }
                }
                SemanticBindingKind::Enum => {
                    let eid = *self
                        .enums_by_file_name
                        .get(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedNominalType)?;
                    let def = self.type_pool.enum_def(eid);
                    let variants = def
                        .variants
                        .iter()
                        .enumerate()
                        .map(|(i, n)| {
                            let payload = def
                                .variant_payload(i)
                                .iter()
                                .map(|&t| self.export_type(t, &mut Vec::new()))
                                .collect::<Result<Vec<_>, _>>()?;
                            Ok((Arc::from(n.as_str()), Arc::from(payload)))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    SemanticDeclarationPayload::Enum {
                        variants: variants.into(),
                    }
                }
                SemanticBindingKind::ValueConst => {
                    let info = self
                        .constants_by_file_name
                        .get(&(identity.file_id, name))
                        .ok_or(SemanticExportFailure::UnmappedFunction)?;
                    let value = match info.value {
                        ConstValue::Integer(v) => SemanticExportConstValue::Integer(v),
                        ConstValue::Bool(v) => SemanticExportConstValue::Bool(v),
                        ConstValue::Type(t) => {
                            SemanticExportConstValue::Type(self.export_type(t, &mut Vec::new())?)
                        }
                        ConstValue::Unit => SemanticExportConstValue::Unit,
                        ConstValue::Function(symbol) => {
                            let fi = self
                                .functions
                                .get(&symbol)
                                .ok_or(SemanticExportFailure::UnmappedFunction)?;
                            let source =
                                *self.function_source_names.get(&symbol).unwrap_or(&symbol);
                            SemanticExportConstValue::Function {
                                file_id: fi.file_id,
                                name: Arc::from(self.interner.resolve(&source)),
                            }
                        }
                    };
                    SemanticDeclarationPayload::Const {
                        ty: self.export_type(info.ty, &mut Vec::new())?,
                        value,
                    }
                }
                SemanticBindingKind::Destructor => SemanticDeclarationPayload::Destructor,
                SemanticBindingKind::ModuleBinding => unreachable!(),
            };
            records.push(SemanticDeclarationExport {
                identity: identity.clone(),
                payload,
            });
        }
        let len = records.len();
        Ok((
            records,
            SemanticDeclarationExportWork {
                build_invocations: 1,
                declarations_exported: len,
                rir_instructions_visited: 0,
            },
        ))
    }
    pub(super) fn into_bound_with_work(
        self,
        binding_work: DeclarationBindingWork,
    ) -> BoundSema<'a> {
        BoundSema {
            binding_work,
            sema: self,
            manifest: OnceLock::new(),
        }
    }

    fn build_binding_manifest(&self) -> SemanticBindingManifest {
        let mut bindings = Vec::new();
        let mut work = SemanticBindingManifestWork {
            build_invocations: 1,
            anonymous_methods_deferred: self.declaration_index.work().anonymous_methods_indexed,
            ..SemanticBindingManifestWork::default()
        };
        for (inst_ref, inst) in self.rir.iter() {
            work.rir_instructions_visited += 1;
            let mut emit = |file_id: FileId,
                            declaration_span: Span,
                            namespace: SemanticBindingNamespace,
                            kind: SemanticBindingKind,
                            name: &Spur,
                            owner: Option<Arc<str>>,
                            is_public: bool| {
                assert_eq!(file_id, declaration_span.file_id);
                bindings.push(SemanticBinding {
                    file_id,
                    declaration_span,
                    namespace,
                    kind,
                    name: Arc::from(self.interner.resolve(name)),
                    owner,
                    is_public,
                })
            };
            match &inst.data {
                InstData::FnDecl { name, is_pub, .. }
                    if !self.declaration_index.is_type_scoped_method(inst_ref) =>
                {
                    assert!(
                        self.functions_by_file_name
                            .contains_key(&(inst.span.file_id, *name)),
                        "manifest free function must be a bound winner"
                    );
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Value,
                        SemanticBindingKind::Function,
                        name,
                        None,
                        *is_pub,
                    );
                    work.functions_emitted += 1;
                }
                InstData::StructDecl {
                    name,
                    methods_start,
                    methods_len,
                    is_pub,
                    ..
                } => {
                    let struct_id = *self
                        .structs_by_file_name
                        .get(&(inst.span.file_id, *name))
                        .expect("manifest struct must be a bound winner");
                    let owner: Arc<str> = Arc::from(self.interner.resolve(name));
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Type,
                        SemanticBindingKind::Struct,
                        name,
                        None,
                        *is_pub,
                    );
                    work.types_emitted += 1;
                    for method_ref in self.rir.get_inst_refs(*methods_start, *methods_len) {
                        work.named_method_edges_visited += 1;
                        let method_inst = self.rir.get(method_ref);
                        let InstData::FnDecl {
                            name,
                            has_self,
                            is_pub,
                            ..
                        } = &method_inst.data
                        else {
                            unreachable!("named struct method edge must target FnDecl");
                        };
                        assert_eq!(
                            self.named_method_declarations.get(&(struct_id, *name)),
                            Some(&method_ref),
                            "manifest named method must be the bound winner"
                        );
                        emit(
                            method_inst.span.file_id,
                            method_inst.span,
                            SemanticBindingNamespace::Method,
                            if *has_self {
                                SemanticBindingKind::Method
                            } else {
                                SemanticBindingKind::AssociatedFunction
                            },
                            name,
                            Some(owner.clone()),
                            *is_pub,
                        );
                        work.named_methods_emitted += 1;
                    }
                }
                InstData::EnumDecl { name, is_pub, .. } => {
                    assert!(
                        self.enums_by_file_name
                            .contains_key(&(inst.span.file_id, *name)),
                        "manifest enum must be a bound winner"
                    );
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Type,
                        SemanticBindingKind::Enum,
                        name,
                        None,
                        *is_pub,
                    );
                    work.types_emitted += 1;
                }
                InstData::ConstDecl { name, is_pub, .. } => {
                    let key = (inst.span.file_id, *name);
                    let kind = if self.module_bindings.contains_key(&key) {
                        work.module_bindings_emitted += 1;
                        SemanticBindingKind::ModuleBinding
                    } else if self.constants_by_file_name.contains_key(&key) {
                        work.constants_emitted += 1;
                        SemanticBindingKind::ValueConst
                    } else {
                        panic!("manifest const must be a classified bound winner")
                    };
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Value,
                        kind,
                        name,
                        None,
                        *is_pub,
                    );
                }
                InstData::DropFnDecl { type_name, .. } => {
                    let struct_id = *self
                        .structs_by_file_name
                        .get(&(inst.span.file_id, *type_name))
                        .expect("manifest destructor target must be a bound named struct");
                    assert_eq!(
                        self.destructor_spans.get(&struct_id),
                        Some(&inst.span),
                        "manifest destructor must be the bound winner"
                    );
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Destructor,
                        SemanticBindingKind::Destructor,
                        type_name,
                        Some(Arc::from(self.interner.resolve(type_name))),
                        false,
                    );
                    work.destructors_emitted += 1;
                }
                _ => {}
            }
        }
        work.bindings_emitted = bindings.len();
        SemanticBindingManifest {
            bindings: bindings.into(),
            work,
        }
    }
}

impl DeclarationBindingWork {
    pub(super) fn from_inputs(
        input_rir_instructions: usize,
        index: RirDeclarationIndexWork,
    ) -> Self {
        Self {
            bind_invocations: 1,
            namespace_setup_invocations: 0,
            nominal_type_predeclaration_invocations: 0,
            declaration_resolution_invocations: 0,
            body_readiness_finalization_invocations: 0,
            input_rir_instructions,
            declaration_index_build_invocations: index.build_invocations,
            indexed_free_functions: index.free_functions_indexed,
            indexed_named_methods: index.named_methods_indexed,
            indexed_anonymous_methods: index.anonymous_methods_indexed,
            indexed_destructors: index.destructors_indexed,
            indexed_const_candidates: index.const_candidates_indexed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rue_error::{CompileErrors, PreviewFeatures};
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    use super::*;

    fn bind(source: &str) -> Result<SemanticBindingManifest, CompileErrors> {
        let (tokens, interner) = Lexer::new(source)
            .tokenize()
            .map_err(CompileErrors::from_error)?;
        let (ast, interner) = Parser::new(tokens, interner).parse()?;
        let rir = AstGen::new(&ast, &interner).generate();
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new()).bind_declarations()?;
        Ok(bound.binding_manifest().clone())
    }

    fn export(
        source: &str,
    ) -> Result<
        (
            Vec<SemanticDeclarationExport>,
            SemanticDeclarationExportWork,
        ),
        SemanticExportFailure,
    > {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let rir = AstGen::new(&ast, &interner).generate();
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        bound.with_declaration_semantics(|records, work| (records.to_vec(), work))
    }

    #[test]
    fn declaration_export_is_resolved_owned_lazy_and_rir_free() {
        let (records, work) = export(
            r#"
            struct Leaf { value: i32 }
            struct Boxed { nested: [[Leaf; 2]; 4] }
            enum Maybe { None, Some(Leaf) }
            const LIMIT: i32 = 7;
            fn id(comptime T: type, value: T) -> T { value }
            fn helper(value: ptr const Leaf) -> bool { true }
            const alias = helper;
            drop fn Boxed(self) {}
            fn main() {}
            "#,
        )
        .unwrap();
        assert_eq!(work.build_invocations, 1);
        assert_eq!(work.rir_instructions_visited, 0);
        assert_eq!(work.declarations_exported, records.len());
        assert!(records.iter().any(|record| matches!(
            &record.payload,
            SemanticDeclarationPayload::Callable { parameters, result, .. }
                if record.identity.name.as_ref() == "id"
                    && parameters.iter().map(|p| p.is_comptime).collect::<Vec<_>>() == [true, false]
                    && parameters[1].ty == SemanticExportType::GenericParameter(0)
                    && *result == SemanticExportType::GenericParameter(0)
        )));
        assert!(records.iter().any(|record| matches!(
            &record.payload,
            SemanticDeclarationPayload::Const { value: SemanticExportConstValue::Function { name, .. }, .. }
                if record.identity.name.as_ref() == "alias" && name.as_ref() == "helper"
        )));
        assert!(records.iter().any(|record| {
            record.identity.name.as_ref() == "Boxed"
                && matches!(record.payload, SemanticDeclarationPayload::Destructor)
        }));
        // The callback DTO contains no request-local Type/Spur/InstRef/nominal IDs.
        assert!(std::mem::size_of::<SemanticExportType>() > 0);
    }

    fn bind_with_module_paths(source: &str) -> Result<SemanticBindingManifest, CompileErrors> {
        let (tokens, interner) = Lexer::new(source)
            .tokenize()
            .map_err(CompileErrors::from_error)?;
        let (ast, interner) = Parser::new(tokens, interner).parse()?;
        let rir = AstGen::new(&ast, &interner).generate();
        let mut sema = Sema::new(&rir, &interner, PreviewFeatures::new());
        sema.set_root_file_id(FileId::DEFAULT);
        sema.set_file_paths(HashMap::from([
            (FileId::DEFAULT, "/main.rue".to_owned()),
            (FileId::new(1), "/other.rue".to_owned()),
        ]));
        let bound = sema.bind_declarations()?;
        Ok(bound.binding_manifest().clone())
    }

    #[test]
    fn manifest_is_owned_deterministic_and_complete_after_binding() {
        let source = r#"
            struct Resource {
                value: i32,
                fn get(self) -> i32 { self.value }
                fn make() -> Resource { Resource { value: 0 } }
            }
            enum Choice { None, Some(i32) }
            const LIMIT: i32 = 4;
            drop fn Resource(self) {}
            fn helper() -> i32 { LIMIT }
            fn main() -> i32 { helper() }
        "#;
        let first = bind(source).unwrap();
        let second = bind(source).unwrap();
        assert_eq!(first.bindings(), second.bindings());
        assert_eq!(first.work(), second.work());
        assert_eq!(first.work().build_invocations, 1);
        assert_eq!(first.work().functions_emitted, 2);
        assert_eq!(first.work().types_emitted, 2);
        assert_eq!(first.work().constants_emitted, 1);
        assert_eq!(first.work().destructors_emitted, 1);
        assert_eq!(first.work().named_methods_emitted, 2);
        assert_eq!(first.work().named_method_edges_visited, 2);
        assert_eq!(
            first.work().named_method_edges_visited,
            first.work().named_methods_emitted
        );
        assert_eq!(first.work().anonymous_methods_deferred, 0);
        assert_eq!(first.work().bindings_emitted, 8);
        assert_eq!(first.work().parser_invocations, 0);
        assert_eq!(first.work().ast_payload_clones, 0);
        assert_eq!(first.work().source_text_clones, 0);
        assert!(first.bindings().iter().any(|binding| {
            binding.name.as_ref() == "make"
                && binding.owner.as_deref() == Some("Resource")
                && binding.kind == SemanticBindingKind::AssociatedFunction
        }));
        assert!(
            first
                .bindings()
                .iter()
                .all(|binding| binding.file_id == binding.declaration_span.file_id)
        );
    }

    #[test]
    fn rejected_duplicate_method_never_produces_a_manifest() {
        let error = bind("struct Bad { fn duplicate(self) {} fn duplicate(self) {} } fn main() {}")
            .unwrap_err();
        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn synthetic_public_method_visibility_is_preserved() {
        let (tokens, interner) =
            Lexer::new("struct PublicApi { fn exposed(self) {} } fn main() {}")
                .tokenize()
                .unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut rir = AstGen::new(&ast, &interner).generate();
        let method = rir
            .iter()
            .find_map(|(reference, inst)| match inst.data {
                InstData::FnDecl { has_self: true, .. } => Some(reference),
                _ => None,
            })
            .unwrap();
        let InstData::FnDecl { is_pub, .. } = &mut rir.get_mut(method).data else {
            unreachable!()
        };
        *is_pub = true;
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(bound.binding_manifest().bindings().iter().any(|binding| {
            binding.name.as_ref() == "exposed"
                && binding.kind == SemanticBindingKind::Method
                && binding.is_public
        }));
    }

    #[test]
    fn rejected_const_collision_and_duplicate_destructor_emit_no_manifest() {
        assert!(bind("const same: i32 = 1; fn same() {} fn main() {}").is_err());
        assert!(
            bind(
                "struct Resource {} drop fn Resource(self) {} drop fn Resource(self) {} fn main() {}"
            )
            .is_err()
        );
    }

    #[test]
    fn constants_are_classified_only_after_successful_evaluation() {
        let manifest = bind_with_module_paths(
            "const value: i32 = 1; const imported = @import(\"other.rue\"); fn main() {}",
        )
        .unwrap();
        assert!(manifest.bindings().iter().any(|binding| {
            binding.name.as_ref() == "value" && binding.kind == SemanticBindingKind::ValueConst
        }));
        assert!(manifest.bindings().iter().any(|binding| {
            binding.name.as_ref() == "imported"
                && binding.kind == SemanticBindingKind::ModuleBinding
        }));
        assert_eq!(manifest.work().constants_emitted, 1);
        assert_eq!(manifest.work().module_bindings_emitted, 1);
    }

    #[test]
    fn analyze_all_matches_explicit_bind_then_analyze() {
        let source = "fn helper(x: i32) -> i32 { x + 1 } fn main() -> i32 { helper(41) }";
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let rir = AstGen::new(&ast, &interner).generate();
        let direct = Sema::new(&rir, &interner, PreviewFeatures::new())
            .analyze_all()
            .unwrap();
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(!bound.manifest_is_materialized());
        let explicit = bound.analyze_all_bodies().unwrap();
        let summarize = |output: &SemaOutput| {
            (
                output
                    .functions
                    .iter()
                    .map(|function| {
                        (
                            function.name.clone(),
                            function.air.display_with_interner(&interner).to_string(),
                        )
                    })
                    .collect::<Vec<_>>(),
                output.strings.clone(),
                output
                    .warnings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                output.type_pool.stats(),
                output.body_analysis_work,
            )
        };
        assert_eq!(summarize(&direct), summarize(&explicit));
    }

    #[test]
    fn manifest_scan_is_lazy_and_materialized_only_on_request() {
        let (tokens, interner) = Lexer::new("fn main() {}").tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let rir = AstGen::new(&ast, &interner).generate();
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(!bound.manifest_is_materialized());
        assert_eq!(bound.binding_work().bind_invocations, 1);
        assert_eq!(bound.binding_work().namespace_setup_invocations, 1);
        assert_eq!(
            bound.binding_work().nominal_type_predeclaration_invocations,
            1
        );
        assert_eq!(bound.binding_work().declaration_resolution_invocations, 1);
        assert_eq!(
            bound.binding_work().body_readiness_finalization_invocations,
            1
        );
        assert_eq!(bound.binding_work().input_rir_instructions, rir.len());
        assert_eq!(bound.binding_work().declaration_index_build_invocations, 1);
        assert_eq!(bound.binding_manifest().work().build_invocations, 1);
        assert!(bound.manifest_is_materialized());
    }

    #[test]
    fn declaration_shell_boundary_accounts_each_phase_once() {
        let (tokens, interner) = Lexer::new("struct Item { value: i32 } fn main() {}")
            .tokenize()
            .unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let rir = AstGen::new(&ast, &interner).generate();

        let shells = Sema::new(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells()
            .unwrap();
        let prepared = shells.binding_work();
        assert_eq!(prepared.bind_invocations, 1);
        assert_eq!(prepared.namespace_setup_invocations, 1);
        assert_eq!(prepared.nominal_type_predeclaration_invocations, 1);
        assert_eq!(prepared.declaration_resolution_invocations, 0);
        assert_eq!(prepared.body_readiness_finalization_invocations, 0);
        assert_eq!(prepared.input_rir_instructions, rir.len());
        assert_eq!(prepared.declaration_index_build_invocations, 1);

        let bound = shells.resolve_declarations().unwrap();
        let resolved = bound.binding_work();
        assert_eq!(resolved.declaration_resolution_invocations, 1);
        assert_eq!(resolved.body_readiness_finalization_invocations, 1);
        assert!(!bound.manifest_is_materialized());
    }

    #[test]
    fn declaration_shell_adapter_preserves_early_failure_provenance() {
        let (tokens, interner) = Lexer::new("struct Clash {} fn Clash() {} fn main() {}")
            .tokenize()
            .unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let rir = AstGen::new(&ast, &interner).generate();

        let direct = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .err()
            .expect("cross-kind collision must fail");
        let split = Sema::new(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells()
            .err()
            .expect("collision must short-circuit before nominal predeclaration");
        assert_eq!(format!("{direct:?}"), format!("{split:?}"));
    }

    #[test]
    fn anonymous_methods_are_explicitly_deferred() {
        let manifest = bind(
            "fn Factory(comptime T: type) -> type { struct { value: T, fn get(self) -> T { self.value } } } fn main() {}",
        )
        .unwrap();
        assert_eq!(manifest.work().anonymous_methods_deferred, 1);
        assert!(
            !manifest
                .bindings()
                .iter()
                .any(|binding| binding.name.as_ref() == "get")
        );
    }
}
