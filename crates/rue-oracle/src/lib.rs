//! # rue-oracle — the executable reference semantics
//!
//! A tree-walking interpreter over the compiler's **CFG** (the typed
//! control-flow IR, produced by [`rue_compiler::compile_to_cfg`], *before* the
//! MIR/codegen lowering where every miscompile of the 2026-07 work lived).
//! Running a program through this interpreter and through the compiled binary
//! and comparing the observable behavior (exit code, `@dbg` output, panic-or-not)
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
//! `loop` all lower to `Goto`/`Branch`/`Switch`), `@dbg`, `@intCast`, and the
//! defined panics (overflow, divide/remainder-by-zero, int-cast overflow);
//! aggregates (structs/arrays) with nested place projections and bounds traps;
//! `inout`/`borrow` parameters (copy-in / copy-out, observably identical to
//! by-reference under the law of exclusivity); and drop/destructors, executed in
//! spec-3.9 order (user destructor, then fields in declaration order / elements
//! ascending) so the oracle validates drop *order* and drop-exactly-once, not
//! just final values; and `String` — content modeled directly, covering `new`,
//! `push` (ASCII), `push_str`, `len`, `is_empty`, `clear`, `clone`, `@to_string`,
//! byte indexing `s[i]` (`__rue_String_byte_at`), and the `.chars()` /
//! `.chars_lossy()` scalar view (`__rue_String_char_scalar`/`_char_next`, whose
//! strict and lossy forms coincide over the oracle's always-valid-UTF-8 content).
//!
//! Note that many intrinsics never reach the interpreter *as* intrinsics because
//! sema lowers them earlier: `@target_arch`/`@target_os` fold to a compile-time
//! `EnumVariant` against `Target::host()` (the oracle runs on that same host, so
//! the discriminant it evaluates agrees with the compiled binary); `@to_string`,
//! `@parse*`, byte indexing, and `.chars()` become `Call`s; `@intCast` becomes
//! `IntCast`. Those are all covered through the resulting instruction paths.
//!
//! ## What is *not* modeled (reported [`Unsupported`], never guessed)
//!
//! A program hitting any of these is **skipped** by the differential harness, so
//! "compiles + runs under the oracle" is NOT the same as "differentially
//! checked" — the gaps below are genuinely unvalidated:
//!
//! - **All CFG intrinsics except `@dbg`.** The `Intrinsic` arm models only
//!   `@dbg`; every other intrinsic that survives to the CFG bails: the
//!   non-deterministic `@read_line`, `@random_u32`/`@random_u64`, `@syscall`;
//!   the heap intrinsics `@alloc`/`@free`/`@realloc`; the raw-pointer intrinsics
//!   `@raw`/`@raw_mut`/`@field_ptr`/`@ptr_read`/`@ptr_write`/`@ptr_offset`/
//!   `@ptr_to_int`/`@int_to_ptr` (heap-/layout-dependent, and `checked`-only);
//!   and `@panic`/`@assert` (deterministic, but not yet modeled).
//! - **`String::capacity`/`reserve`/`with_capacity` capacity behavior** — the
//!   heap layout (ptr, len, cap) is implementation-defined, so `capacity` is
//!   reported [`Unsupported`] rather than guessed.
//! - **Non-ASCII `String::push`** — a byte `>= 0x80` is not standalone valid
//!   UTF-8, which the Rust-`String`-backed `Value::Str` cannot represent.
//! - **Deeply-nested `inout` field writes** (non-zero inner offset).

use lasso::ThreadedRodeo;
use rue_air::{Type, TypeKind};
use rue_cfg::{Cfg, CfgArgMode, CfgInstData, CfgValue, Place, PlaceBase, Projection, Terminator};
use rue_compiler::{CompileState, compile_to_cfg};
use std::collections::HashMap;

/// Observable result of running a program under the oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Process exit code (the OS masks the returned `i32` to a `u8`).
    pub exit_code: i32,
    /// Everything `@dbg` wrote, in order, newline-terminated per call.
    pub stdout: String,
    /// `Some(reason)` if execution ended in a runtime panic (exit code 101),
    /// `None` on normal completion. The reason mirrors the runtime's category.
    pub panic: Option<String>,
}

/// A construct the interpreter does not yet model (see the coverage note). This
/// is *not* a program error — it means the oracle cannot judge this program yet,
/// so a differential harness should skip it rather than report a disagreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported(pub String);

/// Compile `source` to CFG and run it under the reference semantics.
///
/// `Err(Unsupported)` means the program uses a construct outside the current
/// slice. A compile error is surfaced as `Err(Unsupported("compile: .."))` so
/// callers can distinguish it from a real interpreter result; a well-typed
/// program in the supported subset always yields `Ok`.
pub fn run_source(source: &str) -> Result<Outcome, Unsupported> {
    let state = compile_to_cfg(source).map_err(|e| Unsupported(format!("compile: {e:?}")))?;
    // Interpret on a dedicated large-stack worker thread. The tree-walking
    // interpreter recurses per expression *and* per call, so deep-but-valid
    // programs need far more stack than a default thread provides. Running on
    // our own generous stack makes `run_source` safe to call from any thread
    // (a 2 MiB Rust test thread included) and, together with `MAX_DEPTH`, lets
    // unbounded recursion resolve to an `Unsupported` skip *before* the Rust
    // stack is exhausted rather than aborting the process (RUE-340).
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(WORKER_STACK)
            .spawn_scoped(scope, || {
                Interp {
                    state: &state,
                    stdout: String::new(),
                    budget: STEP_BUDGET,
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
/// combination — and reports [`Unsupported`] instead of hanging. Generous
/// enough that no legitimate program in the differential corpus reaches it.
const STEP_BUDGET: u64 = 50_000_000;

/// Maximum interpreter call-recursion depth, shared across all activations.
/// Each Rue call activation is a nested `call` -> `eval` Rust recursion, so
/// unbounded Rue recursion would otherwise overflow the *Rust* stack — an
/// uncatchable process abort that kills the whole differential harness rather
/// than yielding a clean skip (RUE-340). This bound fires first, turning
/// deep/infinite recursion into an [`Unsupported`] skip. It sits far above any
/// legitimately-recursive corpus/fuzzer program yet far below the number of
/// activations that fit in `WORKER_STACK`, so the skip always wins the race
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
    /// A `String`, modeled by its content only; the heap layout (ptr, len, cap)
    /// and capacity are implementation details the oracle abstracts over
    /// (`capacity`/`reserve` are unmodeled — see the builtin dispatch).
    Str(String),
}

impl Value {
    fn as_int(&self) -> i128 {
        match self {
            Value::Int(n) => *n,
            Value::Bool(b) => *b as i128,
            // Unreachable for a well-typed program (aggregates/strings never
            // reach a scalar context); defined so callers need not thread an error.
            Value::Unit | Value::Aggregate(_) | Value::Str(_) => 0,
        }
    }
    fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Unit | Value::Aggregate(_) | Value::Str(_) => false,
        }
    }
}

/// A runtime panic, carrying its category (mirrors the runtime's panic classes).
struct Panic(String);

type Step<T> = Result<T, Flow>;

/// Non-local outcomes of evaluating an instruction.
enum Flow {
    /// A modeled runtime panic (maps to exit 101).
    Panic(Panic),
    /// The program uses an unmodeled construct.
    Unsupported(Unsupported),
}

impl From<Unsupported> for Flow {
    fn from(u: Unsupported) -> Self {
        Flow::Unsupported(u)
    }
}

struct Interp<'a> {
    state: &'a CompileState,
    stdout: String,
    /// Remaining total step budget (see [`STEP_BUDGET`]). Shared across every
    /// activation and decremented per instruction, so it bounds total work
    /// including recursion, not just per-frame loops.
    budget: u64,
    /// Current call-recursion depth (see [`MAX_DEPTH`]). Incremented on entry to
    /// each `call` and decremented on exit, bounding Rust-stack recursion.
    depth: u32,
}

/// Per-call activation record. `cache` is scoped to a *single block execution*
/// (cleared on entry to each block): the CFG is block-parameter SSA, so all
/// cross-block dataflow flows through block parameters, and a persistent cache
/// would return stale values on loop back-edges.
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
        // A String is a fat value occupying three ABI slots (ptr, len, cap).
        Value::Str(_) => 3,
        Value::Unit => 0,
        _ => 1,
    }
}

impl<'a> Interp<'a> {
    fn run(mut self) -> Result<Outcome, Unsupported> {
        match self.call("main", &[]) {
            Ok((v, _)) => Ok(Outcome {
                exit_code: (v.as_int() & 0xFF) as i32,
                stdout: self.stdout,
                panic: None,
            }),
            Err(Flow::Panic(Panic(reason))) => Ok(Outcome {
                exit_code: 101,
                stdout: self.stdout,
                panic: Some(reason),
            }),
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
        // so unbounded Rue recursion resolves to a clean `Unsupported` skip
        // instead of overflowing the Rust stack and aborting the process
        // (RUE-340). Decrement on every exit path by capturing the result.
        if self.depth >= MAX_DEPTH {
            return Err(Flow::Unsupported(Unsupported(
                "recursion depth budget exhausted".into(),
            )));
        }
        self.depth += 1;
        let result = self.call_inner(name, args);
        self.depth -= 1;
        result
    }

    fn call_inner(&mut self, name: &str, args: &[Value]) -> Step<(Value, Vec<Option<Value>>)> {
        let cfg = self
            .find_cfg(name)
            .ok_or_else(|| Flow::Unsupported(Unsupported(format!("call to '{name}'"))))?;
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
            frame.cache.clear();
            for (i, (pv, _)) in block.params.iter().enumerate() {
                let val = incoming
                    .get(i)
                    .cloned()
                    .ok_or_else(|| Flow::Unsupported(Unsupported("block arg arity".into())))?;
                frame.cache.insert(pv.as_u32(), val);
            }
            for &v in &block.insts {
                // Decrement the shared total-work budget (see `STEP_BUDGET`): a
                // runaway loop or deep recursion reports `Unsupported` here
                // rather than hanging.
                self.budget = self.budget.checked_sub(1).ok_or_else(|| {
                    Flow::Unsupported(Unsupported("step budget exhausted".into()))
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
                    return Err(Flow::Panic(Panic("reached unreachable".into())));
                }
                Terminator::None => {
                    return Err(Flow::Unsupported(Unsupported("terminator None".into())));
                }
            }
        }
    }

    /// Dispatch a builtin `String` method (these have no CFG body). Returns
    /// `Ok(None)` if `name` is not a String builtin (so the caller falls back to
    /// the ordinary CFG call). `capacity`/`reserve`/`with_capacity` capacity
    /// behavior is implementation-defined and deliberately not modeled.
    fn string_builtin(&self, name: &str, args: &[Value]) -> Step<Option<Value>> {
        // Logical-argument count each modeled builtin expects (receiver
        // included). A call shape that doesn't match means the runtime-fn
        // signature drifted from what the oracle models (the RUE-314 class):
        // skip honestly rather than read args positionally from the wrong
        // slots and return a plausible-but-wrong value.
        let expected_arity = match name {
            "__rue_String_new" => Some(0),
            "__rue_to_string"
            | "__rue_to_string_unsigned"
            | "__rue_String_with_capacity"
            | "__rue_String_len"
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
        };
        if let Some(expected) = expected_arity
            && args.len() != expected
        {
            return Err(Flow::Unsupported(Unsupported(format!(
                "builtin '{name}' called with {} args, oracle models {expected} (signature drift?)",
                args.len()
            ))));
        }
        // Same honesty rule for argument TYPES: a non-string where the modeled
        // signature has a string is drift, not an empty string.
        let s = |v: &Value| -> Result<String, Flow> {
            match v {
                Value::Str(s) => Ok(s.clone()),
                _ => Err(Flow::Unsupported(Unsupported(format!(
                    "builtin '{name}' received a non-string argument (signature drift?)"
                )))),
            }
        };
        let out = match name {
            // `@to_string(n)` (RUE-314). The argument is already widened to a
            // 64-bit value by sema (sign-extended to i64 for signed types,
            // zero-extended to u64 for unsigned); format it with the matching
            // signedness so a high-bit-set unsigned value prints unsigned.
            "__rue_to_string" => Value::Str((args[0].as_int() as i64).to_string()),
            "__rue_to_string_unsigned" => Value::Str((args[0].as_int() as u64).to_string()),
            "__rue_String_new" => Value::Str(String::new()),
            "__rue_String_with_capacity" => Value::Str(String::new()),
            "__rue_String_push_str" => Value::Str(s(&args[0])? + &s(&args[1])?),
            "__rue_String_push" => {
                let mut base = s(&args[0])?;
                let byte = args[1].as_int() as u8;
                if byte < 0x80 {
                    // ASCII: exactly one byte, matching the runtime's raw-byte
                    // buffer (__rue_String_push writes a single byte).
                    base.push(byte as char);
                } else {
                    // push takes a u8 and a Rue String is a raw byte buffer, but
                    // a byte >= 0x80 is not valid standalone UTF-8, which
                    // Value::Str (a Rust String) cannot represent. The runtime
                    // stores the single raw byte; encoding a char here would
                    // store two. Skip rather than silently diverge (see RUE-342).
                    return Err(Flow::Unsupported(Unsupported(
                        "String::push of a non-ASCII byte (oracle cannot model non-UTF-8 String content)".into(),
                    )));
                }
                Value::Str(base)
            }
            "__rue_String_len" => Value::Int(s(&args[0])?.len() as i128),
            "__rue_String_is_empty" => Value::Bool(s(&args[0])?.is_empty()),
            "__rue_String_clear" => Value::Str(String::new()),
            "__rue_String_clone" => Value::Str(s(&args[0])?),
            "__rue_String_reserve" => Value::Str(s(&args[0])?),
            // `s[i]` byte indexing (ADR-0035): the runtime `__rue_String_byte_at`
            // bounds-checks `index >= len` — trapping like array indexing — and
            // returns the raw byte zero-extended. Model it over the byte content
            // directly. A negative index is passed to the runtime as a huge u64,
            // so it is likewise out of bounds and traps.
            "__rue_String_byte_at" => {
                let text = s(&args[0])?;
                let bytes = text.as_bytes();
                let idx = args[1].as_int();
                if idx < 0 || idx as u128 >= bytes.len() as u128 {
                    return Err(Flow::Panic(Panic("index out of bounds".into())));
                }
                Value::Int(bytes[idx as usize] as i128)
            }
            // `str` byte indexing `s[i]` (ADR-0043 Phase 3, RUE-324): the runtime
            // `__rue_str_byte_at(ptr, len, index)` is the 2-word analog of
            // `__rue_String_byte_at`, reading the i-th PACKED byte with the same
            // bounds-check-and-trap discipline. Modeled identically over the byte
            // content (the `str` receiver is `arg[0]`, the index `arg[1]`).
            "__rue_str_byte_at" => {
                let text = s(&args[0])?;
                let bytes = text.as_bytes();
                let idx = args[1].as_int();
                if idx < 0 || idx as u128 >= bytes.len() as u128 {
                    return Err(Flow::Panic(Panic("index out of bounds".into())));
                }
                Value::Int(bytes[idx as usize] as i128)
            }
            // `for c in s.chars()` scalar view (RUE-220): `__rue_String_char_scalar`
            // decodes the Unicode scalar at byte `offset`, and
            // `__rue_String_char_next` returns the byte offset of the following
            // character (`offset + utf8_width`). Both trap on invalid UTF-8 in the
            // runtime, but the oracle models String content as *valid* UTF-8
            // (`Value::Str` is a Rust `String`; a non-ASCII `push` bails to
            // `Unsupported`), so decoding never hits that trap in practice and the
            // strict and `_lossy` variants coincide over the modeled content — a
            // well-formed loop only ever passes whole-character-boundary offsets.
            "__rue_String_char_scalar" | "__rue_String_char_scalar_lossy" => {
                let text = s(&args[0])?;
                match char_at(&text, args[1].as_int()) {
                    Some((scalar, _)) => Value::Int(scalar as i128),
                    None => return Err(Flow::Panic(Panic("invalid UTF-8".into()))),
                }
            }
            "__rue_String_char_next" | "__rue_String_char_next_lossy" => {
                let text = s(&args[0])?;
                let offset = args[1].as_int();
                match char_at(&text, offset) {
                    Some((_, width)) => Value::Int(offset + width as i128),
                    None => return Err(Flow::Panic(Panic("invalid UTF-8".into()))),
                }
            }
            "__rue_String_capacity" => {
                return Err(Flow::Unsupported(Unsupported(
                    "String::capacity is implementation-defined".into(),
                )));
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
        match ty.kind() {
            TypeKind::Unit | TypeKind::Never | TypeKind::ComptimeType => true,
            TypeKind::Struct(sid) => {
                let sd = self.state.type_pool.struct_def(sid);
                sd.fields.iter().all(|f| self.is_zero_sized(f.ty))
            }
            TypeKind::Array(aid) => {
                let (elem_ty, len) = self.state.type_pool.array_def(aid);
                len == 0 || self.is_zero_sized(elem_ty)
            }
            _ => false,
        }
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
            CfgInstData::StringConst(idx) => Value::Str(
                self.state
                    .strings
                    .get(*idx as usize)
                    .cloned()
                    .ok_or_else(|| Flow::Unsupported(Unsupported("string const index".into())))?,
            ),
            CfgInstData::Param { index } => {
                if self.is_zero_sized(ty) {
                    // A zero-sized parameter occupies NO slot (abi_slot_count
                    // = 0), but the CFG still emits a Param read for it —
                    // sharing its `index` with the NEXT parameter's slot. Do
                    // not read the slot (that would grab the next parameter's
                    // value); materialize the unique ZST value instead.
                    self.zero_sized_value(ty)
                } else {
                    frame
                        .params
                        .get(*index as usize)
                        .and_then(|o| o.clone())
                        .ok_or_else(|| Flow::Unsupported(Unsupported("param index".into())))?
                }
            }
            CfgInstData::BlockParam { .. } => frame
                .cache
                .get(&v.as_u32())
                .cloned()
                .ok_or_else(|| Flow::Unsupported(Unsupported("unbound block param".into())))?,

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
                self.place_read(cfg, frame, &place)?
            }
            CfgInstData::PlaceWrite { place, value } => {
                let place = place.clone();
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
            CfgInstData::FieldSet {
                slot,
                field_index,
                value,
                ..
            } => {
                let val = self.eval(cfg, frame, *value)?;
                Self::set_agg_elem(frame, *slot, *field_index as usize, val)?;
                Value::Unit
            }
            CfgInstData::IndexSet {
                slot, index, value, ..
            } => {
                let idx = self.eval(cfg, frame, *index)?.as_int();
                let val = self.eval(cfg, frame, *value)?;
                if idx < 0 {
                    return Err(Flow::Panic(Panic("index out of bounds".into())));
                }
                Self::set_agg_elem(frame, *slot, idx as usize, val)?;
                Value::Unit
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
                    return Err(Flow::Unsupported(Unsupported(
                        "enum payload get on non-payload value".into(),
                    )));
                }
            },

            // Writes to an `inout` parameter inside the callee (visible to the
            // caller via copy-out).
            CfgInstData::ParamStore { param_slot, value } => {
                let val = self.eval(cfg, frame, *value)?;
                Self::set_param(frame, *param_slot, val);
                Value::Unit
            }
            CfgInstData::ParamFieldSet {
                param_slot,
                inner_offset,
                field_index,
                value,
                ..
            } => {
                if *inner_offset != 0 {
                    return Err(Flow::Unsupported(Unsupported(
                        "nested inout field write".into(),
                    )));
                }
                let val = self.eval(cfg, frame, *value)?;
                Self::set_param_elem(frame, *param_slot, *field_index as usize, val)?;
                Value::Unit
            }
            CfgInstData::ParamIndexSet {
                param_slot,
                index,
                value,
                ..
            } => {
                let idx = self.eval(cfg, frame, *index)?.as_int();
                let val = self.eval(cfg, frame, *value)?;
                if idx < 0 {
                    return Err(Flow::Panic(Panic("index out of bounds".into())));
                }
                Self::set_param_elem(frame, *param_slot, idx as usize, val)?;
                Value::Unit
            }

            CfgInstData::Call {
                name,
                args_start,
                args_len,
            } => {
                let fname = self.interner().resolve(name).to_string();
                let call_args = cfg.get_call_args(*args_start, *args_len).to_vec();
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
                        writebacks.push((base, Self::lvalue_of(cfg, a.value)?));
                    }
                    argvals.push(v);
                    base += w;
                }
                // Builtin String methods have no CFG body; dispatch them here.
                if let Some(v) = self.string_builtin(&fname, &argvals)? {
                    v
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
                if iname != "dbg" {
                    return Err(Flow::Unsupported(Unsupported(format!(
                        "intrinsic @{iname}"
                    ))));
                }
                let args = cfg.get_extra(*args_start, *args_len).to_vec();
                let arg = args
                    .first()
                    .copied()
                    .ok_or_else(|| Flow::Unsupported(Unsupported("@dbg arity".into())))?;
                let arg_ty = cfg.get_inst(arg).ty;
                let val = self.eval(cfg, frame, arg)?;
                self.stdout.push_str(&format_dbg(&val, arg_ty)?);
                self.stdout.push('\n');
                Value::Unit
            }

            CfgInstData::IntCast { value, from_ty: _ } => {
                let x = self.eval(cfg, frame, *value)?.as_int();
                let (lo, hi) = int_bounds(ty).ok_or_else(|| {
                    Flow::Unsupported(Unsupported("intcast target not an int".into()))
                })?;
                if x < lo || x > hi {
                    return Err(Flow::Panic(Panic("integer cast overflow".into())));
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
            let reason = if is_mod {
                "remainder by zero"
            } else {
                "divide by zero"
            };
            return Err(Flow::Panic(Panic(reason.into())));
        }
        // MIN / -1 (and MIN % -1) trap at the operand width: the hardware `idiv`
        // faults even though the mathematical remainder is 0. Our i128 model
        // wouldn't otherwise catch it (the value fits in i128).
        if let Some((lo, _)) = int_bounds(ty) {
            if ty.is_signed() && x == lo && y == -1 {
                return Err(Flow::Panic(Panic("arithmetic overflow".into())));
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
            (Value::Str(sx), Value::Str(sy)) => sx.cmp(sy),
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
            .ok_or_else(|| Flow::Unsupported(Unsupported(format!("read of uninit local {slot}"))))
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
    ) -> Step<(PlaceBase, Vec<usize>)> {
        let projs = cfg.get_place_projections(place).to_vec();
        let mut path = Vec::with_capacity(projs.len());
        for p in projs {
            match p {
                Projection::Field { field_index, .. } => path.push(field_index as usize),
                Projection::Index { index, .. } => {
                    let i = self.eval(cfg, frame, index)?.as_int();
                    if i < 0 {
                        return Err(Flow::Panic(Panic("index out of bounds".into())));
                    }
                    path.push(i as usize);
                }
            }
        }
        Ok((place.base, path))
    }

    fn base_value(frame: &Frame, base: PlaceBase) -> Step<Value> {
        match base {
            PlaceBase::Local(slot) => Self::get_local(frame, slot),
            PlaceBase::Param(slot) => frame
                .params
                .get(slot as usize)
                .and_then(|o| o.clone())
                .ok_or_else(|| Flow::Unsupported(Unsupported("param place".into()))),
        }
    }

    fn place_read(&mut self, cfg: &'a Cfg, frame: &mut Frame, place: &Place) -> Step<Value> {
        let (base, path) = self.resolve_path(cfg, frame, place)?;
        let mut cur = Self::base_value(frame, base)?;
        for idx in path {
            cur = match cur {
                Value::Aggregate(mut v) if idx < v.len() => v.swap_remove(idx),
                Value::Aggregate(_) => {
                    return Err(Flow::Panic(Panic("index out of bounds".into())));
                }
                _ => {
                    return Err(Flow::Unsupported(Unsupported(
                        "projection of non-aggregate".into(),
                    )));
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
        for idx in &path {
            cur = match cur {
                Value::Aggregate(v) if *idx < v.len() => &mut v[*idx],
                Value::Aggregate(_) => {
                    return Err(Flow::Panic(Panic("index out of bounds".into())));
                }
                _ => {
                    return Err(Flow::Unsupported(Unsupported(
                        "projection of non-aggregate".into(),
                    )));
                }
            };
        }
        *cur = val;
        Ok(())
    }

    /// Set element `idx` of the aggregate held in local `slot` (used by the
    /// `FieldSet`/`IndexSet` convenience instructions).
    fn set_agg_elem(frame: &mut Frame, slot: u32, idx: usize, val: Value) -> Step<()> {
        match frame.locals.get_mut(slot as usize).and_then(|o| o.as_mut()) {
            Some(Value::Aggregate(v)) if idx < v.len() => {
                v[idx] = val;
                Ok(())
            }
            Some(Value::Aggregate(_)) => Err(Flow::Panic(Panic("index out of bounds".into()))),
            _ => Err(Flow::Unsupported(Unsupported(
                "field/index set on non-aggregate".into(),
            ))),
        }
    }

    /// Recover the caller place an `inout` argument was loaded from, so its
    /// mutated value can be copied back after the call.
    fn lvalue_of(cfg: &'a Cfg, v: CfgValue) -> Step<Place> {
        match &cfg.get_inst(v).data {
            CfgInstData::Load { slot } => Ok(Place::local(*slot)),
            CfgInstData::PlaceRead { place } => Ok(place.clone()),
            other => Err(Flow::Unsupported(Unsupported(format!(
                "inout argument is not an lvalue: {other:?}"
            )))),
        }
    }

    fn set_param(frame: &mut Frame, slot: u32, val: Value) {
        let s = slot as usize;
        if s >= frame.params.len() {
            frame.params.resize(s + 1, None);
        }
        frame.params[s] = Some(val);
    }

    fn set_param_elem(frame: &mut Frame, slot: u32, idx: usize, val: Value) -> Step<()> {
        match frame.params.get_mut(slot as usize).and_then(|o| o.as_mut()) {
            Some(Value::Aggregate(v)) if idx < v.len() => {
                v[idx] = val;
                Ok(())
            }
            Some(Value::Aggregate(_)) => Err(Flow::Panic(Panic("index out of bounds".into()))),
            _ => Err(Flow::Unsupported(Unsupported(
                "field/index set on non-aggregate inout param".into(),
            ))),
        }
    }
}

/// Decode the character starting at byte `offset` in a valid-UTF-8 string,
/// returning `(scalar, utf8_width)`. Backs the oracle's model of the
/// `__rue_String_char_scalar`/`__rue_String_char_next` runtime primitives.
/// Returns `None` when `offset` is out of range or not on a char boundary — a
/// case a well-formed `.chars()` loop never produces, since it starts at 0 and
/// advances by whole-character widths (and the oracle only ever holds valid
/// UTF-8, so every in-range boundary offset decodes).
fn char_at(text: &str, offset: i128) -> Option<(u32, u64)> {
    if offset < 0 || offset as u128 > text.len() as u128 {
        return None;
    }
    let off = offset as usize;
    if !text.is_char_boundary(off) {
        return None;
    }
    let ch = text[off..].chars().next()?;
    Some((ch as u32, ch.len_utf8() as u64))
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
            return Err(Flow::Unsupported(Unsupported(format!(
                "non-int type {kind:?}"
            ))));
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
    let n = v.ok_or_else(|| Flow::Panic(Panic("arithmetic overflow".into())))?;
    match int_bounds(ty) {
        Some((lo, hi)) if n < lo || n > hi => Err(Flow::Panic(Panic("arithmetic overflow".into()))),
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
fn format_dbg(val: &Value, ty: Type) -> Result<String, Flow> {
    // `@dbg` of a String prints its content (matches __rue_dbg_str).
    if let Value::Str(s) = val {
        return Ok(s.clone());
    }
    Ok(match ty.kind() {
        TypeKind::Bool => val.as_bool().to_string(),
        k if kind_signed(k) => val.as_int().to_string(),
        TypeKind::U8 | TypeKind::U16 | TypeKind::U32 | TypeKind::U64 => {
            let (lo, hi) = int_bounds(ty).unwrap();
            let n = val.as_int();
            let _ = (lo, hi);
            (n as u128).to_string()
        }
        other => {
            return Err(Flow::Unsupported(Unsupported(format!(
                "@dbg of type {other:?}"
            ))));
        }
    })
}

#[cfg(test)]
mod tests;
