use super::*;

/// The items of a module body: a file, `mod m { .. }`, an `impl` or `trait`
/// body, or an `extern` block. The grammar accepts any item in any of them;
/// which items are allowed where is a semantic check.
pub(super) fn mod_contents(p: &mut Parser<'_, '_>) {
    while !p.at_eof() {
        let start = p.pos();
        item_or_macro(p);
        if p.pos() == start {
            p.err_and_bump("expected an item");
        }
    }
}

fn item_or_macro(p: &mut Parser<'_, '_>) {
    let m = p.start();
    attributes::outer_attrs(p);
    let m = match opt_item(p, m) {
        Ok(()) => return,
        Err(m) => m,
    };
    if paths::is_use_path_start(p) {
        macro_call(p, m);
        return;
    }
    m.abandon(p);
    if p.at_eof() {
        p.error("expected an item after attributes");
    } else {
        p.err_and_bump("expected an item");
    }
}

/// An item-position macro invocation: `m!(..);`, `m![..];` or `m! { .. }`.
fn macro_call(p: &mut Parser<'_, '_>, m: Marker) {
    paths::use_path(p);
    if !p.eat(T![!]) {
        p.error("expected `!` or an item");
        m.complete(p, ERROR);
        return;
    }
    match p.current() {
        T!['{'] => token_tree_deferrable(p),
        T!['('] | T!['['] => {
            token_tree_deferrable(p);
            p.expect(T![;]);
        }
        _ => p.error("expected a macro argument list"),
    }
    m.complete(p, MACRO_CALL);
}

/// Parses an item into `m` if one starts here; otherwise hands `m` back
/// untouched (apart from any outer attributes already in it).
pub(super) fn opt_item(p: &mut Parser<'_, '_>, m: Marker) -> Result<(), Marker> {
    let has_visibility = opt_visibility(p);

    // Qualifiers, in the order Rust requires them.
    let mut has_qualifiers = false;
    let mut has_extern = false;
    if p.at_contextual_kw(T![default])
        && (matches!(
            p.nth(1),
            T![fn] | T![type] | T![const] | T![impl] | T![unsafe] | T![async] | T![extern]
        ) || p.nth_at_contextual_kw(1, T![safe]))
    {
        p.bump_remap(T![default]);
        has_qualifiers = true;
    }
    if p.at(T![const])
        && matches!(
            p.nth(1),
            T![fn] | T![unsafe] | T![async] | T![extern] | T![impl] | T![trait]
        )
    {
        p.bump(T![const]);
        has_qualifiers = true;
    }
    if p.at(T![async])
        && (matches!(p.nth(1), T![fn] | T![unsafe] | T![extern])
            || (p.nth_at_contextual_kw(1, T![gen])
                && matches!(p.nth(2), T![fn] | T![unsafe] | T![extern])))
    {
        p.bump(T![async]);
        has_qualifiers = true;
    }
    if p.at_contextual_kw(T![gen]) && matches!(p.nth(1), T![fn] | T![unsafe] | T![extern]) {
        p.bump_remap(T![gen]);
        has_qualifiers = true;
    }
    if p.at(T![unsafe]) && p.nth(1) != T!['{'] {
        p.bump(T![unsafe]);
        has_qualifiers = true;
    }
    if p.at_contextual_kw(T![safe]) && matches!(p.nth(1), T![fn] | T![static] | T![extern]) {
        p.bump_remap(T![safe]);
        has_qualifiers = true;
    }
    if p.at(T![extern]) && p.nth(1) != T![crate] {
        abi(p);
        has_qualifiers = true;
        has_extern = true;
    }
    if p.at_contextual_kw(T![auto]) && p.nth(1) == T![trait] {
        p.bump_remap(T![auto]);
        has_qualifiers = true;
    }

    match p.current() {
        T![fn] => fn_(p, m),
        T![trait] => trait_(p, m),
        T![impl] => impl_(p, m),
        T!['{'] if has_extern => {
            p.defer_or(EXTERN_ITEM_LIST, extern_item_list);
            m.complete(p, EXTERN_BLOCK);
        }
        T![static] if has_qualifiers => static_(p, m),
        T![mod] if has_qualifiers => module(p, m),
        // `default type` and `default const` in impls.
        T![type] => type_alias(p, m),
        T![const] if matches!(p.nth(1), IDENT | T![_]) => const_(p, m),
        _ if has_qualifiers => {
            p.error("expected `fn`, `trait`, `impl` or an extern block");
            m.complete(p, ERROR);
        }
        T![use] => use_item(p, m),
        T![struct] => struct_or_union(p, m, T![struct]),
        IDENT if p.at_contextual_kw(T![union]) && p.nth(1) == IDENT => {
            struct_or_union(p, m, T![union])
        }
        T![enum] => enum_(p, m),
        T![static] if matches!(p.nth(1), IDENT | T![mut] | T![_]) => static_(p, m),
        T![mod] => module(p, m),
        T![extern] => extern_crate(p, m),
        IDENT if p.at_contextual_kw(T![macro_rules]) && p.nth(1) == T![!] && p.nth(2) == IDENT => {
            macro_rules(p, m)
        }
        T![macro] => macro_def(p, m),
        _ if has_visibility => {
            p.error("expected an item after the visibility");
            m.complete(p, ERROR);
        }
        _ => return Err(m),
    }
    Ok(())
}

fn fn_(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![fn]);
    name_r(p, TokenSet::new(&[T!['('], T!['{'], T![<]]));
    generics::opt_generic_param_list(p);
    if p.at(T!['(']) {
        params::param_list_fn_def(p);
    } else {
        p.error("expected function parameters");
    }
    opt_ret_type(p);
    generics::opt_where_clause(p);
    if p.at(T![;]) {
        p.bump(T![;]);
    } else if p.at(T!['{']) {
        expressions::block_expr_deferrable(p);
    } else {
        p.error("expected a function body or `;`");
    }
    m.complete(p, FN);
}

fn struct_or_union(p: &mut Parser<'_, '_>, m: Marker, kw: SyntaxKind) {
    if kw == T![union] {
        p.bump_remap(T![union]);
    } else {
        p.bump(T![struct]);
    }
    name_r(p, TokenSet::new(&[T!['{'], T!['('], T![<], T![;]]));
    generics::opt_generic_param_list(p);
    match p.current() {
        T![where] => {
            generics::opt_where_clause(p);
            match p.current() {
                T![;] => p.bump(T![;]),
                T!['{'] => record_field_list(p),
                _ => p.error("expected `;` or `{`"),
            }
        }
        T![;] => p.bump(T![;]),
        T!['{'] => record_field_list(p),
        T!['('] if kw == T![struct] => {
            tuple_field_list(p);
            generics::opt_where_clause(p);
            p.expect(T![;]);
        }
        _ => p.error("expected `;`, `{` or `(`"),
    }
    m.complete(p, if kw == T![union] { UNION } else { STRUCT });
}

pub(super) fn record_field_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        comma_separated(p, |p| {
            let field = p.start();
            attributes::outer_attrs(p);
            opt_visibility(p);
            if p.at(IDENT) {
                name(p);
                p.expect(T![:]);
                types::type_(p);
                if p.eat(T![=]) {
                    expressions::expr(p);
                }
                field.complete(p, RECORD_FIELD);
            } else {
                field.abandon(p);
                p.err_and_bump("expected a field");
            }
        })
    });
    m.complete(p, RECORD_FIELD_LIST);
}

fn tuple_field_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        comma_separated(p, |p| {
            let field = p.start();
            attributes::outer_attrs(p);
            opt_visibility(p);
            if p.at_ts(types::TYPE_FIRST) {
                types::type_(p);
                field.complete(p, TUPLE_FIELD);
            } else {
                field.abandon(p);
                p.err_and_bump("expected a field type");
            }
        })
    });
    m.complete(p, TUPLE_FIELD_LIST);
}

fn enum_(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![enum]);
    name_r(p, TokenSet::new(&[T!['{'], T![<]]));
    generics::opt_generic_param_list(p);
    generics::opt_where_clause(p);
    if p.at(T!['{']) {
        variant_list(p);
    } else {
        p.error("expected `{`");
    }
    m.complete(p, ENUM);
}

pub(super) fn variant_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        comma_separated(p, |p| {
            let variant = p.start();
            attributes::outer_attrs(p);
            opt_visibility(p);
            if p.at(IDENT) {
                name(p);
                match p.current() {
                    T!['{'] => record_field_list(p),
                    T!['('] => tuple_field_list(p),
                    _ => {}
                }
                if p.eat(T![=]) {
                    expressions::expr(p);
                }
                variant.complete(p, VARIANT);
            } else {
                variant.abandon(p);
                p.err_and_bump("expected a variant");
            }
        })
    });
    m.complete(p, VARIANT_LIST);
}

fn trait_(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![trait]);
    name_r(p, TokenSet::new(&[T!['{'], T![<], T![:]]));
    generics::opt_generic_param_list(p);
    if p.at(T![=]) {
        // A trait alias: `trait A = B + C;`.
        p.bump(T![=]);
        generics::bounds_without_colon(p);
        generics::opt_where_clause(p);
        p.expect(T![;]);
        m.complete(p, TRAIT_ALIAS);
        return;
    }
    if p.at(T![:]) {
        generics::bounds(p);
    }
    generics::opt_where_clause(p);
    if p.at(T!['{']) {
        p.defer_or(ASSOC_ITEM_LIST, assoc_item_list);
    } else {
        p.error("expected `{`");
    }
    m.complete(p, TRAIT);
}

fn impl_(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![impl]);
    if p.at(T![<]) && not_a_qualified_path(p) {
        generics::generic_param_list(p);
    }
    p.eat(T![const]);
    // `impl !Send for T` is a negative impl; `impl ! {}` is an inherent impl
    // on the never type.
    if p.at(T![!])
        && matches!(
            p.nth(1),
            IDENT | T![:] | T![<] | T![crate] | T![self] | T![super]
        )
    {
        p.bump(T![!]);
    }
    // Bounds are allowed on the self type: `impl Trait for dyn A + 'a`.
    types::type_(p);
    if p.eat(T![for]) {
        types::type_(p);
    }
    generics::opt_where_clause(p);
    if p.at(T!['{']) {
        p.defer_or(ASSOC_ITEM_LIST, assoc_item_list);
    } else {
        p.error("expected `{`");
    }
    m.complete(p, IMPL);
}

/// After `impl`, `<` starts either generic parameters or a qualified path
/// (`impl <T as Trait>::Assoc {}`). These shapes can only be parameters.
fn not_a_qualified_path(p: &Parser<'_, '_>) -> bool {
    if matches!(p.nth(1), T![#] | T![>] | T![const]) {
        return true;
    }
    matches!(p.nth(1), LIFETIME_IDENT | IDENT) && matches!(p.nth(2), T![>] | T![,] | T![:] | T![=])
}

pub(super) fn assoc_item_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        attributes::inner_attrs(p);
        mod_contents(p);
    });
    m.complete(p, ASSOC_ITEM_LIST);
}

pub(super) fn item_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        attributes::inner_attrs(p);
        mod_contents(p);
    });
    m.complete(p, ITEM_LIST);
}

pub(super) fn extern_item_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        attributes::inner_attrs(p);
        mod_contents(p);
    });
    m.complete(p, EXTERN_ITEM_LIST);
}

fn module(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![mod]);
    name_r(p, TokenSet::new(&[T![;], T!['{']]));
    if p.at(T![;]) {
        p.bump(T![;]);
    } else if p.at(T!['{']) {
        p.defer_or(ITEM_LIST, item_list);
    } else {
        p.error("expected `;` or `{`");
    }
    m.complete(p, MODULE);
}

fn type_alias(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![type]);
    name_r(p, TokenSet::new(&[T![=], T![;], T![<], T![:]]));
    generics::opt_generic_param_list(p);
    if p.at(T![:]) {
        generics::bounds(p);
    }
    generics::opt_where_clause(p);
    if p.eat(T![=]) {
        types::type_(p);
    }
    generics::opt_where_clause(p);
    p.expect(T![;]);
    m.complete(p, TYPE_ALIAS);
}

fn const_(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![const]);
    if !p.eat(T![_]) {
        name_r(p, TokenSet::new(&[T![:], T![=], T![;]]));
    }
    generics::opt_generic_param_list(p);
    if p.eat(T![:]) {
        types::type_(p);
    } else {
        p.error("missing type for `const` item");
    }
    if p.eat(T![=]) && expressions::expr(p).is_none() {
        p.error("expected an initializer");
    }
    generics::opt_where_clause(p);
    p.expect(T![;]);
    m.complete(p, CONST);
}

fn static_(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![static]);
    p.eat(T![mut]);
    if !p.eat(T![_]) {
        name_r(p, TokenSet::new(&[T![:], T![=], T![;]]));
    }
    if p.eat(T![:]) {
        types::type_(p);
    } else {
        p.error("missing type for `static` item");
    }
    if p.eat(T![=]) && expressions::expr(p).is_none() {
        p.error("expected an initializer");
    }
    p.expect(T![;]);
    m.complete(p, STATIC);
}

fn use_item(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![use]);
    use_tree(p);
    p.expect(T![;]);
    m.complete(p, USE);
}

fn use_tree(p: &mut Parser<'_, '_>) {
    let m = p.start();
    match p.current() {
        T![*] => p.bump(T![*]),
        T![:] if p.at(T![::]) && p.nth(2) == T![*] => {
            p.bump(T![::]);
            p.bump(T![*]);
        }
        T!['{'] => use_tree_list(p),
        T![:] if p.at(T![::]) && p.nth(2) == T!['{'] => {
            p.bump(T![::]);
            use_tree_list(p);
        }
        _ if paths::is_use_path_start(p) => {
            paths::use_path(p);
            match p.current() {
                T![as] => opt_rename(p),
                T![:] if p.at(T![::]) => {
                    p.bump(T![::]);
                    match p.current() {
                        T![*] => p.bump(T![*]),
                        T!['{'] => use_tree_list(p),
                        _ => p.error("expected `{` or `*`"),
                    }
                }
                _ => {}
            }
        }
        _ => {
            m.abandon(p);
            p.err_recover("expected a use tree", TokenSet::new(&[T![;], T![,]]));
            return;
        }
    }
    m.complete(p, USE_TREE);
}

fn use_tree_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| comma_separated(p, use_tree));
    m.complete(p, USE_TREE_LIST);
}

fn extern_crate(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![extern]);
    p.bump(T![crate]);
    if p.at(T![self]) {
        let name = p.start();
        p.bump(T![self]);
        name.complete(p, NAME_REF);
    } else {
        name_ref(p);
    }
    opt_rename(p);
    p.expect(T![;]);
    m.complete(p, EXTERN_CRATE);
}

fn macro_rules(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump_remap(T![macro_rules]);
    p.bump(T![!]);
    name(p);
    match p.current() {
        T!['{'] => token_tree_deferrable(p),
        T!['('] | T!['['] => {
            token_tree_deferrable(p);
            p.expect(T![;]);
        }
        _ => p.error("expected `{`, `(` or `[`"),
    }
    m.complete(p, MACRO_RULES);
}

/// `macro name(args) { body }` or `macro name { rules }` (declarative macros
/// 2.0).
fn macro_def(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![macro]);
    name(p);
    if p.at(T!['(']) {
        token_tree(p);
    }
    if p.at(T!['{']) {
        token_tree_deferrable(p);
    } else {
        p.error("expected `{`");
    }
    m.complete(p, MACRO_DEF);
}
