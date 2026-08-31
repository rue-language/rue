//! Frames, outcomes, traps, patterns, and call admission records.

use super::*;

#[derive(Debug)]
pub struct ComptimeFrame<V, T, N, F, P, I> {
    pub program: P,
    pub body: InstRef,
    pub name: Option<N>,
    pub context: Option<F>,
    pub span: Span,
    pub function_span: Span,
    pub type_bindings: AHashMap<N, T>,
    pub value_bindings: AHashMap<N, V>,
    pub name_bindings: AHashMap<N, N>,
    pub call_identity: Option<I>,
    pub expected_result: Option<T>,
}

impl<V, T, N, F, P, I> ComptimeFrame<V, T, N, F, P, I> {
    pub fn expression(program: P, body: InstRef) -> Self {
        Self {
            program,
            body,
            name: None,
            context: None,
            span: Span::new(0, 0),
            function_span: Span::new(0, 0),
            type_bindings: AHashMap::new(),
            value_bindings: AHashMap::new(),
            name_bindings: AHashMap::new(),
            call_identity: None,
            expected_result: None,
        }
    }

    /// Create a ticket-free root for a callable body whose canonical identity
    /// was already established by the surrounding query. Keeping that root on
    /// the explicit frame stack makes its depth contribution structural.
    pub fn callable_body(program: P, body: InstRef, call_identity: I) -> Self {
        Self {
            call_identity: Some(call_identity),
            ..Self::expression(program, body)
        }
    }
}

#[derive(Debug)]
pub enum ComptimeCallPreparation<V, T, N, File, P, I, Failure, K> {
    /// A completed fact from the evaluation-local memo. This includes
    /// not-ready/runtime-dependent facts and therefore must not be confused
    /// with a cache miss.
    Memoized(ComptimeOutcome<V, Failure>),
    /// A cache miss represented by an owned foreign frame. The engine enters
    /// it and evaluates it; hosts never recursively dispatch its RIR.
    Enter {
        frame: ComptimeFrame<V, T, N, File, P, I>,
        ticket: K,
    },
}

#[derive(Debug)]
pub enum ComptimeOutcome<V, F> {
    Known(V),
    RuntimeDependent,
    NotReady,
    UnsupportedContext,
    Trap(ComptimeTrap),
    HostFailure(F),
    Abort(F),
}

/// A fact produced by the canonical evaluator for a control-flow selector.
/// The index is the source-order arm index, rather than an instruction
/// reference, so the fact remains valid for both inference and semantic AIR
/// emission without exposing the evaluator's traversal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeSelection {
    Branch { taken: bool },
    Match { arm: usize },
}

impl<V, F> ComptimeOutcome<V, F> {
    pub(crate) fn into_result(self, trap: impl FnOnce(ComptimeTrap) -> F) -> Result<Option<V>, F> {
        match self {
            Self::Known(value) => Ok(Some(value)),
            Self::RuntimeDependent | Self::NotReady | Self::UnsupportedContext => Ok(None),
            Self::Trap(value) => Err(trap(value)),
            Self::HostFailure(error) | Self::Abort(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComptimeTrap {
    pub operation: &'static str,
    pub span: Span,
}

pub type ComptimeArgMode = (rue_rir::RirArgMode, Span);

/// A match pattern decoded by the canonical AIR engine into semantic facts.
/// The compact RIR representation, including symbol handles and instruction
/// references used by qualified paths, never crosses the host boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComptimeMatchPattern<N> {
    Wildcard,
    Bool(bool),
    Integer(i128),
    Path {
        module_qualified: bool,
        ctor_qualified: bool,
        type_name: N,
        variant: N,
        binding_count: usize,
    },
}

/// A semantic reason why the canonical engine cannot reduce an expression.
/// The ordinary host maps every reason to runtime dependence; a durable host
/// can preserve the declaration-time failure associated with the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeIntegerOperation {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Gt,
    Le,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeUnaryOperation {
    Neg,
    BitNot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComptimeSemanticRejection<V> {
    ConditionNotBoolean(V),
    ArithmeticOperandNotInteger {
        operation: ComptimeIntegerOperation,
        lhs: V,
        rhs: Option<V>,
    },
    UnaryOperandNotInteger(V),
    UnaryTypeNotInteger {
        operation: ComptimeUnaryOperation,
        value: V,
    },
    Assignment,
    AggregateExpression,
    EmptyBlock,
    UnsupportedIntrinsic(String),
    UnsupportedExpression,
}

/// Decode one compact RIR pattern into semantic facts.  Callers supply the
/// owning-program symbol mapping; the pattern itself is never exposed beyond
/// this canonical decoder.
pub fn decode_comptime_match_pattern<N>(
    pattern: &rue_rir::RirPatternView<'_>,
    mut name_from_symbol: impl FnMut(SymbolHandle) -> N,
) -> ComptimeMatchPattern<N> {
    match pattern {
        rue_rir::RirPatternView::Wildcard(_) => ComptimeMatchPattern::Wildcard,
        rue_rir::RirPatternView::Bool(value, _) => ComptimeMatchPattern::Bool(*value),
        rue_rir::RirPatternView::Int {
            value, negative, ..
        } => ComptimeMatchPattern::Integer(if *negative {
            -(*value as i128)
        } else {
            *value as i128
        }),
        rue_rir::RirPatternView::Path {
            module,
            ctor_head,
            type_name,
            variant,
            bindings,
            ..
        } => ComptimeMatchPattern::Path {
            module_qualified: module.is_some(),
            ctor_qualified: ctor_head.is_some(),
            type_name: name_from_symbol((*type_name).into()),
            variant: name_from_symbol((*variant).into()),
            binding_count: bindings.len(),
        },
    }
}

/// An already-evaluated call argument together with the engine-derived fact
/// that its source node was an immediate `UnitConst`. The source instruction
/// and owning program remain engine-private; hosts receive only this semantic
/// provenance alongside the reduced value.
#[derive(Debug, Clone)]
pub struct ComptimeCallArgument<V> {
    pub(super) value: V,
    pub(super) direct_unit_literal: bool,
}

impl<V> ComptimeCallArgument<V> {
    pub fn value(&self) -> &V {
        &self.value
    }

    pub fn is_direct_unit_literal(&self) -> bool {
        self.direct_unit_literal
    }

    pub(super) fn new(value: V, direct_unit_literal: bool) -> Self {
        Self {
            value,
            direct_unit_literal,
        }
    }
}

pub struct ComptimeCallAdmission<A, N> {
    pub name: N,
    pub payload: A,
}
