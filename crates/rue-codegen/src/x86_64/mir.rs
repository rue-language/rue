//! X86-64 Machine Intermediate Representation.
//!
//! X86Mir represents x86-64 instructions with virtual registers. This IR:
//! - Maps closely to actual x86-64 instructions
//! - Uses virtual registers (unlimited) that are later allocated to physical registers
//! - Can be emitted to machine code or assembly text

use std::collections::HashMap;
use std::fmt;

pub use crate::vreg::{LabelId, VReg};

/// A physical x86-64 register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Reg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Reg {
    /// Get the register encoding for ModR/M and SIB bytes.
    #[inline]
    pub const fn encoding(self) -> u8 {
        self as u8
    }

    /// Whether this register requires a REX prefix (R8-R15).
    #[inline]
    pub const fn needs_rex(self) -> bool {
        (self as u8) >= 8
    }

    /// The 32-bit version of this register's name.
    pub const fn name32(self) -> &'static str {
        match self {
            Reg::Rax => "eax",
            Reg::Rcx => "ecx",
            Reg::Rdx => "edx",
            Reg::Rbx => "ebx",
            Reg::Rsp => "esp",
            Reg::Rbp => "ebp",
            Reg::Rsi => "esi",
            Reg::Rdi => "edi",
            Reg::R8 => "r8d",
            Reg::R9 => "r9d",
            Reg::R10 => "r10d",
            Reg::R11 => "r11d",
            Reg::R12 => "r12d",
            Reg::R13 => "r13d",
            Reg::R14 => "r14d",
            Reg::R15 => "r15d",
        }
    }

    /// The 64-bit version of this register's name.
    pub const fn name64(self) -> &'static str {
        match self {
            Reg::Rax => "rax",
            Reg::Rcx => "rcx",
            Reg::Rdx => "rdx",
            Reg::Rbx => "rbx",
            Reg::Rsp => "rsp",
            Reg::Rbp => "rbp",
            Reg::Rsi => "rsi",
            Reg::Rdi => "rdi",
            Reg::R8 => "r8",
            Reg::R9 => "r9",
            Reg::R10 => "r10",
            Reg::R11 => "r11",
            Reg::R12 => "r12",
            Reg::R13 => "r13",
            Reg::R14 => "r14",
            Reg::R15 => "r15",
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

/// An x86-64 MIR instruction.
#[derive(Debug, Clone)]
pub enum X86Inst {
    /// `mov dst, imm32` - Move 32-bit immediate to register.
    MovRI32 { dst: Operand, imm: i32 },

    /// `mov dst, imm64` - Move 64-bit immediate to register.
    MovRI64 { dst: Operand, imm: i64 },

    /// `mov dst, src` - Move register to register.
    MovRR { dst: Operand, src: Operand },

    /// `mov dst, [base + offset]` - Load from memory (stack local).
    MovRM {
        dst: Operand,
        base: Reg,
        offset: i32,
    },

    /// `mov [base + offset], src` - Store to memory (stack local).
    MovMR {
        base: Reg,
        offset: i32,
        src: Operand,
    },

    // Arithmetic instructions
    /// `add dst, src` - Add src to dst (dst = dst + src).
    AddRR { dst: Operand, src: Operand },

    /// `add dst, src` (64-bit) - Add src to dst treating operands as 64-bit.
    ///
    /// Used for 64-bit arithmetic where 32-bit truncation would give incorrect overflow detection.
    AddRR64 { dst: Operand, src: Operand },

    /// `sub dst, src` - Subtract src from dst (dst = dst - src).
    SubRR { dst: Operand, src: Operand },

    /// `sub dst, src` (64-bit) - Subtract src from dst treating operands as 64-bit.
    ///
    /// Used for pointer arithmetic where 32-bit truncation would break addresses.
    SubRR64 { dst: Operand, src: Operand },

    /// `add dst, imm` - Add immediate to register (dst = dst + imm).
    AddRI { dst: Operand, imm: i32 },

    /// `imul dst, src` - Signed multiply (dst = dst * src).
    ImulRR { dst: Operand, src: Operand },

    /// `imul dst, src` (64-bit) - Signed multiply treating operands as 64-bit.
    ///
    /// Used for 64-bit multiplication where 32-bit truncation would give incorrect overflow detection.
    ImulRR64 { dst: Operand, src: Operand },

    /// `neg dst` - Two's complement negation (dst = -dst).
    Neg { dst: Operand },

    /// `neg dst` (64-bit) - Two's complement negation treating operand as 64-bit.
    ///
    /// Used for 64-bit negation where 32-bit truncation would give incorrect overflow detection.
    Neg64 { dst: Operand },

    /// `xor dst, imm` - XOR with immediate (dst = dst ^ imm).
    XorRI { dst: Operand, imm: i32 },

    /// `and dst, src` - Bitwise AND (dst = dst & src).
    AndRR { dst: Operand, src: Operand },

    /// `or dst, src` - Bitwise OR (dst = dst | src).
    OrRR { dst: Operand, src: Operand },

    /// `xor dst, src` - Bitwise XOR (dst = dst ^ src).
    XorRR { dst: Operand, src: Operand },

    /// `not dst` - Bitwise NOT (dst = ~dst).
    NotR { dst: Operand },

    /// `shl dst, cl` - Shift left 64-bit by count in CL register (dst = dst << CL).
    ShlRCl { dst: Operand },

    /// `shl dst, cl` - Shift left 32-bit by count in CL register (dst = dst << CL).
    Shl32RCl { dst: Operand },

    /// `shl dst, imm` - Shift left 64-bit by immediate (dst = dst << imm).
    ShlRI { dst: Operand, imm: u8 },

    /// `shl dst, imm` - Shift left 32-bit by immediate (dst = dst << imm).
    Shl32RI { dst: Operand, imm: u8 },

    /// `shr dst, cl` - Logical shift right 64-bit by count in CL register (dst = dst >> CL).
    ShrRCl { dst: Operand },

    /// `shr dst, cl` - Logical shift right 32-bit by count in CL register (dst = dst >> CL).
    Shr32RCl { dst: Operand },

    /// `shr dst, imm` - Logical shift right 64-bit by immediate (dst = dst >> imm).
    ShrRI { dst: Operand, imm: u8 },

    /// `shr dst, imm` - Logical shift right 32-bit by immediate (dst = dst >> imm).
    Shr32RI { dst: Operand, imm: u8 },

    /// `sar dst, cl` - Arithmetic shift right 64-bit by count in CL register.
    SarRCl { dst: Operand },

    /// `sar dst, cl` - Arithmetic shift right 32-bit by count in CL register.
    Sar32RCl { dst: Operand },

    /// `sar dst, imm` - Arithmetic shift right 64-bit by immediate.
    SarRI { dst: Operand, imm: u8 },

    /// `sar dst, imm` - Arithmetic shift right 32-bit by immediate.
    Sar32RI { dst: Operand, imm: u8 },

    /// `cdq` - Sign-extend EAX into EDX:EAX (for division).
    Cdq,

    /// `idiv src` - Signed divide EDX:EAX by src.
    /// Quotient in EAX, remainder in EDX.
    IdivR { src: Operand },

    // Comparison and control flow
    /// `cmp src1, src2` - Compare 32-bit (subtract and set flags, discard result).
    CmpRR { src1: Operand, src2: Operand },

    /// `cmp src1, src2` - Compare 64-bit (subtract and set flags, discard result).
    Cmp64RR { src1: Operand, src2: Operand },

    /// `cmp src, imm` - Compare register with immediate (32-bit).
    CmpRI { src: Operand, imm: i32 },

    /// `cmp src, imm` - Compare register with immediate (64-bit).
    Cmp64RI { src: Operand, imm: i32 },

    /// `sete dst` - Set byte if equal (ZF=1).
    Sete { dst: Operand },

    /// `setne dst` - Set byte if not equal (ZF=0).
    Setne { dst: Operand },

    /// `setl dst` - Set byte if less (signed: SF!=OF).
    Setl { dst: Operand },

    /// `setg dst` - Set byte if greater (signed: ZF=0 and SF=OF).
    Setg { dst: Operand },

    /// `setle dst` - Set byte if less or equal (signed: ZF=1 or SF!=OF).
    Setle { dst: Operand },

    /// `setge dst` - Set byte if greater or equal (signed: SF=OF).
    Setge { dst: Operand },

    /// `setb dst` - Set byte if below (unsigned: CF=1).
    Setb { dst: Operand },

    /// `seta dst` - Set byte if above (unsigned: CF=0 and ZF=0).
    Seta { dst: Operand },

    /// `setbe dst` - Set byte if below or equal (unsigned: CF=1 or ZF=1).
    Setbe { dst: Operand },

    /// `setae dst` - Set byte if above or equal (unsigned: CF=0).
    Setae { dst: Operand },

    /// `movzx dst, src` - Move with zero-extend (byte to dword).
    Movzx { dst: Operand, src: Operand },

    /// `movsx dst, src` - Sign-extend 8-bit to 64-bit.
    Movsx8To64 { dst: Operand, src: Operand },

    /// `movsx dst, src` - Sign-extend 16-bit to 64-bit.
    Movsx16To64 { dst: Operand, src: Operand },

    /// `movsxd dst, src` - Sign-extend 32-bit to 64-bit.
    Movsx32To64 { dst: Operand, src: Operand },

    /// `movzx dst, src` - Zero-extend 8-bit to 64-bit.
    Movzx8To64 { dst: Operand, src: Operand },

    /// `movzx dst, src` - Zero-extend 16-bit to 64-bit.
    Movzx16To64 { dst: Operand, src: Operand },

    /// `test src1, src2` - Bitwise AND, set flags, discard result.
    TestRR { src1: Operand, src2: Operand },

    /// `jz label` - Jump if zero flag is set.
    Jz { label: LabelId },

    /// `jnz label` - Jump if zero flag is not set.
    Jnz { label: LabelId },

    /// `jo label` - Jump if overflow flag is set.
    Jo { label: LabelId },

    /// `jno label` - Jump if overflow flag is not set.
    Jno { label: LabelId },

    /// `jb label` - Jump if below (unsigned: CF=1).
    Jb { label: LabelId },

    /// `jae label` - Jump if above or equal (unsigned: CF=0).
    Jae { label: LabelId },

    /// `jbe label` - Jump if below or equal (unsigned: CF=1 or ZF=1).
    Jbe { label: LabelId },

    /// `jge label` - Jump if greater or equal (signed: SF=OF).
    Jge { label: LabelId },

    /// `jle label` - Jump if less or equal (signed: ZF=1 or SF≠OF).
    Jle { label: LabelId },

    /// `jmp label` - Unconditional jump.
    Jmp { label: LabelId },

    /// Label marker (not a real instruction).
    Label { id: LabelId },

    /// `call symbol` - Call a function by symbol name (PC-relative).
    ///
    /// The symbol will be resolved by the linker. This emits a `call rel32`
    /// instruction with a relocation for the target address.
    ///
    /// The `symbol_id` is an index into the symbol table stored in `X86Mir`.
    CallRel { symbol_id: u32 },

    /// `syscall` - Invoke system call.
    Syscall,

    /// `ret` - Return from function.
    Ret,

    /// `pop dst` - Pop value from stack into register.
    Pop { dst: Operand },

    /// `push src` - Push value from register onto stack.
    Push { src: Operand },

    /// `lea dst, [base + disp]` - Load effective address.
    Lea {
        dst: Operand,
        base: Reg,
        index: Option<VReg>,
        scale: u8,
        disp: i32,
    },

    /// `shl dst, count` - Shift left (multiply by 2^count).
    Shl { dst: Operand, count: Operand },

    /// `mov dst, [base]` - Load from memory via register.
    MovRMIndexed {
        dst: Operand,
        base: VReg,
        offset: i32,
    },

    /// `mov [base], src` - Store to memory via register.
    MovMRIndexed {
        base: VReg,
        offset: i32,
        src: Operand,
    },

    /// Load pointer to string constant (pseudo-instruction resolved during emission)
    StringConstPtr { dst: Operand, string_id: u32 },

    /// Load string length (pseudo-instruction resolved during emission)
    StringConstLen { dst: Operand, string_id: u32 },

    /// Load string capacity (pseudo-instruction resolved during emission)
    /// For string literals, this is always 0 (indicating rodata, not heap)
    StringConstCap { dst: Operand, string_id: u32 },
}

impl X86Inst {
    /// Returns physical registers clobbered by this instruction.
    ///
    /// This information is used by the register allocator to avoid assigning
    /// virtual registers to physical registers that would be clobbered.
    pub fn clobbers(&self) -> &'static [Reg] {
        match self {
            // Division clobbers RAX (quotient) and RDX (remainder)
            X86Inst::IdivR { .. } => &[Reg::Rax, Reg::Rdx],
            // CDQ sign-extends EAX into EDX, clobbering RDX
            X86Inst::Cdq => &[Reg::Rdx],
            // Function calls clobber all caller-saved registers per System V AMD64 ABI
            X86Inst::CallRel { .. } => &[
                Reg::Rax,
                Reg::Rcx,
                Reg::Rdx,
                Reg::Rsi,
                Reg::Rdi,
                Reg::R8,
                Reg::R9,
                Reg::R10,
                Reg::R11,
            ],
            // Syscall clobbers RAX (return value), RCX (saved RIP), R11 (saved RFLAGS)
            X86Inst::Syscall => &[Reg::Rax, Reg::Rcx, Reg::R11],
            // All other instructions don't clobber additional registers
            _ => &[],
        }
    }
}

impl fmt::Display for X86Inst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            X86Inst::MovRI32 { dst, imm } => write!(f, "mov {}, {}", dst, imm),
            X86Inst::MovRI64 { dst, imm } => write!(f, "mov {}, {}", dst, imm),
            X86Inst::MovRR { dst, src } => write!(f, "mov {}, {}", dst, src),
            X86Inst::MovRM { dst, base, offset } => {
                if *offset >= 0 {
                    write!(f, "mov {}, [{}+{}]", dst, base, offset)
                } else {
                    write!(f, "mov {}, [{}-{}]", dst, base, -offset)
                }
            }
            X86Inst::MovMR { base, offset, src } => {
                if *offset >= 0 {
                    write!(f, "mov [{}+{}], {}", base, offset, src)
                } else {
                    write!(f, "mov [{}-{}], {}", base, -offset, src)
                }
            }
            X86Inst::AddRR { dst, src } => write!(f, "add {}, {}", dst, src),
            X86Inst::AddRR64 { dst, src } => write!(f, "addq {}, {}", dst, src),
            X86Inst::AddRI { dst, imm } => write!(f, "add {}, {}", dst, imm),
            X86Inst::SubRR { dst, src } => write!(f, "sub {}, {}", dst, src),
            X86Inst::SubRR64 { dst, src } => write!(f, "subq {}, {}", dst, src),
            X86Inst::ImulRR { dst, src } => write!(f, "imul {}, {}", dst, src),
            X86Inst::ImulRR64 { dst, src } => write!(f, "imulq {}, {}", dst, src),
            X86Inst::Neg { dst } => write!(f, "neg {}", dst),
            X86Inst::Neg64 { dst } => write!(f, "negq {}", dst),
            X86Inst::XorRI { dst, imm } => write!(f, "xor {}, {}", dst, imm),
            X86Inst::AndRR { dst, src } => write!(f, "and {}, {}", dst, src),
            X86Inst::OrRR { dst, src } => write!(f, "or {}, {}", dst, src),
            X86Inst::XorRR { dst, src } => write!(f, "xor {}, {}", dst, src),
            X86Inst::NotR { dst } => write!(f, "not {}", dst),
            X86Inst::ShlRCl { dst } => write!(f, "shlq {}, cl", dst),
            X86Inst::Shl32RCl { dst } => write!(f, "shll {}, cl", dst),
            X86Inst::ShlRI { dst, imm } => write!(f, "shlq {}, {}", dst, imm),
            X86Inst::Shl32RI { dst, imm } => write!(f, "shll {}, {}", dst, imm),
            X86Inst::ShrRCl { dst } => write!(f, "shrq {}, cl", dst),
            X86Inst::Shr32RCl { dst } => write!(f, "shrl {}, cl", dst),
            X86Inst::ShrRI { dst, imm } => write!(f, "shrq {}, {}", dst, imm),
            X86Inst::Shr32RI { dst, imm } => write!(f, "shrl {}, {}", dst, imm),
            X86Inst::SarRCl { dst } => write!(f, "sarq {}, cl", dst),
            X86Inst::Sar32RCl { dst } => write!(f, "sarl {}, cl", dst),
            X86Inst::SarRI { dst, imm } => write!(f, "sarq {}, {}", dst, imm),
            X86Inst::Sar32RI { dst, imm } => write!(f, "sarl {}, {}", dst, imm),
            X86Inst::Cdq => write!(f, "cdq"),
            X86Inst::IdivR { src } => write!(f, "idiv {}", src),
            X86Inst::CmpRR { src1, src2 } => write!(f, "cmp {}, {}", src1, src2),
            X86Inst::Cmp64RR { src1, src2 } => write!(f, "cmpq {}, {}", src1, src2),
            X86Inst::CmpRI { src, imm } => write!(f, "cmp {}, {}", src, imm),
            X86Inst::Cmp64RI { src, imm } => write!(f, "cmpq {}, {}", src, imm),
            X86Inst::Sete { dst } => write!(f, "sete {}", dst),
            X86Inst::Setne { dst } => write!(f, "setne {}", dst),
            X86Inst::Setl { dst } => write!(f, "setl {}", dst),
            X86Inst::Setg { dst } => write!(f, "setg {}", dst),
            X86Inst::Setle { dst } => write!(f, "setle {}", dst),
            X86Inst::Setge { dst } => write!(f, "setge {}", dst),
            X86Inst::Setb { dst } => write!(f, "setb {}", dst),
            X86Inst::Seta { dst } => write!(f, "seta {}", dst),
            X86Inst::Setbe { dst } => write!(f, "setbe {}", dst),
            X86Inst::Setae { dst } => write!(f, "setae {}", dst),
            X86Inst::Movzx { dst, src } => write!(f, "movzx {}, {}", dst, src),
            X86Inst::Movsx8To64 { dst, src } => write!(f, "movsx {}, byte {}", dst, src),
            X86Inst::Movsx16To64 { dst, src } => write!(f, "movsx {}, word {}", dst, src),
            X86Inst::Movsx32To64 { dst, src } => write!(f, "movsxd {}, {}", dst, src),
            X86Inst::Movzx8To64 { dst, src } => write!(f, "movzx {}, byte {}", dst, src),
            X86Inst::Movzx16To64 { dst, src } => write!(f, "movzx {}, word {}", dst, src),
            X86Inst::TestRR { src1, src2 } => write!(f, "test {}, {}", src1, src2),
            X86Inst::Jz { label } => write!(f, "jz {}", label),
            X86Inst::Jnz { label } => write!(f, "jnz {}", label),
            X86Inst::Jo { label } => write!(f, "jo {}", label),
            X86Inst::Jno { label } => write!(f, "jno {}", label),
            X86Inst::Jb { label } => write!(f, "jb {}", label),
            X86Inst::Jae { label } => write!(f, "jae {}", label),
            X86Inst::Jbe { label } => write!(f, "jbe {}", label),
            X86Inst::Jge { label } => write!(f, "jge {}", label),
            X86Inst::Jle { label } => write!(f, "jle {}", label),
            X86Inst::Jmp { label } => write!(f, "jmp {}", label),
            X86Inst::Label { id } => write!(f, "{}:", id),
            X86Inst::CallRel { symbol_id } => write!(f, "call sym{}", symbol_id),
            X86Inst::Syscall => write!(f, "syscall"),
            X86Inst::Ret => write!(f, "ret"),
            X86Inst::Pop { dst } => write!(f, "pop {}", dst),
            X86Inst::Push { src } => write!(f, "push {}", src),
            X86Inst::Lea {
                dst,
                base,
                index,
                scale: _,
                disp,
            } => {
                if let Some(_idx) = index {
                    // With index register
                    if *disp >= 0 {
                        write!(f, "lea {}, [{}+{}]", dst, base, disp)
                    } else {
                        write!(f, "lea {}, [{}-{}]", dst, base, -disp)
                    }
                } else if *disp >= 0 {
                    write!(f, "lea {}, [{}+{}]", dst, base, disp)
                } else {
                    write!(f, "lea {}, [{}-{}]", dst, base, -disp)
                }
            }
            X86Inst::Shl { dst, count } => write!(f, "shl {}, {}", dst, count),
            X86Inst::MovRMIndexed { dst, base, offset } => {
                if *offset == 0 {
                    write!(f, "mov {}, [{}]", dst, base)
                } else if *offset > 0 {
                    write!(f, "mov {}, [{}+{}]", dst, base, offset)
                } else {
                    write!(f, "mov {}, [{}-{}]", dst, base, -offset)
                }
            }
            X86Inst::MovMRIndexed { base, offset, src } => {
                if *offset == 0 {
                    write!(f, "mov [{}], {}", base, src)
                } else if *offset > 0 {
                    write!(f, "mov [{}+{}], {}", base, offset, src)
                } else {
                    write!(f, "mov [{}-{}], {}", base, -offset, src)
                }
            }
            X86Inst::StringConstPtr { dst, string_id } => {
                write!(f, "string_const_ptr {}, str{}", dst, string_id)
            }
            X86Inst::StringConstLen { dst, string_id } => {
                write!(f, "string_const_len {}, str{}", dst, string_id)
            }
            X86Inst::StringConstCap { dst, string_id } => {
                write!(f, "string_const_cap {}, str{}", dst, string_id)
            }
        }
    }
}

/// X86-64 MIR for a function.
#[derive(Debug, Default)]
pub struct X86Mir {
    /// The instructions in this function.
    instructions: Vec<X86Inst>,
    /// The next virtual register index.
    next_vreg: u32,
    /// The next label index.
    next_label: u32,
    /// Symbol table for call targets.
    ///
    /// Stores symbol names indexed by `symbol_id` in `CallRel` instructions.
    /// This avoids heap-allocating a String for every call instruction.
    symbols: Vec<String>,
    /// Index for O(1) symbol lookup during interning.
    ///
    /// Maps symbol names to their indices in the `symbols` vector.
    symbol_index: HashMap<String, u32>,
}

impl X86Mir {
    /// Create a new empty X86Mir.
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            next_vreg: 0,
            next_label: 0,
            symbols: Vec::new(),
            symbol_index: HashMap::new(),
        }
    }

    /// Intern a symbol name and return its ID.
    ///
    /// If the symbol already exists, returns its existing ID.
    /// Otherwise, adds it to the table and returns the new ID.
    pub fn intern_symbol(&mut self, symbol: &str) -> u32 {
        // O(1) lookup via HashMap
        if let Some(&idx) = self.symbol_index.get(symbol) {
            return idx;
        }
        // Add new symbol
        let idx = self.symbols.len() as u32;
        let owned = symbol.to_string();
        self.symbol_index.insert(owned.clone(), idx);
        self.symbols.push(owned);
        idx
    }

    /// Get a symbol name by its ID.
    ///
    /// # Panics
    /// Panics if the symbol_id is out of bounds.
    #[inline]
    pub fn get_symbol(&self, symbol_id: u32) -> &str {
        &self.symbols[symbol_id as usize]
    }

    /// Get the symbol table.
    #[inline]
    pub fn symbols(&self) -> &[String] {
        &self.symbols
    }

    /// Take ownership of the symbol table.
    ///
    /// Used during register allocation to transfer symbols to the new MIR.
    pub fn take_symbols(&mut self) -> Vec<String> {
        self.symbol_index.clear();
        std::mem::take(&mut self.symbols)
    }

    /// Set the symbol table.
    ///
    /// Used during register allocation to restore symbols from the old MIR.
    pub fn set_symbols(&mut self, symbols: Vec<String>) {
        // Rebuild the index from the symbol table
        self.symbol_index.clear();
        for (idx, sym) in symbols.iter().enumerate() {
            self.symbol_index.insert(sym.clone(), idx as u32);
        }
        self.symbols = symbols;
    }

    /// Allocate a new virtual register.
    pub fn alloc_vreg(&mut self) -> VReg {
        let vreg = VReg::new(self.next_vreg);
        self.next_vreg += 1;
        vreg
    }

    /// Allocate a new label ID.
    pub fn alloc_label(&mut self) -> LabelId {
        let label = LabelId::new(self.next_label);
        self.next_label += 1;
        label
    }

    /// Get the number of virtual registers allocated.
    #[inline]
    pub fn vreg_count(&self) -> u32 {
        self.next_vreg
    }

    /// Get the number of instructions.
    #[inline]
    pub fn inst_count(&self) -> usize {
        self.instructions.len()
    }

    /// Add an instruction.
    pub fn push(&mut self, inst: X86Inst) {
        self.instructions.push(inst);
    }

    /// Get the instructions.
    #[inline]
    pub fn instructions(&self) -> &[X86Inst] {
        &self.instructions
    }

    /// Get mutable access to instructions (for register allocation).
    #[inline]
    pub fn instructions_mut(&mut self) -> &mut [X86Inst] {
        &mut self.instructions
    }

    /// Get mutable access to the instruction vector (for peephole optimization).
    #[inline]
    pub fn instructions_vec_mut(&mut self) -> &mut Vec<X86Inst> {
        &mut self.instructions
    }

    /// Iterate over instructions.
    pub fn iter(&self) -> impl Iterator<Item = &X86Inst> {
        self.instructions.iter()
    }

    /// Consume the MIR and return its instructions.
    pub fn into_instructions(self) -> Vec<X86Inst> {
        self.instructions
    }
}

impl fmt::Display for X86Mir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for inst in &self.instructions {
            // Special handling for CallRel to show actual symbol name
            if let X86Inst::CallRel { symbol_id } = inst {
                writeln!(f, "    call {}", self.get_symbol(*symbol_id))?;
            } else {
                writeln!(f, "    {}", inst)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vreg_allocation() {
        let mut mir = X86Mir::new();
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
        assert_eq!(Reg::Rax.encoding(), 0);
        assert_eq!(Reg::Rdi.encoding(), 7);
        assert_eq!(Reg::R8.encoding(), 8);
        assert_eq!(Reg::R15.encoding(), 15);
    }

    #[test]
    fn test_reg_needs_rex() {
        assert!(!Reg::Rax.needs_rex());
        assert!(!Reg::Rdi.needs_rex());
        assert!(Reg::R8.needs_rex());
        assert!(Reg::R15.needs_rex());
    }

    #[test]
    fn test_instruction_display() {
        let inst = X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rdi),
            imm: 42,
        };
        assert_eq!(format!("{}", inst), "mov rdi, 42");
    }
}
