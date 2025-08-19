//! x86-64 target architecture definitions
//!
//! This module contains x86-64 specific types for registers, instructions,
//! and related enums used in code generation.

/// x86-64 registers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum X86Register {
    Rax, // Accumulator, return value
    Rbx, // Base
    Rcx, // Counter
    Rdx, // Data
    Rsp, // Stack pointer
    Rbp, // Base pointer
    Rsi, // Source index
    Rdi, // Destination index
    R8,  // Extended registers
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
}

impl X86Register {
    /// Check if register requires REX prefix (R8-R15)
    #[inline]
    pub fn needs_rex(&self) -> bool {
        matches!(
            self,
            X86Register::R8
                | X86Register::R9
                | X86Register::R10
                | X86Register::R11
                | X86Register::R12
                | X86Register::R13
                | X86Register::R14
                | X86Register::R15
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionCode {
    Equal,        // ZF=1
    NotEqual,     // ZF=0
    Less,         // SF≠OF
    LessEqual,    // ZF=1 or SF≠OF
    Greater,      // ZF=0 and SF=OF
    GreaterEqual, // SF=OF
    Below,        // CF=1 (unsigned less than)
    BelowEqual,   // CF=1 or ZF=1 (unsigned less than or equal)
    Above,        // CF=0 and ZF=0 (unsigned greater than)
    AboveEqual,   // CF=0 (unsigned greater than or equal)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelRef {
    Local(u32),     // Local label ID
    Global(String), // Global symbol name
}

/// x86-64 specific machine instructions
/// These map directly to x86 opcodes with concrete registers
#[derive(Debug, Clone)]
pub enum X8664Instr {
    /// mov dest, src (register to register)
    MovRR { dest: X86Register, src: X86Register },

    /// mov dest, imm32 (sign-extended to 64-bit)
    MovRI32 { dest: X86Register, imm: i32 },

    /// mov dest, imm64
    MovRI64 { dest: X86Register, imm: i64 },

    /// mov dest, [rbp + offset] (load from stack)
    MovRM {
        dest: X86Register,
        base: X86Register,
        offset: i32,
    },

    /// mov [rbp + offset], src (store to stack)
    MovMR {
        base: X86Register,
        offset: i32,
        src: X86Register,
    },

    /// mov byte ptr [base + offset], src (store byte to memory)
    MovMR8 {
        base: X86Register,
        offset: i32,
        src: X86Register,
    },

    /// mov dest, byte ptr [base + offset] (load byte from memory)
    MovRM8 {
        dest: X86Register,
        base: X86Register,
        offset: i32,
    },

    /// mov dest, dword ptr [base + offset] (load 32-bit from memory)
    MovRM32 {
        dest: X86Register,
        base: X86Register,
        offset: i32,
    },

    /// mov dword ptr [base + offset], src (store 32-bit to memory)
    MovMR32 {
        base: X86Register,
        offset: i32,
        src: X86Register,
    },

    /// add dest, src
    AddRR { dest: X86Register, src: X86Register },

    /// add dest, imm32
    AddRI { dest: X86Register, imm: i32 },

    /// add dest, src (32-bit)
    AddRR32 { dest: X86Register, src: X86Register },

    /// add dest, imm32 (32-bit)
    AddRI32 { dest: X86Register, imm: i32 },

    /// sub dest, src
    SubRR { dest: X86Register, src: X86Register },

    /// sub dest, imm32
    SubRI { dest: X86Register, imm: i32 },

    /// sub dest, src (32-bit)
    SubRR32 { dest: X86Register, src: X86Register },

    /// sub dest, imm32 (32-bit)
    SubRI32 { dest: X86Register, imm: i32 },

    /// xor dest, src
    XorRR { dest: X86Register, src: X86Register },

    /// imul dest, src
    ImulRR { dest: X86Register, src: X86Register },

    /// imul dest, dest, imm32
    ImulRI { dest: X86Register, imm: i32 },

    /// imul dest, src (32-bit)
    ImulRR32 { dest: X86Register, src: X86Register },

    /// imul dest, dest, imm32 (32-bit)
    ImulRI32 { dest: X86Register, imm: i32 },

    /// and dest, src
    AndRR { dest: X86Register, src: X86Register },

    /// and dest, imm32
    AndRI { dest: X86Register, imm: i32 },

    /// shl dest, cl (shift left)
    Shl {
        dest: X86Register,
        count: X86Register,
    },

    /// sar dest, cl (arithmetic shift right)
    Sar {
        dest: X86Register,
        count: X86Register,
    },

    /// idiv divisor (signed division, dividend in RAX, quotient in RAX, remainder in RDX)
    Idiv { divisor: X86Register },

    /// cqo (sign extend RAX to RDX:RAX)
    Cqo,

    /// cmp left, right
    CmpRR {
        left: X86Register,
        right: X86Register,
    },

    /// cmp reg, imm32
    CmpRI { reg: X86Register, imm: i32 },

    /// setcc dest (set byte based on condition code)
    SetCC {
        dest: X86Register,
        cc: ConditionCode,
    },

    /// movzx dest, src (zero extend byte to qword)
    Movzx { dest: X86Register, src: X86Register },

    /// movsxd dest, src (sign extend dword to qword)
    Movsxd { dest: X86Register, src: X86Register },

    /// push reg
    Push { reg: X86Register },

    /// pop reg
    Pop { reg: X86Register },

    /// call label
    Call { target: String },

    /// ret
    Ret,

    /// jmp label
    Jmp { target: LabelRef },

    /// jcc label (conditional jump)
    JmpCC { cc: ConditionCode, target: LabelRef },

    /// label definition
    Label { id: u32 },

    /// lea dest, [rip + label] - Load effective address of label
    LeaLabel { dest: X86Register, label: String },

    /// cld - Clear direction flag
    Cld,

    /// cpuid - CPU identification instruction
    Cpuid,

    /// bt reg, imm8 - Bit test
    BtRI { reg: X86Register, bit: u8 },

    /// test reg, reg - Test (sets flags without modifying)
    TestRR {
        left: X86Register,
        right: X86Register,
    },

    /// loop target - Decrement RCX and jump if not zero
    Loop { target: LabelRef },

    /// inc reg - Increment register
    IncR { reg: X86Register },

    /// dec reg - Decrement register
    DecR { reg: X86Register },

    /// shr reg, imm8 - Shift right immediate
    ShrRI { dest: X86Register, imm: u8 },

    /// rep movsq - Repeat move qword (8-byte copy)
    RepMovsq,

    /// rep stosq - Repeat store qword (8-byte fill)
    RepStosq,

    /// movzx dest32, src8 - Move with zero extension (byte to dword)
    Movzx8to32 { dest: X86Register, src: X86Register },

    /// imul reg, reg, imm64 - Multiply by large immediate
    ImulRI64 { dest: X86Register, imm64: i64 },

    /// mov word ptr [base + offset], src (16-bit store)
    MovMR16 {
        base: X86Register,
        offset: i32,
        src: X86Register,
    },

    /// mov dest, word ptr [base + offset] (16-bit load)
    MovRM16 {
        dest: X86Register,
        base: X86Register,
        offset: i32,
    },

    /// rep stosb - Repeat store byte (fill memory)
    RepStosb,

    /// rep movsb - Repeat move byte (copy memory)
    RepMovsb,

    /// std - Set direction flag (for backward copy)
    Std,

    /// syscall
    Syscall,

    /// Stack frame management
    /// push rbp; mov rbp, rsp
    EnterFrame,

    /// mov rsp, rbp; pop rbp
    LeaveFrame,

    /// sub rsp, size (allocate stack space)
    AllocStack { size: u32 },

    /// Data section instructions
    /// Raw byte data (db equivalent)
    DataBytes { bytes: Vec<u8> },

    /// Zero-initialized space reservation (resb equivalent)
    ReserveBytes { count: u32 },

    /// ud2 - undefined instruction (causes SIGILL)
    Ud2,

    /// Section directive for assembly generation
    Section { name: String },
}
