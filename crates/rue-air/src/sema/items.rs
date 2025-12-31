//! Declaration gathering and validation.
//!
//! This module handles the declaration phases of semantic analysis:
//! - Registering type names (Phase 1)
//! - Resolving struct fields
//! - Validating @copy and @handle structs
//! - Collecting function and method signatures

use std::collections::HashSet;

use lasso::Spur;
use rue_builtins::is_reserved_type_name;
use rue_error::{
    CompileError, CompileResult, CopyStructNonCopyFieldError, ErrorKind, PreviewFeature,
};
use rue_rir::{InstData, RirDirective, RirParamMode};
use rue_span::Span;

use crate::types::{EnumDef, EnumId, StructDef, StructField, StructId, Type};

use super::{FunctionInfo, MethodInfo, Sema};

impl Sema<'_> {
    /// Phase 1: Register all type names (enum and struct IDs).
    ///
    /// This creates name → ID mappings for all enums and structs in a single pass,
    /// allowing types to reference each other in any order. Struct definitions are
    /// created with placeholder empty fields that will be filled in during phase 2.
    pub(crate) fn register_type_names(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            match &inst.data {
                InstData::EnumDecl {
                    name,
                    variants_start,
                    variants_len,
                } => {
                    let enum_id = EnumId(self.enum_defs.len() as u32);
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

                    self.enum_defs.push(EnumDef {
                        name: enum_name,
                        variants: variant_names,
                    });
                    self.enums.insert(*name, enum_id);
                }
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    is_linear,
                    name,
                    ..
                } => {
                    let struct_id = StructId(self.struct_defs.len() as u32);
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

                    // Linear types require preview feature
                    if *is_linear {
                        self.require_preview(PreviewFeature::AffineMvs, "linear types", inst.span)?;

                        // Linear types cannot be @copy
                        if is_copy {
                            return Err(CompileError::new(
                                ErrorKind::LinearStructCopy(struct_name.clone()),
                                inst.span,
                            ));
                        }
                    }

                    // Create placeholder struct def (fields will be resolved in phase 2)
                    self.struct_defs.push(StructDef {
                        name: struct_name,
                        fields: Vec::new(), // Filled in during resolve_declarations
                        is_copy,
                        is_handle,
                        is_linear: *is_linear,
                        destructor: None,  // Filled in during resolve_declarations
                        is_builtin: false, // User-defined struct
                    });
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
    pub(crate) fn resolve_declarations(&mut self) -> CompileResult<()> {
        self.resolve_struct_fields()?;
        self.resolve_remaining_declarations()
    }

    /// Resolve struct field types. Must run before @copy validation.
    fn resolve_struct_fields(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                name,
                fields_start,
                fields_len,
                ..
            } = &inst.data
            {
                let struct_id = *self.structs.get(name).unwrap();
                let struct_name = self.struct_defs[struct_id.0 as usize].name.clone();
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

                self.struct_defs[struct_id.0 as usize].fields = resolved_fields;
            }
        }
        Ok(())
    }

    /// Resolve @copy validation, destructors, functions, and methods.
    fn resolve_remaining_declarations(&mut self) -> CompileResult<()> {
        // First pass: collect all declarations and validate @copy structs
        for (_, inst) in self.rir.iter() {
            match &inst.data {
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    name,
                    ..
                } => {
                    self.validate_copy_struct(
                        *directives_start,
                        *directives_len,
                        *name,
                        inst.span,
                    )?;
                }

                InstData::DropFnDecl { type_name, .. } => {
                    self.collect_destructor(*type_name, inst.span)?;
                }

                InstData::FnDecl {
                    name,
                    params_start,
                    params_len,
                    return_type,
                    ..
                } => {
                    self.collect_function_signature(
                        *name,
                        *params_start,
                        *params_len,
                        *return_type,
                        inst.span,
                    )?;
                }

                InstData::ImplDecl {
                    type_name,
                    methods_start,
                    methods_len,
                } => {
                    self.collect_impl_methods(*type_name, *methods_start, *methods_len, inst.span)?;
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
        let struct_id = *self.structs.get(&name).unwrap();
        let struct_def = &self.struct_defs[struct_id.0 as usize];

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
    fn validate_handle_structs(&self) -> CompileResult<()> {
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
                let struct_id = *self.structs.get(name).unwrap();
                let struct_type = Type::Struct(struct_id);

                // Look for a .handle() method
                let handle_sym = self.interner.get("handle");
                let method_key = match handle_sym {
                    Some(sym) => (*name, sym),
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
                    let found_signature = format!(
                        "fn handle({}) -> {}",
                        method_info
                            .param_types
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
                if !method_info.param_types.is_empty() {
                    let params = std::iter::once(format!("self: {}", struct_name))
                        .chain(
                            method_info
                                .param_types
                                .iter()
                                .zip(&method_info.param_names)
                                .map(|(ty, name)| {
                                    format!(
                                        "{}: {}",
                                        self.interner.resolve(name),
                                        self.format_type_name(*ty)
                                    )
                                }),
                        )
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

        let struct_id = match self.structs.get(&type_name) {
            Some(id) => *id,
            None => {
                return Err(CompileError::new(
                    ErrorKind::DestructorUnknownType {
                        type_name: type_name_str,
                    },
                    span,
                ));
            }
        };

        let struct_def = &self.struct_defs[struct_id.0 as usize];
        if struct_def.destructor.is_some() {
            return Err(CompileError::new(
                ErrorKind::DuplicateDestructor {
                    type_name: type_name_str,
                },
                span,
            ));
        }

        let destructor_name = format!("{}.__drop", type_name_str);
        self.struct_defs[struct_id.0 as usize].destructor = Some(destructor_name);
        Ok(())
    }

    /// Collect a function signature for forward reference.
    pub(crate) fn collect_function_signature(
        &mut self,
        name: Spur,
        params_start: u32,
        params_len: u32,
        return_type: Spur,
        span: Span,
    ) -> CompileResult<()> {
        let ret_type = self.resolve_type(return_type, span)?;
        let params = self.rir.get_params(params_start, params_len);
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| self.resolve_type(p.ty, span))
            .collect::<CompileResult<Vec<_>>>()?;
        let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();

        self.functions.insert(
            name,
            FunctionInfo {
                param_types,
                param_modes,
                return_type: ret_type,
            },
        );
        Ok(())
    }

    /// Collect method definitions from an impl block.
    pub(crate) fn collect_impl_methods(
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
        let struct_type = Type::Struct(struct_id);

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
                let key = (type_name, *method_name);
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

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        param_names,
                        param_types,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
            }
        }
        Ok(())
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
}
