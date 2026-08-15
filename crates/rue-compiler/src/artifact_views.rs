//! Immutable, owner-retaining projections of canonical compiler artifacts.
//!
//! These views resolve strings and expose checked structural traversal. They
//! do not reveal phase storage, interners, type pools, owner-relative handles,
//! or the phases' textual debug formats.

use std::{borrow::Cow, fmt, sync::Arc};

use crate::{CanonicalRirOutput, ParsedProgram};

/// Opaque import-discovery product retained by the session.
#[derive(Debug, Clone)]
pub struct ImportDiscoveryView {
    pub(crate) inner: Arc<crate::session::ImportDiscoveryRevisionArtifact>,
}

/// Read-only status of an import-discovery product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportDiscoveryStatus {
    Open,
    ClosedAttempted,
    ClosedValid,
}

impl ImportDiscoveryView {
    pub(crate) fn new(inner: Arc<crate::session::ImportDiscoveryRevisionArtifact>) -> Self {
        Self { inner }
    }

    pub fn status(&self) -> ImportDiscoveryStatus {
        match self.inner.status() {
            crate::session::ImportDiscoveryRevisionStatus::Open => ImportDiscoveryStatus::Open,
            crate::session::ImportDiscoveryRevisionStatus::ClosedAttempted => {
                ImportDiscoveryStatus::ClosedAttempted
            }
            crate::session::ImportDiscoveryRevisionStatus::ClosedValid => {
                ImportDiscoveryStatus::ClosedValid
            }
        }
    }

    pub fn source_revision(&self) -> &crate::SourceRevision {
        self.inner.source_revision()
    }

    pub fn diagnostics(&self) -> &crate::CompileErrors {
        self.inner.diagnostics()
    }

    pub fn diagnostic_snapshot(&self) -> Option<&Arc<crate::FrontendDiagnosticSnapshot>> {
        self.inner.diagnostic_snapshot()
    }
}

#[derive(Clone)]
#[allow(dead_code)] // Variant payloads deliberately pin the authorizing owner.
enum SourceLocationOwner {
    Syntax(Arc<crate::parsed_modules::ParsedModule>),
    Rir(Arc<CanonicalRirOutput>),
}

/// Opaque exact identity for a source retained by a compiler artifact.
#[derive(Clone)]
pub struct SourceIdentityView {
    owner: SourceLocationOwner,
    source: crate::ModuleRevision,
}

impl SourceIdentityView {
    pub fn same_source(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl fmt::Debug for SourceIdentityView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _retain_owner = &self.owner;
        formatter.write_str("SourceIdentityView(<opaque>)")
    }
}

/// Validated byte bounds and opaque source identity retained by an artifact.
#[derive(Clone)]
pub struct SourceLocationView {
    owner: SourceLocationOwner,
    source: crate::ModuleRevision,
    start: u32,
    end: u32,
    source_length: u32,
}

impl SourceLocationView {
    fn syntax(owner: Arc<crate::parsed_modules::ParsedModule>, span: crate::Span) -> Self {
        let source_length = u32::try_from(owner.source_text().len())
            .expect("parsed source length is bounded by SourceSnapshot");
        debug_assert_eq!(span.file_id, owner.file_id());
        debug_assert!(span.start <= span.end && span.end <= source_length);
        Self {
            source: owner.revision().clone(),
            owner: SourceLocationOwner::Syntax(owner),
            start: span.start,
            end: span.end,
            source_length,
        }
    }

    fn rir(owner: Arc<CanonicalRirOutput>, span: crate::Span) -> Self {
        let (source, source_length) = owner.source_identity_and_length(span.file_id);
        let source = source.clone();
        debug_assert!(span.start <= span.end && span.end <= source_length);
        Self {
            source,
            owner: SourceLocationOwner::Rir(owner),
            start: span.start,
            end: span.end,
            source_length,
        }
    }

    pub fn source_identity(&self) -> SourceIdentityView {
        SourceIdentityView {
            owner: self.owner.clone(),
            source: self.source.clone(),
        }
    }

    pub fn start(&self) -> u32 {
        self.start
    }

    pub fn end(&self) -> u32 {
        self.end
    }

    pub fn source_length(&self) -> u32 {
        self.source_length
    }

    pub fn is_valid(&self) -> bool {
        self.start <= self.end && self.end <= self.source_length
    }
}

impl fmt::Debug for SourceLocationView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceLocationView")
            .field("source", &self.source_identity())
            .field("start", &self.start)
            .field("end", &self.end)
            .field("source_length", &self.source_length)
            .finish()
    }
}

/// One resolved token tied to an immutable syntax owner.
#[derive(Clone)]
pub struct TokenView {
    owner: Arc<crate::parsed_modules::ParsedModule>,
    index: usize,
}

impl TokenView {
    fn token(&self) -> &rue_lexer::Token {
        &self.owner.tokens()[self.index]
    }

    pub fn kind(&self) -> &str {
        use rue_lexer::TokenKind::*;
        match self.token().kind {
            Fn => "FN",
            Let => "LET",
            Mut => "MUT",
            Inout => "INOUT",
            Borrow => "BORROW",
            If => "IF",
            Else => "ELSE",
            Match => "MATCH",
            While => "WHILE",
            Loop => "LOOP",
            Yield => "YIELD",
            For => "FOR",
            In => "IN",
            Break => "BREAK",
            Continue => "CONTINUE",
            Return => "RETURN",
            True => "TRUE",
            False => "FALSE",
            Struct => "STRUCT",
            Enum => "ENUM",
            Impl => "IMPL",
            Drop => "DROP",
            Linear => "LINEAR",
            SelfValue => "SELF",
            SelfType => "SELFTYPE",
            Comptime => "COMPTIME",
            Pub => "PUB",
            Const => "CONST",
            Checked => "CHECKED",
            Unchecked => "UNCHECKED",
            Ptr => "PTR",
            Extern => "EXTERN",
            I8 => "TYPE(i8)",
            I16 => "TYPE(i16)",
            I32 => "TYPE(i32)",
            I64 => "TYPE(i64)",
            U8 => "TYPE(u8)",
            U16 => "TYPE(u16)",
            U32 => "TYPE(u32)",
            U64 => "TYPE(u64)",
            Bool => "TYPE(bool)",
            Type => "TYPE(type)",
            Underscore => "UNDERSCORE",
            Int(_) => "INT",
            Float(_) => "FLOAT",
            String(_) => "STRING",
            Ident(_) => "IDENT",
            Plus => "PLUS",
            Minus => "MINUS",
            Star => "STAR",
            Slash => "SLASH",
            Percent => "PERCENT",
            Eq => "EQ",
            EqEq => "EQEQ",
            Bang => "BANG",
            BangEq => "BANGEQ",
            Lt => "LT",
            Gt => "GT",
            LtEq => "LTEQ",
            GtEq => "GTEQ",
            AmpAmp => "AMPAMP",
            PipePipe => "PIPEPIPE",
            Amp => "AMP",
            Pipe => "PIPE",
            Caret => "CARET",
            Tilde => "TILDE",
            LtLt => "LTLT",
            GtGt => "GTGT",
            PlusEq => "PLUSEQ",
            MinusEq => "MINUSEQ",
            StarEq => "STAREQ",
            SlashEq => "SLASHEQ",
            PercentEq => "PERCENTEQ",
            AmpEq => "AMPEQ",
            PipeEq => "PIPEEQ",
            CaretEq => "CARETEQ",
            LtLtEq => "LTLTEQ",
            GtGtEq => "GTGTEQ",
            LParen => "LPAREN",
            RParen => "RPAREN",
            LBrace => "LBRACE",
            RBrace => "RBRACE",
            LBracket => "LBRACKET",
            RBracket => "RBRACKET",
            Arrow => "ARROW",
            FatArrow => "FATARROW",
            ColonColon => "COLONCOLON",
            Colon => "COLON",
            Semi => "SEMI",
            Comma => "COMMA",
            Dot => "DOT",
            At => "AT",
            Question => "QUESTION",
            Eof => "EOF",
        }
    }

    pub fn value(&self) -> Option<Cow<'_, str>> {
        match self.token().kind {
            rue_lexer::TokenKind::Ident(symbol)
            | rue_lexer::TokenKind::String(symbol)
            | rue_lexer::TokenKind::Float(symbol) => {
                Some(Cow::Borrowed(self.owner.resolve_raw_symbol(symbol)))
            }
            rue_lexer::TokenKind::Int(value) => Some(Cow::Owned(value.to_string())),
            _ => None,
        }
    }

    pub fn start(&self) -> u32 {
        self.token().span.start
    }

    pub fn end(&self) -> u32 {
        self.token().span.end
    }

    pub fn location(&self) -> SourceLocationView {
        SourceLocationView::syntax(self.owner.clone(), self.token().span)
    }
}

impl fmt::Debug for TokenView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenView")
            .field("kind", &self.kind())
            .field("value", &self.value())
            .field("start", &self.start())
            .field("end", &self.end())
            .finish()
    }
}

struct SyntaxNodeRecord {
    kind: &'static str,
    span: crate::Span,
    name: Option<Arc<str>>,
    value: Option<Arc<str>>,
    children: Arc<[Arc<SyntaxNodeRecord>]>,
}

/// One owner-bound syntax node with resolved strings and checked children.
#[derive(Clone)]
pub struct SyntaxNodeView {
    owner: Arc<crate::parsed_modules::ParsedModule>,
    node: Arc<SyntaxNodeRecord>,
}

impl SyntaxNodeView {
    pub fn kind(&self) -> &str {
        self.node.kind
    }

    pub fn location(&self) -> SourceLocationView {
        SourceLocationView::syntax(self.owner.clone(), self.node.span)
    }

    pub fn name(&self) -> Option<&str> {
        self.node.name.as_deref()
    }

    pub fn value(&self) -> Option<&str> {
        self.node.value.as_deref()
    }

    pub fn child_count(&self) -> usize {
        self.node.children.len()
    }

    pub fn children(&self) -> impl ExactSizeIterator<Item = SyntaxNodeView> + '_ {
        self.node
            .children
            .iter()
            .cloned()
            .map(|node| SyntaxNodeView {
                owner: self.owner.clone(),
                node,
            })
    }
}

impl fmt::Debug for SyntaxNodeView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyntaxNodeView")
            .field("kind", &self.kind())
            .field("location", &self.location())
            .field("name", &self.name())
            .field("value", &self.value())
            .field("child_count", &self.child_count())
            .finish()
    }
}

/// One source module in a [`SyntaxView`].
#[derive(Clone)]
pub struct SyntaxModuleView {
    owner: Arc<crate::parsed_modules::ParsedModule>,
}

impl SyntaxModuleView {
    pub fn file_id(&self) -> crate::FileId {
        self.owner.file_id()
    }

    pub fn path(&self) -> &str {
        self.owner.physical_path()
    }

    pub fn source(&self) -> &str {
        self.owner.source_text()
    }

    pub fn tokens(&self) -> impl ExactSizeIterator<Item = TokenView> + '_ {
        (0..self.owner.tokens().len()).map(|index| TokenView {
            owner: self.owner.clone(),
            index,
        })
    }

    pub fn item_count(&self) -> usize {
        self.owner.ast().items.len()
    }

    pub fn nodes(&self) -> impl ExactSizeIterator<Item = SyntaxNodeView> + '_ {
        self.owner
            .ast()
            .items
            .iter()
            .map(|item| syntax_item_record(&self.owner, item))
            .map(|node| SyntaxNodeView {
                owner: self.owner.clone(),
                node,
            })
    }
}

impl fmt::Debug for SyntaxModuleView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyntaxModuleView")
            .field("file_id", &self.file_id())
            .field("path", &self.path())
            .field("token_count", &self.owner.tokens().len())
            .field("item_count", &self.item_count())
            .finish()
    }
}

/// Retainable syntax projection of the session's canonical parsed program.
#[derive(Clone)]
pub struct SyntaxView {
    owner: Arc<ParsedProgram>,
}

impl SyntaxView {
    pub(crate) fn new(owner: Arc<ParsedProgram>) -> Self {
        Self { owner }
    }

    pub fn modules(&self) -> impl ExactSizeIterator<Item = SyntaxModuleView> + '_ {
        self.owner
            .modules()
            .iter()
            .cloned()
            .map(|owner| SyntaxModuleView { owner })
    }

    pub fn source_revision(&self) -> &crate::SourceRevision {
        self.owner.source_revision()
    }

    #[allow(dead_code)]
    pub(crate) fn owner(&self) -> &Arc<ParsedProgram> {
        &self.owner
    }
}

impl fmt::Debug for SyntaxView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyntaxView")
            .field("source_revision", self.source_revision())
            .field("module_count", &self.owner.modules().len())
            .finish()
    }
}

/// One checked RIR instruction record.
#[derive(Clone)]
pub struct RirInstructionView {
    owner: Arc<CanonicalRirOutput>,
    ordinal: usize,
}

impl RirInstructionView {
    fn instruction(&self) -> &rue_rir::Inst {
        self.owner
            .rir()
            .get(rue_rir::InstRef::from_raw(self.ordinal as u32))
    }

    pub fn ordinal(&self) -> usize {
        self.ordinal
    }

    pub fn kind(&self) -> &str {
        rir_kind(&self.instruction().data)
    }

    pub fn location(&self) -> SourceLocationView {
        SourceLocationView::rir(self.owner.clone(), self.instruction().span)
    }

    pub fn operand_count(&self) -> usize {
        self.operands().len()
    }

    pub fn operands(&self) -> impl ExactSizeIterator<Item = RirOperandView> + '_ {
        rir_operands(self.owner.rir(), &self.instruction().data)
            .into_iter()
            .map(|operand| RirOperandView {
                owner: self.owner.clone(),
                role: operand.role,
                target: operand.target,
            })
    }
}

impl fmt::Debug for RirInstructionView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RirInstructionView")
            .field("ordinal", &self.ordinal())
            .field("kind", &self.kind())
            .field("location", &self.location())
            .field("operand_count", &self.operand_count())
            .finish()
    }
}

struct RirOperandRecord {
    role: &'static str,
    target: usize,
}

/// A checked reference from one RIR instruction to another in the same owner.
#[derive(Clone)]
pub struct RirOperandView {
    owner: Arc<CanonicalRirOutput>,
    role: &'static str,
    target: usize,
}

impl RirOperandView {
    pub fn role(&self) -> &str {
        self.role
    }

    pub fn target_ordinal(&self) -> usize {
        self.target
    }

    pub fn target(&self) -> RirInstructionView {
        debug_assert!(self.target < self.owner.rir().len());
        RirInstructionView {
            owner: self.owner.clone(),
            ordinal: self.target,
        }
    }
}

impl fmt::Debug for RirOperandView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RirOperandView")
            .field("role", &self.role())
            .field("target_ordinal", &self.target_ordinal())
            .finish()
    }
}

/// Retainable immutable RIR projection.
#[derive(Clone)]
pub struct RirView {
    owner: Arc<CanonicalRirOutput>,
}

impl RirView {
    pub(crate) fn new(owner: Arc<CanonicalRirOutput>) -> Self {
        Self { owner }
    }

    pub fn len(&self) -> usize {
        self.owner.rir().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn instructions(&self) -> impl ExactSizeIterator<Item = RirInstructionView> + '_ {
        (0..self.len()).map(|ordinal| RirInstructionView {
            owner: self.owner.clone(),
            ordinal,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn owner(&self) -> &Arc<CanonicalRirOutput> {
        &self.owner
    }

    #[allow(dead_code)]
    pub(crate) fn rir(&self) -> &rue_rir::Rir {
        self.owner.rir()
    }

    #[allow(dead_code)]
    pub(crate) fn presentation_order(
        &self,
        files: impl IntoIterator<Item = crate::FileId>,
    ) -> crate::canonical_lower::CanonicalRirPresentationOrder {
        self.owner.presentation_order(files)
    }
}

impl fmt::Debug for RirView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RirView")
            .field("instruction_count", &self.len())
            .finish()
    }
}

fn syntax_record(
    kind: &'static str,
    span: crate::Span,
    name: Option<Arc<str>>,
    value: Option<Arc<str>>,
    children: Vec<Arc<SyntaxNodeRecord>>,
) -> Arc<SyntaxNodeRecord> {
    Arc::new(SyntaxNodeRecord {
        kind,
        span,
        name,
        value,
        children: children.into(),
    })
}

fn resolved_ident(
    owner: &crate::parsed_modules::ParsedModule,
    ident: rue_parser::Ident,
) -> Arc<str> {
    Arc::from(owner.resolve_raw_symbol(ident.name))
}

fn resolved_idents(
    owner: &crate::parsed_modules::ParsedModule,
    idents: &[rue_parser::Ident],
    separator: &str,
) -> Arc<str> {
    idents
        .iter()
        .map(|ident| owner.resolve_raw_symbol(ident.name))
        .collect::<Vec<_>>()
        .join(separator)
        .into()
}

fn directive_records<'a>(
    owner: &'a crate::parsed_modules::ParsedModule,
    directives: &'a [rue_parser::Directive],
) -> impl Iterator<Item = Arc<SyntaxNodeRecord>> + 'a {
    directives.iter().map(|directive| {
        let children = directive
            .args
            .iter()
            .map(|arg| match arg {
                rue_parser::DirectiveArg::Ident(ident) => syntax_record(
                    "directive_argument",
                    ident.span,
                    Some(resolved_ident(owner, *ident)),
                    None,
                    Vec::new(),
                ),
            })
            .collect();
        syntax_record(
            "directive",
            directive.span,
            Some(resolved_ident(owner, directive.name)),
            None,
            children,
        )
    })
}

fn syntax_item_record(
    owner: &crate::parsed_modules::ParsedModule,
    item: &rue_parser::Item,
) -> Arc<SyntaxNodeRecord> {
    match item {
        rue_parser::Item::Function(function) => function_record(owner, function, "function"),
        rue_parser::Item::Struct(structure) => {
            let mut children = directive_records(owner, &structure.directives).collect::<Vec<_>>();
            children.push(modifier_record(
                structure.span,
                "visibility",
                visibility_name(structure.visibility),
            ));
            children.push(modifier_record(
                structure.span,
                "linear",
                bool_name(structure.is_linear),
            ));
            children.extend(structure.fields.iter().map(|field| {
                syntax_record(
                    "field",
                    field.span,
                    Some(resolved_ident(owner, field.name)),
                    None,
                    vec![type_record(owner, &field.ty)],
                )
            }));
            children.extend(
                structure
                    .methods
                    .iter()
                    .map(|method| method_record(owner, method)),
            );
            syntax_record(
                "struct",
                structure.span,
                Some(resolved_ident(owner, structure.name)),
                None,
                children,
            )
        }
        rue_parser::Item::Enum(enumeration) => {
            let mut children = vec![modifier_record(
                enumeration.span,
                "visibility",
                visibility_name(enumeration.visibility),
            )];
            children.extend(enumeration.variants.iter().map(|variant| {
                syntax_record(
                    "enum_variant",
                    variant.span,
                    Some(resolved_ident(owner, variant.name)),
                    None,
                    variant
                        .payload
                        .iter()
                        .map(|ty| type_record(owner, ty))
                        .collect(),
                )
            }));
            syntax_record(
                "enum",
                enumeration.span,
                Some(resolved_ident(owner, enumeration.name)),
                None,
                children,
            )
        }
        rue_parser::Item::DropFn(drop_fn) => syntax_record(
            "drop_function",
            drop_fn.span,
            Some(resolved_ident(owner, drop_fn.type_name)),
            None,
            vec![
                receiver_record(drop_fn.self_param.clone()),
                expr_record(owner, &drop_fn.body),
            ],
        ),
        rue_parser::Item::Extern(extern_block) => {
            let children = extern_block
                .fns
                .iter()
                .map(|foreign| {
                    syntax_record(
                        "extern_function",
                        foreign.span,
                        Some(resolved_ident(owner, foreign.name)),
                        None,
                        Vec::new(),
                    )
                })
                .collect::<Vec<_>>();
            syntax_record("extern", extern_block.span, None, None, children)
        }
        rue_parser::Item::Const(constant) => {
            let mut children = directive_records(owner, &constant.directives).collect::<Vec<_>>();
            children.push(modifier_record(
                constant.span,
                "visibility",
                visibility_name(constant.visibility),
            ));
            children.extend(constant.ty.iter().map(|ty| type_record(owner, ty)));
            children.push(expr_record(owner, &constant.init));
            syntax_record(
                "const",
                constant.span,
                Some(resolved_ident(owner, constant.name)),
                None,
                children,
            )
        }
        rue_parser::Item::Error(span) => syntax_record("error", *span, None, None, Vec::new()),
    }
}

fn function_record(
    owner: &crate::parsed_modules::ParsedModule,
    function: &rue_parser::Function,
    kind: &'static str,
) -> Arc<SyntaxNodeRecord> {
    let mut children = directive_records(owner, &function.directives).collect::<Vec<_>>();
    children.push(modifier_record(
        function.span,
        "visibility",
        visibility_name(function.visibility),
    ));
    children.push(modifier_record(
        function.span,
        "unchecked",
        bool_name(function.is_unchecked),
    ));
    children.extend(
        function
            .params
            .iter()
            .map(|param| param_record(owner, param)),
    );
    children.extend(function.return_type.iter().map(|ty| type_record(owner, ty)));
    children.push(expr_record(owner, &function.body));
    syntax_record(
        kind,
        function.span,
        Some(resolved_ident(owner, function.name)),
        None,
        children,
    )
}

fn method_record(
    owner: &crate::parsed_modules::ParsedModule,
    method: &rue_parser::Method,
) -> Arc<SyntaxNodeRecord> {
    let mut children = directive_records(owner, &method.directives).collect::<Vec<_>>();
    children.extend(method.receiver.iter().cloned().map(receiver_record));
    children.extend(method.params.iter().map(|param| param_record(owner, param)));
    children.extend(method.return_type.iter().map(|ty| type_record(owner, ty)));
    children.push(expr_record(owner, &method.body));
    syntax_record(
        "method",
        method.span,
        Some(resolved_ident(owner, method.name)),
        None,
        children,
    )
}

fn visibility_name(visibility: rue_parser::ast::Visibility) -> &'static str {
    match visibility {
        rue_parser::ast::Visibility::Private => "private",
        rue_parser::ast::Visibility::Public => "public",
    }
}

fn bool_name(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn modifier_record(
    span: crate::Span,
    name: &'static str,
    value: &'static str,
) -> Arc<SyntaxNodeRecord> {
    syntax_record(
        "modifier",
        span,
        Some(Arc::from(name)),
        Some(Arc::from(value)),
        Vec::new(),
    )
}

fn receiver_record(receiver: rue_parser::SelfParam) -> Arc<SyntaxNodeRecord> {
    syntax_record(
        "receiver",
        receiver.span,
        Some(Arc::from("self")),
        Some(
            format!(
                "mode={};mutable={}",
                if receiver.mode == rue_parser::ParamMode::Normal {
                    "normal"
                } else {
                    receiver.mode.keyword()
                },
                bool_name(receiver.is_mut)
            )
            .into(),
        ),
        Vec::new(),
    )
}

fn argument_mode_name(mode: rue_parser::ArgMode) -> &'static str {
    match mode {
        rue_parser::ArgMode::Normal => "normal",
        rue_parser::ArgMode::Inout => "inout",
        rue_parser::ArgMode::Borrow => "borrow",
    }
}

fn call_argument_record(
    owner: &crate::parsed_modules::ParsedModule,
    argument: &rue_parser::CallArg,
) -> Arc<SyntaxNodeRecord> {
    syntax_record(
        "argument",
        argument.span,
        None,
        Some(Arc::from(argument_mode_name(argument.mode))),
        vec![expr_record(owner, &argument.expr)],
    )
}

fn param_record(
    owner: &crate::parsed_modules::ParsedModule,
    param: &rue_parser::Param,
) -> Arc<SyntaxNodeRecord> {
    syntax_record(
        "parameter",
        param.span,
        Some(resolved_ident(owner, param.name)),
        Some(Arc::from(if param.mode == rue_parser::ParamMode::Normal {
            "normal"
        } else {
            param.mode.keyword()
        })),
        vec![type_record(owner, &param.ty)],
    )
}

fn type_record(
    owner: &crate::parsed_modules::ParsedModule,
    ty: &rue_parser::TypeExpr,
) -> Arc<SyntaxNodeRecord> {
    use rue_parser::TypeExpr;
    let span = ty.span();
    match ty {
        TypeExpr::Named(ident) => syntax_record(
            "named_type",
            span,
            Some(resolved_ident(owner, *ident)),
            None,
            Vec::new(),
        ),
        TypeExpr::Qualified { segments, .. } => syntax_record(
            "qualified_type",
            span,
            Some(resolved_idents(owner, segments, ".")),
            None,
            Vec::new(),
        ),
        TypeExpr::Unit(_) => syntax_record("unit_type", span, None, None, Vec::new()),
        TypeExpr::Never(_) => syntax_record("never_type", span, None, None, Vec::new()),
        TypeExpr::Array {
            element, length, ..
        } => syntax_record(
            "array_type",
            span,
            None,
            None,
            vec![
                type_record(owner, element),
                array_length_record(owner, length, span),
            ],
        ),
        TypeExpr::Slice { element, .. } => syntax_record(
            "slice_type",
            span,
            None,
            None,
            vec![type_record(owner, element)],
        ),
        TypeExpr::AnonymousStruct {
            fields, methods, ..
        } => {
            let mut children = fields
                .iter()
                .map(|field| {
                    syntax_record(
                        "field",
                        field.span,
                        Some(resolved_ident(owner, field.name)),
                        None,
                        vec![type_record(owner, &field.ty)],
                    )
                })
                .collect::<Vec<_>>();
            children.extend(methods.iter().map(|method| method_record(owner, method)));
            syntax_record("anonymous_struct_type", span, None, None, children)
        }
        TypeExpr::AnonymousEnum { variants, .. } => syntax_record(
            "anonymous_enum_type",
            span,
            None,
            None,
            variants
                .iter()
                .map(|variant| {
                    syntax_record(
                        "enum_variant",
                        variant.span,
                        Some(resolved_ident(owner, variant.name)),
                        None,
                        variant
                            .payload
                            .iter()
                            .map(|ty| type_record(owner, ty))
                            .collect(),
                    )
                })
                .collect(),
        ),
        TypeExpr::PointerConst { pointee, .. } => syntax_record(
            "const_pointer_type",
            span,
            None,
            None,
            vec![type_record(owner, pointee)],
        ),
        TypeExpr::PointerMut { pointee, .. } => syntax_record(
            "mutable_pointer_type",
            span,
            None,
            None,
            vec![type_record(owner, pointee)],
        ),
        TypeExpr::TypeCall { name, args, .. } => syntax_record(
            "type_call",
            span,
            Some(resolved_ident(owner, *name)),
            None,
            args.iter().map(|arg| type_record(owner, arg)).collect(),
        ),
        TypeExpr::QualifiedTypeCall { segments, args, .. } => syntax_record(
            "qualified_type_call",
            span,
            Some(resolved_idents(owner, segments, ".")),
            None,
            args.iter().map(|arg| type_record(owner, arg)).collect(),
        ),
        TypeExpr::StrFixed { name, length, .. } => syntax_record(
            "fixed_string_type",
            span,
            Some(resolved_ident(owner, *name)),
            Some(length.to_string().into()),
            Vec::new(),
        ),
        TypeExpr::IntArg { value, .. } => syntax_record(
            "integer_type_argument",
            span,
            None,
            Some(value.to_string().into()),
            Vec::new(),
        ),
    }
}

fn array_length_record(
    owner: &crate::parsed_modules::ParsedModule,
    length: &rue_parser::ArrayLength,
    span: crate::Span,
) -> Arc<SyntaxNodeRecord> {
    match length {
        rue_parser::ArrayLength::Literal(value) => syntax_record(
            "array_length",
            span,
            None,
            Some(value.to_string().into()),
            Vec::new(),
        ),
        rue_parser::ArrayLength::Named(ident) => syntax_record(
            "array_length",
            ident.span,
            Some(resolved_ident(owner, *ident)),
            None,
            Vec::new(),
        ),
        rue_parser::ArrayLength::Call { name, args } => syntax_record(
            "array_length_call",
            name.span,
            Some(resolved_ident(owner, *name)),
            None,
            args.iter()
                .map(|arg| array_length_record(owner, arg, name.span))
                .collect(),
        ),
    }
}

fn block_record(
    owner: &crate::parsed_modules::ParsedModule,
    block: &rue_parser::BlockExpr,
) -> Arc<SyntaxNodeRecord> {
    let mut children = block
        .statements
        .iter()
        .map(|statement| statement_record(owner, statement))
        .collect::<Vec<_>>();
    children.push(expr_record(owner, &block.expr));
    syntax_record("block", block.span, None, None, children)
}

fn statement_record(
    owner: &crate::parsed_modules::ParsedModule,
    statement: &rue_parser::Statement,
) -> Arc<SyntaxNodeRecord> {
    match statement {
        rue_parser::Statement::Let(statement) => {
            let (name, pattern) = match statement.pattern {
                rue_parser::LetPattern::Ident(ident) => (Some(resolved_ident(owner, ident)), "let"),
                rue_parser::LetPattern::Wildcard(_) => (None, "let_wildcard"),
            };
            let mut children = directive_records(owner, &statement.directives).collect::<Vec<_>>();
            children.push(modifier_record(
                statement.span,
                "mutable",
                bool_name(statement.is_mut),
            ));
            children.extend(statement.ty.iter().map(|ty| type_record(owner, ty)));
            children.push(expr_record(owner, &statement.init));
            syntax_record(pattern, statement.span, name, None, children)
        }
        rue_parser::Statement::Assign(statement) => {
            let target = match &statement.target {
                rue_parser::AssignTarget::Var(ident) => syntax_record(
                    "assignment_target",
                    ident.span,
                    Some(resolved_ident(owner, *ident)),
                    None,
                    Vec::new(),
                ),
                rue_parser::AssignTarget::Field(field) => field_record(owner, field),
                rue_parser::AssignTarget::Index(index) => syntax_record(
                    "index",
                    index.span,
                    None,
                    None,
                    vec![
                        expr_record(owner, &index.base),
                        expr_record(owner, &index.index),
                    ],
                ),
            };
            syntax_record(
                "assignment",
                statement.span,
                None,
                None,
                vec![target, expr_record(owner, &statement.value)],
            )
        }
        rue_parser::Statement::Expr(expression) => syntax_record(
            "expression_statement",
            expression.span(),
            None,
            None,
            vec![expr_record(owner, expression)],
        ),
    }
}

fn field_record(
    owner: &crate::parsed_modules::ParsedModule,
    field: &rue_parser::FieldExpr,
) -> Arc<SyntaxNodeRecord> {
    syntax_record(
        "field_access",
        field.span,
        Some(resolved_ident(owner, field.field)),
        None,
        vec![expr_record(owner, &field.base)],
    )
}

fn expr_record(
    owner: &crate::parsed_modules::ParsedModule,
    expression: &rue_parser::Expr,
) -> Arc<SyntaxNodeRecord> {
    use rue_parser::Expr;
    let span = expression.span();
    match expression {
        Expr::Int(literal) => syntax_record(
            "integer_literal",
            span,
            None,
            Some(literal.value.to_string().into()),
            Vec::new(),
        ),
        Expr::Float(literal) => syntax_record(
            "float_literal",
            span,
            None,
            Some(owner.resolve_raw_symbol(literal.value).to_string().into()),
            Vec::new(),
        ),
        Expr::String(literal) => syntax_record(
            "string_literal",
            span,
            None,
            Some(Arc::from(owner.resolve_raw_symbol(literal.value))),
            Vec::new(),
        ),
        Expr::Bool(literal) => syntax_record(
            "boolean_literal",
            span,
            None,
            Some(literal.value.to_string().into()),
            Vec::new(),
        ),
        Expr::Unit(_) => syntax_record("unit_literal", span, None, None, Vec::new()),
        Expr::Ident(ident) => syntax_record(
            "identifier",
            span,
            Some(resolved_ident(owner, *ident)),
            None,
            Vec::new(),
        ),
        Expr::Binary(binary) => syntax_record(
            "binary",
            span,
            None,
            Some(binary_operator(binary.op).into()),
            vec![
                expr_record(owner, &binary.left),
                expr_record(owner, &binary.right),
            ],
        ),
        Expr::Unary(unary) => syntax_record(
            "unary",
            span,
            None,
            Some(unary_operator(unary.op).into()),
            vec![expr_record(owner, &unary.operand)],
        ),
        Expr::Paren(paren) => syntax_record(
            "parenthesized",
            span,
            None,
            None,
            vec![expr_record(owner, &paren.inner)],
        ),
        Expr::Block(block) => block_record(owner, block),
        Expr::If(branch) => {
            let mut children = vec![
                expr_record(owner, &branch.cond),
                block_record(owner, &branch.then_block),
            ];
            children.extend(
                branch
                    .else_block
                    .iter()
                    .map(|block| block_record(owner, block)),
            );
            syntax_record("if", span, None, None, children)
        }
        Expr::Match(matched) => {
            let mut children = vec![expr_record(owner, &matched.scrutinee)];
            children.extend(matched.arms.iter().map(|arm| {
                syntax_record(
                    "match_arm",
                    arm.span,
                    None,
                    None,
                    vec![
                        pattern_record(owner, &arm.pattern),
                        expr_record(owner, &arm.body),
                    ],
                )
            }));
            syntax_record("match", span, None, None, children)
        }
        Expr::While(loop_expression) => syntax_record(
            "while",
            span,
            None,
            None,
            vec![
                expr_record(owner, &loop_expression.cond),
                block_record(owner, &loop_expression.body),
            ],
        ),
        Expr::Loop(loop_expression) => syntax_record(
            "loop",
            span,
            None,
            None,
            vec![block_record(owner, &loop_expression.body)],
        ),
        Expr::For(loop_expression) => {
            let binder = match loop_expression.binder {
                rue_parser::LetPattern::Ident(ident) => syntax_record(
                    "binding_pattern",
                    ident.span,
                    Some(resolved_ident(owner, ident)),
                    None,
                    Vec::new(),
                ),
                rue_parser::LetPattern::Wildcard(span) => {
                    syntax_record("wildcard_pattern", span, None, None, Vec::new())
                }
            };
            syntax_record(
                "for",
                span,
                None,
                None,
                vec![
                    binder,
                    expr_record(owner, &loop_expression.iterable),
                    block_record(owner, &loop_expression.body),
                ],
            )
        }
        Expr::Call(call) => syntax_record(
            "call",
            span,
            Some(resolved_ident(owner, call.name)),
            None,
            call.args
                .iter()
                .map(|argument| call_argument_record(owner, argument))
                .collect(),
        ),
        Expr::Break(break_expression) => syntax_record(
            "break",
            span,
            None,
            None,
            break_expression
                .value
                .iter()
                .map(|value| expr_record(owner, value))
                .collect(),
        ),
        Expr::Continue(_) => syntax_record("continue", span, None, None, Vec::new()),
        Expr::Return(return_expression) => syntax_record(
            "return",
            span,
            None,
            None,
            return_expression
                .value
                .iter()
                .map(|value| expr_record(owner, value))
                .collect(),
        ),
        Expr::Yield(yield_expression) => syntax_record(
            "yield",
            span,
            None,
            None,
            vec![expr_record(owner, &yield_expression.value)],
        ),
        Expr::StructLit(literal) => {
            let mut children = literal
                .base
                .iter()
                .map(|base| expr_record(owner, base))
                .collect::<Vec<_>>();
            children.extend(
                literal
                    .ctor_args
                    .iter()
                    .flatten()
                    .map(|argument| call_argument_record(owner, argument)),
            );
            children.extend(literal.fields.iter().map(|field| {
                syntax_record(
                    "field_initializer",
                    field.span,
                    Some(resolved_ident(owner, field.name)),
                    Some(
                        if field.shorthand {
                            "shorthand"
                        } else {
                            "explicit"
                        }
                        .into(),
                    ),
                    vec![expr_record(owner, &field.value)],
                )
            }));
            syntax_record(
                "struct_literal",
                span,
                Some(resolved_ident(owner, literal.name)),
                None,
                children,
            )
        }
        Expr::Field(field) => field_record(owner, field),
        Expr::MethodCall(call) => {
            let mut children = vec![expr_record(owner, &call.receiver)];
            children.extend(
                call.args
                    .iter()
                    .map(|argument| call_argument_record(owner, argument)),
            );
            syntax_record(
                "method_call",
                span,
                Some(resolved_ident(owner, call.method)),
                None,
                children,
            )
        }
        Expr::Try(try_expression) => syntax_record(
            "try",
            span,
            None,
            None,
            vec![expr_record(owner, &try_expression.operand)],
        ),
        Expr::IntrinsicCall(call) => syntax_record(
            "intrinsic_call",
            span,
            Some(resolved_ident(owner, call.name)),
            None,
            call.args
                .iter()
                .map(|argument| match argument {
                    rue_parser::IntrinsicArg::Expr(expression) => expr_record(owner, expression),
                    rue_parser::IntrinsicArg::Type(ty) => type_record(owner, ty),
                })
                .collect(),
        ),
        Expr::ArrayLit(array) => {
            let mut children = array
                .elements
                .iter()
                .map(|element| expr_record(owner, element))
                .collect::<Vec<_>>();
            children.extend(
                array
                    .repeat
                    .iter()
                    .map(|length| array_length_record(owner, length, span)),
            );
            syntax_record("array_literal", span, None, None, children)
        }
        Expr::Index(index) => syntax_record(
            "index",
            span,
            None,
            None,
            vec![
                expr_record(owner, &index.base),
                expr_record(owner, &index.index),
            ],
        ),
        Expr::Path(path) => syntax_record(
            "path",
            span,
            Some(
                format!(
                    "{}::{}",
                    owner.resolve_raw_symbol(path.type_name.name),
                    owner.resolve_raw_symbol(path.variant.name)
                )
                .into(),
            ),
            None,
            path.base
                .iter()
                .map(|base| expr_record(owner, base))
                .collect(),
        ),
        Expr::SelfExpr(_) => syntax_record("self", span, None, None, Vec::new()),
        Expr::Comptime(block) => syntax_record(
            "comptime",
            span,
            None,
            None,
            vec![expr_record(owner, &block.expr)],
        ),
        Expr::Checked(block) => syntax_record(
            "checked",
            span,
            None,
            None,
            vec![expr_record(owner, &block.expr)],
        ),
        Expr::TypeLit(literal) => syntax_record(
            "type_literal",
            span,
            None,
            None,
            vec![type_record(owner, &literal.type_expr)],
        ),
        Expr::Error(_) => syntax_record("error", span, None, None, Vec::new()),
    }
}

fn pattern_record(
    owner: &crate::parsed_modules::ParsedModule,
    pattern: &rue_parser::Pattern,
) -> Arc<SyntaxNodeRecord> {
    let span = pattern.span();
    match pattern {
        rue_parser::Pattern::Wildcard(_) => {
            syntax_record("wildcard_pattern", span, None, None, Vec::new())
        }
        rue_parser::Pattern::Int(literal) => syntax_record(
            "integer_pattern",
            span,
            None,
            Some(literal.value.to_string().into()),
            Vec::new(),
        ),
        rue_parser::Pattern::NegInt(literal) => syntax_record(
            "integer_pattern",
            span,
            None,
            Some(format!("-{}", literal.value).into()),
            Vec::new(),
        ),
        rue_parser::Pattern::Bool(literal) => syntax_record(
            "boolean_pattern",
            span,
            None,
            Some(literal.value.to_string().into()),
            Vec::new(),
        ),
        rue_parser::Pattern::Path(path) => {
            let mut children = path
                .base
                .iter()
                .map(|base| expr_record(owner, base))
                .collect::<Vec<_>>();
            children.extend(
                path.ctor_args
                    .iter()
                    .flatten()
                    .map(|argument| call_argument_record(owner, argument)),
            );
            children.extend(path.bindings.iter().map(|binding| {
                syntax_record(
                    "binding",
                    binding.span,
                    Some(resolved_ident(owner, *binding)),
                    None,
                    Vec::new(),
                )
            }));
            syntax_record(
                "path_pattern",
                span,
                Some(
                    format!(
                        "{}::{}",
                        owner.resolve_raw_symbol(path.type_name.name),
                        owner.resolve_raw_symbol(path.variant.name)
                    )
                    .into(),
                ),
                None,
                children,
            )
        }
    }
}

fn binary_operator(operator: rue_parser::BinaryOp) -> &'static str {
    use rue_parser::BinaryOp::*;
    match operator {
        Add => "add",
        Sub => "subtract",
        Mul => "multiply",
        Div => "divide",
        Mod => "modulo",
        Eq => "equal",
        Ne => "not_equal",
        Lt => "less_than",
        Gt => "greater_than",
        Le => "less_or_equal",
        Ge => "greater_or_equal",
        And => "logical_and",
        Or => "logical_or",
        BitAnd => "bit_and",
        BitOr => "bit_or",
        BitXor => "bit_xor",
        Shl => "shift_left",
        Shr => "shift_right",
    }
}

fn unary_operator(operator: rue_parser::UnaryOp) -> &'static str {
    match operator {
        rue_parser::UnaryOp::Neg => "negate",
        rue_parser::UnaryOp::Not => "logical_not",
        rue_parser::UnaryOp::BitNot => "bit_not",
    }
}

fn rir_kind(data: &rue_rir::InstData) -> &'static str {
    use rue_rir::InstData::*;
    match data {
        IntConst(_) => "integer_constant",
        FloatConst { .. } => "float_constant",
        BoolConst(_) => "boolean_constant",
        StringConst { .. } => "string_constant",
        UnitConst => "unit_constant",
        Add { .. } => "add",
        Sub { .. } => "subtract",
        Mul { .. } => "multiply",
        Div { .. } => "divide",
        Mod { .. } => "modulo",
        Eq { .. } => "equal",
        Ne { .. } => "not_equal",
        Lt { .. } => "less_than",
        Gt { .. } => "greater_than",
        Le { .. } => "less_or_equal",
        Ge { .. } => "greater_or_equal",
        And { .. } => "logical_and",
        Or { .. } => "logical_or",
        BitAnd { .. } => "bit_and",
        BitOr { .. } => "bit_or",
        BitXor { .. } => "bit_xor",
        Shl { .. } => "shift_left",
        Shr { .. } => "shift_right",
        Neg { .. } => "negate",
        Not { .. } => "logical_not",
        BitNot { .. } => "bit_not",
        Try { .. } => "try",
        Branch { .. } => "branch",
        Loop { .. } => "loop",
        InfiniteLoop { .. } => "infinite_loop",
        Match { .. } => "match",
        Break { .. } => "break",
        Continue => "continue",
        FnDecl { .. } => "function_declaration",
        ConstDecl { .. } => "constant_declaration",
        Call { .. } => "call",
        Intrinsic { .. } => "intrinsic",
        InternalIntrinsic { .. } => "internal_intrinsic",
        TypeIntrinsic { .. } => "type_intrinsic",
        OffsetOf { .. } => "offset_of",
        Ret(_) => "return",
        Yield(_) => "yield",
        Block { .. } => "block",
        Alloc { .. } => "allocate",
        VarRef { .. } => "variable_reference",
        Assign { .. } => "assignment",
        StructDecl { .. } => "struct_declaration",
        StructInit { .. } => "struct_initializer",
        FieldGet { .. } => "field_read",
        FieldSet { .. } => "field_write",
        EnumDecl { .. } => "enum_declaration",
        EnumVariant { .. } => "enum_variant",
        ArrayInit { .. } => "array_initializer",
        ArrayRepeat { .. } => "array_repeat",
        IndexGet { .. } => "index_read",
        IndexSet { .. } => "index_write",
        MethodCall { .. } => "method_call",
        DropFnDecl { .. } => "drop_function_declaration",
        Comptime { .. } => "comptime",
        Checked { .. } => "checked",
        TypeConst { .. } => "type_constant",
        AnonStructType { .. } => "anonymous_struct_type",
        AnonEnumType { .. } => "anonymous_enum_type",
    }
}

fn rir_operands(rir: &rue_rir::Rir, data: &rue_rir::InstData) -> Vec<RirOperandRecord> {
    use rue_rir::InstData::*;
    let mut operands = Vec::new();
    let mut push = |role, reference: rue_rir::InstRef| {
        let target = reference.as_u32() as usize;
        debug_assert!(target < rir.len());
        operands.push(RirOperandRecord { role, target });
    };
    match data {
        Add { lhs, rhs }
        | Sub { lhs, rhs }
        | Mul { lhs, rhs }
        | Div { lhs, rhs }
        | Mod { lhs, rhs }
        | Eq { lhs, rhs }
        | Ne { lhs, rhs }
        | Lt { lhs, rhs }
        | Gt { lhs, rhs }
        | Le { lhs, rhs }
        | Ge { lhs, rhs }
        | And { lhs, rhs }
        | Or { lhs, rhs }
        | BitAnd { lhs, rhs }
        | BitOr { lhs, rhs }
        | BitXor { lhs, rhs }
        | Shl { lhs, rhs }
        | Shr { lhs, rhs } => {
            push("left", *lhs);
            push("right", *rhs);
        }
        Neg { operand } | Not { operand } | BitNot { operand } | Try { operand } => {
            push("operand", *operand);
        }
        Branch {
            cond,
            then_block,
            else_block,
        } => {
            push("condition", *cond);
            push("then", *then_block);
            if let Some(block) = else_block {
                push("else", *block);
            }
        }
        Loop { cond, body } => {
            push("condition", *cond);
            push("body", *body);
        }
        InfiniteLoop { body, .. } => push("body", *body),
        Match { scrutinee, arms } => {
            push("scrutinee", *scrutinee);
            for (pattern, body) in rir.match_arms(arms).iter() {
                if let rue_rir::RirPatternView::Path {
                    module, ctor_head, ..
                } = pattern
                {
                    if let Some(module) = module {
                        push("pattern_module", module);
                    }
                    if let Some(ctor_head) = ctor_head {
                        push("pattern_constructor", ctor_head);
                    }
                }
                push("arm_body", body);
            }
        }
        Break { value } | Ret(value) => {
            if let Some(value) = value {
                push("value", *value);
            }
        }
        Yield(value) => push("value", *value),
        FnDecl { body, .. } | DropFnDecl { body, .. } => push("body", *body),
        ConstDecl { init, .. } | Alloc { init, .. } => push("initializer", *init),
        Assign { value, .. } => push("value", *value),
        Call { args, .. } => {
            for argument in rir.call_args(args) {
                push("argument", argument.value);
            }
        }
        Intrinsic { args, .. } => {
            for argument in rir.intrinsic_args(args) {
                push("argument", argument);
            }
        }
        InternalIntrinsic { args, .. } => {
            for argument in rir.internal_intrinsic_args(args) {
                push("argument", argument);
            }
        }
        Block { instructions } => {
            for instruction in rir.block_insts(instructions) {
                push("instruction", instruction);
            }
        }
        StructDecl { methods, .. } => {
            for method in rir.struct_methods(methods) {
                push("method", method);
            }
        }
        StructInit {
            module,
            ctor_head,
            fields,
            ..
        } => {
            if let Some(module) = module {
                push("module", *module);
            }
            if let Some(ctor_head) = ctor_head {
                push("constructor", *ctor_head);
            }
            for (_, value) in rir.field_inits(fields) {
                push("field", value);
            }
        }
        FieldGet { base, .. } => push("base", *base),
        FieldSet { base, value, .. } => {
            push("base", *base);
            push("value", *value);
        }
        EnumVariant { module, .. } => {
            if let Some(module) = module {
                push("module", *module);
            }
        }
        ArrayInit { elements } => {
            for element in rir.array_elements(elements) {
                push("element", element);
            }
        }
        ArrayRepeat { value, .. } => push("value", *value),
        IndexGet { base, index } => {
            push("base", *base);
            push("index", *index);
        }
        IndexSet { base, index, value } => {
            push("base", *base);
            push("index", *index);
            push("value", *value);
        }
        MethodCall { receiver, args, .. } => {
            push("receiver", *receiver);
            for argument in rir.call_args(args) {
                push("argument", argument.value);
            }
        }
        Comptime { expr } | Checked { expr } => push("expression", *expr),
        AnonStructType { methods, .. } => {
            for method in rir.anon_struct_methods(methods) {
                push("method", method);
            }
        }
        IntConst(_)
        | FloatConst { .. }
        | BoolConst(_)
        | StringConst { .. }
        | UnitConst
        | Continue
        | TypeIntrinsic { .. }
        | OffsetOf { .. }
        | VarRef { .. }
        | EnumDecl { .. }
        | TypeConst { .. }
        | AnonEnumType { .. } => {}
    }
    operands
}
