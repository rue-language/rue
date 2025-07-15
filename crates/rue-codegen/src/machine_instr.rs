use crate::Register;

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
