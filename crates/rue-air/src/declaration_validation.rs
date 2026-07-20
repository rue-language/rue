//! Position-free declaration rules shared by semantic producers.
//!
//! Callers own query scheduling and source locators. These helpers own only
//! deterministic policy over canonical declaration facts, so the one-shot AIR
//! adapter and the keyed semantic graph cannot drift on the rule itself.

use std::collections::HashSet;

use rue_error::ErrorKind;

pub fn duplicate_parameter(names: impl IntoIterator<Item = impl AsRef<str>>) -> Option<ErrorKind> {
    duplicate_parameter_with_ordinal(names).map(|(kind, _)| kind)
}

pub fn duplicate_parameter_with_ordinal(
    names: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<(ErrorKind, usize)> {
    let mut seen = HashSet::new();
    for (ordinal, name) in names.into_iter().enumerate() {
        let name = name.as_ref();
        if !seen.insert(name.to_owned()) {
            return Some((
                ErrorKind::DuplicateParameter {
                    name: name.to_owned(),
                },
                ordinal,
            ));
        }
    }
    None
}

pub fn reserved_function_name(name: &str) -> Option<ErrorKind> {
    rue_builtins::is_reserved_function_name(name).then(|| ErrorKind::ReservedFunctionName {
        function_name: name.to_owned(),
    })
}

pub fn duplicate_constant(name: &str) -> ErrorKind {
    ErrorKind::DuplicateConstant {
        name: name.to_owned(),
        kind: "constant".to_owned(),
    }
}

pub fn const_cross_kind_collision(
    name: &str,
    is_value_const: bool,
    has_non_const_declaration: bool,
) -> Option<ErrorKind> {
    (is_value_const && has_non_const_declaration).then(|| ErrorKind::DuplicateFunctionDefinition {
        function_name: name.to_owned(),
    })
}

pub fn duplicate_field(
    struct_name: &str,
    fields: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<ErrorKind> {
    let mut seen = HashSet::new();
    for field in fields {
        let field = field.as_ref();
        if !seen.insert(field.to_owned()) {
            return Some(ErrorKind::DuplicateField {
                struct_name: struct_name.to_owned(),
                field_name: field.to_owned(),
            });
        }
    }
    None
}

pub fn duplicate_variant(
    enum_name: &str,
    variants: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<ErrorKind> {
    let mut seen = HashSet::new();
    for variant in variants {
        let variant = variant.as_ref();
        if !seen.insert(variant.to_owned()) {
            return Some(ErrorKind::DuplicateVariant {
                enum_name: enum_name.to_owned(),
                variant_name: variant.to_owned(),
            });
        }
    }
    None
}

pub fn linear_copy_struct(struct_name: &str, is_linear: bool, is_copy: bool) -> Option<ErrorKind> {
    (is_linear && is_copy).then(|| ErrorKind::LinearStructCopy(struct_name.to_owned()))
}

pub fn destructor_unknown_type(type_name: &str) -> ErrorKind {
    ErrorKind::DestructorUnknownType {
        type_name: type_name.to_owned(),
    }
}

pub fn duplicate_destructor(type_name: &str) -> ErrorKind {
    ErrorKind::DuplicateDestructor {
        type_name: type_name.to_owned(),
    }
}
