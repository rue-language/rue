use super::*;

pub(super) const TYPE_FIRST: TokenSet = paths::PATH_FIRST.union(TokenSet::new(&[
    T!['('],
    T!['['],
    T![!],
    T![*],
    T![&],
    T![_],
    T![fn],
    T![unsafe],
    T![extern],
    T![for],
    T![impl],
    T![dyn],
]));

const TYPE_RECOVERY: TokenSet = TokenSet::new(&[T![')'], T![,], T![=], T![;], T![>]]);

pub(super) fn type_(p: &mut Parser<'_, '_>) {
    type_with_bounds_cond(p, true);
}

/// A type that does not absorb a following `+`: used where `+` means
/// something else, as in `x as u32 + 1` or a return type inside a bound.
pub(super) fn type_no_bounds(p: &mut Parser<'_, '_>) {
    type_with_bounds_cond(p, false);
}

fn type_with_bounds_cond(p: &mut Parser<'_, '_>, allow_bounds: bool) {
    match p.current() {
        T!['('] => paren_or_tuple_type(p),
        T![!] => {
            let m = p.start();
            p.bump(T![!]);
            m.complete(p, NEVER_TYPE);
        }
        T![*] => ptr_type(p),
        T!['['] => array_or_slice_type(p),
        T![&] => ref_type(p),
        T![_] => {
            let m = p.start();
            p.bump(T![_]);
            m.complete(p, INFER_TYPE);
        }
        T![fn] | T![unsafe] | T![extern] => fn_ptr_type(p),
        T![for] => for_type(p, allow_bounds),
        T![impl] => {
            let m = p.start();
            p.bump(T![impl]);
            generics::bounds_without_colon(p);
            m.complete(p, IMPL_TRAIT_TYPE);
        }
        T![dyn] => dyn_trait_type(p, T![dyn]),
        IDENT if is_2015_dyn(p) => dyn_trait_type(p, IDENT),
        _ if paths::is_path_start(p) => path_or_macro_type(p, allow_bounds),
        // Bound modifiers start a bare trait object: `?Sized`, `~const Tr`,
        // `const Tr`, `async Fn()`. Later phases reject these where a type
        // is required.
        T![?] | T![~] => bare_trait_object(p),
        T![const] | T![async] if paths::is_path_start_at(p, 1) => bare_trait_object(p),
        LIFETIME_IDENT if allow_bounds && p.nth_at(1, T![+]) => {
            // A bare trait object starting with a lifetime: `'a + Trait`.
            let m = p.start();
            generics::bounds_without_colon(p);
            m.complete(p, DYN_TRAIT_TYPE);
        }
        LIFETIME_IDENT => p.err_and_bump("expected type, found lifetime"),
        _ => p.err_recover("expected a type", TYPE_RECOVERY),
    }
}

fn bare_trait_object(p: &mut Parser<'_, '_>) {
    let m = p.start();
    generics::bounds_without_colon(p);
    m.complete(p, DYN_TRAIT_TYPE);
}

/// In the 2015 edition `dyn` is a contextual keyword in type position.
fn is_2015_dyn(p: &Parser<'_, '_>) -> bool {
    p.edition() < crate::Edition::Edition2018
        && p.at_contextual_kw_text("dyn")
        && matches!(
            p.nth(1),
            IDENT | LIFETIME_IDENT | T![?] | T![for] | T!['('] | T![self] | T![super] | T![crate]
        )
        && !p.nth_at(1, T![::])
}

fn dyn_trait_type(p: &mut Parser<'_, '_>, kw: SyntaxKind) {
    let m = p.start();
    if kw == IDENT {
        p.bump_remap(T![dyn]);
    } else {
        p.bump(T![dyn]);
    }
    generics::bounds_without_colon(p);
    m.complete(p, DYN_TRAIT_TYPE);
}

fn paren_or_tuple_type(p: &mut Parser<'_, '_>) {
    let m = p.start();
    let mut n_types = 0;
    let mut trailing_comma = false;
    p.group(|p| {
        while !p.at_eof() {
            type_(p);
            n_types += 1;
            trailing_comma = false;
            if p.at_eof() || !p.expect(T![,]) {
                break;
            }
            trailing_comma = true;
        }
    });
    let kind = if n_types == 1 && !trailing_comma {
        PAREN_TYPE
    } else {
        TUPLE_TYPE
    };
    m.complete(p, kind);
}

fn ptr_type(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(T![*]);
    if !p.eat(T![const]) && !p.eat(T![mut]) {
        p.error("expected `mut` or `const` in raw pointer type");
    }
    type_no_bounds(p);
    m.complete(p, PTR_TYPE);
}

fn array_or_slice_type(p: &mut Parser<'_, '_>) {
    let m = p.start();
    let mut kind = SLICE_TYPE;
    p.group(|p| {
        type_(p);
        if p.eat(T![;]) {
            kind = ARRAY_TYPE;
            if expressions::expr(p).is_none() {
                p.error("expected an array length");
            }
        }
    });
    m.complete(p, kind);
}

fn ref_type(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(T![&]);
    if p.at(LIFETIME_IDENT) {
        lifetime(p);
    }
    p.eat(T![mut]);
    type_no_bounds(p);
    m.complete(p, REF_TYPE);
}

fn fn_ptr_type(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.eat(T![unsafe]);
    if p.at(T![extern]) {
        abi(p);
    }
    if !p.eat(T![fn]) {
        p.error("expected `fn`");
        m.complete(p, ERROR);
        return;
    }
    if p.at(T!['(']) {
        params::param_list_fn_ptr(p);
    } else {
        p.error("expected parameters");
    }
    opt_ret_type(p);
    m.complete(p, FN_PTR_TYPE);
}

/// `for<'a> fn(&'a u8)` or `for<'a> Trait<'a>`.
fn for_type(p: &mut Parser<'_, '_>, allow_bounds: bool) {
    let m = p.start();
    generics::for_binder(p);
    match p.current() {
        T![fn] | T![unsafe] | T![extern] => fn_ptr_type(p),
        _ if paths::is_path_start(p) => path_or_macro_type(p, allow_bounds),
        _ => p.error("expected a function pointer or path"),
    }
    m.complete(p, FOR_TYPE);
}

fn path_or_macro_type(p: &mut Parser<'_, '_>, allow_bounds: bool) {
    let m = p.start();
    let path = paths::type_path(p);
    if p.at(T![!]) && !p.at(T![!=]) {
        let call = path.precede(p);
        p.bump(T![!]);
        if p.current().closing_delimiter().is_some() {
            token_tree(p);
        } else {
            p.error("expected a macro argument list");
        }
        call.complete(p, MACRO_CALL);
        m.complete(p, MACRO_TYPE);
        return;
    }
    let ty = m.complete(p, PATH_TYPE);
    if allow_bounds {
        opt_bounds_as_dyn_trait_type(p, ty);
    }
}

/// A path type in bound position: no `+` absorption, no macro.
pub(super) fn path_type_for_bound(p: &mut Parser<'_, '_>) {
    let m = p.start();
    paths::type_path(p);
    m.complete(p, PATH_TYPE);
}

/// Wraps `Trait + Send` (a 2015-style bare trait object) in a
/// `DYN_TRAIT_TYPE` when a `+` follows a path type.
pub(super) fn opt_bounds_as_dyn_trait_type(p: &mut Parser<'_, '_>, ty: CompletedMarker) {
    if !p.at(T![+]) {
        return;
    }
    let list = ty.precede(p).complete(p, TYPE_BOUND).precede(p);
    p.bump(T![+]);
    let list = generics::bounds_without_colon_m(p, list);
    list.precede(p).complete(p, DYN_TRAIT_TYPE);
}
