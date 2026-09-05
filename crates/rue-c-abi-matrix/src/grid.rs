//! The generated C-boundary conformance grid: shapes x positions x directions.
//!
//! One [`Program`] is a complete pair of sources — one `.c` file and one
//! `.rue` file — covering every shape at every position for one direction and
//! one ABI spelling, plus the exact stdout the pair must produce. The expected
//! output is computed here, from the same values and multipliers the two
//! sources are emitted with, so a boundary that drops, swaps, truncates, or
//! sign-extends a field wrongly produces a different line.
//!
//! # What a cell asserts
//!
//! Every cell reduces its whole argument list to one `u64` checksum:
//!
//! ```text
//! acc = 0; for each contribution v:  acc = acc + v * multiplier
//! ```
//!
//! computed with wrapping arithmetic on both sides. Contributions are the
//! 64-bit patterns of the received values — every filler argument, and every
//! *leaf* of the shape argument separately — each with its own odd multiplier.
//! A swapped pair of fields, a truncated high half, a missing sign extension,
//! or a slot read from the wrong stack offset all change the sum.
//!
//! The `return` position inverts the crossing: the callee builds the shape and
//! the caller checksums it. The callee also receives a seed and returns a
//! deliberately different ("poisoned") value when the seed did not arrive
//! intact, so a broken argument crossing cannot hide behind a correct result.
//!
//! # Extending the table
//!
//! Floats are still rejected at Rue's C boundary. Adding them later is a table
//! edit: one [`Leaf`] variant with its two type spellings and its conversion
//! rules, and one [`SHAPES`] row per new shape. Nothing else in this module
//! knows the leaf inventory.

use rue_target::CConventionSpec;

/// Bytes the C side exposes through `c_probe`, the only source of pointer
/// values in the grid. A pointer's contribution is the byte it points at, not
/// its address, so a cell's expected value stays independent of where the
/// image happens to load.
pub const PROBE_BYTES: [u8; 8] = [17, 47, 125, 163, 197, 233, 4, 254];

/// One scalar type at the boundary, or one scalar leaf inside an aggregate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leaf {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    Bool,
    /// `ptr const u8` in Rue, `const rue_u8 *` in C.
    Ptr,
}

impl Leaf {
    pub fn rue_type(self) -> &'static str {
        match self {
            Leaf::I8 => "i8",
            Leaf::U8 => "u8",
            Leaf::I16 => "i16",
            Leaf::U16 => "u16",
            Leaf::I32 => "i32",
            Leaf::U32 => "u32",
            Leaf::I64 => "i64",
            Leaf::U64 => "u64",
            Leaf::Bool => "bool",
            Leaf::Ptr => "ptr const u8",
        }
    }

    /// The C spelling, always one of the data-model typedefs the generated
    /// prelude defines, so the C side names widths the way the target's data
    /// model does rather than assuming a fixed-width header exists.
    pub fn c_type(self) -> &'static str {
        match self {
            Leaf::I8 => "rue_i8",
            Leaf::U8 => "rue_u8",
            Leaf::I16 => "rue_i16",
            Leaf::U16 => "rue_u16",
            Leaf::I32 => "rue_i32",
            Leaf::U32 => "rue_u32",
            Leaf::I64 => "rue_i64",
            Leaf::U64 => "rue_u64",
            Leaf::Bool => "rue_bool",
            Leaf::Ptr => "const rue_u8 *",
        }
    }

    fn integer_width(self) -> Option<u32> {
        match self {
            Leaf::I8 | Leaf::U8 => Some(8),
            Leaf::I16 | Leaf::U16 => Some(16),
            Leaf::I32 | Leaf::U32 => Some(32),
            Leaf::I64 | Leaf::U64 => Some(64),
            Leaf::Bool | Leaf::Ptr => None,
        }
    }

    fn is_signed(self) -> bool {
        matches!(self, Leaf::I8 | Leaf::I16 | Leaf::I32 | Leaf::I64)
    }

    /// The suffix a C integer literal of this type needs so the literal's own
    /// type is the field's, rather than whatever `int` promotion would pick.
    fn c_literal_suffix(self) -> &'static str {
        match self {
            Leaf::I64 => "L",
            Leaf::U64 => "UL",
            Leaf::U8 | Leaf::U16 | Leaf::U32 => "U",
            _ => "",
        }
    }
}

/// A concrete value for one leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Int(i128),
    Bool(bool),
    /// An index into [`PROBE_BYTES`]; the pointer itself is `&c_probe_bytes[i]`.
    Ptr(u8),
}

impl Value {
    /// The 64-bit pattern this value contributes to a checksum: an integer
    /// sign- or zero-extended to 64 bits, a bool's 0/1, or the byte a pointer
    /// points at.
    pub fn contribution(self, leaf: Leaf) -> u64 {
        match self {
            Value::Int(v) => {
                if leaf.is_signed() {
                    (v as i64) as u64
                } else {
                    v as u64
                }
            }
            Value::Bool(b) => u64::from(b),
            Value::Ptr(index) => u64::from(PROBE_BYTES[usize::from(index)]),
        }
    }

    fn rue_literal(self) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Bool(true) => "true".to_string(),
            Value::Bool(false) => "false".to_string(),
            Value::Ptr(index) => index.to_string(),
        }
    }

    fn c_literal(self, leaf: Leaf) -> String {
        match self {
            Value::Int(v) => format!("{v}{}", leaf.c_literal_suffix()),
            Value::Bool(b) => (if b { "1" } else { "0" }).to_string(),
            Value::Ptr(index) => format!("&c_probe_bytes[{index}]"),
        }
    }

    /// A different value of the same type, returned by a callee whose seed did
    /// not arrive intact. `0`/`1` is in range for every integer type the grid
    /// uses, so the rule needs no per-type table.
    fn poisoned(self) -> Value {
        match self {
            Value::Int(0) => Value::Int(1),
            Value::Int(_) => Value::Int(0),
            Value::Bool(b) => Value::Bool(!b),
            Value::Ptr(index) => Value::Ptr((index + 1) % 8),
        }
    }
}

/// One field of a `@repr(c)` struct in the grid.
pub struct Field {
    pub name: &'static str,
    pub ty: Ty,
}

/// A `@repr(c)` struct the grid declares in both languages.
pub struct StructDef {
    pub name: &'static str,
    pub fields: &'static [Field],
}

/// A type at the boundary: a scalar, a fixed array (only ever a struct field),
/// or a struct.
#[derive(Clone, Copy)]
pub enum Ty {
    Leaf(Leaf),
    Array(Leaf, usize),
    Struct(&'static StructDef),
}

impl Ty {
    pub fn rue_type(self) -> String {
        match self {
            Ty::Leaf(leaf) => leaf.rue_type().to_string(),
            Ty::Array(leaf, len) => format!("[{}; {len}]", leaf.rue_type()),
            Ty::Struct(def) => def.name.to_string(),
        }
    }

    /// The C spelling for a parameter, local, or return type. Arrays never
    /// appear here — they exist only as struct fields, where the declarator
    /// puts the extent after the name.
    pub fn c_type(self) -> String {
        match self {
            Ty::Leaf(leaf) => leaf.c_type().to_string(),
            Ty::Array(..) => unreachable!("an array is only ever a struct field"),
            Ty::Struct(def) => def.name.to_string(),
        }
    }

    /// Every scalar leaf, in memory order, as the access path that reaches it
    /// from a value of this type. The path spelling is deliberately identical
    /// in both languages: `.f0`, `.f0.f1`, and `.f0[2]` all parse the same way
    /// in Rue and in C, so one string drives both sides.
    pub fn leaves(self) -> Vec<(String, Leaf)> {
        let mut out = Vec::new();
        self.walk_leaves("", &mut out);
        out
    }

    fn walk_leaves(self, path: &str, out: &mut Vec<(String, Leaf)>) {
        match self {
            Ty::Leaf(leaf) => out.push((path.to_string(), leaf)),
            Ty::Array(leaf, len) => {
                for index in 0..len {
                    out.push((format!("{path}[{index}]"), leaf));
                }
            }
            Ty::Struct(def) => {
                for field in def.fields {
                    field.ty.walk_leaves(&format!("{path}.{}", field.name), out);
                }
            }
        }
    }

    /// Every struct this type reaches, innermost first and without repeats, so
    /// both languages can declare them in an order where each name is already
    /// defined where it is used.
    fn structs(self, out: &mut Vec<&'static StructDef>) {
        if let Ty::Struct(def) = self {
            for field in def.fields {
                field.ty.structs(out);
            }
            if !out.iter().any(|existing| existing.name == def.name) {
                out.push(def);
            }
        }
    }
}

/// One row of the shape table.
pub struct Shape {
    pub key: &'static str,
    pub ty: Ty,
}

static ABI_U8: StructDef = StructDef {
    name: "AbiU8",
    fields: &[Field {
        name: "f0",
        ty: Ty::Leaf(Leaf::U8),
    }],
};

static ABI_U8_U8: StructDef = StructDef {
    name: "AbiU8U8",
    fields: &[
        Field {
            name: "f0",
            ty: Ty::Leaf(Leaf::U8),
        },
        Field {
            name: "f1",
            ty: Ty::Leaf(Leaf::U8),
        },
    ],
};

static ABI_I32_I32: StructDef = StructDef {
    name: "AbiI32I32",
    fields: &[
        Field {
            name: "f0",
            ty: Ty::Leaf(Leaf::I32),
        },
        Field {
            name: "f1",
            ty: Ty::Leaf(Leaf::I32),
        },
    ],
};

static ABI_I64_U8: StructDef = StructDef {
    name: "AbiI64U8",
    fields: &[
        Field {
            name: "f0",
            ty: Ty::Leaf(Leaf::I64),
        },
        Field {
            name: "f1",
            ty: Ty::Leaf(Leaf::U8),
        },
    ],
};

static ABI_I64_I64: StructDef = StructDef {
    name: "AbiI64I64",
    fields: &[
        Field {
            name: "f0",
            ty: Ty::Leaf(Leaf::I64),
        },
        Field {
            name: "f1",
            ty: Ty::Leaf(Leaf::I64),
        },
    ],
};

static ABI_I64_I64_U8: StructDef = StructDef {
    name: "AbiI64I64U8",
    fields: &[
        Field {
            name: "f0",
            ty: Ty::Leaf(Leaf::I64),
        },
        Field {
            name: "f1",
            ty: Ty::Leaf(Leaf::I64),
        },
        Field {
            name: "f2",
            ty: Ty::Leaf(Leaf::U8),
        },
    ],
};

static ABI_I64_X3: StructDef = StructDef {
    name: "AbiI64X3",
    fields: &[
        Field {
            name: "f0",
            ty: Ty::Leaf(Leaf::I64),
        },
        Field {
            name: "f1",
            ty: Ty::Leaf(Leaf::I64),
        },
        Field {
            name: "f2",
            ty: Ty::Leaf(Leaf::I64),
        },
    ],
};

static ABI_U8_I64: StructDef = StructDef {
    name: "AbiU8I64",
    fields: &[
        Field {
            name: "f0",
            ty: Ty::Leaf(Leaf::U8),
        },
        Field {
            name: "f1",
            ty: Ty::Leaf(Leaf::I64),
        },
    ],
};

static ABI_NESTED: StructDef = StructDef {
    name: "AbiNested",
    fields: &[
        Field {
            name: "f0",
            ty: Ty::Struct(&ABI_I32_I32),
        },
        Field {
            name: "f1",
            ty: Ty::Leaf(Leaf::I64),
        },
    ],
};

static ABI_ARRAY: StructDef = StructDef {
    name: "AbiArray",
    fields: &[
        Field {
            name: "f0",
            ty: Ty::Array(Leaf::U8, 4),
        },
        Field {
            name: "f1",
            ty: Ty::Leaf(Leaf::I32),
        },
    ],
};

/// The shape rows. Scalars first, then `@repr(c)` structs chosen to land on the
/// classification boundaries both current psABI rows care about: one and two
/// bytes, two fields sharing one eightbyte, a padded 16, an exact 16, a 17-byte
/// footprint padded to 24, an exact 24 (past the two-register budget on both
/// rows), a leading narrow field, a nested struct, and an array field.
pub static SHAPES: &[Shape] = &[
    Shape {
        key: "i8",
        ty: Ty::Leaf(Leaf::I8),
    },
    Shape {
        key: "u8",
        ty: Ty::Leaf(Leaf::U8),
    },
    Shape {
        key: "i16",
        ty: Ty::Leaf(Leaf::I16),
    },
    Shape {
        key: "u16",
        ty: Ty::Leaf(Leaf::U16),
    },
    Shape {
        key: "i32",
        ty: Ty::Leaf(Leaf::I32),
    },
    Shape {
        key: "u32",
        ty: Ty::Leaf(Leaf::U32),
    },
    Shape {
        key: "i64",
        ty: Ty::Leaf(Leaf::I64),
    },
    Shape {
        key: "u64",
        ty: Ty::Leaf(Leaf::U64),
    },
    Shape {
        key: "bool",
        ty: Ty::Leaf(Leaf::Bool),
    },
    Shape {
        key: "ptr",
        ty: Ty::Leaf(Leaf::Ptr),
    },
    Shape {
        key: "s_u8",
        ty: Ty::Struct(&ABI_U8),
    },
    Shape {
        key: "s_u8u8",
        ty: Ty::Struct(&ABI_U8_U8),
    },
    Shape {
        key: "s_i32i32",
        ty: Ty::Struct(&ABI_I32_I32),
    },
    Shape {
        key: "s_i64u8",
        ty: Ty::Struct(&ABI_I64_U8),
    },
    Shape {
        key: "s_i64i64",
        ty: Ty::Struct(&ABI_I64_I64),
    },
    Shape {
        key: "s_i64i64u8",
        ty: Ty::Struct(&ABI_I64_I64_U8),
    },
    Shape {
        key: "s_i64x3",
        ty: Ty::Struct(&ABI_I64_X3),
    },
    Shape {
        key: "s_u8i64",
        ty: Ty::Struct(&ABI_U8_I64),
    },
    Shape {
        key: "s_nested",
        ty: Ty::Struct(&ABI_NESTED),
    },
    Shape {
        key: "s_array",
        ty: Ty::Struct(&ABI_ARRAY),
    },
];

/// Which side of the boundary the Rue code is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Rue calls generated C.
    Import,
    /// Generated C calls `pub extern` Rue.
    Export,
}

impl Direction {
    pub fn key(self) -> &'static str {
        match self {
            Direction::Import => "import",
            Direction::Export => "export",
        }
    }
}

/// Where the shape sits in the crossing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    /// The shape occupies argument `index`; every other argument is an `i64`
    /// filler.
    Argument { key: &'static str, index: usize },
    /// The shape is the result type.
    Return,
}

impl Position {
    pub fn key(self) -> &'static str {
        match self {
            Position::Argument { key, .. } => key,
            Position::Return => "return",
        }
    }
}

/// The argument positions for one convention, plus the arity every argument
/// cell uses. Positions are named by what they exercise, and their indices come
/// from the convention's own register budget rather than a hard-coded number.
pub struct Positions {
    pub arity: usize,
    pub positions: Vec<Position>,
}

pub fn positions_for(spec: &CConventionSpec) -> Positions {
    let registers = spec.gp_argument_registers as usize;
    assert!(
        registers >= 2,
        "a C convention with fewer than two argument registers has no distinct positions"
    );
    // Four slots past the register budget, so `deep_stack` is genuinely deep and
    // every cell — whatever its position — also stacks arguments.
    let arity = registers + 4;
    Positions {
        arity,
        positions: vec![
            Position::Argument {
                key: "arg0",
                index: 0,
            },
            Position::Argument {
                key: "last_reg",
                index: registers - 1,
            },
            Position::Argument {
                key: "first_stack",
                index: registers,
            },
            Position::Argument {
                key: "deep_stack",
                index: registers + 3,
            },
            Position::Return,
        ],
    }
}

/// What one line of the program's stdout proves, for a mismatch report.
#[derive(Clone, Debug)]
pub struct Cell {
    pub function: String,
    pub shape: &'static str,
    pub position: &'static str,
    pub direction: Direction,
    pub abi: String,
}

impl Cell {
    pub fn describe(&self) -> String {
        format!(
            "{} direction, shape `{}`, position `{}`, extern \"{}\" (function `{}`)",
            self.direction.key(),
            self.shape,
            self.position,
            self.abi,
            self.function
        )
    }
}

/// One generated program: a `.c` and a `.rue` source, the exact stdout they must
/// produce, and what each of those lines means.
pub struct Program {
    pub c_source: String,
    pub rue_source: String,
    pub expected: Vec<String>,
    pub cells: Vec<Cell>,
}

/// SplitMix64, seeded from a cell's own name, so every cell's values and
/// multipliers are reproducible from the grid alone and independent of
/// iteration order.
struct Rng(u64);

impl Rng {
    fn seeded(name: &str) -> Rng {
        // FNV-1a over the cell name.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Rng(hash)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in range for `leaf`. Signed ranges deliberately exclude the type
    /// minimum, so every value is writable as a negated positive literal in both
    /// languages.
    fn value(&mut self, leaf: Leaf) -> Value {
        match leaf {
            Leaf::Bool => Value::Bool(self.next() & 1 == 1),
            Leaf::Ptr => Value::Ptr((self.next() % PROBE_BYTES.len() as u64) as u8),
            _ => {
                let width = leaf
                    .integer_width()
                    .expect("bool and pointer leaves are handled above");
                let raw = u128::from(self.next());
                if leaf.is_signed() {
                    let span = (1u128 << width) - 1;
                    let magnitude = (1i128 << (width - 1)) - 1;
                    Value::Int((raw % span) as i128 - magnitude)
                } else {
                    Value::Int((raw % (1u128 << width)) as i128)
                }
            }
        }
    }

    /// An odd multiplier below 2^62: no checksum coefficient is zero, every one
    /// is invertible modulo 2^64, and no literal approaches the range edge of
    /// either language's `u64` parsing.
    fn multiplier(&mut self) -> u64 {
        (self.next() & 0x3fff_ffff_ffff_ffff) | 1
    }
}

/// The data-model typedefs and the assertions that hold them, plus the probe
/// bytes every pointer value points into. No headers and no libc: the object
/// this becomes is linked with `-nostdlib`.
fn c_prelude() -> String {
    let probe = PROBE_BYTES
        .iter()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = String::new();
    out.push_str("/* Generated by //crates/rue-c-abi-matrix. Do not edit. */\n");
    out.push_str("typedef signed char rue_i8;\n");
    out.push_str("typedef unsigned char rue_u8;\n");
    out.push_str("typedef short rue_i16;\n");
    out.push_str("typedef unsigned short rue_u16;\n");
    out.push_str("typedef int rue_i32;\n");
    out.push_str("typedef unsigned int rue_u32;\n");
    out.push_str("typedef long rue_i64;\n");
    out.push_str("typedef unsigned long rue_u64;\n");
    out.push_str("typedef _Bool rue_bool;\n\n");
    out.push_str("_Static_assert(sizeof(rue_i8) == 1, \"signed char must be 8-bit\");\n");
    out.push_str("_Static_assert(sizeof(rue_i16) == 2, \"short must be 16-bit\");\n");
    out.push_str("_Static_assert(sizeof(rue_i32) == 4, \"int must be 32-bit\");\n");
    out.push_str("_Static_assert(sizeof(rue_i64) == 8, \"long must be 64-bit under LP64\");\n");
    out.push_str("_Static_assert(sizeof(void *) == 8, \"pointers must be 64-bit\");\n\n");
    out.push_str(&format!(
        "static const rue_u8 c_probe_bytes[{}] = {{ {probe} }};\n\n",
        PROBE_BYTES.len()
    ));
    out.push_str("const rue_u8 *c_probe(rue_i64 index) {\n");
    out.push_str(&format!(
        "    return &c_probe_bytes[index & {}];\n}}\n\n",
        PROBE_BYTES.len() - 1
    ));
    out
}

fn c_struct_declarations(structs: &[&'static StructDef]) -> String {
    let mut out = String::new();
    for def in structs {
        out.push_str("typedef struct {\n");
        for field in def.fields {
            match field.ty {
                Ty::Array(leaf, len) => {
                    out.push_str(&format!("    {} {}[{len}];\n", leaf.c_type(), field.name));
                }
                other => out.push_str(&format!("    {} {};\n", other.c_type(), field.name)),
            }
        }
        out.push_str(&format!("}} {};\n\n", def.name));
    }
    out
}

fn rue_struct_declarations(structs: &[&'static StructDef]) -> String {
    let mut out = String::new();
    for def in structs {
        out.push_str("@repr(c)\n");
        out.push_str(&format!("struct {} {{\n", def.name));
        for (index, field) in def.fields.iter().enumerate() {
            let comma = if index + 1 == def.fields.len() {
                ""
            } else {
                ","
            };
            out.push_str(&format!(
                "    {}: {}{comma}\n",
                field.name,
                field.ty.rue_type()
            ));
        }
        out.push_str("}\n\n");
    }
    out
}

/// One `acc += value * multiplier` step in C, reading `expr` as `leaf`.
fn c_mix(expr: &str, leaf: Leaf, multiplier: u64) -> String {
    let widened = match leaf {
        Leaf::Ptr => format!("(rue_u64)(*({expr}))"),
        Leaf::Bool => format!("(rue_u64)(({expr}) ? 1 : 0)"),
        _ if leaf.is_signed() => format!("(rue_u64)(rue_i64)({expr})"),
        _ => format!("(rue_u64)({expr})"),
    };
    format!("    acc += {widened} * {multiplier}UL;\n")
}

/// The same step in Rue. Rue integer arithmetic traps on overflow, so the
/// accumulation uses the wrapping intrinsics; the conversions are explicit
/// because Rue has no implicit widening.
fn rue_mix(index: usize, expr: &str, leaf: Leaf, multiplier: u64) -> String {
    let mut out = String::new();
    match leaf {
        Leaf::Ptr => {
            out.push_str(&format!(
                "    let b{index}: u8 = checked {{ @ptr_read({expr}) }};\n"
            ));
            out.push_str(&format!("    let x{index}: u64 = @intCast(b{index});\n"));
        }
        Leaf::Bool => {
            out.push_str(&format!(
                "    let x{index}: u64 = if {expr} {{ 1 }} else {{ 0 }};\n"
            ));
        }
        Leaf::I64 => {
            out.push_str(&format!("    let x{index}: u64 = @bitCast({expr});\n"));
        }
        Leaf::U64 => {
            out.push_str(&format!("    let x{index}: u64 = {expr};\n"));
        }
        _ if leaf.is_signed() => {
            out.push_str(&format!("    let t{index}: i64 = @intCast({expr});\n"));
            out.push_str(&format!("    let x{index}: u64 = @bitCast(t{index});\n"));
        }
        _ => {
            out.push_str(&format!("    let x{index}: u64 = @intCast({expr});\n"));
        }
    }
    out.push_str(&format!(
        "    acc = @wrapping_add(acc, @wrapping_mul(x{index}, {multiplier}));\n"
    ));
    out
}

/// The Rue expression building one value of `ty` from `values` in leaf order.
fn rue_value_literal(ty: Ty, values: &mut std::slice::Iter<'_, Value>) -> String {
    match ty {
        Ty::Leaf(_) => values.next().expect("one value per leaf").rue_literal(),
        Ty::Array(_, len) => {
            let elements: Vec<String> = (0..len)
                .map(|_| values.next().expect("one value per leaf").rue_literal())
                .collect();
            format!("[{}]", elements.join(", "))
        }
        Ty::Struct(def) => {
            let fields: Vec<String> = def
                .fields
                .iter()
                .map(|field| format!("{}: {}", field.name, rue_value_literal(field.ty, values)))
                .collect();
            format!("{} {{ {} }}", def.name, fields.join(", "))
        }
    }
}

/// The C statements that declare `name` and fill every leaf of `ty`.
fn c_value_definition(name: &str, ty: Ty, leaves: &[(String, Leaf)], values: &[Value]) -> String {
    match ty {
        Ty::Leaf(Leaf::Ptr) => format!(
            "    const rue_u8 *{name} = {};\n",
            values[0].c_literal(Leaf::Ptr)
        ),
        Ty::Leaf(leaf) => format!(
            "    {} {name} = {};\n",
            leaf.c_type(),
            values[0].c_literal(leaf)
        ),
        _ => {
            let mut out = format!("    {} {name};\n", ty.c_type());
            for ((path, leaf), value) in leaves.iter().zip(values) {
                out.push_str(&format!("    {name}{path} = {};\n", value.c_literal(*leaf)));
            }
            out
        }
    }
}

/// One cell's values, multipliers, and the checksum they must produce.
struct CellPlan {
    name: String,
    shape: &'static Shape,
    position: Position,
    /// One value per argument slot. The slot the shape occupies is unused.
    fillers: Vec<Value>,
    shape_values: Vec<Value>,
    /// Multipliers in contribution order.
    multipliers: Vec<u64>,
    /// The seed a return-position cell round-trips through the callee.
    seed: i128,
    checksum: u64,
}

fn plan_cell(
    shape: &'static Shape,
    position: Position,
    arity: usize,
    direction: Direction,
    abi: &str,
) -> CellPlan {
    let name = format!(
        "{}_{}_{}_{}",
        direction.key(),
        abi.replace('-', "_"),
        shape.key,
        position.key()
    );
    let mut rng = Rng::seeded(&name);
    let leaves = shape.ty.leaves();

    let (fillers, shape_values, multipliers) = match position {
        Position::Argument { index, .. } => {
            let mut fillers: Vec<Value> = (0..arity).map(|_| rng.value(Leaf::I64)).collect();
            // The shape occupies this slot; its filler value is never emitted.
            fillers[index] = Value::Int(0);
            let shape_values: Vec<Value> =
                leaves.iter().map(|(_, leaf)| rng.value(*leaf)).collect();
            // One multiplier per contribution: every filler, and every leaf of
            // the shape argument.
            let contributions = arity - 1 + leaves.len();
            let multipliers: Vec<u64> = (0..contributions).map(|_| rng.multiplier()).collect();
            (fillers, shape_values, multipliers)
        }
        Position::Return => {
            let shape_values: Vec<Value> =
                leaves.iter().map(|(_, leaf)| rng.value(*leaf)).collect();
            let multipliers: Vec<u64> = (0..leaves.len()).map(|_| rng.multiplier()).collect();
            (Vec::new(), shape_values, multipliers)
        }
    };

    let seed = match (position, shape.ty) {
        // A pointer result is produced by handing the seed to `c_probe`, so the
        // seed must be a probe index rather than an arbitrary 64-bit value.
        (Position::Return, Ty::Leaf(Leaf::Ptr)) => match shape_values[0] {
            Value::Ptr(index) => i128::from(index),
            _ => unreachable!("a pointer leaf carries a pointer value"),
        },
        (Position::Return, _) => match rng.value(Leaf::I64) {
            Value::Int(v) => v,
            _ => unreachable!("an i64 leaf carries an integer value"),
        },
        _ => 0,
    };

    let mut checksum: u64 = 0;
    let mut multiplier = multipliers.iter();
    let mut mix = |value: &Value, leaf: Leaf, checksum: &mut u64| {
        *checksum = checksum.wrapping_add(
            value
                .contribution(leaf)
                .wrapping_mul(*multiplier.next().expect("a multiplier per contribution")),
        );
    };
    match position {
        Position::Argument { index, .. } => {
            for (slot, value) in fillers.iter().enumerate() {
                if slot == index {
                    for ((_, leaf), leaf_value) in leaves.iter().zip(&shape_values) {
                        mix(leaf_value, *leaf, &mut checksum);
                    }
                } else {
                    mix(value, Leaf::I64, &mut checksum);
                }
            }
        }
        Position::Return => {
            for ((_, leaf), leaf_value) in leaves.iter().zip(&shape_values) {
                mix(leaf_value, *leaf, &mut checksum);
            }
        }
    }

    CellPlan {
        name,
        shape,
        position,
        fillers,
        shape_values,
        multipliers,
        seed,
        checksum,
    }
}

/// Parameter list for an argument cell, as `(name, type)` pairs.
fn argument_parameters(plan: &CellPlan, arity: usize, index: usize) -> Vec<(String, Ty)> {
    (0..arity)
        .map(|slot| {
            let ty = if slot == index {
                plan.shape.ty
            } else {
                Ty::Leaf(Leaf::I64)
            };
            (format!("a{slot}"), ty)
        })
        .collect()
}

/// The Rue statement binding `v` to the shape value an argument cell passes.
fn rue_argument_value(plan: &CellPlan) -> String {
    match plan.shape.ty {
        Ty::Leaf(Leaf::Ptr) => format!(
            "    let v = checked {{ c_probe({}) }};\n",
            plan.shape_values[0].rue_literal()
        ),
        Ty::Leaf(leaf) => format!(
            "    let v: {} = {};\n",
            leaf.rue_type(),
            plan.shape_values[0].rue_literal()
        ),
        ty => {
            let mut values = plan.shape_values.iter();
            format!("    let v = {};\n", rue_value_literal(ty, &mut values))
        }
    }
}

/// The C function a return-position import cell calls. It answers the grid's
/// values only when the seed arrived intact and a different value otherwise, so
/// one line proves the argument crossing as well as the result crossing.
fn c_return_builder(plan: &CellPlan, leaves: &[(String, Leaf)]) -> String {
    if matches!(plan.shape.ty, Ty::Leaf(Leaf::Ptr)) {
        return format!(
            "const rue_u8 *{}(rue_i64 seed) {{\n    return c_probe(seed);\n}}\n\n",
            plan.name
        );
    }
    let c_type = plan.shape.ty.c_type();
    let mut out = format!("{c_type} {}(rue_i64 seed) {{\n", plan.name);
    out.push_str(&c_value_definition(
        "v",
        plan.shape.ty,
        leaves,
        &plan.shape_values,
    ));
    out.push_str(&format!("    if (seed != {}L) {{\n", plan.seed));
    for ((path, leaf), value) in leaves.iter().zip(&plan.shape_values) {
        out.push_str(&format!(
            "        v{path} = {};\n",
            value.poisoned().c_literal(*leaf)
        ));
    }
    out.push_str("    }\n    return v;\n}\n\n");
    out
}

/// The Rue export a return-position export cell calls, under the same
/// seed-or-poison contract as its C counterpart.
fn rue_return_builder(plan: &CellPlan, abi: &str) -> String {
    if matches!(plan.shape.ty, Ty::Leaf(Leaf::Ptr)) {
        return format!(
            "pub extern \"{abi}\" fn {}(seed: i64) -> ptr const u8 {{\n    let p = checked {{ c_probe(seed) }};\n    p\n}}\n\n",
            plan.name
        );
    }
    let mut good = plan.shape_values.iter();
    let good_literal = rue_value_literal(plan.shape.ty, &mut good);
    let poisoned: Vec<Value> = plan
        .shape_values
        .iter()
        .map(|value| value.poisoned())
        .collect();
    let mut bad = poisoned.iter();
    let bad_literal = rue_value_literal(plan.shape.ty, &mut bad);
    format!(
        "pub extern \"{abi}\" fn {}(seed: i64) -> {} {{\n    if seed == {} {{ {good_literal} }} else {{ {bad_literal} }}\n}}\n\n",
        plan.name,
        plan.shape.ty.rue_type(),
        plan.seed,
    )
}

/// Generate the paired sources and expected output for one direction and one
/// ABI spelling.
pub fn generate(direction: Direction, abi: &str, spec: &CConventionSpec) -> Program {
    let layout = positions_for(spec);
    let mut structs: Vec<&'static StructDef> = Vec::new();
    for shape in SHAPES {
        shape.ty.structs(&mut structs);
    }

    let plans: Vec<CellPlan> = SHAPES
        .iter()
        .flat_map(|shape| {
            layout
                .positions
                .iter()
                .map(move |position| (shape, *position))
        })
        .map(|(shape, position)| plan_cell(shape, position, layout.arity, direction, abi))
        .collect();

    let mut c_body = String::new();
    let mut rue_extern = String::new();
    let mut rue_body = String::new();
    let mut rue_main = String::new();

    for plan in &plans {
        let leaves = plan.shape.ty.leaves();
        match (direction, plan.position) {
            // --- Rue calls C with the shape in one argument slot -------------
            (Direction::Import, Position::Argument { index, .. }) => {
                let parameters = argument_parameters(plan, layout.arity, index);
                let c_params: Vec<String> = parameters
                    .iter()
                    .map(|(name, ty)| format!("{} {name}", ty.c_type()))
                    .collect();
                c_body.push_str(&format!(
                    "rue_u64 {}({}) {{\n    rue_u64 acc = 0;\n",
                    plan.name,
                    c_params.join(", ")
                ));
                let mut multiplier = plan.multipliers.iter();
                for slot in 0..parameters.len() {
                    if slot == index {
                        for (path, leaf) in &leaves {
                            c_body.push_str(&c_mix(
                                &format!("a{slot}{path}"),
                                *leaf,
                                *multiplier.next().expect("a multiplier per leaf"),
                            ));
                        }
                    } else {
                        c_body.push_str(&c_mix(
                            &format!("a{slot}"),
                            Leaf::I64,
                            *multiplier.next().expect("a multiplier per filler"),
                        ));
                    }
                }
                c_body.push_str("    return acc;\n}\n\n");

                let rue_params: Vec<String> = parameters
                    .iter()
                    .map(|(name, ty)| format!("{name}: {}", ty.rue_type()))
                    .collect();
                rue_extern.push_str(&format!(
                    "    fn {}({}) -> u64;\n",
                    plan.name,
                    rue_params.join(", ")
                ));

                rue_body.push_str(&format!("fn call_{}() -> u64 {{\n", plan.name));
                rue_body.push_str(&rue_argument_value(plan));
                let arguments: Vec<String> = (0..layout.arity)
                    .map(|slot| {
                        if slot == index {
                            "v".to_string()
                        } else {
                            plan.fillers[slot].rue_literal()
                        }
                    })
                    .collect();
                rue_body.push_str(&format!(
                    "    let r = checked {{ {}({}) }};\n    r\n}}\n\n",
                    plan.name,
                    arguments.join(", ")
                ));
                rue_main.push_str(&format!("    @dbg(call_{}());\n", plan.name));
            }

            // --- Rue reads a shape C returned --------------------------------
            (Direction::Import, Position::Return) => {
                c_body.push_str(&c_return_builder(plan, &leaves));
                rue_extern.push_str(&format!(
                    "    fn {}(seed: i64) -> {};\n",
                    plan.name,
                    plan.shape.ty.rue_type()
                ));
                rue_body.push_str(&format!("fn call_{}() -> u64 {{\n", plan.name));
                rue_body.push_str(&format!(
                    "    let v = checked {{ {}({}) }};\n    let mut acc: u64 = 0;\n",
                    plan.name, plan.seed
                ));
                for (step, ((path, leaf), multiplier)) in
                    leaves.iter().zip(&plan.multipliers).enumerate()
                {
                    rue_body.push_str(&rue_mix(step, &format!("v{path}"), *leaf, *multiplier));
                }
                rue_body.push_str("    acc\n}\n\n");
                rue_main.push_str(&format!("    @dbg(call_{}());\n", plan.name));
            }

            // --- C calls a Rue export with the shape in one argument slot ----
            (Direction::Export, Position::Argument { index, .. }) => {
                let parameters = argument_parameters(plan, layout.arity, index);
                let rue_params: Vec<String> = parameters
                    .iter()
                    .map(|(name, ty)| format!("{name}: {}", ty.rue_type()))
                    .collect();
                rue_body.push_str(&format!(
                    "pub extern \"{abi}\" fn {}({}) -> u64 {{\n    let mut acc: u64 = 0;\n",
                    plan.name,
                    rue_params.join(", ")
                ));
                let mut multiplier = plan.multipliers.iter();
                let mut step = 0usize;
                for slot in 0..parameters.len() {
                    if slot == index {
                        for (path, leaf) in &leaves {
                            rue_body.push_str(&rue_mix(
                                step,
                                &format!("a{slot}{path}"),
                                *leaf,
                                *multiplier.next().expect("a multiplier per leaf"),
                            ));
                            step += 1;
                        }
                    } else {
                        rue_body.push_str(&rue_mix(
                            step,
                            &format!("a{slot}"),
                            Leaf::I64,
                            *multiplier.next().expect("a multiplier per filler"),
                        ));
                        step += 1;
                    }
                }
                rue_body.push_str("    acc\n}\n\n");

                let c_params: Vec<String> = parameters
                    .iter()
                    .map(|(name, ty)| format!("{} {name}", ty.c_type()))
                    .collect();
                c_body.push_str(&format!(
                    "rue_u64 {}({});\n",
                    plan.name,
                    c_params.join(", ")
                ));
                c_body.push_str(&format!("rue_u64 drive_{}(void) {{\n", plan.name));
                c_body.push_str(&c_value_definition(
                    "v",
                    plan.shape.ty,
                    &leaves,
                    &plan.shape_values,
                ));
                let arguments: Vec<String> = (0..layout.arity)
                    .map(|slot| {
                        if slot == index {
                            "v".to_string()
                        } else {
                            plan.fillers[slot].c_literal(Leaf::I64)
                        }
                    })
                    .collect();
                c_body.push_str(&format!(
                    "    return {}({});\n}}\n\n",
                    plan.name,
                    arguments.join(", ")
                ));

                rue_extern.push_str(&format!("    fn drive_{}() -> u64;\n", plan.name));
                rue_main.push_str(&format!("    @dbg(checked {{ drive_{}() }});\n", plan.name));
            }

            // --- C reads a shape a Rue export returned -----------------------
            (Direction::Export, Position::Return) => {
                rue_body.push_str(&rue_return_builder(plan, abi));

                c_body.push_str(&format!(
                    "{} {}(rue_i64 seed);\n",
                    plan.shape.ty.c_type(),
                    plan.name
                ));
                c_body.push_str(&format!(
                    "rue_u64 drive_{}(void) {{\n    {} v = {}({}L);\n    rue_u64 acc = 0;\n",
                    plan.name,
                    plan.shape.ty.c_type(),
                    plan.name,
                    plan.seed
                ));
                for ((path, leaf), multiplier) in leaves.iter().zip(&plan.multipliers) {
                    c_body.push_str(&c_mix(&format!("v{path}"), *leaf, *multiplier));
                }
                c_body.push_str("    return acc;\n}\n\n");

                rue_extern.push_str(&format!("    fn drive_{}() -> u64;\n", plan.name));
                rue_main.push_str(&format!("    @dbg(checked {{ drive_{}() }});\n", plan.name));
            }
        }
    }

    let c_source = format!(
        "{}{}{}",
        c_prelude(),
        c_struct_declarations(&structs),
        c_body
    );

    let rue_source = format!(
        "// Generated by //crates/rue-c-abi-matrix. Do not edit.\n\n\
         {}extern \"{abi}\" {{\n    fn c_probe(index: i64) -> ptr const u8;\n{}}}\n\n\
         {}fn main() -> i32 {{\n{}    0\n}}\n",
        rue_struct_declarations(&structs),
        rue_extern,
        rue_body,
        rue_main,
    );

    let expected = plans.iter().map(|plan| plan.checksum.to_string()).collect();
    let cells = plans
        .iter()
        .map(|plan| Cell {
            function: plan.name.clone(),
            shape: plan.shape.key,
            position: plan.position.key(),
            direction,
            abi: abi.to_string(),
        })
        .collect();

    Program {
        c_source,
        rue_source,
        expected,
        cells,
    }
}
