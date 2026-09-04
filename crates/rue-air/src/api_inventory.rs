//! Structural guard for AIR's read-only canonical import consumption boundary.

use std::collections::BTreeSet;

use syn::{
    BinOp, Expr, ExprCall, ExprField, ExprIf, ExprLet, ExprLit, ExprMatch, ExprMethodCall,
    ExprPath, ExprStruct, File, ImplItem, Item, ItemFn, ItemImpl, ItemMod, Lit, PatStruct,
    PatTupleStruct,
    parse::Parser,
    visit::{self, Visit},
};

#[derive(Clone, Debug)]
struct AstFunction {
    module: String,
    owner: Option<String>,
    name: String,
    test_only: bool,
    dispatch: bool,
    decode: bool,
    operation: bool,
    selection: bool,
    direct_selection: bool,
    canonical_selection_call: bool,
    child_traversal: bool,
    value_result: bool,
    calls: Vec<AstCall>,
}

#[derive(Clone, Debug)]
enum AstCall {
    Path {
        segments: Vec<String>,
        method: bool,
        receiver: Option<Vec<String>>,
    },
}

struct FunctionBodyVisitor {
    dispatch: bool,
    control_dispatch: bool,
    decode: bool,
    operation: bool,
    selection: bool,
    direct_selection: bool,
    canonical_selection_call: bool,
    child_traversal: bool,
    calls: Vec<AstCall>,
}

impl<'ast> Visit<'ast> for FunctionBodyVisitor {
    fn visit_pat(&mut self, pattern: &'ast syn::Pat) {
        self.decode |= pattern_has_const_value(pattern);
        visit::visit_pat(self, pattern);
    }

    fn visit_expr_match(&mut self, expression: &'ast ExprMatch) {
        self.dispatch = self.dispatch
            || expression
                .arms
                .iter()
                .any(|arm| pattern_comptime_instdata(&arm.pat));
        self.control_dispatch |= self.dispatch;
        visit::visit_expr_match(self, expression);
    }

    fn visit_expr_if(&mut self, expression: &'ast ExprIf) {
        if let Expr::Let(ExprLet { pat, .. }) = &*expression.cond {
            self.dispatch = self.dispatch || pattern_comptime_instdata(pat);
            self.control_dispatch |= self.dispatch;
        }
        visit::visit_expr_if(self, expression);
    }

    fn visit_expr_path(&mut self, expression: &'ast ExprPath) {
        self.decode |= expression
            .path
            .segments
            .iter()
            .any(|segment| segment.ident == "ConstValue");
        visit::visit_expr_path(self, expression);
    }

    fn visit_expr_field(&mut self, expression: &'ast ExprField) {
        visit::visit_expr_field(self, expression);
    }

    fn visit_expr_binary(&mut self, expression: &'ast syn::ExprBinary) {
        self.operation |= matches!(
            expression.op,
            BinOp::Add(_)
                | BinOp::Sub(_)
                | BinOp::Mul(_)
                | BinOp::Div(_)
                | BinOp::Rem(_)
                | BinOp::Shl(_)
                | BinOp::Shr(_)
                | BinOp::Lt(_)
                | BinOp::Le(_)
                | BinOp::Gt(_)
                | BinOp::Ge(_)
                | BinOp::Eq(_)
                | BinOp::Ne(_)
                | BinOp::And(_)
                | BinOp::Or(_)
        );
        visit::visit_expr_binary(self, expression);
    }

    fn visit_expr_struct(&mut self, expression: &'ast ExprStruct) {
        self.direct_selection |= expression.path.segments.iter().any(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "ComptimeOutcome" | "ComptimeSelection"
            )
        });
        visit::visit_expr_struct(self, expression);
    }

    fn visit_expr_call(&mut self, expression: &'ast ExprCall) {
        if let Expr::Path(path) = &*expression.func {
            let names = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            // Constructing a ConstValue is not decoding an input value.  A
            // peer evaluator is identified by reading a value (the engine's
            // `as_*`/integer kernels), while ordinary adapters may construct
            // typed facts as part of their handoff.
            self.decode |= names.iter().any(|name| {
                matches!(
                    name.as_str(),
                    "as_integer"
                        | "as_bool"
                        | "as_string"
                        | "as_type"
                        | "as_function"
                        | "to_i128"
                        | "to_u128"
                )
            });
            self.selection |= names
                .iter()
                .any(|name| matches!(name.as_str(), "ComptimeOutcome" | "ComptimeSelection"));
            if path.path.segments.last().is_some() {
                self.calls.push(AstCall::Path {
                    segments: path
                        .path
                        .segments
                        .iter()
                        .map(|segment| segment.ident.to_string())
                        .collect(),
                    method: false,
                    receiver: None,
                });
            }
        }
        visit::visit_expr_call(self, expression);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast ExprMethodCall) {
        self.decode |= matches!(
            expression.method.to_string().as_str(),
            "as_integer"
                | "as_bool"
                | "as_string"
                | "as_type"
                | "as_function"
                | "to_i128"
                | "to_u128"
        );
        self.child_traversal |= matches!(
            expression.method.to_string().as_str(),
            "iter" | "iter_mut" | "children" | "walk" | "visit" | "recurse" | "fold"
        );
        self.canonical_selection_call |= matches!(
            expression.method.to_string().as_str(),
            "select_branch" | "select_match"
        );
        if matches!(&*expression.receiver, Expr::Path(path) if path.path.is_ident("self")) {
            self.calls.push(AstCall::Path {
                segments: vec!["self".to_owned(), expression.method.to_string()],
                method: true,
                receiver: Some(vec!["self".to_owned()]),
            });
        } else {
            // Keep every method edge in the scoped graph.  Calls on a nested
            // expression (for example `ComptimeEngine::new(...).select`)
            // still have a stable method identity even when their receiver is
            // not a simple path.
            let mut segments = match &*expression.receiver {
                Expr::Path(path) => path
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            segments.push(expression.method.to_string());
            self.calls.push(AstCall::Path {
                segments,
                method: true,
                receiver: expression_receiver_shape(&expression.receiver),
            });
        }
        visit::visit_expr_method_call(self, expression);
    }

    fn visit_macro(&mut self, macro_call: &'ast syn::Macro) {
        if macro_call.path.is_ident("matches")
            && comptime_instdata_syntax(&macro_call.tokens.to_string())
        {
            self.dispatch = true;
            self.control_dispatch = true;
        }
        visit::visit_macro(self, macro_call);
    }
}

/// Preserve enough receiver structure for adapter calls to be authorized by
/// the exact object being used.  In particular, `self.get` must not become
/// indistinguishable from the `.get` on `self.body_rir_ref()`.
fn expression_receiver_shape(expression: &Expr) -> Option<Vec<String>> {
    match expression {
        Expr::Path(path) => Some(
            path.path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect(),
        ),
        Expr::MethodCall(call) => {
            let mut shape = expression_receiver_shape(&call.receiver)?;
            shape.push(call.method.to_string());
            Some(shape)
        }
        Expr::Call(call) => {
            let Expr::Path(path) = &*call.func else {
                return None;
            };
            Some(
                path.path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect(),
            )
        }
        _ => None,
    }
}

fn pattern_has_const_value(pattern: &syn::Pat) -> bool {
    let path = match pattern {
        syn::Pat::Path(path) => Some(&path.path),
        syn::Pat::Struct(pattern) => Some(&pattern.path),
        syn::Pat::TupleStruct(pattern) => Some(&pattern.path),
        _ => None,
    };
    path.is_some_and(|path| {
        path.segments
            .iter()
            .any(|segment| segment.ident == "ConstValue")
    })
}

/// RIR consumers also match `InstData` for declaration and bookkeeping
/// records.  Those are not value-evaluator dispatch.  Require a comptime
/// value/control variant in the pattern so the graph tracks evaluator roots
/// without treating every ordinary body projection as one.
fn comptime_instdata_syntax(text: &str) -> bool {
    const VARIANTS: &[&str] = &[
        "IntConst",
        "BoolConst",
        "StringConst",
        "UnitConst",
        "FloatConst",
        "Add",
        "Sub",
        "Mul",
        "Div",
        "Mod",
        "BitAnd",
        "BitOr",
        "BitXor",
        "Shl",
        "Shr",
        "Eq",
        "Ne",
        "Lt",
        "Le",
        "Gt",
        "Ge",
        "And",
        "Or",
        "Neg",
        "Not",
        "BitNot",
        "Branch",
        "Match",
    ];
    text.match_indices("InstData").any(|(offset, _)| {
        let preceding_is_ident = text[..offset]
            .chars()
            .next_back()
            .is_some_and(|character| character == '_' || character.is_ascii_alphanumeric());
        if preceding_is_ident {
            return false;
        }
        let suffix = &text[offset + "InstData".len()..];
        let suffix = suffix.trim_start();
        let Some(suffix) = suffix.strip_prefix("::") else {
            return false;
        };
        VARIANTS.iter().any(|variant| {
            suffix
                .trim_start()
                .strip_prefix(variant)
                .is_some_and(|rest| {
                    rest.chars().next().is_none_or(|character| {
                        !(character == '_' || character.is_ascii_alphanumeric())
                    })
                })
        })
    })
}

struct PatternComptimeInstData {
    found: bool,
}

impl<'ast> Visit<'ast> for PatternComptimeInstData {
    fn visit_pat_struct(&mut self, pattern: &'ast PatStruct) {
        self.found |= path_is_comptime_instdata(&pattern.path);
        visit::visit_pat_struct(self, pattern);
    }

    fn visit_pat_tuple_struct(&mut self, pattern: &'ast PatTupleStruct) {
        self.found |= path_is_comptime_instdata(&pattern.path);
        visit::visit_pat_tuple_struct(self, pattern);
    }
}

fn path_is_comptime_instdata(path: &syn::Path) -> bool {
    let mut segments = path.segments.iter();
    segments.any(|segment| segment.ident == "InstData")
        && path.segments.iter().any(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "IntConst"
                    | "BoolConst"
                    | "StringConst"
                    | "UnitConst"
                    | "FloatConst"
                    | "Add"
                    | "Sub"
                    | "Mul"
                    | "Div"
                    | "Mod"
                    | "BitAnd"
                    | "BitOr"
                    | "BitXor"
                    | "Shl"
                    | "Shr"
                    | "Eq"
                    | "Ne"
                    | "Lt"
                    | "Le"
                    | "Gt"
                    | "Ge"
                    | "And"
                    | "Or"
                    | "Neg"
                    | "Not"
                    | "BitNot"
                    | "Branch"
                    | "Match"
            )
        })
}

fn pattern_comptime_instdata(pattern: &syn::Pat) -> bool {
    let mut visitor = PatternComptimeInstData { found: false };
    match pattern {
        syn::Pat::Path(pattern) => visitor.found = path_is_comptime_instdata(&pattern.path),
        syn::Pat::Struct(pattern) => visitor.found = path_is_comptime_instdata(&pattern.path),
        syn::Pat::TupleStruct(pattern) => visitor.found = path_is_comptime_instdata(&pattern.path),
        _ => {}
    }
    visitor.visit_pat(pattern);
    visitor.found
}

struct AstCollector {
    module: String,
    functions: Vec<AstFunction>,
}

impl AstCollector {
    fn new(module: &str) -> Self {
        Self {
            module: module.to_owned(),
            functions: Vec::new(),
        }
    }

    fn visit_function(&mut self, function: &ItemFn, owner: Option<String>, test_only: bool) {
        let mut body = FunctionBodyVisitor {
            dispatch: false,
            control_dispatch: false,
            decode: false,
            operation: false,
            selection: false,
            direct_selection: false,
            canonical_selection_call: false,
            child_traversal: false,
            calls: Vec::new(),
        };
        body.visit_block(&function.block);
        self.functions.push(AstFunction {
            module: self.module.clone(),
            owner,
            name: function.sig.ident.to_string(),
            test_only,
            dispatch: body.dispatch && body.control_dispatch,
            decode: body.decode,
            operation: body.operation,
            selection: body.selection
                || return_mentions_direct(
                    &function.sig.output,
                    &["ComptimeOutcome", "ComptimeSelection"],
                ),
            direct_selection: body.direct_selection,
            canonical_selection_call: body.canonical_selection_call,
            child_traversal: body.child_traversal,
            value_result: return_mentions_direct(&function.sig.output, &["ConstValue"])
                && (body.decode || body.operation || body.selection),
            calls: body.calls,
        });
    }

    fn visit_impl(&mut self, item: &ItemImpl, test_only: bool) {
        let owner = match &*item.self_ty {
            syn::Type::Path(path) => path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string()),
            _ => None,
        };
        for item in &item.items {
            if let ImplItem::Fn(function) = item {
                let mut body = FunctionBodyVisitor {
                    dispatch: false,
                    control_dispatch: false,
                    decode: false,
                    operation: false,
                    selection: false,
                    direct_selection: false,
                    canonical_selection_call: false,
                    child_traversal: false,
                    calls: Vec::new(),
                };
                body.visit_block(&function.block);
                self.functions.push(AstFunction {
                    module: self.module.clone(),
                    owner: owner.clone(),
                    name: function.sig.ident.to_string(),
                    test_only,
                    dispatch: body.dispatch && body.control_dispatch,
                    decode: body.decode,
                    operation: body.operation,
                    selection: body.selection
                        || return_mentions_direct(
                            &function.sig.output,
                            &["ComptimeOutcome", "ComptimeSelection"],
                        ),
                    direct_selection: body.direct_selection,
                    canonical_selection_call: body.canonical_selection_call,
                    child_traversal: body.child_traversal,
                    value_result: return_mentions_direct(&function.sig.output, &["ConstValue"])
                        && (body.decode || body.operation || body.selection),
                    calls: body.calls,
                });
            }
        }
    }

    fn visit_items(&mut self, items: &[Item], test_only: bool) {
        for item in items {
            match item {
                Item::Fn(function) => self.visit_function(function, None, test_only),
                Item::Impl(item) => self.visit_impl(item, test_only),
                Item::Mod(ItemMod {
                    content: Some((_, items)),
                    ident,
                    attrs,
                    ..
                }) => {
                    let old = self.module.clone();
                    self.module.push('/');
                    self.module.push_str(&ident.to_string());
                    let nested_test = test_only
                        || attrs.iter().any(|attr| {
                            attr.path().is_ident("cfg")
                                && attr
                                    .parse_args::<syn::Path>()
                                    .is_ok_and(|path| path.is_ident("test"))
                        });
                    self.visit_items(items, nested_test);
                    self.module = old;
                }
                _ => {}
            }
        }
    }
}

fn return_mentions_direct(output: &syn::ReturnType, names: &[&str]) -> bool {
    let syn::ReturnType::Type(_, ty) = output else {
        return false;
    };
    let mut visitor = ReturnTypeNameVisitor {
        names,
        found: false,
        blocked: 0,
    };
    visitor.visit_type(ty);
    visitor.found
}

struct ReturnTypeNameVisitor<'a> {
    names: &'a [&'a str],
    found: bool,
    blocked: usize,
}

impl<'ast> Visit<'ast> for ReturnTypeNameVisitor<'_> {
    fn visit_type_path(&mut self, ty: &'ast syn::TypePath) {
        let is_container = ty.path.segments.iter().any(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "AHashMap" | "HashMap" | "BTreeMap" | "Vec" | "Tuple"
            )
        });
        if self.blocked == 0
            && ty
                .path
                .segments
                .iter()
                .any(|segment| self.names.iter().any(|name| segment.ident == *name))
        {
            self.found = true;
        }
        if is_container {
            self.blocked += 1;
        }
        visit::visit_type_path(self, ty);
        if is_container {
            self.blocked -= 1;
        }
    }
}

fn ast_functions(module: &str, source: &str) -> Option<Vec<AstFunction>> {
    let file: File = syn::parse_file(source).ok()?;
    let mut collector = AstCollector::new(&normalize_manifest_module(module));
    collector.visit_items(&file.items, false);
    Some(collector.functions)
}

fn is_test_only_attrs(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || (attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Path>()
                    .is_ok_and(|path| path.is_ident("test")))
    })
}

/// Identify a hand-spelled core string definition by its semantic shape, not
/// by the spelling `str`.  Generated `Str(N)` and slice views deliberately use
/// this same fat-pointer shape, so their canonical names are exempted.
fn has_core_str_definition(module: &str, source: &str) -> bool {
    fn string_literal(expression: &Expr) -> Option<String> {
        match expression {
            Expr::Lit(ExprLit {
                lit: Lit::Str(value),
                ..
            }) => Some(value.value()),
            Expr::Call(call) => call.args.first().and_then(string_literal),
            Expr::MethodCall(call) => string_literal(&call.receiver),
            Expr::Macro(expression) if expression.mac.path.is_ident("format") => expression
                .mac
                .tokens
                .clone()
                .into_iter()
                .next()
                .and_then(|token| syn::parse_str::<syn::LitStr>(&token.to_string()).ok())
                .map(|value| value.value()),
            Expr::Paren(paren) => string_literal(&paren.expr),
            Expr::Reference(reference) => string_literal(&reference.expr),
            _ => None,
        }
    }
    fn literal_name(expression: &Expr, bindings: &BTreeSet<(String, String)>) -> Option<String> {
        string_literal(expression).or_else(|| {
            let Expr::Path(path) = expression else {
                return None;
            };
            let mut segments = path.path.segments.iter();
            let Some(ident) = segments.next() else {
                return None;
            };
            if segments.next().is_some() {
                return None;
            }
            let name = ident.ident.to_string();
            bindings
                .iter()
                .find(|(binding, _)| binding == &name)
                .map(|(_, value)| value.clone())
        })
    }
    fn field_expressions(expression: &Expr) -> Option<Vec<Expr>> {
        match expression {
            Expr::Array(array) => Some(array.elems.iter().cloned().collect()),
            Expr::Macro(expression) if expression.mac.path.is_ident("vec") => {
                syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated
                    .parse2(expression.mac.tokens.clone())
                    .ok()
                    .map(|fields| fields.into_iter().collect())
            }
            _ => None,
        }
    }
    struct Visitor {
        found: bool,
        test_only: bool,
        bindings: BTreeSet<(String, String)>,
        module: String,
        owner: Option<String>,
        function: Option<String>,
    }
    impl<'ast> Visit<'ast> for Visitor {
        fn visit_item_fn(&mut self, item: &'ast ItemFn) {
            let was_test_only = self.test_only;
            let previous_function = self.function.replace(item.sig.ident.to_string());
            let previous_owner = self.owner.take();
            self.test_only |= is_test_only_attrs(&item.attrs);
            visit::visit_item_fn(self, item);
            self.test_only = was_test_only;
            self.function = previous_function;
            self.owner = previous_owner;
        }

        fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
            let previous_owner = self.owner.take();
            self.owner = match &*item.self_ty {
                syn::Type::Path(path) => path
                    .path
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string()),
                _ => None,
            };
            visit::visit_item_impl(self, item);
            self.owner = previous_owner;
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            let previous_function = self.function.replace(item.sig.ident.to_string());
            visit::visit_impl_item_fn(self, item);
            self.function = previous_function;
        }

        fn visit_local(&mut self, local: &'ast syn::Local) {
            if !self.test_only {
                if let syn::Pat::Ident(pattern) = &local.pat {
                    if let Some(value) = local
                        .init
                        .as_ref()
                        .and_then(|init| literal_name(&init.expr, &self.bindings))
                    {
                        self.bindings.replace((pattern.ident.to_string(), value));
                    }
                }
            }
            visit::visit_local(self, local);
        }

        fn visit_item_mod(&mut self, item: &'ast ItemMod) {
            let was_test_only = self.test_only;
            self.test_only |= is_test_only_attrs(&item.attrs);
            visit::visit_item_mod(self, item);
            self.test_only = was_test_only;
        }

        fn visit_expr_struct(&mut self, expression: &'ast ExprStruct) {
            if self.test_only
                || self.found
                || !expression
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "StructDef")
            {
                visit::visit_expr_struct(self, expression);
                return;
            }
            let field = |name: &str| {
                expression
                    .fields
                    .iter()
                    .find(|field| {
                        matches!(&field.member, syn::Member::Named(member) if member == name)
                    })
                    .map(|field| &field.expr)
            };
            let Some(Expr::Lit(lit)) = field("is_builtin") else {
                visit::visit_expr_struct(self, expression);
                return;
            };
            let syn::Lit::Bool(is_builtin) = &lit.lit else {
                visit::visit_expr_struct(self, expression);
                return;
            };
            if !is_builtin.value {
                visit::visit_expr_struct(self, expression);
                return;
            }
            let Some(fields) = field("fields").and_then(field_expressions) else {
                visit::visit_expr_struct(self, expression);
                return;
            };
            let names = fields
                .iter()
                .filter_map(|field| {
                    let Expr::Struct(field) = field else {
                        return None;
                    };
                    let name = field
                        .fields
                        .iter()
                        .find(|field| {
                            matches!(&field.member, syn::Member::Named(member) if member == "name")
                        })?;
                    string_literal(&name.expr)
                })
                .collect::<Vec<_>>();
            if names != ["ptr", "len"] {
                visit::visit_expr_struct(self, expression);
                return;
            }
            let generated =
                field("name").and_then(|expression| literal_name(expression, &self.bindings));
            // A computed name is how generated `Str(N)` and slice views are
            // built; only the four production constructors below may emit
            // that shape. A literal (including `Arc::from("...")`) is a
            // hand-spelled candidate and is rejected everywhere else.
            let legitimate_generated_owner = matches!(
                (
                    self.module.as_str(),
                    self.owner.as_deref(),
                    self.function.as_deref()
                ),
                (
                    "sema/body_identity",
                    Some("BodyIdentityPool"),
                    Some("get_or_create_str_fixed")
                ) | (
                    "sema/body_identity",
                    Some("BodyIdentityPool"),
                    Some("resolve")
                ) | (
                    "semantic_import",
                    Some("SemanticImportEpoch"),
                    Some("resolve_builtin_nominal_in_pool")
                ) | (
                    "semantic_import",
                    Some("SemanticImportEpoch"),
                    Some("import_type_local_with")
                )
            );
            let generated_view_name = generated.as_ref().is_some_and(|name| {
                name.starts_with("Str(") || (name.starts_with('[') && name.ends_with(']'))
            });
            let exempt = legitimate_generated_owner && (generated_view_name || generated.is_none());
            self.found = !exempt;
            visit::visit_expr_struct(self, expression);
        }
    }
    let parsed = if let Ok(file) = syn::parse_file(source) {
        let mut visitor = Visitor {
            found: false,
            test_only: false,
            bindings: BTreeSet::new(),
            module: normalize_manifest_module(module),
            owner: None,
            function: None,
        };
        visitor.visit_file(&file);
        if visitor.found {
            return true;
        }
        true
    } else {
        false
    };
    if parsed {
        return false;
    }
    // For malformed fixture text that cannot be parsed, keep a conservative
    // lexical fallback for a direct literal definition: this is the spelling
    // variation an authority guard must reject, and it also makes the
    // adversarial fixture independent of formatting details in syn's
    // expression representation.
    let compact = source.split_whitespace().collect::<String>();
    compact.contains("StructDef{name:")
        && compact.contains("is_builtin:true")
        && compact.contains("name:\"ptr\"")
        && compact.contains("name:\"len\"")
}

fn has_builtin_enum_membership(source: &str) -> bool {
    struct Visitor {
        found: bool,
    }
    impl<'ast> Visit<'ast> for Visitor {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.found |= path.segments.last().is_some_and(|segment| {
                segment.ident == "BUILTIN_ENUMS"
                    || segment.ident == "get_builtin_enum"
                    || segment.ident == "is_reserved_enum_name"
            });
            visit::visit_path(self, path);
        }
    }
    let Ok(file) = syn::parse_file(source) else {
        return false;
    };
    let mut visitor = Visitor { found: false };
    visitor.visit_file(&file);
    visitor.found
}

/// Identify a production bootstrap of the target builtin-enum universe by its
/// definition shape. Ordinary source/anonymous enums use non-public or
/// non-empty payload metadata, while the target universe is public,
/// exhaustive, and payload-free. The registration evidence is tracked per
/// function, so an unrelated `register_enum` elsewhere cannot turn a source
/// declaration into a bootstrap match.
fn has_builtin_enum_bootstrap(source: &str) -> bool {
    struct Visitor {
        found: bool,
        test_only: bool,
        function: Option<(bool, bool)>,
    }
    impl<'ast> Visit<'ast> for Visitor {
        fn visit_item_fn(&mut self, item: &'ast ItemFn) {
            let was_test_only = self.test_only;
            let previous_function = self.function.take();
            self.test_only |= is_test_only_attrs(&item.attrs);
            self.function = Some((false, false));
            visit::visit_item_fn(self, item);
            let (candidate, registered) = self.function.take().unwrap();
            self.found |= !self.test_only && candidate && registered;
            self.test_only = was_test_only;
            self.function = previous_function;
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            let previous_function = self.function.take();
            let was_test_only = self.test_only;
            self.test_only |= is_test_only_attrs(&item.attrs);
            self.function = Some((false, false));
            visit::visit_impl_item_fn(self, item);
            let (candidate, registered) = self.function.take().unwrap();
            self.found |= !self.test_only && candidate && registered;
            self.test_only = was_test_only;
            self.function = previous_function;
        }

        fn visit_item_mod(&mut self, item: &'ast ItemMod) {
            let was_test_only = self.test_only;
            self.test_only |= is_test_only_attrs(&item.attrs);
            visit::visit_item_mod(self, item);
            self.test_only = was_test_only;
        }

        fn visit_expr_struct(&mut self, expression: &'ast ExprStruct) {
            if self.test_only
                || !expression
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "EnumDef")
            {
                visit::visit_expr_struct(self, expression);
                return;
            }
            let field = |name: &str| {
                expression
                    .fields
                    .iter()
                    .find(|field| {
                        matches!(&field.member, syn::Member::Named(member) if member == name)
                    })
                    .map(|field| &field.expr)
            };
            let bool_field = |name: &str, expected: bool| {
                matches!(
                    field(name),
                    Some(Expr::Lit(ExprLit {
                        lit: Lit::Bool(value),
                        ..
                    })) if value.value == expected
                )
            };
            let empty_payloads = match field("variant_payloads") {
                Some(Expr::Array(array)) => array.elems.is_empty(),
                Some(Expr::Call(call)) => {
                    call.args.is_empty()
                        && matches!(&*call.func, Expr::Path(path)
                            if path.path.segments.last().is_some_and(|segment| segment.ident == "new"))
                }
                _ => false,
            };
            if bool_field("is_pub", true)
                && bool_field("is_non_exhaustive", false)
                && empty_payloads
            {
                if let Some((candidate, _)) = &mut self.function {
                    *candidate = true;
                }
            }
            visit::visit_expr_struct(self, expression);
        }

        fn visit_expr_call(&mut self, expression: &'ast ExprCall) {
            if matches!(&*expression.func, Expr::Path(path)
                if path.path.segments.last().is_some_and(|segment| segment.ident == "register_enum"))
            {
                if let Some((_, registered)) = &mut self.function {
                    *registered = true;
                }
            }
            visit::visit_expr_call(self, expression);
        }

        fn visit_expr_method_call(&mut self, expression: &'ast ExprMethodCall) {
            if expression.method == "register_enum" {
                if let Some((_, registered)) = &mut self.function {
                    *registered = true;
                }
            }
            visit::visit_expr_method_call(self, expression);
        }
    }
    let Ok(file) = syn::parse_file(source) else {
        return false;
    };
    let mut visitor = Visitor {
        found: false,
        test_only: false,
        function: None,
    };
    visitor.visit_file(&file);
    visitor.found
}

fn builtin_authority_call_sites(
    module: &str,
    source: &str,
) -> Vec<(String, Option<String>, String, String)> {
    let Some(functions) = ast_functions(module, source) else {
        return Vec::new();
    };
    let mut sites = Vec::new();
    for function in functions.into_iter().filter(|function| !function.test_only) {
        for call in function.calls {
            let AstCall::Path {
                segments, method, ..
            } = call;
            let Some(name) = segments.last() else {
                continue;
            };
            let qualified_authority = !method
                && segments
                    .iter()
                    .rev()
                    .nth(1)
                    .is_some_and(|segment| segment == "BuiltinUniverse");
            let target = if qualified_authority {
                matches!(
                    name.as_str(),
                    "begin" | "finish_core_str" | "register_core_str_with_symbol"
                )
                .then(|| name.clone())
            } else if method && name == "finish_core_str" {
                Some(name.clone())
            } else {
                None
            };
            if let Some(target) = target {
                sites.push((
                    function.module.clone(),
                    function.owner.clone(),
                    function.name.clone(),
                    target,
                ));
            }
        }
    }
    sites
}

fn has_exact_builtin_authority_inventory(sources: &[(&str, &str)]) -> bool {
    let mut actual = sources
        .iter()
        .flat_map(|(module, source)| builtin_authority_call_sites(module, source))
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = vec![
        (
            "sema/body_identity".to_owned(),
            Some("BodyIdentityPool".to_owned()),
            "try_new".to_owned(),
            "begin".to_owned(),
        ),
        (
            "sema/body_identity".to_owned(),
            Some("BodyIdentityPool".to_owned()),
            "try_new".to_owned(),
            "finish_core_str".to_owned(),
        ),
        (
            "semantic_import".to_owned(),
            Some("SemanticImportEpoch".to_owned()),
            "new_in_space".to_owned(),
            "begin".to_owned(),
        ),
        (
            "semantic_import".to_owned(),
            Some("SemanticImportEpoch".to_owned()),
            "new_in_space".to_owned(),
            "finish_core_str".to_owned(),
        ),
        (
            "sema/ordinary_engine".to_owned(),
            Some("OrdinaryBodyEngine".to_owned()),
            "get_or_create_str_struct".to_owned(),
            "register_core_str_with_symbol".to_owned(),
        ),
    ];
    expected.sort();
    actual == expected
}

/// Convert a Buck source path to the semantic Rust module it contributes to.
/// `lib.rs` is the crate root and `mod.rs` contributes to its parent; retaining
/// those filenames as module segments makes `crate::`, `super::`, and
/// cross-file paths resolve to different identities than the compiler sees.
fn normalize_manifest_module(module: &str) -> String {
    let mut segments = module
        .replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(last) = segments.last_mut() {
        if last == "lib.rs" || last == "lib" {
            segments.pop();
        } else if last == "mod.rs" || last == "mod" {
            segments.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_owned();
        }
    }
    segments.join("/")
}

/// Apply the structural authority check to a manifest-wide, syntax-aware call
/// graph. Function identity includes the complete module path, impl owner,
/// name, and occurrence, so same-named free functions and methods remain
/// distinct even when they delegate across source files.
fn has_peer_comptime_evaluator(module: &str, source: &str) -> bool {
    has_peer_comptime_evaluator_in_sources(&[(module, source)])
}

fn has_peer_comptime_evaluator_in_sources(sources: &[(&str, &str)]) -> bool {
    let mut functions = Vec::new();
    for (module, source) in sources {
        let Some(mut parsed) = ast_functions(module, source) else {
            continue;
        };
        functions.append(&mut parsed);
    }
    let mut edges = vec![Vec::new(); functions.len()];
    for (index, function) in functions.iter().enumerate() {
        if function.test_only {
            continue;
        }
        for call in &function.calls {
            let AstCall::Path {
                segments, method, ..
            } = call;
            if segments.is_empty() {
                continue;
            }
            let name = segments.last().expect("nonempty call path");
            let prefix = &segments[..segments.len() - 1];
            // A path can have both interpretations: `crate::helpers::decode`
            // may name a free function in module `helpers`, or an associated
            // method on a root-level type named `helpers`.  Resolve both, but
            // only admit the associated interpretation when that owner exists
            // in the module immediately before it.  Looking for an owner by
            // name across the whole manifest would let an unrelated
            // `other.rs` declaration suppress the real free-function edge.
            let mut target_specs = Vec::<(String, Option<String>)>::new();
            if *method {
                // A method call keeps its caller module and impl owner.  The
                // receiver is retained separately for adapter proof, but it
                // must not become a synthetic module path in the graph.
                target_specs.push((function.module.clone(), function.owner.clone()));
            } else {
                target_specs.push((resolve_call_module(&function.module, prefix), None));
                for (owner_index, owner_segment) in prefix.iter().enumerate() {
                    let owner = if owner_segment == "Self" {
                        function.owner.clone()
                    } else {
                        Some(owner_segment.clone())
                    };
                    let Some(owner) = owner else {
                        continue;
                    };
                    let target_module =
                        resolve_call_module(&function.module, &prefix[..owner_index]);
                    let owner_exists_here = functions.iter().any(|candidate| {
                        candidate.name == *name
                            && candidate.owner.as_deref() == Some(owner.as_str())
                            && module_matches(&candidate.module, &target_module)
                    });
                    if owner_exists_here {
                        target_specs.push((target_module, Some(owner)));
                    }
                }
            }
            for (target, candidate) in functions.iter().enumerate() {
                if candidate.name == *name
                    && target_specs.iter().any(|(target_module, target_owner)| {
                        candidate.owner.as_deref() == target_owner.as_deref()
                            && module_matches(&candidate.module, target_module)
                    })
                {
                    edges[index].push(target);
                }
            }
        }
    }
    let canonical = |function: &AstFunction| {
        function.module == "sema/comptime" && function.owner.as_deref() == Some("ComptimeEngine")
    };
    // These are individually identified lowering adapters. They consume a
    // canonical fact and hand it to the ordinary analyzer; unlike the engine
    // they must not decode or compute values themselves.
    let thin_adapter =
        |function: &AstFunction| is_adapter_identity(function) && assert_thin_adapter(function);
    for start in 0..functions.len() {
        if functions[start].test_only {
            continue;
        }
        let mut pending = vec![start];
        let mut reachable = BTreeSet::new();
        while let Some(current) = pending.pop() {
            if !reachable.insert(current) {
                continue;
            }
            pending.extend(edges[current].iter().copied());
        }
        // An allowlisted adapter is still analyzed before exemption. This
        // catches mutations to the exact production identity even when the
        // injected operation is not itself a selector root.
        if reachable.iter().any(|&index| {
            is_adapter_identity(&functions[index]) && !assert_thin_adapter(&functions[index])
        }) {
            return true;
        }
        // Traits may be split across a dispatcher, a decoder, an operator
        // helper, and a return-value adapter.  The authority rule is about the
        // reachable computation, not about any single function or a recursion
        // cycle, so union each trait over the complete reachable component.
        let noncanonical =
            |index: usize| !canonical(&functions[index]) && !thin_adapter(&functions[index]);
        let has_dispatch = reachable
            .iter()
            .any(|&index| noncanonical(index) && functions[index].dispatch);
        let has_selection = reachable
            .iter()
            .any(|&index| noncanonical(index) && functions[index].selection);
        let has_decode = reachable
            .iter()
            .any(|&index| noncanonical(index) && functions[index].decode);
        let has_operation = reachable
            .iter()
            .any(|&index| noncanonical(index) && functions[index].operation);
        let has_value_result = reachable
            .iter()
            .any(|&index| noncanonical(index) && functions[index].value_result);
        if has_dispatch && (has_selection || (has_decode && has_operation && has_value_result)) {
            return true;
        }
    }
    false
}

/// The ordinary analyzer may consume canonical facts, but it is not an
/// evaluator. Keep the exception narrow and structural: an adapter cannot
/// dispatch comptime `InstData`, decode a value, perform an operator, or
/// construct a selection result in its own body. Any such addition must move
/// to the canonical engine (and consequently makes this guard fail).
fn assert_thin_adapter(function: &AstFunction) -> bool {
    if !is_adapter_identity(function) {
        return false;
    }
    // The allowlist is intentionally structural, not a naming exemption.
    // These wrappers may prepare an environment and map the engine's fact,
    // but may not inspect InstData, decode a value, perform an operation, or
    // delegate to another evaluator.  The canonical selection call is the
    // only evaluator-shaped operation permitted in the body.
    let only_canonical_calls =
        function.canonical_selection_call && function.calls.iter().all(adapter_call_is_allowed);
    only_canonical_calls
        && !function.dispatch
        && !function.decode
        && !function.operation
        && !function.selection
        && !function.direct_selection
        && !function.value_result
        && !function.child_traversal
}

/// Positive call authority for the two real semantic selector adapters.  The
/// adapter may prepare the canonical environment, read its RIR/type facts,
/// invoke the engine, and map the resulting selection.  No other call is
/// allowed: in particular, calling a canonical method does not immunize a
/// peer helper that also computes or traverses values.
fn adapter_call_is_allowed(call: &AstCall) -> bool {
    let AstCall::Path {
        segments,
        method,
        receiver,
    } = call;
    let Some(name) = segments.last().map(String::as_str) else {
        return false;
    };
    if *method {
        let Some(receiver) = receiver.as_deref() else {
            return false;
        };
        return match (receiver, name) {
            ([receiver], method)
                if receiver == "self"
                    && matches!(
                        method,
                        "active_anonymous_producer" | "body_rir_ref" | "trap_failure"
                    ) =>
            {
                true
            }
            (receiver, "unwrap_or")
                if receiver.len() == 1
                    && matches!(receiver[0].as_str(), "type_subst" | "value_subst") =>
            {
                true
            }
            (receiver, "get")
                if receiver.len() == 2
                    && receiver[0] == "self"
                    && receiver[1] == "body_rir_ref" =>
            {
                true
            }
            (receiver, "cloned")
                if receiver.len() == 2
                    && receiver[0] == "self"
                    && receiver[1] == "active_anonymous_producer" =>
            {
                true
            }
            (receiver, "select_branch" | "select_match")
                if receiver.len() == 2
                    && receiver[0] == "ComptimeEngine"
                    && receiver[1] == "new" =>
            {
                true
            }
            _ => false,
        };
    }
    (segments.len() == 2
        && ((segments[0] == "AHashMap" && segments[1] == "new")
            || (segments[0] == "ComptimeEnv" && segments[1] == "with_subst")
            || (segments[0] == "ComptimeEngine" && segments[1] == "new")))
        || (segments.len() == 1 && matches!(name, "Ok" | "Err" | "Some"))
}

/// Stable identities are intentionally the full semantic location. A peer
/// with the same function name in another module or impl never inherits this
/// exemption.
fn is_adapter_identity(function: &AstFunction) -> bool {
    matches!(
        (
            function.module.as_str(),
            function.owner.as_deref(),
            function.name.as_str()
        ),
        (
            "sema/comptime_eval",
            Some("OrdinaryBodyEngine"),
            "select_comptime_branch_with_resolved_types_and_membership"
        ) | (
            "sema/comptime_eval",
            Some("OrdinaryBodyEngine"),
            "select_comptime_match_with_resolved_types_and_membership"
        )
    )
}

fn resolve_call_module(current: &str, prefix: &[String]) -> String {
    let mut module = current.split('/').map(str::to_owned).collect::<Vec<_>>();
    let mut index = 0;
    if prefix.first().is_some_and(|segment| segment == "crate") {
        module.clear();
        index = 1;
    }
    while index < prefix.len() {
        match prefix[index].as_str() {
            "self" => {}
            "super" => {
                module.pop();
            }
            segment => module.push(segment.to_owned()),
        }
        index += 1;
    }
    module.join("/")
}

fn module_matches(candidate: &str, requested: &str) -> bool {
    candidate == requested
        || (requested.is_empty() && candidate.is_empty())
        || candidate.ends_with(&format!("/{requested}"))
}

#[test]
fn peer_one_body_authority_cannot_return() {
    let sources = [
        include_str!("lib.rs"),
        include_str!("sema/mod.rs"),
        include_str!("sema/binding_manifest.rs"),
    ]
    .concat();
    assert!(!sources.contains("mod one_body;"));
    assert!(!sources.contains("OneBodyTransactionOutcome"));
    assert!(!sources.contains("analyze_one_body"));
}

#[test]
fn integer_consumers_use_one_representation_independent_kernel() {
    let semantics = include_str!("integer_semantics.rs");
    let types = include_str!("types.rs");
    let comptime = crate::sema::COMPTIME_SOURCE;

    assert!(types.contains("pub fn integer_semantics(&self) -> Option<IntegerType>"));
    assert!(comptime.contains("integer.shift_i128"));
    assert!(comptime.contains("type_integer_semantics"));
    assert!(!comptime.contains("fn truncate_to_type("));
    assert!(semantics.contains("pub struct IntegerType"));
    assert!(semantics.contains("pub fn checked_div_i128"));
    assert!(semantics.contains("pub fn checked_rem_i128"));
    assert!(semantics.contains("pub struct CheckedIntegerResult"));
    assert!(semantics.contains("pub fn checked_add_report_i128"));
    assert!(semantics.contains("pub fn checked_neg_literal_i128"));
    assert!(comptime.contains("checked_neg_literal_report_i128"));
}

#[test]
fn comptime_instdata_evaluation_has_one_production_authority() {
    let inference = include_str!("inference/generate.rs");
    let type_inference = include_str!("sema/analysis/type_inference.rs");
    let control_flow = include_str!("sema/control_flow.rs");
    let comptime_adapter = include_str!("sema/comptime_eval.rs");
    let canonical = crate::sema::COMPTIME_SOURCE;
    for (name, source) in [
        ("inference", inference),
        ("type inference", type_inference),
        ("control flow", control_flow),
    ] {
        for retired in [
            "fn eval_comptime_value(",
            "fn extract_int_argument(",
            "fn comptime_selected_arm(",
            "fn eval_int_binop(",
            "fn eval_int_cmp(",
            "fn eval_bool_binop(",
            "fn eval_eq(",
        ] {
            assert!(
                !source.contains(retired),
                "retired evaluator escaped {name}: {retired}"
            );
        }
    }
    assert!(!control_flow.contains("try_evaluate_const_in_fn"));
    assert!(!control_flow.contains("ComptimeEngine::new"));
    assert!(comptime_adapter.contains("ComptimeEngine::new(self).select_branch"));
    assert!(comptime_adapter.contains("ComptimeEngine::new(self).select_match"));
    assert!(control_flow.contains("comptime_selections"));
    assert!(canonical.contains("pub enum ComptimeSelection"));
    assert!(canonical.contains("pub fn select_branch("));
    assert!(canonical.contains("pub fn select_match("));

    // Keep this guard semantic rather than name-based. The inventory is the
    // complete Buck-generated source inventory for this crate (including test
    // modules); a future helper must not evade the guard by renaming
    // itself. The AST visitor below tracks nested modules and lexical scopes,
    // so nested matches/closures cannot hide a peer evaluator.
    let production = [
        ("api_inventory", include_str!("api_inventory.rs")),
        ("builtin_universe", include_str!("builtin_universe.rs")),
        ("call_abi", include_str!("call_abi.rs")),
        (
            "declaration_validation",
            include_str!("declaration_validation.rs"),
        ),
        ("drop_glue", include_str!("drop_glue.rs")),
        ("drop_glue_names", include_str!("drop_glue_names.rs")),
        ("exact_decimal", include_str!("exact_decimal.rs")),
        ("ffi_predicates", include_str!("ffi_predicates.rs")),
        (
            "inference/constraint",
            include_str!("inference/constraint.rs"),
        ),
        ("inference/generate", include_str!("inference/generate.rs")),
        ("inference/mod", include_str!("inference/mod.rs")),
        ("inference/types", include_str!("inference/types.rs")),
        ("inference/unify", include_str!("inference/unify.rs")),
        ("inst", include_str!("inst.rs")),
        (
            "inst/payload_support",
            include_str!("inst/payload_support.rs"),
        ),
        ("integer_semantics", include_str!("integer_semantics.rs")),
        ("intern_pool", include_str!("intern_pool.rs")),
        ("intrinsic", include_str!("intrinsic.rs")),
        ("layout", include_str!("layout.rs")),
        ("lib", include_str!("lib.rs")),
        ("module_registry", include_str!("module_registry.rs")),
        ("param_arena", include_str!("param_arena.rs")),
        ("path_norm", include_str!("path_norm.rs")),
        ("runtime_call", include_str!("runtime_call.rs")),
        ("scope", include_str!("scope.rs")),
        (
            "sema/aggregate_resolution",
            include_str!("sema/aggregate_resolution.rs"),
        ),
        ("sema/aggregates", include_str!("sema/aggregates.rs")),
        ("sema/analysis", include_str!("sema/analysis.rs")),
        (
            "sema/analysis/builtin_ops",
            include_str!("sema/analysis/builtin_ops.rs"),
        ),
        (
            "sema/analysis/calls",
            include_str!("sema/analysis/calls.rs"),
        ),
        (
            "sema/analysis/instructions",
            include_str!("sema/analysis/instructions.rs"),
        ),
        (
            "sema/analysis/intrinsics",
            include_str!("sema/analysis/intrinsics.rs"),
        ),
        (
            "sema/analysis/ownership",
            include_str!("sema/analysis/ownership.rs"),
        ),
        (
            "sema/analysis/pointers",
            include_str!("sema/analysis/pointers.rs"),
        ),
        (
            "sema/analysis/type_inference",
            include_str!("sema/analysis/type_inference.rs"),
        ),
        ("sema/analyze_ops", include_str!("sema/analyze_ops.rs")),
        ("sema/anon_structs", include_str!("sema/anon_structs.rs")),
        (
            "sema/binding_manifest",
            include_str!("sema/binding_manifest.rs"),
        ),
        ("sema/body_endpoint", include_str!("sema/body_endpoint.rs")),
        ("sema/body_identity", include_str!("sema/body_identity.rs")),
        (
            "sema/call_resolution",
            include_str!("sema/call_resolution.rs"),
        ),
        ("sema/comptime", include_str!("sema/comptime.rs")),
        (
            "sema/comptime/frames",
            include_str!("sema/comptime/frames.rs"),
        ),
        (
            "sema/comptime/intrinsics",
            include_str!("sema/comptime/intrinsics.rs"),
        ),
        (
            "sema/comptime/model",
            include_str!("sema/comptime/model.rs"),
        ),
        (
            "sema/comptime/registry",
            include_str!("sema/comptime/registry.rs"),
        ),
        (
            "sema/comptime/sites",
            include_str!("sema/comptime/sites.rs"),
        ),
        (
            "sema/comptime/structured_type",
            include_str!("sema/comptime/structured_type.rs"),
        ),
        (
            "sema/comptime/value_domain_tests",
            include_str!("sema/comptime/value_domain_tests.rs"),
        ),
        ("sema/comptime_eval", include_str!("sema/comptime_eval.rs")),
        (
            "sema/consistency_tests",
            include_str!("sema/consistency_tests.rs"),
        ),
        ("sema/context", include_str!("sema/context.rs")),
        ("sema/control_flow", include_str!("sema/control_flow.rs")),
        (
            "sema/declaration_index",
            include_str!("sema/declaration_index.rs"),
        ),
        ("sema/declarations", include_str!("sema/declarations.rs")),
        ("sema/fact_mode", include_str!("sema/fact_mode.rs")),
        ("sema/inference_ctx", include_str!("sema/inference_ctx.rs")),
        ("sema/info", include_str!("sema/info.rs")),
        ("sema/known_symbols", include_str!("sema/known_symbols.rs")),
        ("sema/mod", include_str!("sema/mod.rs")),
        (
            "sema/ordinary_engine",
            include_str!("sema/ordinary_engine.rs"),
        ),
        ("sema/output", include_str!("sema/output.rs")),
        (
            "sema/ownership_state",
            include_str!("sema/ownership_state.rs"),
        ),
        ("sema/provider", include_str!("sema/provider.rs")),
        (
            "sema/provider_accessor_tests",
            include_str!("sema/provider_accessor_tests.rs"),
        ),
        (
            "sema/provider_body_host",
            include_str!("sema/provider_body_host.rs"),
        ),
        (
            "sema/provider_fixture",
            include_str!("sema/provider_fixture.rs"),
        ),
        (
            "sema/provider_fixture_tests",
            include_str!("sema/provider_fixture_tests.rs"),
        ),
        (
            "sema/provider_module_registry",
            include_str!("sema/provider_module_registry.rs"),
        ),
        (
            "sema/provider_semantics_tests",
            include_str!("sema/provider_semantics_tests.rs"),
        ),
        (
            "sema/provider_strings_ownership_tests",
            include_str!("sema/provider_strings_ownership_tests.rs"),
        ),
        (
            "sema/semantic_body_export",
            include_str!("sema/semantic_body_export.rs"),
        ),
        ("sema/tests", include_str!("sema/tests.rs")),
        ("sema/typeck", include_str!("sema/typeck.rs")),
        ("sema/visibility", include_str!("sema/visibility.rs")),
        ("semantic_body", include_str!("semantic_body.rs")),
        ("semantic_identity", include_str!("semantic_identity.rs")),
        ("semantic_import", include_str!("semantic_import.rs")),
        (
            "semantic_type_resolution",
            include_str!("semantic_type_resolution.rs"),
        ),
        ("specialize", include_str!("specialize.rs")),
        ("stable_digest", include_str!("stable_digest.rs")),
        ("type_encoding", include_str!("type_encoding.rs")),
        ("type_properties", include_str!("type_properties.rs")),
        ("types", include_str!("types.rs")),
    ];
    // Keep the guard's source set tied to the same canonical Buck glob used by
    // the crate. The mapped manifest is generated from that glob, so adding a
    // production module necessarily enters this exact predicate.
    let actual = include_str!("rue_air_source_manifest.txt")
        .lines()
        .map(|path| path.trim_start_matches("./").trim_end_matches(".rs"))
        .map(|path| path.trim_start_matches("src/"))
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let expected = production
        .iter()
        .map(|(module, _)| module.to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual, expected,
        "source inventory must match Buck's src glob"
    );
    assert!(
        !has_peer_comptime_evaluator_in_sources(&production),
        "transitive peer comptime evaluator/selector in the manifest"
    );

    // Builtin nominal construction has one authority.  Scan the complete
    // Buck inventory by semantic shape so a renamed `str` definition cannot
    // evade the guard, while generated `Str(N)` and slice fat pointers remain
    // legitimate consumers of the same shape.
    for (module, source) in &production {
        if *module != "builtin_universe" {
            assert!(
                !has_core_str_definition(module, source),
                "core str StructDef construction escaped builtin_universe: {module}"
            );
            assert!(
                !has_builtin_enum_bootstrap(source),
                "builtin enum bootstrap escaped builtin_universe: {module}"
            );
            assert!(
                !has_builtin_enum_membership(source),
                "builtin enum membership escaped builtin_universe: {module}"
            );
        }
    }
    assert!(
        has_exact_builtin_authority_inventory(&production),
        "builtin universe authority calls must have exactly one owner-qualified site per phase"
    );
    assert!(has_core_str_definition(
        "fixture",
        "let value = StructDef { name: \"StringView\", fields: [StructField { name: \"ptr\", ty: Type::U8 }, StructField { name: \"len\", ty: Type::U64 }], is_builtin: true };"
    ));
    assert!(has_core_str_definition(
        "fixture",
        "let value = StructDef { name: \"Str(32)\", fields: [StructField { name: \"ptr\", ty: Type::U8 }, StructField { name: \"len\", ty: Type::U64 }], is_builtin: true };"
    ));
    assert!(has_core_str_definition(
        "fixture",
        "let value = StructDef { name: \"[i32]\", fields: [StructField { name: \"ptr\", ty: Type::U8 }, StructField { name: \"len\", ty: Type::U64 }], is_builtin: true };"
    ));
    assert!(has_core_str_definition(
        "fixture",
        "fn fixture() { let hidden = helper(); let value = StructDef { name: hidden, fields: vec![StructField { name: \"ptr\", ty: Type::U8 }, StructField { name: \"len\", ty: Type::U64 }], is_builtin: true }; }"
    ));
    assert!(has_core_str_definition(
        "fixture",
        "fn fixture() { let hidden = helper(); let value = crate::types::StructDef { name: hidden, fields: vec![crate::types::StructField { name: \"ptr\", ty: Type::U8 }, crate::types::StructField { name: \"len\", ty: Type::U64 }], is_builtin: true }; }"
    ));
    assert!(has_core_str_definition(
        "fixture",
        "fn fixture() { let value = crate::types::StructDef { name: \"Str(32)\", fields: vec![crate::types::StructField { name: \"ptr\", ty: Type::U8 }, crate::types::StructField { name: \"len\", ty: Type::U64 }], is_builtin: true }; }"
    ));
    assert!(has_builtin_enum_bootstrap(
        "fn fixture() { let value = EnumDef { name: \"RenamedArch\", variants: vec![\"X\"], variant_payloads: Vec::new(), is_pub: true, is_non_exhaustive: false, file_id: FileId::DEFAULT }; type_pool.register_enum(symbol, value); }"
    ));
    assert!(has_builtin_enum_bootstrap(
        "fn fixture() { let value = EnumDef { name: \"RenamedArch\", variants: [\"X\"], variant_payloads: [], is_pub: true, is_non_exhaustive: false, file_id: FileId::DEFAULT }; type_pool.register_enum(symbol, value); }"
    ));
    assert!(has_builtin_enum_bootstrap(
        "fn fixture() { let value = crate::EnumDef { name: \"RenamedArch\", variants: vec![\"X\"], variant_payloads: Vec::new(), is_pub: true, is_non_exhaustive: false, file_id: FileId::DEFAULT }; type_pool.register_enum(symbol, value); }"
    ));
    assert!(!has_builtin_enum_bootstrap(
        "fn fixture() { let value = EnumDef { name: \"PublicSourceEnum\", variants: vec![\"X\"], variant_payloads: Vec::new(), is_pub: true, is_non_exhaustive: false, file_id: FileId::DEFAULT }; }"
    ));
    assert!(!has_builtin_enum_bootstrap(
        "fn candidate() { let value = EnumDef { name: \"PublicSourceEnum\", variants: vec![\"X\"], variant_payloads: Vec::new(), is_pub: true, is_non_exhaustive: false, file_id: FileId::DEFAULT }; } fn unrelated() { type_pool.register_enum(symbol, value); }"
    ));
    assert!(has_builtin_enum_membership(
        "fn fixture() { alias::get_builtin_enum(name); let _ = alias::BUILTIN_ENUMS; alias::is_reserved_enum_name(name); }"
    ));
    assert!(!has_exact_builtin_authority_inventory(&[(
        "sema/body_identity",
        "impl BodyIdentityPool { fn try_new() { BuiltinUniverse::begin(pool, space); BuiltinUniverse::begin(pool, space); let mut universe = BuiltinUniverse::begin(pool, space); universe.finish_core_str(pool, space); } }"
    ),]));
    assert!(!has_exact_builtin_authority_inventory(&[
        (
            "sema/body_identity",
            "impl BodyIdentityPool { fn try_new() { let mut universe = BuiltinUniverse::begin(pool, space); universe.finish_core_str(pool, space); } }"
        ),
        (
            "semantic_import",
            "impl SemanticImportEpoch { fn new_in_space() { let mut universe = BuiltinUniverse::begin(pool, space); universe.finish_core_str(pool, space); universe.finish_core_str(pool, space); } }"
        ),
        (
            "sema/ordinary_engine",
            "impl OrdinaryBodyEngine { fn get_or_create_str_struct() { BuiltinUniverse::register_core_str_with_symbol(pool, symbols, symbol); } }"
        ),
    ]));
    let qualified_exact = [
        (
            "sema/body_identity",
            "impl BodyIdentityPool { fn try_new() { let mut universe = crate::builtin_universe::BuiltinUniverse::begin(pool, space); universe.finish_core_str(pool, space); } }",
        ),
        (
            "semantic_import",
            "impl SemanticImportEpoch { fn new_in_space() { let mut universe = crate::builtin_universe::BuiltinUniverse::begin(pool, space); universe.finish_core_str(pool, space); } }",
        ),
        (
            "sema/ordinary_engine",
            "impl OrdinaryBodyEngine { fn get_or_create_str_struct() { crate::builtin_universe::BuiltinUniverse::register_core_str_with_symbol(pool, symbols, symbol); } }",
        ),
    ];
    assert!(has_exact_builtin_authority_inventory(&qualified_exact));
    let qualified_exact = [
        (
            "sema/body_identity",
            "impl BodyIdentityPool { fn try_new() { crate::builtin_universe::BuiltinUniverse::begin(pool, space); let mut universe = crate::builtin_universe::BuiltinUniverse::begin(pool, space); universe.finish_core_str(pool, space); } }",
        ),
        (
            "semantic_import",
            "impl SemanticImportEpoch { fn new_in_space() { crate::builtin_universe::BuiltinUniverse::begin(pool, space); let mut universe = crate::builtin_universe::BuiltinUniverse::begin(pool, space); universe.finish_core_str(pool, space); } }",
        ),
        (
            "sema/ordinary_engine",
            "impl OrdinaryBodyEngine { fn get_or_create_str_struct() { crate::builtin_universe::BuiltinUniverse::register_core_str_with_symbol(pool, symbols, symbol); } }",
        ),
    ];
    assert!(!has_exact_builtin_authority_inventory(&qualified_exact));

    // A renamed peer must fail the same semantic guard; this protects the
    // invariant independently of the retired helper names above.
    let renamed_peer = "fn fold(&mut self, inst: InstRef) -> (ConstValue, ComptimeSelection) { let value = ConstValue::Integer(1); let op = ComptimeIntegerOperation::Add; match &inst.data { InstData::IntConst(_) => self.fold(inst), _ => self.fold(inst) } }";
    assert!(has_peer_comptime_evaluator("fixture", renamed_peer));
    assert!(has_peer_comptime_evaluator(
        "sema/comptime",
        "fn fold(&mut self, inst: InstRef) -> ComptimeOutcome { let value = ConstValue::Integer(1); match &inst.data { InstData::IntConst(_) => self.fold(inst), _ => value == ConstValue::Integer(1) } }"
    ));
    for evil in ["EvilComptimeEngine", "ComptimeEnginePeer"] {
        let fixture = format!(
            "impl {evil} {{ fn fold(&mut self, inst: InstRef) -> ComptimeOutcome {{ match inst.data {{ InstData::IntConst(_) => self.fold(inst), _ => ConstValue::Bool(true) && true }} }} }}"
        );
        assert!(
            has_peer_comptime_evaluator("sema/comptime", &fixture),
            "near-miss canonical owners must not be exempted"
        );
    }
    for fixture in [
        "fn fold(&mut self, inst: InstRef) -> ComptimeOutcome { let value = ConstValue::Bool(true); if let InstData::Branch { .. } = inst.data { return self.fold(inst); } value && true }",
        "fn decode(&mut self, inst: InstRef) -> ComptimeOutcome { let value = ConstValue::Integer(1); if matches!(inst.data, InstData::IntConst(_)) { self.decode(inst); } value == ConstValue::Integer(1) }",
        "fn choose(&mut self, inst: InstRef) -> ComptimeOutcome { let value = ConstValue::Bool(true); match inst.data { InstData::Branch { .. } => self.choose(inst), _ => value && true } }",
    ] {
        assert!(
            has_peer_comptime_evaluator("fixture", fixture),
            "renamed/spelling variants must use the production predicate"
        );
    }
    let split_and_mutual = "fn dispatch(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::Branch { .. } => op(inst), _ => op(inst) } } fn op(inst: InstRef) -> ComptimeOutcome { let value = ConstValue::Integer(1); dispatch(inst); value == ConstValue::Integer(1) }";
    assert!(
        has_peer_comptime_evaluator("fixture", split_and_mutual),
        "split and mutually recursive evaluator helpers must be rejected"
    );
    let same_named_impl_and_free = "impl Decoder { fn fold(&mut self, inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => self.fold(inst), _ => free_fold(inst) } } } fn free_fold(inst: InstRef) -> ComptimeOutcome { let value = ConstValue::Bool(true); value && matches!(inst.data, InstData::IntConst(_)) }";
    assert!(
        has_peer_comptime_evaluator("other/module", same_named_impl_and_free),
        "same-named impl/free helpers must retain distinct call identities"
    );
}

#[test]
fn structural_guard_rejects_cross_file_dispatch_decode_and_operator() {
    let sources = [
        (
            "lib.rs",
            "fn dispatch(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => crate::helpers::fold(inst), _ => crate::helpers::fold(inst) } }",
        ),
        (
            "helpers.rs",
            "fn fold(inst: InstRef) -> Option<ConstValue> { let value = ConstValue::Integer(1); let _ = inst; value + ConstValue::Integer(1) }",
        ),
    ];
    assert!(has_peer_comptime_evaluator_in_sources(&sources));
}

#[test]
fn structural_guard_unions_split_traits_without_recursion() {
    let sources = [
        (
            "lib.rs",
            "fn dispatch(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => crate::decode::read(inst), _ => crate::decode::read(inst) } }",
        ),
        (
            "decode.rs",
            "fn read(inst: InstRef) -> Option<ConstValue> { let value = ConstValue::Integer(1); crate::operators::add(value, inst) }",
        ),
        (
            "operators.rs",
            "fn add(value: ConstValue, inst: InstRef) -> Option<ConstValue> { let _ = inst; value + ConstValue::Integer(1) }",
        ),
    ];
    assert!(has_peer_comptime_evaluator_in_sources(&sources));
}

#[test]
fn structural_guard_normalizes_lib_and_mod_modules() {
    let sources = [
        (
            "lib.rs",
            "fn dispatch(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => crate::helpers::fold(inst), _ => crate::helpers::fold(inst) } }",
        ),
        (
            "helpers/mod.rs",
            "fn fold(inst: InstRef) -> Option<ConstValue> { let value = ConstValue::Integer(1); let _ = inst; value + ConstValue::Integer(1) }",
        ),
    ];
    assert!(has_peer_comptime_evaluator_in_sources(&sources));
}

#[test]
fn structural_guard_resolves_self_and_type_name_calls() {
    let self_call = "impl Peer { fn dispatch(&mut self, inst: InstRef) -> bool { match inst.data { InstData::IntConst(_) => Self::decode(self, inst).is_some(), _ => Self::decode(self, inst).is_some() } } fn decode(&mut self, inst: InstRef) -> Option<ConstValue> { let value = ConstValue::Integer(1); let _ = inst; Some(value + ConstValue::Integer(1)) } }";
    assert!(has_peer_comptime_evaluator("sema/comptime", self_call));

    let type_name_call = "impl Peer { fn dispatch(&mut self, inst: InstRef) -> bool { match inst.data { InstData::IntConst(_) => Peer::decode(self, inst).is_some(), _ => Peer::decode(self, inst).is_some() } } fn decode(&mut self, inst: InstRef) -> Option<ConstValue> { let value = ConstValue::Integer(1); let _ = inst; Some(value + ConstValue::Integer(1)) } }";
    assert!(has_peer_comptime_evaluator("sema/comptime", type_name_call));
}

#[test]
fn staged_selector_collection_uses_one_bounded_walk() {
    let source = include_str!("sema/analysis/type_inference.rs");
    let collector = source
        .split("    fn collect_comptime_facts(")
        .nth(1)
        .and_then(|source| {
            source
                .split("    fn collect_generic_argument_facts(")
                .next()
        })
        .expect("staged selector collector");
    assert!(collector.contains("enum FactTask"));
    assert!(collector.contains("let mut visited = ahash::AHashSet::new()"));
    assert!(collector.contains("FactTask::SelectBranch"));
    assert!(collector.contains("FactTask::SelectMatch"));
    assert!(collector.contains("frontier.push_back(ComptimeInferenceFrontier"));
    assert!(collector.contains("bindings: FrontierScope"));
    assert!(!collector.contains("Vec<FrontierBinding>"));
    assert!(
        !collector.contains("inst_ref: selected,\n                                bindings"),
        "a selected body must be a frontier checkpoint, not recursive collector work"
    );
    let driver = source
        .split("    pub(crate) fn run_type_inference(")
        .nth(1)
        .and_then(|source| source.split("    fn has_comptime_fact_sites(").next())
        .expect("inference staging driver");
    assert!(driver.contains("if return_type != Type::COMPTIME_TYPE"));
    assert!(driver.contains("loop {"));
    assert!(driver.contains("let Some(front) = frontier.pop_front()"));
    assert!(driver.contains("generated_frontier"));
    assert!(source.contains("FrontierParamOverlay"));
    assert!(driver.contains("staged_frontier_instructions"));
    assert!(driver.contains("staged_fact_nodes"));
    assert!(driver.contains("staged_canonical_evaluations"));
    assert!(source.contains("staged_constraints_generated"));
    assert!(source.contains("staged_binding_scope_nodes"));
    assert!(source.contains("staged_binding_materializations"));
    assert!(source.contains("staged_probe_nodes"));
    assert!(!source.contains(concat!("frontier_", "params")));
    assert!(!source.contains(concat!("materialize_", "scope")));
    assert!(!source.contains(concat!("scope_", "names")));
    assert!(
        !collector.contains("body_rir_ref().iter()"),
        "selector facts must not rescan the entire body for every selector"
    );
}

#[test]
fn structural_guard_rejects_same_module_peer_owner() {
    let fixture = "impl ComptimeEnginePeer { fn fold(&mut self, inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => ConstValue::Bool(true) && false, _ => ConstValue::Bool(false) } } }";
    assert!(has_peer_comptime_evaluator("sema/comptime", fixture));
}

#[test]
fn structural_guard_rejects_nonrecursive_bool_selector() {
    let fixture = "fn choose(inst: InstRef) -> ComptimeSelection { if let InstData::Branch { .. } = inst.data { ComptimeSelection::Branch { taken: true } } else { ComptimeSelection::Branch { taken: false } } }";
    assert!(has_peer_comptime_evaluator("sema/other", fixture));
}

#[test]
fn structural_guard_rejects_split_direct_operator_helpers() {
    let fixture = "fn fold(inst: InstRef) -> ComptimeOutcome { dispatch(inst) } fn dispatch(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => add(inst), _ => add(inst) } } fn add(_inst: InstRef) -> ComptimeOutcome { let value = ConstValue::Integer(1); let _ = value + value; ConstValue::Bool(true) && true }";
    assert!(has_peer_comptime_evaluator("sema/other", fixture));
}

#[test]
fn structural_guard_rejects_mutual_and_free_recursion() {
    let fixture = "fn left(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => right(inst), _ => right(inst) } } fn right(inst: InstRef) -> ComptimeOutcome { left(inst); let value = ConstValue::Bool(true); value && true }";
    assert!(has_peer_comptime_evaluator("sema/other", fixture));
}

#[test]
fn structural_guard_distinguishes_same_named_self_and_free_calls() {
    let fixture = "impl Decoder { fn fold(&mut self, inst: InstRef) -> ComptimeOutcome { self.fold(inst) } } fn fold(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => ConstValue::Bool(true) && true, _ => ConstValue::Bool(false) } }";
    assert!(has_peer_comptime_evaluator("sema/other", fixture));
}

#[test]
fn structural_guard_tracks_nested_module_placement() {
    let fixture = "mod nested { fn fold(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => ConstValue::Bool(true) && true, _ => ConstValue::Bool(false) } } }";
    assert!(has_peer_comptime_evaluator("sema/nested", fixture));
}

#[test]
fn structural_guard_tracks_qualified_cross_source_edges() {
    let caller = "fn fold(inst: InstRef) -> ComptimeOutcome { super::decode(inst) }";
    let callee = "fn decode(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => ConstValue::Integer(1) + ConstValue::Integer(2), _ => ConstValue::Integer(0) } }";
    assert!(has_peer_comptime_evaluator_in_sources(&[
        ("sema/analysis/calls", caller),
        ("sema/analysis", callee),
    ]));

    let crate_caller = "fn fold(inst: InstRef) -> ComptimeOutcome { crate::shared::decode(inst) }";
    let crate_callee = "fn decode(inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::IntConst(_) => ConstValue::Integer(1) + ConstValue::Integer(2), _ => ConstValue::Integer(0) } }";
    assert!(has_peer_comptime_evaluator_in_sources(&[
        ("sema/analysis", crate_caller),
        ("shared", crate_callee),
    ]));

    // The dispatcher and value-producing associated method are deliberately
    // split across files.  Only resolving the owner-qualified path through
    // `helpers::Peer` exposes the complete dispatch/decode/operation graph.
    let associated_caller = "fn dispatch(inst: InstRef) -> bool { match inst.data { InstData::IntConst(_) => crate::helpers::Peer::decode(inst).is_some(), _ => crate::helpers::Peer::decode(inst).is_some() } }";
    let associated_callee = "impl Peer { fn decode(inst: InstRef) -> Option<ConstValue> { Some(ConstValue::Integer(1) + ConstValue::Integer(2)) } }";
    assert!(has_peer_comptime_evaluator_in_sources(&[
        ("lib.rs", associated_caller),
        ("helpers.rs", associated_callee),
    ]));

    let super_associated_caller = "fn dispatch(inst: InstRef) -> bool { match inst.data { InstData::IntConst(_) => super::helpers::Peer::decode(inst).is_some(), _ => super::helpers::Peer::decode(inst).is_some() } }";
    assert!(has_peer_comptime_evaluator_in_sources(&[
        ("sema/calls/mod.rs", super_associated_caller),
        ("sema/helpers.rs", associated_callee),
    ]));

    // A module/free-function path must remain reachable even when an
    // unrelated source declares a same-named type.  The old global owner-name
    // lookup incorrectly reinterpreted `crate::helpers::decode` as an
    // associated call on that unrelated type and dropped the free edge.
    let collision_dispatcher = "fn dispatch(inst: InstRef) -> bool { match inst.data { InstData::IntConst(_) => crate::helpers::decode(inst).is_some(), _ => crate::helpers::decode(inst).is_some() } }";
    let collision_decoder = "fn decode(inst: InstRef) -> Option<ConstValue> { Some(ConstValue::Integer(1) + ConstValue::Integer(2)) }";
    let unrelated_owner = "struct helpers; impl helpers { fn unrelated(&self) {} }";
    assert!(has_peer_comptime_evaluator_in_sources(&[
        ("lib.rs", collision_dispatcher),
        ("helpers.rs", collision_decoder),
        ("other.rs", unrelated_owner),
    ]));
}

#[test]
fn structural_guard_rejects_nonrecursive_value_operator_split() {
    let caller = "fn fold(inst: InstRef) -> Option<ConstValue> { decode(inst) }";
    let callee = "fn decode(inst: InstRef) -> Option<ConstValue> { match inst.data { InstData::IntConst(_) => Some(ConstValue::Integer(1) + ConstValue::Integer(2)), _ => None } }";
    assert!(has_peer_comptime_evaluator_in_sources(&[
        ("sema/analysis", caller),
        ("sema/analysis", callee),
    ]));
}

#[test]
fn structural_guard_enforces_thin_adapter_mutations() {
    let allowed = ast_functions("sema/comptime_eval", include_str!("sema/comptime_eval.rs"))
        .expect("canonical adapter source");
    for name in [
        "select_comptime_branch_with_resolved_types_and_membership",
        "select_comptime_match_with_resolved_types_and_membership",
    ] {
        let adapter = allowed
            .iter()
            .find(|function| function.name == name)
            .expect("allowlisted adapter identity");
        assert!(
            assert_thin_adapter(adapter),
            "canonical adapter must satisfy its AST proof: {name}"
        );
    }

    let mutations = [
        // InstData dispatch must never be hidden behind an allowlisted name.
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { match inst.data { InstData::Branch { .. } => self.select_branch(inst), _ => self.select_branch(inst) } } }",
        // Value decoding is an engine responsibility.
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { let _ = inst.as_integer(); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        // Arithmetic on a decoded/selector value is forbidden in adapters.
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { let _ = inst + inst; ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        // Recursive traversal/delegation cannot be smuggled through the name.
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { self.evaluate_const(inst); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        // Positive call whitelisting rejects an otherwise unfamiliar peer
        // helper even when it is adjacent to the canonical selection call.
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { self.compute(inst); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        // Both a renamed recursive helper and a child walk are forbidden,
        // even when no value operation appears in the adapter itself.
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { self.fold(inst); inst.children(); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        // The match adapter has the same proof; its identity is not a second
        // authority and receives the same mutation coverage.
        "impl OrdinaryBodyEngine { fn select_comptime_match_with_resolved_types_and_membership(&mut self, inst: InstRef) { match inst.data { InstData::Match { .. } => self.select_match(inst), _ => self.select_match(inst) } } }",
        // Receiver-sensitive method authorization: a matching method name on
        // `self` (rather than the specific value returned by the wrapper) is
        // not a permitted adapter operation.
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { self.get(inst); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { self.unwrap_or(inst); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { self.cloned(inst); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { self.select_branch(inst); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { self.select_match(inst); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
        // Likewise, a permitted method on an unrelated local is not enough;
        // the receiver path is part of the authority proof.
        "impl OrdinaryBodyEngine { fn select_comptime_branch_with_resolved_types_and_membership(&mut self, inst: InstRef) { other.unwrap_or(inst); ComptimeEngine::new(self).select_branch((), inst, &mut env); } }",
    ];
    for source in mutations {
        let functions = ast_functions("sema/comptime_eval", source).expect("mutation parses");
        let adapter = functions
            .iter()
            .find(|function| {
                function.name == "select_comptime_branch_with_resolved_types_and_membership"
                    || function.name == "select_comptime_match_with_resolved_types_and_membership"
            })
            .expect("mutation identity");
        assert!(!assert_thin_adapter(adapter));
        assert!(has_peer_comptime_evaluator("sema/comptime_eval", source));
    }

    // Calling the canonical engine is not an authority exemption for a peer
    // which still dispatches and constructs a selection itself.
    let peer = "impl Peer { fn fold(&mut self, inst: InstRef) -> ComptimeOutcome { match inst.data { InstData::Branch { .. } => ComptimeSelection::Branch { taken: true }, _ => ComptimeSelection::Branch { taken: false } }; ComptimeEngine::new(self).select_branch((), inst, &mut env) } }";
    assert!(has_peer_comptime_evaluator("sema/comptime", peer));
}

#[test]
fn structured_registry_authority_keeps_storage_keyed_and_identity_rich() {
    let comptime = crate::sema::COMPTIME_SOURCE;
    let stable = comptime
        .split("pub fn structured_type_authority<")
        .nth(1)
        .and_then(|source| {
            source
                .split("pub fn structured_type_authority_with_program")
                .next()
        })
        .expect("stable structured authority accessor");
    assert_eq!(
        stable
            .matches("self.structured_type_authority_with_program(")
            .count(),
        1,
        "stable-key authority must delegate to the richer constructor"
    );
    let richer = comptime
        .split("pub fn structured_type_authority_with_program<")
        .nth(1)
        .and_then(|source| source.split("\n}\n\n/// Stable key").next())
        .expect("richer structured authority constructor");
    assert_eq!(richer.matches("self.programs.get(key)").count(), 1);
    assert!(richer.contains("registered.rir.type_syntax()"));
    assert!(richer.contains("registered_symbol_authority_is_valid"));
    assert!(richer.contains("from_registered(\n                program,"));
    assert!(!richer.contains("program.rir"));
    assert!(!richer.contains("program.symbols"));
}

/// The whole comptime host contract: the domain supertrait, every capability
/// trait, and the umbrella that re-forms them into `ComptimeHost`.
///
/// RUE-1831 Stage 2 split the contract across those traits, so a guard can no
/// longer find it by slicing from `pub trait ComptimeHost {` -- the umbrella is
/// empty now. The region is contiguous and ends where the engine begins.
fn comptime_host_contract(source: &str) -> &str {
    let start = source
        .find("pub trait ComptimeDomain {")
        .expect("comptime host contract starts at the domain supertrait");
    let end = source[start..]
        .find("pub struct ComptimeEngine")
        .expect("comptime host contract ends where the engine begins");
    &source[start..start + end]
}

#[test]
fn comptime_match_patterns_have_one_decoder_and_a_semantic_host_boundary() {
    let comptime = crate::sema::COMPTIME_SOURCE;
    let production = comptime
        .split("#[derive(Debug)]\npub struct ComptimeFrame")
        .nth(1)
        .expect("bounded production comptime source");
    let (before_decoder, decoder_and_after) = production
        .split_once("pub fn decode_comptime_match_pattern")
        .expect("canonical production decoder");
    let (decoder_body, after_decoder) = decoder_and_after
        .split_once("/// An already-evaluated call argument")
        .expect("decoder body boundary");
    let outside_decoder = format!("{before_decoder}{after_decoder}");
    assert!(before_decoder.contains("pub enum ComptimeMatchPattern"));
    for variant in [
        "RirPatternView::Wildcard",
        "RirPatternView::Bool",
        "RirPatternView::Int",
        "RirPatternView::Path",
    ] {
        assert!(decoder_body.contains(variant), "decoder misses {variant}");
        assert!(
            !outside_decoder.contains(variant),
            "raw pattern decoding escaped the canonical decoder: {variant}"
        );
    }
    assert_eq!(
        comptime
            .matches("pub fn decode_comptime_match_pattern")
            .count(),
        1,
        "AIR must have one production pattern decoder"
    );

    let host = comptime_host_contract(comptime);
    let match_hook = host
        .split("fn match_pattern(")
        .nth(1)
        .and_then(|source| source.split("fn match_no_selected_arm(").next())
        .expect("bounded ComptimeHost match hook");
    assert!(match_hook.contains("ComptimeMatchPattern<Self::Name>"));
    assert!(!match_hook.contains("ProgramKey"));
    assert!(!match_hook.contains("RirPatternView"));

    let engine_match = comptime
        .rsplit("InstData::Match { scrutinee, arms } => {")
        .next()
        .and_then(|source| source.split("InstData::AnonStructType {").next())
        .expect("bounded canonical match dispatch");
    let decode = engine_match
        .find("self.decode_match_pattern")
        .expect("engine decodes reached patterns");
    let host_match = engine_match
        .find("self.host.match_pattern")
        .expect("engine offers semantic patterns to host");
    assert!(decode < host_match, "decode must precede the host hook");
    assert!(engine_match.contains("for (pattern, body) in arms.iter()"));
    assert!(!engine_match.contains("RirPatternView::"));
}

// RUE-1831 Stage 2. The 64-method `ComptimeHost` is now a domain supertrait
// plus seven capability traits under an empty umbrella. Two ways that decays
// silently, neither of which fails to compile: a new method lands on the
// umbrella instead of a capability, reassembling the god object one signature
// at a time; or a capability is dropped from the umbrella's bound list, which
// quietly narrows the contract for every consumer that bounds on
// `ComptimeHost`.
#[test]
fn comptime_host_is_an_empty_umbrella_over_its_capabilities() {
    let contract = comptime_host_contract(crate::sema::COMPTIME_PRODUCTION_SOURCE);

    let capabilities = [
        "ComptimeInterrupts",
        "ComptimeProgramFacts",
        "ComptimeTypeAlgebra",
        "ComptimeValueAlgebra",
        "ComptimeCallProtocol",
        "ComptimeStructuredTypes",
        "ComptimeRejections",
    ];

    let umbrella = contract
        .split("pub trait ComptimeHost:")
        .nth(1)
        .expect("umbrella declaration");
    let (bounds, body) = umbrella.split_once('{').expect("umbrella body");
    assert_eq!(
        body.trim(),
        "}",
        "ComptimeHost must stay empty; a method belongs on a capability trait"
    );
    assert!(bounds.contains("ComptimeDomain"));
    for capability in capabilities {
        assert!(
            bounds.contains(capability),
            "{capability} dropped from the ComptimeHost bound would narrow the contract silently"
        );
    }

    // Every method sits in exactly one capability, and the total is unchanged
    // from the single trait this replaced.
    let mut owner: Vec<(&str, &str)> = Vec::new();
    let mut current = "";
    for line in contract.lines() {
        if let Some(rest) = line.strip_prefix("pub trait ") {
            current = rest.split([':', ' ']).next().unwrap_or_default();
        } else if let Some(rest) = line.strip_prefix("    fn ") {
            owner.push((rest.split('(').next().unwrap_or_default(), current));
        }
    }
    assert_eq!(owner.len(), 68, "the host contract lost or gained a method");
    for (method, trait_name) in &owner {
        assert!(
            capabilities.contains(trait_name),
            "{method} is declared on {trait_name}, not a capability trait"
        );
        assert_eq!(
            owner.iter().filter(|(name, _)| name == method).count(),
            1,
            "{method} is declared on more than one capability trait"
        );
    }
}

#[test]
fn diagnostic_hooks_are_keyed_by_the_engine_program() {
    let comptime = crate::sema::COMPTIME_SOURCE;
    let host = comptime_host_contract(comptime);
    for hook in [
        "fn match_no_selected_arm(",
        "fn require_preview(",
        "fn depth_exceeded(",
        "fn literal_out_of_range(",
        "fn cannot_negate(",
        "fn unsupported_anon_method_type_param(",
        "fn non_function_anon_method(",
        "fn resolve_named_array_length(",
        "fn check_require_droppable(",
        "fn check_trivially_droppable(",
        "fn resolve_comptime_type_intrinsic(",
        "fn integer_operation_type(",
        "fn unary_integer_type(",
        "fn compare_comptime_values(",
        "fn reject_non_type_array_repeat(",
        "fn finish_arith(",
    ] {
        let body = host
            .split(hook)
            .nth(1)
            .and_then(|source| source.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("missing diagnostic hook {hook}"));
        assert!(
            body.contains("ComptimeDiagnosticSite<Self::ProgramKey>"),
            "diagnostic hook is not producer-keyed: {hook}"
        );
        assert!(!body.contains("span: Span"), "raw span leaked into {hook}");
    }
    let site = comptime
        .split("pub struct ComptimeDiagnosticSite<P>")
        .nth(1)
        .and_then(|source| source.split("\n}\n\nimpl<P> ComptimeDiagnosticSite").next())
        .expect("diagnostic site fields");
    assert!(site.contains("program: P"));
    assert!(site.contains("span: Span"));
    assert!(!site.contains("pub program"));
    assert!(!site.contains("pub span"));

    let run_frame = comptime
        .split("fn run_frame(")
        .nth(1)
        .and_then(|source| source.split("fn diagnostic_site(").next())
        .expect("bounded frame runner");
    let depth = run_frame
        .find("depth_exceeded(")
        .expect("depth diagnostic call");
    let rejected = run_frame[..depth].rfind("frame.program.clone()");
    assert!(
        rejected.is_some(),
        "depth site must use rejected frame program"
    );
}

#[test]
fn comptime_generic_contract_has_no_local_lexical_or_call_payloads() {
    let comptime = crate::sema::COMPTIME_SOURCE;
    let ordinary = include_str!("sema/comptime_eval.rs");
    let facade = include_str!("lib.rs");
    for export in [
        "ComptimeAnonymousKind",
        "ComptimeArgMode",
        "ComptimeArrayLengthBinding",
        "ComptimeCallAdmission",
        "ComptimeCallArgument",
        "ComptimeCallKey",
        "ComptimeCallMemoLookup",
        "ComptimeNamedValueResolution",
        "ComptimeCallPreparation",
        "ComptimeCompletedCallMemo",
        "ComptimeEngine",
        "ComptimeStructuredTypeResolution",
        "ComptimeStructuredTypeSuspension",
        "ComptimeEnv",
        "ComptimeField",
        "ComptimeFile",
        "ComptimeFrame",
        "ComptimeMethodDescriptor",
        "ComptimeMethodParameter",
        "ComptimeMethodType",
        "ComptimeHost",
        "ComptimeHostError",
        "ComptimeHostResult",
        "ComptimeExpressionIntrinsic",
        "ComptimeExpressionIntrinsicRequest",
        "ComptimeIntegerBound",
        "ComptimeTargetIntrinsic",
        "ComptimeTypeIntrinsic",
        "ComptimeSite",
        "ComptimeSiteKind",
        "ComptimeIdentity",
        "ComptimeName",
        "ComptimeOutcome",
        "ComptimeMemoInsertError",
        "ComptimeMemoizedOutcome",
        "ComptimeProgram",
        "ComptimeProgramKey",
        "ComptimeProgramRegistrationError",
        "ComptimeProgramRegistry",
        "ComptimeTrap",
        "ComptimeType",
        "ComptimeValue",
        "ComptimeStructuredTypeJob",
        "ComptimeStructuredTypePoll",
        "ComptimeStructuredTypeRequest",
        "ComptimeStructuredTypeAuthority",
        "ComptimeStructuredTypeSymbolAuthority",
    ] {
        assert!(
            facade.contains(export),
            "canonical comptime export missing: {export}"
        );
    }
    let classifier = comptime
        .split("impl ComptimeTypeIntrinsic {")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n}\n\n/// The finite set of expression")
                .next()
        })
        .expect("type intrinsic classifier");
    assert_eq!(classifier.matches("fn from_name(").count(), 1);
    assert_eq!(
        comptime
            .matches("ComptimeTypeIntrinsic::from_name(")
            .count(),
        1
    );
    let expression_classifier = comptime
        .split("impl ComptimeExpressionIntrinsic {")
        .nth(1)
        .and_then(|source| source.split("\n}\n\n/// Structural facts").next())
        .expect("expression intrinsic classifier");
    assert_eq!(expression_classifier.matches("fn from_name(").count(), 1);
    for spelling in [
        "\"import\"",
        "\"target_arch\"",
        "\"target_os\"",
        "\"target_data_model\"",
    ] {
        assert!(expression_classifier.contains(spelling));
    }
    assert!(!comptime.contains("fn admit_comptime_intrinsic("));
    assert!(!comptime.contains("fn resolve_comptime_intrinsic("));
    assert!(comptime.contains("fn resolve_comptime_expression_intrinsic("));
    let expression_decoder = comptime
        .split("fn decode_expression_intrinsic(")
        .nth(1)
        .and_then(|source| source.split("\n    fn semantic_site(").next())
        .expect("shared expression intrinsic decoder");
    assert_eq!(
        comptime.matches("fn decode_expression_intrinsic(").count(),
        1
    );
    assert!(expression_decoder.contains("ComptimeExpressionIntrinsic::from_name"));
    assert!(expression_decoder.contains("ComptimeExpressionIntrinsicRequest"));
    assert!(expression_decoder.contains("ComptimeSiteKind"));
    assert!(!expression_decoder.contains("== \"import\""));
    let semantic_site = comptime
        .split("fn semantic_site(")
        .nth(1)
        .and_then(|source| source.split("\n    fn name_from_rir(").next())
        .expect("semantic-site occurrence scanner");
    assert!(semantic_site.contains("decode_expression_intrinsic("));
    assert!(!semantic_site.contains("display_name(&self.host.name_from_symbol"));
    assert!(!semantic_site.contains("== \"import\""));
    let intrinsic_dispatch = comptime
        .split("InstData::Intrinsic { name, args } => {")
        .nth(1)
        .and_then(|source| source.split("\n            // Enum variants").next())
        .expect("expression intrinsic dispatch");
    let decode_position = intrinsic_dispatch
        .find("decode_expression_intrinsic(")
        .expect("dispatch uses shared intrinsic decoder");
    let site_position = intrinsic_dispatch
        .find("self.semantic_site(")
        .expect("dispatch constructs semantic site");
    let hook_position = intrinsic_dispatch
        .find("resolve_comptime_expression_intrinsic(")
        .expect("dispatch calls typed intrinsic hook");
    assert!(decode_position < site_position && site_position < hook_position);
    assert!(!intrinsic_dispatch.contains("from_name(&display_name)"));
    assert!(comptime.contains("impl ComptimeIntegerBound {"));
    assert!(comptime.contains("pub fn as_str(self) -> &'static str"));
    assert_eq!(
        comptime
            .matches("fn resolve_comptime_type_intrinsic(")
            .count(),
        2,
        "one trait seam and one fake-host override must cover type intrinsics"
    );
    assert!(
        !comptime.contains("resolve_comptime_integer_bound("),
        "integer bounds must not grow a competing host override seam"
    );
    let production = crate::sema::COMPTIME_PRODUCTION_SOURCE;
    let production_forbidden = [
        "LocalVar",
        "ParamInfo",
        "ParamIndex",
        "FunctionCallInfo",
        "crate::types::Type",
        "TypeKind",
        "TypeInternPool",
        "ArrayTypeId",
        "StructField",
        "crate::types::StructId",
        "Spur",
        "FileId",
        "ThreadedRodeo",
        "body_interner",
        "anon_structs",
        "AnonymousNominalKey",
        "AnonymousNominalKind",
        "crate::types::ArrayLen",
        "SemanticBodyExportFailure",
        "CompileError",
        "CompileResult",
        "ConstValue",
        "ErrorKind",
        "comptime_panic_err",
        "ProducerFailure",
        "PreparedComptimeCall",
        "current_program",
        "call_depth",
    ];
    for forbidden in production_forbidden {
        assert!(
            !production.contains(forbidden),
            "canonical comptime production leaked local symbol: {forbidden}"
        );
    }
    assert!(
        !production
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|token| token == "ConstInfo"),
        "canonical comptime production leaked the local ConstInfo identifier"
    );
    let engine_start = production
        .find("pub struct ComptimeEngine")
        .expect("canonical comptime engine declaration");
    let contract = &production[..engine_start];
    for forbidden in production_forbidden {
        assert!(
            !contract.contains(forbidden),
            "generic comptime contract leaked local symbol: {forbidden}"
        );
    }
    assert!(contract.contains("type CallAdmission;"));
    assert!(contract.contains("type CallBinding;"));
    assert!(contract.contains("type BoundCall;"));
    assert!(contract.contains("fn begin_comptime_call_binding("));
    assert!(contract.contains("fn bind_comptime_call_argument("));
    assert!(contract.contains("fn finish_comptime_call_binding("));
    let binding_contract = contract
        .split("fn begin_comptime_call_binding(")
        .nth(1)
        .and_then(|source| source.split("fn finish_comptime_call_binding(").next())
        .expect("incremental binding contract");
    assert!(binding_contract.contains("&self"));
    assert!(!binding_contract.contains("&mut self"));
    assert!(contract.contains("type CompletionTicket;"));
    assert!(!contract.contains("type CompletionTicket: Clone"));
    assert!(!contract.contains("pub completion_ticket"));
    assert!(contract.contains("type Type: ComptimeType;"));
    assert!(contract.contains("type Failure;"));
    assert!(contract.contains("Self::CanonicalIdentity"));
    assert!(contract.contains("Self::File,"));
    assert!(contract.contains("fn match_no_selected_arm("));
    let rejection_hook = contract
        .split("fn reject_comptime_expression(")
        .nth(1)
        .and_then(|source| {
            source
                .split("fn evaluate_binary_rhs_after_rejection(")
                .next()
        })
        .expect("semantic rejection hook");
    assert!(rejection_hook.contains("ComptimeSemanticRejection<Self::Value>"));
    assert!(contract.contains("fn evaluate_binary_rhs_after_rejection("));
    assert!(contract.contains("ComptimeOutcome<Self::Value, Self::Failure>"));
    assert!(production.contains("pub enum ComptimeSemanticRejection"));
    assert!(production.contains("pub enum ComptimeIntegerOperation"));
    assert!(production.contains("pub enum ComptimeUnaryOperation"));
    for operation in [
        "Add", "Sub", "Mul", "Div", "Mod", "Lt", "Gt", "Le", "Ge", "BitAnd", "BitOr", "BitXor",
        "Shl", "Shr",
    ] {
        assert!(
            production.contains(operation),
            "missing integer operation: {operation}"
        );
    }
    for (start, end, operation) in [
        (
            "InstData::Add { lhs, rhs } => {",
            "InstData::Sub { lhs, rhs } => {",
            "Add",
        ),
        (
            "InstData::Sub { lhs, rhs } => {",
            "InstData::Mul { lhs, rhs } => {",
            "Sub",
        ),
        (
            "InstData::Mul { lhs, rhs } => {",
            "InstData::Div { lhs, rhs } | InstData::Mod { lhs, rhs } => {",
            "Mul",
        ),
        (
            "InstData::Lt { lhs, rhs } => {",
            "InstData::Gt { lhs, rhs } => {",
            "Lt",
        ),
        (
            "InstData::Gt { lhs, rhs } => {",
            "InstData::Le { lhs, rhs } => {",
            "Gt",
        ),
        (
            "InstData::Le { lhs, rhs } => {",
            "InstData::Ge { lhs, rhs } => {",
            "Le",
        ),
        (
            "InstData::Ge { lhs, rhs } => {",
            "// Logical operations",
            "Ge",
        ),
        (
            "InstData::BitAnd { lhs, rhs } => {",
            "InstData::BitOr { lhs, rhs } => {",
            "BitAnd",
        ),
        (
            "InstData::BitOr { lhs, rhs } => {",
            "InstData::BitXor { lhs, rhs } => {",
            "BitOr",
        ),
        (
            "InstData::BitXor { lhs, rhs } => {",
            "InstData::Shl { lhs, rhs } | InstData::Shr { lhs, rhs } => {",
            "BitXor",
        ),
    ] {
        let arm = production
            .split(start)
            .nth(1)
            .and_then(|source| source.split(end).next())
            .unwrap_or_else(|| panic!("missing bounded {operation} dispatch arm"));
        assert!(
            arm.contains(&format!("ComptimeIntegerOperation::{operation},")),
            "{start} is not routed through {operation} rejection policy"
        );
    }
    let div_mod_arm = production
        .split("InstData::Div { lhs, rhs } | InstData::Mod { lhs, rhs } => {")
        .nth(1)
        .and_then(|source| source.split("// Comparison operations").next())
        .expect("bounded division/remainder dispatch arm");
    assert!(div_mod_arm.contains(
        "if is_div {\n                    ComptimeIntegerOperation::Div\n                } else {\n                    ComptimeIntegerOperation::Mod\n                }"
    ));
    let shift_arm = production
        .split("InstData::Shl { lhs, rhs } | InstData::Shr { lhs, rhs } => {")
        .nth(1)
        .and_then(|source| source.split("InstData::BitNot { operand } => {").next())
        .expect("bounded shift dispatch arm");
    assert!(shift_arm.contains(
        "if is_shl {\n                        ComptimeIntegerOperation::Shl\n                    } else {\n                        ComptimeIntegerOperation::Shr\n                    }"
    ));
    for rejection in [
        "ConditionNotBoolean",
        "ArithmeticOperandNotInteger",
        "UnaryOperandNotInteger",
        "UnaryTypeNotInteger",
        "FloatOperandWidthMismatch",
        "FloatRemainder",
        "Assignment",
        "AggregateExpression",
        "EmptyBlock",
        "UnsupportedIntrinsic(String)",
        "UnsupportedExpression",
    ] {
        assert!(
            production.contains(rejection),
            "missing rejection: {rejection}"
        );
    }
    assert!(production.contains("decode_anon_method_descriptors"));
    let neg_dispatch = production
        .split("InstData::Neg { operand } => {")
        .nth(1)
        .and_then(|source| source.split("// These control-flow").next())
        .expect("negation dispatch");
    let evaluated_neg = neg_dispatch
        .split("match self.eval(*operand, env)")
        .nth(1)
        .expect("nonliteral negation dispatch");
    assert!(
        evaluated_neg.find("value.as_integer()").unwrap()
            < evaluated_neg.find("unary_integer_type_for").unwrap(),
        "nonliteral negation must extract the scalar before unary type policy"
    );
    assert!(production.contains("Root expression frames are intentionally ticket-free"));
    assert!(production.contains("if frame.name.is_none()"));
    assert!(!production.contains("eval_const_expr"));
    assert!(!production.contains("ComptimeEngine::new"));
    assert!(!production.contains("reduce_external_comptime_call"));
    let match_dispatch = production
        .split("InstData::Match { scrutinee, arms } => {")
        .nth(1)
        .and_then(|source| source.split("InstData::AnonStructType {").next())
        .expect("match dispatch");
    assert!(match_dispatch.contains(
        "}\n                self.host.match_no_selected_arm(&self.diagnostic_site(span))"
    ));
    assert!(
        !match_dispatch
            .contains("}\n                ComptimeOutcome::RuntimeDependent\n            }")
    );
    let ordinary_match = ordinary
        .split("fn match_no_selected_arm(")
        .nth(1)
        .and_then(|source| source.split("\n    fn ").next())
        .expect("ordinary match terminal hook");
    assert!(ordinary_match.contains("ComptimeOutcome::RuntimeDependent"));
    let macro_start = production
        .find("macro_rules! host_value")
        .expect("canonical host-value funnel");
    let macro_end = macro_start
        + production[macro_start..]
            .find("\n}\n\n/// Error classification")
            .map(|offset| offset + 2)
            .expect("canonical host-value funnel end");
    let production_outside_host_value =
        format!("{}{}", &production[..macro_start], &production[macro_end..]);
    assert!(
        !production_outside_host_value.contains("ComptimeHostError::"),
        "tagged host errors must be converted only by host_value!"
    );
    assert_eq!(production.matches("struct PreparedComptimeCall").count(), 0);

    let host_start = production
        .find("pub trait ComptimeDomain")
        .expect("canonical host contract");
    let host_contract = &production[host_start..engine_start];
    assert!(host_contract.contains("fn program_rir(&self, program:"));
    assert!(host_contract.contains("fn file_for_program_span("));
    assert!(host_contract.contains("program: &Self::ProgramKey"));
    assert!(!host_contract.contains("fn file_from_span("));
    assert_eq!(
        host_contract
            .matches("fn resolve_comptime_named_value(")
            .count(),
        1,
        "VarRef semantic lookup must have one atomic host hook"
    );
    let atomic_named_value_hook = host_contract
        .split("fn resolve_comptime_named_value(")
        .nth(1)
        .and_then(|source| source.split("\n    fn ").next())
        .expect("atomic named-value host hook");
    assert!(atomic_named_value_hook.contains("&mut self"));
    assert!(atomic_named_value_hook.contains("ComptimeNamedValueResolution"));
    assert!(!host_contract.contains("fn value_const("));
    assert!(!host_contract.contains("fn record_value_const_dependency("));
    assert!(!host_contract.contains("fn record_named_type_dependency("));
    for removed in [
        "fn anonymous_struct_id(",
        "fn has_method(",
        "fn check_unqualified_visibility(",
        "fn set_anon_struct_type_subst(",
    ] {
        assert!(
            !host_contract.contains(removed),
            "obsolete engine-unused host hook remains: {removed}"
        );
    }
    let named_type_hook = host_contract
        .split("fn resolve_named_type_value(")
        .nth(1)
        .and_then(|source| source.split("\n    fn ").next())
        .expect("keyed TypeConst host hook");
    assert!(named_type_hook.contains("program: &Self::ProgramKey"));
    for hook in [
        "fn find_or_create_anon_struct(",
        "fn find_or_create_anon_enum(",
    ] {
        let body = host_contract
            .split(hook)
            .nth(1)
            .and_then(|source| source.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("missing anonymous nominal hook: {hook}"));
        assert!(
            body.contains("type_subst"),
            "{hook} must receive type captures"
        );
        assert!(
            body.contains("value_subst"),
            "{hook} must receive value captures"
        );
    }
    assert!(production.contains("enum ComptimeArrayLengthBinding"));
    let array_dispatch = production
        .split("InstData::ArrayRepeat { value, count } => {")
        .nth(1)
        .and_then(|source| source.split("InstData::VarRef { name, .. } => {").next())
        .expect("array repeat dispatch");
    assert!(array_dispatch.contains("classify_array_length_binding"));
    let array_hook = host_contract
        .split("fn resolve_named_array_length(")
        .nth(1)
        .and_then(|source| source.split("\n    fn ").next())
        .expect("array-length host hook");
    assert!(array_hook.contains("ComptimeArrayLengthBinding<Self::Value>"));
    assert!(array_hook.contains("ComptimeOutcome<u64, Self::Failure>"));
    assert!(array_dispatch.contains("outcome_value!(self.host.resolve_named_array_length"));
    let struct_dispatch = production
        .split("InstData::AnonStructType {")
        .nth(1)
        .and_then(|source| source.split("InstData::AnonEnumType {").next())
        .expect("anonymous struct dispatch");
    assert!(struct_dispatch.contains("local_type_subst"));
    assert!(struct_dispatch.contains("local_value_subst"));
    let enum_dispatch = production
        .split("InstData::AnonEnumType {")
        .nth(1)
        .and_then(|source| source.split("InstData::TypeConst {").next())
        .expect("anonymous enum dispatch");
    assert!(enum_dispatch.contains("enum_type_subst"));
    assert!(enum_dispatch.contains("enum_value_subst"));
    let classifier = production
        .split("fn classify_array_length_binding(")
        .nth(1)
        .and_then(|source| source.split("\n    fn ").next())
        .expect("engine array-length classifier");
    assert!(classifier.contains("ComptimeArrayLengthBinding::Unbound"));
    assert!(classifier.contains("ComptimeArrayLengthBinding::Shadowed"));
    assert!(classifier.contains("ComptimeArrayLengthBinding::LocalValue(value.clone())"));
    assert!(classifier.contains("ComptimeArrayLengthBinding::RuntimeDependent"));
    assert!(
        !classifier.contains("value.as_integer().is_some()"),
        "array-length classification must preserve non-integer lexical values"
    );
    let classifier_order = [
        "env.locals",
        "env.is_runtime_local_name",
        "env.type_subst",
        "env.value_subst",
        "env.runtime_binding_names",
    ];
    let mut previous = 0;
    for needle in classifier_order {
        let offset = classifier
            .find(needle)
            .unwrap_or_else(|| panic!("classifier missing {needle}"));
        assert!(
            offset >= previous,
            "array-length precedence regressed at {needle}"
        );
        previous = offset;
    }
    let subst_helper = production
        .split("fn substs_with_locals(")
        .nth(1)
        .and_then(|source| source.split("\n    pub fn new").next())
        .expect("local substitution helper");
    assert!(subst_helper.contains("value_subst.remove(name)"));
    assert!(subst_helper.contains("type_subst.remove(name)"));
    let argument_start = production
        .find("pub struct ComptimeCallArgument<")
        .expect("engine-owned call argument provenance wrapper");
    let argument_end = production[argument_start..]
        .find("\n}\n\nimpl<V> ComptimeCallArgument")
        .map(|offset| argument_start + offset + 2)
        .expect("call argument provenance wrapper body");
    let argument_wrapper = &production[argument_start..argument_end];
    assert!(argument_wrapper.contains("value: V"));
    assert!(argument_wrapper.contains("direct_unit_literal: bool"));
    for forbidden in [
        "pub value",
        "pub direct_unit_literal",
        "InstRef",
        "Rir",
        "ProgramKey",
        "Span",
        "Deref",
        "pub fn new",
    ] {
        assert!(
            !argument_wrapper.contains(forbidden),
            "call argument provenance leaked source authority: {forbidden}"
        );
    }
    let argument_impl_start = production[argument_end..]
        .find("impl<V> ComptimeCallArgument")
        .map(|offset| argument_end + offset)
        .expect("call argument provenance impl");
    let argument_impl_end = production[argument_impl_start..]
        .find("\n}\n")
        .map(|offset| argument_impl_start + offset + 3)
        .expect("call argument provenance impl end");
    let argument_impl = &production[argument_impl_start..argument_impl_end];
    assert!(argument_impl.contains("fn new("));
    assert!(!argument_impl.contains("pub fn new("));
    assert!(production.contains("argument: ComptimeCallArgument<Self::Value>"));
    assert!(production.contains("fn begin_comptime_call_binding("));
    assert!(production.contains("fn finish_comptime_call_binding("));
    let producer_hook = host_contract
        .split("fn canonical_function_producer(")
        .nth(1)
        .and_then(|source| source.split("\n    fn ").next())
        .expect("canonical producer hook");
    assert!(producer_hook.contains("program: &Self::ProgramKey"));
    assert!(producer_hook.contains("ticket: &Self::CompletionTicket"));
    for forbidden in [
        "ComptimeFrame",
        "Rir",
        "ValidatedRir",
        "InstRef",
        "InstData",
        "ComptimeEnv",
        "program_rir",
        "child_instructions",
        "eval(",
        "evaluate",
        "callback",
        "closure",
        "ComptimeEngine",
        "SemanticConstEvaluator",
        "ComptimeCallDepth",
    ] {
        assert!(
            !producer_hook.contains(forbidden),
            "canonical producer hook leaked evaluator authority: {forbidden}"
        );
    }
    assert!(!host_contract.contains("extract_anon_method_sigs"));
    assert!(!host_contract.contains("find_method_own_comptime_type_param"));
    assert!(!host_contract.contains("ComptimeEngine::new"));
    assert!(
        !host_contract.contains("-> Result<"),
        "fallible host hooks must use ComptimeHostResult"
    );
    for hook in ["fn integer_operation_type(", "fn unary_integer_type("] {
        let start = host_contract
            .find(hook)
            .unwrap_or_else(|| panic!("missing semantic integer hook: {hook}"));
        let end = host_contract[start + hook.len()..]
            .find("\n    fn ")
            .map(|offset| start + hook.len() + offset)
            .unwrap_or(host_contract.len());
        let signature = &host_contract[start..end];
        for forbidden in ["ComptimeEnv", "InstRef"] {
            assert!(
                !signature.contains(forbidden),
                "{hook} leaked evaluator authority: {forbidden}"
            );
        }
    }
    let preparation_start = host_contract
        .find("fn prepare_comptime_call")
        .expect("canonical call preparation hook");
    assert_eq!(
        host_contract.matches("fn prepare_comptime_call").count(),
        1,
        "call preparation must have one host authority"
    );
    let finish_start = host_contract[preparation_start..]
        .find("fn finish_comptime_call")
        .map(|offset| preparation_start + offset)
        .expect("call finish hook");
    let preparation = &host_contract[preparation_start..finish_start];
    assert!(!preparation.contains("RirCallArg"));
    assert!(!preparation.contains("InstRef"));
    assert_eq!(
        production.matches("fn eval(").count(),
        1,
        "canonical comptime module must have one recursive dispatcher"
    );
    assert_eq!(
        production.matches("match &inst.data").count(),
        1,
        "the AIR instruction dispatcher must have one authoritative data match"
    );
    assert_eq!(
        production.matches("match data").count(),
        1,
        "the AIR recursion trampoline must have one routing match"
    );
    assert_eq!(
        production.matches("fn eval_dispatch(").count(),
        1,
        "the AIR instruction dispatcher must have one implementation"
    );
    assert_eq!(
        production.matches("fn evaluate_call_arguments(").count(),
        1,
        "call argument provenance must have one engine-owned helper"
    );
    assert_eq!(
        production.matches("self.evaluate_call_arguments(").count(),
        2,
        "direct and qualified calls must share argument provenance"
    );
    let provenance_helper_start = production
        .find("fn evaluate_call_arguments(")
        .expect("argument provenance helper");
    let provenance_helper_end = production[provenance_helper_start..]
        .find("\n    fn ")
        .map(|offset| provenance_helper_start + offset)
        .unwrap_or(production.len());
    let provenance_helper = &production[provenance_helper_start..provenance_helper_end];
    let eval_offset = provenance_helper
        .find("self.eval(arg.value, env)")
        .expect("argument evaluation in provenance helper");
    let provenance_offset = provenance_helper
        .find("self.host.program_rir(&program)")
        .expect("parent-program provenance lookup");
    assert!(
        provenance_helper.contains("let program = self.program_key()"),
        "argument provenance must capture the parent program before recursion"
    );
    assert!(
        eval_offset < provenance_offset,
        "argument provenance must classify only after a Known child evaluation"
    );
    assert!(
        !ordinary.contains("is_direct_unit_literal"),
        "ordinary hosts must consume provenance, never recover it from RIR"
    );
    assert!(ordinary.contains("fn bind_comptime_call_argument("));
    let ordinary_argument_binding = ordinary
        .split("fn bind_comptime_call_argument(")
        .nth(1)
        .and_then(|source| source.split("\n    fn ").next())
        .expect("ordinary argument binding implementation");
    assert!(ordinary_argument_binding.contains("argument.value()"));
    assert!(!ordinary_argument_binding.contains("program_rir"));
    let ordinary_named_value_start = ordinary
        .find("fn resolve_comptime_named_value(")
        .expect("ordinary atomic named-value adapter");
    let ordinary_named_value_end = ordinary[ordinary_named_value_start..]
        .find("\n    fn ")
        .map(|offset| ordinary_named_value_start + offset)
        .unwrap_or(ordinary.len());
    let ordinary_named_value = &ordinary[ordinary_named_value_start..ordinary_named_value_end];
    assert!(ordinary_named_value.contains("&mut self"));
    assert!(ordinary_named_value.contains("ComptimeNamedValueResolution"));
    let value_lookup = ordinary_named_value
        .find("OrdinaryBodyEngine::value_const(self")
        .expect("ordinary value-constant lookup");
    let value_dependency = ordinary_named_value
        .find("NamedConstDependencyTargetEvent::ValueConst")
        .expect("ordinary value-constant dependency observation");
    let visibility_check = ordinary_named_value
        .find("OrdinaryBodyEngine::check_unqualified_visibility")
        .expect("ordinary value-constant visibility check");
    let value_classification = ordinary_named_value
        .find("let value = match info.value")
        .expect("ordinary value classification");
    let type_lookup = ordinary_named_value
        .find("OrdinaryBodyEngine::resolve_named_type_value")
        .expect("ordinary named-type fallback");
    let type_dependency = ordinary_named_value
        .find("NamedConstDependencyTargetEvent::NamedType")
        .expect("ordinary named-type dependency observation");
    let type_return = ordinary_named_value
        .find("return Ok(ComptimeNamedValueResolution::Known(ConstValue::Type(ty)))")
        .expect("ordinary named-type branch returns before const handling");
    assert!(value_lookup < value_dependency);
    assert!(value_dependency < visibility_check);
    assert!(visibility_check < value_classification);
    assert!(value_lookup < type_lookup);
    assert!(type_lookup < type_dependency);
    assert!(type_dependency < type_return);
    assert!(type_return < value_dependency);
    assert!(ordinary.contains("type CallBinding = OrdinaryComptimeCallBinding"));
    assert!(ordinary.contains("type BoundCall = OrdinaryComptimeBoundCall"));
    let var_ref_start = production
        .find("InstData::VarRef { name, .. } => {")
        .expect("canonical VarRef dispatch arm");
    let var_ref = &production[var_ref_start..][..production[var_ref_start..]
        .find("InstData::FieldGet { base, field }")
        .expect("VarRef dispatch boundary")];
    assert_eq!(var_ref.matches("resolve_comptime_named_value(").count(), 1);
    assert!(var_ref.contains("file_for_program_span(&program, &span)"));
    assert!(!var_ref.contains("value_const("));
    assert!(!var_ref.contains("record_value_const_dependency("));
    assert!(!var_ref.contains("record_named_type_dependency("));
    let binding_state = ordinary
        .split("struct OrdinaryComptimeCallBinding")
        .nth(1)
        .and_then(|source| {
            source
                .split("impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeDomain")
                .next()
        })
        .expect("ordinary binding state");
    assert!(!binding_state.contains("derive(Clone"));
    assert!(!binding_state.contains("impl Clone"));
    assert!(production.contains("begin_comptime_call_binding"));
    assert!(production.contains("bind_comptime_call_argument"));
    assert!(production.contains("finish_comptime_call_binding"));
    assert!(production.contains("self.frames.push(frame)"));
    assert!(production.contains("self.frames.pop()"));
    assert!(production.contains("ComptimeCallPreparation::Memoized(outcome)"));
    let run_frame = production
        .find("fn run_frame(")
        .map(|start| &production[start..])
        .expect("frame lifecycle");
    assert!(run_frame.find("entered_depth").is_some());
    assert!(run_frame.find("canonical_function_producer").is_some());
    assert!(run_frame.find("self.frames.push(frame)").is_some());
    assert!(run_frame.find("self.frames.pop()").is_some());
    assert!(
        run_frame.find("self.frames.push(frame)").unwrap()
            < run_frame.find("self.frames.pop()").unwrap(),
        "frame cleanup must follow entry"
    );

    let env_source = crate::sema::COMPTIME_SOURCE;
    let env_start = env_source
        .find("pub struct ComptimeEnv")
        .expect("generic comptime environment declaration");
    let env_end = env_source[env_start..]
        .find("#[cfg(test)]")
        .map(|offset| env_start + offset)
        .expect("generic comptime environment boundary");
    let env = &env_source[env_start..env_end];
    for forbidden in production_forbidden {
        assert!(
            !env.contains(forbidden),
            "generic comptime environment leaked local symbol: {forbidden}"
        );
    }
    assert!(env.contains("runtime_local_names"));
    assert!(env.contains("runtime_binding_names"));
    assert!(env.contains("resolved_types: Option<&'a AHashMap<InstRef, T>>"));
    let analysis_source = include_str!("sema/comptime_eval.rs");
    let analysis_source = analysis_source
        .find("pub(crate) fn for_analysis")
        .and_then(|start| {
            analysis_source[start..].find("ctx.params.iter().map(|param| param.name)")
        })
        .is_some();
    assert!(analysis_source);
    assert!(!include_str!("sema/comptime_eval.rs").contains("filter(|param| !param.is_comptime)"));

    let alias_source = include_str!("sema/comptime_eval.rs");
    let alias_start = alias_source
        .find("fn try_eval_type_alias_init")
        .expect("pre-inference type-alias adapter");
    let alias_end = alias_source[alias_start..]
        .find("/// Pre-reduce inline type-constructor heads")
        .map(|offset| alias_start + offset)
        .expect("type-alias adapter boundary");
    let alias = &alias_source[alias_start..alias_end];
    assert!(alias.contains("env.runtime_local_names = runtime_bindings.clone();"));
    assert!(!alias.contains("env.runtime_binding_names = runtime_bindings.clone();"));
}

#[test]
fn comptime_depth_has_one_canonical_authority() {
    let comptime = crate::sema::COMPTIME_SOURCE;
    let sema = include_str!("sema/mod.rs");
    let specialize = include_str!("specialize.rs");
    assert_eq!(
        comptime
            .matches("pub const MAX_COMPTIME_CALL_DEPTH")
            .count(),
        1,
        "the depth limit must have one definition"
    );
    assert!(
        comptime.contains("pub const fn next_comptime_depth")
            && comptime.contains("pub const fn comptime_depth_over_limit")
            && comptime.contains("pub fn callable_body")
            && comptime.contains("comptime_depth_over_limit(entered_depth)"),
        "the evaluator must use its canonical depth predicate"
    );
    assert!(
        sema.contains("comptime_depth_over_limit")
            && specialize.contains("pub use crate::sema::MAX_COMPTIME_CALL_DEPTH")
            && !specialize.contains("MAX_SPECIALIZATION_ROUNDS"),
        "specialization must consume the canonical limit without a divergent guard"
    );
}

#[test]
fn type_syntax_dependency_admission_indexes_only_the_large_case() {
    let typeck = include_str!("sema/typeck.rs");

    assert!(
        typeck.contains("observed_type_dependency_index: Option<AHashSet<ObservedTypeDependency>>")
    );
    assert!(typeck.contains("const LINEAR_ADMISSION_LIMIT: usize = 8"));
    assert!(typeck.contains("if index.insert(dependency.clone())"));
    assert!(typeck.contains("index.clear()"));
    assert!(typeck.contains(".len() == LINEAR_ADMISSION_LIMIT"));
}

#[test]
fn accessor_producers_do_not_spell_their_own_declaration_diagnostics() {
    // RUE-1232: the 6.6:3-6.6:7 accessor rules have one source of truth in
    // `declaration_validation`. Each producer lowers its own representation
    // into that vocabulary and reports what it hands back; none of them may
    // construct an accessor declaration diagnostic itself, which is how the
    // RIR walks and the driver's reparsed-AST walk stay unable to drift on
    // wording.
    let producers = [
        include_str!("sema/declarations.rs"),
        include_str!("sema/ordinary_engine.rs"),
        include_str!("sema/control_flow.rs"),
    ]
    .concat();

    for kind in [
        ["ErrorKind::Accessor", "RequiresBorrowSelf"].concat(),
        ["ErrorKind::Accessor", "ParamModeUnsupported"].concat(),
        ["ErrorKind::Accessor", "BodyMissingYield"].concat(),
        ["ErrorKind::Accessor", "BodyOtherExit"].concat(),
        ["ErrorKind::Accessor", "YieldNotReceiverRooted"].concat(),
    ] {
        assert!(
            !producers.contains(&kind),
            "an accessor producer regained its own copy of a declaration rule: {kind}"
        );
    }
    // The declaration subject is a rule too: 6.6:3 names the same form
    // everywhere.
    assert!(
        !producers.contains("\"a `-> borrow` accessor\""),
        "an accessor producer regained its own 6.6:3 gate subject"
    );

    // Signature legality is decided before either body-analysis host can run:
    // declaration binding owns the whole-RIR path, and the durable signature
    // query owns the provider path. The ordinary engine therefore must not
    // become a third signature producer again (RUE-1233).
    assert!(
        !include_str!("sema/ordinary_engine.rs").contains("accessor_signature("),
        "the ordinary body engine regained an accessor signature re-check"
    );
}

#[test]
fn canonical_type_surface_has_one_checked_handle_and_private_storage_ids() {
    let types = include_str!("types.rs");
    let pool = include_str!("intern_pool.rs");
    let encoding = include_str!("type_encoding.rs");
    let exports = include_str!("lib.rs");
    let public_surface = [types, pool, encoding, exports].concat();

    let peer_handle = ["Interned", "Type"].concat();
    let compatibility_module = ["mod ", "compatibility"].concat();
    for retired in [
        peer_handle,
        ["type_to_", "interned"].concat(),
        ["interned_to_", "type"].concat(),
        compatibility_module,
        ["update_", "struct_def"].concat(),
        ["update_", "enum_def"].concat(),
    ] {
        assert!(
            !public_surface.contains(&retired),
            "AIR regained a peer type representation: {retired}"
        );
    }

    for line in public_surface.lines().map(str::trim) {
        for raw_api in [
            "pub const fn from_pool_index(",
            "pub fn from_pool_index(",
            "pub const fn pool_index(",
            "pub fn pool_index(",
            "pub const fn raw_encoding(",
            "pub fn raw_encoding(",
            "pub const fn from_u32(",
            "pub fn from_u32(",
            "pub const fn as_u32(",
            "pub fn as_u32(",
        ] {
            assert!(
                !line.starts_with(raw_api),
                "AIR exposed an unchecked raw type API: {line}"
            );
        }
    }

    for line in types.lines().map(str::trim) {
        for id in [
            "StructId",
            "EnumId",
            "ArrayTypeId",
            "PtrConstTypeId",
            "PtrMutTypeId",
        ] {
            assert!(
                !line.starts_with(&format!("pub struct {id}(pub ")),
                "AIR exposed {id}'s raw storage field"
            );
            assert!(
                !types.contains(&format!("Display for {id}")),
                "AIR exposed {id}'s raw numeric display"
            );
        }
    }

    assert_eq!(encoding.matches("enum Primitive").count(), 1);
    assert_eq!(encoding.matches("enum Composite").count(), 1);
    let consumers = [types, pool, exports].concat();
    for duplicated_tag in [
        "Struct = 100",
        "Enum = 101",
        "Array = 102",
        "Module = 103",
        "PtrConst = 104",
        "PtrMut = 105",
    ] {
        assert!(
            !consumers.contains(duplicated_tag),
            "composite tag escaped the authoritative encoding: {duplicated_tag}"
        );
    }

    assert!(types.contains("pub fn try_from_u32(v: u32) -> Option<Self>"));
    assert!(types.contains("pub fn try_kind(&self) -> Option<TypeKind>"));
    assert!(public_surface.contains("pub struct TypeInternPool"));
    assert!(pool.contains("pub fn all_types(&self) -> impl ExactSizeIterator<Item = Type> + '_"));
    assert!(pool.contains("pub(crate) fn set_struct_destructor("));
}

#[test]
fn air_payload_ownership_and_validation_boundary_cannot_regress() {
    let inst = include_str!("inst.rs");
    let exports = include_str!("lib.rs");
    let semantic_output = include_str!("sema/output.rs");
    let imported_body = include_str!("semantic_body.rs");

    assert!(inst.contains("pub struct AirEditor {"));
    assert!(inst.contains("pub struct ValidatedAir {"));
    assert!(inst.contains("impl std::ops::Deref for ValidatedAir"));
    assert!(!inst.contains("impl std::ops::DerefMut for ValidatedAir"));
    assert!(!inst.contains("pub fn add_inst("));
    assert!(!inst.contains("pub fn add_extra("));
    assert!(!inst.contains("pub fn get_extra("));
    let validated_impl = inst
        .split("impl ValidatedAir {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n\nimpl Air {").next())
        .expect("validated AIR implementation");
    assert!(validated_impl.contains("pub fn into_editor(self) -> AirEditor"));
    assert!(!validated_impl.contains("&mut self"));

    // Payload ranges expose logical lengths, but their positions and
    // construction remain owner-private. They are deliberately non-Copy so a
    // detached token cannot be casually duplicated into another owner.
    let range_macro = inst
        .split("macro_rules! word_range")
        .nth(1)
        .and_then(|rest| rest.split("word_range!(AirMatchArms)").next())
        .expect("typed AIR range macro");
    assert!(range_macro.contains("start: u32"));
    assert!(range_macro.contains("extent: u32"));
    assert!(!range_macro.contains("pub start"));
    assert!(!range_macro.contains("pub extent"));
    assert!(!range_macro.contains("Clone"));
    assert!(!range_macro.contains("Copy"));

    assert!(exports.contains("AirEditor"));
    assert!(exports.contains("ValidatedAir"));
    assert!(semantic_output.contains("pub air: crate::ValidatedAir"));
    assert!(imported_body.contains("pub air: crate::ValidatedAir"));

    let air = inst
        .split("pub struct Air {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("AIR owner declaration");
    for store in ["instructions", "extra", "projections", "places"] {
        assert!(
            !air.contains(&format!("pub {store}:")),
            "AIR exposed {store} store"
        );
    }
    let place = inst
        .split("pub struct AirPlace {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("AIR place declaration");
    assert!(!place.contains("pub projections:"));
    for raw_api in [
        "pub fn add_extra(",
        "pub fn get_extra(",
        "pub fn extra_mut(",
        "pub fn projection_store_mut(",
        "pub fn from_parts(",
    ] {
        assert!(
            !inst.contains(raw_api),
            "AIR exposed raw payload API: {raw_api}"
        );
    }
    assert_eq!(inst.matches("word_range!(Air").count(), 10);
    assert_eq!(crate::AIR_PAYLOAD_FAMILY_NAMES.len(), 10);

    // Semantic consumers receive an immutable RIR: the provider host reads
    // bodies through `BodyRirView`/`BodyRirBundle` borrows and no semantic
    // construction takes a mutable RIR, so no body-analysis entry point is a
    // payload escape hatch.
    let identity = include_str!("sema/body_identity.rs");
    assert!(identity.contains("pub fn from_parts(rir: &'a Rir,"));
    let sema_sources = [
        include_str!("sema/mod.rs"),
        include_str!("sema/body_identity.rs"),
        include_str!("sema/provider_body_host.rs"),
        include_str!("sema/ordinary_engine.rs"),
    ]
    .concat();
    assert!(!sema_sources.contains("&'a mut Rir"));
}

#[test]
fn semantic_schema_scaffolding_stays_exhaustive_and_reviewable() {
    let body = include_str!("semantic_body.rs");
    let import = include_str!("semantic_import.rs");

    assert!(body.contains("macro_rules! semantic_body_inst_schema"));
    assert!(body.contains("pub enum SemanticBodyInstKind"));
    // The traversal is the contract; its exact type-parameter list is not. The
    // sibling checks in this test pin `pub fn name(` for the same reason --
    // adding a bound to `K2`/`M2` changes nothing this guard exists to protect.
    assert!(body.contains("pub fn try_map_keys<"));
    assert!(body.contains("pub fn visit_dependencies("));
    assert!(body.contains("pub struct SemanticBodyInstFailureContext<E>"));
    assert!(body.contains("let data = inst.data.try_map_keys(key, module)?;"));
    assert!(import.contains("macro_rules! semantic_import_type_schema"));
    assert!(import.contains("macro_rules! semantic_import_const_schema"));
    assert!(import.contains("pub enum SemanticImportTypeKind"));
    assert!(import.contains("pub enum SemanticImportConstKind"));

    let generated_kind = body
        .split("pub const fn kind(&self) -> SemanticBodyInstKind")
        .nth(1)
        .and_then(|source| source.split("semantic_body_inst_schema!(").next())
        .expect("generated semantic instruction kind implementation");
    assert!(generated_kind.contains("match self"));
    assert!(!generated_kind.contains("_ =>"));
}

#[test]
fn local_semantic_materialization_owns_complete_cfg_inputs_without_a_peer_cache() {
    let import = include_str!("semantic_import.rs");
    let providers = include_str!("sema/provider_body_host.rs");
    let materialization = import
        .split("pub struct SemanticLocalMaterialization")
        .nth(1)
        .and_then(|source| source.split("\n}").next())
        .expect("local materialization declaration");

    for owned in [
        "pub air: crate::ValidatedAir",
        "pub type_pool: crate::FrozenTypeInternPool",
        "pub interner: Arc<ThreadedRodeo>",
        "aggregate_types: ahash::AHashMap<",
        "pub strings: Vec<String>",
        "pub body_span: Span",
        "pub completeness: SemanticLocalCompleteness",
    ] {
        assert!(
            materialization.contains(owned),
            "local semantic materialization lost owned CFG input: {owned}"
        );
    }
    let aggregate_accessors = import
        .split("impl<K, M> SemanticLocalMaterialization<K, M>")
        .nth(1)
        .and_then(|source| source.split("/// An AIR type branded").next())
        .expect("local materialization accessor implementation");
    for accessor in [
        "pub fn aggregate_type(",
        "pub fn has_aggregate_type(",
        "pub fn aggregate_type_count(",
        "pub fn aggregate_type_entries(",
    ] {
        assert!(
            aggregate_accessors.contains(accessor),
            "local materialization lost accessor: {accessor}"
        );
    }
    assert!(
        !materialization.contains("pub aggregate_types:"),
        "local materialization leaked its aggregate map"
    );
    assert!(import.contains("pub fn new_local("));
    assert!(import.contains("pub fn materialize_local_body("));
    assert!(import.contains("local_completeness: Option<SemanticLocalCompleteness>"));
    let materialize_signature = import
        .split("pub fn materialize_local_body(")
        .nth(1)
        .and_then(|source| source.split(") -> Result").next())
        .expect("local materialization signature");
    assert!(
        !materialize_signature.contains("completeness:"),
        "a caller-provided completeness witness could be crossed between epochs"
    );
    assert!(import.contains("NominalInstanceKey::Anonymous"));
    assert!(import.contains("SemanticImportFailure::BuiltinNominalShadow"));
    assert!(import.contains("specialization_key(specialization)"));
    assert!(import.contains("self.type_pool.complete_type_handles()"));
    assert!(
        !import.contains("self.type_pool.clone().freeze()"),
        "local materialization must not deep-clone the semantic type universe before publication"
    );
    for provider in ["ProviderSpecializedBody", "ProviderAnonymousBody"] {
        let body = providers
            .split(&format!("pub struct {provider}"))
            .nth(1)
            .and_then(|source| source.split("\n}").next())
            .expect("provider result declaration");
        assert!(body.contains("pub function: AnalyzedFunction"));
        assert!(body.contains("pub type_pool: Rc<TypeInternPool>"));
        // The symbol interner is revision-shared and crosses worker threads
        // (ADR-0076); the type pool stays body-private and `Rc`.
        assert!(body.contains("pub interner: Arc<ThreadedRodeo>"));
    }
    for forbidden in ["Mutex<", "RwLock<", "QueryFamily<", "cache:", "selected:"] {
        assert!(
            !materialization.contains(forbidden),
            "body-local materialization became peer state: {forbidden}"
        );
    }
}

#[test]
fn semantic_hash_boundaries_pin_only_their_exact_authorities() {
    fn region<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        source
            .split(start)
            .nth(1)
            .and_then(|source| source.split(end).next())
            .unwrap_or_else(|| panic!("missing guarded source region {start:?}..{end:?}"))
    }

    // 1. Expression inference is an AHash lookup table, while the production
    // allocation call site imposes RIR order before creating array types.
    let generate = include_str!("inference/generate.rs");
    let generator = region(
        generate,
        "pub struct ConstraintGenerator<'a>",
        "impl<'a> ConstraintGenerator<'a>",
    );
    assert!(generator.contains("expr_types: ahash::AHashMap<InstRef, InferType>"));
    assert!(!generator.contains("std::collections::HashMap"));
    let expr_api = region(
        generate,
        "/// Get the expression type mapping.",
        "/// Enter a lexical scope:",
    );
    assert!(expr_api.contains("pub fn expr_types(&self) -> &ahash::AHashMap<InstRef, InferType>"));
    assert!(expr_api.contains("ahash::AHashMap<InstRef, InferType>,"));
    assert!(!expr_api.contains("std::collections::HashMap"));
    let type_inference = include_str!("sema/analysis/type_inference.rs");
    let array_allocation = region(
        type_inference,
        "// Pre-collect all array types from resolved InferTypes",
        "let unification_resolution_ns",
    );
    let ordered = array_allocation
        .find("expr_types_in_rir_order(&expr_types)")
        .expect("production expression ordering call");
    let precreate = array_allocation
        .find("self.pre_create_array_types_from_infer_type(&resolved)")
        .expect("production array precreation call");
    assert!(ordered < precreate);
    assert!(!array_allocation.contains("for (_, infer_ty) in &expr_types"));

    // 2. Method reachability stays request-local AHash state through analysis
    // and export; the imported body intentionally exposes no live-key set.
    let context = include_str!("sema/context.rs");
    let analysis_context = region(
        context,
        "pub(crate) struct AnalysisContext<'a>",
        "impl<'a> AnalysisContext<'a>",
    );
    assert!(
        analysis_context
            .contains("pub(crate) referenced_methods: ahash::AHashSet<(StructId, Spur)>")
    );
    assert!(!analysis_context.contains("std::collections::HashSet"));
    let export = include_str!("sema/semantic_body_export.rs");
    let export_signature = region(export, "pub(crate) fn export_body", ") -> Result");
    assert!(
        export_signature.contains("method_references: &ahash::AHashSet<(crate::StructId, Spur)>")
    );
    assert!(!export_signature.contains("std::collections::HashSet"));
    let specialized = include_str!("specialize.rs");
    let specialized_body = region(specialized, "pub(crate) struct OneSpecializedBody", "\n}");
    assert!(
        specialized_body
            .contains("pub(crate) referenced_methods: ahash::AHashSet<(StructId, Spur)>")
    );
    let provider = include_str!("sema/provider_body_host.rs");
    let provider_body = region(provider, "pub struct ProviderOrdinaryBody", "\n}");
    assert!(!provider_body.contains("referenced_methods"));
    let semantic_body = include_str!("semantic_body.rs");
    let imported_body = region(
        semantic_body,
        "pub struct SemanticImportedBody<K, M>",
        "\n}",
    );
    assert!(!imported_body.contains("method_references"));
    // 3-4. The two anonymous live-type registries use AHash only as lookup
    // storage. Their ordering/selection helpers cannot consult a live pool id.
    let provider_host = region(
        provider,
        "struct ProviderBodyHost<'a, P, S, K, M>",
        "impl<'a, P, S, K, M> ProviderBodyHost",
    );
    assert!(provider_host.contains(
        "canonical_anonymous_types:\n        ahash::AHashMap<Type, super::anon_structs::IssuedAnonymousNominalKey>"
    ));
    assert!(provider_host.contains(
        "RefCell<ahash::AHashMap<Type, super::anon_structs::IssuedAnonymousNominalKey>>"
    ));
    assert!(!provider_host.contains("std::collections::HashMap"));
    let anonymous_helpers = include_str!("sema/anon_structs.rs");
    let export_order = region(
        anonymous_helpers,
        "fn anonymous_export_cmp",
        "/// Project live anonymous entries",
    );
    let consulted_selection = region(
        anonymous_helpers,
        "pub(crate) fn canonical_consulted_type",
        "#[cfg(test)]",
    );
    assert!(!export_order.contains("as_u32"));
    assert!(!consulted_selection.contains("as_u32"));

    // 5. CFG materialization owns a private AHash table and exports only
    // hasher-neutral point/size/ordered-entry accessors.
    let import = include_str!("semantic_import.rs");
    let materialization = region(
        import,
        "pub struct SemanticLocalMaterialization<K, M>",
        "\n}",
    );
    assert!(
        materialization
            .contains("aggregate_types: ahash::AHashMap<crate::Type, TypeInstanceKey<K, M>>")
    );
    assert!(!materialization.contains("pub aggregate_types:"));
    assert!(!materialization.contains("std::collections::HashMap"));
}

#[test]
fn semantic_definition_taxonomy_has_one_enum_declaration() {
    let canonical = include_str!("semantic_identity.rs");
    let bindings = include_str!("sema/binding_manifest.rs");
    let bodies = include_str!("semantic_body.rs");

    assert!(canonical.contains("macro_rules! stable_definition_kind_schema"));
    assert_eq!(
        canonical.matches("pub enum StableDefinitionKind").count(),
        1
    );
    assert_eq!(
        canonical
            .matches("pub enum StableDefinitionNamespace")
            .count(),
        1
    );
    assert!(!bindings.contains("pub enum StableDefinitionKind"));
    assert!(!bindings.contains("pub enum StableDefinitionNamespace"));
    assert!(!bodies.contains("pub enum StableDefinitionKind"));
}

#[test]
fn retired_source_owned_sema_plane_cannot_return() {
    // RUE-1538: the source-owned `Sema` declaration/body plane is deleted.
    // Body analysis has exactly one production shape: the compiler's query
    // graph supplies durable facts through `BodyFactProvider`, and the
    // provider host drives the shared engine over one demanded body. This
    // gate holds the deletion closed three ways.
    //
    // (a) No retired driver, phase type, or install entry point may reappear
    // anywhere in AIR's production sources. Needles are spelled with
    // `concat!` so the gate does not match itself; `_tests.rs` files and the
    // test fixture are excluded because they spell needle fragments while
    // documenting the migration.
    let production = [
        include_str!("lib.rs"),
        include_str!("specialize.rs"),
        include_str!("sema/aggregate_resolution.rs"),
        include_str!("sema/aggregates.rs"),
        include_str!("sema/analysis.rs"),
        include_str!("sema/analysis/builtin_ops.rs"),
        include_str!("sema/analysis/calls.rs"),
        include_str!("sema/analysis/instructions.rs"),
        include_str!("sema/analysis/intrinsics.rs"),
        include_str!("sema/analysis/ownership.rs"),
        include_str!("sema/analysis/pointers.rs"),
        include_str!("sema/analysis/type_inference.rs"),
        include_str!("sema/analyze_ops.rs"),
        include_str!("sema/anon_structs.rs"),
        include_str!("sema/binding_manifest.rs"),
        include_str!("sema/body_endpoint.rs"),
        include_str!("sema/body_identity.rs"),
        include_str!("sema/call_resolution.rs"),
        include_str!("sema/comptime_eval.rs"),
        include_str!("sema/context.rs"),
        include_str!("sema/control_flow.rs"),
        include_str!("sema/declaration_index.rs"),
        include_str!("sema/declarations.rs"),
        include_str!("sema/fact_mode.rs"),
        include_str!("sema/inference_ctx.rs"),
        include_str!("sema/info.rs"),
        include_str!("sema/known_symbols.rs"),
        include_str!("sema/mod.rs"),
        include_str!("sema/ordinary_engine.rs"),
        include_str!("sema/output.rs"),
        include_str!("sema/provider.rs"),
        include_str!("sema/provider_body_host.rs"),
        include_str!("sema/provider_module_registry.rs"),
        include_str!("sema/semantic_body_export.rs"),
        include_str!("sema/typeck.rs"),
        include_str!("sema/visibility.rs"),
    ]
    .concat();
    for retired in [
        concat!("new_", "synthetic"),
        concat!("bind_", "declarations"),
        concat!("analyze_all", "_for_test"),
        concat!("analyze_all", "_bodies"),
        concat!("analyze_all", "_function_bodies"),
        concat!("predeclare_", "declaration_shells"),
        concat!("analyze_function_bodies", "_lazy"),
        concat!("compose_", "queried_bodies"),
        concat!("resolve_", "declarations"),
        concat!("install_", "declaration_semantics"),
        concat!("install_", "ordinary_body_candidates"),
        concat!("install_", "stable_identity_endpoints"),
        concat!("install_", "body_owner_tokens"),
        concat!("run_", "to_fixpoint"),
        concat!("create_", "specialized_function"),
        concat!("Bound", "Sema"),
        concat!("Body", "Sema"),
        concat!("Sema", "<"),
        concat!("Declaration", "Shells"),
        concat!("Declaration", "Phase"),
        concat!("Mutable", "Declarations"),
        concat!("Source", "Declarations"),
        concat!("Speci", "alizer"),
    ] {
        assert!(
            !production.contains(retired),
            "retired source-owned Sema plane returned: {retired}"
        );
    }

    // (b) Exactly one implementation of the body-analysis host contract
    // exists, and it is the provider host. A renamed source-owned host
    // cannot reappear without tripping this count.
    assert_eq!(
        production.matches("OrdinaryBodyAnalysisHost for").count(),
        1,
        "body analysis must have exactly one host implementation"
    );
    let provider_host = include_str!("sema/provider_body_host.rs");
    assert!(
        provider_host.contains("OrdinaryBodyAnalysisHost for ProviderBodyHost<"),
        "the one host implementation must be the provider host"
    );

    // (c) The production ordinary-body entry point exists and drives the
    // shared engine.
    assert!(
        provider_host.contains("pub fn analyze_provider_ordinary_body"),
        "the provider ordinary-body entry point is the production driver"
    );
    assert!(
        provider_host.contains("OrdinaryBodyEngine::new"),
        "the provider host must construct the shared body engine"
    );
}

#[test]
fn sema_diagnostics_use_the_friendly_type_display_authority() {
    // Diagnostic payloads in the ordinary body engine must go through its
    // presentation authority. This is the complete production sema inventory
    // (the same Buck-generated manifest used by the broader inventory test),
    // split below only to make the presentation boundary auditable. The
    // non-presentation half is checked for raw captures, so adding a new sema
    // module cannot silently evade this guard.
    let all_sema_sources = [
        (
            "sema/aggregate_resolution",
            include_str!("sema/aggregate_resolution.rs"),
        ),
        ("sema/aggregates", include_str!("sema/aggregates.rs")),
        ("sema/analysis", include_str!("sema/analysis.rs")),
        (
            "sema/analysis/builtin_ops",
            include_str!("sema/analysis/builtin_ops.rs"),
        ),
        (
            "sema/analysis/calls",
            include_str!("sema/analysis/calls.rs"),
        ),
        (
            "sema/analysis/instructions",
            include_str!("sema/analysis/instructions.rs"),
        ),
        (
            "sema/analysis/intrinsics",
            include_str!("sema/analysis/intrinsics.rs"),
        ),
        (
            "sema/analysis/ownership",
            include_str!("sema/analysis/ownership.rs"),
        ),
        (
            "sema/analysis/pointers",
            include_str!("sema/analysis/pointers.rs"),
        ),
        (
            "sema/analysis/type_inference",
            include_str!("sema/analysis/type_inference.rs"),
        ),
        ("sema/analyze_ops", include_str!("sema/analyze_ops.rs")),
        ("sema/anon_structs", include_str!("sema/anon_structs.rs")),
        (
            "sema/binding_manifest",
            include_str!("sema/binding_manifest.rs"),
        ),
        ("sema/body_endpoint", include_str!("sema/body_endpoint.rs")),
        ("sema/body_identity", include_str!("sema/body_identity.rs")),
        (
            "sema/call_resolution",
            include_str!("sema/call_resolution.rs"),
        ),
        ("sema/comptime", include_str!("sema/comptime.rs")),
        (
            "sema/comptime/frames",
            include_str!("sema/comptime/frames.rs"),
        ),
        (
            "sema/comptime/intrinsics",
            include_str!("sema/comptime/intrinsics.rs"),
        ),
        (
            "sema/comptime/model",
            include_str!("sema/comptime/model.rs"),
        ),
        (
            "sema/comptime/registry",
            include_str!("sema/comptime/registry.rs"),
        ),
        (
            "sema/comptime/sites",
            include_str!("sema/comptime/sites.rs"),
        ),
        (
            "sema/comptime/structured_type",
            include_str!("sema/comptime/structured_type.rs"),
        ),
        (
            "sema/comptime/value_domain_tests",
            include_str!("sema/comptime/value_domain_tests.rs"),
        ),
        ("sema/comptime_eval", include_str!("sema/comptime_eval.rs")),
        ("sema/context", include_str!("sema/context.rs")),
        ("sema/control_flow", include_str!("sema/control_flow.rs")),
        (
            "sema/declaration_index",
            include_str!("sema/declaration_index.rs"),
        ),
        ("sema/declarations", include_str!("sema/declarations.rs")),
        ("sema/fact_mode", include_str!("sema/fact_mode.rs")),
        ("sema/inference_ctx", include_str!("sema/inference_ctx.rs")),
        ("sema/info", include_str!("sema/info.rs")),
        ("sema/known_symbols", include_str!("sema/known_symbols.rs")),
        ("sema/mod", include_str!("sema/mod.rs")),
        (
            "sema/ordinary_engine",
            include_str!("sema/ordinary_engine.rs"),
        ),
        ("sema/output", include_str!("sema/output.rs")),
        (
            "sema/ownership_state",
            include_str!("sema/ownership_state.rs"),
        ),
        ("sema/provider", include_str!("sema/provider.rs")),
        (
            "sema/provider_body_host",
            include_str!("sema/provider_body_host.rs"),
        ),
        (
            "sema/provider_module_registry",
            include_str!("sema/provider_module_registry.rs"),
        ),
        (
            "sema/semantic_body_export",
            include_str!("sema/semantic_body_export.rs"),
        ),
        ("sema/typeck", include_str!("sema/typeck.rs")),
        ("sema/visibility", include_str!("sema/visibility.rs")),
    ];
    let manifest = include_str!("rue_air_source_manifest.txt");
    let test_only_sema_modules = [
        "sema/consistency_tests",
        "sema/provider_accessor_tests",
        "sema/provider_fixture",
        "sema/provider_fixture_tests",
        "sema/provider_semantics_tests",
        "sema/provider_strings_ownership_tests",
        "sema/tests",
    ];
    let manifest_sema_modules = manifest
        .lines()
        .map(|path| path.trim_start_matches("./").trim_end_matches(".rs"))
        .map(|path| path.trim_start_matches("src/"))
        .filter(|path| path.starts_with("sema/"))
        .filter(|path| !test_only_sema_modules.contains(path))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let inventoried_sema_modules = all_sema_sources
        .iter()
        .map(|(module, _)| module.to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        manifest_sema_modules, inventoried_sema_modules,
        "friendly-display inventory must cover every production sema source in Buck's manifest"
    );
    let presentation_modules = [
        "sema/aggregates",
        "sema/analysis",
        "sema/analysis/builtin_ops",
        "sema/analysis/calls",
        "sema/analysis/instructions",
        "sema/analysis/intrinsics",
        "sema/analysis/ownership",
        "sema/analysis/pointers",
        "sema/analysis/type_inference",
        "sema/analyze_ops",
        "sema/comptime_eval",
        "sema/control_flow",
        "sema/typeck",
    ];
    for (module, source) in &all_sema_sources {
        if presentation_modules.contains(module)
            || matches!(*module, "sema/provider_body_host" | "sema/ordinary_engine")
        {
            continue;
        }
        assert!(
            !source.contains("safe_name_with_pool(") && !source.contains("name_with_pool("),
            "non-presentation sema module captured a raw type name: {module}"
        );
    }

    // Keep the raw pool renderer available only to the authority itself and
    // provider-owned identity/symbol paths; a new direct capture in one of
    // the presentation modules is an accidental bypass that would leak
    // anonymous nominal names again.
    let presentation_sources = [
        include_str!("sema/aggregates.rs"),
        include_str!("sema/analysis.rs"),
        include_str!("sema/analysis/builtin_ops.rs"),
        include_str!("sema/analysis/calls.rs"),
        include_str!("sema/analysis/instructions.rs"),
        include_str!("sema/analysis/intrinsics.rs"),
        include_str!("sema/analysis/ownership.rs"),
        include_str!("sema/analysis/pointers.rs"),
        include_str!("sema/analysis/type_inference.rs"),
        include_str!("sema/analyze_ops.rs"),
        include_str!("sema/comptime_eval.rs"),
        include_str!("sema/control_flow.rs"),
        include_str!("sema/typeck.rs"),
    ]
    .concat();
    let mut presentation_code = String::with_capacity(presentation_sources.len());
    let mut chars = presentation_sources.chars().peekable();
    let mut in_line_comment = false;
    let mut block_comment_depth = 0usize;
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
                presentation_code.push(ch);
            }
            continue;
        }
        if block_comment_depth > 0 {
            if ch == '/' && chars.peek() == Some(&'*') {
                chars.next();
                block_comment_depth += 1;
            } else if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_comment_depth -= 1;
            }
            continue;
        }
        if in_string {
            if ch == '\\' {
                chars.next();
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            in_line_comment = true;
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            block_comment_depth = 1;
        } else if ch == '"' {
            in_string = true;
        } else {
            presentation_code.push(ch);
        }
    }
    assert!(presentation_code.contains("format_type_name"));
    assert!(
        !presentation_code.contains("safe_name_with_pool(")
            && !presentation_code.contains("name_with_pool("),
        "a sema diagnostic captured a raw type name"
    );
    assert!(!presentation_code.contains("struct_def.name"));
    assert!(!presentation_code.contains("enum_def.name"));
    let ordinary = include_str!("sema/ordinary_engine.rs");
    let provider = include_str!("sema/provider_body_host.rs");
    assert!(ordinary.contains("self.storage.friendly_type_display(ty)"));
    assert!(
        ordinary.contains("fn format_internal_type_name")
            && ordinary.contains("ty.safe_name_with_pool")
    );
    assert!(provider.contains("fn friendly_type_display(&self, ty: Type) -> String"));
    assert!(provider.contains("ctor_displays.get(&ty)"));
    assert!(provider.contains("crate::format_canonical_application("));
    let identity = include_str!("semantic_identity.rs");
    assert_eq!(
        identity
            .matches("pub fn format_canonical_application<")
            .count(),
        1,
        "canonical comptime display interleaving must have one AIR owner"
    );
}

#[test]
fn intrinsic_semantics_have_one_typed_authority_across_sema_and_durable_air() {
    let known = include_str!("sema/known_symbols.rs");
    let analysis = include_str!("sema/analysis/intrinsics.rs");
    let pointers = include_str!("sema/analysis/pointers.rs");
    let ownership = include_str!("sema/analysis/ownership.rs");
    let air = include_str!("inst.rs");
    let durable = include_str!("semantic_body.rs");
    let export = include_str!("sema/semantic_body_export.rs");
    let import = include_str!("semantic_import.rs");

    assert_eq!(
        known
            .matches("pub fn get_parse_intrinsic_operation(")
            .count(),
        1,
        "parse intrinsic symbols must have one typed classifier"
    );
    assert_eq!(
        analysis
            .matches("known.get_parse_intrinsic_operation(name)")
            .count(),
        1,
        "sema must consume the one parse classifier exactly once"
    );
    assert!(analysis.contains("\"arg_ptr\",\n                crate::IntrinsicOperation::ArgPtr"));
    assert!(analysis.contains("\"env_ptr\",\n                crate::IntrinsicOperation::EnvPtr"));

    assert!(air.contains(
        "Intrinsic {\n        /// Typed intrinsic semantics selected by semantic analysis.\n        operation: crate::IntrinsicOperation,"
    ));
    assert!(durable.contains(
        "Intrinsic {\n        operation: crate::IntrinsicOperation,\n        name: Arc<str>,"
    ));
    assert!(durable.contains(
        "D::Intrinsic {\n                operation,\n                name,\n                args,\n            } => D::Intrinsic {\n                operation: *operation,"
    ));
    assert!(export.contains("operation: *operation"));
    assert!(import.contains("operation.validate_call(type_pool, &arguments, ty)"));
    assert!(import.contains("source_name != operation.expected_spelling()"));
    assert!(
        import
            .find("operation.validate_call(type_pool, &arguments, ty)")
            .unwrap()
            < import.find("let name = intern(source_name)?").unwrap(),
        "durable import must validate before publishing the diagnostic symbol"
    );

    let empty_slice = ownership
        .split("if arr_len == 0 {")
        .nth(1)
        .and_then(|source| source.split("} else {").next())
        .expect("empty-slice lowering branch");
    assert!(empty_slice.contains("data: AirInstData::Const(0)"));
    assert!(empty_slice.contains("ty: ptr_ty"));
    assert!(!empty_slice.contains("IntrinsicOperation::IntToPtr"));

    for (name, source) in [
        ("known symbols", known),
        ("sema intrinsic analysis", analysis),
        ("sema pointer analysis", pointers),
        ("sema ownership analysis", ownership),
        ("AIR", air),
        ("durable AIR", durable),
        ("durable export", export),
        ("durable import", import),
    ] {
        for forbidden in [
            "IntrinsicSelector",
            "resolve_intrinsic_operation",
            "intrinsic_operation_from_name",
            "operation_from_name",
            "expect(\"parse intrinsic",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} contains a second intrinsic selection path: {forbidden}"
            );
        }
    }
}

#[test]
fn copy_semantics_have_one_air_policy_owner() {
    let types = include_str!("types.rs");
    let pool = include_str!("intern_pool.rs");
    let import = include_str!("semantic_import.rs");
    let ordinary = include_str!("sema/ordinary_engine.rs");
    let identity = include_str!("sema/body_identity.rs");

    assert!(pool.contains("fn is_copy_type_inner("));
    assert!(
        !types.contains("pub fn is_copy(&self)"),
        "a pool-free Type::is_copy would be an incomplete composite policy"
    );
    assert!(ordinary.contains("ty.is_copy_in_pool(self.body_type_pool())"));
    assert!(identity.contains("ty.is_copy_in_pool(pool)"));
    assert!(import.contains("matches!(key, NominalInstanceKey::Anonymous(_))"));
    assert!(import.contains("field.ty.is_copy_in_pool(type_pool)"));
    assert!(
        !import.contains("NominalInstanceKey::Anonymous(_) => false"),
        "anonymous import completion must not restore a hardcoded non-Copy policy"
    );
    assert_eq!(
        ordinary.matches("fn is_type_copy(&self, ty: Type)").count(),
        1,
        "ordinary ownership must retain only its thin pool delegate"
    );
}

#[test]
fn drop_glue_semantics_have_one_air_policy_owner() {
    let policy = include_str!("drop_glue.rs");
    let pool = include_str!("intern_pool.rs");
    let provider_host = include_str!("sema/provider_body_host.rs");
    let ordinary = include_str!("sema/ordinary_engine.rs");
    let identity = include_str!("sema/body_identity.rs");

    assert_eq!(policy.matches("pub fn requires_drop_glue(").count(), 1);
    assert_eq!(policy.matches("pub fn is_anonymous_destructor(").count(), 1);
    assert_eq!(pool.matches("drop_glue::requires_drop_glue(").count(), 2);
    assert!(!pool.contains("needs_drop |= facts[child].needs_drop"));
    assert_eq!(
        provider_host
            .matches("drop_glue::is_anonymous_destructor(")
            .count(),
        4
    );
    assert!(!provider_host.contains("name.as_ref() == \"__drop\""));
    assert!(!provider_host.contains("resolve(name) == \"__drop\""));
    assert_eq!(
        ordinary
            .matches("drop_glue::is_anonymous_destructor(")
            .count(),
        1,
        "ordinary anonymous materialization must use the canonical predicate"
    );
    assert_eq!(
        identity
            .matches("drop_glue::is_anonymous_destructor(")
            .count(),
        1,
        "durable anonymous materialization must use the canonical predicate"
    );
    assert!(identity.contains("struct_methods: Vec<(Arc<str>, bool)>,"));
    for (name, source) in [("ordinary engine", ordinary), ("body identity", identity)] {
        assert!(!source.contains("== ANON_DROP_METHOD"), "{name}");
        assert!(!source.contains("== drop_marker"), "{name}");
        assert!(!source.contains("== \"__drop\""), "{name}");
    }
}
