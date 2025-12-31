//! Proptest strategies for generating valid Rue source code.
//!
//! These generators produce syntactically valid (and often semantically valid)
//! Rue programs, enabling much more effective fuzzing than random byte mutation.

use proptest::prelude::*;

/// Generate a valid Rue identifier.
pub fn arb_ident() -> impl Strategy<Value = String> {
    // Start with letter or underscore, followed by alphanumerics
    prop::string::string_regex("[a-z_][a-z0-9_]{0,15}")
        .expect("valid regex")
        .prop_filter("not a keyword", |s| !is_keyword(s))
}

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "fn" | "let"
            | "mut"
            | "if"
            | "else"
            | "match"
            | "while"
            | "loop"
            | "break"
            | "continue"
            | "return"
            | "true"
            | "false"
            | "struct"
            | "enum"
            | "impl"
            | "drop"
            | "linear"
            | "self"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "bool"
            | "inout"
            | "borrow"
    )
}

/// Generate a primitive type name.
pub fn arb_primitive_type() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("i8"),
        Just("i16"),
        Just("i32"),
        Just("i64"),
        Just("u8"),
        Just("u16"),
        Just("u32"),
        Just("u64"),
        Just("bool"),
    ]
}

/// Generate an integer literal.
pub fn arb_int_literal() -> impl Strategy<Value = String> {
    prop_oneof![
        // Small integers (common case)
        (0i64..=100).prop_map(|n| n.to_string()),
        // Boundary values
        Just("0".to_string()),
        Just("127".to_string()),
        Just("128".to_string()),
        Just("255".to_string()),
        Just("256".to_string()),
        Just("32767".to_string()),
        Just("32768".to_string()),
        Just("65535".to_string()),
        Just("2147483647".to_string()),
        // Any i64
        any::<i64>().prop_map(|n| n.abs().to_string()),
    ]
}

/// Generate a boolean literal.
pub fn arb_bool_literal() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("true"), Just("false"),]
}

/// Generate a simple expression (no recursion).
pub fn arb_simple_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        arb_int_literal(),
        arb_bool_literal().prop_map(|s| s.to_string()),
        arb_ident(),
    ]
}

/// Generate a binary operator.
pub fn arb_binop() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        // Arithmetic
        Just("+"),
        Just("-"),
        Just("*"),
        Just("/"),
        Just("%"),
        // Comparison
        Just("=="),
        Just("!="),
        Just("<"),
        Just(">"),
        Just("<="),
        Just(">="),
        // Logical
        Just("&&"),
        Just("||"),
        // Bitwise
        Just("&"),
        Just("|"),
        Just("^"),
        Just("<<"),
        Just(">>"),
    ]
}

/// Generate a unary operator.
pub fn arb_unaryop() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("-"), Just("!"), Just("~"),]
}

/// Generate an expression with configurable depth.
pub fn arb_expr(depth: u32) -> BoxedStrategy<String> {
    if depth == 0 {
        arb_simple_expr().boxed()
    } else {
        prop_oneof![
            // Simple expression
            arb_simple_expr(),
            // Binary expression
            (arb_expr(depth - 1), arb_binop(), arb_expr(depth - 1))
                .prop_map(|(l, op, r)| format!("({} {} {})", l, op, r)),
            // Unary expression
            (arb_unaryop(), arb_expr(depth - 1)).prop_map(|(op, e)| format!("({}{})", op, e)),
            // Parenthesized
            arb_expr(depth - 1).prop_map(|e| format!("({})", e)),
            // If expression
            (
                arb_expr(depth - 1),
                arb_expr(depth - 1),
                arb_expr(depth - 1)
            )
                .prop_map(|(cond, then, else_)| format!(
                    "if {} {{ {} }} else {{ {} }}",
                    cond, then, else_
                )),
            // Block expression
            arb_expr(depth - 1).prop_map(|e| format!("{{ {} }}", e)),
        ]
        .boxed()
    }
}

/// Generate a let statement.
pub fn arb_let_stmt(depth: u32) -> impl Strategy<Value = String> {
    (
        prop::bool::ANY,
        arb_ident(),
        prop::option::of(arb_primitive_type()),
        arb_expr(depth),
    )
        .prop_map(|(is_mut, name, ty, expr)| {
            let mut_kw = if is_mut { "mut " } else { "" };
            match ty {
                Some(t) => format!("let {}{}: {} = {};", mut_kw, name, t, expr),
                None => format!("let {}{} = {};", mut_kw, name, expr),
            }
        })
}

/// Generate an assignment statement.
pub fn arb_assign_stmt(depth: u32) -> impl Strategy<Value = String> {
    (arb_ident(), arb_expr(depth)).prop_map(|(name, expr)| format!("{} = {};", name, expr))
}

/// Generate a return statement.
pub fn arb_return_stmt(depth: u32) -> impl Strategy<Value = String> {
    arb_expr(depth).prop_map(|e| format!("return {};", e))
}

/// Generate a statement.
pub fn arb_stmt(depth: u32) -> impl Strategy<Value = String> {
    prop_oneof![
        arb_let_stmt(depth),
        arb_assign_stmt(depth),
        arb_return_stmt(depth),
        arb_expr(depth).prop_map(|e| format!("{};", e)),
    ]
}

/// Generate a function parameter.
pub fn arb_param() -> impl Strategy<Value = String> {
    (arb_ident(), arb_primitive_type()).prop_map(|(name, ty)| format!("{}: {}", name, ty))
}

/// Generate a list of parameters.
pub fn arb_params() -> impl Strategy<Value = String> {
    prop::collection::vec(arb_param(), 0..5).prop_map(|params| params.join(", "))
}

/// Generate a function body.
pub fn arb_function_body(depth: u32) -> impl Strategy<Value = String> {
    prop::collection::vec(arb_stmt(depth), 0..10).prop_map(|stmts| {
        if stmts.is_empty() {
            "0".to_string()
        } else {
            stmts.join("\n    ")
        }
    })
}

/// Generate a complete function.
pub fn arb_function(depth: u32) -> impl Strategy<Value = String> {
    (
        arb_ident(),
        arb_params(),
        arb_primitive_type(),
        arb_function_body(depth),
    )
        .prop_map(|(name, params, ret_ty, body)| {
            format!("fn {}({}) -> {} {{\n    {}\n}}", name, params, ret_ty, body)
        })
}

/// Generate a main function.
pub fn arb_main_function(depth: u32) -> impl Strategy<Value = String> {
    (arb_primitive_type(), arb_function_body(depth))
        .prop_map(|(ret_ty, body)| format!("fn main() -> {} {{\n    {}\n}}", ret_ty, body))
}

/// Generate a struct definition.
pub fn arb_struct_def() -> impl Strategy<Value = String> {
    (
        arb_ident(),
        prop::collection::vec((arb_ident(), arb_primitive_type()), 1..5),
    )
        .prop_map(|(name, fields)| {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(n, t)| format!("    {}: {},", n, t))
                .collect();
            format!("struct {} {{\n{}\n}}", name, field_strs.join("\n"))
        })
}

/// Generate an enum definition.
pub fn arb_enum_def() -> impl Strategy<Value = String> {
    (arb_ident(), prop::collection::vec(arb_ident(), 1..5)).prop_map(|(name, variants)| {
        let variant_strs: Vec<String> = variants.iter().map(|v| format!("    {},", v)).collect();
        format!("enum {} {{\n{}\n}}", name, variant_strs.join("\n"))
    })
}

/// Generate a complete Rue program.
pub fn arb_program(depth: u32) -> impl Strategy<Value = String> {
    (
        // Optional struct definitions
        prop::collection::vec(arb_struct_def(), 0..3),
        // Optional enum definitions
        prop::collection::vec(arb_enum_def(), 0..2),
        // Optional helper functions
        prop::collection::vec(arb_function(depth), 0..3),
        // Main function (required)
        arb_main_function(depth),
    )
        .prop_map(|(structs, enums, funcs, main)| {
            let mut parts = Vec::new();
            for s in structs {
                parts.push(s);
            }
            for e in enums {
                parts.push(e);
            }
            for f in funcs {
                parts.push(f);
            }
            parts.push(main);
            parts.join("\n\n")
        })
}

/// Generate a syntactically valid but possibly semantically invalid program.
/// Good for testing error handling in semantic analysis.
pub fn arb_maybe_invalid_program(depth: u32) -> impl Strategy<Value = String> {
    prop_oneof![
        // Valid program
        arb_program(depth),
        // Missing main
        prop::collection::vec(arb_function(depth), 1..3).prop_map(|funcs| funcs.join("\n\n")),
        // Type mismatch in return
        (arb_primitive_type(), arb_expr(depth)).prop_map(|(ty, expr)| {
            // Return bool from i32 function or vice versa
            let wrong_ty = if ty == "bool" { "i32" } else { "bool" };
            format!("fn main() -> {} {{ {} }}", wrong_ty, expr)
        }),
        // Undefined variable
        (arb_ident(), arb_ident()).prop_map(|(good, bad)| {
            format!("fn main() -> i32 {{ let {} = 42; {} }}", good, bad)
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    #[test]
    fn test_arb_ident_not_keyword() {
        let mut runner = TestRunner::default();
        for _ in 0..100 {
            let val = arb_ident().new_tree(&mut runner).unwrap().current();
            assert!(!is_keyword(&val), "generated keyword: {}", val);
        }
    }

    #[test]
    fn test_arb_program_parses() {
        let mut runner = TestRunner::default();
        for _ in 0..20 {
            let program = arb_program(2).new_tree(&mut runner).unwrap().current();
            // Just verify it's valid UTF-8 and non-empty
            assert!(!program.is_empty());
            assert!(program.contains("fn main"));
        }
    }

    #[test]
    fn test_arb_expr_terminates() {
        let mut runner = TestRunner::default();
        for _ in 0..50 {
            let expr = arb_expr(3).new_tree(&mut runner).unwrap().current();
            assert!(!expr.is_empty());
        }
    }
}
