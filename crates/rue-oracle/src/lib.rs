//! # rue-oracle — the executable reference semantics
//!
//! A tree-walking interpreter over the compiler's **CFG** (the typed
//! control-flow IR, produced by `CompilerSession`, before MIR/codegen). The
//! native differential retains the canonical post-transform O0 observation,
//! while the CFG-boundary differential also executes the raw CFG artifact.
//! This keeps mandatory CFG transformations out of the shared trusted base.
//! Running a program through this interpreter and through the compiled binary
//! and comparing the observable behavior (exit code, stdout, stderr, trap cause)
//! is the differential-testing oracle of RUE-50, and the executable form of the
//! operational semantics in `docs/formal/01-core-calculus.md` §6.
//!
//! Because the interpreter shares parsing, semantic analysis, and raw CFG
//! construction with the compiler, the paired comparisons localize different
//! regions without adding a second builder: raw/post CFG disagreement identifies
//! a mandatory CFG transformation, while post-transform/native disagreement
//! identifies MIR lowering, code generation, or runtime execution.
//!
//! ## Coverage
//!
//! Scalars (all integer widths + `bool` + `unit`), the full arithmetic /
//! comparison / bitwise / shift operator set with **trapping** overflow, locals,
//! parameters, calls and recursion, block-parameter control flow (`if`/`match`/
//! `loop` all lower to `Goto`/`Branch`/`Switch`), `@dbg`, `@intCast`, `@panic`,
//! `@assert`, and the defined panics (overflow, divide/remainder-by-zero,
//! int-cast overflow);
//! aggregates (structs/arrays) with nested place projections and bounds traps;
//! `inout`/`borrow` parameters (copy-in / copy-out, with the copy-in taken at
//! the moment the callee receives the argument rather than at the argument's
//! position in the list, so an alias mutated later in the same argument list is
//! observed — see [`Interp::reread_by_ref_operand`]); and drop/destructors,
//! executed in spec-3.9 order (user destructor, then fields in declaration
//! order / elements ascending) so the oracle validates drop *order* and
//! drop-exactly-once, not just final values; and text values — source-defined
//! `StrBuf` algorithms, `@to_string`, core `str` byte indexing, and
//! strict/lossy scalar iteration.
//! Text content is modeled as raw bytes, matching the runtime's packed view:
//! strict `.chars()` traps on invalid UTF-8, while `.chars_lossy()` substitutes
//! U+FFFD.
//!
//! Note that many intrinsics never reach the interpreter *as* intrinsics because
//! sema lowers them earlier: `@target_arch`/`@target_os` fold to a compile-time
//! `EnumVariant` against `Target::host()` (the oracle runs on that same host, so
//! the discriminant it evaluates agrees with the compiled binary); `@to_string`,
//! byte indexing, and `.chars()` become `Call`s; `@intCast` becomes `IntCast`.
//! Those are all covered through the resulting instruction paths.
//!
//! ## Typed incomplete execution
//!
//! [`Unsupported`] is not an opaque skip: every producer supplies a closed
//! [`UnsupportedKind`]. [`Unsupported::model_gap`] identifies the semantic,
//! external-dependency, and implementation-defined cases that a corpus may
//! explicitly register as coverage debt. Resource limits and compiler/oracle
//! contract violations are deliberately not registrable and must fail closed.
//! Generated-program mode has the stronger promise that no `Unsupported` kind
//! is valid and reports every one as a generator-contract failure.
//!
//! - **CFG intrinsics and runtime output.** The `Intrinsic` arm models `@dbg`,
//!   `@panic`, `@assert`, compiler-inserted slice bounds checks, deterministic
//!   heap/raw-pointer representation paths, and the exact target `write(1, ..)`
//!   syscall shape. Nondeterministic input and host effects (`@read_line`,
//!   `@random_*`, and all other `@syscall` calls) remain typed gaps, as do
//!   unsupported layout and external-effect boundaries. String print/println
//!   runtime calls append to the same ordered fd-1 byte trace as modeled writes.
//! - **`String::capacity`/`reserve`/`with_capacity` capacity behavior** — the
//!   exact capacity value is implementation-defined, so `capacity` is reported
//!   as a typed gap rather than guessed. Specified bounds and growth/preservation
//!   relationships can still be modeled independently.
//! - **Deeply-nested `inout` field writes** (non-zero inner offset).

use lasso::ThreadedRodeo;
use rue_air::{FrozenTypeInternPool, LayoutKind, RuntimeCallKind, Type, TypeKind};
use rue_cfg::{Cfg, CfgArgMode, CfgInstData, CfgValue, Place, PlaceBase, Projection, Terminator};
use rue_compiler::{
    CompileErrors, CompileOptions, CompilerSession, PreviewFeatures, SourceSnapshot,
};
use rue_runtime_abi::RuntimeTarget;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CompileState {
    functions: Vec<OracleFunction>,
    current_function: AtomicUsize,
}

struct OracleFunction {
    /// The declaration's source name, for ordinary definitions. A CFG's
    /// `fn_name` is the internal symbol, which an ordinary definition
    /// qualifies by module (RUE-1125); interpretation dispatches on that
    /// symbol, so only in-tree probes that name a function the way its source
    /// does need this.
    #[cfg(test)]
    source_name: Option<String>,
    cfg: rue_cfg::ValidatedCfg,
    interner: Arc<ThreadedRodeo>,
    type_pool: FrozenTypeInternPool,
    strings: Arc<[String]>,
}

#[cfg(test)]
impl CompileState {
    fn select_source_function(&self, name: &str) -> usize {
        let index = self
            .functions
            .iter()
            .position(|function| function.is_source_named(name))
            .unwrap_or_else(|| panic!("oracle test program has no {name} CFG"));
        self.current_function.store(index, Ordering::Relaxed);
        index
    }

    fn selected_function(&self) -> &OracleFunction {
        &self.functions[self.current_function.load(Ordering::Relaxed)]
    }

    fn type_pool(&self) -> &FrozenTypeInternPool {
        &self.selected_function().type_pool
    }

    fn interner(&self) -> &ThreadedRodeo {
        &self.selected_function().interner
    }
}

#[cfg(test)]
impl OracleFunction {
    fn is_source_named(&self, name: &str) -> bool {
        self.source_name.as_deref() == Some(name)
    }
}

fn query_cfg_state(source: &str) -> Result<CompileState, CompileErrors> {
    query_cfg_state_with_options(source, &CompileOptions::default())
}

fn query_cfg_state_with_preview_features(
    source: &str,
    preview_features: &PreviewFeatures,
) -> Result<CompileState, CompileErrors> {
    query_cfg_state_with_options(
        source,
        &CompileOptions {
            preview_features: preview_features.clone(),
            ..CompileOptions::default()
        },
    )
}

fn query_cfg_state_with_options(
    source: &str,
    options: &CompileOptions,
) -> Result<CompileState, CompileErrors> {
    let snapshot = SourceSnapshot::single("<oracle>", source).map_err(CompileErrors::from)?;
    query_cfg_state_from_snapshot(&snapshot, options)
}

fn query_cfg_state_from_snapshot(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> Result<CompileState, CompileErrors> {
    let mut session = CompilerSession::new();
    session.update(snapshot).into_result()?;
    query_cfg_state_from_session(session, options)
}

fn query_cfg_state_from_session(
    mut session: CompilerSession,
    options: &CompileOptions,
) -> Result<CompileState, CompileErrors> {
    query_cfg_state_from_session_at_stage(&mut session, options, CfgStage::PostOptimization)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CfgStage {
    PreOptimization,
    PostOptimization,
}

fn query_cfg_state_from_session_at_stage(
    session: &mut CompilerSession,
    options: &CompileOptions,
    stage: CfgStage,
) -> Result<CompileState, CompileErrors> {
    let functions = match stage {
        CfgStage::PreOptimization => {
            let rooted = rue_compiler::unstable::rooted_pre_optimization_cfg(session, options)?;
            rooted
                .functions()
                .iter()
                .map(|function| OracleFunction {
                    #[cfg(test)]
                    source_name: function.definition_source_name().map(str::to_owned),
                    cfg: function.cfg().clone(),
                    interner: function.interner().clone(),
                    type_pool: function.type_pool().clone(),
                    strings: function.strings().clone(),
                })
                .collect::<Vec<_>>()
        }
        CfgStage::PostOptimization => {
            let rooted = rue_compiler::unstable::rooted_cfg(session, options)?;
            rooted
                .functions()
                .iter()
                .map(|function| OracleFunction {
                    #[cfg(test)]
                    source_name: function.definition_source_name().map(str::to_owned),
                    cfg: function.cfg().clone(),
                    interner: function.interner().clone(),
                    type_pool: function.type_pool().clone(),
                    strings: function.strings().clone(),
                })
                .collect::<Vec<_>>()
        }
    };
    let entry = functions
        .iter()
        .position(|function| function.cfg.fn_name() == "main")
        .expect("successful oracle compilation publishes a main CFG");
    Ok(CompileState {
        functions,
        current_function: AtomicUsize::new(entry),
    })
}

/// A modeled oracle trap category.
///
/// The runtime-handled variants exit with code 101. [`TrapKind::Unreachable`]
/// instead records a defensive CFG condition whose native counterpart normally
/// faults. Keeping the category typed lets differential callers distinguish
/// programs that terminate alike but trap for different semantic reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrapKind {
    ArithmeticOverflow,
    DivisionByZero,
    IntegerCastOverflow,
    IndexOutOfBounds,
    InvalidUtf8,
    UserPanic,
    AssertionFailure,
    Unreachable,
}

impl fmt::Display for TrapKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ArithmeticOverflow => "arithmetic overflow",
            Self::DivisionByZero => "division by zero",
            Self::IntegerCastOverflow => "integer cast overflow",
            Self::IndexOutOfBounds => "index out of bounds",
            Self::InvalidUtf8 => "invalid UTF-8",
            Self::UserPanic => "user panic",
            Self::AssertionFailure => "assertion failure",
            Self::Unreachable => "reached unreachable",
        })
    }
}

/// Observable result of running a program under the oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Process exit code (the OS masks the returned `i32` to a `u8`).
    pub exit_code: i32,
    /// Everything fd 1 received, in order, decoded lossily at the public
    /// observation boundary.
    pub stdout: String,
    /// The exact raw bytes observed on fd 1. Differential callers that need
    /// byte identity must compare this field, rather than the lossy display
    /// string above.
    pub stdout_bytes: Vec<u8>,
    /// Everything the modeled runtime wrote to stderr, decoded with the same
    /// lossy UTF-8 boundary as the native differential runner so the resulting
    /// observation remains exactly comparable.
    pub stderr: String,
    /// `Some(kind)` if execution ended in a modeled trap, `None` on normal
    /// completion. Runtime-handled kinds mirror the runtime's trap categories.
    pub panic: Option<TrapKind>,
}

/// Maximum number of raw bytes accepted from a program's `@dbg` output.
///
/// Both the interpreter and the native differential runner use this shared
/// limit so neither side can exhaust harness memory through retained stdout or
/// reject output that the other side accepts. Output at or below the limit
/// remains exact; crossing it is surfaced explicitly and never accepted by
/// comparing a truncated prefix.
pub const MAX_STDOUT_BYTES: usize = 256 * 1024;

/// Maximum size of a raw `write(1, ...)` observation that the differential
/// oracle treats as deterministic.  The bound is the POSIX minimum `PIPE_BUF`:
/// Within the controlled blocking stdout capture sink, the native runner drains
/// concurrently and does not deliver signals to the child; combined with the
/// POSIX atomicity guarantee for writes of this size or less, that gives a
/// stable byte trace. Larger writes are retained as external syscall
/// dependencies rather than guessing about an OS short write.
pub const MAX_MODELED_STDOUT_WRITE_BYTES: usize = 512;

/// Maximum number of raw bytes accepted from modeled runtime stderr.
///
/// Rue currently writes stderr only when terminating in a runtime trap. The
/// bound matches the native differential runner's retained stderr limit so a
/// long user panic cannot consume unbounded harness memory or manufacture
/// agreement from a retained prefix.
pub const MAX_STDERR_BYTES: usize = 8 * 1024;

/// The policy class of a typed oracle execution failure.
///
/// Corpus callers may defer genuine semantic, external-dependency, or
/// implementation-defined gaps. Resource exhaustion and contract violations
/// are never ordinary coverage gaps: they mean the oracle could not complete
/// its promised judgment safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnsupportedClass {
    /// Rue semantics that the interpreter has not implemented yet.
    SemanticGap,
    /// Execution depends on input or host behavior unavailable to the oracle.
    ExternalDependency,
    /// Rue deliberately leaves the exact observation implementation-defined.
    ImplementationDefined,
    /// A bounded oracle resource was exhausted.
    ResourceLimit,
    /// A compiler/oracle contract that should hold for valid CFG was broken.
    ContractViolation,
}

/// A user-facing intrinsic whose deterministic semantics are not modeled yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnsupportedIntrinsicKind {
    ParseI32,
    ParseI64,
    ParseU32,
    ParseU64,
    PointerRead,
    PointerWrite,
    PointerOffset,
    PointerToInt,
    IntToPointer,
    EmptySlicePointer,
    RawAddress,
    RawMutableAddress,
    FieldPointer,
    Allocate,
    AllocateZeroed,
    Free,
    Reallocate,
    Resize,
    ByteCopy,
    ByteMove,
    ByteSet,
    IntToFloat,
    FloatToInt,
    FloatCast,
}

/// A compiler-emitted runtime call whose deterministic semantics are not
/// modeled yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnsupportedRuntimeCallKind {
    Print,
    Println,
}

/// A deterministic semantic feature missing from the interpreter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticGapKind {
    Intrinsic(UnsupportedIntrinsicKind),
    RuntimeCall(UnsupportedRuntimeCallKind),
    FlattenedParameterSlot,
    TextProjectionRead,
    FloatArithmetic,
}

/// A dependency whose value comes from outside deterministic Rue semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExternalDependencyKind {
    StandardInput,
    RandomU32,
    RandomU64,
    SystemCall,
    /// `@arg_count` — the process argument count captured at entry (RUE-935).
    ArgCount,
    /// `@arg_ptr` — a pointer into the loader-owned argv vector (RUE-935).
    ArgPtr,
    /// `@arg_len` — the byte length of a captured argv entry (RUE-935).
    ArgLen,
    /// `@env_count` — the process environment entry count (RUE-935).
    EnvCount,
    /// `@env_ptr` — a pointer into the loader-owned envp vector (RUE-935).
    EnvPtr,
    /// `@env_len` — the byte length of a captured envp entry (RUE-935).
    EnvLen,
}

/// An observation for which Rue does not specify one exact value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImplementationDefinedKind {
    /// The exact capacity value, as distinct from specified capacity bounds and
    /// preservation/growth relationships.
    StringCapacityValue,
}

/// A registrable oracle coverage gap.
///
/// Corpus registries use this type rather than [`UnsupportedKind`], making
/// resource limits and contract violations impossible to register as accepted
/// debt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelGapKind {
    Semantic(SemanticGapKind),
    ExternalDependency(ExternalDependencyKind),
    ImplementationDefined(ImplementationDefinedKind),
}

/// A bounded interpreter resource that was exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceLimitKind {
    StdoutBytes,
    StderrBytes,
    RecursionDepth,
    InterpreterSteps,
}

/// A compiler/oracle invariant that valid CFG is expected to satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContractViolationKind {
    MissingFunctionBody,
    BlockArgumentArity,
    MissingTerminator,
    BuiltinArity,
    BuiltinArgumentType,
    BuiltinArgumentMode,
    BuiltinResultType,
    RuntimeCallArity,
    RuntimeCallSignature,
    IntrinsicArity,
    IntrinsicSignature,
    StringConstantIndex,
    CallParameterLayout,
    ParameterSlotOutOfBounds,
    UnboundBlockParameter,
    EnumPayloadProjection,
    UnexpectedIntrinsic,
    DebugArity,
    IntCastTargetType,
    UninitializedLocal,
    PlaceBaseOutOfBounds,
    PlaceBaseNotWritable,
    NonAggregateProjectionRead,
    NonAggregateProjectionWrite,
    PlaceProjectionMetadata,
    InoutArgumentNotLvalue,
    NonIntegerOperationType,
    UnsupportedDebugType,
    UnsplicedAccessor,
}

/// The closed, machine-readable cause of an oracle execution failure.
///
/// Dynamic names, indices, and limits live only in [`Unsupported::detail`]; no
/// caller needs to parse display text to decide policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnsupportedKind {
    SemanticGap(SemanticGapKind),
    ExternalDependency(ExternalDependencyKind),
    ImplementationDefined(ImplementationDefinedKind),
    ResourceLimit(ResourceLimitKind),
    ContractViolation(ContractViolationKind),
}

impl UnsupportedKind {
    /// Return the exhaustive policy class for this cause.
    pub const fn class(self) -> UnsupportedClass {
        match self {
            Self::SemanticGap(_) => UnsupportedClass::SemanticGap,
            Self::ExternalDependency(_) => UnsupportedClass::ExternalDependency,
            Self::ImplementationDefined(_) => UnsupportedClass::ImplementationDefined,
            Self::ResourceLimit(_) => UnsupportedClass::ResourceLimit,
            Self::ContractViolation(_) => UnsupportedClass::ContractViolation,
        }
    }

    /// Return the registrable gap, or `None` for a hard resource/contract
    /// failure.
    pub const fn model_gap(self) -> Option<ModelGapKind> {
        match self {
            Self::SemanticGap(kind) => Some(ModelGapKind::Semantic(kind)),
            Self::ExternalDependency(kind) => Some(ModelGapKind::ExternalDependency(kind)),
            Self::ImplementationDefined(kind) => Some(ModelGapKind::ImplementationDefined(kind)),
            Self::ResourceLimit(_) | Self::ContractViolation(_) => None,
        }
    }
}

/// A typed reason the interpreter could not produce an observable outcome.
///
/// The kind is stable and drives policy. The detail is diagnostic-only and may
/// contain a dynamic name, index, type, or configured limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    kind: UnsupportedKind,
    detail: String,
}

impl Unsupported {
    fn new(kind: UnsupportedKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// Return the closed cause; callers should match this instead of display
    /// text.
    pub const fn kind(&self) -> UnsupportedKind {
        self.kind
    }

    /// Return diagnostic context for humans. This is not a policy key.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Return the broad policy class for this cause.
    pub const fn class(&self) -> UnsupportedClass {
        self.kind.class()
    }

    /// Return the registrable model gap, if this failure is eligible for a
    /// corpus coverage registry.
    pub const fn model_gap(&self) -> Option<ModelGapKind> {
        self.kind.model_gap()
    }
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for Unsupported {}

/// Failure to compile or interpret a source program under the oracle.
///
/// These variants deliberately distinguish a front-end rejection from a
/// well-typed program that reaches a construct outside the interpreter's
/// coverage. Callers must choose an explicit policy for each case instead of
/// inferring it from an error-message prefix.
#[derive(Debug, Clone)]
pub enum RunSourceError {
    /// Rue's front end rejected the source before interpretation could begin.
    Compile(CompileErrors),
    /// The source compiled, but interpretation ended with a typed model gap,
    /// resource limit, or compiler/oracle contract violation.
    Unsupported(Unsupported),
    /// The canonical pre- and post-optimization CFGs produced different
    /// observable behavior under the same interpreter.
    CfgTransformationDisagreement {
        pre_optimization: Result<Outcome, Unsupported>,
        post_optimization: Result<Outcome, Unsupported>,
    },
}

impl fmt::Display for RunSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunSourceError::Compile(errors) => write!(f, "source failed to compile: {errors}"),
            RunSourceError::Unsupported(unsupported) => {
                write!(f, "unsupported by the oracle: {unsupported}")
            }
            RunSourceError::CfgTransformationDisagreement {
                pre_optimization,
                post_optimization,
            } => write!(
                f,
                "pre/post CFG execution disagreement: pre={pre_optimization:?}, post={post_optimization:?}"
            ),
        }
    }
}

impl std::error::Error for RunSourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunSourceError::Compile(errors) => Some(errors),
            RunSourceError::Unsupported(unsupported) => Some(unsupported),
            RunSourceError::CfgTransformationDisagreement { .. } => None,
        }
    }
}

/// Compile `source` to CFG and run it under the reference semantics.
///
/// [`RunSourceError::Compile`] means the front end rejected the source;
/// [`RunSourceError::Unsupported`] means compilation succeeded but interpretation
/// could not produce an outcome; inspect [`Unsupported::kind`] rather than
/// parsing its diagnostic text.
pub fn run_source(source: &str) -> Result<Outcome, RunSourceError> {
    let state = query_cfg_state(source).map_err(RunSourceError::Compile)?;
    run_state(state).map_err(RunSourceError::Unsupported)
}

/// Compile `source` with explicit preview features, then run it under the
/// reference semantics.
///
/// This is the preview-aware form of [`run_source`]. It lets differential
/// harnesses opt individual cases into the same preview gates the compiler CLI
/// uses, while keeping the default oracle API stable-only.
pub fn run_source_with_preview_features(
    source: &str,
    preview_features: &PreviewFeatures,
) -> Result<Outcome, RunSourceError> {
    let state = query_cfg_state_with_preview_features(source, preview_features)
        .map_err(RunSourceError::Compile)?;
    run_state(state).map_err(RunSourceError::Unsupported)
}

/// Execute both canonical CFG boundaries and require identical observations.
/// The returned outcome is the raw-CFG result; the comparison side runs the
/// existing post-transform O0 pipeline without compiling or executing an
/// additional native artifact.
pub fn run_source_with_cfg_differential(
    source: &str,
    preview_features: &PreviewFeatures,
) -> Result<Outcome, RunSourceError> {
    let snapshot = SourceSnapshot::single("<oracle>", source)
        .map_err(CompileErrors::from)
        .map_err(RunSourceError::Compile)?;
    let mut session = CompilerSession::new();
    session
        .update(&snapshot)
        .into_result()
        .map_err(RunSourceError::Compile)?;
    run_session_cfg_differential_inner(&mut session, preview_features)
}

/// Stable-language convenience form of [`run_source_with_cfg_differential`].
pub fn run_source_cfg_differential(source: &str) -> Result<Outcome, RunSourceError> {
    run_source_with_cfg_differential(source, &PreviewFeatures::new())
}

/// Run a compiler session whose caller has already completed any required
/// import discovery. This keeps filesystem policy in the harness while letting
/// the oracle consume the same canonical multi-module compiler state.
pub fn run_session_with_preview_features(
    session: CompilerSession,
    preview_features: &PreviewFeatures,
) -> Result<Outcome, RunSourceError> {
    let state = query_cfg_state_from_session(
        session,
        &CompileOptions {
            preview_features: preview_features.clone(),
            ..CompileOptions::default()
        },
    )
    .map_err(RunSourceError::Compile)?;
    run_state(state).map_err(RunSourceError::Unsupported)
}

/// Session-aware form of [`run_source_with_cfg_differential`].
pub fn run_session_with_cfg_differential(
    mut session: CompilerSession,
    preview_features: &PreviewFeatures,
) -> Result<Outcome, RunSourceError> {
    run_session_cfg_differential_inner(&mut session, preview_features)
}

fn run_session_cfg_differential_inner(
    session: &mut CompilerSession,
    preview_features: &PreviewFeatures,
) -> Result<Outcome, RunSourceError> {
    let options = CompileOptions {
        preview_features: preview_features.clone(),
        ..CompileOptions::default()
    };
    let pre_state =
        query_cfg_state_from_session_at_stage(session, &options, CfgStage::PreOptimization)
            .map_err(RunSourceError::Compile)?;
    let pre = run_state(pre_state);
    let post_state =
        query_cfg_state_from_session_at_stage(session, &options, CfgStage::PostOptimization)
            .map_err(RunSourceError::Compile)?;
    let post = run_state(post_state);
    if pre == post {
        return pre.map_err(RunSourceError::Unsupported);
    }
    Err(RunSourceError::CfgTransformationDisagreement {
        pre_optimization: pre,
        post_optimization: post,
    })
}

fn run_state(state: CompileState) -> Result<Outcome, Unsupported> {
    run_state_with_budget(state, STEP_BUDGET)
}

fn run_state_with_budget(state: CompileState, budget: u64) -> Result<Outcome, Unsupported> {
    run_state_with_limits(state, budget, MAX_STDOUT_BYTES)
}

fn run_state_with_limits(
    state: CompileState,
    budget: u64,
    stdout_cap: usize,
) -> Result<Outcome, Unsupported> {
    run_state_with_output_limits(state, budget, stdout_cap, MAX_STDERR_BYTES)
}

fn run_state_with_output_limits(
    state: CompileState,
    budget: u64,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<Outcome, Unsupported> {
    // Interpret on a dedicated large-stack worker thread. The tree-walking
    // interpreter recurses per expression *and* per call, so deep-but-valid
    // programs need far more stack than a default thread provides. Running on
    // our own generous stack makes `run_source` safe to call from any thread
    // (a 2 MiB Rust test thread included) and, together with `MAX_DEPTH`, lets
    // unbounded recursion resolve to a typed resource failure *before* the Rust
    // stack is exhausted rather than aborting the process (RUE-340).
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(WORKER_STACK)
            .spawn_scoped(scope, || {
                Interp {
                    state: &state,
                    stdout_trace: Vec::new(),
                    stdout_bytes: 0,
                    stdout_cap,
                    stderr_cap,
                    budget,
                    depth: 0,
                    heap: Vec::new(),
                    small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
                    heap_metadata_bytes: 0,
                }
                .run()
            })
            .expect("spawn oracle interpreter worker thread")
            .join()
            .expect("oracle interpreter worker thread panicked")
    })
}

/// Stack size of the interpreter worker thread. Sized so all `MAX_DEPTH`
/// activations (each ~a few tens of KiB of Rust stack in an unoptimized build,
/// so ~tens of MiB total) fit with several-fold headroom, plus room for
/// per-expression eval recursion. Only touched pages are committed, so the
/// large reservation is cheap; it matches the harness's own worker stack.
const WORKER_STACK: usize = 256 * 1024 * 1024;

/// Total interpreter step budget, shared across **all** call activations (not
/// per-frame): every instruction executed anywhere in the run decrements it, so
/// it bounds *total work* — a runaway loop, deep/unbounded recursion, or any
/// combination — and reports a typed [`ResourceLimitKind`] instead of hanging. Generous
/// enough that no legitimate program in the differential corpus reaches it.
const STEP_BUDGET: u64 = 50_000_000;

/// Maximum interpreter call-recursion depth, shared across all activations.
/// Each Rue call activation is a nested `call` -> `eval` Rust recursion, so
/// unbounded Rue recursion would otherwise overflow the *Rust* stack — an
/// uncatchable process abort that kills the whole differential harness rather
/// than yielding a typed failure (RUE-340). This bound fires first, turning
/// deep/infinite recursion into a [`ResourceLimitKind::RecursionDepth`] result. It sits far above any
/// legitimately-recursive corpus/fuzzer program yet far below the number of
/// activations that fit in `WORKER_STACK`, so the bound always wins the race
/// against stack exhaustion.
const MAX_DEPTH: u32 = 2_000;

/// The direct-write syscall number for the target executing this oracle. The
/// oracle only models the ABI it can identify from its own compiled target;
/// target-specific corpus filters remain authoritative for cross-target cases.
const fn host_write_syscall_number() -> Option<u64> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some(RuntimeTarget::X86_64Linux.write_syscall_number())
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some(RuntimeTarget::Aarch64Linux.write_syscall_number())
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some(RuntimeTarget::Aarch64Macos.write_syscall_number())
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    {
        None
    }
}

/// Byte-address space reserved per abstract heap allocation when synthesizing a
/// `@ptr_to_int` value. Comfortably larger than [`MAX_ALLOC_BYTES`] so an
/// allocation's whole byte range fits in its segment and adjacent allocations
/// never alias, while `alloc_index * HEAP_SEG` stays far below `u64::MAX` for
/// any realistic allocation count.
const HEAP_SEG: u128 = 1 << 44;

/// Base of the synthetic heap address space. Non-zero so no live pointer ever
/// collides with the null address (`0`).
const HEAP_BASE: u128 = HEAP_SEG;

/// Largest byte size the modeled allocator satisfies before returning null.
///
/// The real runtime allocator fails (returns null) once a request exceeds
/// what `mmap` can back — astronomically large sizes such as the corpus's
/// `2305843009213693951`-byte `@realloc`. Every legitimate corpus allocation is
/// a few bytes to kilobytes, so any threshold between them models the platform
/// failure faithfully. Kept below [`HEAP_SEG`] so a satisfiable allocation's
/// byte range never overflows its address segment.
const MAX_ALLOC_BYTES: u128 = 1 << 40;

/// A runtime value. Integers are held in `i128` (wide enough for every Rue
/// integer type, and to detect overflow of any of them before range-checking).
/// Unsigned values are stored as their non-negative magnitude.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Int(i128),
    /// An integer produced from a pointer address. Numeric operations observe
    /// only `value`; the identity token is retained privately so
    /// `@int_to_ptr` cannot mint provenance from a matching literal.
    AddressInt {
        value: i128,
        provenance: AddressProvenance,
    },
    Bool(bool),
    Unit,
    /// A struct or array value: its fields (declaration order) or elements
    /// (ascending index). A payload-carrying enum variant is also an
    /// `Aggregate`: element 0 is the discriminant (`Int` tag), followed by the
    /// variant's payload fields in declaration order (RUE-285). A
    /// discriminant-only enum (or C-like enum) stays a bare `Int` tag.
    Aggregate(Vec<Value>),
    /// A raw pointer into the abstract heap (RUE model-gap closure). `None` is
    /// the null pointer (`@int_to_ptr` on a zero address, a failed
    /// `@alloc`/`@realloc`); `Some` names a live cell inside an [`Allocation`].
    /// Heap allocations (`@alloc`) and address-taken stack places
    /// (`@raw`/`@raw_mut`/`@field_ptr`) share this one provenance-carrying
    /// representation so a pointer read/write, offset, or int round-trip resolves
    /// to the same backing store the source place or allocation owns.
    Ptr(Option<PtrTarget>),
}

/// A provenance-carrying pointer value: allocation identity plus canonical
/// representation-byte offset. Typed views are supplied by each CFG operation
/// from its current function's type authority and never cross call boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PtrTarget {
    /// Index of the owning [`Allocation`] in [`Interp::heap`].
    alloc: usize,
    /// Allocation lifetime generation. Recycled storage keeps its synthetic
    /// address, but every fresh lifetime gets a new generation so stale
    /// pointers and pointer-derived integers cannot regain provenance.
    generation: u64,
    /// Physical byte offset from the allocation base. This is the sole
    /// addressing authority for the representation-byte heap.
    byte_offset: u64,
}

/// Hidden provenance attached to an integer address. It is deliberately
/// ignored by value equality so numeric observables remain unchanged.
#[derive(Debug, Clone)]
struct AddressProvenance(PtrTarget);

impl PartialEq for AddressProvenance {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for AddressProvenance {}

#[derive(Clone, Copy)]
enum AddressArithmetic {
    Add,
    Sub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllocationOrigin {
    Heap,
    Promoted,
    Text,
}

/// One abstract heap allocation.
///
/// The backing store is the target's canonical little-endian representation
/// bytes. Typed views are decoded and encoded on demand through the compiler's
/// layout authority; there is no peer typed-cell store to drift from bytes.
#[derive(Debug, Clone)]
struct Allocation {
    /// Canonical target representation bytes.
    bytes: Vec<u8>,
    /// Initialization is tracked per byte. Reading an uninitialized typed
    /// value is a typed gap; byte copies preserve this state byte-for-byte.
    initialized: Vec<bool>,
    /// Provenance markers for pointer representations. Partial or arbitrary
    /// integer bytes therefore fail closed on typed pointer decode.
    provenance: Vec<Option<PtrTarget>>,
    /// Root type for promoted typed storage, or `None` for raw byte blocks.
    root_ty: Option<Type>,
    /// `@free` released this allocation. Pointer access checks the flag so
    /// undefined use-after-free remains a typed oracle gap rather than
    /// receiving a deterministic value that native execution does not promise.
    freed: bool,
    /// Lifetime generation for the synthetic address represented by this cell.
    generation: u64,
    /// Next entry in the explicit per-size-class free-list stack. The list is
    /// linked through allocation cells so its order does not depend on heap
    /// vector position.
    free_list_next: Option<usize>,
    /// Allocator family and contract metadata. Promoted stack/text backing is
    /// never eligible for `@free`/`@realloc`/`@resize`.
    origin: AllocationOrigin,
    declared_alignment: u64,
    owner_depth: Option<u32>,
}

/// The runtime allocator's small-block class identity. The oracle retains
/// abstract allocation bytes, but reuse must follow the same power-of-two
/// class policy as `rue-allocator` so pointer-identity observations remain
/// truthful after a matching `@free`/`@alloc` sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmallAllocationClass {
    Block(usize),
}

const ORACLE_PAGE_SIZE: usize = 4096;
const ORACLE_MAX_SMALL_SIZE: usize = 16 * 1024;
const ORACLE_MIN_CLASS_SIZE: usize = 8;
const ORACLE_MIN_CLASS_SHIFT: usize = ORACLE_MIN_CLASS_SIZE.trailing_zeros() as usize;
const ORACLE_MAX_CLASS_SHIFT: usize = ORACLE_MAX_SMALL_SIZE.trailing_zeros() as usize;
const ORACLE_SMALL_CLASS_COUNT: usize = ORACLE_MAX_CLASS_SHIFT - ORACLE_MIN_CLASS_SHIFT + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllocationKind {
    Small(usize),
    Direct(usize),
}

fn small_allocation_class(size: i128, align: u64) -> Option<SmallAllocationClass> {
    if size <= 0 || align == 0 || !align.is_power_of_two() || align as usize > ORACLE_PAGE_SIZE {
        return None;
    }
    let size = usize::try_from(size).ok()?;
    if size > ORACLE_MAX_SMALL_SIZE {
        return None;
    }
    let required = size.max(align as usize).max(ORACLE_MIN_CLASS_SIZE);
    let block_size = required.next_power_of_two();
    (block_size <= ORACLE_MAX_SMALL_SIZE).then_some(SmallAllocationClass::Block(block_size))
}

fn allocation_kind(size: i128, align: u64) -> Option<AllocationKind> {
    let size = usize::try_from(size).ok()?;
    if size == 0 {
        return None;
    }
    if let Some(SmallAllocationClass::Block(block_size)) =
        small_allocation_class(size as i128, align)
    {
        return Some(AllocationKind::Small(block_size));
    }
    let mapping_size = size.checked_add(ORACLE_PAGE_SIZE - 1)? / ORACLE_PAGE_SIZE;
    Some(AllocationKind::Direct(mapping_size * ORACLE_PAGE_SIZE))
}

fn small_class_index(block_size: usize) -> usize {
    block_size.trailing_zeros() as usize - ORACLE_MIN_CLASS_SHIFT
}

impl Value {
    /// A detached two-slot `str` view header (`{null, len}`) for shape/
    /// classification fixtures that never read the bytes.
    #[cfg(test)]
    fn str_view(text: impl AsRef<str>) -> Self {
        Value::Aggregate(vec![
            Value::Ptr(None),
            Value::Int(text.as_ref().len() as i128),
        ])
    }

    fn as_int(&self) -> i128 {
        match self {
            Value::Int(n) => *n,
            Value::AddressInt { value, .. } => *value,
            Value::Bool(b) => *b as i128,
            // Zero is a placeholder, not an address: a pointer becomes an
            // integer only through `@ptr_to_int`, which computes its address
            // explicitly and yields an `AddressInt`. Nothing that observes a
            // pointer's identity may route it here — comparison in particular
            // sends a pointer to `values_equal_typed` instead, because reading
            // every address as zero would call two distinct pointers equal
            // (spec 4.3:3e). Defined so callers need not thread an error.
            Value::Unit | Value::Aggregate(_) | Value::Ptr(_) => 0,
        }
    }
    fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::AddressInt { value, .. } => *value != 0,
            Value::Unit | Value::Aggregate(_) | Value::Ptr(_) => false,
        }
    }
}

/// A runtime panic, carrying its typed category and exact stderr observation.
struct Panic {
    kind: TrapKind,
    stderr: String,
    raw_stderr_bytes: usize,
}

impl Panic {
    fn runtime(kind: TrapKind) -> Self {
        let stderr = match kind {
            TrapKind::ArithmeticOverflow => "error: integer overflow\n",
            TrapKind::DivisionByZero => "error: division by zero\n",
            TrapKind::IntegerCastOverflow => "error: integer cast overflow\n",
            TrapKind::IndexOutOfBounds => "error: index out of bounds\n",
            TrapKind::InvalidUtf8 => "error: invalid UTF-8\n",
            // User-authored panics and assertions require a dynamic diagnostic.
            // Reaching this fixed-message constructor would erase that
            // observation, so keep the invariant loud in debug builds.
            TrapKind::UserPanic | TrapKind::AssertionFailure => {
                debug_assert!(false, "dynamic trap stderr must be supplied explicitly");
                ""
            }
            // `Unreachable` represents defensive malformed-CFG behavior whose
            // native counterpart generally faults rather than using the Rue
            // runtime error channel.
            TrapKind::Unreachable => "",
        };
        Self::with_stderr(kind, stderr.to_string(), stderr.len())
    }

    fn with_stderr(kind: TrapKind, stderr: String, raw_stderr_bytes: usize) -> Self {
        Self {
            kind,
            stderr,
            raw_stderr_bytes,
        }
    }
}

type Step<T> = Result<T, Flow>;

/// Non-local outcomes of evaluating an instruction.
enum Flow {
    /// A modeled runtime panic (maps to exit 101).
    Panic(Panic),
    /// Interpretation stopped with a typed model gap or hard oracle failure.
    Unsupported(Unsupported),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaceAccess {
    Read,
    Write,
}

#[derive(Clone, Copy)]
enum AbortIntrinsic {
    Panic,
    Assert,
}

// `@panic` diverges, so its static contract is `!` (never) end to end — HM,
// AIR, and CFG all carry NEVER for it (RUE-512); the CFG builder terminates
// the block with `Unreachable` after the abort.
const PANIC_CFG_RESULT_TYPE: Type = Type::NEVER;

impl From<Unsupported> for Flow {
    fn from(u: Unsupported) -> Self {
        Flow::Unsupported(u)
    }
}

fn unsupported(kind: UnsupportedKind, detail: impl Into<String>) -> Flow {
    Flow::Unsupported(Unsupported::new(kind, detail))
}

fn unsupported_intrinsic_kind_for_operation(
    operation: rue_air::IntrinsicOperation,
) -> UnsupportedKind {
    use ExternalDependencyKind as External;
    use SemanticGapKind as Semantic;
    use UnsupportedIntrinsicKind as Intrinsic;

    match operation {
        rue_air::IntrinsicOperation::ParseI32 => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ParseI32))
        }
        rue_air::IntrinsicOperation::ParseI64 => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ParseI64))
        }
        rue_air::IntrinsicOperation::ParseU32 => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ParseU32))
        }
        rue_air::IntrinsicOperation::ParseU64 => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ParseU64))
        }
        // `@ptr_read_unaligned`/`@ptr_write_unaligned` (ADR-0059) differ from the
        // aligned forms only in the alignment *requirement* on the address. The
        // oracle does not model alignment, so they share the aligned forms'
        // semantics and gap classification.
        rue_air::IntrinsicOperation::PtrRead | rue_air::IntrinsicOperation::PtrReadUnaligned => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerRead))
        }
        rue_air::IntrinsicOperation::PtrWrite | rue_air::IntrinsicOperation::PtrWriteUnaligned => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerWrite))
        }
        rue_air::IntrinsicOperation::PtrOffset => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerOffset))
        }
        rue_air::IntrinsicOperation::PtrToInt => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerToInt))
        }
        rue_air::IntrinsicOperation::IntToPtr => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::IntToPointer))
        }
        rue_air::IntrinsicOperation::Raw => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::RawAddress))
        }
        rue_air::IntrinsicOperation::RawMut => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::RawMutableAddress))
        }
        rue_air::IntrinsicOperation::FieldPtr => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::FieldPointer))
        }
        rue_air::IntrinsicOperation::Alloc => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::Allocate))
        }
        rue_air::IntrinsicOperation::AllocZeroed => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::AllocateZeroed))
        }
        rue_air::IntrinsicOperation::Free => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::Free))
        }
        rue_air::IntrinsicOperation::Realloc => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::Reallocate))
        }
        rue_air::IntrinsicOperation::Resize => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::Resize))
        }
        rue_air::IntrinsicOperation::ByteCopy => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ByteCopy))
        }
        rue_air::IntrinsicOperation::ByteMove => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ByteMove))
        }
        rue_air::IntrinsicOperation::ByteSet => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ByteSet))
        }
        rue_air::IntrinsicOperation::IntToFloat => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::IntToFloat))
        }
        rue_air::IntrinsicOperation::FloatToInt => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::FloatToInt))
        }
        rue_air::IntrinsicOperation::FloatCast => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::FloatCast))
        }
        rue_air::IntrinsicOperation::ReadLine => {
            UnsupportedKind::ExternalDependency(External::StandardInput)
        }
        rue_air::IntrinsicOperation::RandomU32 => {
            UnsupportedKind::ExternalDependency(External::RandomU32)
        }
        rue_air::IntrinsicOperation::RandomU64 => {
            UnsupportedKind::ExternalDependency(External::RandomU64)
        }
        rue_air::IntrinsicOperation::Syscall => {
            UnsupportedKind::ExternalDependency(External::SystemCall)
        }
        // Process arguments/environment are captured from loader-supplied
        // process state at entry, so their values lie outside deterministic Rue
        // semantics — an external dependency like `@random_*` (RUE-935).
        rue_air::IntrinsicOperation::ArgCount => {
            UnsupportedKind::ExternalDependency(External::ArgCount)
        }
        rue_air::IntrinsicOperation::ArgPtr => {
            UnsupportedKind::ExternalDependency(External::ArgPtr)
        }
        rue_air::IntrinsicOperation::ArgLen => {
            UnsupportedKind::ExternalDependency(External::ArgLen)
        }
        rue_air::IntrinsicOperation::EnvCount => {
            UnsupportedKind::ExternalDependency(External::EnvCount)
        }
        rue_air::IntrinsicOperation::EnvPtr => {
            UnsupportedKind::ExternalDependency(External::EnvPtr)
        }
        rue_air::IntrinsicOperation::EnvLen => {
            UnsupportedKind::ExternalDependency(External::EnvLen)
        }
        rue_air::IntrinsicOperation::PanicNoMessage
        | rue_air::IntrinsicOperation::Panic
        | rue_air::IntrinsicOperation::AssertFailed
        | rue_air::IntrinsicOperation::BoundsCheck
        | rue_air::IntrinsicOperation::DebugI64
        | rue_air::IntrinsicOperation::DebugU64
        | rue_air::IntrinsicOperation::DebugBool
        | rue_air::IntrinsicOperation::DebugStr
        | rue_air::IntrinsicOperation::TotalCmp
        | rue_air::IntrinsicOperation::BitCast => {
            UnsupportedKind::ContractViolation(ContractViolationKind::UnexpectedIntrinsic)
        }
    }
}

fn unsupported_runtime_call_kind(kind: RuntimeCallKind) -> Option<UnsupportedRuntimeCallKind> {
    match kind {
        RuntimeCallKind::StrPrintAggregate | RuntimeCallKind::StrPrintProjected => {
            Some(UnsupportedRuntimeCallKind::Print)
        }
        RuntimeCallKind::StrPrintlnAggregate | RuntimeCallKind::StrPrintlnProjected => {
            Some(UnsupportedRuntimeCallKind::Println)
        }
        RuntimeCallKind::StrByteAt
        | RuntimeCallKind::StrCharScalar
        | RuntimeCallKind::StrCharNext
        | RuntimeCallKind::StrCharScalarLossy
        | RuntimeCallKind::StrCharNextLossy
        | RuntimeCallKind::ToString
        | RuntimeCallKind::ToStringUnsigned
        | RuntimeCallKind::DebugI64
        | RuntimeCallKind::DebugU64
        | RuntimeCallKind::DebugBool
        | RuntimeCallKind::DebugStr
        | RuntimeCallKind::Panic
        | RuntimeCallKind::PanicNoMessage
        | RuntimeCallKind::AssertFailed
        | RuntimeCallKind::BoundsCheck
        | RuntimeCallKind::ReadLine
        | RuntimeCallKind::ParseI32
        | RuntimeCallKind::ParseI64
        | RuntimeCallKind::ParseU32
        | RuntimeCallKind::ParseU64
        | RuntimeCallKind::RandomU32
        | RuntimeCallKind::RandomU64
        | RuntimeCallKind::Alloc
        | RuntimeCallKind::AllocZeroed
        | RuntimeCallKind::Free
        | RuntimeCallKind::Realloc
        | RuntimeCallKind::Resize
        | RuntimeCallKind::ArgCount
        | RuntimeCallKind::ArgPtr
        | RuntimeCallKind::ArgLen
        | RuntimeCallKind::EnvCount
        | RuntimeCallKind::EnvPtr
        | RuntimeCallKind::EnvLen
        | RuntimeCallKind::ByteCopy
        | RuntimeCallKind::ByteMove
        | RuntimeCallKind::ByteSet
        // The ADR-0083 test channel. The dispatcher's own helpers belong to a
        // test image, which the oracle never compiles. Of the reporting helpers
        // reachable in an ordinary program, the two `@assert` lowers to are
        // modeled outright (`preflight_test_channel_call`), because nothing
        // precedes them to stop at. `__rue_test_fail` and its comparison form
        // are reached only after a rendering that opens with the allocation
        // helper above, so the interpreter has already stopped at a registered
        // gap by the time either is evaluated.
        | RuntimeCallKind::TestNormalizeProcess
        | RuntimeCallKind::TestComplete
        | RuntimeCallKind::TestFailureSite
        | RuntimeCallKind::TestFail
        | RuntimeCallKind::TestFailComparison
        | RuntimeCallKind::TestFailAssert
        | RuntimeCallKind::TestUsageError => None,
    }
}

struct Interp<'a> {
    state: &'a CompileState,
    /// Canonical ordered raw-byte observation trace for file descriptor 1.
    stdout_trace: Vec<u8>,
    /// Raw emitted byte count, independent of decoded string length.
    stdout_bytes: usize,
    stdout_cap: usize,
    /// Maximum raw bytes retained for the single terminating runtime stderr
    /// observation.
    stderr_cap: usize,
    /// Remaining total step budget (see [`STEP_BUDGET`]). Shared across every
    /// activation and decremented per instruction, so it bounds total work
    /// including recursion, not just per-frame loops.
    budget: u64,
    /// Current call-recursion depth (see [`MAX_DEPTH`]). Incremented on entry to
    /// each `call` and decremented on exit, bounding Rust-stack recursion.
    depth: u32,
    /// The abstract heap: every `@alloc` block and every promoted
    /// address-taken stack place. Indexed by [`PtrTarget::alloc`]. It lives on
    /// the interpreter (not a frame) so a pointer read across a call boundary
    /// still resolves after the address is passed to a callee.
    heap: Vec<Allocation>,
    small_free_heads: [Option<usize>; ORACLE_SMALL_CLASS_COUNT],
    heap_metadata_bytes: usize,
}

/// Per-call activation record. `cache` preserves values produced along the
/// executed CFG path: Rue SSA values may be used in dominated blocks without
/// being repeated as block arguments. On block entry, that block's own values
/// are invalidated so loop re-entry recomputes the current iteration while
/// values from executed dominators retain their original evaluation snapshot.
struct Frame {
    /// Parameters laid out by ABI **slot**, not by logical argument: an
    /// by-value aggregate spans one slot per flattened scalar leaf (a
    /// `[i32; 3]` occupies three slots), while a physical `borrow` / `inout`
    /// occupies one pointer slot. The whole semantic value is kept at its base
    /// slot; extra by-value slots are `None` (they are only reached through the
    /// base via a projection, never directly).
    params: Vec<Option<Value>>,
    locals: Vec<Option<Value>>,
    cache: HashMap<u32, Value>,
    /// Slots whose address has been taken (`@raw`/`@raw_mut`/`@field_ptr`). The
    /// slot's storage is moved into the heap allocation named here; every
    /// subsequent `Load`/`Store`/`PlaceRead`/`PlaceWrite` of the slot aliases
    /// that allocation so a write through the pointer and a direct read agree.
    /// Keyed by [`promotion_key`] over the slot's [`PlaceBase`].
    promoted: HashMap<u64, usize>,
    /// Physical by-reference parameter slots bound to their caller locations.
    /// Ordinary calls retain copy-in/copy-out behavior and leave this empty;
    /// raw accessor execution uses it to preserve the yielded place identity.
    param_places: HashMap<u32, PtrTarget>,
    /// The single trailing accessor `yield` returns its place rather than the
    /// value loaded from that place.
    place_return: bool,
}

enum WritebackPlace<'a> {
    Simple { base: PlaceBase, base_type: Type },
    Stored(&'a Place),
}

impl<'a> Interp<'a> {
    fn canonical_null_marker() -> PtrTarget {
        PtrTarget {
            alloc: usize::MAX,
            generation: 0,
            byte_offset: 0,
        }
    }

    fn function(&self) -> &OracleFunction {
        &self.state.functions[self.state.current_function.load(Ordering::Relaxed)]
    }

    fn type_pool(&self) -> &FrozenTypeInternPool {
        &self.function().type_pool
    }

    fn string_literal_value(&mut self, text: String, ty: Type) -> Step<Value> {
        // A source string literal materializes as a real text header over an
        // (immortal) heap byte allocation (RUE-1010 §6.13.2). The ABI slot width
        // comes from the compiler's `abi_slot_count` authority: a str/Str(N)
        // view is two slots ({ptr, len}); an owned StrBuf is three
        // ({buf, cap, len}). A literal is non-owning/static-backed, so its
        // capacity word is 0 (matching the compiler's `cap = 0` literal state).
        let slots = self.text_value_slot_width(ty);
        self.materialize_text(text.into_bytes(), slots, 0)
    }

    /// ABI value-slot width the interpreter carries for a text value of type
    /// `ty`, taken from the compiler's `abi_slot_count` authority. A non-struct
    /// `ty` never reaches a string literal; it defaults to the owned-string
    /// width so a malformed value is still shaped like a header rather than a
    /// view.
    fn text_value_slot_width(&self, ty: Type) -> usize {
        ty.as_struct()
            .map(|id| self.type_pool().abi_slot_count(Type::new_struct(id)) as usize)
            .unwrap_or(3)
    }

    fn is_str_like_type(&self, ty: Type) -> bool {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            self.is_str_like_struct(struct_id)
        } else {
            false
        }
    }

    fn is_bare_str_type(&self, ty: Type) -> bool {
        ty.as_struct()
            .is_some_and(|struct_id| &*self.type_pool().struct_def(struct_id).name == "str")
    }

    fn is_str_like_struct(&self, struct_id: rue_air::StructId) -> bool {
        let name: &str = &self.type_pool().struct_def(struct_id).name;
        rue_air::is_string_view_struct_name(name)
    }

    fn text_struct_slots(&self, struct_id: rue_air::StructId) -> Option<usize> {
        // A str/Str(N) view certifies two ABI slots and an
        // owned StrBuf three. Derive the width from `abi_slot_count` rather than
        // carrying the 2/3 literals independently; the guard keeps this `None`
        // for every non-text struct.
        if self.is_str_like_struct(struct_id) || self.is_owned_string_struct(struct_id) {
            Some(self.text_value_slot_width(Type::new_struct(struct_id)))
        } else {
            None
        }
    }

    fn is_owned_string_type(&self, ty: Type) -> bool {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            self.is_owned_string_struct(struct_id)
        } else {
            false
        }
    }

    fn is_owned_string_struct(&self, struct_id: rue_air::StructId) -> bool {
        self.type_pool().is_strbuf(struct_id)
    }

    /// Whether `ty` is a text type — a `str`/`Str(N)` view or an owned `StrBuf`.
    /// Text values are ordinary `Value::Aggregate` headers over a real byte
    /// allocation (RUE-1010 §6.13); this predicate lets a consumer that holds a
    /// value's static type read its bytes through [`Self::text_bytes`] rather
    /// than depending on any layout detail.
    fn is_text_type(&self, ty: Type) -> bool {
        self.is_str_like_type(ty) || self.is_owned_string_type(ty)
    }

    /// Extract the `(buffer pointer, length)` pair from a materialized text
    /// header value. A `str`/`Str(N)` view is a two-slot `{ptr, len}` aggregate;
    /// an owned `StrBuf` is `{core: {buf, cap}, len}`, so its pointer lives one
    /// level down in the nested `RawBuf` core (RUE-1066). Returns `None` for a
    /// value that is not shaped like either header.
    fn text_ptr_len(val: &Value) -> Option<(Option<PtrTarget>, i128)> {
        let Value::Aggregate(cells) = val else {
            return None;
        };
        if cells.len() != 2 {
            return None;
        }
        let len = match &cells[1] {
            Value::Int(n) => *n,
            _ => return None,
        };
        match &cells[0] {
            // `str`/`Str(N)` view: `{ptr, len}`.
            Value::Ptr(target) => Some((target.clone(), len)),
            // Owned `StrBuf`: `{core: {buf, cap}, len}` — the pointer is the
            // core's field 0.
            Value::Aggregate(core) => match core.first() {
                Some(Value::Ptr(target)) => Some((target.clone(), len)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Read the byte content of a materialized text value out of the heap. The
    /// `len` bytes starting at the header's buffer pointer are read cell by cell
    /// through the same `byte_at` path the runtime's byte helpers model, so a
    /// text read rides entirely on the modeled allocation store.
    fn text_bytes(&self, val: &Value) -> Step<Vec<u8>> {
        self.text_bytes_bounded(val, None)
    }

    /// Read a text header while enforcing an output-specific payload bound
    /// before allocating or walking the claimed range. This keeps malformed
    /// length words from turning a bounded failure into an unbounded
    /// allocation or byte loop.
    fn text_bytes_bounded(&self, val: &Value, max_len: Option<usize>) -> Step<Vec<u8>> {
        let gap = unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead);
        let Some((target, len)) = Self::text_ptr_len(val) else {
            return Err(unsupported(gap, "text value is not a materialized header"));
        };
        if len <= 0 {
            return Ok(Vec::new());
        }
        let len = match usize::try_from(len) {
            Ok(len) => len,
            Err(_) if max_len.is_some() => {
                return Err(unsupported(
                    UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                    format!(
                        "stdout byte limit exceeded ({}-byte limit)",
                        self.stdout_cap
                    ),
                ));
            }
            Err(_) => {
                return Err(unsupported(
                    gap,
                    "text header length exceeds the host addressable range",
                ));
            }
        };
        if let Some(max_len) = max_len {
            if len > max_len {
                return Err(unsupported(
                    UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                    format!(
                        "stdout byte limit exceeded ({}-byte limit)",
                        self.stdout_cap
                    ),
                ));
            }
        }
        let Some(target) = target else {
            // A non-null length over a null buffer is a malformed header.
            return Err(unsupported(
                gap,
                "text header has a null buffer with nonzero length",
            ));
        };
        let mut bytes = Vec::with_capacity(len);
        for i in 0..len {
            bytes.push(self.byte_at(&target, i as i128, gap)?);
        }
        Ok(bytes)
    }

    /// Materialize a text value: mint a heap byte allocation holding `bytes` and
    /// build the ABI-shaped header over it. A two-slot `slots` yields a
    /// `str`/`Str(N)` view `{ptr, len}`; a three-slot `slots` yields an owned
    /// `StrBuf` `{core: {buf, cap}, len}` (RUE-1066 nested layout). An empty
    /// value keeps a null buffer (no allocation), matching the source's empty
    /// state. `cap` is the capacity word stored in an owned header.
    fn materialize_text(&mut self, bytes: Vec<u8>, slots: usize, cap: i128) -> Step<Value> {
        let len = bytes.len() as i128;
        let ptr = if bytes.is_empty() {
            Value::Ptr(None)
        } else {
            let len = bytes.len();
            self.reserve_heap_metadata(bytes.len())?;
            let alloc = self.heap_alloc_bytes(
                bytes,
                vec![true; len],
                vec![None; len],
                None,
                if slots >= 3 && cap > 0 {
                    AllocationOrigin::Heap
                } else {
                    AllocationOrigin::Text
                },
                1,
            );
            Value::Ptr(Some(PtrTarget {
                alloc,
                generation: self.heap[alloc].generation,
                byte_offset: 0,
            }))
        };
        let value = if slots >= 3 {
            // Owned `StrBuf`: `{core: {buf, cap}, len}`.
            Value::Aggregate(vec![
                Value::Aggregate(vec![ptr, Value::Int(cap)]),
                Value::Int(len),
            ])
        } else {
            // `str`/`Str(N)` view: `{ptr, len}`.
            Value::Aggregate(vec![ptr, Value::Int(len)])
        };
        Ok(value)
    }

    /// Allocate `bytes` into the heap and return a raw pointer to cell 0, for
    /// tests that drive the projected-char builtins (which take a `ptr`/`len`
    /// pair) directly.
    #[cfg(test)]
    fn test_alloc_str_ptr(&mut self, bytes: &[u8]) -> Value {
        let alloc = self.heap_alloc_bytes(
            bytes.to_vec(),
            vec![true; bytes.len()],
            vec![None; bytes.len()],
            None,
            AllocationOrigin::Text,
            1,
        );
        Value::Ptr(Some(PtrTarget {
            alloc,
            generation: self.heap[alloc].generation,
            byte_offset: 0,
        }))
    }

    /// Whether `kind` is a §5.1 failure-channel call the interpreter models,
    /// validating its whole static shape before an operand is evaluated.
    ///
    /// `@assert` lowers to the staging call and the terminal report in every
    /// build, not only in a test image (spec 4.13:5d, ADR-0083), so an ordinary
    /// corpus program reaches both — and, unlike the comparison family, with no
    /// rendering in front of them to stop at. The channel itself is invisible
    /// here: an ordinary process has no descriptor 3, so the frame write fails
    /// with `EBADF` by design and what is left to model is the staging call's
    /// absence of effect and the terminal call's pinned abort.
    fn preflight_test_channel_call(
        &self,
        kind: RuntimeCallKind,
        arg_types: &[Type],
        arg_modes: &[CfgArgMode],
        result_ty: Type,
    ) -> Step<bool> {
        let expected = match kind {
            RuntimeCallKind::TestFailureSite => 3,
            RuntimeCallKind::TestFailAssert => 2,
            _ => return Ok(false),
        };
        let name = kind.helper().symbol();
        if arg_types.len() != expected || arg_modes.len() != expected {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity),
                format!("runtime call '{name}' arity"),
            ));
        }
        if !arg_modes.iter().all(|mode| *mode == CfgArgMode::Normal) {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature),
                format!("runtime call '{name}' argument mode"),
            ));
        }
        let signature_matches = match kind {
            RuntimeCallKind::TestFailureSite => {
                self.is_str_like_type(arg_types[0])
                    && arg_types[1] == Type::U32
                    && arg_types[2] == Type::U32
                    && result_ty == Type::UNIT
            }
            _ => {
                self.is_str_like_type(arg_types[0])
                    && arg_types[1] == Type::U32
                    && result_ty == PANIC_CFG_RESULT_TYPE
            }
        };
        if !signature_matches {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature),
                format!("runtime call '{name}' signature"),
            ));
        }
        Ok(true)
    }

    /// Execute one modeled failure-channel call.
    ///
    /// The staging call has no observable effect in an ordinary program, and
    /// the terminal one writes exactly what spec 4.13:5d pins — the same two
    /// messages, and the same two trap categories, the conditional `@assert`
    /// intrinsic wrote before the report was added.
    fn eval_test_channel_call(&mut self, kind: RuntimeCallKind, args: &[Value]) -> Step<Value> {
        if kind == RuntimeCallKind::TestFailureSite {
            return Ok(Value::Unit);
        }
        match args {
            [_, Value::Int(0)] => {
                self.abort_with_stderr(TrapKind::AssertionFailure, &[b"assertion failed\n"])
            }
            [message, Value::Int(_)] if Self::text_ptr_len(message).is_some() => {
                let bytes = self.text_bytes(message)?;
                self.abort_with_stderr(TrapKind::UserPanic, &[b"panic: ", &bytes, b"\n"])
            }
            _ => Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature),
                "runtime call '__rue_test_fail_assert' runtime value shape",
            )),
        }
    }

    fn classify_unsupported_runtime_call_static(
        &self,
        kind: RuntimeCallKind,
        arg_types: &[Type],
        arg_modes: &[CfgArgMode],
        result_ty: Type,
    ) -> UnsupportedKind {
        let Some(runtime_call) = unsupported_runtime_call_kind(kind) else {
            return UnsupportedKind::ContractViolation(ContractViolationKind::MissingFunctionBody);
        };
        let projected = matches!(
            kind,
            RuntimeCallKind::StrPrintProjected | RuntimeCallKind::StrPrintlnProjected
        );
        let expected_arity = if projected { 2 } else { 1 };
        let arity_matches = arg_types.len() == expected_arity && arg_modes.len() == expected_arity;
        if !arity_matches {
            return UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity);
        }
        if !arg_modes.iter().all(|mode| *mode == CfgArgMode::Normal) {
            return UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature);
        }

        let signature_matches = match runtime_call {
            UnsupportedRuntimeCallKind::Print | UnsupportedRuntimeCallKind::Println => {
                let text_matches = if projected {
                    self.pointer_pointee(arg_types[0])
                        .is_some_and(|(pointee, _)| pointee == Type::U8)
                        && arg_types[1] == Type::U64
                } else {
                    // Aggregate helpers are emitted only for the canonical
                    // `str`/`Str(N)` view ABI. `StrBuf` always takes the
                    // projected pointer/length route.
                    self.is_str_like_type(arg_types[0])
                };
                text_matches && result_ty == Type::UNIT
            }
        };
        if signature_matches {
            UnsupportedKind::SemanticGap(SemanticGapKind::RuntimeCall(runtime_call))
        } else {
            UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
        }
    }

    fn classify_unsupported_runtime_call(
        &self,
        kind: RuntimeCallKind,
        args: &[Value],
        arg_types: &[Type],
        arg_modes: &[CfgArgMode],
        result_ty: Type,
    ) -> UnsupportedKind {
        let static_kind =
            self.classify_unsupported_runtime_call_static(kind, arg_types, arg_modes, result_ty);
        let UnsupportedKind::SemanticGap(SemanticGapKind::RuntimeCall(runtime_call)) = static_kind
        else {
            return static_kind;
        };
        if args.len() != arg_types.len() {
            return UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity);
        }

        let is_int_value = |index: usize| matches!(args[index], Value::Int(_));
        let values_match = match runtime_call {
            UnsupportedRuntimeCallKind::Print | UnsupportedRuntimeCallKind::Println => {
                if matches!(
                    kind,
                    RuntimeCallKind::StrPrintProjected | RuntimeCallKind::StrPrintlnProjected
                ) {
                    // The projected print path passes a raw text pointer + len.
                    matches!(args[0], Value::Ptr(_)) && is_int_value(1)
                } else {
                    // The aggregate print path passes a materialized text header.
                    Self::text_ptr_len(&args[0]).is_some()
                }
            }
        };
        if values_match {
            static_kind
        } else {
            UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
        }
    }

    /// Execute the compiler's string output helpers against the one canonical
    /// fd-1 byte trace. Aggregate calls carry a materialized text header;
    /// projected calls carry the same header's byte pointer and length after
    /// the native lowering has selected the projected ABI. Both routes read
    /// through the representation-byte heap, preserving provenance,
    /// initialization, liveness, bounds, and exact byte order.
    fn eval_runtime_output_call(
        &mut self,
        kind: RuntimeCallKind,
        args: &[Value],
        arg_types: &[Type],
    ) -> Step<Value> {
        let projected = matches!(
            kind,
            RuntimeCallKind::StrPrintProjected | RuntimeCallKind::StrPrintlnProjected
        );
        let remaining = self.stdout_cap.saturating_sub(self.stdout_bytes);
        let newline_bytes = if matches!(
            kind,
            RuntimeCallKind::StrPrintlnAggregate | RuntimeCallKind::StrPrintlnProjected
        ) {
            1
        } else {
            0
        };
        let max_payload = remaining.saturating_sub(newline_bytes);
        let (bytes, newline) = if projected {
            let [Value::Ptr(pointer), Value::Int(length)] = args else {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature),
                    "projected output runtime value shape",
                ));
            };
            if *length < 0 {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature),
                    "projected output has negative length",
                ));
            }
            let length = usize::try_from(*length).map_err(|_| {
                unsupported(
                    UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                    format!(
                        "stdout byte limit exceeded ({}-byte limit)",
                        self.stdout_cap
                    ),
                )
            })?;
            if length > max_payload {
                return Err(unsupported(
                    UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                    format!(
                        "stdout byte limit exceeded ({}-byte limit)",
                        self.stdout_cap
                    ),
                ));
            }
            if arg_types.len() != 2 {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity),
                    "projected output runtime arity",
                ));
            }
            let bytes = if length == 0 {
                Vec::new()
            } else {
                let Some(pointer) = pointer else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "projected output has a null buffer with nonzero length",
                    ));
                };
                (0..length)
                    .map(|offset| {
                        self.byte_at(
                            pointer,
                            offset as i128,
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrRead,
                            ),
                        )
                    })
                    .collect::<Step<Vec<_>>>()?
            };
            (bytes, matches!(kind, RuntimeCallKind::StrPrintlnProjected))
        } else {
            let [value] = args else {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity),
                    "aggregate output runtime arity",
                ));
            };
            let bytes = self.text_bytes_bounded(value, Some(max_payload))?;
            (bytes, matches!(kind, RuntimeCallKind::StrPrintlnAggregate))
        };

        self.observe_stdout(&bytes)?;
        if newline {
            self.observe_stdout(b"\n")?;
        }
        Ok(Value::Unit)
    }

    /// Model only a direct `write(1, buf, len)` syscall to the controlled,
    /// blocking stdout capture sink. The native runner concurrently drains
    /// that blocking pipe and does not deliver signals to the child; together
    /// with the POSIX `PIPE_BUF` bound, this makes a small write atomic and
    /// deterministic. Larger writes remain a typed external dependency
    /// because an arbitrary OS short write is not an oracle contract. The
    /// syscall number and argument order are target ABI facts; every other
    /// syscall, descriptor, pointer shape, and target ambiguity remains
    /// external. A valid pointer is resolved through the representation-byte
    /// heap before any output is observed.
    fn eval_stdout_syscall(&mut self, args: &[Value]) -> Step<Value> {
        let gap = UnsupportedKind::ExternalDependency(ExternalDependencyKind::SystemCall);
        let Some(write_nr) = host_write_syscall_number() else {
            return Err(unsupported(
                gap,
                "stdout syscall ABI is not modeled for this host",
            ));
        };
        if args.len() != 4 {
            return Err(unsupported(
                gap,
                "syscall is not the target write(fd, buffer, length) shape",
            ));
        }
        let syscall_number = args[0].as_int();
        let fd = args[1].as_int();
        if syscall_number != write_nr as i128 {
            return Err(unsupported(
                gap,
                "syscall is not the target stdout write number",
            ));
        }
        if fd != 1 {
            return Err(unsupported(gap, "syscall write descriptor is not stdout"));
        }
        let length = args[3].as_int();
        if length < 0 {
            return Err(unsupported(gap, "syscall write length is negative"));
        }
        let length = u64::try_from(length)
            .ok()
            .and_then(|length| usize::try_from(length).ok())
            .ok_or_else(|| unsupported(gap, "syscall write length exceeds host range"))?;
        if length > MAX_MODELED_STDOUT_WRITE_BYTES {
            return Err(unsupported(
                gap,
                "syscall write exceeds the deterministic atomic stdout bound",
            ));
        }
        if length > self.stdout_cap.saturating_sub(self.stdout_bytes) {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                "syscall write exceeds the bounded stdout observation",
            ));
        }
        if length == 0 {
            return Ok(Value::Int(0));
        }

        let target = match &args[2] {
            Value::AddressInt { value, provenance } => self
                .address_target(*value as u128, &provenance.0)
                .ok_or_else(|| unsupported(gap, "syscall write pointer has invalid provenance"))?,
            _ => {
                return Err(unsupported(
                    gap,
                    "syscall write pointer is not pointer-derived",
                ));
            }
        };
        let bytes = (0..length)
            .map(|offset| self.byte_at(&target, offset as i128, gap))
            .collect::<Step<Vec<_>>>()?;
        self.observe_stdout(&bytes)?;
        Ok(Value::Int(length as i128))
    }

    fn pointer_pointee(&self, ty: Type) -> Option<(Type, bool)> {
        match ty.kind() {
            TypeKind::PtrConst(id) => Some((self.type_pool().ptr_const_def(id), false)),
            TypeKind::PtrMut(id) => Some((self.type_pool().ptr_mut_def(id), true)),
            _ => None,
        }
    }

    fn option_payload(&self, ty: Type) -> Option<Type> {
        let TypeKind::Enum(enum_id) = ty.kind() else {
            return None;
        };
        let def = self.type_pool().enum_def(enum_id);
        let (Some(some), Some(none)) = (def.find_variant("Some"), def.find_variant("None")) else {
            return None;
        };
        if def.variant_count() != 2 || !def.variant_payload(none).is_empty() {
            return None;
        }
        let [payload] = def.variant_payload(some) else {
            return None;
        };
        Some(*payload)
    }

    fn is_option_of(&self, ty: Type, payload: Type) -> bool {
        self.option_payload(ty) == Some(payload)
    }

    /// Whether this exact pointer-typed `Const(0)` is the compiler's
    /// synthesized null pointer for an empty borrowed array slice.
    ///
    /// A zero constant and const-pointer result are not sufficient provenance:
    /// user-authored `@int_to_ptr(0)` could otherwise be mislabeled as this
    /// model gap. Require the value to feed field 0 of the canonical
    /// two-word `[T] { ptr, len: 0 }` StructInit emitted by slice coercion.
    fn is_empty_slice_pointer(&self, cfg: &Cfg, pointer: CfgValue) -> bool {
        let pointer_inst = cfg.get_inst(pointer);
        if !pointer_inst.ty.is_ptr_const()
            || !matches!(pointer_inst.data, CfgInstData::Const(0))
            || cfg.value_use_count(pointer) != 1
        {
            return false;
        }
        let pointer_ty = pointer_inst.ty;
        cfg.blocks()
            .iter()
            .filter_map(|block| {
                let pointer_index = block.insts.iter().position(|value| *value == pointer)?;
                Some(block.insts.iter().skip(pointer_index + 1).copied())
            })
            .flatten()
            .any(|value| {
                let data = &cfg.get_inst(value).data;
                let CfgInstData::StructInit { struct_id, .. } = data else {
                    return false;
                };
                let def = self.type_pool().struct_def(*struct_id);
                let is_slice = rue_air::is_slice_struct_name(&def.name);
                if !is_slice
                    || def.fields.len() != 2
                    || def.fields[0].ty != pointer_ty
                    || def.fields[1].ty != Type::U64
                    || cfg.get_struct_fields(data).len() != 2
                    || cfg.get_inst(value).ty != Type::new_struct(*struct_id)
                    || cfg.value_use_count(value) != 1
                {
                    return false;
                }
                let fields = cfg.get_struct_fields(data);
                fields[0] == pointer
                    && cfg.get_inst(fields[1]).ty == Type::U64
                    && matches!(cfg.get_inst(fields[1]).data, CfgInstData::Const(0))
                    && cfg.blocks().iter().any(|block| {
                        block.insts.iter().any(|candidate| {
                            let data = &cfg.get_inst(*candidate).data;
                            let CfgInstData::Call { .. } = data else {
                                return false;
                            };
                            cfg.get_call_args(data)
                                .iter()
                                .any(|arg| arg.value == value && arg.mode == CfgArgMode::Normal)
                        })
                    })
            })
    }

    fn projection_types(&self, cfg: &Cfg, projection: Projection) -> Option<(Type, Type)> {
        match projection {
            Projection::Field {
                struct_id,
                field_index,
            } => {
                let def = self.type_pool().struct_def(struct_id);
                let field = def.fields.get(field_index as usize)?;
                Some((Type::new_struct(struct_id), field.ty))
            }
            Projection::Index { array_type, index } => {
                if index.as_u32() as usize >= cfg.value_count()
                    || !cfg.get_inst(index).ty.is_integer()
                {
                    return None;
                }
                let TypeKind::Array(array_id) = array_type.kind() else {
                    return None;
                };
                let (element, _) = self.type_pool().array_def(array_id);
                Some((array_type, element))
            }
        }
    }

    /// Validate the complete typed projection chain carried by a place.
    /// Every projection must consume the base type or the preceding
    /// projection's result, and the last result must match the operation's
    /// expected value type.
    fn place_projection_metadata_is_valid(
        &self,
        cfg: &Cfg,
        place: &Place,
        expected_type: Type,
        access: PlaceAccess,
    ) -> bool {
        let projections = cfg.get_place_projections(place);
        let mut previous_result = place.base_type;
        for &projection in projections {
            let Some((container, result)) = self.projection_types(cfg, projection) else {
                return false;
            };
            if previous_result != container {
                return false;
            }
            previous_result = result;
        }
        previous_result == expected_type
            || (access == PlaceAccess::Read
                && self.is_str_like_type(previous_result)
                && self.is_bare_str_type(expected_type))
    }

    fn place_base_violation(
        &self,
        cfg: &Cfg,
        place: &Place,
        access: PlaceAccess,
    ) -> Option<ContractViolationKind> {
        let (slot, limit, width, param_slot) = match place.base {
            PlaceBase::Local(slot) => (
                slot,
                cfg.num_locals(),
                self.type_pool().abi_slot_count(place.base_type),
                None,
            ),
            PlaceBase::Param(slot) => {
                // A by-reference borrow/inout parameter occupies one physical
                // ABI slot regardless of the logical pointee width. Slices are
                // passed by value and therefore have no by-ref bit here.
                let width = if cfg.is_param_by_ref(slot) {
                    1
                } else {
                    self.type_pool().abi_slot_count(place.base_type)
                };
                (slot, cfg.num_params(), width, Some(slot))
            }
            PlaceBase::Indirect(_) => return None,
            PlaceBase::Accessor(call)
                if (call.as_u32() as usize) < cfg.value_count()
                    && matches!(cfg.get_inst(call).data, CfgInstData::AccessorCall { .. }) =>
            {
                return None;
            }
            PlaceBase::Accessor(_) => return Some(ContractViolationKind::UnsplicedAccessor),
        };
        let out_of_bounds = if width == 0 {
            // Zero-sized roots consume no logical slot, so the canonical base
            // may be exactly one past the final occupied slot.
            slot > limit
        } else {
            slot.checked_add(width).is_none_or(|end| end > limit)
        };
        if out_of_bounds {
            return Some(ContractViolationKind::PlaceBaseOutOfBounds);
        }
        if access == PlaceAccess::Write
            && param_slot.is_some_and(|slot| !cfg.is_param_writable(slot))
        {
            return Some(ContractViolationKind::PlaceBaseNotWritable);
        }
        None
    }

    fn intrinsic_arg_is_place(&self, cfg: &Cfg, value: CfgValue) -> bool {
        match &cfg.get_inst(value).data {
            CfgInstData::Load { slot } => *slot < cfg.num_locals(),
            CfgInstData::Param { index } => *index < cfg.num_params(),
            CfgInstData::PlaceRead { place } => {
                self.place_base_violation(cfg, place, PlaceAccess::Read)
                    .is_none()
                    && self.place_projection_metadata_is_valid(
                        cfg,
                        place,
                        cfg.get_inst(value).ty,
                        PlaceAccess::Read,
                    )
            }
            _ => false,
        }
    }

    fn is_inout_writeback_place(&self, cfg: &Cfg, value: CfgValue, place: &Place) -> bool {
        if !self.place_projection_metadata_is_valid(
            cfg,
            place,
            cfg.get_inst(value).ty,
            PlaceAccess::Read,
        ) {
            return false;
        }
        self.place_base_violation(cfg, place, PlaceAccess::Write)
            .is_none()
    }

    fn intrinsic_arg_is_field_place(&self, cfg: &Cfg, value: CfgValue) -> bool {
        let CfgInstData::PlaceRead { place } = &cfg.get_inst(value).data else {
            return false;
        };
        self.intrinsic_arg_is_place(cfg, value)
            && matches!(
                cfg.get_place_projections(place).last(),
                Some(Projection::Field { .. })
            )
    }

    fn classify_unsupported_intrinsic(
        &self,
        cfg: &Cfg,
        _intrinsic: CfgValue,
        operation: rue_air::IntrinsicOperation,
        args: &[CfgValue],
        result_ty: Type,
    ) -> UnsupportedKind {
        let kind = unsupported_intrinsic_kind_for_operation(operation);
        if kind.model_gap().is_none() {
            return kind;
        }

        let arity_matches = match operation {
            rue_air::IntrinsicOperation::ReadLine
            | rue_air::IntrinsicOperation::RandomU32
            | rue_air::IntrinsicOperation::RandomU64
            | rue_air::IntrinsicOperation::ArgCount
            | rue_air::IntrinsicOperation::EnvCount => args.is_empty(),
            rue_air::IntrinsicOperation::PtrWrite
            | rue_air::IntrinsicOperation::PtrWriteUnaligned
            | rue_air::IntrinsicOperation::PtrOffset
            | rue_air::IntrinsicOperation::Alloc
            | rue_air::IntrinsicOperation::AllocZeroed => args.len() == 2,
            rue_air::IntrinsicOperation::ByteCopy
            | rue_air::IntrinsicOperation::ByteMove
            | rue_air::IntrinsicOperation::ByteSet
            | rue_air::IntrinsicOperation::Free => args.len() == 3,
            rue_air::IntrinsicOperation::Realloc | rue_air::IntrinsicOperation::Resize => {
                args.len() == 4
            }
            rue_air::IntrinsicOperation::Syscall => (1..=7).contains(&args.len()),
            rue_air::IntrinsicOperation::Panic
            | rue_air::IntrinsicOperation::AssertFailed
            | rue_air::IntrinsicOperation::BoundsCheck
            | rue_air::IntrinsicOperation::DebugI64
            | rue_air::IntrinsicOperation::DebugU64
            | rue_air::IntrinsicOperation::DebugBool
            | rue_air::IntrinsicOperation::DebugStr
            | rue_air::IntrinsicOperation::ParseI32
            | rue_air::IntrinsicOperation::ParseI64
            | rue_air::IntrinsicOperation::ParseU32
            | rue_air::IntrinsicOperation::ParseU64
            | rue_air::IntrinsicOperation::PtrToInt
            | rue_air::IntrinsicOperation::IntToPtr
            | rue_air::IntrinsicOperation::PtrRead
            | rue_air::IntrinsicOperation::PtrReadUnaligned
            | rue_air::IntrinsicOperation::ArgPtr
            | rue_air::IntrinsicOperation::ArgLen
            | rue_air::IntrinsicOperation::EnvPtr
            | rue_air::IntrinsicOperation::EnvLen
            | rue_air::IntrinsicOperation::Raw
            | rue_air::IntrinsicOperation::RawMut
            | rue_air::IntrinsicOperation::FieldPtr
            | rue_air::IntrinsicOperation::IntToFloat
            | rue_air::IntrinsicOperation::FloatToInt
            | rue_air::IntrinsicOperation::FloatCast
            | rue_air::IntrinsicOperation::BitCast => args.len() == 1,
            rue_air::IntrinsicOperation::TotalCmp => args.len() == 2,
            rue_air::IntrinsicOperation::PanicNoMessage => args.is_empty(),
        };
        if !arity_matches {
            return UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity);
        }

        let ty = |index: usize| cfg.get_inst(args[index]).ty;
        let is_text =
            |index: usize| self.is_str_like_type(ty(index)) || self.is_owned_string_type(ty(index));
        let validated_kind = kind;
        let signature_matches = match operation {
            rue_air::IntrinsicOperation::ParseI32 => {
                is_text(0) && self.is_option_of(result_ty, Type::I32)
            }
            rue_air::IntrinsicOperation::ParseI64 => {
                is_text(0) && self.is_option_of(result_ty, Type::I64)
            }
            rue_air::IntrinsicOperation::ParseU32 => {
                is_text(0) && self.is_option_of(result_ty, Type::U32)
            }
            rue_air::IntrinsicOperation::ParseU64 => {
                is_text(0) && self.is_option_of(result_ty, Type::U64)
            }
            rue_air::IntrinsicOperation::PtrRead
            | rue_air::IntrinsicOperation::PtrReadUnaligned => self
                .pointer_pointee(ty(0))
                .is_some_and(|(pointee, _)| pointee == result_ty),
            rue_air::IntrinsicOperation::PtrWrite
            | rue_air::IntrinsicOperation::PtrWriteUnaligned => self
                .pointer_pointee(ty(0))
                .is_some_and(|(pointee, mutable)| {
                    mutable && (pointee == ty(1) || ty(1) == Type::NEVER) && result_ty == Type::UNIT
                }),
            rue_air::IntrinsicOperation::PtrOffset => {
                (ty(1).is_integer() || ty(1) == Type::NEVER)
                    && if ty(0) == Type::NEVER {
                        result_ty == Type::NEVER
                    } else {
                        self.pointer_pointee(ty(0)).is_some() && result_ty == ty(0)
                    }
            }
            rue_air::IntrinsicOperation::PtrToInt => {
                (self.pointer_pointee(ty(0)).is_some() || ty(0) == Type::NEVER)
                    && result_ty == Type::U64
            }
            rue_air::IntrinsicOperation::IntToPtr => {
                (ty(0) == Type::U64 || ty(0) == Type::NEVER)
                    && (result_ty == Type::NEVER
                        || self
                            .pointer_pointee(result_ty)
                            .is_some_and(|(_, mutable)| mutable))
            }
            rue_air::IntrinsicOperation::Raw => {
                self.intrinsic_arg_is_place(cfg, args[0])
                    && self
                        .pointer_pointee(result_ty)
                        .is_some_and(|(pointee, mutable)| !mutable && pointee == ty(0))
            }
            rue_air::IntrinsicOperation::RawMut => {
                self.intrinsic_arg_is_place(cfg, args[0])
                    && self
                        .pointer_pointee(result_ty)
                        .is_some_and(|(pointee, mutable)| mutable && pointee == ty(0))
            }
            rue_air::IntrinsicOperation::FieldPtr => {
                self.intrinsic_arg_is_field_place(cfg, args[0])
                    && self
                        .pointer_pointee(result_ty)
                        .is_some_and(|(pointee, mutable)| mutable && pointee == ty(0))
            }
            rue_air::IntrinsicOperation::Alloc | rue_air::IntrinsicOperation::AllocZeroed => {
                ty(0) == Type::U64
                    && ty(1) == Type::U64
                    && self.pointer_pointee(result_ty) == Some((Type::U8, true))
            }
            rue_air::IntrinsicOperation::Free => {
                self.pointer_pointee(ty(0)) == Some((Type::U8, true))
                    && ty(1) == Type::U64
                    && ty(2) == Type::U64
                    && result_ty == Type::UNIT
            }
            rue_air::IntrinsicOperation::Realloc | rue_air::IntrinsicOperation::Resize => {
                // `(p, old_size, align, new_size)`; `@realloc` hands back the
                // (possibly moved) block, `@resize` reports in-place success.
                self.pointer_pointee(ty(0)) == Some((Type::U8, true))
                    && ty(1) == Type::U64
                    && ty(2) == Type::U64
                    && ty(3) == Type::U64
                    && result_ty
                        == if operation == rue_air::IntrinsicOperation::Resize {
                            Type::BOOL
                        } else {
                            ty(0)
                        }
            }
            rue_air::IntrinsicOperation::ByteCopy | rue_air::IntrinsicOperation::ByteMove => {
                self.pointer_pointee(ty(0)) == Some((Type::U8, true))
                    && self
                        .pointer_pointee(ty(1))
                        .is_some_and(|(pointee, _)| pointee == Type::U8)
                    && ty(2) == Type::U64
                    && result_ty == Type::UNIT
            }
            rue_air::IntrinsicOperation::ByteSet => {
                self.pointer_pointee(ty(0)) == Some((Type::U8, true))
                    && ty(1) == Type::U8
                    && ty(2) == Type::U64
                    && result_ty == Type::UNIT
            }
            rue_air::IntrinsicOperation::ReadLine => self
                .option_payload(result_ty)
                .is_some_and(|payload| self.is_owned_string_type(payload)),
            rue_air::IntrinsicOperation::RandomU32 => result_ty == Type::U32,
            rue_air::IntrinsicOperation::RandomU64 => result_ty == Type::U64,
            rue_air::IntrinsicOperation::ArgCount | rue_air::IntrinsicOperation::EnvCount => {
                result_ty == Type::U64
            }
            rue_air::IntrinsicOperation::ArgLen | rue_air::IntrinsicOperation::EnvLen => {
                ty(0) == Type::U64 && result_ty == Type::U64
            }
            rue_air::IntrinsicOperation::ArgPtr | rue_air::IntrinsicOperation::EnvPtr => {
                ty(0) == Type::U64 && self.pointer_pointee(result_ty) == Some((Type::U8, true))
            }
            rue_air::IntrinsicOperation::Syscall => {
                args.iter().all(|arg| {
                    let ty = cfg.get_inst(*arg).ty;
                    ty == Type::U64 || ty == Type::NEVER
                }) && result_ty == Type::I64
            }
            rue_air::IntrinsicOperation::IntToFloat => {
                (ty(0).is_integer() || ty(0) == Type::NEVER) && result_ty.is_float()
            }
            rue_air::IntrinsicOperation::FloatToInt => {
                (ty(0).is_float() || ty(0) == Type::NEVER) && result_ty.is_integer()
            }
            rue_air::IntrinsicOperation::FloatCast => {
                (ty(0) == Type::NEVER && result_ty.is_float())
                    || (ty(0).is_float() && result_ty.is_float() && ty(0) != result_ty)
            }
            rue_air::IntrinsicOperation::PanicNoMessage
            | rue_air::IntrinsicOperation::Panic
            | rue_air::IntrinsicOperation::AssertFailed
            | rue_air::IntrinsicOperation::BoundsCheck
            | rue_air::IntrinsicOperation::DebugI64
            | rue_air::IntrinsicOperation::DebugU64
            | rue_air::IntrinsicOperation::DebugBool
            | rue_air::IntrinsicOperation::DebugStr
            | rue_air::IntrinsicOperation::TotalCmp
            | rue_air::IntrinsicOperation::BitCast => false,
        };
        if signature_matches {
            validated_kind
        } else {
            UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature)
        }
    }

    /// Validate every static part of `@panic` / `@assert` before evaluating an
    /// operand. This keeps malformed outer CFG from being hidden by an
    /// external-dependency or model-gap operand and mirrors the native
    /// lowering's exact ABI assumptions.
    fn preflight_abort_intrinsic(
        &self,
        cfg: &Cfg,
        operation: rue_air::IntrinsicOperation,
        args: &[CfgValue],
        result_ty: Type,
    ) -> Step<Option<AbortIntrinsic>> {
        let intrinsic = match operation {
            rue_air::IntrinsicOperation::PanicNoMessage | rue_air::IntrinsicOperation::Panic => {
                AbortIntrinsic::Panic
            }
            rue_air::IntrinsicOperation::AssertFailed => AbortIntrinsic::Assert,
            rue_air::IntrinsicOperation::BoundsCheck
            | rue_air::IntrinsicOperation::DebugI64
            | rue_air::IntrinsicOperation::DebugU64
            | rue_air::IntrinsicOperation::DebugBool
            | rue_air::IntrinsicOperation::DebugStr
            | rue_air::IntrinsicOperation::ReadLine
            | rue_air::IntrinsicOperation::ParseI32
            | rue_air::IntrinsicOperation::ParseI64
            | rue_air::IntrinsicOperation::ParseU32
            | rue_air::IntrinsicOperation::ParseU64
            | rue_air::IntrinsicOperation::RandomU32
            | rue_air::IntrinsicOperation::RandomU64
            | rue_air::IntrinsicOperation::PtrToInt
            | rue_air::IntrinsicOperation::IntToPtr
            | rue_air::IntrinsicOperation::PtrRead
            | rue_air::IntrinsicOperation::PtrReadUnaligned
            | rue_air::IntrinsicOperation::PtrWrite
            | rue_air::IntrinsicOperation::PtrWriteUnaligned
            | rue_air::IntrinsicOperation::PtrOffset
            | rue_air::IntrinsicOperation::Alloc
            | rue_air::IntrinsicOperation::AllocZeroed
            | rue_air::IntrinsicOperation::Free
            | rue_air::IntrinsicOperation::Realloc
            | rue_air::IntrinsicOperation::Resize
            | rue_air::IntrinsicOperation::ByteCopy
            | rue_air::IntrinsicOperation::ByteMove
            | rue_air::IntrinsicOperation::ByteSet
            | rue_air::IntrinsicOperation::ArgCount
            | rue_air::IntrinsicOperation::ArgPtr
            | rue_air::IntrinsicOperation::ArgLen
            | rue_air::IntrinsicOperation::EnvCount
            | rue_air::IntrinsicOperation::EnvPtr
            | rue_air::IntrinsicOperation::EnvLen
            | rue_air::IntrinsicOperation::Raw
            | rue_air::IntrinsicOperation::RawMut
            | rue_air::IntrinsicOperation::FieldPtr
            | rue_air::IntrinsicOperation::Syscall
            | rue_air::IntrinsicOperation::IntToFloat
            | rue_air::IntrinsicOperation::FloatToInt
            | rue_air::IntrinsicOperation::FloatCast
            | rue_air::IntrinsicOperation::TotalCmp
            | rue_air::IntrinsicOperation::BitCast => return Ok(None),
        };
        let arity_matches = match operation {
            rue_air::IntrinsicOperation::PanicNoMessage => args.is_empty(),
            rue_air::IntrinsicOperation::Panic => args.len() == 1,
            rue_air::IntrinsicOperation::AssertFailed => args.len() == 1,
            rue_air::IntrinsicOperation::BoundsCheck
            | rue_air::IntrinsicOperation::DebugI64
            | rue_air::IntrinsicOperation::DebugU64
            | rue_air::IntrinsicOperation::DebugBool
            | rue_air::IntrinsicOperation::DebugStr
            | rue_air::IntrinsicOperation::ReadLine
            | rue_air::IntrinsicOperation::ParseI32
            | rue_air::IntrinsicOperation::ParseI64
            | rue_air::IntrinsicOperation::ParseU32
            | rue_air::IntrinsicOperation::ParseU64
            | rue_air::IntrinsicOperation::RandomU32
            | rue_air::IntrinsicOperation::RandomU64
            | rue_air::IntrinsicOperation::PtrToInt
            | rue_air::IntrinsicOperation::IntToPtr
            | rue_air::IntrinsicOperation::PtrRead
            | rue_air::IntrinsicOperation::PtrReadUnaligned
            | rue_air::IntrinsicOperation::PtrWrite
            | rue_air::IntrinsicOperation::PtrWriteUnaligned
            | rue_air::IntrinsicOperation::PtrOffset
            | rue_air::IntrinsicOperation::Alloc
            | rue_air::IntrinsicOperation::AllocZeroed
            | rue_air::IntrinsicOperation::Free
            | rue_air::IntrinsicOperation::Realloc
            | rue_air::IntrinsicOperation::Resize
            | rue_air::IntrinsicOperation::ByteCopy
            | rue_air::IntrinsicOperation::ByteMove
            | rue_air::IntrinsicOperation::ByteSet
            | rue_air::IntrinsicOperation::ArgCount
            | rue_air::IntrinsicOperation::ArgPtr
            | rue_air::IntrinsicOperation::ArgLen
            | rue_air::IntrinsicOperation::EnvCount
            | rue_air::IntrinsicOperation::EnvPtr
            | rue_air::IntrinsicOperation::EnvLen
            | rue_air::IntrinsicOperation::Raw
            | rue_air::IntrinsicOperation::RawMut
            | rue_air::IntrinsicOperation::FieldPtr
            | rue_air::IntrinsicOperation::Syscall
            | rue_air::IntrinsicOperation::IntToFloat
            | rue_air::IntrinsicOperation::FloatToInt
            | rue_air::IntrinsicOperation::FloatCast
            | rue_air::IntrinsicOperation::TotalCmp
            | rue_air::IntrinsicOperation::BitCast => false,
        };
        if !arity_matches {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity),
                format!("intrinsic @{} arity", operation.expected_spelling()),
            ));
        }

        let ty = |index: usize| cfg.get_inst(args[index]).ty;
        let signature_matches = match intrinsic {
            AbortIntrinsic::Panic => {
                result_ty == PANIC_CFG_RESULT_TYPE
                    && (args.is_empty()
                        || self.is_str_like_type(ty(0))
                        || self.is_owned_string_type(ty(0)))
            }
            // The conditional `assert` intrinsic carries its condition and
            // nothing else since `@assert` moved to the §5.1 report
            // (RUE-1953); a comptime-decidable `@assert_eq` is what still
            // produces it.
            AbortIntrinsic::Assert => result_ty == Type::UNIT && ty(0) == Type::BOOL,
        };
        if !signature_matches {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                format!("intrinsic @{} signature", operation.expected_spelling()),
            ));
        }
        Ok(Some(intrinsic))
    }

    fn abort_with_stderr(&self, kind: TrapKind, parts: &[&[u8]]) -> Step<Value> {
        let raw_stderr_bytes = parts
            .iter()
            .try_fold(0usize, |total, part| total.checked_add(part.len()));
        let Some(raw_stderr_bytes) = raw_stderr_bytes else {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::StderrBytes),
                format!(
                    "stderr byte limit exceeded ({}-byte limit)",
                    self.stderr_cap
                ),
            ));
        };
        if raw_stderr_bytes > self.stderr_cap {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::StderrBytes),
                format!(
                    "stderr byte limit exceeded ({}-byte limit)",
                    self.stderr_cap
                ),
            ));
        }

        // Decode exactly as the native differential runner does. Capacity is
        // bounded by the raw-byte cap; invalid UTF-8 can expand to replacement
        // characters but by at most a small constant factor.
        let mut stderr = String::with_capacity(raw_stderr_bytes);
        for part in parts {
            stderr.push_str(&String::from_utf8_lossy(part));
        }
        Err(Flow::Panic(Panic::with_stderr(
            kind,
            stderr,
            raw_stderr_bytes,
        )))
    }

    fn eval_abort_intrinsic(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        intrinsic: AbortIntrinsic,
        args: &[CfgValue],
    ) -> Step<Value> {
        // Native lowering materializes both `@assert` operands before testing
        // the condition. Keep that eager, source-ordered behavior even on the
        // true path.
        let values = self.eval_all(cfg, frame, args)?;
        match intrinsic {
            AbortIntrinsic::Panic => match values.as_slice() {
                [] => self.abort_with_stderr(TrapKind::UserPanic, &[b"panic\n"]),
                [message] if Self::text_ptr_len(message).is_some() => {
                    // The message is a materialized `str` view; read its bytes
                    // from the heap before aborting (RUE-1010 §6.13).
                    let bytes = self.text_bytes(message)?;
                    self.abort_with_stderr(TrapKind::UserPanic, &[b"panic: ", &bytes, b"\n"])
                }
                [_] => Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                    "intrinsic @panic runtime value shape",
                )),
                _ => unreachable!("@panic arity was preflighted"),
            },
            AbortIntrinsic::Assert => {
                let [Value::Bool(condition)] = values.as_slice() else {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::IntrinsicSignature,
                        ),
                        "intrinsic @assert runtime value shape",
                    ));
                };
                if *condition {
                    Ok(Value::Unit)
                } else {
                    self.abort_with_stderr(TrapKind::AssertionFailure, &[b"assertion failed\n"])
                }
            }
        }
    }

    /// Validate and execute the compiler-inserted bounds check carried by the
    /// existing `assert` intrinsic shape. Its typed runtime identity is what
    /// distinguishes a slice check from source-level `@assert` when the oracle
    /// assigns a trap category.
    fn eval_bounds_check_intrinsic(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        operation: rue_air::IntrinsicOperation,
        args: &[CfgValue],
        result_ty: Type,
    ) -> Step<Value> {
        if operation != rue_air::IntrinsicOperation::BoundsCheck
            || result_ty != Type::UNIT
            || args.len() != 1
        {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                "compiler bounds-check intrinsic signature",
            ));
        }
        if cfg.get_inst(args[0]).ty != Type::BOOL {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                "compiler bounds-check intrinsic condition",
            ));
        }
        let values = self.eval_all(cfg, frame, args)?;
        let [Value::Bool(condition)] = values.as_slice() else {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                "compiler bounds-check intrinsic runtime condition",
            ));
        };
        if *condition {
            Ok(Value::Unit)
        } else {
            self.abort_with_stderr(
                TrapKind::IndexOutOfBounds,
                &[b"error: index out of bounds\n"],
            )
        }
    }

    /// Evaluate an already typed debug operation. The operation selects the
    /// runtime contract; the operand type is retained only for integer-width
    /// formatting and text layout after that exact contract has passed.
    fn eval_debug_intrinsic(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        operation: rue_air::IntrinsicOperation,
        args: &[CfgValue],
        result_ty: Type,
    ) -> Step<Value> {
        let [arg] = args else {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::DebugArity),
                "@dbg arity",
            ));
        };
        let arg_ty = cfg.get_inst(*arg).ty;
        if !operation.validate_call(
            self.type_pool(),
            &[rue_air::IntrinsicAirArgument::value(
                arg_ty,
                rue_air::AirArgMode::Normal,
            )],
            result_ty,
        ) {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                "@dbg typed operation signature",
            ));
        }
        let val = self.eval(cfg, frame, *arg)?;
        match operation {
            rue_air::IntrinsicOperation::DebugI64 => self.write_dbg(&val, arg_ty)?,
            rue_air::IntrinsicOperation::DebugU64 => self.write_dbg(&val, arg_ty)?,
            rue_air::IntrinsicOperation::DebugBool => self.write_dbg(&val, arg_ty)?,
            rue_air::IntrinsicOperation::DebugStr => self.write_dbg(&val, arg_ty)?,
            rue_air::IntrinsicOperation::PanicNoMessage
            | rue_air::IntrinsicOperation::Panic
            | rue_air::IntrinsicOperation::AssertFailed
            | rue_air::IntrinsicOperation::BoundsCheck
            | rue_air::IntrinsicOperation::ReadLine
            | rue_air::IntrinsicOperation::ParseI32
            | rue_air::IntrinsicOperation::ParseI64
            | rue_air::IntrinsicOperation::ParseU32
            | rue_air::IntrinsicOperation::ParseU64
            | rue_air::IntrinsicOperation::RandomU32
            | rue_air::IntrinsicOperation::RandomU64
            | rue_air::IntrinsicOperation::PtrToInt
            | rue_air::IntrinsicOperation::IntToPtr
            | rue_air::IntrinsicOperation::PtrRead
            | rue_air::IntrinsicOperation::PtrReadUnaligned
            | rue_air::IntrinsicOperation::PtrWrite
            | rue_air::IntrinsicOperation::PtrWriteUnaligned
            | rue_air::IntrinsicOperation::PtrOffset
            | rue_air::IntrinsicOperation::Alloc
            | rue_air::IntrinsicOperation::AllocZeroed
            | rue_air::IntrinsicOperation::Free
            | rue_air::IntrinsicOperation::Realloc
            | rue_air::IntrinsicOperation::Resize
            | rue_air::IntrinsicOperation::ByteCopy
            | rue_air::IntrinsicOperation::ByteMove
            | rue_air::IntrinsicOperation::ByteSet
            | rue_air::IntrinsicOperation::ArgCount
            | rue_air::IntrinsicOperation::ArgPtr
            | rue_air::IntrinsicOperation::ArgLen
            | rue_air::IntrinsicOperation::EnvCount
            | rue_air::IntrinsicOperation::EnvPtr
            | rue_air::IntrinsicOperation::EnvLen
            | rue_air::IntrinsicOperation::Raw
            | rue_air::IntrinsicOperation::RawMut
            | rue_air::IntrinsicOperation::FieldPtr
            | rue_air::IntrinsicOperation::Syscall
            | rue_air::IntrinsicOperation::IntToFloat
            | rue_air::IntrinsicOperation::FloatToInt
            | rue_air::IntrinsicOperation::FloatCast
            | rue_air::IntrinsicOperation::TotalCmp
            | rue_air::IntrinsicOperation::BitCast => {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::UnexpectedIntrinsic),
                    "non-debug operation reached debug evaluation",
                ));
            }
        }
        Ok(Value::Unit)
    }

    fn write_dbg(&mut self, val: &Value, ty: Type) -> Step<()> {
        let remaining = self.stdout_cap.saturating_sub(self.stdout_bytes);

        // `@dbg` of a text value prints its byte content (matches
        // __rue_dbg_str), read from the heap through the materialized header
        // (RUE-1010 §6.13); every other value uses the scalar `format_dbg`.
        // Reserve one byte for the trailing newline; the comparison form avoids
        // overflowing while computing `len + 1`, and rejecting an oversized
        // value before assembling output also bounds that temporary allocation.
        let output = if self.is_text_type(ty) {
            let bytes = self.text_bytes_bounded(val, Some(remaining.saturating_sub(1)))?;
            // Text output is a byte observation, not a string formatting
            // operation. Preserve invalid UTF-8 and every original byte in
            // the canonical trace; the public `Outcome.stdout` display may
            // decode it lossily only after execution has completed.
            bytes
        } else {
            let formatted = format_dbg(val, ty)?;
            formatted.into_owned().into_bytes()
        };
        if output.len() >= remaining {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                format!(
                    "stdout byte limit exceeded ({}-byte limit)",
                    self.stdout_cap
                ),
            ));
        }

        let mut observed = Vec::with_capacity(output.len() + 1);
        observed.extend_from_slice(&output);
        observed.push(b'\n');
        self.observe_stdout(&observed)
    }

    /// Append one ordered fd-1 observation, enforcing the shared raw-byte
    /// bound. A write crossing the bound records exactly the prefix that fits
    /// before returning a hard resource failure; callers never compare a
    /// silently truncated output as agreement.
    fn observe_stdout(&mut self, bytes: &[u8]) -> Step<()> {
        let remaining = self.stdout_cap.saturating_sub(self.stdout_bytes);
        let observed = bytes.len().min(remaining);
        self.stdout_trace.extend_from_slice(&bytes[..observed]);
        self.stdout_bytes += observed;
        if observed != bytes.len() {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                format!(
                    "stdout byte limit exceeded ({}-byte limit)",
                    self.stdout_cap
                ),
            ));
        }
        Ok(())
    }

    fn run(mut self) -> Result<Outcome, Unsupported> {
        match self.call("main", &[]) {
            Ok((v, _)) => Ok(Outcome {
                exit_code: (v.as_int() & 0xFF) as i32,
                stdout: String::from_utf8_lossy(&self.stdout_trace).into_owned(),
                stdout_bytes: self.stdout_trace.clone(),
                stderr: String::new(),
                panic: None,
            }),
            Err(Flow::Panic(panic)) => {
                if panic.raw_stderr_bytes > self.stderr_cap {
                    return Err(Unsupported::new(
                        UnsupportedKind::ResourceLimit(ResourceLimitKind::StderrBytes),
                        format!(
                            "stderr byte limit exceeded ({}-byte limit)",
                            self.stderr_cap
                        ),
                    ));
                }
                Ok(Outcome {
                    exit_code: 101,
                    stdout: String::from_utf8_lossy(&self.stdout_trace).into_owned(),
                    stdout_bytes: self.stdout_trace.clone(),
                    stderr: panic.stderr,
                    panic: Some(panic.kind),
                })
            }
            Err(Flow::Unsupported(u)) => Err(u),
        }
    }

    fn interner(&self) -> &ThreadedRodeo {
        &self.function().interner
    }

    /// Locate a callee by the internal symbol its call site names. This is the
    /// interpreter's dispatch, so it must speak the CFG's own symbol space,
    /// not source names.
    fn find_function(&self, name: &str) -> Option<usize> {
        self.state
            .functions
            .iter()
            .position(|function| function.cfg.fn_name() == name)
    }

    /// Number of physical parameter slots occupied by one CFG call argument.
    ///
    /// `CfgArgMode` is already the physical mode: sema rewrites by-value slice
    /// views to `Normal`, while real `borrow` / `inout` arguments carry one
    /// pointer regardless of the pointee's logical width.
    fn call_arg_slot_width(&self, ty: Type, mode: CfgArgMode) -> usize {
        // Consume the canonical native call-ABI classifier (ADR-0052 phase 5)
        // so the oracle's model of the argument contract agrees with the
        // compiler by construction rather than by coincidence. This is the
        // physical value-decomposition width (representation 2): by-value is
        // one slot per leaf, a physical `borrow` / `inout` is one pointer.
        let convention = match mode {
            CfgArgMode::Normal => rue_air::ArgConvention::ByValue,
            CfgArgMode::Inout | CfgArgMode::Borrow => rue_air::ArgConvention::ByReference,
        };
        rue_air::NativeCallAbi::for_arguments(self.type_pool()).arg_slot_width(ty, convention)
            as usize
    }

    /// Validate the caller's complete physical argument layout against the
    /// callee before any operand runs.
    ///
    /// Slot totals alone are insufficient: all three modes occupy one slot
    /// for scalar arguments, but disagree about whether that slot contains a
    /// value, a shared pointer, or a writable pointer. `Normal` deliberately
    /// ignores the writable bit because sema lowers first-class `borrow` /
    /// `inout` slice views to by-value fat pointers while retaining their
    /// source-level writability metadata on the callee slots.
    fn preflight_call_layout(
        &self,
        cfg: &Cfg,
        name: &str,
        args: impl IntoIterator<Item = (Type, CfgArgMode)>,
    ) -> Step<()> {
        let mut base = 0usize;
        let num_params = cfg.num_params() as usize;

        for (index, (ty, mode)) in args.into_iter().enumerate() {
            let width = self.call_arg_slot_width(ty, mode);
            let Some(end) = base.checked_add(width) else {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::CallParameterLayout),
                    format!("call to '{name}' argument {index} overflows the parameter layout"),
                ));
            };
            if end > num_params {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::CallParameterLayout),
                    format!(
                        "call to '{name}' argument {index} occupies parameter slots {base}..{end}, but the callee declares {num_params}"
                    ),
                ));
            }

            let mode_matches = match mode {
                CfgArgMode::Normal => (base..end).all(|slot| {
                    !cfg.is_param_by_ref(
                        u32::try_from(slot).expect("slot is within u32 CFG bounds"),
                    )
                }),
                CfgArgMode::Borrow => {
                    let slot = u32::try_from(base).expect("slot is within u32 CFG bounds");
                    cfg.is_param_by_ref(slot) && !cfg.is_param_writable(slot)
                }
                CfgArgMode::Inout => {
                    let slot = u32::try_from(base).expect("slot is within u32 CFG bounds");
                    cfg.is_param_by_ref(slot) && cfg.is_param_writable(slot)
                }
            };
            if !mode_matches {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::CallParameterLayout),
                    format!(
                        "call to '{name}' argument {index} has physical mode {mode:?}, which contradicts callee parameter slot {base}"
                    ),
                ));
            }

            base = end;
        }

        if base != num_params {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::CallParameterLayout),
                format!(
                    "call to '{name}' occupies {base} parameter slots, but the callee declares {num_params}"
                ),
            ));
        }
        Ok(())
    }

    /// Returns the call's value **and** its final parameter slots, so the caller
    /// can copy out `inout` parameters (Rue `inout` is copy-in / copy-out, which
    /// is observably identical to by-reference under the law of exclusivity).
    fn call(
        &mut self,
        name: &str,
        args: &[(Value, Type, CfgArgMode)],
    ) -> Step<(Value, Vec<Option<Value>>)> {
        // Bound recursion *depth* (shared across activations) before descending,
        // so unbounded Rue recursion resolves to a typed resource failure
        // instead of overflowing the Rust stack and aborting the process
        // (RUE-340). Decrement on every exit path by capturing the result.
        if self.depth >= MAX_DEPTH {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::RecursionDepth),
                "recursion depth budget exhausted",
            ));
        }
        self.depth += 1;
        let caller = self.state.current_function.load(Ordering::Relaxed);
        let result = self.call_inner(name, args);
        self.retire_promoted_allocations(self.depth);
        self.state.current_function.store(caller, Ordering::Relaxed);
        self.depth -= 1;
        result
    }

    fn call_accessor(
        &mut self,
        name: &str,
        args: &[(Value, Type, CfgArgMode)],
        param_places: HashMap<u32, PtrTarget>,
    ) -> Step<PtrTarget> {
        if self.depth >= MAX_DEPTH {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::RecursionDepth),
                "recursion depth budget exhausted",
            ));
        }
        self.depth += 1;
        let caller = self.state.current_function.load(Ordering::Relaxed);
        let result = self.call_inner_with_places(name, args, param_places, true);
        self.retire_promoted_allocations(self.depth);
        self.state.current_function.store(caller, Ordering::Relaxed);
        self.depth -= 1;
        let (value, _) = result?;
        match value {
            Value::Ptr(Some(target)) => Ok(target),
            _ => Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::UnsplicedAccessor),
                "accessor execution did not return a live place",
            )),
        }
    }

    fn retire_promoted_allocations(&mut self, owner_depth: u32) {
        for allocation in &mut self.heap {
            if allocation.origin == AllocationOrigin::Promoted
                && allocation.owner_depth == Some(owner_depth)
            {
                allocation.freed = true;
            }
        }
    }

    fn call_inner(
        &mut self,
        name: &str,
        args: &[(Value, Type, CfgArgMode)],
    ) -> Step<(Value, Vec<Option<Value>>)> {
        self.call_inner_with_places(name, args, HashMap::new(), false)
    }

    fn call_inner_with_places(
        &mut self,
        name: &str,
        args: &[(Value, Type, CfgArgMode)],
        param_places: HashMap<u32, PtrTarget>,
        place_return: bool,
    ) -> Step<(Value, Vec<Option<Value>>)> {
        let function = self.find_function(name).ok_or_else(|| {
            unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::MissingFunctionBody),
                format!("call to '{name}'"),
            )
        })?;
        let cfg = &*self.state.functions[function].cfg;
        self.preflight_call_layout(cfg, name, args.iter().map(|(_, ty, mode)| (*ty, *mode)))?;
        // Lay arguments out by physical slot: place each whole semantic value
        // at its base slot, then pad with `None` for extra by-value aggregate
        // slots. A zero-sized `Normal` argument occupies no slot; a physical
        // borrow/inout still occupies its one pointer slot.
        let mut param_slots: Vec<Option<Value>> = Vec::with_capacity(args.len());
        for (value, ty, mode) in args {
            // Parameter indices are a property of the static ABI contract,
            // never the active runtime value shape. A by-value enum reserves
            // its widest payload even when its current variant is a bare
            // discriminant; a physical borrow/inout is one pointer slot even
            // when its pointee is an aggregate. Slice views reach CFG as
            // `Normal`, because sema already materialized their by-value fat
            // pointer.
            let w = self.call_arg_slot_width(*ty, *mode);
            if w == 0 {
                continue;
            }
            param_slots.push(Some(value.clone()));
            for _ in 1..w {
                param_slots.push(None);
            }
        }
        debug_assert_eq!(param_slots.len(), cfg.num_params() as usize);
        // Every live handle in the callee CFG belongs to this function's
        // retained local domains. Argument widths above were intentionally
        // interpreted in the caller's domain before crossing the boundary.
        self.state
            .current_function
            .store(function, Ordering::Relaxed);
        let mut frame = Frame {
            params: param_slots,
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
            promoted: HashMap::new(),
            param_places,
            place_return,
        };

        let mut current = cfg.entry;
        let mut incoming: Vec<Value> = Vec::new();

        loop {
            let block = cfg.get_block(current);
            if incoming.len() != block.params.len() {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::BlockArgumentArity),
                    format!(
                        "block arg arity: received {}, expected {}",
                        incoming.len(),
                        block.params.len()
                    ),
                ));
            }
            // Re-entering a block (a loop back-edge) must recompute that
            // block's instructions and receive fresh block arguments. Keep
            // every other cached value: CFG SSA permits dominated blocks to
            // use values produced by executed dominators without threading
            // them through every intervening block, and recomputing a Load
            // after a mutation would change its evaluation-time value.
            for &(param, _) in &block.params {
                frame.cache.remove(&param.as_u32());
            }
            for &inst in &block.insts {
                frame.cache.remove(&inst.as_u32());
            }
            for (i, (pv, _)) in block.params.iter().enumerate() {
                let val = incoming.get(i).cloned().ok_or_else(|| {
                    unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::BlockArgumentArity,
                        ),
                        "block arg arity",
                    )
                })?;
                frame.cache.insert(pv.as_u32(), val);
            }
            for &v in &block.insts {
                // Decrement the shared total-work budget (see `STEP_BUDGET`): a
                // runaway loop or deep recursion reports `Unsupported` here
                // rather than hanging.
                self.budget = self.budget.checked_sub(1).ok_or_else(|| {
                    unsupported(
                        UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
                        "step budget exhausted",
                    )
                })?;
                self.eval(cfg, &mut frame, v)?;
            }

            let term = &block.terminator;
            match term {
                Terminator::Return { value } => {
                    let ret = match (frame.place_return, value) {
                        (true, Some(v)) => {
                            Value::Ptr(Some(self.returned_place_target(cfg, &mut frame, *v)?))
                        }
                        (true, None) => {
                            return Err(unsupported(
                                UnsupportedKind::ContractViolation(
                                    ContractViolationKind::UnsplicedAccessor,
                                ),
                                "accessor returned without a yielded place",
                            ));
                        }
                        (false, Some(v)) => self.eval(cfg, &mut frame, *v)?,
                        (false, None) => Value::Unit,
                    };
                    let mut final_params = std::mem::take(&mut frame.params);
                    // A normal `inout` call copies its final parameter value
                    // out at return. If the callee took the parameter's
                    // address, its canonical bytes (rather than the copy-in
                    // snapshot) are authoritative for that writeback.
                    for (&key, &alloc) in &frame.promoted {
                        if key >> 32 == 1 {
                            let slot = (key & u32::MAX as u64) as usize;
                            if let Some(value) = final_params.get_mut(slot) {
                                *value = Some(self.promoted_slot_value(alloc)?);
                            }
                        }
                    }
                    return Ok((ret, final_params));
                }
                Terminator::Goto { target, .. } => {
                    let ce = cfg.get_goto_args(term).to_vec();
                    incoming = self.eval_all(cfg, &mut frame, &ce)?;
                    current = *target;
                }
                Terminator::Branch {
                    cond,
                    then_block,
                    else_block,
                    ..
                } => {
                    let c = self.eval(cfg, &mut frame, *cond)?.as_bool();
                    let args = if c {
                        cfg.get_branch_then_args(term)
                    } else {
                        cfg.get_branch_else_args(term)
                    }
                    .to_vec();
                    incoming = self.eval_all(cfg, &mut frame, &args)?;
                    current = if c { *then_block } else { *else_block };
                }
                Terminator::Switch {
                    scrutinee, default, ..
                } => {
                    // The scrutinee is an integer, a discriminant-only enum
                    // (`Int` tag), or a payload-carrying enum (`Aggregate` whose
                    // element 0 is the tag, RUE-285). Switch on the discriminant.
                    let s = match self.eval(cfg, &mut frame, *scrutinee)? {
                        Value::Aggregate(elems) => elems.first().map(Value::as_int).unwrap_or(0),
                        other => other.as_int(),
                    };
                    let cases = cfg.get_switch_cases(term);
                    // Case values are stored as i64 bit patterns; compare by the
                    // 64-bit pattern so unsigned extremes (u64::MAX -> -1 as i64)
                    // and i64::MIN match the scrutinee regardless of signedness.
                    let s_bits = s as i64 as u64;
                    current = cases
                        .iter()
                        .find(|(val, _)| *val as u64 == s_bits)
                        .map(|(_, blk)| *blk)
                        .unwrap_or(*default);
                    incoming = Vec::new();
                }
                Terminator::Unreachable => {
                    return Err(Flow::Panic(Panic::runtime(TrapKind::Unreachable)));
                }
                Terminator::None => {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::MissingTerminator,
                        ),
                        "terminator None",
                    ));
                }
            }
        }
    }

    fn string_builtin_arity(kind: RuntimeCallKind) -> Option<usize> {
        match kind {
            RuntimeCallKind::ToString | RuntimeCallKind::ToStringUnsigned => Some(1),
            RuntimeCallKind::StrByteAt => Some(2),
            RuntimeCallKind::StrCharScalar
            | RuntimeCallKind::StrCharScalarLossy
            | RuntimeCallKind::StrCharNext
            | RuntimeCallKind::StrCharNextLossy => Some(3),
            _ => None,
        }
    }

    fn is_str_view_type(&self, ty: Type) -> bool {
        let TypeKind::Struct(struct_id) = ty.kind() else {
            return false;
        };
        self.text_struct_slots(struct_id) == Some(2)
    }

    /// Check every static part of a builtin call before evaluating operands.
    /// This ordering is deliberate: malformed outer CFG must not be hidden by
    /// an otherwise registrable model gap reached while evaluating an operand.
    fn preflight_string_builtin(
        &self,
        kind: RuntimeCallKind,
        arg_types: &[Type],
        arg_modes: &[CfgArgMode],
        result_ty: Type,
    ) -> Step<bool> {
        let Some(expected) = Self::string_builtin_arity(kind) else {
            return Ok(false);
        };
        let name = kind.helper().symbol();
        if arg_types.len() != expected || arg_modes.len() != expected {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArity),
                format!(
                    "builtin '{name}' has {} typed args and {} modes, oracle models {expected}",
                    arg_types.len(),
                    arg_modes.len()
                ),
            ));
        }
        if !arg_modes.iter().all(|mode| *mode == CfgArgMode::Normal) {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentMode),
                format!("builtin '{name}' argument mode drift"),
            ));
        }
        let argument_types_match = match kind {
            RuntimeCallKind::ToString => arg_types[0] == Type::I64,
            RuntimeCallKind::ToStringUnsigned => arg_types[0] == Type::U64,
            RuntimeCallKind::StrByteAt => {
                self.is_str_view_type(arg_types[0]) && arg_types[1].is_integer()
            }
            RuntimeCallKind::StrCharScalar
            | RuntimeCallKind::StrCharScalarLossy
            | RuntimeCallKind::StrCharNext
            | RuntimeCallKind::StrCharNextLossy => {
                self.pointer_pointee(arg_types[0])
                    .is_some_and(|(pointee, _)| pointee == Type::U8)
                    && arg_types[1] == Type::U64
                    && arg_types[2] == Type::U64
            }
            _ => unreachable!("arity and signature tables must stay exhaustive"),
        };
        if !argument_types_match {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType),
                format!("builtin '{name}' argument type drift"),
            ));
        }
        let result_type_matches = match kind {
            RuntimeCallKind::ToString | RuntimeCallKind::ToStringUnsigned => {
                self.is_owned_string_type(result_ty)
            }
            RuntimeCallKind::StrByteAt => result_ty == Type::U8,
            RuntimeCallKind::StrCharScalar | RuntimeCallKind::StrCharScalarLossy => {
                result_ty == Type::U32
            }
            RuntimeCallKind::StrCharNext | RuntimeCallKind::StrCharNextLossy => {
                result_ty == Type::U64
            }
            _ => unreachable!("arity and result tables must stay exhaustive"),
        };
        if !result_type_matches {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinResultType),
                format!("builtin '{name}' result type drift"),
            ));
        }
        Ok(true)
    }

    /// Dispatch a runtime text builtin. Returns `Ok(None)` when `name` is an
    /// ordinary source-defined function with a CFG body.
    fn string_builtin(
        &mut self,
        kind: RuntimeCallKind,
        args: &[Value],
        arg_types: &[Type],
        arg_modes: &[CfgArgMode],
        result_ty: Type,
    ) -> Step<Option<Value>> {
        if !self.preflight_string_builtin(kind, arg_types, arg_modes, result_ty)? {
            return Ok(None);
        }
        let name = kind.helper().symbol();
        if args.len() != arg_types.len() {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArity),
                format!("builtin '{name}' runtime argument count drift"),
            ));
        }
        let shapes_match = match kind {
            RuntimeCallKind::ToString | RuntimeCallKind::ToStringUnsigned => {
                matches!(args[0], Value::Int(_))
            }
            RuntimeCallKind::StrByteAt => {
                // The receiver is a materialized `str` view `{ptr, len}`
                // (RUE-1010 §6.13); the index is a scalar.
                Self::text_ptr_len(&args[0]).is_some() && matches!(args[1], Value::Int(_))
            }
            RuntimeCallKind::StrCharScalar
            | RuntimeCallKind::StrCharScalarLossy
            | RuntimeCallKind::StrCharNext
            | RuntimeCallKind::StrCharNextLossy => {
                // The projected-char path passes a raw text pointer, a length,
                // and a byte offset.
                matches!(args[0], Value::Ptr(_))
                    && matches!(args[1], Value::Int(_))
                    && matches!(args[2], Value::Int(_))
            }
            _ => unreachable!("preflight recognized this builtin"),
        };
        if !shapes_match {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType),
                format!("builtin '{name}' runtime argument shape drift"),
            ));
        }
        let out = match kind {
            RuntimeCallKind::ToString => {
                let digits = (args[0].as_int() as i64).to_string().into_bytes();
                let cap = digits.len() as i128;
                self.materialize_text(digits, self.text_value_slot_width(result_ty), cap)?
            }
            RuntimeCallKind::ToStringUnsigned => {
                let digits = (args[0].as_int() as u64).to_string().into_bytes();
                let cap = digits.len() as i128;
                self.materialize_text(digits, self.text_value_slot_width(result_ty), cap)?
            }
            RuntimeCallKind::StrByteAt => {
                let bytes = self.text_bytes(&args[0])?;
                let index = args[1].as_int();
                if index < 0 || index as u128 >= bytes.len() as u128 {
                    return Err(Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)));
                }
                Value::Int(bytes[index as usize] as i128)
            }
            RuntimeCallKind::StrCharScalar => {
                let bytes = self.bytes_from_ptr(&args[0], args[1].as_int())?;
                match char_at(&bytes, args[2].as_int()) {
                    Some((scalar, _)) => Value::Int(scalar as i128),
                    None => return Err(Flow::Panic(Panic::runtime(TrapKind::InvalidUtf8))),
                }
            }
            RuntimeCallKind::StrCharNext => {
                let bytes = self.bytes_from_ptr(&args[0], args[1].as_int())?;
                let offset = args[2].as_int();
                match char_at(&bytes, offset) {
                    Some((_, width)) => Value::Int(offset + width as i128),
                    None => return Err(Flow::Panic(Panic::runtime(TrapKind::InvalidUtf8))),
                }
            }
            RuntimeCallKind::StrCharScalarLossy => {
                let bytes = self.bytes_from_ptr(&args[0], args[1].as_int())?;
                Value::Int(char_at_lossy(&bytes, args[2].as_int()).0 as i128)
            }
            RuntimeCallKind::StrCharNextLossy => {
                let bytes = self.bytes_from_ptr(&args[0], args[1].as_int())?;
                let offset = args[2].as_int();
                Value::Int(offset + char_at_lossy(&bytes, offset).1 as i128)
            }
            _ => return Ok(None),
        };
        Ok(Some(out))
    }

    /// Read `len` bytes starting at a raw text pointer (the projected-char
    /// path's `ptr`/`len` pair), through the modeled allocation store.
    fn bytes_from_ptr(&self, ptr: &Value, len: i128) -> Step<Vec<u8>> {
        let gap = unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead);
        if len <= 0 {
            return Ok(Vec::new());
        }
        let Value::Ptr(Some(target)) = ptr else {
            return Err(unsupported(
                gap,
                "text pointer is not a live buffer pointer",
            ));
        };
        let mut bytes = Vec::with_capacity(len as usize);
        for i in 0..len {
            bytes.push(self.byte_at(target, i, gap)?);
        }
        Ok(bytes)
    }

    /// Run the drop of a value of type `ty`, in the spec-3.9 order: the type's
    /// user destructor first, then its fields (declaration order) / elements
    /// (ascending). Scalars are trivially droppable (no-op). The compiler's drop
    /// *elaboration* already decided where `drop` instructions live (suppressing
    /// moved values), so this only executes the cleanup for a value that should
    /// be dropped — it does not re-derive move analysis.
    fn run_drop(&mut self, ty: Type, v: Value) -> Step<()> {
        match ty.kind() {
            TypeKind::Struct(sid) => {
                let sd = self.type_pool().struct_def(sid);
                // Synthetic builtin types have no observable destructor and no
                // CFG body; skip them.
                if sd.is_builtin {
                    return Ok(());
                }
                let is_builtin = sd.is_builtin;
                let destructor = sd.destructor.clone();
                let fields = sd.fields.clone();
                if let Some(dtor) = destructor {
                    self.call(&dtor, &[(v.clone(), ty, CfgArgMode::Normal)])?;
                }
                // A builtin type's destructor is its entire drop glue; a
                // user struct then drops its fields in declaration order.
                if !is_builtin {
                    if let Value::Aggregate(elems) = &v {
                        for (i, field) in fields.iter().enumerate() {
                            if let Some(fv) = elems.get(i).cloned() {
                                self.run_drop(field.ty, fv)?;
                            }
                        }
                    }
                }
            }
            TypeKind::Array(aid) => {
                let (elem_ty, _len) = self.type_pool().array_def(aid);
                if let Value::Aggregate(elems) = &v {
                    for fv in elems.iter().cloned() {
                        self.run_drop(elem_ty, fv)?;
                    }
                }
            }
            // Dropping an enum runs the drop glue of its *active* variant's
            // payload (spec 6.3:20). The Aggregate layout is [tag, payload
            // fields...] (see `EnumVariant`), so element 0 selects the variant
            // and elements 1.. are its payload fields, in declaration order. A
            // discriminant-only value is a bare `Int` (no `Aggregate`), so there
            // is nothing to drop.
            TypeKind::Enum(eid) => {
                if let Value::Aggregate(elems) = &v
                    && let Some(Value::Int(tag)) = elems.first()
                {
                    let def = self.type_pool().enum_def(eid);
                    let payload_tys = def.variant_payload(*tag as usize).to_vec();
                    for (i, pty) in payload_tys.into_iter().enumerate() {
                        if let Some(fv) = elems.get(i + 1).cloned() {
                            self.run_drop(pty, fv)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Whether `ty` occupies ZERO by-value ABI parameter slots, matching the
    /// compiler's `abi_slot_count == 0` (rue-air typeck):
    /// unit/never/comptime types, structs whose fields are all
    /// zero-sized, and arrays that are empty or have zero-sized elements.
    /// Enums are never zero-sized (they always carry a discriminant slot).
    fn is_zero_sized(&self, ty: Type) -> bool {
        self.type_pool().abi_slot_count(ty) == 0
    }

    /// Whether equality for `ty` would eventually compare a float leaf.
    ///
    /// The oracle stores scalar values as integer bit patterns, so letting a
    /// float-bearing comparison reach `cmp` would silently model bit equality
    /// instead of IEEE equality. Detect the complete aggregate shape before
    /// evaluating either operand and report the same typed float semantic gap
    /// as scalar float operations.
    fn type_contains_float(&self, ty: Type) -> bool {
        if ty.is_float() {
            return true;
        }
        match ty.kind() {
            TypeKind::Struct(id) => self
                .type_pool()
                .struct_def(id)
                .fields
                .iter()
                .any(|field| self.type_contains_float(field.ty)),
            TypeKind::Array(id) => {
                let (element, len) = self.type_pool().array_def(id);
                len != 0 && self.type_contains_float(element)
            }
            TypeKind::Enum(id) => self
                .type_pool()
                .enum_def(id)
                .variant_payloads
                .iter()
                .flatten()
                .copied()
                .any(|payload| self.type_contains_float(payload)),
            _ => false,
        }
    }

    /// Classify a CFG operation as a well-typed float operation without
    /// evaluating its operands. Any instruction shape involving a float must
    /// satisfy the CFG typing contract before it can become registrable model
    /// debt; malformed mixed-class or result shapes remain hard failures.
    fn is_well_typed_float_operation(
        &self,
        cfg: &Cfg,
        data: &CfgInstData,
        result_ty: Type,
    ) -> Step<bool> {
        let binary_arithmetic = match data {
            CfgInstData::Add(a, b)
            | CfgInstData::Sub(a, b)
            | CfgInstData::Mul(a, b)
            | CfgInstData::Div(a, b) => Some((*a, *b)),
            _ => None,
        };
        let (involves_float, valid) = if let Some((a, b)) = binary_arithmetic {
            let a_ty = cfg.get_inst(a).ty;
            let b_ty = cfg.get_inst(b).ty;
            (
                result_ty.is_float() || a_ty.is_float() || b_ty.is_float(),
                result_ty.is_float() && a_ty == result_ty && b_ty == result_ty,
            )
        } else if let CfgInstData::Neg(value) = data {
            let value_ty = cfg.get_inst(*value).ty;
            (
                result_ty.is_float() || value_ty.is_float(),
                result_ty.is_float() && value_ty == result_ty,
            )
        } else {
            let comparison = match data {
                CfgInstData::Eq(a, b)
                | CfgInstData::Ne(a, b)
                | CfgInstData::Lt(a, b)
                | CfgInstData::Gt(a, b)
                | CfgInstData::Le(a, b)
                | CfgInstData::Ge(a, b) => Some((*a, *b)),
                _ => None,
            };
            let Some((a, b)) = comparison else {
                return Ok(false);
            };
            let a_ty = cfg.get_inst(a).ty;
            let b_ty = cfg.get_inst(b).ty;
            let equality = matches!(data, CfgInstData::Eq(..) | CfgInstData::Ne(..));
            let operand_has_float =
                self.type_contains_float(a_ty) || self.type_contains_float(b_ty);
            (
                result_ty.is_float() || operand_has_float,
                result_ty == Type::BOOL
                    && a_ty == b_ty
                    && if equality {
                        self.type_contains_float(a_ty)
                    } else {
                        a_ty.is_float()
                    },
            )
        };

        if involves_float && !valid {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::NonIntegerOperationType),
                "malformed float operation signature",
            ));
        }
        Ok(involves_float)
    }

    /// Materialize the (unique) value of a zero-sized type, preserving the
    /// aggregate shape so field/element projections still line up.
    fn zero_sized_value(&self, ty: Type) -> Value {
        match ty.kind() {
            TypeKind::Struct(sid) => {
                let sd = self.type_pool().struct_def(sid);
                Value::Aggregate(
                    sd.fields
                        .iter()
                        .map(|f| self.zero_sized_value(f.ty))
                        .collect(),
                )
            }
            TypeKind::Array(aid) => {
                let (elem_ty, len) = self.type_pool().array_def(aid);
                Value::Aggregate((0..len).map(|_| self.zero_sized_value(elem_ty)).collect())
            }
            _ => Value::Unit,
        }
    }

    fn eval_all(&mut self, cfg: &'a Cfg, frame: &mut Frame, vs: &[CfgValue]) -> Step<Vec<Value>> {
        vs.iter().map(|v| self.eval(cfg, frame, *v)).collect()
    }

    fn eval(&mut self, cfg: &'a Cfg, frame: &mut Frame, v: CfgValue) -> Step<Value> {
        if let Some(cached) = frame.cache.get(&v.as_u32()) {
            return Ok(cached.clone());
        }
        let inst = cfg.get_inst(v);
        let ty = inst.ty;
        if self.is_well_typed_float_operation(cfg, &inst.data, ty)? {
            return Err(unsupported(
                UnsupportedKind::SemanticGap(SemanticGapKind::FloatArithmetic),
                "scalar or aggregate float operation",
            ));
        }
        let result = match &inst.data {
            CfgInstData::Const(n) => {
                if *n == 0 && ty.is_ptr_const() && self.is_empty_slice_pointer(cfg, v) {
                    return Err(unsupported(
                        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
                            UnsupportedIntrinsicKind::EmptySlicePointer,
                        )),
                        "compiler-synthesized empty-slice null pointer",
                    ));
                }
                // The constant is stored as a 64-bit two's-complement bit
                // pattern (e.g. i32::MIN is `18446744071562067968` =
                // 0xFFFFFFFF80000000), so a signed type reinterprets it as
                // signed; an unsigned type takes the u64 value directly.
                if ty.is_signed() {
                    Value::Int(*n as i64 as i128)
                } else {
                    Value::Int(*n as i128)
                }
            }
            CfgInstData::BoolConst(b) => Value::Bool(*b),
            CfgInstData::StringConst(idx) => {
                let text = self
                    .function()
                    .strings
                    .get(*idx as usize)
                    .cloned()
                    .ok_or_else(|| {
                        unsupported(
                            UnsupportedKind::ContractViolation(
                                ContractViolationKind::StringConstantIndex,
                            ),
                            "string const index",
                        )
                    })?;
                self.string_literal_value(text, ty)?
            }
            CfgInstData::Param { index } => {
                if self.is_zero_sized(ty) {
                    // A zero-sized parameter occupies NO slot (abi_slot_count
                    // = 0), but the CFG still emits a Param read for it —
                    // sharing its `index` with the NEXT parameter's slot. Do
                    // not read the slot (that would grab the next parameter's
                    // value); materialize the unique ZST value instead.
                    self.zero_sized_value(ty)
                } else if let Some(target) = frame.param_places.get(index).cloned() {
                    // Raw accessor execution redirects by-reference parameter
                    // slots to their caller places, matching canonical
                    // accessor splicing for whole-parameter reads.
                    self.ptr_cell_read(
                        &target,
                        ty,
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::UnsplicedAccessor,
                        ),
                    )?
                } else if let Some(&a) =
                    frame.promoted.get(&promotion_key(PlaceBase::Param(*index)))
                {
                    // The parameter's address was taken; read through its heap
                    // allocation so a `@ptr_write` is observed on re-read.
                    self.promoted_slot_value(a)?
                } else {
                    match frame.params.get(*index as usize) {
                        Some(Some(value)) => value.clone(),
                        Some(None) => {
                            return Err(unsupported(
                                UnsupportedKind::SemanticGap(
                                    SemanticGapKind::FlattenedParameterSlot,
                                ),
                                "param index",
                            ));
                        }
                        None => {
                            return Err(unsupported(
                                UnsupportedKind::ContractViolation(
                                    ContractViolationKind::ParameterSlotOutOfBounds,
                                ),
                                format!("param index {index} out of bounds"),
                            ));
                        }
                    }
                }
            }
            CfgInstData::BlockParam { .. } => {
                frame.cache.get(&v.as_u32()).cloned().ok_or_else(|| {
                    unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::UnboundBlockParameter,
                        ),
                        "unbound block param",
                    )
                })?
            }

            CfgInstData::Add(a, b) => self.arith(
                cfg,
                frame,
                *a,
                *b,
                ty,
                Some(AddressArithmetic::Add),
                |x, y| x.checked_add(y),
            )?,
            CfgInstData::Sub(a, b) => self.arith(
                cfg,
                frame,
                *a,
                *b,
                ty,
                Some(AddressArithmetic::Sub),
                |x, y| x.checked_sub(y),
            )?,
            CfgInstData::Mul(a, b) => {
                self.arith(cfg, frame, *a, *b, ty, None, |x, y| x.checked_mul(y))?
            }
            CfgInstData::WrappingAdd(a, b) => {
                self.wrapping_arith(cfg, frame, *a, *b, ty, |x, y| x.wrapping_add(y))?
            }
            CfgInstData::WrappingSub(a, b) => {
                self.wrapping_arith(cfg, frame, *a, *b, ty, |x, y| x.wrapping_sub(y))?
            }
            CfgInstData::WrappingMul(a, b) => {
                self.wrapping_arith(cfg, frame, *a, *b, ty, |x, y| x.wrapping_mul(y))?
            }
            CfgInstData::Div(a, b) => self.divmod(cfg, frame, *a, *b, ty, false)?,
            CfgInstData::Mod(a, b) => self.divmod(cfg, frame, *a, *b, ty, true)?,

            CfgInstData::Eq(a, b) => {
                self.cmp(cfg, frame, *a, *b, |o| o == std::cmp::Ordering::Equal)?
            }
            CfgInstData::Ne(a, b) => {
                self.cmp(cfg, frame, *a, *b, |o| o != std::cmp::Ordering::Equal)?
            }
            CfgInstData::Lt(a, b) => {
                self.cmp(cfg, frame, *a, *b, |o| o == std::cmp::Ordering::Less)?
            }
            CfgInstData::Gt(a, b) => {
                self.cmp(cfg, frame, *a, *b, |o| o == std::cmp::Ordering::Greater)?
            }
            CfgInstData::Le(a, b) => {
                self.cmp(cfg, frame, *a, *b, |o| o != std::cmp::Ordering::Greater)?
            }
            CfgInstData::Ge(a, b) => {
                self.cmp(cfg, frame, *a, *b, |o| o != std::cmp::Ordering::Less)?
            }

            CfgInstData::BitAnd(a, b) => self.bitop(cfg, frame, *a, *b, ty, |x, y| x & y)?,
            CfgInstData::BitOr(a, b) => self.bitop(cfg, frame, *a, *b, ty, |x, y| x | y)?,
            CfgInstData::BitXor(a, b) => self.bitop(cfg, frame, *a, *b, ty, |x, y| x ^ y)?,
            CfgInstData::Shl(a, b) => self.shift(cfg, frame, *a, *b, ty, true)?,
            CfgInstData::Shr(a, b) => self.shift(cfg, frame, *a, *b, ty, false)?,

            CfgInstData::Neg(a) => {
                let x = self.eval(cfg, frame, *a)?.as_int();
                range_check(x.checked_neg(), ty)?
            }
            CfgInstData::Not(a) => Value::Bool(!self.eval(cfg, frame, *a)?.as_bool()),
            CfgInstData::BitNot(a) => {
                let x = self.eval(cfg, frame, *a)?.as_int();
                let (bits, kind) = int_shape(ty)?;
                let masked = (!to_bits(x, bits)) & width_mask(bits);
                Value::Int(from_bits(masked, bits, kind_signed(kind)))
            }

            CfgInstData::Alloc { slot, init } => {
                let val = self.eval(cfg, frame, *init)?;
                self.store_local(frame, *slot, val)?;
                Value::Unit
            }
            CfgInstData::Load { slot } => self.load_local(frame, *slot)?,
            CfgInstData::Store { slot, value } => {
                let val = self.eval(cfg, frame, *value)?;
                self.store_local(frame, *slot, val)?;
                Value::Unit
            }
            CfgInstData::PlaceRead { place } => {
                if !self.place_projection_metadata_is_valid(cfg, place, ty, PlaceAccess::Read) {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::PlaceProjectionMetadata,
                        ),
                        "PlaceRead projection metadata drift",
                    ));
                }
                if let Some(kind) = self.place_base_violation(cfg, place, PlaceAccess::Read) {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(kind),
                        format!(
                            "PlaceRead base {:?} with type {:?} (logical width {}) violates its CFG contract ({} local slots, {} parameter slots)",
                            place.base,
                            place.base_type,
                            self.type_pool().abi_slot_count(place.base_type),
                            cfg.num_locals(),
                            cfg.num_params(),
                        ),
                    ));
                }
                self.place_read(cfg, frame, place)?
            }
            CfgInstData::PlaceWrite { place, value } => {
                if !self.place_projection_metadata_is_valid(
                    cfg,
                    place,
                    cfg.get_inst(*value).ty,
                    PlaceAccess::Write,
                ) {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::PlaceProjectionMetadata,
                        ),
                        "PlaceWrite projection metadata drift",
                    ));
                }
                if let Some(kind) = self.place_base_violation(cfg, place, PlaceAccess::Write) {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(kind),
                        format!(
                            "PlaceWrite base {:?} with type {:?} (logical width {}) violates its CFG contract ({} local slots, {} parameter slots)",
                            place.base,
                            place.base_type,
                            self.type_pool().abi_slot_count(place.base_type),
                            cfg.num_locals(),
                            cfg.num_params(),
                        ),
                    ));
                }
                let val = self.eval(cfg, frame, *value)?;
                self.place_write(cfg, frame, place, val)?;
                Value::Unit
            }

            CfgInstData::StructInit { .. } => {
                let fields = cfg.get_struct_fields(&inst.data).to_vec();
                Value::Aggregate(self.eval_all(cfg, frame, &fields)?)
            }
            CfgInstData::ArrayInit { .. } => {
                let elems = cfg.get_array_elements(&inst.data).to_vec();
                Value::Aggregate(self.eval_all(cfg, frame, &elems)?)
            }
            // A discriminant-only variant is its tag (an `Int`); a payload-
            // carrying variant is an `Aggregate` whose element 0 is the tag and
            // the rest are the payload fields, in declaration order (RUE-285).
            // This lets structural `==` distinguish `Circle(5)` from `Circle(6)`
            // and from `Square(5)`, and lets `EnumPayloadGet` project a field.
            CfgInstData::EnumVariant { variant_index, .. } => {
                if cfg.get_enum_payload(&inst.data).is_empty() {
                    Value::Int(*variant_index as i128)
                } else {
                    let payload_refs = cfg.get_enum_payload(&inst.data).to_vec();
                    let mut elems = Vec::with_capacity(1 + payload_refs.len());
                    elems.push(Value::Int(*variant_index as i128));
                    elems.extend(self.eval_all(cfg, frame, &payload_refs)?);
                    Value::Aggregate(elems)
                }
            }

            // Read payload field `field_index` of a payload-carrying variant:
            // element `1 + field_index` of the enum's `Aggregate` (element 0 is
            // the discriminant). A discriminant-only enum never reaches here.
            CfgInstData::EnumPayloadGet {
                base, field_index, ..
            } => match self.eval(cfg, frame, *base)? {
                Value::Aggregate(mut elems) if (*field_index as usize) + 1 < elems.len() => {
                    elems.swap_remove(*field_index as usize + 1)
                }
                _ => {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::EnumPayloadProjection,
                        ),
                        "enum payload get on non-payload value",
                    ));
                }
            },

            // Writes to an `inout` parameter inside the callee (visible to the
            // caller via copy-out).
            CfgInstData::ParamStore { param_slot, value } => {
                let val = self.eval(cfg, frame, *value)?;
                if let Some(target) = frame.param_places.get(param_slot).cloned() {
                    // A whole-receiver assignment inside an inout accessor is
                    // an immediate write to the caller's place; the spliced
                    // CFG performs the same redirection.
                    self.ptr_cell_write(
                        &target,
                        cfg.get_inst(*value).ty,
                        val,
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::UnsplicedAccessor,
                        ),
                    )?;
                } else {
                    Self::set_param(frame, *param_slot, val);
                }
                Value::Unit
            }
            CfgInstData::AccessorCall { name, .. } => {
                let fname = self.interner().resolve(name).to_string();
                let call_args = cfg.get_call_args(&inst.data).to_vec();
                let arg_types = call_args
                    .iter()
                    .map(|arg| cfg.get_inst(arg.value).ty)
                    .collect::<Vec<_>>();
                let mut argvals = Vec::with_capacity(call_args.len());
                let mut param_places = HashMap::new();
                let mut base = 0usize;
                for (index, argument) in call_args.iter().enumerate() {
                    let value = match argument.mode {
                        CfgArgMode::Borrow | CfgArgMode::Inout => {
                            let target = self.argument_place_target(cfg, frame, argument.value)?;
                            param_places.insert(base as u32, target.clone());
                            self.ptr_cell_read(
                                &target,
                                arg_types[index],
                                UnsupportedKind::ContractViolation(
                                    ContractViolationKind::UnsplicedAccessor,
                                ),
                            )?
                        }
                        CfgArgMode::Normal => self.eval(cfg, frame, argument.value)?,
                    };
                    argvals.push((value, arg_types[index], argument.mode));
                    base += self.call_arg_slot_width(arg_types[index], argument.mode);
                }
                Value::Ptr(Some(self.call_accessor(&fname, &argvals, param_places)?))
            }
            CfgInstData::Call { runtime, name, .. } => {
                let fname = self.interner().resolve(name).to_string();
                let call_args = cfg.get_call_args(&inst.data).to_vec();
                let arg_types: Vec<Type> = call_args
                    .iter()
                    .map(|arg| cfg.get_inst(arg.value).ty)
                    .collect();
                let arg_modes: Vec<CfgArgMode> = call_args.iter().map(|arg| arg.mode).collect();
                let is_string_builtin = if let Some(runtime) = runtime {
                    self.preflight_string_builtin(*runtime, &arg_types, &arg_modes, ty)?
                } else {
                    false
                };
                let is_channel_call = if let Some(runtime) = runtime {
                    !is_string_builtin
                        && self.preflight_test_channel_call(*runtime, &arg_types, &arg_modes, ty)?
                } else {
                    false
                };
                let missing_call_kind =
                    if !is_string_builtin && !is_channel_call && runtime.is_some() {
                        let runtime = runtime.expect("checked above");
                        // A compiler-synthesized body reaches the memory helpers as
                        // runtime calls rather than through the `@alloc` /
                        // `@byte_copy` intrinsics that name them: drop glue and the
                        // ADR-0083 §1 structural printer are written directly in
                        // semantic-body form, and the printer is what a failing
                        // `@assert_eq` calls before it reports. The interpreter's
                        // gap is the same one either way, so it is reported as that
                        // intrinsic's gap — which a corpus case can register —
                        // rather than as a missing function body, which is a
                        // harness failure. It stops here rather than joining the
                        // output calls below, which are unmodeled *and* executable;
                        // there is nothing to execute for an unmodeled allocation.
                        if let Some(operation) =
                            rue_air::IntrinsicOperation::from_runtime_call(runtime)
                        {
                            let borrowed = unsupported_intrinsic_kind_for_operation(operation);
                            if borrowed.model_gap().is_some() {
                                return Err(unsupported(borrowed, format!("call to '{fname}'")));
                            }
                        }
                        let kind = self.classify_unsupported_runtime_call_static(
                            runtime, &arg_types, &arg_modes, ty,
                        );
                        if kind.model_gap().is_none() {
                            return Err(unsupported(kind, format!("call to '{fname}'")));
                        }
                        Some(kind)
                    } else if !is_string_builtin
                        && !is_channel_call
                        && self.find_function(&fname).is_none()
                    {
                        return Err(unsupported(
                            UnsupportedKind::ContractViolation(
                                ContractViolationKind::MissingFunctionBody,
                            ),
                            format!("call to '{fname}'"),
                        ));
                    } else {
                        None
                    };
                if !is_string_builtin && !is_channel_call && missing_call_kind.is_none() {
                    let callee = &*self.state.functions[self
                        .find_function(&fname)
                        .expect("a present user call has a CFG body")]
                    .cfg;
                    self.preflight_call_layout(
                        callee,
                        &fname,
                        arg_types.iter().copied().zip(arg_modes.iter().copied()),
                    )?;
                }
                // Copy-in every argument (by value); for `inout` args, remember
                // the base parameter slot and the caller place to copy back into.
                let mut argvals = Vec::with_capacity(call_args.len());
                let mut writebacks: Vec<(usize, WritebackPlace<'a>)> = Vec::new();
                let mut base = 0usize;
                for (index, a) in call_args.iter().enumerate() {
                    let v = match a.mode {
                        CfgArgMode::Borrow | CfgArgMode::Inout => {
                            self.reread_by_ref_operand(cfg, frame, a.value)?
                        }
                        CfgArgMode::Normal => self.eval(cfg, frame, a.value)?,
                    };
                    // Derive both callee packing and copy-back bases from the
                    // same static type + physical passing-mode contract.
                    let w = self.call_arg_slot_width(arg_types[index], a.mode);
                    // A physical inout ZST still advances the ABI base by its
                    // pointer slot, but copying its unique value back has no
                    // semantic effect and could overwrite a later local that
                    // shares the ZST's zero-width boundary slot.
                    if matches!(a.mode, CfgArgMode::Inout) && !self.is_zero_sized(arg_types[index])
                    {
                        writebacks.push((base, self.lvalue_of(cfg, a.value)?));
                    }
                    argvals.push(v);
                    base += w;
                }
                // Builtin String methods have no CFG body; dispatch them here.
                if is_string_builtin {
                    self.string_builtin(
                        runtime.expect("typed runtime string builtin"),
                        &argvals,
                        &arg_types,
                        &arg_modes,
                        ty,
                    )?
                    .expect("preflight recognized this String builtin")
                } else if is_channel_call {
                    self.eval_test_channel_call(
                        runtime.expect("typed runtime channel call"),
                        &argvals,
                    )?
                } else if missing_call_kind.is_some() {
                    let runtime = runtime.expect("typed runtime output call");
                    // Static validation above establishes the compiler/runtime
                    // ABI. Keep the dynamic value check fail-closed before the
                    // modeled output path executes.
                    let kind = self.classify_unsupported_runtime_call(
                        runtime, &argvals, &arg_types, &arg_modes, ty,
                    );
                    if kind.model_gap().is_none() {
                        return Err(unsupported(kind, format!("call to '{fname}'")));
                    }
                    self.eval_runtime_output_call(runtime, &argvals, &arg_types)?
                } else {
                    let typed_args: Vec<(Value, Type, CfgArgMode)> = argvals
                        .into_iter()
                        .zip(arg_types)
                        .zip(arg_modes)
                        .map(|((value, ty), mode)| (value, ty, mode))
                        .collect();
                    let (result, final_params) = self.call(&fname, &typed_args)?;
                    // Copy-out: write each inout parameter's final value back into
                    // the caller place it came from.
                    for (slot, place) in writebacks {
                        if let Some(val) = final_params.get(slot).and_then(|o| o.clone()) {
                            match place {
                                WritebackPlace::Simple { base, base_type } => {
                                    let place = match base {
                                        PlaceBase::Local(slot) => Place::local(slot, base_type),
                                        PlaceBase::Param(slot) => Place::param(slot, base_type),
                                        PlaceBase::Accessor(_) => {
                                            return Err(unsupported(
                                                UnsupportedKind::ContractViolation(
                                                    ContractViolationKind::UnsplicedAccessor,
                                                ),
                                                "accessor place reached call writeback",
                                            ));
                                        }
                                        PlaceBase::Indirect(_) => {
                                            return Err(unsupported(
                                                unsupported_intrinsic_kind_for_operation(
                                                    rue_air::IntrinsicOperation::PtrWrite,
                                                ),
                                                "indirect place reached simple call writeback",
                                            ));
                                        }
                                    };
                                    self.place_write(cfg, frame, &place, val)?;
                                }
                                WritebackPlace::Stored(place) => {
                                    self.place_write(cfg, frame, place, val)?;
                                }
                            }
                        }
                    }
                    result
                }
            }

            CfgInstData::Intrinsic { operation, .. } => {
                let args = cfg.get_intrinsic_args(&inst.data).to_vec();
                match *operation {
                    rue_air::IntrinsicOperation::BoundsCheck => {
                        self.eval_bounds_check_intrinsic(cfg, frame, *operation, &args, ty)?
                    }
                    rue_air::IntrinsicOperation::PanicNoMessage
                    | rue_air::IntrinsicOperation::Panic
                    | rue_air::IntrinsicOperation::AssertFailed => {
                        let Some(intrinsic) =
                            self.preflight_abort_intrinsic(cfg, *operation, &args, ty)?
                        else {
                            return Err(unsupported(
                                UnsupportedKind::ContractViolation(
                                    ContractViolationKind::IntrinsicSignature,
                                ),
                                format!("intrinsic @{} signature", operation.expected_spelling()),
                            ));
                        };
                        self.eval_abort_intrinsic(cfg, frame, intrinsic, &args)?
                    }
                    rue_air::IntrinsicOperation::BitCast => {
                        // `@bitCast` is fully modeled, not a gap: a same-width
                        // reinterpretation is exactly a `to_bits`/`from_bits` round
                        // trip in the value model (RUE-952).
                        self.eval_bit_cast(cfg, frame, &args, ty)?
                    }
                    rue_air::IntrinsicOperation::Syscall => {
                        let kind =
                            self.classify_unsupported_intrinsic(cfg, v, *operation, &args, ty);
                        if kind
                            != UnsupportedKind::ExternalDependency(
                                ExternalDependencyKind::SystemCall,
                            )
                        {
                            return Err(unsupported(
                                kind,
                                format!("intrinsic @{}", operation.expected_spelling()),
                            ));
                        }
                        let values = self.eval_all(cfg, frame, &args)?;
                        self.eval_stdout_syscall(&values)?
                    }
                    rue_air::IntrinsicOperation::DebugI64
                    | rue_air::IntrinsicOperation::DebugU64
                    | rue_air::IntrinsicOperation::DebugBool
                    | rue_air::IntrinsicOperation::DebugStr => {
                        self.eval_debug_intrinsic(cfg, frame, *operation, &args, ty)?
                    }
                    rue_air::IntrinsicOperation::ReadLine
                    | rue_air::IntrinsicOperation::ParseI32
                    | rue_air::IntrinsicOperation::ParseI64
                    | rue_air::IntrinsicOperation::ParseU32
                    | rue_air::IntrinsicOperation::ParseU64
                    | rue_air::IntrinsicOperation::RandomU32
                    | rue_air::IntrinsicOperation::RandomU64
                    | rue_air::IntrinsicOperation::PtrToInt
                    | rue_air::IntrinsicOperation::IntToPtr
                    | rue_air::IntrinsicOperation::PtrRead
                    | rue_air::IntrinsicOperation::PtrReadUnaligned
                    | rue_air::IntrinsicOperation::PtrWrite
                    | rue_air::IntrinsicOperation::PtrWriteUnaligned
                    | rue_air::IntrinsicOperation::PtrOffset
                    | rue_air::IntrinsicOperation::Alloc
                    | rue_air::IntrinsicOperation::AllocZeroed
                    | rue_air::IntrinsicOperation::Free
                    | rue_air::IntrinsicOperation::Realloc
                    | rue_air::IntrinsicOperation::Resize
                    | rue_air::IntrinsicOperation::ByteCopy
                    | rue_air::IntrinsicOperation::ByteMove
                    | rue_air::IntrinsicOperation::ByteSet
                    | rue_air::IntrinsicOperation::ArgCount
                    | rue_air::IntrinsicOperation::ArgPtr
                    | rue_air::IntrinsicOperation::ArgLen
                    | rue_air::IntrinsicOperation::EnvCount
                    | rue_air::IntrinsicOperation::EnvPtr
                    | rue_air::IntrinsicOperation::EnvLen
                    | rue_air::IntrinsicOperation::Raw
                    | rue_air::IntrinsicOperation::RawMut
                    | rue_air::IntrinsicOperation::FieldPtr
                    | rue_air::IntrinsicOperation::IntToFloat
                    | rue_air::IntrinsicOperation::FloatToInt
                    | rue_air::IntrinsicOperation::FloatCast
                    | rue_air::IntrinsicOperation::TotalCmp => {
                        // Classify first: the same static arity/signature validation
                        // that gates a model-gap registration also gates execution, so
                        // a malformed intrinsic still reports its contract violation
                        // rather than being run. A validated, now-modeled heap/pointer
                        // intrinsic executes; every other typed gap is reported.
                        let kind =
                            self.classify_unsupported_intrinsic(cfg, v, *operation, &args, ty);
                        match kind {
                            UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(ik))
                                if modeled_pointer_intrinsic(ik) =>
                            {
                                match self
                                    .eval_pointer_intrinsic(cfg, frame, *operation, &args, ty)?
                                {
                                    Some(value) => value,
                                    None => {
                                        return Err(unsupported(
                                            kind,
                                            format!("intrinsic @{}", operation.expected_spelling()),
                                        ));
                                    }
                                }
                            }
                            _ => {
                                return Err(unsupported(
                                    kind,
                                    format!("intrinsic @{}", operation.expected_spelling()),
                                ));
                            }
                        }
                    }
                }
            }

            CfgInstData::IntCast { value, from_ty: _ } => {
                let x = self.eval(cfg, frame, *value)?.as_int();
                let (lo, hi) = int_bounds(ty).ok_or_else(|| {
                    unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::IntCastTargetType,
                        ),
                        "intcast target not an int",
                    )
                })?;
                if x < lo || x > hi {
                    return Err(Flow::Panic(Panic::runtime(TrapKind::IntegerCastOverflow)));
                }
                Value::Int(x)
            }

            CfgInstData::Drop { value } => {
                let ty = cfg.get_inst(*value).ty;
                let v = self.eval(cfg, frame, *value)?;
                self.run_drop(ty, v)?;
                Value::Unit
            }
            CfgInstData::StorageLive { .. } | CfgInstData::StorageDead { .. } => Value::Unit,
        };
        frame.cache.insert(v.as_u32(), result.clone());
        Ok(result)
    }

    fn arith(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        a: CfgValue,
        b: CfgValue,
        ty: Type,
        address_operation: Option<AddressArithmetic>,
        op: impl Fn(i128, i128) -> Option<i128>,
    ) -> Step<Value> {
        let left = self.eval(cfg, frame, a)?;
        let right = self.eval(cfg, frame, b)?;
        let x = left.as_int();
        let y = right.as_int();
        let result = range_check(op(x, y), ty)?;
        let provenance = match (address_operation, &left, &right) {
            (Some(AddressArithmetic::Add), Value::AddressInt { provenance, .. }, Value::Int(_))
            | (Some(AddressArithmetic::Add), Value::Int(_), Value::AddressInt { provenance, .. })
            | (Some(AddressArithmetic::Sub), Value::AddressInt { provenance, .. }, Value::Int(_)) => {
                Some(provenance)
            }
            _ => None,
        };
        if let Some(provenance) = provenance
            && let Some(target) = self.address_target(result.as_int() as u64 as u128, &provenance.0)
        {
            return Ok(Value::AddressInt {
                value: result.as_int(),
                provenance: AddressProvenance(target),
            });
        }
        Ok(result)
    }

    /// Wrapping `Add`/`Sub`/`Mul` (`@wrapping_*`, RUE-647): the true result
    /// reduced modulo 2^N and reinterpreted as the declared type — never a
    /// trap. Operands are ≤64-bit so the `i128` op never itself overflows;
    /// `to_bits` then truncates to the operand width.
    fn wrapping_arith(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        a: CfgValue,
        b: CfgValue,
        ty: Type,
        op: impl Fn(i128, i128) -> i128,
    ) -> Step<Value> {
        let x = self.eval(cfg, frame, a)?.as_int();
        let y = self.eval(cfg, frame, b)?.as_int();
        let (bits, kind) = int_shape(ty)?;
        let r = to_bits(op(x, y), bits) & width_mask(bits);
        Ok(Value::Int(from_bits(r, bits, kind_signed(kind))))
    }

    fn divmod(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        a: CfgValue,
        b: CfgValue,
        ty: Type,
        is_mod: bool,
    ) -> Step<Value> {
        let x = self.eval(cfg, frame, a)?.as_int();
        let y = self.eval(cfg, frame, b)?.as_int();
        if y == 0 {
            return Err(Flow::Panic(Panic::runtime(TrapKind::DivisionByZero)));
        }
        // MIN / -1 (and MIN % -1) trap at the operand width: the hardware `idiv`
        // faults even though the mathematical remainder is 0. Our i128 model
        // wouldn't otherwise catch it (the value fits in i128).
        if let Some((lo, _)) = int_bounds(ty) {
            if ty.is_signed() && x == lo && y == -1 {
                return Err(Flow::Panic(Panic::runtime(TrapKind::ArithmeticOverflow)));
            }
        }
        let r = if is_mod {
            x.checked_rem(y)
        } else {
            x.checked_div(y)
        };
        range_check(r, ty)
    }

    /// Type-directed structural equality (`==`/`!=`). A text field/element/
    /// payload compares by byte content (read from the heap), so two equal
    /// strings held in distinct buffer allocations are equal; everything else
    /// compares by value, recursing into struct fields, array elements, and the
    /// active enum variant's payload (mirroring `run_drop`'s aggregate layout).
    fn values_equal_typed(&self, x: &Value, y: &Value, ty: Type) -> Step<bool> {
        if self.is_text_type(ty) {
            return Ok(self.text_bytes(x)? == self.text_bytes(y)?);
        }
        if matches!(
            ty.kind(),
            TypeKind::I8
                | TypeKind::I16
                | TypeKind::I32
                | TypeKind::I64
                | TypeKind::U8
                | TypeKind::U16
                | TypeKind::U32
                | TypeKind::U64
        ) {
            // Address provenance is intentionally invisible to numeric
            // observables, including integer fields nested in aggregates.
            return Ok(x.as_int() == y.as_int());
        }
        match ty.kind() {
            TypeKind::Struct(sid) => {
                let (Value::Aggregate(xs), Value::Aggregate(ys)) = (x, y) else {
                    return Ok(x == y);
                };
                let def = self.type_pool().struct_def(sid);
                if xs.len() != ys.len() || xs.len() != def.fields.len() {
                    return Ok(x == y);
                }
                for (i, field) in def.fields.iter().enumerate() {
                    if !self.values_equal_typed(&xs[i], &ys[i], field.ty)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            TypeKind::Array(aid) => {
                let (elem_ty, _len) = self.type_pool().array_def(aid);
                let (Value::Aggregate(xs), Value::Aggregate(ys)) = (x, y) else {
                    return Ok(x == y);
                };
                if xs.len() != ys.len() {
                    return Ok(false);
                }
                for (xe, ye) in xs.iter().zip(ys.iter()) {
                    if !self.values_equal_typed(xe, ye, elem_ty)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            TypeKind::Enum(eid) => {
                // A discriminant-only variant is a bare `Int` tag; a payload
                // variant is `[tag, payload...]`. Compare the active variant,
                // then its payload fields by their declared types.
                match (x, y) {
                    (Value::Aggregate(xs), Value::Aggregate(ys)) => {
                        if xs.is_empty() || ys.is_empty() || xs[0] != ys[0] {
                            return Ok(false);
                        }
                        let tag = xs[0].as_int() as usize;
                        let payload_tys = self.type_pool().enum_def(eid).variant_payload(tag);
                        if xs.len() != ys.len() {
                            return Ok(false);
                        }
                        for (i, pty) in payload_tys.iter().enumerate() {
                            match (xs.get(i + 1), ys.get(i + 1)) {
                                (Some(xe), Some(ye)) => {
                                    if !self.values_equal_typed(xe, ye, *pty)? {
                                        return Ok(false);
                                    }
                                }
                                _ => return Ok(false),
                            }
                        }
                        Ok(true)
                    }
                    // Same-variant discriminant-only enums (or a mixed shape).
                    _ => Ok(x == y),
                }
            }
            _ => Ok(x == y),
        }
    }

    fn cmp(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        a: CfgValue,
        b: CfgValue,
        pick: impl Fn(std::cmp::Ordering) -> bool,
    ) -> Step<Value> {
        let x = self.eval(cfg, frame, a)?;
        let y = self.eval(cfg, frame, b)?;
        // Strings compare by byte content (str/StrBuf ==/!=/ordering), read from
        // the heap through the materialized headers (RUE-1010 §6.13). Other
        // aggregates (structs, arrays, payload enums) compare STRUCTURALLY,
        // field-by-field / element-by-element / same-variant-and-equal-payload,
        // via `Value`'s derived `PartialEq` which recurses into nested
        // aggregates (RUE-285). A raw pointer compares by ADDRESS — allocation
        // identity, lifetime generation, and byte offset — which is the same
        // identity `values_equal_typed` gives a pointer leaf nested in an
        // aggregate (spec 4.3:3e). Only `==` / `!=` reach a non-text aggregate
        // or a pointer here (ordering `< > <= >=` is a type error on those), so
        // we report `Equal` iff equal and an arbitrary non-`Equal` ordering
        // otherwise — enough for `pick` to decide `==`/`!=`. Everything else
        // compares by integer value, which is what an *address integer* from
        // `@ptr_to_int` must do: its provenance is invisible to arithmetic.
        let ty = cfg.get_inst(a).ty;
        let ord = if self.is_text_type(ty) {
            let bx = self.text_bytes(&x)?;
            let by = self.text_bytes(&y)?;
            bx.cmp(&by)
        } else {
            match (&x, &y) {
                // A pointer belongs here rather than below: `as_int` reads
                // every pointer as zero, so the numeric path would call two
                // distinct addresses equal. Component-wise aggregate equality
                // (RUE-1992) compares a string-carrying aggregate one
                // component at a time, so a pointer field reaches this
                // comparison directly and not only through the aggregate arm.
                (Value::Aggregate(_), _)
                | (_, Value::Aggregate(_))
                | (Value::Ptr(_), _)
                | (_, Value::Ptr(_)) => {
                    // Compare with text awareness so a text field/element/
                    // payload is judged by its byte content, not by the
                    // identity of its heap buffer pointer (RUE-1010 §6.13).
                    if self.values_equal_typed(&x, &y, ty)? {
                        std::cmp::Ordering::Equal
                    } else {
                        std::cmp::Ordering::Less
                    }
                }
                _ => x.as_int().cmp(&y.as_int()),
            }
        };
        Ok(Value::Bool(pick(ord)))
    }

    fn bitop(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        a: CfgValue,
        b: CfgValue,
        ty: Type,
        op: impl Fn(u128, u128) -> u128,
    ) -> Step<Value> {
        let x = self.eval(cfg, frame, a)?.as_int();
        let y = self.eval(cfg, frame, b)?.as_int();
        let (bits, kind) = int_shape(ty)?;
        let r = op(to_bits(x, bits), to_bits(y, bits)) & width_mask(bits);
        Ok(Value::Int(from_bits(r, bits, kind_signed(kind))))
    }

    fn shift(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        a: CfgValue,
        b: CfgValue,
        ty: Type,
        left: bool,
    ) -> Step<Value> {
        let x = self.eval(cfg, frame, a)?.as_int();
        let amt = self.eval(cfg, frame, b)?.as_int();
        let (bits, kind) = int_shape(ty)?;
        // Shift amount is masked modulo the operand bit width (spec 4.3a:10).
        let sh = (amt.rem_euclid(bits as i128)) as u32;
        let signed = kind_signed(kind);
        let r = if left {
            (to_bits(x, bits) << sh) & width_mask(bits)
        } else if signed {
            // arithmetic right shift: operate on the sign-extended value
            let v = from_bits(to_bits(x, bits), bits, true);
            to_bits(v >> sh, bits)
        } else {
            (to_bits(x, bits) >> sh) & width_mask(bits)
        };
        Ok(Value::Int(from_bits(r, bits, signed)))
    }

    fn set_local(frame: &mut Frame, slot: u32, val: Value) {
        let s = slot as usize;
        if s >= frame.locals.len() {
            frame.locals.resize(s + 1, None);
        }
        frame.locals[s] = Some(val);
    }

    fn get_local(frame: &Frame, slot: u32) -> Step<Value> {
        frame
            .locals
            .get(slot as usize)
            .and_then(|o| o.clone())
            .ok_or_else(|| {
                unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::UninitializedLocal),
                    format!("read of uninit local {slot}"),
                )
            })
    }

    // ---- aggregates & places ---------------------------------------------

    /// Resolve a place to its base and a fully-evaluated projection path
    /// (field indices and *evaluated* array indices). Index subexpressions are
    /// evaluated here, in place order.
    fn resolve_path(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        place: &Place,
    ) -> Step<(PlaceBase, Vec<(usize, Projection)>)> {
        let projs = cfg.get_place_projections(place).to_vec();
        let mut path = Vec::with_capacity(projs.len());
        for p in projs {
            match p {
                Projection::Field { field_index, .. } => {
                    path.push((field_index as usize, p));
                }
                Projection::Index { index, .. } => {
                    let i = self.eval(cfg, frame, index)?.as_int();
                    if i < 0 {
                        return Err(Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)));
                    }
                    let i = usize::try_from(i)
                        .map_err(|_| Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)))?;
                    path.push((i, p));
                }
            }
        }
        Ok((place.base, path))
    }

    fn projection_offset(&self, mut ty: Type, path: &[(usize, Projection)]) -> Option<(u64, Type)> {
        if !self.is_materializable_type(ty) {
            return None;
        }
        let mut offset = 0u64;
        for (index, projection) in path {
            let layout = self.try_layout(ty)?;
            match (ty.kind(), &layout.kind, projection) {
                (
                    TypeKind::Struct(id),
                    LayoutKind::Struct { field_offsets, .. },
                    Projection::Field { .. },
                ) => {
                    let field = self.type_pool().try_struct_def(id)?.fields.get(*index)?;
                    offset = offset.checked_add(*field_offsets.get(*index)?)?;
                    ty = field.ty;
                }
                (
                    TypeKind::Array(id),
                    LayoutKind::Array { element, .. },
                    Projection::Index { .. },
                ) => {
                    let (element_ty, count) = self.type_pool().try_array_def(id)?;
                    if *index as u64 >= count {
                        return None;
                    }
                    offset = offset.checked_add((*index as u64).checked_mul(element.stride)?)?;
                    ty = element_ty;
                }
                // A payload projection is variant-dependent. The place
                // metadata alone does not carry the active discriminant, so
                // never guess variant zero: callers that need such a pointer
                // remain a typed gap until the value-aware path supplies it.
                (TypeKind::Enum(_), LayoutKind::Enum { .. }, Projection::Field { .. }) => {
                    return None;
                }
                _ => return None,
            }
        }
        Some((offset, ty))
    }

    fn compose_pointer_path(
        &self,
        mut target: PtrTarget,
        base_type: Type,
        path: &[(usize, Projection)],
    ) -> Option<PtrTarget> {
        if let Some((offset, _)) = self.projection_offset(base_type, path) {
            target.byte_offset = target.byte_offset.checked_add(offset)?;
        } else if !path.is_empty() {
            return None;
        }
        Some(target)
    }

    fn external_place_target(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        base: PlaceBase,
        base_type: Type,
        _view_type: Type,
        path: &[(usize, Projection)],
    ) -> Step<Option<PtrTarget>> {
        let target = match base {
            PlaceBase::Param(slot) => frame.param_places.get(&slot).cloned(),
            PlaceBase::Accessor(call) => match self.eval(cfg, frame, call)? {
                Value::Ptr(target) => target,
                _ => {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::UnsplicedAccessor,
                        ),
                        "accessor call did not yield a place",
                    ));
                }
            },
            PlaceBase::Indirect(pointer) => match self.eval(cfg, frame, pointer)? {
                Value::Ptr(target) => target,
                _ => None,
            },
            PlaceBase::Local(_) => None,
        };
        match target {
            Some(target) => {
                // Physical by-reference targets cross function-local type
                // pools. Retype the root view from this function's Place
                // metadata before composing projections; the byte allocation
                // and provenance remain unchanged.
                let projection_base = base_type;
                self.compose_pointer_path(target, projection_base, path)
                    .map(|target| Some(target))
                    .ok_or_else(|| {
                        unsupported(
                            UnsupportedKind::ContractViolation(
                                ContractViolationKind::PlaceProjectionMetadata,
                            ),
                            "pointer projection layout",
                        )
                    })
            }
            None => Ok(None),
        }
    }

    fn returned_place_target(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        value: CfgValue,
    ) -> Step<PtrTarget> {
        let CfgInstData::PlaceRead { place } = &cfg.get_inst(value).data else {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::UnsplicedAccessor),
                "accessor return is not the trailing yielded place",
            ));
        };
        self.promote_place(cfg, frame, place, cfg.get_inst(value).ty)
    }

    fn base_value(frame: &Frame, base: PlaceBase) -> Step<Value> {
        match base {
            PlaceBase::Local(slot) => Self::get_local(frame, slot),
            PlaceBase::Param(slot) => match frame.params.get(slot as usize) {
                Some(Some(value)) => Ok(value.clone()),
                Some(None) => Err(unsupported(
                    UnsupportedKind::SemanticGap(SemanticGapKind::FlattenedParameterSlot),
                    "param place",
                )),
                None => Err(unsupported(
                    UnsupportedKind::ContractViolation(
                        ContractViolationKind::ParameterSlotOutOfBounds,
                    ),
                    format!("param place {slot} out of bounds"),
                )),
            },
            PlaceBase::Accessor(_) => Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::UnsplicedAccessor),
                "accessor place reached oracle storage",
            )),
            PlaceBase::Indirect(_) => Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                "indirect place requires evaluated pointer storage",
            )),
        }
    }

    fn place_read(&mut self, cfg: &'a Cfg, frame: &mut Frame, place: &Place) -> Step<Value> {
        let (base, path) = self.resolve_path(cfg, frame, place)?;
        let view_type = self
            .projection_offset(place.base_type, &path)
            .map(|(_, ty)| ty)
            .unwrap_or(place.base_type);
        if let Some(target) =
            self.external_place_target(cfg, frame, base, place.base_type, view_type, &path)?
        {
            return self.ptr_cell_read(
                &target,
                view_type,
                UnsupportedKind::ContractViolation(ContractViolationKind::UnsplicedAccessor),
            );
        }
        let mut cur = match base {
            PlaceBase::Indirect(pointer) => {
                let pointer = self.eval(cfg, frame, pointer)?;
                let target = self.expect_ptr(
                    pointer,
                    unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                )?;
                self.ptr_cell_read(
                    &target,
                    view_type,
                    unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                )?
            }
            _ => self.base_value_of(frame, base)?,
        };
        let mut current_ty = place.base_type;
        for (position, (idx, projection)) in path.iter().copied().enumerate() {
            let one_projection = (idx, projection);
            let next_ty = self
                .projection_offset(current_ty, std::slice::from_ref(&one_projection))
                .map(|(_, ty)| ty);
            cur = match cur {
                Value::Aggregate(mut v) if idx < v.len() => v.swap_remove(idx),
                Value::Aggregate(_) => {
                    return Err(Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)));
                }
                // A pre-splice accessor may materialize a valid place pointer
                // in a local slot while retaining the logical aggregate place
                // metadata. Compose the remaining projections directly on that
                // canonical target; decoding an intermediate aggregate would
                // lose the place's byte/provenance authority.
                Value::Ptr(Some(mut target)) => {
                    // Pointer views cross function-local type pools. Retype
                    // the recovered allocation using this place's canonical
                    // view before asking the layout authority for offsets.
                    // The pointer's pointee is the canonical type of the
                    // materialized place. The surrounding Place metadata can
                    // carry a generic/recovery handle that is not identical
                    // to that type, so use the pointer view as the layout
                    // authority while composing the remaining path.
                    let (offset, pointee) = self
                        .projection_offset(current_ty, &path[position..])
                        .ok_or_else(|| {
                        unsupported(
                            UnsupportedKind::ContractViolation(
                                ContractViolationKind::PlaceProjectionMetadata,
                            ),
                            "pointer-valued place projection layout",
                        )
                    })?;
                    target.byte_offset =
                        target.byte_offset.checked_add(offset).ok_or_else(|| {
                            unsupported(
                                UnsupportedKind::ContractViolation(
                                    ContractViolationKind::PlaceProjectionMetadata,
                                ),
                                "pointer-valued place projection offset overflow",
                            )
                        })?;
                    return self.ptr_cell_read(
                        &target,
                        pointee,
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::UnsplicedAccessor,
                        ),
                    );
                }
                // Text values (str/StrBuf) are now ordinary aggregate headers
                // over a real byte allocation (RUE-1010 §6.13), so their
                // `{ptr, len}` / `{core: {buf, cap}, len}` fields project through
                // the aggregate arm above — no special text-projection handling.
                _ => {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::NonAggregateProjectionRead,
                        ),
                        "projection of non-aggregate",
                    ));
                }
            };
            if let Some(next_ty) = next_ty {
                current_ty = next_ty;
            }
        }
        Ok(cur)
    }

    fn place_write(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        place: &Place,
        val: Value,
    ) -> Step<()> {
        let (base, path) = self.resolve_path(cfg, frame, place)?;
        let view_type = self
            .projection_offset(place.base_type, &path)
            .map(|(_, ty)| ty)
            .unwrap_or(place.base_type);
        if let Some(target) =
            self.external_place_target(cfg, frame, base, place.base_type, view_type, &path)?
        {
            return self.ptr_cell_write(
                &target,
                view_type,
                val,
                UnsupportedKind::ContractViolation(ContractViolationKind::UnsplicedAccessor),
            );
        }
        if let PlaceBase::Indirect(pointer) = base {
            let pointer = self.eval(cfg, frame, pointer)?;
            let target = self.expect_ptr(
                pointer,
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
            )?;
            if path.is_empty() {
                return self
                    .ptr_cell_write(
                        &target,
                        view_type,
                        val,
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                    )
                    .map_err(Flow::from);
            }
            let mut root = self.ptr_cell_read(
                &target,
                place.base_type,
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
            )?;
            let mut cur = &mut root;
            for (idx, _) in &path {
                cur = match cur {
                    Value::Aggregate(values) if *idx < values.len() => &mut values[*idx],
                    Value::Aggregate(_) => {
                        return Err(Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)));
                    }
                    _ => {
                        return Err(unsupported(
                            UnsupportedKind::ContractViolation(
                                ContractViolationKind::NonAggregateProjectionWrite,
                            ),
                            "projection of non-aggregate indirect place",
                        ));
                    }
                };
            }
            *cur = val;
            return self
                .ptr_cell_write(
                    &target,
                    place.base_type,
                    root,
                    unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                )
                .map_err(Flow::from);
        }
        // A promoted base writes through the canonical byte allocation. The
        // unpromoted path can still mutate the frame's logical value directly.
        if let Some(&a) = frame.promoted.get(&promotion_key(base)) {
            let (byte_offset, pointee) = self
                .projection_offset(place.base_type, &path)
                .ok_or_else(|| {
                    unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::PlaceProjectionMetadata,
                        ),
                        "promoted place layout",
                    )
                })?;
            let target = PtrTarget {
                alloc: a,
                generation: self.heap[a].generation,
                byte_offset,
            };
            return self.ptr_cell_write(
                &target,
                pointee,
                val,
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
            );
        }
        let root: &mut Value = {
            let (store, slot) = match base {
                PlaceBase::Local(slot) => (&mut frame.locals, slot as usize),
                PlaceBase::Param(slot) => (&mut frame.params, slot as usize),
                PlaceBase::Accessor(_) => {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::UnsplicedAccessor,
                        ),
                        "accessor place reached oracle write",
                    ));
                }
                PlaceBase::Indirect(_) => unreachable!("indirect places return above"),
            };
            if slot >= store.len() {
                store.resize(slot + 1, None);
            }
            store[slot].get_or_insert(Value::Unit)
        };
        let mut cur = root;
        for (idx, _) in &path {
            cur = match cur {
                Value::Aggregate(v) if *idx < v.len() => &mut v[*idx],
                Value::Aggregate(_) => {
                    return Err(Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)));
                }
                _ => {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::NonAggregateProjectionWrite,
                        ),
                        "projection of non-aggregate",
                    ));
                }
            };
        }
        *cur = val;
        Ok(())
    }

    /// Copy in a by-reference argument by reading its place *now*, at the point
    /// the callee receives it, rather than reusing the value the operand
    /// instruction produced at its own position in the block.
    ///
    /// `docs/formal/01-core-calculus.md` §6.2 admits only a by-VALUE argument as
    /// a redex; a by-reference argument is a place and is not reduced (§6.9), so
    /// nothing about it is observed until the callee reads through it. The
    /// interpreter still hands the callee a value, but taking that value at the
    /// operand's own evaluation point makes it a snapshot — and a later argument
    /// in the same list that mutates the same root then goes unobserved. That is
    /// RUE-1789: `a.bump(grow(inout a))` computed 0 where the compiler, which
    /// passes `a`'s address, computes 1.
    ///
    /// Only a re-readable *place* operand is re-read. Anything else keeps its
    /// original value, which is not a fallback but the matching behavior: an
    /// operand the compiler materializes into a value at argument-evaluation
    /// time (a `borrow str` view over a `StrBuf`, say) really is snapshotted
    /// there, and a program that could observe the difference is rejected by the
    /// call-loan rule (spec 6.1:30) before it ever reaches the oracle.
    fn reread_by_ref_operand(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        v: CfgValue,
    ) -> Step<Value> {
        if !Self::is_rereadable_place_operand(cfg, v) {
            return self.eval(cfg, frame, v);
        }
        // Drop only this instruction's memo. Its operands (a dynamic index, say)
        // keep the values they took at their own evaluation points, so the place
        // being read is still the one argument evaluation selected; only its
        // contents are re-read. The memo is then restored, so an unrelated
        // consumer of the same SSA value still sees its own snapshot — the
        // property `Frame::cache` documents for dominated blocks.
        let saved = frame.cache.remove(&v.as_u32());
        let fresh = self.eval(cfg, frame, v);
        match saved {
            Some(prev) => frame.cache.insert(v.as_u32(), prev),
            None => frame.cache.remove(&v.as_u32()),
        };
        fresh
    }

    /// Whether re-evaluating this operand is a pure re-read of storage: a local
    /// slot, a parameter slot, or a place projection. Every other instruction
    /// may compute or allocate, so re-running it is not a read.
    fn is_rereadable_place_operand(cfg: &Cfg, v: CfgValue) -> bool {
        matches!(
            cfg.get_inst(v).data,
            CfgInstData::Load { .. } | CfgInstData::Param { .. } | CfgInstData::PlaceRead { .. }
        )
    }

    /// Recover the caller place an `inout` argument was loaded from, so its
    /// mutated value can be copied back after the call.
    fn lvalue_of(&self, cfg: &'a Cfg, v: CfgValue) -> Step<WritebackPlace<'a>> {
        match &cfg.get_inst(v).data {
            CfgInstData::Load { slot } if *slot < cfg.num_locals() => Ok(WritebackPlace::Simple {
                base: PlaceBase::Local(*slot),
                base_type: cfg.get_inst(v).ty,
            }),
            CfgInstData::PlaceRead { place } if self.is_inout_writeback_place(cfg, v, place) => {
                Ok(WritebackPlace::Stored(place))
            }
            // Forwarding a writable `inout` parameter as a nested call's `inout`
            // argument (the container `self`-chain: `push` -> `self.reserve()`).
            // The caller place is the parameter slot itself; the post-call
            // copy-out writes the callee's final value back into it, and this
            // frame in turn copies its own parameter back to *its* caller on
            // return, so the mutation threads all the way up the chain.
            // `place_write` routes a `Param` base through the promoted heap
            // allocation when the slot's address was taken, matching the `Param`
            // read path, so an address-taken forwarded parameter stays coherent.
            CfgInstData::Param { index }
                if *index < cfg.num_params() && cfg.is_param_writable(*index) =>
            {
                Ok(WritebackPlace::Simple {
                    base: PlaceBase::Param(*index),
                    base_type: cfg.get_inst(v).ty,
                })
            }
            other => Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::InoutArgumentNotLvalue),
                format!("inout argument is not an lvalue: {other:?}"),
            )),
        }
    }

    fn argument_place_target(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        value: CfgValue,
    ) -> Step<PtrTarget> {
        let ty = cfg.get_inst(value).ty;
        match &cfg.get_inst(value).data {
            CfgInstData::Load { slot } => {
                self.promote_place(cfg, frame, &Place::local(*slot, ty), ty)
            }
            CfgInstData::Param { index } => {
                self.promote_place(cfg, frame, &Place::param(*index, ty), ty)
            }
            CfgInstData::PlaceRead { place } => self.promote_place(cfg, frame, place, ty),
            other => Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::InoutArgumentNotLvalue),
                format!("accessor argument is not a place: {other:?}"),
            )),
        }
    }

    fn set_param(frame: &mut Frame, slot: u32, val: Value) {
        let s = slot as usize;
        if s >= frame.params.len() {
            frame.params.resize(s + 1, None);
        }
        frame.params[s] = Some(val);
    }

    // ---- abstract heap & pointer intrinsics ------------------------------

    /// Current value of a place base, transparently reading through a promoted
    /// (address-taken) slot's heap allocation so a pointer write is observed by
    /// a later direct read of the same slot.
    fn base_value_of(&self, frame: &Frame, base: PlaceBase) -> Step<Value> {
        if let Some(&a) = frame.promoted.get(&promotion_key(base)) {
            return self.promoted_slot_value(a);
        }
        Self::base_value(frame, base)
    }

    /// The logical value a promoted slot holds: the wrapped scalar unwrapped, or
    /// the aggregate root as-is.
    fn promoted_slot_value(&self, alloc: usize) -> Step<Value> {
        let allocation = self.heap.get(alloc).ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                "promoted allocation is missing",
            )
        })?;
        if allocation.freed {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                "promoted allocation was freed",
            ));
        }
        let ty = allocation.root_ty.ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                "raw allocation has no typed view",
            )
        })?;
        self.decode_value(
            &allocation.bytes,
            &allocation.initialized,
            &allocation.provenance,
            ty,
        )
    }

    /// Write the logical value of a promoted slot back into its allocation,
    /// preserving the scalar wrap.
    fn set_promoted_slot(&mut self, alloc: usize, val: Value) -> Step<()> {
        let ty = self
            .heap
            .get(alloc)
            .and_then(|allocation| allocation.root_ty)
            .ok_or_else(|| {
                unsupported(
                    unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                    "raw allocation has no typed view",
                )
            })?;
        if self
            .heap
            .get(alloc)
            .is_some_and(|allocation| allocation.freed)
        {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                "promoted allocation was freed",
            ));
        }
        let (bytes, initialized, provenance) = self.encode_value(&val, ty)?;
        let allocation = &mut self.heap[alloc];
        if bytes.len() != allocation.bytes.len() {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                "promoted value layout changed",
            ));
        }
        allocation.bytes.copy_from_slice(&bytes);
        allocation.initialized.copy_from_slice(&initialized);
        allocation.provenance.clone_from_slice(&provenance);
        Ok(())
    }

    /// Read a local slot, honoring promotion.
    fn load_local(&self, frame: &Frame, slot: u32) -> Step<Value> {
        if let Some(&a) = frame.promoted.get(&promotion_key(PlaceBase::Local(slot))) {
            return self.promoted_slot_value(a);
        }
        Self::get_local(frame, slot)
    }

    /// Write a local slot, honoring promotion.
    fn store_local(&mut self, frame: &mut Frame, slot: u32, val: Value) -> Step<()> {
        if let Some(&a) = frame.promoted.get(&promotion_key(PlaceBase::Local(slot))) {
            self.set_promoted_slot(a, val)
        } else {
            Self::set_local(frame, slot, val);
            Ok(())
        }
    }

    /// Query the canonical layout authority without allowing malformed CFG
    /// handles to panic the oracle. A complete semantic type normally makes
    /// this infallible; the checked wrapper is required at the oracle boundary
    /// because generated/corpus inputs may contain stale recovery handles.
    fn try_layout(&self, ty: Type) -> Option<rue_air::Layout> {
        if !self.is_materializable_type(ty) {
            return None;
        }
        catch_unwind(AssertUnwindSafe(|| self.type_pool().layout(ty))).ok()
    }

    /// Byte size of one value of `ty` from the compact-layout authority.
    fn try_type_byte_size(&self, ty: Type) -> Option<u64> {
        self.try_layout(ty).map(|layout| layout.size)
    }

    fn is_materializable_type(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Error | TypeKind::Never | TypeKind::ComptimeType | TypeKind::Module(_) => {
                false
            }
            TypeKind::Struct(id) => self.type_pool().try_struct_def(id).is_some_and(|def| {
                def.fields
                    .iter()
                    .all(|field| self.is_materializable_type(field.ty))
            }),
            TypeKind::Array(id) => self
                .type_pool()
                .try_array_def(id)
                .is_some_and(|(element, _)| self.is_materializable_type(element)),
            TypeKind::Enum(id) => self.type_pool().try_enum_def(id).is_some_and(|def| {
                def.variant_payloads
                    .iter()
                    .flatten()
                    .copied()
                    .all(|field| self.is_materializable_type(field))
            }),
            _ => true,
        }
    }

    /// Push a canonical raw allocation. `root_ty` is retained only as the
    /// minimum metadata needed to decode promoted typed views; bytes remain the
    /// sole storage authority.
    fn heap_alloc_bytes(
        &mut self,
        bytes: Vec<u8>,
        initialized: Vec<bool>,
        provenance: Vec<Option<PtrTarget>>,
        root_ty: Option<Type>,
        origin: AllocationOrigin,
        declared_alignment: u64,
    ) -> usize {
        debug_assert_eq!(bytes.len(), initialized.len());
        debug_assert_eq!(bytes.len(), provenance.len());
        self.heap.push(Allocation {
            bytes,
            initialized,
            provenance,
            root_ty,
            freed: false,
            generation: 1,
            free_list_next: None,
            origin,
            declared_alignment,
            owner_depth: (origin == AllocationOrigin::Promoted).then_some(self.depth),
        });
        self.heap.len() - 1
    }

    /// Return the head of a size-class free list. Lists are linked through the
    /// freed allocation cells, while the head itself is stored by class so a
    /// reuse never scans the abstract heap.
    fn small_free_list_head(&self, block_size: usize) -> Option<usize> {
        self.small_free_heads[small_class_index(block_size)]
    }

    fn push_small_free_list(&mut self, alloc: usize) {
        let Some(SmallAllocationClass::Block(block_size)) = self
            .heap
            .get(alloc)
            .and_then(|allocation| {
                (allocation.origin == AllocationOrigin::Heap).then(|| {
                    small_allocation_class(
                        allocation.bytes.len() as i128,
                        allocation.declared_alignment,
                    )
                })
            })
            .flatten()
        else {
            if let Some(allocation) = self.heap.get_mut(alloc) {
                allocation.free_list_next = None;
            }
            return;
        };
        let head = self.small_free_list_head(block_size);
        self.heap[alloc].free_list_next = head;
        self.small_free_heads[small_class_index(block_size)] = Some(alloc);
    }

    /// Encode one typed value using Rue's target representation. All integer
    /// and pointer words are explicitly little-endian; host memory is never
    /// inspected or transmuted.
    fn encode_value(
        &self,
        value: &Value,
        ty: Type,
    ) -> Step<(Vec<u8>, Vec<bool>, Vec<Option<PtrTarget>>)> {
        if !self.is_materializable_type(ty) {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                "type has no target representation",
            ));
        }
        let size = usize::try_from(self.try_type_byte_size(ty).ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                "type has no target layout",
            )
        })?)
        .map_err(|_| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                "layout exceeds host range",
            )
        })?;
        let mut bytes = vec![0; size];
        // Padding bytes are not an observable part of a typed value. Keep them
        // uninitialized; recursive decodes validate only field bytes.
        let mut initialized = vec![false; size];
        let mut provenance = vec![None; size];
        let layout = self.try_layout(ty).ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                "type has no target layout",
            )
        })?;
        match (ty.kind(), value) {
            (TypeKind::Bool, Value::Bool(b)) => {
                bytes[0] = u8::from(*b);
                initialized[0] = true;
            }
            (
                TypeKind::I8
                | TypeKind::I16
                | TypeKind::I32
                | TypeKind::I64
                | TypeKind::U8
                | TypeKind::U16
                | TypeKind::U32
                | TypeKind::U64,
                v,
            ) => {
                let (bits, _) = int_shape(ty)?;
                initialized.fill(true);
                let mut raw = to_bits(v.as_int(), bits);
                for byte in &mut bytes {
                    *byte = raw as u8;
                    raw >>= 8;
                }
            }
            (TypeKind::PtrConst(_) | TypeKind::PtrMut(_), Value::Ptr(ptr)) => {
                let address = ptr.as_ref().map_or(0, |target| self.ptr_address(target));
                initialized.fill(true);
                bytes[..size.min(8)].copy_from_slice(&address.to_le_bytes()[..size.min(8)]);
                if let Some(target) = ptr {
                    for marker in &mut provenance[..size] {
                        *marker = Some(target.clone());
                    }
                } else {
                    let marker = Self::canonical_null_marker();
                    for slot in &mut provenance[..size] {
                        *slot = Some(marker.clone());
                    }
                }
            }
            (TypeKind::Unit | TypeKind::Never, Value::Unit) => {}
            (TypeKind::Struct(id), Value::Aggregate(fields)) => {
                initialized.fill(true);
                let def = self.type_pool().try_struct_def(id).ok_or_else(|| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "struct type metadata",
                    )
                })?;
                let LayoutKind::Struct { field_offsets, .. } = layout.kind else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "struct layout kind",
                    ));
                };
                if fields.len() != def.fields.len() {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "struct value shape",
                    ));
                }
                for ((field, value), offset) in def.fields.iter().zip(fields).zip(field_offsets) {
                    let (field_bytes, field_init, field_prov) =
                        self.encode_value(value, field.ty)?;
                    let start = usize::try_from(offset).map_err(|_| {
                        unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "field offset exceeds host range",
                        )
                    })?;
                    let end = start.checked_add(field_bytes.len()).ok_or_else(|| {
                        unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "field representation offset overflow",
                        )
                    })?;
                    if end > bytes.len() {
                        return Err(unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "field representation exceeds layout",
                        ));
                    }
                    bytes[start..end].copy_from_slice(&field_bytes);
                    initialized[start..end].copy_from_slice(&field_init);
                    provenance[start..end].clone_from_slice(&field_prov);
                }
            }
            (TypeKind::Array(id), Value::Aggregate(elements)) => {
                initialized.fill(true);
                let (element_ty, count) = self.type_pool().try_array_def(id).ok_or_else(|| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "array type metadata",
                    )
                })?;
                let LayoutKind::Array { element, .. } = layout.kind else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "array layout kind",
                    ));
                };
                let count = usize::try_from(count).map_err(|_| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "array count exceeds host range",
                    )
                })?;
                if elements.len() != count {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "array value shape",
                    ));
                }
                let stride = usize::try_from(element.stride).map_err(|_| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "array stride exceeds host range",
                    )
                })?;
                for (index, value) in elements.iter().enumerate() {
                    let (field_bytes, field_init, field_prov) =
                        self.encode_value(value, element_ty)?;
                    let start = index.checked_mul(stride).ok_or_else(|| {
                        unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "array representation offset overflow",
                        )
                    })?;
                    let end = start.checked_add(field_bytes.len()).ok_or_else(|| {
                        unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "array representation offset overflow",
                        )
                    })?;
                    if end > bytes.len() {
                        return Err(unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "array element exceeds layout",
                        ));
                    }
                    bytes[start..end].copy_from_slice(&field_bytes);
                    initialized[start..end].copy_from_slice(&field_init);
                    provenance[start..end].clone_from_slice(&field_prov);
                }
            }
            (TypeKind::Enum(id), value) => {
                initialized.fill(true);
                let def = self.type_pool().try_enum_def(id).ok_or_else(|| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "enum type metadata",
                    )
                })?;
                let (tag, fields) = match value {
                    Value::Int(tag) => (*tag as usize, &[][..]),
                    Value::Aggregate(values) if !values.is_empty() => {
                        (values[0].as_int() as usize, &values[1..])
                    }
                    _ => {
                        return Err(unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "enum value shape",
                        ));
                    }
                };
                let tag_value = match value {
                    Value::Int(tag) => *tag,
                    Value::Aggregate(values) => values[0].as_int(),
                    _ => unreachable!(),
                };
                let LayoutKind::Enum {
                    tag: tag_layout,
                    variants,
                    payload_offset,
                    ..
                } = layout.kind
                else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "enum layout kind",
                    ));
                };
                let tag_size = usize::try_from(tag_layout.size).map_err(|_| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "enum tag exceeds host range",
                    )
                })?;
                if tag_size > bytes.len() {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "enum tag exceeds layout",
                    ));
                }
                if tag >= def.variant_count() || tag >= variants.len() {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "enum discriminant outside representation",
                    ));
                }
                let mut raw = tag_value as u128;
                initialized[..tag_size].fill(true);
                for byte in &mut bytes[..tag_size] {
                    *byte = raw as u8;
                    raw >>= 8;
                }
                let Some(payload) = def.variant_payloads.get(tag) else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "enum payload metadata is incomplete",
                    ));
                };
                if payload.len() != fields.len() {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrWrite,
                        ),
                        "enum payload shape",
                    ));
                }
                for ((field_ty, value), offset) in payload.iter().zip(fields).zip(&variants[tag]) {
                    let (field_bytes, field_init, field_prov) =
                        self.encode_value(value, *field_ty)?;
                    let start = usize::try_from(*offset).map_err(|_| {
                        unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "enum field offset exceeds host range",
                        )
                    })?;
                    let end = start.checked_add(field_bytes.len()).ok_or_else(|| {
                        unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "enum field offset overflow",
                        )
                    })?;
                    if end > bytes.len() {
                        return Err(unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrWrite,
                            ),
                            "enum field exceeds layout",
                        ));
                    }
                    bytes[start..end].copy_from_slice(&field_bytes);
                    initialized[start..end].copy_from_slice(&field_init);
                    provenance[start..end].clone_from_slice(&field_prov);
                }
                let _ = payload_offset;
            }
            _ => {
                return Err(unsupported(
                    unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                    "value/type representation mismatch",
                ));
            }
        }
        Ok((bytes, initialized, provenance))
    }

    /// Decode one complete typed value from canonical bytes. The initialized
    /// and provenance checks make partial pointer/value observations typed gaps.
    fn decode_value(
        &self,
        bytes: &[u8],
        initialized: &[bool],
        provenance: &[Option<PtrTarget>],
        ty: Type,
    ) -> Step<Value> {
        let size = usize::try_from(self.try_type_byte_size(ty).ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                "type has no target layout",
            )
        })?)
        .map_err(|_| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                "layout exceeds host range",
            )
        })?;
        if bytes.len() < size || initialized.len() < size || provenance.len() < size {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                "typed view exceeds allocation",
            ));
        }
        let layout = self.try_layout(ty).ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                "type has no target layout",
            )
        })?;
        match ty.kind() {
            TypeKind::Bool => {
                if !initialized[0] {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "read of uninitialized bool",
                    ));
                }
                match bytes[0] {
                    0 => Ok(Value::Bool(false)),
                    1 => Ok(Value::Bool(true)),
                    _ => Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "invalid bool representation",
                    )),
                }
            }
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64 => {
                if initialized[..size].iter().any(|ready| !ready) {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "read of uninitialized integer representation",
                    ));
                }
                let (bits, kind) = int_shape(ty)?;
                let mut raw = 0u128;
                for (index, byte) in bytes[..size].iter().enumerate() {
                    raw |= (*byte as u128) << (index * 8);
                }
                Ok(Value::Int(from_bits(raw, bits, kind_signed(kind))))
            }
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => {
                if initialized[..size].iter().any(|ready| !ready) {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "read of uninitialized pointer representation",
                    ));
                }
                let mut raw = 0u128;
                for (index, byte) in bytes[..size.min(16)].iter().enumerate() {
                    raw |= (*byte as u128) << (index * 8);
                }
                if raw == 0 {
                    // A complete initialized zero representation is the
                    // canonical null pointer, even when it has no provenance
                    // marker (spec 9.2:6c). Partial words were rejected above;
                    // nonzero words still require copied allocation identity.
                    return Ok(Value::Ptr(None));
                }
                let Some(first) = provenance[..size].first().and_then(|p| p.clone()) else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "pointer representation lacks provenance",
                    ));
                };
                if provenance[..size]
                    .iter()
                    .any(|p| p.as_ref() != Some(&first))
                    || self.ptr_address(&first) != raw
                {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "pointer representation provenance mismatch",
                    ));
                }
                let Some((pointee, _)) = self.pointer_pointee(ty) else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "pointer representation has no pointee type",
                    ));
                };
                let _ = pointee;
                Ok(Value::Ptr(Some(first)))
            }
            TypeKind::Unit | TypeKind::Never => Ok(Value::Unit),
            TypeKind::Struct(id) => {
                let def = self.type_pool().try_struct_def(id).ok_or_else(|| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "struct type metadata",
                    )
                })?;
                let LayoutKind::Struct { field_offsets, .. } = layout.kind else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "struct layout kind",
                    ));
                };
                let mut fields = Vec::with_capacity(def.fields.len());
                for (field, offset) in def.fields.iter().zip(field_offsets) {
                    let start = usize::try_from(offset).map_err(|_| {
                        unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrRead,
                            ),
                            "struct field offset exceeds host range",
                        )
                    })?;
                    if start > size {
                        return Err(unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrRead,
                            ),
                            "struct field offset outside representation",
                        ));
                    }
                    fields.push(self.decode_value(
                        &bytes[start..],
                        &initialized[start..],
                        &provenance[start..],
                        field.ty,
                    )?);
                }
                Ok(Value::Aggregate(fields))
            }
            TypeKind::Array(id) => {
                let (element_ty, count) = self.type_pool().try_array_def(id).ok_or_else(|| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "array type metadata",
                    )
                })?;
                let LayoutKind::Array { element, .. } = layout.kind else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "array layout kind",
                    ));
                };
                let count = usize::try_from(count).map_err(|_| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "array count exceeds host range",
                    )
                })?;
                let stride = usize::try_from(element.stride).map_err(|_| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "array stride exceeds host range",
                    )
                })?;
                let mut values = Vec::with_capacity(count);
                for index in 0..count {
                    let start = index.checked_mul(stride).ok_or_else(|| {
                        unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrRead,
                            ),
                            "array element offset overflow",
                        )
                    })?;
                    if start > size {
                        return Err(unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrRead,
                            ),
                            "array element offset outside representation",
                        ));
                    }
                    values.push(self.decode_value(
                        &bytes[start..],
                        &initialized[start..],
                        &provenance[start..],
                        element_ty,
                    )?);
                }
                Ok(Value::Aggregate(values))
            }
            TypeKind::Enum(id) => {
                let def = self.type_pool().try_enum_def(id).ok_or_else(|| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "enum type metadata",
                    )
                })?;
                let LayoutKind::Enum { tag, variants, .. } = layout.kind else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "enum layout kind",
                    ));
                };
                let tag_size = usize::try_from(tag.size).map_err(|_| {
                    unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "enum tag exceeds host range",
                    )
                })?;
                if tag_size > size {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "enum tag exceeds layout",
                    ));
                }
                if initialized[..tag_size].iter().any(|ready| !ready) {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "read of uninitialized enum discriminant",
                    ));
                }
                let mut raw = 0usize;
                for (index, byte) in bytes[..tag_size].iter().enumerate() {
                    raw |= (*byte as usize) << (index * 8);
                }
                if raw >= def.variant_count() || raw >= variants.len() {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "enum discriminant outside representation",
                    ));
                }
                let Some(payload) = def.variant_payloads.get(raw) else {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::PtrRead,
                        ),
                        "enum payload metadata is incomplete",
                    ));
                };
                if payload.is_empty() {
                    return Ok(Value::Int(raw as i128));
                }
                let mut values = vec![Value::Int(raw as i128)];
                for (field_ty, offset) in payload.iter().zip(&variants[raw]) {
                    let start = usize::try_from(*offset).map_err(|_| {
                        unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrRead,
                            ),
                            "enum field offset exceeds host range",
                        )
                    })?;
                    let field_end = start
                        .checked_add(
                            usize::try_from(self.try_type_byte_size(*field_ty).ok_or_else(
                                || {
                                    unsupported(
                                        unsupported_intrinsic_kind_for_operation(
                                            rue_air::IntrinsicOperation::PtrRead,
                                        ),
                                        "enum field has no target layout",
                                    )
                                },
                            )?)
                            .map_err(|_| {
                                unsupported(
                                    unsupported_intrinsic_kind_for_operation(
                                        rue_air::IntrinsicOperation::PtrRead,
                                    ),
                                    "enum field layout exceeds host range",
                                )
                            })?,
                        )
                        .ok_or_else(|| {
                            unsupported(
                                unsupported_intrinsic_kind_for_operation(
                                    rue_air::IntrinsicOperation::PtrRead,
                                ),
                                "enum field offset overflow",
                            )
                        })?;
                    if field_end > size {
                        return Err(unsupported(
                            unsupported_intrinsic_kind_for_operation(
                                rue_air::IntrinsicOperation::PtrRead,
                            ),
                            "enum field offset outside representation",
                        ));
                    }
                    values.push(self.decode_value(
                        &bytes[start..],
                        &initialized[start..],
                        &provenance[start..],
                        *field_ty,
                    )?);
                }
                Ok(Value::Aggregate(values))
            }
            _ => Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrRead),
                "unsupported typed representation",
            )),
        }
    }

    fn heap_alloc_value(&mut self, root: Value, ty: Type, _wrapped_scalar: bool) -> Step<usize> {
        let size = usize::try_from(self.try_type_byte_size(ty).ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                "type has no target layout",
            )
        })?)
        .map_err(|_| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::PtrWrite),
                "layout exceeds host range",
            )
        })?;
        self.materialize_cells_guard(size as i128)?;
        self.reserve_heap_metadata(size)?;
        let (bytes, initialized, provenance) = self.encode_value(&root, ty)?;
        let alignment = self
            .try_layout(ty)
            .map_or(1, |layout| layout.alignment.max(1));
        Ok(self.heap_alloc_bytes(
            bytes,
            initialized,
            provenance,
            Some(ty),
            AllocationOrigin::Promoted,
            alignment,
        ))
    }

    /// Synthesize a stable non-null address for `target`, used by `@ptr_to_int`.
    fn ptr_address(&self, target: &PtrTarget) -> u128 {
        HEAP_BASE + (target.alloc as u128) * HEAP_SEG + target.byte_offset as u128
    }

    /// Decode a synthetic address back to a pointer target, honoring byte-offset
    /// arithmetic performed on a `@ptr_to_int` value. Returns `None` for an
    /// address that names no known allocation (an arbitrary integer, which stays
    /// an unsupported `@int_to_ptr`).
    fn address_target(&self, addr: u128, provenance: &PtrTarget) -> Option<PtrTarget> {
        if addr < HEAP_BASE {
            return None;
        }
        let n = addr - HEAP_BASE;
        let alloc = usize::try_from(n / HEAP_SEG).ok()?;
        if alloc != provenance.alloc || alloc >= self.heap.len() {
            return None;
        }
        let allocation = self.heap.get(alloc)?;
        if allocation.generation != provenance.generation || allocation.freed {
            return None;
        }
        let byte_off = u64::try_from(n % HEAP_SEG).ok()?;
        if byte_off > u64::try_from(allocation.bytes.len()).ok()? {
            return None;
        }
        Some(PtrTarget {
            alloc,
            generation: allocation.generation,
            byte_offset: byte_off,
        })
    }

    fn allocation_for_target(&self, target: &PtrTarget, gap: UnsupportedKind) -> Step<&Allocation> {
        let allocation = self
            .heap
            .get(target.alloc)
            .ok_or_else(|| unsupported(gap, "pointer to unknown allocation"))?;
        if allocation.generation != target.generation {
            return Err(unsupported(gap, "pointer has stale allocation provenance"));
        }
        if allocation.freed {
            return Err(unsupported(gap, "pointer read after free"));
        }
        Ok(allocation)
    }

    fn allocation_for_target_mut(
        &mut self,
        target: &PtrTarget,
        gap: UnsupportedKind,
    ) -> Step<&mut Allocation> {
        let allocation = self
            .heap
            .get_mut(target.alloc)
            .ok_or_else(|| unsupported(gap, "pointer to unknown allocation"))?;
        if allocation.generation != target.generation {
            return Err(unsupported(gap, "pointer has stale allocation provenance"));
        }
        if allocation.freed {
            return Err(unsupported(gap, "pointer read after free"));
        }
        Ok(allocation)
    }

    /// Read the typed view at `target` from canonical bytes.
    fn ptr_cell_read(&self, target: &PtrTarget, ty: Type, gap: UnsupportedKind) -> Step<Value> {
        self.ptr_cell_read_impl(target, ty, gap, true)
    }

    fn ptr_cell_read_unaligned(
        &self,
        target: &PtrTarget,
        ty: Type,
        gap: UnsupportedKind,
    ) -> Step<Value> {
        self.ptr_cell_read_impl(target, ty, gap, false)
    }

    fn ptr_cell_read_impl(
        &self,
        target: &PtrTarget,
        ty: Type,
        gap: UnsupportedKind,
        aligned: bool,
    ) -> Step<Value> {
        let alloc = self.allocation_for_target(target, gap)?;
        let layout = self
            .try_layout(ty)
            .ok_or_else(|| unsupported(gap, "pointee has no target layout"))?;
        let alignment = layout.alignment.max(1);
        if aligned && !target.byte_offset.is_multiple_of(alignment) {
            return Err(unsupported(gap, "misaligned typed read"));
        }
        let start = usize::try_from(target.byte_offset)
            .map_err(|_| unsupported(gap, "pointer read offset exceeds host range"))?;
        let end = start
            .checked_add(
                usize::try_from(
                    self.try_type_byte_size(ty)
                        .ok_or_else(|| unsupported(gap, "pointee has no target layout"))?,
                )
                .map_err(|_| unsupported(gap, "pointee layout exceeds host range"))?,
            )
            .ok_or_else(|| unsupported(gap, "pointer read offset overflow"))?;
        if end > alloc.bytes.len() {
            return Err(unsupported(gap, "pointer read out of bounds"));
        }
        self.decode_value(
            &alloc.bytes[start..end],
            &alloc.initialized[start..end],
            &alloc.provenance[start..end],
            ty,
        )
        .map_err(|flow| match flow {
            Flow::Unsupported(mut u) => {
                u.kind = gap;
                Flow::Unsupported(u)
            }
            other => other,
        })
    }

    /// Encode a typed value directly into canonical bytes.
    fn ptr_cell_write(
        &mut self,
        target: &PtrTarget,
        ty: Type,
        val: Value,
        gap: UnsupportedKind,
    ) -> Step<()> {
        self.ptr_cell_write_impl(target, ty, val, gap, true)
    }

    fn ptr_cell_write_unaligned(
        &mut self,
        target: &PtrTarget,
        ty: Type,
        val: Value,
        gap: UnsupportedKind,
    ) -> Step<()> {
        self.ptr_cell_write_impl(target, ty, val, gap, false)
    }

    fn ptr_cell_write_impl(
        &mut self,
        target: &PtrTarget,
        ty: Type,
        val: Value,
        gap: UnsupportedKind,
        aligned: bool,
    ) -> Step<()> {
        let layout = self
            .try_layout(ty)
            .ok_or_else(|| unsupported(gap, "pointee has no target layout"))?;
        let (bytes, initialized, provenance) =
            self.encode_value(&val, ty).map_err(|flow| match flow {
                Flow::Unsupported(mut u) => {
                    u.kind = gap;
                    Flow::Unsupported(u)
                }
                other => other,
            })?;
        let alignment = layout.alignment.max(1);
        if aligned && !target.byte_offset.is_multiple_of(alignment) {
            return Err(unsupported(gap, "misaligned typed write"));
        }
        let alloc = self.allocation_for_target_mut(target, gap)?;
        let start = usize::try_from(target.byte_offset)
            .map_err(|_| unsupported(gap, "pointer write offset exceeds host range"))?;
        let end = start
            .checked_add(bytes.len())
            .ok_or_else(|| unsupported(gap, "pointer write offset overflow"))?;
        if end > alloc.bytes.len() {
            return Err(unsupported(gap, "pointer write out of bounds"));
        }
        alloc.bytes[start..end].copy_from_slice(&bytes);
        alloc.initialized[start..end].copy_from_slice(&initialized);
        alloc.provenance[start..end].clone_from_slice(&provenance);
        Ok(())
    }

    /// Read one byte from a byte-store allocation at `target.byte_offset + offset`.
    fn byte_at(&self, target: &PtrTarget, offset: i128, gap: UnsupportedKind) -> Step<u8> {
        let alloc = self.allocation_for_target(target, gap)?;
        let at = (target.byte_offset as i128)
            .checked_add(offset)
            .ok_or_else(|| unsupported(gap, "byte offset overflow"))?;
        let at = usize::try_from(at)
            .map_err(|_| unsupported(gap, "byte read offset exceeds host range"))?;
        if at >= alloc.bytes.len() {
            return Err(unsupported(gap, "byte read out of bounds"));
        }
        if !alloc.initialized[at] {
            return Err(unsupported(gap, "byte read of uninitialized storage"));
        }
        Ok(alloc.bytes[at])
    }

    fn byte_range(
        &self,
        target: &PtrTarget,
        count: usize,
        gap: UnsupportedKind,
    ) -> Step<(usize, usize, usize)> {
        let allocation = self.allocation_for_target(target, gap)?;
        let start = usize::try_from(target.byte_offset)
            .map_err(|_| unsupported(gap, "byte offset exceeds host range"))?;
        let end = start
            .checked_add(count)
            .ok_or_else(|| unsupported(gap, "byte range overflow"))?;
        if end > allocation.bytes.len() {
            return Err(unsupported(gap, "byte range out of bounds"));
        }
        Ok((target.alloc, start, end))
    }

    /// Ensure the place's owning slot is heap-backed (promoted) and return a
    /// pointer to the addressed pointee cell. `pointee` sizes the allocation's
    /// address stride.
    fn promote_place(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        place: &Place,
        pointee: Type,
    ) -> Step<PtrTarget> {
        let (base, path) = self.resolve_path(cfg, frame, place)?;
        if let Some(target) =
            self.external_place_target(cfg, frame, base, place.base_type, pointee, &path)?
        {
            return Ok(target);
        }
        let key = promotion_key(base);
        // A pre-splice accessor can represent a borrowed receiver as the
        // canonical pointer value itself rather than in `param_places`. In
        // that form, a nested place such as `self.values` must stay rooted at
        // the pointer's allocation; promoting the base again would interpret
        // the pointer representation as the enclosing aggregate and lose the
        // receiver path.
        if let Value::Ptr(Some(mut target)) = self.base_value_of(frame, base)? {
            let (offset, view) =
                self.projection_offset(place.base_type, &path)
                    .ok_or_else(|| {
                        unsupported(
                            UnsupportedKind::ContractViolation(
                                ContractViolationKind::PlaceProjectionMetadata,
                            ),
                            "pointer-backed place projection layout",
                        )
                    })?;
            target.byte_offset = target.byte_offset.checked_add(offset).ok_or_else(|| {
                unsupported(
                    UnsupportedKind::ContractViolation(
                        ContractViolationKind::PlaceProjectionMetadata,
                    ),
                    "pointer-backed place projection offset overflow",
                )
            })?;
            debug_assert_eq!(view, pointee);
            return Ok(target);
        }
        let alloc = if let Some(&a) = frame.promoted.get(&key) {
            a
        } else {
            let value = self.base_value_of(frame, base)?;
            let a = self.heap_alloc_value(value, place.base_type, false)?;
            frame.promoted.insert(key, a);
            a
        };
        let (byte_offset, _) = self
            .projection_offset(place.base_type, &path)
            .ok_or_else(|| {
                unsupported(
                    UnsupportedKind::ContractViolation(
                        ContractViolationKind::PlaceProjectionMetadata,
                    ),
                    "place projection layout",
                )
            })?;
        Ok(PtrTarget {
            alloc,
            generation: self.heap[alloc].generation,
            byte_offset,
        })
    }

    /// `@bitCast` (RUE-952, spec 4.13:118): reinterpret the operand's
    /// `N`-bit two's-complement pattern at the same-width target type.
    ///
    /// This is total — no bound is checked and no trap is reachable, unlike
    /// `IntCast` — so the model is exactly the value model's own bit
    /// round trip: take the operand's pattern at its width, then read that
    /// pattern back at the target's signedness. Sema guarantees both types are
    /// integers of one width; a CFG that violates that is a contract failure,
    /// not a gap.
    fn eval_bit_cast(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        args: &[CfgValue],
        result_ty: Type,
    ) -> Step<Value> {
        let [arg] = args else {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity),
                "@bitCast arity",
            ));
        };
        let source_ty = cfg.get_inst(*arg).ty;
        let (source_bits, _) = int_shape(source_ty)?;
        let (target_bits, target_kind) = int_shape(result_ty)?;
        if source_bits != target_bits {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                "@bitCast operand and result widths differ",
            ));
        }
        let value = self.eval(cfg, frame, *arg)?.as_int();
        let pattern = to_bits(value, source_bits);
        Ok(Value::Int(from_bits(
            pattern,
            target_bits,
            kind_signed(target_kind),
        )))
    }

    /// Execute a heap/pointer intrinsic whose signature already validated as a
    /// modeled model-gap kind. Returns `Ok(None)` to fall through to the existing
    /// typed-gap path (for example an arbitrary-integer `@int_to_ptr`).
    fn eval_pointer_intrinsic(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        operation: rue_air::IntrinsicOperation,
        args: &[CfgValue],
        result_ty: Type,
    ) -> Step<Option<Value>> {
        let gap = unsupported_intrinsic_kind_for_operation(operation);
        // Evaluate operands eagerly and left-to-right, matching native lowering,
        // except `@raw`/`@raw_mut`/`@field_ptr` whose operand is an lvalue place.
        match operation {
            rue_air::IntrinsicOperation::Raw
            | rue_air::IntrinsicOperation::RawMut
            | rue_air::IntrinsicOperation::FieldPtr => {
                let pointee = cfg.get_inst(args[0]).ty;
                let target = match &cfg.get_inst(args[0]).data {
                    CfgInstData::PlaceRead { place } => {
                        self.promote_place(cfg, frame, place, pointee)?
                    }
                    CfgInstData::Load { slot } => {
                        let place = Place::local(*slot, pointee);
                        self.promote_place(cfg, frame, &place, pointee)?
                    }
                    CfgInstData::Param { index } => {
                        let place = Place::param(*index, pointee);
                        self.promote_place(cfg, frame, &place, pointee)?
                    }
                    other => {
                        return Err(unsupported(
                            UnsupportedKind::ContractViolation(
                                ContractViolationKind::InoutArgumentNotLvalue,
                            ),
                            format!("address-of operand is not an lvalue: {other:?}"),
                        ));
                    }
                };
                Ok(Some(Value::Ptr(Some(target))))
            }
            // `@alloc(size, align)` and `@alloc_zeroed(size, align)` reserve
            // representation bytes; only the latter marks those bytes
            // initialized. Typed reads from ordinary `@alloc` therefore stay
            // fail-closed until the program writes the representation.
            rue_air::IntrinsicOperation::Alloc | rue_air::IntrinsicOperation::AllocZeroed => {
                let size = self.eval(cfg, frame, args[0])?.as_int();
                let align = self.eval(cfg, frame, args[1])?.as_int();
                Ok(Some(self.do_alloc(
                    size,
                    operation == rue_air::IntrinsicOperation::AllocZeroed,
                    align,
                )?))
            }
            rue_air::IntrinsicOperation::Free => {
                let p = self.eval(cfg, frame, args[0])?;
                let size = self.eval(cfg, frame, args[1])?.as_int();
                let align = self.eval(cfg, frame, args[2])?.as_int();
                self.do_free(p, size, align, gap)?;
                Ok(Some(Value::Unit))
            }
            rue_air::IntrinsicOperation::Realloc => {
                let p = self.eval(cfg, frame, args[0])?;
                let old_size = self.eval(cfg, frame, args[1])?.as_int();
                let align = self.eval(cfg, frame, args[2])?.as_int();
                let new_size = self.eval(cfg, frame, args[3])?.as_int();
                Ok(Some(self.do_realloc(p, old_size, align, new_size)?))
            }
            // `@resize` is in-place-only and the interpreter's allocator has no
            // size classes to grow into, so the honest model is "never resizes
            // in place": it changes nothing and reports `false`, which the
            // native allocator is also always free to do. A program whose
            // result depends on a `true` here is depending on allocator
            // internals the language does not promise.
            rue_air::IntrinsicOperation::Resize => {
                let p = self.eval(cfg, frame, args[0])?;
                let old_size = self.eval(cfg, frame, args[1])?.as_int();
                let align = self.eval(cfg, frame, args[2])?.as_int();
                let new_size = self.eval(cfg, frame, args[3])?.as_int();
                if matches!(p, Value::Ptr(None)) {
                    return Err(unsupported(gap, "resize of null pointer"));
                }
                self.validate_allocator_contract(p, old_size, align, gap)?;
                if new_size < 0 {
                    return Err(unsupported(gap, "negative resize size"));
                }
                Ok(Some(Value::Bool(false)))
            }
            rue_air::IntrinsicOperation::PtrRead
            | rue_air::IntrinsicOperation::PtrReadUnaligned => {
                let p = self.eval(cfg, frame, args[0])?;
                let target = self.expect_ptr(p, gap)?;
                let pointee = self
                    .pointer_pointee(cfg.get_inst(args[0]).ty)
                    .map(|(ty, _)| ty)
                    .ok_or_else(|| unsupported(gap, "pointer read operand type"))?;
                let value = if operation == rue_air::IntrinsicOperation::PtrReadUnaligned {
                    self.ptr_cell_read_unaligned(&target, pointee, gap)?
                } else {
                    self.ptr_cell_read(&target, pointee, gap)?
                };
                Ok(Some(value))
            }
            rue_air::IntrinsicOperation::PtrWrite
            | rue_air::IntrinsicOperation::PtrWriteUnaligned => {
                let p = self.eval(cfg, frame, args[0])?;
                let val = self.eval(cfg, frame, args[1])?;
                let target = self.expect_ptr(p, gap)?;
                let pointee = self
                    .pointer_pointee(cfg.get_inst(args[0]).ty)
                    .map(|(ty, _)| ty)
                    .ok_or_else(|| unsupported(gap, "pointer write operand type"))?;
                if operation == rue_air::IntrinsicOperation::PtrWriteUnaligned {
                    self.ptr_cell_write_unaligned(&target, pointee, val, gap)?;
                } else {
                    self.ptr_cell_write(&target, pointee, val, gap)?;
                }
                Ok(Some(Value::Unit))
            }
            rue_air::IntrinsicOperation::PtrOffset => {
                let p = self.eval(cfg, frame, args[0])?;
                let by = self.eval(cfg, frame, args[1])?.as_int();
                match p {
                    Value::Ptr(Some(mut t)) => {
                        let allocation = self.allocation_for_target(&t, gap)?;
                        let operand_ty = cfg.get_inst(args[0]).ty;
                        let (pointee_ty, _) = self
                            .pointer_pointee(operand_ty)
                            .ok_or_else(|| unsupported(gap, "pointer offset operand type"))?;
                        let stride = self
                            .try_type_byte_size(pointee_ty)
                            .ok_or_else(|| unsupported(gap, "pointee has no target layout"))?
                            as i128;
                        let delta = by
                            .checked_mul(stride)
                            .ok_or_else(|| unsupported(gap, "pointer offset overflow"))?;
                        let next = (t.byte_offset as i128)
                            .checked_add(delta)
                            .ok_or_else(|| unsupported(gap, "pointer offset overflow"))?;
                        if next < 0 || next > u64::MAX as i128 {
                            return Err(unsupported(gap, "pointer offset outside address range"));
                        }
                        if next > allocation.bytes.len() as i128 {
                            return Err(unsupported(gap, "pointer offset outside live allocation"));
                        }
                        t.byte_offset = next as u64;
                        Ok(Some(Value::Ptr(Some(t))))
                    }
                    Value::Ptr(None) => Ok(Some(Value::Ptr(None))),
                    _ => Err(unsupported(gap, "@ptr_offset of non-pointer")),
                }
            }
            rue_air::IntrinsicOperation::PtrToInt => {
                let p = self.eval(cfg, frame, args[0])?;
                let value = match p {
                    Value::Ptr(Some(t)) => Value::AddressInt {
                        value: self.ptr_address(&t) as i128,
                        provenance: AddressProvenance(t),
                    },
                    Value::Ptr(None) => Value::Int(0),
                    _ => return Err(unsupported(gap, "@ptr_to_int of non-pointer")),
                };
                Ok(Some(value))
            }
            rue_air::IntrinsicOperation::IntToPtr => {
                let address = self.eval(cfg, frame, args[0])?;
                let (addr, provenance) = match address {
                    Value::AddressInt { value, provenance } => {
                        (value as u64 as u128, Some(provenance.0))
                    }
                    other => (other.as_int() as u64 as u128, None),
                };
                if addr == 0 {
                    return Ok(Some(Value::Ptr(None)));
                }
                let Some((_pointee, _)) = self.pointer_pointee(result_ty) else {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::IntrinsicSignature,
                        ),
                        "@int_to_ptr result is not a pointer",
                    ));
                };
                match provenance.and_then(|p| self.address_target(addr, &p)) {
                    // A pointer-derived integer round-trips to its allocation.
                    Some(target) => Ok(Some(Value::Ptr(Some(target)))),
                    // An arbitrary integer names no allocation: unmodelable,
                    // stays the typed `IntToPointer` model gap.
                    None => Ok(None),
                }
            }
            rue_air::IntrinsicOperation::ByteCopy | rue_air::IntrinsicOperation::ByteMove => {
                let dst = self.eval(cfg, frame, args[0])?;
                let src = self.eval(cfg, frame, args[1])?;
                let count = self.eval(cfg, frame, args[2])?.as_int();
                if count < 0 {
                    return Err(unsupported(gap, "negative byte count"));
                }
                if count == 0 {
                    return Ok(Some(Value::Unit));
                }
                let dst = self.expect_ptr(dst, gap)?;
                let src = self.expect_ptr(src, gap)?;
                let count = usize::try_from(count)
                    .map_err(|_| unsupported(gap, "byte count exceeds host range"))?;
                let (src_alloc, src_start, src_end) = self.byte_range(&src, count, gap)?;
                let (dst_alloc, dst_start, dst_end) = self.byte_range(&dst, count, gap)?;
                if operation == rue_air::IntrinsicOperation::ByteCopy
                    && src_alloc == dst_alloc
                    && src_start < dst_end
                    && dst_start < src_end
                {
                    return Err(unsupported(gap, "byte_copy source and destination overlap"));
                }
                let (bytes, initialized, provenance) = {
                    let allocation = self
                        .heap
                        .get(src_alloc)
                        .ok_or_else(|| unsupported(gap, "source allocation missing"))?;
                    (
                        allocation.bytes[src_start..src_end].to_vec(),
                        allocation.initialized[src_start..src_end].to_vec(),
                        allocation.provenance[src_start..src_end].to_vec(),
                    )
                };
                let allocation = self
                    .heap
                    .get_mut(dst_alloc)
                    .ok_or_else(|| unsupported(gap, "destination allocation missing"))?;
                allocation.bytes[dst_start..dst_end].copy_from_slice(&bytes);
                allocation.initialized[dst_start..dst_end].copy_from_slice(&initialized);
                allocation.provenance[dst_start..dst_end].clone_from_slice(&provenance);
                Ok(Some(Value::Unit))
            }
            rue_air::IntrinsicOperation::ByteSet => {
                let p = self.eval(cfg, frame, args[0])?;
                let val = self.eval(cfg, frame, args[1])?.as_int();
                let count = self.eval(cfg, frame, args[2])?.as_int();
                if count < 0 {
                    return Err(unsupported(gap, "negative byte count"));
                }
                if count == 0 {
                    return Ok(Some(Value::Unit));
                }
                let target = self.expect_ptr(p, gap)?;
                let count = usize::try_from(count)
                    .map_err(|_| unsupported(gap, "byte count exceeds host range"))?;
                let (alloc, start, end) = self.byte_range(&target, count, gap)?;
                let allocation = self
                    .heap
                    .get_mut(alloc)
                    .ok_or_else(|| unsupported(gap, "allocation missing"))?;
                allocation.bytes[start..end].fill((val & 0xFF) as u8);
                allocation.initialized[start..end].fill(true);
                allocation.provenance[start..end].fill(None);
                Ok(Some(Value::Unit))
            }
            rue_air::IntrinsicOperation::PanicNoMessage
            | rue_air::IntrinsicOperation::Panic
            | rue_air::IntrinsicOperation::AssertFailed
            | rue_air::IntrinsicOperation::BoundsCheck
            | rue_air::IntrinsicOperation::DebugI64
            | rue_air::IntrinsicOperation::DebugU64
            | rue_air::IntrinsicOperation::DebugBool
            | rue_air::IntrinsicOperation::DebugStr
            | rue_air::IntrinsicOperation::ReadLine
            | rue_air::IntrinsicOperation::ParseI32
            | rue_air::IntrinsicOperation::ParseI64
            | rue_air::IntrinsicOperation::ParseU32
            | rue_air::IntrinsicOperation::ParseU64
            | rue_air::IntrinsicOperation::RandomU32
            | rue_air::IntrinsicOperation::RandomU64
            | rue_air::IntrinsicOperation::ArgCount
            | rue_air::IntrinsicOperation::ArgPtr
            | rue_air::IntrinsicOperation::ArgLen
            | rue_air::IntrinsicOperation::EnvCount
            | rue_air::IntrinsicOperation::EnvPtr
            | rue_air::IntrinsicOperation::EnvLen
            | rue_air::IntrinsicOperation::Syscall
            | rue_air::IntrinsicOperation::IntToFloat
            | rue_air::IntrinsicOperation::FloatToInt
            | rue_air::IntrinsicOperation::FloatCast
            | rue_air::IntrinsicOperation::TotalCmp
            | rue_air::IntrinsicOperation::BitCast => Ok(None),
        }
    }

    fn expect_ptr(&self, v: Value, gap: UnsupportedKind) -> Step<PtrTarget> {
        match v {
            Value::Ptr(Some(t)) => Ok(t),
            // A null-pointer dereference is undefined behavior; the oracle cannot
            // model the faulting read, so it stays a typed gap.
            Value::Ptr(None) => Err(unsupported(gap, "dereference of null pointer")),
            _ => Err(unsupported(gap, "pointer operation on non-pointer")),
        }
    }

    fn checked_alignment(&self, align: i128, gap: UnsupportedKind) -> Step<u64> {
        let align =
            u64::try_from(align).map_err(|_| unsupported(gap, "invalid allocation alignment"))?;
        if align == 0 || !align.is_power_of_two() || align > ORACLE_PAGE_SIZE as u64 {
            return Err(unsupported(gap, "invalid allocation alignment"));
        }
        Ok(align)
    }

    fn validate_allocator_contract(
        &self,
        p: Value,
        size: i128,
        align: i128,
        gap: UnsupportedKind,
    ) -> Step<()> {
        let align = self.checked_alignment(align, gap)?;
        if size < 0 {
            return Err(unsupported(gap, "negative allocation size"));
        }
        let Value::Ptr(Some(target)) = p else {
            return Ok(());
        };
        if target.byte_offset != 0 {
            return Err(unsupported(
                gap,
                "allocator operation requires base ptr mut u8",
            ));
        }
        let allocation = self.allocation_for_target(&target, gap)?;
        if allocation.origin != AllocationOrigin::Heap {
            return Err(unsupported(gap, "allocation is not allocator-owned"));
        }
        if allocation.freed {
            return Err(unsupported(gap, "allocation was already freed"));
        }
        if allocation.bytes.len() as i128 != size {
            return Err(unsupported(
                gap,
                "allocation size does not match live extent",
            ));
        }
        if allocation.declared_alignment != align {
            return Err(unsupported(
                gap,
                "allocation alignment does not match original",
            ));
        }
        Ok(())
    }

    fn do_free(&mut self, p: Value, size: i128, align: i128, gap: UnsupportedKind) -> Step<()> {
        if let Value::Ptr(Some(target)) = &p {
            if let Some(allocation) = self.heap.get(target.alloc)
                && allocation.origin == AllocationOrigin::Text
                && size == 0
                && target.byte_offset == 0
                && allocation.declared_alignment == self.checked_alignment(align, gap)?
            {
                // A literal-backed header carries cap=0 and its text storage
                // is static/non-owning. The drop glue's zero-size release is
                // therefore a canonical no-op, not a free.
                return Ok(());
            }
        }
        self.validate_allocator_contract(p.clone(), size, align, gap)?;
        if let Value::Ptr(Some(target)) = p {
            let allocation = self
                .heap
                .get_mut(target.alloc)
                .ok_or_else(|| unsupported(gap, "allocation missing"))?;
            allocation.freed = true;
            allocation.free_list_next = None;
            self.push_small_free_list(target.alloc);
        }
        Ok(())
    }

    /// `@alloc(size, align)`: reserve canonical representation bytes. Ordinary
    /// allocation bytes are uninitialized; `alloc_zeroed` marks them ready.
    fn do_alloc(&mut self, size: i128, zeroed: bool, align: i128) -> Step<Value> {
        let align = self.checked_alignment(
            align,
            unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Alloc),
        )?;
        let Some(_bytes) = self.alloc_byte_size(size, 1)? else {
            return Ok(Value::Ptr(None));
        };
        self.materialize_cells_guard(size)?;
        let count = usize::try_from(size).map_err(|_| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Alloc),
                "allocation exceeds host range",
            )
        })?;

        // `rue-allocator` recycles freed small blocks by power-of-two class.
        // Reuse the abstract cell itself so pointer identity observes the
        // same LIFO class behavior as native execution. Direct mappings are
        // intentionally not reused: their OS address identity is not a
        // deterministic language observation.
        if let Some(SmallAllocationClass::Block(block_size)) = small_allocation_class(size, align)
            && let Some(alloc) = self.small_free_list_head(block_size)
        {
            let old_len = self.heap[alloc].bytes.len();
            let generation = self.heap[alloc].generation.checked_add(1).ok_or_else(|| {
                unsupported(
                    UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
                    "allocation lifetime generation exhausted",
                )
            })?;
            // Reuse replaces the logical extent. Account for any retained
            // metadata growth before changing the allocation, so reuse cannot
            // bypass the global heap-resource bound.
            if count > old_len {
                self.reserve_heap_metadata(count - old_len)?;
            }
            self.small_free_heads[small_class_index(block_size)] = self.heap[alloc].free_list_next;
            let allocation = &mut self.heap[alloc];
            allocation.bytes = vec![0; count];
            allocation.initialized = vec![zeroed; count];
            allocation.provenance = vec![None; count];
            allocation.root_ty = None;
            allocation.freed = false;
            allocation.generation = generation;
            allocation.free_list_next = None;
            allocation.origin = AllocationOrigin::Heap;
            allocation.declared_alignment = align;
            allocation.owner_depth = None;
            return Ok(Value::Ptr(Some(PtrTarget {
                alloc,
                generation,
                byte_offset: 0,
            })));
        }

        self.reserve_heap_metadata(count)?;
        let alloc = self.heap_alloc_bytes(
            vec![0; count],
            vec![zeroed; count],
            vec![None; count],
            None,
            AllocationOrigin::Heap,
            align,
        );
        Ok(Value::Ptr(Some(PtrTarget {
            alloc,
            generation: self.heap[alloc].generation,
            byte_offset: 0,
        })))
    }

    /// `@realloc`: grow/shrink an allocation according to the runtime
    /// allocator's size-class and page-mapping rules, preserving the first
    /// `min(old, new)` representation bytes and leaving growth uninitialized.
    /// Returns null on `new == 0` or allocator failure, leaving the original
    /// allocation valid (spec 8.6:3), and traps on a `new * stride` overflow
    /// like the compiled size arithmetic (spec 8.6:1).
    fn do_realloc(&mut self, p: Value, old_size: i128, align: i128, new_size: i128) -> Step<Value> {
        let align = self.checked_alignment(
            align,
            unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
        )?;
        if old_size < 0 || new_size < 0 {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "negative realloc size",
            ));
        }
        let target = match p {
            Value::Ptr(Some(t)) => t,
            // realloc(null, .., new_size) behaves like a fresh allocation.
            Value::Ptr(None) => {
                if old_size != 0 {
                    return Err(unsupported(
                        unsupported_intrinsic_kind_for_operation(
                            rue_air::IntrinsicOperation::Realloc,
                        ),
                        "null realloc requires old size zero",
                    ));
                }
                return self.do_alloc(new_size, false, i128::from(align));
            }
            _ => return Ok(Value::Ptr(None)),
        };
        self.allocation_for_target(
            &target,
            unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
        )?;
        if target.byte_offset != 0 {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "realloc requires the allocation base as ptr mut u8",
            ));
        }
        let Some(allocation) = self.heap.get(target.alloc) else {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "realloc allocation missing",
            ));
        };
        if allocation.origin != AllocationOrigin::Heap {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "realloc requires an allocator-owned allocation",
            ));
        }
        if allocation.declared_alignment != align {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "realloc alignment does not match the live allocation",
            ));
        }
        let actual_old_size = allocation.bytes.len() as i128;
        if old_size != actual_old_size {
            return Err(unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "realloc old size does not match the live allocation",
            ));
        }
        if new_size == 0 {
            if let Some(alloc) = self.heap.get_mut(target.alloc) {
                alloc.freed = true;
                alloc.free_list_next = None;
            }
            self.push_small_free_list(target.alloc);
            return Ok(Value::Ptr(None));
        }
        if self.alloc_byte_size(new_size, 1)?.is_none() {
            return Ok(Value::Ptr(None));
        }
        let new_count = usize::try_from(new_size).map_err(|_| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "allocation exceeds host range",
            )
        })?;
        self.materialize_cells_guard(new_size)?;
        let old_kind = allocation_kind(old_size, align).ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "old allocation has no allocator classification",
            )
        })?;
        let new_kind = allocation_kind(new_size, align).ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "new allocation has no allocator classification",
            )
        })?;

        // The allocator can relabel storage in place only when both layouts
        // select the same small class or the same page-rounded mapping.
        if old_kind == new_kind {
            let old_len = self.heap[target.alloc].bytes.len();
            if new_count > old_len {
                self.reserve_heap_metadata(new_count - old_len)?;
            }
            let allocation = &mut self.heap[target.alloc];
            allocation.bytes.resize(new_count, 0);
            allocation.initialized.resize(new_count, false);
            allocation.provenance.resize(new_count, None);
            return Ok(Value::Ptr(Some(PtrTarget {
                alloc: target.alloc,
                generation: allocation.generation,
                byte_offset: 0,
            })));
        }

        // A class/mapping change allocates through the canonical path, so the
        // destination observes the real free-list order. The old allocation
        // is retired only after destination allocation and copying succeed;
        // any failure therefore leaves it live and usable.
        let (old_bytes, old_initialized, old_provenance) = {
            let old = self.heap.get(target.alloc).ok_or_else(|| {
                unsupported(
                    unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                    "realloc allocation missing",
                )
            })?;
            (
                old.bytes.clone(),
                old.initialized.clone(),
                old.provenance.clone(),
            )
        };
        let new_pointer = self.do_alloc(new_size, false, i128::from(align))?;
        let Value::Ptr(Some(new_target)) = new_pointer else {
            return Ok(new_pointer);
        };
        let copy = old_bytes.len().min(new_count);
        let destination = self.heap.get_mut(new_target.alloc).ok_or_else(|| {
            unsupported(
                unsupported_intrinsic_kind_for_operation(rue_air::IntrinsicOperation::Realloc),
                "new allocation missing",
            )
        })?;
        destination.bytes[..copy].copy_from_slice(&old_bytes[..copy]);
        destination.initialized[..copy].copy_from_slice(&old_initialized[..copy]);
        destination.provenance[..copy].clone_from_slice(&old_provenance[..copy]);
        self.heap[target.alloc].freed = true;
        self.heap[target.alloc].free_list_next = None;
        self.push_small_free_list(target.alloc);
        Ok(Value::Ptr(Some(new_target)))
    }

    /// Classify an allocation request by its total byte size:
    /// - `Err(ArithmeticOverflow trap)` when `count * stride` overflows `u64`,
    ///   matching the compiled size arithmetic's checked multiply (spec 8.6:1;
    ///   corpus `alloc_count_size_overflow_traps`).
    /// - `Ok(None)` when the allocator returns null: a zero or negative request,
    ///   or a byte size beyond [`MAX_ALLOC_BYTES`] (the platform `mmap` failure
    ///   the raw intrinsics surface as null, spec 8.6:4).
    /// - `Ok(Some(bytes))` for a satisfiable request.
    fn alloc_byte_size(&self, count: i128, stride: u64) -> Step<Option<u128>> {
        if count < 0 {
            return Ok(None);
        }
        let product = (count as u128) * (stride as u128);
        if product > u64::MAX as u128 {
            return Err(Flow::Panic(Panic::runtime(TrapKind::ArithmeticOverflow)));
        }
        if product == 0 || product > MAX_ALLOC_BYTES {
            return Ok(None);
        }
        Ok(Some(product))
    }

    /// Bound the number of bytes the interpreter will materialize for one
    /// allocation. A satisfiable-but-enormous request (only reachable from a
    /// synthetic fuzz program, never the differential corpus) resolves to a typed
    /// resource limit rather than exhausting harness memory building cells.
    fn materialize_cells_guard(&self, count: i128) -> Step<()> {
        const MAX_MATERIALIZED_BYTES: i128 = 1 << 24;
        if count > MAX_MATERIALIZED_BYTES {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
                "allocation byte count exceeds the interpreter materialization bound",
            ));
        }
        Ok(())
    }

    fn reserve_heap_metadata(&mut self, bytes: usize) -> Step<()> {
        const MAX_HEAP_METADATA_BYTES: usize = 128 * 1024 * 1024;
        let per_byte = 1usize
            .checked_add(std::mem::size_of::<bool>())
            .and_then(|n| n.checked_add(std::mem::size_of::<Option<PtrTarget>>()))
            .ok_or_else(|| {
                unsupported(
                    UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
                    "heap metadata footprint overflow",
                )
            })?;
        let footprint = bytes.checked_mul(per_byte).ok_or_else(|| {
            unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
                "heap metadata footprint overflow",
            )
        })?;
        let next = self
            .heap_metadata_bytes
            .checked_add(footprint)
            .ok_or_else(|| {
                unsupported(
                    UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
                    "cumulative heap metadata footprint overflow",
                )
            })?;
        if next > MAX_HEAP_METADATA_BYTES {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
                "cumulative heap metadata footprint exceeds interpreter limit",
            ));
        }
        self.heap_metadata_bytes = next;
        Ok(())
    }
}

/// Whether the interpreter now executes this pointer/heap intrinsic instead of
/// reporting it as a model gap. Excludes the still-unmodeled text parses; the
/// compiler-owned empty-slice null is a typed `Const(0)`, not an intrinsic.
fn modeled_pointer_intrinsic(kind: UnsupportedIntrinsicKind) -> bool {
    use UnsupportedIntrinsicKind as I;
    matches!(
        kind,
        I::PointerRead
            | I::PointerWrite
            | I::PointerOffset
            | I::PointerToInt
            | I::IntToPointer
            | I::RawAddress
            | I::RawMutableAddress
            | I::FieldPointer
            | I::Allocate
            | I::AllocateZeroed
            | I::Free
            | I::Reallocate
            | I::Resize
            | I::ByteCopy
            | I::ByteMove
            | I::ByteSet
    )
}

/// Stable map key for a promoted place base within a frame.
fn promotion_key(base: PlaceBase) -> u64 {
    match base {
        // Every payload is u32, so two high tag bits give the four base kinds
        // disjoint key spaces. In particular, an indirect value must never
        // alias an odd local's promoted allocation before the oracle reports
        // the unsupported address-of-indirect shape.
        PlaceBase::Local(slot) => slot as u64,
        PlaceBase::Param(slot) => (1u64 << 32) | slot as u64,
        PlaceBase::Accessor(value) => (2u64 << 32) | value.as_u32() as u64,
        PlaceBase::Indirect(value) => (3u64 << 32) | value.as_u32() as u64,
    }
}

/// Strictly decode the UTF-8 scalar starting at byte `offset`, returning
/// `(scalar, utf8_width)`. Backs the oracle's model of the
/// `__rue_str_char_scalar`/`__rue_str_char_next` runtime primitives.
/// Returns `None` when `offset` is out of range or the byte sequence at that
/// offset is invalid UTF-8; callers translate that to the runtime's
/// `invalid UTF-8` trap.
fn char_at(bytes: &[u8], offset: i128) -> Option<(u32, u64)> {
    if offset < 0 || offset as u128 >= bytes.len() as u128 {
        return None;
    }
    let off = offset as usize;
    let b0 = bytes[off];
    if b0 < 0x80 {
        return Some((b0 as u32, 1));
    }
    let (width, min, mut cp) = if (0xC2..=0xDF).contains(&b0) {
        (2usize, 0x80u32, (b0 as u32) & 0x1F)
    } else if (0xE0..=0xEF).contains(&b0) {
        (3usize, 0x800u32, (b0 as u32) & 0x0F)
    } else if (0xF0..=0xF4).contains(&b0) {
        (4usize, 0x10000u32, (b0 as u32) & 0x07)
    } else {
        return None;
    };
    if off + width > bytes.len() {
        return None;
    }
    for b in &bytes[off + 1..off + width] {
        if b & 0xC0 != 0x80 {
            return None;
        }
        cp = (cp << 6) | ((*b as u32) & 0x3F);
    }
    if cp < min || (0xD800..=0xDFFF).contains(&cp) || cp > 0x10FFFF {
        return None;
    }
    Some((cp, width as u64))
}

/// Leniently decode the UTF-8 scalar starting at byte `offset`, replacing
/// invalid UTF-8 with U+FFFD and advancing by the runtime's maximal-subpart
/// width. Backs the oracle's model of the lossy character runtime primitives.
fn char_at_lossy(bytes: &[u8], offset: i128) -> (u32, u64) {
    const FFFD: u32 = 0xFFFD;
    if offset < 0 || offset as u128 >= bytes.len() as u128 {
        return (FFFD, 1);
    }
    let off = offset as usize;
    let b0 = bytes[off];
    if b0 < 0x80 {
        return (b0 as u32, 1);
    }
    let (width, second_lo, second_hi) = match b0 {
        0xC2..=0xDF => (2usize, 0x80u8, 0xBFu8),
        0xE0 => (3, 0xA0, 0xBF),
        0xE1..=0xEC => (3, 0x80, 0xBF),
        0xED => (3, 0x80, 0x9F),
        0xEE..=0xEF => (3, 0x80, 0xBF),
        0xF0 => (4, 0x90, 0xBF),
        0xF1..=0xF3 => (4, 0x80, 0xBF),
        0xF4 => (4, 0x80, 0x8F),
        _ => return (FFFD, 1),
    };
    let mask = match width {
        2 => 0x1F,
        3 => 0x0F,
        _ => 0x07,
    };
    let mut cp = (b0 as u32) & mask;
    if off + 1 >= bytes.len() {
        return (FFFD, 1);
    }
    let b1 = bytes[off + 1];
    if b1 < second_lo || b1 > second_hi {
        return (FFFD, 1);
    }
    cp = (cp << 6) | ((b1 as u32) & 0x3F);
    let mut consumed = 2usize;
    while consumed < width {
        if off + consumed >= bytes.len() {
            return (FFFD, consumed as u64);
        }
        let b = bytes[off + consumed];
        if b & 0xC0 != 0x80 {
            return (FFFD, consumed as u64);
        }
        cp = (cp << 6) | ((b as u32) & 0x3F);
        consumed += 1;
    }
    (cp, width as u64)
}

// ---- integer type helpers -------------------------------------------------

fn int_shape(ty: Type) -> Result<(u32, TypeKind), Flow> {
    let kind = ty.kind();
    let bits = match kind {
        TypeKind::I8 | TypeKind::U8 => 8,
        TypeKind::I16 | TypeKind::U16 => 16,
        TypeKind::I32 | TypeKind::U32 => 32,
        TypeKind::I64 | TypeKind::U64 => 64,
        _ => {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::NonIntegerOperationType),
                format!("non-int type {kind:?}"),
            ));
        }
    };
    Ok((bits, kind))
}

fn kind_signed(kind: TypeKind) -> bool {
    matches!(
        kind,
        TypeKind::I8 | TypeKind::I16 | TypeKind::I32 | TypeKind::I64
    )
}

fn int_bounds(ty: Type) -> Option<(i128, i128)> {
    match ty.kind() {
        TypeKind::I8 => Some((i8::MIN as i128, i8::MAX as i128)),
        TypeKind::I16 => Some((i16::MIN as i128, i16::MAX as i128)),
        TypeKind::I32 => Some((i32::MIN as i128, i32::MAX as i128)),
        TypeKind::I64 => Some((i64::MIN as i128, i64::MAX as i128)),
        TypeKind::U8 => Some((0, u8::MAX as i128)),
        TypeKind::U16 => Some((0, u16::MAX as i128)),
        TypeKind::U32 => Some((0, u32::MAX as i128)),
        TypeKind::U64 => Some((0, u64::MAX as i128)),
        _ => None,
    }
}

/// `None` (a checked-arithmetic overflow) or an out-of-range result traps as a
/// runtime overflow panic (spec 3.1:6/13); otherwise the value is returned.
fn range_check(v: Option<i128>, ty: Type) -> Step<Value> {
    let n = v.ok_or(Flow::Panic(Panic::runtime(TrapKind::ArithmeticOverflow)))?;
    match int_bounds(ty) {
        Some((lo, hi)) if n < lo || n > hi => {
            Err(Flow::Panic(Panic::runtime(TrapKind::ArithmeticOverflow)))
        }
        _ => Ok(Value::Int(n)),
    }
}

fn width_mask(bits: u32) -> u128 {
    if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    }
}

/// Two's-complement bit pattern of `v` at `bits` width.
fn to_bits(v: i128, bits: u32) -> u128 {
    (v as u128) & width_mask(bits)
}

/// Interpret a `bits`-wide two's-complement pattern as an `i128`, sign-extending
/// when `signed`.
fn from_bits(bits_val: u128, bits: u32, signed: bool) -> i128 {
    let masked = bits_val & width_mask(bits);
    if signed && bits < 128 && (masked >> (bits - 1)) & 1 == 1 {
        (masked as i128) - (1i128 << bits)
    } else {
        masked as i128
    }
}

/// Format a value exactly as the `@dbg` runtime intrinsic prints it (decimal for
/// integers respecting signedness, `true`/`false` for bool), sans the newline.
fn format_dbg(val: &Value, ty: Type) -> Result<Cow<'_, str>, Flow> {
    // Text values (`@dbg` of a str/StrBuf) are decoded from the heap by the
    // caller (`write_dbg`); this scalar formatter only sees non-text values.
    Ok(match ty.kind() {
        TypeKind::Bool => Cow::Owned(val.as_bool().to_string()),
        k if kind_signed(k) => Cow::Owned(val.as_int().to_string()),
        TypeKind::U8 | TypeKind::U16 | TypeKind::U32 | TypeKind::U64 => {
            let (lo, hi) = int_bounds(ty).unwrap();
            let n = val.as_int();
            let _ = (lo, hi);
            Cow::Owned((n as u128).to_string())
        }
        other => {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::UnsupportedDebugType),
                format!("@dbg of type {other:?}"),
            ));
        }
    })
}

#[cfg(test)]
mod tests;
