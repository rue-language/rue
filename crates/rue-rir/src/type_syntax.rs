//! Dense, parser-structured type syntax shared by declaration artifacts.
//!
//! The parser already distinguishes arrays, pointers, calls, paths, and value
//! arguments.  This arena preserves that structure in source order instead of
//! rendering it to an interned string and asking semantic analysis to recover
//! the grammar later.  Symbol identity is deliberately generic: candidate RIR
//! uses candidate-local `Spur`s, while durable signature projections use their
//! own compact spelling authority.

use std::hash::Hash;
use std::sync::Arc;

use ahash::AHashMap;
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

impl<S> Default for RirTypeSyntaxArena<S> {
    fn default() -> Self {
        Self {
            nodes: Arc::from([]),
            extra: Arc::from([]),
            symbols: Arc::from([]),
        }
    }
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

    /// Logical heap bytes retained by the arena's three shared slices.
    ///
    /// The enclosing RIR owns the inline arena headers. Rue's retained-value
    /// accounting charges each reached Arc pointee in full and uses logical
    /// element length rather than allocator capacity.
    pub fn retained_allocation_charge(&self) -> u64 {
        self.nodes
            .len()
            .saturating_mul(std::mem::size_of::<RirTypeSyntaxNode>())
            .saturating_add(self.extra.len().saturating_mul(std::mem::size_of::<u32>()))
            .saturating_add(self.symbols.len().saturating_mul(std::mem::size_of::<S>()))
            as u64
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

    /// Visit every direct child type reference in canonical schema order.
    /// Variable-width anonymous records are decoded here so semantic consumers
    /// never need a second handwritten payload parser.
    pub fn visit_child_references(
        &self,
        reference: RirTypeSyntaxRef,
        mut visit: impl FnMut(RirTypeSyntaxRef),
    ) -> bool {
        let Some(node) = self.node(reference) else {
            return false;
        };
        match node {
            RirTypeSyntaxNode::Named(_)
            | RirTypeSyntaxNode::Qualified { .. }
            | RirTypeSyntaxNode::Unit
            | RirTypeSyntaxNode::Never
            | RirTypeSyntaxNode::Integer(_) => {}
            RirTypeSyntaxNode::Array { element, length } => {
                visit(*element);
                visit(*length);
            }
            RirTypeSyntaxNode::Slice { element } => visit(*element),
            RirTypeSyntaxNode::PointerConst { pointee }
            | RirTypeSyntaxNode::PointerMut { pointee } => visit(*pointee),
            RirTypeSyntaxNode::TypeCall { arguments, .. }
            | RirTypeSyntaxNode::ValueCall { arguments, .. } => {
                let Some(arguments) = self.words(*arguments) else {
                    return false;
                };
                for argument in arguments {
                    visit(RirTypeSyntaxRef::from_u32(*argument));
                }
            }
            RirTypeSyntaxNode::AnonymousStruct { fields, methods } => {
                let Some(fields) = self.words(*fields) else {
                    return false;
                };
                if fields.len() % 2 != 0 {
                    return false;
                }
                for field in fields.chunks_exact(2) {
                    visit(RirTypeSyntaxRef::from_u32(field[1]));
                }
                let Some(methods) = self.words(*methods) else {
                    return false;
                };
                let mut position = 0usize;
                while position < methods.len() {
                    let Some(header) = methods.get(position..position + 4) else {
                        return false;
                    };
                    position += 4;
                    for _ in 0..header[3] {
                        let Some(parameter) = methods.get(position..position + 3) else {
                            return false;
                        };
                        visit(RirTypeSyntaxRef::from_u32(parameter[2]));
                        position += 3;
                    }
                    let Some(result) = methods.get(position) else {
                        return false;
                    };
                    visit(RirTypeSyntaxRef::from_u32(*result));
                    let Some(directive_count) = methods.get(position + 1) else {
                        return false;
                    };
                    position += 2;
                    for _ in 0..*directive_count {
                        let Some(directive) = methods.get(position..position + 2) else {
                            return false;
                        };
                        let Some(next) = position.checked_add(2 + directive[1] as usize) else {
                            return false;
                        };
                        if next > methods.len() {
                            return false;
                        }
                        position = next;
                    }
                }
            }
            RirTypeSyntaxNode::AnonymousEnum { variants } => {
                let Some(variants) = self.words(*variants) else {
                    return false;
                };
                let mut position = 0usize;
                while position < variants.len() {
                    let Some(header) = variants.get(position..position + 2) else {
                        return false;
                    };
                    position += 2;
                    let Some(next) = position.checked_add(header[1] as usize) else {
                        return false;
                    };
                    let Some(payloads) = variants.get(position..next) else {
                        return false;
                    };
                    for payload in payloads {
                        visit(RirTypeSyntaxRef::from_u32(*payload));
                    }
                    position = next;
                }
            }
        }
        true
    }
}

impl<S> RirTypeSyntaxArena<S> {
    /// Render the canonical diagnostic spelling through the owner's symbol
    /// authority. Semantic consumers inspect nodes directly; this is only for
    /// human-readable RIR presentation and diagnostics.
    pub fn render_type_with<'a>(
        &'a self,
        reference: RirTypeSyntaxRef,
        resolve: impl Copy + Fn(&'a S) -> &'a str,
    ) -> Option<String> {
        let mut output = String::new();
        self.write_type(reference, &mut output, resolve)
            .then_some(output)
    }

    fn write_type<'a>(
        &'a self,
        reference: RirTypeSyntaxRef,
        output: &mut String,
        resolve: impl Copy + Fn(&'a S) -> &'a str,
    ) -> bool {
        let Some(node) = self.node(reference) else {
            return false;
        };
        match node {
            RirTypeSyntaxNode::Named(symbol) => {
                let Some(symbol) = self.symbol(*symbol) else {
                    return false;
                };
                output.push_str(resolve(symbol));
            }
            RirTypeSyntaxNode::Qualified { path } => {
                if !self.write_path(*path, output, resolve) {
                    return false;
                }
            }
            RirTypeSyntaxNode::Unit => output.push_str("()"),
            RirTypeSyntaxNode::Never => output.push('!'),
            RirTypeSyntaxNode::Array { element, length } => {
                output.push('[');
                if !self.write_type(*element, output, resolve) {
                    return false;
                }
                output.push_str("; ");
                if !self.write_type(*length, output, resolve) {
                    return false;
                }
                output.push(']');
            }
            RirTypeSyntaxNode::Slice { element } => {
                output.push('[');
                if !self.write_type(*element, output, resolve) {
                    return false;
                }
                output.push(']');
            }
            RirTypeSyntaxNode::AnonymousStruct { .. } => output.push_str("struct { ... }"),
            RirTypeSyntaxNode::AnonymousEnum { .. } => output.push_str("enum { ... }"),
            RirTypeSyntaxNode::PointerConst { pointee } => {
                output.push_str("ptr const ");
                if !self.write_type(*pointee, output, resolve) {
                    return false;
                }
            }
            RirTypeSyntaxNode::PointerMut { pointee } => {
                output.push_str("ptr mut ");
                if !self.write_type(*pointee, output, resolve) {
                    return false;
                }
            }
            RirTypeSyntaxNode::TypeCall { path, arguments } => {
                if !self.write_path(*path, output, resolve) {
                    return false;
                }
                output.push('(');
                if !self.write_arguments(*arguments, output, resolve) {
                    return false;
                }
                output.push(')');
            }
            RirTypeSyntaxNode::ValueCall { name, arguments } => {
                let Some(name) = self.symbol(*name) else {
                    return false;
                };
                output.push_str(resolve(name));
                output.push('(');
                if !self.write_arguments(*arguments, output, resolve) {
                    return false;
                }
                output.push(')');
            }
            RirTypeSyntaxNode::Integer(value) => output.push_str(&value.to_string()),
        }
        true
    }

    fn write_path<'a>(
        &'a self,
        range: RirTypeSyntaxRange,
        output: &mut String,
        resolve: impl Copy + Fn(&'a S) -> &'a str,
    ) -> bool {
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
            output.push_str(resolve(symbol));
        }
        true
    }

    fn write_arguments<'a>(
        &'a self,
        range: RirTypeSyntaxRange,
        output: &mut String,
        resolve: impl Copy + Fn(&'a S) -> &'a str,
    ) -> bool {
        let Some(words) = self.words(range) else {
            return false;
        };
        for (index, word) in words.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            if !self.write_type(RirTypeSyntaxRef::from_u32(*word), output, resolve) {
                return false;
            }
        }
        true
    }
}

impl<S: AsRef<str>> RirTypeSyntaxArena<S> {
    pub fn render_type(&self, reference: RirTypeSyntaxRef) -> Option<String> {
        self.render_type_with(reference, AsRef::as_ref)
    }
}

/// Capacity failure while projecting parser-owned type structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RirTypeSyntaxBuildError {
    TooManyNodes,
    TooManySymbols,
    TooMuchPayload,
}

/// A malformed dense type-syntax arena. The reason is intentionally static so
/// query failures stay deterministic and do not retain source-owned strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RirTypeSyntaxValidationError {
    pub node: Option<RirTypeSyntaxRef>,
    pub reason: &'static str,
}

#[derive(Debug)]
pub enum RirTypeSyntaxAppendError<E> {
    Malformed(RirTypeSyntaxValidationError),
    Checkpoint(E),
    Build(RirTypeSyntaxBuildError),
}

impl<E> From<RirTypeSyntaxBuildError> for RirTypeSyntaxAppendError<E> {
    fn from(error: RirTypeSyntaxBuildError) -> Self {
        Self::Build(error)
    }
}

impl<E> From<RirTypeSyntaxValidationError> for RirTypeSyntaxAppendError<E> {
    fn from(error: RirTypeSyntaxValidationError) -> Self {
        Self::Malformed(error)
    }
}

impl<S> RirTypeSyntaxArena<S> {
    /// Validate the complete typed arena, including unused dense symbols.
    /// Child references must be postorder so a declaration fragment is closed
    /// and can be relocated by a single linear pass.
    pub fn validate_with_symbol(
        &self,
        mut symbol_is_valid: impl FnMut(&S) -> bool,
    ) -> Result<(), RirTypeSyntaxValidationError> {
        fn invalid(
            node: Option<RirTypeSyntaxRef>,
            reason: &'static str,
        ) -> RirTypeSyntaxValidationError {
            RirTypeSyntaxValidationError { node, reason }
        }

        if self.symbols.iter().any(|symbol| !symbol_is_valid(symbol)) {
            return Err(invalid(None, "type-syntax symbol is outside its owner"));
        }

        for (owner, node) in self.nodes.iter().enumerate() {
            let owner_ref = RirTypeSyntaxRef::from_u32(owner as u32);
            let check_ref = |reference: RirTypeSyntaxRef| {
                if reference.index() < owner {
                    Ok(())
                } else {
                    Err(invalid(
                        Some(owner_ref),
                        "type-syntax child is not a preceding node",
                    ))
                }
            };
            let check_symbol = |symbol: RirTypeSyntaxSymbol| {
                if symbol.index() < self.symbols.len() {
                    Ok(())
                } else {
                    Err(invalid(
                        Some(owner_ref),
                        "type-syntax symbol index is outside its arena",
                    ))
                }
            };
            let words = |range: RirTypeSyntaxRange| {
                self.words(range).ok_or_else(|| {
                    invalid(
                        Some(owner_ref),
                        "type-syntax payload range is outside its arena",
                    )
                })
            };
            let check_path = |range: RirTypeSyntaxRange| {
                let path = words(range)?;
                if path.is_empty() {
                    return Err(invalid(Some(owner_ref), "type path is empty"));
                }
                for word in path {
                    check_symbol(RirTypeSyntaxSymbol::from_u32(*word))?;
                }
                Ok(())
            };
            let check_arguments = |range: RirTypeSyntaxRange| {
                for word in words(range)? {
                    check_ref(RirTypeSyntaxRef::from_u32(*word))?;
                }
                Ok(())
            };

            match node {
                RirTypeSyntaxNode::Named(symbol) => check_symbol(*symbol)?,
                RirTypeSyntaxNode::Qualified { path } => check_path(*path)?,
                RirTypeSyntaxNode::Unit
                | RirTypeSyntaxNode::Never
                | RirTypeSyntaxNode::Integer(_) => {}
                RirTypeSyntaxNode::Array { element, length } => {
                    check_ref(*element)?;
                    check_ref(*length)?;
                }
                RirTypeSyntaxNode::Slice { element }
                | RirTypeSyntaxNode::PointerConst { pointee: element }
                | RirTypeSyntaxNode::PointerMut { pointee: element } => check_ref(*element)?,
                RirTypeSyntaxNode::AnonymousStruct { fields, methods } => {
                    let fields = words(*fields)?;
                    if !fields.len().is_multiple_of(2) {
                        return Err(invalid(Some(owner_ref), "struct field record is truncated"));
                    }
                    for field in fields.chunks_exact(2) {
                        check_symbol(RirTypeSyntaxSymbol::from_u32(field[0]))?;
                        check_ref(RirTypeSyntaxRef::from_u32(field[1]))?;
                    }

                    let methods = words(*methods)?;
                    let mut position = 0usize;
                    while position < methods.len() {
                        let header = methods
                            .get(position..position.saturating_add(4))
                            .ok_or_else(|| {
                                invalid(Some(owner_ref), "anonymous method header is truncated")
                            })?;
                        check_symbol(RirTypeSyntaxSymbol::from_u32(header[0]))?;
                        if header[1] != u32::MAX && header[1] > 3 {
                            return Err(invalid(
                                Some(owner_ref),
                                "anonymous receiver mode is invalid",
                            ));
                        }
                        if header[2] > 1 {
                            return Err(invalid(
                                Some(owner_ref),
                                "anonymous borrow-return flag is invalid",
                            ));
                        }
                        position += 4;
                        let param_count = header[3] as usize;
                        for _ in 0..param_count {
                            let parameter = methods
                                .get(position..position.saturating_add(3))
                                .ok_or_else(|| {
                                    invalid(
                                        Some(owner_ref),
                                        "anonymous parameter record is truncated",
                                    )
                                })?;
                            if parameter[0] > 3 {
                                return Err(invalid(
                                    Some(owner_ref),
                                    "anonymous parameter mode is invalid",
                                ));
                            }
                            check_symbol(RirTypeSyntaxSymbol::from_u32(parameter[1]))?;
                            check_ref(RirTypeSyntaxRef::from_u32(parameter[2]))?;
                            position += 3;
                        }
                        let result_and_count = methods
                            .get(position..position.saturating_add(2))
                            .ok_or_else(|| {
                                invalid(Some(owner_ref), "anonymous method result is truncated")
                            })?;
                        check_ref(RirTypeSyntaxRef::from_u32(result_and_count[0]))?;
                        position += 2;
                        let directive_count = result_and_count[1] as usize;
                        for _ in 0..directive_count {
                            let directive = methods
                                .get(position..position.saturating_add(2))
                                .ok_or_else(|| {
                                    invalid(Some(owner_ref), "anonymous directive is truncated")
                                })?;
                            check_symbol(RirTypeSyntaxSymbol::from_u32(directive[0]))?;
                            position += 2;
                            let argument_count = directive[1] as usize;
                            let arguments = methods
                                .get(position..position.saturating_add(argument_count))
                                .ok_or_else(|| {
                                    invalid(
                                        Some(owner_ref),
                                        "anonymous directive arguments are truncated",
                                    )
                                })?;
                            for argument in arguments {
                                check_symbol(RirTypeSyntaxSymbol::from_u32(*argument))?;
                            }
                            position += argument_count;
                        }
                    }
                }
                RirTypeSyntaxNode::AnonymousEnum { variants } => {
                    let variants = words(*variants)?;
                    let mut position = 0usize;
                    while position < variants.len() {
                        let header = variants
                            .get(position..position.saturating_add(2))
                            .ok_or_else(|| {
                                invalid(Some(owner_ref), "anonymous enum variant is truncated")
                            })?;
                        check_symbol(RirTypeSyntaxSymbol::from_u32(header[0]))?;
                        position += 2;
                        let payload_count = header[1] as usize;
                        let payload = variants
                            .get(position..position.saturating_add(payload_count))
                            .ok_or_else(|| {
                                invalid(Some(owner_ref), "anonymous enum payload is truncated")
                            })?;
                        for reference in payload {
                            check_ref(RirTypeSyntaxRef::from_u32(*reference))?;
                        }
                        position += payload_count;
                    }
                }
                RirTypeSyntaxNode::TypeCall { path, arguments } => {
                    check_path(*path)?;
                    check_arguments(*arguments)?;
                }
                RirTypeSyntaxNode::ValueCall { name, arguments } => {
                    check_symbol(*name)?;
                    check_arguments(*arguments)?;
                }
            }
        }
        Ok(())
    }
}

/// Mutable producer for one declaration-local syntax arena.
#[derive(Debug)]
pub struct RirTypeSyntaxBuilder<S> {
    nodes: Vec<RirTypeSyntaxNode>,
    extra: Vec<u32>,
    symbols: Vec<S>,
    symbol_indexes: Option<AHashMap<S, RirTypeSyntaxSymbol>>,
}

// Most declaration-local type grammars mention only a handful of distinct
// spellings. Keep those builders allocation-free while bounding the linear
// prefix before promoting larger arenas to the expected-linear hash index.
const LINEAR_SYMBOL_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy)]
pub(crate) struct RirTypeSyntaxBuilderSnapshot {
    nodes: usize,
    extra: usize,
    symbols: usize,
}

impl<S> Default for RirTypeSyntaxBuilder<S> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            extra: Vec::new(),
            symbols: Vec::new(),
            symbol_indexes: None,
        }
    }
}

impl<S: Clone + Eq + Hash> RirTypeSyntaxBuilder<S> {
    pub(crate) fn snapshot(&self) -> RirTypeSyntaxBuilderSnapshot {
        RirTypeSyntaxBuilderSnapshot {
            nodes: self.nodes.len(),
            extra: self.extra.len(),
            symbols: self.symbols.len(),
        }
    }

    pub(crate) fn rollback(&mut self, snapshot: RirTypeSyntaxBuilderSnapshot) {
        self.nodes.truncate(snapshot.nodes);
        self.extra.truncate(snapshot.extra);
        self.symbols.truncate(snapshot.symbols);
        if let Some(symbol_indexes) = &mut self.symbol_indexes {
            symbol_indexes.retain(|_, index| index.index() < snapshot.symbols);
        }
    }

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

    pub fn push_never_type(&mut self) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        self.push_node(RirTypeSyntaxNode::Never)
    }

    pub fn push_named_type(
        &mut self,
        symbol: S,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        let symbol = self.symbol(symbol)?;
        self.push_node(RirTypeSyntaxNode::Named(symbol))
    }

    pub fn push_integer(
        &mut self,
        value: i128,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        self.push_node(RirTypeSyntaxNode::Integer(value))
    }

    pub fn push_array_type(
        &mut self,
        element: RirTypeSyntaxRef,
        length: RirTypeSyntaxRef,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        self.push_node(RirTypeSyntaxNode::Array { element, length })
    }

    pub fn push_slice_type(
        &mut self,
        element: RirTypeSyntaxRef,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        self.push_node(RirTypeSyntaxNode::Slice { element })
    }

    pub fn push_pointer_const_type(
        &mut self,
        pointee: RirTypeSyntaxRef,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        self.push_node(RirTypeSyntaxNode::PointerConst { pointee })
    }

    pub fn push_pointer_mut_type(
        &mut self,
        pointee: RirTypeSyntaxRef,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        self.push_node(RirTypeSyntaxNode::PointerMut { pointee })
    }

    /// Append one validated declaration-local arena, remapping its dense symbol
    /// domain and every child reference exactly once. The append is atomic;
    /// cancellation or malformed input restores the complete destination
    /// builder, including its symbol interning table.
    pub fn append_remapped<T, E>(
        &mut self,
        source: &RirTypeSyntaxArena<T>,
        mut remap_symbol: impl FnMut(&T) -> S,
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<Vec<RirTypeSyntaxRef>, RirTypeSyntaxAppendError<E>> {
        source
            .validate_with_symbol(|_| true)
            .map_err(RirTypeSyntaxAppendError::Malformed)?;

        let node_len = self.nodes.len();
        let extra_len = self.extra.len();
        let symbol_len = self.symbols.len();
        let result = (|| {
            let mut symbols = Vec::new();
            symbols
                .try_reserve_exact(source.symbols.len())
                .map_err(|_| RirTypeSyntaxBuildError::TooManySymbols)?;
            for source_symbol in source.symbols.iter() {
                checkpoint().map_err(RirTypeSyntaxAppendError::Checkpoint)?;
                symbols.push(self.symbol(remap_symbol(source_symbol))?);
            }

            let mut nodes: Vec<RirTypeSyntaxRef> = Vec::new();
            nodes
                .try_reserve_exact(source.nodes.len())
                .map_err(|_| RirTypeSyntaxBuildError::TooManyNodes)?;
            for source_node in source.nodes.iter() {
                checkpoint().map_err(RirTypeSyntaxAppendError::Checkpoint)?;
                let map_symbol = |symbol: RirTypeSyntaxSymbol| {
                    symbols
                        .get(symbol.index())
                        .copied()
                        .ok_or(RirTypeSyntaxValidationError {
                            node: None,
                            reason: "type-syntax symbol index is outside its arena",
                        })
                };
                let map_ref = |reference: RirTypeSyntaxRef| {
                    nodes
                        .get(reference.index())
                        .copied()
                        .ok_or(RirTypeSyntaxValidationError {
                            node: None,
                            reason: "type-syntax child is not a preceding node",
                        })
                };
                let map_path = |this: &mut Self, range: RirTypeSyntaxRange| {
                    let words = source.words(range).ok_or(RirTypeSyntaxValidationError {
                        node: None,
                        reason: "type-syntax payload range is outside its arena",
                    })?;
                    let mut mapped = Vec::new();
                    mapped
                        .try_reserve_exact(words.len())
                        .map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)?;
                    for word in words {
                        mapped.push(map_symbol(RirTypeSyntaxSymbol::from_u32(*word))?.as_u32());
                    }
                    Ok::<_, RirTypeSyntaxAppendError<E>>(this.push_words(mapped)?)
                };
                let map_refs = |this: &mut Self, range: RirTypeSyntaxRange| {
                    let words = source.words(range).ok_or(RirTypeSyntaxValidationError {
                        node: None,
                        reason: "type-syntax payload range is outside its arena",
                    })?;
                    let mut mapped = Vec::new();
                    mapped
                        .try_reserve_exact(words.len())
                        .map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)?;
                    for word in words {
                        mapped.push(map_ref(RirTypeSyntaxRef::from_u32(*word))?.as_u32());
                    }
                    Ok::<_, RirTypeSyntaxAppendError<E>>(this.push_words(mapped)?)
                };

                let node = match source_node {
                    RirTypeSyntaxNode::Named(symbol) => RirTypeSyntaxNode::Named(
                        map_symbol(*symbol).map_err(RirTypeSyntaxAppendError::Malformed)?,
                    ),
                    RirTypeSyntaxNode::Qualified { path } => RirTypeSyntaxNode::Qualified {
                        path: map_path(self, *path)?,
                    },
                    RirTypeSyntaxNode::Unit => RirTypeSyntaxNode::Unit,
                    RirTypeSyntaxNode::Never => RirTypeSyntaxNode::Never,
                    RirTypeSyntaxNode::Array { element, length } => RirTypeSyntaxNode::Array {
                        element: map_ref(*element).map_err(RirTypeSyntaxAppendError::Malformed)?,
                        length: map_ref(*length).map_err(RirTypeSyntaxAppendError::Malformed)?,
                    },
                    RirTypeSyntaxNode::Slice { element } => RirTypeSyntaxNode::Slice {
                        element: map_ref(*element).map_err(RirTypeSyntaxAppendError::Malformed)?,
                    },
                    RirTypeSyntaxNode::PointerConst { pointee } => {
                        RirTypeSyntaxNode::PointerConst {
                            pointee: map_ref(*pointee)
                                .map_err(RirTypeSyntaxAppendError::Malformed)?,
                        }
                    }
                    RirTypeSyntaxNode::PointerMut { pointee } => RirTypeSyntaxNode::PointerMut {
                        pointee: map_ref(*pointee).map_err(RirTypeSyntaxAppendError::Malformed)?,
                    },
                    RirTypeSyntaxNode::TypeCall { path, arguments } => {
                        RirTypeSyntaxNode::TypeCall {
                            path: map_path(self, *path)?,
                            arguments: map_refs(self, *arguments)?,
                        }
                    }
                    RirTypeSyntaxNode::ValueCall { name, arguments } => {
                        RirTypeSyntaxNode::ValueCall {
                            name: map_symbol(*name).map_err(RirTypeSyntaxAppendError::Malformed)?,
                            arguments: map_refs(self, *arguments)?,
                        }
                    }
                    RirTypeSyntaxNode::Integer(value) => RirTypeSyntaxNode::Integer(*value),
                    RirTypeSyntaxNode::AnonymousStruct { fields, methods } => {
                        let field_words =
                            source.words(*fields).ok_or(RirTypeSyntaxValidationError {
                                node: None,
                                reason: "type-syntax payload range is outside its arena",
                            })?;
                        let mut mapped_fields = Vec::new();
                        mapped_fields
                            .try_reserve_exact(field_words.len())
                            .map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)?;
                        for field in field_words.chunks_exact(2) {
                            checkpoint().map_err(RirTypeSyntaxAppendError::Checkpoint)?;
                            mapped_fields.extend([
                                map_symbol(RirTypeSyntaxSymbol::from_u32(field[0]))
                                    .map_err(RirTypeSyntaxAppendError::Malformed)?
                                    .as_u32(),
                                map_ref(RirTypeSyntaxRef::from_u32(field[1]))
                                    .map_err(RirTypeSyntaxAppendError::Malformed)?
                                    .as_u32(),
                            ]);
                        }
                        let fields = self.push_words(mapped_fields)?;

                        let method_words =
                            source.words(*methods).ok_or(RirTypeSyntaxValidationError {
                                node: None,
                                reason: "type-syntax payload range is outside its arena",
                            })?;
                        let mut mapped_methods = Vec::new();
                        mapped_methods
                            .try_reserve_exact(method_words.len())
                            .map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)?;
                        let mut position = 0usize;
                        while position < method_words.len() {
                            checkpoint().map_err(RirTypeSyntaxAppendError::Checkpoint)?;
                            let header = &method_words[position..position + 4];
                            mapped_methods.extend([
                                map_symbol(RirTypeSyntaxSymbol::from_u32(header[0]))
                                    .map_err(RirTypeSyntaxAppendError::Malformed)?
                                    .as_u32(),
                                header[1],
                                header[2],
                                header[3],
                            ]);
                            position += 4;
                            for _ in 0..header[3] {
                                let parameter = &method_words[position..position + 3];
                                mapped_methods.extend([
                                    parameter[0],
                                    map_symbol(RirTypeSyntaxSymbol::from_u32(parameter[1]))
                                        .map_err(RirTypeSyntaxAppendError::Malformed)?
                                        .as_u32(),
                                    map_ref(RirTypeSyntaxRef::from_u32(parameter[2]))
                                        .map_err(RirTypeSyntaxAppendError::Malformed)?
                                        .as_u32(),
                                ]);
                                position += 3;
                            }
                            mapped_methods.push(
                                map_ref(RirTypeSyntaxRef::from_u32(method_words[position]))
                                    .map_err(RirTypeSyntaxAppendError::Malformed)?
                                    .as_u32(),
                            );
                            let directive_count = method_words[position + 1];
                            mapped_methods.push(directive_count);
                            position += 2;
                            for _ in 0..directive_count {
                                let directive_name = method_words[position];
                                let argument_count = method_words[position + 1];
                                mapped_methods.push(
                                    map_symbol(RirTypeSyntaxSymbol::from_u32(directive_name))
                                        .map_err(RirTypeSyntaxAppendError::Malformed)?
                                        .as_u32(),
                                );
                                mapped_methods.push(argument_count);
                                position += 2;
                                for _ in 0..argument_count {
                                    mapped_methods.push(
                                        map_symbol(RirTypeSyntaxSymbol::from_u32(
                                            method_words[position],
                                        ))
                                        .map_err(RirTypeSyntaxAppendError::Malformed)?
                                        .as_u32(),
                                    );
                                    position += 1;
                                }
                            }
                        }
                        let methods = self.push_words(mapped_methods)?;
                        RirTypeSyntaxNode::AnonymousStruct { fields, methods }
                    }
                    RirTypeSyntaxNode::AnonymousEnum { variants } => {
                        let variant_words =
                            source
                                .words(*variants)
                                .ok_or(RirTypeSyntaxValidationError {
                                    node: None,
                                    reason: "type-syntax payload range is outside its arena",
                                })?;
                        let mut mapped = Vec::new();
                        mapped
                            .try_reserve_exact(variant_words.len())
                            .map_err(|_| RirTypeSyntaxBuildError::TooMuchPayload)?;
                        let mut position = 0usize;
                        while position < variant_words.len() {
                            checkpoint().map_err(RirTypeSyntaxAppendError::Checkpoint)?;
                            mapped.push(
                                map_symbol(RirTypeSyntaxSymbol::from_u32(variant_words[position]))
                                    .map_err(RirTypeSyntaxAppendError::Malformed)?
                                    .as_u32(),
                            );
                            let payload_count = variant_words[position + 1];
                            mapped.push(payload_count);
                            position += 2;
                            for _ in 0..payload_count {
                                mapped.push(
                                    map_ref(RirTypeSyntaxRef::from_u32(variant_words[position]))
                                        .map_err(RirTypeSyntaxAppendError::Malformed)?
                                        .as_u32(),
                                );
                                position += 1;
                            }
                        }
                        RirTypeSyntaxNode::AnonymousEnum {
                            variants: self.push_words(mapped)?,
                        }
                    }
                };
                nodes.push(self.push_node(node)?);
            }
            Ok(nodes)
        })();

        if result.is_err() {
            self.nodes.truncate(node_len);
            self.extra.truncate(extra_len);
            self.symbols.truncate(symbol_len);
            if let Some(symbol_indexes) = &mut self.symbol_indexes {
                symbol_indexes.retain(|_, index| index.index() < symbol_len);
            }
        }
        result
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
        if let Some(symbol_indexes) = &self.symbol_indexes {
            if let Some(index) = symbol_indexes.get(&symbol) {
                return Ok(*index);
            }
        } else if let Some(index) = self.symbols.iter().position(|existing| existing == &symbol) {
            return Ok(RirTypeSyntaxSymbol(index as u32));
        }
        let index = u32::try_from(self.symbols.len())
            .map_err(|_| RirTypeSyntaxBuildError::TooManySymbols)?;
        let index = RirTypeSyntaxSymbol(index);
        self.symbols.push(symbol.clone());
        if let Some(symbol_indexes) = &mut self.symbol_indexes {
            symbol_indexes.insert(symbol, index);
        } else if self.symbols.len() > LINEAR_SYMBOL_LIMIT {
            let mut symbol_indexes = AHashMap::with_capacity(self.symbols.len());
            for (ordinal, symbol) in self.symbols.iter().cloned().enumerate() {
                symbol_indexes.insert(symbol, RirTypeSyntaxSymbol(ordinal as u32));
            }
            self.symbol_indexes = Some(symbol_indexes);
        }
        Ok(index)
    }

    pub(crate) fn push_node(
        &mut self,
        node: RirTypeSyntaxNode,
    ) -> Result<RirTypeSyntaxRef, RirTypeSyntaxBuildError> {
        let index =
            u32::try_from(self.nodes.len()).map_err(|_| RirTypeSyntaxBuildError::TooManyNodes)?;
        self.nodes.push(node);
        Ok(RirTypeSyntaxRef(index))
    }

    pub(crate) fn push_words(
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
