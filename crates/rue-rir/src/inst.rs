//! RIR instruction definitions.
//!
//! Instructions are stored in a dense array and referenced by index.
//! This provides good cache locality and efficient traversal.

use std::fmt;

use lasso::{Key, Spur};
use rue_span::Span;

/// A reference to an instruction in the RIR.
///
/// This is a lightweight handle (4 bytes) that indexes into the instruction array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstRef(u32);

impl InstRef {
    /// Create an instruction reference from a raw index.
    #[inline]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Get the raw index.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// A directive in the RIR (e.g., @allow(unused_variable))
#[derive(Debug, Clone)]
pub struct RirDirective {
    /// Directive name (e.g., "allow")
    pub name: Spur,
    /// Arguments (e.g., ["unused_variable"])
    pub args: Vec<Spur>,
    /// Span covering the directive
    pub span: Span,
}

/// Parameter passing mode in RIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RirParamMode {
    /// Normal pass-by-value parameter
    #[default]
    Normal,
    /// Inout parameter - mutated in place and returned to caller
    Inout,
    /// Borrow parameter - immutable borrow without ownership transfer
    Borrow,
    /// Comptime parameter - evaluated at compile time (used for type parameters)
    Comptime,
}

/// A parameter in a function declaration.
#[derive(Debug, Clone)]
pub struct RirParam {
    /// Parameter name
    pub name: Spur,
    /// Parameter type
    pub ty: Spur,
    /// Parameter passing mode
    pub mode: RirParamMode,
    /// Whether this parameter is evaluated at compile time
    pub is_comptime: bool,
}

/// Argument passing mode in RIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RirArgMode {
    /// Normal pass-by-value argument
    #[default]
    Normal,
    /// Inout argument - mutated in place
    Inout,
    /// Borrow argument - immutable borrow
    Borrow,
}

/// An argument in a function call.
#[derive(Debug, Clone)]
pub struct RirCallArg {
    /// The argument expression
    pub value: InstRef,
    /// The passing mode for this argument
    pub mode: RirArgMode,
}

impl RirCallArg {
    /// Returns true if this argument is passed as inout.
    /// This is a convenience method for backwards compatibility.
    pub fn is_inout(&self) -> bool {
        self.mode == RirArgMode::Inout
    }

    /// Returns true if this argument is passed as borrow.
    pub fn is_borrow(&self) -> bool {
        self.mode == RirArgMode::Borrow
    }
}

/// A pattern in a match expression (RIR level - untyped).
#[derive(Debug, Clone)]
pub enum RirPattern {
    /// Wildcard pattern `_` - matches anything
    Wildcard(Span),
    /// Integer literal pattern (can be positive or negative)
    Int(i64, Span),
    /// Boolean literal pattern
    Bool(bool, Span),
    /// Path pattern for enum variants (e.g., `Color::Red` or `module.Color::Red`)
    Path {
        /// Optional module reference for qualified paths (e.g., the `module` in `module.Color::Red`)
        module: Option<InstRef>,
        /// The enum type name
        type_name: Spur,
        /// The variant name
        variant: Spur,
        /// Span of the pattern
        span: Span,
    },
}

impl RirPattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            RirPattern::Wildcard(span) => *span,
            RirPattern::Int(_, span) => *span,
            RirPattern::Bool(_, span) => *span,
            RirPattern::Path { span, .. } => *span,
        }
    }
}

/// Extra data marker types for type-safe storage in the extra array.
/// These types represent data stored in the extra array.

/// Stored representation of RirCallArg in the extra array.
/// Layout: [value: u32, mode: u32] = 2 u32s per arg
const CALL_ARG_SIZE: u32 = 2;

/// Stored representation of RirParam in the extra array.
/// Layout: [name: u32, ty: u32, mode: u32, is_comptime: u32] = 4 u32s per param
const PARAM_SIZE: u32 = 4;

/// Stored representation of match arm in the extra array.
/// Layout: pattern data + [body: u32]
/// Pattern data varies by kind (see PatternKind enum).

/// Pattern kinds encoded in extra array
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    /// Wildcard pattern: [kind, span_start, span_len]
    Wildcard = 0,
    /// Int pattern: [kind, span_start, span_len, value_lo, value_hi]
    Int = 1,
    /// Bool pattern: [kind, span_start, span_len, value]
    Bool = 2,
    /// Path pattern: [kind, span_start, span_len, module, type_name, variant]
    /// module is u32::MAX for None, otherwise an InstRef
    Path = 3,
}

/// Size of each pattern kind in the extra array (including body InstRef)
const PATTERN_WILDCARD_SIZE: u32 = 4; // kind, span_start, span_len, body
const PATTERN_INT_SIZE: u32 = 6; // kind, span_start, span_len, value_lo, value_hi, body
const PATTERN_BOOL_SIZE: u32 = 5; // kind, span_start, span_len, value, body
const PATTERN_PATH_SIZE: u32 = 7; // kind, span_start, span_len, module, type_name, variant, body

/// Stored representation of struct field initializer.
/// Layout: [field_name: u32, value: u32] = 2 u32s per field
const FIELD_INIT_SIZE: u32 = 2;

/// Stored representation of struct field declaration.
/// Layout: [field_name: u32, field_type: u32] = 2 u32s per field
const FIELD_DECL_SIZE: u32 = 2;

/// Stored representation of directive in the extra array.
/// Layout: [name: u32, span_start: u32, span_len: u32, args_len: u32, args...]
/// Variable size due to args.

/// A span marking the boundaries of a function in the RIR.
///
/// This allows efficient per-function analysis by identifying which instructions
/// belong to each function without scanning the entire instruction array.
#[derive(Debug, Clone)]
pub struct FunctionSpan {
    /// Function name symbol
    pub name: Spur,
    /// Index of the first instruction of this function's body.
    /// This is the first instruction generated for the function's expressions/statements.
    pub body_start: InstRef,
    /// Index of the FnDecl instruction for this function.
    /// This is always the last instruction of the function.
    pub decl: InstRef,
}

impl FunctionSpan {
    /// Create a new function span.
    pub fn new(name: Spur, body_start: InstRef, decl: InstRef) -> Self {
        Self {
            name,
            body_start,
            decl,
        }
    }

    /// Get the number of instructions in this function (including the FnDecl).
    pub fn instruction_count(&self) -> u32 {
        self.decl.as_u32() - self.body_start.as_u32() + 1
    }
}

/// A view into a function's instructions within the RIR.
///
/// This provides a way to iterate over just the instructions belonging to a
/// specific function, enabling per-function analysis without copying data.
#[derive(Debug)]
pub struct RirFunctionView<'a> {
    rir: &'a Rir,
    body_start: InstRef,
    decl: InstRef,
}

impl<'a> RirFunctionView<'a> {
    /// Get the instruction at the given reference.
    ///
    /// Note: The reference must be within this function's range.
    #[inline]
    pub fn get(&self, inst_ref: InstRef) -> &'a Inst {
        debug_assert!(
            inst_ref.as_u32() >= self.body_start.as_u32()
                && inst_ref.as_u32() <= self.decl.as_u32(),
            "InstRef {} is outside function range [{}, {}]",
            inst_ref,
            self.body_start,
            self.decl
        );
        self.rir.get(inst_ref)
    }

    /// Get the FnDecl instruction for this function.
    #[inline]
    pub fn fn_decl(&self) -> &'a Inst {
        self.rir.get(self.decl)
    }

    /// Iterate over all instructions in this function (including FnDecl).
    pub fn iter(&self) -> impl Iterator<Item = (InstRef, &'a Inst)> {
        let start = self.body_start.as_u32();
        let end = self.decl.as_u32() + 1;
        (start..end).map(move |i| {
            let inst_ref = InstRef::from_raw(i);
            (inst_ref, self.rir.get(inst_ref))
        })
    }

    /// Get the number of instructions in this function view.
    pub fn len(&self) -> usize {
        (self.decl.as_u32() - self.body_start.as_u32() + 1) as usize
    }

    /// Whether this view is empty (should never be true for valid functions).
    pub fn is_empty(&self) -> bool {
        self.body_start.as_u32() > self.decl.as_u32()
    }

    /// Access the underlying RIR for operations that need the full context
    /// (e.g., accessing extra data).
    pub fn rir(&self) -> &'a Rir {
        self.rir
    }
}

/// The complete RIR for a source file.
#[derive(Debug, Default)]
pub struct Rir {
    /// All instructions in the file
    instructions: Vec<Inst>,
    /// Extra data for variable-length instruction payloads
    extra: Vec<u32>,
    /// Function boundaries for per-function analysis
    function_spans: Vec<FunctionSpan>,
}

impl Rir {
    /// Create a new empty RIR.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an instruction and return its reference.
    pub fn add_inst(&mut self, inst: Inst) -> InstRef {
        // Debug assertion for u32 overflow - catches pathological inputs during development
        debug_assert!(
            self.instructions.len() < u32::MAX as usize,
            "RIR instruction count overflow: {} instructions exceeds u32::MAX - 1",
            self.instructions.len()
        );

        let index = self.instructions.len() as u32;
        self.instructions.push(inst);
        InstRef::from_raw(index)
    }

    /// Get an instruction by reference.
    #[inline]
    pub fn get(&self, inst_ref: InstRef) -> &Inst {
        &self.instructions[inst_ref.0 as usize]
    }

    /// Get a mutable reference to an instruction.
    #[inline]
    pub fn get_mut(&mut self, inst_ref: InstRef) -> &mut Inst {
        &mut self.instructions[inst_ref.0 as usize]
    }

    /// The number of instructions.
    #[inline]
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// Whether there are no instructions.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// Iterate over all instructions with their references.
    pub fn iter(&self) -> impl Iterator<Item = (InstRef, &Inst)> {
        self.instructions
            .iter()
            .enumerate()
            .map(|(i, inst)| (InstRef::from_raw(i as u32), inst))
    }

    /// Add extra data and return the start index.
    pub fn add_extra(&mut self, data: &[u32]) -> u32 {
        // Debug assertions for u32 overflow - catches pathological inputs during development
        debug_assert!(
            self.extra.len() <= u32::MAX as usize,
            "RIR extra data overflow: {} entries exceeds u32::MAX",
            self.extra.len()
        );
        debug_assert!(
            self.extra.len().saturating_add(data.len()) <= u32::MAX as usize,
            "RIR extra data would overflow: {} + {} exceeds u32::MAX",
            self.extra.len(),
            data.len()
        );

        let start = self.extra.len() as u32;
        self.extra.extend_from_slice(data);
        start
    }

    /// Get extra data by index.
    #[inline]
    pub fn get_extra(&self, start: u32, len: u32) -> &[u32] {
        let start = start as usize;
        let end = start + len as usize;
        &self.extra[start..end]
    }

    // ===== Helper methods for storing/retrieving typed data in the extra array =====

    /// Store a slice of InstRefs and return (start, len).
    pub fn add_inst_refs(&mut self, refs: &[InstRef]) -> (u32, u32) {
        let data: Vec<u32> = refs.iter().map(|r| r.as_u32()).collect();
        let start = self.add_extra(&data);
        (start, refs.len() as u32)
    }

    /// Retrieve InstRefs from the extra array.
    pub fn get_inst_refs(&self, start: u32, len: u32) -> Vec<InstRef> {
        self.get_extra(start, len)
            .iter()
            .map(|&v| InstRef::from_raw(v))
            .collect()
    }

    /// Store a slice of Spurs and return (start, len).
    pub fn add_symbols(&mut self, symbols: &[Spur]) -> (u32, u32) {
        let data: Vec<u32> = symbols.iter().map(|s| s.into_usize() as u32).collect();
        let start = self.add_extra(&data);
        (start, symbols.len() as u32)
    }

    /// Retrieve Spurs from the extra array.
    pub fn get_symbols(&self, start: u32, len: u32) -> Vec<Spur> {
        self.get_extra(start, len)
            .iter()
            .map(|&v| Spur::try_from_usize(v as usize).unwrap())
            .collect()
    }

    /// Store RirCallArgs and return (start, len).
    /// Layout: [value: u32, mode: u32] per arg
    pub fn add_call_args(&mut self, args: &[RirCallArg]) -> (u32, u32) {
        let mut data = Vec::with_capacity(args.len() * CALL_ARG_SIZE as usize);
        for arg in args {
            data.push(arg.value.as_u32());
            data.push(arg.mode as u32);
        }
        let start = self.add_extra(&data);
        (start, args.len() as u32)
    }

    /// Retrieve RirCallArgs from the extra array.
    pub fn get_call_args(&self, start: u32, len: u32) -> Vec<RirCallArg> {
        let data = self.get_extra(start, len * CALL_ARG_SIZE);
        let mut args = Vec::with_capacity(len as usize);
        for chunk in data.chunks(CALL_ARG_SIZE as usize) {
            let value = InstRef::from_raw(chunk[0]);
            let mode = match chunk[1] {
                0 => RirArgMode::Normal,
                1 => RirArgMode::Inout,
                2 => RirArgMode::Borrow,
                _ => RirArgMode::Normal, // Fallback, shouldn't happen
            };
            args.push(RirCallArg { value, mode });
        }
        args
    }

    /// Store RirParams and return (start, len).
    /// Layout: [name: u32, ty: u32, mode: u32, is_comptime: u32] per param
    pub fn add_params(&mut self, params: &[RirParam]) -> (u32, u32) {
        let mut data = Vec::with_capacity(params.len() * PARAM_SIZE as usize);
        for param in params {
            data.push(param.name.into_usize() as u32);
            data.push(param.ty.into_usize() as u32);
            data.push(param.mode as u32);
            data.push(param.is_comptime as u32);
        }
        let start = self.add_extra(&data);
        (start, params.len() as u32)
    }

    /// Retrieve RirParams from the extra array.
    pub fn get_params(&self, start: u32, len: u32) -> Vec<RirParam> {
        let data = self.get_extra(start, len * PARAM_SIZE);
        let mut params = Vec::with_capacity(len as usize);
        for chunk in data.chunks(PARAM_SIZE as usize) {
            let name = Spur::try_from_usize(chunk[0] as usize).unwrap();
            let ty = Spur::try_from_usize(chunk[1] as usize).unwrap();
            let mode = match chunk[2] {
                0 => RirParamMode::Normal,
                1 => RirParamMode::Inout,
                2 => RirParamMode::Borrow,
                3 => RirParamMode::Comptime,
                _ => RirParamMode::Normal, // Fallback
            };
            let is_comptime = chunk[3] != 0;
            params.push(RirParam {
                name,
                ty,
                mode,
                is_comptime,
            });
        }
        params
    }

    /// Store match arms (pattern + body pairs) and return (start, arm_count).
    /// Each arm is stored with variable size depending on pattern kind.
    pub fn add_match_arms(&mut self, arms: &[(RirPattern, InstRef)]) -> (u32, u32) {
        let start = self.extra.len() as u32;
        for (pattern, body) in arms {
            match pattern {
                RirPattern::Wildcard(span) => {
                    self.extra.push(PatternKind::Wildcard as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(body.as_u32());
                }
                RirPattern::Int(value, span) => {
                    self.extra.push(PatternKind::Int as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    // Store i64 as two u32s (little-endian)
                    self.extra.push(*value as u32);
                    self.extra.push((*value >> 32) as u32);
                    self.extra.push(body.as_u32());
                }
                RirPattern::Bool(value, span) => {
                    self.extra.push(PatternKind::Bool as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(if *value { 1 } else { 0 });
                    self.extra.push(body.as_u32());
                }
                RirPattern::Path {
                    module,
                    type_name,
                    variant,
                    span,
                } => {
                    self.extra.push(PatternKind::Path as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    // Store module as u32::MAX for None, otherwise the InstRef
                    self.extra.push(module.map_or(u32::MAX, |r| r.as_u32()));
                    self.extra.push(type_name.into_usize() as u32);
                    self.extra.push(variant.into_usize() as u32);
                    self.extra.push(body.as_u32());
                }
            }
        }
        (start, arms.len() as u32)
    }

    /// Retrieve match arms from the extra array.
    pub fn get_match_arms(&self, start: u32, arm_count: u32) -> Vec<(RirPattern, InstRef)> {
        let mut arms = Vec::with_capacity(arm_count as usize);
        let mut pos = start as usize;

        for _ in 0..arm_count {
            let kind = self.extra[pos];
            match kind {
                k if k == PatternKind::Wildcard as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let body = InstRef::from_raw(self.extra[pos + 3]);
                    arms.push((RirPattern::Wildcard(span), body));
                    pos += PATTERN_WILDCARD_SIZE as usize;
                }
                k if k == PatternKind::Int as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let value_lo = self.extra[pos + 3] as i64;
                    let value_hi = self.extra[pos + 4] as i64;
                    let value = value_lo | (value_hi << 32);
                    let body = InstRef::from_raw(self.extra[pos + 5]);
                    arms.push((RirPattern::Int(value, span), body));
                    pos += PATTERN_INT_SIZE as usize;
                }
                k if k == PatternKind::Bool as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let value = self.extra[pos + 3] != 0;
                    let body = InstRef::from_raw(self.extra[pos + 4]);
                    arms.push((RirPattern::Bool(value, span), body));
                    pos += PATTERN_BOOL_SIZE as usize;
                }
                k if k == PatternKind::Path as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    // Decode module: u32::MAX means None
                    let module_raw = self.extra[pos + 3];
                    let module = if module_raw == u32::MAX {
                        None
                    } else {
                        Some(InstRef::from_raw(module_raw))
                    };
                    let type_name = Spur::try_from_usize(self.extra[pos + 4] as usize).unwrap();
                    let variant = Spur::try_from_usize(self.extra[pos + 5] as usize).unwrap();
                    let body = InstRef::from_raw(self.extra[pos + 6]);
                    arms.push((
                        RirPattern::Path {
                            module,
                            type_name,
                            variant,
                            span,
                        },
                        body,
                    ));
                    pos += PATTERN_PATH_SIZE as usize;
                }
                _ => panic!("Unknown pattern kind: {}", kind),
            }
        }
        arms
    }

    /// Store field initializers (name, value) and return (start, len).
    /// Layout: [name: u32, value: u32] per field
    pub fn add_field_inits(&mut self, fields: &[(Spur, InstRef)]) -> (u32, u32) {
        let mut data = Vec::with_capacity(fields.len() * FIELD_INIT_SIZE as usize);
        for (name, value) in fields {
            data.push(name.into_usize() as u32);
            data.push(value.as_u32());
        }
        let start = self.add_extra(&data);
        (start, fields.len() as u32)
    }

    /// Retrieve field initializers from the extra array.
    pub fn get_field_inits(&self, start: u32, len: u32) -> Vec<(Spur, InstRef)> {
        let data = self.get_extra(start, len * FIELD_INIT_SIZE);
        let mut fields = Vec::with_capacity(len as usize);
        for chunk in data.chunks(FIELD_INIT_SIZE as usize) {
            let name = Spur::try_from_usize(chunk[0] as usize).unwrap();
            let value = InstRef::from_raw(chunk[1]);
            fields.push((name, value));
        }
        fields
    }

    /// Store field declarations (name, type) and return (start, len).
    /// Layout: [name: u32, type: u32] per field
    pub fn add_field_decls(&mut self, fields: &[(Spur, Spur)]) -> (u32, u32) {
        let mut data = Vec::with_capacity(fields.len() * FIELD_DECL_SIZE as usize);
        for (name, ty) in fields {
            data.push(name.into_usize() as u32);
            data.push(ty.into_usize() as u32);
        }
        let start = self.add_extra(&data);
        (start, fields.len() as u32)
    }

    /// Retrieve field declarations from the extra array.
    pub fn get_field_decls(&self, start: u32, len: u32) -> Vec<(Spur, Spur)> {
        let data = self.get_extra(start, len * FIELD_DECL_SIZE);
        let mut fields = Vec::with_capacity(len as usize);
        for chunk in data.chunks(FIELD_DECL_SIZE as usize) {
            let name = Spur::try_from_usize(chunk[0] as usize).unwrap();
            let ty = Spur::try_from_usize(chunk[1] as usize).unwrap();
            fields.push((name, ty));
        }
        fields
    }

    /// Store directives and return (start, directive_count).
    /// Layout: [name: u32, span_start: u32, span_len: u32, args_len: u32, args...] per directive
    pub fn add_directives(&mut self, directives: &[RirDirective]) -> (u32, u32) {
        let start = self.extra.len() as u32;
        for directive in directives {
            self.extra.push(directive.name.into_usize() as u32);
            self.extra.push(directive.span.start());
            self.extra.push(directive.span.len());
            self.extra.push(directive.args.len() as u32);
            for arg in &directive.args {
                self.extra.push(arg.into_usize() as u32);
            }
        }
        (start, directives.len() as u32)
    }

    /// Retrieve directives from the extra array.
    pub fn get_directives(&self, start: u32, directive_count: u32) -> Vec<RirDirective> {
        let mut directives = Vec::with_capacity(directive_count as usize);
        let mut pos = start as usize;

        for _ in 0..directive_count {
            let name = Spur::try_from_usize(self.extra[pos] as usize).unwrap();
            let span = Span::new(self.extra[pos + 1], self.extra[pos + 2]);
            let args_len = self.extra[pos + 3] as usize;
            pos += 4;

            let args: Vec<Spur> = (0..args_len)
                .map(|i| Spur::try_from_usize(self.extra[pos + i] as usize).unwrap())
                .collect();
            pos += args_len;

            directives.push(RirDirective { name, args, span });
        }
        directives
    }

    // ===== Function span methods =====

    /// Add a function span to track function boundaries.
    pub fn add_function_span(&mut self, span: FunctionSpan) {
        self.function_spans.push(span);
    }

    /// Get all function spans.
    pub fn function_spans(&self) -> &[FunctionSpan] {
        &self.function_spans
    }

    /// Iterate over function spans.
    pub fn functions(&self) -> impl Iterator<Item = &FunctionSpan> {
        self.function_spans.iter()
    }

    /// Get the number of functions.
    pub fn function_count(&self) -> usize {
        self.function_spans.len()
    }

    /// Get a view of just one function's instructions.
    pub fn function_view(&self, fn_span: &FunctionSpan) -> RirFunctionView<'_> {
        RirFunctionView {
            rir: self,
            body_start: fn_span.body_start,
            decl: fn_span.decl,
        }
    }

    /// Find a function span by name.
    pub fn find_function(&self, name: Spur) -> Option<&FunctionSpan> {
        self.function_spans.iter().find(|span| span.name == name)
    }

    /// Get the current instruction count (useful for tracking body start).
    pub fn current_inst_index(&self) -> u32 {
        self.instructions.len() as u32
    }

    /// Merge multiple RIRs into a single RIR.
    ///
    /// This is used for parallel per-file RIR generation. Each file generates
    /// its own RIR in parallel, then they are merged into a single RIR with
    /// all instruction references renumbered to be valid in the merged RIR.
    ///
    /// # Arguments
    ///
    /// * `rirs` - Slice of RIRs to merge (typically one per source file)
    ///
    /// # Returns
    ///
    /// A new merged RIR containing all instructions from all input RIRs.
    pub fn merge(rirs: &[Rir]) -> Rir {
        if rirs.is_empty() {
            return Rir::new();
        }

        if rirs.len() == 1 {
            // Clone the single RIR directly
            return Rir {
                instructions: rirs[0].instructions.clone(),
                extra: rirs[0].extra.clone(),
                function_spans: rirs[0].function_spans.clone(),
            };
        }

        // Calculate total sizes for preallocation
        let total_instructions: usize = rirs.iter().map(|r| r.instructions.len()).sum();
        let total_extra: usize = rirs.iter().map(|r| r.extra.len()).sum();
        let total_functions: usize = rirs.iter().map(|r| r.function_spans.len()).sum();

        let mut merged = Rir {
            instructions: Vec::with_capacity(total_instructions),
            extra: Vec::with_capacity(total_extra),
            function_spans: Vec::with_capacity(total_functions),
        };

        // Track offsets as we merge each RIR
        let mut inst_offset: u32 = 0;
        let mut extra_offset: u32 = 0;

        for rir in rirs {
            // Merge extra data first (append raw bytes)
            merged.extra.extend_from_slice(&rir.extra);

            // Merge and renumber instructions
            for inst in &rir.instructions {
                let renumbered = Self::renumber_instruction(inst, inst_offset, extra_offset);
                merged.instructions.push(renumbered);
            }

            // Renumber InstRefs in the extra array
            // This handles call args, match arms, field inits, etc.
            Self::renumber_extra_inst_refs(
                &mut merged.extra,
                &rir.instructions,
                inst_offset,
                extra_offset,
            );

            // Merge function spans with renumbered references
            for fn_span in &rir.function_spans {
                merged.function_spans.push(FunctionSpan {
                    name: fn_span.name,
                    body_start: InstRef::from_raw(fn_span.body_start.as_u32() + inst_offset),
                    decl: InstRef::from_raw(fn_span.decl.as_u32() + inst_offset),
                });
            }

            // Update offsets for the next RIR
            inst_offset += rir.instructions.len() as u32;
            extra_offset += rir.extra.len() as u32;
        }

        merged
    }

    /// Renumber a single instruction's InstRef fields.
    fn renumber_instruction(inst: &Inst, inst_offset: u32, extra_offset: u32) -> Inst {
        let renumber = |r: InstRef| InstRef::from_raw(r.as_u32() + inst_offset);
        let renumber_opt = |r: Option<InstRef>| r.map(renumber);

        let data = match &inst.data {
            // No renumbering needed for these
            InstData::IntConst(v) => InstData::IntConst(*v),
            InstData::BoolConst(v) => InstData::BoolConst(*v),
            InstData::StringConst(s) => InstData::StringConst(*s),
            InstData::UnitConst => InstData::UnitConst,
            InstData::Break => InstData::Break,
            InstData::Continue => InstData::Continue,
            InstData::VarRef { name } => InstData::VarRef { name: *name },
            InstData::ParamRef { index, name } => InstData::ParamRef {
                index: *index,
                name: *name,
            },
            InstData::EnumVariant {
                module,
                type_name,
                variant,
            } => InstData::EnumVariant {
                module: module.map(renumber),
                type_name: *type_name,
                variant: *variant,
            },
            InstData::TypeIntrinsic { name, type_arg } => InstData::TypeIntrinsic {
                name: *name,
                type_arg: *type_arg,
            },

            // Binary operations - renumber both operands
            InstData::Add { lhs, rhs } => InstData::Add {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Sub { lhs, rhs } => InstData::Sub {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Mul { lhs, rhs } => InstData::Mul {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Div { lhs, rhs } => InstData::Div {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Mod { lhs, rhs } => InstData::Mod {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Eq { lhs, rhs } => InstData::Eq {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Ne { lhs, rhs } => InstData::Ne {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Lt { lhs, rhs } => InstData::Lt {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Gt { lhs, rhs } => InstData::Gt {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Le { lhs, rhs } => InstData::Le {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Ge { lhs, rhs } => InstData::Ge {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::And { lhs, rhs } => InstData::And {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Or { lhs, rhs } => InstData::Or {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::BitAnd { lhs, rhs } => InstData::BitAnd {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::BitOr { lhs, rhs } => InstData::BitOr {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::BitXor { lhs, rhs } => InstData::BitXor {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Shl { lhs, rhs } => InstData::Shl {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Shr { lhs, rhs } => InstData::Shr {
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },

            // Unary operations
            InstData::Neg { operand } => InstData::Neg {
                operand: renumber(*operand),
            },
            InstData::Not { operand } => InstData::Not {
                operand: renumber(*operand),
            },
            InstData::BitNot { operand } => InstData::BitNot {
                operand: renumber(*operand),
            },

            // Control flow
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => InstData::Branch {
                cond: renumber(*cond),
                then_block: renumber(*then_block),
                else_block: renumber_opt(*else_block),
            },
            InstData::Loop { cond, body } => InstData::Loop {
                cond: renumber(*cond),
                body: renumber(*body),
            },
            InstData::InfiniteLoop { body } => InstData::InfiniteLoop {
                body: renumber(*body),
            },
            InstData::Ret(value) => InstData::Ret(renumber_opt(*value)),

            // Match - InstRefs in extra are handled separately
            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => InstData::Match {
                scrutinee: renumber(*scrutinee),
                arms_start: *arms_start + extra_offset,
                arms_len: *arms_len,
            },

            // Block - InstRefs in extra are handled separately
            InstData::Block { extra_start, len } => InstData::Block {
                extra_start: *extra_start + extra_offset,
                len: *len,
            },

            // Variable operations
            InstData::Alloc {
                directives_start,
                directives_len,
                name,
                is_mut,
                ty,
                init,
            } => InstData::Alloc {
                directives_start: *directives_start + extra_offset,
                directives_len: *directives_len,
                name: *name,
                is_mut: *is_mut,
                ty: *ty,
                init: renumber(*init),
            },
            InstData::Assign { name, value } => InstData::Assign {
                name: *name,
                value: renumber(*value),
            },

            // Function definition - body and params in extra
            InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub,
                is_unchecked,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
            } => InstData::FnDecl {
                directives_start: *directives_start + extra_offset,
                directives_len: *directives_len,
                is_pub: *is_pub,
                is_unchecked: *is_unchecked,
                name: *name,
                params_start: *params_start + extra_offset,
                params_len: *params_len,
                return_type: *return_type,
                body: renumber(*body),
                has_self: *has_self,
            },

            // Constant declaration - init is an InstRef
            InstData::ConstDecl {
                directives_start,
                directives_len,
                is_pub,
                name,
                ty,
                init,
            } => InstData::ConstDecl {
                directives_start: *directives_start + extra_offset,
                directives_len: *directives_len,
                is_pub: *is_pub,
                name: *name,
                ty: *ty,
                init: renumber(*init),
            },

            // Function call - args in extra
            InstData::Call {
                name,
                args_start,
                args_len,
            } => InstData::Call {
                name: *name,
                args_start: *args_start + extra_offset,
                args_len: *args_len,
            },

            // Intrinsic - args in extra
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => InstData::Intrinsic {
                name: *name,
                args_start: *args_start + extra_offset,
                args_len: *args_len,
            },

            // Struct operations
            InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub,
                is_linear,
                name,
                fields_start,
                fields_len,
            } => InstData::StructDecl {
                directives_start: *directives_start + extra_offset,
                directives_len: *directives_len,
                is_pub: *is_pub,
                is_linear: *is_linear,
                name: *name,
                fields_start: *fields_start + extra_offset,
                fields_len: *fields_len,
            },
            InstData::StructInit {
                module,
                type_name,
                fields_start,
                fields_len,
            } => InstData::StructInit {
                module: module.map(renumber),
                type_name: *type_name,
                fields_start: *fields_start + extra_offset,
                fields_len: *fields_len,
            },
            InstData::FieldGet { base, field } => InstData::FieldGet {
                base: renumber(*base),
                field: *field,
            },
            InstData::FieldSet { base, field, value } => InstData::FieldSet {
                base: renumber(*base),
                field: *field,
                value: renumber(*value),
            },

            // Enum operations
            InstData::EnumDecl {
                is_pub,
                name,
                variants_start,
                variants_len,
            } => InstData::EnumDecl {
                is_pub: *is_pub,
                name: *name,
                variants_start: *variants_start + extra_offset,
                variants_len: *variants_len,
            },

            // Array operations
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => InstData::ArrayInit {
                elems_start: *elems_start + extra_offset,
                elems_len: *elems_len,
            },
            InstData::IndexGet { base, index } => InstData::IndexGet {
                base: renumber(*base),
                index: renumber(*index),
            },
            InstData::IndexSet { base, index, value } => InstData::IndexSet {
                base: renumber(*base),
                index: renumber(*index),
                value: renumber(*value),
            },

            // Method operations
            InstData::ImplDecl {
                type_name,
                methods_start,
                methods_len,
            } => InstData::ImplDecl {
                type_name: *type_name,
                methods_start: *methods_start + extra_offset,
                methods_len: *methods_len,
            },
            InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            } => InstData::MethodCall {
                receiver: renumber(*receiver),
                method: *method,
                args_start: *args_start + extra_offset,
                args_len: *args_len,
            },
            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => InstData::AssocFnCall {
                type_name: *type_name,
                function: *function,
                args_start: *args_start + extra_offset,
                args_len: *args_len,
            },
            InstData::DropFnDecl { type_name, body } => InstData::DropFnDecl {
                type_name: *type_name,
                body: renumber(*body),
            },
            InstData::Comptime { expr } => InstData::Comptime {
                expr: renumber(*expr),
            },
            InstData::Checked { expr } => InstData::Checked {
                expr: renumber(*expr),
            },
            InstData::TypeConst { type_name } => InstData::TypeConst {
                type_name: *type_name,
            },
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            } => InstData::AnonStructType {
                fields_start: *fields_start + extra_offset,
                fields_len: *fields_len,
                methods_start: *methods_start + extra_offset,
                methods_len: *methods_len,
            },
        };

        Inst {
            data,
            span: inst.span,
        }
    }

    /// Renumber InstRefs stored in the extra array.
    ///
    /// This handles:
    /// - Block instruction refs (simple u32 array)
    /// - Array init element refs (simple u32 array)
    /// - Intrinsic arg refs (simple u32 array)
    /// - Impl method refs (simple u32 array)
    /// - Call args (value field of each arg)
    /// - Field inits (value field of each init)
    /// - Match arm bodies (last field of each arm)
    fn renumber_extra_inst_refs(
        extra: &mut [u32],
        instructions: &[Inst],
        inst_offset: u32,
        extra_offset: u32,
    ) {
        for inst in instructions {
            match &inst.data {
                // Block - contains InstRef array
                InstData::Block { extra_start, len } => {
                    let start = (*extra_start + extra_offset) as usize;
                    for i in 0..*len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Array init - contains InstRef array
                InstData::ArrayInit {
                    elems_start,
                    elems_len,
                } => {
                    let start = (*elems_start + extra_offset) as usize;
                    for i in 0..*elems_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Intrinsic - contains InstRef array
                InstData::Intrinsic {
                    args_start,
                    args_len,
                    ..
                } => {
                    let start = (*args_start + extra_offset) as usize;
                    for i in 0..*args_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Impl decl - contains InstRef array for methods
                InstData::ImplDecl {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    let start = (*methods_start + extra_offset) as usize;
                    for i in 0..*methods_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Anonymous struct type - contains InstRef array for methods
                InstData::AnonStructType {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    let start = (*methods_start + extra_offset) as usize;
                    for i in 0..*methods_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Call args - layout: [value, mode] pairs
                InstData::Call {
                    args_start,
                    args_len,
                    ..
                }
                | InstData::MethodCall {
                    args_start,
                    args_len,
                    ..
                }
                | InstData::AssocFnCall {
                    args_start,
                    args_len,
                    ..
                } => {
                    let start = (*args_start + extra_offset) as usize;
                    for i in 0..*args_len as usize {
                        // Each arg is 2 u32s: [value, mode]
                        extra[start + i * 2] += inst_offset;
                    }
                }

                // Field inits - layout: [name, value] pairs
                InstData::StructInit {
                    fields_start,
                    fields_len,
                    ..
                } => {
                    let start = (*fields_start + extra_offset) as usize;
                    for i in 0..*fields_len as usize {
                        // Each field is 2 u32s: [name, value]
                        extra[start + i * 2 + 1] += inst_offset;
                    }
                }

                // Match arms - variable size patterns with body InstRef at end
                InstData::Match {
                    arms_start,
                    arms_len,
                    ..
                } => {
                    let mut pos = (*arms_start + extra_offset) as usize;
                    for _ in 0..*arms_len {
                        let kind = extra[pos];
                        let pattern_size = match kind {
                            k if k == PatternKind::Wildcard as u32 => {
                                PATTERN_WILDCARD_SIZE as usize
                            }
                            k if k == PatternKind::Int as u32 => PATTERN_INT_SIZE as usize,
                            k if k == PatternKind::Bool as u32 => PATTERN_BOOL_SIZE as usize,
                            k if k == PatternKind::Path as u32 => PATTERN_PATH_SIZE as usize,
                            _ => panic!("Unknown pattern kind during merge: {}", kind),
                        };
                        // The body InstRef is always the last element of each pattern
                        extra[pos + pattern_size - 1] += inst_offset;
                        pos += pattern_size;
                    }
                }

                // These don't have InstRefs in extra
                InstData::IntConst(_)
                | InstData::BoolConst(_)
                | InstData::StringConst(_)
                | InstData::UnitConst
                | InstData::Add { .. }
                | InstData::Sub { .. }
                | InstData::Mul { .. }
                | InstData::Div { .. }
                | InstData::Mod { .. }
                | InstData::Eq { .. }
                | InstData::Ne { .. }
                | InstData::Lt { .. }
                | InstData::Gt { .. }
                | InstData::Le { .. }
                | InstData::Ge { .. }
                | InstData::And { .. }
                | InstData::Or { .. }
                | InstData::BitAnd { .. }
                | InstData::BitOr { .. }
                | InstData::BitXor { .. }
                | InstData::Shl { .. }
                | InstData::Shr { .. }
                | InstData::Neg { .. }
                | InstData::Not { .. }
                | InstData::BitNot { .. }
                | InstData::Branch { .. }
                | InstData::Loop { .. }
                | InstData::InfiniteLoop { .. }
                | InstData::Break
                | InstData::Continue
                | InstData::Ret(_)
                | InstData::VarRef { .. }
                | InstData::ParamRef { .. }
                | InstData::Alloc { .. }
                | InstData::Assign { .. }
                | InstData::FnDecl { .. }
                | InstData::ConstDecl { .. }
                | InstData::FieldGet { .. }
                | InstData::FieldSet { .. }
                | InstData::StructDecl { .. }
                | InstData::EnumDecl { .. }
                | InstData::EnumVariant { .. }
                | InstData::IndexGet { .. }
                | InstData::IndexSet { .. }
                | InstData::TypeIntrinsic { .. }
                | InstData::DropFnDecl { .. }
                | InstData::Comptime { .. }
                | InstData::Checked { .. }
                | InstData::TypeConst { .. } => {}
            }
        }
    }
}

/// A single RIR instruction.
#[derive(Debug, Clone)]
pub struct Inst {
    pub data: InstData,
    pub span: Span,
}

/// Instruction data - the actual operation.
#[derive(Debug, Clone)]
pub enum InstData {
    /// Integer constant
    IntConst(u64),

    /// Boolean constant
    BoolConst(bool),

    /// String constant (interned string content)
    StringConst(Spur),

    /// Unit constant (for blocks that produce unit type)
    UnitConst,

    // Binary arithmetic operations
    /// Addition: lhs + rhs
    Add { lhs: InstRef, rhs: InstRef },
    /// Subtraction: lhs - rhs
    Sub { lhs: InstRef, rhs: InstRef },
    /// Multiplication: lhs * rhs
    Mul { lhs: InstRef, rhs: InstRef },
    /// Division: lhs / rhs
    Div { lhs: InstRef, rhs: InstRef },
    /// Modulo: lhs % rhs
    Mod { lhs: InstRef, rhs: InstRef },

    // Comparison operations
    /// Equality: lhs == rhs
    Eq { lhs: InstRef, rhs: InstRef },
    /// Inequality: lhs != rhs
    Ne { lhs: InstRef, rhs: InstRef },
    /// Less than: lhs < rhs
    Lt { lhs: InstRef, rhs: InstRef },
    /// Greater than: lhs > rhs
    Gt { lhs: InstRef, rhs: InstRef },
    /// Less than or equal: lhs <= rhs
    Le { lhs: InstRef, rhs: InstRef },
    /// Greater than or equal: lhs >= rhs
    Ge { lhs: InstRef, rhs: InstRef },

    // Logical operations
    /// Logical AND: lhs && rhs
    And { lhs: InstRef, rhs: InstRef },
    /// Logical OR: lhs || rhs
    Or { lhs: InstRef, rhs: InstRef },

    // Bitwise operations
    /// Bitwise AND: lhs & rhs
    BitAnd { lhs: InstRef, rhs: InstRef },
    /// Bitwise OR: lhs | rhs
    BitOr { lhs: InstRef, rhs: InstRef },
    /// Bitwise XOR: lhs ^ rhs
    BitXor { lhs: InstRef, rhs: InstRef },
    /// Left shift: lhs << rhs
    Shl { lhs: InstRef, rhs: InstRef },
    /// Right shift: lhs >> rhs (arithmetic for signed, logical for unsigned)
    Shr { lhs: InstRef, rhs: InstRef },

    // Unary operations
    /// Negation: -operand
    Neg { operand: InstRef },
    /// Logical NOT: !operand
    Not { operand: InstRef },
    /// Bitwise NOT: ~operand
    BitNot { operand: InstRef },

    // Control flow
    /// Branch: if cond then then_block else else_block
    Branch {
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
    },

    /// While loop: while cond { body }
    Loop { cond: InstRef, body: InstRef },

    /// Infinite loop: loop { body }
    InfiniteLoop { body: InstRef },

    /// Match expression: match scrutinee { pattern => expr, ... }
    /// Arms are stored in the extra array using add_match_arms/get_match_arms.
    Match {
        /// The value being matched
        scrutinee: InstRef,
        /// Index into extra data where arms start
        arms_start: u32,
        /// Number of match arms
        arms_len: u32,
    },

    /// Break: exits the innermost loop
    Break,

    /// Continue: jumps to the next iteration of the innermost loop
    Continue,

    /// Function definition
    /// Contains: name symbol, parameters, return type symbol, body instruction ref
    /// Directives and params are stored in the extra array.
    FnDecl {
        /// Index into extra data where directives start
        directives_start: u32,
        /// Number of directives
        directives_len: u32,
        /// Whether this function is public (requires --preview modules)
        is_pub: bool,
        /// Whether this function is marked `unchecked` (can only be called from checked blocks)
        is_unchecked: bool,
        name: Spur,
        /// Index into extra data where params start
        params_start: u32,
        /// Number of parameters
        params_len: u32,
        return_type: Spur,
        body: InstRef,
        /// Whether this function/method takes `self` as a receiver.
        /// Only true for methods in impl blocks that have a self parameter.
        /// Used by sema to know to add the implicit self parameter.
        has_self: bool,
    },

    /// Constant declaration
    /// Contains: name symbol, optional type, initializer expression ref
    /// Directives are stored in the extra array.
    /// Used for module re-exports: `pub const strings = @import("utils/strings.rue");`
    ConstDecl {
        /// Index into extra data where directives start
        directives_start: u32,
        /// Number of directives
        directives_len: u32,
        /// Whether this constant is public (requires --preview modules)
        is_pub: bool,
        /// Constant name
        name: Spur,
        /// Optional type annotation (interned string, None if inferred)
        ty: Option<Spur>,
        /// Initializer expression
        init: InstRef,
    },

    /// Function call
    /// Args are stored in the extra array using add_call_args/get_call_args.
    Call {
        /// Function name
        name: Spur,
        /// Index into extra data where args start
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Intrinsic call with expression arguments (e.g., @dbg)
    /// Args are stored in the extra array using add_inst_refs/get_inst_refs.
    Intrinsic {
        /// Intrinsic name (without @)
        name: Spur,
        /// Index into extra data where args start
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Intrinsic call with a type argument (e.g., @size_of, @align_of)
    TypeIntrinsic {
        /// Intrinsic name (without @)
        name: Spur,
        /// Type argument (as an interned string, e.g., "i32", "Point", "[i32; 4]")
        type_arg: Spur,
    },

    /// Reference to a function parameter
    ParamRef {
        /// Parameter index (0-based)
        index: u32,
        /// Parameter name (for error messages)
        name: Spur,
    },

    /// Return value from function (None for `return;` in unit-returning functions)
    Ret(Option<InstRef>),

    /// Block of instructions (for function bodies)
    /// The result is the last instruction in the block
    Block {
        /// Index into extra data where instruction refs start
        extra_start: u32,
        /// Number of instructions in the block
        len: u32,
    },

    // Variable operations
    /// Local variable declaration: allocates storage and initializes
    /// If name is None, this is a wildcard pattern that discards the value
    /// Directives are stored in the extra array using add_directives/get_directives.
    Alloc {
        /// Index into extra data where directives start
        directives_start: u32,
        /// Number of directives
        directives_len: u32,
        /// Variable name (None for wildcard `_` pattern that discards the value)
        name: Option<Spur>,
        /// Whether the variable is mutable
        is_mut: bool,
        /// Optional type annotation
        ty: Option<Spur>,
        /// Initial value instruction
        init: InstRef,
    },

    /// Variable reference: reads the value of a variable
    VarRef {
        /// Variable name
        name: Spur,
    },

    /// Assignment: stores a value into a mutable variable
    Assign {
        /// Variable name
        name: Spur,
        /// Value to store
        value: InstRef,
    },

    // Struct operations
    /// Struct type declaration
    /// Directives and fields are stored in the extra array.
    StructDecl {
        /// Index into extra data where directives start
        directives_start: u32,
        /// Number of directives
        directives_len: u32,
        /// Whether this struct is public (requires --preview modules)
        is_pub: bool,
        /// Whether this struct is a linear type (must be consumed)
        is_linear: bool,
        /// Struct name
        name: Spur,
        /// Index into extra data where fields start
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
    },

    /// Struct literal: creates a new struct instance
    /// Fields are stored in the extra array using add_field_inits/get_field_inits.
    StructInit {
        /// Optional module reference (for qualified struct literals like `module.Point { ... }`)
        /// If Some, the struct is looked up in the module's exports.
        module: Option<InstRef>,
        /// Struct type name
        type_name: Spur,
        /// Index into extra data where fields start
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
    },

    /// Field access: reads a field from a struct
    FieldGet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Spur,
    },

    /// Field assignment: writes a value to a struct field
    FieldSet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Spur,
        /// Value to store
        value: InstRef,
    },

    // Enum operations
    /// Enum type declaration
    /// Variants are stored in the extra array using add_symbols/get_symbols.
    EnumDecl {
        /// Whether this enum is public (requires --preview modules)
        is_pub: bool,
        /// Enum name
        name: Spur,
        /// Index into extra data where variants start
        variants_start: u32,
        /// Number of variants
        variants_len: u32,
    },

    /// Enum variant: creates a value of an enum type
    EnumVariant {
        /// Optional module reference (for qualified paths like `module.Color::Red`)
        /// If Some, the enum is looked up in the module's exports.
        module: Option<InstRef>,
        /// Enum type name
        type_name: Spur,
        /// Variant name
        variant: Spur,
    },

    // Array operations
    /// Array literal: creates a new array from element values
    /// Elements are stored in the extra array using add_inst_refs/get_inst_refs.
    ArrayInit {
        /// Index into extra data where elements start
        elems_start: u32,
        /// Number of elements
        elems_len: u32,
    },

    /// Array index read: reads an element from an array
    IndexGet {
        /// Base array value
        base: InstRef,
        /// Index expression
        index: InstRef,
    },

    /// Array index write: writes a value to an array element
    IndexSet {
        /// Base array value (must be a VarRef to a mutable variable)
        base: InstRef,
        /// Index expression
        index: InstRef,
        /// Value to store
        value: InstRef,
    },

    // Method operations
    /// Impl block declaration
    /// Methods are stored in the extra array using add_inst_refs/get_inst_refs.
    ImplDecl {
        /// Type name this impl block is for
        type_name: Spur,
        /// Index into extra data where method refs start
        methods_start: u32,
        /// Number of methods
        methods_len: u32,
    },

    /// Method call: receiver.method(args)
    /// Args are stored in the extra array using add_call_args/get_call_args.
    MethodCall {
        /// Receiver expression (the struct value)
        receiver: InstRef,
        /// Method name
        method: Spur,
        /// Index into extra data where args start
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Associated function call: Type::function(args)
    /// Args are stored in the extra array using add_call_args/get_call_args.
    AssocFnCall {
        /// Type name (e.g., Point)
        type_name: Spur,
        /// Function name (e.g., origin)
        function: Spur,
        /// Index into extra data where args start
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// User-defined destructor declaration: drop fn TypeName(self) { ... }
    DropFnDecl {
        /// The struct type this destructor is for
        type_name: Spur,
        /// Destructor body instruction ref
        body: InstRef,
    },

    /// Comptime block expression: comptime { expr }
    /// The inner expression must be evaluable at compile time.
    Comptime {
        /// The expression to evaluate at compile time
        expr: InstRef,
    },

    /// Checked block expression: checked { expr }
    /// Unchecked operations (raw pointer manipulation, calling unchecked functions)
    /// are only allowed inside checked blocks.
    Checked {
        /// The expression inside the checked block
        expr: InstRef,
    },

    /// Type constant: a type used as a value expression (e.g., `i32` in `identity(i32, 42)`)
    /// The type_name is the symbol for the type (e.g., "i32", "bool").
    TypeConst {
        /// The type name symbol
        type_name: Spur,
    },

    /// Anonymous struct type: a struct type used as a value expression
    /// (e.g., `struct { first: T, second: T, fn method(self) -> T { ... } }` in comptime type construction)
    /// Fields are stored in the extra array using add_field_decls/get_field_decls.
    /// Methods are stored as InstRefs to FnDecl instructions in the extra array.
    AnonStructType {
        /// Index into extra data where fields start
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
        /// Index into extra data where method InstRefs start
        methods_start: u32,
        /// Number of methods (InstRefs to FnDecl instructions)
        methods_len: u32,
    },
}

impl fmt::Display for InstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Printer for RIR that resolves symbols to their string values.
pub struct RirPrinter<'a, 'b> {
    rir: &'a Rir,
    interner: &'b lasso::ThreadedRodeo,
}

impl<'a, 'b> RirPrinter<'a, 'b> {
    /// Create a new RIR printer.
    pub fn new(rir: &'a Rir, interner: &'b lasso::ThreadedRodeo) -> Self {
        Self { rir, interner }
    }

    /// Format a call argument with its mode prefix.
    fn format_call_arg(arg: &RirCallArg) -> String {
        match arg.mode {
            RirArgMode::Inout => format!("inout {}", arg.value),
            RirArgMode::Borrow => format!("borrow {}", arg.value),
            RirArgMode::Normal => format!("{}", arg.value),
        }
    }

    /// Format a list of call arguments.
    fn format_call_args(args: &[RirCallArg]) -> String {
        args.iter()
            .map(Self::format_call_arg)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Format a pattern for printing.
    fn format_pattern(&self, pat: &RirPattern) -> String {
        match pat {
            RirPattern::Wildcard(_) => "_".to_string(),
            RirPattern::Int(n, _) => n.to_string(),
            RirPattern::Bool(b, _) => b.to_string(),
            RirPattern::Path {
                module,
                type_name,
                variant,
                ..
            } => {
                let prefix = if let Some(module_ref) = module {
                    format!("%{}..", module_ref.as_u32())
                } else {
                    String::new()
                };
                format!(
                    "{}{}::{}",
                    prefix,
                    self.interner.resolve(&*type_name),
                    self.interner.resolve(&*variant)
                )
            }
        }
    }

    /// Format the RIR as a string.
    pub fn to_string(&self) -> String {
        use std::fmt::Write;

        let mut out = String::new();
        for (inst_ref, inst) in self.rir.iter() {
            write!(out, "{} = ", inst_ref).unwrap();
            match &inst.data {
                // Constants
                InstData::IntConst(v) => writeln!(out, "const {}", v).unwrap(),
                InstData::BoolConst(v) => writeln!(out, "const {}", v).unwrap(),
                InstData::StringConst(s) => {
                    writeln!(out, "const {:?}", self.interner.resolve(&*s)).unwrap()
                }
                InstData::UnitConst => writeln!(out, "const ()").unwrap(),

                // Binary operations
                InstData::Add { lhs, rhs } => writeln!(out, "add {}, {}", lhs, rhs).unwrap(),
                InstData::Sub { lhs, rhs } => writeln!(out, "sub {}, {}", lhs, rhs).unwrap(),
                InstData::Mul { lhs, rhs } => writeln!(out, "mul {}, {}", lhs, rhs).unwrap(),
                InstData::Div { lhs, rhs } => writeln!(out, "div {}, {}", lhs, rhs).unwrap(),
                InstData::Mod { lhs, rhs } => writeln!(out, "mod {}, {}", lhs, rhs).unwrap(),
                InstData::Eq { lhs, rhs } => writeln!(out, "eq {}, {}", lhs, rhs).unwrap(),
                InstData::Ne { lhs, rhs } => writeln!(out, "ne {}, {}", lhs, rhs).unwrap(),
                InstData::Lt { lhs, rhs } => writeln!(out, "lt {}, {}", lhs, rhs).unwrap(),
                InstData::Gt { lhs, rhs } => writeln!(out, "gt {}, {}", lhs, rhs).unwrap(),
                InstData::Le { lhs, rhs } => writeln!(out, "le {}, {}", lhs, rhs).unwrap(),
                InstData::Ge { lhs, rhs } => writeln!(out, "ge {}, {}", lhs, rhs).unwrap(),
                InstData::And { lhs, rhs } => writeln!(out, "and {}, {}", lhs, rhs).unwrap(),
                InstData::Or { lhs, rhs } => writeln!(out, "or {}, {}", lhs, rhs).unwrap(),
                InstData::BitAnd { lhs, rhs } => writeln!(out, "bit_and {}, {}", lhs, rhs).unwrap(),
                InstData::BitOr { lhs, rhs } => writeln!(out, "bit_or {}, {}", lhs, rhs).unwrap(),
                InstData::BitXor { lhs, rhs } => writeln!(out, "bit_xor {}, {}", lhs, rhs).unwrap(),
                InstData::Shl { lhs, rhs } => writeln!(out, "shl {}, {}", lhs, rhs).unwrap(),
                InstData::Shr { lhs, rhs } => writeln!(out, "shr {}, {}", lhs, rhs).unwrap(),

                // Unary operations
                InstData::Neg { operand } => writeln!(out, "neg {}", operand).unwrap(),
                InstData::Not { operand } => writeln!(out, "not {}", operand).unwrap(),
                InstData::BitNot { operand } => writeln!(out, "bit_not {}", operand).unwrap(),

                // Control flow
                InstData::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    if let Some(else_b) = else_block {
                        writeln!(out, "branch {}, {}, {}", cond, then_block, else_b).unwrap();
                    } else {
                        writeln!(out, "branch {}, {}", cond, then_block).unwrap();
                    }
                }
                InstData::Loop { cond, body } => writeln!(out, "loop {}, {}", cond, body).unwrap(),
                InstData::InfiniteLoop { body } => writeln!(out, "infinite_loop {}", body).unwrap(),
                InstData::Match {
                    scrutinee,
                    arms_start,
                    arms_len,
                } => {
                    let arms = self.rir.get_match_arms(*arms_start, *arms_len);
                    let arms_str: Vec<String> = arms
                        .iter()
                        .map(|(pat, body)| format!("{} => {}", self.format_pattern(pat), body))
                        .collect();
                    writeln!(out, "match {} {{ {} }}", scrutinee, arms_str.join(", ")).unwrap();
                }
                InstData::Break => writeln!(out, "break").unwrap(),
                InstData::Continue => writeln!(out, "continue").unwrap(),

                // Functions
                InstData::FnDecl {
                    directives_start: _,
                    directives_len: _,
                    is_pub,
                    is_unchecked,
                    name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let unchecked_str = if *is_unchecked { "unchecked " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let ret_str = self.interner.resolve(&*return_type);
                    let self_str = if *has_self { "self, " } else { "" };
                    let params = self.rir.get_params(*params_start, *params_len);
                    let params_str: Vec<String> = params
                        .iter()
                        .map(|p| {
                            let mode_prefix = match p.mode {
                                RirParamMode::Inout => "inout ",
                                RirParamMode::Borrow => "borrow ",
                                RirParamMode::Comptime => "comptime ",
                                RirParamMode::Normal => "",
                            };
                            format!(
                                "{}{}: {}",
                                mode_prefix,
                                self.interner.resolve(&p.name),
                                self.interner.resolve(&p.ty)
                            )
                        })
                        .collect();
                    writeln!(
                        out,
                        "{}{}fn {}({}{}) -> {} {{",
                        pub_str,
                        unchecked_str,
                        name_str,
                        self_str,
                        params_str.join(", "),
                        ret_str
                    )
                    .unwrap();
                    writeln!(out, "    {}", body).unwrap();
                    writeln!(out, "}}").unwrap();
                }
                InstData::ConstDecl {
                    directives_start: _,
                    directives_len: _,
                    is_pub,
                    name,
                    ty,
                    init,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let ty_str = ty
                        .map(|t| format!(": {}", self.interner.resolve(&t)))
                        .unwrap_or_default();
                    writeln!(out, "{}const {}{} = {}", pub_str, name_str, ty_str, init).unwrap();
                }
                InstData::Ret(inner) => {
                    if let Some(inner) = inner {
                        writeln!(out, "ret {}", inner).unwrap();
                    } else {
                        writeln!(out, "ret").unwrap();
                    }
                }
                InstData::Call {
                    name,
                    args_start,
                    args_len,
                } => {
                    let name_str = self.interner.resolve(&*name);
                    let args = self.rir.get_call_args(*args_start, *args_len);
                    writeln!(out, "call {}({})", name_str, Self::format_call_args(&args)).unwrap();
                }
                InstData::Intrinsic {
                    name,
                    args_start,
                    args_len,
                } => {
                    let name_str = self.interner.resolve(&*name);
                    let args = self.rir.get_inst_refs(*args_start, *args_len);
                    let args_str: Vec<String> = args.iter().map(|a| format!("{}", a)).collect();
                    writeln!(out, "intrinsic @{}({})", name_str, args_str.join(", ")).unwrap();
                }
                InstData::TypeIntrinsic { name, type_arg } => {
                    let name_str = self.interner.resolve(&*name);
                    let type_str = self.interner.resolve(&*type_arg);
                    writeln!(out, "type_intrinsic @{}({})", name_str, type_str).unwrap();
                }
                InstData::ParamRef { index, name } => {
                    writeln!(out, "param {} ({})", index, self.interner.resolve(&*name)).unwrap();
                }
                InstData::Block { extra_start, len } => {
                    writeln!(out, "block({}, {})", extra_start, len).unwrap();
                }

                // Variables
                InstData::Alloc {
                    directives_start: _,
                    directives_len: _,
                    name,
                    is_mut,
                    ty,
                    init,
                } => {
                    let name_str = name
                        .map(|n| self.interner.resolve(&n).to_string())
                        .unwrap_or_else(|| "_".to_string());
                    let mut_str = if *is_mut { "mut " } else { "" };
                    let ty_str = ty
                        .map(|t| format!(": {}", self.interner.resolve(&t)))
                        .unwrap_or_default();
                    writeln!(out, "alloc {}{}{}= {}", mut_str, name_str, ty_str, init).unwrap();
                }
                InstData::VarRef { name } => {
                    writeln!(out, "var_ref {}", self.interner.resolve(&*name)).unwrap();
                }
                InstData::Assign { name, value } => {
                    writeln!(out, "assign {} = {}", self.interner.resolve(&*name), value).unwrap();
                }

                // Structs
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    is_pub,
                    is_linear,
                    name,
                    fields_start,
                    fields_len,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let fields = self.rir.get_field_decls(*fields_start, *fields_len);
                    let fields_str: Vec<String> = fields
                        .iter()
                        .map(|(fname, ftype)| {
                            format!(
                                "{}: {}",
                                self.interner.resolve(&*fname),
                                self.interner.resolve(&*ftype)
                            )
                        })
                        .collect();
                    let directives = self.rir.get_directives(*directives_start, *directives_len);
                    let linear_str = if *is_linear { "linear " } else { "" };
                    let directives_str = if directives.is_empty() {
                        String::new()
                    } else {
                        let dir_names: Vec<String> = directives
                            .iter()
                            .map(|d| format!("@{}", self.interner.resolve(&d.name)))
                            .collect();
                        format!("{} ", dir_names.join(" "))
                    };
                    writeln!(
                        out,
                        "{}{}{}struct {} {{ {} }}",
                        directives_str,
                        pub_str,
                        linear_str,
                        name_str,
                        fields_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::StructInit {
                    module,
                    type_name,
                    fields_start,
                    fields_len,
                } => {
                    let module_str = module.map(|m| format!("{}.", m)).unwrap_or_default();
                    let type_str = self.interner.resolve(&*type_name);
                    let fields = self.rir.get_field_inits(*fields_start, *fields_len);
                    let fields_str: Vec<String> = fields
                        .iter()
                        .map(|(fname, value)| {
                            format!("{}: {}", self.interner.resolve(&*fname), value)
                        })
                        .collect();
                    writeln!(
                        out,
                        "struct_init {}{} {{ {} }}",
                        module_str,
                        type_str,
                        fields_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::FieldGet { base, field } => {
                    writeln!(out, "field_get {}.{}", base, self.interner.resolve(&*field)).unwrap();
                }
                InstData::FieldSet { base, field, value } => {
                    writeln!(
                        out,
                        "field_set {}.{} = {}",
                        base,
                        self.interner.resolve(&*field),
                        value
                    )
                    .unwrap();
                }

                // Enums
                InstData::EnumDecl {
                    is_pub,
                    name,
                    variants_start,
                    variants_len,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let variants = self.rir.get_symbols(*variants_start, *variants_len);
                    let variants_str: Vec<String> = variants
                        .iter()
                        .map(|v| self.interner.resolve(&*v).to_string())
                        .collect();
                    writeln!(
                        out,
                        "{}enum {} {{ {} }}",
                        pub_str,
                        name_str,
                        variants_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::EnumVariant {
                    module,
                    type_name,
                    variant,
                } => {
                    let module_str = module.map(|m| format!("{}.", m)).unwrap_or_default();
                    writeln!(
                        out,
                        "enum_variant {}{}::{}",
                        module_str,
                        self.interner.resolve(&*type_name),
                        self.interner.resolve(&*variant)
                    )
                    .unwrap();
                }

                // Arrays
                InstData::ArrayInit {
                    elems_start,
                    elems_len,
                } => {
                    let elements = self.rir.get_inst_refs(*elems_start, *elems_len);
                    let elems_str: Vec<String> =
                        elements.iter().map(|e| format!("{}", e)).collect();
                    writeln!(out, "array_init [{}]", elems_str.join(", ")).unwrap();
                }
                InstData::IndexGet { base, index } => {
                    writeln!(out, "index_get {}[{}]", base, index).unwrap();
                }
                InstData::IndexSet { base, index, value } => {
                    writeln!(out, "index_set {}[{}] = {}", base, index, value).unwrap();
                }

                // Methods
                InstData::ImplDecl {
                    type_name,
                    methods_start,
                    methods_len,
                } => {
                    let type_str = self.interner.resolve(&*type_name);
                    let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let methods_str: Vec<String> =
                        methods.iter().map(|m| format!("{}", m)).collect();
                    writeln!(out, "impl {} {{ {} }}", type_str, methods_str.join(", ")).unwrap();
                }
                InstData::MethodCall {
                    receiver,
                    method,
                    args_start,
                    args_len,
                } => {
                    let args = self.rir.get_call_args(*args_start, *args_len);
                    writeln!(
                        out,
                        "method_call {}.{}({})",
                        receiver,
                        self.interner.resolve(&*method),
                        Self::format_call_args(&args)
                    )
                    .unwrap();
                }
                InstData::AssocFnCall {
                    type_name,
                    function,
                    args_start,
                    args_len,
                } => {
                    let args = self.rir.get_call_args(*args_start, *args_len);
                    writeln!(
                        out,
                        "assoc_fn_call {}::{}({})",
                        self.interner.resolve(&*type_name),
                        self.interner.resolve(&*function),
                        Self::format_call_args(&args)
                    )
                    .unwrap();
                }

                // Drop
                InstData::DropFnDecl { type_name, body } => {
                    writeln!(
                        out,
                        "drop fn {}(self) {{",
                        self.interner.resolve(&*type_name)
                    )
                    .unwrap();
                    writeln!(out, "    {}", body).unwrap();
                    writeln!(out, "}}").unwrap();
                }

                // Comptime block
                InstData::Comptime { expr } => {
                    writeln!(out, "comptime {{ {} }}", expr).unwrap();
                }

                // Checked block
                InstData::Checked { expr } => {
                    writeln!(out, "checked {{ {} }}", expr).unwrap();
                }

                // Type constant
                InstData::TypeConst { type_name } => {
                    let name = self.interner.resolve(type_name);
                    writeln!(out, "type {}", name).unwrap();
                }

                // Anonymous struct type
                InstData::AnonStructType {
                    fields_start,
                    fields_len,
                    methods_start,
                    methods_len,
                } => {
                    write!(out, "struct {{ ").unwrap();
                    let fields = self.rir.get_field_decls(*fields_start, *fields_len);
                    for (i, (name, ty)) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(out, ", ").unwrap();
                        }
                        let name_str = self.interner.resolve(name);
                        let ty_str = self.interner.resolve(ty);
                        write!(out, "{}: {}", name_str, ty_str).unwrap();
                    }
                    // Print methods if any
                    if *methods_len > 0 {
                        let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                        let methods_str: Vec<String> =
                            methods.iter().map(|m| format!("{}", m)).collect();
                        if !fields.is_empty() {
                            write!(out, ", ").unwrap();
                        }
                        write!(out, "methods: [{}]", methods_str.join(", ")).unwrap();
                    }
                    writeln!(out, " }}").unwrap();
                }
            }
        }
        out
    }
}

impl fmt::Display for RirPrinter<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;

    #[test]
    fn test_inst_ref_size() {
        assert_eq!(std::mem::size_of::<InstRef>(), 4);
    }

    #[test]
    fn test_add_and_get_inst() {
        let mut rir = Rir::new();
        let inst = Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        };
        let inst_ref = rir.add_inst(inst);

        let retrieved = rir.get(inst_ref);
        assert!(matches!(retrieved.data, InstData::IntConst(42)));
    }

    #[test]
    fn test_rir_is_empty() {
        let rir = Rir::new();
        assert!(rir.is_empty());
        assert_eq!(rir.len(), 0);
    }

    #[test]
    fn test_rir_extra_data() {
        let mut rir = Rir::new();
        let data = [1, 2, 3, 4, 5];
        let start = rir.add_extra(&data);
        assert_eq!(start, 0);

        let retrieved = rir.get_extra(start, 5);
        assert_eq!(retrieved, &data);

        // Add more extra data
        let data2 = [10, 20];
        let start2 = rir.add_extra(&data2);
        assert_eq!(start2, 5);
    }

    #[test]
    fn test_rir_iter() {
        let mut rir = Rir::new();
        rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(2, 3),
        });

        let items: Vec<_> = rir.iter().collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0.as_u32(), 0);
        assert_eq!(items[1].0.as_u32(), 1);
    }

    #[test]
    fn test_inst_ref_display() {
        let inst_ref = InstRef::from_raw(42);
        assert_eq!(format!("{}", inst_ref), "%42");
    }

    // RirPattern tests
    #[test]
    fn test_rir_pattern_wildcard_span() {
        let span = Span::new(10, 11);
        let pattern = RirPattern::Wildcard(span);
        assert_eq!(pattern.span(), span);
    }

    #[test]
    fn test_rir_pattern_int_span() {
        let span = Span::new(20, 22);
        let pattern = RirPattern::Int(42, span);
        assert_eq!(pattern.span(), span);

        // Test negative int
        let pattern_neg = RirPattern::Int(-100, span);
        assert_eq!(pattern_neg.span(), span);
    }

    #[test]
    fn test_rir_pattern_bool_span() {
        let span = Span::new(30, 34);
        let pattern = RirPattern::Bool(true, span);
        assert_eq!(pattern.span(), span);

        let pattern_false = RirPattern::Bool(false, span);
        assert_eq!(pattern_false.span(), span);
    }

    #[test]
    fn test_rir_pattern_path_span() {
        let span = Span::new(40, 50);
        let interner = ThreadedRodeo::new();
        let type_name = interner.get_or_intern("Color");
        let variant = interner.get_or_intern("Red");

        let pattern = RirPattern::Path {
            module: None,
            type_name,
            variant,
            span,
        };
        assert_eq!(pattern.span(), span);
    }

    // RirCallArg tests
    #[test]
    fn test_rir_call_arg_is_inout() {
        let arg_normal = RirCallArg {
            value: InstRef::from_raw(0),
            mode: RirArgMode::Normal,
        };
        assert!(!arg_normal.is_inout());
        assert!(!arg_normal.is_borrow());

        let arg_inout = RirCallArg {
            value: InstRef::from_raw(0),
            mode: RirArgMode::Inout,
        };
        assert!(arg_inout.is_inout());
        assert!(!arg_inout.is_borrow());

        let arg_borrow = RirCallArg {
            value: InstRef::from_raw(0),
            mode: RirArgMode::Borrow,
        };
        assert!(!arg_borrow.is_inout());
        assert!(arg_borrow.is_borrow());
    }

    // RirPrinter tests
    fn create_printer_test_rir() -> (Rir, ThreadedRodeo) {
        let rir = Rir::new();
        let interner = ThreadedRodeo::new();
        (rir, interner)
    }

    #[test]
    fn test_printer_int_const() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const 42"));
    }

    #[test]
    fn test_printer_bool_const() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        rir.add_inst(Inst {
            data: InstData::BoolConst(false),
            span: Span::new(0, 5),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const true"));
        assert!(output.contains("%1 = const false"));
    }

    #[test]
    fn test_printer_string_const() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let hello = interner.get_or_intern("hello world");
        rir.add_inst(Inst {
            data: InstData::StringConst(hello),
            span: Span::new(0, 13),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const \"hello world\""));
    }

    #[test]
    fn test_printer_unit_const() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const ()"));
    }

    #[test]
    fn test_printer_binary_ops() {
        let (mut rir, interner) = create_printer_test_rir();
        let lhs = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let rhs = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(2, 3),
        });

        // Test all binary operations
        let ops = vec![
            (InstData::Add { lhs, rhs }, "add"),
            (InstData::Sub { lhs, rhs }, "sub"),
            (InstData::Mul { lhs, rhs }, "mul"),
            (InstData::Div { lhs, rhs }, "div"),
            (InstData::Mod { lhs, rhs }, "mod"),
            (InstData::Eq { lhs, rhs }, "eq"),
            (InstData::Ne { lhs, rhs }, "ne"),
            (InstData::Lt { lhs, rhs }, "lt"),
            (InstData::Gt { lhs, rhs }, "gt"),
            (InstData::Le { lhs, rhs }, "le"),
            (InstData::Ge { lhs, rhs }, "ge"),
            (InstData::And { lhs, rhs }, "and"),
            (InstData::Or { lhs, rhs }, "or"),
            (InstData::BitAnd { lhs, rhs }, "bit_and"),
            (InstData::BitOr { lhs, rhs }, "bit_or"),
            (InstData::BitXor { lhs, rhs }, "bit_xor"),
            (InstData::Shl { lhs, rhs }, "shl"),
            (InstData::Shr { lhs, rhs }, "shr"),
        ];

        for (data, op_name) in ops {
            let mut test_rir = Rir::new();
            let lhs = test_rir.add_inst(Inst {
                data: InstData::IntConst(1),
                span: Span::new(0, 1),
            });
            let rhs = test_rir.add_inst(Inst {
                data: InstData::IntConst(2),
                span: Span::new(2, 3),
            });
            // Recreate the data with new refs
            let data = match op_name {
                "add" => InstData::Add { lhs, rhs },
                "sub" => InstData::Sub { lhs, rhs },
                "mul" => InstData::Mul { lhs, rhs },
                "div" => InstData::Div { lhs, rhs },
                "mod" => InstData::Mod { lhs, rhs },
                "eq" => InstData::Eq { lhs, rhs },
                "ne" => InstData::Ne { lhs, rhs },
                "lt" => InstData::Lt { lhs, rhs },
                "gt" => InstData::Gt { lhs, rhs },
                "le" => InstData::Le { lhs, rhs },
                "ge" => InstData::Ge { lhs, rhs },
                "and" => InstData::And { lhs, rhs },
                "or" => InstData::Or { lhs, rhs },
                "bit_and" => InstData::BitAnd { lhs, rhs },
                "bit_or" => InstData::BitOr { lhs, rhs },
                "bit_xor" => InstData::BitXor { lhs, rhs },
                "shl" => InstData::Shl { lhs, rhs },
                "shr" => InstData::Shr { lhs, rhs },
                _ => unreachable!(),
            };
            test_rir.add_inst(Inst {
                data,
                span: Span::new(0, 5),
            });

            let printer = RirPrinter::new(&test_rir, &interner);
            let output = printer.to_string();
            let expected = format!("%2 = {} %0, %1", op_name);
            assert!(
                output.contains(&expected),
                "Expected '{}' in output:\n{}",
                expected,
                output
            );
        }
    }

    #[test]
    fn test_printer_unary_ops() {
        let (mut rir, interner) = create_printer_test_rir();
        let operand = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        rir.add_inst(Inst {
            data: InstData::Neg { operand },
            span: Span::new(0, 3),
        });
        rir.add_inst(Inst {
            data: InstData::Not { operand },
            span: Span::new(0, 3),
        });
        rir.add_inst(Inst {
            data: InstData::BitNot { operand },
            span: Span::new(0, 3),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("neg %0"));
        assert!(output.contains("not %0"));
        assert!(output.contains("bit_not %0"));
    }

    #[test]
    fn test_printer_branch() {
        let (mut rir, interner) = create_printer_test_rir();
        let cond = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let then_block = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let else_block = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        // With else block
        rir.add_inst(Inst {
            data: InstData::Branch {
                cond,
                then_block,
                else_block: Some(else_block),
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("branch %0, %1, %2"));
    }

    #[test]
    fn test_printer_branch_no_else() {
        let (mut rir, interner) = create_printer_test_rir();
        let cond = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let then_block = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::Branch {
                cond,
                then_block,
                else_block: None,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        // Should not have the third argument
        assert!(output.contains("branch %0, %1\n"));
    }

    #[test]
    fn test_printer_loop() {
        let (mut rir, interner) = create_printer_test_rir();
        let cond = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::Loop { cond, body },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("loop %0, %1"));
    }

    #[test]
    fn test_printer_infinite_loop() {
        let (mut rir, interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::InfiniteLoop { body },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("infinite_loop %0"));
    }

    #[test]
    fn test_printer_break_continue() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::Break,
            span: Span::new(0, 5),
        });
        rir.add_inst(Inst {
            data: InstData::Continue,
            span: Span::new(0, 8),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("break\n"));
        assert!(output.contains("continue\n"));
    }

    #[test]
    fn test_printer_ret() {
        let (mut rir, interner) = create_printer_test_rir();
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        // Return with value
        rir.add_inst(Inst {
            data: InstData::Ret(Some(value)),
            span: Span::new(0, 10),
        });
        // Return without value
        rir.add_inst(Inst {
            data: InstData::Ret(None),
            span: Span::new(0, 6),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("ret %0"));
        assert!(output.contains("%2 = ret\n"));
    }

    #[test]
    fn test_printer_fn_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("main");
        let return_type = interner.get_or_intern("i32");
        let param_name = interner.get_or_intern("x");
        let param_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (params_start, params_len) = rir.add_params(&[RirParam {
            name: param_name,
            ty: param_type,
            mode: RirParamMode::Normal,
            is_comptime: false,
        }]);

        rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: false,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("fn main(x: i32) -> i32"));
    }

    #[test]
    fn test_printer_fn_decl_with_self() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let name = interner.get_or_intern("get_x");
        let return_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (params_start, params_len) = rir.add_params(&[]);

        rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: true,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("fn get_x(self, ) -> i32"));
    }

    #[test]
    fn test_printer_fn_decl_param_modes() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("modify");
        let return_type = interner.get_or_intern("()");
        let param1_name = interner.get_or_intern("a");
        let param1_type = interner.get_or_intern("i32");
        let param2_name = interner.get_or_intern("b");
        let param2_type = interner.get_or_intern("i32");
        let param3_name = interner.get_or_intern("c");
        let param3_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (params_start, params_len) = rir.add_params(&[
            RirParam {
                name: param1_name,
                ty: param1_type,
                mode: RirParamMode::Normal,
                is_comptime: false,
            },
            RirParam {
                name: param2_name,
                ty: param2_type,
                mode: RirParamMode::Inout,
                is_comptime: false,
            },
            RirParam {
                name: param3_name,
                ty: param3_type,
                mode: RirParamMode::Borrow,
                is_comptime: false,
            },
        ]);

        rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: false,
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("a: i32"));
        assert!(output.contains("inout b: i32"));
        assert!(output.contains("borrow c: i32"));
    }

    #[test]
    fn test_printer_call() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let arg = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("foo");

        let (args_start, args_len) = rir.add_call_args(&[RirCallArg {
            value: arg,
            mode: RirArgMode::Normal,
        }]);

        rir.add_inst(Inst {
            data: InstData::Call {
                name,
                args_start,
                args_len,
            },
            span: Span::new(0, 8),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("call foo(%0)"));
    }

    #[test]
    fn test_printer_call_with_arg_modes() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let arg1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let arg2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let arg3 = rir.add_inst(Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 1),
        });

        let name = interner.get_or_intern("modify");

        let (args_start, args_len) = rir.add_call_args(&[
            RirCallArg {
                value: arg1,
                mode: RirArgMode::Normal,
            },
            RirCallArg {
                value: arg2,
                mode: RirArgMode::Inout,
            },
            RirCallArg {
                value: arg3,
                mode: RirArgMode::Borrow,
            },
        ]);

        rir.add_inst(Inst {
            data: InstData::Call {
                name,
                args_start,
                args_len,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("call modify(%0, inout %1, borrow %2)"));
    }

    #[test]
    fn test_printer_intrinsic() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let arg = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("dbg");

        let (args_start, args_len) = rir.add_call_args(&[RirCallArg {
            value: arg,
            mode: RirArgMode::Normal,
        }]);

        rir.add_inst(Inst {
            data: InstData::Intrinsic {
                name,
                args_start,
                args_len,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("intrinsic @dbg(%0)"));
    }

    #[test]
    fn test_printer_type_intrinsic() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.get_or_intern("size_of");
        let type_arg = interner.get_or_intern("i32");

        rir.add_inst(Inst {
            data: InstData::TypeIntrinsic { name, type_arg },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("type_intrinsic @size_of(i32)"));
    }

    #[test]
    fn test_printer_param_ref() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::ParamRef { index: 0, name },
            span: Span::new(0, 1),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("param 0 (x)"));
    }

    #[test]
    fn test_printer_block() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::Block {
                extra_start: 0,
                len: 3,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("block(0, 3)"));
    }

    #[test]
    fn test_printer_alloc() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let init = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("x");
        let ty = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);

        // Normal alloc with type
        rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start,
                directives_len,
                name: Some(name),
                is_mut: false,
                ty: Some(ty),
                init,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc x: i32= %0"));
    }

    #[test]
    fn test_printer_alloc_mut() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let init = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("x");

        let (directives_start, directives_len) = rir.add_directives(&[]);

        rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start,
                directives_len,
                name: Some(name),
                is_mut: true,
                ty: None,
                init,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc mut x= %0"));
    }

    #[test]
    fn test_printer_alloc_wildcard() {
        let (mut rir, interner) = create_printer_test_rir();
        let init = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let (directives_start, directives_len) = rir.add_directives(&[]);

        rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start,
                directives_len,
                name: None,
                is_mut: false,
                ty: None,
                init,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc _= %0"));
    }

    #[test]
    fn test_printer_var_ref() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::VarRef { name },
            span: Span::new(0, 1),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("var_ref x"));
    }

    #[test]
    fn test_printer_assign() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::Assign { name, value },
            span: Span::new(0, 6),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("assign x = %0"));
    }

    #[test]
    fn test_printer_struct_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.get_or_intern("Point");
        let x_name = interner.get_or_intern("x");
        let y_name = interner.get_or_intern("y");
        let i32_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (fields_start, fields_len) =
            rir.add_field_decls(&[(x_name, i32_type), (y_name, i32_type)]);

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_linear: false,
                name,
                fields_start,
                fields_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct Point { x: i32, y: i32 }"));
    }

    #[test]
    fn test_printer_struct_decl_with_directive() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.get_or_intern("Point");
        let x_name = interner.get_or_intern("x");
        let i32_type = interner.get_or_intern("i32");
        let copy_name = interner.get_or_intern("copy");

        let (directives_start, directives_len) = rir.add_directives(&[RirDirective {
            name: copy_name,
            args: vec![],
            span: Span::new(0, 5),
        }]);
        let (fields_start, fields_len) = rir.add_field_decls(&[(x_name, i32_type)]);

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_linear: false,
                name,
                fields_start,
                fields_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("@copy struct Point { x: i32 }"));
    }

    #[test]
    fn test_printer_struct_init() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let x_val = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });
        let y_val = rir.add_inst(Inst {
            data: InstData::IntConst(20),
            span: Span::new(0, 2),
        });

        let type_name = interner.get_or_intern("Point");
        let x_name = interner.get_or_intern("x");
        let y_name = interner.get_or_intern("y");

        let (fields_start, fields_len) = rir.add_field_inits(&[(x_name, x_val), (y_name, y_val)]);

        rir.add_inst(Inst {
            data: InstData::StructInit {
                module: None,
                type_name,
                fields_start,
                fields_len,
            },
            span: Span::new(0, 25),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct_init Point { x: %0, y: %1 }"));
    }

    #[test]
    fn test_printer_field_get() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for a struct value
            span: Span::new(0, 1),
        });

        let field = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::FieldGet { base, field },
            span: Span::new(0, 5),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("field_get %0.x"));
    }

    #[test]
    fn test_printer_field_set() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder
            span: Span::new(0, 1),
        });
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let field = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::FieldSet { base, field, value },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("field_set %0.x = %1"));
    }

    #[test]
    fn test_printer_enum_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.get_or_intern("Color");
        let red = interner.get_or_intern("Red");
        let green = interner.get_or_intern("Green");
        let blue = interner.get_or_intern("Blue");

        let (variants_start, variants_len) = rir.add_symbols(&[red, green, blue]);

        rir.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: false,
                name,
                variants_start,
                variants_len,
            },
            span: Span::new(0, 35),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("enum Color { Red, Green, Blue }"));
    }

    #[test]
    fn test_printer_enum_variant() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let type_name = interner.get_or_intern("Color");
        let variant = interner.get_or_intern("Red");

        rir.add_inst(Inst {
            data: InstData::EnumVariant {
                module: None,
                type_name,
                variant,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("enum_variant Color::Red"));
    }

    #[test]
    fn test_printer_array_init() {
        let (mut rir, interner) = create_printer_test_rir();
        let elem1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let elem2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let elem3 = rir.add_inst(Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 1),
        });

        let (elems_start, elems_len) = rir.add_inst_refs(&[elem1, elem2, elem3]);

        rir.add_inst(Inst {
            data: InstData::ArrayInit {
                elems_start,
                elems_len,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("array_init [%0, %1, %2]"));
    }

    #[test]
    fn test_printer_index_get() {
        let (mut rir, interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for array
            span: Span::new(0, 1),
        });
        let index = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::IndexGet { base, index },
            span: Span::new(0, 5),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("index_get %0[%1]"));
    }

    #[test]
    fn test_printer_index_set() {
        let (mut rir, interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for array
            span: Span::new(0, 1),
        });
        let index = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        rir.add_inst(Inst {
            data: InstData::IndexSet { base, index, value },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("index_set %0[%1] = %2"));
    }

    // Impl block tests
    #[test]
    fn test_printer_impl_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();

        // Create a method first
        let method_body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });
        let method_name = interner.get_or_intern("get_x");
        let return_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (params_start, params_len) = rir.add_params(&[]);

        let method_ref = rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name: method_name,
                params_start,
                params_len,
                return_type,
                body: method_body,
                has_self: true,
            },
            span: Span::new(0, 30),
        });

        let type_name = interner.get_or_intern("Point");

        let (methods_start, methods_len) = rir.add_inst_refs(&[method_ref]);

        rir.add_inst(Inst {
            data: InstData::ImplDecl {
                type_name,
                methods_start,
                methods_len,
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("impl Point { %1 }"));
    }

    #[test]
    fn test_printer_method_call() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let receiver = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for struct value
            span: Span::new(0, 1),
        });
        let arg = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        let method = interner.get_or_intern("add");

        let (args_start, args_len) = rir.add_call_args(&[RirCallArg {
            value: arg,
            mode: RirArgMode::Normal,
        }]);

        rir.add_inst(Inst {
            data: InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("method_call %0.add(%1)"));
    }

    #[test]
    fn test_printer_method_call_with_arg_modes() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let receiver = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });
        let arg1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let arg2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });

        let method = interner.get_or_intern("modify");

        let (args_start, args_len) = rir.add_call_args(&[
            RirCallArg {
                value: arg1,
                mode: RirArgMode::Inout,
            },
            RirCallArg {
                value: arg2,
                mode: RirArgMode::Borrow,
            },
        ]);

        rir.add_inst(Inst {
            data: InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            },
            span: Span::new(0, 25),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("method_call %0.modify(inout %1, borrow %2)"));
    }

    #[test]
    fn test_printer_assoc_fn_call() {
        let (mut rir, mut interner) = create_printer_test_rir();

        let type_name = interner.get_or_intern("Point");
        let function = interner.get_or_intern("origin");

        let (args_start, args_len) = rir.add_call_args(&[]);

        rir.add_inst(Inst {
            data: InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("assoc_fn_call Point::origin()"));
    }

    #[test]
    fn test_printer_assoc_fn_call_with_args() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let arg1 = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });
        let arg2 = rir.add_inst(Inst {
            data: InstData::IntConst(20),
            span: Span::new(0, 2),
        });

        let type_name = interner.get_or_intern("Point");
        let function = interner.get_or_intern("new");

        let (args_start, args_len) = rir.add_call_args(&[
            RirCallArg {
                value: arg1,
                mode: RirArgMode::Normal,
            },
            RirCallArg {
                value: arg2,
                mode: RirArgMode::Normal,
            },
        ]);

        rir.add_inst(Inst {
            data: InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("assoc_fn_call Point::new(%0, %1)"));
    }

    #[test]
    fn test_printer_drop_fn_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });

        let type_name = interner.get_or_intern("Resource");

        rir.add_inst(Inst {
            data: InstData::DropFnDecl { type_name, body },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("drop fn Resource(self)"));
    }

    // Match and pattern tests
    #[test]
    fn test_printer_match_wildcard() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let (arms_start, arms_len) =
            rir.add_match_arms(&[(RirPattern::Wildcard(Span::new(0, 1)), body)]);

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { _ => %1 }"));
    }

    #[test]
    fn test_printer_match_int_pattern() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });
        let body1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let body2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let body_default = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let (arms_start, arms_len) = rir.add_match_arms(&[
            (RirPattern::Int(1, Span::new(0, 1)), body1),
            (RirPattern::Int(-5, Span::new(0, 2)), body2),
            (RirPattern::Wildcard(Span::new(0, 1)), body_default),
        ]);

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { 1 => %1, -5 => %2, _ => %3 }"));
    }

    #[test]
    fn test_printer_match_bool_pattern() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let body_true = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let body_false = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let (arms_start, arms_len) = rir.add_match_arms(&[
            (RirPattern::Bool(true, Span::new(0, 4)), body_true),
            (RirPattern::Bool(false, Span::new(0, 5)), body_false),
        ]);

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { true => %1, false => %2 }"));
    }

    #[test]
    fn test_printer_match_path_pattern() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for enum value
            span: Span::new(0, 1),
        });
        let body_red = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let body_green = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let body_default = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let color = interner.get_or_intern("Color");
        let red = interner.get_or_intern("Red");
        let green = interner.get_or_intern("Green");

        let (arms_start, arms_len) = rir.add_match_arms(&[
            (
                RirPattern::Path {
                    module: None,
                    type_name: color,
                    variant: red,
                    span: Span::new(0, 10),
                },
                body_red,
            ),
            (
                RirPattern::Path {
                    module: None,
                    type_name: color,
                    variant: green,
                    span: Span::new(0, 12),
                },
                body_green,
            ),
            (RirPattern::Wildcard(Span::new(0, 1)), body_default),
        ]);

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { Color::Red => %1, Color::Green => %2, _ => %3 }"));
    }

    #[test]
    fn test_printer_display_trait() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let printer = RirPrinter::new(&rir, &interner);
        // Test Display trait implementation
        let output = format!("{}", printer);
        assert!(output.contains("%0 = const 42"));
    }

    // ===== RIR merge tests =====

    #[test]
    fn test_merge_empty_rirs() {
        let merged = Rir::merge(&[]);
        assert!(merged.is_empty());
        assert!(merged.function_spans().is_empty());
    }

    #[test]
    fn test_merge_single_rir() {
        let mut rir = Rir::new();
        rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });
        rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(3, 7),
        });

        let merged = Rir::merge(&[rir]);
        assert_eq!(merged.len(), 2);

        // Check that instructions are preserved
        assert!(matches!(
            merged.get(InstRef::from_raw(0)).data,
            InstData::IntConst(42)
        ));
        assert!(matches!(
            merged.get(InstRef::from_raw(1)).data,
            InstData::BoolConst(true)
        ));
    }

    #[test]
    fn test_merge_two_rirs_simple() {
        // RIR 1: just an int constant
        let mut rir1 = Rir::new();
        rir1.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        // RIR 2: another int constant
        let mut rir2 = Rir::new();
        rir2.add_inst(Inst {
            data: InstData::IntConst(20),
            span: Span::new(5, 7),
        });

        let merged = Rir::merge(&[rir1, rir2]);
        assert_eq!(merged.len(), 2);

        // First instruction from rir1
        assert!(matches!(
            merged.get(InstRef::from_raw(0)).data,
            InstData::IntConst(10)
        ));
        // Second instruction from rir2 (renumbered to index 1)
        assert!(matches!(
            merged.get(InstRef::from_raw(1)).data,
            InstData::IntConst(20)
        ));
    }

    #[test]
    fn test_merge_renumbers_inst_refs() {
        // RIR 1: const and an add that references it
        let mut rir1 = Rir::new();
        let const1 = rir1.add_inst(Inst {
            data: InstData::IntConst(5),
            span: Span::new(0, 1),
        });
        rir1.add_inst(Inst {
            data: InstData::Add {
                lhs: const1,
                rhs: const1,
            },
            span: Span::new(2, 5),
        });

        // RIR 2: const and an add that references it (local indices)
        let mut rir2 = Rir::new();
        let const2 = rir2.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(10, 12),
        });
        rir2.add_inst(Inst {
            data: InstData::Add {
                lhs: const2,
                rhs: const2,
            },
            span: Span::new(12, 16),
        });

        let merged = Rir::merge(&[rir1, rir2]);
        assert_eq!(merged.len(), 4);

        // Check rir1's add still references %0
        if let InstData::Add { lhs, rhs } = &merged.get(InstRef::from_raw(1)).data {
            assert_eq!(lhs.as_u32(), 0);
            assert_eq!(rhs.as_u32(), 0);
        } else {
            panic!("Expected Add instruction at index 1");
        }

        // Check rir2's add now references %2 (renumbered from %0)
        if let InstData::Add { lhs, rhs } = &merged.get(InstRef::from_raw(3)).data {
            assert_eq!(lhs.as_u32(), 2);
            assert_eq!(rhs.as_u32(), 2);
        } else {
            panic!("Expected Add instruction at index 3");
        }
    }

    #[test]
    fn test_merge_renumbers_extra_data() {
        // RIR 1: function call with args in extra
        let mut rir1 = Rir::new();
        let interner = ThreadedRodeo::new();
        let fn_name = interner.get_or_intern("foo");

        let const1 = rir1.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let (args_start, args_len) = rir1.add_call_args(&[RirCallArg {
            value: const1,
            mode: RirArgMode::Normal,
        }]);
        rir1.add_inst(Inst {
            data: InstData::Call {
                name: fn_name,
                args_start,
                args_len,
            },
            span: Span::new(2, 8),
        });

        // RIR 2: function call with args in extra
        let mut rir2 = Rir::new();
        let const2 = rir2.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(10, 11),
        });
        let (args_start2, args_len2) = rir2.add_call_args(&[RirCallArg {
            value: const2,
            mode: RirArgMode::Normal,
        }]);
        rir2.add_inst(Inst {
            data: InstData::Call {
                name: fn_name,
                args_start: args_start2,
                args_len: args_len2,
            },
            span: Span::new(12, 18),
        });

        let merged = Rir::merge(&[rir1, rir2]);
        assert_eq!(merged.len(), 4);

        // Check rir1's call still has correct args_start
        if let InstData::Call {
            args_start,
            args_len,
            ..
        } = &merged.get(InstRef::from_raw(1)).data
        {
            let args = merged.get_call_args(*args_start, *args_len);
            assert_eq!(args.len(), 1);
            assert_eq!(args[0].value.as_u32(), 0); // Still references const1 at %0
        } else {
            panic!("Expected Call instruction at index 1");
        }

        // Check rir2's call has updated args_start and renumbered arg value
        if let InstData::Call {
            args_start,
            args_len,
            ..
        } = &merged.get(InstRef::from_raw(3)).data
        {
            let args = merged.get_call_args(*args_start, *args_len);
            assert_eq!(args.len(), 1);
            assert_eq!(args[0].value.as_u32(), 2); // Now references const2 at %2
        } else {
            panic!("Expected Call instruction at index 3");
        }
    }

    #[test]
    fn test_merge_function_spans() {
        let interner = ThreadedRodeo::new();
        let main_name = interner.get_or_intern("main");
        let helper_name = interner.get_or_intern("helper");

        // RIR 1: main function
        let mut rir1 = Rir::new();
        let body_start1 = InstRef::from_raw(rir1.current_inst_index());
        let const1 = rir1.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });
        let (params_start, params_len) = rir1.add_params(&[]);
        let (dirs_start, dirs_len) = rir1.add_directives(&[]);
        let ret_type = interner.get_or_intern("i32");
        let decl1 = rir1.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start: dirs_start,
                directives_len: dirs_len,
                is_pub: false,
                is_unchecked: false,
                name: main_name,
                params_start,
                params_len,
                return_type: ret_type,
                body: const1,
                has_self: false,
            },
            span: Span::new(0, 10),
        });
        rir1.add_function_span(FunctionSpan::new(main_name, body_start1, decl1));

        // RIR 2: helper function
        let mut rir2 = Rir::new();
        let body_start2 = InstRef::from_raw(rir2.current_inst_index());
        let const2 = rir2.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(20, 22),
        });
        let (params_start2, params_len2) = rir2.add_params(&[]);
        let (dirs_start2, dirs_len2) = rir2.add_directives(&[]);
        let decl2 = rir2.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start: dirs_start2,
                directives_len: dirs_len2,
                is_pub: false,
                is_unchecked: false,
                name: helper_name,
                params_start: params_start2,
                params_len: params_len2,
                return_type: ret_type,
                body: const2,
                has_self: false,
            },
            span: Span::new(20, 35),
        });
        rir2.add_function_span(FunctionSpan::new(helper_name, body_start2, decl2));

        let merged = Rir::merge(&[rir1, rir2]);

        // Check we have 2 function spans
        assert_eq!(merged.function_spans().len(), 2);

        // Check main function span (from rir1, indices unchanged)
        let main_span = &merged.function_spans()[0];
        assert_eq!(main_span.name, main_name);
        assert_eq!(main_span.body_start.as_u32(), 0);
        assert_eq!(main_span.decl.as_u32(), 1);

        // Check helper function span (from rir2, indices shifted by 2)
        let helper_span = &merged.function_spans()[1];
        assert_eq!(helper_span.name, helper_name);
        assert_eq!(helper_span.body_start.as_u32(), 2); // Was 0, now 0 + 2 = 2
        assert_eq!(helper_span.decl.as_u32(), 3); // Was 1, now 1 + 2 = 3
    }

    #[test]
    fn test_merge_three_rirs() {
        let mut rir1 = Rir::new();
        rir1.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });

        let mut rir2 = Rir::new();
        rir2.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(10, 11),
        });

        let mut rir3 = Rir::new();
        rir3.add_inst(Inst {
            data: InstData::IntConst(3),
            span: Span::new(20, 21),
        });

        let merged = Rir::merge(&[rir1, rir2, rir3]);
        assert_eq!(merged.len(), 3);

        assert!(matches!(
            merged.get(InstRef::from_raw(0)).data,
            InstData::IntConst(1)
        ));
        assert!(matches!(
            merged.get(InstRef::from_raw(1)).data,
            InstData::IntConst(2)
        ));
        assert!(matches!(
            merged.get(InstRef::from_raw(2)).data,
            InstData::IntConst(3)
        ));
    }

    #[test]
    fn test_merge_preserves_spans() {
        let mut rir1 = Rir::new();
        rir1.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(5, 10),
        });

        let mut rir2 = Rir::new();
        rir2.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(100, 105),
        });

        let merged = Rir::merge(&[rir1, rir2]);

        // Spans should be preserved exactly
        assert_eq!(merged.get(InstRef::from_raw(0)).span, Span::new(5, 10));
        assert_eq!(merged.get(InstRef::from_raw(1)).span, Span::new(100, 105));
    }
}
