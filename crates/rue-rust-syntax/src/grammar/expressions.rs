use super::*;

pub(super) const LITERAL_FIRST: TokenSet = TokenSet::new(&[
    T![true],
    T![false],
    INT_NUMBER,
    FLOAT_NUMBER,
    CHAR,
    BYTE,
    STRING,
    BYTE_STRING,
    C_STRING,
]);

const EXPR_FIRST: TokenSet = LITERAL_FIRST
    .union(paths::PATH_FIRST)
    .union(TokenSet::new(&[
        T!['('],
        T!['['],
        T!['{'],
        T![|],
        T![!],
        T![-],
        T![*],
        T![&],
        T![.],
        T![move],
        T![async],
        T![static],
        T![for],
        T![if],
        T![loop],
        T![while],
        T![match],
        T![unsafe],
        T![const],
        T![return],
        T![become],
        T![break],
        T![continue],
        T![yield],
        T![let],
        T![_],
        T![box],
        T![try],
        T![do],
        LIFETIME_IDENT,
    ]));

const EXPR_RECOVERY: TokenSet = TokenSet::new(&[T![;], T![,], T![let]]);

/// Binding power of prefix operators: tighter than every binary operator
/// (so `-a as u8` is `(-a) as u8`) and looser than postfix ones (so `-a.b()`
/// is `-(a.b())`).
const PREFIX_BP: u8 = 13;
/// The left operand of `&&` in a `let` chain stops here: `let p = a && b`
/// tests `a` and then `b`.
const LET_SCRUTINEE_BP: u8 = 5;

/// Context that changes how an expression parses.
#[derive(Clone, Copy, Default)]
struct Restrictions {
    /// `{` cannot start a struct literal: in `if S {}` the braces are the
    /// body. Every delimited group resets this.
    forbid_structs: bool,
    /// At statement start a block-like expression (`if`, `match`, `{}`)
    /// ends the statement: `match x {} - 1` is two statements.
    prefer_stmt: bool,
    /// `let` is an expression only in an `if` or `while` condition or a match
    /// guard, possibly chained with `&&`.
    allow_let: bool,
}

impl Restrictions {
    /// The restrictions for an operand of an operator.
    fn operand(self) -> Restrictions {
        Restrictions {
            prefer_stmt: false,
            allow_let: false,
            ..self
        }
    }
}

const NO_STRUCTS: Restrictions = Restrictions {
    forbid_structs: true,
    prefer_stmt: false,
    allow_let: false,
};

const CONDITION: Restrictions = Restrictions {
    allow_let: true,
    ..NO_STRUCTS
};

pub(super) fn expr(p: &mut Parser<'_, '_>) -> Option<CompletedMarker> {
    expr_bp(p, None, Restrictions::default(), 1).map(|(m, _)| m)
}

fn expr_no_struct(p: &mut Parser<'_, '_>) {
    expr_bp(p, None, NO_STRUCTS, 1);
}

/// An expression at statement start; reports whether it was block-like.
pub(super) fn expr_stmt(
    p: &mut Parser<'_, '_>,
    m: Option<Marker>,
) -> Option<(CompletedMarker, BlockLike)> {
    let r = Restrictions {
        prefer_stmt: true,
        ..Restrictions::default()
    };
    expr_bp(p, m, r, 1)
}

/// The value of a const generic argument or default: a block, a literal
/// (possibly negated), or a path.
pub(super) fn const_arg(p: &mut Parser<'_, '_>) {
    match p.current() {
        T!['{'] => {
            block_expr(p);
        }
        T![-] => {
            let m = p.start();
            p.bump(T![-]);
            if literal(p).is_none() {
                p.error("expected a literal");
            }
            m.complete(p, PREFIX_EXPR);
        }
        _ if literal(p).is_some() => {}
        _ if paths::is_path_start(p) => {
            let m = p.start();
            paths::expr_path(p);
            m.complete(p, PATH_EXPR);
        }
        _ => p.err_recover("expected a const argument", TokenSet::new(&[T![>], T![,]])),
    }
}

/// Pratt parsing of binary operators above binding power `bp`.
fn expr_bp(
    p: &mut Parser<'_, '_>,
    m: Option<Marker>,
    r: Restrictions,
    bp: u8,
) -> Option<(CompletedMarker, BlockLike)> {
    let m = m.unwrap_or_else(|| {
        let m = p.start();
        attributes::outer_attrs(p);
        m
    });
    if !p.at_ts(EXPR_FIRST) {
        p.err_recover("expected an expression", EXPR_RECOVERY);
        m.abandon(p);
        return None;
    }
    let mut lhs = match lhs(p, r) {
        Some((lhs, blocklike)) => {
            let lhs = lhs.extend_to(p, m);
            if r.prefer_stmt && blocklike.is_block() {
                return Some((lhs, BlockLike::Block));
            }
            lhs
        }
        None => {
            m.abandon(p);
            return None;
        }
    };

    let mut lhs_is_comparison = false;
    loop {
        let is_range = p.at(T![..]) || p.at(T![..=]);
        let Some((op_bp, op, right_assoc)) = current_op(p) else {
            break;
        };
        if op_bp < bp {
            break;
        }
        let is_comparison = matches!(op, T![==] | T![!=] | T![<] | T![>] | T![<=] | T![>=]);
        if is_comparison && lhs_is_comparison {
            p.error("comparison operators cannot be chained");
        }
        lhs_is_comparison = is_comparison;
        if op == T![as] {
            let m = lhs.precede(p);
            p.bump(T![as]);
            types::type_no_bounds(p);
            lhs = m.complete(p, CAST_EXPR);
            continue;
        }
        let m = lhs.precede(p);
        p.bump(op);
        if is_range {
            // `a..` may have no end.
            if p.at_ts(EXPR_FIRST) && !(r.forbid_structs && p.at(T!['{'])) {
                expr_bp(p, None, r.operand(), op_bp + 1);
            }
            lhs = m.complete(p, RANGE_EXPR);
            continue;
        }
        let rhs_bp = if right_assoc { op_bp } else { op_bp + 1 };
        // `let` chains: both sides of `&&` may be `let` expressions.
        let rhs_r = if op == T![&&] {
            Restrictions {
                prefer_stmt: false,
                ..r
            }
        } else {
            r.operand()
        };
        expr_bp(p, None, rhs_r, rhs_bp);
        lhs = m.complete(p, BIN_EXPR);
    }
    Some((lhs, BlockLike::NotBlock))
}

/// The binary operator at the current position: its binding power, its
/// (possibly composite) token, and whether it is right-associative.
fn current_op(p: &Parser<'_, '_>) -> Option<(u8, SyntaxKind, bool)> {
    let op = match p.current() {
        T![|] if p.at(T![||]) => (3, T![||], false),
        T![|] if p.at(T![|=]) => (1, T![|=], true),
        T![|] => (6, T![|], false),
        T![>] if p.at(T![>>=]) => (1, T![>>=], true),
        T![>] if p.at(T![>>]) => (9, T![>>], false),
        T![>] if p.at(T![>=]) => (5, T![>=], false),
        T![>] => (5, T![>], false),
        T![=] if p.at(T![=>]) => return None,
        T![=] if p.at(T![==]) => (5, T![==], false),
        T![=] => (1, T![=], true),
        T![!] if p.at(T![!=]) => (5, T![!=], false),
        T![-] if p.at(T![-=]) => (1, T![-=], true),
        T![-] if p.at(T![->]) => return None,
        T![-] => (10, T![-], false),
        T![<] if p.at(T![<<=]) => (1, T![<<=], true),
        T![<] if p.at(T![<<]) => (9, T![<<], false),
        T![<] if p.at(T![<=]) => (5, T![<=], false),
        T![<] => (5, T![<], false),
        T![+] if p.at(T![+=]) => (1, T![+=], true),
        T![+] => (10, T![+], false),
        T![^] if p.at(T![^=]) => (1, T![^=], true),
        T![^] => (7, T![^], false),
        T![%] if p.at(T![%=]) => (1, T![%=], true),
        T![%] => (11, T![%], false),
        T![&] if p.at(T![&=]) => (1, T![&=], true),
        T![&] if p.at(T![&&]) => (4, T![&&], false),
        T![&] => (8, T![&], false),
        T![/] if p.at(T![/=]) => (1, T![/=], true),
        T![/] => (11, T![/], false),
        T![*] if p.at(T![*=]) => (1, T![*=], true),
        T![*] => (11, T![*], false),
        T![.] if p.at(T![..=]) => (2, T![..=], false),
        T![.] if p.at(T![..]) && !p.at(T![...]) => (2, T![..], false),
        T![as] => (12, T![as], false),
        _ => return None,
    };
    Some(op)
}

/// A prefix-operator expression, a prefix range, or a postfix chain.
fn lhs(p: &mut Parser<'_, '_>, r: Restrictions) -> Option<(CompletedMarker, BlockLike)> {
    let m = p.start();
    let kind = match p.current() {
        T![&] => {
            p.bump(T![&]);
            if p.at_contextual_kw(T![raw]) && matches!(p.nth(1), T![const] | T![mut]) {
                p.bump_remap(T![raw]);
                p.bump_any();
            } else {
                p.eat(T![mut]);
            }
            REF_EXPR
        }
        T![*] | T![!] | T![-] => {
            p.bump_any();
            PREFIX_EXPR
        }
        T![box] => {
            p.bump(T![box]);
            BOX_EXPR
        }
        T![.] if p.at(T![..=]) || p.at(T![..]) => {
            if !p.eat(T![..=]) {
                p.bump(T![..]);
            }
            if p.at_ts(EXPR_FIRST) && !(r.forbid_structs && p.at(T!['{'])) {
                expr_bp(p, None, r.operand(), 3);
            }
            return Some((m.complete(p, RANGE_EXPR), BlockLike::NotBlock));
        }
        _ => {
            m.abandon(p);
            let (atom, blocklike) = atom_expr(p, r)?;
            let allow_calls = !(r.prefer_stmt && blocklike.is_block());
            return Some(postfix_expr(p, atom, blocklike, allow_calls));
        }
    };
    expr_bp(p, None, r.operand(), PREFIX_BP);
    Some((m.complete(p, kind), BlockLike::NotBlock))
}

fn postfix_expr(
    p: &mut Parser<'_, '_>,
    mut lhs: CompletedMarker,
    mut blocklike: BlockLike,
    mut allow_calls: bool,
) -> (CompletedMarker, BlockLike) {
    loop {
        lhs = match p.current() {
            // At statement start, `match x {} (a)` is two statements; only
            // method calls and `?` continue a block-like expression.
            T!['('] if allow_calls => {
                let m = lhs.precede(p);
                arg_list(p);
                m.complete(p, CALL_EXPR)
            }
            T!['['] if allow_calls => {
                let m = lhs.precede(p);
                p.group(|p| {
                    if expr(p).is_none() {
                        p.error("expected an index");
                    }
                });
                m.complete(p, INDEX_EXPR)
            }
            T![.] if !p.at(T![..]) => postfix_dot_expr(p, lhs),
            T![?] => {
                let m = lhs.precede(p);
                p.bump(T![?]);
                m.complete(p, TRY_EXPR)
            }
            _ => break,
        };
        allow_calls = true;
        blocklike = BlockLike::NotBlock;
    }
    (lhs, blocklike)
}

fn postfix_dot_expr(p: &mut Parser<'_, '_>, lhs: CompletedMarker) -> CompletedMarker {
    let m = lhs.precede(p);
    p.bump(T![.]);
    match p.current() {
        IDENT | T![self] | T![Self] | T![super] | T![crate]
            if p.nth(1) == T!['('] || p.nth_at(1, T![::]) =>
        {
            let name = p.start();
            p.bump_any();
            name.complete(p, NAME_REF);
            if p.at(T![::]) {
                p.bump(T![::]);
                if p.at(T![<]) {
                    paths::generic_arg_list(p);
                } else {
                    p.error("expected `<`");
                }
            }
            if p.at(T!['(']) {
                arg_list(p);
            } else {
                p.error("expected argument list");
            }
            m.complete(p, METHOD_CALL_EXPR)
        }
        T![await] => {
            p.bump(T![await]);
            m.complete(p, AWAIT_EXPR)
        }
        IDENT | INT_NUMBER | T![self] | T![Self] | T![super] | T![crate] => {
            name_ref_or_index(p);
            m.complete(p, FIELD_EXPR)
        }
        FLOAT_NUMBER if p.current_text().contains('.') => {
            let suffixed = has_suffix(p);
            let (ends_in_dot, outer) = p.split_float(m);
            if suffixed {
                p.error_on_previous_token("suffixes on a tuple index are invalid");
            }
            if ends_in_dot {
                if p.at(IDENT) || p.at(INT_NUMBER) {
                    name_ref_or_index(p);
                } else {
                    p.error("expected a field name");
                }
            }
            outer.complete(p, FIELD_EXPR)
        }
        FLOAT_NUMBER => {
            // `x.1e5`: a float with no dot to split at.
            p.error("invalid field name");
            let name = p.start();
            p.bump_any();
            name.complete(p, NAME_REF);
            m.complete(p, FIELD_EXPR)
        }
        _ => {
            p.error("expected a field name or method call");
            m.complete(p, FIELD_EXPR)
        }
    }
}

fn arg_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        comma_separated(p, |p| {
            expr(p);
        })
    });
    m.complete(p, ARG_LIST);
}

pub(super) fn literal(p: &mut Parser<'_, '_>) -> Option<CompletedMarker> {
    if !p.at_ts(LITERAL_FIRST) {
        return None;
    }
    let m = p.start();
    p.bump_any();
    Some(m.complete(p, LITERAL))
}

/// Whether the current token is a number literal whose suffix is not a
/// numeric type. rustc's parser rejects such a literal as a pattern; in an
/// expression the suffix is checked later.
pub(super) fn has_invalid_number_suffix(p: &Parser<'_, '_>) -> bool {
    use rue_rust_lexer::{Base, LiteralKind, TokenKind};
    const INT: &[&str] = &[
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    ];
    const FLOAT: &[&str] = &["f16", "f32", "f64", "f128"];
    let Some(token) = p.current_lexer_token() else {
        return false;
    };
    let TokenKind::Literal { kind, suffix_start } = token.kind else {
        return false;
    };
    let suffix = &p.current_text()[suffix_start as usize..];
    match kind {
        _ if suffix.is_empty() => false,
        LiteralKind::Int { base, .. } => {
            !(INT.contains(&suffix) || (base == Base::Decimal && FLOAT.contains(&suffix)))
        }
        LiteralKind::Float { .. } => !FLOAT.contains(&suffix),
        _ => false,
    }
}

/// Whether the current literal token has a suffix (`1u8`, `"s"x`).
///
/// Suffixes are checked by later phases, except on a tuple index, where
/// rustc's parser rejects them.
pub(super) fn has_suffix(p: &Parser<'_, '_>) -> bool {
    let Some(token) = p.current_lexer_token() else {
        return false;
    };
    match token.kind {
        rue_rust_lexer::TokenKind::Literal { suffix_start, .. } => suffix_start < token.len,
        _ => false,
    }
}

fn atom_expr(p: &mut Parser<'_, '_>, r: Restrictions) -> Option<(CompletedMarker, BlockLike)> {
    if let Some(m) = literal(p) {
        return Some((m, BlockLike::NotBlock));
    }
    // Contextual keywords that start expressions are identifiers, so they
    // are checked before paths.
    if p.at_contextual_kw(T![gen])
        && (p.nth(1) == T!['{'] || (p.nth(1) == T![move] && p.nth(2) == T!['{']))
    {
        let m = p.start();
        p.bump_remap(T![gen]);
        p.eat(T![move]);
        stmt_list(p);
        return Some((m.complete(p, BLOCK_EXPR), BlockLike::Block));
    }
    if paths::is_path_start(p) {
        return Some(path_expr(p, r));
    }

    let la = p.nth(1);
    let m = match p.current() {
        T!['('] => tuple_expr(p),
        T!['['] => array_expr(p),
        T![|] => closure_expr(p, r),
        T![move] if la == T![|] => closure_expr(p, r),
        T![static] if matches!(la, T![|] | T![move] | T![async]) => closure_expr(p, r),
        T![async] if la == T![|] || (la == T![move] && p.nth(2) == T![|]) => closure_expr(p, r),
        T![for] if la == T![<] => closure_expr(p, r),
        T![async] | T![unsafe] | T![const] | T![try] if la == T!['{'] || la == T![move] => {
            let m = p.start();
            p.bump_any();
            if p.at_contextual_kw(T![gen]) {
                p.bump_remap(T![gen]);
            }
            p.eat(T![move]);
            if p.at(T!['{']) {
                stmt_list(p);
            } else {
                p.error("expected a block");
            }
            m.complete(p, BLOCK_EXPR)
        }
        T![async] if p.nth_at_contextual_kw(1, T![gen]) => {
            let m = p.start();
            p.bump(T![async]);
            p.bump_remap(T![gen]);
            p.eat(T![move]);
            block_or_error(p);
            m.complete(p, BLOCK_EXPR)
        }
        T!['{'] => {
            let m = p.start();
            stmt_list(p);
            m.complete(p, BLOCK_EXPR)
        }
        T![if] => if_expr(p),
        T![let] => {
            if !r.allow_let {
                p.error("expected expression, found `let` statement");
            }
            let_expr(p, r)
        }
        T![_] => {
            let m = p.start();
            p.bump(T![_]);
            m.complete(p, UNDERSCORE_EXPR)
        }
        T![loop] | T![while] | T![for] => loop_like(p, None),
        LIFETIME_IDENT if p.nth_at(1, T![:]) => {
            let m = p.start();
            let label = p.start();
            lifetime(p);
            p.bump(T![:]);
            label.complete(p, LABEL);
            match p.current() {
                T![loop] | T![while] | T![for] => loop_like(p, Some(m)),
                T!['{'] => {
                    stmt_list(p);
                    m.complete(p, BLOCK_EXPR)
                }
                _ => {
                    p.error("expected a loop or block after a label");
                    m.complete(p, ERROR)
                }
            }
        }
        T![match] => match_expr(p),
        T![return] => jump_expr(p, r, RETURN_EXPR),
        T![become] => jump_expr(p, r, BECOME_EXPR),
        T![yield] => jump_expr(p, r, YIELD_EXPR),
        T![do] if p.nth_at_contextual_kw(1, T![yeet]) => {
            let m = p.start();
            p.bump(T![do]);
            p.bump_remap(T![yeet]);
            if p.at_ts(EXPR_FIRST) {
                expr_bp(p, None, r.operand(), 1);
            }
            m.complete(p, YEET_EXPR)
        }
        T![break] => {
            let m = p.start();
            p.bump(T![break]);
            if p.at(LIFETIME_IDENT) {
                lifetime(p);
            }
            if p.at_ts(EXPR_FIRST) && !(r.forbid_structs && p.at(T!['{'])) {
                expr_bp(p, None, r.operand(), 1);
            }
            m.complete(p, BREAK_EXPR)
        }
        T![continue] => {
            let m = p.start();
            p.bump(T![continue]);
            if p.at(LIFETIME_IDENT) {
                lifetime(p);
            }
            m.complete(p, CONTINUE_EXPR)
        }
        _ => {
            p.err_recover("expected an expression", EXPR_RECOVERY);
            return None;
        }
    };
    Some((m, BlockLike::of(m.kind())))
}

fn jump_expr(p: &mut Parser<'_, '_>, r: Restrictions, kind: SyntaxKind) -> CompletedMarker {
    let m = p.start();
    p.bump_any();
    if p.at_ts(EXPR_FIRST) && !(r.forbid_structs && p.at(T!['{'])) {
        expr_bp(p, None, r.operand(), 1);
    }
    m.complete(p, kind)
}

fn path_expr(p: &mut Parser<'_, '_>, r: Restrictions) -> (CompletedMarker, BlockLike) {
    let m = p.start();
    let path = paths::expr_path(p);
    match p.current() {
        T!['{'] if !r.forbid_structs => {
            record_expr_field_list(p);
            (m.complete(p, RECORD_EXPR), BlockLike::NotBlock)
        }
        T![!] if !p.at(T![!=]) => {
            let call = path.precede(p);
            p.bump(T![!]);
            // `m! { .. }` at statement start is a statement, like a block.
            let blocklike = if p.at(T!['{']) {
                BlockLike::Block
            } else {
                BlockLike::NotBlock
            };
            if p.current().closing_delimiter().is_some() {
                token_tree(p);
            } else {
                p.error("expected a macro argument list");
            }
            call.complete(p, MACRO_CALL);
            (m.complete(p, MACRO_EXPR), blocklike)
        }
        _ => (m.complete(p, PATH_EXPR), BlockLike::NotBlock),
    }
}

pub(super) fn record_expr_field_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        comma_separated(p, |p| {
            let field = p.start();
            attributes::outer_attrs(p);
            match p.current() {
                T![.] if p.at(T![..]) => {
                    field.abandon(p);
                    p.bump(T![..]);
                    // `..base`, or a bare `..` for default field values.
                    if p.at_ts(EXPR_FIRST) {
                        expr(p);
                    }
                }
                IDENT | INT_NUMBER if p.nth_at(1, T![:]) => {
                    name_ref_or_index(p);
                    p.bump(T![:]);
                    expr(p);
                    field.complete(p, RECORD_EXPR_FIELD);
                }
                IDENT => {
                    // Shorthand: `S { x }`.
                    name_ref(p);
                    field.complete(p, RECORD_EXPR_FIELD);
                }
                _ => {
                    field.abandon(p);
                    p.err_and_bump("expected a field");
                }
            }
        })
    });
    m.complete(p, RECORD_EXPR_FIELD_LIST);
}

fn tuple_expr(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    let mut n_exprs = 0;
    let mut trailing_comma = false;
    p.group(|p| {
        while !p.at_eof() {
            if expr(p).is_none() {
                break;
            }
            n_exprs += 1;
            trailing_comma = false;
            if p.at_eof() || !p.expect(T![,]) {
                break;
            }
            trailing_comma = true;
        }
    });
    let kind = if n_exprs == 1 && !trailing_comma {
        PAREN_EXPR
    } else {
        TUPLE_EXPR
    };
    m.complete(p, kind)
}

fn array_expr(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.group(|p| {
        if p.at_eof() {
            return;
        }
        expr(p);
        if p.eat(T![;]) {
            expr(p);
            return;
        }
        if p.eat(T![,]) {
            comma_separated(p, |p| {
                expr(p);
            });
        }
    });
    m.complete(p, ARRAY_EXPR)
}

fn closure_expr(p: &mut Parser<'_, '_>, r: Restrictions) -> CompletedMarker {
    let m = p.start();
    if p.at(T![for]) {
        generics::for_binder(p);
    }
    while matches!(p.current(), T![static] | T![async] | T![move] | T![const]) {
        p.bump_any();
    }
    if p.at(T![||]) {
        let params = p.start();
        p.bump(T![||]);
        params.complete(p, PARAM_LIST);
    } else if p.at(T![|]) {
        closure_param_list(p);
    } else {
        p.error("expected `|`");
    }
    if opt_ret_type(p) {
        // With an explicit return type the body must be a block.
        block_expr(p);
    } else if expr_bp(p, None, r.operand(), 1).is_none() {
        p.error("expected a closure body");
    }
    m.complete(p, CLOSURE_EXPR)
}

fn closure_param_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(T![|]);
    while !p.at(T![|]) && !p.at_eof() {
        let param = p.start();
        attributes::outer_attrs(p);
        patterns::pattern_single(p);
        if p.eat(T![:]) {
            types::type_(p);
        }
        param.complete(p, PARAM);
        if !p.at(T![|]) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T![|]);
    m.complete(p, PARAM_LIST);
}

fn if_expr(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(T![if]);
    expr_bp(p, None, CONDITION, 1);
    block_or_error(p);
    if p.eat(T![else]) {
        if p.at(T![if]) {
            if_expr(p);
        } else {
            block_or_error(p);
        }
    }
    m.complete(p, IF_EXPR)
}

fn let_expr(p: &mut Parser<'_, '_>, r: Restrictions) -> CompletedMarker {
    let m = p.start();
    p.bump(T![let]);
    patterns::pattern_top(p);
    p.expect(T![=]);
    expr_bp(p, None, r.operand(), LET_SCRUTINEE_BP);
    m.complete(p, LET_EXPR)
}

/// `loop`, `while` and `for`, with an optional label already parsed into
/// `m`.
fn loop_like(p: &mut Parser<'_, '_>, m: Option<Marker>) -> CompletedMarker {
    let m = m.unwrap_or_else(|| p.start());
    let kind = match p.current() {
        T![loop] => {
            p.bump(T![loop]);
            LOOP_EXPR
        }
        T![while] => {
            p.bump(T![while]);
            expr_bp(p, None, CONDITION, 1);
            WHILE_EXPR
        }
        _ => {
            p.bump(T![for]);
            patterns::pattern_top(p);
            p.expect(T![in]);
            expr_no_struct(p);
            FOR_EXPR
        }
    };
    block_or_error(p);
    m.complete(p, kind)
}

fn match_expr(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(T![match]);
    expr_no_struct(p);
    if p.at(T!['{']) {
        match_arm_list(p);
    } else {
        p.error("expected match arms");
    }
    m.complete(p, MATCH_EXPR)
}

pub(super) fn match_arm_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        attributes::inner_attrs(p);
        while !p.at_eof() {
            let start = p.pos();
            match_arm(p);
            if p.pos() == start {
                p.err_and_bump("expected a match arm");
            }
        }
    });
    m.complete(p, MATCH_ARM_LIST);
}

fn match_arm(p: &mut Parser<'_, '_>) {
    let m = p.start();
    attributes::outer_attrs(p);
    let pattern_start = p.pos();
    patterns::pattern_top_r(p, TokenSet::new(&[T![=], T![if], T![,]]));
    if p.at(T![if]) {
        let guard = p.start();
        p.bump(T![if]);
        let r = Restrictions {
            allow_let: true,
            ..Restrictions::default()
        };
        if expr_bp(p, None, r, 1).is_none() {
            p.error("expected a guard expression");
        }
        guard.complete(p, MATCH_GUARD);
    }
    // An arm may omit its body for never patterns (`Err(!)`). rustc's parser
    // accepts a body-less arm at the end of the list, or before a comma when
    // the pattern could be a never pattern (contains `!`); later phases
    // reject the rest.
    if p.at_eof() || (p.at(T![,]) && p.consumed_since(pattern_start, T![!])) {
        p.eat(T![,]);
        m.complete(p, MATCH_ARM);
        return;
    }
    p.expect(T![=>]);
    let blocklike = match expr_stmt(p, None) {
        Some((_, blocklike)) => blocklike,
        None => BlockLike::NotBlock,
    };
    if !p.eat(T![,]) && !blocklike.is_block() && !p.at_eof() {
        p.error("expected `,`");
    }
    m.complete(p, MATCH_ARM);
}

fn block_or_error(p: &mut Parser<'_, '_>) {
    if block_expr(p).is_none() {
        p.error("expected a block");
    }
}

pub(super) fn block_expr(p: &mut Parser<'_, '_>) -> Option<CompletedMarker> {
    if !p.at(T!['{']) {
        return None;
    }
    let m = p.start();
    stmt_list(p);
    Some(m.complete(p, BLOCK_EXPR))
}

/// A function body, which may be deferred and parsed in parallel.
pub(super) fn block_expr_deferrable(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.defer_or(STMT_LIST, stmt_list);
    m.complete(p, BLOCK_EXPR);
}

pub(super) fn stmt_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.group(|p| {
        attributes::inner_attrs(p);
        while !p.at_eof() {
            let start = p.pos();
            stmt(p);
            if p.pos() == start {
                p.err_and_bump("expected a statement");
            }
        }
    });
    m.complete(p, STMT_LIST);
}

fn stmt(p: &mut Parser<'_, '_>) {
    let m = p.start();
    attributes::outer_attrs(p);
    if p.at(T![let]) {
        let_stmt(p, m);
        return;
    }
    if p.at(T![;]) {
        m.abandon(p);
        p.bump(T![;]);
        return;
    }
    let m = match items::opt_item(p, m) {
        Ok(()) => return,
        Err(m) => m,
    };
    if let Some((cm, blocklike)) = expr_stmt(p, Some(m)) {
        // A final expression without `;` is the block's value, not a
        // statement.
        if !p.at_eof() {
            let m = cm.precede(p);
            if blocklike.is_block() {
                p.eat(T![;]);
            } else {
                p.expect(T![;]);
            }
            m.complete(p, EXPR_STMT);
        }
    }
}

fn let_stmt(p: &mut Parser<'_, '_>, m: Marker) {
    p.bump(T![let]);
    patterns::pattern_top(p);
    if p.eat(T![:]) {
        types::type_(p);
    }
    if p.eat(T![=]) {
        if expr(p).is_none() {
            p.error("expected an initializer");
        }
        if p.at(T![else]) {
            let else_branch = p.start();
            p.bump(T![else]);
            block_or_error(p);
            else_branch.complete(p, LET_ELSE);
        }
    }
    p.expect(T![;]);
    m.complete(p, LET_STMT);
}
