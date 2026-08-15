//! Dense, parser-structured type syntax shared by declaration artifacts.
//!
//! The parser already distinguishes arrays, pointers, calls, paths, and value
//! arguments.  This arena preserves that structure in source order instead of
//! rendering it to an interned string and asking semantic analysis to recover
//! the grammar later.  Symbol identity is deliberately generic: candidate RIR
//! uses candidate-local `Spur`s, while durable signature projections use their
//! own compact spelling authority.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use rue_parser::ast::{ArrayLength, DirectiveArg, ParamMode, TypeExpr};

/// Dense index of one structured type-syntax node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RirTypeSyntaxRef(u32);

impl RirTypeSyntaxRef {
    pub fn from_u32(value: u32) -> Self {
        Self(value)
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Dense index of one spelling in the arena's caller-selected symbol domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RirTypeSyntaxSymbol(u32);

impl RirTypeSyntaxSymbol {
    pub fn from_u32(value: u32) -> Self {
        Self(value)
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Canonical word range in the arena's variable-width storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RirTypeSyntaxRange {
    start: u32,
    len: u32,
}

impl RirTypeSyntaxRange {
    pub fn start(self) -> u32 {
        self.start
    }

    pub fn len(self) -> u32 {
        self.len
    }

    pub fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// One canonical structured type node.
///
/// Child references are postorder: every child has a smaller index than its
/// owner.  `TypeCall` arguments are type/value syntax nodes. `ValueCall` is the
/// corresponding grammar used by array-length expressions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RirTypeSyntaxNode {
    Named(RirTypeSyntaxSymbol),
    Qualified {
        path: RirTypeSyntaxRange,
    },
    Unit,
    Never,
    Array {
        element: RirTypeSyntaxRef,
        length: RirTypeSyntaxRef,
    },
    Slice {
        element: RirTypeSyntaxRef,
    },
    AnonymousStruct {
        /// Fixed-width `(name symbol, type ref)` pairs.
        fields: RirTypeSyntaxRange,
        /// Canonical variable-width method-signature records. Method bodies
        /// belong to the candidate body artifact, not the type grammar.
        methods: RirTypeSyntaxRange,
    },
    AnonymousEnum {
        /// Canonical variable-width `(name, payload count, payload refs...)`
        /// records.
        variants: RirTypeSyntaxRange,
    },
    PointerConst {
        pointee: RirTypeSyntaxRef,
    },
    PointerMut {
        pointee: RirTypeSyntaxRef,
    },
    TypeCall {
        path: RirTypeSyntaxRange,
        arguments: RirTypeSyntaxRange,
    },
    ValueCall {
        name: RirTypeSyntaxSymbol,
        arguments: RirTypeSyntaxRange,
    },
    Integer(i128),
}

/// Immutable dense type-syntax owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RirTypeSyntaxArena<S> {
    nodes: Arc<[RirTypeSyntaxNode]>,
    extra: Arc<[u32]>,
    symbols: Arc<[S]>,
}

impl<S> RirTypeSyntaxArena<S> {
    pub fn nodes(&self) -> &[RirTypeSyntaxNode] {
        &self.nodes
    }

    /// Number of words retained by variable-width typed records.
    pub fn payload_word_count(&self) -> usize {
        self.extra.len()
    }

    pub fn symbols(&self) -> &[S] {
        &self.symbols
    }

    pub fn node(&self, reference: RirTypeSyntaxRef) -> Option<&RirTypeSyntaxNode> {
        self.nodes.get(reference.index())
    }

    pub fn symbol(&self, symbol: RirTypeSyntaxSymbol) -> Option<&S> {
        self.symbols.get(symbol.index())
    }

    pub fn words(&self, range: RirTypeSyntaxRange) -> Option<&[u32]> {
        let start = range.start as usize;
        let end = start.checked_add(range.len as usize)?;
        self.extra.get(start..end)
    }
}

impl<S: AsRef<str>> RirTypeSyntaxArena<S> {
    /// Render the canonical diagnostic spelling of one structured node.
    /// Semantic consumers must inspect nodes directly; this exists only for
    /// diagnostics and the temporary legacy body-RIR adapter.
    pub fn render_type(&self, reference: RirTypeSyntaxRef) -> Option<String> {
        let mut output = String::new();
        self.write_type(reference, &mut output).then_some(output)
    }

    fn write_type(&self, reference: RirTypeSyntaxRef, output: &mut String) -> bool {
        let Some(node) = self.node(reference) else {
            return false;
        };
        match node {
            RirTypeSyntaxNode::Named(symbol) => {
                let Some(symbol) = self.symbol(*symbol) else {
                    return false;
                };
                output.push_str(symbol.as_ref());
            }
            RirTypeSyntaxNode::Qualified { path } => {
                if !self.write_path(*path, output) {
                    return false;
                }
            }
            RirTypeSyntaxNode::Unit => output.push_str("()"),
            RirTypeSyntaxNode::Never => output.push('!'),
            RirTypeSyntaxNode::Array { element, length } => {
                output.push('[');
                if !self.write_type(*element, output) {
                    return false;
                }
                output.push_str("; ");
                if !self.write_type(*length, output) {
                    return false;
                }
                output.push(']');
            }
            RirTypeSyntaxNode::Slice { element } => {
                output.push('[');
                if !self.write_type(*element, output) {
                    return false;
                }
                output.push(']');
            }
            RirTypeSyntaxNode::AnonymousStruct { .. } => output.push_str("struct { ... }"),
            RirTypeSyntaxNode::AnonymousEnum { .. } => output.push_str("enum { ... }"),
            RirTypeSyntaxNode::PointerConst { pointee } => {
                output.push_str("ptr const ");
                if !self.write_type(*pointee, output) {
                    return false;
                }
            }
            RirTypeSyntaxNode::PointerMut { pointee } => {
                output.push_str("ptr mut ");
                if !self.write_type(*pointee, output) {
                    return false;
                }
            }
            RirTypeSyntaxNode::TypeCall { path, arguments } => {
                if !self.write_path(*path, output) {
                    return false;
                }
                output.push('(');
                if !self.write_arguments(*arguments, output) {
                    return false;
                }
                output.push(')');
            }
            RirTypeSyntaxNode::ValueCall { name, arguments } => {
                let Some(name) = self.symbol(*name) else {
                    return false;
                };
                output.push_str(name.as_ref());
                output.push('(');
                if !self.write_arguments(*arguments, output) {
                    return false;
                }
                output.push(')');
            }
            RirTypeSyntaxNode::Integer(value) => output.push_str(&value.to_string()),
        }
        true
    }

    fn write_path(&self, range: RirTypeSyntaxRange, output: &mut String) -> bool {
        let Some(words) = self.words(range) else {
            return false;
        };
        for (index, word) in words.iter().enumerate() {
            let Some(symbol) = self.symbol(RirTypeSyntaxSymbol::from_u32(*word)) else {
                return false;
            };
            if index != 0 {
                output.push('.');
            }
            output.push_str(symbol.as_ref());
        }
        true
    }

    fn write_arguments(&self, range: RirTypeSyntaxRange, output: &mut String) -> bool {
        let Some(words) = self.words(range) else {
            return false;
        };
        for (index, word) in words.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            if !self.write_type(RirTypeSyntaxRef::from_u32(*word), output) {
                return false;
            }
        }
        true
    }
}

/// Capacity failure while projecting parser-owned type structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RirTypeSyntaxBuildError {
    TooManyNodes,
    TooManySymbols,
    TooMuchPayload,
}

/// Mutable producer for one declaration-local syntax arena.
pub struct RirTypeSyntaxBuilder<S> {
    nodes: Vec<RirTypeSyntaxNode>,
    extra: Vec<u32>,
    symbols: Vec<S>,
    symbol_indexes: HashMap<S, RirTypeSyntaxSymbol>,
}

impl<S> Default for RirTypeSyntaxBuilder<S> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            extra: Vec::new(),
            symbols: Vec::new(),
            symbol_indexes: HashMap::new(),
        }
    }
}

impl<S: Clone + Eq + Hash> RirTypeSyntaxBuilder<S> {
    pub fn finish(self) -> RirTypeSyntaxArena<S> {
        RirTypeSyntaxArena {
            nodes: self.nodes.into(),
            extra: self.extra.into(),
            symbols: self.symbols.into(),
        }
    }

    pub fn push_parser_type(
        &mut self,
        ty: &TypeExpr,
        resolve: impl Copy + Fn(lasso::Spur) -> S,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        let node = match ty {
            TypeExpr::Named(ident) => RirTypeSyntaxNode::Named(self.symbol(resolve(ident.name))?),
            TypeExpr::Qualified { segments, .. } => {
                let path =
                    self.symbol_path(segments.iter().map(|segment| resolve(segment.name)))?;
                RirTypeSyntaxNode::Qualified { path }
            }
            TypeExpr::Unit(_) => RirTypeSyntaxNode::Unit,
            TypeExpr::Never(_) => RirTypeSyntaxNode::Never,
            TypeExpr::Array {
                element, length, ..
            } => RirTypeSyntaxNode::Array {
                element: self.push_parser_type(element, resolve)?,
                length: self.push_array_length(length, resolve)?,
            },
            TypeExpr::Slice { element, .. } => RirTypeSyntaxNode::Slice {
                element: self.push_parser_type(element, resolve)?,
            },
            TypeExpr::AnonymousStruct {
                fields, methods, ..
            } => {
                let mut field_words = Vec::with_capacity(fields.len().saturating_mul(2));
                for field in fields {
                    let name = self.symbol(resolve(field.name.name))?;
                    let ty = self.push_parser_type(&field.ty, resolve)?;
                    field_words.extend([name.as_u32(), ty.as_u32()]);
                }
                let fields = self.push_words(field_words)?;

                let mut method_words = Vec::new();
                for method in methods {
                    let name = self.symbol(resolve(method.name.name))?;
                    method_words.push(name.as_u32());
                    let receiver = method
                        .receiver
                        .as_ref()
                        .map_or(u32::MAX, |receiver| stable_param_mode(receiver.mode));
                    method_words.push(receiver);
                    method_words.push(u32::from(method.borrow_return.is_some()));
                    method_words.push(
                        u32::try_from(method.params.len())
                            .map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)?,
                    );
                    for parameter in &method.params {
                        method_words.push(stable_param_mode(parameter.mode));
                        let parameter_name = self.symbol(resolve(parameter.name.name))?;
                        let parameter_ty = self.push_parser_type(&parameter.ty, resolve)?;
                        method_words.push(parameter_name.as_u32());
                        method_words.push(parameter_ty.as_u32());
                    }
                    let result = match &method.return_type {
                        Some(result) => self.push_parser_type(result, resolve)?,
                        None => self.push_node(RirTypeSyntaxNode::Unit)?,
                    };
                    method_words.push(result.as_u32());
                    method_words.push(
                        u32::try_from(method.directives.len())
                            .map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)?,
                    );
                    for directive in &method.directives {
                        let directive_name = self.symbol(resolve(directive.name.name))?;
                        method_words.push(directive_name.as_u32());
                        method_words.push(
                            u32::try_from(directive.args.len())
                                .map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)?,
                        );
                        for argument in &directive.args {
                            let DirectiveArg::Ident(argument) = argument;
                            let argument = self.symbol(resolve(argument.name))?;
                            method_words.push(argument.as_u32());
                        }
                    }
                }
                let methods = self.push_words(method_words)?;
                RirTypeSyntaxNode::AnonymousStruct { fields, methods }
            }
            TypeExpr::AnonymousEnum { variants, .. } => {
                let mut words = Vec::new();
                for variant in variants {
                    let name = self.symbol(resolve(variant.name.name))?;
                    words.push(name.as_u32());
                    words.push(
                        u32::try_from(variant.payload.len())
                            .map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)?,
                    );
                    for payload in &variant.payload {
                        let payload = self.push_parser_type(payload, resolve)?;
                        words.push(payload.as_u32());
                    }
                }
                RirTypeSyntaxNode::AnonymousEnum {
                    variants: self.push_words(words)?,
                }
            }
            TypeExpr::PointerConst { pointee, .. } => RirTypeSyntaxNode::PointerConst {
                pointee: self.push_parser_type(pointee, resolve)?,
            },
            TypeExpr::PointerMut { pointee, .. } => RirTypeSyntaxNode::PointerMut {
                pointee: self.push_parser_type(pointee, resolve)?,
            },
            TypeExpr::TypeCall { name, args, .. } => {
                let name = self.symbol(resolve(name.name))?;
                let path = self.push_words([name.as_u32()])?;
                let arguments = self.type_arguments(args, resolve)?;
                RirTypeSyntaxNode::TypeCall { path, arguments }
            }
            TypeExpr::QualifiedTypeCall { segments, args, .. } => {
                let path =
                    self.symbol_path(segments.iter().map(|segment| resolve(segment.name)))?;
                let arguments = self.type_arguments(args, resolve)?;
                RirTypeSyntaxNode::TypeCall { path, arguments }
            }
            TypeExpr::StrFixed { name, length, .. } => {
                let name = self.symbol(resolve(name.name))?;
                let path = self.push_words([name.as_u32()])?;
                let argument = self.push_node(RirTypeSyntaxNode::Integer(i128::from(*length)))?;
                let arguments = self.push_words([argument.as_u32()])?;
                RirTypeSyntaxNode::TypeCall { path, arguments }
            }
            TypeExpr::IntArg { value, .. } => RirTypeSyntaxNode::Integer(*value),
        };
        self.push_node(node)
    }

    pub fn intern_symbol(
        &mut self,
        symbol: S,
    ) -> Result<RirTypeSyntaxSymbol, RirTypeSyntaxBuildError> {
        self.symbol(symbol)
    }

    pub fn push_unit_type(&mut self) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        self.push_node(RirTypeSyntaxNode::Unit)
    }

    fn push_array_length(
        &mut self,
        length: &ArrayLength,
        resolve: impl Copy + Fn(lasso::Spur) -> S,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        let node = match length {
            ArrayLength::Literal(value) => RirTypeSyntaxNode::Integer(i128::from(*value)),
            ArrayLength::Named(name) => RirTypeSyntaxNode::Named(self.symbol(resolve(name.name))?),
            ArrayLength::Call { name, args } => RirTypeSyntaxNode::ValueCall {
                name: self.symbol(resolve(name.name))?,
                arguments: {
                    let mut arguments = Vec::with_capacity(args.len());
                    for argument in args {
                        let argument = self.push_array_length(argument, resolve)?;
                        arguments.push(argument.as_u32());
                    }
                    self.push_words(arguments)?
                },
            },
        };
        self.push_node(node)
    }

    fn type_arguments(
        &mut self,
        args: &[TypeExpr],
        resolve: impl Copy + Fn(lasso::Spur) -> S,
    ) -> Result<RirTypeSyntaxRange, RirTypeSyntaxBuildError> {
        let mut arguments = Vec::with_capacity(args.len());
        for argument in args {
            let argument = self.push_parser_type(argument, resolve)?;
            arguments.push(argument.as_u32());
        }
        self.push_words(arguments)
    }

    fn symbol_path(
        &mut self,
        symbols: impl IntoIterator<Item = S>,
    ) -> Result<RirTypeSyntaxRange, RirTypeSyntaxBuildError> {
        let start = self.extra_start()?;
        for symbol in symbols {
            let symbol = self.symbol(symbol)?;
            self.push_extra(symbol.as_u32())?;
        }
        self.extra_range(start)
    }

    fn symbol(&mut self, symbol: S) -> Result<RirTypeSyntaxSymbol, RirTypeSyntaxBuildError> {
        if let Some(index) = self.symbol_indexes.get(&symbol) {
            return Ok(*index);
        }
        let index = u32::try_from(self.symbols.len())
            .map_err(|_| RirTypeSyntaxBuildError::TooManySymbols)?;
        let index = RirTypeSyntaxSymbol(index);
        self.symbols.push(symbol.clone());
        self.symbol_indexes.insert(symbol, index);
        Ok(index)
    }

    fn push_node(
        &mut self,
        node: RirTypeSyntaxNode,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        let index =
            u32::try_from(self.nodes.len()).map_err(|_| RirTypeSyntaxBuildError::TooManyNodes)?;
        self.nodes.push(node);
        Ok(RirTypeSyntaxRef(index))
    }

    fn push_words(
        &mut self,
        words: impl IntoIterator<Item = u32>,
    ) -> Result<RirTypeSyntaxRange, RirTypeSyntaxBuildError> {
        let start = self.extra_start()?;
        for word in words {
            self.push_extra(word)?;
        }
        self.extra_range(start)
    }

    fn push_extra(&mut self, word: u32) -> Result<(), RirTypeSyntaxBuildError> {
        if self.extra.len() >= u32::MAX as usize {
            return Err(RirTypeSyntaxBuildError::TooMuchPayload);
        }
        self.extra.push(word);
        Ok(())
    }

    fn extra_start(&self) -> Result<u32, RirTypeSyntaxBuildError> {
        u32::try_from(self.extra.len()).map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)
    }

    fn extra_range(&self, start: u32) -> Result<RirTypeSyntaxRange, RirTypeSyntaxBuildError> {
        let end = self.extra_start()?;
        Ok(RirTypeSyntaxRange {
            start,
            len: end
                .checked_sub(start)
                .ok_or(RirTypeSyntaxBuildError::TooMuchPayload)?,
        })
    }
}

fn stable_param_mode(mode: ParamMode) -> u32 {
    match mode {
        ParamMode::Normal => 0,
        ParamMode::Inout => 1,
        ParamMode::Borrow => 2,
        ParamMode::Comptime => 3,
    }
}

#[cfg(test)]
mod tests {
    use lasso::ThreadedRodeo;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_parser::ast::Item;

    use super::*;

    fn parse_type(source: &str) -> (TypeExpr, ThreadedRodeo) {
        let source = format!("fn probe(value: {source}) {{}}");
        let (tokens, interner) = Lexer::new(&source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let Item::Function(function) = &ast.items[0] else {
            panic!("fixture parses as a function");
        };
        (function.params[0].ty.clone(), interner)
    }

    #[test]
    fn parser_structure_is_dense_postorder_and_symbol_stable() {
        let (ty, interner) = parse_type("ptr mut Result([Widget; fact(N)], lib.Option(Str(8)))");
        let mut builder = RirTypeSyntaxBuilder::default();
        let root = builder
            .push_parser_type(&ty, |symbol| Arc::<str>::from(interner.resolve(&symbol)))
            .unwrap();
        let arena = builder.finish();

        assert_eq!(root.index(), arena.nodes().len() - 1);
        assert_eq!(
            arena
                .symbols()
                .iter()
                .filter(|name| name.as_ref() == "Result")
                .count(),
            1
        );
        assert!(matches!(
            arena.node(root),
            Some(RirTypeSyntaxNode::PointerMut { .. })
        ));
        for (owner, node) in arena.nodes().iter().enumerate() {
            let mut children = Vec::new();
            match node {
                RirTypeSyntaxNode::Array { element, length } => {
                    children.extend([*element, *length]);
                }
                RirTypeSyntaxNode::Slice { element }
                | RirTypeSyntaxNode::PointerConst { pointee: element }
                | RirTypeSyntaxNode::PointerMut { pointee: element } => children.push(*element),
                RirTypeSyntaxNode::TypeCall { arguments, .. }
                | RirTypeSyntaxNode::ValueCall { arguments, .. } => {
                    children.extend(
                        arena
                            .words(*arguments)
                            .unwrap()
                            .iter()
                            .copied()
                            .map(RirTypeSyntaxRef::from_u32),
                    );
                }
                _ => {}
            }
            assert!(children.iter().all(|child| child.index() < owner));
        }
    }
}
