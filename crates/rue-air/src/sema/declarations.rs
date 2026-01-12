//! Declaration gathering for semantic analysis.
//!
//! This module handles the first phase of semantic analysis: gathering all
//! type and function declarations from the RIR. This includes:
//!
//! - Registering struct and enum type names
//! - Resolving struct field types
//! - Collecting function signatures
//! - Collecting method signatures from impl blocks
//! - Validating @copy and @handle structs

use std::collections::{HashMap, HashSet};

use lasso::Spur;
use rue_builtins::is_reserved_type_name;
use rue_error::{
    CompileError, CompileResult, CopyStructNonCopyFieldError, ErrorKind, PreviewFeature, ice,
};
use rue_rir::{InstData, InstRef, RirDirective, RirParamMode};
use rue_span::Span;

use super::{ConstInfo, FunctionInfo, InferenceContext, MethodInfo, Sema};
use crate::inference::{FunctionSig, MethodSig};
use crate::types::{EnumDef, StructDef, StructField, StructId, Type};

impl<'a> Sema<'a> {
    /// Build an `InferenceContext` from the collected type information.
    ///
    /// This should be called after the collection phase and builds the
    /// pre-computed maps needed for Hindley-Milner type inference.
    /// Building this once and reusing for all function analyses avoids
    /// the O(n²) cost of rebuilding these maps per function.
    ///
    /// # Performance
    ///
    /// This converts all function/method signatures to use `InferType`
    /// (which handles arrays structurally rather than by ID). This conversion
    /// is done once instead of per-function.
    pub fn build_inference_context(&self) -> InferenceContext {
        // Build function signatures with InferType for constraint generation
        let func_sigs: HashMap<Spur, FunctionSig> = self
            .functions
            .iter()
            .map(|(name, info)| {
                (
                    *name,
                    FunctionSig {
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                        is_generic: info.is_generic,
                        param_modes: self.param_arena.modes(info.params).to_vec(),
                        param_comptime: self.param_arena.comptime(info.params).to_vec(),
                        param_names: self.param_arena.names(info.params).to_vec(),
                        return_type_sym: info.return_type_sym,
                    },
                )
            })
            .collect();

        // Build struct types map (name -> Type::new_struct(id))
        let struct_types: HashMap<Spur, Type> = self
            .structs
            .iter()
            .map(|(name, id)| (*name, Type::new_struct(*id)))
            .collect();

        // Build enum types map (name -> Type::new_enum(id))
        let enum_types: HashMap<Spur, Type> = self
            .enums
            .iter()
            .map(|(name, id)| (*name, Type::new_enum(*id)))
            .collect();

        // Build method signatures with InferType for constraint generation
        let method_sigs: HashMap<(StructId, Spur), MethodSig> = self
            .methods
            .iter()
            .map(|((struct_id, method_name), info)| {
                (
                    (*struct_id, *method_name),
                    MethodSig {
                        struct_type: info.struct_type,
                        has_self: info.has_self,
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                    },
                )
            })
            .collect();

        InferenceContext {
            func_sigs,
            struct_types,
            enum_types,
            method_sigs,
        }
    }
    /// Check if a directive list contains the @copy directive
    pub(crate) fn has_copy_directive(&self, directives: &[RirDirective]) -> bool {
        let copy_sym = self.interner.get("copy");
        for directive in directives {
            if Some(directive.name) == copy_sym {
                return true;
            }
        }
        false
    }

    /// Check if a directive list contains the @handle directive
    pub(crate) fn has_handle_directive(&self, directives: &[RirDirective]) -> bool {
        let handle_sym = self.interner.get("handle");
        for directive in directives {
            if Some(directive.name) == handle_sym {
                return true;
            }
        }
        false
    }
    /// Phase 1: Register all type names (enum and struct IDs).
    ///
    /// This creates name → ID mappings for all enums and structs in a single pass,
    /// allowing types to reference each other in any order. Struct definitions are
    /// created with placeholder empty fields that will be filled in during phase 2.
    pub(crate) fn register_type_names(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            match &inst.data {
                InstData::EnumDecl {
                    is_pub,
                    name,
                    variants_start,
                    variants_len,
                } => {
                    let enum_name = self.interner.resolve(&*name).to_string();

                    // Check for collision with built-in type names
                    if is_reserved_type_name(&enum_name) {
                        return Err(CompileError::new(
                            ErrorKind::ReservedTypeName {
                                type_name: enum_name,
                            },
                            inst.span,
                        ));
                    }

                    // Check for duplicate type definitions (struct or enum with same name)
                    if self.enums.contains_key(name) || self.structs.contains_key(name) {
                        return Err(CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: enum_name,
                            },
                            inst.span,
                        ));
                    }

                    let variants = self.rir.get_symbols(*variants_start, *variants_len);

                    // Check for duplicate variant names
                    let mut seen_variants: HashSet<Spur> = HashSet::new();
                    for variant_name in &variants {
                        if !seen_variants.insert(*variant_name) {
                            let variant_name_str =
                                self.interner.resolve(&*variant_name).to_string();
                            return Err(CompileError::new(
                                ErrorKind::DuplicateVariant {
                                    enum_name: enum_name.clone(),
                                    variant_name: variant_name_str,
                                },
                                inst.span,
                            ));
                        }
                    }

                    // Convert variant symbols to strings
                    let variant_names: Vec<String> = variants
                        .iter()
                        .map(|v| self.interner.resolve(&*v).to_string())
                        .collect();

                    let enum_def = EnumDef {
                        name: enum_name,
                        variants: variant_names,
                        is_pub: *is_pub,
                        file_id: inst.span.file_id,
                    };

                    // Register in type pool and get pool-based EnumId
                    let (enum_id, _) = self.type_pool.register_enum(*name, enum_def);

                    // Register in enum lookup with pool-based EnumId
                    self.enums.insert(*name, enum_id);
                }
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    is_pub,
                    is_linear,
                    name,
                    ..
                } => {
                    let struct_name = self.interner.resolve(&*name).to_string();

                    // Check for collision with built-in type names
                    if is_reserved_type_name(&struct_name) {
                        return Err(CompileError::new(
                            ErrorKind::ReservedTypeName {
                                type_name: struct_name,
                            },
                            inst.span,
                        ));
                    }

                    // Check for duplicate type definitions (struct or enum with same name)
                    if self.structs.contains_key(name) || self.enums.contains_key(name) {
                        return Err(CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: struct_name,
                            },
                            inst.span,
                        ));
                    }

                    let directives = self.rir.get_directives(*directives_start, *directives_len);
                    let is_copy = self.has_copy_directive(&directives);
                    let is_handle = self.has_handle_directive(&directives);

                    // Linear types cannot be @copy
                    if *is_linear && is_copy {
                        return Err(CompileError::new(
                            ErrorKind::LinearStructCopy(struct_name.clone()),
                            inst.span,
                        ));
                    }

                    // Create placeholder struct def (fields will be resolved in phase 2)
                    let struct_def = StructDef {
                        name: struct_name,
                        fields: Vec::new(), // Filled in during resolve_declarations
                        is_copy,
                        is_handle,
                        is_linear: *is_linear,
                        destructor: None,  // Filled in during resolve_declarations
                        is_builtin: false, // User-defined struct
                        is_pub: *is_pub,
                        file_id: inst.span.file_id,
                    };

                    // Register in type pool and get pool-based StructId
                    let (struct_id, _) = self.type_pool.register_struct(*name, struct_def);

                    // Register in struct lookup with pool-based StructId
                    self.structs.insert(*name, struct_id);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Phase 2: Resolve all declarations.
    ///
    /// Now that all type names are registered, this resolves:
    /// - Struct field types (must be done first for @copy validation)
    /// - @copy struct validation, destructors, functions, and methods
    ///
    /// # Array Type Registration
    ///
    /// Array types from explicit type annotations (struct fields, function parameters,
    /// return types, local variable annotations) are registered during this phase via
    /// `resolve_type()` calls. Array types from literals (inferred during HM inference)
    /// are created on-demand via the thread-safe `TypeInternPool` during function
    /// body analysis.
    pub(crate) fn resolve_declarations(&mut self) -> CompileResult<()> {
        self.resolve_struct_fields()?;
        self.resolve_remaining_declarations()?;
        Ok(())
    }

    /// Resolve struct field types. Must run before @copy validation.
    pub(crate) fn resolve_struct_fields(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                name,
                fields_start,
                fields_len,
                ..
            } = &inst.data
            {
                let name_str = self.interner.resolve(&*name).to_string();
                // Verify the struct exists in our lookup table
                if !self.structs.contains_key(name) {
                    return Err(CompileError::new(
                        ErrorKind::InternalError(
                            ice!(
                                "struct not found in struct map",
                                phase: "sema/declarations",
                                details: {
                                    "struct_name" => name_str.to_string()
                                }
                            )
                            .to_string(),
                        ),
                        inst.span,
                    ));
                }

                // Get the struct ID from the lookup table
                let struct_id = *self.structs.get(name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(
                            ice!(
                                "struct not found in structs map",
                                phase: "sema/declarations",
                                details: {
                                    "struct_name" => name_str.to_string()
                                }
                            )
                            .to_string(),
                        ),
                        inst.span,
                    )
                })?;

                let struct_name = name_str.clone();
                let fields = self.rir.get_field_decls(*fields_start, *fields_len);

                // Check for duplicate field names
                let mut seen_fields: HashSet<Spur> = HashSet::new();
                for (field_name, _) in &fields {
                    if !seen_fields.insert(*field_name) {
                        let field_name_str = self.interner.resolve(&*field_name).to_string();
                        return Err(CompileError::new(
                            ErrorKind::DuplicateField {
                                struct_name,
                                field_name: field_name_str,
                            },
                            inst.span,
                        ));
                    }
                }

                // Resolve field types
                let mut resolved_fields = Vec::new();
                for (field_name, field_type) in &fields {
                    let field_ty = self.resolve_type(*field_type, inst.span)?;
                    resolved_fields.push(StructField {
                        name: self.interner.resolve(&*field_name).to_string(),
                        ty: field_ty,
                    });
                }

                // Update the struct definition in the pool with resolved fields
                let mut struct_def = self.type_pool.struct_def(struct_id);
                struct_def.fields = resolved_fields;
                self.type_pool.update_struct_def(struct_id, struct_def);
            }
        }
        Ok(())
    }

    /// Resolve @copy validation, destructors, functions, and methods.
    pub(crate) fn resolve_remaining_declarations(&mut self) -> CompileResult<()> {
        // First pass: collect all declarations and validate @copy structs
        for (_, inst) in self.rir.iter() {
            match &inst.data {
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    name,
                    methods_start,
                    methods_len,
                    ..
                } => {
                    self.validate_copy_struct(
                        *directives_start,
                        *directives_len,
                        *name,
                        inst.span,
                    )?;
                    // Collect methods defined inline in the struct
                    self.collect_struct_methods(*name, *methods_start, *methods_len, inst.span)?;
                }

                InstData::DropFnDecl { type_name, .. } => {
                    self.collect_destructor(*type_name, inst.span)?;
                }

                InstData::FnDecl {
                    is_pub,
                    is_unchecked,
                    name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    ..
                } => {
                    // Skip methods (has_self = true) - these are handled elsewhere:
                    // - Named struct methods are collected via ImplDecl
                    // - Anonymous struct methods are registered during comptime evaluation
                    if *has_self {
                        continue;
                    }

                    // Unchecked functions require preview feature
                    if *is_unchecked {
                        self.require_preview(
                            PreviewFeature::UncheckedCode,
                            "unchecked functions",
                            inst.span,
                        )?;
                    }
                    self.collect_function_signature(
                        *name,
                        *params_start,
                        *params_len,
                        *return_type,
                        *body,
                        inst.span,
                        *is_pub,
                    )?;
                }

                InstData::ConstDecl {
                    is_pub, name, init, ..
                } => {
                    self.collect_const_declaration(*name, *is_pub, *init, inst.span)?;
                }

                _ => {}
            }
        }

        // Second pass: validate @handle structs (after all methods are collected)
        self.validate_handle_structs()?;

        Ok(())
    }

    /// Validate that a @copy struct only contains Copy type fields.
    fn validate_copy_struct(
        &self,
        directives_start: u32,
        directives_len: u32,
        name: Spur,
        span: Span,
    ) -> CompileResult<()> {
        let directives = self.rir.get_directives(directives_start, directives_len);
        if !self.has_copy_directive(&directives) {
            return Ok(());
        }

        let struct_name = self.interner.resolve(&name).to_string();
        // Verify struct exists in our lookup
        if !self.structs.contains_key(&name) {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    ice!(
                        "struct not found during @copy validation",
                        phase: "sema/declarations",
                        details: {
                            "struct_name" => struct_name.clone()
                        }
                    )
                    .to_string(),
                ),
                span,
            ));
        }

        // Get the struct ID from the lookup table
        let struct_id = *self.structs.get(&name).ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(
                    ice!(
                        "struct not found during @copy validation",
                        phase: "sema/declarations",
                        details: {
                            "struct_name" => struct_name.clone()
                        }
                    )
                    .to_string(),
                ),
                span,
            )
        })?;

        // Get struct definition from the pool
        let struct_def = self.type_pool.struct_def(struct_id);

        for field in &struct_def.fields {
            if !self.is_type_copy(field.ty) {
                let field_type_name = self.format_type_name(field.ty);
                return Err(CompileError::new(
                    ErrorKind::CopyStructNonCopyField(Box::new(CopyStructNonCopyFieldError {
                        struct_name,
                        field_name: field.name.clone(),
                        field_type: field_type_name,
                    })),
                    span,
                ));
            }
        }
        Ok(())
    }

    /// Validate that all @handle structs have a valid .handle() method.
    ///
    /// This runs after all methods are collected so we can look up
    /// method signatures in the `methods` map.
    pub(crate) fn validate_handle_structs(&self) -> CompileResult<()> {
        // We need to iterate through structs and find their spans
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                directives_start,
                directives_len,
                name,
                ..
            } = &inst.data
            {
                let directives = self.rir.get_directives(*directives_start, *directives_len);
                if !self.has_handle_directive(&directives) {
                    continue;
                }

                let struct_name = self.interner.resolve(&*name).to_string();
                let struct_id = *self.structs.get(name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(
                            ice!(
                                "struct not found during @handle validation",
                                phase: "sema/declarations",
                                details: {
                                    "struct_name" => struct_name.clone()
                                }
                            )
                            .to_string(),
                        ),
                        inst.span,
                    )
                })?;
                let struct_type = Type::new_struct(struct_id);

                // Look for a .handle() method using StructId
                let handle_sym = self.interner.get("handle");
                let method_key = match handle_sym {
                    Some(sym) => (struct_id, sym),
                    None => {
                        // "handle" not interned means no .handle() method exists
                        return Err(CompileError::new(
                            ErrorKind::HandleStructMissingMethod { struct_name },
                            inst.span,
                        ));
                    }
                };

                let method_info = match self.methods.get(&method_key) {
                    Some(info) => info,
                    None => {
                        return Err(CompileError::new(
                            ErrorKind::HandleStructMissingMethod { struct_name },
                            inst.span,
                        ));
                    }
                };

                // Validate: must be a method (has self), not associated function
                if !method_info.has_self {
                    let param_types = self.param_arena.types(method_info.params);
                    let found_signature = format!(
                        "fn handle({}) -> {}",
                        param_types
                            .iter()
                            .map(|t| self.format_type_name(*t))
                            .collect::<Vec<_>>()
                            .join(", "),
                        self.format_type_name(method_info.return_type)
                    );
                    return Err(CompileError::new(
                        ErrorKind::HandleMethodWrongSignature {
                            struct_name,
                            found_signature,
                        },
                        method_info.span,
                    ));
                }

                // Validate: should take no extra parameters (just self)
                let param_types = self.param_arena.types(method_info.params);
                if !param_types.is_empty() {
                    let param_names = self.param_arena.names(method_info.params);
                    let params = std::iter::once(format!("self: {}", struct_name))
                        .chain(param_types.iter().zip(param_names).map(|(ty, name)| {
                            format!(
                                "{}: {}",
                                self.interner.resolve(name),
                                self.format_type_name(*ty)
                            )
                        }))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let found_signature = format!(
                        "fn handle({}) -> {}",
                        params,
                        self.format_type_name(method_info.return_type)
                    );
                    return Err(CompileError::new(
                        ErrorKind::HandleMethodWrongSignature {
                            struct_name,
                            found_signature,
                        },
                        method_info.span,
                    ));
                }

                // Validate: return type must be the same struct type
                if method_info.return_type != struct_type {
                    let found_signature = format!(
                        "fn handle(self: {}) -> {}",
                        struct_name,
                        self.format_type_name(method_info.return_type)
                    );
                    return Err(CompileError::new(
                        ErrorKind::HandleMethodWrongSignature {
                            struct_name,
                            found_signature,
                        },
                        method_info.span,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Collect a destructor definition and register it with its struct.
    fn collect_destructor(&mut self, type_name: Spur, span: Span) -> CompileResult<()> {
        let type_name_str = self.interner.resolve(&type_name).to_string();

        // Verify the struct exists
        if !self.structs.contains_key(&type_name) {
            return Err(CompileError::new(
                ErrorKind::DestructorUnknownType {
                    type_name: type_name_str,
                },
                span,
            ));
        }

        // Get the struct ID from the lookup table
        let struct_id = *self.structs.get(&type_name).ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(
                    ice!(
                        "struct not found during destructor collection",
                        phase: "sema/declarations",
                        details: {
                            "struct_name" => type_name_str.to_string()
                        }
                    )
                    .to_string(),
                ),
                span,
            )
        })?;

        let mut struct_def = self.type_pool.struct_def(struct_id);
        if struct_def.destructor.is_some() {
            return Err(CompileError::new(
                ErrorKind::DuplicateDestructor {
                    type_name: type_name_str,
                },
                span,
            ));
        }

        let destructor_name = format!("{}.__drop", type_name_str);
        struct_def.destructor = Some(destructor_name);
        self.type_pool.update_struct_def(struct_id, struct_def);
        Ok(())
    }

    /// Collect a function signature for forward reference.
    fn collect_function_signature(
        &mut self,
        name: Spur,
        params_start: u32,
        params_len: u32,
        return_type_sym: Spur,
        body: InstRef,
        span: Span,
        is_pub: bool,
    ) -> CompileResult<()> {
        let params = self.rir.get_params(params_start, params_len);

        let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
        let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();

        // Check if this function has any comptime TYPE parameters (not value parameters).
        // A type parameter is a comptime param where the type is `type`.
        // - `comptime T: type` -> type parameter (is_generic = true)
        // - `comptime n: i32` -> value parameter (is_generic = false)
        let type_sym = self.interner.get_or_intern("type");
        let is_generic = params.iter().any(|p| p.is_comptime && p.ty == type_sym);

        // Collect type parameter names (comptime parameters whose type is `type`)
        let type_param_names: Vec<Spur> = params
            .iter()
            .filter(|p| p.is_comptime && p.ty == type_sym)
            .map(|p| p.name)
            .collect();

        // For generic functions, we defer type resolution of type parameters until specialization.
        // We use Type::COMPTIME_TYPE as a placeholder for comptime T: type parameters.
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| {
                if p.is_comptime && p.ty == type_sym {
                    // For comptime TYPE parameters (comptime T: type), the type is `type`
                    Ok(Type::COMPTIME_TYPE)
                } else if type_param_names.contains(&p.ty) {
                    // This parameter's type is a type parameter (e.g., `x: T` where T is comptime)
                    // Use ComptimeType as a placeholder - actual type determined at specialization
                    Ok(Type::COMPTIME_TYPE)
                } else {
                    // Regular params OR comptime VALUE params (comptime n: i32)
                    self.resolve_type(p.ty, span)
                }
            })
            .collect::<CompileResult<Vec<_>>>()?;
        let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();

        // For generic functions, we can't resolve the return type yet if it references
        // a type parameter. For now, check if it matches any type parameter name.
        let ret_type = if type_param_names.contains(&return_type_sym) {
            // Return type is a type parameter - use placeholder
            Type::COMPTIME_TYPE
        } else {
            self.resolve_type(return_type_sym, span)?
        };

        // Allocate parameter data in the arena
        let params_range = self.param_arena.alloc(
            param_names.into_iter(),
            param_types.into_iter(),
            param_modes.into_iter(),
            param_comptime.into_iter(),
        );

        self.functions.insert(
            name,
            FunctionInfo {
                params: params_range,
                return_type: ret_type,
                return_type_sym,
                body,
                span,
                is_generic,
                is_pub,
                file_id: span.file_id,
            },
        );
        Ok(())
    }

    /// Collect methods defined inline in a struct.
    fn collect_struct_methods(
        &mut self,
        type_name: Spur,
        methods_start: u32,
        methods_len: u32,
        span: Span,
    ) -> CompileResult<()> {
        let struct_id = match self.structs.get(&type_name) {
            Some(id) => *id,
            None => {
                let type_name_str = self.interner.resolve(&type_name).to_string();
                return Err(CompileError::new(
                    ErrorKind::UnknownType(type_name_str),
                    span,
                ));
            }
        };
        let struct_type = Type::new_struct(struct_id);

        let methods = self.rir.get_inst_refs(methods_start, methods_len);
        for method_ref in methods {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                ..
            } = &method_inst.data
            {
                // Use StructId in key to support anonymous struct methods
                let key = (struct_id, *method_name);
                if self.methods.contains_key(&key) {
                    let type_name_str = self.interner.resolve(&type_name).to_string();
                    let method_name_str = self.interner.resolve(&*method_name).to_string();
                    return Err(CompileError::new(
                        ErrorKind::DuplicateMethod {
                            type_name: type_name_str,
                            method_name: method_name_str,
                        },
                        method_inst.span,
                    ));
                }

                let params = self.rir.get_params(*params_start, *params_len);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type(p.ty, method_inst.span))
                    .collect::<CompileResult<Vec<_>>>()?;
                let ret_type = self.resolve_type(*return_type, method_inst.span)?;

                // Allocate method parameters in the arena
                let param_range = self
                    .param_arena
                    .alloc_method(param_names.into_iter(), param_types.into_iter());

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
            }
        }
        Ok(())
    }

    /// Collect a constant declaration.
    ///
    /// Constants are compile-time values. In the module system, they're primarily
    /// used for re-exports:
    /// ```rue
    /// pub const strings = @import("utils/strings.rue");
    /// ```
    ///
    /// When the initializer is an `@import(...)`, we evaluate it at compile time
    /// to resolve the module and register it in the module registry. This enables
    /// subsequent member access via `const_name.function()` syntax.
    fn collect_const_declaration(
        &mut self,
        name: Spur,
        is_pub: bool,
        init: InstRef,
        span: Span,
    ) -> CompileResult<()> {
        let name_str = self.interner.resolve(&name).to_string();

        // Check for duplicate constant names
        if self.constants.contains_key(&name) {
            return Err(CompileError::new(
                ErrorKind::DuplicateConstant {
                    name: name_str,
                    kind: "constant".to_string(),
                },
                span,
            ));
        }

        // Check for collision with function names
        if self.functions.contains_key(&name) {
            return Err(CompileError::new(
                ErrorKind::DuplicateConstant {
                    name: name_str.clone(),
                    kind: "constant (conflicts with function)".to_string(),
                },
                span,
            ));
        }

        // Evaluate the initializer at compile time to determine the constant type.
        // Currently we only handle @import(...) - other constant expressions will
        // be supported as part of the broader comptime feature (ADR-0025).
        let const_type = self.evaluate_const_init(init, span)?;

        self.constants.insert(
            name,
            ConstInfo {
                is_pub,
                ty: const_type,
                init,
                span,
            },
        );

        Ok(())
    }

    /// Evaluate a constant initializer at compile time.
    ///
    /// Currently handles:
    /// - `@import("path")` - Returns Type::Module
    /// - Integer literals - Returns the integer type
    ///
    /// Future extensions (ADR-0025 comptime) will support:
    /// - Arithmetic on constants
    /// - comptime blocks
    /// - comptime function calls
    fn evaluate_const_init(&mut self, init: InstRef, span: Span) -> CompileResult<Type> {
        let init_inst = self.rir.get(init);

        match &init_inst.data {
            // @import("path") evaluates to Type::Module at compile time
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                if *name == self.known.import {
                    // Validate exactly one argument
                    if *args_len != 1 {
                        return Err(CompileError::new(
                            ErrorKind::IntrinsicWrongArgCount {
                                name: "import".to_string(),
                                expected: 1,
                                found: *args_len as usize,
                            },
                            span,
                        ));
                    }

                    // Get the string literal argument
                    let arg_refs = self.rir.get_inst_refs(*args_start, *args_len);
                    let arg_inst = self.rir.get(arg_refs[0]);
                    let import_path = match &arg_inst.data {
                        InstData::StringConst(path_spur) => {
                            self.interner.resolve(path_spur).to_string()
                        }
                        _ => {
                            return Err(CompileError::new(
                                ErrorKind::ImportRequiresStringLiteral,
                                arg_inst.span,
                            ));
                        }
                    };

                    // Resolve the import path to an absolute file path
                    let resolved_path = self.resolve_import_path(&import_path, span)?;

                    // Register the module in the registry
                    let (module_id, _is_new) = self
                        .module_registry
                        .get_or_create(import_path, resolved_path);

                    Ok(Type::new_module(module_id))
                } else {
                    // For other intrinsics in const context, we don't support them yet
                    let intrinsic_name = self.interner.resolve(name).to_string();
                    Err(CompileError::new(
                        ErrorKind::ConstExprNotSupported {
                            expr_kind: format!("@{} intrinsic", intrinsic_name),
                        },
                        span,
                    ))
                }
            }

            // Integer literals evaluate to i32 (the default integer type)
            // Note: RIR doesn't distinguish between integer types at this level;
            // type inference happens later. For now, we treat all integer consts as i32.
            InstData::IntConst(_) => Ok(Type::I32),

            // Boolean literals
            InstData::BoolConst(_) => Ok(Type::BOOL),

            // Unit literal
            InstData::UnitConst => Ok(Type::UNIT),

            // String literals
            InstData::StringConst(_) => {
                // String constants would need the String type
                // For now, we don't support them in const context
                Err(CompileError::new(
                    ErrorKind::ConstExprNotSupported {
                        expr_kind: "string literals".to_string(),
                    },
                    span,
                ))
            }

            // Other expressions are not yet supported in const context
            _ => Err(CompileError::new(
                ErrorKind::ConstExprNotSupported {
                    expr_kind: "this expression".to_string(),
                },
                span,
            )),
        }
    }
}
