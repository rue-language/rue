//! Target-level IR (machine instructions)
//!
//! This module defines the low-level IR that maps directly to machine instructions.

/// x86-64 registers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Register {
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

impl Register {
    /// Check if register requires REX prefix (R8-R15)
    #[inline]
    pub fn needs_rex(&self) -> bool {
        matches!(
            self,
            Register::R8
                | Register::R9
                | Register::R10
                | Register::R11
                | Register::R12
                | Register::R13
                | Register::R14
                | Register::R15
        )
    }
}

/// x86-64 specific machine instructions
/// These map directly to x86 opcodes with concrete registers
#[derive(Debug, Clone)]
pub enum MachineInstr {
    /// mov dest, src (register to register)
    MovRR { dest: Register, src: Register },

    /// mov dest, imm32 (sign-extended to 64-bit)
    MovRI32 { dest: Register, imm: i32 },

    /// mov dest, imm64
    MovRI64 { dest: Register, imm: i64 },

    /// mov dest, [rbp + offset] (load from stack)
    MovRM {
        dest: Register,
        base: Register,
        offset: i32,
    },

    /// mov [rbp + offset], src (store to stack)
    MovMR {
        base: Register,
        offset: i32,
        src: Register,
    },

    /// mov byte ptr [base + offset], src (store byte to memory)
    MovMR8 {
        base: Register,
        offset: i32,
        src: Register,
    },

    /// mov dest, byte ptr [base + offset] (load byte from memory)
    MovRM8 {
        dest: Register,
        base: Register,
        offset: i32,
    },

    /// add dest, src
    AddRR { dest: Register, src: Register },

    /// add dest, imm32
    AddRI { dest: Register, imm: i32 },

    /// sub dest, src
    SubRR { dest: Register, src: Register },

    /// sub dest, imm32
    SubRI { dest: Register, imm: i32 },

    /// imul dest, src
    ImulRR { dest: Register, src: Register },

    /// imul dest, dest, imm32
    ImulRI { dest: Register, imm: i32 },

    /// and dest, src
    AndRR { dest: Register, src: Register },

    /// shl dest, cl (shift left)
    Shl { dest: Register, count: Register },

    /// sar dest, cl (arithmetic shift right)
    Sar { dest: Register, count: Register },

    /// idiv divisor (signed division, dividend in RAX, quotient in RAX, remainder in RDX)
    Idiv { divisor: Register },

    /// cqo (sign extend RAX to RDX:RAX)
    Cqo,

    /// cmp left, right
    CmpRR { left: Register, right: Register },

    /// cmp reg, imm32
    CmpRI { reg: Register, imm: i32 },

    /// setcc dest (set byte based on condition code)
    SetCC { dest: Register, cc: ConditionCode },

    /// movzx dest, src (zero extend byte to qword)
    Movzx { dest: Register, src: Register },

    /// push reg
    Push { reg: Register },

    /// pop reg
    Pop { reg: Register },

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
    LeaLabel { dest: Register, label: String },

    /// cld - Clear direction flag
    Cld,

    /// rep stosb - Repeat store byte (fill memory)
    RepStosb,

    /// syscall
    Syscall,

    /// Stack frame management
    /// push rbp; mov rbp, rsp
    EnterFrame,

    /// mov rsp, rbp; pop rbp
    LeaveFrame,

    /// sub rsp, size (allocate stack space)
    AllocStack { size: u32 },
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
