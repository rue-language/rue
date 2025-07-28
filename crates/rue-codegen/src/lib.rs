// High-level code generation using HIR (High-level Intermediate Representation)
//
// This crate handles code generation from HIR to executable binaries.
// The old AST-based code generation has been removed in favor of HIR-based compilation.

use std::collections::HashMap;

// Internal modules
mod backend;
mod compile_hir_with_mir;
mod elf_writer;
mod lowering;
mod mir_to_instructions;
mod regalloc;
mod util;
mod x86_emitter;

// Re-export all public API through a single module
pub mod compile {
    pub use crate::compile_hir_with_mir::{
        compile_hir_via_mir_to_assembly, compile_hir_via_mir_to_executable,
    };

    /// Wrapper for compile_hir_via_mir_to_executable that matches the signature
    pub fn compile_hir_via_mir_to_bytes(
        hir: &rue_ir::hir::HirProgram,
        enable_optimizations: bool,
    ) -> Result<Vec<u8>, crate::CodegenError> {
        compile_hir_via_mir_to_executable(hir, enable_optimizations)
    }
}

// Re-export internals for advanced usage
pub mod internals {
    pub use crate::format_instructions_as_assembly;
    pub use crate::lowering::Lowering;
    pub use crate::regalloc::RegisterAllocator;
    pub use crate::x86_emitter::X86Emitter;
}

// Import types from rue-ir
use rue_ir::target::{LabelRef, MachineInstr, Register};

#[derive(Debug, Clone, PartialEq)]
pub enum CodegenError {
    UnsupportedInstruction(String),
    UndefinedLabel(String),
    RegisterAllocation(String),
    Io,
    InvalidOperation(String),
    MirVerificationFailed,
    Lowering(crate::lowering::LoweringError),
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodegenError::UnsupportedInstruction(msg) => {
                write!(f, "Unsupported instruction: {msg}")
            }
            CodegenError::UndefinedLabel(label) => write!(f, "Undefined label: {label}"),
            CodegenError::RegisterAllocation(msg) => {
                write!(f, "Register allocation error: {msg}")
            }
            CodegenError::Io => write!(f, "I/O error"),
            CodegenError::InvalidOperation(msg) => write!(f, "Invalid operation: {msg}"),
            CodegenError::MirVerificationFailed => {
                write!(f, "MIR verification failed")
            }
            CodegenError::Lowering(err) => write!(f, "Lowering error: {err}"),
        }
    }
}

impl std::error::Error for CodegenError {}

/// Virtual register - will be allocated to a physical register or stack slot
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VReg(pub u32);

/// Value operand for instructions
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    VReg(VReg),
    SignedImm(i64),
    UnsignedImm(u64),
    PhysicalReg(Register),
}

/// Binary operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// Label space to distinguish between runtime and user labels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabelSpace {
    Runtime,
    User,
}

/// Type-safe label that tracks which space it belongs to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Label {
    id: u32,
    space: LabelSpace,
}

impl Label {
    /// Create a new runtime label
    pub fn runtime(id: u32) -> Self {
        Label {
            id,
            space: LabelSpace::Runtime,
        }
    }

    /// Create a new user label
    pub fn user(id: u32) -> Self {
        Label {
            id,
            space: LabelSpace::User,
        }
    }

    /// Get the raw ID without offset
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Get the label space
    pub fn space(&self) -> LabelSpace {
        self.space
    }

    /// Convert to a machine label ID with proper offset
    /// Runtime labels keep their IDs, user labels are offset by runtime_label_count
    pub fn to_machine_id(&self, runtime_label_count: u32) -> u32 {
        match self.space {
            LabelSpace::Runtime => self.id,
            LabelSpace::User => self.id + runtime_label_count,
        }
    }

    /// Create a label from a machine ID, determining its space based on offset
    pub fn from_machine_id(machine_id: u32, runtime_label_count: u32) -> Self {
        // Use <= to correctly handle the case when runtime_label_count == 0
        if runtime_label_count == 0 || machine_id >= runtime_label_count {
            Label::user(machine_id - runtime_label_count)
        } else {
            Label::runtime(machine_id)
        }
    }
}

/// Platform-independent instruction set
///
/// Examples:
/// - `2 + 3` generates: Copy{v0, Imm(2)}, Copy{v1, Imm(3)}, BinaryOp{v2, v0, v1, Add}
/// - `x = 42` generates: Copy{v0, Imm(42)}, then maps variable "x" to v0
/// - `n * factorial(n-1)` generates: Push{v0}, Call{v1, "factorial", [v2]}, Pop{v3}, BinaryOp{v4, v3, v1, Mul}
#[derive(Debug, Clone)]
pub enum Instruction {
    // Data movement
    Copy {
        dest: VReg,
        src: Value,
    },
    // Block parameter assignment - for MIR block parameters (never reads dest's old value)
    BlockParamAssign {
        dest: VReg,
        src: Value,
    },

    // Arithmetic and comparison operations
    BinaryOp {
        dest: VReg,
        lhs: Value,
        rhs: Value,
        op: BinOp,
    },

    // Memory operations
    Load {
        dest: VReg,
        offset: i64,
    }, // Load from stack
    Store {
        src: VReg,
        offset: i64,
    }, // Store to stack

    // Stack operations for value preservation
    Push {
        src: Value,
    }, // Push register to stack
    Pop {
        dest: VReg,
    }, // Pop from stack to register

    // Control flow
    Label(Label),
    Jump(Label),
    Branch {
        condition: VReg,
        true_label: Label,
        false_label: Label,
    },

    // Function operations
    Call {
        dest: Option<VReg>,
        function: String,
        args: Vec<VReg>,
    },
    Return {
        value: Option<VReg>,
    },

    // System operations
    Syscall {
        result: VReg,
        syscall_num: VReg,
        args: Vec<VReg>,
    },

    // Register preservation for calling convention
    SaveRegisters {
        registers: Vec<Register>,
    },
    RestoreRegisters {
        registers: Vec<Register>,
    },

    // Stack frame management
    EnterFrame,
    LeaveFrame,
}

// Convert machine instructions to AT&T assembly syntax
pub fn format_instructions_as_assembly(
    instructions: &[MachineInstr],
    function_labels: &HashMap<String, Label>,
    runtime_label_count: u32,
) -> String {
    use rue_ir::target::ConditionCode;

    let mut output = String::new();

    // Assembly header
    output.push_str(".section .text\n");
    output.push_str(".global _start\n\n");

    // Reverse map for labels
    let mut label_names: HashMap<u32, String> = HashMap::new();
    for (name, label) in function_labels {
        label_names.insert(label.to_machine_id(runtime_label_count), name.clone());
    }

    for instr in instructions {
        match instr {
            MachineInstr::Label { id } => {
                if let Some(name) = label_names.get(id) {
                    output.push_str(&format!("{name}:\n"));
                } else {
                    output.push_str(&format!(".L{id}:\n"));
                }
            }
            MachineInstr::MovRR { dest, src } => {
                output.push_str(&format!(
                    "    movq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::MovRI32 { dest, imm } => {
                // MovRI32 is encoded as sign-extending mov imm32 → r64 with REX.W
                // Use movq to reflect the actual 64-bit sign-extending behavior
                output.push_str(&format!("    movq ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::MovRI64 { dest, imm } => {
                output.push_str(&format!("    movabsq ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::MovRM { dest, base, offset } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movq (%{}), %{}\n",
                        reg_name(base),
                        reg_name(dest)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movq {:+}(%{}), %{}\n",
                        offset,
                        reg_name(base),
                        reg_name(dest)
                    ));
                }
            }
            MachineInstr::MovMR { base, offset, src } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movq %{}, (%{})\n",
                        reg_name(src),
                        reg_name(base)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movq %{}, {:+}(%{})\n",
                        reg_name(src),
                        offset,
                        reg_name(base)
                    ));
                }
            }
            MachineInstr::MovMR8 { base, offset, src } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movb %{}, (%{})\n",
                        reg_name_8(src),
                        reg_name(base)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movb %{}, {:+}(%{})\n",
                        reg_name_8(src),
                        offset,
                        reg_name(base)
                    ));
                }
            }
            MachineInstr::MovRM8 { dest, base, offset } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movb (%{}), %{}\n",
                        reg_name(base),
                        reg_name_8(dest)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movb {:+}(%{}), %{}\n",
                        offset,
                        reg_name(base),
                        reg_name_8(dest)
                    ));
                }
            }
            MachineInstr::AddRR { dest, src } => {
                output.push_str(&format!(
                    "    addq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::AddRI { dest, imm } => {
                output.push_str(&format!("    addq ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::SubRR { dest, src } => {
                output.push_str(&format!(
                    "    subq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::SubRI { dest, imm } => {
                output.push_str(&format!("    subq ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::ImulRR { dest, src } => {
                output.push_str(&format!(
                    "    imulq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::ImulRI { dest, imm } => {
                output.push_str(&format!("    imulq ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::Idiv { divisor } => {
                output.push_str(&format!("    idivq %{}\n", reg_name(divisor)));
            }
            MachineInstr::Cqo => {
                output.push_str("    cqo\n");
            }
            MachineInstr::AndRR { dest, src } => {
                output.push_str(&format!(
                    "    andq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::Shl { dest, count: _ } => {
                output.push_str(&format!("    shlq %cl, %{}\n", reg_name(dest)));
            }
            MachineInstr::Sar { dest, count: _ } => {
                output.push_str(&format!("    sarq %cl, %{}\n", reg_name(dest)));
            }
            MachineInstr::CmpRR { left, right } => {
                output.push_str(&format!(
                    "    cmpq %{}, %{}\n",
                    reg_name(right),
                    reg_name(left)
                ));
            }
            MachineInstr::CmpRI { reg, imm } => {
                output.push_str(&format!("    cmpq ${}, %{}\n", imm, reg_name(reg)));
            }
            MachineInstr::SetCC { cc, dest } => {
                let cc_str = match cc {
                    ConditionCode::Equal => "sete",
                    ConditionCode::NotEqual => "setne",
                    ConditionCode::Less => "setl",
                    ConditionCode::LessEqual => "setle",
                    ConditionCode::Greater => "setg",
                    ConditionCode::GreaterEqual => "setge",
                };
                output.push_str(&format!("    {} %{}\n", cc_str, reg_name_8(dest)));
            }
            MachineInstr::Movzx { dest, src } => {
                output.push_str(&format!(
                    "    movzbq %{}, %{}\n",
                    reg_name_8(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::Movsxd { dest, src } => {
                output.push_str(&format!(
                    "    movslq %{}, %{}\n",
                    reg_name_32(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::Push { reg } => {
                output.push_str(&format!("    pushq %{}\n", reg_name(reg)));
            }
            MachineInstr::Pop { reg } => {
                output.push_str(&format!("    popq %{}\n", reg_name(reg)));
            }
            MachineInstr::Call { target } => {
                output.push_str(&format!("    call {target}\n"));
            }
            MachineInstr::Ret => {
                output.push_str("    ret\n");
            }
            MachineInstr::Jmp { target } => match target {
                LabelRef::Local(id) => {
                    if let Some(name) = label_names.get(id) {
                        output.push_str(&format!("    jmp {name}\n"));
                    } else {
                        output.push_str(&format!("    jmp .L{id}\n"));
                    }
                }
                LabelRef::Global(name) => {
                    output.push_str(&format!("    jmp {name}\n"));
                }
            },
            MachineInstr::JmpCC { cc, target } => {
                let cc_str = match cc {
                    ConditionCode::Equal => "je",
                    ConditionCode::NotEqual => "jne",
                    ConditionCode::Less => "jl",
                    ConditionCode::LessEqual => "jle",
                    ConditionCode::Greater => "jg",
                    ConditionCode::GreaterEqual => "jge",
                };
                match target {
                    LabelRef::Local(id) => {
                        if let Some(name) = label_names.get(id) {
                            output.push_str(&format!("    {cc_str} {name}\n"));
                        } else {
                            output.push_str(&format!("    {cc_str} .L{id}\n"));
                        }
                    }
                    LabelRef::Global(name) => {
                        output.push_str(&format!("    {cc_str} {name}\n"));
                    }
                }
            }
            MachineInstr::Syscall => {
                output.push_str("    syscall\n");
            }
            MachineInstr::AllocStack { size } => {
                // Align size to 16 bytes as done by the encoder
                let aligned_size = crate::util::align_to_16(*size as u64);
                output.push_str(&format!("    subq ${aligned_size}, %rsp\n"));
            }
            MachineInstr::LeaLabel { dest, label } => {
                output.push_str(&format!("    leaq {}(%rip), %{}\n", label, reg_name(dest)));
            }
            MachineInstr::Cld => {
                output.push_str("    cld\n");
            }
            MachineInstr::RepStosb => {
                output.push_str("    rep stosb\n");
            }
            MachineInstr::EnterFrame => {
                output.push_str("    pushq %rbp\n");
                output.push_str("    movq %rsp, %rbp\n");
            }
            MachineInstr::LeaveFrame => {
                output.push_str("    movq %rbp, %rsp\n");
                output.push_str("    popq %rbp\n");
            }
            MachineInstr::XorRR { dest, src } => {
                output.push_str(&format!(
                    "    xorq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::Ud2 => {
                output.push_str("    ud2\n");
            }
            MachineInstr::DataBytes { bytes } => {
                // Emit raw data bytes using .byte directive
                for byte in bytes {
                    output.push_str(&format!("    .byte {byte}\n"));
                }
            }
            MachineInstr::ReserveBytes { count } => {
                // Reserve uninitialized space using .space directive
                output.push_str(&format!("    .space {count}\n"));
            }
        }
    }

    output
}

// Macro to generate register name tables consistently
macro_rules! register_names {
    (
        $(
            $reg:ident => { r64: $r64:literal, r32: $r32:literal, r8: $r8:literal }
        ),+ $(,)?
    ) => {
        fn reg_name(reg: &Register) -> &'static str {
            match reg {
                $(Register::$reg => $r64,)+
            }
        }

        fn reg_name_8(reg: &Register) -> &'static str {
            match reg {
                $(Register::$reg => $r8,)+
            }
        }

        fn reg_name_32(reg: &Register) -> &'static str {
            match reg {
                $(Register::$reg => $r32,)+
            }
        }
    };
}

// Define all register names in one place
register_names! {
    Rax => { r64: "rax", r32: "eax", r8: "al" },
    Rbx => { r64: "rbx", r32: "ebx", r8: "bl" },
    Rcx => { r64: "rcx", r32: "ecx", r8: "cl" },
    Rdx => { r64: "rdx", r32: "edx", r8: "dl" },
    Rsi => { r64: "rsi", r32: "esi", r8: "sil" },
    Rdi => { r64: "rdi", r32: "edi", r8: "dil" },
    Rbp => { r64: "rbp", r32: "ebp", r8: "bpl" },
    Rsp => { r64: "rsp", r32: "esp", r8: "spl" },
    R8  => { r64: "r8",  r32: "r8d", r8: "r8b" },
    R9  => { r64: "r9",  r32: "r9d", r8: "r9b" },
    R10 => { r64: "r10", r32: "r10d", r8: "r10b" },
    R11 => { r64: "r11", r32: "r11d", r8: "r11b" },
    R12 => { r64: "r12", r32: "r12d", r8: "r12b" },
    R13 => { r64: "r13", r32: "r13d", r8: "r13b" },
    R14 => { r64: "r14", r32: "r14d", r8: "r14b" },
    R15 => { r64: "r15", r32: "r15d", r8: "r15b" },
}

#[cfg(test)]
mod mir_to_instructions_test;
