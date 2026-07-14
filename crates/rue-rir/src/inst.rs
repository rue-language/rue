//! RIR instruction definitions.
//!
//! Instructions are stored in a dense array and referenced by index.
//! This provides good cache locality and efficient traversal.

use std::fmt;

use lasso::{Key, Spur};
use rue_span::{FileId, Span};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RirDirective {
    /// Directive name (e.g., "allow")
    pub name: Spur,
    /// Arguments (e.g., ["unused_variable"])
    pub args: Vec<Spur>,
    /// Span covering the directive
    pub span: Span,
}

/// Parameter passing mode in RIR.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RirParamMode {
    /// Normal pass-by-value parameter
    #[default]
    Normal,
    /// Inout parameter - mutated in place and returned to caller
    Inout,
    /// Borrow parameter - immutable borrow without ownership transfer
    Borrow,
}

impl RirParamMode {
    /// Convert the serialized parameter mode from the RIR extra array.
    ///
    /// Invalid values indicate corrupted RIR or a producer/consumer mismatch.
    /// They must not silently recover as normal by-value parameters, because
    /// that changes ownership and aliasing semantics.
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => RirParamMode::Normal,
            1 => RirParamMode::Inout,
            2 => RirParamMode::Borrow,
            _ => panic!("invalid RirParamMode value: {}", v),
        }
    }

    /// Serialize this mode into the RIR extra array.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
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
    /// Whether this parameter is evaluated at compile time (declared with
    /// the `comptime` modifier; carried separately from `mode`, which stays
    /// `Normal` for comptime parameters)
    pub is_comptime: bool,
    /// Span of the parameter's name, used to point diagnostics (e.g. the
    /// duplicate-parameter error, RUE-349) at the offending occurrence.
    pub span: Span,
}

/// Argument passing mode in RIR.
#[repr(u32)]
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

impl RirArgMode {
    /// Convert the serialized call-argument mode from the RIR extra array.
    ///
    /// Invalid values indicate corrupted RIR or a producer/consumer mismatch.
    /// They must not silently recover as normal by-value arguments, because
    /// that changes ownership and aliasing semantics.
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => RirArgMode::Normal,
            1 => RirArgMode::Inout,
            2 => RirArgMode::Borrow,
            _ => panic!("invalid RirArgMode value: {}", v),
        }
    }

    /// Serialize this mode into the RIR extra array.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
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
    /// Integer literal pattern. The magnitude and sign are kept separate so
    /// Sema can range-check the literal against the scrutinee type exactly
    /// like `let` bindings (E0800/E0801) instead of silently wrapping
    /// (RUE-74). `negative` means the source had a leading `-`.
    Int {
        /// Magnitude of the literal as written (e.g. 128 for `-128`).
        value: u64,
        /// True if the pattern was written with a leading minus sign.
        negative: bool,
        /// Span of the pattern (including the minus sign, if any).
        span: Span,
    },
    /// Boolean literal pattern
    Bool(bool, Span),
    /// Path pattern for enum variants (e.g., `Color::Red` or `module.Color::Red`)
    Path {
        /// Optional module reference for qualified paths (e.g., the `module` in `module.Color::Red`)
        module: Option<InstRef>,
        /// Optional inline type-constructor call head — the instruction that
        /// reduces to the enum type at comptime for `F(args).Variant(..)`
        /// (RUE-596, preview `inline_type_ctor_paths`). When `Some`, the enum is
        /// the reduction of this head and `type_name` is only the constructor
        /// function's name; `None` for an ordinary `Enum.Variant` pattern.
        ctor_head: Option<InstRef>,
        /// The enum type name
        type_name: Spur,
        /// The variant name
        variant: Spur,
        /// Payload binding names for a tuple-variant pattern `Circle(r)`
        /// (RUE-221), in payload order. Empty for a discriminant-only pattern.
        bindings: Vec<Spur>,
        /// Span of the pattern
        span: Span,
    },
}

impl RirPattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            RirPattern::Wildcard(span) => *span,
            RirPattern::Int { span, .. } => *span,
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
/// Layout: [name: u32, ty: u32, mode: u32, is_comptime: u32,
///          span.file_id: u32, span.start: u32, span.end: u32] = 7 u32s per
/// param (must match `add_params`/`get_params`)
const PARAM_SIZE: u32 = 7;

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

/// Size of each pattern kind in the extra array (including body InstRef).
///
/// The span is stored as three words — start, len, AND file id — so pattern
/// diagnostics in multi-file compilations attribute to the right file
/// (dropping the file id here was why pattern-anchored errors reported
/// "span has an unknown file id", RUE-185).
const PATTERN_WILDCARD_SIZE: u32 = 5; // kind, span_start, span_len, span_file, body
const PATTERN_INT_SIZE: u32 = 8; // kind, span_start, span_len, span_file, value_lo, value_hi, negative, body
const PATTERN_BOOL_SIZE: u32 = 6; // kind, span_start, span_len, span_file, value, body
// Path patterns are variable-length (RUE-221): kind, span×3, module,
// type_name, variant, n_bindings, bindings…, body = 9 + n_bindings words.
// See `add_match_arms`/`get_match_arms` for the layout.

/// Stored representation of struct field initializer.
/// Layout: [field_name: u32, value: u32] = 2 u32s per field
const FIELD_INIT_SIZE: u32 = 2;

/// Stored representation of struct field declaration.
/// Layout: [field_name: u32, field_type: u32] = 2 u32s per field
const FIELD_DECL_SIZE: u32 = 2;

/// Stored representation of directive in the extra array.
/// Layout: [name: u32, span_start: u32, span_len: u32, args_len: u32, args...]
/// Variable size due to args.

/// The complete canonical RIR for one source revision.
#[derive(Debug, Default)]
pub struct Rir {
    /// All instructions across the canonical module sequence.
    instructions: Vec<Inst>,
    /// Extra data for variable-length instruction payloads.
    extra: Vec<u32>,
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

    /// The number of words in the variable-length payload store.
    #[inline]
    pub fn extra_len(&self) -> usize {
        self.extra.len()
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
            data.push(arg.mode.as_u32());
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
            let mode = RirArgMode::from_u32(chunk[1]);
            args.push(RirCallArg { value, mode });
        }
        args
    }

    /// Store RirParams and return (start, len).
    /// Layout: [name: u32, ty: u32, mode: u32, is_comptime: u32,
    ///          span.file_id: u32, span.start: u32, span.end: u32] per param
    pub fn add_params(&mut self, params: &[RirParam]) -> (u32, u32) {
        let mut data = Vec::with_capacity(params.len() * PARAM_SIZE as usize);
        for param in params {
            data.push(param.name.into_usize() as u32);
            data.push(param.ty.into_usize() as u32);
            data.push(param.mode.as_u32());
            data.push(param.is_comptime as u32);
            data.push(param.span.file_id.index());
            data.push(param.span.start);
            data.push(param.span.end);
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
            let mode = RirParamMode::from_u32(chunk[2]);
            let is_comptime = chunk[3] != 0;
            let span = Span::with_file(FileId::new(chunk[4]), chunk[5], chunk[6]);
            params.push(RirParam {
                name,
                ty,
                mode,
                is_comptime,
                span,
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
                    self.extra.push(span.file_id.index());
                    self.extra.push(body.as_u32());
                }
                RirPattern::Int {
                    value,
                    negative,
                    span,
                } => {
                    self.extra.push(PatternKind::Int as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(span.file_id.index());
                    // Store u64 magnitude as two u32s (little-endian) plus sign flag
                    self.extra.push(*value as u32);
                    self.extra.push((*value >> 32) as u32);
                    self.extra.push(u32::from(*negative));
                    self.extra.push(body.as_u32());
                }
                RirPattern::Bool(value, span) => {
                    self.extra.push(PatternKind::Bool as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(span.file_id.index());
                    self.extra.push(if *value { 1 } else { 0 });
                    self.extra.push(body.as_u32());
                }
                RirPattern::Path {
                    module,
                    ctor_head,
                    type_name,
                    variant,
                    bindings,
                    span,
                } => {
                    self.extra.push(PatternKind::Path as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(span.file_id.index());
                    // Store module as u32::MAX for None, otherwise the InstRef
                    self.extra.push(module.map_or(u32::MAX, |r| r.as_u32()));
                    // Store ctor_head (inline type-constructor pattern head,
                    // RUE-596) the same way — u32::MAX for None.
                    self.extra.push(ctor_head.map_or(u32::MAX, |r| r.as_u32()));
                    self.extra.push(type_name.into_usize() as u32);
                    self.extra.push(variant.into_usize() as u32);
                    // Variable-length payload bindings (RUE-221): a count
                    // followed by the binding symbols, then the body last.
                    self.extra.push(bindings.len() as u32);
                    for b in bindings {
                        self.extra.push(b.into_usize() as u32);
                    }
                    self.extra.push(body.as_u32());
                }
            }
        }
        (start, arms.len() as u32)
    }

    /// Decode a pattern's span from the extra array. Every pattern kind
    /// stores its span as [start, len, file_id] at offsets 1..=3 after the
    /// kind word (see the `PATTERN_*_SIZE` layout comments).
    fn decode_pattern_span(extra: &[u32], pos: usize) -> Span {
        let span_start = extra[pos + 1];
        let span_len = extra[pos + 2];
        let file_id = rue_span::FileId::new(extra[pos + 3]);
        Span::with_file(file_id, span_start, span_start + span_len)
    }

    /// Retrieve match arms from the extra array.
    pub fn get_match_arms(&self, start: u32, arm_count: u32) -> Vec<(RirPattern, InstRef)> {
        let mut arms = Vec::with_capacity(arm_count as usize);
        let mut pos = start as usize;

        for _ in 0..arm_count {
            let kind = self.extra[pos];
            match kind {
                k if k == PatternKind::Wildcard as u32 => {
                    let span = Self::decode_pattern_span(&self.extra, pos);
                    let body = InstRef::from_raw(self.extra[pos + 4]);
                    arms.push((RirPattern::Wildcard(span), body));
                    pos += PATTERN_WILDCARD_SIZE as usize;
                }
                k if k == PatternKind::Int as u32 => {
                    let span = Self::decode_pattern_span(&self.extra, pos);
                    let value_lo = self.extra[pos + 4] as u64;
                    let value_hi = self.extra[pos + 5] as u64;
                    let value = value_lo | (value_hi << 32);
                    let negative = self.extra[pos + 6] != 0;
                    let body = InstRef::from_raw(self.extra[pos + 7]);
                    arms.push((
                        RirPattern::Int {
                            value,
                            negative,
                            span,
                        },
                        body,
                    ));
                    pos += PATTERN_INT_SIZE as usize;
                }
                k if k == PatternKind::Bool as u32 => {
                    let span = Self::decode_pattern_span(&self.extra, pos);
                    let value = self.extra[pos + 4] != 0;
                    let body = InstRef::from_raw(self.extra[pos + 5]);
                    arms.push((RirPattern::Bool(value, span), body));
                    pos += PATTERN_BOOL_SIZE as usize;
                }
                k if k == PatternKind::Path as u32 => {
                    let span = Self::decode_pattern_span(&self.extra, pos);
                    // Decode module: u32::MAX means None
                    let module_raw = self.extra[pos + 4];
                    let module = if module_raw == u32::MAX {
                        None
                    } else {
                        Some(InstRef::from_raw(module_raw))
                    };
                    // Decode ctor_head (inline type-constructor pattern head,
                    // RUE-596): u32::MAX means None.
                    let ctor_head_raw = self.extra[pos + 5];
                    let ctor_head = if ctor_head_raw == u32::MAX {
                        None
                    } else {
                        Some(InstRef::from_raw(ctor_head_raw))
                    };
                    let type_name = Spur::try_from_usize(self.extra[pos + 6] as usize).unwrap();
                    let variant = Spur::try_from_usize(self.extra[pos + 7] as usize).unwrap();
                    // Variable-length payload bindings (RUE-221).
                    let n_bindings = self.extra[pos + 8] as usize;
                    let mut bindings = Vec::with_capacity(n_bindings);
                    for i in 0..n_bindings {
                        bindings
                            .push(Spur::try_from_usize(self.extra[pos + 9 + i] as usize).unwrap());
                    }
                    let body = InstRef::from_raw(self.extra[pos + 9 + n_bindings]);
                    arms.push((
                        RirPattern::Path {
                            module,
                            ctor_head,
                            type_name,
                            variant,
                            bindings,
                            span,
                        },
                        body,
                    ));
                    pos += 10 + n_bindings;
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
    /// Layout: [name: u32, span_start: u32, span_len: u32, span_file: u32, args_len: u32, args...] per directive
    ///
    /// The span is stored as three words — start, len, AND file id — so
    /// directive-anchored diagnostics in multi-file compilations attribute
    /// to the right file (dropping the file id here was the same loss shape
    /// as the RUE-185 pattern-span bug; RUE-189).
    pub fn add_directives(&mut self, directives: &[RirDirective]) -> (u32, u32) {
        let start = self.extra.len() as u32;
        for directive in directives {
            self.extra.push(directive.name.into_usize() as u32);
            self.extra.push(directive.span.start());
            self.extra.push(directive.span.len());
            self.extra.push(directive.span.file_id.index());
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
            let span_start = self.extra[pos + 1];
            let span_len = self.extra[pos + 2];
            let file_id = rue_span::FileId::new(self.extra[pos + 3]);
            let span = Span::with_file(file_id, span_start, span_start + span_len);
            let args_len = self.extra[pos + 4] as usize;
            pos += 5;

            let args: Vec<Spur> = (0..args_len)
                .map(|i| Spur::try_from_usize(self.extra[pos + i] as usize).unwrap())
                .collect();
            pos += args_len;

            directives.push(RirDirective { name, args, span });
        }
        directives
    }
}

/// A single RIR instruction.
#[derive(Debug, Clone)]
pub struct Inst {
    pub data: InstData,
    pub span: Span,
}

/// The repeat count of an array-repeat literal `[value; count]` (RUE-235).
///
/// The count is a compile-time constant: either an integer literal (`[0; 128]`)
/// or a name referring to a `const` / `comptime` value parameter (`[0; N]`).
/// Named counts are resolved to a concrete value during semantic analysis using
/// the same const-eval machinery as array-type lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatCount {
    /// A literal count, e.g. the `128` in `[0; 128]`.
    Literal(u64),
    /// A named count, e.g. the `N` in `[0; N]`.
    Named(Spur),
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

    /// Try/`?` propagation: `operand?` unwraps an `Option`, evaluating to the
    /// `Some` payload, and early-returns `None` from the enclosing function
    /// when the operand is `None` (RUE-6, ADR-0038). Sema lowers it to a
    /// discriminant match with an early `return` on the `None` arm; the
    /// enclosing function must itself return an `Option`.
    Try { operand: InstRef },

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
    ///
    /// `iter_borrow` names the collection variable held as a scoped shared
    /// borrow for the loop's duration when this loop is the desugaring of a
    /// `for` over a named variable (spec 4.8:26, RUE-233). Sema rejects
    /// mutation of that variable inside the body (E0428). `None` for a plain
    /// `loop {}` or a `for` over a temporary (which is unnameable, so
    /// unmutatable, in the body).
    InfiniteLoop {
        body: InstRef,
        iter_borrow: Option<Spur>,
    },

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

    /// Break: exits the innermost loop.
    /// `value` is a value operand (e.g. `break 42`); it is carried through
    /// for diagnostics but always rejected by sema - break does not carry a
    /// value (see spec 4.8).
    Break { value: Option<InstRef> },

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
        /// The receiver's passing mode when `has_self` is true (`Normal`
        /// by-value, `Borrow`, or `Inout`; RUE-15). Always `Normal` for
        /// associated functions and free functions.
        self_mode: RirParamMode,
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

    /// `@offset_of(T, field)` — the compile-time byte offset of `field` within
    /// struct type `T` (RUE-301). Carries a type argument (as an interned
    /// string, exactly like [`InstData::TypeIntrinsic`]) and the field name;
    /// neither is an `InstRef`, so this variant has no operands to renumber.
    OffsetOf {
        /// Type argument (as an interned string, e.g., "Point").
        type_arg: Spur,
        /// Field name whose offset is requested.
        field: Spur,
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
        /// True when this binding is a `for`-loop element binding, which reads
        /// the element by shared borrow (spec 4.8:26): the initializer is
        /// analyzed as a by-ref read (never a move-out of the collection, so a
        /// non-Copy element is fine, RUE-259) and a non-Copy binder is a
        /// non-owning borrow slot that drop elaboration must not drop (the
        /// collection retains ownership and drops the element).
        iter_elem: bool,
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
    /// Directives, fields, and methods are stored in the extra array.
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
        /// Index into extra data where method refs start
        methods_start: u32,
        /// Number of methods
        methods_len: u32,
    },

    /// Struct literal: creates a new struct instance
    /// Fields are stored in the extra array using add_field_inits/get_field_inits.
    StructInit {
        /// Optional module reference (for qualified struct literals like `module.Point { ... }`)
        /// If Some, the struct is looked up in the module's exports.
        module: Option<InstRef>,
        /// Optional inline type-constructor call head — the instruction that
        /// reduces to the struct type at comptime for `F(args) { ... }` (RUE-596,
        /// preview `inline_type_ctor_paths`). When `Some`, the struct type is
        /// the reduction of this head and `type_name` is only the constructor
        /// function's name (kept for diagnostics); `None` for `Name { ... }`.
        ctor_head: Option<InstRef>,
        /// Struct type name
        type_name: Spur,
        /// Index into extra data where fields start
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
        /// Span of the first field-init-shorthand field, if any (`P { x }`
        /// desugaring to `P { x: x }`, RUE-613, preview `field_init_shorthand`).
        /// `Some` iff at least one field used the shorthand; Sema uses it to gate
        /// the form behind its preview flag and to point the diagnostic. `None`
        /// when every field was written explicitly (`P { x: x }`).
        shorthand_span: Option<Span>,
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
        /// Index into extra data where the tuple-variant payloads start
        /// (RUE-221). The region is a self-describing flat sequence: for each
        /// variant in declaration order, a count `k` followed by `k`
        /// type-name symbols (as `Spur`s). A count of 0 means a
        /// discriminant-only variant. `payloads_len` is the total number of
        /// u32 words in the region (0 when no variant carries a payload).
        payloads_start: u32,
        /// Number of u32 words in the payloads region.
        payloads_len: u32,
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

    /// Array-repeat literal `[value; count]` (RUE-235): creates an array of
    /// `count` copies of a single `value`. `value` is evaluated once and its
    /// (Copy) result fills every slot. The count is a compile-time constant
    /// resolved during semantic analysis.
    ArrayRepeat {
        /// The value being repeated (evaluated once).
        value: InstRef,
        /// The repeat count (literal or named compile-time constant).
        count: RepeatCount,
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

    /// Anonymous enum type: an enum (sum) type used as a value expression
    /// (e.g., `enum { Some(T), None }` in comptime type construction). The
    /// enum analog of [`InstData::AnonStructType`]; enables generic sum types
    /// like `Option`/`Result` as comptime type functions (ADR-0038, RUE-6
    /// phase 2). Variant names and tuple-variant payloads are encoded exactly
    /// as in [`InstData::EnumDecl`].
    AnonEnumType {
        /// Index into extra data where variant name symbols start
        variants_start: u32,
        /// Number of variants
        variants_len: u32,
        /// Index into extra data where the tuple-variant payloads start,
        /// encoded as in [`InstData::EnumDecl`]: a self-describing flat
        /// sequence of `count` + `count` type-name symbols per variant.
        payloads_start: u32,
        /// Number of u32 words in the payloads region (0 when no payloads).
        payloads_len: u32,
    },
}

impl fmt::Display for InstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

struct DisplayedInstRef(u32);

impl fmt::Display for DisplayedInstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Printer for RIR that resolves symbols to their string values.
pub struct RirPrinter<'a, 'b> {
    rir: &'a Rir,
    interner: &'b lasso::ThreadedRodeo,
    instruction_order: Option<Vec<InstRef>>,
    displayed_refs: Option<Vec<u32>>,
    displayed_extra: Option<Vec<u32>>,
}

impl<'a, 'b> RirPrinter<'a, 'b> {
    /// Create a new RIR printer.
    pub fn new(rir: &'a Rir, interner: &'b lasso::ThreadedRodeo) -> Self {
        Self {
            rir,
            interner,
            instruction_order: None,
            displayed_refs: None,
            displayed_extra: None,
        }
    }

    /// Create a read-only presentation of `rir` in a different instruction order.
    ///
    /// The supplied order must be a permutation of every instruction in the RIR.
    /// References are displayed in that order without cloning or rewriting the RIR.
    pub fn with_instruction_order(
        rir: &'a Rir,
        interner: &'b lasso::ThreadedRodeo,
        instruction_order: Vec<InstRef>,
    ) -> Self {
        assert_eq!(instruction_order.len(), rir.len());
        let mut displayed_refs = vec![u32::MAX; rir.len()];
        for (displayed, canonical) in instruction_order.iter().enumerate() {
            let slot = &mut displayed_refs[canonical.as_u32() as usize];
            assert_eq!(
                *slot,
                u32::MAX,
                "RIR presentation order contains a duplicate"
            );
            *slot = displayed as u32;
        }
        assert!(
            displayed_refs
                .iter()
                .all(|displayed| *displayed != u32::MAX)
        );
        Self {
            rir,
            interner,
            instruction_order: Some(instruction_order),
            displayed_refs: Some(displayed_refs),
            displayed_extra: None,
        }
    }

    /// Create a presentation that remaps both instruction and payload ordering.
    pub fn with_presentation_order(
        rir: &'a Rir,
        interner: &'b lasso::ThreadedRodeo,
        instruction_order: Vec<InstRef>,
        extra_order: Vec<u32>,
    ) -> Self {
        let mut printer = Self::with_instruction_order(rir, interner, instruction_order);
        assert_eq!(extra_order.len(), rir.extra_len());
        let mut displayed_extra = vec![u32::MAX; rir.extra_len()];
        for (displayed, canonical) in extra_order.into_iter().enumerate() {
            let slot = &mut displayed_extra[canonical as usize];
            assert_eq!(
                *slot,
                u32::MAX,
                "RIR payload presentation contains a duplicate"
            );
            *slot = displayed as u32;
        }
        assert!(
            displayed_extra
                .iter()
                .all(|displayed| *displayed != u32::MAX)
        );
        printer.displayed_extra = Some(displayed_extra);
        printer
    }

    fn display_ref(&self, inst: InstRef) -> DisplayedInstRef {
        DisplayedInstRef(
            self.displayed_refs
                .as_ref()
                .map_or(inst.as_u32(), |refs| refs[inst.as_u32() as usize]),
        )
    }

    fn display_extra(&self, index: u32) -> u32 {
        self.displayed_extra
            .as_ref()
            .map_or(index, |extra| extra[index as usize])
    }

    /// Format a call argument with its mode prefix.
    fn format_call_arg(&self, arg: &RirCallArg) -> String {
        match arg.mode {
            RirArgMode::Inout => format!("inout {}", self.display_ref(arg.value)),
            RirArgMode::Borrow => format!("borrow {}", self.display_ref(arg.value)),
            RirArgMode::Normal => format!("{}", self.display_ref(arg.value)),
        }
    }

    /// Format a list of call arguments.
    fn format_call_args(&self, args: &[RirCallArg]) -> String {
        args.iter()
            .map(|arg| self.format_call_arg(arg))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Format an item's directives as a `"@copy @allow(..) "` prefix
    /// (empty string when there are none).
    fn format_directives(&self, start: u32, len: u32) -> String {
        let directives = self.rir.get_directives(start, len);
        if directives.is_empty() {
            return String::new();
        }
        let dir_names: Vec<String> = directives
            .iter()
            .map(|d| format!("@{}", self.interner.resolve(&d.name)))
            .collect();
        format!("{} ", dir_names.join(" "))
    }

    /// Format a pattern for printing.
    fn format_pattern(&self, pat: &RirPattern) -> String {
        match pat {
            RirPattern::Wildcard(_) => "_".to_string(),
            RirPattern::Int {
                value, negative, ..
            } => {
                if *negative {
                    format!("-{}", value)
                } else {
                    value.to_string()
                }
            }
            RirPattern::Bool(b, _) => b.to_string(),
            RirPattern::Path {
                module,
                type_name,
                variant,
                bindings,
                ..
            } => {
                let prefix = if let Some(module_ref) = module {
                    format!("{}..", self.display_ref(*module_ref))
                } else {
                    String::new()
                };
                let base = format!(
                    "{}{}::{}",
                    prefix,
                    self.interner.resolve(&*type_name),
                    self.interner.resolve(&*variant)
                );
                if bindings.is_empty() {
                    base
                } else {
                    let names: Vec<&str> =
                        bindings.iter().map(|b| self.interner.resolve(b)).collect();
                    format!("{}({})", base, names.join(", "))
                }
            }
        }
    }

    /// Format the RIR as a string.
    pub fn to_string(&self) -> String {
        use std::fmt::Write;

        let mut out = String::new();
        let instruction_order: Box<dyn Iterator<Item = InstRef> + '_> =
            match &self.instruction_order {
                Some(order) => Box::new(order.iter().copied()),
                None => Box::new(self.rir.iter().map(|(inst_ref, _)| inst_ref)),
            };
        for inst_ref in instruction_order {
            let inst = self.rir.get(inst_ref);
            write!(out, "{} = ", self.display_ref(inst_ref)).unwrap();
            match &inst.data {
                // Constants
                InstData::IntConst(v) => writeln!(out, "const {}", v).unwrap(),
                InstData::BoolConst(v) => writeln!(out, "const {}", v).unwrap(),
                InstData::StringConst(s) => {
                    writeln!(out, "const {:?}", self.interner.resolve(&*s)).unwrap()
                }
                InstData::UnitConst => writeln!(out, "const ()").unwrap(),

                // Binary operations
                InstData::Add { lhs, rhs } => writeln!(
                    out,
                    "add {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Sub { lhs, rhs } => writeln!(
                    out,
                    "sub {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Mul { lhs, rhs } => writeln!(
                    out,
                    "mul {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Div { lhs, rhs } => writeln!(
                    out,
                    "div {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Mod { lhs, rhs } => writeln!(
                    out,
                    "mod {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Eq { lhs, rhs } => writeln!(
                    out,
                    "eq {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Ne { lhs, rhs } => writeln!(
                    out,
                    "ne {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Lt { lhs, rhs } => writeln!(
                    out,
                    "lt {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Gt { lhs, rhs } => writeln!(
                    out,
                    "gt {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Le { lhs, rhs } => writeln!(
                    out,
                    "le {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Ge { lhs, rhs } => writeln!(
                    out,
                    "ge {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::And { lhs, rhs } => writeln!(
                    out,
                    "and {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Or { lhs, rhs } => writeln!(
                    out,
                    "or {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::BitAnd { lhs, rhs } => writeln!(
                    out,
                    "bit_and {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::BitOr { lhs, rhs } => writeln!(
                    out,
                    "bit_or {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::BitXor { lhs, rhs } => writeln!(
                    out,
                    "bit_xor {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Shl { lhs, rhs } => writeln!(
                    out,
                    "shl {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Shr { lhs, rhs } => writeln!(
                    out,
                    "shr {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),

                // Unary operations
                InstData::Neg { operand } => {
                    writeln!(out, "neg {}", self.display_ref(*operand)).unwrap()
                }
                InstData::Not { operand } => {
                    writeln!(out, "not {}", self.display_ref(*operand)).unwrap()
                }
                InstData::BitNot { operand } => {
                    writeln!(out, "bit_not {}", self.display_ref(*operand)).unwrap()
                }
                InstData::Try { operand } => {
                    writeln!(out, "try {}", self.display_ref(*operand)).unwrap()
                }

                // Control flow
                InstData::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    if let Some(else_b) = else_block {
                        writeln!(
                            out,
                            "branch {}, {}, {}",
                            self.display_ref(*cond),
                            self.display_ref(*then_block),
                            self.display_ref(*else_b)
                        )
                        .unwrap();
                    } else {
                        writeln!(
                            out,
                            "branch {}, {}",
                            self.display_ref(*cond),
                            self.display_ref(*then_block)
                        )
                        .unwrap();
                    }
                }
                InstData::Loop { cond, body } => writeln!(
                    out,
                    "loop {}, {}",
                    self.display_ref(*cond),
                    self.display_ref(*body)
                )
                .unwrap(),
                InstData::InfiniteLoop { body, iter_borrow } => {
                    let borrow_str = iter_borrow
                        .map(|c| format!(" borrows {}", self.interner.resolve(&c)))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "infinite_loop {}{}",
                        self.display_ref(*body),
                        borrow_str
                    )
                    .unwrap()
                }
                InstData::Match {
                    scrutinee,
                    arms_start,
                    arms_len,
                } => {
                    let arms = self.rir.get_match_arms(*arms_start, *arms_len);
                    let arms_str: Vec<String> = arms
                        .iter()
                        .map(|(pat, body)| {
                            format!(
                                "{} => {}",
                                self.format_pattern(pat),
                                self.display_ref(*body)
                            )
                        })
                        .collect();
                    writeln!(
                        out,
                        "match {} {{ {} }}",
                        self.display_ref(*scrutinee),
                        arms_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::Break { value } => match value {
                    Some(v) => writeln!(out, "break {}", self.display_ref(*v)).unwrap(),
                    None => writeln!(out, "break").unwrap(),
                },
                InstData::Continue => writeln!(out, "continue").unwrap(),

                // Functions
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
                    self_mode,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let unchecked_str = if *is_unchecked { "unchecked " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let ret_str = self.interner.resolve(&*return_type);
                    let self_str = if *has_self {
                        match self_mode {
                            RirParamMode::Inout => "inout self, ",
                            RirParamMode::Borrow => "borrow self, ",
                            RirParamMode::Normal => "self, ",
                        }
                    } else {
                        ""
                    };
                    let params = self.rir.get_params(*params_start, *params_len);
                    let params_str: Vec<String> = params
                        .iter()
                        .map(|p| {
                            let comptime_prefix = if p.is_comptime { "comptime " } else { "" };
                            let mode_prefix = match p.mode {
                                RirParamMode::Inout => "inout ",
                                RirParamMode::Borrow => "borrow ",
                                RirParamMode::Normal => "",
                            };
                            format!(
                                "{}{}{}: {}",
                                comptime_prefix,
                                mode_prefix,
                                self.interner.resolve(&p.name),
                                self.interner.resolve(&p.ty)
                            )
                        })
                        .collect();
                    let directives_str = self.format_directives(*directives_start, *directives_len);
                    writeln!(
                        out,
                        "{}{}{}fn {}({}{}) -> {} {{",
                        directives_str,
                        pub_str,
                        unchecked_str,
                        name_str,
                        self_str,
                        params_str.join(", "),
                        ret_str
                    )
                    .unwrap();
                    writeln!(out, "    {}", self.display_ref(*body)).unwrap();
                    writeln!(out, "}}").unwrap();
                }
                InstData::ConstDecl {
                    directives_start,
                    directives_len,
                    is_pub,
                    name,
                    ty,
                    init,
                } => {
                    let directives_str = self.format_directives(*directives_start, *directives_len);
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let ty_str = ty
                        .map(|t| format!(": {}", self.interner.resolve(&t)))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "{}{}const {}{} = {}",
                        directives_str,
                        pub_str,
                        name_str,
                        ty_str,
                        self.display_ref(*init)
                    )
                    .unwrap();
                }
                InstData::Ret(inner) => {
                    if let Some(inner) = inner {
                        writeln!(out, "ret {}", self.display_ref(*inner)).unwrap();
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
                    writeln!(out, "call {}({})", name_str, self.format_call_args(&args)).unwrap();
                }
                InstData::Intrinsic {
                    name,
                    args_start,
                    args_len,
                } => {
                    let name_str = self.interner.resolve(&*name);
                    let args = self.rir.get_inst_refs(*args_start, *args_len);
                    let args_str: Vec<String> = args
                        .iter()
                        .map(|a| self.display_ref(*a).to_string())
                        .collect();
                    writeln!(out, "intrinsic @{}({})", name_str, args_str.join(", ")).unwrap();
                }
                InstData::TypeIntrinsic { name, type_arg } => {
                    let name_str = self.interner.resolve(&*name);
                    let type_str = self.interner.resolve(&*type_arg);
                    writeln!(out, "type_intrinsic @{}({})", name_str, type_str).unwrap();
                }
                InstData::OffsetOf { type_arg, field } => {
                    let type_str = self.interner.resolve(&*type_arg);
                    let field_str = self.interner.resolve(&*field);
                    writeln!(out, "offset_of @offset_of({}, {})", type_str, field_str).unwrap();
                }
                InstData::Block { extra_start, len } => {
                    writeln!(out, "block({}, {})", self.display_extra(*extra_start), len).unwrap();
                }

                // Variables
                InstData::Alloc {
                    directives_start,
                    directives_len,
                    name,
                    is_mut,
                    ty,
                    init,
                    iter_elem,
                } => {
                    let directives_str = self.format_directives(*directives_start, *directives_len);
                    let name_str = name
                        .map(|n| self.interner.resolve(&n).to_string())
                        .unwrap_or_else(|| "_".to_string());
                    let mut_str = if *is_mut { "mut " } else { "" };
                    let ty_str = ty
                        .map(|t| format!(": {}", self.interner.resolve(&t)))
                        .unwrap_or_default();
                    let iter_str = if *iter_elem { " [iter_elem]" } else { "" };
                    writeln!(
                        out,
                        "{}alloc {}{}{}= {}{}",
                        directives_str,
                        mut_str,
                        name_str,
                        ty_str,
                        self.display_ref(*init),
                        iter_str
                    )
                    .unwrap();
                }
                InstData::VarRef { name } => {
                    writeln!(out, "var_ref {}", self.interner.resolve(&*name)).unwrap();
                }
                InstData::Assign { name, value } => {
                    writeln!(
                        out,
                        "assign {} = {}",
                        self.interner.resolve(&*name),
                        self.display_ref(*value)
                    )
                    .unwrap();
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
                    methods_start,
                    methods_len,
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
                    let linear_str = if *is_linear { "linear " } else { "" };
                    let directives_str = self.format_directives(*directives_start, *directives_len);
                    let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let methods_str = if methods.is_empty() {
                        String::new()
                    } else {
                        let method_refs: Vec<String> = methods
                            .iter()
                            .map(|m| self.display_ref(*m).to_string())
                            .collect();
                        format!(" methods: [{}]", method_refs.join(", "))
                    };
                    writeln!(
                        out,
                        "{}{}{}struct {} {{ {} }}{}",
                        directives_str,
                        pub_str,
                        linear_str,
                        name_str,
                        fields_str.join(", "),
                        methods_str
                    )
                    .unwrap();
                }
                InstData::StructInit {
                    module,
                    ctor_head,
                    type_name,
                    fields_start,
                    fields_len,
                    shorthand_span: _,
                } => {
                    let module_str = match ctor_head {
                        Some(head) => format!("<{}>.", self.display_ref(*head)),
                        None => module
                            .map(|m| format!("{}.", self.display_ref(m)))
                            .unwrap_or_default(),
                    };
                    let type_str = self.interner.resolve(&*type_name);
                    let fields = self.rir.get_field_inits(*fields_start, *fields_len);
                    let fields_str: Vec<String> = fields
                        .iter()
                        .map(|(fname, value)| {
                            format!(
                                "{}: {}",
                                self.interner.resolve(&*fname),
                                self.display_ref(*value)
                            )
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
                    writeln!(
                        out,
                        "field_get {}.{}",
                        self.display_ref(*base),
                        self.interner.resolve(&*field)
                    )
                    .unwrap();
                }
                InstData::FieldSet { base, field, value } => {
                    writeln!(
                        out,
                        "field_set {}.{} = {}",
                        self.display_ref(*base),
                        self.interner.resolve(&*field),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }

                // Enums
                InstData::EnumDecl {
                    is_pub,
                    name,
                    variants_start,
                    variants_len,
                    payloads_start,
                    payloads_len,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let variants = self.rir.get_symbols(*variants_start, *variants_len);
                    // Decode payloads (self-describing [k, t0..t_{k-1}] per variant).
                    let payload_words = self.rir.get_extra(*payloads_start, *payloads_len);
                    let mut payload_arities: Vec<usize> = Vec::new();
                    let mut pi = 0usize;
                    while pi < payload_words.len() {
                        let k = payload_words[pi] as usize;
                        payload_arities.push(k);
                        pi += 1 + k;
                    }
                    let variants_str: Vec<String> = variants
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let base = self.interner.resolve(&*v).to_string();
                            match payload_arities.get(i) {
                                Some(k) if *k > 0 => format!("{}/{}", base, k),
                                _ => base,
                            }
                        })
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
                    let module_str = module
                        .map(|m| format!("{}.", self.display_ref(m)))
                        .unwrap_or_default();
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
                    let elems_str: Vec<String> = elements
                        .iter()
                        .map(|e| self.display_ref(*e).to_string())
                        .collect();
                    writeln!(out, "array_init [{}]", elems_str.join(", ")).unwrap();
                }
                InstData::ArrayRepeat { value, count } => {
                    let count_str = match count {
                        RepeatCount::Literal(n) => n.to_string(),
                        RepeatCount::Named(sym) => {
                            format!("sym:{}", sym.into_usize())
                        }
                    };
                    writeln!(
                        out,
                        "array_repeat [{}; {}]",
                        self.display_ref(*value),
                        count_str
                    )
                    .unwrap();
                }
                InstData::IndexGet { base, index } => {
                    writeln!(
                        out,
                        "index_get {}[{}]",
                        self.display_ref(*base),
                        self.display_ref(*index)
                    )
                    .unwrap();
                }
                InstData::IndexSet { base, index, value } => {
                    writeln!(
                        out,
                        "index_set {}[{}] = {}",
                        self.display_ref(*base),
                        self.display_ref(*index),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }

                // Methods
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
                        self.display_ref(*receiver),
                        self.interner.resolve(&*method),
                        self.format_call_args(&args)
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
                    writeln!(out, "    {}", self.display_ref(*body)).unwrap();
                    writeln!(out, "}}").unwrap();
                }

                // Comptime block
                InstData::Comptime { expr } => {
                    writeln!(out, "comptime {{ {} }}", self.display_ref(*expr)).unwrap();
                }

                // Checked block
                InstData::Checked { expr } => {
                    writeln!(out, "checked {{ {} }}", self.display_ref(*expr)).unwrap();
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
                        let methods_str: Vec<String> = methods
                            .iter()
                            .map(|m| self.display_ref(*m).to_string())
                            .collect();
                        if !fields.is_empty() {
                            write!(out, ", ").unwrap();
                        }
                        write!(out, "methods: [{}]", methods_str.join(", ")).unwrap();
                    }
                    writeln!(out, " }}").unwrap();
                }

                // Anonymous enum type
                InstData::AnonEnumType {
                    variants_start,
                    variants_len,
                    payloads_start,
                    payloads_len,
                } => {
                    let variants = self.rir.get_symbols(*variants_start, *variants_len);
                    // Decode payloads (self-describing [k, t0..t_{k-1}] per variant).
                    let payload_words = self.rir.get_extra(*payloads_start, *payloads_len);
                    let mut payload_arities: Vec<usize> = Vec::new();
                    let mut pi = 0usize;
                    while pi < payload_words.len() {
                        let k = payload_words[pi] as usize;
                        payload_arities.push(k);
                        pi += 1 + k;
                    }
                    let variants_str: Vec<String> = variants
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let base = self.interner.resolve(&*v).to_string();
                            match payload_arities.get(i) {
                                Some(k) if *k > 0 => format!("{}/{}", base, k),
                                _ => base,
                            }
                        })
                        .collect();
                    writeln!(out, "enum {{ {} }}", variants_str.join(", ")).unwrap();
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
        let pattern = RirPattern::Int {
            value: 42,
            negative: false,
            span,
        };
        assert_eq!(pattern.span(), span);

        // Test negative int
        let pattern_neg = RirPattern::Int {
            value: 100,
            negative: true,
            span,
        };
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
            ctor_head: None,
            type_name,
            variant,
            bindings: Vec::new(),
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

    #[test]
    fn test_rir_call_arg_modes_round_trip() {
        let mut rir = Rir::new();
        let (args_start, args_len) = rir.add_call_args(&[
            RirCallArg {
                value: InstRef::from_raw(1),
                mode: RirArgMode::Normal,
            },
            RirCallArg {
                value: InstRef::from_raw(2),
                mode: RirArgMode::Inout,
            },
            RirCallArg {
                value: InstRef::from_raw(3),
                mode: RirArgMode::Borrow,
            },
        ]);

        let args = rir.get_call_args(args_start, args_len);
        assert_eq!(args.len(), 3);
        assert_eq!(args[0].value, InstRef::from_raw(1));
        assert_eq!(args[0].mode, RirArgMode::Normal);
        assert_eq!(args[1].value, InstRef::from_raw(2));
        assert_eq!(args[1].mode, RirArgMode::Inout);
        assert_eq!(args[2].value, InstRef::from_raw(3));
        assert_eq!(args[2].mode, RirArgMode::Borrow);
    }

    #[test]
    #[should_panic(expected = "invalid RirArgMode value: 99")]
    fn test_rir_call_arg_invalid_mode_panics() {
        let mut rir = Rir::new();
        let args_start = rir.add_extra(&[InstRef::from_raw(1).as_u32(), 99]);

        let _ = rir.get_call_args(args_start, 1);
    }

    #[test]
    fn test_rir_param_modes_round_trip() {
        let mut rir = Rir::new();
        let interner = ThreadedRodeo::new();
        let name = interner.get_or_intern("value");
        let ty = interner.get_or_intern("i32");
        let span = Span::new(3, 8);
        let modes = [
            RirParamMode::Normal,
            RirParamMode::Inout,
            RirParamMode::Borrow,
        ];
        let params: Vec<_> = modes
            .iter()
            .map(|&mode| RirParam {
                name,
                ty,
                mode,
                is_comptime: false,
                span,
            })
            .collect();

        let (params_start, params_len) = rir.add_params(&params);
        let decoded = rir.get_params(params_start, params_len);

        assert_eq!(decoded.len(), modes.len());
        assert_eq!(
            decoded.iter().map(|param| param.mode).collect::<Vec<_>>(),
            modes
        );
        assert_eq!(modes.map(RirParamMode::as_u32), [0, 1, 2]);
    }

    #[test]
    #[should_panic(expected = "invalid RirParamMode value: 3")]
    fn test_rir_param_old_comptime_mode_panics() {
        let mut rir = Rir::new();
        let params_start = rir.add_extra(&[0, 0, 3, 0, 0, 0, 0]);

        let _ = rir.get_params(params_start, 1);
    }

    #[test]
    #[should_panic(expected = "invalid RirParamMode value: 99")]
    fn test_rir_param_invalid_mode_panics() {
        let mut rir = Rir::new();
        let params_start = rir.add_extra(&[0, 0, 99, 0, 0, 0, 0]);

        let _ = rir.get_params(params_start, 1);
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
        let (mut rir, interner) = create_printer_test_rir();
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
        let (_, interner) = create_printer_test_rir();

        // Test all binary operations
        let ops = [
            "add", "sub", "mul", "div", "mod", "eq", "ne", "lt", "gt", "le", "ge", "and", "or",
            "bit_and", "bit_or", "bit_xor", "shl", "shr",
        ];

        for op_name in ops {
            let mut test_rir = Rir::new();
            let lhs = test_rir.add_inst(Inst {
                data: InstData::IntConst(1),
                span: Span::new(0, 1),
            });
            let rhs = test_rir.add_inst(Inst {
                data: InstData::IntConst(2),
                span: Span::new(2, 3),
            });
            // Create the op instruction with refs into this iteration's RIR
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
            data: InstData::InfiniteLoop {
                body,
                iter_borrow: None,
            },
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
            data: InstData::Break { value: None },
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
        let (mut rir, interner) = create_printer_test_rir();
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
            span: Span::default(),
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
                self_mode: RirParamMode::Normal,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("fn main(x: i32) -> i32"));
    }

    #[test]
    fn test_printer_fn_decl_with_self() {
        let (mut rir, interner) = create_printer_test_rir();
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
                self_mode: RirParamMode::Normal,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("fn get_x(self, ) -> i32"));
    }

    #[test]
    fn test_printer_fn_decl_param_modes() {
        let (mut rir, interner) = create_printer_test_rir();
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
                span: Span::default(),
            },
            RirParam {
                name: param2_name,
                ty: param2_type,
                mode: RirParamMode::Inout,
                is_comptime: false,
                span: Span::default(),
            },
            RirParam {
                name: param3_name,
                ty: param3_type,
                mode: RirParamMode::Borrow,
                is_comptime: false,
                span: Span::default(),
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
                self_mode: RirParamMode::Normal,
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
    fn test_printer_fn_decl_comptime_param() {
        let (mut rir, interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });
        let name = interner.get_or_intern("identity");
        let return_type = interner.get_or_intern("type");
        let param_name = interner.get_or_intern("T");
        let param_type = interner.get_or_intern("type");
        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (params_start, params_len) = rir.add_params(&[RirParam {
            name: param_name,
            ty: param_type,
            mode: RirParamMode::Normal,
            is_comptime: true,
            span: Span::default(),
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
                self_mode: RirParamMode::Normal,
            },
            span: Span::new(0, 40),
        });

        let output = RirPrinter::new(&rir, &interner).to_string();
        assert!(output.contains("fn identity(comptime T: type) -> type"));
    }

    #[test]
    fn test_printer_call() {
        let (mut rir, interner) = create_printer_test_rir();
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
        let (mut rir, interner) = create_printer_test_rir();
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
        let (mut rir, interner) = create_printer_test_rir();
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
        let (mut rir, interner) = create_printer_test_rir();
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
        let (mut rir, interner) = create_printer_test_rir();
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
                iter_elem: false,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc x: i32= %0"));
    }

    #[test]
    fn test_printer_alloc_mut() {
        let (mut rir, interner) = create_printer_test_rir();
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
                iter_elem: false,
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
                iter_elem: false,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc _= %0"));
    }

    #[test]
    fn test_printer_var_ref() {
        let (mut rir, interner) = create_printer_test_rir();
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
        let (mut rir, interner) = create_printer_test_rir();
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
        let (mut rir, interner) = create_printer_test_rir();
        let name = interner.get_or_intern("Point");
        let x_name = interner.get_or_intern("x");
        let y_name = interner.get_or_intern("y");
        let i32_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (fields_start, fields_len) =
            rir.add_field_decls(&[(x_name, i32_type), (y_name, i32_type)]);
        let (methods_start, methods_len) = rir.add_inst_refs(&[]);

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_linear: false,
                name,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct Point { x: i32, y: i32 }"));
    }

    #[test]
    fn test_printer_struct_decl_with_directive() {
        let (mut rir, interner) = create_printer_test_rir();
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
        let (methods_start, methods_len) = rir.add_inst_refs(&[]);

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_linear: false,
                name,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("@copy struct Point { x: i32 }"));
    }

    #[test]
    fn test_directive_span_round_trips_file_id() {
        // Directive spans must keep start, len, AND file id through the
        // extra-array encoding (RUE-189): dropping the file id made every
        // directive-anchored diagnostic in a multi-file build render
        // "unknown file id", and decoding len as end corrupted the range.
        let (mut rir, interner) = create_printer_test_rir();
        let copy_name = interner.get_or_intern("copy");
        let allow_name = interner.get_or_intern("allow");
        let arg = interner.get_or_intern("unused_variable");

        let original = vec![
            RirDirective {
                name: copy_name,
                args: vec![],
                span: Span::with_file(rue_span::FileId::new(2), 7, 12),
            },
            RirDirective {
                name: allow_name,
                args: vec![arg],
                span: Span::with_file(rue_span::FileId::new(3), 40, 62),
            },
        ];
        let (start, len) = rir.add_directives(&original);
        let decoded = rir.get_directives(start, len);
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_printer_struct_init() {
        let (mut rir, interner) = create_printer_test_rir();
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
                ctor_head: None,
                type_name,
                fields_start,
                fields_len,
                shorthand_span: None,
            },
            span: Span::new(0, 25),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct_init Point { x: %0, y: %1 }"));
    }

    #[test]
    fn test_printer_field_get() {
        let (mut rir, interner) = create_printer_test_rir();
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
        let (mut rir, interner) = create_printer_test_rir();
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
        let (mut rir, interner) = create_printer_test_rir();
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
                payloads_start: 0,
                payloads_len: 0,
            },
            span: Span::new(0, 35),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("enum Color { Red, Green, Blue }"));
    }

    #[test]
    fn test_printer_enum_variant() {
        let (mut rir, interner) = create_printer_test_rir();
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

    // Struct with methods tests
    #[test]
    fn test_printer_struct_decl_with_methods() {
        let (mut rir, interner) = create_printer_test_rir();

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
                self_mode: RirParamMode::Normal,
            },
            span: Span::new(0, 30),
        });

        let struct_name = interner.get_or_intern("Point");
        let x_field = interner.get_or_intern("x");
        let i32_type = interner.get_or_intern("i32");

        let (fields_start, fields_len) = rir.add_field_decls(&[(x_field, i32_type)]);
        let (methods_start, methods_len) = rir.add_inst_refs(&[method_ref]);

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_linear: false,
                name: struct_name,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct Point { x: i32 } methods: [%1]"));
    }

    #[test]
    fn test_printer_method_call() {
        let (mut rir, interner) = create_printer_test_rir();
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
        let (mut rir, interner) = create_printer_test_rir();
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
    fn test_printer_drop_fn_decl() {
        let (mut rir, interner) = create_printer_test_rir();
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
            (
                RirPattern::Int {
                    value: 1,
                    negative: false,
                    span: Span::new(0, 1),
                },
                body1,
            ),
            (
                RirPattern::Int {
                    value: 5,
                    negative: true,
                    span: Span::new(0, 2),
                },
                body2,
            ),
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
        let (mut rir, interner) = create_printer_test_rir();
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
                    ctor_head: None,
                    type_name: color,
                    variant: red,
                    bindings: Vec::new(),
                    span: Span::new(0, 10),
                },
                body_red,
            ),
            (
                RirPattern::Path {
                    module: None,
                    ctor_head: None,
                    type_name: color,
                    variant: green,
                    bindings: Vec::new(),
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
}
