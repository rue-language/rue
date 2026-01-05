//! Anonymous struct handling.
//!
//! This module implements structural type equality for anonymous structs.
//! Two anonymous structs with the same field names/types (in order) AND
//! the same method signatures are the same type.

use crate::types::{StructDef, StructField, Type};

use super::Sema;
use super::info::AnonMethodSig;

impl Sema<'_> {
    /// Find an existing anonymous struct with the same fields and methods, or create a new one.
    ///
    /// This implements structural type equality for anonymous structs: two anonymous
    /// structs with the same field names/types (in the same order) AND the same method
    /// signatures are the same type. Method bodies do NOT affect structural equality.
    ///
    /// Returns a tuple of (Type, is_new) where is_new indicates whether the struct was
    /// newly created (true) or an existing match was found (false). Callers should only
    /// register methods for newly created structs.
    pub(crate) fn find_or_create_anon_struct(
        &mut self,
        fields: &[StructField],
        method_sigs: &[AnonMethodSig],
    ) -> (Type, bool) {
        // Check if an equivalent anonymous struct already exists
        // Anonymous structs have names starting with "__anon_struct_"
        for struct_id in self.type_pool.all_struct_ids() {
            let struct_def = self.type_pool.struct_def(struct_id);
            if struct_def.name.starts_with("__anon_struct_") {
                // Check fields match
                if struct_def.fields.len() != fields.len() {
                    continue;
                }
                let mut fields_match = true;
                for (def_field, new_field) in struct_def.fields.iter().zip(fields.iter()) {
                    if def_field.name != new_field.name || def_field.ty != new_field.ty {
                        fields_match = false;
                        break;
                    }
                }
                if !fields_match {
                    continue;
                }

                // Check method signatures match
                let existing_sigs = self
                    .anon_struct_method_sigs
                    .get(&struct_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                if existing_sigs.len() != method_sigs.len() {
                    continue;
                }
                let mut methods_match = true;
                for (existing, new) in existing_sigs.iter().zip(method_sigs.iter()) {
                    if existing != new {
                        methods_match = false;
                        break;
                    }
                }
                if methods_match {
                    // Found a matching struct - return it with is_new=false
                    return (Type::new_struct(struct_id), false);
                }
            }
        }

        // No matching struct found - create a new one
        let anon_name_temp = format!("__anon_struct_temp_{}", self.type_pool.len());
        let name_spur = self.interner.get_or_intern(&anon_name_temp);

        // Determine if the struct is Copy (all fields are Copy)
        let is_copy = fields.iter().all(|f| f.ty.is_copy_in_pool(&self.type_pool));

        let struct_def = StructDef {
            name: anon_name_temp.clone(),
            fields: fields.to_vec(),
            is_copy,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,                     // Anonymous structs are private
            file_id: rue_span::FileId::new(0), // Anonymous, no source file
        };

        let (struct_id, _) = self.type_pool.register_struct(name_spur, struct_def);

        // Store method signatures for future structural equality checks
        if !method_sigs.is_empty() {
            self.anon_struct_method_sigs
                .insert(struct_id, method_sigs.to_vec());
        }

        // Register in struct lookup
        self.structs.insert(name_spur, struct_id);

        // Update the name now that we have the ID
        let final_name = format!("__anon_struct_{}", struct_id.0);
        let final_name_spur = self.interner.get_or_intern(&final_name);

        // Update the struct definition with the correct name
        let mut updated_def = self.type_pool.struct_def(struct_id);
        updated_def.name = final_name.clone();
        self.type_pool.update_struct_def(struct_id, updated_def);

        // Update the struct lookup
        self.structs.remove(&name_spur);
        self.structs.insert(final_name_spur, struct_id);

        // Return with is_new=true
        (Type::new_struct(struct_id), true)
    }
}
