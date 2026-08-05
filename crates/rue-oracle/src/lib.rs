//! # rue-oracle — the executable reference semantics
//!
//! A tree-walking interpreter over the compiler's **CFG** (the typed
//! control-flow IR, produced by `CompilerSession`, *before* the
//! MIR/codegen lowering where every miscompile of the 2026-07 work lived).
//! Running a program through this interpreter and through the compiled binary
//! and comparing the observable behavior (exit code, stdout, stderr, trap cause)
//! is the differential-testing oracle of RUE-50, and the executable form of the
//! operational semantics in `docs/formal/01-core-calculus.md` §6.
//!
//! Because the interpreter shares the compiler's *front half* (parser + sema +
//! CFG build) but is an entirely independent *back half*, a disagreement between
//! it and the compiled binary localizes a bug to lowering/codegen — exactly the
//! layer that has been buggy.
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
//! `inout`/`borrow` parameters (copy-in / copy-out, observably identical to
//! by-reference under the law of exclusivity); and drop/destructors, executed in
//! spec-3.9 order (user destructor, then fields in declaration order / elements
//! ascending) so the oracle validates drop *order* and drop-exactly-once, not
//! just final values; and text values — source-defined `StrBuf` algorithms,
//! `@to_string`, core `str` byte indexing, and strict/lossy scalar iteration.
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
//! - **All other CFG intrinsics.** The `Intrinsic` arm models `@dbg`, `@panic`,
//!   and `@assert`; every other intrinsic that is allowed to survive to the CFG
//!   is a typed model gap: the
//!   non-deterministic `@read_line`, `@random_u32`/`@random_u64`, `@syscall`;
//!   the heap intrinsics `@alloc`/`@free`/`@realloc`; the raw-pointer intrinsics
//!   `@raw`/`@raw_mut`/`@field_ptr`/`@ptr_read`/`@ptr_write`/`@ptr_offset`/
//!   `@ptr_to_int`/`@int_to_ptr` (heap-/layout-dependent, and `checked`-only).
//! - **`String::capacity`/`reserve`/`with_capacity` capacity behavior** — the
//!   exact capacity value is implementation-defined, so `capacity` is reported
//!   as a typed gap rather than guessed. Specified bounds and growth/preservation
//!   relationships can still be modeled independently.
//! - **Deeply-nested `inout` field writes** (non-zero inner offset).

use lasso::ThreadedRodeo;
use rue_air::{FrozenTypeInternPool, RuntimeCallKind, Type, TypeKind, parse_array_type_syntax};
use rue_cfg::{Cfg, CfgArgMode, CfgInstData, CfgValue, Place, PlaceBase, Projection, Terminator};
use rue_compiler::{
    CompileErrors, CompileOptions, CompilerSession, PreviewFeatures, SourceSnapshot,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

struct CompileState {
    interner: ThreadedRodeo,
    functions: Vec<OracleFunction>,
    type_pool: FrozenTypeInternPool,
    strings: Vec<String>,
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
    let semantic = session.semantic(options)?;
    drop(session);
    let state = rue_compiler::unstable::into_oracle_semantic_state(semantic)
        .expect("one-shot oracle session uniquely owns its frontend artifacts");
    Ok(CompileState {
        interner: Arc::try_unwrap(state.interner)
            .expect("one-shot oracle uniquely owns its semantic symbol interner"),
        functions: state
            .functions
            .into_iter()
            .map(|function| OracleFunction {
                #[cfg(test)]
                source_name: function.source_name,
                cfg: function.cfg,
            })
            .collect(),
        type_pool: state.type_pool,
        strings: state.strings,
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
    /// Everything `@dbg` wrote, in order, newline-terminated per call.
    pub stdout: String,
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
    Free,
    Reallocate,
    AllocateBytes,
    FreeBytes,
    ReallocateBytes,
    ByteRead,
    ByteWrite,
    ByteCopy,
    ByteSet,
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
}

impl fmt::Display for RunSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunSourceError::Compile(errors) => write!(f, "source failed to compile: {errors}"),
            RunSourceError::Unsupported(unsupported) => {
                write!(f, "unsupported by the oracle: {unsupported}")
            }
        }
    }
}

impl std::error::Error for RunSourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunSourceError::Compile(errors) => Some(errors),
            RunSourceError::Unsupported(unsupported) => Some(unsupported),
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
                    stdout: String::new(),
                    stdout_bytes: 0,
                    stdout_cap,
                    stderr_cap,
                    budget,
                    depth: 0,
                    heap: Vec::new(),
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
    Bool(bool),
    Unit,
    /// A struct or array value: its fields (declaration order) or elements
    /// (ascending index). A payload-carrying enum variant is also an
    /// `Aggregate`: element 0 is the discriminant (`Int` tag), followed by the
    /// variant's payload fields in declaration order (RUE-285). A
    /// discriminant-only enum (or C-like enum) stays a bare `Int` tag.
    Aggregate(Vec<Value>),
    /// A raw pointer into the abstract heap (RUE model-gap closure). `None` is
    /// the null pointer (`@int_to_ptr(0)`, a failed `@alloc`/`@realloc`); `Some`
    /// names a live cell inside an [`Allocation`]. Heap allocations
    /// (`@alloc`/`@alloc_bytes`) and address-taken stack places
    /// (`@raw`/`@raw_mut`/`@field_ptr`) share this one provenance-carrying
    /// representation so a pointer read/write, offset, or int round-trip resolves
    /// to the same backing store the source place or allocation owns.
    Ptr(Option<PtrTarget>),
}

/// A provenance-carrying pointer value: the allocation it addresses plus a
/// positional path to the pointee cell.
///
/// Struct fields and array elements share the interpreter's positional
/// `Aggregate` layout, so a single `Vec<usize>` path plus a trailing `index`
/// navigates both. `@ptr_offset` (and byte-address arithmetic) move `index`;
/// `@field_ptr` sets `path`/`index` from the source place's projection.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PtrTarget {
    /// Index of the owning [`Allocation`] in [`Interp::heap`].
    alloc: usize,
    /// Positional navigation into the allocation root reaching the *container*
    /// aggregate whose element `index` is the pointee.
    path: Vec<usize>,
    /// Position of the pointee within its container. Signed so an out-of-range
    /// `@ptr_offset` is detectable rather than silently wrapping.
    index: i128,
}

/// One abstract heap allocation.
///
/// The backing store is always a positional [`Value::Aggregate`] of *cells* —
/// array elements, struct fields, or individual bytes. A promoted stack place
/// wraps a bare scalar in a one-cell aggregate so every pointee is uniformly a
/// cell of some container.
#[derive(Debug, Clone)]
struct Allocation {
    /// Backing cells as a positional aggregate.
    root: Value,
    /// Byte size of one addressable element. Used only to synthesize a stable
    /// `@ptr_to_int` address so an integer round-trips back to the same cell.
    elem_stride: u64,
    /// This allocation backs a promoted whole scalar (`@raw(x)` with no
    /// projection): its single cell is the scalar, and a redirected `Load` of
    /// the owning slot must unwrap `root[0]` rather than return the aggregate.
    wrapped_scalar: bool,
    /// `@free`/`@free_bytes` released this allocation. Pointer access checks the
    /// flag so undefined use-after-free remains a typed oracle gap rather than
    /// receiving a deterministic value that native execution does not promise.
    freed: bool,
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
            Value::Bool(b) => *b as i128,
            // Unreachable for a well-typed program (aggregates/pointers never
            // reach a bare scalar context; a pointer becomes an integer only
            // through `@ptr_to_int`, which computes its address explicitly);
            // defined so callers need not thread an error.
            Value::Unit | Value::Aggregate(_) | Value::Ptr(_) => 0,
        }
    }
    fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
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

fn unsupported_intrinsic_kind(name: &str) -> UnsupportedKind {
    use ExternalDependencyKind as External;
    use SemanticGapKind as Semantic;
    use UnsupportedIntrinsicKind as Intrinsic;

    match name {
        "parse_i32" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ParseI32)),
        "parse_i64" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ParseI64)),
        "parse_u32" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ParseU32)),
        "parse_u64" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ParseU64)),
        // `@ptr_read_unaligned`/`@ptr_write_unaligned` (ADR-0059) differ from the
        // aligned forms only in the alignment *requirement* on the address. The
        // oracle does not model alignment, so they share the aligned forms'
        // semantics and gap classification.
        "ptr_read" | "ptr_read_unaligned" => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerRead))
        }
        "ptr_write" | "ptr_write_unaligned" => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerWrite))
        }
        "ptr_offset" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerOffset)),
        "ptr_to_int" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerToInt)),
        "int_to_ptr" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::IntToPointer)),
        "raw" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::RawAddress)),
        "raw_mut" => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::RawMutableAddress))
        }
        "field_ptr" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::FieldPointer)),
        "alloc" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::Allocate)),
        "free" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::Free)),
        "realloc" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::Reallocate)),
        "alloc_bytes" => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::AllocateBytes))
        }
        "free_bytes" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::FreeBytes)),
        "realloc_bytes" => {
            UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ReallocateBytes))
        }
        "byte_read" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ByteRead)),
        "byte_write" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ByteWrite)),
        "byte_copy" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ByteCopy)),
        "byte_set" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::ByteSet)),
        "read_line" => UnsupportedKind::ExternalDependency(External::StandardInput),
        "random_u32" => UnsupportedKind::ExternalDependency(External::RandomU32),
        "random_u64" => UnsupportedKind::ExternalDependency(External::RandomU64),
        "syscall" => UnsupportedKind::ExternalDependency(External::SystemCall),
        // Process arguments/environment are captured from loader-supplied
        // process state at entry, so their values lie outside deterministic Rue
        // semantics — an external dependency like `@random_*` (RUE-935).
        "arg_count" => UnsupportedKind::ExternalDependency(External::ArgCount),
        "arg_ptr" => UnsupportedKind::ExternalDependency(External::ArgPtr),
        "arg_len" => UnsupportedKind::ExternalDependency(External::ArgLen),
        "env_count" => UnsupportedKind::ExternalDependency(External::EnvCount),
        "env_ptr" => UnsupportedKind::ExternalDependency(External::EnvPtr),
        "env_len" => UnsupportedKind::ExternalDependency(External::EnvLen),
        _ => UnsupportedKind::ContractViolation(ContractViolationKind::UnexpectedIntrinsic),
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
        _ => None,
    }
}

struct Interp<'a> {
    state: &'a CompileState,
    stdout: String,
    /// Raw emitted byte count. `stdout.len()` can be larger because invalid
    /// UTF-8 bytes render as the three-byte replacement character.
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
    /// The abstract heap: every `@alloc`/`@alloc_bytes` block and every promoted
    /// address-taken stack place. Indexed by [`PtrTarget::alloc`]. It lives on
    /// the interpreter (not a frame) so a pointer read across a call boundary
    /// still resolves after the address is passed to a callee.
    heap: Vec<Allocation>,
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
}

enum WritebackPlace<'a> {
    Simple { base: PlaceBase, base_type: Type },
    Stored(&'a Place),
}

impl<'a> Interp<'a> {
    fn string_literal_value(&mut self, text: String, ty: Type) -> Value {
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
            .map(|id| self.state.type_pool.abi_slot_count(Type::new_struct(id)) as usize)
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
            .is_some_and(|struct_id| &*self.state.type_pool.struct_def(struct_id).name == "str")
    }

    fn is_str_like_struct(&self, struct_id: rue_air::StructId) -> bool {
        let name: &str = &self.state.type_pool.struct_def(struct_id).name;
        name == "str" || (name.starts_with("Str(") && name.ends_with(')'))
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
        self.state.type_pool.is_strbuf(struct_id)
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
        let gap = unsupported_intrinsic_kind("byte_read");
        let Some((target, len)) = Self::text_ptr_len(val) else {
            return Err(unsupported(gap, "text value is not a materialized header"));
        };
        if len <= 0 {
            return Ok(Vec::new());
        }
        let Some(target) = target else {
            // A non-null length over a null buffer is a malformed header.
            return Err(unsupported(
                gap,
                "text header has a null buffer with nonzero length",
            ));
        };
        let mut bytes = Vec::with_capacity(len as usize);
        for i in 0..len {
            bytes.push(self.byte_at(&target, i, gap)?);
        }
        Ok(bytes)
    }

    /// Materialize a text value: mint a heap byte allocation holding `bytes` and
    /// build the ABI-shaped header over it. A two-slot `slots` yields a
    /// `str`/`Str(N)` view `{ptr, len}`; a three-slot `slots` yields an owned
    /// `StrBuf` `{core: {buf, cap}, len}` (RUE-1066 nested layout). An empty
    /// value keeps a null buffer (no allocation), matching the source's empty
    /// state. `cap` is the capacity word stored in an owned header.
    fn materialize_text(&mut self, bytes: Vec<u8>, slots: usize, cap: i128) -> Value {
        let len = bytes.len() as i128;
        let ptr = if bytes.is_empty() {
            Value::Ptr(None)
        } else {
            let cells = bytes.into_iter().map(|b| Value::Int(b as i128)).collect();
            let alloc = self.heap_alloc(Value::Aggregate(cells), 1, false);
            Value::Ptr(Some(PtrTarget {
                alloc,
                path: Vec::new(),
                index: 0,
            }))
        };
        if slots >= 3 {
            // Owned `StrBuf`: `{core: {buf, cap}, len}`.
            Value::Aggregate(vec![
                Value::Aggregate(vec![ptr, Value::Int(cap)]),
                Value::Int(len),
            ])
        } else {
            // `str`/`Str(N)` view: `{ptr, len}`.
            Value::Aggregate(vec![ptr, Value::Int(len)])
        }
    }

    /// Allocate `bytes` into the heap and return a raw pointer to cell 0, for
    /// tests that drive the projected-char builtins (which take a `ptr`/`len`
    /// pair) directly.
    #[cfg(test)]
    fn test_alloc_str_ptr(&mut self, bytes: &[u8]) -> Value {
        let cells = bytes.iter().map(|b| Value::Int(*b as i128)).collect();
        let alloc = self.heap_alloc(Value::Aggregate(cells), 1, false);
        Value::Ptr(Some(PtrTarget {
            alloc,
            path: Vec::new(),
            index: 0,
        }))
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

    fn pointer_pointee(&self, ty: Type) -> Option<(Type, bool)> {
        match ty.kind() {
            TypeKind::PtrConst(id) => Some((self.state.type_pool.ptr_const_def(id), false)),
            TypeKind::PtrMut(id) => Some((self.state.type_pool.ptr_mut_def(id), true)),
            _ => None,
        }
    }

    fn option_payload(&self, ty: Type) -> Option<Type> {
        let TypeKind::Enum(enum_id) = ty.kind() else {
            return None;
        };
        let def = self.state.type_pool.enum_def(enum_id);
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

    /// Whether this exact `@int_to_ptr(0) -> ptr const T` is the compiler's
    /// synthesized null pointer for an empty borrowed array slice.
    ///
    /// A zero constant and const-pointer result are not sufficient provenance:
    /// user-authored `@int_to_ptr(0)` could otherwise be mislabeled as this
    /// model gap. Require the intrinsic to feed field 0 of the canonical
    /// two-word `[T] { ptr, len: 0 }` StructInit emitted by slice coercion.
    fn is_empty_slice_pointer(&self, cfg: &Cfg, intrinsic: CfgValue) -> bool {
        if cfg.value_use_count(intrinsic) != 1 {
            return false;
        }
        let pointer_ty = cfg.get_inst(intrinsic).ty;
        cfg.blocks()
            .iter()
            .filter_map(|block| {
                let intrinsic_index = block.insts.iter().position(|value| *value == intrinsic)?;
                Some(block.insts.iter().skip(intrinsic_index + 1).copied())
            })
            .flatten()
            .any(|value| {
                let data = &cfg.get_inst(value).data;
                let CfgInstData::StructInit { struct_id, .. } = data else {
                    return false;
                };
                let def = self.state.type_pool.struct_def(*struct_id);
                let is_slice = def.name.starts_with('[')
                    && def.name.ends_with(']')
                    && parse_array_type_syntax(&def.name).is_none();
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
                fields[0] == intrinsic
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
                let def = self.state.type_pool.struct_def(struct_id);
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
                let (element, _) = self.state.type_pool.array_def(array_id);
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
                self.state.type_pool.abi_slot_count(place.base_type),
                None,
            ),
            PlaceBase::Param(slot) => {
                // A by-reference borrow/inout parameter occupies one physical
                // ABI slot regardless of the logical pointee width. Slices are
                // passed by value and therefore have no by-ref bit here.
                let width = if cfg.is_param_by_ref(slot) {
                    1
                } else {
                    self.state.type_pool.abi_slot_count(place.base_type)
                };
                (slot, cfg.num_params(), width, Some(slot))
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
        intrinsic: CfgValue,
        name: &str,
        args: &[CfgValue],
        result_ty: Type,
    ) -> UnsupportedKind {
        let kind = unsupported_intrinsic_kind(name);
        if kind.model_gap().is_none() {
            return kind;
        }

        let arity_matches = match name {
            "read_line" | "random_u32" | "random_u64" | "arg_count" | "env_count" => {
                args.is_empty()
            }
            "ptr_write"
            | "ptr_write_unaligned"
            | "ptr_offset"
            | "free"
            | "byte_read"
            | "alloc_bytes" => args.len() == 2,
            "realloc" | "byte_write" | "byte_copy" | "byte_set" | "free_bytes" => args.len() == 3,
            "realloc_bytes" => args.len() == 4,
            "syscall" => (1..=7).contains(&args.len()),
            _ => args.len() == 1,
        };
        if !arity_matches {
            return UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity);
        }

        let ty = |index: usize| cfg.get_inst(args[index]).ty;
        let is_text =
            |index: usize| self.is_str_like_type(ty(index)) || self.is_owned_string_type(ty(index));
        let mut validated_kind = kind;
        let signature_matches = match name {
            "parse_i32" => is_text(0) && self.is_option_of(result_ty, Type::I32),
            "parse_i64" => is_text(0) && self.is_option_of(result_ty, Type::I64),
            "parse_u32" => is_text(0) && self.is_option_of(result_ty, Type::U32),
            "parse_u64" => is_text(0) && self.is_option_of(result_ty, Type::U64),
            "ptr_read" | "ptr_read_unaligned" => self
                .pointer_pointee(ty(0))
                .is_some_and(|(pointee, _)| pointee == result_ty),
            "ptr_write" | "ptr_write_unaligned" => {
                self.pointer_pointee(ty(0))
                    .is_some_and(|(pointee, mutable)| {
                        mutable && pointee == ty(1) && result_ty == Type::UNIT
                    })
            }
            "ptr_offset" => {
                self.pointer_pointee(ty(0)).is_some() && ty(1).is_integer() && result_ty == ty(0)
            }
            "ptr_to_int" => self.pointer_pointee(ty(0)).is_some() && result_ty == Type::U64,
            "int_to_ptr" => {
                let pointer = self.pointer_pointee(result_ty);
                if ty(0) == Type::U64 && pointer.is_some_and(|(_, mutable)| mutable) {
                    true
                } else if ty(0) == Type::U64
                    && pointer.is_some_and(|(_, mutable)| !mutable)
                    && matches!(cfg.get_inst(args[0]).data, CfgInstData::Const(0))
                    && self.is_empty_slice_pointer(cfg, intrinsic)
                {
                    validated_kind = UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
                        UnsupportedIntrinsicKind::EmptySlicePointer,
                    ));
                    true
                } else {
                    false
                }
            }
            "raw" => {
                self.intrinsic_arg_is_place(cfg, args[0])
                    && self
                        .pointer_pointee(result_ty)
                        .is_some_and(|(pointee, mutable)| !mutable && pointee == ty(0))
            }
            "raw_mut" => {
                self.intrinsic_arg_is_place(cfg, args[0])
                    && self
                        .pointer_pointee(result_ty)
                        .is_some_and(|(pointee, mutable)| mutable && pointee == ty(0))
            }
            "field_ptr" => {
                self.intrinsic_arg_is_field_place(cfg, args[0])
                    && self
                        .pointer_pointee(result_ty)
                        .is_some_and(|(pointee, mutable)| mutable && pointee == ty(0))
            }
            "alloc" => {
                ty(0) == Type::U64
                    && self
                        .pointer_pointee(result_ty)
                        .is_some_and(|(_, mutable)| mutable)
            }
            "free" => {
                self.pointer_pointee(ty(0))
                    .is_some_and(|(_, mutable)| mutable)
                    && ty(1) == Type::U64
                    && result_ty == Type::UNIT
            }
            "realloc" => {
                self.pointer_pointee(ty(0))
                    .is_some_and(|(_, mutable)| mutable)
                    && ty(1) == Type::U64
                    && ty(2) == Type::U64
                    && result_ty == ty(0)
            }
            "alloc_bytes" => {
                ty(0) == Type::U64
                    && ty(1) == Type::U64
                    && self.pointer_pointee(result_ty) == Some((Type::U8, true))
            }
            "free_bytes" => {
                self.pointer_pointee(ty(0)) == Some((Type::U8, true))
                    && ty(1) == Type::U64
                    && ty(2) == Type::U64
                    && result_ty == Type::UNIT
            }
            "realloc_bytes" => {
                self.pointer_pointee(ty(0)) == Some((Type::U8, true))
                    && ty(1) == Type::U64
                    && ty(2) == Type::U64
                    && ty(3) == Type::U64
                    && result_ty == ty(0)
            }
            "byte_read" => {
                self.pointer_pointee(ty(0))
                    .is_some_and(|(pointee, _)| pointee == Type::U8)
                    && ty(1) == Type::U64
                    && result_ty == Type::U8
            }
            "byte_write" => {
                self.pointer_pointee(ty(0)) == Some((Type::U8, true))
                    && ty(1) == Type::U64
                    && ty(2) == Type::U8
                    && result_ty == Type::UNIT
            }
            "byte_copy" => {
                self.pointer_pointee(ty(0)) == Some((Type::U8, true))
                    && self
                        .pointer_pointee(ty(1))
                        .is_some_and(|(pointee, _)| pointee == Type::U8)
                    && ty(2) == Type::U64
                    && result_ty == Type::UNIT
            }
            "byte_set" => {
                self.pointer_pointee(ty(0)) == Some((Type::U8, true))
                    && ty(1) == Type::U8
                    && ty(2) == Type::U64
                    && result_ty == Type::UNIT
            }
            "read_line" => self
                .option_payload(result_ty)
                .is_some_and(|payload| self.is_owned_string_type(payload)),
            "random_u32" => result_ty == Type::U32,
            "random_u64" => result_ty == Type::U64,
            "arg_count" | "env_count" => result_ty == Type::U64,
            "arg_len" | "env_len" => ty(0) == Type::U64 && result_ty == Type::U64,
            "arg_ptr" | "env_ptr" => {
                ty(0) == Type::U64 && self.pointer_pointee(result_ty) == Some((Type::U8, true))
            }
            "syscall" => {
                args.iter().all(|arg| cfg.get_inst(*arg).ty == Type::U64) && result_ty == Type::I64
            }
            _ => false,
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
        name: &str,
        args: &[CfgValue],
        result_ty: Type,
    ) -> Step<Option<AbortIntrinsic>> {
        let intrinsic = match name {
            "panic" => AbortIntrinsic::Panic,
            "assert" => AbortIntrinsic::Assert,
            _ => return Ok(None),
        };
        let arity_matches = match intrinsic {
            AbortIntrinsic::Panic => args.len() <= 1,
            AbortIntrinsic::Assert => (1..=2).contains(&args.len()),
        };
        if !arity_matches {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity),
                format!("intrinsic @{name} arity"),
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
            AbortIntrinsic::Assert => {
                result_ty == Type::UNIT
                    && ty(0) == Type::BOOL
                    && (args.len() == 1
                        || self.is_str_like_type(ty(1))
                        || self.is_owned_string_type(ty(1)))
            }
        };
        if !signature_matches {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                format!("intrinsic @{name} signature"),
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
                let (condition, message) = match values.as_slice() {
                    [Value::Bool(condition)] => (*condition, None),
                    [Value::Bool(condition), message] if Self::text_ptr_len(message).is_some() => {
                        (*condition, Some(self.text_bytes(message)?))
                    }
                    _ => {
                        return Err(unsupported(
                            UnsupportedKind::ContractViolation(
                                ContractViolationKind::IntrinsicSignature,
                            ),
                            "intrinsic @assert runtime value shape",
                        ));
                    }
                };
                if condition {
                    Ok(Value::Unit)
                } else if let Some(message) = message {
                    // The message-carrying assertion uses the same runtime path
                    // and trap category as `@panic(message)`.
                    self.abort_with_stderr(TrapKind::UserPanic, &[b"panic: ", &message, b"\n"])
                } else {
                    self.abort_with_stderr(TrapKind::AssertionFailure, &[b"assertion failed\n"])
                }
            }
        }
    }

    fn write_dbg(&mut self, val: &Value, ty: Type) -> Step<()> {
        let remaining = self.stdout_cap.saturating_sub(self.stdout_bytes);

        // `@dbg` of a text value prints its byte content (matches
        // __rue_dbg_str), read from the heap through the materialized header
        // (RUE-1010 §6.13); every other value uses the scalar `format_dbg`.
        // Reserve one byte for the trailing newline; the comparison form avoids
        // overflowing while computing `len + 1`, and rejecting an oversized
        // value before decoding also bounds that temporary allocation.
        let (formatted, emitted_len) = if self.is_text_type(ty) {
            let bytes = self.text_bytes(val)?;
            if bytes.len() >= remaining {
                return Err(unsupported(
                    UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                    format!(
                        "stdout byte limit exceeded ({}-byte limit)",
                        self.stdout_cap
                    ),
                ));
            }
            let len = bytes.len();
            (String::from_utf8_lossy(&bytes).into_owned(), len)
        } else {
            let formatted = format_dbg(val, ty)?;
            let len = formatted.len();
            (formatted.into_owned(), len)
        };
        if emitted_len >= remaining {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                format!(
                    "stdout byte limit exceeded ({}-byte limit)",
                    self.stdout_cap
                ),
            ));
        }

        self.stdout.push_str(&formatted);
        self.stdout.push('\n');
        self.stdout_bytes += emitted_len + 1;
        Ok(())
    }

    fn run(mut self) -> Result<Outcome, Unsupported> {
        match self.call("main", &[]) {
            Ok((v, _)) => Ok(Outcome {
                exit_code: (v.as_int() & 0xFF) as i32,
                stdout: self.stdout,
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
                    stdout: self.stdout,
                    stderr: panic.stderr,
                    panic: Some(panic.kind),
                })
            }
            Err(Flow::Unsupported(u)) => Err(u),
        }
    }

    fn interner(&self) -> &ThreadedRodeo {
        &self.state.interner
    }

    /// Locate a callee by the internal symbol its call site names. This is the
    /// interpreter's dispatch, so it must speak the CFG's own symbol space,
    /// not source names.
    fn find_cfg(&self, name: &str) -> Option<&'a Cfg> {
        self.state
            .functions
            .iter()
            .map(|f| &f.cfg)
            .find(|c| c.fn_name() == name)
            .map(|cfg| &**cfg)
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
        rue_air::NativeCallAbi::for_arguments(&self.state.type_pool).arg_slot_width(ty, convention)
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
        let result = self.call_inner(name, args);
        self.depth -= 1;
        result
    }

    fn call_inner(
        &mut self,
        name: &str,
        args: &[(Value, Type, CfgArgMode)],
    ) -> Step<(Value, Vec<Option<Value>>)> {
        let cfg = self.find_cfg(name).ok_or_else(|| {
            unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::MissingFunctionBody),
                format!("call to '{name}'"),
            )
        })?;
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
        let mut frame = Frame {
            params: param_slots,
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
            promoted: HashMap::new(),
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
                    let ret = match value {
                        Some(v) => self.eval(cfg, &mut frame, *v)?,
                        None => Value::Unit,
                    };
                    return Ok((ret, std::mem::take(&mut frame.params)));
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
                self.materialize_text(digits, self.text_value_slot_width(result_ty), cap)
            }
            RuntimeCallKind::ToStringUnsigned => {
                let digits = (args[0].as_int() as u64).to_string().into_bytes();
                let cap = digits.len() as i128;
                self.materialize_text(digits, self.text_value_slot_width(result_ty), cap)
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
        let gap = unsupported_intrinsic_kind("byte_read");
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
                let sd = self.state.type_pool.struct_def(sid);
                // Synthetic builtin types have no observable destructor and no
                // CFG body; skip them.
                if sd.is_builtin {
                    return Ok(());
                }
                if let Some(dtor) = sd.destructor.clone() {
                    self.call(&dtor, &[(v.clone(), ty, CfgArgMode::Normal)])?;
                }
                // A builtin type's destructor is its entire drop glue; a
                // user struct then drops its fields in declaration order.
                if !sd.is_builtin {
                    if let Value::Aggregate(elems) = &v {
                        for (i, field) in sd.fields.iter().enumerate() {
                            if let Some(fv) = elems.get(i).cloned() {
                                self.run_drop(field.ty, fv)?;
                            }
                        }
                    }
                }
            }
            TypeKind::Array(aid) => {
                let (elem_ty, _len) = self.state.type_pool.array_def(aid);
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
                    let def = self.state.type_pool.enum_def(eid);
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
        self.state.type_pool.abi_slot_count(ty) == 0
    }

    /// Materialize the (unique) value of a zero-sized type, preserving the
    /// aggregate shape so field/element projections still line up.
    fn zero_sized_value(&self, ty: Type) -> Value {
        match ty.kind() {
            TypeKind::Struct(sid) => {
                let sd = self.state.type_pool.struct_def(sid);
                Value::Aggregate(
                    sd.fields
                        .iter()
                        .map(|f| self.zero_sized_value(f.ty))
                        .collect(),
                )
            }
            TypeKind::Array(aid) => {
                let (elem_ty, len) = self.state.type_pool.array_def(aid);
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
        let result = match &inst.data {
            CfgInstData::Const(n) => {
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
                    .state
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
                self.string_literal_value(text, ty)
            }
            CfgInstData::Param { index } => {
                if self.is_zero_sized(ty) {
                    // A zero-sized parameter occupies NO slot (abi_slot_count
                    // = 0), but the CFG still emits a Param read for it —
                    // sharing its `index` with the NEXT parameter's slot. Do
                    // not read the slot (that would grab the next parameter's
                    // value); materialize the unique ZST value instead.
                    self.zero_sized_value(ty)
                } else if let Some(&a) =
                    frame.promoted.get(&promotion_key(PlaceBase::Param(*index)))
                {
                    // The parameter's address was taken; read through its heap
                    // allocation so a `@ptr_write` is observed on re-read.
                    self.promoted_slot_value(a)
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

            CfgInstData::Add(a, b) => {
                self.arith(cfg, frame, *a, *b, ty, |x, y| x.checked_add(y))?
            }
            CfgInstData::Sub(a, b) => {
                self.arith(cfg, frame, *a, *b, ty, |x, y| x.checked_sub(y))?
            }
            CfgInstData::Mul(a, b) => {
                self.arith(cfg, frame, *a, *b, ty, |x, y| x.checked_mul(y))?
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
                self.store_local(frame, *slot, val);
                Value::Unit
            }
            CfgInstData::Load { slot } => self.load_local(frame, *slot)?,
            CfgInstData::Store { slot, value } => {
                let val = self.eval(cfg, frame, *value)?;
                self.store_local(frame, *slot, val);
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
                            self.state.type_pool.abi_slot_count(place.base_type),
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
                            self.state.type_pool.abi_slot_count(place.base_type),
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
                Self::set_param(frame, *param_slot, val);
                Value::Unit
            }
            CfgInstData::AccessorCall { .. } => {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::UnsplicedAccessor),
                    "accessor call reached the oracle",
                ));
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
                let missing_call_kind = if !is_string_builtin && runtime.is_some() {
                    let kind = self.classify_unsupported_runtime_call_static(
                        runtime.expect("checked above"),
                        &arg_types,
                        &arg_modes,
                        ty,
                    );
                    if kind.model_gap().is_none() {
                        return Err(unsupported(kind, format!("call to '{fname}'")));
                    }
                    Some(kind)
                } else if !is_string_builtin && self.find_cfg(&fname).is_none() {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::MissingFunctionBody,
                        ),
                        format!("call to '{fname}'"),
                    ));
                } else {
                    None
                };
                if !is_string_builtin && missing_call_kind.is_none() {
                    let callee = self
                        .find_cfg(&fname)
                        .expect("a present user call has a CFG body");
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
                    let v = self.eval(cfg, frame, a.value)?;
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
                } else if missing_call_kind.is_some() {
                    return Err(unsupported(
                        self.classify_unsupported_runtime_call(
                            runtime.expect("typed unsupported runtime call"),
                            &argvals,
                            &arg_types,
                            &arg_modes,
                            ty,
                        ),
                        format!("call to '{fname}'"),
                    ));
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

            CfgInstData::Intrinsic { name, .. } => {
                let iname = self.interner().resolve(name).to_string();
                let args = cfg.get_intrinsic_args(&inst.data).to_vec();
                if let Some(intrinsic) = self.preflight_abort_intrinsic(cfg, &iname, &args, ty)? {
                    self.eval_abort_intrinsic(cfg, frame, intrinsic, &args)?
                } else if iname != "dbg" {
                    // Classify first: the same static arity/signature validation
                    // that gates a model-gap registration also gates execution, so
                    // a malformed intrinsic still reports its contract violation
                    // rather than being run. A validated, now-modeled heap/pointer
                    // intrinsic executes; every other typed gap is reported.
                    let kind = self.classify_unsupported_intrinsic(cfg, v, &iname, &args, ty);
                    match kind {
                        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(ik))
                            if modeled_pointer_intrinsic(ik) =>
                        {
                            match self.eval_pointer_intrinsic(cfg, frame, &iname, &args, ty)? {
                                Some(value) => value,
                                None => {
                                    return Err(unsupported(kind, format!("intrinsic @{iname}")));
                                }
                            }
                        }
                        _ => return Err(unsupported(kind, format!("intrinsic @{iname}"))),
                    }
                } else {
                    let [arg] = args.as_slice() else {
                        return Err(unsupported(
                            UnsupportedKind::ContractViolation(ContractViolationKind::DebugArity),
                            "@dbg arity",
                        ));
                    };
                    let arg = *arg;
                    let arg_ty = cfg.get_inst(arg).ty;
                    let val = self.eval(cfg, frame, arg)?;
                    self.write_dbg(&val, arg_ty)?;
                    Value::Unit
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
        op: impl Fn(i128, i128) -> Option<i128>,
    ) -> Step<Value> {
        let x = self.eval(cfg, frame, a)?.as_int();
        let y = self.eval(cfg, frame, b)?.as_int();
        range_check(op(x, y), ty)
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
        match ty.kind() {
            TypeKind::Struct(sid) => {
                let (Value::Aggregate(xs), Value::Aggregate(ys)) = (x, y) else {
                    return Ok(x == y);
                };
                let def = self.state.type_pool.struct_def(sid);
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
                let (elem_ty, _len) = self.state.type_pool.array_def(aid);
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
                        let payload_tys = self.state.type_pool.enum_def(eid).variant_payload(tag);
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
        // aggregates (RUE-285). Only `==` / `!=` reach a non-text aggregate here
        // (ordering `< > <= >=` is a type error on those), so we report `Equal`
        // iff structurally equal and an arbitrary non-`Equal` ordering
        // otherwise — enough for `pick` to decide `==`/`!=`. Everything else
        // compares by integer value.
        let ty = cfg.get_inst(a).ty;
        let ord = if self.is_text_type(ty) {
            let bx = self.text_bytes(&x)?;
            let by = self.text_bytes(&y)?;
            bx.cmp(&by)
        } else {
            match (&x, &y) {
                (Value::Aggregate(_), _) | (_, Value::Aggregate(_)) => {
                    // Only `==`/`!=` reach an aggregate; compare with text
                    // awareness so a text field/element/payload is judged by its
                    // byte content, not by the identity of its heap buffer
                    // pointer (RUE-1010 §6.13).
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
                    path.push((i as usize, p));
                }
            }
        }
        Ok((place.base, path))
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
        }
    }

    fn place_read(&mut self, cfg: &'a Cfg, frame: &mut Frame, place: &Place) -> Step<Value> {
        let (base, path) = self.resolve_path(cfg, frame, place)?;
        let mut cur = self.base_value_of(frame, base)?;
        for (idx, _projection) in path {
            cur = match cur {
                Value::Aggregate(mut v) if idx < v.len() => v.swap_remove(idx),
                Value::Aggregate(_) => {
                    return Err(Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)));
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
        // Select the storage root. A promoted (address-taken) base writes through
        // its heap allocation so the mutation is observed by both a later direct
        // read and any live pointer. Otherwise write frame storage directly;
        // writing through an `inout` parameter place targets the param slot
        // (copied back to the caller on return).
        let root: &mut Value = if let Some(&a) = frame.promoted.get(&promotion_key(base)) {
            let alloc = &mut self.heap[a];
            if alloc.wrapped_scalar {
                match &mut alloc.root {
                    Value::Aggregate(cells) if !cells.is_empty() => &mut cells[0],
                    other => other,
                }
            } else {
                &mut alloc.root
            }
        } else {
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
            return Ok(self.promoted_slot_value(a));
        }
        Self::base_value(frame, base)
    }

    /// The logical value a promoted slot holds: the wrapped scalar unwrapped, or
    /// the aggregate root as-is.
    fn promoted_slot_value(&self, alloc: usize) -> Value {
        let allocation = &self.heap[alloc];
        if allocation.wrapped_scalar {
            match &allocation.root {
                Value::Aggregate(cells) if !cells.is_empty() => cells[0].clone(),
                other => other.clone(),
            }
        } else {
            allocation.root.clone()
        }
    }

    /// Write the logical value of a promoted slot back into its allocation,
    /// preserving the scalar wrap.
    fn set_promoted_slot(&mut self, alloc: usize, val: Value) {
        if self.heap[alloc].wrapped_scalar {
            self.heap[alloc].root = Value::Aggregate(vec![val]);
        } else {
            self.heap[alloc].root = val;
        }
    }

    /// Read a local slot, honoring promotion.
    fn load_local(&self, frame: &Frame, slot: u32) -> Step<Value> {
        if let Some(&a) = frame.promoted.get(&promotion_key(PlaceBase::Local(slot))) {
            return Ok(self.promoted_slot_value(a));
        }
        Self::get_local(frame, slot)
    }

    /// Write a local slot, honoring promotion.
    fn store_local(&mut self, frame: &mut Frame, slot: u32, val: Value) {
        if let Some(&a) = frame.promoted.get(&promotion_key(PlaceBase::Local(slot))) {
            self.set_promoted_slot(a, val);
        } else {
            Self::set_local(frame, slot, val);
        }
    }

    /// A deterministic image for storage that Rue specifies as uninitialized.
    /// Valid modeled programs initialize cells before observing them.
    fn zeroed_value(&self, ty: Type) -> Value {
        match ty.kind() {
            TypeKind::Bool => Value::Bool(false),
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64 => Value::Int(0),
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => Value::Ptr(None),
            TypeKind::Struct(sid) => {
                if self.is_str_like_struct(sid) || self.is_owned_string_struct(sid) {
                    // Empty text header: a null buffer with zero length and
                    // capacity (RUE-1010 §6.13). A str/Str(N) view is
                    // `{ptr, len}`; an owned StrBuf is `{core: {buf, cap}, len}`.
                    if self.text_value_slot_width(ty) >= 3 {
                        Value::Aggregate(vec![
                            Value::Aggregate(vec![Value::Ptr(None), Value::Int(0)]),
                            Value::Int(0),
                        ])
                    } else {
                        Value::Aggregate(vec![Value::Ptr(None), Value::Int(0)])
                    }
                } else {
                    let sd = self.state.type_pool.struct_def(sid);
                    Value::Aggregate(sd.fields.iter().map(|f| self.zeroed_value(f.ty)).collect())
                }
            }
            TypeKind::Array(aid) => {
                let (elem, len) = self.state.type_pool.array_def(aid);
                Value::Aggregate((0..len).map(|_| self.zeroed_value(elem)).collect())
            }
            // A zeroed enum is discriminant 0; a bare tag under the interpreter's
            // model (payload cells are only reached after a variant write).
            TypeKind::Enum(_) => Value::Int(0),
            _ => Value::Unit,
        }
    }

    /// Byte size of one value of `ty` from the compact-layout authority.
    fn type_byte_size(&self, ty: Type) -> u64 {
        self.state.type_pool.layout(ty).size
    }

    /// Push a new allocation, returning its index.
    fn heap_alloc(&mut self, root: Value, elem_stride: u64, wrapped_scalar: bool) -> usize {
        self.heap.push(Allocation {
            root,
            elem_stride: elem_stride.max(1),
            wrapped_scalar,
            freed: false,
        });
        self.heap.len() - 1
    }

    /// Synthesize a stable non-null address for `target`, used by `@ptr_to_int`.
    fn ptr_address(&self, target: &PtrTarget) -> u128 {
        let stride = self.heap[target.alloc].elem_stride as u128;
        HEAP_BASE + (target.alloc as u128) * HEAP_SEG + target.index.max(0) as u128 * stride
    }

    /// Decode a synthetic address back to a pointer target, honoring byte-offset
    /// arithmetic performed on a `@ptr_to_int` value. Returns `None` for an
    /// address that names no known allocation (an arbitrary integer, which stays
    /// an unsupported `@int_to_ptr`).
    fn address_target(&self, addr: u128, pointee: Type) -> Option<PtrTarget> {
        if addr < HEAP_BASE {
            return None;
        }
        let n = addr - HEAP_BASE;
        let alloc = (n / HEAP_SEG) as usize;
        if alloc >= self.heap.len() {
            return None;
        }
        let byte_off = n % HEAP_SEG;
        let stride = self.type_byte_size(pointee).max(1) as u128;
        Some(PtrTarget {
            alloc,
            path: Vec::new(),
            index: (byte_off / stride) as i128,
        })
    }

    fn nav<'v>(root: &'v Value, path: &[usize]) -> Option<&'v Value> {
        let mut cur = root;
        for &p in path {
            match cur {
                Value::Aggregate(cells) => cur = cells.get(p)?,
                _ => return None,
            }
        }
        Some(cur)
    }

    fn nav_mut<'v>(root: &'v mut Value, path: &[usize]) -> Option<&'v mut Value> {
        let mut cur = root;
        for &p in path {
            match cur {
                Value::Aggregate(cells) => cur = cells.get_mut(p)?,
                _ => return None,
            }
        }
        Some(cur)
    }

    /// Read the pointee cell of `target`. `gap` is the model-gap kind reported if
    /// the access is out of bounds or malformed (unmodelable UB, kept as a typed
    /// gap rather than a false agreement).
    fn ptr_cell_read(&self, target: &PtrTarget, gap: UnsupportedKind) -> Step<Value> {
        let alloc = self
            .heap
            .get(target.alloc)
            .ok_or_else(|| unsupported(gap, "pointer to unknown allocation"))?;
        if alloc.freed {
            return Err(unsupported(gap, "pointer read after free"));
        }
        let container = Self::nav(&alloc.root, &target.path)
            .ok_or_else(|| unsupported(gap, "pointer path escapes allocation"))?;
        match container {
            Value::Aggregate(cells) => {
                if target.index < 0 || target.index as usize >= cells.len() {
                    return Err(unsupported(gap, "pointer read out of bounds"));
                }
                Ok(cells[target.index as usize].clone())
            }
            _ => Err(unsupported(gap, "pointer to non-aggregate cell")),
        }
    }

    /// Write the pointee cell of `target`.
    fn ptr_cell_write(&mut self, target: &PtrTarget, val: Value, gap: UnsupportedKind) -> Step<()> {
        let alloc = self
            .heap
            .get_mut(target.alloc)
            .ok_or_else(|| unsupported(gap, "pointer to unknown allocation"))?;
        if alloc.freed {
            return Err(unsupported(gap, "pointer write after free"));
        }
        let container = Self::nav_mut(&mut alloc.root, &target.path)
            .ok_or_else(|| unsupported(gap, "pointer path escapes allocation"))?;
        match container {
            Value::Aggregate(cells) => {
                if target.index < 0 || target.index as usize >= cells.len() {
                    return Err(unsupported(gap, "pointer write out of bounds"));
                }
                cells[target.index as usize] = val;
                Ok(())
            }
            _ => Err(unsupported(gap, "pointer to non-aggregate cell")),
        }
    }

    /// Read one byte from a byte-store allocation at `target.index + offset`.
    fn byte_at(&self, target: &PtrTarget, offset: i128, gap: UnsupportedKind) -> Step<u8> {
        let at = PtrTarget {
            alloc: target.alloc,
            path: target.path.clone(),
            index: target.index + offset,
        };
        match self.ptr_cell_read(&at, gap)? {
            Value::Int(n) => Ok((n & 0xFF) as u8),
            _ => Err(unsupported(gap, "byte access of non-byte allocation")),
        }
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
        let key = promotion_key(base);
        let alloc = if let Some(&a) = frame.promoted.get(&key) {
            a
        } else {
            let value = self.base_value_of(frame, base)?;
            // A whole-local address (no projection) wraps its value in a single
            // cell so every pointee is uniformly a cell of some container; a
            // projected address keeps the aggregate root and navigates into it.
            let (root, wrapped) = if path.is_empty() {
                (Value::Aggregate(vec![value]), true)
            } else {
                (value, false)
            };
            let stride = self.type_byte_size(pointee);
            let a = self.heap_alloc(root, stride, wrapped);
            frame.promoted.insert(key, a);
            a
        };
        match path.last().copied() {
            Some((last, _)) => Ok(PtrTarget {
                alloc,
                path: path[..path.len() - 1].iter().map(|(i, _)| *i).collect(),
                index: last as i128,
            }),
            None => Ok(PtrTarget {
                alloc,
                path: Vec::new(),
                index: 0,
            }),
        }
    }

    /// Execute a heap/pointer intrinsic whose signature already validated as a
    /// modeled model-gap kind. Returns `Ok(None)` to fall through to the existing
    /// typed-gap path (e.g. the empty-slice `@int_to_ptr(0)` special case, or an
    /// arbitrary-integer `@int_to_ptr`).
    fn eval_pointer_intrinsic(
        &mut self,
        cfg: &'a Cfg,
        frame: &mut Frame,
        name: &str,
        args: &[CfgValue],
        result_ty: Type,
    ) -> Step<Option<Value>> {
        let gap = unsupported_intrinsic_kind(name);
        // Evaluate operands eagerly and left-to-right, matching native lowering,
        // except `@raw`/`@raw_mut`/`@field_ptr` whose operand is an lvalue place.
        match name {
            "raw" | "raw_mut" | "field_ptr" => {
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
            "alloc" => {
                let count = self.eval(cfg, frame, args[0])?.as_int();
                let (pointee, _) = self
                    .pointer_pointee(result_ty)
                    .expect("validated @alloc result is a pointer");
                Ok(Some(self.do_alloc(count, pointee)?))
            }
            "alloc_bytes" => {
                let size = self.eval(cfg, frame, args[0])?.as_int();
                let _align = self.eval(cfg, frame, args[1])?.as_int();
                Ok(Some(self.do_alloc_bytes(size)?))
            }
            "free" | "free_bytes" => {
                // Evaluate every operand before releasing the allocation.
                let p = self.eval(cfg, frame, args[0])?;
                for a in &args[1..] {
                    self.eval(cfg, frame, *a)?;
                }
                if let Value::Ptr(Some(t)) = p {
                    if let Some(alloc) = self.heap.get_mut(t.alloc) {
                        if alloc.freed {
                            return Err(unsupported(gap, "double free"));
                        }
                        alloc.freed = true;
                    }
                }
                Ok(Some(Value::Unit))
            }
            "realloc" => {
                let p = self.eval(cfg, frame, args[0])?;
                let old_count = self.eval(cfg, frame, args[1])?.as_int();
                let new_count = self.eval(cfg, frame, args[2])?.as_int();
                let (pointee, _) = self
                    .pointer_pointee(result_ty)
                    .expect("validated @realloc result is a pointer");
                let stride = self.type_byte_size(pointee);
                Ok(Some(self.do_realloc(
                    p, old_count, new_count, stride, pointee, false,
                )?))
            }
            "realloc_bytes" => {
                let p = self.eval(cfg, frame, args[0])?;
                let old_size = self.eval(cfg, frame, args[1])?.as_int();
                let _align = self.eval(cfg, frame, args[2])?.as_int();
                let new_size = self.eval(cfg, frame, args[3])?.as_int();
                Ok(Some(self.do_realloc(
                    p,
                    old_size,
                    new_size,
                    1,
                    Type::U8,
                    true,
                )?))
            }
            "ptr_read" | "ptr_read_unaligned" => {
                let p = self.eval(cfg, frame, args[0])?;
                let target = self.expect_ptr(p, gap)?;
                Ok(Some(self.ptr_cell_read(&target, gap)?))
            }
            "ptr_write" | "ptr_write_unaligned" => {
                let p = self.eval(cfg, frame, args[0])?;
                let val = self.eval(cfg, frame, args[1])?;
                let target = self.expect_ptr(p, gap)?;
                self.ptr_cell_write(&target, val, gap)?;
                Ok(Some(Value::Unit))
            }
            "ptr_offset" => {
                let p = self.eval(cfg, frame, args[0])?;
                let by = self.eval(cfg, frame, args[1])?.as_int();
                match p {
                    Value::Ptr(Some(mut t)) => {
                        t.index += by;
                        Ok(Some(Value::Ptr(Some(t))))
                    }
                    Value::Ptr(None) => Ok(Some(Value::Ptr(None))),
                    _ => Err(unsupported(gap, "@ptr_offset of non-pointer")),
                }
            }
            "ptr_to_int" => {
                let p = self.eval(cfg, frame, args[0])?;
                let addr = match p {
                    Value::Ptr(Some(t)) => self.ptr_address(&t),
                    Value::Ptr(None) => 0,
                    _ => return Err(unsupported(gap, "@ptr_to_int of non-pointer")),
                };
                Ok(Some(Value::Int((addr as u64) as i128)))
            }
            "int_to_ptr" => {
                let addr = self.eval(cfg, frame, args[0])?.as_int() as u64 as u128;
                if addr == 0 {
                    return Ok(Some(Value::Ptr(None)));
                }
                let (pointee, _) = self
                    .pointer_pointee(result_ty)
                    .expect("validated @int_to_ptr result is a pointer");
                match self.address_target(addr, pointee) {
                    // A pointer-derived integer round-trips to its allocation.
                    Some(target) => Ok(Some(Value::Ptr(Some(target)))),
                    // An arbitrary integer names no allocation: unmodelable,
                    // stays the typed `IntToPointer` model gap.
                    None => Ok(None),
                }
            }
            "byte_read" => {
                let p = self.eval(cfg, frame, args[0])?;
                let off = self.eval(cfg, frame, args[1])?.as_int();
                let target = self.expect_ptr(p, gap)?;
                Ok(Some(Value::Int(self.byte_at(&target, off, gap)? as i128)))
            }
            "byte_write" => {
                let p = self.eval(cfg, frame, args[0])?;
                let off = self.eval(cfg, frame, args[1])?.as_int();
                let val = self.eval(cfg, frame, args[2])?.as_int();
                let target = self.expect_ptr(p, gap)?;
                let at = PtrTarget {
                    alloc: target.alloc,
                    path: target.path.clone(),
                    index: target.index + off,
                };
                self.ptr_cell_write(&at, Value::Int(val & 0xFF), gap)?;
                Ok(Some(Value::Unit))
            }
            "byte_copy" => {
                let dst = self.eval(cfg, frame, args[0])?;
                let src = self.eval(cfg, frame, args[1])?;
                let count = self.eval(cfg, frame, args[2])?.as_int();
                let dst = self.expect_ptr(dst, gap)?;
                let src = self.expect_ptr(src, gap)?;
                // Read all source bytes first so an overlapping copy is well
                // defined (the runtime uses copy_nonoverlapping; corpus copies
                // never overlap, but buffering keeps a same-allocation copy sane).
                let mut buf = Vec::with_capacity(count.max(0) as usize);
                for k in 0..count {
                    buf.push(self.byte_at(&src, k, gap)?);
                }
                for (k, byte) in buf.into_iter().enumerate() {
                    let at = PtrTarget {
                        alloc: dst.alloc,
                        path: dst.path.clone(),
                        index: dst.index + k as i128,
                    };
                    self.ptr_cell_write(&at, Value::Int(byte as i128), gap)?;
                }
                Ok(Some(Value::Unit))
            }
            "byte_set" => {
                let p = self.eval(cfg, frame, args[0])?;
                let val = self.eval(cfg, frame, args[1])?.as_int();
                let count = self.eval(cfg, frame, args[2])?.as_int();
                let target = self.expect_ptr(p, gap)?;
                for k in 0..count {
                    let at = PtrTarget {
                        alloc: target.alloc,
                        path: target.path.clone(),
                        index: target.index + k,
                    };
                    self.ptr_cell_write(&at, Value::Int(val & 0xFF), gap)?;
                }
                Ok(Some(Value::Unit))
            }
            _ => Ok(None),
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

    /// `@alloc(count)`: `count` zeroed cells of the pointee type, or null on
    /// allocator failure (zero size or a byte size beyond [`MAX_ALLOC_BYTES`]).
    fn do_alloc(&mut self, count: i128, pointee: Type) -> Step<Value> {
        let stride = self.type_byte_size(pointee);
        let Some(_bytes) = self.alloc_byte_size(count, stride)? else {
            return Ok(Value::Ptr(None));
        };
        self.materialize_cells_guard(count)?;
        let cells = (0..count).map(|_| self.zeroed_value(pointee)).collect();
        let alloc = self.heap_alloc(Value::Aggregate(cells), stride, false);
        Ok(Value::Ptr(Some(PtrTarget {
            alloc,
            path: Vec::new(),
            index: 0,
        })))
    }

    /// `@alloc_bytes(size, align)`: `size` zeroed byte cells, or null on failure.
    fn do_alloc_bytes(&mut self, size: i128) -> Step<Value> {
        let Some(_bytes) = self.alloc_byte_size(size, 1)? else {
            return Ok(Value::Ptr(None));
        };
        self.materialize_cells_guard(size)?;
        let cells = (0..size).map(|_| Value::Int(0)).collect();
        let alloc = self.heap_alloc(Value::Aggregate(cells), 1, false);
        Ok(Value::Ptr(Some(PtrTarget {
            alloc,
            path: Vec::new(),
            index: 0,
        })))
    }

    /// `@realloc`/`@realloc_bytes`: grow/shrink an allocation, preserving the
    /// first `min(old, new)` cells and deterministically filling growth. This
    /// model keeps shrink-in-place and move-on-grow behavior; native pointer
    /// identity is deliberately unspecified. Returns null on `new == 0` or allocator failure, leaving the
    /// original allocation valid (spec 8.6:3), and traps on a `new * stride`
    /// overflow like the compiled size arithmetic (spec 8.6:1).
    fn do_realloc(
        &mut self,
        p: Value,
        old_count: i128,
        new_count: i128,
        stride: u64,
        pointee: Type,
        bytes: bool,
    ) -> Step<Value> {
        let target = match p {
            Value::Ptr(Some(t)) => t,
            // realloc(null, .., new) behaves like a fresh allocation.
            Value::Ptr(None) => {
                return if bytes {
                    self.do_alloc_bytes(new_count)
                } else {
                    self.do_alloc(new_count, pointee)
                };
            }
            _ => return Ok(Value::Ptr(None)),
        };
        if self
            .heap
            .get(target.alloc)
            .is_some_and(|allocation| allocation.freed)
        {
            let name = if bytes { "realloc_bytes" } else { "realloc" };
            return Err(unsupported(
                unsupported_intrinsic_kind(name),
                "realloc after free",
            ));
        }
        if new_count == 0 {
            if let Some(alloc) = self.heap.get_mut(target.alloc) {
                alloc.freed = true;
            }
            return Ok(Value::Ptr(None));
        }
        let Some(_new_bytes) = self.alloc_byte_size(new_count, stride)? else {
            return Ok(Value::Ptr(None));
        };
        if new_count <= old_count {
            // No move needed; the original block already holds the data.
            return Ok(Value::Ptr(Some(PtrTarget {
                alloc: target.alloc,
                path: Vec::new(),
                index: 0,
            })));
        }
        self.materialize_cells_guard(new_count)?;
        let old_cells = match self.heap.get(target.alloc).map(|a| &a.root) {
            Some(Value::Aggregate(cells)) => cells.clone(),
            _ => Vec::new(),
        };
        let mut cells: Vec<Value> = Vec::with_capacity(new_count as usize);
        for i in 0..new_count {
            if (i as usize) < old_cells.len() && i < old_count {
                cells.push(old_cells[i as usize].clone());
            } else if bytes {
                cells.push(Value::Int(0));
            } else {
                cells.push(self.zeroed_value(pointee));
            }
        }
        let alloc = self.heap_alloc(Value::Aggregate(cells), stride, false);
        self.heap[target.alloc].freed = true;
        Ok(Value::Ptr(Some(PtrTarget {
            alloc,
            path: Vec::new(),
            index: 0,
        })))
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

    /// Bound the number of cells the interpreter will materialize for one
    /// allocation. A satisfiable-but-enormous request (only reachable from a
    /// synthetic fuzz program, never the differential corpus) resolves to a typed
    /// resource limit rather than exhausting harness memory building cells.
    fn materialize_cells_guard(&self, count: i128) -> Step<()> {
        const MAX_MATERIALIZED_CELLS: i128 = 1 << 24;
        if count > MAX_MATERIALIZED_CELLS {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
                "allocation cell count exceeds the interpreter materialization bound",
            ));
        }
        Ok(())
    }
}

/// Whether the interpreter now executes this pointer/heap intrinsic instead of
/// reporting it as a model gap. Excludes the still-unmodeled text parses and the
/// empty-slice `@int_to_ptr(0)` special case (kept as its own typed gap).
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
            | I::Free
            | I::Reallocate
            | I::AllocateBytes
            | I::FreeBytes
            | I::ReallocateBytes
            | I::ByteRead
            | I::ByteWrite
            | I::ByteCopy
            | I::ByteSet
    )
}

/// Stable map key for a promoted place base within a frame.
fn promotion_key(base: PlaceBase) -> u64 {
    match base {
        PlaceBase::Local(slot) => (slot as u64) << 1,
        PlaceBase::Param(slot) => ((slot as u64) << 1) | 1,
        PlaceBase::Accessor(value) => ((value.as_u32() as u64) << 2) | 3,
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
