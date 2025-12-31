//! Built-in type definitions for the Rue compiler.
//!
//! This crate provides the registry of built-in types like `String`. These are
//! types that behave like user-defined structs but have runtime implementations
//! for their methods rather than generated code.
//!
//! # Architecture
//!
//! Built-in types are represented as "synthetic structs" — the compiler injects
//! them as `StructDef` entries before processing user code. This allows them to
//! flow through the same code paths as user-defined types, eliminating scattered
//! special-case handling throughout the compiler.
//!
//! # Adding a New Built-in Type
//!
//! 1. Define a `BuiltinTypeDef` constant describing the type's fields, methods,
//!    operators, and runtime functions.
//! 2. Add it to the `BUILTIN_TYPES` slice.
//! 3. Implement the runtime functions in `rue-runtime`.
//!
//! See `STRING_TYPE` for an example.

/// Binary operators that can be overloaded for built-in types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// Equality: `==`
    Eq,
    /// Inequality: `!=`
    Ne,
    /// Less than: `<`
    Lt,
    /// Less than or equal: `<=`
    Le,
    /// Greater than: `>`
    Gt,
    /// Greater than or equal: `>=`
    Ge,
}

/// Field type for built-in struct fields.
///
/// This is a simplified type representation for defining builtin struct layouts.
/// It maps to actual `Type` variants during struct injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinFieldType {
    /// 64-bit unsigned integer (used for pointers, lengths, capacities)
    U64,
    /// 8-bit unsigned integer
    U8,
    /// Boolean
    Bool,
}

/// A field in a built-in struct.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinField {
    /// Field name
    pub name: &'static str,
    /// Field type
    pub ty: BuiltinFieldType,
}

/// How the receiver is passed to a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverMode {
    /// Method takes ownership: `fn method(self)`
    ByValue,
    /// Method borrows: `fn method(&self)`
    ByRef,
    /// Method mutably borrows: `fn method(&mut self)`
    ByMutRef,
}

/// A parameter to a built-in method or associated function.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinParam {
    /// Parameter name
    pub name: &'static str,
    /// Parameter type (simplified)
    pub ty: BuiltinParamType,
}

/// Type of a parameter to a built-in function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinParamType {
    /// 64-bit unsigned integer
    U64,
    /// 8-bit unsigned integer
    U8,
    /// Boolean
    Bool,
    /// The built-in type itself (e.g., String for String methods)
    SelfType,
}

/// Return type of a built-in method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinReturnType {
    /// No return value (unit type)
    Unit,
    /// 64-bit unsigned integer
    U64,
    /// 8-bit unsigned integer
    U8,
    /// Boolean (returned as u8: 0 or 1)
    Bool,
    /// Returns the built-in type itself
    SelfType,
}

/// An operator overload for a built-in type.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinOperator {
    /// The operator being overloaded
    pub op: BinOp,
    /// Runtime function to call (e.g., `__rue_str_eq`)
    pub runtime_fn: &'static str,
    /// Whether to invert the result (for `!=` using `==` implementation)
    pub invert_result: bool,
}

/// An associated function on a built-in type (e.g., `String::new`).
#[derive(Debug, Clone, Copy)]
pub struct BuiltinAssociatedFn {
    /// Function name (e.g., "new")
    pub name: &'static str,
    /// Parameters (excluding any implicit out pointer)
    pub params: &'static [BuiltinParam],
    /// Return type
    pub return_ty: BuiltinReturnType,
    /// Runtime function name (e.g., "String__new")
    pub runtime_fn: &'static str,
}

/// An instance method on a built-in type (e.g., `s.len()`).
#[derive(Debug, Clone, Copy)]
pub struct BuiltinMethod {
    /// Method name (e.g., "len")
    pub name: &'static str,
    /// How the receiver is passed
    pub receiver_mode: ReceiverMode,
    /// Additional parameters after self
    pub params: &'static [BuiltinParam],
    /// Return type
    pub return_ty: BuiltinReturnType,
    /// Runtime function name (e.g., "String__len")
    pub runtime_fn: &'static str,
}

/// Definition of a built-in type.
///
/// This describes everything the compiler needs to know about a built-in type:
/// its layout (fields), behavior (operators), and available operations (methods).
#[derive(Debug, Clone)]
pub struct BuiltinTypeDef {
    /// Type name as it appears in source code (e.g., "String")
    pub name: &'static str,
    /// Fields that make up the struct layout
    pub fields: &'static [BuiltinField],
    /// Whether this type is Copy (can be implicitly duplicated)
    pub is_copy: bool,
    /// Runtime function to call for drop, if any
    pub drop_fn: Option<&'static str>,
    /// Supported operators and their implementations
    pub operators: &'static [BuiltinOperator],
    /// Associated functions (e.g., `String::new`)
    pub associated_fns: &'static [BuiltinAssociatedFn],
    /// Instance methods (e.g., `s.len()`)
    pub methods: &'static [BuiltinMethod],
}

// ============================================================================
// String Type Definition
// ============================================================================

/// The built-in String type.
///
/// Layout: `{ ptr: u64, len: u64, cap: u64 }` (24 bytes)
///
/// - `ptr`: Pointer to heap-allocated byte buffer (or .rodata for literals)
/// - `len`: Current length in bytes
/// - `cap`: Capacity of allocated buffer (0 for .rodata literals)
///
/// String is a move type (not Copy) because it owns heap-allocated memory.
/// The drop function checks `cap > 0` before freeing, allowing .rodata
/// literals (with `cap = 0`) to be safely dropped without freeing.
pub static STRING_TYPE: BuiltinTypeDef = BuiltinTypeDef {
    name: "String",
    fields: &[
        BuiltinField {
            name: "ptr",
            ty: BuiltinFieldType::U64,
        },
        BuiltinField {
            name: "len",
            ty: BuiltinFieldType::U64,
        },
        BuiltinField {
            name: "cap",
            ty: BuiltinFieldType::U64,
        },
    ],
    is_copy: false,
    drop_fn: Some("__rue_drop_String"),
    operators: &[
        BuiltinOperator {
            op: BinOp::Eq,
            runtime_fn: "__rue_str_eq",
            invert_result: false,
        },
        BuiltinOperator {
            op: BinOp::Ne,
            runtime_fn: "__rue_str_eq",
            invert_result: true,
        },
        // Note: String does NOT support Lt, Le, Gt, Ge
    ],
    associated_fns: &[
        BuiltinAssociatedFn {
            name: "new",
            params: &[],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__new",
        },
        BuiltinAssociatedFn {
            name: "with_capacity",
            params: &[BuiltinParam {
                name: "capacity",
                ty: BuiltinParamType::U64,
            }],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__with_capacity",
        },
    ],
    methods: &[
        // Query methods (take &self)
        BuiltinMethod {
            name: "len",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: BuiltinReturnType::U64,
            runtime_fn: "String__len",
        },
        BuiltinMethod {
            name: "capacity",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: BuiltinReturnType::U64,
            runtime_fn: "String__capacity",
        },
        BuiltinMethod {
            name: "is_empty",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: BuiltinReturnType::Bool,
            runtime_fn: "String__is_empty",
        },
        BuiltinMethod {
            name: "clone",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__clone",
        },
        // Mutation methods (take &mut self, return modified String)
        BuiltinMethod {
            name: "push_str",
            receiver_mode: ReceiverMode::ByMutRef,
            params: &[BuiltinParam {
                name: "other",
                ty: BuiltinParamType::SelfType,
            }],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__push_str",
        },
        BuiltinMethod {
            name: "push",
            receiver_mode: ReceiverMode::ByMutRef,
            params: &[BuiltinParam {
                name: "byte",
                ty: BuiltinParamType::U8,
            }],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__push",
        },
        BuiltinMethod {
            name: "clear",
            receiver_mode: ReceiverMode::ByMutRef,
            params: &[],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__clear",
        },
        BuiltinMethod {
            name: "reserve",
            receiver_mode: ReceiverMode::ByMutRef,
            params: &[BuiltinParam {
                name: "additional",
                ty: BuiltinParamType::U64,
            }],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__reserve",
        },
    ],
};

// ============================================================================
// Registry
// ============================================================================

/// All built-in types.
///
/// The compiler iterates over this to inject synthetic structs before
/// processing user code.
pub static BUILTIN_TYPES: &[&BuiltinTypeDef] = &[&STRING_TYPE];

/// Look up a built-in type by name.
pub fn get_builtin_type(name: &str) -> Option<&'static BuiltinTypeDef> {
    BUILTIN_TYPES.iter().find(|t| t.name == name).copied()
}

/// Check if a name is reserved for a built-in type.
pub fn is_reserved_type_name(name: &str) -> bool {
    BUILTIN_TYPES.iter().any(|t| t.name == name)
}

// ============================================================================
// Helper methods
// ============================================================================

impl BuiltinTypeDef {
    /// Get the number of slots this type uses in the ABI.
    ///
    /// Each field uses one slot (all fields are currently scalar types).
    pub fn slot_count(&self) -> u32 {
        self.fields.len() as u32
    }

    /// Find an associated function by name.
    pub fn find_associated_fn(&self, name: &str) -> Option<&BuiltinAssociatedFn> {
        self.associated_fns.iter().find(|f| f.name == name)
    }

    /// Find a method by name.
    pub fn find_method(&self, name: &str) -> Option<&BuiltinMethod> {
        self.methods.iter().find(|m| m.name == name)
    }

    /// Find an operator implementation.
    pub fn find_operator(&self, op: BinOp) -> Option<&BuiltinOperator> {
        self.operators.iter().find(|o| o.op == op)
    }

    /// Check if this type supports a given operator.
    pub fn supports_operator(&self, op: BinOp) -> bool {
        self.operators.iter().any(|o| o.op == op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_type_exists() {
        assert_eq!(STRING_TYPE.name, "String");
        assert_eq!(STRING_TYPE.fields.len(), 3);
        assert!(!STRING_TYPE.is_copy);
        assert_eq!(STRING_TYPE.drop_fn, Some("__rue_drop_String"));
    }

    #[test]
    fn test_string_slot_count() {
        assert_eq!(STRING_TYPE.slot_count(), 3);
    }

    #[test]
    fn test_string_associated_fns() {
        let new_fn = STRING_TYPE.find_associated_fn("new").unwrap();
        assert_eq!(new_fn.runtime_fn, "String__new");
        assert!(new_fn.params.is_empty());

        let with_cap = STRING_TYPE.find_associated_fn("with_capacity").unwrap();
        assert_eq!(with_cap.runtime_fn, "String__with_capacity");
        assert_eq!(with_cap.params.len(), 1);
    }

    #[test]
    fn test_string_methods() {
        let len = STRING_TYPE.find_method("len").unwrap();
        assert_eq!(len.runtime_fn, "String__len");
        assert_eq!(len.receiver_mode, ReceiverMode::ByRef);

        let push_str = STRING_TYPE.find_method("push_str").unwrap();
        assert_eq!(push_str.runtime_fn, "String__push_str");
        assert_eq!(push_str.receiver_mode, ReceiverMode::ByMutRef);
    }

    #[test]
    fn test_string_operators() {
        assert!(STRING_TYPE.supports_operator(BinOp::Eq));
        assert!(STRING_TYPE.supports_operator(BinOp::Ne));
        assert!(!STRING_TYPE.supports_operator(BinOp::Lt));
        assert!(!STRING_TYPE.supports_operator(BinOp::Gt));

        let eq = STRING_TYPE.find_operator(BinOp::Eq).unwrap();
        assert_eq!(eq.runtime_fn, "__rue_str_eq");
        assert!(!eq.invert_result);

        let ne = STRING_TYPE.find_operator(BinOp::Ne).unwrap();
        assert_eq!(ne.runtime_fn, "__rue_str_eq");
        assert!(ne.invert_result);
    }

    #[test]
    fn test_get_builtin_type() {
        assert!(get_builtin_type("String").is_some());
        assert!(get_builtin_type("Vec").is_none());
    }

    #[test]
    fn test_is_reserved_type_name() {
        assert!(is_reserved_type_name("String"));
        assert!(!is_reserved_type_name("MyStruct"));
    }

    #[test]
    fn test_all_string_methods_present() {
        // Verify all expected methods are defined
        let expected_methods = [
            "len", "capacity", "is_empty", "clone", "push_str", "push", "clear", "reserve",
        ];
        for name in expected_methods {
            assert!(
                STRING_TYPE.find_method(name).is_some(),
                "missing method: {}",
                name
            );
        }
    }
}
