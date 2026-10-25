//! The kinds of syntax tree tokens and nodes.

/// Defines [`SyntaxKind`] and its classification tables from one list, so a
/// kind's spelling, keyword status and position in the enum cannot drift
/// apart.
macro_rules! syntax_kinds {
    (
        punct: [$($punct:ident = $punct_text:literal,)*],
        keywords: [$($kw:ident = $kw_text:literal,)*],
        contextual_keywords: [$($ckw:ident = $ckw_text:literal,)*],
        literals: [$($lit:ident,)*],
        tokens: [$($tok:ident,)*],
        nodes: [$($node:ident,)*],
    ) => {
        /// The kind of a token or node in the syntax tree.
        ///
        /// Token kinds come first so that [`crate::TokenSet`] can represent
        /// any set of tokens as a fixed-size bit set.
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u16)]
        pub enum SyntaxKind {
            /// The kind of an abandoned node marker; never in a finished tree.
            TOMBSTONE,
            /// The parser's view past the end of its input; never in a tree.
            EOF,
            $($punct,)*
            $($kw,)*
            $($ckw,)*
            $($lit,)*
            $($tok,)*
            $($node,)*
        }

        use SyntaxKind::*;

        impl SyntaxKind {
            /// The fixed spelling of a punctuation or keyword token.
            pub fn static_text(self) -> Option<&'static str> {
                match self {
                    $($punct => Some($punct_text),)*
                    $($kw => Some($kw_text),)*
                    $($ckw => Some($ckw_text),)*
                    _ => None,
                }
            }

            pub fn is_punct(self) -> bool {
                matches!(self, $($punct)|*)
            }

            /// Strict and reserved keywords, in any edition.
            pub fn is_keyword(self) -> bool {
                matches!(self, $($kw)|*)
            }

            /// Keywords that are identifiers outside a specific position, such
            /// as `union` or `default`.
            pub fn is_contextual_keyword(self) -> bool {
                matches!(self, $($ckw)|*)
            }

            pub fn is_literal(self) -> bool {
                matches!(self, $($lit)|*)
            }

            fn keyword_from_text(text: &str) -> Option<SyntaxKind> {
                match text {
                    $($kw_text => Some($kw),)*
                    _ => None,
                }
            }

            /// The contextual keyword spelled `text`, if any.
            pub fn contextual_keyword_from_text(text: &str) -> Option<SyntaxKind> {
                match text {
                    $($ckw_text => Some($ckw),)*
                    _ => None,
                }
            }
        }
    };
}

syntax_kinds! {
    punct: [
        SEMICOLON = ";",
        COMMA = ",",
        L_PAREN = "(",
        R_PAREN = ")",
        L_CURLY = "{",
        R_CURLY = "}",
        L_BRACK = "[",
        R_BRACK = "]",
        L_ANGLE = "<",
        R_ANGLE = ">",
        AT = "@",
        POUND = "#",
        TILDE = "~",
        QUESTION = "?",
        DOLLAR = "$",
        AMP = "&",
        PIPE = "|",
        PLUS = "+",
        STAR = "*",
        SLASH = "/",
        CARET = "^",
        PERCENT = "%",
        UNDERSCORE = "_",
        DOT = ".",
        COLON = ":",
        EQ = "=",
        BANG = "!",
        MINUS = "-",
        // Composite punctuation. The lexer never produces these; the parser
        // glues adjacent single-character tokens into them.
        DOT2 = "..",
        DOT3 = "...",
        DOT2EQ = "..=",
        COLON2 = "::",
        FAT_ARROW = "=>",
        THIN_ARROW = "->",
        EQ2 = "==",
        NEQ = "!=",
        LTEQ = "<=",
        GTEQ = ">=",
        AMP2 = "&&",
        PIPE2 = "||",
        PLUSEQ = "+=",
        MINUSEQ = "-=",
        STAREQ = "*=",
        SLASHEQ = "/=",
        PERCENTEQ = "%=",
        CARETEQ = "^=",
        AMPEQ = "&=",
        PIPEEQ = "|=",
        SHL = "<<",
        SHR = ">>",
        SHLEQ = "<<=",
        SHREQ = ">>=",
    ],
    keywords: [
        ABSTRACT_KW = "abstract",
        AS_KW = "as",
        ASYNC_KW = "async",
        AWAIT_KW = "await",
        BECOME_KW = "become",
        BOX_KW = "box",
        BREAK_KW = "break",
        CONST_KW = "const",
        CONTINUE_KW = "continue",
        CRATE_KW = "crate",
        DO_KW = "do",
        DYN_KW = "dyn",
        ELSE_KW = "else",
        ENUM_KW = "enum",
        EXTERN_KW = "extern",
        FALSE_KW = "false",
        FINAL_KW = "final",
        FN_KW = "fn",
        FOR_KW = "for",
        IF_KW = "if",
        IMPL_KW = "impl",
        IN_KW = "in",
        LET_KW = "let",
        LOOP_KW = "loop",
        MACRO_KW = "macro",
        MATCH_KW = "match",
        MOD_KW = "mod",
        MOVE_KW = "move",
        MUT_KW = "mut",
        OVERRIDE_KW = "override",
        PRIV_KW = "priv",
        PUB_KW = "pub",
        REF_KW = "ref",
        RETURN_KW = "return",
        SELF_KW = "self",
        SELF_TYPE_KW = "Self",
        STATIC_KW = "static",
        STRUCT_KW = "struct",
        SUPER_KW = "super",
        TRAIT_KW = "trait",
        TRUE_KW = "true",
        TRY_KW = "try",
        TYPE_KW = "type",
        TYPEOF_KW = "typeof",
        UNSAFE_KW = "unsafe",
        UNSIZED_KW = "unsized",
        USE_KW = "use",
        VIRTUAL_KW = "virtual",
        WHERE_KW = "where",
        WHILE_KW = "while",
        YIELD_KW = "yield",
    ],
    contextual_keywords: [
        AUTO_KW = "auto",
        DEFAULT_KW = "default",
        GEN_KW = "gen",
        MACRO_RULES_KW = "macro_rules",
        RAW_KW = "raw",
        SAFE_KW = "safe",
        UNION_KW = "union",
        YEET_KW = "yeet",
    ],
    literals: [
        INT_NUMBER,
        FLOAT_NUMBER,
        CHAR,
        BYTE,
        STRING,
        BYTE_STRING,
        C_STRING,
    ],
    tokens: [
        // An unknown character or an unmatched closing delimiter. Also the
        // kind of a node wrapping tokens the parser could not use.
        ERROR,
        IDENT,
        LIFETIME_IDENT,
        WHITESPACE,
        COMMENT,
        SHEBANG,
    ],
    nodes: [
        SOURCE_FILE,

        // Items.
        CONST,
        ENUM,
        EXTERN_BLOCK,
        EXTERN_CRATE,
        FN,
        IMPL,
        MACRO_CALL,
        MACRO_DEF,
        MACRO_RULES,
        MODULE,
        STATIC,
        STRUCT,
        TRAIT,
        TRAIT_ALIAS,
        TYPE_ALIAS,
        UNION,
        USE,

        // Item components.
        ABI,
        ASSOC_ITEM_LIST,
        EXTERN_ITEM_LIST,
        ITEM_LIST,
        NAME,
        NAME_REF,
        PARAM,
        PARAM_LIST,
        RECORD_FIELD,
        RECORD_FIELD_LIST,
        RENAME,
        RET_TYPE,
        SELF_PARAM,
        TUPLE_FIELD,
        TUPLE_FIELD_LIST,
        USE_TREE,
        USE_TREE_LIST,
        VARIANT,
        VARIANT_LIST,
        VISIBILITY,

        // Attributes and macros.
        ATTR,
        META,
        TOKEN_TREE,

        // Generics.
        CONST_PARAM,
        GENERIC_PARAM_LIST,
        LIFETIME,
        LIFETIME_PARAM,
        TYPE_BOUND,
        TYPE_BOUND_LIST,
        TYPE_PARAM,
        USE_BOUND_GENERIC_ARGS,
        WHERE_CLAUSE,
        WHERE_PRED,

        // Paths.
        ASSOC_TYPE_ARG,
        CONST_ARG,
        GENERIC_ARG_LIST,
        LIFETIME_ARG,
        PATH,
        PATH_SEGMENT,
        TYPE_ARG,

        // Types.
        ARRAY_TYPE,
        DYN_TRAIT_TYPE,
        FN_PTR_TYPE,
        FOR_TYPE,
        IMPL_TRAIT_TYPE,
        INFER_TYPE,
        MACRO_TYPE,
        NEVER_TYPE,
        PAREN_TYPE,
        PATH_TYPE,
        PTR_TYPE,
        REF_TYPE,
        SLICE_TYPE,
        TUPLE_TYPE,

        // Patterns.
        BOX_PAT,
        CONST_BLOCK_PAT,
        IDENT_PAT,
        LITERAL_PAT,
        MACRO_PAT,
        NEVER_PAT,
        OR_PAT,
        PAREN_PAT,
        PATH_PAT,
        RANGE_PAT,
        RECORD_PAT,
        RECORD_PAT_FIELD,
        RECORD_PAT_FIELD_LIST,
        REF_PAT,
        REST_PAT,
        SLICE_PAT,
        TUPLE_PAT,
        TUPLE_STRUCT_PAT,
        WILDCARD_PAT,

        // Expressions.
        ARG_LIST,
        ARRAY_EXPR,
        AWAIT_EXPR,
        BECOME_EXPR,
        BIN_EXPR,
        BLOCK_EXPR,
        BOX_EXPR,
        BREAK_EXPR,
        CALL_EXPR,
        CAST_EXPR,
        CLOSURE_EXPR,
        CONTINUE_EXPR,
        FIELD_EXPR,
        FOR_EXPR,
        IF_EXPR,
        INDEX_EXPR,
        LABEL,
        LET_EXPR,
        LITERAL,
        LOOP_EXPR,
        MACRO_EXPR,
        MATCH_ARM,
        MATCH_ARM_LIST,
        MATCH_EXPR,
        MATCH_GUARD,
        METHOD_CALL_EXPR,
        PAREN_EXPR,
        PATH_EXPR,
        PREFIX_EXPR,
        RANGE_EXPR,
        RECORD_EXPR,
        RECORD_EXPR_FIELD,
        RECORD_EXPR_FIELD_LIST,
        REF_EXPR,
        RETURN_EXPR,
        STMT_LIST,
        TRY_EXPR,
        TUPLE_EXPR,
        UNDERSCORE_EXPR,
        WHILE_EXPR,
        YEET_EXPR,
        YIELD_EXPR,

        // Statements.
        EXPR_STMT,
        LET_ELSE,
        LET_STMT,
    ],
}

impl SyntaxKind {
    /// Whitespace, comments and the shebang line.
    pub fn is_trivia(self) -> bool {
        matches!(self, WHITESPACE | COMMENT | SHEBANG)
    }

    /// Whether the kind is a token rather than a node.
    pub fn is_token(self) -> bool {
        self < SOURCE_FILE
    }

    /// The keyword spelled `text` in `edition`, if any. Contextual keywords
    /// are not included: they are identifiers until the parser remaps them.
    pub fn from_keyword(text: &str, edition: Edition) -> Option<SyntaxKind> {
        let kind = SyntaxKind::keyword_from_text(text)?;
        match kind {
            // Keywords introduced by the 2018 edition.
            ASYNC_KW | AWAIT_KW | DYN_KW | TRY_KW if edition < Edition::Edition2018 => None,
            _ => Some(kind),
        }
    }

    /// The closing delimiter matching an opening one.
    pub fn closing_delimiter(self) -> Option<SyntaxKind> {
        match self {
            L_PAREN => Some(R_PAREN),
            L_BRACK => Some(R_BRACK),
            L_CURLY => Some(R_CURLY),
            _ => None,
        }
    }

    pub(crate) fn is_closing_delimiter(self) -> bool {
        matches!(self, R_PAREN | R_BRACK | R_CURLY)
    }
}

/// A Rust edition. Editions change which identifiers are keywords and which
/// lexical forms are reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Edition {
    Edition2015,
    Edition2018,
    Edition2021,
    #[default]
    Edition2024,
}

/// Spells a token kind by its text: `T![fn]`, `T![::]`, `T!['{']`.
#[macro_export]
macro_rules! T {
    [;] => { $crate::SyntaxKind::SEMICOLON };
    [,] => { $crate::SyntaxKind::COMMA };
    ['('] => { $crate::SyntaxKind::L_PAREN };
    [')'] => { $crate::SyntaxKind::R_PAREN };
    ['{'] => { $crate::SyntaxKind::L_CURLY };
    ['}'] => { $crate::SyntaxKind::R_CURLY };
    ['['] => { $crate::SyntaxKind::L_BRACK };
    [']'] => { $crate::SyntaxKind::R_BRACK };
    [<] => { $crate::SyntaxKind::L_ANGLE };
    [>] => { $crate::SyntaxKind::R_ANGLE };
    [@] => { $crate::SyntaxKind::AT };
    [#] => { $crate::SyntaxKind::POUND };
    [~] => { $crate::SyntaxKind::TILDE };
    [?] => { $crate::SyntaxKind::QUESTION };
    [$] => { $crate::SyntaxKind::DOLLAR };
    [&] => { $crate::SyntaxKind::AMP };
    [|] => { $crate::SyntaxKind::PIPE };
    [+] => { $crate::SyntaxKind::PLUS };
    [*] => { $crate::SyntaxKind::STAR };
    [/] => { $crate::SyntaxKind::SLASH };
    [^] => { $crate::SyntaxKind::CARET };
    [%] => { $crate::SyntaxKind::PERCENT };
    [_] => { $crate::SyntaxKind::UNDERSCORE };
    [.] => { $crate::SyntaxKind::DOT };
    [:] => { $crate::SyntaxKind::COLON };
    [=] => { $crate::SyntaxKind::EQ };
    [!] => { $crate::SyntaxKind::BANG };
    [-] => { $crate::SyntaxKind::MINUS };
    [..] => { $crate::SyntaxKind::DOT2 };
    [...] => { $crate::SyntaxKind::DOT3 };
    [..=] => { $crate::SyntaxKind::DOT2EQ };
    [::] => { $crate::SyntaxKind::COLON2 };
    [=>] => { $crate::SyntaxKind::FAT_ARROW };
    [->] => { $crate::SyntaxKind::THIN_ARROW };
    [==] => { $crate::SyntaxKind::EQ2 };
    [!=] => { $crate::SyntaxKind::NEQ };
    [<=] => { $crate::SyntaxKind::LTEQ };
    [>=] => { $crate::SyntaxKind::GTEQ };
    [&&] => { $crate::SyntaxKind::AMP2 };
    [||] => { $crate::SyntaxKind::PIPE2 };
    [+=] => { $crate::SyntaxKind::PLUSEQ };
    [-=] => { $crate::SyntaxKind::MINUSEQ };
    [*=] => { $crate::SyntaxKind::STAREQ };
    [/=] => { $crate::SyntaxKind::SLASHEQ };
    [%=] => { $crate::SyntaxKind::PERCENTEQ };
    [^=] => { $crate::SyntaxKind::CARETEQ };
    [&=] => { $crate::SyntaxKind::AMPEQ };
    [|=] => { $crate::SyntaxKind::PIPEEQ };
    [<<] => { $crate::SyntaxKind::SHL };
    [>>] => { $crate::SyntaxKind::SHR };
    [<<=] => { $crate::SyntaxKind::SHLEQ };
    [>>=] => { $crate::SyntaxKind::SHREQ };
    [as] => { $crate::SyntaxKind::AS_KW };
    [async] => { $crate::SyntaxKind::ASYNC_KW };
    [await] => { $crate::SyntaxKind::AWAIT_KW };
    [become] => { $crate::SyntaxKind::BECOME_KW };
    [box] => { $crate::SyntaxKind::BOX_KW };
    [break] => { $crate::SyntaxKind::BREAK_KW };
    [const] => { $crate::SyntaxKind::CONST_KW };
    [continue] => { $crate::SyntaxKind::CONTINUE_KW };
    [crate] => { $crate::SyntaxKind::CRATE_KW };
    [do] => { $crate::SyntaxKind::DO_KW };
    [dyn] => { $crate::SyntaxKind::DYN_KW };
    [else] => { $crate::SyntaxKind::ELSE_KW };
    [enum] => { $crate::SyntaxKind::ENUM_KW };
    [extern] => { $crate::SyntaxKind::EXTERN_KW };
    [false] => { $crate::SyntaxKind::FALSE_KW };
    [fn] => { $crate::SyntaxKind::FN_KW };
    [for] => { $crate::SyntaxKind::FOR_KW };
    [if] => { $crate::SyntaxKind::IF_KW };
    [impl] => { $crate::SyntaxKind::IMPL_KW };
    [in] => { $crate::SyntaxKind::IN_KW };
    [let] => { $crate::SyntaxKind::LET_KW };
    [loop] => { $crate::SyntaxKind::LOOP_KW };
    [macro] => { $crate::SyntaxKind::MACRO_KW };
    [match] => { $crate::SyntaxKind::MATCH_KW };
    [mod] => { $crate::SyntaxKind::MOD_KW };
    [move] => { $crate::SyntaxKind::MOVE_KW };
    [mut] => { $crate::SyntaxKind::MUT_KW };
    [pub] => { $crate::SyntaxKind::PUB_KW };
    [ref] => { $crate::SyntaxKind::REF_KW };
    [return] => { $crate::SyntaxKind::RETURN_KW };
    [self] => { $crate::SyntaxKind::SELF_KW };
    [Self] => { $crate::SyntaxKind::SELF_TYPE_KW };
    [static] => { $crate::SyntaxKind::STATIC_KW };
    [struct] => { $crate::SyntaxKind::STRUCT_KW };
    [super] => { $crate::SyntaxKind::SUPER_KW };
    [trait] => { $crate::SyntaxKind::TRAIT_KW };
    [true] => { $crate::SyntaxKind::TRUE_KW };
    [try] => { $crate::SyntaxKind::TRY_KW };
    [type] => { $crate::SyntaxKind::TYPE_KW };
    [unsafe] => { $crate::SyntaxKind::UNSAFE_KW };
    [use] => { $crate::SyntaxKind::USE_KW };
    [where] => { $crate::SyntaxKind::WHERE_KW };
    [while] => { $crate::SyntaxKind::WHILE_KW };
    [yield] => { $crate::SyntaxKind::YIELD_KW };
    [auto] => { $crate::SyntaxKind::AUTO_KW };
    [default] => { $crate::SyntaxKind::DEFAULT_KW };
    [gen] => { $crate::SyntaxKind::GEN_KW };
    [macro_rules] => { $crate::SyntaxKind::MACRO_RULES_KW };
    [raw] => { $crate::SyntaxKind::RAW_KW };
    [safe] => { $crate::SyntaxKind::SAFE_KW };
    [union] => { $crate::SyntaxKind::UNION_KW };
    [yeet] => { $crate::SyntaxKind::YEET_KW };
    [ident] => { $crate::SyntaxKind::IDENT };
    [lifetime_ident] => { $crate::SyntaxKind::LIFETIME_IDENT };
}
