//! Self-contained immutable parsed-module artifacts.
//!
//! This is the reuse-safe syntax boundary. The root-level `ParsedProgram`
//! remains the shared-interner compatibility representation until assembly
//! symbol translation is introduced separately.

use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(test)]
use lasso::Key;
use lasso::{RodeoResolver, Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileErrors, CompileResult, ErrorKind};
use rue_parser::{
    AssignTarget, Ast, Expr, IntrinsicArg, Item, Pattern, Statement, TypeExpr, ast::Visibility,
};
use rue_span::{FileId, Span};

use crate::definition_snapshot::{definition_parts, validate_span};
use crate::{
    DefinitionKind, DefinitionNamespace, ImportDirective, ImportDirectives, ModuleId,
    ModuleRevision, SourceId, SourceRevision, SourceSnapshot, SyntaxWork,
};

#[derive(Debug)]
struct SymbolProvenance;

/// A symbol handle bound to exactly one frozen module universe.
#[derive(Debug, Clone)]
pub struct ParsedSymbol {
    spur: Spur,
    provenance: Arc<SymbolProvenance>,
}

impl ParsedSymbol {
    #[cfg(test)]
    pub(crate) fn test_local_ordinal(&self) -> usize {
        self.spur.into_usize()
    }
}

/// Immutable symbol resolver for one parsed module.
#[derive(Debug)]
pub struct FrozenSymbolResolver {
    resolver: RodeoResolver<Spur>,
    provenance: Arc<SymbolProvenance>,
}

impl FrozenSymbolResolver {
    /// Resolve only a handle issued by this exact symbol universe.
    pub fn resolve<'a>(&'a self, symbol: &ParsedSymbol) -> CompileResult<&'a str> {
        if !Arc::ptr_eq(&self.provenance, &symbol.provenance) {
            return Err(invalid_input(
                "parsed symbol belongs to a foreign symbol universe",
            ));
        }
        self.resolver
            .try_resolve(&symbol.spur)
            .ok_or_else(|| invalid_input("parsed symbol is absent from its frozen resolver"))
    }

    fn symbol(&self, spur: Spur) -> CompileResult<ParsedSymbol> {
        self.resolver
            .try_resolve(&spur)
            .ok_or_else(|| invalid_input("AST symbol is absent from its frozen resolver"))?;
        Ok(ParsedSymbol {
            spur,
            provenance: self.provenance.clone(),
        })
    }
}

#[derive(Debug)]
struct ProvenancedAst {
    ast: Arc<Ast>,
    provenance: Arc<SymbolProvenance>,
    source: SourceId,
}

/// Snapshot-local occurrence of one parsed definition candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParsedDefinitionOccurrence(u32);

impl ParsedDefinitionOccurrence {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One presemantic definition candidate; duplicates remain distinct values.
#[derive(Debug, Clone)]
pub struct ParsedDefinitionCandidate {
    occurrence: ParsedDefinitionOccurrence,
    namespace: DefinitionNamespace,
    kind: DefinitionKind,
    visibility: Option<Visibility>,
    name: Arc<str>,
    symbol: ParsedSymbol,
    name_span: Span,
    declaration_span: Span,
}

impl ParsedDefinitionCandidate {
    pub fn occurrence(&self) -> ParsedDefinitionOccurrence {
        self.occurrence
    }
    pub fn namespace(&self) -> DefinitionNamespace {
        self.namespace
    }
    pub fn kind(&self) -> DefinitionKind {
        self.kind
    }
    pub fn visibility(&self) -> Option<Visibility> {
        self.visibility
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn symbol(&self) -> &ParsedSymbol {
        &self.symbol
    }
    pub fn name_span(&self) -> Span {
        self.name_span
    }
    pub fn declaration_span(&self) -> Span {
        self.declaration_span
    }
}

/// Immutable per-module definition-candidate index.
#[derive(Debug, Clone)]
pub struct ParsedDefinitionIndex {
    candidates: Arc<[ParsedDefinitionCandidate]>,
    by_name: BTreeMap<(DefinitionNamespace, Arc<str>), Arc<[ParsedDefinitionOccurrence]>>,
}

impl ParsedDefinitionIndex {
    pub fn candidates(&self) -> &[ParsedDefinitionCandidate] {
        &self.candidates
    }

    pub fn candidates_named(
        &self,
        namespace: DefinitionNamespace,
        name: &str,
    ) -> impl Iterator<Item = &ParsedDefinitionCandidate> + '_ {
        self.by_name
            .get(&(namespace, Arc::from(name)))
            .into_iter()
            .flat_map(|occurrences| occurrences.iter())
            .map(|occurrence| &self.candidates[occurrence.index()])
    }
}

/// One import occurrence extracted directly into a reusable module artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParsedImportDirective {
    importer: ModuleId,
    source_offset: u32,
    specifier: Arc<str>,
}

impl ParsedImportDirective {
    pub fn importer(&self) -> &ModuleId {
        &self.importer
    }
    pub fn source_offset(&self) -> u32 {
        self.source_offset
    }
    pub fn specifier(&self) -> &str {
        &self.specifier
    }
}

/// Immutable, Arc-shareable parsed syntax and exact local provenance.
#[derive(Debug)]
pub struct ParsedModule {
    revision: ModuleRevision,
    file_id: FileId,
    physical_path: Arc<str>,
    source_text: Arc<String>,
    ast: ProvenancedAst,
    resolver: FrozenSymbolResolver,
    definitions: ParsedDefinitionIndex,
    imports: Arc<[ParsedImportDirective]>,
}

/// An AST paired with the exact parsed module that owns all of its symbols.
///
/// Views are issued only by [`ParsedProgram`]; cloning a view retains the
/// pointer-identical parsed module rather than copying its AST payload.
#[derive(Debug, Clone)]
pub struct ParsedAstView {
    module: Arc<ParsedModule>,
}

impl ParsedAstView {
    pub(crate) fn from_module(module: Arc<ParsedModule>) -> Self {
        Self { module }
    }

    pub fn module(&self) -> &Arc<ParsedModule> {
        &self.module
    }

    pub fn module_id(&self) -> &ModuleId {
        self.module.module_id()
    }

    pub fn ast(&self) -> &Ast {
        self.module.ast()
    }

    pub fn items(&self) -> impl ExactSizeIterator<Item = ParsedItemView> + '_ {
        (0..self.module.ast().items.len()).map(|index| ParsedItemView {
            module: self.module.clone(),
            index,
        })
    }
}

/// One parsed item paired with the module that owns its local symbols.
#[derive(Debug, Clone)]
pub struct ParsedItemView {
    module: Arc<ParsedModule>,
    index: usize,
}

impl ParsedItemView {
    pub(crate) fn from_module_index(module: Arc<ParsedModule>, index: usize) -> Self {
        debug_assert!(index < module.ast().items.len());
        Self { module, index }
    }

    pub fn module(&self) -> &Arc<ParsedModule> {
        &self.module
    }

    pub fn module_id(&self) -> &ModuleId {
        self.module.module_id()
    }

    pub fn item(&self) -> &Item {
        &self.module.ast().items[self.index]
    }
}

impl ParsedModule {
    pub fn revision(&self) -> &ModuleRevision {
        &self.revision
    }
    pub fn module_id(&self) -> &ModuleId {
        &self.revision.module
    }
    pub fn source_id(&self) -> &SourceId {
        &self.revision.source
    }
    pub fn file_id(&self) -> FileId {
        self.file_id
    }
    pub fn physical_path(&self) -> &str {
        &self.physical_path
    }
    pub fn source_text(&self) -> &str {
        &self.source_text
    }
    pub(crate) fn shared_source_text(&self) -> Arc<String> {
        self.source_text.clone()
    }
    pub fn ast(&self) -> &Ast {
        &self.ast.ast
    }
    pub fn definitions(&self) -> &ParsedDefinitionIndex {
        &self.definitions
    }
    pub fn imports(&self) -> &[ParsedImportDirective] {
        &self.imports
    }

    pub fn resolve(&self, symbol: &ParsedSymbol) -> CompileResult<&str> {
        self.resolver.resolve(symbol)
    }

    pub(crate) fn parsed_symbol(&self, spur: Spur) -> CompileResult<ParsedSymbol> {
        self.resolver.symbol(spur)
    }
}

/// Deterministically ordered collection of independently parsed modules.
#[derive(Debug, Clone)]
pub struct ParsedProgram {
    source_revision: SourceRevision,
    modules: Arc<[Arc<ParsedModule>]>,
    imports: ImportDirectives,
}

impl ParsedProgram {
    pub fn new(root: ModuleId, mut modules: Vec<Arc<ParsedModule>>) -> CompileResult<Self> {
        modules.sort_by(|left, right| left.module_id().cmp(right.module_id()));
        let mut file_ids = modules
            .iter()
            .map(|module| (module.file_id(), module.module_id()))
            .collect::<Vec<_>>();
        file_ids.sort_by_key(|(file_id, _)| file_id.index());
        if let Some(duplicate) = file_ids.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(invalid_input(format!(
                "parsed program contains duplicate file ID {} for modules {} and {}",
                duplicate[0].0.index(),
                duplicate[0].1,
                duplicate[1].1
            )));
        }
        let source_revision = SourceRevision::new(
            root,
            modules
                .iter()
                .map(|module| module.revision().clone())
                .collect(),
        )?;
        let imports = ImportDirectives::from_records(
            modules
                .iter()
                .flat_map(|module| module.imports().iter())
                .map(|directive| {
                    ImportDirective::new(
                        directive.importer.clone(),
                        directive.source_offset,
                        directive.specifier.clone(),
                    )
                })
                .collect(),
        );
        Ok(Self {
            source_revision,
            modules: modules.into(),
            imports,
        })
    }

    pub fn root(&self) -> &ModuleId {
        self.source_revision.root()
    }
    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }
    pub fn modules(&self) -> &[Arc<ParsedModule>] {
        &self.modules
    }

    /// Traverse module-qualified ASTs in canonical logical-module order.
    pub fn ast_views(&self) -> impl ExactSizeIterator<Item = ParsedAstView> + '_ {
        self.modules
            .iter()
            .cloned()
            .map(|module| ParsedAstView { module })
    }

    /// Canonical program-wide import occurrences, ready for graph resolution.
    pub fn import_directives(&self) -> &ImportDirectives {
        &self.imports
    }
}

pub(crate) struct ParsedModulesOutcome {
    pub(crate) result: Result<ParsedProgram, CompileErrors>,
    pub(crate) work: SyntaxWork,
}

/// Parse every snapshot module independently and assemble canonical artifacts.
pub fn parse_source_snapshot_modules(
    snapshot: &SourceSnapshot,
) -> Result<ParsedProgram, CompileErrors> {
    parse_source_snapshot_modules_with_stats(snapshot).map(|(program, _)| program)
}

/// Parse canonical modules and return the exact syntax work performed.
pub fn parse_source_snapshot_modules_with_stats(
    snapshot: &SourceSnapshot,
) -> Result<(ParsedProgram, SyntaxWork), CompileErrors> {
    let outcome = parse_source_snapshot_modules_with_work(snapshot);
    outcome.result.map(|program| (program, outcome.work))
}

/// Parse one module selected by its stable logical identity.
pub fn parse_source_snapshot_module(
    snapshot: &SourceSnapshot,
    module: &ModuleId,
) -> Result<Arc<ParsedModule>, CompileErrors> {
    parse_source_snapshot_module_with_stats(snapshot, module).map(|(module, _)| module)
}

/// Parse one stable module and return the exact syntax work performed.
pub fn parse_source_snapshot_module_with_stats(
    snapshot: &SourceSnapshot,
    module: &ModuleId,
) -> Result<(Arc<ParsedModule>, SyntaxWork), CompileErrors> {
    let file_id = snapshot
        .metadata()
        .file_ids()
        .find(|file_id| snapshot.module_id(*file_id) == Some(module))
        .ok_or_else(|| {
            CompileErrors::from(invalid_input(format!(
                "source snapshot contains no module {module}"
            )))
        })?;
    let (result, work) = parse_snapshot_file(snapshot, file_id);
    result.map(|module| (module, work))
}

fn parse_snapshot_file(
    snapshot: &SourceSnapshot,
    file_id: FileId,
) -> (Result<Arc<ParsedModule>, CompileErrors>, SyntaxWork) {
    let source = snapshot.source_file(file_id).expect("metadata membership");
    let outcome = crate::syntax::parse_file(source, ThreadedRodeo::new());
    let work = outcome.work;
    let result = outcome.result.and_then(|file| {
        build_module(snapshot, file_id, file.ast, outcome.interner)
            .map(Arc::new)
            .map_err(CompileErrors::from)
    });
    (result, work)
}

pub(crate) fn parse_source_snapshot_modules_with_work(
    snapshot: &SourceSnapshot,
) -> ParsedModulesOutcome {
    let mut file_ids: Vec<_> = snapshot.metadata().file_ids().collect();
    file_ids.sort_by(|left, right| {
        snapshot
            .module_id(*left)
            .unwrap()
            .cmp(snapshot.module_id(*right).unwrap())
    });
    let mut modules = Vec::with_capacity(file_ids.len());
    let mut errors = CompileErrors::new();
    let mut work = SyntaxWork::default();
    for file_id in file_ids {
        let (result, file_work) = parse_snapshot_file(snapshot, file_id);
        work.lexer_invocations += file_work.lexer_invocations;
        work.parser_invocations += file_work.parser_invocations;
        work.lexed_bytes += file_work.lexed_bytes;
        work.tokens += file_work.tokens;
        match result {
            Ok(module) => modules.push(module),
            Err(file_errors) => errors.extend(file_errors),
        }
    }
    let result = if errors.is_empty() {
        ParsedProgram::new(snapshot.source_revision().root().clone(), modules)
            .map_err(CompileErrors::from)
    } else {
        Err(errors)
    };
    ParsedModulesOutcome { result, work }
}

fn build_module(
    snapshot: &SourceSnapshot,
    file_id: FileId,
    ast: Arc<Ast>,
    interner: ThreadedRodeo,
) -> CompileResult<ParsedModule> {
    let module = snapshot
        .module_id(file_id)
        .expect("snapshot membership")
        .clone();
    let source = snapshot
        .source_id(file_id)
        .expect("snapshot membership")
        .clone();
    let source_text = snapshot
        .shared_source_text(file_id)
        .expect("snapshot membership");
    let physical_path = Arc::from(snapshot.metadata().physical_path(file_id).unwrap());

    let imports = collect_imports(&ast, &module, &interner)?;

    let token = Arc::new(SymbolProvenance);
    let resolver = FrozenSymbolResolver {
        resolver: interner.into_resolver(),
        provenance: token.clone(),
    };
    let provenanced_ast = ProvenancedAst {
        ast,
        provenance: token,
        source: source.clone(),
    };
    let revision = ModuleRevision { module, source };
    validate_pair(&provenanced_ast, &resolver, &revision)?;
    let definitions =
        build_definition_index(file_id, &source_text, &provenanced_ast.ast, &resolver)?;
    Ok(ParsedModule {
        revision,
        file_id,
        physical_path,
        source_text,
        ast: provenanced_ast,
        resolver,
        definitions,
        imports: imports.into(),
    })
}

fn collect_imports(
    ast: &Ast,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
) -> CompileResult<Vec<ParsedImportDirective>> {
    let mut imports = Vec::new();
    for item in &ast.items {
        match item {
            Item::Function(value) => {
                walk_signature(
                    &value.params,
                    value.return_type.as_ref(),
                    module,
                    resolver,
                    &mut imports,
                )?;
                walk_expr(&value.body, module, resolver, &mut imports)?;
            }
            Item::Struct(value) => {
                for field in &value.fields {
                    walk_type_expr(&field.ty, module, resolver, &mut imports)?;
                }
                for method in &value.methods {
                    walk_signature(
                        &method.params,
                        method.return_type.as_ref(),
                        module,
                        resolver,
                        &mut imports,
                    )?;
                    walk_expr(&method.body, module, resolver, &mut imports)?;
                }
            }
            Item::DropFn(value) => walk_expr(&value.body, module, resolver, &mut imports)?,
            Item::Const(value) => {
                if let Some(ty) = &value.ty {
                    walk_type_expr(ty, module, resolver, &mut imports)?;
                }
                walk_expr(&value.init, module, resolver, &mut imports)?;
            }
            Item::Enum(value) => {
                for variant in &value.variants {
                    for ty in &variant.payload {
                        walk_type_expr(ty, module, resolver, &mut imports)?;
                    }
                }
            }
            Item::Error(_) => {}
        }
    }
    imports.sort();
    Ok(imports)
}

fn walk_signature(
    params: &[rue_parser::ast::Param],
    return_type: Option<&TypeExpr>,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportDirective>,
) -> CompileResult<()> {
    for param in params {
        walk_type_expr(&param.ty, module, resolver, imports)?;
    }
    if let Some(return_type) = return_type {
        walk_type_expr(return_type, module, resolver, imports)?;
    }
    Ok(())
}

fn walk_type_expr(
    ty: &TypeExpr,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportDirective>,
) -> CompileResult<()> {
    match ty {
        TypeExpr::Named(_)
        | TypeExpr::Qualified { .. }
        | TypeExpr::Unit(_)
        | TypeExpr::Never(_)
        | TypeExpr::StrFixed { .. }
        | TypeExpr::IntArg { .. } => {}
        TypeExpr::Array { element, .. } | TypeExpr::Slice { element, .. } => {
            walk_type_expr(element, module, resolver, imports)?;
        }
        TypeExpr::AnonymousStruct {
            fields, methods, ..
        } => {
            for field in fields {
                walk_type_expr(&field.ty, module, resolver, imports)?;
            }
            for method in methods {
                walk_signature(
                    &method.params,
                    method.return_type.as_ref(),
                    module,
                    resolver,
                    imports,
                )?;
                walk_expr(&method.body, module, resolver, imports)?;
            }
        }
        TypeExpr::AnonymousEnum { variants, .. } => {
            for variant in variants {
                for payload in &variant.payload {
                    walk_type_expr(payload, module, resolver, imports)?;
                }
            }
        }
        TypeExpr::PointerConst { pointee, .. } | TypeExpr::PointerMut { pointee, .. } => {
            walk_type_expr(pointee, module, resolver, imports)?;
        }
        TypeExpr::TypeCall { args, .. } | TypeExpr::QualifiedTypeCall { args, .. } => {
            for arg in args {
                walk_type_expr(arg, module, resolver, imports)?;
            }
        }
    }
    Ok(())
}

fn walk_args(
    args: &[rue_parser::ast::CallArg],
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportDirective>,
) -> CompileResult<()> {
    for arg in args {
        walk_expr(&arg.expr, module, resolver, imports)?;
    }
    Ok(())
}

fn walk_block(
    block: &rue_parser::ast::BlockExpr,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportDirective>,
) -> CompileResult<()> {
    for statement in &block.statements {
        match statement {
            Statement::Let(value) => walk_expr(&value.init, module, resolver, imports)?,
            Statement::Assign(value) => {
                match &value.target {
                    AssignTarget::Var(_) => {}
                    AssignTarget::Field(field) => {
                        walk_expr(&field.base, module, resolver, imports)?
                    }
                    AssignTarget::Index(index) => {
                        walk_expr(&index.base, module, resolver, imports)?;
                        walk_expr(&index.index, module, resolver, imports)?;
                    }
                }
                walk_expr(&value.value, module, resolver, imports)?;
            }
            Statement::Expr(value) => walk_expr(value, module, resolver, imports)?,
        }
    }
    walk_expr(&block.expr, module, resolver, imports)
}

fn walk_expr(
    expr: &Expr,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportDirective>,
) -> CompileResult<()> {
    match expr {
        Expr::Int(_)
        | Expr::String(_)
        | Expr::Bool(_)
        | Expr::Unit(_)
        | Expr::Ident(_)
        | Expr::Continue(_)
        | Expr::SelfExpr(_)
        | Expr::Error(_) => {}
        Expr::TypeLit(value) => {
            walk_type_expr(&value.type_expr, module, resolver, imports)?;
        }
        Expr::Binary(value) => {
            walk_expr(&value.left, module, resolver, imports)?;
            walk_expr(&value.right, module, resolver, imports)?;
        }
        Expr::Unary(value) => walk_expr(&value.operand, module, resolver, imports)?,
        Expr::Paren(value) => walk_expr(&value.inner, module, resolver, imports)?,
        Expr::Block(value) => walk_block(value, module, resolver, imports)?,
        Expr::If(value) => {
            walk_expr(&value.cond, module, resolver, imports)?;
            walk_block(&value.then_block, module, resolver, imports)?;
            if let Some(block) = &value.else_block {
                walk_block(block, module, resolver, imports)?;
            }
        }
        Expr::Match(value) => {
            walk_expr(&value.scrutinee, module, resolver, imports)?;
            for arm in &value.arms {
                if let Pattern::Path(path) = &arm.pattern {
                    if let Some(base) = &path.base {
                        walk_expr(base, module, resolver, imports)?;
                    }
                    if let Some(args) = &path.ctor_args {
                        walk_args(args, module, resolver, imports)?;
                    }
                }
                walk_expr(&arm.body, module, resolver, imports)?;
            }
        }
        Expr::While(value) => {
            walk_expr(&value.cond, module, resolver, imports)?;
            walk_block(&value.body, module, resolver, imports)?;
        }
        Expr::Loop(value) => walk_block(&value.body, module, resolver, imports)?,
        Expr::For(value) => {
            walk_expr(&value.iterable, module, resolver, imports)?;
            walk_block(&value.body, module, resolver, imports)?;
        }
        Expr::Call(value) => walk_args(&value.args, module, resolver, imports)?,
        Expr::Break(value) => {
            if let Some(value) = &value.value {
                walk_expr(value, module, resolver, imports)?;
            }
        }
        Expr::Return(value) => {
            if let Some(value) = &value.value {
                walk_expr(value, module, resolver, imports)?;
            }
        }
        Expr::StructLit(value) => {
            if let Some(base) = &value.base {
                walk_expr(base, module, resolver, imports)?;
            }
            if let Some(args) = &value.ctor_args {
                walk_args(args, module, resolver, imports)?;
            }
            for field in &value.fields {
                walk_expr(&field.value, module, resolver, imports)?;
            }
        }
        Expr::Field(value) => walk_expr(&value.base, module, resolver, imports)?,
        Expr::MethodCall(value) => {
            walk_expr(&value.receiver, module, resolver, imports)?;
            walk_args(&value.args, module, resolver, imports)?;
        }
        Expr::Try(value) => walk_expr(&value.operand, module, resolver, imports)?,
        Expr::IntrinsicCall(value) => {
            let name = resolver.try_resolve(&value.name.name).ok_or_else(|| {
                invalid_input("intrinsic name is absent from the module symbol universe")
            })?;
            if name == "import"
                && let [IntrinsicArg::Expr(Expr::String(literal))] = value.args.as_slice()
            {
                let specifier = resolver.try_resolve(&literal.value).ok_or_else(|| {
                    invalid_input("import literal is absent from the module symbol universe")
                })?;
                imports.push(ParsedImportDirective {
                    importer: module.clone(),
                    source_offset: value.span.start,
                    specifier: Arc::from(specifier),
                });
            }
            for arg in &value.args {
                if let IntrinsicArg::Expr(expr) = arg {
                    walk_expr(expr, module, resolver, imports)?;
                }
            }
        }
        Expr::ArrayLit(value) => {
            for element in &value.elements {
                walk_expr(element, module, resolver, imports)?;
            }
        }
        Expr::Index(value) => {
            walk_expr(&value.base, module, resolver, imports)?;
            walk_expr(&value.index, module, resolver, imports)?;
        }
        Expr::Path(value) => {
            if let Some(base) = &value.base {
                walk_expr(base, module, resolver, imports)?;
            }
        }
        Expr::AssocFnCall(value) => {
            if let Some(base) = &value.base {
                walk_expr(base, module, resolver, imports)?;
            }
            walk_args(&value.args, module, resolver, imports)?;
        }
        Expr::Comptime(value) => walk_expr(&value.expr, module, resolver, imports)?,
        Expr::Checked(value) => walk_expr(&value.expr, module, resolver, imports)?,
    }
    Ok(())
}

fn validate_pair(
    ast: &ProvenancedAst,
    resolver: &FrozenSymbolResolver,
    revision: &ModuleRevision,
) -> CompileResult<()> {
    if !Arc::ptr_eq(&ast.provenance, &resolver.provenance) {
        return Err(invalid_input(
            "parsed AST and resolver have foreign provenance",
        ));
    }
    if ast.source != revision.source {
        return Err(invalid_input(
            "parsed AST and module revision have foreign source provenance",
        ));
    }
    Ok(())
}

fn build_definition_index(
    file_id: FileId,
    source_text: &str,
    ast: &Ast,
    resolver: &FrozenSymbolResolver,
) -> CompileResult<ParsedDefinitionIndex> {
    let mut pending = Vec::new();
    for item in &ast.items {
        let Some(parts) = definition_parts(item) else {
            let Item::Error(span) = item else {
                unreachable!()
            };
            return Err(invalid_input(format!(
                "parsed module contains recovered error item at {}..{}",
                span.start, span.end
            )));
        };
        validate_span(
            "definition declaration",
            parts.declaration_span,
            file_id,
            source_text,
        )?;
        validate_span("definition name", parts.name.span, file_id, source_text)?;
        if parts.name.span.start < parts.declaration_span.start
            || parts.name.span.end > parts.declaration_span.end
        {
            return Err(invalid_input(
                "definition name span is outside its declaration span",
            ));
        }
        let symbol = resolver.symbol(parts.name.name)?;
        let name: Arc<str> = Arc::from(resolver.resolve(&symbol)?);
        pending.push((parts, symbol, name));
    }
    pending.sort_by(|(left, _, left_name), (right, _, right_name)| {
        (
            left.declaration_span.start,
            left.declaration_span.end,
            left.kind,
            left_name,
        )
            .cmp(&(
                right.declaration_span.start,
                right.declaration_span.end,
                right.kind,
                right_name,
            ))
    });
    let mut by_name = BTreeMap::<_, Vec<_>>::new();
    let mut candidates = Vec::with_capacity(pending.len());
    for (index, (parts, symbol, name)) in pending.into_iter().enumerate() {
        let index = u32::try_from(index)
            .map_err(|_| invalid_input("parsed definition occurrence count exceeds u32"))?;
        let occurrence = ParsedDefinitionOccurrence(index);
        by_name
            .entry((parts.namespace, name.clone()))
            .or_default()
            .push(occurrence);
        candidates.push(ParsedDefinitionCandidate {
            occurrence,
            namespace: parts.namespace,
            kind: parts.kind,
            visibility: parts.visibility,
            name,
            symbol,
            name_span: parts.name.span,
            declaration_span: parts.declaration_span,
        });
    }
    let by_name = by_name
        .into_iter()
        .map(|(key, value)| (key, value.into()))
        .collect();
    Ok(ParsedDefinitionIndex {
        candidates: candidates.into(),
        by_name,
    })
}

fn invalid_input(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lasso::Key;
    use rue_error::{PreviewFeature, PreviewFeatures};

    use super::*;
    use crate::{
        CompilationUnit, CompileOptions, ModuleResolutionInput, ModuleResolutionInputs,
        SemanticInputDescriptor, SourceFile, SourceMetadata, extract_import_directives,
    };

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<HashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<HashMap<_, _>>();
        let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
        SourceSnapshot::new(
            metadata,
            entries
                .iter()
                .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    fn error_fingerprint(errors: &CompileErrors) -> Vec<String> {
        errors
            .iter()
            .map(|error| {
                format!(
                    "{}|{:?}|{}|{:?}",
                    error.kind.code(),
                    error.span(),
                    error,
                    error.diagnostic()
                )
            })
            .collect()
    }

    #[test]
    fn modules_are_canonical_arc_shareable_and_carry_import_values() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ParsedModule>();
        assert_send_sync::<ParsedProgram>();

        let snapshot = snapshot(
            &[
                (
                    20,
                    "/p/main.rue",
                    "app/main.rue",
                    "fn same() {} fn same() {} fn main() -> i32 { if true { let h = @import(\"helper.rue\"); } 0 }",
                ),
                (1, "/p/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            20,
        );
        let outcome = parse_source_snapshot_modules_with_work(&snapshot);
        assert_eq!(outcome.work.lexer_invocations, 2);
        assert_eq!(outcome.work.parser_invocations, 2);
        let program = outcome.result.unwrap();
        assert_eq!(
            program
                .modules()
                .iter()
                .map(|m| m.module_id().as_str())
                .collect::<Vec<_>>(),
            ["app/helper.rue", "app/main.rue"]
        );
        let main = &program.modules()[1];
        assert_eq!(main.imports().len(), 1);
        assert_eq!(main.imports()[0].specifier(), "helper.rue");
        assert_eq!(main.imports()[0].importer(), main.module_id());
        let duplicates = main
            .definitions()
            .candidates_named(DefinitionNamespace::ModuleItem, "same")
            .collect::<Vec<_>>();
        assert_eq!(duplicates.len(), 2);
        assert_ne!(duplicates[0].occurrence(), duplicates[1].occurrence());
        assert_eq!(main.resolve(duplicates[0].symbol()).unwrap(), "same");
        let graph = crate::resolve_canonical_import_graph(
            program.import_directives(),
            &ModuleResolutionInputs::from_metadata(snapshot.metadata()),
            None,
        )
        .unwrap();
        assert_eq!(graph.records().len(), 1);
    }

    #[test]
    fn foreign_same_numeric_spur_and_ast_resolver_pairs_fail_closed() {
        let make = || {
            let rodeo = ThreadedRodeo::new();
            let spur = rodeo.get_or_intern("same-index");
            let provenance = Arc::new(SymbolProvenance);
            (
                FrozenSymbolResolver {
                    resolver: rodeo.into_resolver(),
                    provenance: provenance.clone(),
                },
                ParsedSymbol { spur, provenance },
            )
        };
        let (first, symbol) = make();
        let (foreign, foreign_symbol) = make();
        assert_eq!(symbol.spur.into_usize(), foreign_symbol.spur.into_usize());
        assert_eq!(first.resolve(&symbol).unwrap(), "same-index");
        assert_eq!(
            foreign.resolve(&symbol).unwrap_err().to_string(),
            "invalid compiler input: parsed symbol belongs to a foreign symbol universe"
        );
        let source = SourceId::from_shared_text(Arc::new(String::from("one")));
        let ast = ProvenancedAst {
            ast: Arc::new(Ast { items: Vec::new() }),
            provenance: symbol.provenance,
            source: source.clone(),
        };
        let revision = ModuleRevision {
            module: ModuleId::from_logical_path("module.rue").unwrap(),
            source: source.clone(),
        };
        assert_eq!(
            validate_pair(&ast, &foreign, &revision)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: parsed AST and resolver have foreign provenance"
        );
        let own_resolver = FrozenSymbolResolver {
            resolver: ThreadedRodeo::new().into_resolver(),
            provenance: ast.provenance.clone(),
        };
        let foreign_revision = ModuleRevision {
            module: revision.module,
            source: SourceId::from_shared_text(Arc::new(String::from("two"))),
        };
        assert_eq!(
            validate_pair(&ast, &own_resolver, &foreign_revision)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: parsed AST and module revision have foreign source provenance"
        );
    }

    #[test]
    fn assembling_reused_modules_does_no_additional_syntax_work() {
        let snapshot = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/helper.rue", "helper.rue", "fn helper() {}"),
            ],
            7,
        );
        let outcome = parse_source_snapshot_modules_with_work(&snapshot);
        let work = outcome.work;
        let first = outcome.result.unwrap();
        let modules = first.modules().to_vec();
        let second = ParsedProgram::new(first.root().clone(), modules.clone()).unwrap();
        assert_eq!(first.source_revision(), second.source_revision());
        assert_eq!(first.source_revision(), snapshot.source_revision());
        assert!(
            first
                .modules()
                .iter()
                .zip(second.modules())
                .all(|(left, right)| Arc::ptr_eq(left, right))
        );

        let base = SemanticInputDescriptor::new(
            &snapshot,
            crate::Target::default(),
            &PreviewFeatures::default(),
        );
        let mut changed_target = base.clone();
        changed_target.target = if base.target == crate::Target::X86_64Linux {
            crate::Target::Aarch64Linux
        } else {
            crate::Target::X86_64Linux
        };
        let changed_resolution = ModuleResolutionInputs::new(
            base.resolution.root().clone(),
            base.resolution
                .modules()
                .iter()
                .map(|entry| ModuleResolutionInput {
                    module: entry.module.clone(),
                    physical_path: Arc::from(format!("/moved/{}", entry.module.as_str())),
                })
                .collect(),
        )
        .unwrap();
        let mut changed_features = base.clone();
        let mut features = PreviewFeatures::default();
        features.insert(PreviewFeature::TestInfra);
        changed_features.preview_features = crate::StablePreviewFeatures::new(&features);
        assert_ne!(base.target, changed_target.target);
        assert_ne!(base.resolution, changed_resolution);
        assert_ne!(base.preview_features, changed_features.preview_features);
        assert_eq!(work.lexer_invocations, 2);
        assert_eq!(work.parser_invocations, 2);
    }

    #[test]
    fn assembly_rejects_duplicate_request_local_file_ids_deterministically() {
        let first = snapshot(&[(7, "/a.rue", "a.rue", "fn a() {}")], 7);
        let second = snapshot(&[(7, "/b.rue", "b.rue", "fn b() {}")], 7);
        let a = parse_source_snapshot_modules(&first).unwrap().modules()[0].clone();
        let b = parse_source_snapshot_modules(&second).unwrap().modules()[0].clone();

        let error = ParsedProgram::new(a.module_id().clone(), vec![b, a])
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "invalid compiler input: parsed program contains duplicate file ID 7 for modules a.rue and b.rue"
        );
    }

    #[test]
    fn one_edited_module_reparses_once_and_reuses_unchanged_module_arc() {
        let initial = snapshot(
            &[
                (1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/helper.rue", "helper.rue", "fn helper() -> i32 { 1 }"),
            ],
            1,
        );
        let edited = snapshot(
            &[
                (1, "/main.rue", "main.rue", "fn main() -> i32 { 2 }"),
                (2, "/helper.rue", "helper.rue", "fn helper() -> i32 { 1 }"),
            ],
            1,
        );
        let old = parse_source_snapshot_modules(&initial).unwrap();
        let old_main = old
            .modules()
            .iter()
            .find(|module| module.module_id().as_str() == "main.rue")
            .unwrap();
        let unchanged = old
            .modules()
            .iter()
            .find(|module| module.module_id().as_str() == "helper.rue")
            .unwrap()
            .clone();
        let main_id = ModuleId::from_logical_path("main.rue").unwrap();
        let (new_main, work) = parse_source_snapshot_module_with_stats(&edited, &main_id).unwrap();
        assert_eq!(work.lexer_invocations, 1);
        assert_eq!(work.parser_invocations, 1);
        assert_ne!(old_main.revision(), new_main.revision());

        let assembled =
            ParsedProgram::new(main_id, vec![new_main.clone(), unchanged.clone()]).unwrap();
        let reused = assembled
            .modules()
            .iter()
            .find(|module| module.module_id().as_str() == "helper.rue")
            .unwrap();
        assert!(Arc::ptr_eq(reused, &unchanged));
        assert_eq!(assembled.source_revision(), edited.source_revision());
    }

    #[test]
    fn type_position_anonymous_method_imports_match_positional_rir_extraction() {
        let source = r#"
const top = @import("top");
fn consume(value: i32) {}
fn make_type() -> type {
    struct {
        field: i32,
        fn load() -> i32 {
            let body = @import("body");
            4
        }
    }
}
fn main() -> i32 {
    let array = [@import("array"), @import("array2")];
    if true { consume(@import("call_arg")); } else { let other = @import("else_block"); }
    let nested = @dbg(@import("intrinsic_arg"));
    let indexed = [@import("index_base")][0];
    comptime { @import("comptime") };
    0
}
"#;
        let snapshot = snapshot(&[(3, "/main.rue", "main.rue", source)], 3);
        let parsed = parse_source_snapshot_modules(&snapshot).unwrap();
        let parsed_values = parsed
            .import_directives()
            .iter()
            .map(|directive| (directive.source_offset(), directive.specifier()))
            .collect::<Vec<_>>();

        let file_id = FileId::new(3);
        let sources = vec![SourceFile::new("/main.rue", source, file_id)];
        let metadata = SourceMetadata::from_sources(
            &sources,
            file_id,
            HashMap::from([(file_id, String::from("main.rue"))]),
        )
        .unwrap();
        let mut unit = CompilationUnit::with_source_metadata(
            sources,
            metadata.clone(),
            CompileOptions::default(),
        )
        .unwrap();
        unit.parse().unwrap();
        unit.lower().unwrap();
        let rir = extract_import_directives(unit.rir(), unit.interner(), &metadata).unwrap();
        let rir_values = rir
            .iter()
            .map(|directive| (directive.source_offset(), directive.specifier()))
            .collect::<Vec<_>>();

        assert_eq!(parsed_values, rir_values);
        assert_eq!(parsed_values.len(), 9);
        assert!(
            parsed_values
                .iter()
                .any(|(_, specifier)| *specifier == "body")
        );
    }

    #[test]
    fn input_order_and_file_ids_do_not_change_canonical_module_values() {
        let first = snapshot(
            &[
                (9, "/one/main.rue", "app/main.rue", "fn main() -> i32 { 0 }"),
                (2, "/one/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            9,
        );
        let moved = snapshot(
            &[
                (70, "/moved/helper.rue", "app/helper.rue", "fn helper() {}"),
                (
                    100,
                    "/moved/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { 0 }",
                ),
            ],
            100,
        );
        let first = parse_source_snapshot_modules(&first).unwrap();
        let moved = parse_source_snapshot_modules(&moved).unwrap();
        assert_eq!(first.root(), moved.root());
        assert_eq!(
            first
                .modules()
                .iter()
                .map(|module| module.revision())
                .collect::<Vec<_>>(),
            moved
                .modules()
                .iter()
                .map(|module| module.revision())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn reordered_broken_inputs_have_identical_diagnostics_and_legacy_kernel_parity() {
        let entries = [
            (9, "/z.rue", "z.rue", "fn z( {"),
            (2, "/a.rue", "a.rue", "fn a() { let x = #; }"),
        ];
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect();
        let metadata = SourceMetadata::new(FileId::new(2), physical, logical).unwrap();
        let canonical_contents = vec![
            (FileId::new(2), Arc::new(entries[1].3.to_owned())),
            (FileId::new(9), Arc::new(entries[0].3.to_owned())),
        ];
        let mut reversed_contents = canonical_contents.clone();
        reversed_contents.reverse();
        let canonical = SourceSnapshot::new(metadata.clone(), canonical_contents).unwrap();
        let reversed = SourceSnapshot::new(metadata, reversed_contents).unwrap();

        let first = parse_source_snapshot_modules(&canonical).unwrap_err();
        let second = parse_source_snapshot_modules(&reversed).unwrap_err();
        assert_eq!(error_fingerprint(&first), error_fingerprint(&second));

        let legacy = crate::parse_all_files_with_source_snapshot(&canonical).unwrap_err();
        assert_eq!(error_fingerprint(&first), error_fingerprint(&legacy));
    }
}
