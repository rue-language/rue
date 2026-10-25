use super::*;

pub(super) const PATTERN_FIRST: TokenSet = expressions::LITERAL_FIRST
    .union(paths::PATH_FIRST)
    .union(TokenSet::new(&[
        T![box],
        T![ref],
        T![mut],
        T![const],
        T!['('],
        T!['['],
        T![&],
        T![_],
        T![-],
        T![.],
        T![|],
        T![!],
    ]));

const PAT_RECOVERY: TokenSet = TokenSet::new(&[T![let], T![if], T![,], T![=]]);

/// A pattern with alternatives and an optional leading `|`, as in a match
/// arm or `let` statement.
pub(super) fn pattern_top(p: &mut Parser<'_, '_>) {
    pattern_top_r(p, PAT_RECOVERY);
}

pub(super) fn pattern_top_r(p: &mut Parser<'_, '_>, recovery: TokenSet) {
    let m = p.start();
    let leading_pipe = at_single_pipe(p);
    if leading_pipe {
        p.bump(T![|]);
    }
    pattern_r(p, recovery);
    if leading_pipe {
        m.complete(p, OR_PAT);
    } else {
        m.abandon(p);
    }
}

/// A pattern with alternatives: `A | B`.
pub(super) fn pattern(p: &mut Parser<'_, '_>) {
    pattern_r(p, PAT_RECOVERY);
}

fn pattern_r(p: &mut Parser<'_, '_>, recovery: TokenSet) {
    let m = p.start();
    pattern_single_r(p, recovery);
    if !at_single_pipe(p) {
        m.abandon(p);
        return;
    }
    while at_single_pipe(p) {
        p.bump(T![|]);
        pattern_single_r(p, recovery);
    }
    m.complete(p, OR_PAT);
}

fn at_single_pipe(p: &Parser<'_, '_>) -> bool {
    p.at(T![|]) && !p.at(T![||]) && !p.at(T![|=])
}

/// A pattern without top-level alternatives, as in a closure parameter.
pub(super) fn pattern_single(p: &mut Parser<'_, '_>) {
    pattern_single_r(p, PAT_RECOVERY);
}

fn pattern_single_r(p: &mut Parser<'_, '_>, recovery: TokenSet) {
    let Some(lhs) = atom_pat(p, recovery) else {
        return;
    };
    if matches!(lhs.kind(), LITERAL_PAT | PATH_PAT | CONST_BLOCK_PAT) && at_range_op(p) {
        let m = lhs.precede(p);
        let half_open = eat_range_op(p);
        if !half_open || at_range_end(p) {
            atom_pat(p, recovery);
        }
        m.complete(p, RANGE_PAT);
    }
}

fn at_range_op(p: &Parser<'_, '_>) -> bool {
    p.at(T![..]) || p.at(T![..=]) || p.at(T![...])
}

/// Consumes `...`, `..=` or `..`; returns whether it was `..`, whose end is
/// optional.
fn eat_range_op(p: &mut Parser<'_, '_>) -> bool {
    if p.eat(T![...]) || p.eat(T![..=]) {
        return false;
    }
    p.bump(T![..]);
    true
}

fn at_range_end(p: &Parser<'_, '_>) -> bool {
    p.at_ts(expressions::LITERAL_FIRST)
        || p.at(T![-])
        || paths::is_path_start(p)
        || (p.at(T![const]) && p.nth(1) == T!['{'])
}

fn atom_pat(p: &mut Parser<'_, '_>, recovery: TokenSet) -> Option<CompletedMarker> {
    let m = match p.current() {
        T![box] => {
            let m = p.start();
            p.bump(T![box]);
            pattern_single_r(p, recovery);
            m.complete(p, BOX_PAT)
        }
        T![ref] | T![mut] => ident_pat(p, true),
        T![const] if p.nth(1) == T!['{'] => {
            let m = p.start();
            p.bump(T![const]);
            expressions::block_expr(p);
            m.complete(p, CONST_BLOCK_PAT)
        }
        IDENT if is_binding(p) => ident_pat(p, true),
        _ if paths::is_path_start(p) => path_or_struct_pat(p),
        T![-] => literal_pat(p),
        k if expressions::LITERAL_FIRST.contains(k) => literal_pat(p),
        T![_] => {
            let m = p.start();
            p.bump(T![_]);
            m.complete(p, WILDCARD_PAT)
        }
        // A never pattern, `!`; feature-gated after parsing.
        T![!] => {
            let m = p.start();
            p.bump(T![!]);
            m.complete(p, NEVER_PAT)
        }
        T![&] => {
            let m = p.start();
            p.bump(T![&]);
            p.eat(T![mut]);
            pattern_single_r(p, recovery);
            m.complete(p, REF_PAT)
        }
        T!['('] => tuple_pat(p),
        T!['['] => {
            let m = p.start();
            p.group(|p| comma_separated(p, pattern_top));
            m.complete(p, SLICE_PAT)
        }
        T![.] if p.at(T![..=]) || p.at(T![...]) => {
            let m = p.start();
            eat_range_op(p);
            atom_pat(p, recovery);
            m.complete(p, RANGE_PAT)
        }
        T![.] if p.at(T![..]) => {
            let m = p.start();
            p.bump(T![..]);
            if p.at_ts(expressions::LITERAL_FIRST) || p.at(T![-]) {
                atom_pat(p, recovery);
                m.complete(p, RANGE_PAT)
            } else {
                m.complete(p, REST_PAT)
            }
        }
        _ => {
            p.err_recover("expected a pattern", recovery);
            return None;
        }
    };
    Some(m)
}

/// Whether an identifier starts a plain binding rather than a path (`A::B`,
/// `S(..)`, `S { .. }`, `m!()`) or a range bound (`A..=B`).
fn is_binding(p: &Parser<'_, '_>) -> bool {
    match p.nth(1) {
        T![:] => p.nth_at(1, T![:]),
        T!['('] | T!['{'] => false,
        T![!] => p.nth_at(1, T![!=]),
        T![.] => !p.nth_at(1, T![..]),
        _ => true,
    }
}

fn ident_pat(p: &mut Parser<'_, '_>, with_at: bool) -> CompletedMarker {
    let m = p.start();
    p.eat(T![ref]);
    p.eat(T![mut]);
    name(p);
    if with_at && p.eat(T![@]) {
        pattern_single(p);
    }
    m.complete(p, IDENT_PAT)
}

fn literal_pat(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    let negated = p.eat(T![-]);
    let invalid = !negated && expressions::has_invalid_number_suffix(p);
    let text = p.current_text();
    if expressions::literal(p).is_none() {
        p.error("expected a literal");
    } else if invalid {
        p.error_on_previous_token(format!("expected identifier, found `{text}`"));
    }
    m.complete(p, LITERAL_PAT)
}

fn tuple_pat(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    let mut n_pats = 0;
    let mut trailing_comma = false;
    let mut has_rest = false;
    p.group(|p| {
        while !p.at_eof() {
            has_rest |= p.at(T![..]) && !p.at(T![..=]);
            pattern_top(p);
            n_pats += 1;
            trailing_comma = false;
            if p.at_eof() || !p.expect(T![,]) {
                break;
            }
            trailing_comma = true;
        }
    });
    let kind = if n_pats == 1 && !trailing_comma && !has_rest {
        PAREN_PAT
    } else {
        TUPLE_PAT
    };
    m.complete(p, kind)
}

fn path_or_struct_pat(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    let path = paths::expr_path(p);
    match p.current() {
        T!['('] => {
            p.group(|p| comma_separated(p, pattern_top));
            m.complete(p, TUPLE_STRUCT_PAT)
        }
        T!['{'] => {
            record_pat_field_list(p);
            m.complete(p, RECORD_PAT)
        }
        T![!] if !p.at(T![!=]) => {
            let call = path.precede(p);
            p.bump(T![!]);
            if p.current().closing_delimiter().is_some() {
                token_tree(p);
            } else {
                p.error("expected a macro argument list");
            }
            call.complete(p, MACRO_CALL);
            m.complete(p, MACRO_PAT)
        }
        _ => m.complete(p, PATH_PAT),
    }
}

fn record_pat_field_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        comma_separated(p, |p| {
            let field = p.start();
            attributes::outer_attrs(p);
            match p.current() {
                T![.] if p.at(T![..]) => {
                    p.bump(T![..]);
                    field.complete(p, REST_PAT);
                }
                IDENT | INT_NUMBER if p.nth_at(1, T![:]) => {
                    name_ref_or_index(p);
                    p.bump(T![:]);
                    pattern_top(p);
                    field.complete(p, RECORD_PAT_FIELD);
                }
                T![box] => {
                    let inner = p.start();
                    p.bump(T![box]);
                    ident_pat(p, false);
                    inner.complete(p, BOX_PAT);
                    field.complete(p, RECORD_PAT_FIELD);
                }
                IDENT | T![ref] | T![mut] => {
                    ident_pat(p, false);
                    field.complete(p, RECORD_PAT_FIELD);
                }
                _ => {
                    field.abandon(p);
                    p.err_and_bump("expected a field pattern");
                }
            }
        })
    });
    m.complete(p, RECORD_PAT_FIELD_LIST);
}
