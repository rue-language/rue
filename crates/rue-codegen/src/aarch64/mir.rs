//! AArch64 Machine Intermediate Representation.
//!
//! Aarch64Mir represents AArch64 instructions with virtual registers. This IR:
//! - Maps closely to actual AArch64 instructions
//! - Uses virtual registers (unlimited) that are later allocated to physical registers
//! - Can be emitted to machine code or assembly text

use std::fmt;

/// A virtual register.
///
/// Virtual registers are unlimited and allocated to physical registers
/// during register allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VReg(u32);

impl VReg {
    /// Create a new virtual register with the given index.
    #[inline]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Get the index of this virtual register.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for VReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A label identifier.
///
/// Labels are local to a function and are represented as a lightweight u32 index
/// rather than as heap-allocated strings. This avoids allocations during codegen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId(u32);

impl LabelId {
    /// Create a new label with the given index.
    #[inline]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Get the index of this label.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for LabelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ".L{}", self.0)
    }
}

/// A physical AArch64 register.
///
/// AArch64 has 31 general-purpose registers (X0-X30), plus SP and XZR.
/// - X0-X7: Argument/result registers
/// - X8: Indirect result location register
/// - X9-X15: Caller-saved temporaries
/// - X16-X17: Intra-procedure-call scratch registers (IP0, IP1)
/// - X18: Platform register (reserved on some platforms)
/// - X19-X28: Callee-saved registers
/// - X29: Frame pointer (FP)
/// - X30: Link register (LR)
/// - SP: Stack pointer (not X31, separate register)
/// - XZR: Zero register (reads as zero, writes discarded)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Reg {
    X0 = 0,
    X1 = 1,
    X2 = 2,
    X3 = 3,
    X4 = 4,
    X5 = 5,
    X6 = 6,
    X7 = 7,
    X8 = 8,
    X9 = 9,
    X10 = 10,
    X11 = 11,
    X12 = 12,
    X13 = 13,
    X14 = 14,
    X15 = 15,
    X16 = 16,
    X17 = 17,
    X18 = 18,
    X19 = 19,
    X20 = 20,
    X21 = 21,
    X22 = 22,
    X23 = 23,
    X24 = 24,
    X25 = 25,
    X26 = 26,
    X27 = 27,
    X28 = 28,
    /// Frame pointer (X29)
    Fp = 29,
    /// Link register (X30)
    Lr = 30,
    /// Stack pointer (special, not X31)
    Sp = 31,
    /// Zero register (reads as zero, writes discarded)
    Xzr = 32,
}

impl Reg {
    /// Get the register encoding for instruction fields (0-30 for X0-X30, 31 for SP/XZR).
    #[inline]
    pub const fn encoding(self) -> u8 {
        match self {
            Reg::X0 => 0,
            Reg::X1 => 1,
            Reg::X2 => 2,
            Reg::X3 => 3,
            Reg::X4 => 4,
            Reg::X5 => 5,
            Reg::X6 => 6,
            Reg::X7 => 7,
            Reg::X8 => 8,
            Reg::X9 => 9,
            Reg::X10 => 10,
            Reg::X11 => 11,
            Reg::X12 => 12,
            Reg::X13 => 13,
            Reg::X14 => 14,
            Reg::X15 => 15,
            Reg::X16 => 16,
            Reg::X17 => 17,
            Reg::X18 => 18,
            Reg::X19 => 19,
            Reg::X20 => 20,
            Reg::X21 => 21,
            Reg::X22 => 22,
            Reg::X23 => 23,
            Reg::X24 => 24,
            Reg::X25 => 25,
            Reg::X26 => 26,
            Reg::X27 => 27,
            Reg::X28 => 28,
            Reg::Fp => 29,
            Reg::Lr => 30,
            Reg::Sp => 31,
            Reg::Xzr => 31, // Same encoding as SP, context determines meaning
        }
    }

    /// Whether this is a callee-saved register (X19-X28, FP, LR).
    #[inline]
    pub const fn is_callee_saved(self) -> bool {
        matches!(
            self,
            Reg::X19
                | Reg::X20
                | Reg::X21
                | Reg::X22
                | Reg::X23
                | Reg::X24
                | Reg::X25
                | Reg::X26
                | Reg::X27
                | Reg::X28
                | Reg::Fp
                | Reg::Lr
        )
    }

    /// The 64-bit version of this register's name.
    pub const fn name64(self) -> &'static str {
        match self {
            Reg::X0 => "x0",
            Reg::X1 => "x1",
            Reg::X2 => "x2",
            Reg::X3 => "x3",
            Reg::X4 => "x4",
            Reg::X5 => "x5",
            Reg::X6 => "x6",
            Reg::X7 => "x7",
            Reg::X8 => "x8",
            Reg::X9 => "x9",
            Reg::X10 => "x10",
            Reg::X11 => "x11",
            Reg::X12 => "x12",
            Reg::X13 => "x13",
            Reg::X14 => "x14",
            Reg::X15 => "x15",
            Reg::X16 => "x16",
            Reg::X17 => "x17",
            Reg::X18 => "x18",
            Reg::X19 => "x19",
            Reg::X20 => "x20",
            Reg::X21 => "x21",
            Reg::X22 => "x22",
            Reg::X23 => "x23",
            Reg::X24 => "x24",
            Reg::X25 => "x25",
            Reg::X26 => "x26",
            Reg::X27 => "x27",
            Reg::X28 => "x28",
            Reg::Fp => "fp",
            Reg::Lr => "lr",
            Reg::Sp => "sp",
            Reg::Xzr => "xzr",
        }
    }

    /// The 32-bit version of this register's name.
    pub const fn name32(self) -> &'static str {
        match self {
            Reg::X0 => "w0",
            Reg::X1 => "w1",
            Reg::X2 => "w2",
            Reg::X3 => "w3",
            Reg::X4 => "w4",
            Reg::X5 => "w5",
            Reg::X6 => "w6",
            Reg::X7 => "w7",
            Reg::X8 => "w8",
            Reg::X9 => "w9",
            Reg::X10 => "w10",
            Reg::X11 => "w11",
            Reg::X12 => "w12",
            Reg::X13 => "w13",
            Reg::X14 => "w14",
            Reg::X15 => "w15",
            Reg::X16 => "w16",
            Reg::X17 => "w17",
            Reg::X18 => "w18",
            Reg::X19 => "w19",
            Reg::X20 => "w20",
            Reg::X21 => "w21",
            Reg::X22 => "w22",
            Reg::X23 => "w23",
            Reg::X24 => "w24",
            Reg::X25 => "w25",
            Reg::X26 => "w26",
            Reg::X27 => "w27",
            Reg::X28 => "w28",
            Reg::Fp => "w29",
            Reg::Lr => "w30",
            Reg::Sp => "wsp",
            Reg::Xzr => "wzr",
        }
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name64())
    }
}

/// An operand that can be either a virtual or physical register.
///
/// Before register allocation, operands are `Virtual`.
/// After register allocation, operands are `Physical`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    /// A virtual register (pre-regalloc).
    Virtual(VReg),
    /// A physical register (post-regalloc).
    Physical(Reg),
}

impl Operand {
    /// Unwrap this operand as a physical register.
    ///
    /// # Panics
    /// Panics if this is a virtual register.
    #[inline]
    pub fn as_physical(self) -> Reg {
        match self {
            Operand::Physical(reg) => reg,
            Operand::Virtual(vreg) => panic!("expected physical register, got {}", vreg),
        }
    }

    /// Check if this operand is a physical register.
    #[inline]
    pub const fn is_physical(self) -> bool {
        matches!(self, Operand::Physical(_))
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Virtual(vreg) => write!(f, "{}", vreg),
            Operand::Physical(reg) => write!(f, "{}", reg),
        }
    }
}

impl From<VReg> for Operand {
    fn from(vreg: VReg) -> Self {
        Operand::Virtual(vreg)
    }
}

impl From<Reg> for Operand {
    fn from(reg: Reg) -> Self {
        Operand::Physical(reg)
    }
}

/// Condition code for conditional operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    // === Equality conditions ===
    /// Equal (Z=1)
    Eq,
    /// Not equal (Z=0)
    Ne,

    // === Signed comparison conditions ===
    /// Signed less than (N!=V)
    Lt,
    /// Signed greater than (Z=0 && N=V)
    Gt,
    /// Signed less than or equal (Z=1 || N!=V)
    Le,
    /// Signed greater than or equal (N=V)
    Ge,

    // === Unsigned comparison conditions ===
    /// Unsigned higher (C=1 && Z=0) - strictly greater than
    Hi,
    /// Unsigned lower or same (C=0 || Z=1) - less than or equal
    Ls,
    /// Unsigned higher or same / Carry set (C=1) - greater than or equal
    Hs,
    /// Unsigned lower / Carry clear (C=0) - strictly less than
    Lo,
}

impl Cond {
    /// Get the 4-bit encoding for this condition code.
    pub const fn encoding(self) -> u8 {
        match self {
            Cond::Eq => 0b0000,
            Cond::Ne => 0b0001,
            Cond::Hs => 0b0010, // CS (Carry Set)
            Cond::Lo => 0b0011, // CC (Carry Clear)
            Cond::Hi => 0b1000,
            Cond::Ls => 0b1001,
            Cond::Ge => 0b1010,
            Cond::Lt => 0b1011,
            Cond::Gt => 0b1100,
            Cond::Le => 0b1101,
        }
    }

    /// Get the inverted condition.
    pub const fn invert(self) -> Cond {
        match self {
            Cond::Eq => Cond::Ne,
            Cond::Ne => Cond::Eq,
            Cond::Lt => Cond::Ge,
            Cond::Gt => Cond::Le,
            Cond::Le => Cond::Gt,
            Cond::Ge => Cond::Lt,
            Cond::Hi => Cond::Ls,
            Cond::Ls => Cond::Hi,
            Cond::Hs => Cond::Lo,
            Cond::Lo => Cond::Hs,
        }
    }

    /// Returns true if this is an unsigned comparison condition.
    pub const fn is_unsigned(self) -> bool {
        matches!(self, Cond::Hi | Cond::Ls | Cond::Hs | Cond::Lo)
    }
}

impl fmt::Display for Cond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cond::Eq => write!(f, "eq"),
            Cond::Ne => write!(f, "ne"),
            Cond::Lt => write!(f, "lt"),
            Cond::Gt => write!(f, "gt"),
            Cond::Le => write!(f, "le"),
            Cond::Ge => write!(f, "ge"),
            Cond::Hi => write!(f, "hi"),
            Cond::Ls => write!(f, "ls"),
            Cond::Hs => write!(f, "hs"),
            Cond::Lo => write!(f, "lo"),
        }
    }
}

/// An AArch64 MIR instruction.
#[derive(Debug, Clone)]
pub enum Aarch64Inst {
    // === Move instructions ===
    /// `mov dst, #imm` - Move immediate to register (MOVZ/MOVN/MOVK sequence).
    MovImm { dst: Operand, imm: i64 },

    /// `mov dst, src` - Move register to register.
    MovRR { dst: Operand, src: Operand },

    /// `ldr dst, [base, #offset]` - Load from memory.
    Ldr {
        dst: Operand,
        base: Reg,
        offset: i32,
    },

    /// `str src, [base, #offset]` - Store to memory.
    Str {
        src: Operand,
        base: Reg,
        offset: i32,
    },

    /// `ldr dst, [base]` - Load from memory via register (indexed).
    LdrIndexed { dst: Operand, base: VReg },

    /// `str src, [base]` - Store to memory via register (indexed).
    StrIndexed { src: Operand, base: VReg },

    /// `add dst, src, #imm, lsl #12` - Add immediate with shift (for large offsets).
    /// Note: Using regular AddImm for now, this is for future large offset support.

    /// `lsl dst, src, #imm` - Logical shift left by immediate (64-bit).
    LslImm { dst: Operand, src: Operand, imm: u8 },

    /// `lsl dst, src, #imm` - Logical shift left by immediate (32-bit).
    Lsl32Imm { dst: Operand, src: Operand, imm: u8 },

    /// `lsr dst, src, #imm` - Logical shift right by immediate (32-bit).
    Lsr32Imm { dst: Operand, src: Operand, imm: u8 },

    /// `asr dst, src, #imm` - Arithmetic shift right by immediate (32-bit).
    Asr32Imm { dst: Operand, src: Operand, imm: u8 },

    // === Arithmetic instructions ===
    /// `add dst, src1, src2` - Add two registers.
    AddRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `adds dst, src1, src2` - Add and set flags (32-bit).
    AddsRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `adds dst, src1, src2` - Add and set flags (64-bit).
    AddsRR64 {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `add dst, src, #imm` - Add immediate.
    AddImm {
        dst: Operand,
        src: Operand,
        imm: i32,
    },

    /// `sub dst, src1, src2` - Subtract.
    SubRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `subs dst, src1, src2` - Subtract and set flags (32-bit).
    SubsRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `subs dst, src1, src2` - Subtract and set flags (64-bit).
    SubsRR64 {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `sub dst, src, #imm` - Subtract immediate.
    SubImm {
        dst: Operand,
        src: Operand,
        imm: i32,
    },

    /// `mul dst, src1, src2` - Multiply.
    MulRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `smull dst, src1, src2` - Signed multiply long (32x32->64 for overflow check).
    SmullRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `umull dst, src1, src2` - Unsigned multiply long (32x32->64 for overflow check).
    UmullRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `smulh dst, src1, src2` - Signed multiply high (high 64 bits of 64x64->128).
    SmulhRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `umulh dst, src1, src2` - Unsigned multiply high (high 64 bits of 64x64->128).
    UmulhRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `lsr dst, src, #imm` - Logical shift right by immediate (64-bit).
    Lsr64Imm { dst: Operand, src: Operand, imm: u8 },

    /// `asr dst, src, #imm` - Arithmetic shift right by immediate (64-bit).
    Asr64Imm { dst: Operand, src: Operand, imm: u8 },

    /// `sdiv dst, src1, src2` - Signed divide.
    SdivRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `msub dst, src1, src2, src3` - Multiply-subtract: dst = src3 - (src1 * src2)
    /// Used for computing remainder: rem = dividend - (quotient * divisor)
    Msub {
        dst: Operand,
        src1: Operand,
        src2: Operand,
        src3: Operand,
    },

    /// `neg dst, src` - Negate (sub from zero).
    Neg { dst: Operand, src: Operand },

    /// `negs dst, src` - Negate and set flags (64-bit, for i64/u64 overflow detection).
    Negs { dst: Operand, src: Operand },

    /// `negs dst, src` - Negate and set flags (32-bit, for i32/u32 overflow detection).
    Negs32 { dst: Operand, src: Operand },

    // === Logical instructions ===
    /// `and dst, src1, src2` - Bitwise AND.
    AndRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `orr dst, src1, src2` - Bitwise OR.
    OrrRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `eor dst, src1, src2` - Bitwise XOR.
    EorRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `eor dst, src, #imm` - XOR with immediate.
    EorImm {
        dst: Operand,
        src: Operand,
        imm: u64,
    },

    /// `mvn dst, src` - Bitwise NOT.
    MvnRR { dst: Operand, src: Operand },

    /// `lsl dst, src1, src2` - Logical shift left 64-bit by register.
    LslRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `lsl dst, src1, src2` - Logical shift left 32-bit by register.
    Lsl32RR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `lsr dst, src1, src2` - Logical shift right 64-bit by register.
    LsrRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `lsr dst, src1, src2` - Logical shift right 32-bit by register.
    Lsr32RR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `asr dst, src1, src2` - Arithmetic shift right 64-bit by register.
    AsrRR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    /// `asr dst, src1, src2` - Arithmetic shift right 32-bit by register.
    Asr32RR {
        dst: Operand,
        src1: Operand,
        src2: Operand,
    },

    // === Comparison instructions ===
    /// `cmp src1, src2` - Compare (subtract and set flags, discard result). Uses 32-bit form.
    CmpRR { src1: Operand, src2: Operand },

    /// `cmp src1, src2` - Compare using 64-bit form (for 64-bit values like SMULL result).
    Cmp64RR { src1: Operand, src2: Operand },

    /// `cmp src, #imm` - Compare with immediate.
    CmpImm { src: Operand, imm: i32 },

    /// `cbz src, label` - Compare and branch if zero.
    Cbz { src: Operand, label: LabelId },

    /// `cbnz src, label` - Compare and branch if not zero.
    Cbnz { src: Operand, label: LabelId },

    /// `cset dst, cond` - Conditional set: dst = 1 if cond, else 0.
    Cset { dst: Operand, cond: Cond },

    /// `tst src1, src2` - Test bits (AND and set flags).
    TstRR { src1: Operand, src2: Operand },

    // === Sign/zero extension ===
    /// `sxtb dst, src` - Sign-extend byte to 64-bit.
    Sxtb { dst: Operand, src: Operand },

    /// `sxth dst, src` - Sign-extend halfword to 64-bit.
    Sxth { dst: Operand, src: Operand },

    /// `sxtw dst, src` - Sign-extend word to 64-bit.
    Sxtw { dst: Operand, src: Operand },

    /// `uxtb dst, src` - Zero-extend byte to 64-bit.
    Uxtb { dst: Operand, src: Operand },

    /// `uxth dst, src` - Zero-extend halfword to 64-bit.
    Uxth { dst: Operand, src: Operand },

    // Note: UXTW is implicit in W-register operations; no separate instruction needed.

    // === Control flow ===
    /// `b label` - Unconditional branch.
    B { label: LabelId },

    /// `b.cond label` - Conditional branch.
    BCond { cond: Cond, label: LabelId },

    /// `b.vs label` - Branch if overflow set.
    Bvs { label: LabelId },

    /// `b.vc label` - Branch if overflow clear.
    Bvc { label: LabelId },

    /// Label marker (not a real instruction).
    Label { id: LabelId },

    /// `bl symbol` - Branch with link (call).
    Bl { symbol: String },

    /// `ret` - Return (branch to LR).
    Ret,

    // === Stack operations ===
    /// `stp x1, x2, [sp, #offset]!` - Store pair with pre-index (push).
    StpPre {
        src1: Operand,
        src2: Operand,
        offset: i32,
    },

    /// `ldp x1, x2, [sp], #offset` - Load pair with post-index (pop).
    LdpPost {
        dst1: Operand,
        dst2: Operand,
        offset: i32,
    },

    /// Load pointer to string constant (pseudo-instruction resolved during emission)
    StringConstPtr { dst: Operand, string_id: u32 },

    /// Load string length (pseudo-instruction resolved during emission)
    StringConstLen { dst: Operand, string_id: u32 },
}

impl Aarch64Inst {
    /// Returns physical registers clobbered by this instruction.
    ///
    /// This information is used by the register allocator to avoid assigning
    /// virtual registers to physical registers that would be clobbered.
    pub fn clobbers(&self) -> &'static [Reg] {
        match self {
            // Function calls clobber all caller-saved registers per AAPCS64
            Aarch64Inst::Bl { .. } => &[
                Reg::X0,
                Reg::X1,
                Reg::X2,
                Reg::X3,
                Reg::X4,
                Reg::X5,
                Reg::X6,
                Reg::X7,
                Reg::X8,
                Reg::X9,
                Reg::X10,
                Reg::X11,
                Reg::X12,
                Reg::X13,
                Reg::X14,
                Reg::X15,
                Reg::X16,
                Reg::X17,
                Reg::Lr,
            ],
            // All other instructions don't clobber additional registers
            _ => &[],
        }
    }
}

impl fmt::Display for Aarch64Inst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Aarch64Inst::MovImm { dst, imm } => write!(f, "mov {}, #{}", dst, imm),
            Aarch64Inst::MovRR { dst, src } => write!(f, "mov {}, {}", dst, src),
            Aarch64Inst::Ldr { dst, base, offset } => {
                if *offset == 0 {
                    write!(f, "ldr {}, [{}]", dst, base)
                } else {
                    write!(f, "ldr {}, [{}, #{}]", dst, base, offset)
                }
            }
            Aarch64Inst::Str { src, base, offset } => {
                if *offset == 0 {
                    write!(f, "str {}, [{}]", src, base)
                } else {
                    write!(f, "str {}, [{}, #{}]", src, base, offset)
                }
            }
            Aarch64Inst::LdrIndexed { dst, base } => write!(f, "ldr {}, [{}]", dst, base),
            Aarch64Inst::StrIndexed { src, base } => write!(f, "str {}, [{}]", src, base),
            Aarch64Inst::LslImm { dst, src, imm } => write!(f, "lsl {}, {}, #{}", dst, src, imm),
            Aarch64Inst::Lsl32Imm { dst, src, imm } => {
                write!(f, "lsl {}, {}, #{} // 32-bit", dst, src, imm)
            }
            Aarch64Inst::Lsr32Imm { dst, src, imm } => {
                write!(f, "lsr {}, {}, #{} // 32-bit", dst, src, imm)
            }
            Aarch64Inst::Asr32Imm { dst, src, imm } => {
                write!(f, "asr {}, {}, #{} // 32-bit", dst, src, imm)
            }
            Aarch64Inst::AddRR { dst, src1, src2 } => {
                write!(f, "add {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::AddsRR { dst, src1, src2 } => {
                write!(f, "adds {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::AddsRR64 { dst, src1, src2 } => {
                write!(f, "adds {}, {}, {} // 64-bit", dst, src1, src2)
            }
            Aarch64Inst::AddImm { dst, src, imm } => write!(f, "add {}, {}, #{}", dst, src, imm),
            Aarch64Inst::SubRR { dst, src1, src2 } => {
                write!(f, "sub {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::SubsRR { dst, src1, src2 } => {
                write!(f, "subs {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::SubsRR64 { dst, src1, src2 } => {
                write!(f, "subs {}, {}, {} // 64-bit", dst, src1, src2)
            }
            Aarch64Inst::SubImm { dst, src, imm } => write!(f, "sub {}, {}, #{}", dst, src, imm),
            Aarch64Inst::MulRR { dst, src1, src2 } => {
                write!(f, "mul {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::SmullRR { dst, src1, src2 } => {
                write!(f, "smull {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::UmullRR { dst, src1, src2 } => {
                write!(f, "umull {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::SmulhRR { dst, src1, src2 } => {
                write!(f, "smulh {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::UmulhRR { dst, src1, src2 } => {
                write!(f, "umulh {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::Lsr64Imm { dst, src, imm } => {
                write!(f, "lsr {}, {}, #{}", dst, src, imm)
            }
            Aarch64Inst::Asr64Imm { dst, src, imm } => {
                write!(f, "asr {}, {}, #{}", dst, src, imm)
            }
            Aarch64Inst::SdivRR { dst, src1, src2 } => {
                write!(f, "sdiv {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::Msub {
                dst,
                src1,
                src2,
                src3,
            } => write!(f, "msub {}, {}, {}, {}", dst, src1, src2, src3),
            Aarch64Inst::Neg { dst, src } => write!(f, "neg {}, {}", dst, src),
            Aarch64Inst::Negs { dst, src } => write!(f, "negs {}, {}", dst, src),
            Aarch64Inst::Negs32 { dst, src } => write!(f, "negs32 {}, {}", dst, src),
            Aarch64Inst::AndRR { dst, src1, src2 } => {
                write!(f, "and {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::OrrRR { dst, src1, src2 } => {
                write!(f, "orr {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::EorRR { dst, src1, src2 } => {
                write!(f, "eor {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::EorImm { dst, src, imm } => write!(f, "eor {}, {}, #{}", dst, src, imm),
            Aarch64Inst::MvnRR { dst, src } => write!(f, "mvn {}, {}", dst, src),
            Aarch64Inst::LslRR { dst, src1, src2 } => {
                write!(f, "lslq {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::Lsl32RR { dst, src1, src2 } => {
                write!(f, "lsll {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::LsrRR { dst, src1, src2 } => {
                write!(f, "lsrq {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::Lsr32RR { dst, src1, src2 } => {
                write!(f, "lsrl {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::AsrRR { dst, src1, src2 } => {
                write!(f, "asrq {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::Asr32RR { dst, src1, src2 } => {
                write!(f, "asrl {}, {}, {}", dst, src1, src2)
            }
            Aarch64Inst::CmpRR { src1, src2 } => write!(f, "cmp {}, {}", src1, src2),
            Aarch64Inst::Cmp64RR { src1, src2 } => write!(f, "cmp {}, {}", src1, src2),
            Aarch64Inst::CmpImm { src, imm } => write!(f, "cmp {}, #{}", src, imm),
            Aarch64Inst::Cbz { src, label } => write!(f, "cbz {}, {}", src, label),
            Aarch64Inst::Cbnz { src, label } => write!(f, "cbnz {}, {}", src, label),
            Aarch64Inst::Cset { dst, cond } => write!(f, "cset {}, {}", dst, cond),
            Aarch64Inst::TstRR { src1, src2 } => write!(f, "tst {}, {}", src1, src2),
            Aarch64Inst::Sxtb { dst, src } => write!(f, "sxtb {}, {}", dst, src),
            Aarch64Inst::Sxth { dst, src } => write!(f, "sxth {}, {}", dst, src),
            Aarch64Inst::Sxtw { dst, src } => write!(f, "sxtw {}, {}", dst, src),
            Aarch64Inst::Uxtb { dst, src } => write!(f, "uxtb {}, {}", dst, src),
            Aarch64Inst::Uxth { dst, src } => write!(f, "uxth {}, {}", dst, src),
            Aarch64Inst::B { label } => write!(f, "b {}", label),
            Aarch64Inst::BCond { cond, label } => write!(f, "b.{} {}", cond, label),
            Aarch64Inst::Bvs { label } => write!(f, "b.vs {}", label),
            Aarch64Inst::Bvc { label } => write!(f, "b.vc {}", label),
            Aarch64Inst::Label { id } => write!(f, "{}:", id),
            Aarch64Inst::Bl { symbol } => write!(f, "bl {}", symbol),
            Aarch64Inst::Ret => write!(f, "ret"),
            Aarch64Inst::StpPre { src1, src2, offset } => {
                write!(f, "stp {}, {}, [sp, #{}]!", src1, src2, offset)
            }
            Aarch64Inst::LdpPost { dst1, dst2, offset } => {
                write!(f, "ldp {}, {}, [sp], #{}", dst1, dst2, offset)
            }
            Aarch64Inst::StringConstPtr { dst, string_id } => {
                write!(f, "string_const_ptr {}, str{}", dst, string_id)
            }
            Aarch64Inst::StringConstLen { dst, string_id } => {
                write!(f, "string_const_len {}, str{}", dst, string_id)
            }
        }
    }
}

/// AArch64 MIR for a function.
#[derive(Debug, Default)]
pub struct Aarch64Mir {
    /// The instructions in this function.
    instructions: Vec<Aarch64Inst>,
    /// The next virtual register index.
    next_vreg: u32,
    /// The next label index.
    next_label: u32,
}

impl Aarch64Mir {
    /// Create a new empty Aarch64Mir.
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            next_vreg: 0,
            next_label: 0,
        }
    }

    /// Allocate a new virtual register.
    pub fn alloc_vreg(&mut self) -> VReg {
        let vreg = VReg::new(self.next_vreg);
        self.next_vreg += 1;
        vreg
    }

    /// Allocate a new label.
    pub fn alloc_label(&mut self) -> LabelId {
        let label = LabelId::new(self.next_label);
        self.next_label += 1;
        label
    }

    /// Get the label for a block.
    ///
    /// Block IDs are mapped to label IDs with a simple offset to avoid
    /// collisions with other allocated labels. Block labels use IDs starting
    /// at u32::MAX / 2.
    pub fn block_label(block_id: u32) -> LabelId {
        // Use high label IDs for blocks to avoid collision with alloc_label()
        LabelId::new(u32::MAX / 2 + block_id)
    }

    /// Get the number of virtual registers allocated.
    #[inline]
    pub fn vreg_count(&self) -> u32 {
        self.next_vreg
    }

    /// Add an instruction.
    pub fn push(&mut self, inst: Aarch64Inst) {
        self.instructions.push(inst);
    }

    /// Get the instructions.
    #[inline]
    pub fn instructions(&self) -> &[Aarch64Inst] {
        &self.instructions
    }

    /// Get mutable access to instructions (for register allocation).
    #[inline]
    pub fn instructions_mut(&mut self) -> &mut [Aarch64Inst] {
        &mut self.instructions
    }

    /// Iterate over instructions.
    pub fn iter(&self) -> impl Iterator<Item = &Aarch64Inst> {
        self.instructions.iter()
    }

    /// Consume the MIR and return its instructions.
    pub fn into_instructions(self) -> Vec<Aarch64Inst> {
        self.instructions
    }
}

impl fmt::Display for Aarch64Mir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for inst in &self.instructions {
            writeln!(f, "    {}", inst)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vreg_allocation() {
        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let v2 = mir.alloc_vreg();

        assert_eq!(v0.index(), 0);
        assert_eq!(v1.index(), 1);
        assert_eq!(v2.index(), 2);
        assert_eq!(mir.vreg_count(), 3);
    }

    #[test]
    fn test_reg_encoding() {
        assert_eq!(Reg::X0.encoding(), 0);
        assert_eq!(Reg::X15.encoding(), 15);
        assert_eq!(Reg::Fp.encoding(), 29);
        assert_eq!(Reg::Lr.encoding(), 30);
        assert_eq!(Reg::Sp.encoding(), 31);
        assert_eq!(Reg::Xzr.encoding(), 31);
    }

    #[test]
    fn test_callee_saved() {
        assert!(!Reg::X0.is_callee_saved());
        assert!(!Reg::X15.is_callee_saved());
        assert!(Reg::X19.is_callee_saved());
        assert!(Reg::X28.is_callee_saved());
        assert!(Reg::Fp.is_callee_saved());
        assert!(Reg::Lr.is_callee_saved());
    }

    #[test]
    fn test_instruction_display() {
        let inst = Aarch64Inst::MovImm {
            dst: Operand::Physical(Reg::X0),
            imm: 42,
        };
        assert_eq!(format!("{}", inst), "mov x0, #42");

        let inst = Aarch64Inst::AddRR {
            dst: Operand::Physical(Reg::X2),
            src1: Operand::Physical(Reg::X0),
            src2: Operand::Physical(Reg::X1),
        };
        assert_eq!(format!("{}", inst), "add x2, x0, x1");
    }

    #[test]
    fn test_condition_codes() {
        // Signed conditions
        assert_eq!(Cond::Eq.encoding(), 0b0000);
        assert_eq!(Cond::Ne.encoding(), 0b0001);
        assert_eq!(Cond::Lt.encoding(), 0b1011);
        assert_eq!(Cond::Gt.encoding(), 0b1100);
        assert_eq!(Cond::Le.encoding(), 0b1101);
        assert_eq!(Cond::Ge.encoding(), 0b1010);

        // Unsigned conditions
        assert_eq!(Cond::Hi.encoding(), 0b1000);
        assert_eq!(Cond::Ls.encoding(), 0b1001);
        assert_eq!(Cond::Hs.encoding(), 0b0010);
        assert_eq!(Cond::Lo.encoding(), 0b0011);

        // Inversions
        assert_eq!(Cond::Lt.invert(), Cond::Ge);
        assert_eq!(Cond::Eq.invert(), Cond::Ne);
        assert_eq!(Cond::Hi.invert(), Cond::Ls);
        assert_eq!(Cond::Hs.invert(), Cond::Lo);

        // is_unsigned
        assert!(!Cond::Lt.is_unsigned());
        assert!(!Cond::Eq.is_unsigned());
        assert!(Cond::Hi.is_unsigned());
        assert!(Cond::Lo.is_unsigned());
    }
}
