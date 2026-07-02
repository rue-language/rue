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
//! ## Coverage (first vertical slice)
//!
//! Scalars (all integer widths + `bool` + `unit`), the full arithmetic /
//! comparison / bitwise / shift operator set with **trapping** overflow, locals,
//! parameters, calls and recursion, block-parameter control flow (`if`/`match`/
//! `loop` all lower to `Goto`/`Branch`/`Switch`), `@dbg`, `@intCast`, and the
//! defined panics (overflow, divide/remainder-by-zero, int-cast overflow). Not
//! yet: aggregates (structs/arrays/strings), places with projections, `inout`/
//! `borrow`, and drop/destructors — these are the next slices (see `docs/formal`
//! extension rubric). Programs using them return [`Unsupported`].

use lasso::ThreadedRodeo;
use rue_air::{Type, TypeKind};
use rue_cfg::{Cfg, CfgArgMode, CfgInstData, CfgValue, PlaceBase, Terminator};
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
    Interp {
        state: &state,
        stdout: String::new(),
    }
    .run()
}

/// A runtime value. Integers are held in `i128` (wide enough for every Rue
/// integer type, and to detect overflow of any of them before range-checking).
/// Unsigned values are stored as their non-negative magnitude.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Int(i128),
    Bool(bool),
    Unit,
}

impl Value {
    fn as_int(&self) -> i128 {
        match self {
            Value::Int(n) => *n,
            Value::Bool(b) => *b as i128,
            Value::Unit => 0,
        }
    }
    fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Unit => false,
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
}

/// Per-call activation record. `cache` is scoped to a *single block execution*
/// (cleared on entry to each block): the CFG is block-parameter SSA, so all
/// cross-block dataflow flows through block parameters, and a persistent cache
/// would return stale values on loop back-edges.
struct Frame {
    params: Vec<Value>,
    locals: Vec<Option<Value>>,
    cache: HashMap<u32, Value>,
}

impl<'a> Interp<'a> {
    fn run(mut self) -> Result<Outcome, Unsupported> {
        match self.call("main", &[]) {
            Ok(v) => Ok(Outcome {
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

    fn call(&mut self, name: &str, args: &[Value]) -> Step<Value> {
        let cfg = self
            .find_cfg(name)
            .ok_or_else(|| Flow::Unsupported(Unsupported(format!("call to '{name}'"))))?;
        let mut frame = Frame {
            params: args.to_vec(),
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
        };

        let mut current = cfg.entry;
        let mut incoming: Vec<Value> = Vec::new();
        // A generous step budget so a mis-modeled loop reports instead of hanging.
        let mut budget: u64 = 50_000_000;

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
                budget = budget.checked_sub(1).ok_or_else(|| {
                    Flow::Unsupported(Unsupported("step budget exhausted".into()))
                })?;
                self.eval(cfg, &mut frame, v)?;
            }

            let term = block.terminator;
            match term {
                Terminator::Return { value } => {
                    return match value {
                        Some(v) => self.eval(cfg, &mut frame, v),
                        None => Ok(Value::Unit),
                    };
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
                    let s = self.eval(cfg, &mut frame, scrutinee)?.as_int();
                    let cases = cfg.get_switch_cases(cases_start, cases_len);
                    current = cases
                        .iter()
                        .find(|(val, _)| *val as i128 == s)
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
            CfgInstData::Const(n) => Value::Int(*n as i128),
            CfgInstData::BoolConst(b) => Value::Bool(*b),
            CfgInstData::Param { index } => frame
                .params
                .get(*index as usize)
                .cloned()
                .ok_or_else(|| Flow::Unsupported(Unsupported("param index".into())))?,
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
            CfgInstData::PlaceRead { place } if place.is_simple() => match place.base {
                PlaceBase::Local(slot) => Self::get_local(frame, slot)?,
                PlaceBase::Param(slot) => frame
                    .params
                    .get(slot as usize)
                    .cloned()
                    .ok_or_else(|| Flow::Unsupported(Unsupported("param place".into())))?,
            },
            CfgInstData::PlaceWrite { place, value } if place.is_simple() => {
                let val = self.eval(cfg, frame, *value)?;
                match place.base {
                    PlaceBase::Local(slot) => Self::set_local(frame, slot, val),
                    PlaceBase::Param(_) => {
                        return Err(Flow::Unsupported(Unsupported(
                            "write to param place".into(),
                        )));
                    }
                }
                Value::Unit
            }

            CfgInstData::Call {
                name,
                args_start,
                args_len,
            } => {
                let fname = self.interner().resolve(name).to_string();
                let call_args = cfg.get_call_args(*args_start, *args_len).to_vec();
                let mut argvals = Vec::with_capacity(call_args.len());
                for a in &call_args {
                    if !matches!(a.mode, CfgArgMode::Normal) {
                        return Err(Flow::Unsupported(Unsupported(
                            "by-reference argument (inout/borrow)".into(),
                        )));
                    }
                    argvals.push(self.eval(cfg, frame, a.value)?);
                }
                self.call(&fname, &argvals)?
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

            CfgInstData::Drop { .. }
            | CfgInstData::StorageLive { .. }
            | CfgInstData::StorageDead { .. } => Value::Unit,

            other => {
                return Err(Flow::Unsupported(Unsupported(format!(
                    "instruction {other:?}"
                ))));
            }
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
        let x = self.eval(cfg, frame, a)?.as_int();
        let y = self.eval(cfg, frame, b)?.as_int();
        Ok(Value::Bool(pick(x.cmp(&y))))
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
