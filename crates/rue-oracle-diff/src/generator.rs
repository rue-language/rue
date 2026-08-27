//! Seed-driven generator of **valid, well-typed** Rue programs in the subset the
//! [`rue_oracle`] reference interpreter models (RUE-247).
//!
//! Everything here is deterministic from a `u64` seed — no clock, no `rand` — so
//! a disagreement found by the differential fuzzer (`--fuzz` mode of
//! `rue-oracle-diff`) reproduces exactly from its seed. Programs are constructed
//! to be well-typed *by construction*: every expression is generated to an exact
//! required type, every literal is emitted inside a typed context (a `let`
//! annotation, a struct-field position, or a same-typed sibling), and aggregate
//! values are never used after a by-value move. A generated program therefore
//! compiles cleanly and stays inside the oracle's coverage. A compile failure or
//! `Unsupported` result is therefore a generator-contract finding, not noise or
//! a coverage skip.
//!
//! The generator biases toward the historically-fragile shapes that every
//! 2026-07 miscompile lived in: aggregates crossing the call ABI (struct
//! by-value parameter + return), nested struct projections, mixed-width struct
//! fields (multi-slot layout), arithmetic with trapping overflow, `@intCast`
//! range checks, match/enum, and recursion.

/// A `splitmix64` PRNG — tiny, fast, and fully determined by its seed.
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, n)` (returns 0 when `n == 0`).
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        let i = self.below(xs.len() as u64) as usize;
        &xs[i]
    }
}

/// The scalar types the generator uses. Aggregates (structs/arrays/enums/String)
/// are handled by dedicated snippet emitters, not by the scalar-expression core.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,
}

const INT_TYPES: [Ty; 8] = [
    Ty::I8,
    Ty::I16,
    Ty::I32,
    Ty::I64,
    Ty::U8,
    Ty::U16,
    Ty::U32,
    Ty::U64,
];

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::I8 => "i8",
            Ty::I16 => "i16",
            Ty::I32 => "i32",
            Ty::I64 => "i64",
            Ty::U8 => "u8",
            Ty::U16 => "u16",
            Ty::U32 => "u32",
            Ty::U64 => "u64",
            Ty::Bool => "bool",
        }
    }

    fn is_signed(self) -> bool {
        matches!(self, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64)
    }

    /// Inclusive `[min, max]` literal range for an integer type. Emitting only
    /// in-range literals keeps generated programs compiling (an out-of-range
    /// literal is a *compile* error, whereas arithmetic overflow at runtime is a
    /// trap both engines agree on).
    fn int_range(self) -> (i128, i128) {
        match self {
            Ty::I8 => (i8::MIN as i128, i8::MAX as i128),
            Ty::I16 => (i16::MIN as i128, i16::MAX as i128),
            Ty::I32 => (i32::MIN as i128, i32::MAX as i128),
            Ty::I64 => (i64::MIN as i128, i64::MAX as i128),
            Ty::U8 => (0, u8::MAX as i128),
            Ty::U16 => (0, u16::MAX as i128),
            Ty::U32 => (0, u32::MAX as i128),
            Ty::U64 => (0, u64::MAX as i128),
            Ty::Bool => (0, 1),
        }
    }
}

/// A scalar variable in scope, tracked so leaves are always a typed variable
/// (never an inference-ambiguous bare literal).
#[derive(Clone)]
struct Var {
    name: String,
    ty: Ty,
}

#[derive(Default, Clone)]
struct Scope {
    vars: Vec<Var>,
}

impl Scope {
    fn of(&self, ty: Ty) -> Vec<&Var> {
        self.vars.iter().filter(|v| v.ty == ty).collect()
    }
    fn push(&mut self, name: String, ty: Ty) {
        self.vars.push(Var { name, ty });
    }
}

/// A generated struct type: `f0` is always `i32` (so a helper can return it
/// without a cast), the remaining fields are mixed-width ints or a nested struct
/// — the multi-slot / nested-projection shapes that broke codegen historically.
struct StructDef {
    name: String,
    /// Scalar fields (name, type).
    scalar_fields: Vec<(String, Ty)>,
    /// Optional single nested-struct field (name, index into `structs`).
    nested: Option<(String, usize)>,
    /// Whether this struct has a `drop fn` (exercises drop order/exactly-once).
    has_drop: bool,
}

/// A generated scalar helper `fn(a: T, b: T) -> T`, used to cross the call ABI
/// with scalar arguments and recursion.
struct HelperSig {
    name: String,
    param_ty: Ty,
    ret_ty: Ty,
}

macro_rules! define_generated_shapes {
    ($($shape:ident),+ $(,)?) => {
        /// A source shape whose presence is part of the generated smoke corpus's
        /// coverage contract.
        ///
        /// Shapes are recorded only when their syntax is actually emitted. In
        /// particular, a randomly selected snippet that cannot run because its
        /// helper or top-level item was not generated does not count toward
        /// coverage.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u8)]
        pub enum GeneratedShape {
            $($shape),+
        }

        impl GeneratedShape {
            #[cfg(test)]
            pub const ALL: [Self; Self::COUNT] = [$(Self::$shape),+];
            const COUNT: usize = [$(define_generated_shapes!(@unit $shape)),+].len();
        }
    };
    (@unit $shape:ident) => { () };
}

// Keep the enum, exhaustive iteration list, and backing-array width generated
// from one declaration so a new required shape cannot silently escape the
// smoke-window contract.
define_generated_shapes!(
    ScalarExpressionLet,
    IntCast,
    StructCallAndReturn,
    NestedProjection,
    ArrayIndex,
    IntegerMatch,
    PlainEnumMatch,
    PayloadEnumMatch,
    InoutCall,
    RecursionCall,
    Loop,
    String,
    StructEquality,
    ArrayEquality,
    EnumEquality,
    BoolEquality,
    ScalarHelperCall,
    BoolAnd,
    BoolOr,
    BoolNot,
    ComptimePositionTwin,
    ComptimeTwinArithmetic,
    ComptimeTwinComparison,
    ComptimeTwinLogical,
    ComptimeTwinBitwise,
    ComptimeTwinShiftLeft,
    ComptimeTwinShiftRight,
    ComptimeTwinNegation,
    ComptimeTwinI8,
    ComptimeTwinI16,
    ComptimeTwinI32,
    ComptimeTwinI64,
    ComptimeTwinU8,
    ComptimeTwinU16,
    ComptimeTwinU32,
    ComptimeTwinU64,
    ComptimeTwinBool,
    ExtraDbg,
);

/// Occurrence counts for the generated source shapes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapeCounts {
    counts: [u32; GeneratedShape::COUNT],
}

impl Default for ShapeCounts {
    fn default() -> Self {
        Self {
            counts: [0; GeneratedShape::COUNT],
        }
    }
}

impl ShapeCounts {
    /// Number of times `shape` was emitted into the generated program.
    #[cfg(test)]
    pub fn get(&self, shape: GeneratedShape) -> u32 {
        self.counts[shape as usize]
    }

    /// Iterate over every known shape and its occurrence count.
    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = (GeneratedShape, u32)> + '_ {
        GeneratedShape::ALL
            .into_iter()
            .map(|shape| (shape, self.get(shape)))
    }

    fn record(&mut self, shape: GeneratedShape) {
        self.counts[shape as usize] += 1;
    }
}

/// Generated Rue source together with the shapes actually emitted into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedProgram {
    pub source: String,
    pub shapes: ShapeCounts,
}

pub struct Program {
    seed: u64,
    rng: Rng,
    counter: usize,
    structs: Vec<StructDef>,
    helpers: Vec<HelperSig>,
    /// Whether a recursive helper `rec0` was emitted.
    has_rec: bool,
    /// Whether the scalar in/out helper `bump0` was emitted.
    has_bump: bool,
    /// Number of generated enums (variants are always 3: `V0`,`V1`,`V2`).
    enums: usize,
    /// Whether `E0` carries Copy payloads on `V1`/`V2`.
    enum_payload: bool,
    top_level: Vec<String>,
    shapes: ShapeCounts,
}

/// Generate a complete, well-typed Rue program from `seed`.
pub fn generate(seed: u64) -> String {
    generate_with_shapes(seed).source
}

/// Generate source and report the coverage shapes actually emitted for it.
pub fn generate_with_shapes(seed: u64) -> GeneratedProgram {
    let mut p = Program {
        seed,
        rng: Rng::new(seed),
        counter: 0,
        structs: Vec::new(),
        helpers: Vec::new(),
        has_rec: false,
        has_bump: false,
        enums: 0,
        enum_payload: false,
        top_level: Vec::new(),
        shapes: ShapeCounts::default(),
    };
    let source = p.build();
    GeneratedProgram {
        source,
        shapes: p.shapes,
    }
}

impl Program {
    fn fresh(&mut self, prefix: &str) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("{prefix}{n}")
    }

    fn int_literal(&mut self, ty: Ty) -> String {
        let (lo, hi) = ty.int_range();
        // Bias toward boundary values (where width bugs hide) and small values.
        let v: i128 = match self.rng.below(6) {
            0 => lo,
            1 => hi,
            2 => hi - 1,
            3 => 0,
            4 => 1,
            _ => self.rng.below(64) as i128,
        };
        let v = v.clamp(lo, hi);
        v.to_string()
    }

    fn bool_literal(&mut self) -> &'static str {
        if self.rng.chance(1, 2) {
            "true"
        } else {
            "false"
        }
    }

    /// A leaf of exact type `ty`: a variable if one is in scope, else a literal
    /// (only reached for the seed `let`s, which carry an explicit annotation).
    fn leaf(&mut self, ty: Ty, scope: &Scope) -> String {
        let candidates = scope.of(ty);
        if !candidates.is_empty() {
            return self.rng.pick(&candidates).name.clone();
        }
        if ty == Ty::Bool {
            self.bool_literal().to_string()
        } else {
            self.int_literal(ty)
        }
    }

    /// A well-typed expression of exact type `ty`.
    fn expr(&mut self, ty: Ty, scope: &Scope, depth: u32) -> String {
        if depth == 0 || self.rng.chance(1, 3) {
            return self.leaf(ty, scope);
        }
        if ty == Ty::Bool {
            return self.bool_expr(scope, depth);
        }
        self.int_expr(ty, scope, depth)
    }

    fn bool_expr(&mut self, scope: &Scope, depth: u32) -> String {
        if depth == 0 {
            return self.leaf(Ty::Bool, scope);
        }
        match self.rng.below(5) {
            0 => {
                // Comparison of two same-typed ints. Only compare a type that
                // has a variable in scope, so both operands are typed
                // variables — comparing bare literals (`0 != 4294967295`) has no
                // inference context and would default to `i32` (out of range).
                let usable: Vec<Ty> = INT_TYPES
                    .iter()
                    .copied()
                    .filter(|t| !scope.of(*t).is_empty())
                    .collect();
                if usable.is_empty() {
                    return self.leaf(Ty::Bool, scope);
                }
                let it = *self.rng.pick(&usable);
                let cmp = *self.rng.pick(&["==", "!=", "<", ">", "<=", ">="]);
                let l = self.expr(it, scope, depth - 1);
                let r = self.expr(it, scope, depth - 1);
                format!("({l} {cmp} {r})")
            }
            1 => {
                // Bool equality is a distinct structural-equality path from
                // integer comparison, and RUE-357 specifically wants it in the
                // generator mix.
                if scope.of(Ty::Bool).is_empty() {
                    return self.leaf(Ty::Bool, scope);
                }
                let cmp = *self.rng.pick(&["==", "!="]);
                let l = self.expr(Ty::Bool, scope, depth - 1);
                let r = self.expr(Ty::Bool, scope, depth - 1);
                format!("({l} {cmp} {r})")
            }
            2 => {
                let op = *self.rng.pick(&["&&", "||"]);
                let l = self.expr(Ty::Bool, scope, depth - 1);
                let r = self.expr(Ty::Bool, scope, depth - 1);
                self.shapes.record(if op == "&&" {
                    GeneratedShape::BoolAnd
                } else {
                    GeneratedShape::BoolOr
                });
                format!("({l} {op} {r})")
            }
            3 => {
                let inner = self.bool_expr(scope, depth - 1);
                self.shapes.record(GeneratedShape::BoolNot);
                format!("(!{inner})")
            }
            _ => self.leaf(Ty::Bool, scope),
        }
    }

    fn int_expr(&mut self, ty: Ty, scope: &Scope, depth: u32) -> String {
        match self.rng.below(8) {
            0 => {
                let op = *self.rng.pick(&["+", "-", "*", "&", "|", "^"]);
                let l = self.expr(ty, scope, depth - 1);
                let r = self.expr(ty, scope, depth - 1);
                format!("({l} {op} {r})")
            }
            1 => {
                // Division / remainder — guard the divisor nonzero with `| 1`
                // (keeps type `ty`, is always odd hence never 0). Overflow cases
                // like `i32::MIN % -1` still trap, and both engines agree.
                let op = *self.rng.pick(&["/", "%"]);
                let l = self.expr(ty, scope, depth - 1);
                let r = self.expr(ty, scope, depth - 1);
                format!("({l} {op} ({r} | 1))")
            }
            2 => {
                // Shift by a same-typed amount; the shift count is masked modulo
                // the operand width (spec 4.3a, formal core (D-Shl)/(D-Shr)) in
                // both engines, so an over-shift is a defined value and never
                // traps — no divisor-style guard is needed.
                let op = *self.rng.pick(&["<<", ">>"]);
                let l = self.expr(ty, scope, depth - 1);
                let r = self.expr(ty, scope, depth - 1);
                format!("({l} {op} {r})")
            }
            3 if ty.is_signed() => {
                let inner = self.expr(ty, scope, depth - 1);
                format!("(0 - {inner})")
            }
            4 => {
                let inner = self.expr(ty, scope, depth - 1);
                format!("(~{inner})")
            }
            5 => {
                let cond = self.bool_expr(scope, depth - 1);
                let then = self.expr(ty, scope, depth - 1);
                let els = self.expr(ty, scope, depth - 1);
                format!("(if {cond} {{ {then} }} else {{ {els} }})")
            }
            6 => {
                // Call a scalar helper returning `ty`, if one exists.
                let matches: Vec<(String, Ty)> = self
                    .helpers
                    .iter()
                    .filter(|h| h.ret_ty == ty)
                    .map(|h| (h.name.clone(), h.param_ty))
                    .collect();
                if let Some((name, pty)) = matches.first().cloned() {
                    let a = self.expr(pty, scope, depth - 1);
                    let b = self.expr(pty, scope, depth - 1);
                    self.shapes.record(GeneratedShape::ScalarHelperCall);
                    format!("{name}({a}, {b})")
                } else {
                    self.leaf(ty, scope)
                }
            }
            _ => self.leaf(ty, scope),
        }
    }

    // ---- top-level item emitters ------------------------------------------

    fn emit_structs(&mut self) {
        let count = self.rng.below(3); // 0..=2
        for _ in 0..count {
            let idx = self.structs.len();
            let name = format!("S{idx}");
            // f0 is always i32 (returnable without cast).
            let mut scalar_fields = vec![("f0".to_string(), Ty::I32)];
            let extra = 1 + self.rng.below(3); // 1..=3 more scalar fields
            for fi in 0..extra {
                let ty = *self.rng.pick(&INT_TYPES);
                scalar_fields.push((format!("f{}", fi + 1), ty));
            }
            // Optionally nest a previously-defined struct (nested projection).
            let nested = if idx > 0 && self.rng.chance(1, 3) {
                Some(("inner".to_string(), self.rng.below(idx as u64) as usize))
            } else {
                None
            };
            let has_drop = self.rng.chance(1, 3);
            self.structs.push(StructDef {
                name: name.clone(),
                scalar_fields,
                nested,
                has_drop,
            });
        }
        // Emit definitions and a by-value `pass`/`make`/`borrow` helper per struct.
        for i in 0..self.structs.len() {
            let def_src = self.struct_def_src(i);
            self.top_level.push(def_src);
            if self.structs[i].has_drop {
                let sname = self.structs[i].name.clone();
                self.top_level
                    .push(format!("drop fn {sname}(self) {{ @dbg(self.f0); }}"));
            }
            let name = self.structs[i].name.clone();
            // by-value in, i32 out — the ABI-crossing shape.
            self.top_level
                .push(format!("fn pass{name}(s: {name}) -> i32 {{ s.f0 }}"));
            // borrow in, i32 out.
            self.top_level.push(format!(
                "fn borrow{name}(borrow s: {name}) -> i32 {{ s.f0 }}"
            ));
            // by-value out (return an aggregate).
            let ctor = self.struct_ctor_src(i);
            self.top_level
                .push(format!("fn make{name}() -> {name} {{ {ctor} }}"));
        }
    }

    fn struct_def_src(&self, i: usize) -> String {
        let def = &self.structs[i];
        let mut fields: Vec<String> = def
            .scalar_fields
            .iter()
            .map(|(n, t)| format!("    {}: {},", n, t.name()))
            .collect();
        if let Some((fname, target)) = &def.nested {
            fields.push(format!("    {}: {},", fname, self.structs[*target].name));
        }
        format!("struct {} {{\n{}\n}}", def.name, fields.join("\n"))
    }

    /// A constructor expression for struct `i` with all-literal leaves (used in
    /// `make`/inline contexts that have no scope). Recurses into nested structs.
    fn struct_ctor_src(&mut self, i: usize) -> String {
        let plan: Vec<(String, Ty)> = self.structs[i].scalar_fields.clone();
        let nested = self.structs[i].nested.clone();
        let mut parts = Vec::new();
        for (fname, fty) in plan {
            let val = if fty == Ty::Bool {
                self.bool_literal().to_string()
            } else {
                self.int_literal(fty)
            };
            parts.push(format!("{fname}: {val}"));
        }
        if let Some((fname, target)) = nested {
            let inner = self.struct_ctor_src(target);
            parts.push(format!("{fname}: {inner}"));
        }
        format!("{} {{ {} }}", self.structs[i].name, parts.join(", "))
    }

    fn emit_helpers(&mut self) {
        let count = self.rng.below(3); // 0..=2 scalar helpers
        for _ in 0..count {
            let ty = *self.rng.pick(&INT_TYPES);
            let name = self.fresh("h");
            let mut scope = Scope::default();
            scope.push("a".to_string(), ty);
            scope.push("b".to_string(), ty);
            let body = self.expr(ty, &scope, 2);
            self.top_level.push(format!(
                "fn {name}(a: {t}, b: {t}) -> {t} {{ {body} }}",
                t = ty.name()
            ));
            self.helpers.push(HelperSig {
                name,
                param_ty: ty,
                ret_ty: ty,
            });
        }
        if self.rng.chance(1, 2) {
            self.top_level.push(
                "fn rec0(n: i32, acc: i32) -> i32 { if n <= 0 { acc } else { rec0(n - 1, acc + n) } }"
                    .to_string(),
            );
            self.has_rec = true;
        }
        if self.rng.chance(1, 2) {
            self.top_level
                .push("fn bump0(inout x: i32) { x = x + 1; }".to_string());
            self.has_bump = true;
        }
    }

    fn emit_enums(&mut self) {
        if self.rng.chance(1, 2) {
            if self.rng.chance(1, 2) {
                self.top_level
                    .push("enum E0 { V0, V1(i32), V2(bool) }".to_string());
                self.enum_payload = true;
            } else {
                self.top_level.push("enum E0 { V0, V1, V2 }".to_string());
            }
            self.enums = 1;
        }
    }

    fn enum_ctor_src(&mut self) -> String {
        if self.enum_payload {
            match self.rng.below(3) {
                0 => "E0.V0".to_string(),
                1 => {
                    let v = self.int_literal(Ty::I32);
                    format!("E0.V1({v})")
                }
                _ => {
                    let v = self.bool_literal();
                    format!("E0.V2({v})")
                }
            }
        } else {
            format!("E0.V{}", self.rng.below(3))
        }
    }

    // ---- main-body snippet emitters ---------------------------------------

    /// Seed 1-2 variables of each integer type plus a couple of bools, so every
    /// expression leaf can be a typed variable.
    fn seed_vars(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        for &ty in &INT_TYPES {
            let n = 1 + self.rng.below(2);
            for _ in 0..n {
                let name = self.fresh("v");
                let lit = self.int_literal(ty);
                body.push(format!("    let {name}: {t} = {lit};", t = ty.name()));
                scope.push(name, ty);
            }
        }
        for _ in 0..2 {
            let name = self.fresh("b");
            let lit = self.bool_literal();
            body.push(format!("    let {name}: bool = {lit};"));
            scope.push(name, Ty::Bool);
        }
    }

    /// A `let vN: T = <expr>;` statement, registering the new variable.
    fn snippet_let(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        let ty = if self.rng.chance(1, 4) {
            Ty::Bool
        } else {
            *self.rng.pick(&INT_TYPES)
        };
        let name = self.fresh("v");
        let e = self.expr(ty, scope, 3);
        body.push(format!("    let {name}: {t} = {e};", t = ty.name()));
        scope.push(name, ty);
        self.shapes.record(GeneratedShape::ScalarExpressionLet);
    }

    /// `let vN: T = @intCast(<src var>);` — exercises the range-checked cast.
    fn snippet_intcast(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        let dst = *self.rng.pick(&INT_TYPES);
        // Cast *from* a variable of some int type.
        let src_candidates: Vec<Var> = scope
            .vars
            .iter()
            .filter(|v| v.ty != Ty::Bool)
            .cloned()
            .collect();
        if src_candidates.is_empty() {
            return;
        }
        let src = self.rng.pick(&src_candidates).clone();
        let name = self.fresh("v");
        body.push(format!(
            "    let {name}: {t} = @intCast({s});",
            t = dst.name(),
            s = src.name
        ));
        scope.push(name, dst);
        self.shapes.record(GeneratedShape::IntCast);
    }

    fn snippet_dbg(&mut self, body: &mut Vec<String>, scope: &Scope) -> bool {
        // @dbg only supports int/bool (and String, handled separately).
        if scope.vars.is_empty() {
            return false;
        }
        let names: Vec<String> = scope.vars.iter().map(|v| v.name.clone()).collect();
        let name = self.rng.pick(&names).clone();
        body.push(format!("    @dbg({name});"));
        true
    }

    fn snippet_struct(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        if self.structs.is_empty() {
            return;
        }
        let i = self.rng.below(self.structs.len() as u64) as usize;
        let name = self.structs[i].name.clone();
        let pname = self.fresh("p");
        // Bind a value: either freshly constructed or returned by `make` (ABI).
        let called_make = if self.rng.chance(1, 2) {
            let ctor = self.struct_ctor_src(i);
            body.push(format!("    let {pname}: {name} = {ctor};"));
            false
        } else {
            body.push(format!("    let {pname} = make{name}();"));
            true
        };
        // Read scalar fields into typed vars (scalar copies; struct still owned).
        let fields: Vec<(String, Ty)> = self.structs[i].scalar_fields.clone();
        for (fname, fty) in &fields {
            if self.rng.chance(1, 2) {
                let vn = self.fresh("v");
                body.push(format!(
                    "    let {vn}: {t} = {pname}.{fname};",
                    t = fty.name()
                ));
                scope.push(vn, *fty);
            }
        }
        // Nested projection read.
        if let Some((fname, _target)) = self.structs[i].nested.clone() {
            let vn = self.fresh("v");
            body.push(format!("    let {vn}: i32 = {pname}.{fname}.f0;"));
            scope.push(vn, Ty::I32);
            self.shapes.record(GeneratedShape::NestedProjection);
        }
        // borrow (does not move) — always safe on the bound value.
        let vb = self.fresh("v");
        body.push(format!("    let {vb}: i32 = borrow{name}(borrow {pname});"));
        scope.push(vb, Ty::I32);
        // by-value pass: only via a fresh inline temp (never move the bound
        // `pname`, which must survive to its scope-exit drop when droppable).
        let vp = self.fresh("v");
        let ctor = self.struct_ctor_src(i);
        body.push(format!("    let {vp}: i32 = pass{name}({ctor});"));
        scope.push(vp, Ty::I32);
        if called_make {
            self.shapes.record(GeneratedShape::StructCallAndReturn);
        }
    }

    fn snippet_array(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        let elems: Vec<String> = (0..3).map(|_| self.int_literal(Ty::I32)).collect();
        let aname = self.fresh("a");
        body.push(format!(
            "    let {aname}: [i32; 3] = [{}];",
            elems.join(", ")
        ));
        // Index — in-bounds usually; occasionally out of bounds (both trap).
        let idx = if self.rng.chance(1, 8) {
            3 + self.rng.below(3)
        } else {
            self.rng.below(3)
        };
        let iname = self.fresh("ix");
        body.push(format!("    let {iname}: usize = {idx};"));
        let vn = self.fresh("v");
        body.push(format!("    let {vn}: i32 = {aname}[{iname}];"));
        scope.push(vn, Ty::I32);
        self.shapes.record(GeneratedShape::ArrayIndex);
    }

    fn snippet_match_int(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        let scr = self.leaf(Ty::I32, scope);
        let a = self.expr(Ty::I32, scope, 1);
        let b = self.expr(Ty::I32, scope, 1);
        let vn = self.fresh("v");
        body.push(format!(
            "    let {vn}: i32 = match ({scr} & 1) {{ 0 => {a}, _ => {b} }};"
        ));
        scope.push(vn, Ty::I32);
        self.shapes.record(GeneratedShape::IntegerMatch);
    }

    fn snippet_match_enum(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        if self.enums == 0 {
            return;
        }
        let ename = self.fresh("e");
        let ctor = self.enum_ctor_src();
        body.push(format!("    let {ename}: E0 = {ctor};"));
        let a = self.expr(Ty::I32, scope, 1);
        let b = self.expr(Ty::I32, scope, 1);
        let c = self.expr(Ty::I32, scope, 1);
        let vn = self.fresh("v");
        if self.enum_payload {
            body.push(format!(
                "    let {vn}: i32 = match {ename} {{ E0.V0 => {a}, E0.V1(x) => (x + {b}), E0.V2(flag) => if flag {{ {c} }} else {{ {a} }} }};"
            ));
            self.shapes.record(GeneratedShape::PayloadEnumMatch);
        } else {
            body.push(format!(
                "    let {vn}: i32 = match {ename} {{ E0.V0 => {a}, E0.V1 => {b}, E0.V2 => {c} }};"
            ));
            self.shapes.record(GeneratedShape::PlainEnumMatch);
        }
        scope.push(vn, Ty::I32);
    }

    fn snippet_equality(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        match self.rng.below(4) {
            0 if !self.structs.is_empty() => {
                let i = self.rng.below(self.structs.len() as u64) as usize;
                let sname = self.structs[i].name.clone();
                let left = self.fresh("p");
                let right = self.fresh("p");
                let a = self.struct_ctor_src(i);
                let b = self.struct_ctor_src(i);
                body.push(format!("    let {left}: {sname} = {a};"));
                body.push(format!("    let {right}: {sname} = {b};"));
                let vn = self.fresh("b");
                let cmp = *self.rng.pick(&["==", "!="]);
                body.push(format!("    let {vn}: bool = ({left} {cmp} {right});"));
                scope.push(vn, Ty::Bool);
                self.shapes.record(GeneratedShape::StructEquality);
            }
            1 => {
                let left = self.fresh("a");
                let right = self.fresh("a");
                let xs: Vec<String> = (0..3).map(|_| self.int_literal(Ty::I32)).collect();
                let ys: Vec<String> = (0..3).map(|_| self.int_literal(Ty::I32)).collect();
                body.push(format!("    let {left}: [i32; 3] = [{}];", xs.join(", ")));
                body.push(format!("    let {right}: [i32; 3] = [{}];", ys.join(", ")));
                let vn = self.fresh("b");
                let cmp = *self.rng.pick(&["==", "!="]);
                body.push(format!("    let {vn}: bool = ({left} {cmp} {right});"));
                scope.push(vn, Ty::Bool);
                self.shapes.record(GeneratedShape::ArrayEquality);
            }
            2 if self.enums != 0 => {
                let left = self.fresh("e");
                let right = self.fresh("e");
                let a = self.enum_ctor_src();
                let b = self.enum_ctor_src();
                body.push(format!("    let {left}: E0 = {a};"));
                body.push(format!("    let {right}: E0 = {b};"));
                let vn = self.fresh("b");
                let cmp = *self.rng.pick(&["==", "!="]);
                body.push(format!("    let {vn}: bool = ({left} {cmp} {right});"));
                scope.push(vn, Ty::Bool);
                self.shapes.record(GeneratedShape::EnumEquality);
            }
            _ => {
                let l = self.expr(Ty::Bool, scope, 2);
                let r = self.expr(Ty::Bool, scope, 2);
                let vn = self.fresh("b");
                let cmp = *self.rng.pick(&["==", "!="]);
                body.push(format!("    let {vn}: bool = ({l} {cmp} {r});"));
                scope.push(vn, Ty::Bool);
                self.shapes.record(GeneratedShape::BoolEquality);
            }
        }
    }

    fn snippet_inout(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        if !self.has_bump {
            return;
        }
        let mname = self.fresh("m");
        let lit = self.int_literal(Ty::I32);
        body.push(format!("    let mut {mname}: i32 = {lit};"));
        body.push(format!("    bump0(inout {mname});"));
        scope.push(mname, Ty::I32);
        self.shapes.record(GeneratedShape::InoutCall);
    }

    fn snippet_recursion(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        if !self.has_rec {
            return;
        }
        let n = self.rng.below(12); // small, guarantees termination
        let vn = self.fresh("v");
        body.push(format!("    let {vn}: i32 = rec0({n}, 0);"));
        scope.push(vn, Ty::I32);
        self.shapes.record(GeneratedShape::RecursionCall);
    }

    fn snippet_loop(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        // A bounded counted loop; the constant bound guarantees termination.
        let bound = 1 + self.rng.below(6);
        let acc = self.fresh("v");
        let ctr = self.fresh("ix");
        body.push(format!("    let mut {acc}: i32 = 0;"));
        body.push(format!("    let mut {ctr}: i32 = 0;"));
        body.push(format!(
            "    loop {{ if {ctr} >= {bound} {{ break; }} {acc} = {acc} + {ctr}; {ctr} = {ctr} + 1; }}"
        ));
        scope.push(acc, Ty::I32);
        self.shapes.record(GeneratedShape::Loop);
    }

    fn snippet_string(&mut self, body: &mut Vec<String>, scope: &mut Scope) {
        let sname = self.fresh("s");
        let word = *self.rng.pick(&["foo", "bar", "baz", "hi", "x"]);
        body.push(format!("    let {sname}: str = \"{word}\";"));
        let ln = self.fresh("v");
        body.push(format!("    let {ln}: u64 = {sname}.len();"));
        scope.push(ln, Ty::U64);
        body.push(format!("    @dbg({sname});"));
        self.shapes.record(GeneratedShape::String);
    }

    /// Emit one expression in all three positions that use the independent
    /// constant-evaluation paths: a durable file-level const, a `comptime`
    /// block in the body, and a normal runtime call.  The runtime identity
    /// function is deliberately non-comptime, so its argument remains a
    /// runtime evaluation even though the expression has no runtime leaves.
    fn emit_position_twin(&mut self, body: &mut Vec<String>, ty: Ty, expr: &str) {
        let const_name = self.fresh("POSITION_CONST_");
        let comptime_name = self.fresh("position_comptime_");
        let runtime_name = self.fresh("position_runtime_");
        let identity_name = self.fresh("position_identity_");
        let type_name = ty.name();

        self.top_level
            .push(format!("const {const_name}: {type_name} = {expr};"));
        self.top_level.push(format!(
            "fn {identity_name}(x: {type_name}) -> {type_name} {{ x }}"
        ));
        body.push(format!(
            "    let {comptime_name}: {type_name} = comptime {{ {expr} }};"
        ));
        body.push(format!(
            "    let {runtime_name}: {type_name} = {identity_name}({expr});"
        ));
        body.push(format!(
            "    @assert(({const_name} == {comptime_name}) && ({const_name} == {runtime_name}));"
        ));
        self.shapes.record(GeneratedShape::ComptimePositionTwin);
    }

    /// A closed scalar subset of the comptime-evaluable set (4.14:26–29).
    /// Every operand is a typed-context literal or another value-forming
    /// operator; no runtime binding, call, intrinsic, branch, or cast can
    /// enter these expressions.  Small arithmetic operands avoid compile-time
    /// traps, while shifts intentionally use an amount at the operand width
    /// to exercise the specified masked-shift behavior on every integer type.
    fn emit_comptime_position_twins(&mut self, body: &mut Vec<String>) {
        // The first 48 seeds form a complete 8-width × 6-operator matrix for
        // `~`, `<<`, `>>`, `&`, `|`, and `^`; this is intentional coverage,
        // not a probabilistic bias.  The following slots cover arithmetic and
        // unary negation, with the ordinary generated expressions continuing
        // to provide deeper mixed trees.
        let schedule = self.seed % 60;
        let (ty, op) = if schedule < 48 {
            (
                INT_TYPES[(schedule / 6) as usize],
                schedule % 6, // not, shl, shr, and, or, xor
            )
        } else {
            let arithmetic = (schedule - 48) % 6;
            let ty = if arithmetic == 5 {
                Ty::I32
            } else {
                let reduced_seed = self.seed % INT_TYPES.len() as u64;
                INT_TYPES[(reduced_seed * 5 % INT_TYPES.len() as u64) as usize]
            };
            (ty, arithmetic + 6) // add, sub, mul, div, rem, neg
        };
        let (expr, shape) = match op {
            0 => ("(~1)".to_string(), GeneratedShape::ComptimeTwinBitwise),
            1 => {
                let width = self.integer_width(ty);
                (
                    format!("((1 << {width}) | (1 << {}))", width + 1),
                    GeneratedShape::ComptimeTwinShiftLeft,
                )
            }
            2 => {
                let width = self.integer_width(ty);
                let (operand, joiner) = if ty.is_signed() {
                    ("-2", "&")
                } else {
                    ("3", "|")
                };
                (
                    format!(
                        "(({operand} >> {width}) {joiner} ({operand} >> {}))",
                        width + 1
                    ),
                    GeneratedShape::ComptimeTwinShiftRight,
                )
            }
            3 => ("(1 & 3)".to_string(), GeneratedShape::ComptimeTwinBitwise),
            4 => ("(1 | 2)".to_string(), GeneratedShape::ComptimeTwinBitwise),
            5 => ("(3 ^ 1)".to_string(), GeneratedShape::ComptimeTwinBitwise),
            6 => (
                "(1 + 2)".to_string(),
                GeneratedShape::ComptimeTwinArithmetic,
            ),
            7 => (
                "(2 - 1)".to_string(),
                GeneratedShape::ComptimeTwinArithmetic,
            ),
            8 => (
                "(2 * 3)".to_string(),
                GeneratedShape::ComptimeTwinArithmetic,
            ),
            9 => (
                "(3 / 2)".to_string(),
                GeneratedShape::ComptimeTwinArithmetic,
            ),
            10 => (
                "(3 % 2)".to_string(),
                GeneratedShape::ComptimeTwinArithmetic,
            ),
            _ => ("(-1)".to_string(), GeneratedShape::ComptimeTwinNegation),
        };
        self.emit_position_twin(body, ty, &expr);
        self.shapes.record(shape);
        self.shapes.record(match ty {
            Ty::I8 => GeneratedShape::ComptimeTwinI8,
            Ty::I16 => GeneratedShape::ComptimeTwinI16,
            Ty::I32 => GeneratedShape::ComptimeTwinI32,
            Ty::I64 => GeneratedShape::ComptimeTwinI64,
            Ty::U8 => GeneratedShape::ComptimeTwinU8,
            Ty::U16 => GeneratedShape::ComptimeTwinU16,
            Ty::U32 => GeneratedShape::ComptimeTwinU32,
            Ty::U64 => GeneratedShape::ComptimeTwinU64,
            Ty::Bool => unreachable!(),
        });

        // Add a bool twin periodically.  It covers the remaining scalar
        // comptime operators while keeping every generated source bounded.
        if self.seed % 4 == 0 {
            let (bool_expr, bool_shape) = match (self.seed / 4) % 3 {
                0 => ("(true && false)", GeneratedShape::ComptimeTwinLogical),
                1 => ("(true == false)", GeneratedShape::ComptimeTwinComparison),
                _ => ("(!false)", GeneratedShape::ComptimeTwinLogical),
            };
            self.emit_position_twin(body, Ty::Bool, bool_expr);
            self.shapes.record(bool_shape);
            self.shapes.record(GeneratedShape::ComptimeTwinBool);
        }
    }

    fn integer_width(&self, ty: Ty) -> u32 {
        match ty {
            Ty::I8 | Ty::U8 => 8,
            Ty::I16 | Ty::U16 => 16,
            Ty::I32 | Ty::U32 => 32,
            Ty::I64 | Ty::U64 => 64,
            Ty::Bool => unreachable!(),
        }
    }

    fn build(&mut self) -> String {
        // Top-level items first (main refers to them; forward refs are fine).
        self.emit_structs();
        self.emit_enums();
        self.emit_helpers();

        let mut body: Vec<String> = Vec::new();
        let mut scope = Scope::default();
        self.seed_vars(&mut body, &mut scope);
        self.emit_comptime_position_twins(&mut body);

        // A random sequence of feature snippets.
        let steps = 4 + self.rng.below(8);
        for _ in 0..steps {
            match self.rng.below(12) {
                0 => self.snippet_let(&mut body, &mut scope),
                1 => self.snippet_intcast(&mut body, &mut scope),
                2 => self.snippet_struct(&mut body, &mut scope),
                3 => self.snippet_array(&mut body, &mut scope),
                4 => self.snippet_match_int(&mut body, &mut scope),
                5 => self.snippet_match_enum(&mut body, &mut scope),
                6 => self.snippet_inout(&mut body, &mut scope),
                7 => self.snippet_recursion(&mut body, &mut scope),
                8 => self.snippet_loop(&mut body, &mut scope),
                9 => self.snippet_string(&mut body, &mut scope),
                10 => self.snippet_equality(&mut body, &mut scope),
                _ => {
                    if self.snippet_dbg(&mut body, &scope) {
                        self.shapes.record(GeneratedShape::ExtraDbg);
                    }
                }
            }
        }

        // A couple of @dbg calls for stdout coverage, then return an i32.
        self.snippet_dbg(&mut body, &scope);
        let ret = self.expr(Ty::I32, &scope, 2);
        body.push(format!("    return {ret};"));

        let mut out = String::new();
        for item in &self.top_level {
            out.push_str(item);
            out.push_str("\n\n");
        }
        out.push_str("fn main() -> i32 {\n");
        out.push_str(&body.join("\n"));
        out.push_str("\n}\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_compiler::{CompileOptions, CompilerSession, SourceSnapshot};

    fn validate_semantics(source: &str) -> rue_compiler::MultiErrorResult<()> {
        let snapshot = SourceSnapshot::single("<generator>", source)
            .map_err(rue_compiler::CompileErrors::from)?;
        let mut session = CompilerSession::new();
        session.update(&snapshot).into_result()?;
        rue_compiler::unstable::rooted_cfg(&mut session, &CompileOptions::default()).map(drop)
    }

    const COMPILE_CONTRACT_SEEDS: u64 = 500;

    fn position_entries(source: &str) -> Vec<(String, String, String)> {
        let lines: Vec<&str> = source.lines().collect();
        let consts: Vec<(String, String)> = lines
            .iter()
            .filter(|line| line.starts_with("const POSITION_CONST_"))
            .map(|line| {
                let (name, value) = line.split_once(" = ").expect("position const initializer");
                (
                    name.strip_prefix("const ")
                        .expect("position const name")
                        .to_string(),
                    value
                        .strip_suffix(';')
                        .expect("position const terminator")
                        .to_string(),
                )
            })
            .collect();
        let comptimes: Vec<String> = lines
            .iter()
            .filter(|line| line.trim_start().starts_with("let position_comptime_"))
            .map(|line| {
                line.split_once("comptime { ")
                    .and_then(|(_, rest)| rest.strip_suffix(" };"))
                    .expect("position comptime expression")
                    .to_string()
            })
            .collect();
        assert_eq!(consts.len(), comptimes.len());

        consts
            .into_iter()
            .enumerate()
            .map(|(index, (const_name, const_expression))| {
                let const_line = lines
                    .iter()
                    .position(|line| line.starts_with(&format!("const {const_name}")))
                    .expect("position const line");
                let identity_line = lines[const_line + 1..]
                    .iter()
                    .copied()
                    .find(|line| line.starts_with("fn position_identity_"))
                    .expect("position identity line");
                let identity = identity_line
                    .strip_prefix("fn ")
                    .and_then(|line| line.split_once('('))
                    .map(|(name, _)| name)
                    .expect("position identity function");
                assert!(identity_line.contains("(x: ") && identity_line.ends_with("{ x }"));
                let runtime_expression = lines
                    .iter()
                    .find_map(|line| {
                        let (_, rest) = line.split_once(&format!("= {identity}("))?;
                        Some(rest.strip_suffix(");")?.to_string())
                    })
                    .expect("position runtime expression");
                (
                    const_expression,
                    comptimes[index].clone(),
                    runtime_expression,
                )
            })
            .collect()
    }

    #[test]
    fn deterministic_from_seed() {
        for seed in 0..50u64 {
            let generated = generate_with_shapes(seed);
            assert_eq!(
                generated,
                generate_with_shapes(seed),
                "seed {seed} not deterministic"
            );
            assert_eq!(generate(seed), generated.source);
        }
    }

    #[test]
    fn high_u64_seeds_remain_deterministic_and_panic_free() {
        for seed in [u64::MAX - 12, u64::MAX - 1, u64::MAX] {
            let first = generate_with_shapes(seed);
            assert_eq!(
                first,
                generate_with_shapes(seed),
                "seed {seed} not deterministic"
            );
            assert!(first.source.contains("fn main() -> i32 {"));
        }
    }

    #[test]
    fn always_has_main() {
        for seed in 0..200u64 {
            let src = generate(seed);
            assert!(src.contains("fn main() -> i32 {"), "seed {seed}: {src}");
        }
    }

    #[test]
    fn smoke_window_covers_every_required_shape() {
        let mut totals = ShapeCounts::default();

        for seed in 0..64u64 {
            let generated = generate_with_shapes(seed);
            for (shape, count) in generated.shapes.iter() {
                totals.counts[shape as usize] += count;
            }
        }

        let missing: Vec<GeneratedShape> = totals
            .iter()
            .filter_map(|(shape, count)| (count == 0).then_some(shape))
            .collect();
        assert!(
            missing.is_empty(),
            "generated smoke seeds 0..64 did not emit required shapes: {missing:?}"
        );
    }

    /// The generator is part of the compiler's correctness boundary: it
    /// promises valid, well-typed Rue programs. Compile a deterministic corpus
    /// through the shared front end so grammar, name-resolution, or typing
    /// drift is a failing test; generated fuzz mode also fails closed if the
    /// oracle reports `Unsupported`.
    #[test]
    fn generated_programs_compile() {
        let mut saw_string_method_call = false;

        for seed in 0..COMPILE_CONTRACT_SEEDS {
            let source = generate(seed);
            saw_string_method_call |= source.contains(": str =") && source.contains(".len()");
            if let Err(errors) = validate_semantics(&source) {
                panic!(
                    "generated seed {seed} did not compile: {errors:#?}\n\n--- source ---\n{source}"
                );
            }
        }

        assert!(
            saw_string_method_call,
            "compile-contract corpus did not exercise stable str method calls"
        );
    }

    #[test]
    fn position_twins_repeat_one_closed_expression_in_three_positions() {
        for seed in 0..65u64 {
            let source = generate(seed);
            let entries = position_entries(&source);
            assert!(!entries.is_empty(), "seed {seed} has no position twin");

            for (const_expression, comptime_expression, runtime_expression) in entries {
                assert!(
                    const_expression == comptime_expression
                        && const_expression == runtime_expression,
                    "seed {seed} did not repeat one expression: const={const_expression:?}, comptime={comptime_expression:?}, runtime={runtime_expression:?}"
                );
                assert!(
                    !const_expression.contains("if ")
                        && !const_expression.contains("match ")
                        && !const_expression.contains('@')
                        && !const_expression.contains("fn "),
                    "seed {seed} position twin escaped the closed comptime subset: {const_expression}"
                );
            }
        }
    }

    #[test]
    fn position_twin_smoke_covers_widths_and_operator_classes() {
        let mut totals = ShapeCounts::default();
        for seed in 0..65u64 {
            let generated = generate_with_shapes(seed);
            for (shape, count) in generated.shapes.iter() {
                totals.counts[shape as usize] += count;
            }
        }

        let required = [
            GeneratedShape::ComptimePositionTwin,
            GeneratedShape::ComptimeTwinArithmetic,
            GeneratedShape::ComptimeTwinComparison,
            GeneratedShape::ComptimeTwinLogical,
            GeneratedShape::ComptimeTwinBitwise,
            GeneratedShape::ComptimeTwinShiftLeft,
            GeneratedShape::ComptimeTwinShiftRight,
            GeneratedShape::ComptimeTwinNegation,
            GeneratedShape::ComptimeTwinI8,
            GeneratedShape::ComptimeTwinI16,
            GeneratedShape::ComptimeTwinI32,
            GeneratedShape::ComptimeTwinI64,
            GeneratedShape::ComptimeTwinU8,
            GeneratedShape::ComptimeTwinU16,
            GeneratedShape::ComptimeTwinU32,
            GeneratedShape::ComptimeTwinU64,
            GeneratedShape::ComptimeTwinBool,
        ];
        for shape in required {
            assert!(
                totals.get(shape) > 0,
                "position-twin smoke window did not emit {shape:?}"
            );
        }

        for ty in ["i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64"] {
            let source = (0..65u64).map(generate).collect::<Vec<_>>().join("\n");
            assert!(
                source.contains(&format!(": {ty} = (~1);")),
                "smoke window lacks bitwise-not twin for {ty}"
            );
            let width = match ty {
                "i8" | "u8" => 8,
                "i16" | "u16" => 16,
                "i32" | "u32" => 32,
                "i64" | "u64" => 64,
                _ => unreachable!(),
            };
            assert!(
                source.contains(&format!(
                    ": {ty} = ((1 << {width}) | (1 << {}));",
                    width + 1
                )),
                "smoke window lacks width-sensitive left shift for {ty}"
            );
            let right = if ty.starts_with('i') {
                format!(": {ty} = ((-2 >> {width}) & (-2 >> {}));", width + 1)
            } else {
                format!(": {ty} = ((3 >> {width}) | (3 >> {}));", width + 1)
            };
            assert!(
                source.contains(&right),
                "smoke window lacks width-sensitive right shift for {ty}"
            );
        }
    }

    #[test]
    fn position_twins_remain_inside_oracle_model() {
        for seed in 0..65u64 {
            let source = generate(seed);
            rue_oracle::run_source(&source)
                .unwrap_or_else(|error| panic!("seed {seed} left oracle model: {error:?}"));
        }
    }

    #[test]
    fn generates_payload_enum_cases() {
        let mut saw_payload_enum = false;
        let mut saw_payload_ctor = false;
        let mut saw_payload_match = false;

        for seed in 0..200u64 {
            let src = generate(seed);
            saw_payload_enum |= src.contains("enum E0 { V0, V1(i32), V2(bool) }");
            saw_payload_ctor |= src.contains("E0.V1(") || src.contains("E0.V2(");
            saw_payload_match |= src.contains("E0.V1(x)") && src.contains("E0.V2(flag)");
        }

        assert!(saw_payload_enum);
        assert!(saw_payload_ctor);
        assert!(saw_payload_match);
    }

    #[test]
    fn generates_structural_equality_cases() {
        let mut saw_bool_eq = false;
        let mut saw_array_eq = false;
        let mut saw_struct_eq = false;
        let mut saw_enum_eq = false;

        for seed in 0..500u64 {
            let src = generate(seed);
            saw_bool_eq |= src.contains(" == b") || src.contains(" != b");
            saw_array_eq |= src.contains(": [i32; 3]") && src.contains(" = (a");
            saw_struct_eq |= src.contains(" = (p");
            saw_enum_eq |= src.contains(" = (e");
        }

        assert!(saw_bool_eq);
        assert!(saw_array_eq);
        assert!(saw_struct_eq);
        assert!(saw_enum_eq);
    }
}
