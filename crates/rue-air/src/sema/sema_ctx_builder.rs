//! Builtin method/assoc-fn signature registration for inference.
//!
//! Builtin types' methods have no RIR bodies, so the constraint generator
//! can't discover their signatures the way it does for user methods; this
//! registers equivalents from the `rue-builtins` registry. (RUE-95/RUE-117)

use std::collections::HashMap;

use lasso::Spur;

use crate::inference::MethodSig;
use crate::types::{StructId, Type};

use super::Sema;

impl<'a> Sema<'a> {
    /// Builtin methods live in the `rue-builtins` registry, not in `self.methods`
    /// (they have no RIR body), so the constraint generator can't find them by the
    /// `(StructId, method_name)` key it uses for user methods. We add an equivalent
    /// `MethodSig` for each builtin method whose name is interned (i.e. referenced
    /// somewhere in the program — uncalled methods need no signature). Names are
    /// looked up, never interned, so this stays compatible with `&self`. (RUE-95)
    pub(crate) fn register_builtin_method_sigs(
        &self,
        method_sigs: &mut HashMap<(StructId, Spur), MethodSig>,
    ) {
        use rue_builtins::{BUILTIN_TYPES, BuiltinParamType, BuiltinReturnType};

        for builtin in BUILTIN_TYPES {
            let Some(name_spur) = self.interner.get(builtin.name) else {
                continue;
            };
            let Some(&struct_id) = self.structs.get(&name_spur) else {
                continue;
            };
            let struct_type = Type::new_struct(struct_id);

            for method in builtin.methods {
                let Some(method_spur) = self.interner.get(method.name) else {
                    continue; // method never referenced in this program
                };
                let param_types = method
                    .params
                    .iter()
                    .map(|p| {
                        let ty = match p.ty {
                            BuiltinParamType::U64 => Type::U64,
                            BuiltinParamType::U8 => Type::U8,
                            BuiltinParamType::Bool => Type::BOOL,
                            BuiltinParamType::SelfType => struct_type,
                        };
                        self.type_to_infer_type(ty)
                    })
                    .collect();
                let return_ty = match method.return_ty {
                    BuiltinReturnType::Unit => Type::UNIT,
                    BuiltinReturnType::U64 => Type::U64,
                    BuiltinReturnType::U8 => Type::U8,
                    BuiltinReturnType::Bool => Type::BOOL,
                    BuiltinReturnType::SelfType => struct_type,
                };
                method_sigs.insert(
                    (struct_id, method_spur),
                    MethodSig {
                        struct_type,
                        has_self: true,
                        param_types,
                        return_type: self.type_to_infer_type(return_ty),
                    },
                );
            }

            // Associated functions (e.g. `String::new`, `String::with_capacity`) are
            // resolved by inference through the same `(StructId, name)` method map (see
            // `InstData::AssocFnCall` in inference/generate.rs). Like methods, they live
            // only in the rue-builtins registry, so without registering them an untyped
            // literal arg (`String::with_capacity(8)`) isn't constrained to the param's
            // `u64` and a result feeding a literal resolves to `<error>`. (RUE-95 sibling)
            for assoc_fn in builtin.associated_fns {
                let Some(fn_spur) = self.interner.get(assoc_fn.name) else {
                    continue; // never referenced in this program
                };
                let param_types = assoc_fn
                    .params
                    .iter()
                    .map(|p| {
                        let ty = match p.ty {
                            BuiltinParamType::U64 => Type::U64,
                            BuiltinParamType::U8 => Type::U8,
                            BuiltinParamType::Bool => Type::BOOL,
                            BuiltinParamType::SelfType => struct_type,
                        };
                        self.type_to_infer_type(ty)
                    })
                    .collect();
                let return_ty = match assoc_fn.return_ty {
                    BuiltinReturnType::Unit => Type::UNIT,
                    BuiltinReturnType::U64 => Type::U64,
                    BuiltinReturnType::U8 => Type::U8,
                    BuiltinReturnType::Bool => Type::BOOL,
                    BuiltinReturnType::SelfType => struct_type,
                };
                method_sigs.insert(
                    (struct_id, fn_spur),
                    MethodSig {
                        struct_type,
                        has_self: false,
                        param_types,
                        return_type: self.type_to_infer_type(return_ty),
                    },
                );
            }
        }
    }
}
