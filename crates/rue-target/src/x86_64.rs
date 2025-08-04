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

    /// add dest, src
    AddRR { dest: X86Register, src: X86Register },

    /// add dest, imm32
    AddRI { dest: X86Register, imm: i32 },

    /// sub dest, src
    SubRR { dest: X86Register, src: X86Register },

    /// sub dest, imm32
    SubRI { dest: X86Register, imm: i32 },

    /// xor dest, src
    XorRR { dest: X86Register, src: X86Register },

    /// imul dest, src
    ImulRR { dest: X86Register, src: X86Register },

    /// imul dest, dest, imm32
    ImulRI { dest: X86Register, imm: i32 },

    /// and dest, src
    AndRR { dest: X86Register, src: X86Register },

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
}
