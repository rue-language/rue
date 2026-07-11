//! # rue-oracle — the executable reference semantics
//!
//! A tree-walking interpreter over the compiler's **CFG** (the typed
//! control-flow IR, produced by [`rue_compiler::compile_to_cfg`], *before* the
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
//! just final values; and `String` — content modeled directly, covering `new`,
//! `push`, `push_str`, `len`, `is_empty`, `clear`, `clone`, `@to_string`, byte
//! indexing `s[i]` (`__rue_String_byte_at`), and the `.chars()` /
//! `.chars_lossy()` scalar view (`__rue_String_char_scalar`/`_char_next`). String
//! content is modeled as raw bytes, matching the runtime's byte-string buffer:
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
use rue_air::{Type, TypeKind, parse_array_type_syntax};
use rue_cfg::{Cfg, CfgArgMode, CfgInstData, CfgValue, Place, PlaceBase, Projection, Terminator};
use rue_compiler::{
    CompileErrors, CompileState, PreviewFeatures, compile_to_cfg,
    compile_to_cfg_with_preview_features,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

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
}

/// A compiler-emitted runtime call whose deterministic semantics are not
/// modeled yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnsupportedRuntimeCallKind {
    StringConcat,
    StringContains,
    StringStartsWith,
    StringSubstring,
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
    InoutParameterForwarding,
}

/// A dependency whose value comes from outside deterministic Rue semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExternalDependencyKind {
    StandardInput,
    RandomU32,
    RandomU64,
    SystemCall,
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
    UnexpectedLegacyMutationInstruction,
    InoutArgumentNotLvalue,
    NonIntegerOperationType,
    UnsupportedDebugType,
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
    let state = compile_to_cfg(source).map_err(RunSourceError::Compile)?;
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
    let state = compile_to_cfg_with_preview_features(source, preview_features)
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
    /// String-like data, modeled as raw byte content plus ABI slot width.
    ///
    /// `String`/`StrBuf` occupies three slots (`ptr`, `len`, `cap`), while
    /// preview `str`/`Str(N)` occupies two (`ptr`, `len`). Keeping the width on
    /// the value prevents a `str` parameter from shifting later parameters as if
    /// it were a growable string.
    Str {
        bytes: Vec<u8>,
        slots: usize,
    },
}

impl Value {
    fn string(text: impl Into<String>) -> Self {
        Self::string_bytes(text.into().into_bytes())
    }

    fn string_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Str {
            bytes: bytes.into(),
            slots: 3,
        }
    }

    fn str_view(text: impl Into<String>) -> Self {
        Self::str_view_bytes(text.into().into_bytes())
    }

    fn str_view_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Str {
            bytes: bytes.into(),
            slots: 2,
        }
    }

    fn as_int(&self) -> i128 {
        match self {
            Value::Int(n) => *n,
            Value::Bool(b) => *b as i128,
            // Unreachable for a well-typed program (aggregates/strings never
            // reach a scalar context); defined so callers need not thread an error.
            Value::Unit | Value::Aggregate(_) | Value::Str { .. } => 0,
        }
    }
    fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Unit | Value::Aggregate(_) | Value::Str { .. } => false,
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

// `@panic` aborts dynamically but remains an ordinary unit expression in Rue's
// static contract.
const PANIC_CFG_RESULT_TYPE: Type = Type::UNIT;

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
        "ptr_read" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerRead)),
        "ptr_write" => UnsupportedKind::SemanticGap(Semantic::Intrinsic(Intrinsic::PointerWrite)),
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
        "read_line" => UnsupportedKind::ExternalDependency(External::StandardInput),
        "random_u32" => UnsupportedKind::ExternalDependency(External::RandomU32),
        "random_u64" => UnsupportedKind::ExternalDependency(External::RandomU64),
        "syscall" => UnsupportedKind::ExternalDependency(External::SystemCall),
        _ => UnsupportedKind::ContractViolation(ContractViolationKind::UnexpectedIntrinsic),
    }
}

fn unsupported_runtime_call_kind(name: &str) -> Option<UnsupportedRuntimeCallKind> {
    match name {
        "__rue_String_concat" => Some(UnsupportedRuntimeCallKind::StringConcat),
        "__rue_String_contains" => Some(UnsupportedRuntimeCallKind::StringContains),
        "__rue_String_starts_with" => Some(UnsupportedRuntimeCallKind::StringStartsWith),
        "__rue_String_substring" => Some(UnsupportedRuntimeCallKind::StringSubstring),
        "__rue_print" => Some(UnsupportedRuntimeCallKind::Print),
        "__rue_println" => Some(UnsupportedRuntimeCallKind::Println),
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
}

/// Per-call activation record. `cache` preserves values produced along the
/// executed CFG path: Rue SSA values may be used in dominated blocks without
/// being repeated as block arguments. On block entry, that block's own values
/// are invalidated so loop re-entry recomputes the current iteration while
/// values from executed dominators retain their original evaluation snapshot.
struct Frame {
    /// Parameters laid out by ABI **slot**, not by logical argument: an
    /// aggregate argument spans one slot per flattened scalar leaf (a `[i32; 3]`
    /// occupies three slots), so a later parameter's `Param{index}` is offset by
    /// the widths of the aggregates before it. The whole aggregate value is kept
    /// at its base slot; the slots it "occupies" after that are `None` (they are
    /// only ever reached through the base via a projection, never directly).
    params: Vec<Option<Value>>,
    locals: Vec<Option<Value>>,
    cache: HashMap<u32, Value>,
}

/// Number of ABI parameter slots a value occupies: one per flattened scalar
/// leaf. Matches the CFG's slot numbering for `Param{index}`, including
/// zero-sized values: `abi_slot_count` (rue-air typeck) gives unit and empty
/// structs ZERO slots, so a parameter after a ZST is NOT shifted up.
fn slot_width(v: &Value) -> usize {
    match v {
        Value::Aggregate(elems) => elems.iter().map(slot_width).sum(),
        Value::Str { slots, .. } => *slots,
        Value::Unit => 0,
        _ => 1,
    }
}

impl<'a> Interp<'a> {
    fn string_literal_value(&self, text: String, ty: Type) -> Value {
        if self.is_str_like_type(ty) {
            Value::str_view(text)
        } else {
            Value::string(text)
        }
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
            .is_some_and(|struct_id| self.state.type_pool.struct_def(struct_id).name == "str")
    }

    fn is_str_like_struct(&self, struct_id: rue_air::StructId) -> bool {
        let name = &self.state.type_pool.struct_def(struct_id).name;
        name == "str" || (name.starts_with("Str(") && name.ends_with(')'))
    }

    fn text_struct_slots(&self, struct_id: rue_air::StructId) -> Option<usize> {
        if self.is_str_like_struct(struct_id) {
            Some(2)
        } else if self.is_owned_string_struct(struct_id) {
            Some(3)
        } else {
            None
        }
    }

    /// Return the ABI width certified by a real text-field projection.
    ///
    /// The oracle stores text as an opaque byte vector instead of exposing its
    /// compiler ABI fields. Only a field projection whose struct metadata and
    /// field index match that ABI may therefore become a registrable model
    /// gap. In particular, an arbitrary `Index` projection or an out-of-range
    /// field must remain a compiler/oracle contract violation even if a broken
    /// frame happens to contain a `Value::Str` at runtime.
    fn text_projection_slots(&self, projection: Projection) -> Option<usize> {
        let Projection::Field {
            struct_id,
            field_index,
        } = projection
        else {
            return None;
        };
        let slots = self.text_struct_slots(struct_id)?;
        let def = self.state.type_pool.struct_def(struct_id);
        (def.field_count() == slots && (field_index as usize) < 2).then_some(slots)
    }

    fn is_matching_text_projection(&self, projection: Projection, actual_slots: usize) -> bool {
        self.text_projection_slots(projection) == Some(actual_slots)
    }

    fn is_owned_string_type(&self, ty: Type) -> bool {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            self.is_owned_string_struct(struct_id)
        } else {
            false
        }
    }

    fn is_owned_string_struct(&self, struct_id: rue_air::StructId) -> bool {
        matches!(
            self.state.type_pool.struct_def(struct_id).name.as_str(),
            "String" | "StrBuf"
        )
    }

    fn classify_unsupported_runtime_call_static(
        &self,
        name: &str,
        arg_types: &[Type],
        arg_modes: &[CfgArgMode],
        result_ty: Type,
    ) -> UnsupportedKind {
        let Some(runtime_call) = unsupported_runtime_call_kind(name) else {
            return UnsupportedKind::ContractViolation(ContractViolationKind::MissingFunctionBody);
        };
        let expected_arity = match runtime_call {
            UnsupportedRuntimeCallKind::StringConcat
            | UnsupportedRuntimeCallKind::StringContains
            | UnsupportedRuntimeCallKind::StringStartsWith => 2,
            UnsupportedRuntimeCallKind::StringSubstring => 3,
            UnsupportedRuntimeCallKind::Print | UnsupportedRuntimeCallKind::Println => 1,
        };
        if arg_types.len() != expected_arity || arg_modes.len() != expected_arity {
            return UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity);
        }
        if !arg_modes.iter().all(|mode| *mode == CfgArgMode::Normal) {
            return UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature);
        }

        let signature_matches = match runtime_call {
            UnsupportedRuntimeCallKind::StringConcat => {
                arg_types.iter().all(|ty| self.is_owned_string_type(*ty))
                    && self.is_owned_string_type(result_ty)
            }
            UnsupportedRuntimeCallKind::StringContains
            | UnsupportedRuntimeCallKind::StringStartsWith => {
                arg_types.iter().all(|ty| self.is_owned_string_type(*ty)) && result_ty == Type::BOOL
            }
            UnsupportedRuntimeCallKind::StringSubstring => {
                self.is_owned_string_type(arg_types[0])
                    && arg_types[1] == Type::U64
                    && arg_types[2] == Type::U64
                    && self.is_owned_string_type(result_ty)
            }
            UnsupportedRuntimeCallKind::Print | UnsupportedRuntimeCallKind::Println => {
                self.is_owned_string_type(arg_types[0]) && result_ty == Type::UNIT
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
        name: &str,
        args: &[Value],
        arg_types: &[Type],
        arg_modes: &[CfgArgMode],
        result_ty: Type,
    ) -> UnsupportedKind {
        let static_kind =
            self.classify_unsupported_runtime_call_static(name, arg_types, arg_modes, result_ty);
        let UnsupportedKind::SemanticGap(SemanticGapKind::RuntimeCall(runtime_call)) = static_kind
        else {
            return static_kind;
        };
        if args.len() != arg_types.len() {
            return UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity);
        }

        let is_owned_value = |index: usize| matches!(args[index], Value::Str { slots: 3, .. });
        let is_int_value = |index: usize| matches!(args[index], Value::Int(_));
        let values_match = match runtime_call {
            UnsupportedRuntimeCallKind::StringConcat
            | UnsupportedRuntimeCallKind::StringContains
            | UnsupportedRuntimeCallKind::StringStartsWith => {
                is_owned_value(0) && is_owned_value(1)
            }
            UnsupportedRuntimeCallKind::StringSubstring => {
                is_owned_value(0) && is_int_value(1) && is_int_value(2)
            }
            UnsupportedRuntimeCallKind::Print | UnsupportedRuntimeCallKind::Println => {
                is_owned_value(0)
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
                let CfgInstData::StructInit {
                    struct_id,
                    fields_start,
                    fields_len,
                } = cfg.get_inst(value).data
                else {
                    return false;
                };
                let def = self.state.type_pool.struct_def(struct_id);
                let is_slice = def.name.starts_with('[')
                    && def.name.ends_with(']')
                    && parse_array_type_syntax(&def.name).is_none();
                if !is_slice
                    || def.fields.len() != 2
                    || def.fields[0].ty != pointer_ty
                    || def.fields[1].ty != Type::U64
                    || fields_len != 2
                    || cfg.get_inst(value).ty != Type::new_struct(struct_id)
                    || cfg.value_use_count(value) != 1
                {
                    return false;
                }
                let fields = cfg.get_extra(fields_start, fields_len);
                fields[0] == intrinsic
                    && cfg.get_inst(fields[1]).ty == Type::U64
                    && matches!(cfg.get_inst(fields[1]).data, CfgInstData::Const(0))
                    && cfg.blocks().iter().any(|block| {
                        block.insts.iter().any(|candidate| {
                            let CfgInstData::Call {
                                args_start,
                                args_len,
                                ..
                            } = &cfg.get_inst(*candidate).data
                            else {
                                return false;
                            };
                            cfg.get_call_args(*args_start, *args_len)
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
                let width = if cfg.is_param_inout(slot) {
                    1
                } else {
                    self.state.type_pool.abi_slot_count(place.base_type)
                };
                (slot, cfg.num_params(), width, Some(slot))
            }
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
            "read_line" | "random_u32" | "random_u64" => args.is_empty(),
            "ptr_write" | "ptr_offset" | "free" => args.len() == 2,
            "realloc" => args.len() == 3,
            "syscall" => (1..=7).contains(&args.len()),
            _ => args.len() == 1,
        };
        if !arity_matches {
            return UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity);
        }

        let ty = |index: usize| cfg.get_inst(args[index]).ty;
        let is_owned_string = |index: usize| self.is_owned_string_type(ty(index));
        let mut validated_kind = kind;
        let signature_matches = match name {
            "parse_i32" => is_owned_string(0) && self.is_option_of(result_ty, Type::I32),
            "parse_i64" => is_owned_string(0) && self.is_option_of(result_ty, Type::I64),
            "parse_u32" => is_owned_string(0) && self.is_option_of(result_ty, Type::U32),
            "parse_u64" => is_owned_string(0) && self.is_option_of(result_ty, Type::U64),
            "ptr_read" => self
                .pointer_pointee(ty(0))
                .is_some_and(|(pointee, _)| pointee == result_ty),
            "ptr_write" => self
                .pointer_pointee(ty(0))
                .is_some_and(|(pointee, mutable)| {
                    mutable && pointee == ty(1) && result_ty == Type::UNIT
                }),
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
            "read_line" => self
                .option_payload(result_ty)
                .is_some_and(|payload| self.is_owned_string_type(payload)),
            "random_u32" => result_ty == Type::U32,
            "random_u64" => result_ty == Type::U64,
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
                    && (args.is_empty() || self.is_owned_string_type(ty(0)))
            }
            AbortIntrinsic::Assert => {
                result_ty == Type::UNIT
                    && ty(0) == Type::BOOL
                    && (args.len() == 1 || self.is_owned_string_type(ty(1)))
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
                [Value::Str { bytes, slots: 3 }] => self
                    .abort_with_stderr(TrapKind::UserPanic, &[b"panic: ", bytes.as_slice(), b"\n"]),
                [_] => Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                    "intrinsic @panic runtime value shape",
                )),
                _ => unreachable!("@panic arity was preflighted"),
            },
            AbortIntrinsic::Assert => {
                let (condition, message) = match values.as_slice() {
                    [Value::Bool(condition)] => (*condition, None),
                    [Value::Bool(condition), Value::Str { bytes, slots: 3 }] => {
                        (*condition, Some(bytes.as_slice()))
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
                    self.abort_with_stderr(TrapKind::UserPanic, &[b"panic: ", message, b"\n"])
                } else {
                    self.abort_with_stderr(TrapKind::AssertionFailure, &[b"assertion failed\n"])
                }
            }
        }
    }

    fn write_dbg(&mut self, val: &Value, ty: Type) -> Step<()> {
        let remaining = self.stdout_cap.saturating_sub(self.stdout_bytes);

        // Formatting a String normally clones its complete contents. Reject an
        // oversized value before formatting so the output limit also bounds
        // that temporary allocation.
        if let Value::Str { bytes, .. } = val
            && bytes.len() >= remaining
        {
            return Err(unsupported(
                UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes),
                format!(
                    "stdout byte limit exceeded ({}-byte limit)",
                    self.stdout_cap
                ),
            ));
        }

        let formatted = format_dbg(val, ty)?;
        let emitted_len = match val {
            Value::Str { bytes, .. } => bytes.len(),
            _ => formatted.len(),
        };
        // Reserve one byte for the newline emitted by this `@dbg` call. Using
        // the comparison form avoids overflowing while computing `len + 1`.
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

    fn find_cfg(&self, name: &str) -> Option<&'a Cfg> {
        self.state
            .functions
            .iter()
            .map(|f| &f.cfg)
            .find(|c| c.fn_name() == name)
    }

    /// Returns the call's value **and** its final parameter slots, so the caller
    /// can copy out `inout` parameters (Rue `inout` is copy-in / copy-out, which
    /// is observably identical to by-reference under the law of exclusivity).
    fn call(&mut self, name: &str, args: &[Value]) -> Step<(Value, Vec<Option<Value>>)> {
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

    fn call_inner(&mut self, name: &str, args: &[Value]) -> Step<(Value, Vec<Option<Value>>)> {
        let cfg = self.find_cfg(name).ok_or_else(|| {
            unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::MissingFunctionBody),
                format!("call to '{name}'"),
            )
        })?;
        // Lay arguments out by slot: place each whole value at its base slot,
        // then pad with `None` for the extra slots an aggregate occupies. A
        // zero-sized argument occupies no slot at all (matching
        // `abi_slot_count`), so it is never materialized in `params` and the
        // following arguments are not shifted.
        let mut param_slots: Vec<Option<Value>> = Vec::with_capacity(args.len());
        for a in args {
            let w = slot_width(a);
            if w == 0 {
                continue;
            }
            param_slots.push(Some(a.clone()));
            for _ in 1..w {
                param_slots.push(None);
            }
        }
        let mut frame = Frame {
            params: param_slots,
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
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

            let term = block.terminator;
            match term {
                Terminator::Return { value } => {
                    let ret = match value {
                        Some(v) => self.eval(cfg, &mut frame, v)?,
                        None => Value::Unit,
                    };
                    return Ok((ret, std::mem::take(&mut frame.params)));
                }
                Terminator::Goto { target, .. } => {
                    let ce = cfg.get_goto_args(&term).to_vec();
                    incoming = self.eval_all(cfg, &mut frame, &ce)?;
                    current = target;
                }
                Terminator::Branch {
                    cond,
                    then_block,
                    else_block,
                    ..
                } => {
                    let c = self.eval(cfg, &mut frame, cond)?.as_bool();
                    let args = if c {
                        cfg.get_branch_then_args(&term)
                    } else {
                        cfg.get_branch_else_args(&term)
                    }
                    .to_vec();
                    incoming = self.eval_all(cfg, &mut frame, &args)?;
                    current = if c { then_block } else { else_block };
                }
                Terminator::Switch {
                    scrutinee,
                    cases_start,
                    cases_len,
                    default,
                } => {
                    // The scrutinee is an integer, a discriminant-only enum
                    // (`Int` tag), or a payload-carrying enum (`Aggregate` whose
                    // element 0 is the tag, RUE-285). Switch on the discriminant.
                    let s = match self.eval(cfg, &mut frame, scrutinee)? {
                        Value::Aggregate(elems) => elems.first().map(Value::as_int).unwrap_or(0),
                        other => other.as_int(),
                    };
                    let cases = cfg.get_switch_cases(cases_start, cases_len);
                    // Case values are stored as i64 bit patterns; compare by the
                    // 64-bit pattern so unsigned extremes (u64::MAX -> -1 as i64)
                    // and i64::MIN match the scrutinee regardless of signedness.
                    let s_bits = s as i64 as u64;
                    current = cases
                        .iter()
                        .find(|(val, _)| *val as u64 == s_bits)
                        .map(|(_, blk)| *blk)
                        .unwrap_or(default);
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

    fn string_builtin_arity(name: &str) -> Option<usize> {
        match name {
            "__rue_String_new" => Some(0),
            "__rue_to_string"
            | "__rue_to_string_unsigned"
            | "__rue_String_with_capacity"
            | "__rue_String_len"
            | "__rue_String_capacity"
            | "__rue_String_is_empty"
            | "__rue_String_clear"
            | "__rue_String_clone" => Some(1),
            "__rue_String_push_str"
            | "__rue_String_push"
            | "__rue_String_reserve"
            | "__rue_String_byte_at"
            | "__rue_str_byte_at"
            | "__rue_String_char_scalar"
            | "__rue_String_char_scalar_lossy"
            | "__rue_String_char_next"
            | "__rue_String_char_next_lossy" => Some(2),
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
        name: &str,
        arg_types: &[Type],
        arg_modes: &[CfgArgMode],
        result_ty: Type,
    ) -> Step<bool> {
        let Some(expected) = Self::string_builtin_arity(name) else {
            return Ok(false);
        };
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

        let owned = |index: usize| self.is_owned_string_type(arg_types[index]);
        let argument_types_match = match name {
            "__rue_String_new" => true,
            "__rue_to_string" => arg_types[0] == Type::I64,
            "__rue_to_string_unsigned" | "__rue_String_with_capacity" => arg_types[0] == Type::U64,
            "__rue_String_len"
            | "__rue_String_capacity"
            | "__rue_String_is_empty"
            | "__rue_String_clear"
            | "__rue_String_clone" => owned(0),
            "__rue_String_push_str" => owned(0) && owned(1),
            "__rue_String_push" => owned(0) && arg_types[1] == Type::U8,
            "__rue_String_reserve" => owned(0) && arg_types[1] == Type::U64,
            "__rue_String_byte_at" => owned(0) && arg_types[1].is_integer(),
            "__rue_str_byte_at" => self.is_str_view_type(arg_types[0]) && arg_types[1].is_integer(),
            "__rue_String_char_scalar"
            | "__rue_String_char_scalar_lossy"
            | "__rue_String_char_next"
            | "__rue_String_char_next_lossy" => owned(0) && arg_types[1] == Type::U64,
            _ => unreachable!("arity table and signature table must stay exhaustive"),
        };
        if !argument_types_match {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType),
                format!("builtin '{name}' argument type drift"),
            ));
        }

        let result_type_matches = match name {
            "__rue_String_len" | "__rue_String_capacity" => result_ty == Type::U64,
            "__rue_String_is_empty" => result_ty == Type::BOOL,
            "__rue_String_byte_at" | "__rue_str_byte_at" => result_ty == Type::U8,
            "__rue_String_char_scalar" | "__rue_String_char_scalar_lossy" => result_ty == Type::U32,
            "__rue_String_char_next" | "__rue_String_char_next_lossy" => result_ty == Type::U64,
            _ => self.is_owned_string_type(result_ty),
        };
        if !result_type_matches {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinResultType),
                format!("builtin '{name}' result type drift"),
            ));
        }

        Ok(true)
    }

    /// Dispatch a builtin `String` method (these have no CFG body). Returns
    /// `Ok(None)` if `name` is not a String builtin (so the caller falls back to
    /// the ordinary CFG call). `capacity`/`reserve`/`with_capacity` capacity
    /// behavior is implementation-defined and deliberately not modeled.
    fn string_builtin(
        &self,
        name: &str,
        args: &[Value],
        arg_types: &[Type],
        arg_modes: &[CfgArgMode],
        result_ty: Type,
    ) -> Step<Option<Value>> {
        if !self.preflight_string_builtin(name, arg_types, arg_modes, result_ty)? {
            return Ok(None);
        }
        if args.len() != arg_types.len() {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArity),
                format!("builtin '{name}' runtime argument count drift"),
            ));
        }
        let value_shapes_match = match name {
            "__rue_String_new" => true,
            "__rue_to_string" | "__rue_to_string_unsigned" | "__rue_String_with_capacity" => {
                matches!(args[0], Value::Int(_))
            }
            "__rue_String_len"
            | "__rue_String_capacity"
            | "__rue_String_is_empty"
            | "__rue_String_clear"
            | "__rue_String_clone" => matches!(args[0], Value::Str { slots: 3, .. }),
            "__rue_String_push_str" => {
                matches!(args[0], Value::Str { slots: 3, .. })
                    && matches!(args[1], Value::Str { slots: 3, .. })
            }
            "__rue_String_push"
            | "__rue_String_reserve"
            | "__rue_String_byte_at"
            | "__rue_String_char_scalar"
            | "__rue_String_char_scalar_lossy"
            | "__rue_String_char_next"
            | "__rue_String_char_next_lossy" => {
                matches!(args[0], Value::Str { slots: 3, .. }) && matches!(args[1], Value::Int(_))
            }
            "__rue_str_byte_at" => {
                matches!(args[0], Value::Str { slots: 2, .. }) && matches!(args[1], Value::Int(_))
            }
            _ => unreachable!("preflight recognized this builtin"),
        };
        if !value_shapes_match {
            return Err(unsupported(
                UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType),
                format!("builtin '{name}' runtime argument shape drift"),
            ));
        }
        // Same honesty rule for argument TYPES: a non-string where the modeled
        // signature has a string is drift, not an empty string.
        let s = |v: &Value| -> Result<Vec<u8>, Flow> {
            match v {
                Value::Str { bytes, .. } => Ok(bytes.clone()),
                _ => Err(unsupported(
                    UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType),
                    format!("builtin '{name}' received a non-string argument (signature drift?)"),
                )),
            }
        };
        let out = match name {
            // `@to_string(n)` (RUE-314). The argument is already widened to a
            // 64-bit value by sema (sign-extended to i64 for signed types,
            // zero-extended to u64 for unsigned); format it with the matching
            // signedness so a high-bit-set unsigned value prints unsigned.
            "__rue_to_string" => Value::string((args[0].as_int() as i64).to_string()),
            "__rue_to_string_unsigned" => Value::string((args[0].as_int() as u64).to_string()),
            "__rue_String_new" => Value::string(String::new()),
            "__rue_String_with_capacity" => Value::string(String::new()),
            "__rue_String_push_str" => {
                let mut base = s(&args[0])?;
                base.extend_from_slice(&s(&args[1])?);
                Value::string_bytes(base)
            }
            "__rue_String_push" => {
                let mut base = s(&args[0])?;
                let byte = args[1].as_int() as u8;
                // `String::push` appends exactly one raw byte. A byte >= 0x80
                // may make the buffer invalid UTF-8; that is permitted for the
                // byte-string `String` model, and only strict UTF-8 operations
                // such as `.chars()` trap later.
                base.push(byte);
                Value::string_bytes(base)
            }
            "__rue_String_len" => Value::Int(s(&args[0])?.len() as i128),
            "__rue_String_is_empty" => Value::Bool(s(&args[0])?.is_empty()),
            "__rue_String_clear" => Value::string(String::new()),
            "__rue_String_clone" => Value::string_bytes(s(&args[0])?),
            "__rue_String_reserve" => Value::string_bytes(s(&args[0])?),
            // `s[i]` byte indexing (ADR-0035): the runtime `__rue_String_byte_at`
            // bounds-checks `index >= len` — trapping like array indexing — and
            // returns the raw byte zero-extended. Model it over the byte content
            // directly. A negative index is passed to the runtime as a huge u64,
            // so it is likewise out of bounds and traps.
            "__rue_String_byte_at" => {
                let bytes = s(&args[0])?;
                let idx = args[1].as_int();
                if idx < 0 || idx as u128 >= bytes.len() as u128 {
                    return Err(Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)));
                }
                Value::Int(bytes[idx as usize] as i128)
            }
            // `str` byte indexing `s[i]` (ADR-0043 Phase 3, RUE-324): the runtime
            // `__rue_str_byte_at(ptr, len, index)` is the 2-word analog of
            // `__rue_String_byte_at`, reading the i-th PACKED byte with the same
            // bounds-check-and-trap discipline. Modeled identically over the byte
            // content (the `str` receiver is `arg[0]`, the index `arg[1]`).
            "__rue_str_byte_at" => {
                let bytes = s(&args[0])?;
                let idx = args[1].as_int();
                if idx < 0 || idx as u128 >= bytes.len() as u128 {
                    return Err(Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)));
                }
                Value::Int(bytes[idx as usize] as i128)
            }
            // `for c in s.chars()` scalar view (RUE-220): strict decoding traps
            // on invalid UTF-8, matching `__rue_String_char_scalar`.
            "__rue_String_char_scalar" => {
                let bytes = s(&args[0])?;
                match char_at(&bytes, args[1].as_int()) {
                    Some((scalar, _)) => Value::Int(scalar as i128),
                    None => return Err(Flow::Panic(Panic::runtime(TrapKind::InvalidUtf8))),
                }
            }
            "__rue_String_char_next" => {
                let bytes = s(&args[0])?;
                let offset = args[1].as_int();
                match char_at(&bytes, offset) {
                    Some((_, width)) => Value::Int(offset + width as i128),
                    None => return Err(Flow::Panic(Panic::runtime(TrapKind::InvalidUtf8))),
                }
            }
            // The lossy character view never traps: invalid UTF-8 becomes U+FFFD
            // and advances by the same maximal-subpart width as the runtime.
            "__rue_String_char_scalar_lossy" => {
                let bytes = s(&args[0])?;
                let (scalar, _) = char_at_lossy(&bytes, args[1].as_int());
                Value::Int(scalar as i128)
            }
            "__rue_String_char_next_lossy" => {
                let bytes = s(&args[0])?;
                let offset = args[1].as_int();
                let (_, width) = char_at_lossy(&bytes, offset);
                Value::Int(offset + width as i128)
            }
            "__rue_String_capacity" => {
                return Err(unsupported(
                    UnsupportedKind::ImplementationDefined(
                        ImplementationDefinedKind::StringCapacityValue,
                    ),
                    "String::capacity is implementation-defined",
                ));
            }
            _ => return Ok(None),
        };
        Ok(Some(out))
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
                // A builtin type's drop (e.g. String freeing its heap buffer) has
                // no observable effect and no CFG body; skip it.
                if sd.is_builtin {
                    return Ok(());
                }
                if let Some(dtor) = sd.destructor.clone() {
                    self.call(&dtor, &[v.clone()])?;
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

    /// Whether `ty` occupies ZERO ABI parameter slots — the type-level twin of
    /// [`slot_width`] and of the compiler's `abi_slot_count == 0` (rue-air
    /// typeck): unit/never/comptime types, structs whose fields are all
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
                Self::set_local(frame, *slot, val);
                Value::Unit
            }
            CfgInstData::Load { slot } => Self::get_local(frame, *slot)?,
            CfgInstData::Store { slot, value } => {
                let val = self.eval(cfg, frame, *value)?;
                Self::set_local(frame, *slot, val);
                Value::Unit
            }
            CfgInstData::PlaceRead { place } => {
                let place = place.clone();
                if !self.place_projection_metadata_is_valid(cfg, &place, ty, PlaceAccess::Read) {
                    return Err(unsupported(
                        UnsupportedKind::ContractViolation(
                            ContractViolationKind::PlaceProjectionMetadata,
                        ),
                        "PlaceRead projection metadata drift",
                    ));
                }
                if let Some(kind) = self.place_base_violation(cfg, &place, PlaceAccess::Read) {
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
                self.place_read(cfg, frame, &place)?
            }
            CfgInstData::PlaceWrite { place, value } => {
                let place = place.clone();
                if !self.place_projection_metadata_is_valid(
                    cfg,
                    &place,
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
                if let Some(kind) = self.place_base_violation(cfg, &place, PlaceAccess::Write) {
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
                self.place_write(cfg, frame, &place, val)?;
                Value::Unit
            }

            CfgInstData::StructInit {
                fields_start,
                fields_len,
                ..
            } => {
                let fields = cfg.get_extra(*fields_start, *fields_len).to_vec();
                Value::Aggregate(self.eval_all(cfg, frame, &fields)?)
            }
            CfgInstData::ArrayInit {
                elements_start,
                elements_len,
            } => {
                let elems = cfg.get_extra(*elements_start, *elements_len).to_vec();
                Value::Aggregate(self.eval_all(cfg, frame, &elems)?)
            }
            legacy @ (CfgInstData::FieldSet { .. }
            | CfgInstData::IndexSet { .. }
            | CfgInstData::ParamFieldSet { .. }
            | CfgInstData::ParamIndexSet { .. }) => {
                return Err(unsupported(
                    UnsupportedKind::ContractViolation(
                        ContractViolationKind::UnexpectedLegacyMutationInstruction,
                    ),
                    format!("legacy convenience mutation instruction reached oracle: {legacy:?}"),
                ));
            }
            // A discriminant-only variant is its tag (an `Int`); a payload-
            // carrying variant is an `Aggregate` whose element 0 is the tag and
            // the rest are the payload fields, in declaration order (RUE-285).
            // This lets structural `==` distinguish `Circle(5)` from `Circle(6)`
            // and from `Square(5)`, and lets `EnumPayloadGet` project a field.
            CfgInstData::EnumVariant {
                variant_index,
                payload_start,
                payload_len,
                ..
            } => {
                if *payload_len == 0 {
                    Value::Int(*variant_index as i128)
                } else {
                    let payload_refs = cfg.get_extra(*payload_start, *payload_len).to_vec();
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
            CfgInstData::Call {
                name,
                args_start,
                args_len,
            } => {
                let fname = self.interner().resolve(name).to_string();
                let call_args = cfg.get_call_args(*args_start, *args_len).to_vec();
                let arg_types: Vec<Type> = call_args
                    .iter()
                    .map(|arg| cfg.get_inst(arg.value).ty)
                    .collect();
                let arg_modes: Vec<CfgArgMode> = call_args.iter().map(|arg| arg.mode).collect();
                let is_string_builtin =
                    self.preflight_string_builtin(&fname, &arg_types, &arg_modes, ty)?;
                let missing_call_kind = if !is_string_builtin && self.find_cfg(&fname).is_none() {
                    let kind = self.classify_unsupported_runtime_call_static(
                        &fname, &arg_types, &arg_modes, ty,
                    );
                    if kind.model_gap().is_none() {
                        return Err(unsupported(kind, format!("call to '{fname}'")));
                    }
                    Some(kind)
                } else {
                    None
                };
                // Copy-in every argument (by value); for `inout` args, remember
                // the base parameter slot and the caller place to copy back into.
                let mut argvals = Vec::with_capacity(call_args.len());
                let mut writebacks: Vec<(usize, Place)> = Vec::new();
                let mut base = 0usize;
                for a in &call_args {
                    let v = self.eval(cfg, frame, a.value)?;
                    // A zero-sized argument occupies no parameter slot (see
                    // `slot_width`): it neither advances `base` nor gets an
                    // inout write-back (the callee's params hold no slot for
                    // it to copy back from).
                    let w = slot_width(&v);
                    if matches!(a.mode, CfgArgMode::Inout) && w > 0 {
                        writebacks.push((base, self.lvalue_of(cfg, a.value)?));
                    }
                    argvals.push(v);
                    base += w;
                }
                // Builtin String methods have no CFG body; dispatch them here.
                if is_string_builtin {
                    self.string_builtin(&fname, &argvals, &arg_types, &arg_modes, ty)?
                        .expect("preflight recognized this String builtin")
                } else if missing_call_kind.is_some() {
                    return Err(unsupported(
                        self.classify_unsupported_runtime_call(
                            &fname, &argvals, &arg_types, &arg_modes, ty,
                        ),
                        format!("call to '{fname}'"),
                    ));
                } else {
                    let (result, final_params) = self.call(&fname, &argvals)?;
                    // Copy-out: write each inout parameter's final value back into
                    // the caller place it came from.
                    for (slot, place) in writebacks {
                        if let Some(val) = final_params.get(slot).and_then(|o| o.clone()) {
                            self.place_write(cfg, frame, &place, val)?;
                        }
                    }
                    result
                }
            }

            CfgInstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                let iname = self.interner().resolve(name).to_string();
                let args = cfg.get_extra(*args_start, *args_len).to_vec();
                if let Some(intrinsic) = self.preflight_abort_intrinsic(cfg, &iname, &args, ty)? {
                    self.eval_abort_intrinsic(cfg, frame, intrinsic, &args)?
                } else if iname != "dbg" {
                    return Err(unsupported(
                        self.classify_unsupported_intrinsic(cfg, v, &iname, &args, ty),
                        format!("intrinsic @{iname}"),
                    ));
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
        // Strings compare by content (String ==/!=). Aggregates (structs,
        // arrays, payload enums) compare STRUCTURALLY, field-by-field /
        // element-by-element / same-variant-and-equal-payload, via `Value`'s
        // derived `PartialEq` which recurses into nested aggregates (RUE-285).
        // Only `==` / `!=` reach an aggregate here (ordering `< > <= >=` is a
        // type error on aggregates), so we report `Equal` iff structurally
        // equal and an arbitrary non-`Equal` ordering otherwise — enough for
        // `pick` to decide `==`/`!=`. Everything else compares by integer value.
        let ord = match (&x, &y) {
            (Value::Str { bytes: sx, .. }, Value::Str { bytes: sy, .. }) => sx.cmp(sy),
            (Value::Aggregate(_), _) | (_, Value::Aggregate(_)) => {
                if x == y {
                    std::cmp::Ordering::Equal
                } else {
                    std::cmp::Ordering::Less
                }
            }
            _ => x.as_int().cmp(&y.as_int()),
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
        }
    }

    fn place_read(&mut self, cfg: &'a Cfg, frame: &mut Frame, place: &Place) -> Step<Value> {
        let (base, path) = self.resolve_path(cfg, frame, place)?;
        let mut cur = Self::base_value(frame, base)?;
        for (idx, projection) in path {
            cur = match cur {
                Value::Aggregate(mut v) if idx < v.len() => v.swap_remove(idx),
                Value::Aggregate(_) => {
                    return Err(Flow::Panic(Panic::runtime(TrapKind::IndexOutOfBounds)));
                }
                Value::Str { slots, .. } if self.is_matching_text_projection(projection, slots) => {
                    return Err(unsupported(
                        UnsupportedKind::SemanticGap(SemanticGapKind::TextProjectionRead),
                        "projection of non-aggregate",
                    ));
                }
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
        // Select the storage vector: writing through an `inout` parameter place
        // targets the param slot (copied back to the caller on return).
        let (store, slot) = match base {
            PlaceBase::Local(slot) => (&mut frame.locals, slot as usize),
            PlaceBase::Param(slot) => (&mut frame.params, slot as usize),
        };
        if slot >= store.len() {
            store.resize(slot + 1, None);
        }
        let root = store[slot].get_or_insert(Value::Unit);
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
    fn lvalue_of(&self, cfg: &'a Cfg, v: CfgValue) -> Step<Place> {
        match &cfg.get_inst(v).data {
            CfgInstData::Load { slot } if *slot < cfg.num_locals() => {
                Ok(Place::local(*slot, cfg.get_inst(v).ty))
            }
            CfgInstData::PlaceRead { place } if self.is_inout_writeback_place(cfg, v, place) => {
                Ok(place.clone())
            }
            other @ CfgInstData::Param { index }
                if *index < cfg.num_params() && cfg.is_param_writable(*index) =>
            {
                Err(unsupported(
                    UnsupportedKind::SemanticGap(SemanticGapKind::InoutParameterForwarding),
                    format!("inout argument is not an lvalue: {other:?}"),
                ))
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
}

/// Strictly decode the UTF-8 scalar starting at byte `offset`, returning
/// `(scalar, utf8_width)`. Backs the oracle's model of the
/// `__rue_String_char_scalar`/`__rue_String_char_next` runtime primitives.
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
    // `@dbg` of a String prints its content (matches __rue_dbg_str).
    if let Value::Str { bytes, .. } = val {
        return Ok(String::from_utf8_lossy(bytes));
    }
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
