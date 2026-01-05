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
//! The injection happens in `Sema::inject_builtin_types()` during the declaration
//! gathering phase. After injection:
//!
//! - The type is accessible by name (e.g., `String`)
//! - Methods are registered and callable (e.g., `s.len()`)
//! - Associated functions work (e.g., `String::new()`)
//! - Operators are supported (e.g., `s1 == s2`)
//! - Drop glue is automatically generated if `drop_fn` is set
//!
//! # Adding a New Built-in Type
//!
//! To add a new built-in type (e.g., `Vec<T>` when generics are available):
//!
//! ## Step 1: Define the Type
//!
//! Create a `BuiltinTypeDef` constant describing the type's structure:
//!
//! ```rust,ignore
//! pub static VEC_TYPE: BuiltinTypeDef = BuiltinTypeDef {
//!     name: "Vec",  // How users refer to it in source code
//!     fields: &[
//!         BuiltinField { name: "ptr", ty: BuiltinFieldType::U64 },
//!         BuiltinField { name: "len", ty: BuiltinFieldType::U64 },
//!         BuiltinField { name: "cap", ty: BuiltinFieldType::U64 },
//!     ],
//!     is_copy: false,  // Vec owns heap memory, so it's a move type
//!     drop_fn: Some("__rue_drop_Vec"),  // Runtime destructor
//!     operators: &[
//!         // Vec might support equality if elements do
//!     ],
//!     associated_fns: &[
//!         BuiltinAssociatedFn {
//!             name: "new",
//!             params: &[],
//!             return_ty: BuiltinReturnType::SelfType,
//!             runtime_fn: "Vec__new",
//!         },
//!         BuiltinAssociatedFn {
//!             name: "with_capacity",
//!             params: &[BuiltinParam { name: "capacity", ty: BuiltinParamType::U64 }],
//!             return_ty: BuiltinReturnType::SelfType,
//!             runtime_fn: "Vec__with_capacity",
//!         },
//!     ],
//!     methods: &[
//!         BuiltinMethod {
//!             name: "len",
//!             receiver_mode: ReceiverMode::ByRef,
//!             params: &[],
//!             return_ty: BuiltinReturnType::U64,
//!             runtime_fn: "Vec__len",
//!         },
//!         BuiltinMethod {
//!             name: "push",
//!             receiver_mode: ReceiverMode::ByMutRef,
//!             params: &[BuiltinParam { name: "value", ty: BuiltinParamType::U64 }],
//!             return_ty: BuiltinReturnType::SelfType,
//!             runtime_fn: "Vec__push",
//!         },
//!         // ... more methods
//!     ],
//! };
//! ```
//!
//! ## Step 2: Register the Type
//!
//! Add it to the `BUILTIN_TYPES` slice:
//!
//! ```rust,ignore
//! pub static BUILTIN_TYPES: &[&BuiltinTypeDef] = &[
//!     &STRING_TYPE,
//!     &VEC_TYPE,  // Add new types here
//! ];
//! ```
//!
//! ## Step 3: Implement Runtime Functions
//!
//! In `rue-runtime`, implement the functions referenced in the type definition:
//!
//! ```rust,ignore
//! // In rue-runtime/src/lib.rs or a new module
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn Vec__new(out: *mut u64) {
//!     // Initialize empty Vec at `out` pointer
//!     unsafe {
//!         *out = 0;           // ptr = null
//!         *out.add(1) = 0;    // len = 0
//!         *out.add(2) = 0;    // cap = 0
//!     }
//! }
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn __rue_drop_Vec(ptr: *mut u64) {
//!     // Free the Vec's heap allocation if any
//!     unsafe {
//!         let data_ptr = *ptr as *mut u8;
//!         let cap = *ptr.add(2);
//!         if cap > 0 {
//!             __rue_free(data_ptr, cap as usize);
//!         }
//!     }
//! }
//! ```
//!
//! ## Naming Conventions
//!
//! - **Associated functions**: `TypeName__function_name` (e.g., `String__new`)
//! - **Methods**: `TypeName__method_name` (e.g., `String__len`)
//! - **Drop functions**: `__rue_drop_TypeName` (e.g., `__rue_drop_String`)
//! - **Operators**: `__rue_typename_op` (e.g., `__rue_str_eq`)
//!
//! ## Key Types
//!
//! - [`BuiltinTypeDef`]: Complete definition of a built-in type
//! - [`BuiltinField`]: A field in the struct layout
//! - [`BuiltinMethod`]: An instance method (takes `self`)
//! - [`BuiltinAssociatedFn`]: A static function (e.g., `Type::new()`)
//! - [`BuiltinOperator`]: Operator overload (e.g., `==`, `!=`)
//! - [`ReceiverMode`]: How `self` is passed to methods
//!
//! See [`STRING_TYPE`] for a complete working example.

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

// ============================================================================
// Built-in Enums (Target Platform)
// ============================================================================

/// Definition of a built-in enum type.
///
/// These are synthetic enums injected by the compiler before processing user code.
/// They are used for compile-time platform detection via intrinsics like
/// `@target_arch()` and `@target_os()`.
#[derive(Debug, Clone)]
pub struct BuiltinEnumDef {
    /// Enum name as it appears in source code (e.g., "Arch")
    pub name: &'static str,
    /// Variant names in order (index matches variant_index in EnumVariant)
    pub variants: &'static [&'static str],
}

/// The built-in Arch enum for CPU architecture detection.
///
/// Variants:
/// - `X86_64` (index 0): x86-64 / AMD64
/// - `Aarch64` (index 1): ARM64 / AArch64
///
/// Used with `@target_arch()` intrinsic for platform-specific code.
pub static ARCH_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "Arch",
    variants: &["X86_64", "Aarch64"],
};

/// The built-in Os enum for operating system detection.
///
/// Variants:
/// - `Linux` (index 0): Linux
/// - `Macos` (index 1): macOS / Darwin
///
/// Used with `@target_os()` intrinsic for platform-specific code.
pub static OS_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "Os",
    variants: &["Linux", "Macos"],
};

/// All built-in enums.
///
/// The compiler iterates over this to inject synthetic enums before
/// processing user code.
pub static BUILTIN_ENUMS: &[&BuiltinEnumDef] = &[&ARCH_ENUM, &OS_ENUM];

/// Look up a built-in enum by name.
pub fn get_builtin_enum(name: &str) -> Option<&'static BuiltinEnumDef> {
    BUILTIN_ENUMS.iter().find(|e| e.name == name).copied()
}

/// Check if a name is reserved for a built-in enum.
pub fn is_reserved_enum_name(name: &str) -> bool {
    BUILTIN_ENUMS.iter().any(|e| e.name == name)
}

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

    // ========================================================================
    // Built-in Enum Tests
    // ========================================================================

    #[test]
    fn test_arch_enum() {
        assert_eq!(ARCH_ENUM.name, "Arch");
        assert_eq!(ARCH_ENUM.variants.len(), 2);
        assert_eq!(ARCH_ENUM.variants[0], "X86_64");
        assert_eq!(ARCH_ENUM.variants[1], "Aarch64");
    }

    #[test]
    fn test_os_enum() {
        assert_eq!(OS_ENUM.name, "Os");
        assert_eq!(OS_ENUM.variants.len(), 2);
        assert_eq!(OS_ENUM.variants[0], "Linux");
        assert_eq!(OS_ENUM.variants[1], "Macos");
    }

    #[test]
    fn test_get_builtin_enum() {
        assert!(get_builtin_enum("Arch").is_some());
        assert!(get_builtin_enum("Os").is_some());
        assert!(get_builtin_enum("Target").is_none());
    }

    #[test]
    fn test_is_reserved_enum_name() {
        assert!(is_reserved_enum_name("Arch"));
        assert!(is_reserved_enum_name("Os"));
        assert!(!is_reserved_enum_name("MyEnum"));
    }

    #[test]
    fn test_builtin_enums_count() {
        assert_eq!(BUILTIN_ENUMS.len(), 2);
    }
}
