//! Anonymous struct handling.
//!
//! This module implements structural type equality for anonymous structs.
//! Two anonymous structs with the same field names/types (in order) AND
//! the same method signatures AND the same captured comptime values are the same type.

use std::collections::HashMap;

use lasso::Spur;

use crate::sema::context::ConstValue;
use crate::types::{StructDef, StructField, Type};

use super::Sema;
use super::info::AnonMethodSig;

impl Sema<'_> {
    /// Find an existing anonymous struct with the same fields, methods, and captured values, or create a new one.
    ///
    /// This implements structural type equality for anonymous structs: two anonymous
    /// structs with the same field names/types (in the same order) AND the same method
    /// signatures AND the same captured comptime values are the same type.
    ///
    /// Method bodies do NOT affect structural equality, but captured comptime values DO.
    /// This means `FixedBuffer(42)` and `FixedBuffer(100)` are different types because
    /// they capture different values, similar to how C++ templates or Zig comptime work.
    ///
    /// Returns a tuple of (Type, is_new) where is_new indicates whether the struct was
    /// newly created (true) or an existing match was found (false). Callers should only
    /// register methods for newly created structs.
    pub(crate) fn find_or_create_anon_struct(
        &mut self,
        fields: &[StructField],
        method_sigs: &[AnonMethodSig],
        captured_values: &HashMap<Spur, ConstValue>,
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
                if !methods_match {
                    continue;
                }

                // Check captured comptime values match
                let empty_map = HashMap::new();
                let existing_captures = self
                    .anon_struct_captured_values
                    .get(&struct_id)
                    .unwrap_or(&empty_map);
                if existing_captures.len() != captured_values.len() {
                    continue;
                }
                let mut captures_match = true;
                for (key, new_val) in captured_values.iter() {
                    if let Some(existing_val) = existing_captures.get(key) {
                        if existing_val != new_val {
                            captures_match = false;
                            break;
                        }
                    } else {
                        captures_match = false;
                        break;
                    }
                }
                if captures_match {
                    // Found a matching struct - return it with is_new=false
                    return (Type::new_struct(struct_id), false);
                }
            }
        }

        // No matching struct found - create a new one using ID reservation
        // This avoids the fragile two-phase naming where a temp name is replaced
        let struct_id = self.type_pool.reserve_struct_id();

        // Now we know the ID, so we can create the final name directly
        let name = format!("__anon_struct_{}", struct_id.0);
        let name_spur = self.interner.get_or_intern(&name);

        // Determine if the struct is Copy (all fields are Copy)
        let is_copy = fields.iter().all(|f| f.ty.is_copy_in_pool(&self.type_pool));

        let struct_def = StructDef {
            name,
            fields: fields.to_vec(),
            is_copy,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,                     // Anonymous structs are private
            file_id: rue_span::FileId::new(0), // Anonymous, no source file
        };

        // Complete the registration with the final name
        self.type_pool
            .complete_struct_registration(struct_id, name_spur, struct_def);

        // Store method signatures for future structural equality checks
        if !method_sigs.is_empty() {
            self.anon_struct_method_sigs
                .insert(struct_id, method_sigs.to_vec());
        }

        // Store captured comptime values for future structural equality checks and method analysis
        if !captured_values.is_empty() {
            self.anon_struct_captured_values
                .insert(struct_id, captured_values.clone());
        }

        // Register in struct lookup
        self.structs.insert(name_spur, struct_id);

        // Return with is_new=true
        (Type::new_struct(struct_id), true)
    }
}
