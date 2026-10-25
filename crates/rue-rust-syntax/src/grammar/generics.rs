use super::*;

pub(super) fn opt_generic_param_list(p: &mut Parser<'_, '_>) {
    if p.at(T![<]) {
        generic_param_list(p);
    }
}

pub(super) fn generic_param_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(T![<]);
    while !p.at(T![>]) && !p.at_eof() {
        let start = p.pos();
        generic_param(p);
        if p.pos() == start {
            p.err_and_bump("expected a generic parameter");
        }
        if !p.at(T![>]) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T![>]);
    m.complete(p, GENERIC_PARAM_LIST);
}

fn generic_param(p: &mut Parser<'_, '_>) {
    let m = p.start();
    attributes::outer_attrs(p);
    match p.current() {
        LIFETIME_IDENT => {
            lifetime(p);
            if p.at(T![:]) {
                lifetime_bounds(p);
            }
            m.complete(p, LIFETIME_PARAM);
        }
        IDENT => {
            name(p);
            if p.at(T![:]) && !p.at(T![::]) {
                bounds(p);
            }
            if p.eat(T![=]) {
                types::type_(p);
            }
            m.complete(p, TYPE_PARAM);
        }
        T![const] => {
            p.bump(T![const]);
            name(p);
            if p.eat(T![:]) {
                types::type_(p);
            } else {
                p.error("expected a type for the const parameter");
            }
            if p.eat(T![=]) {
                expressions::const_arg(p);
            }
            m.complete(p, CONST_PARAM);
        }
        _ => m.abandon(p),
    }
}

/// `'a: 'b + 'c`.
fn lifetime_bounds(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(T![:]);
    while p.at(LIFETIME_IDENT) {
        let bound = p.start();
        lifetime(p);
        bound.complete(p, TYPE_BOUND);
        if !p.eat(T![+]) {
            break;
        }
    }
    m.complete(p, TYPE_BOUND_LIST);
}

/// `for<'a>`, a higher-ranked binder.
pub(super) fn for_binder(p: &mut Parser<'_, '_>) {
    p.bump(T![for]);
    if p.at(T![<]) {
        generic_param_list(p);
    } else {
        p.error("expected `<`");
    }
}

pub(super) fn bounds(p: &mut Parser<'_, '_>) {
    p.bump(T![:]);
    bounds_without_colon(p);
}

pub(super) fn bounds_without_colon(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    bounds_without_colon_m(p, m)
}

pub(super) fn bounds_without_colon_m(p: &mut Parser<'_, '_>, m: Marker) -> CompletedMarker {
    while type_bound(p) {
        if !p.eat(T![+]) {
            break;
        }
    }
    m.complete(p, TYPE_BOUND_LIST)
}

fn type_bound(p: &mut Parser<'_, '_>) -> bool {
    let m = p.start();
    match p.current() {
        LIFETIME_IDENT => lifetime(p),
        T!['('] => p.group(|p| {
            type_bound_inner(p);
        }),
        T![use] => {
            // Precise capturing: `use<'a, T>`.
            p.bump(T![use]);
            let args = p.start();
            if p.eat(T![<]) {
                while !p.at(T![>]) && !p.at_eof() {
                    match p.current() {
                        LIFETIME_IDENT => lifetime(p),
                        IDENT | T![Self] => {
                            let name = p.start();
                            p.bump_any();
                            name.complete(p, NAME_REF);
                        }
                        _ => {
                            p.err_and_bump("expected a lifetime or type parameter");
                        }
                    }
                    if !p.at(T![>]) && !p.expect(T![,]) {
                        break;
                    }
                }
                p.expect(T![>]);
            } else {
                p.error("expected `<`");
            }
            args.complete(p, USE_BOUND_GENERIC_ARGS);
        }
        _ => {
            if !type_bound_inner(p) {
                m.abandon(p);
                return false;
            }
        }
    }
    m.complete(p, TYPE_BOUND);
    true
}

/// Modifiers, an optional binder, and the bound's path.
fn type_bound_inner(p: &mut Parser<'_, '_>) -> bool {
    let mut has_modifiers = false;
    loop {
        match p.current() {
            T![?] | T![!] | T![const] | T![async] => p.bump_any(),
            T![~] if p.nth(1) == T![const] => {
                p.bump(T![~]);
                p.bump(T![const]);
            }
            _ => break,
        }
        has_modifiers = true;
    }
    if p.at(T![for]) {
        for_binder(p);
        has_modifiers = true;
    }
    if paths::is_path_start(p) {
        types::path_type_for_bound(p);
        true
    } else {
        if has_modifiers {
            p.error("expected a trait path");
        }
        has_modifiers
    }
}

pub(super) fn opt_where_clause(p: &mut Parser<'_, '_>) {
    if !p.at(T![where]) {
        return;
    }
    let m = p.start();
    p.bump(T![where]);
    while is_where_predicate_start(p) {
        where_predicate(p);
        if !p.eat(T![,]) {
            break;
        }
    }
    m.complete(p, WHERE_CLAUSE);
}

fn is_where_predicate_start(p: &Parser<'_, '_>) -> bool {
    p.at(LIFETIME_IDENT) || p.at_ts(types::TYPE_FIRST)
}

fn where_predicate(p: &mut Parser<'_, '_>) {
    let m = p.start();
    if p.at(LIFETIME_IDENT) {
        lifetime(p);
        if p.at(T![:]) {
            lifetime_bounds(p);
        } else {
            p.error("expected `:`");
        }
    } else {
        if p.at(T![for]) && p.nth(1) == T![<] {
            for_binder(p);
        }
        types::type_(p);
        if p.at(T![:]) && !p.at(T![::]) {
            bounds(p);
        } else {
            p.error("expected `:`");
        }
    }
    m.complete(p, WHERE_PRED);
}
